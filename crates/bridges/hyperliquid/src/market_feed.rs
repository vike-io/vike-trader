//! Live Hyperliquid market data → the vike-data live seam. `Feeds` implements
//! [`vike_data::DataClient`] directly, the HL twin of bybit's/okx's `market_feed` (COPY that
//! structure). Reachable at `market_feed::Feeds` for a future GUI, mirroring the other crypto venues.
//!
//! Same shape as the sibling feeds: one STOPPABLE thread per subscription. Each thread connects the
//! ONE public WS ([`crate::config::Network::urls`]`.2`), sends its subscribe frame, and runs a
//! read-loop that polls a per-subscription stop flag on the socket read timeout — so
//! [`Feeds::unsubscribe`]/[`Feeds::shutdown`] stop+join deterministically. Data flows out through the
//! [`vike_data::LiveDataSink`] handed to [`Feeds::new`]; the pure decoders live in
//! [`crate::market_data`].
//!
//! **Verbs served** (declared caps `{bars, quotes, trades, depth}`; `book:false`):
//! - `subscribe_bars` → `candle` → `close_bar` (lossless) / `forming_bar` + `bar_close_tick`
//!   (conflating). A PERP (bare-coin) bars subscription also opens `activeAssetCtx`, feeding the
//!   REAL-mark verb `mark_tick` (markPx; default ON, the venue's `mark_streams` row via
//!   [`Feeds::with_mark_streams`]).
//! - `subscribe_quotes` → `bbo` → [`LiveDataSink::quote`].
//! - `subscribe_trades` → `trades` → [`LiveDataSink::trade`].
//! - `subscribe_depth` → `l2Book` → [`LiveDataSink::l2_snapshot`] (the DOM conflating lane).
//! - `subscribe_book` → **`Unsupported`**: HL's `l2Book` is a full snapshot every frame with no
//!   delta lane to record, so the lossless `book`/`book_update` verb has nothing to carry.
//!
//! **HL specifics that shape the pump** (`docs/research/2026-07-16-hyperliquid-adapters` §7/§9):
//! - **Subscribe** = `{"method":"subscribe","subscription":{"type":<t>,"coin":<coin>,…}}`; keepalive
//!   is an APP-LEVEL `{"method":"ping"}` sent every 30 s (server drops a connection idle > 60 s —
//!   the cadence is this venue's `MarketPumpSpec` row), answered with a `{"channel":"pong"}` DATA
//!   frame.
//! - **`l2Book` is ALWAYS a full snapshot** (no deltas, no removals, no sequence — `time` the only
//!   ordering key), so depth emits a fresh `l2_snapshot` each frame, never a delta.
//! - **Candles carry no `confirm` flag** — closed-vs-forming is decided here by **open-time
//!   rollover**: a `candle` with a strictly-newer open-time (`t`) means the previously-forming candle
//!   is final. (There is no REST candle warm-up seed in this feed — WS-only, live-only, like the
//!   trades tape; a chart fills from the first live close forward.)
//! - **Reconnect** re-runs the same subscribe (the shape is captured in each thread's closure — the
//!   registry-per-subscription rule) and preserves the rollover state across the blip.
//!
//! **Dedup A6 (wave 3):** the WS session LIFECYCLE (connect + subscribe replay, app-level ping
//! cadence, silent-stall watchdog, read-timeout stop poll, stop-aware 30×100 ms reconnect backoff)
//! now rides the shared [`vike_bridge_core::market_pump`] driver — the generalization OF this
//! module's original crate-local `reconnect_loop`/`run_session` pair (retired here), so the shape
//! is identical by construction: the SAME `on_text` closure is reused across reconnects (the
//! rollover cursor survives a blip) and the subscribe frame is replayed verbatim each session. The
//! per-subscription stop/join bookkeeping rides [`vike_data::FeedRegistry`]. Only the PROTOCOL
//! stays here: the subscribe-frame builders, the frame decoders in [`crate::market_data`], the
//! rollover close detection, and the sink emission. One deliberate driver delta: the
//! `{"method":"ping"}` keepalive is now evaluated before EVERY read (the driver's wall-clock
//! cadence) rather than only on idle read-timeout ticks — a strict superset (HL answers with a
//! `pong` data frame; the 30 s cadence still satisfies the 60 s idle-drop rule either way).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use vike_bridge_core::depth::infer_tick_size;
use vike_bridge_core::market_pump::{FrameOutcome, MarketPumpOpts, SessionStatus, run_market_feed};
use vike_bridge_core::pump_spec::market_pump_spec;
use vike_data::{DataClient, FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId};
use vike_model::{Bar, now_ms};

use crate::config::Network;
use crate::consts::VENUE;
use crate::symbology::Symbology;

/// The app-level keepalive frame (the cadence knob lives in this venue's `MarketPumpSpec` row).
const PING_MSG: &str = r#"{"method":"ping"}"#;

/// The shared [`MarketPumpOpts`] every Hyperliquid market pump runs with — CONSUMED from this
/// venue's `MarketPumpSpec` row ([`market_pump_spec`], row ownership): subscribe replayed per
/// session, the app-level `{"method":"ping"}` keepalive every 30 s (HL drops a connection
/// idle > 60 s), NO subscribe-ack watchdog (HL's `subscribeResponse` was never armed pre-driver, so
/// the [`FrameOutcome`]s the closures return are inert to the lifecycle), the 60 s silent-stall
/// idle watchdog (this venue's own pre-driver shape — the one the driver's `idle_threshold`
/// generalized), and the classic stop-aware 30×100 ms reconnect backoff. The knob VALUES live in
/// the row (one edit site); only the payload strings stay here.
fn pump_opts(subscribe: &str) -> MarketPumpOpts<'_> {
    market_pump_spec(VENUE).knobs().opts(Some(subscribe), Some(PING_MSG))
}

// --- Subscribe-frame builders (pure; fixture-tested) -----------------------------------------

/// `{"method":"subscribe","subscription":{"type":"candle","coin":<coin>,"interval":<i>}}`.
pub fn subscribe_candle_msg(coin: &str, interval: &str) -> String {
    serde_json::json!({
        "method": "subscribe",
        "subscription": {"type": "candle", "coin": coin, "interval": interval}
    })
    .to_string()
}

/// `{"method":"subscribe","subscription":{"type":"bbo","coin":<coin>}}` (top-of-book quotes).
pub fn subscribe_bbo_msg(coin: &str) -> String {
    serde_json::json!({"method": "subscribe", "subscription": {"type": "bbo", "coin": coin}})
        .to_string()
}

/// `{"method":"subscribe","subscription":{"type":"trades","coin":<coin>}}` (executed prints).
pub fn subscribe_trades_msg(coin: &str) -> String {
    serde_json::json!({"method": "subscribe", "subscription": {"type": "trades", "coin": coin}})
        .to_string()
}

/// `{"method":"subscribe","subscription":{"type":"l2Book","coin":<coin>}}` (full-snapshot depth).
pub fn subscribe_l2book_msg(coin: &str) -> String {
    serde_json::json!({"method": "subscribe", "subscription": {"type": "l2Book", "coin": coin}})
        .to_string()
}

/// `{"method":"subscribe","subscription":{"type":"activeAssetCtx","coin":<coin>}}` — the perp
/// asset-context stream carrying the venue's REAL `markPx` (mark-slot semantics, W2-T4).
pub fn subscribe_active_asset_ctx_msg(coin: &str) -> String {
    serde_json::json!({
        "method": "subscribe",
        "subscription": {"type": "activeAssetCtx", "coin": coin}
    })
    .to_string()
}

/// PURE: the venue mark price out of one `activeAssetCtx` frame —
/// `{"channel":"activeAssetCtx","data":{"coin":…,"ctx":{"markPx":"…",…}}}` → `markPx` (a decimal
/// string; tolerated as a bare number too). `None` for any other channel, a malformed ctx, or a
/// dead (0/neg/NaN) mark. The frame carries no timestamp — the pump stamps receive time.
pub fn mark_from_frame(v: &Value) -> Option<f64> {
    if v.get("channel").and_then(Value::as_str) != Some("activeAssetCtx") {
        return None;
    }
    let mark = v.get("data")?.get("ctx")?.get("markPx")?;
    let px = match mark {
        Value::String(s) => s.parse::<f64>().ok()?,
        other => other.as_f64()?,
    };
    vike_bridge_core::is_valid_mark(px).then_some(px)
}

// --- The per-subscription WS pump ------------------------------------------------------------

struct FeedCtx {
    sink: Arc<dyn LiveDataSink>,
    status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
}

impl FeedCtx {
    fn set_status(&self, s: String) {
        *self.status.lock().unwrap() = s;
        (self.wake)();
    }
}

/// Run one pump on the shared driver: connect → subscribe → read-loop → stop-aware backoff →
/// reconnect, until the stop flag is raised ([`run_market_feed`], the generalization of this
/// module's retired `reconnect_loop`). The SAME `on_text` closure is reused across reconnects, so
/// any per-feed state it holds (the candle rollover cursor) survives a blip; the subscribe shape
/// is captured in `subscribe_msg` and replayed verbatim on every reconnect. The closures return
/// [`FrameOutcome::Confirm`] on emitted data / `Ignore` otherwise purely for driver hygiene — with
/// `ack_timeout: None` the outcome never gates the lifecycle (HL's pre-driver behavior).
fn run_pump(
    label: &str,
    ws_url: &str,
    subscribe_msg: &str,
    ctx: &FeedCtx,
    on_text: impl FnMut(&str) -> FrameOutcome,
) {
    run_market_feed(
        ws_url,
        &pump_opts(subscribe_msg),
        &ctx.stop,
        &now_ms,
        on_text,
        || {},
        |s| match s {
            // THE FIX, for all five lanes at once — this is the one `run_market_feed` call site
            // this venue has. Each `*_main` used to write its OWN `"LIVE · Hyperliquid <lane>
            // {series}"` BEFORE calling here, i.e. once per feed and never again; those five
            // writes are deleted and this is their replacement, made when the venue has actually
            // delivered a frame.
            //
            // ⚠ Two consequences, both accepted and both worth stating. (1) The first session now
            // reads `Feeds::new`'s `"connecting to Hyperliquid…"` until the first Confirm —
            // `Connecting` for `health_from_feed_status` (Healthy), a brief connecting dot in the
            // GUI; hyperliquid holds no `recon_feed_statuses` row, so nothing is at risk. (2) The
            // per-lane labels are gone from the healthy text, per the one-string-per-venue rule
            // (`crates/bridges/bybit/src/market_feed.rs`'s `LIVE_STATUS` argues it): five lanes
            // share ONE mutex here, so five spellings would churn any dedup placed on it.
            SessionStatus::Live => ctx.set_status("LIVE · Hyperliquid".into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{label} ws error (reconnecting): {e}"))
            }
        },
    );
}

/// One decoded `candle` frame → the sink lanes, with open-time rollover close detection (HL has no
/// `confirm` flag). `open_bar` is the rollover cursor: the most recent snapshot of the currently
/// forming candle. A NEW open-time closes the previous candle; the SAME open-time is an in-progress
/// update; a strictly-OLDER open-time is a stale replay (only possible right after a reconnect) and
/// is dropped. Every non-stale frame marks the last price and re-emits the forming bar. Returns the
/// driver's frame classification: any decoded candle (even a dropped stale one) is venue DATA →
/// [`FrameOutcome::Confirm`]; a non-candle frame → [`FrameOutcome::Ignore`] (inert either way — HL
/// runs no ack watchdog, see [`pump_opts`]).
fn handle_candle_frame(
    txt: &str,
    series: &str,
    interval: &str,
    ctx: &FeedCtx,
    open_bar: &mut Option<Bar>,
) -> FrameOutcome {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return FrameOutcome::Ignore; // e.g. the app-level pong / a non-JSON keepalive
    };
    let Some(bar) = crate::market_data::candle_to_bar(&v) else {
        return FrameOutcome::Ignore; // subscribeResponse / another channel / a malformed candle
    };
    if let Some(prev) = open_bar.as_ref() {
        if bar.ts < prev.ts {
            return FrameOutcome::Confirm; // stale replay — the current forming bar stands
        }
        if bar.ts > prev.ts {
            ctx.sink.close_bar(VENUE, series, interval, prev.clone()); // previous candle is final
        }
    }
    // `bar_close_tick`, NOT `mark_tick`: a candle close is not the venue mark (mark-slot
    // semantics; the real markPx rides the `activeAssetCtx` pump, `mark_main`).
    ctx.sink.bar_close_tick(VENUE, series, bar.close, bar.ts);
    *open_bar = Some(bar.clone());
    ctx.sink.forming_bar(VENUE, series, interval, bar);
    (ctx.wake)();
    FrameOutcome::Confirm
}

/// `subscribe_bars` thread body: `candle` stream, closed/forming via open-time rollover.
fn bars_main(coin: String, series: String, interval: String, ws_url: &'static str, ctx: FeedCtx) {
    let sub = subscribe_candle_msg(&coin, &interval);
    let mut open_bar: Option<Bar> = None;
    run_pump(&format!("{series}@{interval}"), ws_url, &sub, &ctx, |txt| {
        handle_candle_frame(txt, &series, &interval, &ctx, &mut open_bar)
    });
}

/// The perp mark-price pump body: `activeAssetCtx` → [`LiveDataSink::mark_tick`] (the
/// `PriceBoard` MARK slot; the candle pump's closes ride `bar_close_tick`). Live-only, no seed —
/// the resolver's bar-close fallback covers the pre-first-frame window. The frame carries no
/// venue timestamp, so each mark is stamped with machine receive time.
fn mark_main(coin: String, series: String, ws_url: &'static str, ctx: FeedCtx) {
    let sub = subscribe_active_asset_ctx_msg(&coin);
    run_pump(&format!("{series} mark"), ws_url, &sub, &ctx, |txt| {
        let Ok(v) = serde_json::from_str::<Value>(txt) else {
            return FrameOutcome::Ignore;
        };
        if let Some(px) = mark_from_frame(&v) {
            ctx.sink.mark_tick(VENUE, &series, px, now_ms());
            (ctx.wake)();
            FrameOutcome::Confirm
        } else {
            FrameOutcome::Ignore
        }
    });
}

/// `subscribe_quotes` thread body: `bbo` → [`LiveDataSink::quote`] (live-only, no seed).
fn quotes_main(coin: String, series: String, ws_url: &'static str, ctx: FeedCtx) {
    let sub = subscribe_bbo_msg(&coin);
    run_pump(&format!("{series} quotes"), ws_url, &sub, &ctx, |txt| {
        let Ok(v) = serde_json::from_str::<Value>(txt) else {
            return FrameOutcome::Ignore;
        };
        if let Some(mut q) = crate::market_data::bbo_to_quote(&v) {
            q.local_ts = now_ms(); // machine receive time (dual-timestamp capture)
            q.symbol = series.clone();
            ctx.sink.quote(VENUE, &series, q);
            (ctx.wake)();
            FrameOutcome::Confirm
        } else {
            FrameOutcome::Ignore
        }
    });
}

/// `subscribe_trades` thread body: `trades` → [`LiveDataSink::trade`] (live-only, no seed).
fn trades_main(coin: String, series: String, ws_url: &'static str, ctx: FeedCtx) {
    let sub = subscribe_trades_msg(&coin);
    run_pump(&format!("{series} trades"), ws_url, &sub, &ctx, |txt| {
        let Ok(v) = serde_json::from_str::<Value>(txt) else {
            return FrameOutcome::Ignore;
        };
        let ticks = crate::market_data::trades_from_frame(&v);
        if ticks.is_empty() {
            return FrameOutcome::Ignore;
        }
        for mut t in ticks {
            t.local_ts = now_ms(); // machine receive time (dual-timestamp capture)
            t.symbol = series.clone();
            ctx.sink.trade(VENUE, &series, t);
        }
        (ctx.wake)();
        FrameOutcome::Confirm
    });
}

/// `subscribe_depth` thread body: `l2Book` → [`LiveDataSink::l2_snapshot`]. HL sends a FULL snapshot
/// every frame (no deltas), so each frame is published verbatim as the DOM's conflating book; the
/// tick is inferred from the snapshot levels (HL has no fixed tick size), and the venue `time` is the
/// snapshot stamp.
fn depth_main(coin: String, series: String, ws_url: &'static str, ctx: FeedCtx) {
    let sub = subscribe_l2book_msg(&coin);
    run_pump(&format!("{series} depth"), ws_url, &sub, &ctx, |txt| {
        let Ok(v) = serde_json::from_str::<Value>(txt) else {
            return FrameOutcome::Ignore;
        };
        if let Some(snap) = crate::market_data::l2book_to_snapshot(&v) {
            let tick = infer_tick_size(&snap.bids, &snap.asks);
            ctx.sink.l2_snapshot(VENUE, &series, tick, snap.bids, snap.asks, snap.time);
            (ctx.wake)();
            FrameOutcome::Confirm
        } else {
            FrameOutcome::Ignore
        }
    });
}

/// All live feed threads, keyed by [`SubscriptionId`] via the shared [`FeedRegistry`] (dedup A6:
/// one stop flag + one `JoinHandle` per subscription; [`Feeds::unsubscribe`] stops+joins exactly
/// one, [`DataClient::shutdown`] stops+joins them all). Nothing is ever detached — the
/// deterministic-teardown rule, mirroring bybit/okx.
pub struct Feeds {
    sink: Arc<dyn LiveDataSink>,
    pub status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    network: Network,
    /// Shared, late-populatable resolver cell. Held behind an `Arc<Mutex<…>>` (not a plain
    /// `Option`) so the app can construct the feed synchronously at startup — no blocking network
    /// I/O, matching every other venue — then fill in the mainnet symbology from a background thread
    /// via [`Feeds::symbology_cell`]. Read once per `subscribe_*` (rare), so the lock never contends.
    symbology: Arc<Mutex<Option<Arc<Symbology>>>>,
    registry: FeedRegistry,
    /// Whether a perp `subscribe_bars` also opens the `activeAssetCtx` mark stream — the venue's
    /// `mark_streams` row (`Feeds::with_mark_streams`), else [`MARK_STREAM_DEFAULT_ON`] (mark-slot
    /// semantics, W2-T4).
    mark_streams: bool,
    /// PER-SYMBOL reference-counted companion mark streams (the mark pump has no id of its own at
    /// the `DataClient` seam), so charting one perp at two intervals opens ONE mark socket and
    /// [`Feeds::unsubscribe`] stops it only when the last bars subscription releases it.
    mark_pairings: vike_bridge_core::MarkPairings<SubscriptionId>,
}

/// This venue's mark wire is documented, so its mark stream ships ON;
/// `venue.hyperliquid.mark_streams = 0` turns it off (decision 0095).
const MARK_STREAM_DEFAULT_ON: bool = true;

/// Whether a `subscribe_bars` on `symbol` should ALSO open the `activeAssetCtx` mark stream: a
/// PERP (a bare coin — no `/`, see `resolve_coin`) AND the venue's `mark_streams` row. Pure — the
/// ONE place the pairing predicate lives, so the spawn site and its tests read the same law. Spot
/// pairs are skipped: their ctx channel is `activeSpotAssetCtx` and carries no `markPx`.
fn should_pair_mark(symbol: &str, mark_streams: bool) -> bool {
    mark_streams && !symbol.contains('/')
}

impl Feeds {
    /// `sink` receives every call from every subscription this `Feeds` spawns (shared — construct
    /// once, subscribe many). `wake` is the GUI repaint nudge; pass `|| {}` headless. Defaults to
    /// **mainnet** with no symbology (so a perp `coin` == its `symbol`); use [`Feeds::with_network`]
    /// / [`Feeds::with_symbology`] to change that. The registry's spawn hook is the venue's HFT
    /// affinity pin (opt-in via `VIKE_PIN_CORES`, no-op otherwise) — threaded in as a closure
    /// because `vike-data` deliberately never depends on `vike-exec`.
    pub fn new(sink: Arc<dyn LiveDataSink>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Feeds {
            sink,
            status: Arc::new(Mutex::new("connecting to Hyperliquid…".into())),
            wake: Arc::new(wake),
            network: Network::Mainnet,
            symbology: Arc::new(Mutex::new(None)),
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    "hyperliquid",
                );
            }),
            mark_streams: MARK_STREAM_DEFAULT_ON,
            mark_pairings: Default::default(),
        }
    }

    /// Apply this venue's `mark_streams` row (decision 0095) — `stored` is the row's value as the
    /// composition root read it, `None` when there is none. See
    /// `vike_bridge_core::mark_streams_from`.
    #[must_use]
    pub fn with_mark_streams(mut self, stored: Option<&str>) -> Self {
        self.mark_streams = vike_bridge_core::mark_streams_from(stored, MARK_STREAM_DEFAULT_ON);
        self
    }

    /// Attach the venue's REAL mark stream (`activeAssetCtx` `markPx`, mark-slot semantics —
    /// default ON, a `mark_streams = 0` row disables) to the bars subscription `bars_id` just
    /// created for `symbol`. Spawns at most ONE mark socket per symbol
    /// ([`vike_bridge_core::MarkPairings`]); a second bars subscription on the same perp only
    /// reference-counts the running one. Fail-soft: if the extra thread can't spawn, the bars feed
    /// stays live and valuation falls back to the resolver's bar-close rung, exactly the
    /// no-mark-stream behavior. Production and the pairing tests share this method — only `body`
    /// differs (tests pass a network-free stand-in).
    fn pair_mark_stream(
        &mut self,
        bars_id: SubscriptionId,
        symbol: &str,
        body: impl FnOnce(FeedCtx) + Send + 'static,
    ) {
        if self.mark_pairings.attach(bars_id, symbol, should_pair_mark(symbol, self.mark_streams)) {
            return; // disabled/spot, or an existing stream was reference-counted
        }
        if let Ok(mid) = self.spawn(&format!("mark-{symbol}"), body) {
            self.mark_pairings.record(bars_id, symbol, mid);
        }
    }

    /// Route feeds to a specific network (default [`Network::Mainnet`]). Public market data is
    /// keyless on both mainnet and testnet.
    pub fn with_network(mut self, network: Network) -> Self {
        self.network = network;
        self
    }

    /// Thread in the resolved [`Symbology`] so a unified `symbol` (`"HYPE/USDC"`) maps to its venue
    /// `coin` (`"@107"`) for the WS subscription. Without it, `symbol` is used AS the coin — correct
    /// for perps (`"BTC"` == coin) and any pre-resolved coin, so the common path needs no resolver.
    pub fn with_symbology(self, symbology: Arc<Symbology>) -> Self {
        *self.symbology.lock().unwrap() = Some(symbology);
        self
    }

    /// A clone of the shared symbology cell so a caller can populate it *after* construction — the
    /// non-blocking path: build the feed at startup, then fetch `meta`/`spotMeta` on a background
    /// thread and `*cell.lock().unwrap() = Some(Arc::new(sym))`. Until it's filled, spot symbols fall
    /// back to being used as their own coin (perps are unaffected — `coin == symbol`).
    pub fn symbology_cell(&self) -> Arc<Mutex<Option<Arc<Symbology>>>> {
        Arc::clone(&self.symbology)
    }

    fn ws_url(&self) -> &'static str {
        self.network.urls().2
    }

    /// Shared per-subscription bookkeeping, now one [`FeedRegistry::spawn`] call (dedup A6): the
    /// registry allocates the id + stop flag and owns the join handle (the MarketData affinity pin
    /// rides its spawn hook, see [`Feeds::new`]); this venue wrapper only assembles its own
    /// [`FeedCtx`] around the registry-issued stop flag. An OS thread-spawn failure is returned
    /// (not panicked) so the `subscribe_*` verb can map it to a [`LiveDataError`]. Mirrors
    /// bybit's `spawn_with`.
    fn spawn(
        &mut self,
        label: &str,
        body: impl FnOnce(FeedCtx) + Send + 'static,
    ) -> std::io::Result<SubscriptionId> {
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        self.registry.spawn(format!("feed-hl-{label}"), move |stop| {
            let ctx = FeedCtx { sink, status, wake, stop };
            body(ctx)
        })
    }
}

/// Max time a spot subscription waits (on its own feed thread) for the background symbology load
/// before falling back to the raw symbol. Perps never wait.
const SYMBOLOGY_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Resolve a unified `symbol` to the venue `coin` for a WS subscription. A bare perp coin (`"BTC"`,
/// no `/`) returns immediately. A spot pair (`"HYPE/USDC"`) needs the resolved [`Symbology`], which
/// is loaded on a BACKGROUND thread after the feed is constructed (so startup stays network-free) —
/// so this WAITS up to [`SYMBOLOGY_WAIT`] for the cell to fill, but only WHILE it is still empty.
/// Runs on the subscription's own feed thread, never the GUI thread, so the brief wait is safe. This
/// closes the startup / restored-workspace race: without it a spot chart present at launch would
/// subscribe with the unresolved symbol (the load hadn't finished) and stay empty forever. On
/// timeout (load failed/slow) or an unknown pair it falls back to the symbol itself.
fn resolve_coin(cell: &Arc<Mutex<Option<Arc<Symbology>>>>, symbol: &str) -> String {
    if !symbol.contains('/') {
        return symbol.to_string(); // perp / already-resolved coin — no symbology needed
    }
    let deadline = std::time::Instant::now() + SYMBOLOGY_WAIT;
    loop {
        if let Some(sym) = cell.lock().unwrap().as_ref() {
            // Symbology loaded: resolve now — a miss won't change with more waiting.
            return sym.coin_for(symbol).unwrap_or(symbol).to_string();
        }
        if std::time::Instant::now() >= deadline {
            return symbol.to_string(); // best-effort fallback
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

impl DataClient for Feeds {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        let (cell, series, interval_owned, ws_url) =
            (self.symbology_cell(), symbol.to_string(), interval.to_string(), self.ws_url());
        let id = self
            .spawn(&format!("bars-{symbol}@{interval}"), move |ctx| {
                let coin = resolve_coin(&cell, &series);
                bars_main(coin, series, interval_owned, ws_url, ctx)
            })
            .map_err(|e| {
                LiveDataError::Subscribe(format!("hyperliquid {symbol}@{interval}: {e}"))
            })?;
        let (series, ws_url) = (symbol.to_string(), self.ws_url());
        self.pair_mark_stream(id, symbol, move |ctx| {
            // A bare perp coin needs no symbology (coin == series).
            mark_main(series.clone(), series, ws_url, ctx)
        });
        Ok(id)
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let (cell, series, ws_url) = (self.symbology_cell(), symbol.to_string(), self.ws_url());
        self.spawn(&format!("quotes-{symbol}"), move |ctx| {
            let coin = resolve_coin(&cell, &series);
            quotes_main(coin, series, ws_url, ctx)
        })
        .map_err(|e| LiveDataError::Subscribe(format!("hyperliquid {symbol} quotes: {e}")))
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let (cell, series, ws_url) = (self.symbology_cell(), symbol.to_string(), self.ws_url());
        self.spawn(&format!("trades-{symbol}"), move |ctx| {
            let coin = resolve_coin(&cell, &series);
            trades_main(coin, series, ws_url, ctx)
        })
        .map_err(|e| LiveDataError::Subscribe(format!("hyperliquid {symbol} trades: {e}")))
    }

    /// Unsupported: HL's `l2Book` is a full snapshot every frame with no delta lane, so the lossless
    /// `book`/`book_update` verb has nothing to carry. The DOM's conflating snapshot lane is served by
    /// [`Self::subscribe_depth`] instead (declared caps `book:false, depth:true`).
    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("hyperliquid l2Book is snapshot-only; use subscribe_depth"))
    }

    /// Live L2 depth for the DOM: HL `l2Book` (a FULL snapshot every frame — no deltas, §7),
    /// delivered via [`LiveDataSink::l2_snapshot`].
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let (cell, series, ws_url) = (self.symbology_cell(), symbol.to_string(), self.ws_url());
        self.spawn(&format!("depth-{symbol}"), move |ctx| {
            let coin = resolve_coin(&cell, &series);
            depth_main(coin, series, ws_url, ctx)
        })
        .map_err(|e| LiveDataError::Subscribe(format!("hyperliquid {symbol} depth: {e}")))
    }

    /// Stop + JOIN exactly the stream `id` names — plus its companion perp mark stream when this
    /// was the LAST bars subscription holding that symbol's mark stream; every other subscription
    /// keeps running. Unknown ids (already stopped, never issued) are a no-op.
    fn unsubscribe(&mut self, id: SubscriptionId) {
        if let Some(mark_id) = self.mark_pairings.detach(id) {
            self.registry.stop_join(mark_id);
        }
        self.registry.stop_join(id);
    }

    /// Deterministic teardown: raise every stop flag, then JOIN every feed thread.
    fn shutdown(&mut self) {
        self.mark_pairings.clear();
        self.registry.shutdown();
    }
}

#[path = "market_feed_tests.rs"]
#[cfg(test)]
mod market_feed_tests;

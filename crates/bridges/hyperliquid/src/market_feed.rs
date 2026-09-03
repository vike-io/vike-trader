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
//!   REAL-mark verb `mark_tick` (markPx; default ON via `VIKE_MARK_STREAMS`).
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
use vike_bridge_core::market_pump::{run_market_feed, FrameOutcome, MarketPumpOpts};
use vike_bridge_core::pump_spec::market_pump_spec;
use vike_data::{DataClient, FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId};
use vike_model::{now_ms, Bar};

use crate::config::Network;
use crate::symbology::Symbology;

const VENUE: &str = crate::consts::VENUE;
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
        |e| ctx.set_status(format!("{label} ws error (reconnecting): {e}")),
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
    ctx.set_status(format!("LIVE · Hyperliquid {series}@{interval}"));
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
    // Same status announcement every sibling `*_main` makes: without it this pump is INVISIBLE in
    // the operator status surface, so a mark stream that never connects looks like no mark stream
    // at all (the resolver silently falls back to the bar-close rung).
    ctx.set_status(format!("LIVE · Hyperliquid mark {series}"));
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
    ctx.set_status(format!("LIVE · Hyperliquid quotes {series}"));
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
    ctx.set_status(format!("LIVE · Hyperliquid trades {series}"));
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
    ctx.set_status(format!("LIVE · Hyperliquid depth {series}"));
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
    /// Whether a perp `subscribe_bars` also opens the `activeAssetCtx` mark stream — resolved
    /// once at construction from `VIKE_MARK_STREAMS` (default ON; mark-slot semantics, W2-T4).
    mark_streams: bool,
    /// PER-SYMBOL reference-counted companion mark streams (the mark pump has no id of its own at
    /// the `DataClient` seam), so charting one perp at two intervals opens ONE mark socket and
    /// [`Feeds::unsubscribe`] stops it only when the last bars subscription releases it.
    mark_pairings: vike_bridge_core::MarkPairings<SubscriptionId>,
}

/// Whether a `subscribe_bars` on `symbol` should ALSO open the `activeAssetCtx` mark stream: a
/// PERP (a bare coin — no `/`, see `resolve_coin`) AND the `VIKE_MARK_STREAMS` knob. Pure — the
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
            mark_streams: vike_bridge_core::mark_streams_enabled(),
            mark_pairings: Default::default(),
        }
    }

    /// Attach the venue's REAL mark stream (`activeAssetCtx` `markPx`, mark-slot semantics —
    /// default ON, `VIKE_MARK_STREAMS=0` disables) to the bars subscription `bars_id` just created
    /// for `symbol`. Spawns at most ONE mark socket per symbol
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    // The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
    // RecordingSink copy that used to live here (same canonical `calls()` line forms).
    use vike_data::RecordingSink;

    /// A network-free stand-in for a real `*_main`: just polls its own stop flag.
    fn fake_body(ctx: FeedCtx) {
        while !ctx.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn test_ctx(sink: Arc<dyn LiveDataSink>) -> FeedCtx {
        FeedCtx {
            sink,
            status: Arc::new(Mutex::new(String::new())),
            wake: Arc::new(|| {}),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn subscribe_messages_have_the_hl_shape() {
        let candle: Value = serde_json::from_str(&subscribe_candle_msg("BTC", "1m")).unwrap();
        assert_eq!(candle["method"], "subscribe");
        assert_eq!(candle["subscription"]["type"], "candle");
        assert_eq!(candle["subscription"]["coin"], "BTC");
        assert_eq!(candle["subscription"]["interval"], "1m");

        let bbo: Value = serde_json::from_str(&subscribe_bbo_msg("@107")).unwrap();
        assert_eq!(bbo["subscription"]["type"], "bbo");
        assert_eq!(bbo["subscription"]["coin"], "@107");

        let trades: Value = serde_json::from_str(&subscribe_trades_msg("ETH")).unwrap();
        assert_eq!(trades["subscription"]["type"], "trades");
        assert_eq!(trades["subscription"]["coin"], "ETH");

        let book: Value = serde_json::from_str(&subscribe_l2book_msg("BTC")).unwrap();
        assert_eq!(book["subscription"]["type"], "l2Book");
        assert_eq!(book["subscription"]["coin"], "BTC");
    }

    /// Open-time rollover: the previous candle is emitted as CLOSED exactly when a strictly-newer
    /// open-time arrives; a same-open-time frame is an in-progress forming update.
    #[test]
    fn candle_rollover_closes_previous_bar() {
        let sink = Arc::new(RecordingSink::default());
        let ctx = test_ctx(sink.clone());
        let mut open_bar: Option<Bar> = None;
        let frame = |t: i64, c: &str| {
            format!(
                r#"{{"channel":"candle","data":{{"t":{t},"o":"1","h":"1","l":"1","c":"{c}","v":"1"}}}}"#
            )
        };
        handle_candle_frame(&frame(1000, "10"), "BTC", "1m", &ctx, &mut open_bar); // first → forming
        handle_candle_frame(&frame(1000, "11"), "BTC", "1m", &ctx, &mut open_bar); // update → forming
        handle_candle_frame(&frame(1060, "12"), "BTC", "1m", &ctx, &mut open_bar); // roll → close+forming
        assert_eq!(
            sink.calls(),
            vec![
                "bar_close_tick(hyperliquid,BTC,10,1000)".to_string(),
                "forming_bar(hyperliquid,BTC,1m,10)".to_string(),
                "bar_close_tick(hyperliquid,BTC,11,1000)".to_string(),
                "forming_bar(hyperliquid,BTC,1m,11)".to_string(),
                "close_bar(hyperliquid,BTC,1m,11)".to_string(),
                "bar_close_tick(hyperliquid,BTC,12,1060)".to_string(),
                "forming_bar(hyperliquid,BTC,1m,12)".to_string(),
            ]
        );
    }

    /// A strictly-older candle (a stale replay right after a reconnect) is dropped — the current
    /// forming bar stands, no close, no forming.
    #[test]
    fn candle_stale_replay_is_dropped() {
        let sink = Arc::new(RecordingSink::default());
        let ctx = test_ctx(sink.clone());
        let mut open_bar: Option<Bar> = None;
        let frame = |t: i64| {
            format!(
                r#"{{"channel":"candle","data":{{"t":{t},"o":"1","h":"1","l":"1","c":"5","v":"1"}}}}"#
            )
        };
        handle_candle_frame(&frame(2000), "BTC", "1m", &ctx, &mut open_bar);
        let n_after_first = sink.calls().len();
        handle_candle_frame(&frame(1000), "BTC", "1m", &ctx, &mut open_bar); // older → dropped
        assert_eq!(sink.calls().len(), n_after_first, "stale frame emitted nothing");
    }

    #[test]
    fn subscribe_returns_distinct_ids_and_shutdown_joins() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id1 = feeds.spawn("a", fake_body).expect("spawn ok");
        let id2 = feeds.spawn("b", fake_body).expect("spawn ok");
        assert_ne!(id1, id2, "distinct ids per subscribe");
        feeds.shutdown();
        assert!(feeds.registry.is_empty());
    }

    #[test]
    fn unsubscribe_stops_only_that_one_feed() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id1 = feeds.spawn("a", fake_body).expect("spawn ok");
        let id2 = feeds.spawn("b", fake_body).expect("spawn ok");
        feeds.unsubscribe(id1);
        assert_eq!(feeds.registry.len(), 1, "only the unsubscribed stream is removed");
        assert!(feeds.registry.contains(id2), "the other keeps running");
        feeds.shutdown();
        assert!(feeds.registry.is_empty());
    }

    /// The pairing PREDICATE (mark-slot semantics, W2-T4): only a PERP (a bare coin) pairs a mark
    /// stream, and only while the knob is on. A spot pair's ctx channel carries no `markPx`.
    #[test]
    fn only_perps_pair_a_mark_stream_and_only_while_the_knob_is_on() {
        assert!(should_pair_mark("BTC", true));
        assert!(!should_pair_mark("HYPE/USDC", true), "spot carries no markPx");
        assert!(!should_pair_mark("BTC", false), "VIKE_MARK_STREAMS=0 suppresses");
    }

    /// SPAWN side, network-free: `pair_mark_stream` is the production path `subscribe_bars` calls
    /// (only `body` differs here). A perp spawns a companion `activeAssetCtx` stream;
    /// unsubscribing the bars id stops BOTH, while unrelated subscriptions keep running.
    #[test]
    fn a_perp_bars_subscription_spawns_and_then_stops_its_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let bars_id = feeds.spawn("bars-BTC@1m", fake_body).expect("spawn ok");
        let other_id = feeds.spawn("bars-ETH@1m", fake_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTC", fake_body);
        assert_eq!(feeds.registry.len(), 3, "bars + mark + the unrelated feed");

        feeds.unsubscribe(bars_id);
        assert_eq!(feeds.registry.len(), 1, "bars AND mark stopped together");
        assert!(feeds.registry.contains(other_id), "unrelated subscriptions keep running");
        assert!(feeds.mark_pairings.is_empty(), "the pairing is consumed");
        feeds.shutdown();
    }

    #[test]
    fn a_spot_bars_subscription_spawns_no_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let bars_id = feeds.spawn("bars-HYPE/USDC@1m", fake_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "HYPE/USDC", fake_body);
        assert_eq!(feeds.registry.len(), 1, "no companion stream for spot");
        feeds.shutdown();
    }

    /// `VIKE_MARK_STREAMS=0` suppresses the spawn even for a perp — the knob's whole job, pinned.
    #[test]
    fn the_mark_streams_knob_off_suppresses_the_perp_spawn() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        feeds.mark_streams = false;
        let bars_id = feeds.spawn("bars-BTC@1m", fake_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTC", fake_body);
        assert_eq!(feeds.registry.len(), 1, "knob off -> no mark stream even for a perp");
        feeds.shutdown();
    }

    /// Per-symbol dedupe: a 1m AND a 5m chart on the same perp share ONE mark socket.
    #[test]
    fn two_intervals_on_one_perp_share_a_single_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let bars_1m = feeds.spawn("bars-BTC@1m", fake_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_1m, "BTC", fake_body);
        let bars_5m = feeds.spawn("bars-BTC@5m", fake_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_5m, "BTC", fake_body);
        assert_eq!(feeds.registry.len(), 3, "two bars feeds but only ONE mark stream");

        feeds.unsubscribe(bars_1m);
        assert_eq!(feeds.registry.len(), 2, "the mark stream the 5m chart still needs stays up");
        feeds.unsubscribe(bars_5m);
        assert!(feeds.registry.is_empty(), "the last release stops the mark stream too");
        feeds.shutdown();
    }

    #[test]
    fn book_is_unsupported() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        assert!(matches!(feeds.subscribe_book("BTC"), Err(LiveDataError::Unsupported(_))));
        // bars/quotes/trades/depth aren't exercised via a real subscribe here (that touches the
        // network); their subscribe-message builders + the candle rollover are covered above.
        feeds.shutdown();
    }

    /// A synthetic testnet spot symbology (BTC perp + HYPE/USDC spot → coin `@107`).
    fn test_symbology() -> Symbology {
        let meta = serde_json::json!({"universe": [{"name": "BTC", "szDecimals": 5}]});
        let spot = serde_json::json!({
            "tokens": [
                {"name": "USDC", "szDecimals": 8, "index": 0},
                {"name": "HYPE", "szDecimals": 2, "index": 150}
            ],
            "universe": [{"name": "@107", "tokens": [150, 0], "index": 107}]
        });
        Symbology::from_meta(&meta, &spot)
    }

    #[test]
    fn resolve_coin_bare_perp_needs_no_symbology() {
        // A perp coin has no `/`, so resolve_coin returns immediately even with an empty cell — it
        // must never wait for the (unneeded) symbology. Also pins the mainnet default.
        let feeds = Feeds::new(Arc::new(RecordingSink::default()), || {});
        assert_eq!(resolve_coin(&feeds.symbology_cell(), "BTC"), "BTC");
        assert_eq!(feeds.ws_url(), crate::consts::MAINNET_WS, "defaults to mainnet");
    }

    #[test]
    fn resolve_coin_resolves_spot_via_symbology() {
        let feeds = Feeds::new(Arc::new(RecordingSink::default()), || {})
            .with_network(Network::Testnet)
            .with_symbology(Arc::new(test_symbology()));
        let cell = feeds.symbology_cell(); // already populated
        assert_eq!(resolve_coin(&cell, "HYPE/USDC"), "@107", "spot symbol → @-coin");
        assert_eq!(resolve_coin(&cell, "BTC"), "BTC", "perp symbol == coin");
        assert_eq!(resolve_coin(&cell, "FOO/BAR"), "FOO/BAR", "loaded-but-unknown pair → fallback");
        assert_eq!(feeds.ws_url(), crate::consts::TESTNET_WS, "with_network took effect");
    }

    #[test]
    fn resolve_coin_waits_for_late_symbology() {
        // The exact race the in-thread wait closes: a spot subscribe fires BEFORE the background
        // symbology load finishes. resolve_coin must block (briefly, well under SYMBOLOGY_WAIT) until
        // the cell fills, then resolve the @-coin — never return the raw symbol.
        let cell: Arc<Mutex<Option<Arc<Symbology>>>> = Arc::new(Mutex::new(None));
        let writer = Arc::clone(&cell);
        let h = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(250));
            *writer.lock().unwrap() = Some(Arc::new(test_symbology()));
        });
        assert_eq!(
            resolve_coin(&cell, "HYPE/USDC"),
            "@107",
            "waited for the late load, then resolved"
        );
        h.join().unwrap();
    }

    // ---- activeAssetCtx mark stream (mark-slot semantics, W2-T4), scripted-frame style ----

    /// A real-shaped `activeAssetCtx` frame → the venue's `markPx` bits (a decimal string; a bare
    /// number is tolerated too — the pump stamps receive time as the mark's ts).
    #[test]
    fn mark_from_active_asset_ctx_frame() {
        let frame: Value = serde_json::from_str(
            r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"markPx":"27123.5","oraclePx":"27120.0","funding":"0.0001"}}}"#,
        )
        .unwrap();
        assert_eq!(mark_from_frame(&frame).unwrap().to_bits(), 27_123.5_f64.to_bits());

        let numeric: Value = serde_json::from_str(
            r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"markPx":42.0}}}"#,
        )
        .unwrap();
        assert_eq!(mark_from_frame(&numeric).unwrap().to_bits(), 42.0_f64.to_bits());
    }

    /// A non-`activeAssetCtx` channel, a missing ctx/markPx, and a dead (0/neg/NaN) mark all yield
    /// `None` — never a bogus mark into the `PriceBoard` mark slot.
    #[test]
    fn mark_from_frame_rejects_wrong_channel_and_dead_marks() {
        let candle: Value =
            serde_json::from_str(r#"{"channel":"candle","data":{"t":1,"c":"5"}}"#).unwrap();
        assert!(mark_from_frame(&candle).is_none(), "wrong channel");

        let no_ctx: Value =
            serde_json::from_str(r#"{"channel":"activeAssetCtx","data":{"coin":"BTC"}}"#).unwrap();
        assert!(mark_from_frame(&no_ctx).is_none(), "missing ctx");

        for dead in ["0", "-1.0", "nan"] {
            let f: Value = serde_json::from_str(&format!(
                r#"{{"channel":"activeAssetCtx","data":{{"ctx":{{"markPx":"{dead}"}}}}}}"#
            ))
            .unwrap();
            assert!(mark_from_frame(&f).is_none(), "dead mark {dead} rejected");
        }
    }

    /// The subscribe message targets the `activeAssetCtx` type for the given coin — the exact
    /// `{"method":"subscribe","subscription":{"type":"activeAssetCtx","coin":...}}` wire grammar.
    #[test]
    fn active_asset_ctx_subscribe_targets_the_coin() {
        let v: Value = serde_json::from_str(&subscribe_active_asset_ctx_msg("BTC")).unwrap();
        assert_eq!(v["method"], "subscribe");
        assert_eq!(v["subscription"]["type"], "activeAssetCtx");
        assert_eq!(v["subscription"]["coin"], "BTC");
    }
}

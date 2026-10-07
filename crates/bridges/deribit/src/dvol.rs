//! Deribit DVOL volatility-index feed — the venue's PUBLIC, keyless market-data half (the read
//! twin of [`crate::user_data`]'s AUTHED fill pump). Subscribes the public channel
//! `deribit_volatility_index.{btc_usd,eth_usd}` on a fresh public WS (no auth — the DVOL index is
//! keyless), maps each tick onto the vike-data live seam as a MARK-style series under a synthetic
//! symbol (`DVOL-BTC` / `DVOL-ETH`), and — behind an OPT-IN recorder gate — appends it to the
//! `HistStore`.
//!
//! WHY A FRESH SOCKET, not the "already-running" order/fill sockets: those two are AUTHED and
//! dedicated (a sync request/response order transport; a private `user.trades` pump). A public,
//! keyless streaming subscription belongs on its own public WS, exactly like every other venue's
//! market feed — so this rides the SHARED [`vike_bridge_core::market_pump`] driver
//! (`connect`/subscribe-each-session/read-on-stop-poll/idle-watchdog/backoff-reconnect), reusing
//! `connect_market_stream`'s `configure_ws_stream` (read timeout + `TCP_NODELAY`) rather than
//! hand-rolling a socket. Only the PROTOCOL lives here: the `public/subscribe` frame and the pure
//! per-frame decode ([`parse_dvol_frame`]) + emit ([`on_dvol_frame`]).
//!
//! LIVE SEAM (mark-slot semantics): a DVOL frame is the venue's own published index value — a
//! latest-wins reference stream — so it rides [`vike_data::LiveDataSink::mark_tick`] under
//! `venue = "deribit"`, `symbol = "DVOL-BTC"|"DVOL-ETH"`. The symbol is SYNTHETIC (not a tradeable
//! instrument), so it never collides with a real BTC/ETH valuation in the downstream `PriceBoard`.
//!
//! RECORDING (opt-in, byte-identical when off): [`DvolRecorder`] mirrors
//! [`vike_data::ChainRecorder`]'s idiom, with ONE deliberate difference — the enable GATE arrives
//! as a PARAMETER ([`DvolRecorder::with_cadence`]), never from `std::env::var`. A LIBRARY reading
//! the variable itself would be a SECOND source of truth for one toggle, the shape
//! `vike_ops::settings`' STEP-2 ledger exists to retire.
//!
//! ⚠ **THERE IS NO SETTING BEHIND THAT PARAMETER ANY MORE.** It used to be
//! `vike_config::Flags::record_dvol`, resolved `env > file > default` by the settings loader; that
//! field and its `VIKE_RECORD_DVOL` fold are DELETED
//! (`docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 0, tombstoned
//! at `vike_config::DEAD_FLAG_KEYS`), because a settings key nothing reads hands an operator
//! positive confirmation of something false. A `flags.record_dvol` row is now refused by name (the
//! load marks the settings unsound and `vike-tradehub` refuses to start on the mark), and
//! exporting `VIKE_RECORD_DVOL` warns that it configures nothing. What is
//! deleted is the SETTING; the parameter below is unchanged, and a root that mounts this feed
//! passes whatever it decides — a key of its own, or a plain `true`. The cadence is the caller's
//! parameter too (default [`DEFAULT_DVOL_CADENCE_MS`], 1 min) — D4 of decision 0095, which retired
//! the `VIKE_RECORD_DVOL_CADENCE_MS` variable this module used to read; a set one refuses startup.
//! It buckets the idempotency key so a fast index stream lands at most one sample per bucket.
//! Recording is best-effort (store errors logged and dropped). The value is persisted as a
//! `kind=trade` row (the DVOL value carried as `price`, `size = 0` — an index observation, not a
//! real print) under the same
//! `(deribit, DVOL-*)` partition, so a scan reads the index history back as a scalar price series.
//!
//! ⚠ **NO PRODUCTION WIRING — and this paragraph went on describing the mount for months after it
//! was deleted.** It read: "`vike-app`'s `main.rs` spawns [`spawn_deribit_dvol_feed`] … **only when
//! `flags.record_dvol` is on**". That mount was this feature's ONE production consumer and it went
//! with the desktop shell's whole local DATA plane. Nothing in the tree calls
//! [`spawn_deribit_dvol_feed`] and nothing constructs a [`DvolRecorder`] outside this file's own
//! `#[cfg(test)]` module. That absence is what finally deleted the settings key: a toggle whose
//! only consumer is gone is not a feature an operator can turn on.
//!
//! ⚠ **The MECHANISM is the store, not the socket, and getting that backwards points the next
//! mount at the wrong problem.** It is tempting to say the feed went because "the desktop opens no
//! venue socket" (rulings 1 and 2 of the 2026-09-09 desktop rename). It did not: this channel is
//! PUBLIC and KEYLESS, the deleted mount's own comment said it "runs whether or not deribit is
//! armed for execution", and the WHY A FRESH SOCKET paragraph above is an argument that this
//! subscription belongs to nobody's venue mount. What the mount actually needed was the two things
//! the `fat` local data plane owned: a concrete `vike_data::DataFusionHist` to open at the root's
//! tick-store path, and the COMPOSED `LiveDataSink` the venue feeds tee'd through. `fat` was
//! deleted outright (`default = ["fat"]`, so a plain build HAD it), the desktop links vike-data
//! with default features now — the trait-only `HistStore` seam, no engine to open — and both
//! halves went with it. The sibling recorder's tombstone in `crates/vike-desktop/src/main.rs`
//! states the same mechanism for `flags.record_chains` at its own site.
//!
//! ⚠ `vike_config::CONSUMPTION`'s `flags.record_dvol` row was the gated authority here — the
//! table's one `Reader::Nothing` — and it argued why the field was KEPT rather than deleted. That
//! row is GONE with the key it described. The authority is now `vike_config::DEAD_FLAG_KEYS`,
//! which says the same true thing from the other side: the feature is unmounted, not removed, and
//! the row names the two symbols below so a deletion is a pointer rather than an erasure.
//!
//! What survives, for whoever mounts it next: [`DvolRecorder::with_cadence`] and
//! [`spawn_deribit_dvol_feed`] both still take the resolved flag as a PARAMETER, so re-arming is a
//! composition root spawning the feed with the composed `LiveDataSink` every other venue feed
//! writes through, plus a recorder over that root's tick store — not a line of change in this
//! file. OFF (the default, and today the only state) is a true no-op: no store open, no thread, no
//! socket, no subscribe, byte-identical. The feed was gated on the RECORD flag rather than mounted
//! unconditionally because the live seam has no reader — nothing displays `DVOL-BTC`/`DVOL-ETH` —
//! so recording is the only thing the subscription would be for, and an always-on public socket
//! would buy an operator nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;

use vike_bridge_core::json::{get_f64_opt, get_i64};
use vike_bridge_core::market_pump::{
    FrameOutcome, MarketPumpOpts, PumpBackoff, SessionStatus, run_market_feed,
};
use vike_data::{HistStore, LiveDataSink};
use vike_model::{TradeTick, now_ms};

/// Canonical venue id — the partition `venue` for both the live seam and the recorded series.
const VENUE: &str = "deribit";

/// Default idempotency-bucket cadence: at most one DVOL sample per symbol per minute. The cadence
/// is the caller's [`DvolRecorder::with_cadence`] parameter; [`DvolRecorder::new`] takes this one.
///
/// ⚠ There is deliberately no environment read for the gate OR the cadence: the ENABLE gate and
/// the cadence are plain PARAMETERS, the `flags.record_dvol` key that used to fill the gate is
/// DELETED (`vike_config::DEAD_FLAG_KEYS`), and decision 0095 retired the cadence variable. A
/// library re-reading a variable would be a second, silently-disagreeing authority over the same
/// toggle, which is the shape this crate refuses whatever the caller's gate turns out to be.
pub const DEFAULT_DVOL_CADENCE_MS: i64 = 60_000;

/// Silent-stall watchdog window: no inbound frame of any kind for this long ⇒ reconnect. The DVOL
/// index publishes continuously, so a healthy feed never trips it.
const IDLE_SECS: u64 = 30;
/// Bounded TCP dial so a black-holed route can't pin the feed thread past the stop flag.
const CONNECT_SECS: u64 = 10;
/// Reconnect backoff after a session fault — the crypto venues' classic 3 s.
const BACKOFF_SECS: u64 = 3;

/// One decoded DVOL volatility-index observation. `index_name` is the venue's own index id
/// (`"btc_usd"`); `ts` is the publish epoch-ms; `volatility` is the DVOL value.
#[derive(Debug, Clone, PartialEq)]
pub struct DvolTick {
    pub index_name: String,
    pub ts: i64,
    pub volatility: f64,
}

/// The public DVOL channel for a base currency, e.g. `"btc"` → `"deribit_volatility_index.btc_usd"`.
pub fn dvol_channel(currency: &str) -> String {
    format!("deribit_volatility_index.{}_usd", currency.to_ascii_lowercase())
}

/// The synthetic mark-series symbol for a DVOL `index_name`, e.g. `"btc_usd"` → `"DVOL-BTC"`.
/// `None` for an empty / non-`_usd` index (the frame is then ignored, never mis-symboled).
pub fn dvol_symbol(index_name: &str) -> Option<String> {
    let base = index_name.strip_suffix("_usd")?;
    if base.is_empty() {
        return None;
    }
    Some(format!("DVOL-{}", base.to_ascii_uppercase()))
}

/// Pure decode: a Deribit `deribit_volatility_index.*` subscription frame → [`DvolTick`], or `None`
/// for any other frame (wrong method/channel, a non-DVOL `subscription`, an ack/response, or a
/// frame missing the load-bearing `volatility` field). `index_name` prefers the payload field,
/// falling back to the channel suffix; `timestamp` defaults to 0 when absent (`int(x or 0)`).
pub fn parse_dvol_frame(frame: &Value) -> Option<DvolTick> {
    if frame.get("method").and_then(|m| m.as_str()) != Some("subscription") {
        return None;
    }
    let params = frame.get("params")?;
    let channel = params.get("channel").and_then(|c| c.as_str()).unwrap_or("");
    if !channel.starts_with("deribit_volatility_index.") {
        return None;
    }
    let data = params.get("data")?;
    // `volatility` is the value the whole frame exists to carry: absent/unparseable ⇒ not a DVOL
    // value frame (fail-closed, never a fabricated 0.0).
    let volatility = get_f64_opt(data, "volatility")?;
    let index_name = data
        .get("index_name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| channel.strip_prefix("deribit_volatility_index.").map(str::to_string))
        .unwrap_or_default();
    let ts = get_i64(data, "timestamp"); // epoch ms; string-or-number, `int(x or 0)` default
    Some(DvolTick { index_name, ts, volatility })
}

/// The DVOL tick as a `kind=trade` row: index value → `price`, `size = 0` (an index observation,
/// not a real print), `ts`/`local_ts` = the venue publish stamp. The synthetic `symbol` is carried
/// by the store partition, not the row (empty here, matching every other tick producer).
fn dvol_trade_tick(tick: &DvolTick) -> TradeTick {
    TradeTick {
        ts: tick.ts,
        local_ts: tick.ts,
        price: tick.volatility,
        size: 0.0,
        is_buyer_maker: false,
        symbol: String::new(),
    }
}

/// Handle one inbound TEXT frame: decode → emit the mark tick on the live seam → (opt-in) record.
/// Returns [`FrameOutcome::Confirm`] for a valid DVOL frame (proves the subscription is live to the
/// driver) and [`FrameOutcome::Ignore`] for anything else (the subscribe ack, a keepalive, junk).
/// The `recorder` half is the ONLY opt-in behavior: the mark tick always flows; recording happens
/// only when a recorder is present AND enabled.
pub fn on_dvol_frame(
    txt: &str,
    sink: &dyn LiveDataSink,
    recorder: Option<&DvolRecorder>,
) -> FrameOutcome {
    let Ok(frame) = serde_json::from_str::<Value>(txt) else {
        return FrameOutcome::Ignore;
    };
    let Some(tick) = parse_dvol_frame(&frame) else {
        return FrameOutcome::Ignore;
    };
    let Some(symbol) = dvol_symbol(&tick.index_name) else {
        return FrameOutcome::Ignore;
    };
    sink.mark_tick(VENUE, &symbol, tick.volatility, tick.ts);
    if let Some(rec) = recorder {
        rec.record(&symbol, &tick);
    }
    FrameOutcome::Confirm
}

/// Opt-in, best-effort DVOL recorder — the [`vike_data::ChainRecorder`] twin for the volatility
/// index. Capture is gated by the `enabled` PARAMETER, and by nothing else — the `VIKE_RECORD_DVOL`
/// / `flags.record_dvol` pair that used to fill it is deleted (`vike_config::DEAD_FLAG_KEYS`, and
/// this module's doc for why). Disabled is a no-op. A store error is logged
/// and dropped — recording must never fail or block the feed thread.
///
/// Idempotency: a cadence-bucket commit key (`dvol:{venue}:{symbol}:{bucket}`, `bucket =
/// ts.div_euclid(cadence_ms)`), so a sub-second index stream lands at most one sample per bucket
/// (default 1 min). The persisted row is a single-row `kind=trade` batch (the DVOL value as
/// `price`).
pub struct DvolRecorder {
    store: Arc<dyn HistStore + Send + Sync>,
    enabled: bool,
    cadence_ms: i64,
}

impl DvolRecorder {
    /// Default cadence ([`DEFAULT_DVOL_CADENCE_MS`]).
    pub fn new(store: Arc<dyn HistStore + Send + Sync>, enabled: bool) -> Self {
        Self::with_cadence(store, enabled, DEFAULT_DVOL_CADENCE_MS)
    }

    /// The app-root constructor: `enabled` and `cadence_ms` are the mounting root's own decisions,
    /// taken as parameters so this crate has no opinion about where they came from (D4 of decision
    /// 0095 — nothing mounts this feed today, so neither has a settings row). Non-positive
    /// `cadence_ms` is clamped to 1 ms — every distinct ms its own bucket.
    ///
    /// ⚠ It replaced a `from_flag` that read the cadence variable itself, which in turn replaced a
    /// `from_env` that read `VIKE_RECORD_DVOL` too. The shape is what matters: ONE authority,
    /// supplied by the caller, never a library reaching for the environment behind that caller's
    /// back.
    pub fn with_cadence(
        store: Arc<dyn HistStore + Send + Sync>,
        enabled: bool,
        cadence_ms: i64,
    ) -> Self {
        Self { store, enabled, cadence_ms: cadence_ms.max(1) }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The resolved idempotency-bucket cadence in ms (post-clamp).
    pub fn cadence_ms(&self) -> i64 {
        self.cadence_ms
    }

    /// Record one DVOL tick under `(deribit, symbol)`. No-op when disabled; store errors logged and
    /// dropped. `symbol` is the resolved synthetic id ([`dvol_symbol`]) so the recorded partition
    /// matches the live-seam mark tick exactly.
    pub fn record(&self, symbol: &str, tick: &DvolTick) {
        if !self.enabled {
            return;
        }
        let bucket = tick.ts.div_euclid(self.cadence_ms);
        let key = format!("dvol:{}:{symbol}:{bucket}", VENUE);
        let row = dvol_trade_tick(tick);
        if let Err(e) = self.store.append_trades(VENUE, symbol, &[row], Some(&key)) {
            tracing::warn!(target: "vike_deribit::dvol", symbol, error = %e, "dvol recording failed (dropped)");
        }
    }
}

/// Deribit `public/subscribe` frame for keyless public channels — the public twin of
/// [`crate::ws_auth::build_private_subscribe`] (DVOL needs no auth, so it rides `public/subscribe`,
/// replayed verbatim each session by the driver). Safe to log (channel names only). The `id` is a
/// fixed sentinel: the driver disarms its handshake on the first `Confirm` (see [`on_dvol_frame`]),
/// never by id-matching the ack.
fn build_public_subscribe(channels: &[String]) -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "public/subscribe",
        "params": {"channels": channels},
    })
    .to_string()
}

/// Join handle for a spawned DVOL feed — deterministic teardown (raise stop, join the thread).
pub struct DvolFeed {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl DvolFeed {
    /// Stop the feed and join its thread. Idempotent-safe teardown.
    pub fn shutdown(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

/// Spawn the persistent public DVOL feed: connect `ws_url`, subscribe the DVOL channels for each
/// `currencies` entry (e.g. `["btc", "eth"]`), and stream ticks to `sink` (mark-style) plus the
/// opt-in `recorder`. Runs the shared reconnect/subscribe/idle-watchdog lifecycle
/// ([`vike_bridge_core::market_pump::run_market_feed`]). Nothing here is auth-gated — DVOL is public.
///
/// `recorder = None` (or a disabled recorder) records nothing — the live-seam delivery still runs.
pub fn spawn_deribit_dvol_feed(
    ws_url: String,
    currencies: Vec<String>,
    sink: Arc<dyn LiveDataSink>,
    recorder: Option<Arc<DvolRecorder>>,
) -> DvolFeed {
    let channels: Vec<String> = currencies.iter().map(|c| dvol_channel(c)).collect();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("deribit-dvol".to_string())
        .spawn(move || {
            let _span = tracing::info_span!("dvol_feed", venue = VENUE).entered();
            dvol_feed_body(&ws_url, &channels, sink.as_ref(), recorder.as_deref(), &stop_thread);
        })
        .expect("spawn deribit dvol feed");
    DvolFeed { stop, handle }
}

/// The feed thread body: build the subscribe + pump opts, then hand the whole
/// connect → subscribe → read → backoff → reconnect lifecycle to the shared driver, decoding each
/// frame through [`on_dvol_frame`].
fn dvol_feed_body(
    ws_url: &str,
    channels: &[String],
    sink: &dyn LiveDataSink,
    recorder: Option<&DvolRecorder>,
    stop: &AtomicBool,
) {
    let sub = build_public_subscribe(channels);
    let opts = MarketPumpOpts {
        subscribe: Some(&sub),
        keepalive: None, // regular index frames + tungstenite auto-pong keep the socket warm
        ack_timeout: None, // the idle watchdog reconnects a dead subscribe; the ack is `Ignore`d
        idle_threshold: Some(Duration::from_secs(IDLE_SECS)),
        read_timeout: Duration::from_secs(1),
        backoff: PumpBackoff::Fixed(Duration::from_secs(BACKOFF_SECS)),
        connect_timeout: Some(Duration::from_secs(CONNECT_SECS)),
    };
    // `&now_ms` (the vike_model fn item) coerces to `&dyn Fn() -> i64` — the same call shape
    // polymarket's `market_feed` uses; no wrapping closure (which would trip `redundant_closure`).
    run_market_feed(
        ws_url,
        &opts,
        stop,
        &now_ms,
        |txt| on_dvol_frame(txt, sink, recorder),
        || {},
        |s| match s {
            // No status mutex exists on this lane — the `Live` arm is the DECLARATION that it was
            // classified rather than skipped, and it mirrors the `warn!` below at the same
            // cadence (once per session, not per frame).
            SessionStatus::Live => {
                tracing::info!(target: "vike_deribit::dvol", "dvol feed session live")
            }
            SessionStatus::Error(e) => {
                tracing::warn!(target: "vike_deribit::dvol", error = %e, "dvol feed session error")
            }
        },
    );
}

#[path = "dvol_tests.rs"]
#[cfg(test)]
mod dvol_tests;

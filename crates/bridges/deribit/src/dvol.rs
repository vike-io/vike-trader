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
//! as a PARAMETER ([`DvolRecorder::from_flag`]), never from `std::env::var`. It is
//! `vike_config::Flags::record_dvol`, resolved `env > file > default` by the settings loader, so
//! `VIKE_RECORD_DVOL=1` and `record_dvol = true` in `<project>/settings/flags.toml` both still arm
//! it — and a LIBRARY reading the variable itself would be a SECOND source of truth for one toggle,
//! the shape `vike_ops::settings`' STEP-2 ledger exists to retire. Only the cadence
//! (`VIKE_RECORD_DVOL_CADENCE_MS`, default 1 min) is still read here; it buckets the idempotency key
//! so a fast index stream lands at most one sample per bucket. Recording is best-effort (store
//! errors logged and dropped). The value is persisted as a `kind=trade` row (the DVOL value carried
//! as `price`, `size = 0` — an index observation, not a real print) under the same
//! `(deribit, DVOL-*)` partition, so a scan reads the index history back as a scalar price series.
//!
//! PRODUCTION WIRING: `vike-app`'s `main.rs` spawns [`spawn_deribit_dvol_feed`] — with the composed
//! `LiveDataSink` every other venue feed already writes through, plus a [`DvolRecorder`] over the
//! app's own tick store — **only when `flags.record_dvol` is on**. OFF (the default) is a true
//! no-op: no store open, no thread, no socket, no subscribe, byte-identical. The feed is gated on
//! the RECORD flag rather than mounted unconditionally because the live seam has no reader yet —
//! nothing in the GUI displays `DVOL-BTC`/`DVOL-ETH` — so recording is the only thing the
//! subscription is currently for, and an always-on public socket would buy an operator nothing.
//! ⚠ Until this wiring existed the flag was a knob with nothing on the other end: `from_env` was
//! called only from this file's own `#[cfg(test)]` module and nothing spawned the feed, so
//! `VIKE_RECORD_DVOL=1` recorded nothing and said so nowhere.

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

/// Env var overriding the idempotency-bucket cadence in ms (unset/unparsable →
/// [`DEFAULT_DVOL_CADENCE_MS`]).
///
/// ⚠ There is deliberately no `RECORD_DVOL_ENV` beside it. The ENABLE gate is
/// `vike_config::Flags::record_dvol`, taken as a parameter by [`DvolRecorder::from_flag`] — the
/// settings loader already resolves `VIKE_RECORD_DVOL` into it, and a library that re-read the
/// variable would be a second, silently-disagreeing authority over the same toggle.
pub const RECORD_DVOL_CADENCE_ENV: &str = "VIKE_RECORD_DVOL_CADENCE_MS";
/// Default idempotency-bucket cadence: at most one DVOL sample per symbol per minute.
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
/// index. Capture is gated on `VIKE_RECORD_DVOL=1`; disabled is a no-op. A store error is logged
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

    /// Explicit cadence (non-positive is clamped to 1 ms — every distinct ms its own bucket).
    pub fn with_cadence(
        store: Arc<dyn HistStore + Send + Sync>,
        enabled: bool,
        cadence_ms: i64,
    ) -> Self {
        Self { store, enabled, cadence_ms: cadence_ms.max(1) }
    }

    /// The app-root constructor: `enabled` is `vike_config::Flags::record_dvol` — the RESOLVED
    /// toggle (`env > file > default`), taken as a parameter so this crate has no opinion about
    /// where it came from. Cadence from `VIKE_RECORD_DVOL_CADENCE_MS` when set and parsable, else
    /// 1 minute.
    ///
    /// ⚠ This replaced a `from_env` that read `VIKE_RECORD_DVOL` itself. The variable still works —
    /// `vike_config::Flags::apply_env` reads it into the flag — but there is now exactly ONE reader
    /// of it in the tree, so a `flags.toml` that says `record_dvol = true` can no longer be
    /// silently overruled by a library consulting an unset environment.
    pub fn from_flag(store: Arc<dyn HistStore + Send + Sync>, enabled: bool) -> Self {
        // ONE explicit `env::var(CONST)` read, kept here rather than folded into a sweep so the
        // settings-registry scanner still resolves it by call site.
        let vars = std::env::var(RECORD_DVOL_CADENCE_ENV)
            .ok()
            .map(|v| (RECORD_DVOL_CADENCE_ENV.to_string(), v))
            .into_iter()
            .collect();
        Self::from_flag_and_vars(store, enabled, &vars)
    }

    /// The PURE half of [`Self::from_flag`]: the same gate-as-parameter, with the cadence resolved
    /// from a caller-supplied map instead of the process env — the shape a test can drive now that
    /// edition 2024 has made `std::env::set_var` an `unsafe fn` this workspace forbids.
    pub fn from_flag_and_vars(
        store: Arc<dyn HistStore + Send + Sync>,
        enabled: bool,
        vars: &std::collections::HashMap<String, String>,
    ) -> Self {
        let cadence_ms = vars
            .get(RECORD_DVOL_CADENCE_ENV)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(DEFAULT_DVOL_CADENCE_MS);
        Self::with_cadence(store, enabled, cadence_ms)
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

#[cfg(test)]
mod tests {
    //! No live network: the pure decode + the opt-in recorder gate are exercised over a
    //! DataFusion-free in-crate trade-capturing [`HistStore`] double (MemHistStore's `append_trades`
    //! is an inert stub, so a trade-round-trip needs this local double) plus the shared
    //! `RecordingSink`.
    use super::*;

    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    use vike_data::{DataError, ExecFillRow, ExecOrderRow, RecordingSink, SinkCall, TsRange};
    use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties};

    fn in_range(ts: i64, range: TsRange) -> bool {
        range.start.map(|s| ts >= s).unwrap_or(true) && range.end.map(|e| ts <= e).unwrap_or(true)
    }

    /// A `kind=trade`-capturing [`HistStore`]: honors the batch-level `commit_key` idempotency
    /// contract (a repeated key is a no-op) and empty-batch fidelity, matching `DataFusionHist`.
    /// Every other method is an inert stub — this double exists for the DVOL trade seam only.
    #[derive(Default)]
    struct CaptureStore {
        trades: Mutex<HashMap<(String, String), Vec<TradeTick>>>,
        seen: Mutex<HashSet<String>>,
    }

    impl HistStore for CaptureStore {
        fn load_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _r: TsRange,
        ) -> Result<Vec<Bar>, DataError> {
            Ok(Vec::new())
        }
        fn scan_quotes(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<QuoteTick>, DataError> {
            Ok(Vec::new())
        }
        fn scan_trades(
            &self,
            venue: &str,
            symbol: &str,
            range: TsRange,
        ) -> Result<Vec<TradeTick>, DataError> {
            let map = self.trades.lock().unwrap();
            let mut out: Vec<TradeTick> = map
                .get(&(venue.to_string(), symbol.to_string()))
                .map(|rows| rows.iter().filter(|t| in_range(t.ts, range)).cloned().collect())
                .unwrap_or_default();
            out.sort_by_key(|r| r.ts);
            Ok(out)
        }
        fn append_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _b: &[Bar],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn append_quotes(
            &self,
            _v: &str,
            _s: &str,
            _t: &[QuoteTick],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn append_trades(
            &self,
            venue: &str,
            symbol: &str,
            ticks: &[TradeTick],
            commit_key: Option<&str>,
        ) -> Result<usize, DataError> {
            if ticks.is_empty() {
                return Ok(0);
            }
            if let Some(k) = commit_key {
                let mut seen = self.seen.lock().unwrap();
                if !seen.insert(k.to_string()) {
                    return Ok(0); // already ingested — batch-level no-op
                }
            }
            self.trades
                .lock()
                .unwrap()
                .entry((venue.to_string(), symbol.to_string()))
                .or_default()
                .extend_from_slice(ticks);
            Ok(ticks.len())
        }
        fn append_book_updates(
            &self,
            _v: &str,
            _s: &str,
            _u: &[BookUpdate],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn scan_book_updates(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<BookUpdate>, DataError> {
            Ok(Vec::new())
        }
        fn append_symbol_properties(
            &self,
            _v: &str,
            _s: &str,
            _r: &[(i64, SymbolProperties)],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn scan_symbol_properties(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
            Ok(Vec::new())
        }
        fn append_equity(
            &self,
            _v: &str,
            _s: &str,
            _r: &[EquitySample],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn scan_equity(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<EquitySample>, DataError> {
            Ok(Vec::new())
        }
        fn append_exec_fills(
            &self,
            _v: &str,
            _s: &str,
            _r: &[ExecFillRow],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn scan_exec_fills(&self, _v: &str, _s: &str) -> Result<Vec<ExecFillRow>, DataError> {
            Ok(Vec::new())
        }
        fn append_exec_orders(
            &self,
            _v: &str,
            _s: &str,
            _r: &[ExecOrderRow],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn scan_exec_orders(&self, _v: &str, _s: &str) -> Result<Vec<ExecOrderRow>, DataError> {
            Ok(Vec::new())
        }
        fn resample_quotes_to_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _r: TsRange,
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn resample_trades_to_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _r: TsRange,
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
    }

    /// A real Deribit `deribit_volatility_index.{index}` subscription frame.
    fn dvol_frame(index: &str, ts: i64, vol: f64) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "subscription",
            "params": {
                "channel": format!("deribit_volatility_index.{index}"),
                "data": { "timestamp": ts, "volatility": vol, "index_name": index }
            }
        })
    }

    fn recorded_marks(sink: &RecordingSink) -> Vec<(String, String, u64, i64)> {
        sink.recorded()
            .into_iter()
            .filter_map(|c| match c {
                SinkCall::MarkTick { venue, symbol, px, ts } => {
                    Some((venue, symbol, px.to_bits(), ts))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_dvol_subscription_frame() {
        let tick =
            parse_dvol_frame(&dvol_frame("btc_usd", 1_700_000_000_000, 62.5)).expect("parsed");
        assert_eq!(tick.index_name, "btc_usd");
        assert_eq!(tick.ts, 1_700_000_000_000);
        assert_eq!(tick.volatility.to_bits(), 62.5f64.to_bits());
    }

    #[test]
    fn parse_falls_back_to_channel_suffix_for_index_and_defaults_ts() {
        // No `index_name`/`timestamp` in data: index comes from the channel suffix, ts defaults 0.
        let f = serde_json::json!({
            "method": "subscription",
            "params": {
                "channel": "deribit_volatility_index.eth_usd",
                "data": { "volatility": 70.0 }
            }
        });
        let tick = parse_dvol_frame(&f).expect("parsed");
        assert_eq!(tick.index_name, "eth_usd");
        assert_eq!(tick.ts, 0);
        assert_eq!(tick.volatility.to_bits(), 70.0f64.to_bits());
    }

    #[test]
    fn parse_rejects_non_dvol_and_malformed_frames() {
        // wrong method
        assert!(parse_dvol_frame(&serde_json::json!({"method": "heartbeat"})).is_none());
        // a user.trades subscription (wrong channel)
        assert!(
            parse_dvol_frame(&serde_json::json!({
                "method": "subscription",
                "params": {"channel": "user.trades.any.any.raw", "data": []}
            }))
            .is_none()
        );
        // a DVOL frame missing the load-bearing `volatility` field
        assert!(
            parse_dvol_frame(&serde_json::json!({
                "method": "subscription",
                "params": {"channel": "deribit_volatility_index.btc_usd", "data": {"timestamp": 1}}
            }))
            .is_none()
        );
        // the subscribe ack (a JSON-RPC response, no `method`)
        assert!(
            parse_dvol_frame(&serde_json::json!({
                "id": 1, "result": ["deribit_volatility_index.btc_usd"]
            }))
            .is_none()
        );
    }

    #[test]
    fn channel_and_symbol_mapping() {
        assert_eq!(dvol_channel("btc"), "deribit_volatility_index.btc_usd");
        assert_eq!(dvol_channel("ETH"), "deribit_volatility_index.eth_usd");
        assert_eq!(dvol_symbol("btc_usd").as_deref(), Some("DVOL-BTC"));
        assert_eq!(dvol_symbol("eth_usd").as_deref(), Some("DVOL-ETH"));
        assert_eq!(dvol_symbol("btc"), None); // no `_usd` suffix
        assert_eq!(dvol_symbol(""), None);
    }

    #[test]
    fn dvol_tick_maps_to_a_trade_row() {
        let row =
            dvol_trade_tick(&DvolTick { index_name: "btc_usd".into(), ts: 123, volatility: 55.5 });
        assert_eq!(row.ts, 123);
        assert_eq!(row.local_ts, 123);
        assert_eq!(row.price.to_bits(), 55.5f64.to_bits());
        assert_eq!(row.size.to_bits(), 0.0f64.to_bits());
        assert!(!row.is_buyer_maker);
    }

    #[test]
    fn on_frame_emits_mark_tick_and_records_when_enabled() {
        let sink = RecordingSink::default();
        let store = Arc::new(CaptureStore::default());
        let rec = DvolRecorder::new(store.clone(), true);

        let txt = dvol_frame("btc_usd", 1_700_000_000_000, 62.5).to_string();
        assert_eq!(on_dvol_frame(&txt, &sink, Some(&rec)), FrameOutcome::Confirm);

        // live seam: exactly one mark tick under the synthetic DVOL-BTC symbol
        assert_eq!(
            recorded_marks(&sink),
            vec![("deribit".into(), "DVOL-BTC".into(), 62.5f64.to_bits(), 1_700_000_000_000)]
        );
        // store: one trade row, the DVOL value carried as price
        let got = store.scan_trades("deribit", "DVOL-BTC", TsRange::all()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].price.to_bits(), 62.5f64.to_bits());
        assert_eq!(got[0].ts, 1_700_000_000_000);
        assert_eq!(got[0].size.to_bits(), 0.0f64.to_bits());
    }

    #[test]
    fn on_frame_emits_mark_tick_but_records_nothing_when_recorder_disabled() {
        let sink = RecordingSink::default();
        let store = Arc::new(CaptureStore::default());
        let rec = DvolRecorder::new(store.clone(), false);

        let txt = dvol_frame("eth_usd", 1_700_000_000_000, 70.0).to_string();
        assert_eq!(on_dvol_frame(&txt, &sink, Some(&rec)), FrameOutcome::Confirm);

        // the live-seam mark tick STILL flows (recording is the only opt-in half)
        assert_eq!(
            recorded_marks(&sink),
            vec![("deribit".into(), "DVOL-ETH".into(), 70.0f64.to_bits(), 1_700_000_000_000)]
        );
        // recording OFF ⇒ the store is untouched (byte-identical to no recorder)
        assert!(store.scan_trades("deribit", "DVOL-ETH", TsRange::all()).unwrap().is_empty());
    }

    #[test]
    fn on_frame_records_nothing_with_no_recorder() {
        let sink = RecordingSink::default();
        // None recorder: mark tick flows, nothing recorded (the default / unwired shape).
        assert_eq!(
            on_dvol_frame(&dvol_frame("btc_usd", 1, 60.0).to_string(), &sink, None),
            FrameOutcome::Confirm
        );
        assert_eq!(recorded_marks(&sink).len(), 1);
    }

    #[test]
    fn on_frame_ignores_junk_and_non_dvol_frames() {
        let sink = RecordingSink::default();
        assert_eq!(on_dvol_frame("not json", &sink, None), FrameOutcome::Ignore);
        assert_eq!(on_dvol_frame(r#"{"id":1,"result":["ok"]}"#, &sink, None), FrameOutcome::Ignore);
        assert!(recorded_marks(&sink).is_empty(), "an ignored frame emits nothing");
    }

    #[test]
    fn record_same_bucket_twice_is_one_row_next_bucket_lands() {
        let store = Arc::new(CaptureStore::default());
        let rec = DvolRecorder::new(store.clone(), true); // default 60 s cadence
        let t0: i64 = 60_000 * 28_000_000; // a minute-bucket boundary
        let tick =
            |ts: i64, vol: f64| DvolTick { index_name: "btc_usd".into(), ts, volatility: vol };

        rec.record("DVOL-BTC", &tick(t0, 60.0));
        rec.record("DVOL-BTC", &tick(t0 + 30_000, 61.0)); // same minute bucket → dropped
        assert_eq!(store.scan_trades("deribit", "DVOL-BTC", TsRange::all()).unwrap().len(), 1);

        rec.record("DVOL-BTC", &tick(t0 + 60_000, 62.0)); // next minute → a second sample lands
        assert_eq!(store.scan_trades("deribit", "DVOL-BTC", TsRange::all()).unwrap().len(), 2);
    }

    #[test]
    fn disabled_record_writes_nothing() {
        let store = Arc::new(CaptureStore::default());
        let rec = DvolRecorder::new(store.clone(), false);
        rec.record("DVOL-BTC", &DvolTick { index_name: "btc_usd".into(), ts: 1, volatility: 60.0 });
        assert!(store.scan_trades("deribit", "DVOL-BTC", TsRange::all()).unwrap().is_empty());
    }

    /// The FLAG gate + the cadence parse, driven over an injected map — nothing here touches the
    /// process env (`std::env::set_var` is an `unsafe fn` since edition 2024, which this workspace
    /// forbids), so no scheduling of sibling tests can race it either.
    #[test]
    fn from_flag_gate_and_cadence() {
        let store = Arc::new(CaptureStore::default());
        let vars = |v: Option<&str>| -> std::collections::HashMap<String, String> {
            v.map(|v| (RECORD_DVOL_CADENCE_ENV.to_string(), v.to_string())).into_iter().collect()
        };
        let rec = |enabled: bool, v: Option<&str>| {
            DvolRecorder::from_flag_and_vars(store.clone(), enabled, &vars(v))
        };

        // The gate is the PARAMETER, and nothing else: no environment state can flip it, which is
        // the whole point of taking `vike_config::Flags::record_dvol` instead of re-reading
        // `VIKE_RECORD_DVOL` here.
        assert!(!rec(false, None).enabled(), "flag off → disabled");
        assert!(rec(true, None).enabled(), "flag on → enabled");

        // cadence parse/clamp (read independently of the enable gate)
        assert_eq!(rec(true, None).cadence_ms(), DEFAULT_DVOL_CADENCE_MS);
        assert_eq!(rec(true, Some("5000")).cadence_ms(), 5_000);
        assert_eq!(rec(true, Some("0")).cadence_ms(), 1, "0 clamps to 1 ms");
        assert_eq!(
            rec(true, Some("not-a-number")).cadence_ms(),
            DEFAULT_DVOL_CADENCE_MS,
            "unparsable falls back to the default"
        );
        // A disabled recorder still resolves its cadence — the two are independent reads.
        assert_eq!(rec(false, Some("5000")).cadence_ms(), 5_000);
    }

    #[test]
    fn public_subscribe_frame_shape() {
        let sub = build_public_subscribe(&[dvol_channel("btc"), dvol_channel("eth")]);
        let v: Value = serde_json::from_str(&sub).unwrap();
        assert_eq!(v.get("method").and_then(|m| m.as_str()), Some("public/subscribe"));
        let channels = v.pointer("/params/channels").and_then(|c| c.as_array()).unwrap();
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].as_str(), Some("deribit_volatility_index.btc_usd"));
        assert_eq!(channels[1].as_str(), Some("deribit_volatility_index.eth_usd"));
    }
}

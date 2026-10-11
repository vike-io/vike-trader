//! Live Bybit V5 **spot** kline market data → the vike-data live seam — the Bybit twin of
//! binance's `market_feed` module (the kline feed implementing [`vike_data::DataClient`], as
//! distinct from [`crate::market_data`], the L2/tick HFT feed).
//!
//! Same shape as the Binance feed: one STOPPABLE thread per (symbol, interval) — REST warmup seed,
//! then the public `kline.<code>.<SYMBOL>` WS. Data flows through the [`vike_data::LiveDataSink`]
//! handed to [`Feeds::new`] at construction: closed bars via `close_bar` (the lossless lane),
//! intrabar forming updates + last-price ticks via `forming_bar`/`bar_close_tick` (the wait-free
//! conflating lane; a `.P` perp bars subscription ALSO opens the linear `tickers` stream feeding
//! the REAL-mark verb `mark_tick`, default ON, the venue's `mark_streams` row via
//! [`Feeds::with_mark_streams`]). Threads poll a
//! PER-SUBSCRIPTION stop flag on a socket read timeout, so
//! [`Feeds::unsubscribe`]/[`Feeds::shutdown`] (via `impl DataClient for Feeds`) stop+join
//! deterministically (the teardown gate).
//!
//! **Dedup A6 (wave 3):** the WS session LIFECYCLE (connect + subscribe replay, subscribe-ack
//! watchdog, read-timeout stop poll, stop-aware 30×100 ms reconnect backoff) now rides the shared
//! [`vike_bridge_core::market_pump`] driver, and the per-subscription stop/join bookkeeping rides
//! [`vike_data::FeedRegistry`] — behavior-identical to the pre-driver copy (bybit is the wave-3
//! proof venue; binance/okx/hyperliquid/polymarket follow in later PRs). Only the PROTOCOL stays
//! here: the pure frame classifiers ([`route_frame`]/`route_trades_frame`), the `.P` perp split +
//! the three-way host pick, the REST warmup seed, and the sink emission.
//!
//! ## ⚠ THE HOST IS A ROUTING DECISION, AND IT IS THE SAME ONE THE HISTORY PATH MAKES
//!
//! `docs/decisions/0061-an-instrument-names-its-kind.md` phase 1 PINNED a contradiction inside this
//! one crate: `crates/bridges/bybit/src/data.rs`'s `route_target` REFUSED a bare symbol the venue
//! lists under both spot and inverse, while every lane in THIS file read the same string through a
//! `bool` (`.P` or not) and reached a host by construction. Phase 4 — this change — ends it.
//! [`feed_route`] IS `data.rs`'s `range_target`, so a symbol the store refuses to write is a symbol
//! this refuses to stream, and [`ws_host`] turns the resolved `Category` into one of the venue's
//! THREE public sockets.
//!
//! Two consequences worth carrying before editing a lane here:
//!
//!   * **The socket is STRICT where the REST endpoint is lenient.** MEASURED 2026-09-16 — see
//!     [`PUBLIC_WS_INVERSE`]'s table for the probes. Every wrong-host lane is a `handler not found`
//!     reconnect loop, except DEPTH, where it is silent.
//!   * **[`depth_main`]'s route CHANGED user-visibly** (a bare symbol's DOM moves from the perp book
//!     to the spot book). Its own ⚠ carries what that was and why.
//!
//! **Closed-bar detection: the `confirm` flag.** A Bybit kline datum carries `confirm` — `true` once
//! the candle is FINAL, `false` while it is still forming. This is the exact analogue of Binance's
//! kline `x`: `confirm=true` → emit a CLOSED bar via `close_bar`; `confirm=false` → a forming bar
//! via `forming_bar`.
//!
//! Endpoint: `wss://stream.bybit.com/v5/public/spot`. Subscribe:
//! `{"op":"subscribe","args":["kline.<code>.<SYMBOL>"]}` where `<code>` is Bybit's interval code
//! ("1","60","D",…) from [`super::data::interval_code`] — the SAME map the REST kline fetcher uses.
//! A kline datum is a JSON OBJECT `{start,open,high,low,close,volume,confirm,…}` (named fields, so
//! JSON order is irrelevant); `start` is the bar-open ms.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::market_pump::{
    FrameOutcome, MarketPumpOpts, MarketStream, SessionStatus, connect_market_stream,
    run_market_feed, run_market_feed_on,
};
use vike_bridge_core::pump_spec::subscribe_only_pump_opts;
use vike_data::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId, require_live_verb,
};
use vike_model::{Bar, LiveVerb, TradeTick};

use super::data::{Category, fetch_klines_latest, interval_code};

const VENUE: &str = "bybit";
const READ_TIMEOUT: Duration = Duration::from_secs(2); // depth driver's stop-flag poll cadence
const SEED_LIMIT: usize = 1000; // newest N klines for the REST warmup (Bybit's per-request cap)
/// Bybit V5 public **spot** stream (matches `data::CATEGORY = "spot"`, the kline category served).
pub const PUBLIC_WS_SPOT: &str = "wss://stream.bybit.com/v5/public/spot";
/// Bybit V5 public **linear-perp** stream (`BTCUSDT` linear — what `BybitPerpRest` trades). Same
/// `orderbook.200.<SYM>`/`kline.<code>.<SYM>` topic shapes as spot; only the stream URL differs. A
/// symbol the venue lists on `category=linear` routes its kline and depth WS here, never by a `.P`
/// suffix alone — see [`feed_route`] and [`ws_host`].
pub const PUBLIC_WS_LINEAR: &str = "wss://stream.bybit.com/v5/public/linear";
/// Bybit V5 public **inverse-perp** (coin-settled) stream.
///
/// ⚠ **This host is the reason the live feed had to change at all, and it is the half no prose in
/// this crate had measured.** `/v5/market/kline` resolves `category` LENIENTLY — MEASURED
/// 2026-09-16, `category=linear&symbol=BTCUSD` returns the inverse tape byte-identically — so the
/// REST side was quietly correct. **The socket is STRICT, in both directions**, MEASURED the same
/// day against the real hosts with the real topics this file builds:
///
/// | host | topic | answer |
/// |---|---|---|
/// | `…/public/linear` | `orderbook.200.BTCUSD`, `orderbook.50.BTCUSD`, `kline.1.BTCUSD`, `publicTrade.BTCUSD`, `tickers.BTCUSD` | `success=false`, `error:handler not found` |
/// | `…/public/inverse` | all five of the above | `type=snapshot` DATA |
/// | `…/public/inverse` | `kline.1.BTCUSDT` | `success=false`, `error:handler not found` |
///
/// So an inverse symbol on [`PUBLIC_WS_LINEAR`] is a `FrameOutcome::Fatal` and a reconnect loop on
/// the kline/trades/mark lanes — and on the DEPTH lane it is SILENT: a `success=false` ack folds to
/// `BookOp::Ignored`, the book never seeds, and the driver's idle watchdog reconnects forever with
/// no error text anywhere. That is why widening the catalog without this const would have been worse
/// than leaving the instruments unreachable.
pub const PUBLIC_WS_INVERSE: &str = "wss://stream.bybit.com/v5/public/inverse";

/// **The ONE host decision**, and the twin of `crates/bridges/bybit/src/data.rs`'s `rest_category`:
/// the same [`Category`] value picks the REST category and this socket, so a lane's warmup seed and
/// its stream can never reach different books.
///
/// Spelled as a `match` with no catch-all on purpose — a fourth category (bybit serves `option`
/// too) must fail to COMPILE here rather than default onto one of these three.
fn ws_host(book: Category) -> &'static str {
    match book {
        Category::Spot => PUBLIC_WS_SPOT,
        Category::Linear => PUBLIC_WS_LINEAR,
        Category::Inverse => PUBLIC_WS_INVERSE,
    }
}

/// Test-only reach into [`ws_host`] from a SIBLING module — see
/// `crates/bridges/bybit/src/data.rs`'s `route_for_test` for why the catalog's routing gate calls
/// the real chooser rather than restating its table.
#[cfg(test)]
pub(crate) fn ws_host_for_test(book: Category) -> &'static str {
    ws_host(book)
}

/// Split an incoming feed symbol into `(api_symbol, is_perp)`: a trailing `.P` marks a perpetual
/// (the catalog's distinct-symbol tag, matching `BYBIT:BTCUSDT.P`) and is STRIPPED to the exchange
/// symbol that drives the WS subscribe topic + REST seed, while the ORIGINAL `.P`-suffixed symbol
/// stays the sink/core series label (so a perp's series key never collides with its spot twin). An
/// owning wrapper over [`vike_catalog::split_perp_at`], which is where that split is defined for
/// every venue.
///
/// ⚠ **It answers "is this a perpetual", NOT "which book".** That was the whole defect: every lane
/// in this file read this one `bool` as a host, so `.P` MEANT linear. [`feed_route`] is the function
/// that answers the host question now, and this one survives only where a plain perp/not-perp
/// predicate is genuinely what is wanted ([`should_pair_mark`] — spot has no mark price, and that is
/// true of both derivative books).
fn perp_split(symbol: &str) -> (String, bool) {
    let (api, perp) = vike_catalog::split_perp_at(VENUE, symbol);
    (api.to_string(), perp)
}

/// **Resolve a feed symbol to `(api_symbol, category)`** — the live twin of
/// `crates/bridges/bybit/src/data.rs`'s `range_target`, and the end of the contradiction this file
/// used to PIN.
///
/// `docs/decisions/0061-an-instrument-names-its-kind.md` phase 1 recorded that *"bybit's own crate
/// reads one string two ways … Two live, opposite readings, one venue, one crate"* and said to PIN
/// it rather than fix it, because changing a live subscription's route was phase 4. This is phase 4:
/// the history path and the live path now call the same `route_target` through the same venue
/// listings, so a symbol that the store refuses to write is a symbol this refuses to stream.
///
/// ⚠ **It can REFUSE, and a caller must surface the refusal rather than fall back.** A bare symbol
/// the venue lists under both spot and inverse has no route (the collision refusal), and a listing
/// read that FAILED is not permission to guess. Every lane below turns an `Err` into the
/// subscription's status string, which is what an operator sees.
///
/// Network I/O on the first call per process only — the derivation is cached in
/// [`crate::instruments`], and a plain spot symbol never reaches it.
fn feed_route(symbol: &str) -> Result<(String, Category), String> {
    let (wire, book) = crate::data::live_route(symbol)?;
    Ok((wire.to_string(), book))
}

/// One Bybit `kline` datum. Named fields, so the wire order (`open,close,high,low`) is irrelevant;
/// `start` is the bar-open ms and `confirm` is Bybit's CLOSED marker. o/h/l/c/v are decimal strings
/// preserved to their exact f64 bits on parse.
#[derive(Deserialize)]
struct KlineData {
    start: i64,
    open: String,
    high: String,
    low: String,
    close: String,
    volume: String,
    confirm: bool,
}

#[derive(Deserialize)]
struct KlineMsg {
    topic: String,
    data: Vec<KlineData>,
}

/// What one decoded Bybit kline frame becomes — the `route_frame` classification (the kline analogue
/// of [`crate::market_data::MdEvent`]).
#[derive(Debug, Clone, PartialEq)]
pub enum KlineEvent {
    /// A CLOSED bar (`confirm=true`) — the lossless `close_bar` sink lane.
    Closed(Bar),
    /// A still-forming bar (`confirm=false`) — the conflating `forming_bar` sink lane.
    Forming(Bar),
    /// The venue's subscribe ACK (`{op:"subscribe", success:true}`) — no data, but it CONFIRMS the
    /// subscribe handshake (net-hardening br7): [`feed_main`] maps it to [`FrameOutcome::Confirm`],
    /// disarming the shared driver's subscribe-ack watchdog.
    Ack,
    /// The venue REJECTED the subscribe (`{op:"subscribe", success:false}`) — an ATTRIBUTABLE error
    /// carrying Bybit's `ret_msg`, surfaced instead of silently dropped (net-hardening br7).
    Error(String),
    /// A non-kline frame (pong, unknown topic, other op) — dropped.
    Ignored,
}

/// One kline datum → a [`Bar`] via the shared binance `kline_to_bar`, so bars are the SAME shape
/// across venues. `None` if any required price string fails to parse; volume defaults to 0.0.
fn data_to_bar(k: &KlineData) -> Option<Bar> {
    Some(kline_to_bar(
        k.start,
        k.open.parse::<f64>().ok()?,
        k.high.parse::<f64>().ok()?,
        k.low.parse::<f64>().ok()?,
        k.close.parse::<f64>().ok()?,
        k.volume.parse::<f64>().unwrap_or(0.0),
    ))
}

/// Route ONE Bybit WS text frame → a classified kline event. Pure — testable without a socket. The
/// subscribe OUTCOME envelope is classified FIRST (net-hardening br7): `{op:"subscribe", success}` →
/// [`KlineEvent::Ack`] (success) or an attributable [`KlineEvent::Error`] carrying `ret_msg` (reject,
/// was silently dropped). Otherwise a `kline.*` frame's first datum maps to a [`Bar`]; `confirm=true`
/// → [`KlineEvent::Closed`], else [`KlineEvent::Forming`]. Pongs / non-kline topics → Ignored.
pub fn route_frame(text: &str) -> KlineEvent {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return KlineEvent::Ignored; // bare-text "ping"/"pong" keepalive is not JSON
    };
    // The subscribe reply carries an `op` — `"subscribe"` is the ack/reject we must attribute; any
    // other op (a `"ping"`/`"pong"` keepalive reply) is ignored. Caught here because these envelopes
    // lack the `{topic,data}` kline shape and were otherwise silently dropped by the kline parse below.
    if let Some(op) = v.get("op").and_then(serde_json::Value::as_str) {
        if op == "subscribe" {
            return if v.get("success").and_then(serde_json::Value::as_bool).unwrap_or(false) {
                KlineEvent::Ack
            } else {
                let msg =
                    v.get("ret_msg").and_then(serde_json::Value::as_str).unwrap_or("(no message)");
                KlineEvent::Error(format!("Bybit subscribe rejected: {msg}"))
            };
        }
        return KlineEvent::Ignored; // pong / other ops
    }
    // A kline data frame: {topic, data:[kline]} (the non-`op` JSON path).
    let Ok(msg) = serde_json::from_value::<KlineMsg>(v) else {
        return KlineEvent::Ignored;
    };
    if !msg.topic.starts_with("kline.") {
        return KlineEvent::Ignored;
    }
    let Some(k) = msg.data.first() else {
        return KlineEvent::Ignored;
    };
    let Some(bar) = data_to_bar(k) else {
        return KlineEvent::Ignored;
    };
    if k.confirm { KlineEvent::Closed(bar) } else { KlineEvent::Forming(bar) }
}

/// The subscribe frame: the kline stream `kline.<code>.<SYMBOL>` (`code` = Bybit's interval code).
pub fn subscribe_frame(code: &str, symbol: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [format!("kline.{code}.{symbol}")]
    })
    .to_string()
}

/// The per-subscription wiring every Bybit feed-thread body receives.
///
/// ⚠ **`pub` so a scripted test can build one**, which is the whole point of
/// [`feed_body`]: the latch this type's `status` field carried on the CI box for 42 hours was
/// invisible to every test in this crate because nothing outside the module could construct the
/// context the real closures write into. Same shape and same reason as
/// `crates/bridges/binance/src/family/market_feed.rs`'s `FeedCtx`, which has been `pub` since its
/// own journal test needed one.
pub struct FeedCtx {
    pub sink: Arc<dyn LiveDataSink>,
    pub status: Arc<Mutex<String>>,
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub stop: Arc<AtomicBool>,
}

/// The prefix every HEALTHY status string this venue publishes starts with. Named rather than
/// spelled at each branch because [`FeedCtx::set_status`] classifies a transition's journal LEVEL
/// on it — a call site inventing a different healthy spelling would log a recovery as a warning,
/// and this is the one place to look for why. Adopted verbatim from
/// `crates/bridges/binance/src/family/market_feed.rs`'s `HEALTHY_STATUS_PREFIX`.
const HEALTHY_STATUS_PREFIX: &str = "LIVE ·";

/// The ONE healthy status text every Bybit lane publishes — kline, mark and trades alike.
///
/// ⚠ **One string per VENUE, not per lane, and that is load-bearing rather than tidy.** All three
/// lanes share ONE `Arc<Mutex<String>>` (`Feeds::spawn_with` clones the one handle into every
/// `FeedCtx`), and [`FeedCtx::set_status`] emits only on a text TRANSITION. Per-lane healthy
/// spellings would therefore turn every session boundary on either lane into a transition against
/// the OTHER lane's text — a journal line per reconnect per lane, in a file layer defaulting to
/// `trace` (the 341 GB shape root `CLAUDE.md` records). Identical text lets the dedup absorb
/// multi-lane alternation entirely. `the_three_lanes_publish_one_healthy_string` is the gate.
pub const LIVE_STATUS: &str = "LIVE · Bybit";

impl FeedCtx {
    /// Publish this thread's human-readable status AND — on a TRANSITION only — say it in the
    /// journal.
    ///
    /// ⚠ **The journal half is not a nicety here, it is what made the the CI box incident
    /// undiagnosable.** This method used to write the mutex and emit nothing at all, and the
    /// mutex is a GUI channel: the daemon that suppressed bybit's reconcile leg 2,516 times holds
    /// no reader for it, so the latched string could not be read from the box at all — the
    /// diagnosis had to reason about which of two candidate literals it must have been. Adopted
    /// wholesale from `crates/bridges/binance/src/family/market_feed.rs`'s `FeedCtx::set_status`,
    /// including its two rules: emit on a TEXT CHANGE only (these strings are produced on a
    /// reconnect loop, and a line per cycle is the 341 GB shape), and split the LEVEL on
    /// [`HEALTHY_STATUS_PREFIX`] so `journalctl -p warning` shows a feed that stopped working
    /// while a recovery lands at `info!`.
    ///
    /// It became REQUIRED with the success disclosure rather than merely desirable: the first
    /// `SessionStatus::Live` of a session now OVERWRITES a `"{key} seed error: {e}"`, and
    /// `feed_main` seeds once per FEED — nothing re-attempts it — so without this a failed REST
    /// warmup would leave no trace anywhere at all.
    pub fn set_status(&self, s: String) {
        let changed = {
            let mut cur = self.status.lock().unwrap();
            let changed = *cur != s;
            if changed {
                cur.clone_from(&s);
            }
            changed
        };
        if changed {
            if s.starts_with(HEALTHY_STATUS_PREFIX) {
                tracing::info!(target: "vike_bybit::market_feed", venue = VENUE, status = %s, "feed status");
            } else {
                tracing::warn!(target: "vike_bybit::market_feed", venue = VENUE, status = %s, "feed status");
            }
        }
        (self.wake)();
    }
}

/// `series_symbol` is the catalog/sink label (`.P`-suffixed for a perp — the core key); `api_symbol`
/// is the `.P`-stripped exchange symbol used for the REST seed + WS subscribe topic (IDENTICAL shape
/// on all three categories — Bybit does not encode category in the topic, only the socket URL). Spot
/// subscriptions pass `series_symbol == api_symbol` and `Category::Spot`, so this is byte-identical
/// to the pre-perp behavior for spot. `book` picks BOTH the seed's REST category and the socket
/// ([`ws_host`]) — one value, so the warmup and the stream cannot reach different books.
///
/// The WS session/reconnect lifecycle rides the shared [`run_market_feed`] driver (dedup A6);
/// each decoded frame folds through the fixture-tested [`route_frame`] inside the `on_text`
/// closure — a data frame emits under the SERIES symbol (last price → the conflated mark cache,
/// then the lossless `close_bar` or conflating `forming_bar` lane) and confirms the br7 subscribe
/// handshake; an ack confirms it dataless; a venue REJECT is surfaced attributably
/// ([`FrameOutcome::Fatal`], never silently dropped).
fn feed_main(series_symbol: String, interval: String, ctx: FeedCtx) {
    let key = format!("{series_symbol}@{interval}");
    // Interval code up-front: an unsupported interval is a permanent config error, so report it and
    // stop (no reconnect could ever fix it).
    let code = match interval_code(&interval) {
        Ok(c) => c.to_string(),
        Err(e) => {
            ctx.set_status(format!("{key}: {e}"));
            return;
        }
    };
    // ...and the ROUTE, on the same terms and for the same reason: a symbol whose book cannot be
    // decided is a permanent config error on this subscription, not something a reconnect fixes. It
    // is resolved HERE rather than at the spawn site deliberately — the first call per process
    // reads the venue's instrument listings, and doing that on the caller's thread would stall the
    // picker; this thread already blocks on a REST seed three lines down. The refusal lands in the
    // status string, which is what an operator actually sees.
    let (api_symbol, book) = match feed_route(&series_symbol) {
        Ok(v) => v,
        Err(e) => {
            ctx.set_status(format!("{key}: {e}"));
            return;
        }
    };
    // REST warmup: newest N klines; Bybit serves the in-progress candle as the last one — seed the
    // closed prefix, route the forming tail through the market lane (mirrors binance).
    //
    // ⚠ **"Once per feed, NOT per session" is the rule for the SEED, and it used to be the rule for
    // the STATUS too.** That second half was the 42-hour the CI box latch: the healthy write below fired
    // once, here, and the pump's only other writer was the session-error hook — so one blip wrote a
    // `Degraded`-reading string and no later session could rewrite it, suppressing this venue's
    // reconcile leg 2,516 times while four sockets stayed ESTABLISHED. The seed rule is unchanged
    // (a reconnect must not re-seed — the pre-driver behaviour, and re-seeding would double-emit
    // closed bars); the STATUS is now disclosed per SESSION by `feed_body`'s `SessionStatus::Live`
    // arm, and the write here covers only the window before the first frame arrives.
    match fetch_klines_latest(&api_symbol, &interval, SEED_LIMIT, book) {
        Ok(mut bars) => {
            let forming = if bars.len() > 1 { bars.pop() } else { None };
            ctx.sink.seed_bars(VENUE, &series_symbol, &interval, bars);
            if let Some(f) = forming {
                ctx.sink.bar_close_tick(VENUE, &series_symbol, f.close, f.ts);
                ctx.sink.forming_bar(VENUE, &series_symbol, &interval, f);
            }
            ctx.set_status(LIVE_STATUS.into());
        }
        Err(e) => ctx.set_status(format!("{key} seed error: {e}")),
    }
    let host = ws_host(book);
    let sub = subscribe_frame(&code, &api_symbol);
    let opts = subscribe_only_pump_opts(VENUE, &sub);
    feed_body(&series_symbol, &interval, &opts, &ctx, || {
        connect_market_stream(host, opts.read_timeout, opts.connect_timeout)
    });
}

/// [`feed_main`]'s SESSION half: everything from the dial onwards, with the dial itself handed to
/// the caller.
///
/// ⚠ **The split exists so the reconnect path can be TESTED**, and the defect it was cut for is
/// why that matters. Bybit's kline lane wrote its healthy status exactly once, before the pump
/// ever ran, and the pump's only status writer was the session-ERROR hook — so one transient blip
/// latched a `Degraded`-reading string that no later successful session ever rewrote. On the CI box
/// that suppressed the venue's reconcile leg once a minute for 42 hours while four sockets stayed
/// ESTABLISHED and ~2,100 events a minute flowed in. Nothing in this crate could reproduce it,
/// because [`feed_main`] dials a real socket and the closures that own the status string were
/// unreachable without one. `tests/offline/market_feed_scripted.rs` drives THESE closures — the
/// real [`route_frame`], the real hooks, a real [`FeedCtx`] — over a scripted stream pair.
///
/// Same shape as `crates/bridges/binance/src/family/trades.rs`'s `run_trades_feed`, which has
/// taken its connect closure since it was written; this is that seam applied one lane over rather
/// than a new idea.
pub fn feed_body<S: MarketStream>(
    series_symbol: &str,
    interval: &str,
    opts: &MarketPumpOpts<'_>,
    ctx: &FeedCtx,
    connect: impl FnMut() -> Result<S, String>,
) {
    let key = format!("{series_symbol}@{interval}");
    run_market_feed_on(
        connect,
        opts,
        &ctx.stop,
        &now_ms,
        |txt| {
            let (bar, confirmed) = match route_frame(txt) {
                KlineEvent::Closed(bar) => (bar, true),
                KlineEvent::Forming(bar) => (bar, false),
                // Subscribe ACK: no data, but it confirms the handshake — disarms the watchdog.
                KlineEvent::Ack => return FrameOutcome::Confirm,
                // Venue REJECTED the subscribe — the attributable session error (was dropped).
                KlineEvent::Error(msg) => return FrameOutcome::Fatal(msg),
                KlineEvent::Ignored => return FrameOutcome::Ignore,
            };
            // last price -> the core's bar-close cache (conflated), same as binance — under the
            // SERIES symbol so it lands on the same key the chart/core reads; data also confirms
            // br7. `bar_close_tick`, NOT `mark_tick`: a candle close is not the venue mark
            // (mark-slot semantics; the real mark rides the perp `tickers` pump, `mark_main`).
            ctx.sink.bar_close_tick(VENUE, series_symbol, bar.close, bar.ts);
            if confirmed {
                // bar CLOSED — lossless lane (a missed close = a series hole)
                ctx.sink.close_bar(VENUE, series_symbol, interval, bar);
            } else {
                ctx.sink.forming_bar(VENUE, series_symbol, interval, bar);
            }
            (ctx.wake)();
            FrameOutcome::Confirm
        },
        || {}, // no dataless-tick judgment on this lane (the polymarket freshness knob)
        |s| match s {
            // THE FIX. The seed-time write above is the first session's healthy state before any
            // frame arrives; this is every LATER session's, and its absence is the 42-hour latch.
            // Identical text, so `set_status`'s dedup emits nothing on the common repeat — the
            // write is free until it actually says something new.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{key} ws error (reconnecting): {e}"))
            }
        },
    );
}

// --- Perp mark-price feed (mark-slot semantics, W2-T4) ---------------------------------------
// Bybit V5 `tickers.<SYMBOL>` on the LINEAR stream: the venue's REAL `markPrice` (the price its
// liquidation/funding engine keys off), pushed as a `snapshot` then field-sparse `delta`s — a
// delta that doesn't move the mark simply omits `markPrice` and is ignored here. Feeds
// `LiveDataSink::mark_tick` (the `PriceBoard` MARK slot); the kline pump's candle closes ride
// `bar_close_tick`. Perp-only: opened automatically next to a `.P` bars subscription
// (a `mark_streams = 0` row disables). Same subscribe/ack grammar as the kline feed.

/// What one decoded `tickers.*` frame becomes — [`route_frame`]'s mark-pump twin.
#[derive(Debug, Clone, PartialEq)]
pub enum MarkEvent {
    /// A frame carrying a fresh `markPrice` — `px` + the frame's top-level `ts` (epoch-ms).
    Mark { px: f64, ts: i64 },
    /// The venue's subscribe ACK (`{op:"subscribe", success:true}`) — confirms the br7 handshake.
    Ack,
    /// The venue REJECTED the subscribe — attributable, carries Bybit's `ret_msg`.
    Error(String),
    /// A non-tickers frame, a `markPrice`-less delta, or a dead (0/neg/NaN) mark — dropped.
    Ignored,
}

/// Route ONE Bybit WS text frame from the `tickers` channel → a classified mark event. Pure —
/// fixture-tested without a socket. The subscribe OUTCOME envelope is classified first (the same
/// br7 shape as [`route_frame`]); then a `tickers.*` data push yields `data.markPrice` (a decimal
/// string; ABSENT in a delta that didn't move the mark → Ignored) stamped with the frame's
/// top-level `ts`.
pub fn route_mark_frame(text: &str) -> MarkEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return MarkEvent::Ignored; // bare-text keepalive is not JSON
    };
    if let Some(op) = v.get("op").and_then(Value::as_str) {
        if op == "subscribe" {
            return if v.get("success").and_then(Value::as_bool).unwrap_or(false) {
                MarkEvent::Ack
            } else {
                let msg = v.get("ret_msg").and_then(Value::as_str).unwrap_or("(no message)");
                MarkEvent::Error(format!("Bybit subscribe rejected: {msg}"))
            };
        }
        return MarkEvent::Ignored; // pong / other ops
    }
    let topic = v.get("topic").and_then(Value::as_str);
    if !topic.is_some_and(|t| t.starts_with("tickers.")) {
        return MarkEvent::Ignored;
    }
    let Some(px) = v
        .get("data")
        .and_then(|d| d.get("markPrice"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
    else {
        return MarkEvent::Ignored; // a delta without a mark move
    };
    if !vike_bridge_core::is_valid_mark(px) {
        return MarkEvent::Ignored;
    }
    let ts = v.get("ts").and_then(Value::as_i64).unwrap_or(0);
    MarkEvent::Mark { px, ts }
}

/// The subscribe frame for the `tickers` channel on `symbol` — same `{"op":"subscribe",...}`
/// shape as [`subscribe_frame`] (klines), no interval code.
pub fn mark_subscribe_frame(symbol: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [format!("tickers.{symbol}")]
    })
    .to_string()
}

/// The perp mark-price feed-thread body — [`feed_main`]'s mark twin on the same shared driver, on
/// whichever derivative host the symbol's book names (spot has no mark price and never spawns one —
/// [`should_pair_mark`]). Emits under the `.P` SERIES symbol. No REST seed — a live-only valuation
/// stream; the resolver's bar-close fallback covers the pre-first-frame window.
///
/// ⚠ It said "always on [`PUBLIC_WS_LINEAR`]" and that was a third silent wrong-host site:
/// MEASURED 2026-09-16, `tickers.BTCUSD` on the linear host answers `error:handler not found`, so
/// an inverse perp's mark lane was a permanent reconnect loop feeding a valuation rung that never
/// filled.
fn mark_main(series_symbol: String, ctx: FeedCtx) {
    let (api_symbol, book) = match feed_route(&series_symbol) {
        Ok(v) => v,
        Err(e) => {
            ctx.set_status(format!("{series_symbol} mark: {e}"));
            return;
        }
    };
    let sub = mark_subscribe_frame(&api_symbol);
    run_market_feed(
        ws_host(book),
        &subscribe_only_pump_opts(VENUE, &sub),
        &ctx.stop,
        &now_ms,
        |txt| match route_mark_frame(txt) {
            MarkEvent::Mark { px, ts } => {
                ctx.sink.mark_tick(VENUE, &series_symbol, px, ts);
                (ctx.wake)();
                FrameOutcome::Confirm
            }
            MarkEvent::Ack => FrameOutcome::Confirm,
            MarkEvent::Error(msg) => FrameOutcome::Fatal(msg),
            MarkEvent::Ignored => FrameOutcome::Ignore,
        },
        || {},
        |s| match s {
            // ⚠ This lane had NO healthy string at all before this change — it was a
            // pure-DEGRADATION writer on a mutex shared with the kline and trades lanes, i.e. a
            // lane that could push the venue to `Degraded` and never back. On a spot mount it never
            // spawns, so the property was invisible; on a `.P` mount it is exactly the latch this
            // PR cures, wearing a different lane. Giving it the arm is a CONDITION of keeping
            // bybit's `recon_feed_statuses` row, not a tidy-up — see that method's doc.
            //
            // The SAME text as the other two lanes, per [`LIVE_STATUS`]'s one-string-per-venue
            // rule: a `.P` mount runs two lanes on one mutex, and per-lane spellings would make
            // every session boundary on either lane a text TRANSITION against the other's.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{series_symbol} mark ws error (reconnecting): {e}"))
            }
        },
    );
}

// --- Trades feed (live prints) ---------------------------------------------------------------
// Bybit V5 `publicTrade.<SYMBOL>` channel: executed prints (no snapshot/seed — a live-only tape,
// unlike the `kline.*`/`orderbook.*` channels which each have a REST/WS seed counterpart). Same
// subscribe shape as the kline feed (`{"op":"subscribe",...}`) and the SAME spot-vs-linear split: a
// `.P`-suffixed perp symbol strips to the exchange symbol for the `publicTrade.<SYMBOL>` topic and
// routes to `PUBLIC_WS_LINEAR`, while spot stays on `PUBLIC_WS_SPOT` (see `perp_split`). Feeds
// `vike_model::TradeTick` through `LiveDataSink::trade`, the input the app's client-side
// tick/volume-bar aggregation reads. The Bybit twin of okx's `market_feed` trades section, and shaped
// identically to `crate::market_data::decode_trade` (the HFT-track twin), so both tracks' ticks match.
//
// **Perp re-labeling.** The linear-perp wire `s` is the plain exchange symbol (`BTCUSDT`, no `.P`),
// but the app's `TradeStore` keys the tape on `tick.symbol` (NOT the sink `symbol` arg) — so every
// parsed tick's `symbol` is overwritten to the `.P` series label (`relabel_series`) before emit, or a
// perp's prints would collide with its spot twin's key. Spot (`series_symbol == wire s`) is a no-op.
//
// Wire shape: `{"topic":"publicTrade.BTCUSDT","type":"snapshot","data":[{"T":<ms ts>,"s":"BTCUSDT",
// "S":"Buy"|"Sell","v":"<size>","p":"<price>","L":"...","i":"...","BT":false}]}` — a push may carry
// more than one print per frame. Field mapping (mirrors binance's `@aggTrade` / okx's `trades`
// mappers so every venue's ticks are shaped identically):
//
// | wire key | -> | `TradeTick` field |
// |----------|----|--------------------|
// | `p`      |    | `price` — decimal STRING -> f64 |
// | `v`      |    | `size` — decimal STRING -> f64 |
// | `T`      |    | `ts` — epoch-ms, a JSON NUMBER (unlike okx's decimal-string ts) -> i64 |
// | `S`      |    | `is_buyer_maker` — Bybit's `S` is the TAKER/aggressor side; Binance's `m` |
// |          |    | convention is `true` exactly when the BUYER was the MAKER (i.e. the |
// |          |    | taker/aggressor SOLD) — so `S=="Sell"` -> `true`, `S=="Buy"` -> `false`. |
// |          |    | (Identical to `crate::market_data::decode_trade`'s `S == "Sell"` rule.) |
// | `s`      |    | `symbol` — read off the per-row wire field. |
// | `i`/`L`  |    | (trade-id / tick-direction — not carried; `vike_model::TradeTick` has neither) |
//
// `local_ts` is stamped by the pump at receive time (0 from the pure mapper — deterministic/
// testable, same "0 = not stamped" convention the binance/okx mappers document).
//
// **Non-finite/non-positive guard:** a decoded price/size that fails to parse, is non-finite, or is
// `<= 0.0` is rejected in place (mirrors okx's/binance's `is_valid_trade` — a downstream volume fold
// would spin on `+Inf` or corrupt on a zero/garbage size).

/// The shared non-finite/non-positive guard (mirrors okx's/`vike_binance::trades`'s `is_valid_trade`).
fn is_valid_trade(price: f64, size: f64) -> bool {
    price.is_finite() && price > 0.0 && size.is_finite() && size > 0.0
}

/// One `publicTrade` channel data row -> a `TradeTick` (`local_ts` left at the "not stamped"
/// sentinel `0` — the pump stamps machine receive time just before each `sink.trade` emit, same
/// convention as the binance/okx mappers). `None` on a missing/unparseable field, a
/// non-finite/non-positive price or size, or an unrecognized `S` side.
fn row_to_trade_tick(row: &Value) -> Option<TradeTick> {
    let symbol = row.get("s").and_then(Value::as_str)?;
    let price = row.get("p").and_then(Value::as_str)?.parse::<f64>().ok()?;
    let size = row.get("v").and_then(Value::as_str)?.parse::<f64>().ok()?;
    if !is_valid_trade(price, size) {
        return None;
    }
    // Bybit sends `T` as a JSON number (epoch-ms), unlike okx's decimal-string `ts`.
    let ts = row.get("T").and_then(Value::as_i64)?;
    let is_buyer_maker = match row.get("S").and_then(Value::as_str)? {
        "Sell" => true, // taker SOLD -> the buyer was the MAKER (Binance `m` convention)
        "Buy" => false,
        _ => return None,
    };
    Some(TradeTick { ts, local_ts: 0, price, size, is_buyer_maker, symbol: symbol.to_string() })
}

/// PURE: one Bybit `publicTrade` channel WS push -> zero or more `TradeTick`s (fixture-tested, no
/// socket). Anything that is not a `publicTrade`-topic data push — another topic, a subscribe
/// ack/reject envelope (which carries `op`, not `topic`), or a frame with no/empty `data` array —
/// yields an empty vec; a malformed/unparseable individual row is skipped in place rather than
/// failing the whole push (mirrors okx's `parse_trades` per-element tolerance).
pub fn parse_trades(payload: &Value) -> Vec<TradeTick> {
    let topic = payload.get("topic").and_then(Value::as_str);
    if !topic.is_some_and(|t| t.starts_with("publicTrade")) {
        return Vec::new();
    }
    let Some(rows) = payload.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter().filter_map(row_to_trade_tick).collect()
}

/// Re-label every parsed tick's `symbol` to the sink/core `series_symbol` (the `.P` key for a perp).
/// The linear-perp wire `s` is the plain exchange symbol (`BTCUSDT`, no `.P`), but the app's
/// `TradeStore` keys the tape on `tick.symbol` — so a perp's prints must carry the `.P` series key or
/// they collide with the spot twin. For SPOT `series_symbol == wire s`, so this is byte-identical (a
/// no-op overwrite of the same string). Mirrors the kline feed's "emit under `series_symbol`" rule.
fn relabel_series(mut ticks: Vec<TradeTick>, series_symbol: &str) -> Vec<TradeTick> {
    for t in &mut ticks {
        t.symbol = series_symbol.to_string();
    }
    ticks
}

/// The subscribe frame for the public `publicTrade` channel on `symbol` — same
/// `{"op":"subscribe","args":[...]}` shape as [`subscribe_frame`] (klines), just the `publicTrade`
/// topic with no interval code.
pub fn trades_subscribe_frame(symbol: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [format!("publicTrade.{symbol}")]
    })
    .to_string()
}

/// [`route_frame`]'s trades-pump twin: classifies ONE raw WS text frame for [`trades_main`].
/// The subscribe OUTCOME envelope is checked first (same net-hardening br7 shape as the kline path):
/// `{op:"subscribe", success:true}` -> [`TradesFrame::Ack`] (disarms the subscribe-ack watchdog),
/// `success:false` -> an attributable [`TradesFrame::Error`] carrying Bybit's `ret_msg`. Otherwise
/// [`parse_trades`] decodes the push; an empty result (non-trades topic, or every row malformed) is
/// [`TradesFrame::Ignored`].
enum TradesFrame {
    Ticks(Vec<TradeTick>),
    Ack,
    Error(String),
    Ignored,
}

fn route_trades_frame(text: &str) -> TradesFrame {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return TradesFrame::Ignored; // bare-text "ping"/"pong" keepalive is not JSON
    };
    if let Some(op) = v.get("op").and_then(Value::as_str) {
        if op == "subscribe" {
            return if v.get("success").and_then(Value::as_bool).unwrap_or(false) {
                TradesFrame::Ack
            } else {
                let msg = v.get("ret_msg").and_then(Value::as_str).unwrap_or("(no message)");
                TradesFrame::Error(format!("Bybit subscribe rejected: {msg}"))
            };
        }
        return TradesFrame::Ignored; // pong / other ops
    }
    let ticks = parse_trades(&v);
    if ticks.is_empty() { TradesFrame::Ignored } else { TradesFrame::Ticks(ticks) }
}

/// `subscribe_trades`'s thread body — the trades twin of [`feed_main`], on the same shared
/// [`run_market_feed`] driver (dedup A6): identical subscribe-replay, br7 ack-watchdog, stop-poll,
/// and 30×100 ms backoff lifecycle, scoped down to prints (no closed/forming bar routing, no
/// mark-tick). Live-only: unlike [`feed_main`]/[`depth_main`], there is no REST warmup/seed here —
/// Bybit's `publicTrade` channel has no historical REST counterpart threaded into this feed, and a
/// tick/volume-bar builder needs no seed to start folding live prints.
///
/// `series_symbol` is the catalog/sink label (`.P`-suffixed for a perp — the `TradeStore` key);
/// `api_symbol` is the `.P`-stripped exchange symbol used for the `publicTrade` subscribe topic, and
/// `is_perp` picks the spot-vs-linear host. Spot passes `series_symbol == api_symbol`, `is_perp =
/// false`, so this is byte-identical to the pre-perp behavior for spot. Emitted ticks are re-labeled
/// to `series_symbol` via [`relabel_series`], because the linear wire `s` is the plain `BTCUSDT` and
/// the `TradeStore` keys on `tick.symbol`.
fn trades_main(series_symbol: String, ctx: FeedCtx) {
    // Same shape as `feed_main`'s route resolution, and for the same reasons — see its comment.
    let (api_symbol, book) = match feed_route(&series_symbol) {
        Ok(v) => v,
        Err(e) => {
            ctx.set_status(format!("{series_symbol} trades: {e}"));
            return;
        }
    };
    let host = ws_host(book);
    let sub = trades_subscribe_frame(&api_symbol);
    run_market_feed(
        host,
        &subscribe_only_pump_opts(VENUE, &sub),
        &ctx.stop,
        &now_ms,
        |txt| match route_trades_frame(txt) {
            TradesFrame::Ticks(ticks) => {
                // Re-label to the `.P` series key BEFORE emit — the `TradeStore` keys on
                // `tick.symbol`, and a linear perp's wire `s` is the plain `BTCUSDT`. Spot is a
                // no-op (series_symbol == wire s), so it stays byte-identical.
                for mut tick in relabel_series(ticks, &series_symbol) {
                    tick.local_ts = now_ms(); // machine receive time (dual-timestamp capture)
                    ctx.sink.trade(VENUE, &series_symbol, tick);
                }
                (ctx.wake)();
                FrameOutcome::Confirm // data confirms the br7 subscribe handshake
            }
            TradesFrame::Ack => FrameOutcome::Confirm,
            TradesFrame::Error(msg) => FrameOutcome::Fatal(msg),
            TradesFrame::Ignored => FrameOutcome::Ignore,
        },
        || {}, // no dataless-tick judgment on this lane
        |s| match s {
            // Replaces the unconditional spawn-time `"LIVE · Bybit trades {series_symbol}"` this
            // function used to write before opening a socket — the MIRROR IMAGE of the kline
            // latch: a claim that the lane is live made before a connection exists, which on a
            // dead venue reads healthy forever. The claim is now made when the venue has actually
            // ACKed, and spells [`LIVE_STATUS`] rather than a per-lane text.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{series_symbol} trades ws error (reconnecting): {e}"))
            }
        },
    );
}

// --- L2 depth feed (DOM) --------------------------------------------------------------------
// Bybit V5 `orderbook.200.<SYM>` on the stream `feed_route` resolves the symbol to (spot, linear or
// inverse — `depth_main` carries the route): a WS `snapshot` then `delta`s — no REST seed. Reuses
// the fixture-tested folding in `crate::market_data::route_frame` for the PROTOCOL + the shared
// `vike_bridge_core::depth` driver for the LIFECYCLE. Delivers the top `DEPTH_LEVELS` via
// `LiveDataSink::l2_snapshot`.
use vike_bridge_core::depth::{BookOp, infer_tick_size, run_depth_feed};
use vike_bridge_core::stream_health::{HealthEvent, health_to_stream_status};

/// Levels per side published to the DOM.
const DEPTH_LEVELS: usize = 200;
/// Reconnect backoff after a socket error / gap.
const DEPTH_BACKOFF: Duration = Duration::from_secs(3);
/// Bybit drops a client that doesn't ping within ~20 s — the driver sends this every ~15 s.
const KEEPALIVE: &str = r#"{"op":"ping"}"#;
/// Net-hardening §B dead-transport watchdog: no orderbook frame of ANY kind (a snapshot/delta OR the
/// `{"op":"pong"}` reply to our keepalive) for this long means the socket is silently dead → the
/// driver discloses a gap and reconnects+re-seeds. Well above the 15 s keepalive cadence, so a quiet
/// book kept alive purely by ping/pong never false-trips; only true transport silence does.
const DEPTH_IDLE_THRESHOLD: Duration = Duration::from_secs(30);
/// §B data-freshness threshold for the depth book: how long the book may go without an applied
/// update, behind a still-live transport, before disclosing `Stale` (disclose-only, no auto-action;
/// the fast dead-socket signal is [`DEPTH_IDLE_THRESHOLD`], 30s). **Validated by live measurement
/// (2026-07-11) across the binance/bybit/okx keyless depth feeds: liquid pairs (BTC/ETH) update
/// ~0.1s, mid-caps ~3.6s max, and the market's THINNEST actively-traded pairs gapped 44–47s (a
/// near-dead pair updated only twice in 5 min).** 120s clears the thin-pair worst case with ~2.5×
/// margin, so only a genuinely near-dead book trips — where the `Stale` disclosure is accurate. Kept
/// at 120s (measured floor, not a placeholder); a uniform per-venue value must cover the thinnest
/// subscribable symbol (#111).
const DEPTH_FRESHNESS_THRESHOLD: Duration = Duration::from_secs(120);
/// Net-hardening timed book re-seed: forced fresh-snapshot cadence. Bybit's `orderbook.200` carries NO
/// checksum, so a dropped/mis-applied `delta` that does NOT break the seq chain corrupts the managed
/// book silently — no `BookOp::Gap`, no idle, no freshness trip fires, and it would stay wrong until
/// the next natural reconnect (hours). Ending the session every `DEPTH_RESEED_INTERVAL` forces the
/// driver to reconnect + re-subscribe (a fresh WS `snapshot` rebuilds the book), bounding that
/// corruption window to <= this — reusing the gap handler's reconnect path but disclosing NO transport
/// gap (a scheduled refresh, not an outage). 5 min is a conservative default, well above the reconnect
/// cost; `None` would disable it.
const DEPTH_RESEED_INTERVAL: Duration = Duration::from_secs(300);

use vike_model::now_ms;

/// The venue EVENT-TIME (epoch-ms) of a Bybit orderbook frame — its top-level `ts` — for the §B
/// data-freshness watchdog (`0` if absent → the driver falls back to receive-time). Re-parses just
/// that field from the raw frame, so it serves ONLY the once-per-seed first-snapshot arm of
/// [`fold_depth_frame`] (which reads that snapshot with `parse_orderbook_snapshot`, not `route_frame`):
/// every seeded frame takes its time from `MdEvent::BookUpdated { ts_ms }`, one parse per delta.
fn frame_ts_ms(txt: &str) -> i64 {
    serde_json::from_str::<serde_json::Value>(txt)
        .ok()
        .and_then(|v| v.get("ts").and_then(|t| t.as_i64()))
        .unwrap_or(0)
}

/// Publish the top [`DEPTH_LEVELS`] to the DOM sink (carrying the book's tick) + nudge a repaint.
fn publish_book(book: &vike_model::L2Book, symbol: &str, ctx: &FeedCtx) {
    let (bids, asks) = book.top_n(DEPTH_LEVELS);
    ctx.sink.l2_snapshot(VENUE, symbol, book.tick_size, bids, asks, now_ms());
    (ctx.wake)();
}

/// Fold one depth WS frame into the (maybe not-yet-seeded) book slot — the driver `decode` body,
/// named so the frame→[`BookOp`] mapping is unit-testable without a socket. Before the first
/// `snapshot` everything else is [`BookOp::Ignored`]; once seeded, frames fold via the
/// fixture-tested `crate::market_data::route_frame` (fully qualified — this module's own
/// `route_frame` decodes KLINES), and its [`MdEvent::Resync`](crate::market_data::MdEvent::Resync)
/// — a dropped delta frame or a venue-restart `u` regression, judged by the shared
/// `L2Book::delta_decision` strict law — maps to [`BookOp::Gap`], on which the shared driver
/// discloses the gap and reconnects + re-seeds IMMEDIATELY (previously the fold had no gap arm
/// and a dropped frame silently corrupted the DOM book until the ~5-min timed reseed).
fn fold_depth_frame(txt: &str, sym: &str, book: &mut Option<vike_model::L2Book>) -> BookOp {
    match book.as_mut() {
        None => match crate::market_data::parse_orderbook_snapshot(txt) {
            Some((seq, b, a)) => {
                let mut bk = vike_model::L2Book::new(infer_tick_size(&b, &a));
                bk.apply_snapshot(seq, &b, &a);
                *book = Some(bk);
                BookOp::Updated(frame_ts_ms(txt)) // §B: venue event-time from the frame's `ts`
            }
            None => BookOp::Ignored, // a delta/ack before the first snapshot
        },
        Some(bk) => match crate::market_data::route_frame(txt, sym, bk) {
            crate::market_data::MdEvent::BookUpdated { ts_ms } => BookOp::Updated(ts_ms),
            crate::market_data::MdEvent::Resync => BookOp::Gap, // seq gap — driver re-seeds
            _ => BookOp::Ignored,                               // Trade or Ignored
        },
    }
}

/// Depth feed body on the shared driver. Bybit's protocol is WS-seeded (no REST): the first
/// `snapshot` frame builds the book (tick inferred from it); subsequent `snapshot`/`delta` frames
/// fold via [`fold_depth_frame`], whose gap arm hands [`BookOp::Gap`] to the driver's
/// reconnect+re-seed lifecycle.
/// ⚠ **THIS LANE'S ROUTE CHANGED, and the change is visible to anyone who opens a bybit DOM.**
/// It used to call NOTHING: the raw `symbol` was interpolated into the topic and the socket was
/// [`PUBLIC_WS_LINEAR`] unconditionally — the third and worst of the three readings this file used
/// to have of one string (`perp_split`'s doc named it as such). Two defects, one line:
///
///   * a `.P` symbol subscribed `orderbook.200.BTCUSDT.P`, a topic that does not exist, so the DOM
///     never populated and nothing said why; and
///   * a BARE symbol got the LINEAR perp book under a SPOT label. That was written down as
///     deliberate ("the DOM ladder matches the venue `BybitPerpRest` executes on") and it is still
///     a book that disagrees with its own label — the same class of silence
///     `docs/decisions/0061-an-instrument-names-its-kind.md` exists about, and the reason a bybit
///     spot DOM has never shown the spot book.
///
/// Both now go through [`feed_route`], so this lane reaches exactly the book the series label
/// names. **A bare bybit symbol's DOM therefore moves from the perp book to the spot book** — a
/// user-visible change, stated here rather than discovered. Nothing that TRADES reads this lane:
/// `subscribe_depth` is the chart DOM (`vike_app_core::ui::feed_lifecycle`'s `ensure_depth`), while
/// `vike-tradehub`'s bybit tick lane is `crates/bridges/bybit/src/market_data.rs` and is untouched.
fn depth_main(series_symbol: String, _interval: String, ctx: FeedCtx) {
    let (api_symbol, book) = match feed_route(&series_symbol) {
        Ok(v) => v,
        Err(e) => {
            ctx.set_status(format!("{series_symbol} depth: {e}"));
            return;
        }
    };
    let sub = format!(r#"{{"op":"subscribe","args":["orderbook.200.{api_symbol}"]}}"#);
    // The book's own key stays the SERIES label — the sink and every consumer key on what the
    // caller asked for, exactly as the kline and trades lanes re-label. Only the topic is stripped.
    let symbol = series_symbol;
    let sym = symbol.as_str();
    let seed = || None::<vike_model::L2Book>;
    let decode = |txt: &str, book: &mut Option<vike_model::L2Book>| -> BookOp {
        fold_depth_frame(txt, sym, book)
    };
    let publish = |b: &vike_model::L2Book| publish_book(b, sym, &ctx);
    // Net-hardening §B: map each driver-neutral `HealthEvent` onto the sink's machine-readable
    // disclosure. The depth driver owns the `StreamHealth` that dedups transport gaps across
    // reconnects (one outage → one GapStart/Live pair) now; the venue just maps + discloses.
    let on_health = |ev: HealthEvent| {
        ctx.sink.stream_status(VENUE, sym, "depth", health_to_stream_status(ev));
    };
    run_depth_feed(
        // The book the SERIES LABEL names — see this function's ⚠ for what that changed.
        ws_host(book),
        Some(&sub),
        Some(KEEPALIVE),
        seed,
        decode,
        publish,
        on_health,
        &ctx.stop,
        READ_TIMEOUT,
        DEPTH_IDLE_THRESHOLD,
        DEPTH_FRESHNESS_THRESHOLD,
        Some(DEPTH_RESEED_INTERVAL),
        &now_ms,
        DEPTH_BACKOFF,
    );
}

/// All live feed threads, keyed by [`SubscriptionId`] via the shared [`FeedRegistry`] (dedup A6:
/// one stop flag + one `JoinHandle` per subscription; [`Feeds::unsubscribe`] stops+joins exactly
/// one stream, [`DataClient::shutdown`] stops+joins them all). Nothing is ever detached — the
/// deterministic-teardown rule.
pub struct Feeds {
    sink: Arc<dyn LiveDataSink>,
    pub status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    registry: FeedRegistry,
    /// Whether a perp `subscribe_bars` also opens the linear `tickers` mark stream — the venue's
    /// `mark_streams` row (`Feeds::with_mark_streams`), else [`MARK_STREAM_DEFAULT_ON`] (mark-slot
    /// semantics, W2-T4).
    mark_streams: bool,
    /// PER-SYMBOL reference-counted companion mark streams (the mark pump has no id of its own at
    /// the `DataClient` seam), so charting one perp at two intervals opens ONE mark socket and
    /// [`Feeds::unsubscribe`] stops it only when the last bars subscription releases it.
    mark_pairings: vike_bridge_core::MarkPairings<SubscriptionId>,
}

/// This venue's mark wire is documented, so its mark stream ships ON;
/// `venue.bybit.mark_streams = 0` turns it off (decision 0095).
const MARK_STREAM_DEFAULT_ON: bool = true;

/// Whether a `subscribe_bars` on `symbol` should ALSO open the linear `tickers` mark stream: the
/// `.P` perp tag AND the venue's `mark_streams` row. Pure — the ONE place the pairing predicate
/// lives, so the spawn site and its tests read the same law. Spot has no mark price at all.
fn should_pair_mark(symbol: &str, mark_streams: bool) -> bool {
    mark_streams && perp_split(symbol).1
}

impl Feeds {
    /// How many feed threads this `Feeds` currently has running — every one of which writes the
    /// ONE `status` string [`Self::status`] hands out.
    ///
    /// ⚠ **It exists so a consumer can decide whether that string is UNAMBIGUOUS evidence**, which
    /// is a question about the mount rather than about this code. `crates/vike-tradehub/src/
    /// feeds.rs`'s `LiveFeeds::recon_feed_statuses` health-gates a venue's reconcile leg on the
    /// string, and the gate can only ever SUPPRESS — so a second lane sharing it is the difference
    /// between a fault report and an ambiguous one. On 2026-09-10 that row suppressed bybit's leg
    /// for 42 hours; the daemon's mount happened to run exactly one lane here, which is the only
    /// reason the latch was diagnosable at all. A `.P` symbol (a paired mark lane) or a second
    /// interval makes it two, and neither is visible from the daemon's source.
    ///
    /// Counts SUBSCRIPTIONS, not lane kinds: a mark lane paired to a bar subscription is its own
    /// registry entry, so it counts — which is the answer the caller wants.
    pub fn status_writer_lanes(&self) -> usize {
        self.registry.len()
    }

    /// `sink` receives every seed/close/forming/mark call from every subscription this `Feeds`
    /// spawns (shared — construct once, subscribe many). `wake` is the GUI repaint nudge fired on
    /// status / forming-bar changes; pass `|| {}` headless. The registry's spawn hook is the
    /// venue's HFT affinity pin (opt-in via `VIKE_PIN_CORES`, no-op otherwise) — threaded in as a
    /// closure because `vike-data` deliberately never depends on `vike-exec`.
    pub fn new(sink: Arc<dyn LiveDataSink>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Feeds {
            sink,
            status: Arc::new(Mutex::new("connecting to Bybit…".into())),
            wake: Arc::new(wake),
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    "bybit",
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

    /// Fallible spawn of the feed thread for `(symbol, interval)`: allocates a fresh
    /// [`SubscriptionId`] and a dedicated stop flag, then runs [`feed_main`] on its own thread —
    /// an OS thread-spawn failure is returned (not panicked) so [`DataClient::subscribe_bars`] can
    /// map it to a [`LiveDataError`].
    ///
    /// A trailing `.P` on `symbol` marks a PERPETUAL (the catalog's distinct-symbol tag, matching
    /// `BYBIT:BTCUSDT.P`): it is stripped to the exchange symbol for the REST seed and the kline
    /// subscribe, while the ORIGINAL `.P`-suffixed `symbol` stays the sink/core series label (so a
    /// perp's series key never collides with its spot twin). Mirrors binance's
    /// `market_feed::Feeds::try_spawn` exactly (same technique, same rationale).
    ///
    /// ⚠ **WHICH perpetual book is resolved inside the thread, not here** ([`feed_route`], called by
    /// [`feed_main`]): the first resolution per process reads the venue's instrument listings, and
    /// this method runs on the caller's — often the picker's — thread. `symbol`/`interval` are
    /// passed into `spawn_with` unchanged (its `FnOnce(String, String, FeedCtx)` bound stays as-is).
    pub fn try_spawn(&mut self, symbol: &str, interval: &str) -> std::io::Result<SubscriptionId> {
        let series_symbol = symbol.to_string();
        let id = self.spawn_with(symbol, interval, move |_symbol, interval, ctx| {
            feed_main(series_symbol, interval, ctx)
        })?;
        let series = symbol.to_string();
        self.pair_mark_stream(id, symbol, move |_s, _i, ctx| mark_main(series, ctx));
        Ok(id)
    }

    /// Attach the venue's REAL mark stream (linear `tickers`, mark-slot semantics — default ON,
    /// a `mark_streams = 0` row disables) to the bars subscription `bars_id` just created for
    /// `symbol`. Spawns at most ONE mark socket per symbol ([`vike_bridge_core::MarkPairings`]);
    /// a second bars subscription on the same perp only reference-counts the running one.
    /// Fail-soft: if the extra thread can't spawn, the bars feed stays live and valuation falls
    /// back to the resolver's bar-close rung, exactly the no-mark-stream behavior. Production and
    /// the pairing tests share this method — only `body` differs (tests pass a network-free
    /// stand-in).
    fn pair_mark_stream(
        &mut self,
        bars_id: SubscriptionId,
        symbol: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) {
        if self.mark_pairings.attach(bars_id, symbol, should_pair_mark(symbol, self.mark_streams)) {
            return; // disabled/spot, or an existing stream was reference-counted
        }
        if let Ok(mid) = self.spawn_with(symbol, "mark", body) {
            self.mark_pairings.record(bars_id, symbol, mid);
        }
    }

    /// Shared per-key spawn bookkeeping, now one [`FeedRegistry::spawn`] call (dedup A6): the
    /// registry allocates the id + stop flag and owns the join handle; this venue wrapper only
    /// assembles its own [`FeedCtx`] around the registry-issued stop flag. `body` is the shared
    /// `feed_main` in production; tests substitute a network-free stand-in to exercise the per-key
    /// lifecycle deterministically without a real venue connection.
    fn spawn_with(
        &mut self,
        symbol: &str,
        interval: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) -> std::io::Result<SubscriptionId> {
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        let (symbol, interval) = (symbol.to_string(), interval.to_string());
        self.registry.spawn(format!("feed-bybit-{symbol}@{interval}"), move |stop| {
            let ctx = FeedCtx { sink, status, wake, stop };
            body(symbol, interval, ctx)
        })
    }
}

impl DataClient for Feeds {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.try_spawn(symbol, interval)
            .map_err(|e| LiveDataError::Subscribe(format!("bybit {symbol}@{interval}: {e}")))
    }

    fn subscribe_quotes(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (bybit live_data.quotes = false): stays in lockstep with the table.
        require_live_verb(VENUE, LiveVerb::Quotes)?;
        unreachable!("bybit declares no live quotes")
    }

    /// Start a live `publicTrade` stream for `symbol` (executed prints, no REST seed). Data flows
    /// out through [`LiveDataSink::trade`]; the returned id stops+joins just this stream (same
    /// per-key bookkeeping as `subscribe_bars`/`subscribe_depth`). Mirrors okx's `subscribe_trades`.
    ///
    /// A trailing `.P` marks a PERPETUAL: it is stripped to the exchange symbol for the
    /// `publicTrade` subscribe topic and for the host that symbol's BOOK names, while the ORIGINAL
    /// `.P`-suffixed `symbol` stays the sink/core series label — the SAME split
    /// [`Feeds::try_spawn`] does for klines (via [`feed_route`], resolved inside the thread). A spot
    /// symbol resolves `series_symbol == api_symbol` and [`Category::Spot`], so the spot path is
    /// byte-identical to the pre-perp behavior.
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let series_symbol = symbol.to_string();
        self.spawn_with(symbol, "trades", move |_symbol, _interval, ctx| {
            trades_main(series_symbol, ctx)
        })
        .map_err(|e| LiveDataError::Subscribe(format!("bybit {symbol} trades: {e}")))
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (bybit live_data.book = false); use subscribe_depth for L2 depth.
        require_live_verb(VENUE, LiveVerb::Book)?;
        unreachable!("bybit declares no lossless book lane")
    }

    /// Start a live L2 depth stream for `symbol` (`orderbook.200`; the book — spot, linear or
    /// inverse — is whichever [`feed_route`] resolves the symbol to, inside the thread). Data flows
    /// out through [`LiveDataSink::l2_snapshot`]; the returned id stops+joins just this stream.
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "depth", depth_main)
            .map_err(|e| LiveDataError::Subscribe(format!("bybit {symbol} depth: {e}")))
    }

    /// Stop + JOIN exactly the stream `id` names — plus its companion perp mark stream when this
    /// was the LAST bars subscription holding that symbol's mark stream; every other subscription
    /// on this `Feeds` keeps running. Unknown ids (already stopped, never issued) are a no-op.
    fn unsubscribe(&mut self, id: SubscriptionId) {
        vike_bridge_core::mark_stream::unsubscribe_with_mark(
            &mut self.mark_pairings,
            &mut self.registry,
            id,
        );
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

#[path = "healthy_string_pin.rs"]
#[cfg(test)]
mod healthy_string_pin;

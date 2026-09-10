//! Live Bybit V5 **spot** kline market data → the vike-data live seam — the Bybit twin of
//! binance's `market_feed` module (the kline feed implementing [`vike_data::DataClient`], as
//! distinct from [`crate::market_data`], the L2/tick HFT feed).
//!
//! Same shape as the Binance feed: one STOPPABLE thread per (symbol, interval) — REST warmup seed,
//! then the public `kline.<code>.<SYMBOL>` WS. Data flows through the [`vike_data::LiveDataSink`]
//! handed to [`Feeds::new`] at construction: closed bars via `close_bar` (the lossless lane),
//! intrabar forming updates + last-price ticks via `forming_bar`/`bar_close_tick` (the wait-free
//! conflating lane; a `.P` perp bars subscription ALSO opens the linear `tickers` stream feeding
//! the REAL-mark verb `mark_tick`, default ON via `VIKE_MARK_STREAMS`). Threads poll a
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
//! spot-vs-linear host pick, the REST warmup seed, and the sink emission.
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
use vike_bridge_core::market_pump::{FrameOutcome, MarketPumpOpts, run_market_feed};
use vike_bridge_core::pump_spec::market_pump_spec;
use vike_data::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, StreamStatus, SubscriptionId,
    require_live_verb,
};
use vike_model::{Bar, LiveVerb, TradeTick};

use super::data::{fetch_klines_latest, interval_code};

const VENUE: &str = "bybit";
const READ_TIMEOUT: Duration = Duration::from_secs(2); // depth driver's stop-flag poll cadence
const SEED_LIMIT: usize = 1000; // newest N klines for the REST warmup (Bybit's per-request cap)
/// Bybit V5 public **spot** stream (matches `data::CATEGORY = "spot"`, the kline category served).
pub const PUBLIC_WS_SPOT: &str = "wss://stream.bybit.com/v5/public/spot";
/// Bybit V5 public **linear-perp** stream — the DOM depth ladder venue, so the displayed book matches
/// what `BybitPerpRest` actually trades (`BTCUSDT` linear). Same `orderbook.200.<SYM>`/`kline.<code>.<SYM>`
/// topic shapes as spot; only the stream URL differs. A `.P`-suffixed `subscribe_bars` symbol (a perp,
/// the catalog's distinct-symbol tag) routes its kline WS here too — see [`Feeds::try_spawn`].
pub const PUBLIC_WS_LINEAR: &str = "wss://stream.bybit.com/v5/public/linear";

/// Split an incoming feed symbol into `(api_symbol, is_perp)`: a trailing `.P` marks a linear perp
/// (the catalog's distinct-symbol tag, matching `BYBIT:BTCUSDT.P`) and is STRIPPED to the exchange
/// symbol that drives the WS subscribe topic + REST seed, while the ORIGINAL `.P`-suffixed symbol
/// stays the sink/core series label (so a perp's series key never collides with its spot twin).
/// The ONE `.P` idiom shared by the kline feed ([`Feeds::try_spawn`]) and the trades feed
/// ([`DataClient::subscribe_trades`]) — an owning wrapper over [`vike_catalog::split_perp`], which
/// is where that split is defined for every venue.
fn perp_split(symbol: &str) -> (String, bool) {
    let (api, perp) = vike_catalog::split_perp(symbol);
    (api.to_string(), perp)
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

/// The shared [`MarketPumpOpts`] every Bybit market pump (kline + trades) runs with — CONSUMED
/// from this venue's `MarketPumpSpec` row ([`market_pump_spec`], row ownership): subscribe
/// replayed per session, NO app-level keepalive on these public streams (the depth feed pings via
/// its own driver), the br7 10 s subscribe-ack watchdog, no idle watchdog, and the classic
/// stop-aware 30×100 ms reconnect backoff. The knob VALUES live in the row (one edit site); only
/// the subscribe payload is built here.
fn pump_opts(subscribe: &str) -> MarketPumpOpts<'_> {
    market_pump_spec(VENUE).knobs().opts(Some(subscribe), None)
}

/// `series_symbol` is the catalog/sink label (`.P`-suffixed for a perp — the core key); `api_symbol`
/// is the `.P`-stripped exchange symbol used for the REST seed + WS subscribe topic (IDENTICAL shape
/// on spot and linear — Bybit does not encode category in the topic, only the socket URL). Spot
/// subscriptions pass `series_symbol == api_symbol` and `is_perp = false`, so this is byte-identical
/// to the pre-perp behavior for spot. `is_perp` picks the host: spot stays on [`PUBLIC_WS_SPOT`],
/// perps route to [`PUBLIC_WS_LINEAR`].
///
/// The WS session/reconnect lifecycle rides the shared [`run_market_feed`] driver (dedup A6);
/// each decoded frame folds through the fixture-tested [`route_frame`] inside the `on_text`
/// closure — a data frame emits under the SERIES symbol (last price → the conflated mark cache,
/// then the lossless `close_bar` or conflating `forming_bar` lane) and confirms the br7 subscribe
/// handshake; an ack confirms it dataless; a venue REJECT is surfaced attributably
/// ([`FrameOutcome::Fatal`], never silently dropped).
fn feed_main(
    series_symbol: String,
    api_symbol: String,
    interval: String,
    is_perp: bool,
    ctx: FeedCtx,
) {
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
    // REST warmup: newest N klines; Bybit serves the in-progress candle as the last one — seed the
    // closed prefix, route the forming tail through the market lane (mirrors binance). Once per
    // feed, NOT per session — a reconnect does not re-seed (the pre-driver behavior).
    match fetch_klines_latest(&api_symbol, &interval, SEED_LIMIT, is_perp) {
        Ok(mut bars) => {
            let forming = if bars.len() > 1 { bars.pop() } else { None };
            ctx.sink.seed_bars(VENUE, &series_symbol, &interval, bars);
            if let Some(f) = forming {
                ctx.sink.bar_close_tick(VENUE, &series_symbol, f.close, f.ts);
                ctx.sink.forming_bar(VENUE, &series_symbol, &interval, f);
            }
            ctx.set_status("LIVE · Bybit".into());
        }
        Err(e) => ctx.set_status(format!("{key} seed error: {e}")),
    }
    let host = if is_perp { PUBLIC_WS_LINEAR } else { PUBLIC_WS_SPOT };
    let sub = subscribe_frame(&code, &api_symbol);
    run_market_feed(
        host,
        &pump_opts(&sub),
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
            ctx.sink.bar_close_tick(VENUE, &series_symbol, bar.close, bar.ts);
            if confirmed {
                // bar CLOSED — lossless lane (a missed close = a series hole)
                ctx.sink.close_bar(VENUE, &series_symbol, &interval, bar);
            } else {
                ctx.sink.forming_bar(VENUE, &series_symbol, &interval, bar);
            }
            (ctx.wake)();
            FrameOutcome::Confirm
        },
        || {}, // no dataless-tick judgment on this lane (the polymarket freshness knob)
        |e| ctx.set_status(format!("{key} ws error (reconnecting): {e}")),
    );
}

// --- Perp mark-price feed (mark-slot semantics, W2-T4) ---------------------------------------
// Bybit V5 `tickers.<SYMBOL>` on the LINEAR stream: the venue's REAL `markPrice` (the price its
// liquidation/funding engine keys off), pushed as a `snapshot` then field-sparse `delta`s — a
// delta that doesn't move the mark simply omits `markPrice` and is ignored here. Feeds
// `LiveDataSink::mark_tick` (the `PriceBoard` MARK slot); the kline pump's candle closes ride
// `bar_close_tick`. Perp-only: opened automatically next to a `.P` bars subscription
// (`VIKE_MARK_STREAMS=0` disables). Same subscribe/ack grammar as the kline feed.

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

/// The perp mark-price feed-thread body — [`feed_main`]'s mark twin on the same shared driver,
/// always on [`PUBLIC_WS_LINEAR`] (spot has no mark price). Emits under the `.P` SERIES symbol.
/// No REST seed — a live-only valuation stream; the resolver's bar-close fallback covers the
/// pre-first-frame window.
fn mark_main(series_symbol: String, api_symbol: String, ctx: FeedCtx) {
    let sub = mark_subscribe_frame(&api_symbol);
    run_market_feed(
        PUBLIC_WS_LINEAR,
        &pump_opts(&sub),
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
        |e| ctx.set_status(format!("{series_symbol} mark ws error (reconnecting): {e}")),
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
fn trades_main(series_symbol: String, api_symbol: String, is_perp: bool, ctx: FeedCtx) {
    ctx.set_status(format!("LIVE · Bybit trades {series_symbol}"));
    let host = if is_perp { PUBLIC_WS_LINEAR } else { PUBLIC_WS_SPOT };
    let sub = trades_subscribe_frame(&api_symbol);
    run_market_feed(
        host,
        &pump_opts(&sub),
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
        |e| ctx.set_status(format!("{series_symbol} trades ws error (reconnecting): {e}")),
    );
}

// --- L2 depth feed (DOM) --------------------------------------------------------------------
// Bybit V5 `orderbook.200.<SYM>` on the LINEAR-perp stream (matches `BybitPerpRest` execution): a WS
// `snapshot` then `delta`s — no REST seed. Reuses the fixture-tested folding in
// `crate::market_data::route_frame` for the PROTOCOL + the shared `vike_bridge_core::depth` driver
// for the LIFECYCLE. Delivers the top `DEPTH_LEVELS` via `LiveDataSink::l2_snapshot`.
use vike_bridge_core::depth::{BookOp, infer_tick_size, run_depth_feed};
use vike_bridge_core::stream_health::HealthEvent;

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

/// Map the driver-neutral [`HealthEvent`] onto this crate's `vike_data::StreamStatus` disclosure
/// vocabulary (1:1). Kept in the venue crate because `vike-bridge-core` is deliberately
/// `vike-data`-free; the depth driver owns the `StreamHealth` that dedups transport gaps across
/// reconnects now, so this is a pure translation at the `LiveDataSink` boundary.
fn health_to_stream_status(ev: HealthEvent) -> StreamStatus {
    match ev {
        HealthEvent::Gap { at_ts_ms } => StreamStatus::GapStart { at_ts_ms },
        HealthEvent::Live { gap_started_ts_ms } => StreamStatus::Live { gap_started_ts_ms },
        HealthEvent::Stale { newest_data_ts_ms, now_ms } => {
            StreamStatus::Stale { newest_data_ts_ms, now_ms }
        }
    }
}

/// The venue EVENT-TIME (epoch-ms) of a Bybit orderbook frame — its top-level `ts` — for the §B
/// data-freshness watchdog (`0` if absent → the driver falls back to receive-time). Re-parses just
/// that field from the raw frame; the fixture-tested `market_data` folders don't surface it, and this
/// keeps them untouched.
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
            crate::market_data::MdEvent::BookUpdated => BookOp::Updated(frame_ts_ms(txt)),
            crate::market_data::MdEvent::Resync => BookOp::Gap, // seq gap — driver re-seeds
            _ => BookOp::Ignored,                               // Trade or Ignored
        },
    }
}

/// Depth feed body on the shared driver. Bybit's protocol is WS-seeded (no REST): the first
/// `snapshot` frame builds the book (tick inferred from it); subsequent `snapshot`/`delta` frames
/// fold via [`fold_depth_frame`], whose gap arm hands [`BookOp::Gap`] to the driver's
/// reconnect+re-seed lifecycle.
fn depth_main(symbol: String, _interval: String, ctx: FeedCtx) {
    let sub = format!(r#"{{"op":"subscribe","args":["orderbook.200.{symbol}"]}}"#);
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
        // LINEAR-perp stream: the DOM ladder matches the venue `BybitPerpRest` executes on.
        PUBLIC_WS_LINEAR,
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
    /// Whether a perp `subscribe_bars` also opens the linear `tickers` mark stream — resolved
    /// once at construction from `VIKE_MARK_STREAMS` (default ON; mark-slot semantics, W2-T4).
    mark_streams: bool,
    /// PER-SYMBOL reference-counted companion mark streams (the mark pump has no id of its own at
    /// the `DataClient` seam), so charting one perp at two intervals opens ONE mark socket and
    /// [`Feeds::unsubscribe`] stops it only when the last bars subscription releases it.
    mark_pairings: vike_bridge_core::MarkPairings<SubscriptionId>,
}

/// Whether a `subscribe_bars` on `symbol` should ALSO open the linear `tickers` mark stream: the
/// `.P` perp tag AND the `VIKE_MARK_STREAMS` knob. Pure — the ONE place the pairing predicate
/// lives, so the spawn site and its tests read the same law. Spot has no mark price at all.
fn should_pair_mark(symbol: &str, mark_streams: bool) -> bool {
    mark_streams && perp_split(symbol).1
}

impl Feeds {
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
            mark_streams: vike_bridge_core::mark_streams_enabled(),
            mark_pairings: Default::default(),
        }
    }

    /// Fallible spawn of the feed thread for `(symbol, interval)`: allocates a fresh
    /// [`SubscriptionId`] and a dedicated stop flag, then runs [`feed_main`] on its own thread —
    /// an OS thread-spawn failure is returned (not panicked) so [`DataClient::subscribe_bars`] can
    /// map it to a [`LiveDataError`].
    ///
    /// A trailing `.P` on `symbol` marks a linear perp (the catalog's distinct-symbol tag, matching
    /// `BYBIT:BTCUSDT.P`): it is stripped to the exchange symbol for the `category=linear` REST seed
    /// and the [`PUBLIC_WS_LINEAR`] kline subscribe, while the ORIGINAL `.P`-suffixed `symbol` stays
    /// the sink/core series label (so a perp's series key never collides with its spot twin). Mirrors
    /// binance's `market_feed::Feeds::try_spawn` exactly (same technique, same rationale).
    /// `symbol`/`interval` are still passed into `spawn_with` unchanged (its
    /// `FnOnce(String, String, FeedCtx)` bound stays as-is); the series/api/is_perp split is captured
    /// by the closure instead.
    pub fn try_spawn(&mut self, symbol: &str, interval: &str) -> std::io::Result<SubscriptionId> {
        let (api_symbol, is_perp) = perp_split(symbol);
        let series_symbol = symbol.to_string();
        let api_for_mark = api_symbol.clone();
        let id = self.spawn_with(symbol, interval, move |_symbol, interval, ctx| {
            feed_main(series_symbol, api_symbol, interval, is_perp, ctx)
        })?;
        let series = symbol.to_string();
        self.pair_mark_stream(id, symbol, move |_s, _i, ctx| mark_main(series, api_for_mark, ctx));
        Ok(id)
    }

    /// Attach the venue's REAL mark stream (linear `tickers`, mark-slot semantics — default ON,
    /// `VIKE_MARK_STREAMS=0` disables) to the bars subscription `bars_id` just created for
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
    /// A trailing `.P` marks a linear perp: it is stripped to the exchange symbol for the
    /// `publicTrade` subscribe topic + the [`PUBLIC_WS_LINEAR`] host, while the ORIGINAL `.P`-suffixed
    /// `symbol` stays the sink/core series label — the SAME split [`Feeds::try_spawn`] does for klines
    /// (via [`perp_split`]). Spot passes `series_symbol == api_symbol`, `is_perp = false`, so the spot
    /// path is byte-identical to the pre-perp behavior.
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let (api_symbol, is_perp) = perp_split(symbol);
        let series_symbol = symbol.to_string();
        self.spawn_with(symbol, "trades", move |_symbol, _interval, ctx| {
            trades_main(series_symbol, api_symbol, is_perp, ctx)
        })
        .map_err(|e| LiveDataError::Subscribe(format!("bybit {symbol} trades: {e}")))
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (bybit live_data.book = false); use subscribe_depth for L2 depth.
        require_live_verb(VENUE, LiveVerb::Book)?;
        unreachable!("bybit declares no lossless book lane")
    }

    /// Start a live L2 depth stream for `symbol` (`orderbook.200`, LINEAR perp). Data flows out through
    /// [`LiveDataSink::l2_snapshot`]; the returned id stops+joins just this stream.
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "depth", depth_main)
            .map_err(|e| LiveDataError::Subscribe(format!("bybit {symbol} depth: {e}")))
    }

    /// Stop + JOIN exactly the stream `id` names — plus its companion perp mark stream when this
    /// was the LAST bars subscription holding that symbol's mark stream; every other subscription
    /// on this `Feeds` keeps running. Unknown ids (already stopped, never issued) are a no-op.
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
    use super::{
        FeedCtx, Feeds, KlineEvent, MarkEvent, mark_subscribe_frame, parse_trades, perp_split,
        relabel_series, route_frame, route_mark_frame, subscribe_frame, trades_subscribe_frame,
    };
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use vike_bridge_core::SubscribeAck;
    use vike_bridge_core::depth::BookOp;
    use vike_data::DataClient;

    // The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
    // RecordingSink copy that used to live here (same canonical `calls()` line forms).
    use vike_data::RecordingSink;

    /// A network-free stand-in for [`super::feed_main`]: just polls its own stop flag, same
    /// cadence as the real feed's WS loop, without ever touching a socket.
    fn fake_feed_body(_symbol: String, _interval: String, ctx: FeedCtx) {
        while !ctx.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn subscribe_returns_distinct_ids() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id1 = feeds.spawn_with("BTCUSDT", "1", fake_feed_body).expect("spawn ok");
        let id2 = feeds.spawn_with("ETHUSDT", "1", fake_feed_body).expect("spawn ok");
        assert_ne!(id1, id2, "distinct ids per subscribe");
        feeds.shutdown();
    }

    #[test]
    fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id1 = feeds.spawn_with("BTCUSDT", "1", fake_feed_body).expect("spawn ok");
        let id2 = feeds.spawn_with("ETHUSDT", "1", fake_feed_body).expect("spawn ok");

        feeds.unsubscribe(id1);
        assert_eq!(feeds.registry.len(), 1, "only the unsubscribed stream is removed");
        assert!(feeds.registry.contains(id2), "the other subscription keeps running");

        feeds.shutdown();
        assert!(feeds.registry.is_empty());
    }

    #[test]
    fn shutdown_joins_every_feed() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        feeds.spawn_with("BTCUSDT", "1", fake_feed_body).expect("spawn ok");
        feeds.spawn_with("ETHUSDT", "1", fake_feed_body).expect("spawn ok");
        feeds.shutdown();
        assert!(feeds.registry.is_empty());
    }

    /// Mark-slot semantics (W2-T4): a linear `tickers` snapshot carries the venue's REAL
    /// `markPrice` → `MarkEvent::Mark` with the exact decimal bits + the frame's top-level `ts`;
    /// a delta that didn't move the mark (no `markPrice` field), a dead mark, the subscribe
    /// ack/reject envelopes, and junk are classified like the kline router's br7 shape.
    #[test]
    fn tickers_mark_frame_classification() {
        let snap = serde_json::json!({
            "topic": "tickers.BTCUSDT",
            "type": "snapshot",
            "cs": 24_987_956_059_i64,
            "ts": 1_673_272_861_686_i64,
            "data": {
                "symbol": "BTCUSDT",
                "markPrice": "16596.00",
                "indexPrice": "16598.54",
                "lastPrice": "16597.00"
            }
        })
        .to_string();
        match route_mark_frame(&snap) {
            MarkEvent::Mark { px, ts } => {
                assert_eq!(px.to_bits(), 16596.00_f64.to_bits(), "markPrice, not lastPrice");
                assert_eq!(ts, 1_673_272_861_686);
            }
            other => panic!("expected Mark, got {other:?}"),
        }
        // a field-sparse delta with no mark move is dropped, not zero-priced
        let delta = r#"{"topic":"tickers.BTCUSDT","type":"delta","ts":1,"data":{"symbol":"BTCUSDT","lastPrice":"16597.50"}}"#;
        assert_eq!(route_mark_frame(delta), MarkEvent::Ignored);
        // a dead (zero) mark is dropped
        let dead = r#"{"topic":"tickers.BTCUSDT","type":"delta","ts":1,"data":{"markPrice":"0"}}"#;
        assert_eq!(route_mark_frame(dead), MarkEvent::Ignored);
        // br7 envelopes: ack confirms, reject is attributable
        assert_eq!(
            route_mark_frame(r#"{"success":true,"op":"subscribe","conn_id":"x"}"#),
            MarkEvent::Ack
        );
        match route_mark_frame(
            r#"{"success":false,"op":"subscribe","ret_msg":"Invalid symbol :tickers.NOPE"}"#,
        ) {
            MarkEvent::Error(m) => assert!(m.contains("Invalid symbol")),
            other => panic!("expected Error, got {other:?}"),
        }
        assert_eq!(route_mark_frame("ping"), MarkEvent::Ignored);
        assert_eq!(
            route_mark_frame(r#"{"topic":"kline.1.BTCUSDT","data":[]}"#),
            MarkEvent::Ignored
        );
    }

    #[test]
    fn mark_subscribe_frame_targets_the_tickers_topic() {
        let f = mark_subscribe_frame("BTCUSDT");
        let v: serde_json::Value = serde_json::from_str(&f).unwrap();
        assert_eq!(v["op"], "subscribe");
        assert_eq!(v["args"][0], "tickers.BTCUSDT");
    }

    /// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `.P` perp pairs a mark stream,
    /// and only while the knob is on. Spot has no venue mark price at all.
    #[test]
    fn only_perps_pair_a_mark_stream_and_only_while_the_knob_is_on() {
        assert!(super::should_pair_mark("BTCUSDT.P", true));
        assert!(!super::should_pair_mark("BTCUSDT", true), "spot has no mark price");
        assert!(!super::should_pair_mark("BTCUSDT.P", false), "VIKE_MARK_STREAMS=0 suppresses");
    }

    /// SPAWN side, network-free: `pair_mark_stream` is the production path `try_spawn` calls (only
    /// `body` differs here). A perp spawns a companion `tickers` mark stream; unsubscribing the
    /// bars id stops BOTH, while unrelated subscriptions keep running.
    #[test]
    fn a_perp_bars_subscription_spawns_and_then_stops_its_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let bars_id = feeds.spawn_with("BTCUSDT.P", "1", fake_feed_body).expect("spawn ok");
        let other_id = feeds.spawn_with("ETHUSDT", "1", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
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
        let bars_id = feeds.spawn_with("BTCUSDT", "1", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTCUSDT", fake_feed_body);
        assert_eq!(feeds.registry.len(), 1, "no companion stream for spot");
        feeds.shutdown();
    }

    /// `VIKE_MARK_STREAMS=0` suppresses the spawn even for a perp — the knob's whole job, pinned.
    #[test]
    fn the_mark_streams_knob_off_suppresses_the_perp_spawn() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        feeds.mark_streams = false;
        let bars_id = feeds.spawn_with("BTCUSDT.P", "1", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
        assert_eq!(feeds.registry.len(), 1, "knob off -> no mark stream even for a perp");
        feeds.shutdown();
    }

    /// Per-symbol dedupe: a 1-min AND a 5-min chart on the same perp share ONE mark socket.
    #[test]
    fn two_intervals_on_one_perp_share_a_single_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let bars_1 = feeds.spawn_with("BTCUSDT.P", "1", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_1, "BTCUSDT.P", fake_feed_body);
        let bars_5 = feeds.spawn_with("BTCUSDT.P", "5", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_5, "BTCUSDT.P", fake_feed_body);
        assert_eq!(feeds.registry.len(), 3, "two bars feeds but only ONE mark stream");

        feeds.unsubscribe(bars_1);
        assert_eq!(feeds.registry.len(), 2, "the mark stream the 5-min chart still needs stays up");
        feeds.unsubscribe(bars_5);
        assert!(feeds.registry.is_empty(), "the last release stops the mark stream too");
        feeds.shutdown();
    }

    #[test]
    fn quotes_and_book_are_unsupported() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        assert!(matches!(
            feeds.subscribe_quotes("BTCUSDT"),
            Err(vike_data::LiveDataError::Unsupported(_))
        ));
        assert!(matches!(
            feeds.subscribe_book("BTCUSDT"),
            Err(vike_data::LiveDataError::Unsupported(_))
        ));
        // subscribe_trades is deliberately NOT asserted here — it is now wired to a real live feed
        // (mirrors subscribe_bars/subscribe_depth, which also aren't exercised via a real subscribe
        // in unit tests since that would touch the network); its pure parser (`parse_trades`) and
        // subscribe-frame builder are covered below instead.
    }

    /// A confirmed Bybit kline frame → a CLOSED bar with the exact o/h/l/c/v bit patterns + ts.
    #[test]
    fn confirmed_kline_classifies_closed_with_exact_bits() {
        let frame = serde_json::json!({
            "topic": "kline.1.BTCUSDT",
            "type": "snapshot",
            "ts": 1_700_000_060_100_i64,
            "data": [{
                "start": 1_700_000_060_000_i64,
                "end": 1_700_000_119_999_i64,
                "interval": "1",
                "open": "27010.25",
                "close": "27080.10",
                "high": "27100.00",
                "low": "27000.00",
                "volume": "8.10000000",
                "turnover": "219000.00",
                "confirm": true,
                "timestamp": 1_700_000_060_100_i64
            }]
        })
        .to_string();
        match route_frame(&frame) {
            KlineEvent::Closed(b) => {
                assert_eq!(b.ts, 1_700_000_060_000);
                assert_eq!(b.open.to_bits(), 27010.25_f64.to_bits());
                assert_eq!(b.high.to_bits(), 27100.00_f64.to_bits());
                assert_eq!(b.low.to_bits(), 27000.00_f64.to_bits());
                assert_eq!(b.close.to_bits(), 27080.10_f64.to_bits());
                assert_eq!(b.volume.to_bits(), 8.1_f64.to_bits());
                assert!(b.symbol.is_none() && b.funding.is_none() && b.bid.is_none());
            }
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    /// A `confirm=false` frame is a still-forming bar (the `forming_bar` sink lane), not a close.
    #[test]
    fn unconfirmed_kline_classifies_forming() {
        let frame = serde_json::json!({
            "topic": "kline.1.BTCUSDT",
            "data": [{
                "start": 1_700_000_120_000_i64,
                "open": "27080.10", "close": "27085.00", "high": "27090.00",
                "low": "27080.00", "volume": "1.5", "confirm": false
            }]
        })
        .to_string();
        match route_frame(&frame) {
            KlineEvent::Forming(b) => {
                assert_eq!(b.ts, 1_700_000_120_000);
                assert_eq!(b.close.to_bits(), 27085.00_f64.to_bits());
                assert_eq!(b.open.to_bits(), 27080.10_f64.to_bits());
            }
            other => panic!("expected Forming, got {other:?}"),
        }
    }

    /// Net-hardening br7: a Bybit subscribe ACK (`{op:"subscribe", success:true}`) now CONFIRMS the
    /// handshake (`KlineEvent::Ack`) instead of being silently dropped — via `FrameOutcome::Confirm`
    /// it disarms the shared driver's subscribe-ack watchdog. A pong-op reply, bare-text `ping`, and
    /// a non-kline topic stay Ignored.
    #[test]
    fn subscribe_ack_is_classified_as_ack_pong_and_junk_ignored() {
        // subscribe success ack → Ack (confirms the subscribe)
        assert_eq!(
            route_frame(r#"{"success":true,"op":"subscribe","conn_id":"x"}"#),
            KlineEvent::Ack
        );
        // a keepalive pong reply (op != subscribe) is ignored
        assert_eq!(
            route_frame(r#"{"success":true,"op":"pong","conn_id":"x","ret_msg":"pong"}"#),
            KlineEvent::Ignored
        );
        // not JSON at all
        assert_eq!(route_frame("ping"), KlineEvent::Ignored);
        // a non-kline topic (well-formed, empty data) is ignored on the topic prefix
        assert_eq!(route_frame(r#"{"topic":"tickers.BTCUSDT","data":[]}"#), KlineEvent::Ignored);
    }

    /// Net-hardening br7: a Bybit subscribe REJECT (`{op:"subscribe", success:false}`) is now an
    /// ATTRIBUTABLE `KlineEvent::Error` carrying the venue's `ret_msg`, NOT silently dropped.
    /// `feed_main` maps it to `FrameOutcome::Fatal`, the session error the driver discloses.
    #[test]
    fn error_frame_is_attributable_not_ignored() {
        match route_frame(
            r#"{"success":false,"op":"subscribe","ret_msg":"Invalid symbol :kline.1.NOPE","conn_id":"x"}"#,
        ) {
            KlineEvent::Error(m) => {
                assert!(m.contains("Invalid symbol"), "carries the Bybit ret_msg: {m}");
            }
            other => panic!("expected an attributable Error, got {other:?}"),
        }
    }

    /// Net-hardening br7 (scripted-clock, deterministic): a subscribe that never acks and never
    /// delivers data trips the `SubscribeAck` watchdog once this venue's declared ack window (the
    /// `MarketPumpSpec` row `pump_opts` consumes) elapses — the attributable "no ack/data" error
    /// the shared driver returns. An ack within the window disarms it.
    #[test]
    fn a_missing_ack_within_the_window_becomes_an_attributable_error() {
        let sub_ack_timeout = vike_bridge_core::pump_spec::market_pump_spec(super::VENUE)
            .knobs()
            .ack_timeout
            .expect("bybit declares the br7 ack watchdog");
        let timeout_ms = sub_ack_timeout.as_millis() as i64;
        let armed_at = 1_000; // scripted "subscribe sent" wall-clock ms
        let ack = SubscribeAck::new(armed_at, timeout_ms);
        assert!(!ack.overdue(armed_at + timeout_ms), "at the deadline, not yet overdue (strict >)");
        assert!(
            ack.overdue(armed_at + timeout_ms + 1),
            "one ms past the {}s window with no ack/data → attributable error",
            sub_ack_timeout.as_secs()
        );
        // the ACK path (route_frame → Ack → confirm) disarms it: no trip however far the clock runs
        let mut acked = SubscribeAck::new(armed_at, timeout_ms);
        assert_eq!(
            route_frame(r#"{"success":true,"op":"subscribe","conn_id":"x"}"#),
            KlineEvent::Ack
        );
        acked.confirm();
        assert!(!acked.overdue(armed_at + timeout_ms * 100), "a confirmed subscribe never trips");
    }

    #[test]
    fn subscribe_frame_targets_the_kline_topic() {
        let f = subscribe_frame("60", "BTCUSDT");
        assert!(f.contains("\"kline.60.BTCUSDT\""), "topic wrong: {f}");
        assert!(f.contains("\"op\":\"subscribe\""));
    }

    // ---- Bybit `publicTrade` channel: parse_trades ----

    #[test]
    fn trades_subscribe_frame_targets_the_public_trade_topic() {
        let f = trades_subscribe_frame("BTCUSDT");
        assert!(f.contains("\"publicTrade.BTCUSDT\""), "topic wrong: {f}");
        assert!(f.contains("\"op\":\"subscribe\""));
    }

    /// A `publicTrade` push with a buy row and a sell row → 2 `TradeTick`s with exact price/size/ts
    /// bit patterns, and `is_buyer_maker` following Binance's `m` convention (true exactly when the
    /// buyer was the MAKER, i.e. the taker/aggressor SOLD): `S=="Buy"` → false, `S=="Sell"` → true.
    /// `symbol` is read off the wire `s` field.
    #[test]
    fn parse_trades_maps_buy_and_sell_rows_with_exact_bits() {
        let payload = serde_json::json!({
            "topic": "publicTrade.BTCUSDT",
            "type": "snapshot",
            "ts": 1_710_000_000_050_i64,
            "data": [
                {
                    "T": 1_710_000_000_000_i64,
                    "s": "BTCUSDT",
                    "S": "Buy",
                    "v": "0.012",
                    "p": "64500.1",
                    "L": "PlusTick",
                    "i": "130639474",
                    "BT": false
                },
                {
                    "T": 1_710_000_000_123_i64,
                    "s": "BTCUSDT",
                    "S": "Sell",
                    "v": "0.5",
                    "p": "64499.9",
                    "L": "MinusTick",
                    "i": "130639475",
                    "BT": false
                }
            ]
        });
        let ticks = parse_trades(&payload);
        assert_eq!(ticks.len(), 2);

        assert_eq!(ticks[0].ts, 1_710_000_000_000);
        assert_eq!(ticks[0].price.to_bits(), 64500.1_f64.to_bits());
        assert_eq!(ticks[0].size.to_bits(), 0.012_f64.to_bits());
        assert!(!ticks[0].is_buyer_maker, "Buy taker → buyer is NOT the maker");
        assert_eq!(ticks[0].symbol, "BTCUSDT");
        assert_eq!(ticks[0].local_ts, 0, "the pure mapper leaves local_ts unstamped");

        assert_eq!(ticks[1].ts, 1_710_000_000_123);
        assert_eq!(ticks[1].price.to_bits(), 64499.9_f64.to_bits());
        assert_eq!(ticks[1].size.to_bits(), 0.5_f64.to_bits());
        assert!(ticks[1].is_buyer_maker, "Sell taker → buyer WAS the maker");
    }

    /// Non-`publicTrade` pushes (a kline data push, and a subscribe ack) yield an empty vec —
    /// `parse_trades` must not misclassify another topic's frame.
    #[test]
    fn parse_trades_ignores_non_trades_pushes() {
        let kline_push = serde_json::json!({
            "topic": "kline.1.BTCUSDT",
            "data": [{"start": 1_700_000_060_000_i64, "open": "1", "close": "1", "high": "1",
                      "low": "1", "volume": "1", "confirm": true}]
        });
        assert!(parse_trades(&kline_push).is_empty());

        let ack = serde_json::json!({"success": true, "op": "subscribe", "conn_id": "x"});
        assert!(parse_trades(&ack).is_empty());

        assert!(parse_trades(&serde_json::json!({})).is_empty());
    }

    /// A malformed/unparseable row (bad price, zero size, unrecognized side, missing field) is
    /// skipped in place — never panics, never produces a bogus tick.
    #[test]
    fn parse_trades_skips_malformed_rows_without_panicking() {
        let payload = serde_json::json!({
            "topic": "publicTrade.BTCUSDT",
            "data": [
                {"T": 1_i64, "s": "BTCUSDT", "S": "Buy", "v": "0.012", "p": "not-a-number"},
                {"T": 1_i64, "s": "BTCUSDT", "S": "Buy", "v": "0", "p": "1.0"},
                {"T": 1_i64, "s": "BTCUSDT", "S": "unknown", "v": "1.0", "p": "1.0"},
                {"s": "BTCUSDT", "S": "Buy", "v": "1.0", "p": "1.0"},
                {"T": 1_i64, "S": "Buy", "v": "1.0", "p": "1.0"},
                "not even an object"
            ]
        });
        assert!(parse_trades(&payload).is_empty());
    }

    /// One bad row alongside a good one: the batch is NOT failed wholesale (per-element tolerance,
    /// mirrors okx's `parse_trades`).
    #[test]
    fn parse_trades_keeps_good_rows_alongside_a_bad_one() {
        let payload = serde_json::json!({
            "topic": "publicTrade.BTCUSDT",
            "data": [
                {"T": 1_i64, "s": "BTCUSDT", "S": "Buy", "v": "1.0", "p": "bad"},
                {"T": 2_i64, "s": "BTCUSDT", "S": "Buy", "v": "1.0", "p": "100.0"}
            ]
        });
        let ticks = parse_trades(&payload);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].ts, 2);
    }

    // ---- DOM depth decode: fold_depth_frame → BookOp (the shared-driver gap wiring) ----

    /// The depth decode over the shared driver: the first `snapshot` seeds the slot (`Updated`),
    /// an in-sequence delta folds (`Updated`), and a GAPPED delta (dropped frame — `u` jumps past
    /// `last_seq + 1`) maps `MdEvent::Resync` → `BookOp::Gap`, on which the driver reconnects and
    /// re-seeds immediately (previously `_ => Ignored` swallowed the corruption until the timed
    /// reseed). The gapped delta must not have folded.
    #[test]
    fn depth_decode_maps_a_seq_gap_to_book_op_gap() {
        let frame = |typ: &str, u: u64, bid_qty: &str| {
            serde_json::json!({
                "topic": "orderbook.200.BTCUSDT", "type": typ, "ts": 1_700_000_000_000_i64,
                "data": {"s":"BTCUSDT","b":[["60000.0",bid_qty],["59999.9","2"]],"a":[["60000.1","4"]],"u":u,"seq":u}
            })
            .to_string()
        };
        let mut slot = None;
        // a delta before the first snapshot is ignored (nothing to fold into)
        assert_eq!(
            super::fold_depth_frame(&frame("delta", 11, "1"), "BTCUSDT", &mut slot),
            BookOp::Ignored
        );
        assert!(slot.is_none());
        // the first snapshot seeds the book
        assert!(matches!(
            super::fold_depth_frame(&frame("snapshot", 10, "5"), "BTCUSDT", &mut slot),
            BookOp::Updated(_)
        ));
        // an in-sequence delta (u = last + 1) folds
        assert!(matches!(
            super::fold_depth_frame(&frame("delta", 11, "8"), "BTCUSDT", &mut slot),
            BookOp::Updated(_)
        ));
        // u jumps 11 → 13: a dropped frame → Gap (and the delta did NOT fold)
        assert_eq!(
            super::fold_depth_frame(&frame("delta", 13, "99"), "BTCUSDT", &mut slot),
            BookOp::Gap
        );
        let book = slot.as_ref().expect("book still present for the driver to discard");
        // best-bid price rides the INFERRED tick grid (float-subtraction tick ⇒ not bit-exact);
        // the qty is exact and is what proves the gapped delta (qty "99") did not fold.
        let (px, qty) = book.best_bid().expect("seeded book has a bid");
        assert!((px - 60000.0).abs() < 1e-3, "best bid price off-grid: {px}");
        assert_eq!(qty, 8.0, "the gapped delta must not fold");
        // a venue-restart regression (u drops far below last) is a Gap too
        assert_eq!(
            super::fold_depth_frame(&frame("delta", 2, "77"), "BTCUSDT", &mut slot),
            BookOp::Gap
        );
    }

    // ---- Bybit `publicTrade` spot-vs-linear perp split (mirrors the kline feed) ----

    /// The `.P` split: a perp symbol strips to the plain exchange symbol (`is_perp = true`); a spot
    /// symbol passes through unchanged (`is_perp = false`) — the same idiom `try_spawn` uses for klines.
    #[test]
    fn perp_split_strips_dot_p_for_the_exchange_symbol() {
        assert_eq!(perp_split("BTCUSDT.P"), ("BTCUSDT".to_string(), true));
        assert_eq!(perp_split("BTCUSDT"), ("BTCUSDT".to_string(), false));
        // only a trailing `.P` triggers it (a bare `P` in the name does not)
        assert_eq!(perp_split("1000PEPEUSDT"), ("1000PEPEUSDT".to_string(), false));
    }

    /// (a) The PERP subscribe topic uses the `.P`-STRIPPED exchange symbol: input `BTCUSDT.P` →
    /// `publicTrade.BTCUSDT` (never `publicTrade.BTCUSDT.P`), because the topic is built from the
    /// `api_symbol` half of `perp_split`.
    #[test]
    fn perp_subscribe_topic_strips_dot_p() {
        let (api_symbol, is_perp) = perp_split("BTCUSDT.P");
        assert!(is_perp);
        let f = trades_subscribe_frame(&api_symbol);
        assert!(f.contains("\"publicTrade.BTCUSDT\""), "perp topic must strip .P: {f}");
        assert!(!f.contains(".P"), "the .P suffix must never reach the exchange topic: {f}");
    }

    /// (b) For a PERP, the emitted `TradeTick.symbol` is the `.P` SERIES label even though the linear
    /// wire `s` is the plain `BTCUSDT` — the `relabel_series` overwrite that keeps the tape's
    /// `TradeStore` key distinct from the spot twin (the store keys on `tick.symbol`, not the sink arg).
    #[test]
    fn perp_emitted_tick_symbol_is_the_dot_p_series_label() {
        // A linear-perp wire push: `s` is the plain exchange symbol, no `.P`.
        let payload = serde_json::json!({
            "topic": "publicTrade.BTCUSDT",
            "type": "snapshot",
            "data": [
                {"T": 1_i64, "s": "BTCUSDT", "S": "Buy", "v": "0.5", "p": "64500.0"},
                {"T": 2_i64, "s": "BTCUSDT", "S": "Sell", "v": "0.25", "p": "64499.0"}
            ]
        });
        let parsed = parse_trades(&payload);
        assert_eq!(parsed.len(), 2);
        assert!(parsed.iter().all(|t| t.symbol == "BTCUSDT"), "wire s is the plain symbol");

        let relabeled = relabel_series(parsed, "BTCUSDT.P");
        assert!(
            relabeled.iter().all(|t| t.symbol == "BTCUSDT.P"),
            "every emitted perp tick carries the .P series key"
        );
        // the re-label touches ONLY `symbol` — price/size/ts/side are untouched
        assert_eq!(relabeled[0].price.to_bits(), 64500.0_f64.to_bits());
        assert!(!relabeled[0].is_buyer_maker && relabeled[1].is_buyer_maker);
    }

    /// (c) SPOT is byte-identical: `series_symbol == wire s`, so `relabel_series` is a no-op overwrite
    /// of the same string — the emitted `TradeTick.symbol` still equals the wire `s`.
    #[test]
    fn spot_relabel_is_a_no_op_byte_identical() {
        let payload = serde_json::json!({
            "topic": "publicTrade.BTCUSDT",
            "data": [{"T": 1_i64, "s": "BTCUSDT", "S": "Buy", "v": "1.0", "p": "100.0"}]
        });
        let parsed = parse_trades(&payload);
        let before = parsed[0].clone();
        let relabeled = relabel_series(parsed, "BTCUSDT"); // spot: series == wire s
        assert_eq!(relabeled[0].symbol, "BTCUSDT");
        // no field changed at all
        assert_eq!(relabeled[0].symbol, before.symbol);
        assert_eq!(relabeled[0].price.to_bits(), before.price.to_bits());
        assert_eq!(relabeled[0].size.to_bits(), before.size.to_bits());
        assert_eq!(relabeled[0].ts, before.ts);
    }
}

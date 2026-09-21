//! Live OKX V5 kline (candlestick) market data → the vike-data live seam — the OKX twin of
//! binance's `market_feed` module (the kline feed implementing [`vike_data::DataClient`], as
//! distinct from [`crate::market_data`], the L2/tick HFT feed).
//!
//! Same shape as the Binance feed: one STOPPABLE thread per (symbol, interval) — REST warmup seed,
//! then the public `candle<bar>` WS. Data flows through the [`vike_data::LiveDataSink`] handed to
//! [`Feeds::new`] at construction: closed bars via `close_bar` (the lossless lane), intrabar
//! forming updates + last-price ticks via `forming_bar`/`bar_close_tick` (the wait-free conflating
//! lane; a `*-SWAP` bars subscription ALSO opens the `mark-price` channel feeding the REAL-mark
//! verb `mark_tick`, default ON via `VIKE_MARK_STREAMS`). Threads poll a PER-SUBSCRIPTION stop
//! flag on a socket read timeout, so
//! [`Feeds::unsubscribe`]/[`Feeds::shutdown`] (via `impl DataClient for Feeds`) join
//! deterministically.
//!
//! **Dedup A6 (wave 3):** the WS session LIFECYCLE (connect + subscribe replay, subscribe-ack
//! watchdog, read-timeout stop poll, stop-aware 30×100 ms reconnect backoff) now rides the shared
//! [`vike_bridge_core::market_pump`] driver, and the per-subscription stop/join bookkeeping rides
//! [`vike_data::FeedRegistry`] — behavior-identical to the pre-driver copy (bybit was the wave-3
//! proof venue; okx is a follow-up conversion). Only the PROTOCOL stays here: the pure frame
//! classifiers ([`route_frame`]/`route_trades_frame`), the `candle<bar>`/`trades` subscribe
//! frames, the REST warmup seed, and the sink emission. The kline/trades channels send NO
//! app-level keepalive (`Keepalive` stays `None` — OKX relies on push frequency here, exactly as
//! before; only the depth feed pings, via its own `run_depth_feed` driver).
//!
//! **Closed-bar detection: the `confirm` flag (authoritative — not ts-rollover).** An OKX WS candle
//! datum is the array `[ts, o, h, l, c, vol, volCcy, volCcyQuote, confirm]` — the SAME shape as the
//! REST `history-candles` row — where `confirm` is `"1"` once the candle is FINAL and `"0"` while it
//! is forming. OKX pushes repeated updates to the in-progress candle (so a naive reader sees the same
//! `ts` many times, then a new `ts` on rollover). Rather than *infer* a close from a `ts` rollover
//! (which would emit the previous bar only when the NEXT one starts, and misclassify the current bar
//! until then), this feed reads OKX's explicit `confirm` marker — present in the candle channel and
//! the exact analogue of Binance's kline `x` / Bybit's `confirm`. `confirm=="1"` → CLOSED bar via
//! `close_bar`; else a forming bar via `forming_bar`.
//!
//! Endpoint: `wss://ws.okx.com:8443/ws/v5/public`. Subscribe:
//! `{"op":"subscribe","args":[{"channel":"candle<bar>","instId":"<SYMBOL>"}]}` where `<bar>` is OKX's
//! bar code ("1m","1H","1D",…) from [`super::data::bar_code`] — the SAME map the REST candle fetcher
//! uses. OKX has no server WS-protocol ping on this channel and relies on candle-push frequency for
//! keepalive; a dropped connection self-heals via the reconnect loop (consistent with the OKX L2
//! feed).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::market_pump::{FrameOutcome, MarketPumpOpts, SessionStatus, run_market_feed};
use vike_bridge_core::pump_spec::market_pump_spec;
use vike_data::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, StreamStatus, SubscriptionId,
    require_live_verb,
};
use vike_model::{Bar, LiveVerb, TradeTick};

use super::data::{bar_code, fetch_klines_latest};

const VENUE: &str = "okx";
const READ_TIMEOUT: Duration = Duration::from_secs(2); // depth driver's stop-flag poll cadence
const SEED_LIMIT: usize = 100; // OKX history-candles caps a single request at 100 rows
/// OKX V5 public WS (the same host the L2 feed uses).
pub const PUBLIC_WS: &str = "wss://ws.okx.com:8443/ws/v5/public";

/// What one decoded OKX candle frame becomes — the `route_frame` classification (mirrors the bybit
/// feed's `KlineEvent`).
#[derive(Debug, Clone, PartialEq)]
pub enum KlineEvent {
    /// A CLOSED bar (`confirm=="1"`) — the lossless `close_bar` sink lane.
    Closed(Bar),
    /// A still-forming bar (`confirm=="0"`) — the conflating `forming_bar` sink lane.
    Forming(Bar),
    /// The venue's subscribe ACK (`event:"subscribe"`) — no data, but it CONFIRMS the subscribe
    /// handshake (net-hardening br7): [`feed_main`] maps it to [`FrameOutcome::Confirm`],
    /// disarming the shared driver's subscribe-ack watchdog.
    Ack,
    /// The venue REJECTED the subscribe (`event:"error"`) — an ATTRIBUTABLE error carrying OKX's
    /// `code`+`msg`, surfaced instead of silently dropped (net-hardening br7). [`feed_main`] maps
    /// it to [`FrameOutcome::Fatal`], the session error the driver discloses before reconnecting.
    Error(String),
    /// A non-candle frame (bare-text `pong`, unrelated channel, other `event`) — dropped.
    Ignored,
}

/// The `i`-th element of an OKX candle row as a `&str`, if present and a string.
fn s(r: &[Value], i: usize) -> Option<&str> {
    r.get(i).and_then(Value::as_str)
}

/// One OKX candle row (`[ts,o,h,l,c,vol,…,confirm]`, decimal strings) → `(Bar, is_closed)`. `None`
/// if a required field is missing/unparseable. `confirm == "1"` ⇒ the candle is FINAL; volume
/// defaults to 0.0 on a parse miss. Uses the shared binance `kline_to_bar` so bars match every venue.
fn row_to_bar(r: &[Value]) -> Option<(Bar, bool)> {
    let ts = s(r, 0)?.parse::<i64>().ok()?;
    let o = s(r, 1)?.parse::<f64>().ok()?;
    let h = s(r, 2)?.parse::<f64>().ok()?;
    let l = s(r, 3)?.parse::<f64>().ok()?;
    let c = s(r, 4)?.parse::<f64>().ok()?;
    let v = s(r, 5).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    let confirmed = s(r, 8) == Some("1"); // OKX confirm flag: "1"=closed, "0"=forming
    Some((kline_to_bar(ts, o, h, l, c, v), confirmed))
}

/// Route ONE OKX WS text frame → a classified kline event. Pure — testable without a socket. An
/// `event` envelope is classified FIRST (net-hardening br7): `event:"error"` → an attributable
/// [`KlineEvent::Error`] carrying OKX's `code`+`msg` (was silently dropped), `event:"subscribe"` →
/// [`KlineEvent::Ack`] (confirms the handshake). Otherwise only a `candle*` channel data frame
/// carries a kline; `confirm=="1"` → [`KlineEvent::Closed`], else [`KlineEvent::Forming`]. Other
/// channels, other `event`s, and the bare-text `pong` keepalive → [`KlineEvent::Ignored`].
pub fn route_frame(text: &str) -> KlineEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return KlineEvent::Ignored; // e.g. the bare-text "pong" keepalive is not JSON
    };
    // The subscribe OUTCOME envelope, checked before the candle path — an OKX subscribe ack also
    // carries `arg.channel` but no `data`, so it must be caught here rather than fall through to the
    // "no data → Ignored" arm that silently dropped both acks AND `event:"error"` rejects (br7).
    if let Some(event) = v.get("event").and_then(Value::as_str) {
        return match event {
            "error" => {
                let code = v.get("code").and_then(Value::as_str).unwrap_or("?");
                let msg = v.get("msg").and_then(Value::as_str).unwrap_or("(no message)");
                KlineEvent::Error(format!("OKX subscribe error {code}: {msg}"))
            }
            "subscribe" => KlineEvent::Ack,
            _ => KlineEvent::Ignored, // e.g. "unsubscribe" — not attributable, not a confirm
        };
    }
    let channel = v.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str);
    if !channel.is_some_and(|c| c.starts_with("candle")) {
        return KlineEvent::Ignored; // non-candle channels
    }
    let Some(row) =
        v.get("data").and_then(Value::as_array).and_then(|d| d.first()).and_then(Value::as_array)
    else {
        return KlineEvent::Ignored;
    };
    let Some((bar, confirmed)) = row_to_bar(row) else {
        return KlineEvent::Ignored;
    };
    if confirmed { KlineEvent::Closed(bar) } else { KlineEvent::Forming(bar) }
}

/// The subscribe frame: the kline stream `candle<bar>` on `inst` (`bar` = OKX's bar code).
pub fn subscribe_frame(bar: &str, inst: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [{"channel": format!("candle{bar}"), "instId": inst}]
    })
    .to_string()
}

struct FeedCtx {
    sink: Arc<dyn LiveDataSink>,
    status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
}

/// The ONE healthy status text every OKX lane publishes — candle, mark and trades alike.
/// One string per VENUE, not per lane: all three lanes share ONE `Arc<Mutex<String>>`, so per-lane
/// spellings would make every session boundary on one lane a text change against the other's. The
/// bybit twin (`crates/bridges/bybit/src/market_feed.rs`'s `LIVE_STATUS`) carries the full
/// argument and the journal-volume hazard it avoids.
///
/// ⚠ **Consequence of moving `trades_main`'s write into the `SessionStatus::Live` arm:** on a mount
/// that subscribes trades and NOT candles, the shared string reads [`Feeds::new`]'s
/// `"connecting to OKX…"` until the first confirmed frame, where it used to claim LIVE before a
/// socket existed. `parse_feed_status` reads that as `Connecting`, which
/// `vike_ops::reconcile_config`'s `health_from_feed_status` maps to `Healthy` — the SAFE direction
/// for a gate that can only suppress. `feed_main` keeps its seed-time write, so the daemon's actual
/// mount (bars) is unaffected.
const LIVE_STATUS: &str = "LIVE · OKX";

impl FeedCtx {
    fn set_status(&self, s: String) {
        *self.status.lock().unwrap() = s;
        (self.wake)();
    }
}

/// The shared [`MarketPumpOpts`] every OKX market pump (kline + trades) runs with — CONSUMED from
/// this venue's `MarketPumpSpec` row ([`market_pump_spec`], row ownership): subscribe replayed per
/// session, NO app-level keepalive on these channels (only the depth feed pings, via its own
/// driver), the br7 10 s subscribe-ack watchdog, no idle watchdog, and the classic stop-aware
/// 30×100 ms reconnect backoff. The knob VALUES live in the row (one edit site); only the
/// subscribe payload is built here.
fn pump_opts(subscribe: &str) -> MarketPumpOpts<'_> {
    market_pump_spec(VENUE).knobs().opts(Some(subscribe), None)
}

/// The kline feed's thread body. `interval` is the ORIGINAL binance-style string ("1m") kept as
/// the series key so series line up across venues; the OKX bar code drives the `candle<bar>`
/// subscribe.
///
/// The WS session/reconnect lifecycle rides the shared [`run_market_feed`] driver (dedup A6);
/// each decoded frame folds through the fixture-tested [`route_frame`] inside the `on_text`
/// closure — a data frame emits under `inst` (last price → the conflated mark cache, then the
/// lossless `close_bar` or conflating `forming_bar` lane) and confirms the br7 subscribe
/// handshake; an ack confirms it dataless; a venue REJECT is surfaced attributably
/// ([`FrameOutcome::Fatal`], never silently dropped).
fn feed_main(inst: String, interval: String, ctx: FeedCtx) {
    let key = format!("{inst}@{interval}");
    // Bar code up-front: an unsupported interval is a permanent config error, so report it and stop.
    let bar = match bar_code(&interval) {
        Ok(b) => b.to_string(),
        Err(e) => {
            ctx.set_status(format!("{key}: {e}"));
            return;
        }
    };
    // REST warmup: newest N CLOSED candles. history-candles is closed-only, so there is NO forming
    // tail to pop — the WS `candle` channel delivers the in-progress bar. Once per feed, NOT per
    // session — a reconnect does not re-seed (the pre-driver behavior).
    match fetch_klines_latest(&inst, &interval, SEED_LIMIT) {
        Ok(bars) => {
            ctx.sink.seed_bars(VENUE, &inst, &interval, bars);
            ctx.set_status(LIVE_STATUS.into());
        }
        Err(e) => ctx.set_status(format!("{key} seed error: {e}")),
    }
    let sub = subscribe_frame(&bar, &inst);
    run_market_feed(
        PUBLIC_WS,
        &pump_opts(&sub),
        &ctx.stop,
        &now_ms,
        |txt| {
            let (candle, confirmed) = match route_frame(txt) {
                KlineEvent::Closed(candle) => (candle, true),
                KlineEvent::Forming(candle) => (candle, false),
                // Subscribe ACK: no data, but it confirms the handshake — disarms the watchdog.
                KlineEvent::Ack => return FrameOutcome::Confirm,
                // Venue REJECTED the subscribe — the attributable session error (was dropped).
                KlineEvent::Error(msg) => return FrameOutcome::Fatal(msg),
                KlineEvent::Ignored => return FrameOutcome::Ignore,
            };
            // last price -> the core's bar-close cache (conflated), same as binance; data also
            // confirms the br7 subscribe handshake. `bar_close_tick`, NOT `mark_tick`: a candle
            // close is not the venue mark (mark-slot semantics; the real mark rides the SWAP
            // `mark-price` pump, `mark_main`).
            ctx.sink.bar_close_tick(VENUE, &inst, candle.close, candle.ts);
            if confirmed {
                // bar CLOSED — lossless lane (a missed close = a series hole)
                ctx.sink.close_bar(VENUE, &inst, &interval, candle);
            } else {
                ctx.sink.forming_bar(VENUE, &inst, &interval, candle);
            }
            (ctx.wake)();
            FrameOutcome::Confirm
        },
        || {}, // no dataless-tick judgment on this lane (the polymarket freshness knob)
        |s| match s {
            // THE FIX: the seed-time write above covers only the FIRST session; this covers every
            // later one. Identical text, so a repeat says nothing.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{key} ws error (reconnecting): {e}"))
            }
        },
    );
}

// --- Perp mark-price feed (mark-slot semantics, W2-T4) ---------------------------------------
// OKX `mark-price` channel: the venue's REAL mark price (the price its liquidation/funding engine
// keys off), pushed on change (and at least every second). Feeds `LiveDataSink::mark_tick` (the
// `PriceBoard` MARK slot); the candle pump's closes ride `bar_close_tick`. SWAP-only: opened
// automatically next to a `*-SWAP` bars subscription (`VIKE_MARK_STREAMS=0` disables). Same host
// + subscribe/ack grammar as the candle feed.

/// What one decoded `mark-price` frame becomes — [`route_frame`]'s mark-pump twin.
#[derive(Debug, Clone, PartialEq)]
pub enum MarkEvent {
    /// A fresh mark: `data[0].markPx` + `data[0].ts` (both decimal strings on the wire).
    Mark { px: f64, ts: i64 },
    /// The venue's subscribe ACK (`event:"subscribe"`) — confirms the br7 handshake.
    Ack,
    /// The venue REJECTED the subscribe (`event:"error"`) — attributable, carries `code`+`msg`.
    Error(String),
    /// A non-`mark-price` frame, a malformed row, or a dead (0/neg/NaN) mark — dropped.
    Ignored,
}

/// Route ONE OKX WS text frame from the `mark-price` channel → a classified mark event. Pure —
/// fixture-tested without a socket; the `event` envelope handling mirrors [`route_frame`] (br7).
pub fn route_mark_frame(text: &str) -> MarkEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return MarkEvent::Ignored; // the bare-text "pong" keepalive is not JSON
    };
    if let Some(event) = v.get("event").and_then(Value::as_str) {
        return match event {
            "error" => {
                let code = v.get("code").and_then(Value::as_str).unwrap_or("?");
                let msg = v.get("msg").and_then(Value::as_str).unwrap_or("(no message)");
                MarkEvent::Error(format!("OKX subscribe error {code}: {msg}"))
            }
            "subscribe" => MarkEvent::Ack,
            _ => MarkEvent::Ignored,
        };
    }
    let channel = v.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str);
    if channel != Some("mark-price") {
        return MarkEvent::Ignored;
    }
    let Some(row) = v.get("data").and_then(Value::as_array).and_then(|d| d.first()) else {
        return MarkEvent::Ignored;
    };
    let Some(px) = row_str(row, "markPx").and_then(|s| s.parse::<f64>().ok()) else {
        return MarkEvent::Ignored;
    };
    if !vike_bridge_core::is_valid_mark(px) {
        return MarkEvent::Ignored;
    }
    let ts = row_str(row, "ts").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    MarkEvent::Mark { px, ts }
}

/// The subscribe frame for the public `mark-price` channel on `inst` — same
/// `{"op":"subscribe",...}` shape as [`subscribe_frame`] (candles), no bar code.
pub fn mark_subscribe_frame(inst: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [{"channel": "mark-price", "instId": inst}]
    })
    .to_string()
}

/// The SWAP mark-price feed-thread body — [`feed_main`]'s mark twin on the same shared driver.
/// Emits under `inst` (the same key the candle feed uses). No REST seed — a live-only valuation
/// stream; the resolver's bar-close fallback covers the pre-first-frame window.
fn mark_main(inst: String, _interval: String, ctx: FeedCtx) {
    let sub = mark_subscribe_frame(&inst);
    run_market_feed(
        PUBLIC_WS,
        &pump_opts(&sub),
        &ctx.stop,
        &now_ms,
        |txt| match route_mark_frame(txt) {
            MarkEvent::Mark { px, ts } => {
                ctx.sink.mark_tick(VENUE, &inst, px, ts);
                (ctx.wake)();
                FrameOutcome::Confirm
            }
            MarkEvent::Ack => FrameOutcome::Confirm,
            MarkEvent::Error(msg) => FrameOutcome::Fatal(msg),
            MarkEvent::Ignored => FrameOutcome::Ignore,
        },
        || {},
        |s| match s {
            // This lane had no healthy string at all — a pure-degradation writer on the shared
            // mutex. Same text as the candle and trades lanes (see `LIVE_STATUS`).
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{inst} mark ws error (reconnecting): {e}"))
            }
        },
    );
}

// --- Trades feed (live prints) ---------------------------------------------------------------
// OKX `trades` channel: executed prints (no snapshot/seed — a live-only tape, unlike the
// `candle*`/`books` channels which each have a REST/WS seed counterpart). Same host + subscribe
// shape as the candle feed; feeds `vike_model::TradeTick` through `LiveDataSink::trade`, the
// input the app's client-side tick/volume-bar aggregation reads (a follow-up task wires that up).
//
// Wire shape: `{"arg":{"channel":"trades","instId":"BTC-USDT"},"data":[{"instId":"BTC-USDT",
// "tradeId":"130639474","px":"64500.1","sz":"0.012","side":"buy","ts":"1710000000000"}]}` — a
// push may carry more than one print per frame. Field mapping mirrors binance's `@aggTrade`
// mapper (`vike_binance::trades::map_wire`) so both venues' ticks are shaped identically:
//
// | wire key | -> | `TradeTick` field |
// |----------|----|--------------------|
// | `px`     |    | `price` — decimal string -> f64 |
// | `sz`     |    | `size` — decimal string -> f64 |
// | `ts`     |    | `ts` — decimal-string epoch-ms -> i64 |
// | `side`   |    | `is_buyer_maker` — OKX's `side` is the TAKER/aggressor side; Binance's `m` |
// |          |    | convention is `true` exactly when the BUYER was the MAKER (i.e. the |
// |          |    | taker/aggressor SOLD) — so `side=="sell"` -> `true`, `side=="buy"` -> `false`. |
// | (caller) |    | `symbol` — never read off a per-row field beyond validating it's present; the |
// |          |    | pump always emits under its own subscribed `inst`, same convention as `close_bar`/`forming_bar`/`mark_tick` above. |
// | `tradeId`|    | (not carried — `vike_model::TradeTick` has no trade-id field) |
//
// `local_ts` is stamped by the pump at receive time (0 from the pure mapper — deterministic/
// testable, same "0 = not stamped" convention `vike_binance::trades::map_wire` documents).
//
// **Non-finite/non-positive guard:** a decoded price/size that fails to parse, is non-finite, or
// is `<= 0.0` is rejected in place (mirrors binance's `is_valid_trade` — a downstream volume fold
// would spin on `+Inf` or corrupt on a zero/garbage size).

/// The `i`-th element of a slice of trade rows, but for the pure per-row string-field reads below
/// — kept local to this section rather than reusing the candle `s()` helper (which is for a
/// `&[Value]` array row; trades rows are `Value` objects keyed by name).
fn row_str<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

/// The shared non-finite/non-positive guard (mirrors `vike_binance::trades::is_valid_trade`).
fn is_valid_trade(price: f64, size: f64) -> bool {
    price.is_finite() && price > 0.0 && size.is_finite() && size > 0.0
}

/// One `trades` channel data row -> a `TradeTick` (`local_ts` left at the "not stamped" sentinel
/// `0` — the pump stamps machine receive time just before each `sink.trade` emit, same convention
/// as binance's aggTrade mapper). `None` on a missing/unparseable field, a non-finite/non-positive
/// price or size, or an unrecognized `side`.
fn row_to_trade_tick(row: &Value) -> Option<TradeTick> {
    let inst = row_str(row, "instId")?;
    let price = row_str(row, "px")?.parse::<f64>().ok()?;
    let size = row_str(row, "sz")?.parse::<f64>().ok()?;
    if !is_valid_trade(price, size) {
        return None;
    }
    let ts = row_str(row, "ts")?.parse::<i64>().ok()?;
    let is_buyer_maker = match row_str(row, "side")? {
        "sell" => true,
        "buy" => false,
        _ => return None,
    };
    Some(TradeTick { ts, local_ts: 0, price, size, is_buyer_maker, symbol: inst.to_string() })
}

/// PURE: one OKX `trades` channel WS push -> zero or more `TradeTick`s (fixture-tested, no
/// socket). Anything that is not a `trades`-channel data push — a non-`trades` channel, the
/// subscribe-ack/error `event` envelope (no `data` field), or a frame with no/empty `data` array —
/// yields an empty vec; a malformed/unparseable individual row is skipped in place rather than
/// failing the whole push (mirrors `vike_binance::trades::rest_agg_trades`'s per-element
/// tolerance).
pub fn parse_trades(payload: &Value) -> Vec<TradeTick> {
    let channel = payload.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str);
    if channel != Some("trades") {
        return Vec::new();
    }
    let Some(rows) = payload.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter().filter_map(row_to_trade_tick).collect()
}

/// The subscribe frame for the public `trades` channel on `inst` — same `{"op":"subscribe",...}`
/// shape as [`subscribe_frame`] (candles), just the `trades` channel with no bar code.
pub fn trades_subscribe_frame(inst: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [{"channel": "trades", "instId": inst}]
    })
    .to_string()
}

/// [`route_frame`]'s trades-pump twin: classifies ONE raw WS text frame for [`trades_main`].
/// An `event` envelope is checked first (same net-hardening br7 shape as the candle path):
/// `event:"error"` -> an attributable [`TradesFrame::Error`], `event:"subscribe"` ->
/// [`TradesFrame::Ack`] (disarms the subscribe-ack watchdog). Otherwise [`parse_trades`] decodes
/// the push; an empty result (non-trades channel, or every row malformed) is
/// [`TradesFrame::Ignored`].
enum TradesFrame {
    Ticks(Vec<TradeTick>),
    Ack,
    Error(String),
    Ignored,
}

fn route_trades_frame(text: &str) -> TradesFrame {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return TradesFrame::Ignored; // e.g. the bare-text "pong" keepalive is not JSON
    };
    if let Some(event) = v.get("event").and_then(Value::as_str) {
        return match event {
            "error" => {
                let code = v.get("code").and_then(Value::as_str).unwrap_or("?");
                let msg = v.get("msg").and_then(Value::as_str).unwrap_or("(no message)");
                TradesFrame::Error(format!("OKX subscribe error {code}: {msg}"))
            }
            "subscribe" => TradesFrame::Ack,
            _ => TradesFrame::Ignored,
        };
    }
    let ticks = parse_trades(&v);
    if ticks.is_empty() { TradesFrame::Ignored } else { TradesFrame::Ticks(ticks) }
}

/// `subscribe_trades`'s thread body — the trades twin of [`feed_main`], on the same shared
/// [`run_market_feed`] driver (dedup A6): identical subscribe-replay, br7 ack-watchdog, stop-poll,
/// and 30×100 ms backoff lifecycle, scoped down to prints (no closed/forming bar routing, no
/// mark-tick). Live-only: unlike [`feed_main`]/[`depth_main`], there is no REST warmup/seed here —
/// OKX's `trades` channel has no historical REST counterpart threaded into this feed, and a
/// tick/volume-bar builder needs no seed to start folding live prints.
fn trades_main(inst: String, _interval: String, ctx: FeedCtx) {
    let sub = trades_subscribe_frame(&inst);
    run_market_feed(
        PUBLIC_WS,
        &pump_opts(&sub),
        &ctx.stop,
        &now_ms,
        |txt| match route_trades_frame(txt) {
            TradesFrame::Ticks(ticks) => {
                for mut tick in ticks {
                    tick.local_ts = now_ms(); // machine receive time (dual-timestamp capture)
                    ctx.sink.trade(VENUE, &inst, tick);
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
            // Replaces the spawn-time `"LIVE · OKX trades {inst}"` write — a LIVE claim made
            // before a socket existed, the mirror image of the kline latch.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{inst} trades ws error (reconnecting): {e}"))
            }
        },
    );
}

// --- L2 depth feed (DOM) --------------------------------------------------------------------
// OKX `books` channel (400-level): a WS `snapshot` then `update`s — no REST seed. Reuses
// `crate::market_data::parse_books_frame` for the PROTOCOL + the shared `vike_bridge_core::depth`
// driver for the LIFECYCLE. Gap detection via `prevSeqId`; the CRC32 `checksum` IS validated by
// `okx_depth_decode` (audit br1) against a raw-string mirror of the merged book — a mismatch resyncs
// through the SAME gap path (`BookOp::Gap`), so it self-heals AND discloses `GapStart`. NOTE: OKX
// deprecated the field (fixed to 0) on 2026-06-23, so validation is skipped for a 0 checksum and is
// effectively dormant against live OKX — see `crate::book_checksum`. Delivers the top `DEPTH_LEVELS`
// via `LiveDataSink::l2_snapshot`.
use crate::book_checksum::ChecksumBook;
use crate::market_data::{
    BooksFrame, PUBLIC_WS as MD_PUBLIC_WS, parse_books_frame, parse_raw_books,
};
use vike_bridge_core::depth::{BookOp, infer_tick_size, run_depth_feed};
use vike_bridge_core::stream_health::HealthEvent;

/// Levels per side published to the DOM (OKX `books` carries 400).
const DEPTH_LEVELS: usize = 400;
/// Reconnect backoff after a socket error / gap.
const DEPTH_BACKOFF: Duration = Duration::from_secs(3);
/// OKX drops a client that doesn't ping within ~30 s — the driver sends this raw text every ~15 s.
const KEEPALIVE: &str = "ping";
/// Net-hardening §B dead-transport watchdog: no `books` frame of ANY kind (a snapshot/update OR the
/// raw `pong` reply to our keepalive) for this long means the socket is silently dead → the driver
/// discloses a gap and reconnects+re-seeds. Well above the 15 s keepalive cadence, so a quiet book
/// kept alive purely by ping/pong never false-trips; only true transport silence does.
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
/// Net-hardening timed book re-seed: forced fresh-snapshot cadence. `okx_depth_decode` now DOES
/// validate the CRC32 `checksum` (audit br1), which catches a dropped/mis-applied `update` that does
/// NOT break the `prevSeqId` chain — the silent-corruption class no `BookOp::Gap`/idle/freshness trip
/// sees. BUT OKX deprecated that field on 2026-06-23 (fixed to `0`, see `crate::book_checksum`), so on
/// a live OKX stream the checksum is skipped and this timed re-seed is once again the ONLY backstop for
/// silent corruption: ending the session every `DEPTH_RESEED_INTERVAL` forces reconnect + re-subscribe
/// (a fresh WS `snapshot` rebuilds the book), bounding the corruption window to <= this — reusing the
/// gap handler's reconnect path but disclosing NO transport gap (a scheduled refresh, not an outage).
/// 5 min is a conservative default, well above the reconnect cost; `None` would disable it.
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

/// The venue EVENT-TIME (epoch-ms) of an OKX `books` frame — its `data[0].ts`, a decimal STRING — for
/// the §B data-freshness watchdog (`0` if absent/unparsable → the driver falls back to receive-time).
/// Re-parses just that field from the raw frame; [`BooksFrame`]/`parse_books_frame` don't surface it,
/// and this keeps them (and their fixtures) untouched.
fn frame_ts_ms(txt: &str) -> i64 {
    serde_json::from_str::<serde_json::Value>(txt)
        .ok()
        .and_then(|v| {
            v.get("data")
                .and_then(|d| d.as_array())
                .and_then(|a| a.first())
                .and_then(|d0| d0.get("ts"))
                .and_then(|t| t.as_str())
                .and_then(|s| s.parse::<i64>().ok())
        })
        .unwrap_or(0)
}

/// Publish the top [`DEPTH_LEVELS`] to the DOM sink (carrying the book's tick) + nudge a repaint.
fn publish_book(book: &vike_model::L2Book, inst: &str, ctx: &FeedCtx) {
    let (bids, asks) = book.top_n(DEPTH_LEVELS);
    ctx.sink.l2_snapshot(VENUE, inst, book.tick_size, bids, asks, now_ms());
    (ctx.wake)();
}

/// `Some(expected)` when a frame carries a REAL (non-zero) `checksum` that DISAGREES with the mirror —
/// the resync trigger; `None` when it matches, is absent, or is the deprecated `0`. This is the ONE
/// place the 2026-06-23 deprecation guard (`checksum == 0` ⇒ skip) lives, so a live OKX stream (which
/// zeroes the field) can never false-trip a resync. See [`crate::book_checksum`].
fn checksum_mismatch(cbook: &ChecksumBook, checksum: Option<i32>) -> Option<i32> {
    match checksum {
        Some(cs) if cs != 0 && !cbook.verify(cs) => Some(cs),
        _ => None,
    }
}

/// Fold ONE OKX `books` frame into BOTH the live [`vike_model::L2Book`] and the raw-string checksum
/// mirror, then validate the CRC32 (audit br1). Pure over (`cbook`, `book`) — testable without a
/// socket, the depth twin of [`crate::market_data::route_frame`]. A `snapshot` rebuilds both (also
/// re-syncing the mirror after a reconnect); an `update` merges the delta into each in lockstep. When
/// a NON-zero `checksum` disagrees with the merged top-25 — silent corruption the `prevSeqId` chain
/// cannot see — it returns [`BookOp::Gap`], so the shared depth driver discards the book, reconnects +
/// re-seeds, and discloses the untrustworthy book via the SAME `GapStart` the seq-gap path uses. A `0`
/// checksum (OKX's deprecated field, 2026-06-23) is skipped, so live OKX is unaffected.
pub(crate) fn okx_depth_decode(
    cbook: &mut ChecksumBook,
    txt: &str,
    book: &mut Option<vike_model::L2Book>,
    inst: &str,
) -> BookOp {
    match parse_books_frame(txt) {
        BooksFrame::Snapshot { seq, bids, asks } => {
            let mut bk = vike_model::L2Book::new(infer_tick_size(&bids, &asks));
            bk.apply_snapshot(seq, &bids, &asks);
            *book = Some(bk);
            // Rebuild the raw-string mirror from the SAME snapshot, then validate it (a corrupted
            // snapshot is possible too). A snapshot always resets the mirror, so this re-syncs it
            // after a reconnect.
            if let Some(raw) = parse_raw_books(txt) {
                cbook.apply_snapshot(&raw.bids, &raw.asks);
                if let Some(expected) = checksum_mismatch(cbook, raw.checksum) {
                    tracing::warn!(
                        target: "vike_okx",
                        inst,
                        expected,
                        "OKX books snapshot CRC32 mismatch — book untrustworthy, resyncing"
                    );
                    return BookOp::Gap;
                }
            }
            BookOp::Updated(frame_ts_ms(txt)) // §B: venue event-time from the frame's `ts`
        }
        BooksFrame::Update { seq, prev_seq, bids, asks } => match book.as_mut() {
            Some(bk) => {
                if prev_seq != 0 && prev_seq != bk.last_seq {
                    BookOp::Gap // sequence chain broken — resync
                } else if bk.apply_delta(seq, &bids, &asks) {
                    // The delta hit the f64 book → mirror the SAME delta into the checksum book
                    // (lockstep), then validate the merged top-25 against the frame's checksum.
                    if let Some(raw) = parse_raw_books(txt) {
                        cbook.apply_delta(&raw.bids, &raw.asks);
                        if let Some(expected) = checksum_mismatch(cbook, raw.checksum) {
                            tracing::warn!(
                                target: "vike_okx",
                                inst,
                                expected,
                                "OKX books update CRC32 mismatch — book untrustworthy, resyncing"
                            );
                            return BookOp::Gap;
                        }
                    }
                    BookOp::Updated(frame_ts_ms(txt))
                } else {
                    BookOp::Ignored // stale (seqId <= last)
                }
            }
            None => BookOp::Ignored, // an update before the snapshot
        },
        BooksFrame::Other => BookOp::Ignored, // ack / pong / non-books frame
    }
}

/// Depth feed body on the shared driver. OKX's protocol is WS-seeded: the `action:"snapshot"`
/// frame builds the book (tick inferred from it); `action:"update"` frames fold as deltas, with a
/// `prevSeqId` chain check that surfaces a break as [`BookOp::Gap`] → the driver reconnects + re-seeds.
/// Each frame's CRC32 `checksum` is validated by [`okx_depth_decode`] (a mismatch also → `Gap`).
fn depth_main(inst: String, _interval: String, ctx: FeedCtx) {
    let sub = format!(r#"{{"op":"subscribe","args":[{{"channel":"books","instId":"{inst}"}}]}}"#);
    let sym = inst.as_str();
    let seed = || None::<vike_model::L2Book>;
    // The raw-string checksum mirror persists ACROSS reconnects inside this `FnMut`; every OKX
    // `snapshot` (the first frame of each session) rebuilds it, so it never desyncs. See
    // `okx_depth_decode`.
    let mut cbook = ChecksumBook::new();
    let decode = move |txt: &str, book: &mut Option<vike_model::L2Book>| {
        okx_depth_decode(&mut cbook, txt, book, sym)
    };
    let publish = |b: &vike_model::L2Book| publish_book(b, sym, &ctx);
    // Net-hardening §B: map each driver-neutral `HealthEvent` onto the sink's machine-readable
    // disclosure. The depth driver owns the `StreamHealth` that dedups transport gaps across
    // reconnects (one outage → one GapStart/Live pair) now; the venue just maps + discloses.
    let on_health = |ev: HealthEvent| {
        ctx.sink.stream_status(VENUE, sym, "depth", health_to_stream_status(ev));
    };
    run_depth_feed(
        MD_PUBLIC_WS,
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
    /// Whether a SWAP `subscribe_bars` also opens the `mark-price` stream — resolved once at
    /// construction from `VIKE_MARK_STREAMS` (default ON; mark-slot semantics, W2-T4).
    mark_streams: bool,
    /// PER-SYMBOL reference-counted companion mark streams (the mark pump has no id of its own at
    /// the `DataClient` seam), so charting one SWAP at two intervals opens ONE mark socket and
    /// [`Feeds::unsubscribe`] stops it only when the last bars subscription releases it.
    mark_pairings: vike_bridge_core::MarkPairings<SubscriptionId>,
}

/// Whether a `subscribe_bars` on `inst` should ALSO open the `mark-price` stream: a `*-SWAP`
/// perpetual AND the `VIKE_MARK_STREAMS` knob. Pure — the ONE place the pairing predicate lives,
/// so the spawn site and its tests read the same law.
///
/// SCOPE, deliberately narrow: OKX also publishes `mark-price` for DATED FUTURES
/// (`BTC-USD-240329`) and for OPTIONS, and the decoder would handle those rows unchanged — the
/// channel shape is identical. They are excluded because `*-SWAP` is the only contract class the
/// rest of this bridge charts and values today; a dated-future or option position therefore keeps
/// valuing through the resolver's bar-close rung. Widening this predicate needs no decoder change,
/// only an instId classifier that does not mistake a SPOT pair (`BTC-USDT`, which has NO mark
/// price) for a dated contract.
fn should_pair_mark(inst: &str, mark_streams: bool) -> bool {
    mark_streams && inst.ends_with("-SWAP")
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
            status: Arc::new(Mutex::new("connecting to OKX…".into())),
            wake: Arc::new(wake),
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    "okx",
                );
            }),
            mark_streams: vike_bridge_core::mark_streams_enabled(),
            mark_pairings: Default::default(),
        }
    }

    /// Fallible spawn of the feed thread for `(inst, interval)`: allocates a fresh
    /// [`SubscriptionId`] and a dedicated stop flag, then runs [`feed_main`] on its own thread —
    /// an OS thread-spawn failure is returned (not panicked) so [`DataClient::subscribe_bars`] can
    /// map it to a [`LiveDataError`].
    ///
    /// A `*-SWAP` inst (an OKX perpetual) also opens the venue's REAL `mark-price` stream — see
    /// [`should_pair_mark`] for the predicate and its deliberate dated-futures exclusion.
    pub fn try_spawn(&mut self, inst: &str, interval: &str) -> std::io::Result<SubscriptionId> {
        let id = self.spawn_with(inst, interval, feed_main)?;
        self.pair_mark_stream(id, inst, mark_main);
        Ok(id)
    }

    /// Attach the venue's REAL `mark-price` stream (mark-slot semantics — default ON,
    /// `VIKE_MARK_STREAMS=0` disables) to the bars subscription `bars_id` just created for `inst`.
    /// Spawns at most ONE mark socket per inst ([`vike_bridge_core::MarkPairings`]); a second bars
    /// subscription on the same SWAP only reference-counts the running one. Fail-soft: if the
    /// extra thread can't spawn, the bars feed stays live and valuation falls back to the
    /// resolver's bar-close rung, exactly the no-mark-stream behavior. Production and the pairing
    /// tests share this method — only `body` differs (tests pass a network-free stand-in).
    fn pair_mark_stream(
        &mut self,
        bars_id: SubscriptionId,
        inst: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) {
        if self.mark_pairings.attach(bars_id, inst, should_pair_mark(inst, self.mark_streams)) {
            return; // disabled/non-SWAP, or an existing stream was reference-counted
        }
        if let Ok(mid) = self.spawn_with(inst, "mark", body) {
            self.mark_pairings.record(bars_id, inst, mid);
        }
    }

    /// Shared per-key spawn bookkeeping, now one [`FeedRegistry::spawn`] call (dedup A6): the
    /// registry allocates the id + stop flag and owns the join handle; this venue wrapper only
    /// assembles its own [`FeedCtx`] around the registry-issued stop flag. `body` is the shared
    /// `feed_main` in production; tests substitute a network-free stand-in to exercise the per-key
    /// lifecycle deterministically without a real venue connection.
    fn spawn_with(
        &mut self,
        inst: &str,
        interval: &str,
        body: impl FnOnce(String, String, FeedCtx) + Send + 'static,
    ) -> std::io::Result<SubscriptionId> {
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        let (inst, interval) = (inst.to_string(), interval.to_string());
        self.registry.spawn(format!("feed-okx-{inst}@{interval}"), move |stop| {
            let ctx = FeedCtx { sink, status, wake, stop };
            body(inst, interval, ctx)
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
            .map_err(|e| LiveDataError::Subscribe(format!("okx {symbol}@{interval}: {e}")))
    }

    fn subscribe_quotes(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (okx live_data.quotes = false): stays in lockstep with the table.
        require_live_verb(VENUE, LiveVerb::Quotes)?;
        unreachable!("okx declares no live quotes")
    }

    /// Start a live `trades` stream for `inst` (the OKX public `trades` channel — executed
    /// prints, no REST seed). Data flows out through [`LiveDataSink::trade`]; the returned id
    /// stops+joins just this stream (same per-key bookkeeping as `subscribe_bars`/
    /// `subscribe_depth`).
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(symbol, "trades", trades_main)
            .map_err(|e| LiveDataError::Subscribe(format!("okx {symbol} trades: {e}")))
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (okx live_data.book = false); use subscribe_depth for L2 depth.
        require_live_verb(VENUE, LiveVerb::Book)?;
        unreachable!("okx declares no lossless book lane")
    }

    /// Start a live L2 depth stream for `inst` (the OKX `books` 400-level channel). Data flows out
    /// through [`LiveDataSink::l2_snapshot`]; the returned id stops+joins just this stream.
    fn subscribe_depth(&mut self, inst: &str) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(inst, "depth", depth_main)
            .map_err(|e| LiveDataError::Subscribe(format!("okx {inst} depth: {e}")))
    }

    /// Stop + JOIN exactly the stream `id` names — plus its companion SWAP mark stream when this
    /// was the LAST bars subscription holding that inst's mark stream; every other subscription on
    /// this `Feeds` keeps running. Unknown ids (already stopped, never issued) are a no-op.
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
        FeedCtx, Feeds, KlineEvent, MarkEvent, mark_subscribe_frame, okx_depth_decode,
        parse_trades, route_frame, route_mark_frame, subscribe_frame, trades_subscribe_frame,
    };
    use crate::book_checksum::ChecksumBook;
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
    fn fake_feed_body(_inst: String, _interval: String, ctx: FeedCtx) {
        while !ctx.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn subscribe_returns_distinct_ids() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id1 = feeds.spawn_with("BTC-USDT", "1m", fake_feed_body).expect("spawn ok");
        let id2 = feeds.spawn_with("ETH-USDT", "1m", fake_feed_body).expect("spawn ok");
        assert_ne!(id1, id2, "distinct ids per subscribe");
        feeds.shutdown();
    }

    #[test]
    fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let id1 = feeds.spawn_with("BTC-USDT", "1m", fake_feed_body).expect("spawn ok");
        let id2 = feeds.spawn_with("ETH-USDT", "1m", fake_feed_body).expect("spawn ok");

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
        feeds.spawn_with("BTC-USDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.spawn_with("ETH-USDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.shutdown();
        assert!(feeds.registry.is_empty());
    }

    /// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `*-SWAP` pairs a mark stream,
    /// and only while the knob is on. Spot has no mark price; dated futures are deliberately out
    /// of scope (see [`super::should_pair_mark`]).
    #[test]
    fn only_swaps_pair_a_mark_stream_and_only_while_the_knob_is_on() {
        assert!(super::should_pair_mark("BTC-USDT-SWAP", true));
        assert!(!super::should_pair_mark("BTC-USDT", true), "spot has no mark price");
        assert!(!super::should_pair_mark("BTC-USD-240329", true), "dated futures are out of scope");
        assert!(!super::should_pair_mark("BTC-USDT-SWAP", false), "VIKE_MARK_STREAMS=0 suppresses");
    }

    /// SPAWN side, network-free: `pair_mark_stream` is the production path `try_spawn` calls (only
    /// `body` differs here). A SWAP spawns a companion `mark-price` stream; unsubscribing the bars
    /// id stops BOTH, while unrelated subscriptions keep running.
    #[test]
    fn a_swap_bars_subscription_spawns_and_then_stops_its_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let bars_id = feeds.spawn_with("BTC-USDT-SWAP", "1m", fake_feed_body).expect("spawn ok");
        let other_id = feeds.spawn_with("ETH-USDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTC-USDT-SWAP", fake_feed_body);
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
        let bars_id = feeds.spawn_with("BTC-USDT", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTC-USDT", fake_feed_body);
        assert_eq!(feeds.registry.len(), 1, "no companion stream for spot");
        feeds.shutdown();
    }

    /// `VIKE_MARK_STREAMS=0` suppresses the spawn even for a SWAP — the knob's whole job, pinned.
    #[test]
    fn the_mark_streams_knob_off_suppresses_the_swap_spawn() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        feeds.mark_streams = false;
        let bars_id = feeds.spawn_with("BTC-USDT-SWAP", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_id, "BTC-USDT-SWAP", fake_feed_body);
        assert_eq!(feeds.registry.len(), 1, "knob off -> no mark stream even for a SWAP");
        feeds.shutdown();
    }

    /// Per-symbol dedupe: a 1m AND a 5m chart on the same SWAP share ONE mark socket.
    #[test]
    fn two_intervals_on_one_swap_share_a_single_mark_stream() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        let bars_1m = feeds.spawn_with("BTC-USDT-SWAP", "1m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_1m, "BTC-USDT-SWAP", fake_feed_body);
        let bars_5m = feeds.spawn_with("BTC-USDT-SWAP", "5m", fake_feed_body).expect("spawn ok");
        feeds.pair_mark_stream(bars_5m, "BTC-USDT-SWAP", fake_feed_body);
        assert_eq!(feeds.registry.len(), 3, "two bars feeds but only ONE mark stream");

        feeds.unsubscribe(bars_1m);
        assert_eq!(feeds.registry.len(), 2, "the mark stream the 5m chart still needs stays up");
        feeds.unsubscribe(bars_5m);
        assert!(feeds.registry.is_empty(), "the last release stops the mark stream too");
        feeds.shutdown();
    }

    #[test]
    fn quotes_and_book_are_unsupported() {
        let sink = Arc::new(RecordingSink::default());
        let mut feeds = Feeds::new(sink, || {});
        assert!(matches!(
            feeds.subscribe_quotes("BTC-USDT"),
            Err(vike_data::LiveDataError::Unsupported(_))
        ));
        assert!(matches!(
            feeds.subscribe_book("BTC-USDT"),
            Err(vike_data::LiveDataError::Unsupported(_))
        ));
        // subscribe_trades is deliberately NOT asserted here — Task okxtrades wired it to a real
        // live feed (mirrors binance's subscribe_bars/subscribe_depth, which also aren't exercised
        // via a real subscribe call in unit tests since that would touch the network); its pure
        // parser (`parse_trades`) and subscribe-frame builder are covered below instead.
    }

    fn candle_frame(row: serde_json::Value) -> String {
        serde_json::json!({
            "arg": {"channel": "candle1m", "instId": "BTC-USDT"},
            "data": [row]
        })
        .to_string()
    }

    /// A `confirm="1"` candle → a CLOSED bar with exact o/h/l/c/v bit patterns + ts.
    #[test]
    fn confirmed_candle_classifies_closed_with_exact_bits() {
        // [ts, o, h, l, c, vol, volCcy, volCcyQuote, confirm]
        let frame = candle_frame(serde_json::json!([
            "1700000060000",
            "27010.25",
            "27100.00",
            "27000.00",
            "27080.10",
            "8.10000000",
            "219000.00",
            "5913000.00",
            "1"
        ]));
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

    /// A `confirm="0"` candle is a still-forming bar (the `forming_bar` sink lane), not a close.
    #[test]
    fn unconfirmed_candle_classifies_forming() {
        let frame = candle_frame(serde_json::json!([
            "1700000120000",
            "27080.10",
            "27090.00",
            "27080.00",
            "27085.00",
            "1.5",
            "40000.0",
            "40000.0",
            "0"
        ]));
        match route_frame(&frame) {
            KlineEvent::Forming(b) => {
                assert_eq!(b.ts, 1_700_000_120_000);
                assert_eq!(b.close.to_bits(), 27085.00_f64.to_bits());
                assert_eq!(b.open.to_bits(), 27080.10_f64.to_bits());
            }
            other => panic!("expected Forming, got {other:?}"),
        }
    }

    /// Net-hardening br7: the OKX subscribe ACK (`event:"subscribe"`) now CONFIRMS the handshake
    /// (`KlineEvent::Ack`) instead of being silently dropped — it is what disarms `run_live`'s
    /// subscribe-ack watchdog. A bare-text `pong` and an unrelated `event` stay Ignored.
    #[test]
    fn subscribe_ack_is_classified_as_ack_pong_and_junk_ignored() {
        // OKX subscribe ack: has arg.channel but NO data → an Ack (confirms the subscribe)
        assert_eq!(
            route_frame(
                r#"{"event":"subscribe","arg":{"channel":"candle1m","instId":"BTC-USDT"}}"#
            ),
            KlineEvent::Ack
        );
        // an unrelated event envelope (e.g. unsubscribe) is neither an error nor a confirm
        assert_eq!(
            route_frame(r#"{"event":"unsubscribe","arg":{"channel":"candle1m"}}"#),
            KlineEvent::Ignored
        );
        // bare-text keepalive is not JSON
        assert_eq!(route_frame("pong"), KlineEvent::Ignored);
    }

    /// Net-hardening br7 anchor (was `subscribe_ack_pong_and_junk_are_ignored` asserting Ignored):
    /// an OKX `event:"error"` subscribe reject is now an ATTRIBUTABLE `KlineEvent::Error` carrying
    /// the venue's `code`+`msg`, NOT silently dropped. `run_live` returns this as the session error.
    #[test]
    fn error_frame_is_attributable_not_ignored() {
        match route_frame(r#"{"event":"error","code":"60012","msg":"Invalid request: channel"}"#) {
            KlineEvent::Error(m) => {
                assert!(m.contains("60012"), "carries the OKX error code: {m}");
                assert!(m.contains("Invalid request: channel"), "carries the OKX message: {m}");
            }
            other => panic!("expected an attributable Error, got {other:?}"),
        }
    }

    /// Net-hardening br7 (scripted-clock, deterministic): a subscribe that never acks and never
    /// delivers data trips the `SubscribeAck` watchdog once this venue's declared ack window (the
    /// `MarketPumpSpec` row `pump_opts` consumes) elapses — the attributable "no ack/data" error
    /// the shared driver returns. An ack (or first data) within the window disarms it. Same
    /// clock-free harness `SubscribeAck`'s own unit tests use, but pinned to the venue's real row.
    #[test]
    fn a_missing_ack_within_the_window_becomes_an_attributable_error() {
        let sub_ack_timeout = vike_bridge_core::pump_spec::market_pump_spec(super::VENUE)
            .knobs()
            .ack_timeout
            .expect("okx declares the br7 ack watchdog");
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
            route_frame(r#"{"event":"subscribe","arg":{"channel":"candle1m"}}"#),
            KlineEvent::Ack
        );
        acked.confirm();
        assert!(!acked.overdue(armed_at + timeout_ms * 100), "a confirmed subscribe never trips");
    }

    #[test]
    fn subscribe_frame_targets_the_candle_channel() {
        let f = subscribe_frame("1H", "BTC-USDT");
        assert!(f.contains("\"channel\":\"candle1H\""), "channel wrong: {f}");
        assert!(f.contains("\"instId\":\"BTC-USDT\""));
        assert!(f.contains("\"op\":\"subscribe\""));
    }

    // ---- OKX `trades` channel: parse_trades (Task okxtrades) ----

    #[test]
    fn trades_subscribe_frame_targets_the_trades_channel() {
        let f = trades_subscribe_frame("BTC-USDT");
        assert!(f.contains("\"channel\":\"trades\""), "channel wrong: {f}");
        assert!(f.contains("\"instId\":\"BTC-USDT\""));
        assert!(f.contains("\"op\":\"subscribe\""));
    }

    /// A `trades` push with a buy row and a sell row -> 2 `TradeTick`s with exact price/size/ts
    /// bit patterns, and `is_buyer_maker` following Binance's `m` convention (true exactly when
    /// the buyer was the MAKER, i.e. the taker/aggressor SOLD): `side=="buy"` -> false,
    /// `side=="sell"` -> true.
    #[test]
    fn parse_trades_maps_buy_and_sell_rows_with_exact_bits() {
        let payload = serde_json::json!({
            "arg": {"channel": "trades", "instId": "BTC-USDT"},
            "data": [
                {
                    "instId": "BTC-USDT",
                    "tradeId": "130639474",
                    "px": "64500.1",
                    "sz": "0.012",
                    "side": "buy",
                    "ts": "1710000000000"
                },
                {
                    "instId": "BTC-USDT",
                    "tradeId": "130639475",
                    "px": "64499.9",
                    "sz": "0.5",
                    "side": "sell",
                    "ts": "1710000000123"
                }
            ]
        });
        let ticks = parse_trades(&payload);
        assert_eq!(ticks.len(), 2);

        assert_eq!(ticks[0].ts, 1_710_000_000_000);
        assert_eq!(ticks[0].price.to_bits(), 64500.1_f64.to_bits());
        assert_eq!(ticks[0].size.to_bits(), 0.012_f64.to_bits());
        assert!(!ticks[0].is_buyer_maker, "buy taker -> buyer is NOT the maker");
        assert_eq!(ticks[0].symbol, "BTC-USDT");
        assert_eq!(ticks[0].local_ts, 0, "the pure mapper leaves local_ts unstamped");

        assert_eq!(ticks[1].ts, 1_710_000_000_123);
        assert_eq!(ticks[1].price.to_bits(), 64499.9_f64.to_bits());
        assert_eq!(ticks[1].size.to_bits(), 0.5_f64.to_bits());
        assert!(ticks[1].is_buyer_maker, "sell taker -> buyer WAS the maker");
    }

    /// Non-`trades` pushes (a candle subscribe ack, and an actual candle data push) yield an
    /// empty vec — `parse_trades` must not misclassify another channel's frame.
    #[test]
    fn parse_trades_ignores_non_trades_pushes() {
        let candle_ack = serde_json::json!({
            "event": "subscribe",
            "arg": {"channel": "candle1m", "instId": "BTC-USDT"}
        });
        assert!(parse_trades(&candle_ack).is_empty());

        let candle_push = serde_json::json!({
            "arg": {"channel": "candle1m", "instId": "BTC-USDT"},
            "data": [["1700000060000", "1", "1", "1", "1", "1", "1", "1", "1"]]
        });
        assert!(parse_trades(&candle_push).is_empty());

        assert!(parse_trades(&serde_json::json!({})).is_empty());
    }

    /// A malformed/unparseable row (bad price, zero size, unrecognized side, missing field) is
    /// skipped in place — never panics, never produces a bogus tick.
    #[test]
    fn parse_trades_skips_malformed_rows_without_panicking() {
        let payload = serde_json::json!({
            "arg": {"channel": "trades", "instId": "BTC-USDT"},
            "data": [
                {"instId": "BTC-USDT", "px": "not-a-number", "sz": "0.012", "side": "buy", "ts": "1710000000000"},
                {"instId": "BTC-USDT", "px": "1.0", "sz": "0", "side": "buy", "ts": "1710000000000"},
                {"instId": "BTC-USDT", "px": "1.0", "sz": "1.0", "side": "unknown", "ts": "1710000000000"},
                {"instId": "BTC-USDT", "px": "1.0", "sz": "1.0", "side": "buy"},
                {"px": "1.0", "sz": "1.0", "side": "buy", "ts": "1710000000000"},
                "not even an object"
            ]
        });
        assert!(parse_trades(&payload).is_empty());
    }

    /// One bad row alongside a good one: the batch is NOT failed wholesale (mirrors
    /// `vike_binance::trades::rest_agg_trades`'s per-element tolerance).
    #[test]
    fn parse_trades_keeps_good_rows_alongside_a_bad_one() {
        let payload = serde_json::json!({
            "arg": {"channel": "trades", "instId": "BTC-USDT"},
            "data": [
                {"instId": "BTC-USDT", "px": "bad", "sz": "1.0", "side": "buy", "ts": "1"},
                {"instId": "BTC-USDT", "px": "100.0", "sz": "1.0", "side": "buy", "ts": "2"}
            ]
        });
        let ticks = parse_trades(&payload);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].ts, 2);
    }

    // ---- OKX `books` CRC32 checksum validation (audit br1), scripted-frame style ----

    /// A `[px, sz, "0", "1"]` level array (OKX's `books` shape; only `[0]`/`[1]` are read).
    fn depth_levels(ls: &[(&str, &str)]) -> Vec<serde_json::Value> {
        ls.iter().map(|(p, s)| serde_json::json!([p, s, "0", "1"])).collect()
    }

    /// A scripted OKX `books` frame with an explicit `seqId`/`prevSeqId`/`checksum`.
    fn books_frame(
        action: &str,
        seq: u64,
        prev_seq: i64,
        bids: &[(&str, &str)],
        asks: &[(&str, &str)],
        checksum: i32,
    ) -> String {
        serde_json::json!({
            "arg": {"channel": "books", "instId": "BTC-USDT-SWAP"},
            "action": action,
            "data": [{
                "bids": depth_levels(bids),
                "asks": depth_levels(asks),
                "ts": "1700000000000",
                "seqId": seq,
                "prevSeqId": prev_seq,
                "checksum": checksum,
            }]
        })
        .to_string()
    }

    fn owned(ls: &[(&str, &str)]) -> Vec<(String, String)> {
        ls.iter().map(|(p, s)| (p.to_string(), s.to_string())).collect()
    }

    /// The correct OKX checksum for a merged book of these already-sorted levels (bids high→low,
    /// asks low→high) — computed through the same `ChecksumBook` the decoder uses.
    fn merged_checksum(bids: &[(&str, &str)], asks: &[(&str, &str)]) -> i32 {
        let mut b = ChecksumBook::new();
        b.apply_snapshot(&owned(bids), &owned(asks));
        b.computed_checksum()
    }

    /// Happy path: a snapshot then an update, each carrying the CORRECT checksum of the merged book,
    /// both decode to `Updated` (no gap). The update ships only the CHANGED levels but its checksum is
    /// over the merged top-of-book — proving the decoder validates the MERGED book, not the delta.
    #[test]
    fn valid_snapshot_and_update_checksums_keep_the_book() {
        let snap_bids = [("100.0", "5"), ("99.0", "3")];
        let snap_asks = [("101.0", "4"), ("102.0", "2")];
        let snap = books_frame(
            "snapshot",
            10,
            -1,
            &snap_bids,
            &snap_asks,
            merged_checksum(&snap_bids, &snap_asks),
        );

        let mut cbook = ChecksumBook::new();
        let mut book: Option<vike_model::L2Book> = None;
        assert_eq!(
            okx_depth_decode(&mut cbook, &snap, &mut book, "BTC-USDT-SWAP"),
            BookOp::Updated(1_700_000_000_000)
        );

        // update: best bid 5→8, add a 99.5 bid. Checksum is over the MERGED book {100.0:8,99.5:1,99.0:3}.
        let merged_bids = [("100.0", "8"), ("99.5", "1"), ("99.0", "3")];
        let upd_cs = merged_checksum(&merged_bids, &snap_asks);
        let upd = books_frame("update", 11, 10, &[("100.0", "8"), ("99.5", "1")], &[], upd_cs);
        assert_eq!(
            okx_depth_decode(&mut cbook, &upd, &mut book, "BTC-USDT-SWAP"),
            BookOp::Updated(1_700_000_000_000)
        );
    }

    /// A structurally-valid update (seq chain intact) whose checksum DISAGREES with the merged book →
    /// `BookOp::Gap`, which the shared driver turns into a reconnect+re-seed and a `GapStart`
    /// disclosure. This is the silent-corruption class the `prevSeqId` chain cannot catch.
    #[test]
    fn a_bad_update_checksum_triggers_a_resync_gap() {
        let bids = [("100.0", "5")];
        let asks = [("101.0", "4")];
        let snap = books_frame("snapshot", 10, -1, &bids, &asks, merged_checksum(&bids, &asks));
        let mut cbook = ChecksumBook::new();
        let mut book: Option<vike_model::L2Book> = None;
        assert!(matches!(okx_depth_decode(&mut cbook, &snap, &mut book, "X"), BookOp::Updated(_)));

        let real = merged_checksum(&[("100.0", "7")], &[("101.0", "4")]);
        let wrong = 424_242; // a non-zero value that is not the real checksum
        assert_ne!(wrong, real, "the fixed wrong checksum must differ from the real one");
        let upd = books_frame("update", 11, 10, &[("100.0", "7")], &[], wrong);
        assert_eq!(okx_depth_decode(&mut cbook, &upd, &mut book, "X"), BookOp::Gap);
    }

    /// A corrupted SNAPSHOT (wrong checksum) is also caught → `Gap`.
    #[test]
    fn a_bad_snapshot_checksum_triggers_a_resync_gap() {
        let bids = [("100.0", "5"), ("99.0", "3")];
        let asks = [("101.0", "4")];
        let real = merged_checksum(&bids, &asks);
        let wrong = 424_242;
        assert_ne!(wrong, real);
        let snap = books_frame("snapshot", 10, -1, &bids, &asks, wrong);
        let mut cbook = ChecksumBook::new();
        let mut book: Option<vike_model::L2Book> = None;
        assert_eq!(okx_depth_decode(&mut cbook, &snap, &mut book, "X"), BookOp::Gap);
    }

    /// OKX deprecated the field on 2026-06-23 (fixed to `0`). A `0` checksum must be SKIPPED, never
    /// validated — so a live OKX stream (which always sends 0) never false-trips a resync, even though
    /// the real book checksum is non-zero.
    #[test]
    fn a_zero_checksum_is_skipped_so_live_okx_never_false_trips() {
        let bids = [("100.0", "5")];
        let asks = [("101.0", "4")];
        assert_ne!(merged_checksum(&bids, &asks), 0, "sanity: the real checksum is non-zero");
        let snap = books_frame("snapshot", 10, -1, &bids, &asks, 0);
        let mut cbook = ChecksumBook::new();
        let mut book: Option<vike_model::L2Book> = None;
        assert!(
            matches!(okx_depth_decode(&mut cbook, &snap, &mut book, "X"), BookOp::Updated(_)),
            "a 0 checksum is skipped, not validated"
        );
        let upd = books_frame("update", 11, 10, &[("100.0", "9")], &[], 0);
        assert!(matches!(okx_depth_decode(&mut cbook, &upd, &mut book, "X"), BookOp::Updated(_)));
    }

    /// A `prevSeqId` break is a `Gap` on the sequence chain regardless of checksum (the existing
    /// resync path is unchanged; the checksum layer is additive).
    #[test]
    fn a_prev_seq_break_still_gaps() {
        let bids = [("100.0", "5")];
        let asks = [("101.0", "4")];
        let snap = books_frame("snapshot", 10, -1, &bids, &asks, merged_checksum(&bids, &asks));
        let mut cbook = ChecksumBook::new();
        let mut book: Option<vike_model::L2Book> = None;
        assert!(matches!(okx_depth_decode(&mut cbook, &snap, &mut book, "X"), BookOp::Updated(_)));
        // prevSeqId 99 != last seqId 10 → seq gap (checksum 0 here is irrelevant — the chain breaks first)
        let upd = books_frame("update", 11, 99, &[("100.0", "6")], &[], 0);
        assert_eq!(okx_depth_decode(&mut cbook, &upd, &mut book, "X"), BookOp::Gap);
    }

    /// An `update` before any `snapshot` is ignored (no book yet) and does not touch the mirror.
    #[test]
    fn an_update_before_the_snapshot_is_ignored() {
        let mut cbook = ChecksumBook::new();
        let mut book: Option<vike_model::L2Book> = None;
        let upd = books_frame("update", 5, 4, &[("100.0", "1")], &[], 999);
        assert_eq!(okx_depth_decode(&mut cbook, &upd, &mut book, "X"), BookOp::Ignored);
    }

    // ---- OKX `mark-price` channel (mark-slot semantics, W2-T4), scripted-frame style ----

    /// A real-shaped `mark-price` data push → `MarkEvent::Mark` with the exact `markPx` bits and
    /// the row's `ts` (both decimal strings on the wire).
    #[test]
    fn mark_price_frame_decodes_px_and_ts() {
        let frame = serde_json::json!({
            "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"},
            "data": [{"instType": "SWAP", "instId": "BTC-USDT-SWAP", "markPx": "27123.45", "ts": "1700000000123"}]
        })
        .to_string();
        match route_mark_frame(&frame) {
            MarkEvent::Mark { px, ts } => {
                assert_eq!(px.to_bits(), 27_123.45_f64.to_bits(), "markPx, not lastPx");
                assert_eq!(ts, 1_700_000_000_123);
            }
            other => panic!("expected Mark, got {other:?}"),
        }
    }

    /// The subscribe ACK envelope is classified `Ack`; the venue's error envelope is an
    /// attributable `Error` carrying code+msg (drives a `Fatal` in the pump).
    #[test]
    fn mark_ack_and_error_envelopes_classify() {
        let ack = serde_json::json!({
            "event": "subscribe",
            "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"}
        })
        .to_string();
        assert_eq!(route_mark_frame(&ack), MarkEvent::Ack);

        let err = serde_json::json!({
            "event": "error", "code": "60012", "msg": "Invalid request"
        })
        .to_string();
        match route_mark_frame(&err) {
            MarkEvent::Error(m) => assert!(m.contains("60012") && m.contains("Invalid request")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// A non-`mark-price` push, a dead (0/neg/NaN) mark, a malformed row, and the bare-text
    /// keepalive are all `Ignored` — never a bogus mark into the `PriceBoard` mark slot.
    #[test]
    fn mark_non_channel_dead_and_malformed_frames_are_ignored() {
        let candle = serde_json::json!({
            "arg": {"channel": "candle1m", "instId": "BTC-USDT-SWAP"},
            "data": [["1", "1", "1", "1", "1", "1", "1", "1", "1"]]
        })
        .to_string();
        assert_eq!(route_mark_frame(&candle), MarkEvent::Ignored);

        for dead in ["0", "-1.0", "not-a-number"] {
            let f = serde_json::json!({
                "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"},
                "data": [{"markPx": dead, "ts": "1"}]
            })
            .to_string();
            assert_eq!(route_mark_frame(&f), MarkEvent::Ignored, "dead mark {dead} dropped");
        }

        // empty data array, and the bare-text "pong" keepalive
        let empty = serde_json::json!({
            "arg": {"channel": "mark-price", "instId": "BTC-USDT-SWAP"}, "data": []
        })
        .to_string();
        assert_eq!(route_mark_frame(&empty), MarkEvent::Ignored);
        assert_eq!(route_mark_frame("pong"), MarkEvent::Ignored);
    }

    /// The subscribe frame targets the public `mark-price` channel on the given inst — the exact
    /// `{"op":"subscribe","args":[{"channel":"mark-price","instId":...}]}` wire grammar.
    #[test]
    fn mark_subscribe_frame_targets_the_mark_price_channel() {
        let v: serde_json::Value =
            serde_json::from_str(&mark_subscribe_frame("BTC-USDT-SWAP")).unwrap();
        assert_eq!(v["op"], "subscribe");
        assert_eq!(v["args"][0]["channel"], "mark-price");
        assert_eq!(v["args"][0]["instId"], "BTC-USDT-SWAP");
    }
}

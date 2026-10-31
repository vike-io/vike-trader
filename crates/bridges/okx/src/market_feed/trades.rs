//! The trades lane: the `trades` print decoder, subscribe frame and feed-thread body.

use serde_json::Value;
use vike_bridge_core::feed_frame::FeedFrame;
use vike_bridge_core::market_pump::{FrameOutcome, SessionStatus, run_market_feed};
use vike_bridge_core::pump_spec::subscribe_only_pump_opts;
use vike_marketdata::TradeTick;
use vike_model::now_ms;

use super::{FeedCtx, LIVE_STATUS, VENUE, subscribe_envelope};
use crate::market_data::PUBLIC_WS;

// --- Trades feed (live prints) ---------------------------------------------------------------
// OKX `trades` channel: executed prints (no snapshot/seed — a live-only tape, unlike the
// `candle*`/`books` channels which each have a REST/WS seed counterpart). Same host + subscribe
// shape as the candle feed; feeds `vike_marketdata::TradeTick` through `LiveDataSink::trade`, the
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
// | `tradeId`|    | (not carried — `vike_marketdata::TradeTick` has no trade-id field) |
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
pub(super) fn row_str<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
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
///
/// [`subscribe_frame`]: super::klines::subscribe_frame
pub fn trades_subscribe_frame(inst: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [{"channel": "trades", "instId": inst}]
    })
    .to_string()
}

/// [`route_frame`]'s trades-pump twin: classifies ONE raw WS text frame for [`trades_main`].
/// The subscribe envelope is checked first ([`subscribe_envelope`]). Otherwise [`parse_trades`]
/// decodes the push; an empty result (non-trades channel, or every row malformed) is
/// [`FeedFrame::Ignored`].
///
/// [`route_frame`]: super::klines::route_frame
fn route_trades_frame(text: &str) -> FeedFrame<Vec<TradeTick>> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return FeedFrame::Ignored; // e.g. the bare-text "pong" keepalive is not JSON
    };
    if let Some(envelope) = subscribe_envelope(&v) {
        return envelope;
    }
    let ticks = parse_trades(&v);
    if ticks.is_empty() { FeedFrame::Ignored } else { FeedFrame::Data(ticks) }
}

/// `subscribe_trades`'s thread body — the trades twin of [`feed_main`], on the same shared
/// [`run_market_feed`] driver (dedup A6): identical subscribe-replay, br7 ack-watchdog, stop-poll,
/// and 30×100 ms backoff lifecycle, scoped down to prints (no closed/forming bar routing, no
/// mark-tick). Live-only: unlike [`feed_main`]/[`depth_main`], there is no REST warmup/seed here —
/// OKX's `trades` channel has no historical REST counterpart threaded into this feed, and a
/// tick/volume-bar builder needs no seed to start folding live prints.
///
/// [`feed_main`]: super::klines::feed_main
/// [`depth_main`]: super::depth::depth_main
pub(super) fn trades_main(inst: String, _interval: String, ctx: FeedCtx) {
    let sub = trades_subscribe_frame(&inst);
    run_market_feed(
        PUBLIC_WS,
        &subscribe_only_pump_opts(VENUE, &sub),
        &ctx.stop,
        &now_ms,
        |txt| {
            route_trades_frame(txt).outcome(|ticks| {
                for mut tick in ticks {
                    tick.local_ts = now_ms(); // machine receive time (dual-timestamp capture)
                    ctx.sink.trade(VENUE, &inst, tick);
                }
                (ctx.wake)();
                FrameOutcome::Confirm // data confirms the br7 subscribe handshake
            })
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

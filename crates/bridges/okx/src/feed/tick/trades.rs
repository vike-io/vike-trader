//! The trades lane: the `trades` print decoder, subscribe frame and the lane's plan.

use serde_json::Value;
use vike_bridge_core::data_client::event::DataClientEvent;
use vike_bridge_core::data_client::ws::DataClientLanePlan;
use vike_bridge_core::feed_frame::{FeedFrame, route_rows};
use vike_bridge_core::is_valid_trade;
use vike_bridge_core::websocket_session::market::WebsocketSessionFrameOutcome;
use vike_marketdata::TickTrade;

use super::KEEPALIVE;
use crate::feed::{channel_subscribe_frame, subscribe_envelope};

// --- Trades feed (live prints) ---------------------------------------------------------------
// OKX `trades` channel: executed prints (no snapshot/seed — a live-only tape, unlike the
// `candle*`/`books` channels which each have a REST/WS seed counterpart). Same host + subscribe
// shape as the candle feed; feeds `vike_marketdata::TickTrade` through `DataClientSink::trade`, the
// input the app's client-side tick/volume-bar aggregation reads (a follow-up task wires that up).
//
// Wire shape: `{"arg":{"channel":"trades","instId":"BTC-USDT"},"data":[{"instId":"BTC-USDT",
// "tradeId":"130639474","px":"64500.1","sz":"0.012","side":"buy","ts":"1710000000000"}]}` — a
// push may carry more than one print per frame. Field mapping mirrors binance's `@aggTrade`
// mapper (`vike_binance::feed::tick::trades::map_wire`) so both venues' ticks are shaped identically:
//
// | wire key | -> | `TickTrade` field |
// |----------|----|--------------------|
// | `px`     |    | `price` — decimal string -> f64 |
// | `sz`     |    | `size` — decimal string -> f64 |
// | `ts`     |    | `ts` — decimal-string epoch-ms -> i64 |
// | `side`   |    | `is_buyer_maker` — OKX's `side` is the TAKER/aggressor side; Binance's `m` |
// |          |    | convention is `true` exactly when the BUYER was the MAKER (i.e. the |
// |          |    | taker/aggressor SOLD) — so `side=="sell"` -> `true`, `side=="buy"` -> `false`. |
// | (caller) |    | `symbol` — never read off a per-row field beyond validating it's present; the |
// |          |    | pump always emits under its own subscribed `instrument`, same convention as `close_bar`/`forming_bar`/`mark_tick` above. |
// | `tradeId`|    | (not carried — `vike_marketdata::TickTrade` has no trade-id field) |
//
// `local_ts` is stamped by the pump at receive time (0 from the pure mapper — deterministic/
// testable, same "0 = not stamped" convention `vike_binance::feed::tick::trades::map_wire` documents).
//
// **Non-finite/non-positive guard:** a decoded price/size that fails to parse, is non-finite, or
// is `<= 0.0` is rejected in place (`vike_bridge_core::is_valid_trade`, the one guard every venue's
// trade decoder applies — a downstream volume fold would spin on `+Inf` or corrupt on a
// zero/garbage size).

/// The `i`-th element of a slice of trade rows, but for the pure per-row string-field reads below
/// — kept local to this section rather than reusing the candle `s()` helper (which is for a
/// `&[Value]` array row; trades rows are `Value` objects keyed by name).
pub(crate) fn row_str<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

/// One `trades` channel data row -> a `TickTrade` (`local_ts` left at the "not stamped" sentinel
/// `0` — the pump stamps machine receive time just before each `sink.trade` emit, same convention
/// as binance's aggTrade mapper). `None` on a missing/unparseable field, a non-finite/non-positive
/// price or size, or an unrecognized `side`.
fn row_to_trade_tick(row: &Value) -> Option<TickTrade> {
    let instrument = row_str(row, "instId")?;
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
    Some(TickTrade {
        timestamp: ts,
        local_timestamp: 0,
        price,
        size,
        is_buyer_maker,
        symbol: instrument.into(),
    })
}

/// PURE: one OKX `trades` channel WS push -> zero or more `TickTrade`s (fixture-tested, no
/// socket). Anything that is not a `trades`-channel data push — a non-`trades` channel, the
/// subscribe-ack/error `event` envelope (no `data` field), or a frame with no/empty `data` array —
/// yields an empty vec; a malformed/unparseable individual row is skipped in place rather than
/// failing the whole push (mirrors `vike_binance::feed::tick::trades::rest_agg_trades`'s per-element
/// tolerance).
pub fn parse_trades(payload: &Value) -> Vec<TickTrade> {
    let channel = payload.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str);
    if channel != Some("trades") {
        return Vec::new();
    }
    let Some(rows) = payload.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter().filter_map(row_to_trade_tick).collect()
}

/// The subscribe frame for the public `trades` channel on `instrument` — same `{"op":"subscribe",...}`
/// shape as [`subscribe_frame`] (candles), just the `trades` channel with no bar code.
///
/// [`subscribe_frame`]: crate::feed::bar::subscribe_frame
pub fn trades_subscribe_frame(instrument: &str) -> String {
    channel_subscribe_frame("trades", instrument)
}

/// [`route_frame`]'s trades-pump twin: classifies ONE raw WS text frame for [`decode_trades`] through
/// [`route_rows`]: the subscribe envelope first ([`subscribe_envelope`]), then [`parse_trades`]; an
/// empty result (non-trades channel, or every row malformed) is [`FeedFrame::Ignored`].
///
/// [`route_frame`]: crate::feed::bar::route_frame
fn route_trades_frame(text: &str) -> FeedFrame<Vec<TickTrade>> {
    route_rows(text, subscribe_envelope, parse_trades)
}

/// The trades lane's plan, on the PUBLIC socket: [`trades_subscribe_frame`], no seed (OKX's `trades`
/// channel has no historical REST counterpart threaded into this feed, and a tick/volume-bar builder needs
/// none to start folding live prints), every frame through [`decode_trades`]. Subscribes `instrument` (the instId)
/// and labels each print with `series` (its USER name, decision 0290): the venue's instId stays in the bridge.
pub(crate) fn trades_plan(
    series: &str,
    instrument: &str,
    public_websocket: &str,
) -> DataClientLanePlan {
    // Interned once for the lane: each print copies the handle.
    let series = vike_marketdata::Symbol::new(series);
    DataClientLanePlan::Session {
        url: public_websocket.to_string(),
        subscribe: Some(trades_subscribe_frame(instrument)),
        keepalive: Some(KEEPALIVE),
        notice: Some(crate::feed::market_service_upgrade_notice),
        seed: None,
        decode: Box::new(move |txt, emit| {
            decode_trades(txt, &mut |event| {
                emit(match event {
                    DataClientEvent::Trades(mut prints) => {
                        prints.iter_mut().for_each(|print| print.symbol = series);
                        DataClientEvent::Trades(prints)
                    }
                    other => other,
                });
            })
        }),
    }
}

/// One frame of the trades lane through [`route_trades_frame`]: the prints are the engine's
/// [`DataClientEvent::Trades`] (it stamps each at receipt) and confirm the subscribe handshake.
pub fn decode_trades(
    txt: &str,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    route_trades_frame(txt).outcome(|ticks| {
        emit(DataClientEvent::Trades(ticks));
        WebsocketSessionFrameOutcome::Confirm
    })
}

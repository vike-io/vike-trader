//! The trades lane: the `publicTrade` decoder, subscribe frame and the live-only lane plan.

use serde_json::Value;
use vike_bridge_core::data_client::event::DataClientEvent;
use vike_bridge_core::data_client::ws::DataClientLanePlan;
use vike_bridge_core::feed_frame::{FeedFrame, route_rows};
use vike_bridge_core::is_valid_trade;
use vike_bridge_core::websocket_session::market::WebsocketSessionFrameOutcome;
use vike_marketdata::TickTrade;

use crate::feed::bar::{subscribe_envelope, topic_subscribe_frame};

use crate::feed::{BybitUrlsFeed, feed_route, websocket_host};

// --- Trades feed (live prints) ---------------------------------------------------------------
// Bybit V5 `publicTrade.<SYMBOL>` channel: executed prints (no snapshot/seed — a live-only tape,
// unlike the `kline.*`/`orderbook.*` channels which each have a REST/WS seed counterpart). Same
// subscribe shape as the kline feed (`{"op":"subscribe",...}`) and the SAME spot-vs-linear split: a
// `.P`-suffixed perp symbol strips to the exchange symbol for the `publicTrade.<SYMBOL>` topic and
// routes to its book's socket, while spot stays on the spot one (see `websocket_host`). Feeds
// `vike_marketdata::TickTrade` through `DataClientSink::trade`, the input the app's client-side
// tick/volume-bar aggregation reads. The Bybit twin of okx's `market_feed` trades section, and shaped
// identically to `crate::feed::tick::decode_trade` (the HFT-track twin), so both tracks' ticks match.
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
// | wire key | -> | `TickTrade` field |
// |----------|----|--------------------|
// | `p`      |    | `price` — decimal STRING -> f64 |
// | `v`      |    | `size` — decimal STRING -> f64 |
// | `T`      |    | `ts` — epoch-ms, a JSON NUMBER (unlike okx's decimal-string ts) -> i64 |
// | `S`      |    | `is_buyer_maker` — Bybit's `S` is the TAKER/aggressor side; Binance's `m` |
// |          |    | convention is `true` exactly when the BUYER was the MAKER (i.e. the |
// |          |    | taker/aggressor SOLD) — so `S=="Sell"` -> `true`, `S=="Buy"` -> `false`. |
// |          |    | (Identical to `crate::feed::tick::decode_trade`'s `S == "Sell"` rule.) |
// | `s`      |    | `symbol` — read off the per-row wire field. |
// | `i`/`L`  |    | (trade-id / tick-direction — not carried; `vike_marketdata::TickTrade` has neither) |
//
// `local_ts` is stamped by the pump at receive time (0 from the pure mapper — deterministic/
// testable, same "0 = not stamped" convention the binance/okx mappers document).
//
// **Non-finite/non-positive guard:** a decoded price/size that fails to parse, is non-finite, or is
// `<= 0.0` is rejected in place (`vike_bridge_core::is_valid_trade`, the one guard every venue's
// trade decoder applies — a downstream volume fold would spin on `+Inf` or corrupt on a zero/garbage
// size).

/// One `publicTrade` channel data row -> a `TickTrade` (`local_ts` left at the "not stamped"
/// sentinel `0` — the pump stamps machine receive time just before each `sink.trade` emit, same
/// convention as the binance/okx mappers). `None` on a missing/unparseable field, a
/// non-finite/non-positive price or size, or an unrecognized `S` side.
fn row_to_trade_tick(row: &Value) -> Option<TickTrade> {
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
    Some(TickTrade {
        timestamp: ts,
        local_timestamp: 0,
        price,
        size,
        is_buyer_maker,
        symbol: symbol.to_string(),
    })
}

/// PURE: one Bybit `publicTrade` channel WS push -> zero or more `TickTrade`s (fixture-tested, no
/// socket). Anything that is not a `publicTrade`-topic data push — another topic, a subscribe
/// ack/reject envelope (which carries `op`, not `topic`), or a frame with no/empty `data` array —
/// yields an empty vec; a malformed/unparseable individual row is skipped in place rather than
/// failing the whole push (mirrors okx's `parse_trades` per-element tolerance).
pub fn parse_trades(payload: &Value) -> Vec<TickTrade> {
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
pub(crate) fn relabel_series(mut ticks: Vec<TickTrade>, series_symbol: &str) -> Vec<TickTrade> {
    for t in &mut ticks {
        t.symbol = series_symbol.to_string();
    }
    ticks
}

/// The subscribe frame for the public `publicTrade` channel on `symbol` — same
/// `{"op":"subscribe","args":[...]}` shape as [`subscribe_frame`] (klines), just the `publicTrade`
/// topic with no interval code.
///
/// [`subscribe_frame`]: crate::feed::bar::subscribe_frame
pub fn trades_subscribe_frame(symbol: &str) -> String {
    topic_subscribe_frame(&format!("publicTrade.{symbol}"))
}

/// [`route_frame`]'s trades-pump twin: classifies ONE raw WS text frame for [`decode_trades`] through
/// [`route_rows`]: the subscribe OUTCOME envelope first ([`subscribe_envelope`]), then
/// [`parse_trades`]; an empty result (non-trades topic, or every row malformed) is
/// [`FeedFrame::Ignored`].
///
/// [`route_frame`]: crate::feed::bar::route_frame
fn route_trades_frame(text: &str) -> FeedFrame<Vec<TickTrade>> {
    route_rows(text, subscribe_envelope, parse_trades)
}

/// The trades lane's plan: the kline lane's route ([`feed_route`], on the lane's thread for the same
/// reasons), the `publicTrade` topic on the `.P`-stripped exchange symbol and the socket of the book the
/// symbol names; live-only (Bybit's `publicTrade` channel has no historical REST counterpart threaded into
/// this feed, and a tick/volume-bar builder needs no seed). Every frame through [`decode_trades`], which
/// re-labels each print to `series_symbol`, because the linear wire `s` is the plain `BTCUSDT` and the
/// `TradeStore` keys on `tick.symbol`. Spot passes `series_symbol == api_symbol` and the spot socket, so
/// the spot path is byte-identical to the pre-perp behavior.
pub(crate) fn trades_plan(
    series_symbol: &str,
    hosts: &BybitUrlsFeed,
) -> Result<DataClientLanePlan, String> {
    let (api_symbol, book) = feed_route(hosts, series_symbol)?;
    let series = series_symbol.to_string();
    Ok(DataClientLanePlan::Session {
        url: websocket_host(&hosts.ws, book),
        subscribe: Some(trades_subscribe_frame(&api_symbol)),
        keepalive: None,
        seed: None,
        decode: Box::new(move |txt, emit| decode_trades(txt, &series, emit)),
    })
}

/// One `publicTrade` frame through [`route_trades_frame`]: the prints, re-labeled to the `.P` series key
/// BEFORE emit ([`relabel_series`]; spot is a no-op), are the engine's [`DataClientEvent::Trades`] (it
/// stamps each at receipt) and confirm the br7 subscribe handshake.
pub fn decode_trades(
    txt: &str,
    series_symbol: &str,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    route_trades_frame(txt).outcome(|ticks| {
        emit(DataClientEvent::Trades(relabel_series(ticks, series_symbol)));
        WebsocketSessionFrameOutcome::Confirm
    })
}

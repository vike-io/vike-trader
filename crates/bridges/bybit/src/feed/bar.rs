//! The kline lane: frame classifier, subscribe frame and the REST-seeded lane plan.

use serde::Deserialize;
use serde_json::Value;
use vike_bridge_core::data_client::event::{BarKline, DataClientEvent};
use vike_bridge_core::data_client::ws::DataClientLanePlan;
use vike_bridge_core::feed_frame::FeedFrame;
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::websocket_session::market::WebsocketSessionFrameOutcome;
use vike_marketdata::Bar;

use super::{BybitUrlsFeed, SEED_LIMIT, bars_route, websocket_host};
use crate::history::{fetch_klines_latest, interval_code};

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
struct KlineMessage {
    topic: String,
    data: Vec<KlineData>,
}

/// The subscribe OUTCOME envelope every lane on this socket shares (net-hardening br7), classified
/// BEFORE a lane's own decode: a frame carrying an `op` is never data. `{op:"subscribe",
/// success:true}` is [`FeedFrame::Ack`]; `success:false` an attributable [`FeedFrame::Error`] carrying
/// Bybit's `ret_msg` (it was silently dropped); any other op (a `ping`/`pong` keepalive reply) is
/// [`FeedFrame::Ignored`]. `None`: no `op`, so the lane decodes the frame as data.
pub(super) fn subscribe_envelope<T>(v: &Value) -> Option<FeedFrame<T>> {
    let op = v.get("op").and_then(Value::as_str)?;
    if op != "subscribe" {
        return Some(FeedFrame::Ignored);
    }
    Some(if v.get("success").and_then(Value::as_bool).unwrap_or(false) {
        FeedFrame::Ack
    } else {
        let msg = v.get("ret_msg").and_then(Value::as_str).unwrap_or("(no message)");
        FeedFrame::Error(format!("Bybit subscribe rejected: {msg}"))
    })
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

/// Route ONE Bybit WS text frame → a classified kline frame. Pure — testable without a socket. The
/// subscribe OUTCOME envelope is classified FIRST ([`subscribe_envelope`]). Otherwise a `kline.*`
/// frame's first datum maps to a [`Bar`] in a [`BarKline`] whose `closed` is the venue's `confirm`.
/// Non-JSON keepalives and non-kline topics → [`FeedFrame::Ignored`].
pub fn route_frame(text: &str) -> FeedFrame<BarKline> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return FeedFrame::Ignored; // bare-text "ping"/"pong" keepalive is not JSON
    };
    if let Some(envelope) = subscribe_envelope(&v) {
        return envelope;
    }
    // A kline data frame: {topic, data:[kline]} (the non-`op` JSON path).
    let Ok(msg) = serde_json::from_value::<KlineMessage>(v) else {
        return FeedFrame::Ignored;
    };
    if !msg.topic.starts_with("kline.") {
        return FeedFrame::Ignored;
    }
    let Some(k) = msg.data.first() else {
        return FeedFrame::Ignored;
    };
    let Some(bar) = data_to_bar(k) else {
        return FeedFrame::Ignored;
    };
    FeedFrame::Data(BarKline { bar, closed: k.confirm })
}

/// The subscribe frame: the kline stream `kline.<code>.<SYMBOL>` (`code` = Bybit's interval code).
pub fn subscribe_frame(code: &str, symbol: &str) -> String {
    topic_subscribe_frame(&format!("kline.{code}.{symbol}"))
}

/// The one subscribe frame every lane on this socket sends, `{"op":"subscribe","args":[<topic>]}`
/// (`kline.<code>.<SYMBOL>`, `tickers.<SYMBOL>`, `publicTrade.<SYMBOL>`).
pub(super) fn topic_subscribe_frame(topic: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [topic]
    })
    .to_string()
}

/// The kline lane's plan. `series_symbol` is the catalog/sink label (`.P`-suffixed for a perp — the core
/// key); the route ([`bars_route`]) gives the `.P`-stripped exchange symbol the REST seed and the
/// `kline.<code>.<SYMBOL>` topic use (IDENTICAL shape on all three categories — Bybit does not encode
/// category in the topic, only the socket URL), and the book that picks BOTH the seed's REST category and
/// the socket ([`websocket_host`]), so the warmup and the stream cannot reach different books.
///
/// The interval code and the ROUTE are decided here, on the lane's thread: an unsupported interval or a
/// symbol whose book cannot be decided is a permanent config error on this subscription (the refusal
/// lands in the status string, which is what an operator sees), and the first route per process reads
/// the venue's instrument listings, which must not stall the caller's (often the picker's) thread.
///
/// The REST warmup is the newest [`SEED_LIMIT`] klines; Bybit serves the in-progress candle as the last
/// one, so the engine seeds the closed prefix and routes the forming tail through the market lane. Once
/// per feed, NOT per session: a reconnect does not re-seed (re-seeding would double-emit closed bars).
/// The healthy status is the engine's, per SESSION, so one blip can never latch a fault reading (the
/// 42-hour the CI box latch: `tests/offline/market_feed_scripted.rs`).
///
/// `engine_names`: the symbol is an engine's own name (`super::DataClientSpecBybit::with_engine_named_bars`).
pub(super) fn bars_plan(
    series_symbol: &str,
    interval: &str,
    hosts: &BybitUrlsFeed,
    engine_names: bool,
) -> Result<DataClientLanePlan, String> {
    let code = interval_code(interval)?;
    let (api_symbol, book) = bars_route(hosts, series_symbol, engine_names)?;
    let (history, api, iv) = (hosts.history.clone(), api_symbol.clone(), interval.to_string());
    Ok(DataClientLanePlan::Session {
        url: websocket_host(&hosts.ws, book),
        subscribe: Some(subscribe_frame(code, &api_symbol)),
        keepalive: None,
        seed: Some(Box::new(move || fetch_klines_latest(&history, &api, &iv, SEED_LIMIT, book))),
        decode: Box::new(decode_kline),
    })
}

/// One kline-lane frame through the fixture-tested [`route_frame`]: a datum is the engine's
/// [`DataClientEvent::Bar`] (Bybit's `confirm` says closed or forming; the engine ticks the bar-close
/// cache — not `mark_tick`: a candle close is not the venue mark — then lands it on the lossless or the
/// conflating lane) and confirms the br7 subscribe handshake; an ack confirms it dataless; a venue REJECT
/// is the session's attributable fatal error (never silently dropped).
pub fn decode_kline(
    txt: &str,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    route_frame(txt).outcome(|kline| {
        emit(DataClientEvent::Bar(kline));
        WebsocketSessionFrameOutcome::Confirm
    })
}

//! The candle lane: the `candle<bar>` frame classifier, subscribe frame and the lane's plan.

use serde_json::Value;
use vike_bridge_core::data_client::event::{BarKline, DataClientEvent};
use vike_bridge_core::data_client::ws::DataClientLanePlan;
use vike_bridge_core::feed_frame::FeedFrame;
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::websocket_session::market::WebsocketSessionFrameOutcome;
use vike_marketdata::Bar;

use super::{channel_subscribe_frame, subscribe_envelope};
use crate::history::{bar_code, fetch_klines_latest, volume_column};

const SEED_LIMIT: usize = 100; // OKX history-candles caps a single request at 100 rows

/// The `i`-th element of an OKX candle row as a `&str`, if present and a string.
fn s(r: &[Value], i: usize) -> Option<&str> {
    r.get(i).and_then(Value::as_str)
}

/// One OKX candle row (`[ts,o,h,l,c,vol,volCcy,volCcyQuote,confirm]`, decimal strings) of `instrument`
/// → `(Bar, is_closed)`. `None` if a required field is missing/unparseable. `confirm == "1"` ⇒ the
/// candle is FINAL; the volume is the base coin's column (`crate::history`'s `volume_column`, the one
/// the REST pager reads) and defaults to 0.0 on a parse miss. Uses the shared binance `kline_to_bar`
/// so bars match every venue.
fn row_to_bar(r: &[Value], instrument: &str) -> Option<(Bar, bool)> {
    let ts = s(r, 0)?.parse::<i64>().ok()?;
    let o = s(r, 1)?.parse::<f64>().ok()?;
    let h = s(r, 2)?.parse::<f64>().ok()?;
    let l = s(r, 3)?.parse::<f64>().ok()?;
    let c = s(r, 4)?.parse::<f64>().ok()?;
    let v = s(r, volume_column(instrument)).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    let confirmed = s(r, 8) == Some("1"); // OKX confirm flag: "1"=closed, "0"=forming
    Some((kline_to_bar(ts, o, h, l, c, v), confirmed))
}

/// Route ONE OKX WS text frame → a classified kline frame. Pure — testable without a socket. The
/// subscribe OUTCOME envelope is classified FIRST ([`subscribe_envelope`]). Otherwise only a
/// `candle*` channel data frame carries a kline, as a [`BarKline`] whose `closed` is `confirm=="1"`.
/// Other channels and the bare-text `pong` keepalive → [`FeedFrame::Ignored`].
pub fn route_frame(text: &str) -> FeedFrame<BarKline> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return FeedFrame::Ignored; // e.g. the bare-text "pong" keepalive is not JSON
    };
    if let Some(envelope) = subscribe_envelope(&v) {
        return envelope;
    }
    let channel = v.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str);
    if !channel.is_some_and(|c| c.starts_with("candle")) {
        return FeedFrame::Ignored; // non-candle channels
    }
    let Some(row) =
        v.get("data").and_then(Value::as_array).and_then(|d| d.first()).and_then(Value::as_array)
    else {
        return FeedFrame::Ignored;
    };
    // The frame names its instrument (`arg.instId`), whose class picks the base-volume column.
    let instrument =
        v.get("arg").and_then(|a| a.get("instId")).and_then(Value::as_str).unwrap_or("");
    let Some((bar, closed)) = row_to_bar(row, instrument) else {
        return FeedFrame::Ignored;
    };
    FeedFrame::Data(BarKline { bar, closed })
}

/// The subscribe frame: the kline stream `candle<bar>` on `instrument` (`bar` = OKX's bar code).
pub fn subscribe_frame(bar: &str, instrument: &str) -> String {
    channel_subscribe_frame(&format!("candle{bar}"), instrument)
}

/// The candle lane's plan. `interval` is the ORIGINAL binance-style string ("1m") kept as the series key
/// so series line up across venues; the OKX bar code drives the `candle<bar>` subscribe. An unsupported
/// interval is the lane's refusal (no reconnect could fix it).
///
/// ⚠ The BUSINESS socket: the public one rejects `candle<bar>` ("wrong URL or channel"), which the driver
/// reads as a session error and retries for ever, so a candle lane there is silent and never delivers a bar.
///
/// The REST warm-up is the newest [`SEED_LIMIT`] CLOSED candles: `history-candles` is closed-only, so there
/// is no forming tail (the spec's `SEED_LAST_BAR_FORMING` is `false`); the WS `candle` channel delivers the
/// in-progress bar. Once per feed, not per session.
pub(super) fn bars_plan(
    instrument: &str,
    interval: &str,
    business_websocket: &str,
    history_rest: &str,
) -> Result<DataClientLanePlan, String> {
    let bar = bar_code(interval)?;
    let (rest, instrument_owned, interval_owned) =
        (history_rest.to_string(), instrument.to_string(), interval.to_string());
    Ok(DataClientLanePlan::Session {
        url: business_websocket.to_string(),
        subscribe: Some(subscribe_frame(bar, instrument)),
        keepalive: None,
        seed: Some(Box::new(move || {
            fetch_klines_latest(&rest, &instrument_owned, &interval_owned, SEED_LIMIT)
        })),
        decode: Box::new(decode_candle),
    })
}

/// One candle-channel frame through the fixture-tested [`route_frame`]: a candle is the engine's
/// [`DataClientEvent::Bar`] (OKX's `confirm` says closed or forming) and confirms the subscribe handshake;
/// an ack confirms it dataless; a venue REJECT is the session's attributable fatal error.
pub fn decode_candle(
    txt: &str,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    route_frame(txt).outcome(|kline| {
        emit(DataClientEvent::Bar(kline));
        WebsocketSessionFrameOutcome::Confirm
    })
}

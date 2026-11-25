//! The SWAP mark-price lane: the `mark-price` frame classifier, subscribe frame and the lane's plan.

use serde_json::Value;
use vike_bridge_core::data_client::event::{DataClientEvent, DataClientMark};
use vike_bridge_core::data_client::ws::DataClientLanePlan;
use vike_bridge_core::feed_frame::FeedFrame;
use vike_bridge_core::websocket_session::market::WebsocketSessionFrameOutcome;
use vike_marketdata::FundingRate;

use super::subscribe_envelope;
use crate::feed::tick::KEEPALIVE;
use crate::feed::tick::trades::row_str;

// --- Perp mark-price feed (mark-slot semantics, W2-T4) ---------------------------------------
// OKX `mark-price` channel: the venue's REAL mark price (the price its liquidation/funding engine
// keys off), pushed on change (and at least every second). Feeds `DataClientSink::mark_tick` (the
// `PriceBoard` MARK slot); the candle pump's closes ride `bar_close_tick`. SWAP-only: opened
// automatically next to a `*-SWAP` bars subscription (a `mark_streams = 0` row disables). Same host
// + subscribe/ack grammar as the candle feed.

/// Route ONE OKX WS text frame from the `mark-price` channel → a classified mark frame,
/// [`route_frame`]'s mark-pump twin. Pure — fixture-tested without a socket; the subscribe envelope
/// is [`subscribe_envelope`]'s (br7). A fresh mark is `data[0].markPx` + `data[0].ts` (both decimal
/// strings on the wire); a non-`mark-price` frame, a malformed row, or a dead (0/neg/NaN) mark is
/// [`FeedFrame::Ignored`].
///
/// [`route_frame`]: crate::feed::bar::route_frame
pub fn route_mark_frame(text: &str) -> FeedFrame<DataClientMark> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return FeedFrame::Ignored; // the bare-text "pong" keepalive is not JSON
    };
    route_mark_value(&v)
}

/// [`route_mark_frame`] over a frame already parsed (the lane parses each frame once and also reads
/// the `funding-rate` channel off the same socket, [`route_funding_value`]).
fn route_mark_value(v: &Value) -> FeedFrame<DataClientMark> {
    let channel = v.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str);
    if let Some(envelope) = subscribe_envelope(v) {
        // The `funding-rate` channel's own ack proves nothing about the mark channel.
        return match envelope {
            FeedFrame::Ack if channel == Some("funding-rate") => FeedFrame::Ignored,
            envelope => envelope,
        };
    }
    if channel != Some("mark-price") {
        return FeedFrame::Ignored;
    }
    let Some(row) = v.get("data").and_then(Value::as_array).and_then(|d| d.first()) else {
        return FeedFrame::Ignored;
    };
    let Some(px) = row_str(row, "markPx").and_then(|s| s.parse::<f64>().ok()) else {
        return FeedFrame::Ignored;
    };
    if !vike_bridge_core::is_valid_mark(px) {
        return FeedFrame::Ignored;
    }
    let ts = row_str(row, "ts").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    FeedFrame::Data(DataClientMark { px, ts })
}

/// The `funding-rate` channel's row as the sink's [`FundingRate`] for `instrument`, or `None` for any other
/// frame: `fundingRate` is the CURRENT period's rate, settled at `fundingTime`; the interval is
/// `nextFundingTime - fundingTime` when both are stated; `nextFundingRate` is the venue's prediction (an
/// empty string when it states none). A row without a finite rate or a settlement time states nothing.
pub fn route_funding_value(v: &Value, instrument: &str) -> Option<FundingRate> {
    if v.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str) != Some("funding-rate") {
        return None;
    }
    let row = v.get("data").and_then(Value::as_array).and_then(|d| d.first())?;
    let number =
        |k: &str| row_str(row, k).and_then(|s| s.parse::<f64>().ok()).filter(|x| x.is_finite());
    let ms = |k: &str| row_str(row, k).and_then(|s| s.parse::<i64>().ok()).filter(|t| *t > 0);
    let (rate, next_ts) = (number("fundingRate")?, ms("fundingTime")?);
    Some(FundingRate {
        timestamp: ms("ts").unwrap_or(0),
        symbol: instrument.to_string(),
        rate,
        interval_ms: ms("nextFundingTime").and_then(|after| u64::try_from(after - next_ts).ok()),
        next_timestamp: Some(next_ts),
        predicted: number("nextFundingRate"),
        // OKX charges funding at the mark.
        price: None,
    })
}

/// The subscribe frame of the SWAP mark lane on `instrument`: the public `mark-price` channel and, on the same
/// socket, the `funding-rate` channel (the venue's public funding rate and next settlement) — the
/// `{"op":"subscribe",...}` shape of [`subscribe_frame`] (candles) with two channels.
///
/// [`subscribe_frame`]: crate::feed::bar::subscribe_frame
pub fn mark_subscribe_frame(instrument: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [
            {"channel": "mark-price", "instId": instrument},
            {"channel": "funding-rate", "instId": instrument}
        ]
    })
    .to_string()
}

/// The SWAP mark-price lane's plan, on the PUBLIC socket: [`mark_subscribe_frame`] (the `mark-price`
/// channel and its `funding-rate` twin), no seed (a live-only valuation stream; the resolver's bar-close
/// fallback covers the pre-first-frame window), every frame through [`decode_mark`]. Subscribes `instrument` (the
/// instId) and emits under `series` (its USER name, decision 0290), the key the candle lane uses.
pub(super) fn mark_plan(
    series: &str,
    instrument: &str,
    public_websocket: &str,
) -> DataClientLanePlan {
    let series = series.to_string();
    DataClientLanePlan::Session {
        url: public_websocket.to_string(),
        subscribe: Some(mark_subscribe_frame(instrument)),
        keepalive: Some(KEEPALIVE),
        notice: Some(super::market_service_upgrade_notice),
        seed: None,
        decode: Box::new(move |txt, emit| decode_mark(txt, &series, emit)),
    }
}

/// One frame of the mark lane: a `funding-rate` row is the engine's [`DataClientEvent::Funding`] and never
/// confirms the mark channel (a stalled `mark-price` feed must not read as healthy while `funding-rate` keeps
/// arriving); a mark is [`DataClientEvent::Mark`] and confirms.
pub fn decode_mark(
    txt: &str,
    instrument: &str,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return WebsocketSessionFrameOutcome::Ignore; // the bare-text "pong" keepalive is not JSON
    };
    if let Some(funding) = route_funding_value(&v, instrument) {
        emit(DataClientEvent::Funding(funding));
        return WebsocketSessionFrameOutcome::Ignore;
    }
    route_mark_value(&v).outcome(|mark| {
        emit(DataClientEvent::Mark(mark));
        WebsocketSessionFrameOutcome::Confirm
    })
}

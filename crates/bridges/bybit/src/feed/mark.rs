//! The perp mark-price lane: the `tickers` classifier, subscribe frame and the lane plan.

use serde_json::Value;
use vike_bridge_core::data_client::event::{DataClientEvent, DataClientMark};
use vike_bridge_core::data_client::ws::DataClientLanePlan;
use vike_bridge_core::feed_frame::FeedFrame;
use vike_bridge_core::websocket_session::market::WebsocketSessionFrameOutcome;
use vike_marketdata::FundingRate;

use crate::feed::bar::{subscribe_envelope, topic_subscribe_frame};

use super::{BybitUrlsFeed, feed_route, websocket_host};
use crate::feed::tick::KEEPALIVE;

// --- Perp mark-price feed (mark-slot semantics, W2-T4) ---------------------------------------
// Bybit V5 `tickers.<SYMBOL>` on the LINEAR stream: the venue's REAL `markPrice` (the price its
// liquidation/funding engine keys off), pushed as a `snapshot` then field-sparse `delta`s — a
// delta that doesn't move the mark simply omits `markPrice` and is ignored here. Feeds
// `DataClientSink::mark_tick` (the `PriceBoard` MARK slot); the kline pump's candle closes ride
// `bar_close_tick`. Perp-only: opened automatically next to a `.P` bars subscription
// (a `mark_streams = 0` row disables). Same subscribe/ack grammar as the kline feed.

/// Route ONE Bybit WS text frame from the `tickers` channel → a classified mark frame,
/// [`route_frame`]'s mark-pump twin. Pure — fixture-tested without a socket. The subscribe OUTCOME
/// envelope is classified first ([`subscribe_envelope`]); then a `tickers.*` data push yields
/// `data.markPrice` (a decimal string; ABSENT in a delta that didn't move the mark → Ignored)
/// stamped with the frame's top-level `ts`. A non-tickers frame or a dead (0/neg/NaN) mark is
/// [`FeedFrame::Ignored`].
///
/// [`route_frame`]: crate::feed::bar::route_frame
pub fn route_mark_frame(text: &str) -> FeedFrame<DataClientMark> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return FeedFrame::Ignored; // bare-text keepalive is not JSON
    };
    route_mark_value(&v)
}

/// [`route_mark_frame`] over a frame already parsed (the lane parses each frame once and also folds
/// its funding fields, [`TickerFunding`]).
fn route_mark_value(v: &Value) -> FeedFrame<DataClientMark> {
    if let Some(envelope) = subscribe_envelope(v) {
        return envelope;
    }
    let topic = v.get("topic").and_then(Value::as_str);
    if !topic.is_some_and(|t| t.starts_with("tickers.")) {
        return FeedFrame::Ignored;
    }
    let Some(px) = v
        .get("data")
        .and_then(|d| d.get("markPrice"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
    else {
        return FeedFrame::Ignored; // a delta without a mark move
    };
    if !vike_bridge_core::is_valid_mark(px) {
        return FeedFrame::Ignored;
    }
    let ts = v.get("ts").and_then(Value::as_i64).unwrap_or(0);
    FeedFrame::Data(DataClientMark { px, ts })
}

/// The `tickers` channel's FUNDING fields, folded across a `snapshot` and its field-sparse `delta`s: a
/// delta names `fundingRate`, `nextFundingTime` or `fundingIntervalHour` only when it moves them, so the
/// lane remembers the last of each. One per mark lane (its state survives a reconnect, whose first frame
/// is a fresh snapshot that replaces it).
#[derive(Debug, Default)]
pub struct TickerFunding {
    rate: Option<f64>,
    next_timestamp: Option<i64>,
    interval_h: Option<u64>,
}

impl TickerFunding {
    /// Fold one `tickers.*` frame: the funding observation of `symbol` (the series symbol) when the frame
    /// moved a funding field and both the rate and the next settlement are known; `None` otherwise (a
    /// delta that moved only the mark, a dated future whose `fundingRate` is empty, another channel).
    pub fn fold(&mut self, v: &Value, symbol: &str) -> Option<FundingRate> {
        if !v.get("topic").and_then(Value::as_str).is_some_and(|t| t.starts_with("tickers.")) {
            return None;
        }
        let data = v.get("data")?;
        if v.get("type").and_then(Value::as_str) == Some("snapshot") {
            *self = Self::default();
        }
        let field = |k: &str| data.get(k).and_then(Value::as_str);
        let mut moved = false;
        if let Some(r) =
            field("fundingRate").and_then(|s| s.parse::<f64>().ok()).filter(|r| r.is_finite())
        {
            self.rate = Some(r);
            moved = true;
        }
        if let Some(t) =
            field("nextFundingTime").and_then(|s| s.parse::<i64>().ok()).filter(|t| *t > 0)
        {
            self.next_timestamp = Some(t);
            moved = true;
        }
        if let Some(h) = field("fundingIntervalHour").and_then(|s| s.parse::<u64>().ok()) {
            self.interval_h = Some(h);
            moved = true;
        }
        if !moved {
            return None;
        }
        Some(FundingRate {
            timestamp: v.get("ts").and_then(Value::as_i64).unwrap_or(0),
            symbol: symbol.to_string(),
            rate: self.rate?,
            interval_ms: self.interval_h.map(|h| h * 3_600_000),
            next_timestamp: Some(self.next_timestamp?),
            predicted: None,
            price: None,
        })
    }
}

/// The subscribe frame for the `tickers` channel on `symbol` — same `{"op":"subscribe",...}`
/// shape as [`subscribe_frame`] (klines), no interval code.
///
/// [`subscribe_frame`]: crate::feed::bar::subscribe_frame
pub fn mark_subscribe_frame(symbol: &str) -> String {
    topic_subscribe_frame(&format!("tickers.{symbol}"))
}

/// The perp mark-price lane's plan — the kline lane's mark twin, on whichever derivative host the
/// symbol's book names (spot has no mark price and never pairs one: [`should_pair_mark`]). The route is
/// the bars subscription's own ([`feed_route`]), so the mark reads the book its bars do. Emits under the
/// `.P` SERIES symbol. No REST seed — a live-only valuation stream; the resolver's bar-close fallback
/// covers the pre-first-frame window.
///
/// ⚠ It said "always on the linear socket" and that was a third silent wrong-host site:
/// MEASURED 2026-09-16, `tickers.BTCUSD` on the linear host answers `error:handler not found`, so
/// an inverse perp's mark lane was a permanent reconnect loop feeding a valuation rung that never
/// filled.
///
/// [`should_pair_mark`]: super::should_pair_mark
pub(super) fn mark_plan(
    series_symbol: &str,
    hosts: &BybitUrlsFeed,
) -> Result<DataClientLanePlan, String> {
    let (api_symbol, book) = feed_route(hosts, series_symbol)?;
    let series = series_symbol.to_string();
    let mut funding = TickerFunding::default();
    Ok(DataClientLanePlan::Session {
        url: websocket_host(&hosts.ws, book),
        subscribe: Some(mark_subscribe_frame(&api_symbol)),
        keepalive: Some(KEEPALIVE),
        notice: None, // bybit documents no announced close
        seed: None,
        decode: Box::new(move |txt, emit| decode_mark(txt, &series, &mut funding, emit)),
    })
}

/// One `tickers` frame of the mark lane, parsed once: its funding fields fold through `funding` (the
/// lane's state, kept across reconnects) and a moved funding observation is the engine's
/// [`DataClientEvent::Funding`]; then a mark is [`DataClientEvent::Mark`] and confirms (the subscribe
/// envelope is classified as on every lane).
pub fn decode_mark(
    txt: &str,
    series: &str,
    funding: &mut TickerFunding,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return WebsocketSessionFrameOutcome::Ignore; // bare-text keepalive is not JSON
    };
    // The same `tickers` frame states the public funding rate and its next settlement.
    if let Some(f) = funding.fold(&v, series) {
        emit(DataClientEvent::Funding(f));
    }
    route_mark_value(&v).outcome(|mark| {
        emit(DataClientEvent::Mark(mark));
        WebsocketSessionFrameOutcome::Confirm
    })
}

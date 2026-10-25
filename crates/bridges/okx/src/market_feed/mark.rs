//! The SWAP mark-price lane: the `mark-price` frame classifier, subscribe frame and feed-thread body.

use serde_json::Value;
use vike_bridge_core::feed_frame::{FeedFrame, MarkPx};
use vike_bridge_core::market_pump::{FrameOutcome, SessionStatus, run_market_feed};
use vike_bridge_core::pump_spec::subscribe_only_pump_opts;
use vike_model::now_ms;

use super::trades::row_str;
use super::{FeedCtx, LIVE_STATUS, VENUE, subscribe_envelope};
use crate::market_data::PUBLIC_WS;

// --- Perp mark-price feed (mark-slot semantics, W2-T4) ---------------------------------------
// OKX `mark-price` channel: the venue's REAL mark price (the price its liquidation/funding engine
// keys off), pushed on change (and at least every second). Feeds `LiveDataSink::mark_tick` (the
// `PriceBoard` MARK slot); the candle pump's closes ride `bar_close_tick`. SWAP-only: opened
// automatically next to a `*-SWAP` bars subscription (a `mark_streams = 0` row disables). Same host
// + subscribe/ack grammar as the candle feed.

/// Route ONE OKX WS text frame from the `mark-price` channel → a classified mark frame,
/// [`route_frame`]'s mark-pump twin. Pure — fixture-tested without a socket; the subscribe envelope
/// is [`subscribe_envelope`]'s (br7). A fresh mark is `data[0].markPx` + `data[0].ts` (both decimal
/// strings on the wire); a non-`mark-price` frame, a malformed row, or a dead (0/neg/NaN) mark is
/// [`FeedFrame::Ignored`].
///
/// [`route_frame`]: super::klines::route_frame
pub fn route_mark_frame(text: &str) -> FeedFrame<MarkPx> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return FeedFrame::Ignored; // the bare-text "pong" keepalive is not JSON
    };
    if let Some(envelope) = subscribe_envelope(&v) {
        return envelope;
    }
    let channel = v.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str);
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
    FeedFrame::Data(MarkPx { px, ts })
}

/// The subscribe frame for the public `mark-price` channel on `inst` — same
/// `{"op":"subscribe",...}` shape as [`subscribe_frame`] (candles), no bar code.
///
/// [`subscribe_frame`]: super::klines::subscribe_frame
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
///
/// [`feed_main`]: super::klines::feed_main
pub(super) fn mark_main(inst: String, _interval: String, ctx: FeedCtx) {
    let sub = mark_subscribe_frame(&inst);
    run_market_feed(
        PUBLIC_WS,
        &subscribe_only_pump_opts(VENUE, &sub),
        &ctx.stop,
        &now_ms,
        |txt| {
            route_mark_frame(txt).outcome(|MarkPx { px, ts }| {
                ctx.sink.mark_tick(VENUE, &inst, px, ts);
                (ctx.wake)();
                FrameOutcome::Confirm
            })
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

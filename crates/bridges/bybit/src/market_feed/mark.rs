//! The perp mark-price lane: the `tickers` classifier, subscribe frame and feed-thread body.

use serde_json::Value;
use vike_bridge_core::feed_frame::{FeedFrame, MarkPx};
use vike_bridge_core::market_pump::{FrameOutcome, SessionStatus, run_market_feed};
use vike_bridge_core::pump_spec::subscribe_only_pump_opts;
use vike_model::now_ms;

use super::klines::subscribe_envelope;
use super::{FeedCtx, LIVE_STATUS, VENUE, feed_route, ws_host};

// --- Perp mark-price feed (mark-slot semantics, W2-T4) ---------------------------------------
// Bybit V5 `tickers.<SYMBOL>` on the LINEAR stream: the venue's REAL `markPrice` (the price its
// liquidation/funding engine keys off), pushed as a `snapshot` then field-sparse `delta`s — a
// delta that doesn't move the mark simply omits `markPrice` and is ignored here. Feeds
// `LiveDataSink::mark_tick` (the `PriceBoard` MARK slot); the kline pump's candle closes ride
// `bar_close_tick`. Perp-only: opened automatically next to a `.P` bars subscription
// (a `mark_streams = 0` row disables). Same subscribe/ack grammar as the kline feed.

/// Route ONE Bybit WS text frame from the `tickers` channel → a classified mark frame,
/// [`route_frame`]'s mark-pump twin. Pure — fixture-tested without a socket. The subscribe OUTCOME
/// envelope is classified first ([`subscribe_envelope`]); then a `tickers.*` data push yields
/// `data.markPrice` (a decimal string; ABSENT in a delta that didn't move the mark → Ignored)
/// stamped with the frame's top-level `ts`. A non-tickers frame or a dead (0/neg/NaN) mark is
/// [`FeedFrame::Ignored`].
///
/// [`route_frame`]: super::klines::route_frame
pub fn route_mark_frame(text: &str) -> FeedFrame<MarkPx> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return FeedFrame::Ignored; // bare-text keepalive is not JSON
    };
    if let Some(envelope) = subscribe_envelope(&v) {
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
    FeedFrame::Data(MarkPx { px, ts })
}

/// The subscribe frame for the `tickers` channel on `symbol` — same `{"op":"subscribe",...}`
/// shape as [`subscribe_frame`] (klines), no interval code.
///
/// [`subscribe_frame`]: super::klines::subscribe_frame
pub fn mark_subscribe_frame(symbol: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [format!("tickers.{symbol}")]
    })
    .to_string()
}

/// The perp mark-price feed-thread body — [`feed_main`]'s mark twin on the same shared driver, on
/// whichever derivative host the symbol's book names (spot has no mark price and never spawns one —
/// [`should_pair_mark`]). Emits under the `.P` SERIES symbol. No REST seed — a live-only valuation
/// stream; the resolver's bar-close fallback covers the pre-first-frame window.
///
/// ⚠ It said "always on [`PUBLIC_WS_LINEAR`]" and that was a third silent wrong-host site:
/// MEASURED 2026-09-16, `tickers.BTCUSD` on the linear host answers `error:handler not found`, so
/// an inverse perp's mark lane was a permanent reconnect loop feeding a valuation rung that never
/// filled.
///
/// [`feed_main`]: super::klines::feed_main
/// [`should_pair_mark`]: super::should_pair_mark
/// [`PUBLIC_WS_LINEAR`]: super::PUBLIC_WS_LINEAR
pub(super) fn mark_main(series_symbol: String, ctx: FeedCtx) {
    let (api_symbol, book) = match feed_route(&series_symbol) {
        Ok(v) => v,
        Err(e) => {
            ctx.set_status(format!("{series_symbol} mark: {e}"));
            return;
        }
    };
    let sub = mark_subscribe_frame(&api_symbol);
    run_market_feed(
        ws_host(book),
        &subscribe_only_pump_opts(VENUE, &sub),
        &ctx.stop,
        &now_ms,
        |txt| {
            route_mark_frame(txt).outcome(|MarkPx { px, ts }| {
                ctx.sink.mark_tick(VENUE, &series_symbol, px, ts);
                (ctx.wake)();
                FrameOutcome::Confirm
            })
        },
        || {},
        |s| match s {
            // ⚠ This lane had NO healthy string at all before this change — it was a
            // pure-DEGRADATION writer on a mutex shared with the kline and trades lanes, i.e. a
            // lane that could push the venue to `Degraded` and never back. On a spot start it never
            // spawns, so the property was invisible; on a `.P` start it is exactly the latch this
            // PR cures, wearing a different lane. Giving it the arm is a CONDITION of keeping
            // bybit's `recon_feed_statuses` row, not a tidy-up — see that method's doc.
            //
            // The SAME text as the other two lanes, per [`LIVE_STATUS`]'s one-string-per-venue
            // rule: a `.P` start runs two lanes on one mutex, and per-lane spellings would make
            // every session boundary on either lane a text TRANSITION against the other's.
            SessionStatus::Live => ctx.set_status(LIVE_STATUS.into()),
            SessionStatus::Error(e) => {
                ctx.set_status(format!("{series_symbol} mark ws error (reconnecting): {e}"))
            }
        },
    );
}

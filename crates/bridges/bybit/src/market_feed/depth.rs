//! The L2 depth (DOM) lane: the `orderbook.200` fold over the shared depth driver.

use std::time::Duration;

use vike_bridge_core::depth::{BookOp, infer_tick_size, run_depth_feed};
use vike_bridge_core::pump_spec::READ_POLL;
use vike_data::StreamLabel;
use vike_model::{StreamStatus, now_ms};
use vike_venues::venue_timings::BYBIT;

use super::{FeedCtx, VENUE, feed_route, ws_host};

// --- L2 depth feed (DOM) --------------------------------------------------------------------
// Bybit V5 `orderbook.200.<SYM>` on the stream `feed_route` resolves the symbol to (spot, linear or
// inverse — `depth_main` carries the route): a WS `snapshot` then `delta`s — no REST seed. Reuses
// the fixture-tested folding in `crate::market_data::route_frame` for the PROTOCOL + the shared
// `vike_bridge_core::depth` driver for the LIFECYCLE. Delivers the top `DEPTH_LEVELS` via
// `LiveDataSink::l2_snapshot`.
//
// The four `DEPTH_*` timings below are this venue's `depth` block in
// `crates/vike-venues/src/venue_timings.rs` (`BYBIT`), which carries each value and its
// reason (decision 0160); the driver's stop-flag poll is `READ_POLL`, the venue-free mechanism
// default.

/// Levels per side published to the DOM.
const DEPTH_LEVELS: usize = 200;
/// Reconnect backoff after a socket error / gap.
const DEPTH_BACKOFF: Duration = BYBIT.depth().backoff();
/// Bybit drops a client that doesn't ping within ~20 s — the driver sends this every ~15 s.
const KEEPALIVE: &str = r#"{"op":"ping"}"#;
/// Net-hardening §B dead-transport watchdog: no orderbook frame of ANY kind (a snapshot/delta OR the
/// `{"op":"pong"}` reply to our keepalive) for this long means the socket is silently dead → the
/// driver discloses a gap and reconnects+re-seeds. Well above the 15 s keepalive cadence, so a quiet
/// book kept alive purely by ping/pong never false-trips; only true transport silence does.
const DEPTH_IDLE_THRESHOLD: Duration = BYBIT.depth().idle_threshold();
/// §B data-freshness threshold for the depth book: how long the book may go without an applied
/// update, behind a still-live transport, before disclosing `Stale` (disclose-only, no auto-action;
/// the fast dead-socket signal is [`DEPTH_IDLE_THRESHOLD`], 30s). **Validated by live measurement
/// (2026-07-11) across the binance/bybit/okx keyless depth feeds: liquid pairs (BTC/ETH) update
/// ~0.1s, mid-caps ~3.6s max, and the market's THINNEST actively-traded pairs gapped 44–47s (a
/// near-dead pair updated only twice in 5 min).** 120s clears the thin-pair worst case with ~2.5×
/// margin, so only a genuinely near-dead book trips — where the `Stale` disclosure is accurate. Kept
/// at 120s (measured floor, not a placeholder); a uniform per-venue value must cover the thinnest
/// subscribable symbol (#111).
const DEPTH_FRESHNESS_THRESHOLD: Duration = BYBIT.depth().freshness_threshold();
/// Net-hardening timed book re-seed: forced fresh-snapshot cadence. Bybit's `orderbook.200` carries NO
/// checksum, so a dropped/mis-applied `delta` that does NOT break the seq chain corrupts the managed
/// book silently — no `BookOp::Gap`, no idle, no freshness trip fires, and it would stay wrong until
/// the next natural reconnect (hours). Ending the session every `DEPTH_RESEED_INTERVAL` forces the
/// driver to reconnect + re-subscribe (a fresh WS `snapshot` rebuilds the book), bounding that
/// corruption window to <= this — reusing the gap handler's reconnect path but disclosing NO transport
/// gap (a scheduled refresh, not an outage). 5 min is a conservative default, well above the reconnect
/// cost; `None` would disable it.
const DEPTH_RESEED_INTERVAL: Duration = BYBIT.depth().reseed_interval();

/// The venue EVENT-TIME (epoch-ms) of a Bybit orderbook frame — its top-level `ts` — for the §B
/// data-freshness watchdog (`0` if absent → the driver falls back to receive-time). Re-parses just
/// that field from the raw frame, so it serves ONLY the once-per-seed first-snapshot arm of
/// [`fold_depth_frame`] (which reads that snapshot with `parse_orderbook_snapshot`, not `route_frame`):
/// every seeded frame takes its time from `MdEvent::BookUpdated { ts_ms }`, one parse per delta.
pub(super) fn frame_ts_ms(txt: &str) -> i64 {
    serde_json::from_str::<serde_json::Value>(txt)
        .ok()
        .and_then(|v| v.get("ts").and_then(|t| t.as_i64()))
        .unwrap_or(0)
}

/// Publish the top [`DEPTH_LEVELS`] to the DOM sink (carrying the book's tick) + nudge a repaint.
fn publish_book(book: &vike_marketdata::L2Book, symbol: &str, ctx: &FeedCtx) {
    let (bids, asks) = book.top_n(DEPTH_LEVELS);
    ctx.sink.l2_snapshot(VENUE, symbol, book.tick_size, bids, asks, now_ms());
    (ctx.wake)();
}

/// Fold one depth WS frame into the (maybe not-yet-seeded) book slot — the driver `decode` body,
/// named so the frame→[`BookOp`] mapping is unit-testable without a socket. Before the first
/// `snapshot` everything else is [`BookOp::Ignored`]; once seeded, frames fold via the
/// fixture-tested `crate::market_data::route_frame` (fully qualified — this module's own
/// `route_frame` decodes KLINES), and its [`MdEvent::Resync`](crate::market_data::MdEvent::Resync)
/// — a dropped delta frame or a venue-restart `u` regression, judged by the shared
/// `L2Book::delta_decision` strict law — maps to [`BookOp::Gap`], on which the shared driver
/// discloses the gap and reconnects + re-seeds IMMEDIATELY (previously the fold had no gap arm
/// and a dropped frame silently corrupted the DOM book until the ~5-min timed reseed).
pub fn fold_depth_frame(
    txt: &str,
    sym: &str,
    book: &mut Option<vike_marketdata::L2Book>,
) -> BookOp {
    match book.as_mut() {
        None => match crate::market_data::parse_orderbook_snapshot(txt) {
            Some((seq, b, a)) => {
                let mut bk = vike_marketdata::L2Book::new(infer_tick_size(&b, &a));
                bk.apply_snapshot(seq, &b, &a);
                *book = Some(bk);
                BookOp::Updated(frame_ts_ms(txt)) // §B: venue event-time from the frame's `ts`
            }
            None => BookOp::Ignored, // a delta/ack before the first snapshot
        },
        Some(bk) => match crate::market_data::route_frame(txt, sym, bk) {
            crate::market_data::MdEvent::BookUpdated { ts_ms } => BookOp::Updated(ts_ms),
            crate::market_data::MdEvent::Resync => BookOp::Gap, // seq gap — driver re-seeds
            _ => BookOp::Ignored,                               // Trade or Ignored
        },
    }
}

/// Depth feed body on the shared driver. Bybit's protocol is WS-seeded (no REST): the first
/// `snapshot` frame builds the book (tick inferred from it); subsequent `snapshot`/`delta` frames
/// fold via [`fold_depth_frame`], whose gap arm hands [`BookOp::Gap`] to the driver's
/// reconnect+re-seed lifecycle.
/// ⚠ **THIS LANE'S ROUTE CHANGED, and the change is visible to anyone who opens a bybit DOM.**
/// It used to call NOTHING: the raw `symbol` was interpolated into the topic and the socket was
/// [`PUBLIC_WS_LINEAR`] unconditionally — the third and worst of the three readings this file used
/// to have of one string (`perp_split`'s doc named it as such). Two defects, one line:
///
///   * a `.P` symbol subscribed `orderbook.200.BTCUSDT.P`, a topic that does not exist, so the DOM
///     never populated and nothing said why; and
///   * a BARE symbol got the LINEAR perp book under a SPOT label. That was written down as
///     deliberate ("the DOM ladder matches the venue `BybitPerpRest` executes on") and it is still
///     a book that disagrees with its own label — the same class of silence
///     `docs/decisions/0061-an-instrument-names-its-kind.md` exists about, and the reason a bybit
///     spot DOM has never shown the spot book.
///
/// Both now go through [`feed_route`], so this lane reaches exactly the book the series label
/// names. **A bare bybit symbol's DOM therefore moves from the perp book to the spot book** — a
/// user-visible change, stated here rather than discovered. Nothing that TRADES reads this lane:
/// `subscribe_depth` is the chart DOM (`vike_app_core::ui::feed_lifecycle`'s `ensure_depth`), while
/// `vike-tradehub`'s bybit tick lane is `crates/bridges/bybit/src/market_data.rs` and is untouched.
///
/// [`PUBLIC_WS_LINEAR`]: super::PUBLIC_WS_LINEAR
pub(super) fn depth_main(series_symbol: String, _interval: String, ctx: FeedCtx) {
    let (api_symbol, book) = match feed_route(&series_symbol) {
        Ok(v) => v,
        Err(e) => {
            ctx.set_status(format!("{series_symbol} depth: {e}"));
            return;
        }
    };
    let sub = format!(r#"{{"op":"subscribe","args":["orderbook.200.{api_symbol}"]}}"#);
    // The book's own key stays the SERIES label — the sink and every consumer key on what the
    // caller asked for, exactly as the kline and trades lanes re-label. Only the topic is stripped.
    let symbol = series_symbol;
    let sym = symbol.as_str();
    let seed = || None::<vike_marketdata::L2Book>;
    let decode = |txt: &str, book: &mut Option<vike_marketdata::L2Book>| -> BookOp {
        fold_depth_frame(txt, sym, book)
    };
    let publish = |b: &vike_marketdata::L2Book| publish_book(b, sym, &ctx);
    // Net-hardening §B: hand each `StreamStatus` the driver emits to the sink's machine-readable
    // disclosure. The depth driver owns the `StreamHealth` that dedups transport gaps across
    // reconnects (one outage → one GapStart/Live pair) now; the venue just discloses.
    let on_health = |ev: StreamStatus| {
        ctx.sink.stream_status(VENUE, sym, StreamLabel::Depth.as_str(), ev);
    };
    run_depth_feed(
        // The book the SERIES LABEL names — see this function's ⚠ for what that changed.
        ws_host(book),
        Some(&sub),
        Some(KEEPALIVE),
        seed,
        decode,
        publish,
        on_health,
        &ctx.stop,
        READ_POLL,
        DEPTH_IDLE_THRESHOLD,
        DEPTH_FRESHNESS_THRESHOLD,
        Some(DEPTH_RESEED_INTERVAL),
        &now_ms,
        DEPTH_BACKOFF,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The EFFECTIVE depth-driver timings, each a literal this file held before the venue timings
    /// table existed (decision 0160): evidence beside the table's own matrix pin, so a row edit that
    /// retunes bybit's DOM watchdog fails here too and is a deliberate act in both places.
    #[test]
    fn the_depth_driver_runs_on_the_timings_it_always_did() {
        assert_eq!(DEPTH_IDLE_THRESHOLD, Duration::from_secs(30));
        assert_eq!(DEPTH_FRESHNESS_THRESHOLD, Duration::from_secs(120));
        assert_eq!(DEPTH_RESEED_INTERVAL, Duration::from_secs(300));
        assert_eq!(DEPTH_BACKOFF, Duration::from_secs(3));
        assert_eq!(READ_POLL, Duration::from_secs(2));
    }
}

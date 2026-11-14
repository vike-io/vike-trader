//! The L2 depth (DOM) lane: the `orderbook.200` fold and the lane's plan on the shared depth driver.

use std::sync::Arc;

use vike_bridge_core::data_client::event::DataClientEvent;
use vike_bridge_core::data_client::ws::DataClientLanePlan;
use vike_bridge_core::depth::{WebsocketSessionDepthOperation, infer_tick_size};
use vike_venues::venue_timings::BYBIT;

use super::{BybitUrlsFeed, feed_route, websocket_host};
use crate::feed::tick::{KEEPALIVE, book_resubscribe};
use crate::history::Category;

// --- L2 depth feed (DOM) --------------------------------------------------------------------
// Bybit V5 `orderbook.200.<SYM>` on the stream `feed_route` resolves the symbol to (spot, linear or
// inverse — `depth_main` carries the route): a WS `snapshot` then `delta`s — no REST seed. Reuses
// the fixture-tested folding in `crate::feed::tick::route_frame` for the PROTOCOL + the shared
// `vike_bridge_core::depth` driver for the LIFECYCLE (the data-client engine's `Depth` plan).
// Delivers the top `DEPTH_LEVELS` via `DataClientSink::l2_snapshot`.
//
// The four depth timings are this venue's `depth` block in `crates/vike-venues/src/venue_timings.rs`
// (`BYBIT`), which carries each value and its reason (decision 0160), handed to the driver as the row:
// the 30 s dead-transport watchdog (no orderbook frame of ANY kind, a snapshot/delta OR the
// `{"op":"pong"}` reply to the keepalive; well above the 15 s keepalive cadence, so a quiet book kept
// alive by ping/pong never false-trips), the 120 s freshness limit (measured 2026-07-11 across the
// binance/bybit/okx keyless depth feeds: the THINNEST actively-traded pairs gapped 44–47 s), the 3 s
// backoff, and the 300 s timed re-seed. ⚠ Bybit's `orderbook.200` carries NO checksum, so a
// dropped/mis-applied `delta` that does NOT break the seq chain corrupts the book silently; the timed
// re-seed (the topic re-subscribed ON THE SAME SOCKET, `crate::feed::tick::book_resubscribe`, its fresh
// snapshot rebuilding the book, no reconnect and no transport gap) is the only bound on that window.
// The driver's stop-flag poll is the venue's pump row's read timeout.

/// Levels per side published to the DOM.
const DEPTH_LEVELS: usize = 200;

/// The venue EVENT-TIME (epoch-ms) of a Bybit orderbook frame — its top-level `ts` — for the §B
/// data-freshness watchdog (`0` if absent → the driver falls back to receive-time). Re-parses just
/// that field from the raw frame, so it serves ONLY the once-per-seed first-snapshot arm of
/// [`fold_depth_frame`] (which reads that snapshot with `parse_orderbook_snapshot`, not `route_frame`):
/// every seeded frame takes its time from `DataClientEvent::BookUpdated { ts_ms }`, one parse per delta.
pub(super) fn frame_ts_ms(txt: &str) -> i64 {
    serde_json::from_str::<serde_json::Value>(txt)
        .ok()
        .and_then(|v| v.get("ts").and_then(|t| t.as_i64()))
        .unwrap_or(0)
}

/// Fold one depth WS frame into the (maybe not-yet-seeded) book slot — the driver `decode` body,
/// named so the frame→[`WebsocketSessionDepthOperation`] mapping is unit-testable without a socket. Before the first
/// `snapshot` everything else is [`WebsocketSessionDepthOperation::Ignored`]; once seeded, frames fold via the
/// fixture-tested `crate::feed::tick::route_frame` (fully qualified — this module's own
/// `route_frame` decodes KLINES), and its [`DataClientEvent::Resync`]
/// — a dropped delta frame or a venue-restart `u` regression, judged by the shared
/// `L2Book::delta_decision` strict law — maps to [`WebsocketSessionDepthOperation::Gap`], on which the shared driver
/// discloses the gap and reconnects + re-seeds IMMEDIATELY (previously the fold had no gap arm
/// and a dropped frame silently corrupted the DOM book until the ~5-min timed reseed).
pub fn fold_depth_frame(
    txt: &str,
    symbol: &str,
    book: &mut Option<Arc<vike_marketdata::L2Book>>,
) -> WebsocketSessionDepthOperation {
    match book.as_mut().map(Arc::make_mut) {
        None => match crate::feed::tick::parse_orderbook_snapshot(txt) {
            Some((seq, b, a)) => {
                let mut bk = vike_marketdata::L2Book::new(infer_tick_size(&b, &a));
                bk.apply_snapshot(seq, &b, &a);
                *book = Some(Arc::new(bk));
                WebsocketSessionDepthOperation::Updated(frame_ts_ms(txt)) // §B: venue event-time from the frame's `ts`
            }
            None => WebsocketSessionDepthOperation::Ignored, // a delta/ack before the first snapshot
        },
        Some(bk) => match crate::feed::tick::route_frame(txt, symbol, bk) {
            DataClientEvent::BookUpdated { timestamp_ms: ts_ms } => {
                WebsocketSessionDepthOperation::Updated(ts_ms)
            }
            DataClientEvent::Resync => WebsocketSessionDepthOperation::Gap, // seq gap — driver re-seeds
            _ => WebsocketSessionDepthOperation::Ignored,                   // Trade or Ignored
        },
    }
}

/// The DOM lane (`subscribe_depth`): its route, then [`depth_plan`]. Bybit's protocol is WS-seeded
/// (no REST): the first `snapshot` frame builds the book (tick inferred from it); subsequent
/// `snapshot`/`delta` frames fold via [`fold_depth_frame`], whose gap arm hands [`WebsocketSessionDepthOperation::Gap`] to the
/// driver's reconnect+re-seed lifecycle.
/// ⚠ **THIS LANE'S ROUTE CHANGED, and the change is visible to anyone who opens a bybit DOM.**
/// It used to call NOTHING: the raw `symbol` was interpolated into the topic and the socket was
/// the linear one unconditionally — the third and worst of the three readings this file used
/// to have of one string (`perp_split`'s doc named it as such). Two defects, one line:
///
///   * a `.P` symbol subscribed `orderbook.200.BTCUSDT.P`, a topic that does not exist, so the DOM
///     never populated and nothing said why; and
///   * a BARE symbol got the LINEAR perp book under a SPOT label. That was written down as
///     deliberate ("the DOM ladder matches the venue `VenueRestBybit` executes on") and it is still
///     a book that disagrees with its own label — the same class of silence
///     `docs/decisions/0061-an-instrument-names-its-kind.md` exists about, and the reason a bybit
///     spot DOM has never shown the spot book.
///
/// Both now go through [`feed_route`], so this lane reaches exactly the book the series label
/// names. **A bare bybit symbol's DOM therefore moves from the perp book to the spot book** — a
/// user-visible change, stated here rather than discovered. Nothing that TRADES reads this lane:
/// `subscribe_depth` is the chart DOM (`vike_app_core::ui::feed_lifecycle`'s `ensure_depth`), while
/// `vike-tradehub`'s bybit tick lane is `crates/bridges/bybit/src/feed/tick/mod.rs` and is untouched.
pub(super) fn depth_main(
    series_symbol: &str,
    hosts: &BybitUrlsFeed,
) -> Result<DataClientLanePlan, String> {
    let (api_symbol, book) = feed_route(hosts, series_symbol)?;
    Ok(depth_plan(&hosts.ws, &api_symbol, book, series_symbol))
}

/// The DOM lane's plan for `api_symbol` on `book`'s socket: the `orderbook.200` subscribe, the
/// keepalive, the in-place re-seed (the topic re-subscribed, its fresh snapshot frame rebuilding the
/// book, [`fold_depth_frame`]'s `None` arm), no REST seed, `BYBIT`'s depth row. The book's own key stays
/// the SERIES label — the sink and every consumer key on what the caller asked for, exactly as the kline
/// and trades lanes re-label; only the topic is stripped.
fn depth_plan(
    ws: &str,
    api_symbol: &str,
    book: Category,
    series_symbol: &str,
) -> DataClientLanePlan {
    let topic = format!("orderbook.200.{api_symbol}");
    let series = series_symbol.to_string();
    DataClientLanePlan::Depth {
        // The book the SERIES LABEL names — see `depth_main`'s ⚠ for what that changed.
        url: websocket_host(ws, book),
        subscribe: Some(format!(r#"{{"op":"subscribe","args":["{topic}"]}}"#)),
        keepalive: Some(KEEPALIVE),
        resubscribe: book_resubscribe(&topic).to_vec(),
        seed: Box::new(|| None),
        decode: Box::new(move |txt, book| fold_depth_frame(txt, &series, book)),
        levels: DEPTH_LEVELS,
        timings: BYBIT.depth(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// The EFFECTIVE depth-driver timings, each a literal this file held before the venue timings
    /// table existed (decision 0160): evidence beside the table's own matrix pin, so a row edit that
    /// retunes bybit's DOM watchdog fails here too and is a deliberate act in both places. The stop-flag
    /// poll is the pump row's read timeout (the engine hands it to the depth driver).
    #[test]
    fn the_depth_driver_runs_on_the_timings_it_always_did() {
        let plan = depth_plan("wss://stream.bybit.com", "BTCUSDT", Category::Linear, "BTCUSDT.P");
        let DataClientLanePlan::Depth {
            url,
            subscribe,
            keepalive,
            resubscribe,
            timings,
            levels,
            ..
        } = plan
        else {
            panic!("the DOM lane is a depth plan")
        };
        assert_eq!(url, "wss://stream.bybit.com/v5/public/linear");
        assert_eq!(
            subscribe.as_deref(),
            Some(r#"{"op":"subscribe","args":["orderbook.200.BTCUSDT"]}"#)
        );
        assert_eq!(keepalive, Some(KEEPALIVE));
        assert_eq!(resubscribe, book_resubscribe("orderbook.200.BTCUSDT"));
        assert_eq!(levels, 200);
        assert_eq!(timings.idle_threshold(), Duration::from_secs(30));
        assert_eq!(timings.freshness_threshold(), Duration::from_secs(120));
        assert_eq!(timings.reseed_interval(), Duration::from_secs(300));
        assert_eq!(timings.backoff(), Duration::from_secs(3));
        let poll = vike_bridge_core::websocket_session::spec::row("bybit").knobs().read_timeout;
        assert_eq!(poll, Duration::from_secs(2));
    }
}

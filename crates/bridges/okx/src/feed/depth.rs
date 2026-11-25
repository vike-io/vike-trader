//! The books-400 DOM lane: the `books` fold and the lane's plan on the shared depth driver.

use std::sync::Arc;

use vike_bridge_core::data_client::ws::DataClientLanePlan;
use vike_bridge_core::depth::{WebsocketSessionDepthOperation, infer_tick_size};
use vike_venues::venue_timings::OKX;

use crate::feed::tick::{BooksFrame, KEEPALIVE, parse_books_frame};

// --- L2 depth feed (DOM) --------------------------------------------------------------------
// OKX `books` channel (400-level): a WS `snapshot` then `update`s — no REST seed. Reuses
// `crate::feed::tick::parse_books_frame` for the PROTOCOL + the shared `vike_bridge_core::depth`
// driver for the LIFECYCLE (through the data-client engine's `Depth` plan). Gap detection via
// `prevSeqId` (a break is `WebsocketSessionDepthOperation::Gap`, which self-heals AND discloses `GapStart`); the CRC32
// `checksum` is not read (see the re-seed below). Delivers the top `DEPTH_LEVELS` via
// `DataClientSink::l2_snapshot`.
//
// The four depth timings are this venue's `depth` block in `crates/vike-venues/src/venue_timings.rs`
// (`OKX`), which carries each value and its reason (decision 0160): the 30 s dead-transport watchdog
// (well above the 15 s keepalive cadence, so a quiet book kept alive by ping/pong never false-trips), the
// 120 s freshness limit (measured 2026-07-11: the thinnest actively-traded pairs gapped 44–47 s), the 3 s
// backoff, and the 300 s timed re-seed. That re-seed is the ONLY backstop for a dropped or mis-applied
// `update` that does not break the `prevSeqId` chain: OKX deprecated the CRC32 `checksum` on 2026-06-23
// (fixed to `0`; a public-WS census on 2026-10-10 saw `0` on every one of 869 `books` frames), so every
// re-seed interval the driver re-subscribes `books` ON THE SAME SOCKET ([`books_resubscribe`]) and the
// fresh WS `snapshot` rebuilds the book, with no reconnect and no transport gap. The driver's stop-flag
// poll is the venue's pump row's read timeout.

/// Levels per side published to the DOM (OKX `books` carries 400).
const DEPTH_LEVELS: usize = 400;

/// Fold ONE OKX `books` frame into the live [`vike_marketdata::L2Book`]. Pure over (`book`) — testable
/// without a socket, the depth twin of [`crate::feed::tick::route_frame`]. A `snapshot` rebuilds the
/// book; an `update` merges the delta, after the `prevSeqId` chain check: a break is [`WebsocketSessionDepthOperation::Gap`], so
/// the shared depth driver discards the book, reconnects + re-seeds, and discloses the untrustworthy
/// book via `GapStart`. The frame is parsed ONCE ([`parse_books_frame`]). The frame's `checksum` is
/// never read: OKX fixed it to `0` on 2026-06-23 (the module's timings note has the evidence).
pub(crate) fn okx_depth_decode(
    txt: &str,
    book: &mut Option<Arc<vike_marketdata::L2Book>>,
) -> WebsocketSessionDepthOperation {
    match parse_books_frame(txt) {
        BooksFrame::Snapshot { seq, timestamp_ms: ts_ms, bids, asks } => {
            let mut bk = vike_marketdata::L2Book::new(infer_tick_size(&bids, &asks));
            bk.apply_snapshot(seq, &bids, &asks);
            *book = Some(Arc::new(bk));
            WebsocketSessionDepthOperation::Updated(ts_ms) // §B: venue event-time from the frame's `ts`
        }
        BooksFrame::Update { seq, previous_sequence, timestamp_ms: ts_ms, bids, asks } => {
            match book.as_mut().map(Arc::make_mut) {
                Some(bk) => {
                    if previous_sequence != 0 && previous_sequence != bk.last_sequence {
                        WebsocketSessionDepthOperation::Gap // sequence chain broken — resync
                    } else if bk.apply_delta(seq, &bids, &asks) {
                        WebsocketSessionDepthOperation::Updated(ts_ms)
                    } else {
                        WebsocketSessionDepthOperation::Ignored // stale (seqId <= last)
                    }
                }
                None => WebsocketSessionDepthOperation::Ignored, // an update before the snapshot
            }
        }
        BooksFrame::Other => WebsocketSessionDepthOperation::Ignored, // ack / pong / non-books frame
    }
}

/// The `books` channel's unsubscribe and subscribe frames for `instrument`: the subscribe is the session's
/// own, and the pair, sent on an open socket, is the timed re-seed (okx answers the subscribe with a
/// fresh `action:"snapshot"`, as at the session's start).
fn books_resubscribe(instrument: &str) -> [String; 2] {
    ["unsubscribe", "subscribe"].map(|op| {
        format!(r#"{{"op":"{op}","args":[{{"channel":"books","instId":"{instrument}"}}]}}"#)
    })
}

/// The DOM lane (`subscribe_depth`): its plan on the shared depth driver. OKX's protocol is WS-seeded:
/// the `action:"snapshot"` frame builds the book (tick inferred from it); `action:"update"` frames fold as
/// deltas, with a `prevSeqId` chain check that surfaces a break as [`WebsocketSessionDepthOperation::Gap`] → the driver reconnects
/// + re-seeds. `public_websocket` is the public socket.
pub(super) fn depth_main(instrument: &str, public_websocket: &str) -> DataClientLanePlan {
    let [unsubscribe, sub] = books_resubscribe(instrument);
    DataClientLanePlan::Depth {
        url: public_websocket.to_string(),
        subscribe: Some(sub.clone()),
        keepalive: Some(KEEPALIVE),
        resubscribe: vec![unsubscribe, sub],
        seed: Box::new(|| None),
        decode: Box::new(okx_depth_decode),
        levels: DEPTH_LEVELS,
        timings: OKX.depth(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// The EFFECTIVE depth-driver timings, each a literal this file held before the venue timings
    /// table existed (decision 0160): evidence beside the table's own matrix pin, so a row edit that
    /// retunes okx's DOM watchdog fails here too and is a deliberate act in both places. The stop-flag
    /// poll is the pump row's read timeout (the engine hands it to the depth driver).
    #[test]
    fn the_depth_driver_runs_on_the_timings_it_always_did() {
        let DataClientLanePlan::Depth { timings, levels, .. } = depth_main("BTC-USDT", "wss://x")
        else {
            panic!("the DOM lane is a depth plan")
        };
        assert_eq!(timings.idle_threshold(), Duration::from_secs(30));
        assert_eq!(timings.freshness_threshold(), Duration::from_secs(120));
        assert_eq!(timings.reseed_interval(), Duration::from_secs(300));
        assert_eq!(timings.backoff(), Duration::from_secs(3));
        assert_eq!(levels, 400);
        let poll = vike_bridge_core::websocket_session::spec::row("okx").read_timeout;
        assert_eq!(poll, Duration::from_secs(2));
    }
}

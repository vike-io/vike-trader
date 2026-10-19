//! The books-400 DOM lane: the `books` fold, publish step and depth feed-thread body.

use std::time::Duration;

use vike_bridge_core::depth::{BookOp, infer_tick_size, run_depth_feed};
use vike_bridge_core::pump_spec::READ_POLL;
use vike_data::StreamLabel;
use vike_model::{StreamStatus, now_ms};
use vike_venues::venue_timings::OKX;

use super::{FeedCtx, VENUE};
use crate::market_data::{BooksFrame, PUBLIC_WS, parse_books_frame};

// --- L2 depth feed (DOM) --------------------------------------------------------------------
// OKX `books` channel (400-level): a WS `snapshot` then `update`s — no REST seed. Reuses
// `crate::market_data::parse_books_frame` for the PROTOCOL + the shared `vike_bridge_core::depth`
// driver for the LIFECYCLE. Gap detection via `prevSeqId` (a break is `BookOp::Gap`, which self-heals
// AND discloses `GapStart`); the CRC32 `checksum` is not read (see `DEPTH_RESEED_INTERVAL`). Delivers
// the top `DEPTH_LEVELS` via `LiveDataSink::l2_snapshot`.
//
// The four `DEPTH_*` timings below are this venue's `depth` block in
// `crates/vike-venues/src/venue_timings.rs` (`OKX`), which carries each value and its
// reason (decision 0160); the driver's stop-flag poll is `READ_POLL`, the venue-free mechanism
// default.

/// Levels per side published to the DOM (OKX `books` carries 400).
const DEPTH_LEVELS: usize = 400;
/// Reconnect backoff after a socket error / gap.
const DEPTH_BACKOFF: Duration = OKX.depth().backoff();
/// OKX drops a client that doesn't ping within ~30 s — the driver sends this raw text every ~15 s.
const KEEPALIVE: &str = "ping";
/// Net-hardening §B dead-transport watchdog: no `books` frame of ANY kind (a snapshot/update OR the
/// raw `pong` reply to our keepalive) for this long means the socket is silently dead → the driver
/// discloses a gap and reconnects+re-seeds. Well above the 15 s keepalive cadence, so a quiet book
/// kept alive purely by ping/pong never false-trips; only true transport silence does.
const DEPTH_IDLE_THRESHOLD: Duration = OKX.depth().idle_threshold();
/// §B data-freshness threshold for the depth book: how long the book may go without an applied
/// update, behind a still-live transport, before disclosing `Stale` (disclose-only, no auto-action;
/// the fast dead-socket signal is [`DEPTH_IDLE_THRESHOLD`], 30s). **Validated by live measurement
/// (2026-07-11) across the binance/bybit/okx keyless depth feeds: liquid pairs (BTC/ETH) update
/// ~0.1s, mid-caps ~3.6s max, and the market's THINNEST actively-traded pairs gapped 44–47s (a
/// near-dead pair updated only twice in 5 min).** 120s clears the thin-pair worst case with ~2.5×
/// margin, so only a genuinely near-dead book trips — where the `Stale` disclosure is accurate. Kept
/// at 120s (measured floor, not a placeholder); a uniform per-venue value must cover the thinnest
/// subscribable symbol (#111).
const DEPTH_FRESHNESS_THRESHOLD: Duration = OKX.depth().freshness_threshold();
/// Net-hardening timed book re-seed: forced fresh-snapshot cadence. A dropped or mis-applied `update`
/// that does NOT break the `prevSeqId` chain is the silent-corruption class no `BookOp::Gap`/idle/
/// freshness trip sees, and OKX's CRC32 `checksum` used to catch it. OKX deprecated that field on
/// 2026-06-23 (fixed to `0`): a public-WS census on 2026-10-10 saw `0` on every one of 869 `books`
/// frames (snapshots and updates, BTC-USDT-SWAP, ETH-USDT-SWAP, BTC-USDT, ETH-USDT), so the
/// validator and its raw-string book mirror were deleted and this timed re-seed is the ONLY backstop for
/// silent corruption: ending the session every `DEPTH_RESEED_INTERVAL` forces reconnect + re-subscribe
/// (a fresh WS `snapshot` rebuilds the book), bounding the corruption window to <= this — reusing the
/// gap handler's reconnect path but disclosing NO transport gap (a scheduled refresh, not an outage).
/// 5 min is a conservative default, well above the reconnect cost; `None` would disable it.
const DEPTH_RESEED_INTERVAL: Duration = OKX.depth().reseed_interval();

/// Publish the top [`DEPTH_LEVELS`] to the DOM sink (carrying the book's tick) + nudge a repaint.
fn publish_book(book: &vike_marketdata::L2Book, inst: &str, ctx: &FeedCtx) {
    let (bids, asks) = book.top_n(DEPTH_LEVELS);
    ctx.sink.l2_snapshot(VENUE, inst, book.tick_size, bids, asks, now_ms());
    (ctx.wake)();
}

/// Fold ONE OKX `books` frame into the live [`vike_marketdata::L2Book`]. Pure over (`book`) — testable
/// without a socket, the depth twin of [`crate::market_data::route_frame`]. A `snapshot` rebuilds the
/// book; an `update` merges the delta, after the `prevSeqId` chain check: a break is [`BookOp::Gap`], so
/// the shared depth driver discards the book, reconnects + re-seeds, and discloses the untrustworthy
/// book via `GapStart`. The frame is parsed ONCE ([`parse_books_frame`]). The frame's `checksum` is
/// never read: OKX fixed it to `0` on 2026-06-23 (see [`DEPTH_RESEED_INTERVAL`] for the evidence).
pub(crate) fn okx_depth_decode(txt: &str, book: &mut Option<vike_marketdata::L2Book>) -> BookOp {
    match parse_books_frame(txt) {
        BooksFrame::Snapshot { seq, ts_ms, bids, asks } => {
            let mut bk = vike_marketdata::L2Book::new(infer_tick_size(&bids, &asks));
            bk.apply_snapshot(seq, &bids, &asks);
            *book = Some(bk);
            BookOp::Updated(ts_ms) // §B: venue event-time from the frame's `ts`
        }
        BooksFrame::Update { seq, prev_seq, ts_ms, bids, asks } => match book.as_mut() {
            Some(bk) => {
                if prev_seq != 0 && prev_seq != bk.last_seq {
                    BookOp::Gap // sequence chain broken — resync
                } else if bk.apply_delta(seq, &bids, &asks) {
                    BookOp::Updated(ts_ms)
                } else {
                    BookOp::Ignored // stale (seqId <= last)
                }
            }
            None => BookOp::Ignored, // an update before the snapshot
        },
        BooksFrame::Other => BookOp::Ignored, // ack / pong / non-books frame
    }
}

/// Depth feed body on the shared driver. OKX's protocol is WS-seeded: the `action:"snapshot"`
/// frame builds the book (tick inferred from it); `action:"update"` frames fold as deltas, with a
/// `prevSeqId` chain check that surfaces a break as [`BookOp::Gap`] → the driver reconnects + re-seeds.
pub(super) fn depth_main(inst: String, _interval: String, ctx: FeedCtx) {
    let sub = format!(r#"{{"op":"subscribe","args":[{{"channel":"books","instId":"{inst}"}}]}}"#);
    let sym = inst.as_str();
    let seed = || None::<vike_marketdata::L2Book>;
    let publish = |b: &vike_marketdata::L2Book| publish_book(b, sym, &ctx);
    // Net-hardening §B: hand each `StreamStatus` the driver emits to the sink's machine-readable
    // disclosure. The depth driver owns the `StreamHealth` that dedups transport gaps across
    // reconnects (one outage → one GapStart/Live pair) now; the venue just discloses.
    let on_health = |ev: StreamStatus| {
        ctx.sink.stream_status(VENUE, sym, StreamLabel::Depth.as_str(), ev);
    };
    run_depth_feed(
        PUBLIC_WS,
        Some(&sub),
        Some(KEEPALIVE),
        seed,
        okx_depth_decode,
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
    /// retunes okx's DOM watchdog fails here too and is a deliberate act in both places.
    #[test]
    fn the_depth_driver_runs_on_the_timings_it_always_did() {
        assert_eq!(DEPTH_IDLE_THRESHOLD, Duration::from_secs(30));
        assert_eq!(DEPTH_FRESHNESS_THRESHOLD, Duration::from_secs(120));
        assert_eq!(DEPTH_RESEED_INTERVAL, Duration::from_secs(300));
        assert_eq!(DEPTH_BACKOFF, Duration::from_secs(3));
        assert_eq!(READ_POLL, Duration::from_secs(2));
    }
}

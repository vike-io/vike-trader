//! T4 and T5: the tape gap with its seq backstop, and the attach path (status first, book only when live, no tape replay).

use super::*;
use vike_data::StreamStatus;
use vike_datahub::md::MD_TAPE_CAP;
use vike_datahub::md::hub::MdKey;
use vike_model::TradeTick;

// ------------------------------------------------------------------------------------------------
// T4 — the tape gap, and the §7.2 backstop
// ------------------------------------------------------------------------------------------------

/// **A hub-side tape overflow emits `TapeGap` BEFORE the next `Trades` frame**, and the seq
/// arithmetic is checked against an INDEPENDENT witness.
///
/// ⚠ Three non-vacuity floors, and the third is the one a careless implementation defeats:
///
/// 1. `dropped > 0` — a tape that never overflowed produces no gap and every ordering assertion
///    above it is vacuous.
/// 2. the batch is exactly `MD_TAPE_CAP` — a tape that silently grew UNBOUNDED also produces no gap,
///    reads green on absence-style assertions, and is the memory bug §12.6's budget depends on not
///    having.
/// 3. `dropped` is checked against the count the TEST emitted, not only against a range the
///    implementation reported. `to_seq - from_seq == dropped` alone is TAUTOLOGICAL if an
///    implementation computes `to_seq` as `from_seq + dropped`; two paths to the same number is what
///    makes neither one circular.
///
/// The mutation this catches and nothing else does: assign `seq` AFTER the drop decision instead of
/// before. Every disclosed number stays self-consistent and every other test in this file still
/// passes.
#[test]
fn a_tape_overflow_emits_its_gap_before_the_next_batch() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Trades);
    let key = MdKey::of(&s);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    h.reconcile(now());

    const EXTRA: usize = 50;
    let sink = h.sink();
    for i in 0..(MD_TAPE_CAP + EXTRA) {
        sink.trade(
            "binance",
            "BTCUSDT.P",
            TradeTick {
                ts: i as i64,
                local_ts: 0,
                price: 1.0,
                size: 1.0,
                is_buyer_maker: false,
                symbol: "BTCUSDT.P".into(),
            },
        );
    }
    let tick = h.publish_tick();
    assert_eq!(tick.frames_produced, 1, "one key, one tick, one frame: {tick:?}");

    let frames = drain(g.mailbox());
    assert_eq!(frames.len(), 2, "the gap and the batch, in that order: {frames:?}");
    match &frames[0] {
        MdFrame::TapeGap { dropped, from_seq, to_seq, venue, symbol } => {
            assert_eq!(venue, &key.venue);
            assert_eq!(symbol, &key.symbol);
            assert!(*dropped > 0, "floor 1: the tape must actually have overflowed");
            assert_eq!(*dropped, EXTRA as u64, "floor 3: the count the TEST emitted");
            assert!(to_seq >= from_seq, "{from_seq}..{to_seq}");
        }
        other => panic!("the gap must come FIRST, before the batch it precedes: {other:?}"),
    }
    match &frames[1] {
        MdFrame::Trades { ticks, .. } => {
            assert_eq!(ticks.len(), MD_TAPE_CAP, "floor 2: the tape is BOUNDED, not merely lossy");
            // §12.4's second finding, held as a property: the hub tape holds a SYMBOL-LESS tick,
            // because a 78-character polymarket token id turns 64 B into ~142 B and 64 keys into
            // 37 MB, breaking the memory budget on its own.
            assert!(
                ticks.iter().all(|t| t.symbol.is_empty()),
                "the hub tape must drop the per-tick symbol — the envelope carries it once"
            );
        }
        other => panic!("expected the batch after the gap: {other:?}"),
    }
    g.release_at(now());
}

/// The wire `seq` is CONTIGUOUS across a key's delivered frames when nothing dropped — the other
/// half of §7.2, and the baseline a jump is judged against.
#[test]
fn the_wire_seq_is_contiguous_with_no_drops() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    h.reconcile(now());

    let sink = h.sink();
    let mut seqs = Vec::new();
    for i in 0..5 {
        sink.l2_snapshot(
            "binance",
            "BTCUSDT.P",
            0.1,
            vec![BookLevel::new(1.0, 1.0)],
            vec![BookLevel::new(2.0, 1.0)],
            i,
        );
        h.publish_tick();
        for f in drain(g.mailbox()) {
            if let MdFrame::Depth(b) = f {
                seqs.push(b.seq);
            }
        }
    }
    assert_eq!(seqs, vec![1, 2, 3, 4, 5], "strictly +1, assigned by the publisher: {seqs:?}");
    g.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// T5 — the attach path
// ------------------------------------------------------------------------------------------------

/// **A STALE key sends its `Status` and NO snapshot on attach** (§5.2 step 5).
///
/// The consequence of getting this wrong is precise, and it is the worst outcome a market-data
/// display has: §7.4 makes the CLIENT stamp a LOCAL receipt on arrival, so a two-minute-old book
/// written into `BookStore` reads LIVE for a whole `DOM_STALE_MS` window — a populated, fresh-looking
/// ladder for a market that stopped ticking two minutes ago, indistinguishable from a quiet market.
///
/// ⚠ This test asserts an ABSENCE, so leg (b) — the SAME key, the SAME harness, status `Live`, the
/// snapshot DOES arrive — is what stops it certifying an outage. Without (b), (a) is worthless.
#[test]
fn attach_sends_status_first_and_the_book_only_when_live() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let key = MdKey::of(&s);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    h.reconcile(now());
    let sink = h.sink();
    sink.l2_snapshot(
        "binance",
        "BTCUSDT.P",
        0.1,
        vec![BookLevel::new(1.0, 1.0)],
        vec![BookLevel::new(2.0, 1.0)],
        5,
    );

    // (a) GapStart — status only, though the hub's slot HOLDS a book.
    sink.stream_status("binance", "BTCUSDT.P", "depth", StreamStatus::GapStart { at_ts_ms: 1 });
    let f = h.attach_frames(&key, now());
    assert_eq!(f.len(), 1, "a gapped key attaches with its STATUS and no book: {f:?}");
    assert!(matches!(f[0], MdFrame::Status { .. }), "...and the status IS there: {f:?}");

    // (c) STALE — the variant a hub that special-cases only GapStart would leak a book on. That is
    // the silently-failed-resubscribe case `DEPTH_FRESHNESS_THRESHOLD` exists to catch, and the
    // exact failure §12.5 found running unnoticed for forty days.
    sink.stream_status(
        "binance",
        "BTCUSDT.P",
        "depth",
        StreamStatus::Stale { newest_data_ts_ms: 1, now_ms: 2 },
    );
    let f = h.attach_frames(&key, now());
    assert_eq!(f.len(), 1, "a STALE key attaches with its status and no book: {f:?}");

    // (b) LIVE — the same key, the same harness: the snapshot DOES arrive, and AFTER the status.
    sink.stream_status(
        "binance",
        "BTCUSDT.P",
        "depth",
        StreamStatus::Live { gap_started_ts_ms: None },
    );
    let f = h.attach_frames(&key, now());
    assert_eq!(f.len(), 2, "live: status THEN book: {f:?}");
    assert!(matches!(f[0], MdFrame::Status { .. }), "the status is FIRST: {f:?}");
    assert!(matches!(f[1], MdFrame::Depth(_)), "...and the book follows it: {f:?}");
    g.release_at(now());
}

/// (d) LIVE but with an EMPTY slot: exactly one frame — the status. The client then shows "waiting
/// for first book" instead of an empty ladder that reads as a real, thin market.
#[test]
fn a_live_key_with_no_book_yet_attaches_with_its_status_alone() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    h.sink().stream_status(
        "binance",
        "BTCUSDT.P",
        "depth",
        StreamStatus::Live { gap_started_ts_ms: None },
    );
    let f = h.attach_frames(&MdKey::of(&s), now());
    assert_eq!(f.len(), 1, "{f:?}");
    g.release_at(now());
}

/// **T8 — a reconnecting subscriber is NEVER handed the tape it missed** (§7.3 rule 4).
///
/// The server keeps no replay buffer, so a reconnect must be a HOLE, never a duplicate: handing a
/// new subscriber prints from before it existed, with no gap marker, double-counts every reconnect
/// into `crates/vike-orderflow/src/bar_agg.rs`'s `OrderflowAgg`, which has no per-trade dedup.
/// §5.2 step 5 specifies status-then-BOOK for attach and says nothing about the tape; this is that
/// silence, closed.
///
/// ⚠ Paired with the POSITIVE: the key's live tape DOES flow to the new subscriber from the next
/// tick onward. Otherwise "received no ticks" passes against a broken attach.
#[test]
fn a_new_subscriber_is_never_handed_the_tape_it_missed() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Trades);
    let key = MdKey::of(&s);
    // A resident key, so the tape accumulates with no session attached at all.
    h.add_resident(&s).expect("a served venue on a supported lane");
    h.reconcile(now());
    let sink = h.sink();
    for i in 0..10 {
        sink.trade(
            "binance",
            "BTCUSDT.P",
            TradeTick {
                ts: i,
                local_ts: 0,
                price: 1.0,
                size: 1.0,
                is_buyer_maker: false,
                symbol: String::new(),
            },
        );
    }
    // ⚠ A tick with NO subscriber CONSUMES the tape. That is the structural half of the property:
    // the server keeps no replay buffer because it never accumulates one, so there is nothing for a
    // later attach to leak. `frames_produced` is zero — the work happened, nothing was sent.
    let idle = h.publish_tick();
    assert_eq!(idle.keys_walked, 1, "the dirty key was walked: {idle:?}");
    assert_eq!(idle.frames_produced, 0, "...and nothing was sent to nobody: {idle:?}");

    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    let attached = h.attach_frames(&key, now());
    assert!(
        attached.iter().all(|f| !matches!(f, MdFrame::Trades { .. })),
        "the accumulated tape must NOT be replayed to a new subscriber: {attached:?}"
    );

    // ...and the POSITIVE: from the next tick, live prints DO flow.
    sink.trade(
        "binance",
        "BTCUSDT.P",
        TradeTick {
            ts: 99,
            local_ts: 0,
            price: 1.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        },
    );
    h.publish_tick();
    let frames = drain(g.mailbox());
    assert!(
        frames.iter().any(|f| matches!(f, MdFrame::Trades { .. })),
        "live prints reach the new subscriber from the next tick: {frames:?}"
    );
    // ...and exactly the ONE live print, with NO gap marker: the ten it never saw are not a hole in
    // ITS tape, they are prints from before it existed.
    match frames.iter().find(|f| matches!(f, MdFrame::Trades { .. })).unwrap() {
        MdFrame::Trades { ticks, .. } => assert_eq!(ticks.len(), 1, "{ticks:?}"),
        _ => unreachable!(),
    }
    assert!(
        frames.iter().all(|f| !matches!(f, MdFrame::TapeGap { .. })),
        "a subscriber is not owed a gap for prints that predate it: {frames:?}"
    );
    g.release_at(now());
}

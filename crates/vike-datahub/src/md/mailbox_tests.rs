use super::*;
use std::assert_matches;
use vike_datahub_client::market::MdLane;

fn key(sym: &str, lane: MdLane) -> MdKey {
    MdKey { venue: "binance".into(), symbol: sym.into(), lane }
}

fn frame(n: usize) -> Arc<Vec<u8>> {
    Arc::new(vec![0u8; n])
}

fn book(k: &MdKey, seq: u64, n: usize) -> Outgoing {
    Outgoing {
        key: k.clone(),
        lane: LaneClass::Book,
        bytes: frame(n),
        seq,
        ticks: 0,
        hub_dropped: 0,
    }
}

fn tape(k: &MdKey, seq: u64, ticks: u64, n: usize) -> Outgoing {
    Outgoing { key: k.clone(), lane: LaneClass::Tape, bytes: frame(n), seq, ticks, hub_dropped: 0 }
}

fn ctrl(k: &MdKey, n: usize) -> Outgoing {
    Outgoing {
        key: k.clone(),
        lane: LaneClass::Ctrl,
        bytes: frame(n),
        seq: 0,
        ticks: 0,
        hub_dropped: 0,
    }
}

/// **T3a — book frames SUPERSEDE PER KEY.**
///
/// Non-vacuity has two floors, and both matter. First, the mailbox must actually REACH its cap
/// before "it holds the newest" means anything — a mailbox that enqueued nothing satisfies
/// `len() <= cap` and "holds the newest" for the wrong reason, because a `None` is not a stale
/// frame. Second, TWO keys: with one key, "the mailbox holds one entry" is indistinguishable
/// from "one entry PER KEY", and per-key supersede is the actual claim.
///
/// The mutation this catches is the likeliest single error in the whole design: copying
/// `crates/vike-tradehub/src/publish.rs`'s `Mailbox` verbatim, whose rule is drop-oldest. Under
/// that rule a laggard is served a book from `cap` ticks ago and then jumps.
#[test]
fn book_frames_supersede_per_key_and_never_drop_oldest() {
    let mb = Mailbox::with_bounds(8, 1 << 20);
    let a = key("AAA", MdLane::Depth);
    let b = key("BBB", MdLane::Depth);
    // Fill past the cap on two keys.
    for seq in 1..=20u64 {
        let out = book(&a, seq, 16);
        let r = mb.push(Outgoing { bytes: Arc::new(seq.to_be_bytes().to_vec()), ..out });
        assert_matches!(r, PushOutcome::Enqueued | PushOutcome::Superseded, "{r:?}");
        mb.push(book(&b, seq, 16));
    }
    assert_eq!(mb.len(), 2, "exactly ONE entry per key, however far behind: {}", mb.len());
    // ...and the surviving frame for `a` is the NEWEST, not one from 20 ticks ago.
    let mut got: Vec<Vec<u8>> = Vec::new();
    while let Recv::Frame { bytes, .. } = mb.recv_timeout(Duration::from_millis(1)) {
        got.push((*bytes).clone());
    }
    assert!(got.contains(&20u64.to_be_bytes().to_vec()), "the NEWEST book must survive: {got:?}");
    assert!(!got.contains(&1u64.to_be_bytes().to_vec()), "a stale book must NOT: {got:?}");
}

/// **T3b — tape frames DISCARD and OWE**, and the owed range MERGES a hub-side eviction with a
/// mailbox-side drop into ONE disclosure that cannot double-count.
///
/// ⚠ **The BACKLOG is the point, and this test pinned the WRONG contract without it.** It used
/// to assert the gap rode the FIRST frame off the queue — which, with seqs 1..3 queued and 4
/// dropped, writes the marker AHEAD of three batches that chronologically PRECEDE the hole, and
/// makes a client reset its aggregator and then re-ingest older prints. The marker belongs on
/// the first POST-hole frame, and `from_seq`/`to_seq` must then read as
/// `vike_datahub_client::market::MdFrame::TapeGap` declares them: the last frame DELIVERED
/// before the hole, and the first one delivered after it.
#[test]
fn tape_frames_discard_and_owe_a_merged_gap() {
    let mb = Mailbox::with_bounds(3, 1 << 20);
    let k = key("AAA", MdLane::Trades);
    // One accepted frame that ALSO discloses a hub-side eviction of 7 prints. That hole is
    // BEFORE seq 1, so seq 1 is itself post-hole and carries the disclosure.
    assert_eq!(mb.push(Outgoing { hub_dropped: 7, ..tape(&k, 1, 10, 16) }), PushOutcome::Enqueued);
    // Fill and then overflow: the overflowing batch is DISCARDED (not superseded — a superseded
    // print is a LOST print) and its prints join the owed count.
    assert_eq!(mb.push(tape(&k, 2, 10, 16)), PushOutcome::Enqueued);
    assert_eq!(mb.push(tape(&k, 3, 10, 16)), PushOutcome::Enqueued);
    assert_eq!(mb.push(tape(&k, 4, 5, 16)), PushOutcome::Dropped);

    // TWO holes now: one BEFORE seq 1 (the hub-side eviction) and one AT seq 4. They merge into
    // one disclosure, and it must be delivered at the EARLIER of the two — ahead of seq 1 —
    // because delivering it at the later one would hand the client `Trades(1..3)` to fold while
    // seven prints were already missing from in front of them.
    let mut seen: Vec<Option<(u64, u64, u64)>> = Vec::new();
    while let Recv::Frame { owed_gap, .. } = mb.recv_timeout(Duration::from_millis(1)) {
        seen.push(owed_gap.map(|(gk, d, f, t)| {
            assert_eq!(gk, k);
            (d, f, t)
        }));
    }
    assert_eq!(seen.len(), 3, "the three queued batches, in order: {seen:?}");
    assert_eq!(
        seen[0],
        Some((12, 0, 1)),
        "7 hub-side + 5 mailbox-side, MERGED, never double-counted — and disclosed on the \
             EARLIEST frame either hole precedes, never after a batch the client would have folded"
    );
    assert_eq!(seen[1], None, "a gap is disclosed once, not on every subsequent frame");
    assert_eq!(seen[2], None, "...and once means once");

    // ...and the frame on the far side of the seq-4 hole carries nothing further: the count
    // above already covers it, and the §7.2 seq jump (3 -> 5) is the backstop for the position.
    assert_eq!(mb.push(tape(&k, 5, 10, 16)), PushOutcome::Enqueued);
    let Recv::Frame { owed_gap, .. } = mb.recv_timeout(Duration::from_millis(1)) else {
        panic!("expected the post-hole frame")
    };
    assert!(owed_gap.is_none(), "the merged gap was already disclosed, once: {owed_gap:?}");
}

/// **`enforce_bounds` evicts the OLDEST, and its disclosure must still span FORWARDS.**
///
/// This is the one eviction path no test reached: the byte bound is pre-checked on the tape arm
/// of `push`, so `enforce_bounds` only ever fires when a CTRL or BOOK push carries the mailbox
/// over — and both of the existing byte-bound tests push one lane only. Deriving `from_seq` from
/// the last seq ENQUEUED put it ABOVE `to_seq` here and wrote an INVERTED range on the wire.
#[test]
fn an_oldest_first_eviction_discloses_a_forward_range() {
    let k = key("AAA", MdLane::Trades);
    let b = key("BBB", MdLane::Depth);
    let mb = Mailbox::with_bounds(64, 4096);
    // Deliver seq 10 so there IS a last-delivered baseline to be wrong about.
    assert_eq!(mb.push(tape(&k, 10, 1, 256)), PushOutcome::Enqueued);
    let Recv::Frame { owed_gap, .. } = mb.recv_timeout(Duration::from_millis(1)) else {
        panic!("expected the seeded frame")
    };
    assert!(owed_gap.is_none(), "nothing has been lost yet");
    for seq in 11..=14u64 {
        assert_eq!(mb.push(tape(&k, seq, 1, 256)), PushOutcome::Enqueued);
    }
    // A BOOK push takes the mailbox over the byte bound, so `enforce_bounds` evicts from the
    // FRONT of the tape — the path `push`'s own byte pre-check can never reach.
    mb.push(book(&b, 1, 3200));
    assert!(mb.bytes() <= 4096, "the byte bound holds: {}", mb.bytes());

    let mut gaps = Vec::new();
    while let Recv::Frame { owed_gap, .. } = mb.recv_timeout(Duration::from_millis(1)) {
        if let Some((gk, dropped, from_seq, to_seq)) = owed_gap {
            assert!(from_seq < to_seq, "an INVERTED range reached the wire: {from_seq}..{to_seq}");
            gaps.push((gk, dropped, from_seq, to_seq));
        }
    }
    assert_eq!(gaps.len(), 1, "one hole, one disclosure: {gaps:?}");
    let (gk, dropped, from_seq, to_seq) = gaps.remove(0);
    assert_eq!(gk, k);
    assert!(dropped > 0, "non-vacuity: something must actually have been evicted");
    assert_eq!(from_seq, 10, "the last seq this subscriber RECEIVED, not the last enqueued");
    assert_eq!(to_seq, 10 + 1 + dropped, "and the first one it receives after the hole");
}

/// **T3c — the CTRL lane is RESERVED and drains FIRST**, and when even it cannot be filled the
/// connection is CLOSED rather than a `Status` being evicted.
///
/// The failure this gates is the worst outcome the design has: a client that loses its
/// `GapStart` keeps rendering a frozen ladder it believes is live.
#[test]
fn a_status_frame_is_never_evicted_by_book_traffic_and_overflow_closes() {
    let mb = Mailbox::with_bounds(4, 1 << 20);
    let k = key("AAA", MdLane::Depth);
    for seq in 1..=50u64 {
        mb.push(book(&k, seq, 16));
    }
    assert_eq!(mb.push(ctrl(&k, 8)), PushOutcome::Enqueued, "the ctrl lane has its own room");
    // It comes out FIRST, ahead of the book backlog.
    let Recv::Frame { bytes, .. } = mb.recv_timeout(Duration::from_millis(1)) else {
        panic!("expected a frame")
    };
    assert_eq!(bytes.len(), 8, "the CTRL frame drains first");

    // ...and overflowing the ctrl lane CLOSES rather than evicting.
    let mb2 = Mailbox::with_bounds(1024, 1 << 20);
    for _ in 0..MD_MAILBOX_CTRL {
        assert_eq!(mb2.push(ctrl(&k, 8)), PushOutcome::Enqueued);
    }
    assert_eq!(mb2.push(ctrl(&k, 8)), PushOutcome::CloseConnection);
    assert!(mb2.must_close());
}

/// **T11 — the BYTE bound bites where the frame cap cannot**, and it degrades the DEEP
/// subscriber's own queue rather than the box's ceiling.
///
/// Non-vacuity: the shallow subscriber's frame count is compared against a same-test baseline,
/// so "the deep one dropped more" cannot be satisfied by an implementation that throttles both.
#[test]
fn the_byte_bound_degrades_the_deep_subscribers_own_queue() {
    let k = key("AAA", MdLane::Trades);
    // Both mailboxes have the SAME generous frame cap, so only bytes can separate them.
    let shallow = Mailbox::with_bounds(64, 4096);
    let deep = Mailbox::with_bounds(64, 4096);
    let mut shallow_dropped = 0;
    let mut deep_dropped = 0;
    for seq in 1..=8u64 {
        if mb_drop(&shallow, tape(&k, seq, 1, 256)) {
            shallow_dropped += 1;
        }
        if mb_drop(&deep, tape(&k, seq, 1, 2048)) {
            deep_dropped += 1;
        }
    }
    assert_eq!(shallow_dropped, 0, "the shallow subscriber is UNAFFECTED — the baseline");
    assert!(deep_dropped > 0, "the deep subscriber's own queue degrades: {deep_dropped}");
    assert!(deep.bytes() <= 4096, "the byte bound holds: {}", deep.bytes());
    // ...and the frame CAP alone would not have caught it: neither mailbox ever reached it.
    assert!(shallow.len() < 64 && deep.len() < 64);
}

fn mb_drop(mb: &Mailbox, out: Outgoing) -> bool {
    matches!(mb.push(out), PushOutcome::Dropped)
}

/// A closed mailbox discards silently and wakes its consumer — the lossy-observer contract.
#[test]
fn a_closed_mailbox_discards_and_wakes() {
    let mb = Mailbox::new();
    let k = key("AAA", MdLane::Depth);
    mb.close();
    assert_eq!(mb.push(book(&k, 1, 16)), PushOutcome::Closed);
    assert_matches!(mb.recv_timeout(Duration::from_millis(1)), Recv::Closed);
}

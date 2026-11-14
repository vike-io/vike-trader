//! The delta-apply law: each `L2BookSequencePolicy` verdict, checked against `apply_delta` and the chain.

use super::*;
use crate::L2BookLevel;

/// The Monotonic policy IS `apply_delta`'s accept rule: for every seq, the decision says
/// Apply exactly when `apply_delta` on a same-state book returns true — and Monotonic never
/// says Gap (jumps are normal on those streams).
#[test]
fn monotonic_decision_mirrors_apply_delta_exactly() {
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(10, &[L2BookLevel::new(100.0, 1.0)], &[L2BookLevel::new(101.0, 1.0)]);
    for seq in [0u64, 5, 9, 10, 11, 12, 1_000] {
        let decision = book.delta_decision(seq, L2BookSequencePolicy::Monotonic);
        let mut probe = book.clone();
        let applied = probe.apply_delta(seq, &[L2BookLevel::new(99.0, 1.0)], &[]);
        assert_eq!(
            decision == L2BookDeltaDecision::Apply,
            applied,
            "decision/apply_delta disagree at seq {seq}"
        );
        assert_ne!(
            decision,
            L2BookDeltaDecision::Gap,
            "Monotonic never declares a gap (seq {seq})"
        );
    }
    // the specific verdicts: 0 = "no seq" sentinel → Apply; <= last → Stale; > last → Apply
    assert_eq!(book.delta_decision(0, L2BookSequencePolicy::Monotonic), L2BookDeltaDecision::Apply);
    assert_eq!(
        book.delta_decision(10, L2BookSequencePolicy::Monotonic),
        L2BookDeltaDecision::Stale
    );
    assert_eq!(
        book.delta_decision(12, L2BookSequencePolicy::Monotonic),
        L2BookDeltaDecision::Apply
    );
}

/// The Strict policy: only `last_sequence + 1` applies; `== last_sequence` is a stale duplicate; a
/// forward jump (dropped frames) AND a regression (venue counter restart) are both Gap.
#[test]
fn strict_decision_contiguous_applies_jump_and_regression_are_gaps() {
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(10, &[L2BookLevel::new(100.0, 1.0)], &[L2BookLevel::new(101.0, 1.0)]);
    assert_eq!(book.delta_decision(11, L2BookSequencePolicy::Strict), L2BookDeltaDecision::Apply);
    assert_eq!(book.delta_decision(10, L2BookSequencePolicy::Strict), L2BookDeltaDecision::Stale);
    assert_eq!(
        book.delta_decision(12, L2BookSequencePolicy::Strict),
        L2BookDeltaDecision::Gap,
        "dropped frame"
    );
    assert_eq!(
        book.delta_decision(9, L2BookSequencePolicy::Strict),
        L2BookDeltaDecision::Gap,
        "regression"
    );
    assert_eq!(
        book.delta_decision(1, L2BookSequencePolicy::Strict),
        L2BookDeltaDecision::Gap,
        "venue restart"
    );
    // seq 0 on a strict stream is judged as a plain number: a broken chain, not a sentinel
    assert_eq!(book.delta_decision(0, L2BookSequencePolicy::Strict), L2BookDeltaDecision::Gap);
    // an unseeded book (last_sequence 0): 1 chains, 0 is the duplicate, anything else gaps
    let fresh = L2Book::new(0.5);
    assert_eq!(fresh.delta_decision(1, L2BookSequencePolicy::Strict), L2BookDeltaDecision::Apply);
    assert_eq!(fresh.delta_decision(0, L2BookSequencePolicy::Strict), L2BookDeltaDecision::Stale);
    assert_eq!(fresh.delta_decision(7, L2BookSequencePolicy::Strict), L2BookDeltaDecision::Gap);
}

/// The recorded-chain fold: a snapshot anchors (on the grid the caller names), an in-sequence delta
/// applies, and a gap, an orphan delta or a `GapStart` / `Stale` marker empties the slot until the next
/// snapshot; a `LiveResume` changes nothing. The same rule through an owned slot and an `Rc` one.
#[test]
fn a_recorded_chain_is_trusted_only_while_it_stays_contiguous() {
    let event = |kind, seq, asks: Vec<L2BookLevel>| L2BookUpdate {
        timestamp: 0,
        local_timestamp: 0,
        sequence: seq,
        kind,
        tick_size: 0.01,
        bids: vec![L2BookLevel::new(0.20, 1.0)],
        asks,
        symbol: String::new(),
    };
    let ask = |p| vec![L2BookLevel::new(p, 1.0)];
    let mut slot: Option<L2Book> = None;
    assert!(!apply_book_event(
        &mut slot,
        &event(L2BookUpdateKind::Delta, 1, ask(0.3)),
        0.01,
        |b| b
    ));
    assert!(slot.is_none(), "an orphan delta anchors nothing");
    assert!(apply_book_event(
        &mut slot,
        &event(L2BookUpdateKind::Snapshot, 4, ask(0.3)),
        0.0001,
        |b| b
    ));
    assert_eq!(slot.as_ref().map(|b| b.tick_size), Some(0.0001), "the caller's grid");
    assert!(apply_book_event(
        &mut slot,
        &event(L2BookUpdateKind::Delta, 5, ask(0.25)),
        0.0001,
        |b| b
    ));
    assert_eq!(slot.as_ref().and_then(L2Book::best_ask).map(|l| l.price), Some(0.25));
    assert!(!apply_book_event(
        &mut slot,
        &event(L2BookUpdateKind::LiveResume, 0, Vec::new()),
        0.0001,
        |b| b
    ));
    assert!(slot.is_some(), "a resume changes nothing");
    assert!(!apply_book_event(
        &mut slot,
        &event(L2BookUpdateKind::Delta, 7, ask(0.2)),
        0.0001,
        |b| b
    ));
    assert!(slot.is_none(), "a hole drops the book");
    for marker in [L2BookUpdateKind::GapStart, L2BookUpdateKind::Stale] {
        let mut rc: Option<std::rc::Rc<L2Book>> = None;
        assert!(apply_book_event(
            &mut rc,
            &event(L2BookUpdateKind::Snapshot, 1, ask(0.3)),
            0.01,
            std::rc::Rc::make_mut
        ));
        assert!(apply_book_event(
            &mut rc,
            &event(L2BookUpdateKind::Delta, 2, ask(0.4)),
            0.01,
            std::rc::Rc::make_mut
        ));
        assert!(!apply_book_event(
            &mut rc,
            &event(marker, 0, Vec::new()),
            0.01,
            std::rc::Rc::make_mut
        ));
        assert!(rc.is_none(), "{marker:?} drops the book");
    }
}

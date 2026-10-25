//! The delta-apply law: each `SeqPolicy` verdict, checked against `apply_delta` and the chain.

use super::*;
use crate::BookLevel;

/// The Monotonic policy IS `apply_delta`'s accept rule: for every seq, the decision says
/// Apply exactly when `apply_delta` on a same-state book returns true — and Monotonic never
/// says Gap (jumps are normal on those streams).
#[test]
fn monotonic_decision_mirrors_apply_delta_exactly() {
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(10, &[BookLevel::new(100.0, 1.0)], &[BookLevel::new(101.0, 1.0)]);
    for seq in [0u64, 5, 9, 10, 11, 12, 1_000] {
        let decision = book.delta_decision(seq, SeqPolicy::Monotonic);
        let mut probe = book.clone();
        let applied = probe.apply_delta(seq, &[BookLevel::new(99.0, 1.0)], &[]);
        assert_eq!(
            decision == DeltaDecision::Apply,
            applied,
            "decision/apply_delta disagree at seq {seq}"
        );
        assert_ne!(decision, DeltaDecision::Gap, "Monotonic never declares a gap (seq {seq})");
    }
    // the specific verdicts: 0 = "no seq" sentinel → Apply; <= last → Stale; > last → Apply
    assert_eq!(book.delta_decision(0, SeqPolicy::Monotonic), DeltaDecision::Apply);
    assert_eq!(book.delta_decision(10, SeqPolicy::Monotonic), DeltaDecision::Stale);
    assert_eq!(book.delta_decision(12, SeqPolicy::Monotonic), DeltaDecision::Apply);
}

/// The Strict policy: only `last_seq + 1` applies; `== last_seq` is a stale duplicate; a
/// forward jump (dropped frames) AND a regression (venue counter restart) are both Gap.
#[test]
fn strict_decision_contiguous_applies_jump_and_regression_are_gaps() {
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(10, &[BookLevel::new(100.0, 1.0)], &[BookLevel::new(101.0, 1.0)]);
    assert_eq!(book.delta_decision(11, SeqPolicy::Strict), DeltaDecision::Apply);
    assert_eq!(book.delta_decision(10, SeqPolicy::Strict), DeltaDecision::Stale);
    assert_eq!(book.delta_decision(12, SeqPolicy::Strict), DeltaDecision::Gap, "dropped frame");
    assert_eq!(book.delta_decision(9, SeqPolicy::Strict), DeltaDecision::Gap, "regression");
    assert_eq!(book.delta_decision(1, SeqPolicy::Strict), DeltaDecision::Gap, "venue restart");
    // seq 0 on a strict stream is judged as a plain number: a broken chain, not a sentinel
    assert_eq!(book.delta_decision(0, SeqPolicy::Strict), DeltaDecision::Gap);
    // an unseeded book (last_seq 0): 1 chains, 0 is the duplicate, anything else gaps
    let fresh = L2Book::new(0.5);
    assert_eq!(fresh.delta_decision(1, SeqPolicy::Strict), DeltaDecision::Apply);
    assert_eq!(fresh.delta_decision(0, SeqPolicy::Strict), DeltaDecision::Stale);
    assert_eq!(fresh.delta_decision(7, SeqPolicy::Strict), DeltaDecision::Gap);
}

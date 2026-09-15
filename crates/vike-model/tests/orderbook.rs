//! R8 L2 book gates: snapshot/delta folding, best-bid/ask ordering, mid/spread/
//! imbalance, zero-qty removal, stale-seq drop.

use vike_model::{DeltaDecision, L2Book, SeqPolicy};

#[test]
fn snapshot_then_deltas() {
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(
        10,
        &[(100.0, 3.0), (99.5, 5.0), (99.0, 2.0)],
        &[(100.5, 4.0), (101.0, 1.0), (101.5, 6.0)],
    );
    assert_eq!(book.best_bid(), Some((100.0, 3.0)));
    assert_eq!(book.best_ask(), Some((100.5, 4.0)));
    assert_eq!(book.mid(), Some(100.25));
    assert_eq!(book.spread(), Some(0.5));
    assert_eq!(book.bid_levels(), 3);
    assert_eq!(book.ask_levels(), 3);

    // top-of-book imbalance: (3 - 4) / (3 + 4)
    assert_eq!(book.imbalance(), Some((3.0 - 4.0) / (3.0 + 4.0)));

    // delta: improve the bid, remove a level (qty 0), add a crossing ask (on-grid)
    assert!(book.apply_delta(11, &[(100.5, 2.0), (99.0, 0.0)], &[(100.0, 1.5)]));
    assert_eq!(book.best_bid(), Some((100.5, 2.0)));
    assert_eq!(book.best_ask(), Some((100.0, 1.5))); // crossed book is allowed at L2
    assert_eq!(book.bid_levels(), 3); // added 100.5, removed 99.0 (net 0)
    assert_eq!(book.ask_levels(), 4);

    // stale seq dropped
    assert!(!book.apply_delta(11, &[(50.0, 9.0)], &[]));
    assert!(!book.apply_delta(5, &[(50.0, 9.0)], &[]));
    assert_eq!(book.best_bid(), Some((100.5, 2.0))); // unchanged

    // seq==0 (venue with no seq) always applies
    assert!(book.apply_delta(0, &[(102.0, 7.0)], &[]));
    assert_eq!(book.best_bid(), Some((102.0, 7.0)));
}

/// The Monotonic policy IS `apply_delta`'s accept rule: for every seq, the decision says
/// Apply exactly when `apply_delta` on a same-state book returns true — and Monotonic never
/// says Gap (jumps are normal on those streams).
#[test]
fn monotonic_decision_mirrors_apply_delta_exactly() {
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(10, &[(100.0, 1.0)], &[(101.0, 1.0)]);
    for seq in [0u64, 5, 9, 10, 11, 12, 1_000] {
        let decision = book.delta_decision(seq, SeqPolicy::Monotonic);
        let mut probe = book.clone();
        let applied = probe.apply_delta(seq, &[(99.0, 1.0)], &[]);
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
    book.apply_snapshot(10, &[(100.0, 1.0)], &[(101.0, 1.0)]);
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

#[test]
fn empty_book_reads_none() {
    let book = L2Book::new(0.01);
    assert_eq!(book.best_bid(), None);
    assert_eq!(book.best_ask(), None);
    assert_eq!(book.mid(), None);
    assert_eq!(book.spread(), None);
    assert_eq!(book.imbalance(), None);
}

#[test]
fn top_n_ordering() {
    let mut book = L2Book::new(1.0);
    book.apply_snapshot(
        1,
        &[(100.0, 1.0), (98.0, 2.0), (99.0, 3.0), (97.0, 4.0)],
        &[(103.0, 1.0), (101.0, 2.0), (102.0, 3.0)],
    );
    let (bids, asks) = book.top_n(2);
    assert_eq!(bids, vec![(100.0, 1.0), (99.0, 3.0)]); // high → low
    assert_eq!(asks, vec![(101.0, 2.0), (102.0, 3.0)]); // low → high
}

#[test]
fn snapshot_clears_prior() {
    let mut book = L2Book::new(1.0);
    book.apply_snapshot(1, &[(100.0, 5.0)], &[(101.0, 5.0)]);
    book.apply_snapshot(2, &[(200.0, 1.0)], &[(201.0, 1.0)]);
    assert_eq!(book.bid_levels(), 1);
    assert_eq!(book.best_bid(), Some((200.0, 1.0)));
}

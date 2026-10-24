use super::*;
use vike_marketdata::BookLevel;
use vike_marketdata::test_support::book;

// bids (99,10)(98,5), asks (101,10)(102,15): n=2 → (15−25)/40 = −0.25;
// n=1 → (10−10)/20 = 0 (matches L2Book::imbalance, the n=1 special case).
#[test]
fn depth_imbalance_expected_values() {
    let b = book(
        &[BookLevel::new(99.0, 10.0), BookLevel::new(98.0, 5.0)],
        &[BookLevel::new(101.0, 10.0), BookLevel::new(102.0, 15.0)],
    );
    assert_eq!(depth_imbalance(&b, 2), Some(-0.25));
    assert_eq!(depth_imbalance(&b, 1), Some(0.0));
    assert_eq!(depth_imbalance(&b, 1), b.imbalance());
    // n beyond the book depth just uses what exists:
    assert_eq!(depth_imbalance(&b, 10), Some(-0.25));
}

#[test]
fn depth_imbalance_unreadable_books() {
    assert_eq!(depth_imbalance(&book(&[BookLevel::new(99.0, 10.0)], &[]), 2), None); // one side empty
    assert_eq!(depth_imbalance(&L2Book::new(0.5), 2), None); // both empty
}

// Symmetric distances (d equal level-for-level) → the weights cancel out of the
// ratio: bids (99,6)(98,4) asks (101,2)(102,4) has pairwise-equal d, so weighted ==
// computable by hand from raw sizes with per-pair weights w1, w2.
#[test]
fn weighted_imbalance_symmetric_distances() {
    let b = book(
        &[BookLevel::new(99.0, 6.0), BookLevel::new(98.0, 4.0)],
        &[BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 4.0)],
    );
    // mid = 100; d = 0.01 (touch pair), 0.02 (second pair)
    let w1 = 1.0 / (1.01_f64 * 1.01_f64);
    let w2 = 1.0 / (1.02_f64 * 1.02_f64);
    let num = w1 * 6.0 + w2 * 4.0 - (w1 * 2.0 + w2 * 4.0);
    let den = w1 * 6.0 + w2 * 4.0 + (w1 * 2.0 + w2 * 4.0);
    let got = weighted_depth_imbalance(&b, 2).unwrap();
    assert!((got - num / den).abs() < 1e-15, "got {got}");
    // single symmetric pair with equal sizes → exactly 0
    let s = book(&[BookLevel::new(99.0, 7.0)], &[BookLevel::new(101.0, 7.0)]);
    assert_eq!(weighted_depth_imbalance(&s, 1), Some(0.0));
}

// Far depth counts less: unweighted sees balance (10 vs 10) but the bid mass is at
// the touch and the ask mass far away → weighted tips positive (bid pressure).
#[test]
fn weighting_dampens_far_levels() {
    let b = book(
        &[BookLevel::new(99.5, 10.0)],
        &[BookLevel::new(100.5, 1.0), BookLevel::new(110.0, 9.0)],
    );
    assert_eq!(depth_imbalance(&b, 2), Some(0.0));
    let w = weighted_depth_imbalance(&b, 2).unwrap();
    assert!(w > 0.0, "weighted should tip toward the near-side mass, got {w}");
    assert!(w < 1.0);
}

#[test]
fn imbalance_bounds_and_sign_flip() {
    let b = book(
        &[BookLevel::new(99.0, 3.0), BookLevel::new(98.5, 2.0)],
        &[BookLevel::new(101.0, 9.0)],
    );
    let m = book(
        &[BookLevel::new(99.0, 9.0)],
        &[BookLevel::new(101.0, 3.0), BookLevel::new(101.5, 2.0)],
    ); // mirrored sizes
    for n in 1..=3 {
        let x = depth_imbalance(&b, n).unwrap();
        let y = depth_imbalance(&m, n).unwrap();
        assert!((-1.0..=1.0).contains(&x));
        assert_eq!(x.to_bits(), (-y).to_bits(), "mirror must flip the sign exactly");
    }
}

#[test]
fn tracker_caches_last_good_value() {
    let mut t = ImbalanceTracker::new(2, false);
    assert_eq!(t.last(), None);
    let b = book(
        &[BookLevel::new(99.0, 10.0), BookLevel::new(98.0, 5.0)],
        &[BookLevel::new(101.0, 10.0), BookLevel::new(102.0, 15.0)],
    );
    assert_eq!(t.update(&b), Some(-0.25));
    assert_eq!(t.last(), Some(-0.25));
    // resync flicker: one-sided book → update None, cache survives
    assert_eq!(t.update(&book(&[BookLevel::new(99.0, 1.0)], &[])), None);
    assert_eq!(t.last(), Some(-0.25));
    // weighted tracker wires the weighted variant
    let mut wt = ImbalanceTracker::new(1, true);
    assert_eq!(
        wt.update(&book(&[BookLevel::new(99.0, 7.0)], &[BookLevel::new(101.0, 7.0)])),
        Some(0.0)
    );
    assert_eq!(wt.last(), Some(0.0));
}

use super::*;

/// Emitted qty, or `None` for a suppressed fill.
fn emit(o: SnapOutcome) -> Option<f64> {
    match o {
        SnapOutcome::Emit(q) => Some(q),
        SnapOutcome::Suppress => None,
    }
}

#[test]
fn untracked_orders_pass_through_unchanged() {
    let t = FillTracker::new();
    assert_eq!(emit(t.snap_fill_qty("nope", "k1", 123.456, 0.5)), Some(123.456));
    assert_eq!(t.check_dust_residual("nope"), None);
    assert!(t.is_empty());
}

#[test]
fn dust_overfill_is_snapped_to_submitted() {
    let t = FillTracker::new();
    t.register("c", 100.0);
    assert_eq!(emit(t.snap_fill_qty("c", "k1", 60.0, 0.5)), Some(60.0));
    // venue reports 40.02 for the tail → 0.02 over; snap to the 40.0 remaining
    assert_eq!(emit(t.snap_fill_qty("c", "k2", 40.02, 0.51)), Some(40.0));
    // fully filled now: a further dust fill emits nothing
    assert_eq!(emit(t.snap_fill_qty("c", "k3", 0.01, 0.51)), None);
    assert_eq!(t.check_dust_residual("c"), None);
}

/// FINDING 1 (critical) regression: Polymarket decodes the same match up to three times
/// (MATCHED → MINED → CONFIRMED). Without per-trade-key dedup the cumulative inflates 3×.
#[test]
fn status_repeats_fold_the_cumulative_exactly_once() {
    let t = FillTracker::new();
    t.register("c", 100.0);
    // one trade, three status deliveries, identical composite key
    for _ in 0..3 {
        assert_eq!(emit(t.snap_fill_qty("c", "t1:0xORD", 0.02, 0.5)), Some(0.02));
    }
    // the real tail match must NOT be snapped: remaining is 99.98, not 99.94
    assert_eq!(emit(t.snap_fill_qty("c", "t2:0xORD", 99.98, 0.5)), Some(99.98));
    assert_eq!(t.check_dust_residual("c"), None, "order is exactly full");
}

/// A repeat of a fill that was SNAPPED replays the snapped qty, not the reported one, and a
/// repeat of a SUPPRESSED fill stays suppressed.
#[test]
fn status_repeats_replay_the_first_decision() {
    let t = FillTracker::new();
    t.register("c", 100.0);
    assert_eq!(emit(t.snap_fill_qty("c", "t1:o", 100.02, 0.5)), Some(100.0));
    assert_eq!(emit(t.snap_fill_qty("c", "t1:o", 100.02, 0.5)), Some(100.0), "replayed");
    assert_eq!(emit(t.snap_fill_qty("c", "t2:o", 0.01, 0.5)), None, "already full");
    assert_eq!(emit(t.snap_fill_qty("c", "t2:o", 0.01, 0.5)), None, "repeat stays suppressed");
    assert_eq!(t.check_dust_residual("c"), None);
}

/// FINDING 6 (minor): a venue-REPORTED zero still emits (so its trade id reaches the core's
/// dedup set, as on the untracked path); only a snap-to-zero suppresses.
#[test]
fn reported_zero_emits_but_snapped_zero_suppresses() {
    let t = FillTracker::new();
    t.register("c", 100.0);
    assert_eq!(emit(t.snap_fill_qty("c", "z", 0.0, 0.0)), Some(0.0), "reported zero emits");
    assert_eq!(emit(t.snap_fill_qty("c", "k1", 100.0, 0.5)), Some(100.0));
    assert_eq!(emit(t.snap_fill_qty("c", "k2", 0.01, 0.5)), None, "snapped to zero");
}

/// NIT: the tolerance is capped at a fraction of the order's own size, so 0.05 is never a wide
/// band for a small order.
#[test]
fn tolerance_is_relative_for_small_orders() {
    let t = FillTracker::new();
    t.register("small", 1.0); // tol = min(0.05, 0.01) = 0.01
    assert_eq!(emit(t.snap_fill_qty("small", "k", 1.03, 0.5)), Some(1.03), "0.03 > 1% of 1");
    t.register("big", 100.0); // tol = min(0.05, 1.0) = 0.05
    assert_eq!(emit(t.snap_fill_qty("big", "k", 100.03, 0.5)), Some(100.0));
}

#[test]
fn large_overfill_is_left_alone() {
    let t = FillTracker::new();
    t.register("c", 100.0);
    assert_eq!(emit(t.snap_fill_qty("c", "k1", 60.0, 0.5)), Some(60.0));
    // 5.0 over ≫ threshold: untouched
    assert_eq!(emit(t.snap_fill_qty("c", "k2", 45.0, 0.5)), Some(45.0));
}

#[test]
fn dust_residual_completes_exactly_once() {
    let t = FillTracker::new();
    t.register("c", 100.0);
    assert_eq!(emit(t.snap_fill_qty("c", "k1", 99.98, 0.62)), Some(99.98));
    let (qty, px) = t.check_dust_residual("c").expect("dust residual");
    assert!((qty - 0.02).abs() < 1e-9, "residual {qty}");
    assert_eq!(px, 0.62);
    assert_eq!(t.check_dust_residual("c"), None, "duplicate terminal must not re-mint");
    assert!(t.is_empty(), "completed order is evicted");
}

#[test]
fn non_dust_remainder_is_not_synthesized() {
    let t = FillTracker::new();
    t.register("c", 100.0);
    t.snap_fill_qty("c", "k1", 40.0, 0.5);
    assert_eq!(t.check_dust_residual("c"), None); // 60 short = a real partial
    assert_eq!(t.len(), 1, "still tracked");
}

#[test]
fn residual_without_a_fill_price_is_not_synthesized() {
    let t = FillTracker::new();
    t.register("c", 0.01); // never filled → no last_px to mint at
    assert_eq!(t.check_dust_residual("c"), None);
}

#[test]
fn capacity_evicts_fifo() {
    let t = FillTracker::with_config(3, DEFAULT_DUST_SNAP_THRESHOLD);
    for i in 0..5 {
        t.register(&format!("c{i}"), 10.0);
    }
    assert_eq!(t.len(), 3);
    // oldest two evicted → untracked pass-through; newest three still snap
    assert_eq!(emit(t.snap_fill_qty("c0", "k", 10.03, 0.5)), Some(10.03));
    // tol for a 10-share order = min(0.05, 0.1) = 0.05, so 0.03 over still snaps
    assert_eq!(emit(t.snap_fill_qty("c4", "k", 10.03, 0.5)), Some(10.0));
}

#[test]
fn zero_threshold_disables_everything() {
    let t = FillTracker::with_config(DEFAULT_CAPACITY, 0.0);
    t.register("c", 100.0);
    assert_eq!(emit(t.snap_fill_qty("c", "k", 100.02, 0.5)), Some(100.02));
    assert_eq!(t.check_dust_residual("c"), None);
}

/// The per-order status-repeat seen-set is FIFO-bounded; evicting the oldest key degrades that
/// key to a re-fold (pre-feature behavior) but must never grow unboundedly.
#[test]
fn per_order_seen_set_is_bounded() {
    let t = FillTracker::new();
    t.register("c", 1e9);
    for i in 0..(DEFAULT_TRADE_KEYS_PER_ORDER + 10) {
        t.snap_fill_qty("c", &format!("k{i}"), 1.0, 0.5);
    }
    let g = t.inner.lock().unwrap();
    assert_eq!(g.orders["c"].seen.len(), DEFAULT_TRADE_KEYS_PER_ORDER);
}

#[test]
fn remove_and_nonpositive_register_are_inert() {
    let t = FillTracker::new();
    t.register("c", 0.0);
    assert!(t.is_empty());
    t.register("c", 5.0);
    t.remove("c");
    assert!(t.is_empty());
    assert!(format!("{t:?}").contains("FillTracker"));
}

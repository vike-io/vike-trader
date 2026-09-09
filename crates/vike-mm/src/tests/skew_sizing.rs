//! Inventory-skew size shaping (`skew_multipliers`) in isolation: neutral reduction, symmetry,
//! monotonicity, band clamping, and the never-negative floor.

use super::*;

// Neutral skew (the default) must reproduce the fixed size EXACTLY — bit-for-bit — for any
// inventory, so a `SpreadMaker::new(..)` quotes identically to before this feature.
#[test]
fn neutral_skew_reproduces_fixed_size_exactly() {
    let qty = 0.001_f64;
    for &pos in &[-5.0, -1.0, -0.3, 0.0, 0.3, 1.0, 5.0] {
        let (b, a) = skew_multipliers(pos, 0.0, 1.0, 0.0);
        assert_eq!(b.to_bits(), 1.0_f64.to_bits(), "bid mult exactly 1.0 (pos {pos})");
        assert_eq!(a.to_bits(), 1.0_f64.to_bits(), "ask mult exactly 1.0 (pos {pos})");
        // the shaped size is the base size, bit-for-bit (x * 1.0 == x for all finite x)
        assert_eq!((qty * b).to_bits(), qty.to_bits(), "bid size unchanged (pos {pos})");
        assert_eq!((qty * a).to_bits(), qty.to_bits(), "ask size unchanged (pos {pos})");
    }
    // a degenerate band also disables (and guards the divide) even with a nonzero skew set
    let (b, a) = skew_multipliers(3.0, 0.0, 0.0, 0.5);
    assert_eq!(b.to_bits(), 1.0_f64.to_bits(), "zero band → neutral bid");
    assert_eq!(a.to_bits(), 1.0_f64.to_bits(), "zero band → neutral ask");
    // negative skew is treated as off, not anti-skew
    let (b, a) = skew_multipliers(3.0, 0.0, 1.0, -0.5);
    assert_eq!(b.to_bits(), 1.0_f64.to_bits(), "negative skew → neutral bid");
    assert_eq!(a.to_bits(), 1.0_f64.to_bits(), "negative skew → neutral ask");
}

// With skew ACTIVE but the position exactly at target, both sides still quote the base size.
#[test]
fn flat_inventory_quotes_symmetric_base_size() {
    let (b, a) = skew_multipliers(2.5, 2.5, 4.0, 0.8);
    assert_eq!(b.to_bits(), 1.0_f64.to_bits(), "flat → neutral bid");
    assert_eq!(a.to_bits(), 1.0_f64.to_bits(), "flat → neutral ask");
}

// Long shrinks the bid + grows the ask; short is the exact mirror; the two sides are symmetric
// about the target and shift by equal-and-opposite amounts around 1.0.
#[test]
fn long_shrinks_bid_grows_ask_and_short_mirrors() {
    let (target, max, skew) = (0.0, 2.0, 0.5);
    let (b_long, a_long) = skew_multipliers(1.0, target, max, skew);
    assert!(b_long < 1.0, "long → bid shrinks: {b_long}");
    assert!(a_long > 1.0, "long → ask grows: {a_long}");

    let (b_short, a_short) = skew_multipliers(-1.0, target, max, skew);
    assert!(b_short > 1.0, "short → bid grows: {b_short}");
    assert!(a_short < 1.0, "short → ask shrinks: {a_short}");

    // symmetry about the target: bid(+d) == ask(−d) and ask(+d) == bid(−d), exactly
    assert_eq!(b_long.to_bits(), a_short.to_bits(), "bid(+d) mirrors ask(−d)");
    assert_eq!(a_long.to_bits(), b_short.to_bits(), "ask(+d) mirrors bid(−d)");
    // conservation: the sides move equal-and-opposite around the base multiplier of 1.0
    assert!(((b_long + a_long) - 2.0).abs() < 1e-12, "sides sum to 2.0: {b_long}+{a_long}");
}

// Bid is monotonically non-increasing and ask monotonically non-decreasing as the position
// rises (each step of the chain — divide, clamp, scale, subtract — is order-preserving in f64,
// so exact `<=`/`>=` hold with no tolerance).
#[test]
fn monotonic_in_position() {
    let (target, max, skew) = (0.0, 3.0, 0.7);
    let mut prev_bid = f64::INFINITY;
    let mut prev_ask = f64::NEG_INFINITY;
    let mut steps = 0;
    let mut p = -5.0;
    while p <= 5.0 {
        let (bid, ask) = skew_multipliers(p, target, max, skew);
        assert!(bid <= prev_bid, "bid non-increasing in position at {p}: {bid} > {prev_bid}");
        assert!(ask >= prev_ask, "ask non-decreasing in position at {p}: {ask} < {prev_ask}");
        prev_bid = bid;
        prev_ask = ask;
        steps += 1;
        p += 0.25;
    }
    assert!(steps > 10, "swept enough points ({steps})");
}

// Beyond ±max_inventory the multipliers saturate to (1∓skew) — clamping holds, no runaway —
// and the short side beyond the band mirrors the long side beyond the band.
#[test]
fn clamps_at_max_inventory() {
    let (target, max, skew) = (0.0, 2.0, 0.6);
    let edge = skew_multipliers(max, target, max, skew);
    let beyond = skew_multipliers(max * 10.0, target, max, skew);
    assert_eq!(edge.0.to_bits(), beyond.0.to_bits(), "bid saturates at the band");
    assert_eq!(edge.1.to_bits(), beyond.1.to_bits(), "ask saturates at the band");
    // saturated values are exactly (1 − skew, 1 + skew)
    assert!((edge.0 - (1.0 - skew)).abs() < 1e-12, "bid saturates to 1−skew: {}", edge.0);
    assert!((edge.1 - (1.0 + skew)).abs() < 1e-12, "ask saturates to 1+skew: {}", edge.1);
    // the short side beyond the band mirrors the long side beyond the band
    let short_beyond = skew_multipliers(-max * 10.0, target, max, skew);
    assert_eq!(short_beyond.0.to_bits(), edge.1.to_bits(), "short bid mirrors long ask");
    assert_eq!(short_beyond.1.to_bits(), edge.0.to_bits(), "short ask mirrors long bid");
}

// A skew intensity > 1 would drive a raw multiplier negative at full imbalance; it must floor
// at 0 so the maker never requests a negative size.
#[test]
fn oversized_skew_never_requests_negative_size() {
    let (bid, ask) = skew_multipliers(100.0, 0.0, 1.0, 1.5);
    assert!((0.0..1e-12).contains(&bid), "long + skew>1 → bid floored to 0: {bid}");
    assert!(ask >= 0.0, "ask never negative: {ask}");
    // and the mirror: heavily short floors the ask
    let (bid, ask) = skew_multipliers(-100.0, 0.0, 1.0, 1.5);
    assert!((0.0..1e-12).contains(&ask), "short + skew>1 → ask floored to 0: {ask}");
    assert!(bid >= 0.0, "bid never negative: {bid}");
}

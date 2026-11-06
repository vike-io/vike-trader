//! The executable-spread arithmetic judged on hand-built legs: the sign rule, the round-trip and
//! carry costs, and the gate an entry edge must clear.

use super::*;

fn leg(ratio: f64, bid: f64, ask: f64, fee: f64) -> SpreadLeg {
    SpreadLeg { ratio, bid, ask, taker_fee: fee }
}

/// Buying the spread LIFTS the ask on a long leg and HITS the bid on a short
/// leg. A naive `bid_A - bid_B` / `ask_A - ask_B` understates the cost.
#[test]
fn negative_ratio_leg_uses_the_opposite_side() {
    let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
    let (bid, ask) = executable_spread(&legs).unwrap();
    // buy spread: pay ask_A(101), receive bid_B(49) -> 52
    assert_eq!(ask.to_bits(), 52.0_f64.to_bits());
    // sell spread: receive bid_A(99), pay ask_B(51) -> 48
    assert_eq!(bid.to_bits(), 48.0_f64.to_bits());
}

/// The property a naive synthetic-instrument quote violates: the executable
/// spread is NEVER crossed, even when the short leg is much wider.
#[test]
fn spread_is_never_crossed_even_with_a_wide_short_leg() {
    let legs = [leg(1.0, 99.9, 100.1, 0.0), leg(-1.0, 40.0, 60.0, 0.0)];
    let (bid, ask) = executable_spread(&legs).unwrap();
    assert!(ask >= bid, "crossed spread: bid {bid} > ask {ask}");
}

/// A round trip crosses FOUR half-spreads: both legs' full widths.
#[test]
fn roundtrip_cost_is_both_legs_full_width_when_fees_are_zero() {
    let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
    let cost = spread_roundtrip_cost(&legs).unwrap();
    assert_eq!(cost.to_bits(), 4.0_f64.to_bits()); // 2.0 + 2.0
}

/// Hedge ratio scales the short leg's contribution to both quote and cost.
#[test]
fn hedge_ratio_scales_the_short_leg() {
    let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-2.0, 49.0, 51.0, 0.0)];
    let cost = spread_roundtrip_cost(&legs).unwrap();
    assert_eq!(cost.to_bits(), 6.0_f64.to_bits()); // 2.0 + 2*2.0
}

/// Fees are charged on TWO crossings per leg (in and out), on |ratio|*mid.
#[test]
fn fees_are_charged_on_two_crossings_per_leg() {
    let legs = [leg(1.0, 100.0, 100.0, 0.001), leg(-1.0, 50.0, 50.0, 0.001)];
    let cost = spread_roundtrip_cost(&legs).unwrap();
    // width 0; fees = 2*(1*100*0.001) + 2*(1*50*0.001) = 0.2 + 0.1
    assert!((cost - 0.3).abs() < 1e-12, "cost {cost}");
}

/// A degenerate or crossed input book is UNKNOWABLE, not free.
#[test]
fn bad_book_is_none_not_zero() {
    assert!(executable_spread(&[leg(1.0, 101.0, 99.0, 0.0)]).is_none(), "crossed leg");
    assert!(executable_spread(&[leg(1.0, f64::NAN, 99.0, 0.0)]).is_none(), "nan leg");
    assert!(executable_spread(&[]).is_none(), "no legs");
}

/// The gate: an edge smaller than the round trip must NOT clear.
#[test]
fn edge_must_exceed_the_full_roundtrip_cost() {
    let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
    assert!(!spread_edge_clears_cost(3.9, &legs), "3.9 < 4.0 must not clear");
    assert!(spread_edge_clears_cost(4.1, &legs), "4.1 > 4.0 must clear");
}

// --- carry (Task 1b) ---

/// Carry is charged per leg on signed exposure: a long leg PAYS positive
/// funding, a short leg RECEIVES it. Net carry on a hedged pair is the
/// DIFFERENTIAL, not either leg's rate.
#[test]
fn carry_is_the_signed_differential_across_legs() {
    let legs = [leg(1.0, 100.0, 100.0, 0.0), leg(-1.0, 100.0, 100.0, 0.0)];
    // identical rates on both legs net to zero carry
    let c = spread_carry_cost(&legs, &[0.0001, 0.0001], 3.0).unwrap();
    assert!(c.abs() < 1e-12, "equal rates must net to zero, got {c}");
    // long leg pays 1bp more than the short receives -> a real cost
    let c = spread_carry_cost(&legs, &[0.0002, 0.0001], 3.0).unwrap();
    assert!((c - 0.03).abs() < 1e-12, "carry {c}"); // 1e-4 * 100 * 3 intervals
}

/// Carry scales with holding time — the term that makes "hold longer to
/// amortize the fees" backfire.
#[test]
fn carry_scales_with_intervals_held() {
    let legs = [leg(1.0, 100.0, 100.0, 0.0)];
    let short_hold = spread_carry_cost(&legs, &[0.0001], 1.0).unwrap();
    let long_hold = spread_carry_cost(&legs, &[0.0001], 30.0).unwrap();
    assert!((long_hold - 30.0 * short_hold).abs() < 1e-12);
}

/// Total cost = crossings + carry. A caller must never charge only one.
#[test]
fn total_cost_is_crossings_plus_carry() {
    let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
    let rt = spread_roundtrip_cost(&legs).unwrap();
    let carry = spread_carry_cost(&legs, &[0.0002, 0.0001], 3.0).unwrap();
    let total = spread_total_cost(&legs, &[0.0002, 0.0001], 3.0).unwrap();
    assert!((total - (rt + carry)).abs() < 1e-12);
}

/// A rate per leg is required; a mismatched slice is unknowable, not free.
#[test]
fn mismatched_rate_slice_is_none() {
    let legs = [leg(1.0, 100.0, 100.0, 0.0)];
    assert!(spread_carry_cost(&legs, &[], 1.0).is_none());
    assert!(spread_carry_cost(&legs, &[0.0001, 0.0002], 1.0).is_none());
    assert!(spread_carry_cost(&legs, &[f64::NAN], 1.0).is_none());
    assert!(spread_carry_cost(&legs, &[0.0001], -1.0).is_none(), "negative hold");
}

/// Carry can exceed the crossing cost on a long hold — the regime that makes
/// a crossings-only floor dangerously optimistic.
#[test]
fn carry_can_dominate_the_crossing_cost() {
    let legs = [leg(1.0, 99.9, 100.1, 0.0), leg(-1.0, 99.9, 100.1, 0.0)];
    let rt = spread_roundtrip_cost(&legs).unwrap(); // 0.4
    // 3bp/interval differential over 90 intervals (~30 days at 8h funding)
    let carry = spread_carry_cost(&legs, &[0.0003, 0.0], 90.0).unwrap();
    assert!(carry > rt, "carry {carry} should dominate crossings {rt}");
}

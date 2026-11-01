//! Walk-the-book and taker-price tests: VWAP, depth at a price, fill simulation, slippage sign.

use super::*;
use crate::test_support::book;

/// Moved here with the law itself (it was in vike-backtest's `fill_model.rs`): the rule is a
/// property of the book, so its proof belongs beside it — and this crate is the one both the
/// backtest engine and the live path can reach.
#[test]
fn book_taker_price_refuses_rather_than_inventing_liquidity() {
    let mut b = L2Book::new(0.01);
    // asks 0.30 x100, 0.31 x100, 0.35 x1000 ; bids 0.28 x100, 0.27 x100
    b.apply_snapshot(
        1,
        &[BookLevel::new(0.28, 100.0), BookLevel::new(0.27, 100.0)],
        &[BookLevel::new(0.30, 100.0), BookLevel::new(0.31, 100.0), BookLevel::new(0.35, 1000.0)],
    );
    assert_eq!(book_taker_price(&b, 0, 10.0, None), None, "no side");
    assert_eq!(book_taker_price(&b, 1, 0.0, None), None, "no size");
    assert_eq!(book_taker_price(&b, 1, -5.0, None), None, "negative size");
    assert_eq!(book_taker_price(&b, 1, f64::NAN, None), None);
    assert_eq!(book_taker_price(&b, 1, 10.0, Some(f64::NAN)), None);
    assert_eq!(book_taker_price(&b, 1, 5_000.0, None), None, "beyond displayed depth");
    assert_eq!(book_taker_price(&L2Book::new(0.01), 1, 1.0, None), None, "empty book");
    // the capped walk equals the uncapped one when the cap is not binding
    assert_eq!(book_taker_price(&b, 1, 150.0, Some(0.31)), book_taker_price(&b, 1, 150.0, None));
}

// ---- walk-the-book helpers ----

/// bids 99.5×5, 99.0×10 | asks 100.5×5, 101.0×10 — mid exactly 100.0, 15 per side.
fn two_level_book() -> L2Book {
    book(
        &[BookLevel::new(99.5, 5.0), BookLevel::new(99.0, 10.0)],
        &[BookLevel::new(100.5, 5.0), BookLevel::new(101.0, 10.0)],
    )
}

#[test]
fn walk_empty_book_is_all_none_and_unfillable() {
    let b = L2Book::new(0.5);
    assert_eq!(b.avg_px_for_quantity(1, 1.0), None);
    assert_eq!(b.avg_px_for_quantity(-1, 1.0), None);
    assert_eq!(b.quantity_for_price(1, 100.0), 0.0);
    assert_eq!(b.quantity_for_price(-1, 100.0), 0.0);
    assert!(!b.can_fill(1, 1.0));
    assert!(!b.can_fill(-1, 1.0));
    assert_eq!(b.fill_ratio(1, 1.0), 0.0);
    let sim = b.simulate_fill(1, 3.0);
    assert!(sim.fills.is_empty());
    assert_eq!(sim.total_filled, 0.0);
    assert_eq!(sim.remaining, 3.0);
    assert_eq!(sim.avg_px, None);
    assert_eq!(sim.worst_px, None);
    assert_eq!(sim.slippage_bps_vs_mid, None);
    assert_eq!(sim.levels_consumed, 0);
    // qty 0 is vacuously fillable even on an empty book
    assert!(b.can_fill(1, 0.0));
    assert_eq!(b.fill_ratio(1, 0.0), 1.0);
}

#[test]
fn walk_zero_qty_on_populated_book_is_vacuous() {
    let b = two_level_book();
    assert_eq!(b.avg_px_for_quantity(1, 0.0), None); // no VWAP of nothing
    assert!(b.can_fill(1, 0.0));
    assert_eq!(b.fill_ratio(1, 0.0), 1.0);
    let sim = b.simulate_fill(1, 0.0);
    assert!(sim.fills.is_empty());
    assert_eq!(sim.total_filled, 0.0);
    assert_eq!(sim.remaining, 0.0);
    assert_eq!(sim.avg_px, None);
    assert_eq!(sim.worst_px, None);
    assert_eq!(sim.slippage_bps_vs_mid, None);
    assert_eq!(sim.levels_consumed, 0);
}

#[test]
fn walk_one_level_exact_qty() {
    let b = book(&[], &[BookLevel::new(100.5, 5.0)]);
    assert_eq!(b.avg_px_for_quantity(1, 5.0), Some(100.5));
    assert!(b.can_fill(1, 5.0));
    assert_eq!(b.fill_ratio(1, 5.0), 1.0);
    let sim = b.simulate_fill(1, 5.0);
    assert_eq!(sim.fills, vec![BookLevel::new(100.5, 5.0)]);
    assert_eq!(sim.total_filled, 5.0);
    assert_eq!(sim.remaining, 0.0);
    assert_eq!(sim.avg_px, Some(100.5));
    assert_eq!(sim.worst_px, Some(100.5));
    assert_eq!(sim.slippage_bps_vs_mid, None); // bid side empty → no mid
    assert_eq!(sim.levels_consumed, 1);
}

#[test]
fn walk_one_level_partial_when_qty_exceeds_book() {
    let b = book(&[], &[BookLevel::new(100.5, 5.0)]);
    assert_eq!(b.avg_px_for_quantity(1, 6.0), None); // can't fill fully
    assert!(!b.can_fill(1, 6.0));
    assert_eq!(b.fill_ratio(1, 6.0), 5.0 / 6.0);
    let sim = b.simulate_fill(1, 6.0);
    assert_eq!(sim.fills, vec![BookLevel::new(100.5, 5.0)]);
    assert_eq!(sim.total_filled, 5.0);
    assert_eq!(sim.remaining, 1.0);
    assert_eq!(sim.avg_px, Some(100.5)); // VWAP of what DID fill
    assert_eq!(sim.worst_px, Some(100.5));
    assert_eq!(sim.levels_consumed, 1);
}

#[test]
fn walk_multi_level_buy_vwap_and_detail() {
    let b = two_level_book();
    // buy 7: 5 @ 100.5 + 2 @ 101.0 (best-first on asks, low→high)
    let expect_avg = (5.0 * 100.5 + 2.0 * 101.0) / 7.0;
    assert_eq!(b.avg_px_for_quantity(1, 7.0), Some(expect_avg));
    let sim = b.simulate_fill(1, 7.0);
    assert_eq!(sim.fills, vec![BookLevel::new(100.5, 5.0), BookLevel::new(101.0, 2.0)]);
    assert_eq!(sim.total_filled, 7.0);
    assert_eq!(sim.remaining, 0.0);
    assert_eq!(sim.avg_px, Some(expect_avg));
    assert_eq!(sim.worst_px, Some(101.0));
    assert_eq!(sim.levels_consumed, 2);
}

#[test]
fn walk_multi_level_sell_walks_bids_high_to_low() {
    let b = two_level_book();
    // sell 7: 5 @ 99.5 + 2 @ 99.0 (best-first on bids, high→low)
    let expect_avg = (5.0 * 99.5 + 2.0 * 99.0) / 7.0;
    assert_eq!(b.avg_px_for_quantity(-1, 7.0), Some(expect_avg));
    let sim = b.simulate_fill(-1, 7.0);
    assert_eq!(sim.fills, vec![BookLevel::new(99.5, 5.0), BookLevel::new(99.0, 2.0)]);
    assert_eq!(sim.worst_px, Some(99.0));
    assert_eq!(sim.levels_consumed, 2);
}

#[test]
fn walk_exact_boundary_consumes_whole_side() {
    let b = two_level_book();
    // exactly the total displayed on each side
    for side in [1, -1] {
        assert!(b.can_fill(side, 15.0));
        assert_eq!(b.fill_ratio(side, 15.0), 1.0);
        let sim = b.simulate_fill(side, 15.0);
        assert_eq!(sim.total_filled, 15.0);
        assert_eq!(sim.remaining, 0.0);
        assert_eq!(sim.levels_consumed, 2);
        // one drop more than displayed → unfillable
        assert!(!b.can_fill(side, 15.0 + 1e-9));
        assert_eq!(b.avg_px_for_quantity(side, 15.0 + 1e-9), None);
    }
}

#[test]
fn walk_partial_multi_level_ratio_and_remaining() {
    let b = two_level_book();
    let sim = b.simulate_fill(1, 20.0);
    assert_eq!(sim.total_filled, 15.0);
    assert_eq!(sim.remaining, 5.0);
    assert_eq!(sim.avg_px, Some((5.0 * 100.5 + 10.0 * 101.0) / 15.0));
    assert_eq!(sim.worst_px, Some(101.0));
    assert_eq!(sim.levels_consumed, 2);
    assert_eq!(b.fill_ratio(1, 20.0), 15.0 / 20.0);
}

#[test]
fn quantity_for_price_at_or_better_both_sides() {
    let b = two_level_book();
    // buy: asks at or below the limit
    assert_eq!(b.quantity_for_price(1, 100.0), 0.0); // below best ask
    assert_eq!(b.quantity_for_price(1, 100.5), 5.0); // exact best-ask boundary included
    assert_eq!(b.quantity_for_price(1, 101.0), 15.0);
    assert_eq!(b.quantity_for_price(1, 200.0), 15.0);
    // sell: bids at or above the limit
    assert_eq!(b.quantity_for_price(-1, 100.0), 0.0); // above best bid
    assert_eq!(b.quantity_for_price(-1, 99.5), 5.0); // exact best-bid boundary included
    assert_eq!(b.quantity_for_price(-1, 99.0), 15.0);
    assert_eq!(b.quantity_for_price(-1, 1.0), 15.0);
}

#[test]
fn quantity_for_price_snaps_limit_to_tick_grid() {
    // realistic 0.01 grid: the exact-boundary compare goes through tick keys, so a
    // limit equal to a level's price always includes that level (no float-compare
    // hazard on prices like 0.1 that aren't binary-exact).
    let mut b = L2Book::new(0.01);
    b.apply_snapshot(
        1,
        &[BookLevel::new(0.29, 7.0)],
        &[BookLevel::new(0.3, 4.0), BookLevel::new(0.31, 6.0)],
    );
    assert_eq!(b.quantity_for_price(1, 0.3), 4.0);
    assert_eq!(b.quantity_for_price(1, 0.31), 10.0);
    assert_eq!(b.quantity_for_price(-1, 0.29), 7.0);
    // off-grid limit snaps with the same round-half-even rule levels use
    assert_eq!(b.quantity_for_price(1, 0.302), 4.0);
}

#[test]
fn slippage_sign_positive_means_worse_than_mid_both_sides() {
    let b = two_level_book(); // mid exactly 100.0
    let buy = b.simulate_fill(1, 10.0); // avg 100.75 > mid → cost
    let avg_b = (5.0 * 100.5 + 5.0 * 101.0) / 10.0;
    assert_eq!(buy.avg_px, Some(avg_b));
    let slip_b = buy.slippage_bps_vs_mid.unwrap();
    assert!((slip_b - 75.0).abs() < 1e-9, "buy slippage {slip_b} != ~75bps");
    assert!(slip_b > 0.0);
    let sell = b.simulate_fill(-1, 10.0); // avg 99.25 < mid → cost, sign flipped
    let avg_s = (5.0 * 99.5 + 5.0 * 99.0) / 10.0;
    assert_eq!(sell.avg_px, Some(avg_s));
    let slip_s = sell.slippage_bps_vs_mid.unwrap();
    assert!((slip_s - 75.0).abs() < 1e-9, "sell slippage {slip_s} != ~75bps");
    assert!(slip_s > 0.0);
}

#[test]
fn slippage_none_when_one_side_empty_but_fill_still_reported() {
    let b = book(&[], &[BookLevel::new(100.5, 5.0), BookLevel::new(101.0, 10.0)]);
    let sim = b.simulate_fill(1, 7.0);
    assert_eq!(sim.total_filled, 7.0);
    assert!(sim.avg_px.is_some());
    assert_eq!(sim.slippage_bps_vs_mid, None); // no bid → no mid → None-safe
}

#[test]
fn walk_degenerate_crossed_book_tolerated() {
    // crossed input (bid above ask) — reducer stores it as pushed; walks must not
    // panic and slippage reads as price IMPROVEMENT (negative) vs the crossed mid.
    let b = book(&[BookLevel::new(101.0, 1.0)], &[BookLevel::new(100.0, 1.0)]); // mid 100.5
    let buy = b.simulate_fill(1, 1.0);
    assert_eq!(buy.avg_px, Some(100.0));
    assert!(buy.slippage_bps_vs_mid.unwrap() < 0.0);
    let sell = b.simulate_fill(-1, 1.0);
    assert_eq!(sell.avg_px, Some(101.0));
    assert!(sell.slippage_bps_vs_mid.unwrap() < 0.0);
    assert!(b.can_fill(1, 1.0) && b.can_fill(-1, 1.0));
}

#[test]
fn walk_side_zero_fills_nothing() {
    let b = two_level_book();
    assert_eq!(b.avg_px_for_quantity(0, 1.0), None);
    assert_eq!(b.quantity_for_price(0, 100.5), 0.0);
    assert!(!b.can_fill(0, 1.0));
    assert!(b.can_fill(0, 0.0)); // still vacuous at qty 0
    assert_eq!(b.fill_ratio(0, 1.0), 0.0);
    let sim = b.simulate_fill(0, 1.0);
    assert!(sim.fills.is_empty());
    assert_eq!(sim.total_filled, 0.0);
    assert_eq!(sim.remaining, 1.0);
}

#[test]
fn walk_degenerate_qty_inputs_graceful() {
    let b = two_level_book();
    // negative qty: nothing to fill → vacuously complete
    assert!(b.can_fill(1, -1.0));
    assert_eq!(b.fill_ratio(1, -1.0), 1.0);
    assert_eq!(b.avg_px_for_quantity(1, -1.0), None);
    let sim = b.simulate_fill(1, -1.0);
    assert!(sim.fills.is_empty());
    assert_eq!(sim.total_filled, 0.0);
    assert_eq!(sim.remaining, 0.0);
    // NaN qty: unfillable, nothing consumed, no panic
    assert!(!b.can_fill(1, f64::NAN));
    assert_eq!(b.fill_ratio(1, f64::NAN), 0.0);
    assert_eq!(b.avg_px_for_quantity(1, f64::NAN), None);
    let sim = b.simulate_fill(1, f64::NAN);
    assert!(sim.fills.is_empty());
    assert_eq!(sim.total_filled, 0.0);
    assert_eq!(sim.avg_px, None);
    // NaN limit price: no size
    assert_eq!(b.quantity_for_price(1, f64::NAN), 0.0);
}

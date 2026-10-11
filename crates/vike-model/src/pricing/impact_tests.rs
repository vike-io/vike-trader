//! The pure book walks of the pre-trade impact veto (`impact_veto`, `fillable_veto`), judged
//! without a gate: the `RiskGate::check_with_book` lane tests stay in vike-exec's
//! `risk/tests/impact.rs`, beside the gate they build.

use super::*;
use crate::BookLevel;

/// asks 100@1, 101@2, 102@3 ; bids 99@1, 98@2, 97@3 ⇒ mid = 99.5, tick 1.0
fn book() -> L2Book {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(
        1,
        &[BookLevel::new(99.0, 1.0), BookLevel::new(98.0, 2.0), BookLevel::new(97.0, 3.0)],
        &[BookLevel::new(100.0, 1.0), BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 3.0)],
    );
    b
}

#[test]
fn impact_veto_none_budget_never_denies() {
    let b = book();
    // even a size the book cannot fill at all passes when the knob is off
    assert_eq!(impact_veto(&b, 1, 1e9, None), None);
    assert_eq!(impact_veto(&b, -1, 1e9, None), None);
    assert_eq!(impact_veto(&L2Book::new(1.0), 1, 5.0, None), None);
}

#[test]
fn impact_veto_empty_book_not_fillable() {
    let empty = L2Book::new(1.0);
    assert_eq!(impact_veto(&empty, 1, 1.0, Some(1e9)), Some(ImpactDeny::NotFillable));
    assert_eq!(impact_veto(&empty, -1, 1.0, Some(1e9)), Some(ImpactDeny::NotFillable));
    assert_eq!(fillable_veto(&empty, 1, 1.0), Some(ImpactDeny::NotFillable));
    // qty 0 is vacuously fillable
    assert_eq!(fillable_veto(&empty, 1, 0.0), None);
}

#[test]
fn impact_veto_partial_walk_is_not_fillable() {
    let b = book();
    // 6 units exhausts each side exactly; 7 cannot fill ⇒ slippage is unbounded ⇒ deny
    assert_eq!(fillable_veto(&b, 1, 6.0), None);
    assert_eq!(impact_veto(&b, 1, 7.0, Some(1e9)), Some(ImpactDeny::NotFillable));
    assert_eq!(impact_veto(&b, -1, 7.0, Some(1e9)), Some(ImpactDeny::NotFillable));
    assert_eq!(fillable_veto(&b, -1, 7.0), Some(ImpactDeny::NotFillable));
}

#[test]
fn impact_veto_exact_fill_both_sides_and_budget_boundary() {
    let b = book();
    // BUY 3 → 100×1 + 101×2 = 302 / 3 = 100.6667 ; mid 99.5 ⇒ +117.25 bps
    let buy = b.simulate_fill(1, 3.0).slippage_bps_vs_mid.unwrap();
    assert!(buy > 117.0 && buy < 118.0, "buy slippage {buy}");
    // SELL 3 → 99×1 + 98×2 = 295 / 3 = 98.3333 ; below mid ⇒ positive (worse for taker)
    let sell = b.simulate_fill(-1, 3.0).slippage_bps_vs_mid.unwrap();
    assert!(sell > 117.0 && sell < 118.0, "sell slippage {sell}");

    // budget-EQUAL passes (strict >), a hair under denies
    assert_eq!(impact_veto(&b, 1, 3.0, Some(buy)), None);
    assert_eq!(impact_veto(&b, 1, 3.0, Some(buy - 1e-9)), Some(ImpactDeny::OverSlippageBudget));
    assert_eq!(impact_veto(&b, -1, 3.0, Some(sell)), None);
    assert_eq!(impact_veto(&b, -1, 3.0, Some(sell - 1e-9)), Some(ImpactDeny::OverSlippageBudget));
    // touching only the top of book is cheapest and passes a tight budget
    assert_eq!(impact_veto(&b, 1, 1.0, Some(51.0)), None);
}

/// The two public pure fns must agree on degenerate size.
#[test]
fn impact_and_fillable_agree_on_non_positive_qty() {
    let b = book();
    for side in [1, -1] {
        assert_eq!(fillable_veto(&b, side, 0.0), None);
        assert_eq!(impact_veto(&b, side, 0.0, Some(0.0)), None);
        assert_eq!(fillable_veto(&b, side, -1.0), None);
        assert_eq!(impact_veto(&b, side, -1.0, Some(0.0)), None);
    }
}

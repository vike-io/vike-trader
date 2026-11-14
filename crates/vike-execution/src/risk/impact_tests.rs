//! The pure book walks of the pre-trade impact veto (`impact_veto`, `fillable_veto`), judged
//! without a gate: the `RiskGate::check_with_book` lane tests are `risk/tests/impact.rs`, beside
//! the gate they build.

use super::*;
use crate::risk::tests::book;

/// The ordinary contract: linear, one unit.
const LIN: ContractValue = ContractValue::linear(1.0);

#[test]
fn impact_veto_none_budget_never_denies() {
    let b = book();
    // even a size the book cannot fill at all passes when the knob is off
    assert_eq!(impact_veto(&b, 1, 1e9, None, LIN), None);
    assert_eq!(impact_veto(&b, -1, 1e9, None, LIN), None);
    assert_eq!(impact_veto(&L2Book::new(1.0), 1, 5.0, None, LIN), None);
}

#[test]
fn impact_veto_empty_book_not_fillable() {
    let empty = L2Book::new(1.0);
    assert_eq!(impact_veto(&empty, 1, 1.0, Some(1e9), LIN), Some(ImpactDeny::NotFillable));
    assert_eq!(impact_veto(&empty, -1, 1.0, Some(1e9), LIN), Some(ImpactDeny::NotFillable));
    assert_eq!(fillable_veto(&empty, 1, 1.0), Some(ImpactDeny::NotFillable));
    // qty 0 is vacuously fillable
    assert_eq!(fillable_veto(&empty, 1, 0.0), None);
}

#[test]
fn impact_veto_partial_walk_is_not_fillable() {
    let b = book();
    // 6 units exhausts each side exactly; 7 cannot fill ⇒ slippage is unbounded ⇒ deny
    assert_eq!(fillable_veto(&b, 1, 6.0), None);
    assert_eq!(impact_veto(&b, 1, 7.0, Some(1e9), LIN), Some(ImpactDeny::NotFillable));
    assert_eq!(impact_veto(&b, -1, 7.0, Some(1e9), LIN), Some(ImpactDeny::NotFillable));
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
    assert_eq!(impact_veto(&b, 1, 3.0, Some(buy), LIN), None);
    let over = Some(ImpactDeny::OverSlippageBudget);
    assert_eq!(impact_veto(&b, 1, 3.0, Some(buy - 1e-9), LIN), over);
    assert_eq!(impact_veto(&b, -1, 3.0, Some(sell), LIN), None);
    assert_eq!(impact_veto(&b, -1, 3.0, Some(sell - 1e-9), LIN), over);
    // touching only the top of book is cheapest and passes a tight budget
    assert_eq!(impact_veto(&b, 1, 1.0, Some(51.0), LIN), None);
}

/// The two public pure fns must agree on degenerate size.
#[test]
fn impact_and_fillable_agree_on_non_positive_quantity() {
    let b = book();
    for side in [1, -1] {
        assert_eq!(fillable_veto(&b, side, 0.0), None);
        assert_eq!(impact_veto(&b, side, 0.0, Some(0.0), LIN), None);
        assert_eq!(fillable_veto(&b, side, -1.0), None);
        assert_eq!(impact_veto(&b, side, -1.0, Some(0.0), LIN), None);
    }
}

/// The veto as it was spelled before the contract value reached it: `simulate_fill`'s slippage.
fn simulate_fill_verdict(b: &L2Book, side: i32, qty: f64, budget: f64) -> Option<ImpactDeny> {
    if qty <= 0.0 {
        return None;
    }
    let sim = b.simulate_fill(side, qty);
    if sim.remaining > 0.0 || sim.total_filled <= 0.0 {
        return Some(ImpactDeny::NotFillable);
    }
    match sim.slippage_bps_vs_mid {
        Some(slip) if slip > budget => Some(ImpactDeny::OverSlippageBudget),
        _ => None,
    }
}

/// A LINEAR contract, at any unit, is judged exactly as before: the same verdict at every size on
/// both sides, and at the old slippage's own bits as the budget (passes) and the float just under it
/// (denies), so the new slippage IS the old one, bit for bit.
#[test]
fn a_linear_contract_is_judged_exactly_as_the_simulated_fill_judged_it() {
    let b = book();
    for v in [LIN, ContractValue::linear(50.0)] {
        for side in [1, -1, 0] {
            for qty in [0.0, 0.5, 1.0, 2.5, 3.0, 6.0, 6.5, f64::NAN] {
                for budget in [0.0, 50.0, 117.0, 200.0, 1e9] {
                    let want = simulate_fill_verdict(&b, side, qty, budget);
                    assert_eq!(impact_veto(&b, side, qty, Some(budget), v), want, "{side} {qty}");
                }
                if let Some(slip) = b.simulate_fill(side, qty).slippage_bps_vs_mid
                    && qty > 0.0
                    && qty <= 6.0
                {
                    assert_eq!(impact_veto(&b, side, qty, Some(slip), v), None);
                    let under = Some(slip.next_down());
                    assert_eq!(
                        impact_veto(&b, side, qty, under, v),
                        Some(ImpactDeny::OverSlippageBudget),
                        "{side} {qty}: the slippage moved off its old bits"
                    );
                }
            }
        }
    }
}

//! The `RiskGate` unit suites, plus the request, book and leg builders several of them share.

use super::*;
use crate::testing::market_order;
use vike_marketdata::L2BookLevel;

#[cfg(test)]
mod combo;
#[cfg(test)]
mod grid;
#[cfg(test)]
mod halt_and_notional;
#[cfg(test)]
mod impact;
#[cfg(test)]
mod mount_leverage;
#[cfg(test)]
mod price_collar;

/// The one rule both sides count a working order by: its remainder, gross, unless it commits
/// nothing. An uncovered reversal and a same-side add commit their whole remainder.
#[test]
fn committed_quantity_counts_all_but_a_covered_reduce_and_a_dead_remainder() {
    assert_eq!(committed_quantity(3.0, 1, 0.0), Some(3.0), "flat");
    assert_eq!(committed_quantity(3.0, 1, 10.0), Some(3.0), "an add");
    assert_eq!(
        committed_quantity(3.0, -1, 2.0),
        Some(3.0),
        "a reversal the position does not cover"
    );
    assert_eq!(committed_quantity(3.0, -1, 3.0), None, "exactly covered");
    assert_eq!(committed_quantity(3.0, 1, -10.0), None, "covered, on the short side");
    for dead in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(committed_quantity(dead, 1, 0.0), None, "{dead} commits nothing");
    }
}

/// The mount leverage judge: off unless the cap is a finite figure `>= 1.0`; at the cap passes,
/// past it refuses in the one reason word; a NaN figure and a non-positive equity refuse.
#[test]
fn the_mount_leverage_judge_refuses_past_the_cap_and_is_off_without_one() {
    for off in [None, Some(0.5), Some(f64::NAN), Some(f64::INFINITY)] {
        assert_eq!(mount_leverage_refusal(off, 1e12, 1.0), None, "{off:?} is off");
    }
    assert_eq!(mount_leverage_refusal(Some(2.0), 20_000.0, 10_000.0), None, "exactly at the cap");
    let why = mount_leverage_refusal(Some(2.0), 20_100.0, 10_000.0).expect("past the cap");
    assert!(why.starts_with(MOUNT_LEVERAGE_REFUSAL), "{why}");
    assert!(why.contains("20100") && why.contains("10000"), "both figures are said: {why}");
    assert!(mount_leverage_refusal(Some(1.0), f64::NAN, 10_000.0).is_some(), "NaN notional");
    assert!(mount_leverage_refusal(Some(1.0), 100.0, f64::NAN).is_some(), "NaN equity");
    assert!(mount_leverage_refusal(Some(5.0), 100.0, 0.0).is_some(), "no equity to hold on");
    assert!(mount_leverage_refusal(Some(5.0), 100.0, -1.0).is_some(), "a negative equity");
}

/// The leverage arm is no latch arm: a budget stating it alone is not active, so it arms no sweep.
#[test]
fn a_budget_with_only_a_leverage_cap_is_not_an_active_latch() {
    let budget = crate::MountBudget {
        max_loss: None,
        max_notional: None,
        loss_action: vike_model::MountLossAction::Flatten,
        max_leverage: Some(3.0),
    };
    assert!(!budget.is_active(), "the leverage cap never latches");
    assert!(crate::MountBudget { max_loss: Some(1.0), ..budget }.is_active());
}

/// `MountBudget::breached` is the rule `vike_core`'s `sweep_mount_budgets_in` spelled inline before it
/// moved, on every arm and figure: an arm off at `None` or `<= 0.0`, at the cap no breach, past it one.
#[test]
fn the_budget_breach_is_the_sweeps_inline_rule_it_came_from() {
    let as_it_was = |b: &crate::MountBudget, loss: f64, notional: f64| {
        let loss_breach = b.max_loss.is_some_and(|ml| ml > 0.0 && loss > ml);
        let notional_breach = b.max_notional.is_some_and(|mn| mn > 0.0 && notional > mn);
        loss_breach || notional_breach
    };
    let arms = [None, Some(-1.0), Some(0.0), Some(100.0), Some(f64::NAN)];
    let figures = [-50.0, 0.0, 100.0, 100.000_001, 1e9, f64::NAN];
    for (max_loss, max_notional) in arms.iter().flat_map(|l| arms.iter().map(move |n| (*l, *n))) {
        let budget = crate::MountBudget {
            max_loss,
            max_notional,
            loss_action: vike_model::MountLossAction::CancelOnly,
            max_leverage: Some(2.0),
        };
        for (loss, notional) in figures.iter().flat_map(|l| figures.iter().map(move |n| (*l, *n))) {
            assert_eq!(
                budget.breached(loss, notional),
                as_it_was(&budget, loss, notional),
                "{budget:?} at loss {loss} notional {notional}"
            );
        }
    }
    let loss_cap = crate::MountBudget {
        max_loss: Some(100.0),
        max_notional: None,
        loss_action: vike_model::MountLossAction::Flatten,
        max_leverage: None,
    };
    assert!(!loss_cap.breached(100.0, 1e12), "at the loss cap, and no notional arm");
    assert!(loss_cap.breached(100.5, 0.0), "past the loss cap");
}

/// A limit order whose `price` is a SIGNED combo net (`OrderCombo::net_limit`) — the shape a
/// `Combo` takes through the gate once PR-2 lowers it.
fn combo_limit(side: i32, qty: f64, net: f64) -> OrderRequest {
    use vike_model::OrderComboLeg;
    OrderRequest {
        order_client_id: "c1".to_string(),
        venue: "deribit".to_string(),
        symbol: String::new(),
        order_type: vike_model::OrderType::Limit,
        side,
        qty,
        price: Some(net),
        combo_legs: vec![
            OrderComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1, limit: None },
            OrderComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1, limit: None },
        ],
        ..Default::default()
    }
}

/// asks 100@1, 101@2, 102@3 ; bids 99@1, 98@2, 97@3 ⇒ mid = 99.5, tick 1.0
/// (`impact_tests.rs` walks it too).
pub(super) fn book() -> L2Book {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(
        1,
        &[L2BookLevel::new(99.0, 1.0), L2BookLevel::new(98.0, 2.0), L2BookLevel::new(97.0, 3.0)],
        &[L2BookLevel::new(100.0, 1.0), L2BookLevel::new(101.0, 2.0), L2BookLevel::new(102.0, 3.0)],
    );
    b
}

fn limit(side: i32, qty: f64, px: f64) -> OrderRequest {
    OrderRequest {
        order_type: vike_model::OrderType::Limit,
        price: Some(px),
        ..market_order(side, qty)
    }
}

const LEG_A: &str = "BTC-27MAR26-100000-C";
const LEG_B: &str = "BTC-27MAR26-120000-C";

/// per-leg marks: LEG_A 50, LEG_B 30 ⇒ a 1×/−1× call spread nets +20 (debit)
fn leg_marks(symbol: &str) -> RiskContext {
    let mark = match symbol {
        LEG_A => 50.0,
        LEG_B => 30.0,
        _ => 0.0,
    };
    RiskContext { mark_price: mark, ..RiskContext::default() }
}

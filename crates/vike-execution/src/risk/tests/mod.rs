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

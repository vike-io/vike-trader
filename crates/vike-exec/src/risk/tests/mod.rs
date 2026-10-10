//! The `RiskGate` unit suites, plus the request, book and leg builders several of them share.

use super::*;
use vike_model::BookLevel;

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

fn market(side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "t1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        order_type: "market".to_string(),
        side,
        qty,
        ..Default::default()
    }
}

/// A limit order whose `price` is a SIGNED combo net (`ComboSpec::net_limit`) — the shape a
/// `Combo` takes through the gate once PR-2 lowers it.
fn combo_limit(side: i32, qty: f64, net: f64) -> OrderRequest {
    use vike_model::ComboLeg;
    OrderRequest {
        client_order_id: "c1".to_string(),
        venue: "deribit".to_string(),
        symbol: String::new(),
        order_type: "limit".to_string(),
        side,
        qty,
        price: Some(net),
        combo_legs: vec![
            ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
            ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
        ],
        ..Default::default()
    }
}

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

fn limit(side: i32, qty: f64, px: f64) -> OrderRequest {
    OrderRequest { order_type: "limit".to_string(), price: Some(px), ..market(side, qty) }
}

const LEG_A: &str = "BTC-27MAR26-100000-C";
const LEG_B: &str = "BTC-27MAR26-120000-C";

/// per-leg marks: LEG_A 50, LEG_B 30 ⇒ a 1×/−1× call spread nets +20 (debit)
fn leg_marks(sym: &str) -> RiskContext {
    let mark = match sym {
        LEG_A => 50.0,
        LEG_B => 30.0,
        _ => 0.0,
    };
    RiskContext { mark_price: mark, ..RiskContext::default() }
}

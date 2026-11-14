//! Wire commands more than one crate's tests send a node, written once: the resting limit order a node
//! books and never fills without a feed, and a limit bracket.

use crate::wire::{CommandWire, OrderBracketWire, OrderRequestWire};

/// A resting limit on the polymarket token `token` far from any market (20 at 0.40): with no feed it never
/// fills, so the node's book carries exactly what was sent.
pub fn resting_submit(order_client_id: &str, token: &str) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: order_client_id.to_string(),
        venue: "polymarket".to_string(),
        symbol: token.to_string(),
        side: 1,
        qty: 20.0,
        order_type: "limit".to_string(),
        price: Some(0.40),
        trigger_price: None,
        reduce_only: false,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

/// A binance limit bracket on `symbol`: a buy of 1 at 100 with its take-profit at 110 and its stop-loss at
/// `stop_loss`.
pub fn bracket(symbol: &str, stop_loss: f64) -> CommandWire {
    CommandWire::Bracket(OrderBracketWire {
        order_client_id: None,
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss,
        take_profit: 110.0,
        account: None,
    })
}

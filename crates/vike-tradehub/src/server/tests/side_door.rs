//! **An order whose side is neither buy nor sell is refused by name at the door.** A raw frame carries any integer;
//! the node's own client surfaces cannot mint one, which is why a raw frame is the test. `RiskGate`'s `invalid-side`
//! denial further in is left as it is.

use super::*;
use crate::server::refusal::submit_side_refusal;
use vike_tradehub_client::wire::OrderBracketWire;

fn sided(side: i32) -> CommandWire {
    let CommandWire::Submit(mut req) = submit("hyperliquid", "BTC", None) else { unreachable!() };
    req.side = side;
    CommandWire::Submit(req)
}

#[test]
fn a_raw_order_with_no_real_side_is_refused_by_name_and_sends_nothing() {
    let mut node = Node::start_with("hyperliquid", "BTC", AccountTier::Demo, &[]);
    for side in [0, 2, -2, i32::MAX, i32::MIN] {
        let why = node.send_rostered(sided(side), None).expect_err("no such side");
        assert!(why.contains("`side` must be 1 (buy) or -1 (sell)"), "{why}");
        assert!(why.contains(&format!("got {side}")) && why.contains("REFUSED"), "{why}");
    }
    assert!(node.submitted.lock().unwrap().is_empty(), "a refusal sends the venue nothing");
    node.core.shutdown_and_join();
}

/// The bracket's own check already stood at this door: pinned beside the order's, so the two stay a pair.
#[test]
fn a_raw_bracket_with_no_real_side_is_refused_by_name_and_sends_nothing() {
    let mut node = Node::start_with("hyperliquid", "BTC", AccountTier::Demo, &[]);
    let bracket = CommandWire::Bracket(OrderBracketWire {
        order_client_id: None,
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        side: 0,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
        account: None,
    });
    let why = node.send_rostered(bracket, None).expect_err("no such side");
    assert!(why.contains("`side` must be 1") && why.contains("got 0"), "{why}");
    assert!(node.submitted.lock().unwrap().is_empty(), "a refusal sends the venue nothing");
    node.core.shutdown_and_join();
}

#[test]
fn a_buy_and_a_sell_are_not_this_gates_and_nor_is_any_other_verb() {
    assert_eq!(submit_side_refusal(&sided(1)), None);
    assert_eq!(submit_side_refusal(&sided(-1)), None);
    assert_eq!(submit_side_refusal(&CommandWire::Cancel("c-1".into())), None);
}

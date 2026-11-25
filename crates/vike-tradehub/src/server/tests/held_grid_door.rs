//! The door and the Previews judge an order on a symbol the engine ALREADY trades against the grid this node
//! holds for it (`SymbolAdmitter::held_grid`), through the one `grid_refusal`: an opening order the venue's grid
//! refuses is refused by name, with no venue asked. A close is never blocked by a minimum, and a node that holds
//! no grid takes today's path. `Node` is a real core over a scripted venue client, as in the symbol door's suite.

use vike_execution::{OrderView, SymbolAdmission};
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_model::{AccountTier, ContractValue, SymbolGrid};
use vike_tradehub_client::wire::{CommandWire, OrderBracketWire};

use super::admits_any_symbol::{Node, limit, sell_to_close};
use crate::server::control::{ControlAcceptError, ControlAcceptReply, accept_command};
use crate::server::symbol_admission::{
    AccountReadiness, SymbolAdmitter, held_grid_refusal, rounding_note, submit_symbol_refusal,
};

const VENUE: &str = "deribit";
const SYMBOL: &str = "BTC-PERPETUAL";

/// A source that holds `0` for every symbol and must never be asked to look one up.
pub(super) struct Held(Option<SymbolGrid>);

impl SymbolAdmitter for Held {
    fn admission(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<SymbolAdmission, SymbolPropertiesRequestError> {
        panic!("judging a symbol the engine already trades must ask no venue")
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(None)
    }
    fn held_grid(&self, _: &str, _: &str) -> Option<SymbolGrid> {
        self.0.clone()
    }
}

impl AccountReadiness for Held {
    fn generation(&self, _: &str) -> u64 {
        0
    }
}

/// `BTC-PERPETUAL`'s grid: min = step = 10.
pub(super) fn ten() -> Held {
    Held(Some(SymbolGrid {
        tick_size: Some(0.5),
        lot_size: Some(10.0),
        min_qty: Some(10.0),
        ..SymbolGrid::default()
    }))
}

fn accept(
    node: &mut Node,
    cmd: CommandWire,
    facts: &Held,
) -> Result<ControlAcceptReply, ControlAcceptError> {
    accept_naming(node, cmd, &[], facts)
}

/// [`accept`] with the open orders the command names, as the door is handed them.
fn accept_naming(
    node: &mut Node,
    cmd: CommandWire,
    orders: &[OrderView],
    facts: &Held,
) -> Result<ControlAcceptReply, ControlAcceptError> {
    let blocks = node.blocks();
    let sink = node.core.command_sink();
    accept_command(
        cmd,
        None,
        &mut node.limits,
        &sink,
        None,
        &[],
        &[],
        &[],
        &blocks,
        orders,
        &[],
        &[],
        Some(facts),
        None,
        None,
    )
}

/// A resting limit on the engine's own symbol, as the door is handed it.
fn resting(order_client_id: &str, side: i32, qty: f64) -> OrderView {
    OrderView {
        order_client_id: order_client_id.to_string(),
        venue: VENUE.to_string(),
        account: None,
        symbol: SYMBOL.to_string(),
        side,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(50_000.0),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        data_delay: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    }
}

fn modify(order_client_id: &str, new_qty: Option<f64>, new_price: Option<f64>) -> CommandWire {
    CommandWire::Modify {
        order_client_id: order_client_id.to_string(),
        new_qty,
        new_price,
        new_trigger_price: None,
    }
}

/// A `Modify` is judged on the terms it would leave the order at, in the same words at the door and the Preview;
/// one left on the grid, one that names no open order and a price-only one of a whole lot pass.
#[test]
fn a_modify_under_the_minimum_is_refused_by_name_at_the_door() {
    let mut node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let facts = ten();
    let orders = [resting("m-1", 1, 20.0)];
    let blocks = node.blocks();

    let small = modify("m-1", Some(1.0), None);
    let said =
        held_grid_refusal(&small, None, &orders, &blocks, Some(&facts)).expect("refused").text;
    assert!(said.starts_with("modify: `BTC-PERPETUAL`: the quantity 1 is below"), "{said}");
    let refused = accept_naming(&mut node, small, &orders, &facts).expect_err("the door");
    assert_eq!(refused.message(), said);

    for fine in [
        modify("m-1", Some(10.0), None),
        modify("m-1", None, Some(51_000.0)),
        modify("not-resting", Some(1.0), None),
    ] {
        assert_eq!(held_grid_refusal(&fine, None, &orders, &blocks, Some(&facts)), None);
    }
}

/// ⚠ The published order carries no reduce-only flag: an order on the side that CLOSES the net position counts as
/// one, so shrinking it under the minimum passes. The other side adds risk and is judged.
#[test]
fn a_modify_of_an_order_that_closes_the_position_is_never_refused_here() {
    let node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let mut blocks = node.blocks();
    blocks[0].positions.push(vike_execution::PositionView {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        position_side: vike_model::events::PositionSide::Both,
        size: 1.0,
        average_price: 50_000.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liquidation_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    });
    let small = modify("m-2", Some(1.0), None);

    let closing = [resting("m-2", -1, 20.0)];
    assert_eq!(held_grid_refusal(&small, None, &closing, &blocks, Some(&ten())), None);

    let adding = [resting("m-2", 1, 20.0)];
    assert!(held_grid_refusal(&small, None, &adding, &blocks, Some(&ten())).is_some());
}

/// A rounded `Modify` is told as a rounded order is, at the door's answer too.
#[test]
fn a_rounded_modify_is_told() {
    let mut node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let orders = [resting("m-3", 1, 20.0)];
    let moved = modify("m-3", Some(15.0), None);
    let said = "qty 15 goes out as 20, the venue's step is 10".to_string();
    let note = rounding_note(&moved, None, &orders, &node.blocks(), Some(&ten()));
    assert_eq!(note.as_ref(), Some(&said));
    assert_eq!(
        accept_naming(&mut node, moved, &orders, &ten()).expect("sent"),
        ControlAcceptReply::OrderClientId {
            order_client_id: "m-3".into(),
            notes: vec![said],
            emulated: None
        }
    );
}

/// THE CASE: qty 1 on `BTC-PERPETUAL` (min 10) is refused by name, in the same words at the Preview and at the
/// door, and nothing reaches the venue client. A whole lot passes both and is sent.
#[test]
fn an_order_under_the_minimum_on_the_engines_own_symbol_is_refused_by_name_at_both() {
    let mut node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let facts = ten();
    let cmd = limit("under-1", VENUE, SYMBOL, 1.0, 50_000.0);

    let said =
        submit_symbol_refusal(&cmd, &node.blocks(), Some(&facts)).expect("the preview says").text;
    assert!(said.contains(SYMBOL) && said.contains("below the symbol's minimum 10"), "{said}");
    let refused = accept(&mut node, cmd, &facts).expect_err("the door refuses").message();
    assert_eq!(refused, said, "a preview and a command answer one frame alike");

    let whole = limit("whole-1", VENUE, SYMBOL, 10.0, 50_000.0);
    assert_eq!(submit_symbol_refusal(&whole, &node.blocks(), Some(&facts)), None);
    accept(&mut node, whole, &facts).expect("a whole lot passes");
    assert_eq!(node.venue_saw("whole-1").symbol, SYMBOL, "and is sent");
}

/// ⚠ A close is never blocked by a minimum: a reduce-only order under the floor passes the Preview and the door.
#[test]
fn a_reduce_only_order_under_the_minimum_is_never_refused_here() {
    let mut node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let facts = ten();
    let close = sell_to_close("close-1", VENUE, SYMBOL, 1.0, 50_000.0);
    assert_eq!(submit_symbol_refusal(&close, &node.blocks(), Some(&facts)), None);
    accept(&mut node, close, &facts).expect("a close passes the door");
}

/// The other closing verbs carry no size this judges.
#[test]
fn flatten_and_market_exit_are_not_judged() {
    let node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let facts = ten();
    let flatten =
        CommandWire::Flatten { venue: VENUE.into(), symbol: SYMBOL.into(), account: None };
    let exit = CommandWire::MarketExit { venue: Some(VENUE.into()), account: None };
    for verb in [flatten, exit] {
        assert_eq!(held_grid_refusal(&verb, None, &[], &node.blocks(), Some(&facts)), None);
    }
}

/// No grid held, and no facts source at all: today's path, nothing refused and nothing asked.
#[test]
fn a_node_that_holds_no_grid_refuses_nothing() {
    let mut node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let nothing = Held(None);
    let cmd = limit("under-2", VENUE, SYMBOL, 1.0, 50_000.0);
    assert_eq!(submit_symbol_refusal(&cmd, &node.blocks(), Some(&nothing)), None);
    assert_eq!(held_grid_refusal(&cmd, None, &[], &node.blocks(), None), None);
    accept(&mut node, cmd, &nothing).expect("today's path");
}

/// A bracket opens with its entry, so the entry is judged as a plain order is.
#[test]
fn a_brackets_entry_is_judged_like_an_order() {
    let node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let bracket = CommandWire::Bracket(OrderBracketWire {
        order_client_id: None,
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(50_000.0),
        stop_loss: 49_000.0,
        take_profit: 51_000.0,
        account: None,
    });
    let why =
        held_grid_refusal(&bracket, None, &[], &node.blocks(), Some(&ten())).expect("refused").text;
    assert!(why.contains("below the symbol's minimum 10"), "{why}");
}

/// Silent rounding is told, half to even as the engine's gate rounds, and an order on the grid says nothing. The
/// step's decimals print the grid multiple the wire owes, never an f64's noise.
#[test]
fn the_note_says_what_the_order_goes_out_as() {
    let note = |qty: f64, step: f64| {
        let facts = Held(Some(SymbolGrid { lot_size: Some(step), ..SymbolGrid::default() }));
        rounding_note(&limit("n", VENUE, SYMBOL, qty, 50_000.0), None, &[], &[], Some(&facts))
    };
    let moved = |qty, sent: &str, step| {
        Some(format!("qty {qty} goes out as {sent}, the venue's step is {step}"))
    };
    assert_eq!(note(14.0, 10.0), moved(14.0, "10", 10.0));
    assert_eq!(note(15.0, 10.0), moved(15.0, "20", 10.0));
    assert_eq!(note(25.0, 10.0), moved(25.0, "20", 10.0), "half to even, as the gate rounds");
    assert_eq!(note(0.34, 0.1), moved(0.34, "0.3", 0.1));
    assert_eq!(note(10.0, 10.0), None);
    assert_eq!(note(0.3, 0.1), None, "on the grid, whatever `3 · 0.1` is in an f64");
    let cancel = CommandWire::Cancel("c".into());
    assert_eq!(rounding_note(&cancel, None, &[], &[], Some(&ten())), None);
    let cmd = limit("n", VENUE, SYMBOL, 15.0, 50_000.0);
    assert_eq!(
        rounding_note(&cmd, None, &[], &[], Some(&Held(None))),
        None,
        "no grid, nothing said"
    );
}

/// A close keeps the rounding too (the note is no refusal), and one the lot rounds away is told that it dies.
#[test]
fn a_reduce_only_order_is_told_and_never_refused() {
    let moved = sell_to_close("n", VENUE, SYMBOL, 14.0, 50_000.0);
    assert_eq!(
        rounding_note(&moved, None, &[], &[], Some(&ten())).as_deref(),
        Some("qty 14 goes out as 10, the venue's step is 10")
    );
    let dust = sell_to_close("n", VENUE, SYMBOL, 1.0, 50_000.0);
    assert_eq!(
        rounding_note(&dust, None, &[], &[], Some(&ten())).as_deref(),
        Some("qty 1 rounds to nothing on the venue's step 10: the engine will deny it")
    );
}

/// **The door refuses only what the gate would deny, and the gate judges the ROUNDED quantity** (`RiskGate::check_inner`'s
/// `round_to`, half to even): 6 on a lot of 10 goes out as 10 and is sent, and the sender is told. 1 rounds to nothing,
/// and 5 too (half to even), so the gate would deny them `non-positive-size`.
#[test]
fn the_grid_is_judged_on_the_quantity_the_gate_would_send() {
    let mut node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let facts = ten();
    let blocks = node.blocks();
    let order = |qty| limit("r-1", VENUE, SYMBOL, qty, 50_000.0);
    for qty in [6.0, 9.0, 15.0] {
        assert_eq!(held_grid_refusal(&order(qty), None, &[], &blocks, Some(&facts)), None, "{qty}");
    }
    for qty in [1.0, 4.0, 5.0] {
        let why =
            held_grid_refusal(&order(qty), None, &[], &blocks, Some(&facts)).expect("refused").text;
        assert!(why.contains("below the symbol's minimum 10"), "{qty}: {why}");
    }
    assert_eq!(
        rounding_note(&order(6.0), None, &[], &blocks, Some(&facts)).as_deref(),
        Some("qty 6 goes out as 10, the venue's step is 10")
    );
    assert_eq!(
        accept(&mut node, limit("r-6", VENUE, SYMBOL, 6.0, 50_000.0), &facts).expect("sent"),
        ControlAcceptReply::OrderClientId {
            order_client_id: "r-6".into(),
            notes: vec!["qty 6 goes out as 10, the venue's step is 10".into()],
            emulated: None,
        }
    );
    node.venue_saw("r-6");

    // No minimum: a whole lot under half still rounds to nothing.
    let lot = Held(Some(SymbolGrid { lot_size: Some(10.0), ..SymbolGrid::default() }));
    let why = held_grid_refusal(&order(4.0), None, &[], &blocks, Some(&lot)).expect("refused").text;
    assert!(why.contains("below one lot (10)"), "{why}");
    assert_eq!(held_grid_refusal(&order(6.0), None, &[], &blocks, Some(&lot)), None);

    // A minimum above the lot: 9.6 on a lot of 1 goes out as 10, over a minimum of 10.
    let min = Held(Some(SymbolGrid {
        lot_size: Some(1.0),
        min_qty: Some(10.0),
        ..SymbolGrid::default()
    }));
    assert_eq!(held_grid_refusal(&order(9.6), None, &[], &blocks, Some(&min)), None);
    assert!(held_grid_refusal(&order(9.4), None, &[], &blocks, Some(&min)).is_some());

    // The money floor reads the rounded order too: 6 goes out as 10, worth 50 at 5, over a floor of 40.
    let floor = Held(Some(SymbolGrid {
        lot_size: Some(10.0),
        min_notional: Some(40.0),
        ..SymbolGrid::default()
    }));
    let low = limit("r-2", VENUE, SYMBOL, 6.0, 5.0);
    assert_eq!(held_grid_refusal(&low, None, &[], &blocks, Some(&floor)), None);
    node.core.shutdown_and_join();
}

/// **Long 10, a plain sell of 1 under a minimum of 10: a close the core exempts from its floors** (`covered_reduce`, the
/// flag or none), so the door has no word on it. A buy adds and is judged.
#[test]
fn a_close_with_no_flag_is_not_refused_by_a_minimum() {
    let node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let mut blocks = node.blocks();
    blocks[0].positions.push(vike_execution::PositionView {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        position_side: vike_model::events::PositionSide::Both,
        size: 10.0,
        average_price: 50_000.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liquidation_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    });
    let side = |cmd, side| {
        let CommandWire::Submit(mut req) = cmd else { unreachable!() };
        req.side = side;
        CommandWire::Submit(req)
    };
    let close = side(limit("c-1", VENUE, SYMBOL, 1.0, 50_000.0), -1);
    assert_eq!(held_grid_refusal(&close, None, &[], &blocks, Some(&ten())), None);
    let open = side(limit("c-2", VENUE, SYMBOL, 1.0, 50_000.0), 1);
    assert!(held_grid_refusal(&open, None, &[], &blocks, Some(&ten())).is_some());
    node.core.shutdown_and_join();
}

/// The door's own answer carries the sentence, so a sender that never previewed is told as well.
#[test]
fn the_doors_answer_carries_the_note() {
    let mut node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let answer = accept(&mut node, limit("n-15", VENUE, SYMBOL, 15.0, 50_000.0), &ten());
    assert_eq!(
        answer.expect("a rounded order is sent"),
        ControlAcceptReply::OrderClientId {
            order_client_id: "n-15".into(),
            notes: vec!["qty 15 goes out as 20, the venue's step is 10".into()],
            emulated: None,
        }
    );
}

/// The money floor is judged at `qty · price` only where that IS the money: on a linear contract an order under it
/// is refused, and on an inverse one (the quantity is dollars) the engine, which values it, keeps the judgement.
#[test]
fn the_money_floor_is_left_to_the_engine_on_a_contract_not_valued_at_quantity_times_price() {
    let node = Node::start(VENUE, SYMBOL, AccountTier::Demo);
    let floor = Held(Some(SymbolGrid { min_notional: Some(100.0), ..SymbolGrid::default() }));
    let cmd = limit("floor-1", VENUE, SYMBOL, 10.0, 5.0);

    let mut blocks = node.blocks();
    let why =
        held_grid_refusal(&cmd, None, &[], &blocks, Some(&floor)).expect("10 x 5 under 100").text;
    assert!(why.contains("minimum notional 100"), "{why}");

    blocks[0].contract_value_default = ContractValue::inverse(1.0);
    assert_eq!(held_grid_refusal(&cmd, None, &[], &blocks, Some(&floor)), None);
}

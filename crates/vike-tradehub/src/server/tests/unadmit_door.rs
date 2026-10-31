//! The give-back of an admitted symbol at the control door (`WireCommand::UnadmitSymbol`, decision 0205).
//!
//! Two layers, as the admission's own suite has. The PLAN ([`plan_unadmit`]) is a pure function of the
//! published block and orders, so each rule has a row: only a symbol the engine ADMITTED is a slot, it must be
//! flat with no live order (a conditional or a held exit is one), the frame carries the block's spelling, and a
//! node with no block to judge by lets the frame go to the core. The END-TO-END test drives `accept_command` into
//! a REAL core: the statement reaches the engine, which lets the symbol go, and the facts source is told the
//! request (it forgets what it kept when the published block shows the engine let go).

use super::*;
use crate::server::unadmit::{UnadmitPlan, plan_unadmit};
use vike_exec::{Command, OrderView, PositionView, VenueBlock};
use vike_model::OrderStatus;
use vike_model::accounts::account_keys::AccountLabel;

fn unadmit(venue: &str, account: &str, symbol: &str) -> WireCommand {
    WireCommand::UnadmitSymbol {
        venue: venue.into(),
        account: account.into(),
        symbol: symbol.into(),
    }
}

fn block(
    route_key: &str,
    mounted: &str,
    extra: &[&str],
    positions: Vec<PositionView>,
) -> VenueBlock {
    VenueBlock {
        venue: route_key.split('#').next().unwrap().into(),
        route_key: route_key.into(),
        symbol: mounted.into(),
        extra_symbols: extra.iter().map(|s| s.to_string()).collect::<Vec<_>>().into(),
        positions,
        ..Default::default()
    }
}

fn position(symbol: &str, size: f64) -> PositionView {
    PositionView {
        venue: "binance".into(),
        symbol: symbol.into(),
        position_side: vike_model::events::PositionSide::Both,
        size,
        avg_px: 100.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liq_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }
}

fn order(account: Option<&str>, symbol: &str, status: OrderStatus) -> OrderView {
    OrderView {
        data_delay: None,
        client_order_id: "o-1".to_string(),
        venue: "binance".to_string(),
        account: account.map(|a| AccountLabel::parse(a).expect("a legal label")),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
        pending_price: None,
    }
}

fn refusal(plan: UnadmitPlan) -> String {
    match plan {
        UnadmitPlan::Refuse(why) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn every_other_verb_passes_the_gate_untouched() {
    let blocks = [block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![])];
    assert_eq!(plan_unadmit(&WireCommand::Cancel("c".into()), &blocks, &[]), UnadmitPlan::Pass);
    assert_eq!(
        plan_unadmit(&submit("binance", "ETHUSDT.P", None), &blocks, &[]),
        UnadmitPlan::Pass
    );
}

#[test]
fn a_quiet_admitted_symbol_is_released_in_the_engines_own_spelling() {
    let blocks = [block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![])];
    // Typed in any case, sent in the list's.
    assert_eq!(
        plan_unadmit(&unadmit("binance", "DEFAULT", "ethusdt.p"), &blocks, &[]),
        UnadmitPlan::Release { route_key: "binance".into(), symbol: "ETHUSDT.P".into() }
    );
    // A labelled account is its own book: its key, its block.
    let two = [
        block("binance", "BTCUSDT", &[], vec![]),
        block("binance#ALT", "BTCUSDT", &["SOLUSDT"], vec![]),
    ];
    assert_eq!(
        plan_unadmit(&unadmit("binance", "ALT", " SOLUSDT "), &two, &[]),
        UnadmitPlan::Release { route_key: "binance#ALT".into(), symbol: "SOLUSDT".into() }
    );
    // A flat position entry (the size a closed position leaves) and ended orders hold nothing.
    let flat = [block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![position("ETHUSDT.P", 0.0)])];
    let ended = [order(None, "ETHUSDT.P", OrderStatus::Canceled)];
    assert!(matches!(
        plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &flat, &ended),
        UnadmitPlan::Release { .. }
    ));
}

#[test]
fn the_engines_own_symbol_and_an_unlisted_one_are_refused_in_words() {
    let blocks = [block("binance", "BTCUSDT", &["ETHUSDT.P", "SOLUSDT"], vec![])];
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "btcusdt"), &blocks, &[]));
    assert!(why.contains("started on") && why.contains("no slot"), "{why}");
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "XRPUSDT"), &blocks, &[]));
    assert!(
        why.contains("XRPUSDT")
            && why.contains("ETHUSDT.P, SOLUSDT")
            && why.contains("nothing to give back"),
        "it names what it did admit: {why}"
    );
    let none = [block("binance", "BTCUSDT", &[], vec![])];
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT"), &none, &[]));
    assert!(why.contains("admitted none"), "{why}");
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "  "), &blocks, &[]));
    assert!(why.contains("empty symbol"), "{why}");
}

#[test]
fn a_position_or_a_live_order_in_the_symbol_refuses_it_naming_the_cause() {
    let held = [block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![position("ETHUSDT.P", -2.5)])];
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &held, &[]));
    assert!(why.contains("position of -2.5") && why.contains("close it first"), "{why}");

    let quiet = [block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![])];
    // Resting, pre-ack, and a conditional held by the core: each is a live order in the symbol.
    for status in [OrderStatus::Accepted, OrderStatus::Initialized, OrderStatus::Emulated] {
        let orders = [order(None, "ETHUSDT.P", status)];
        let why =
            refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &quiet, &orders));
        assert!(why.contains("live order") && why.contains("o-1"), "{status:?}: {why}");
    }
    // An order of ANOTHER account of the exchange, or in another symbol, holds nothing of this one.
    let others = [
        order(Some("ALT"), "ETHUSDT.P", OrderStatus::Accepted),
        order(None, "SOLUSDT", OrderStatus::Accepted),
    ];
    assert!(matches!(
        plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &quiet, &others),
        UnadmitPlan::Release { .. }
    ));
}

#[test]
fn a_node_with_nothing_to_judge_by_lets_the_core_answer_and_the_newest_block_speaks() {
    assert_eq!(
        plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &[], &[]),
        UnadmitPlan::Pass
    );
    let elsewhere = [block("bybit", "BTCUSDT", &["ETHUSDT"], vec![])];
    assert_eq!(
        plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &elsewhere, &[]),
        UnadmitPlan::Pass,
        "no block for the key: the account gate's business, not this one's"
    );
    // After a restart of the account, the stopped engine's block sits before the new one.
    let restarted = [
        block("binance", "BTCUSDT", &["ETHUSDT.P"], vec![]),
        block("binance", "BTCUSDT", &[], vec![]),
    ];
    let why = refusal(plan_unadmit(&unadmit("binance", "DEFAULT", "ETHUSDT.P"), &restarted, &[]));
    assert!(why.contains("not one of the symbols"), "{why}");
    let why = refusal(plan_unadmit(&unadmit("binance", "no good", "ETHUSDT.P"), &restarted, &[]));
    assert!(why.contains("`account`"), "{why}");
}

fn admission_of(route_key: &str, symbol: &str) -> SymbolAdmission {
    SymbolAdmission {
        route_key: route_key.into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.001), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: Some(3_000.0),
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
        release: false,
        margin_mode: None,
    }
}

/// **End to end**: the verb through `accept_command` into a real core. The engine admitted ETH, the verb is
/// accepted for the lower-case spelling, the facts source is told the request, the engine lets the symbol go
/// (the published block stops listing it), and the same verb again is refused in words because the block no
/// longer lists it.
#[test]
fn the_verb_gives_an_admitted_symbol_back_through_a_real_core() {
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let facts = Table::eth();
    node.core.send_command(Command::AdmitSymbol(Box::new(admission_of("bybit", "ETHUSDT"))));
    node.blocks_once_trading("bybit", "ETHUSDT");

    let why = node
        .send_rostered(unadmit("bybit", "DEFAULT", "SOLUSDT"), Some(&facts))
        .expect_err("a symbol it never admitted");
    assert!(why.contains("not one of the symbols") && why.contains("ETHUSDT"), "{why}");
    assert!(
        !facts.events().iter().any(|e| e.starts_with("release_requested")),
        "a refused frame asks nothing of the source"
    );

    node.send_rostered(unadmit("bybit", "DEFAULT", "ethusdt"), Some(&facts))
        .expect("an admitted, quiet symbol is given back");
    assert!(
        facts.events().iter().any(|e| e == "release_requested:ETHUSDT"),
        "the source is told, in the engine's spelling: {:?}",
        facts.events()
    );
    wait_until("the engine to let the symbol go", || {
        node.blocks().iter().all(|b| !b.trades("ETHUSDT"))
    });

    let why = node
        .send_rostered(unadmit("bybit", "DEFAULT", "ETHUSDT"), Some(&facts))
        .expect_err("it is no longer admitted");
    assert!(why.contains("not one of the symbols") && why.contains("admitted none"), "{why}");
    // An account the node does not run is the account gate's refusal, before this one.
    let why = node
        .send_rostered(unadmit("bybit", "NOSUCH", "ETHUSDT"), Some(&facts))
        .expect_err("no such account");
    assert!(why.contains("NOSUCH"), "{why}");
    node.core.shutdown_and_join();
}

/// A symbol the engine holds a position in is refused AT THE DOOR once the published block shows it, and the
/// symbol stays: the give-back of a held symbol is not a way to lose a position's pricing.
#[test]
fn a_symbol_with_a_position_is_refused_at_the_door_and_stays_admitted() {
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let facts = Table::eth();
    node.send(limit("open", "bybit", "ETHUSDT", 0.01, 3_000.0), Some(&facts)).expect("admitted");
    node.venue_saw("open");
    node.blocks_once_trading("bybit", "ETHUSDT");
    wait_until("the position to publish", || {
        node.blocks()
            .iter()
            .any(|b| b.positions.iter().any(|p| p.symbol.as_str() == "ETHUSDT" && p.size > 0.0))
    });
    let why = node
        .send_rostered(unadmit("bybit", "DEFAULT", "ETHUSDT"), Some(&facts))
        .expect_err("it holds a position");
    assert!(why.contains("holds a position") && why.contains("close it first"), "{why}");
    assert!(node.blocks().iter().any(|b| b.trades("ETHUSDT")), "the symbol stays admitted");
    node.core.shutdown_and_join();
}

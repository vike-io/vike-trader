//! **A node that is not ACTIVE admits no symbol** (review r2, item 9).
//!
//! Admitting a symbol commits to the gatewayhub, which on a venue that posts a leverage (bybit, okx, binance)
//! makes the exchange hold the account's figure for the symbol: a WRITE on the venue account. The HALT
//! sentinel and the kill switch stopped the order, not that write. The door now refuses an OPENING order whose
//! admission would commit while the addressed engine is `Halted` or `Reducing`, before anything is sent, and
//! lets a reducing order through without the admission. `Active` is what it always was.

use super::*;
use vike_model::TradingState;

/// Put the whole node in `state` and wait for every published block to say so.
fn put_in(node: &mut Node, state: TradingState) {
    node.send(CommandWire::SetTradingState(state), None).expect("the state change is accepted");
    wait_until("the state to publish", || node.blocks().iter().all(|b| b.trading_state == state));
}

fn admissions(facts: &Table, symbol: &str) -> usize {
    facts.events().iter().filter(|e| **e == format!("admitted:{symbol}")).count()
}

/// Halted and Reducing refuse the opening order that would admit a symbol, in words, with no commit and no
/// order at the venue, on every venue that posts a leverage and at both real tiers; Active admits it as ever.
#[test]
fn a_halted_or_reducing_node_commits_no_symbol_to_the_exchange() {
    let cases = [
        ("bybit", "BTCUSDT", "ETHUSDT", AccountTier::Demo),
        ("binance", "BTCUSDT", "ETHUSDT", AccountTier::Demo),
        ("okx", "BTCUSDT.P", "ETHUSDT.P", AccountTier::Demo),
        ("bybit", "BTCUSDT", "ETHUSDT", AccountTier::Live),
    ];
    for (venue, mounted, symbol, tier) in cases {
        let what = format!("{venue} {tier:?}");
        let mut node = Node::start(venue, mounted, tier);
        let facts = Table::eth();
        let order = |order_client_id: &str| limit(order_client_id, venue, symbol, 0.01, 3_000.0);

        for (state, word) in
            [(TradingState::Halted, "HALTED"), (TradingState::Reducing, "REDUCING")]
        {
            put_in(&mut node, state);
            let why = node.send(order("held"), Some(&facts)).expect_err("not active: refused");
            for want in [word, "REFUSED", "nothing was sent", symbol] {
                assert!(why.contains(want), "{what} {word}: {want:?} missing: {why}");
            }
            assert_eq!(admissions(&facts, symbol), 0, "{what} {word}: no commit to the exchange");
            assert!(
                node.submitted.lock().unwrap().is_empty(),
                "{what} {word}: nothing at the venue"
            );
        }

        // Active again: the very same order is admitted and sent, as it always was.
        put_in(&mut node, TradingState::Active);
        node.send(order("go"), Some(&facts)).expect("active: unchanged");
        node.venue_saw("go");
        assert_eq!(
            admissions(&facts, symbol),
            1,
            "{what}: the commit runs when the node is active"
        );
        node.core.shutdown_and_join();
    }
}

/// A REDUCING order is never held up by the gate: with a stale admission to refresh it goes WITHOUT the
/// admission (no commit), so a close works on a node that is Halted-adjacent. Under `Active` the refresh rides it.
#[test]
fn a_reducing_order_goes_without_its_admission_while_the_node_is_not_active() {
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let facts = Table::eth();
    node.send(limit("open", "bybit", "ETHUSDT", 0.01, 3_000.0), Some(&facts)).expect("admitted");
    node.venue_saw("open");
    node.blocks_once_trading("bybit", "ETHUSDT");
    assert_eq!(admissions(&facts, "ETHUSDT"), 1);

    let close = |order_client_id: &str| {
        let CommandWire::Submit(mut req) =
            limit(order_client_id, "bybit", "ETHUSDT", 0.01, 3_000.0)
        else {
            unreachable!()
        };
        (req.side, req.reduce_only) = (-1, true);
        CommandWire::Submit(req)
    };
    let stale = |facts: &Table| {
        let admission = facts.admission("bybit", "bybit", "ETHUSDT").expect("a row");
        *facts.refresh.lock().unwrap() = Some(admission);
    };

    put_in(&mut node, TradingState::Reducing);
    stale(&facts);
    node.send(close("close-1"), Some(&facts)).expect("a close is never refused");
    node.venue_saw("close-1");
    assert_eq!(admissions(&facts, "ETHUSDT"), 1, "Reducing: the refresh was dropped, no commit");

    put_in(&mut node, TradingState::Active);
    stale(&facts);
    node.send(close("close-2"), Some(&facts)).expect("active");
    node.venue_saw("close-2");
    assert_eq!(admissions(&facts, "ETHUSDT"), 1, "Active: a close asks for no refresh either");
    node.core.shutdown_and_join();
}

/// **A close is one by what the position says**: a plain sell the book covers, with a stale admission to refresh, goes
/// without it on a node that is not active; the sentence about committing a leverage would have stopped a close.
#[test]
fn a_plain_close_goes_without_its_admission_while_the_node_is_not_active() {
    let mut node = Node::start("bybit", "BTCUSDT", AccountTier::Demo);
    let facts = Table::eth();
    node.send(limit("open", "bybit", "ETHUSDT", 0.01, 3_000.0), Some(&facts)).expect("admitted");
    node.venue_saw("open");
    let published = node.blocks_once_trading("bybit", "ETHUSDT");
    let long = |block: &vike_execution::VenueBlock| {
        let mut held = block.clone();
        held.trading_state = TradingState::Halted;
        held.positions.push(vike_execution::PositionView {
            venue: "bybit".into(),
            symbol: "ETHUSDT".into(),
            position_side: vike_model::events::PositionSide::Both,
            size: 1.0,
            average_price: 3_000.0,
            unrealized: 0.0,
            mark_source: None,
            leverage: 0.0,
            liquidation_price: 0.0,
            margin_mode: vike_model::MarginMode::Cross,
            isolated_margin: None,
        });
        held
    };
    let blocks = [long(&published[0])];
    let stale = || {
        *facts.refresh.lock().unwrap() =
            Some(facts.admission("bybit", "bybit", "ETHUSDT").expect("a row"));
    };
    let send = |node: &mut Node, side, order_client_id: &str| {
        let CommandWire::Submit(mut req) = limit(order_client_id, "bybit", "ETHUSDT", 0.5, 3_000.0)
        else {
            unreachable!()
        };
        req.side = side;
        let sink = node.core.command_sink();
        accept_command(
            CommandWire::Submit(req),
            None,
            &mut node.limits,
            &sink,
            None,
            &[],
            &[],
            &[],
            &blocks,
            &[],
            &[],
            &[],
            Some(&facts),
            None,
            None,
        )
    };
    stale();
    send(&mut node, -1, "plain-close").expect("a close is never refused");
    node.venue_saw("plain-close");
    assert_eq!(admissions(&facts, "ETHUSDT"), 1, "the refresh was dropped, no commit");
    stale();
    let why = send(&mut node, 1, "plain-open").expect_err("an open commits, and is refused");
    assert!(why.message().contains("HALTED"), "{}", why.message());
    node.core.shutdown_and_join();
}

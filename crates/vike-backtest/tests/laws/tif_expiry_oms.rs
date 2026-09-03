//! TIF expiry through the OMS, not just the paper book: a `Gtd` order whose deadline passes must
//! terminalize in `ExecutionEngine`'s `ManagedOrder` FSM as `OrderStatus::Expired` (not
//! `Canceled`), exactly ONCE, and must surface to a mounted strategy as
//! `OrderEventKind::Expired` — the documented seam for a GTD elapse, and the whole reason the
//! paper client emits `Event::OrderExpired` rather than a stringly-reasoned cancel.
//!
//! The unit tests in `paper.rs` only prove which EVENT is emitted; this proves the OMS accepts
//! that event on the transition the live venues use (`Accepted -> Expired`).

use vike_backtest::paper::PaperExecutionClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, OrderStatus, Outbox,
    RiskGate, RiskLimits,
};
use vike_model::events::Event;
use vike_model::strategy::OrderEventKind;
use vike_model::{Bar, OrderRequest, TimeInForce};

const VENUE: &str = "paper";
const SYMBOL: &str = "BTCUSDT";

fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 100.0,
        high: 101.0,
        low: 95.0, // never trades down to the 90 limit — the order rests until it expires
        close: 100.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn engine() -> ExecutionEngine<PaperExecutionClient> {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        PaperExecutionClient::new(VENUE, SYMBOL, 0.0, 0.0, 0.0),
        VENUE,
        SYMBOL,
    );
    // what the runtime sets iff a strategy is mounted — makes `order_events` observable here
    engine.collect_applied_fills = true;
    engine
}

/// Drain the client's queued venue events through the engine's fold (what the core runtime does).
fn pump(engine: &mut ExecutionEngine<PaperExecutionClient>, outbox: &mut Outbox) {
    while let Some(event) = engine.client.poll_events() {
        engine.on_event(&event, outbox);
    }
}

#[test]
fn a_gtd_order_terminalizes_as_expired_in_the_oms_exactly_once() {
    let mut engine = engine();
    let mut outbox = Outbox::default();

    let request = OrderRequest {
        client_order_id: "gtd-1".into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(90.0),
        time_in_force: TimeInForce::Gtd,
        gtd_expiry: Some(2_000),
        ts: 0,
        ..Default::default()
    };
    engine.submit_order(&request, 0, &mut outbox);
    pump(&mut engine, &mut outbox);
    assert_eq!(
        engine.registry["gtd-1"].status,
        OrderStatus::Accepted,
        "the paper book accepts and rests it"
    );

    // a bar before the deadline changes nothing
    engine.client.on_bar(&bar(1_000));
    pump(&mut engine, &mut outbox);
    assert_eq!(engine.registry["gtd-1"].status, OrderStatus::Accepted, "still live");

    // the deadline bar expires it — in the FSM, on the venue's own Expired transition
    engine.client.on_bar(&bar(2_000));
    pump(&mut engine, &mut outbox);
    assert_eq!(
        engine.registry["gtd-1"].status,
        OrderStatus::Expired,
        "terminalized as EXPIRED, not CANCELED — the live venues' status for a GTD elapse"
    );

    // the strategy-facing lifecycle seam saw it, exactly once
    let expiries: Vec<&String> = engine
        .order_events
        .iter()
        .filter(|o| o.event.kind == OrderEventKind::Expired)
        .map(|o| &o.event.client_order_id)
        .collect();
    assert_eq!(expiries, vec!["gtd-1"], "one OrderEventKind::Expired delivery");

    // later bars re-emit nothing at all: no second expiry, no dropped-terminal accounting
    let before = engine.order_events.len();
    for ts in [3_000, 4_000, 5_000] {
        engine.client.on_bar(&bar(ts));
        pump(&mut engine, &mut outbox);
    }
    assert_eq!(engine.order_events.len(), before, "expiry fires exactly once");
    assert_eq!(engine.dropped_terminal_on_live, 0, "no lost terminal");
    assert_eq!(engine.stranded_terminal_drops, 0, "nothing executed after the terminal");
}

/// The default TIF (`Gtc`) is what every in-tree producer builds — it must never expire, at any
/// horizon. A `Gtc` that expired would move the sacred parity fixtures.
#[test]
fn a_gtc_order_never_expires_in_the_oms() {
    let mut engine = engine();
    let mut outbox = Outbox::default();

    let request = OrderRequest {
        client_order_id: "gtc-1".into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(90.0),
        // time_in_force deliberately omitted: the Default is Gtc
        ts: 0,
        ..Default::default()
    };
    assert!(matches!(request.time_in_force, TimeInForce::Gtc), "the default must stay Gtc");
    engine.submit_order(&request, 0, &mut outbox);
    pump(&mut engine, &mut outbox);

    for day in 0..10 {
        engine.client.on_bar(&bar(day * 86_400_000));
        pump(&mut engine, &mut outbox);
    }
    assert_eq!(
        engine.registry["gtc-1"].status,
        OrderStatus::Accepted,
        "still resting and live after ten days"
    );
    assert!(
        !engine.order_events.iter().any(|o| o.event.kind == OrderEventKind::Expired),
        "no expiry event on the Gtc path"
    );
    // and the client emitted no expiry/cancel at all
    let mut saw_terminal = false;
    while let Some(event) = engine.client.poll_events() {
        saw_terminal |= matches!(event, Event::OrderExpired(_) | Event::OrderCanceled(_));
    }
    assert!(!saw_terminal, "the Gtc path is byte-identical: no expiry, no cancel");
}

//! A fill reaches the ENGINE's account, not just the order's FSM (review 18, R9 and its re-review, F1).
//!
//! `Engine::on_event` drops a bare `Event::Fill` in silence (no log, no counter) unless the fill's
//! `symbol` is the engine's mounted symbol (`accepts_symbol`) or the symbol of the order the fill names
//! (`owns_fill_symbol`: registry hit on the order client id AND `request.symbol == fill.symbol`). IB reports an
//! execution with the EXEC CONTRACT'S symbol/exchange/currency, which for a forex pair or a stock
//! executed on a named exchange is not the canonical the order was submitted under, so a bridge that stamps
//! that echo on the fill has its position effect dropped while the order's FSM advances (the same symptom
//! as the missing bare fill itself). These tests run the real exec loop over the fake transport INTO the real
//! engine's `on_event`, and read the account's position: the thing the unit tests on `Account::apply_fill`
//! could not see.
//!
//! Feature-gated on `test-support` like its siblings.
#![cfg(feature = "test-support")]

use std::time::{Duration, Instant};

use vike_bridge_core::execution_drive::drain;

use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, ExecutionClient, Ingest,
    RestoredOrder, RiskGate, event_channel,
};
use vike_ibkr::run_execution_for_test;
use vike_ibkr::testing::{FAKE_CLIENT_ID, IbkrTransportFake, ScriptedInbound};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus, RiskLimits};

/// Submit one market BUY of 10 through an engine mounted on `mounted` and fold the bridge's events into
/// it, the venue echoing `echoed` as the execution's instrument. Returns the engine (position and order
/// state are read off it).
fn trade(
    mounted: &str,
    echoed: &str,
) -> Engine<vike_bridge_core::exec_actor::ExecutionClientActor> {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let echoed = echoed.to_string();
    let mut fake = IbkrTransportFake::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(move |order_id| {
        vec![
            ScriptedInbound::OrderStatus {
                order_id,
                order_ref: "order-client-id-1".into(),
                status: "Submitted".into(),
            },
            ScriptedInbound::ExecutionOf {
                order_id,
                order_ref: "order-client-id-1".into(),
                exec_id: "e1".into(),
                symbol: echoed.clone(),
                shares: 10.0,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    let actor = run_execution_for_test(events, Box::new(fake));
    let mut engine = Engine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        actor,
        "ibkr",
        mounted,
    );
    let order = OrderRequest {
        order_client_id: "order-client-id-1".into(),
        venue: "ibkr".into(),
        symbol: mounted.into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    let mut outbox = EventBusOutbox::default();
    engine.submit_order(&order, 1, &mut outbox);
    // OrderSubmitted, OrderAccepted, the bare Fill, OrderFilled.
    let got = drain(|| rx.try_recv().ok(), 4, Duration::from_secs(3));
    assert_eq!(got.len(), 4, "the bridge emits four events: {got:?}");
    for event in &got {
        engine.on_event(event, &mut outbox);
    }
    engine.client.detach();
    engine
}

/// A stock executed on a named exchange: the exec contract echoes that exchange (`AAPL.IBEOS.USD` for an
/// overnight execution, measured), the order and the engine are the user name `AAPL` (decision 0290). The position
/// must move.
#[test]
fn a_stock_fill_echoed_with_another_exchange_still_books_into_the_engine() {
    let engine = trade("AAPL", "AAPL.IBEOS.USD");
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 10.0);
    assert_eq!(engine.registry["order-client-id-1"].status, OrderStatus::Filled);
}

/// Forex: the order and the engine are the user name `USDJPY`, its kept row's wire `USD.JPY.IDEALPRO`, and the exec
/// contract reconstructs `USD.IDEALPRO.JPY` (symbol.exchange.currency): the fill comes back as `USDJPY` and books.
#[test]
fn a_forex_fill_echoed_in_the_reconstructed_order_books_into_the_engine() {
    let engine = trade("USDJPY", "USD.IDEALPRO.JPY");
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 10.0);
    assert_eq!(engine.registry["order-client-id-1"].status, OrderStatus::Filled);
}

/// The control: an echo of the very contract the order went out on (`AAPL.SMART.USD`) worked before and must keep
/// working.
#[test]
fn a_fill_echoed_as_submitted_books_into_the_engine() {
    let engine = trade("AAPL", "AAPL.SMART.USD");
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 10.0);
}

/// **A commission IB REVISES after the fill reaches the engine's fee ledger once, by the difference** (decision 0238).
/// The fill joins its first commission (1.00 USD) and books it; IB then states 1.25 (and re-sends it): the bridge
/// emits ONE `FeeAdjustment`, and the real engine's `on_event` books the 0.25 into `fees_paid`, `balance` and the
/// per-asset tally. A bare second `Fill` could not have: the account's fill ledger refuses it as a duplicate.
#[test]
fn a_revised_commission_books_the_difference_into_the_engines_fee_ledger_once() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(|order_id| {
        vec![
            ScriptedInbound::OrderStatus {
                order_id,
                order_ref: "order-client-id-1".into(),
                status: "Submitted".into(),
            },
            ScriptedInbound::ExecutionOf {
                order_id,
                order_ref: "order-client-id-1".into(),
                exec_id: "e1".into(),
                symbol: "AAPL.SMART.USD".into(),
                shares: 10.0,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.25 },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.25 },
        ]
    });
    let actor = run_execution_for_test(events, Box::new(fake));
    let mut engine = Engine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        actor,
        "ibkr",
        "AAPL",
    );
    let order = OrderRequest {
        order_client_id: "order-client-id-1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    let mut outbox = EventBusOutbox::default();
    engine.submit_order(&order, 1, &mut outbox);
    // OrderSubmitted, OrderAccepted, the bare Fill, OrderFilled, and the ONE fee adjustment.
    let got = drain(|| rx.try_recv().ok(), 5, Duration::from_secs(3));
    assert_eq!(got.len(), 5, "the bridge emits five events: {got:?}");
    assert!(matches!(got[4], Event::FeeAdjustment(_)), "the last is the adjustment: {got:?}");
    for event in &got {
        engine.on_event(event, &mut outbox);
    }
    engine.client.detach();
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 10.0);
    assert!((engine.account.fees_paid - 1.25).abs() < 1e-12, "{}", engine.account.fees_paid);
    assert!((engine.account.balance + 1.25).abs() < 1e-12, "{}", engine.account.balance);
    let usd = engine
        .account
        .fees_by_asset
        .get(&vike_marketdata::Symbol::new("USD"))
        .copied()
        .unwrap_or(0.0);
    assert!((usd - 1.25).abs() < 1e-12, "{usd}");
}

/// Fold everything the bridge sends into `engine` until it announces the link up (the learning is over).
fn fold_until_link_up<C: ExecutionClient>(
    engine: &mut Engine<C>,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut outbox = EventBusOutbox::default();
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(event)) => {
                engine.on_event(&event, &mut outbox);
            }
            Ok(Ingest::TradeLink(u)) if u.up => return,
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("the bridge never announced the link up");
}

/// **F4.** A rebuilt connection replays the day's executions at EVERY connect, and the ones of an order that is
/// finished are recovered as partials. The recovery used to synthesize an `OrderAccepted` for the order first;
/// the engine's FSM refuses a liveness event on an order it already canceled, and counts it as a possible
/// stranded position (`events_dropped_order_already_closed`, plus an error line) at every replay. The replayed fill itself
/// is the engine's to dedup (it already folded `e1`): the position stays put.
#[test]
fn replaying_the_executions_of_a_canceled_order_strands_nothing_in_the_engine() {
    vike_log::test_init();
    // Connection 1: the order fills 4 of 10, then is canceled.
    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(|order_id| {
        vec![
            ScriptedInbound::OrderStatus {
                order_id,
                order_ref: "order-client-id-1".into(),
                status: "Submitted".into(),
            },
            ScriptedInbound::ExecutionOf {
                order_id,
                order_ref: "order-client-id-1".into(),
                exec_id: "e1".into(),
                symbol: "AAPL.SMART.USD".into(),
                shares: 4.0,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
            ScriptedInbound::OrderStatus {
                order_id,
                order_ref: "order-client-id-1".into(),
                status: "Cancelled".into(),
            },
        ]
    });
    let actor = run_execution_for_test(events, Box::new(fake));
    let mut engine = Engine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        actor,
        "ibkr",
        "AAPL",
    );
    let order = OrderRequest {
        order_client_id: "order-client-id-1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    let mut outbox = EventBusOutbox::default();
    engine.submit_order(&order, 1, &mut outbox);
    // Submitted, Accepted, Fill, PartiallyFilled, Canceled.
    let got = drain(|| rx.try_recv().ok(), 5, Duration::from_secs(3));
    assert_eq!(got.len(), 5, "{got:?}");
    for event in &got {
        engine.on_event(event, &mut outbox);
    }
    engine.client.detach();
    assert_eq!(engine.registry["order-client-id-1"].status, OrderStatus::Canceled);
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 4.0);

    // Connection 2 (the gateway rebuilt the bridge): it knows nothing, and the venue replays the day's executions.
    let (events2, mut rx2) = event_channel(64);
    let mut fake2 = IbkrTransportFake::new().with_socket_identity();
    fake2.script_socket_connect(101);
    fake2.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 101,
                order_ref: "order-client-id-1".into(),
                exec_id: "e1".into(),
                symbol: "AAPL.SMART.USD".into(),
                shares: 4.0,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    // The real client, handed over the way the host does: the order is known to it (taught), so its replayed
    // execution is recovered as a partial rather than dropped in the bridge.
    let mut actor2 = vike_ibkr::client_for_test(events2, Box::new(fake2));
    actor2.adopt_restored(&[RestoredOrder::new("order-client-id-1", "AAPL")]);
    fold_until_link_up(&mut engine, &mut rx2);
    actor2.detach();

    assert_eq!(
        engine.events_dropped_order_already_closed, 0,
        "no liveness event was thrown at a canceled order"
    );
    assert_eq!(engine.closing_events_dropped_order_still_live, 0);
    assert_eq!(
        engine.position_size(vike_model::events::PositionSide::Both),
        4.0,
        "the replay moved nothing"
    );
    assert_eq!(engine.registry["order-client-id-1"].status, OrderStatus::Canceled);
}

/// **The books are not protected by a SPELLING.** A rebuilt connection replays the day's executions of orders that
/// finished before it, and the host taught it nothing about this one (the daemon restarted, its ledger is empty).
/// The fill could carry only the instrument IB echoed; when that happens to equal the engine's mounted symbol the
/// engine books it (`accepts_symbol`), and an engine that had not folded the fill would book a down-time fill
/// before the operator confirmed it. So the bridge emits nothing for it and the reconcile reports it: the
/// position stays put whatever IB echoes (here, the very symbol the engine mounts).
#[test]
fn an_untaught_finished_orders_replay_books_nothing_even_when_ibkr_echoes_the_mounted_symbol() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 101,
                order_ref: "order-client-id-down".into(),
                exec_id: "e1".into(),
                symbol: "AAPL".into(),
                shares: 4.0,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    let client = vike_ibkr::client_for_test(events, Box::new(fake));
    let mut engine = Engine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "ibkr",
        "AAPL",
    );
    // The handover with nothing to teach: the replay runs, for an order the connection was never told of.
    engine.client.adopt_restored(&[]);
    fold_until_link_up(&mut engine, &mut rx);
    engine.client.detach();

    assert_eq!(
        engine.position_size(vike_model::events::PositionSide::Both),
        0.0,
        "an untaught replayed execution is left to the reconcile, not booked on the strength of its spelling"
    );
}

/// **F1, the rebound order.** After a rebuild the bridge learns an order from the venue's open orders and knows
/// nothing of the symbol the engine registered it under; the host teaches it (`adopt_restored`, which the restore
/// calls with the order's order client id and unified symbol). The fill of that order then carries the taught symbol, and
/// the engine, which mounts another symbol and owns the order under this one, books it. Without the teaching the
/// bridge emits NOTHING for the order's fills (`an_untaught_open_orders_replay_books_nothing_whatever_ibkr_echoes`).
#[test]
fn a_restored_order_the_host_taught_books_its_fill_whatever_ibkr_echoes() {
    use vike_ibkr::testing::{SnapshotMode, SnapshotRequest};

    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    fake.on_request_open_orders(|| {
        vec![ScriptedInbound::venue_order(105, "order-client-id-rested", 10.0)]
    });
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 105,
                order_ref: "order-client-id-rested".into(),
                exec_id: "e1".into(),
                symbol: "EUR.IDEALPRO.USD".into(),
                shares: 4.0,
                price: 1.1,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    let client = vike_ibkr::client_for_test(events, Box::new(fake.clone()));
    let mut engine = Engine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "ibkr",
        "AAPL",
    );
    // The engine restored the order (a registry entry at the symbol it was placed under) and tells its client.
    let restored = OrderRequest {
        order_client_id: "order-client-id-rested".into(),
        venue: "ibkr".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        ..Default::default()
    };
    engine.registry.insert("order-client-id-rested".into(), vike_model::OrderRecord::new(restored));
    engine.client.adopt_restored(&[RestoredOrder::new("order-client-id-rested", "EURUSD")]);
    std::thread::sleep(Duration::from_millis(150)); // the loop drains the teaching on its next turn
    fake.inject(ScriptedInbound::SnapshotEnd {
        snapshot: SnapshotRequest::OpenOrders,
        complete: true,
    });
    fold_until_link_up(&mut engine, &mut rx);
    engine.client.detach();

    assert_eq!(
        engine.position_size_of("EURUSD", vike_model::events::PositionSide::Both),
        4.0,
        "the replayed fill of the restored order booked under the symbol it was registered with"
    );
}

/// One connection of a rebuilt bridge, as `identity`, whose venue lists the rested order, replays `replay` of its
/// executions, and whose host makes the handover with an empty ledger (the daemon restarted since it was sent).
fn rebuilt_with_an_empty_ledger(
    identity: &str,
    engine: &mut Engine<vike_bridge_core::exec_actor::ExecutionClientActor>,
    replay: f64,
) {
    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| {
        vec![ScriptedInbound::venue_order(105, "order-client-id-rested", 10.0)]
    });
    fake.on_request_executions(move || {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 105,
                order_ref: "order-client-id-rested".into(),
                exec_id: "e1".into(),
                symbol: "AAPL.IBEOS.USD".into(),
                shares: replay,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    let mut client = vike_ibkr::client_for_test_as(identity, events, Box::new(fake));
    client.adopt_restored(&[]);
    fold_until_link_up(engine, &mut rx);
    client.detach();
}

/// An engine holding the restored `order-client-id-rested` under `AAPL`, over a first connection as `identity`
/// that made its handover with `handover`.
fn engine_holding_the_restored_order(
    identity: &str,
    handover: &[RestoredOrder],
) -> Engine<vike_bridge_core::exec_actor::ExecutionClientActor> {
    let (events, _rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    let client = vike_ibkr::client_for_test_as(identity, events, Box::new(fake));
    let mut engine = Engine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "ibkr",
        "MSFT",
    );
    let restored = OrderRequest {
        order_client_id: "order-client-id-rested".into(),
        venue: "ibkr".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        ..Default::default()
    };
    engine.registry.insert("order-client-id-rested".into(), vike_model::OrderRecord::new(restored));
    engine.client.adopt_restored(handover);
    engine
}

/// **IB logs the gateway off every day, and an adopted order must survive it.** The restore adopted an order a
/// PREVIOUS daemon placed and taught this client its symbol. The next rebuild is the same daemon's (no epoch
/// change, so no re-teaching) and its ledger never held the order, so the handover teaches nothing: what the host
/// taught the first client is what the new one stamps the fill with. Without it the fill reaches no engine, the
/// engine holds the order `Accepted` for ever, and the position moves only through a held `MissingFill`.
#[test]
fn an_adopted_order_books_its_fill_after_the_daily_rebuild() {
    vike_log::test_init();
    let identity = "gateway-a:1";
    let mut engine = engine_holding_the_restored_order(identity, &[]);
    engine.client.adopt_restored(&[RestoredOrder::new("order-client-id-rested", "AAPL")]);
    engine.client.detach();

    rebuilt_with_an_empty_ledger(identity, &mut engine, 4.0);

    assert_eq!(
        engine.position_size_of("AAPL", vike_model::events::PositionSide::Both),
        4.0,
        "the fill of the adopted order booked under the symbol the restore taught"
    );
}

/// PRESERVE: the handover's pairs are the gatewayhub ledger's, which teaches them again while it holds them, so a
/// pair ONLY the handover named is not kept (an order the ledger retired stays untaught at the next rebuild, as it
/// always was), and what one client was taught is not another client's.
#[test]
fn only_the_hosts_later_teaching_is_kept_and_only_for_its_own_client() {
    vike_log::test_init();
    let pair = [RestoredOrder::new("order-client-id-rested", "AAPL")];
    let mut engine = engine_holding_the_restored_order("gateway-b:1", &pair);
    engine.client.detach();
    rebuilt_with_an_empty_ledger("gateway-b:1", &mut engine, 4.0);
    assert_eq!(
        engine.position_size_of("AAPL", vike_model::events::PositionSide::Both),
        0.0,
        "the handover's pair was not kept"
    );

    let mut engine = engine_holding_the_restored_order("gateway-c:1", &[]);
    engine.client.adopt_restored(&pair);
    engine.client.detach();
    rebuilt_with_an_empty_ledger("gateway-c:2", &mut engine, 4.0);
    assert_eq!(
        engine.position_size_of("AAPL", vike_model::events::PositionSide::Both),
        0.0,
        "another client id is another client"
    );
}

/// **The untaught twin, through the real engine.** The same restored order, but the host taught the client nothing
/// (an empty handover: the daemon's ledger was empty), and IB echoes the replayed execution under the very symbol
/// the engine mounts: the one spelling an echo-stamped fill is BOOKED under. The order is rebound and announced
/// (its `OrderAccepted` is the engine's to fold), its execution is replayed, and the position stays where it was:
/// the fill of an order nobody taught is left to the reconcile, not booked on the strength of its spelling.
#[test]
fn an_untaught_open_orders_replay_books_nothing_whatever_ibkr_echoes() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| {
        vec![ScriptedInbound::venue_order(105, "order-client-id-rested", 10.0)]
    });
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 105,
                order_ref: "order-client-id-rested".into(),
                exec_id: "e1".into(),
                symbol: "AAPL".into(),
                shares: 4.0,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    let client = vike_ibkr::client_for_test(events, Box::new(fake));
    let mut engine = Engine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "ibkr",
        "AAPL",
    );
    let restored = OrderRequest {
        order_client_id: "order-client-id-rested".into(),
        venue: "ibkr".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        ..Default::default()
    };
    engine.registry.insert("order-client-id-rested".into(), vike_model::OrderRecord::new(restored));
    engine.client.adopt_restored(&[]); // the handover with nothing to teach
    fold_until_link_up(&mut engine, &mut rx);
    engine.client.detach();

    assert_eq!(
        engine.position_size(vike_model::events::PositionSide::Both),
        0.0,
        "the replayed fill of an order nobody taught is not booked, whatever IB echoes"
    );
}

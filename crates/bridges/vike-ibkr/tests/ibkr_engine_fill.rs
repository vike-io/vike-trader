//! A fill reaches the ENGINE's account, not just the order's FSM (review 18, R9 and its re-review, F1).
//!
//! `ExecutionEngine::on_event` drops a bare `Event::Fill` in silence (no log, no counter) unless the fill's
//! `symbol` is the engine's mounted symbol (`accepts_symbol`) or the symbol of the order the fill names
//! (`owns_fill_symbol`: registry hit on the coid AND `request.symbol == fill.symbol`). IB reports an
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

use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, Ingest, Outbox, RiskGate,
    event_channel,
};
use vike_ibkr::run_exec_for_test;
use vike_ibkr::testing::{FAKE_CLIENT_ID, FakeTransport, ScriptedInbound};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus, RiskLimits};

fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, n: usize, timeout: Duration) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < n && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(ev)) => out.push(ev),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    out
}

/// Submit one market BUY of 10 through an engine mounted on `mounted` and fold the bridge's events into
/// it, the venue echoing `echoed` as the execution's instrument. Returns the engine (position and order
/// state are read off it).
fn trade(mounted: &str, echoed: &str) -> ExecutionEngine<vike_bridge_core::exec_actor::ExecActor> {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let echoed = echoed.to_string();
    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(move |order_id| {
        vec![
            ScriptedInbound::OrderStatus {
                order_id,
                order_ref: "coid-1".into(),
                status: "Submitted".into(),
            },
            ScriptedInbound::ExecutionOf {
                order_id,
                order_ref: "coid-1".into(),
                exec_id: "e1".into(),
                symbol: echoed.clone(),
                shares: 10.0,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    let actor = run_exec_for_test(events, Box::new(fake));
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        actor,
        "ibkr",
        mounted,
    );
    let order = OrderRequest {
        client_order_id: "coid-1".into(),
        venue: "ibkr".into(),
        symbol: mounted.into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    let mut outbox = Outbox::default();
    engine.submit_order(&order, 1, &mut outbox);
    // OrderSubmitted, OrderAccepted, the bare Fill, OrderFilled.
    let got = drain(&mut rx, 4, Duration::from_secs(3));
    assert_eq!(got.len(), 4, "the bridge emits four events: {got:?}");
    for ev in &got {
        engine.on_event(ev, &mut outbox);
    }
    engine.client.detach();
    engine
}

/// A stock executed on a named exchange: the exec contract echoes that exchange (`AAPL.IBEOS.USD` for an
/// overnight execution, measured), the order and the engine are `AAPL.SMART.USD`. The position must move.
#[test]
fn a_stock_fill_echoed_with_another_exchange_still_books_into_the_engine() {
    let engine = trade("AAPL.SMART.USD", "AAPL.IBEOS.USD");
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 10.0);
    assert_eq!(engine.registry["coid-1"].status, OrderStatus::Filled);
}

/// Forex: the tree writes `EUR.USD.IDEALPRO` and the exec contract reconstructs `EUR.IDEALPRO.USD`
/// (symbol.exchange.currency). The bridge CLAUDE.md said no Fill is booked for it.
#[test]
fn a_forex_fill_echoed_in_the_reconstructed_order_books_into_the_engine() {
    let engine = trade("EUR.USD.IDEALPRO", "EUR.IDEALPRO.USD");
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 10.0);
    assert_eq!(engine.registry["coid-1"].status, OrderStatus::Filled);
}

/// The control: an echo equal to the submitted symbol worked before and must keep working.
#[test]
fn a_fill_echoed_as_submitted_books_into_the_engine() {
    let engine = trade("AAPL.SMART.USD", "AAPL.SMART.USD");
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 10.0);
}

/// Fold everything the bridge sends into `engine` until it announces the link up (the learning is over).
fn fold_until_link_up<C: ExecutionClient>(
    engine: &mut ExecutionEngine<C>,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut outbox = Outbox::default();
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(ev)) => {
                engine.on_event(&ev, &mut outbox);
            }
            Ok(Ingest::ExecLink(u)) if u.up => return,
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("the bridge never announced the link up");
}

/// **F4.** A rebuilt connection replays the day's executions at EVERY connect, and the ones of an order that is
/// finished are recovered as partials. The recovery used to synthesize an `OrderAccepted` for the order first;
/// the engine's FSM refuses a liveness event on an order it already canceled, and counts it as a possible
/// stranded position (`stranded_terminal_drops`, plus an error line) at every replay. The replayed fill itself
/// is the engine's to dedup (it already folded `e1`): the position stays put.
#[test]
fn replaying_the_executions_of_a_canceled_order_strands_nothing_in_the_engine() {
    vike_log::test_init();
    // Connection 1: the order fills 4 of 10, then is canceled.
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(|order_id| {
        vec![
            ScriptedInbound::OrderStatus {
                order_id,
                order_ref: "coid-1".into(),
                status: "Submitted".into(),
            },
            ScriptedInbound::ExecutionOf {
                order_id,
                order_ref: "coid-1".into(),
                exec_id: "e1".into(),
                symbol: "AAPL.SMART.USD".into(),
                shares: 4.0,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
            ScriptedInbound::OrderStatus {
                order_id,
                order_ref: "coid-1".into(),
                status: "Cancelled".into(),
            },
        ]
    });
    let actor = run_exec_for_test(events, Box::new(fake));
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        actor,
        "ibkr",
        "AAPL.SMART.USD",
    );
    let order = OrderRequest {
        client_order_id: "coid-1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    let mut outbox = Outbox::default();
    engine.submit_order(&order, 1, &mut outbox);
    // Submitted, Accepted, Fill, PartiallyFilled, Canceled.
    let got = drain(&mut rx, 5, Duration::from_secs(3));
    assert_eq!(got.len(), 5, "{got:?}");
    for ev in &got {
        engine.on_event(ev, &mut outbox);
    }
    engine.client.detach();
    assert_eq!(engine.registry["coid-1"].status, OrderStatus::Canceled);
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 4.0);

    // Connection 2 (the gateway rebuilt the bridge): it knows nothing, and the venue replays the day's executions.
    let (events2, mut rx2) = event_channel(64);
    let mut fake2 = FakeTransport::new().with_socket_identity();
    fake2.script_socket_connect(101);
    fake2.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 101,
                order_ref: "coid-1".into(),
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
    actor2.adopt_restored(&[("coid-1".to_string(), "AAPL.SMART.USD".to_string())]);
    fold_until_link_up(&mut engine, &mut rx2);
    actor2.detach();

    assert_eq!(
        engine.stranded_terminal_drops, 0,
        "no liveness event was thrown at a canceled order"
    );
    assert_eq!(engine.dropped_terminal_on_live, 0);
    assert_eq!(
        engine.position_size(vike_model::events::PositionSide::Both),
        4.0,
        "the replay moved nothing"
    );
    assert_eq!(engine.registry["coid-1"].status, OrderStatus::Canceled);
}

/// **The books are not protected by a SPELLING.** A rebuilt connection replays the day's executions of orders that
/// finished before it, and the host taught it nothing about this one (the daemon restarted, its ledger is empty).
/// The fill could carry only the instrument IB echoed; when that happens to equal the engine's mounted symbol the
/// engine books it (`accepts_symbol`), and an engine that had not folded the fill would book a down-time fill
/// before the operator confirmed it. So the bridge emits nothing for it and the reconcile reports it: the
/// position stays put whatever IB echoes (here, the very symbol the engine mounts).
#[test]
fn an_untaught_finished_orders_replay_books_nothing_even_when_ib_echoes_the_mounted_symbol() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 101,
                order_ref: "coid-down".into(),
                exec_id: "e1".into(),
                symbol: "AAPL.SMART.USD".into(),
                shares: 4.0,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    let client = vike_ibkr::client_for_test(events, Box::new(fake));
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "ibkr",
        "AAPL.SMART.USD",
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
/// calls with the order's coid and unified symbol). The fill of that order then carries the taught symbol, and
/// the engine, which mounts another symbol and owns the order under this one, books it. Without the teaching the
/// bridge emits NOTHING for the order's fills (`an_untaught_open_orders_replay_books_nothing_whatever_ib_echoes`).
#[test]
fn a_restored_order_the_host_taught_books_its_fill_whatever_ib_echoes() {
    use vike_ibkr::testing::{Snapshot, SnapshotMode};

    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    fake.on_request_open_orders(|| vec![ScriptedInbound::venue_order(105, "coid-rested", 10.0)]);
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 105,
                order_ref: "coid-rested".into(),
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
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "ibkr",
        "AAPL.SMART.USD",
    );
    // The engine restored the order (a registry entry at the symbol it was placed under) and tells its client.
    let restored = OrderRequest {
        client_order_id: "coid-rested".into(),
        venue: "ibkr".into(),
        symbol: "EUR.USD.IDEALPRO".into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        ..Default::default()
    };
    engine.registry.insert("coid-rested".into(), vike_model::ManagedOrder::new(restored));
    engine.client.adopt_restored(&[("coid-rested".to_string(), "EUR.USD.IDEALPRO".to_string())]);
    std::thread::sleep(Duration::from_millis(150)); // the loop drains the teaching on its next turn
    fake.inject(ScriptedInbound::SnapshotEnd { snapshot: Snapshot::OpenOrders, complete: true });
    fold_until_link_up(&mut engine, &mut rx);
    engine.client.detach();

    assert_eq!(
        engine.position_size_of("EUR.USD.IDEALPRO", vike_model::events::PositionSide::Both),
        4.0,
        "the replayed fill of the restored order booked under the symbol it was registered with"
    );
}

/// **The untaught twin, through the real engine.** The same restored order, but the host taught the client nothing
/// (an empty handover: the daemon's ledger was empty), and IB echoes the replayed execution under the very symbol
/// the engine mounts: the one spelling an echo-stamped fill is BOOKED under. The order is rebound and announced
/// (its `OrderAccepted` is the engine's to fold), its execution is replayed, and the position stays where it was:
/// the fill of an order nobody taught is left to the reconcile, not booked on the strength of its spelling.
#[test]
fn an_untaught_open_orders_replay_books_nothing_whatever_ib_echoes() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| vec![ScriptedInbound::venue_order(105, "coid-rested", 10.0)]);
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 105,
                order_ref: "coid-rested".into(),
                exec_id: "e1".into(),
                symbol: "AAPL.SMART.USD".into(),
                shares: 4.0,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    let client = vike_ibkr::client_for_test(events, Box::new(fake));
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "ibkr",
        "AAPL.SMART.USD",
    );
    let restored = OrderRequest {
        client_order_id: "coid-rested".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        ..Default::default()
    };
    engine.registry.insert("coid-rested".into(), vike_model::ManagedOrder::new(restored));
    engine.client.adopt_restored(&[]); // the handover with nothing to teach
    fold_until_link_up(&mut engine, &mut rx);
    engine.client.detach();

    assert_eq!(
        engine.position_size(vike_model::events::PositionSide::Both),
        0.0,
        "the replayed fill of an order nobody taught is not booked, whatever IB echoes"
    );
}

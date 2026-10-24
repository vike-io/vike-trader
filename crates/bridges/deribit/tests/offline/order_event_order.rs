//! A fill the venue pushes BEFORE it answers the order must still reach the core AFTER
//! `OrderSubmitted` and `OrderAccepted` -- the CI box 2026-10-24, `trade position flatten deribit
//! ETH-PERPETUAL`: a market sell filled at the venue and the position went flat, but the engine's row
//! stayed `ACCEPTED filled 0` with `terminal event dropped on a live order (out-of-order) ...
//! status="INITIALIZED"`, and a held `OrphanLocalOrder` alert followed. The order socket had re-dialed
//! just before the order was sent (`order-WS failed its re-auth/liveness probe`).
//!
//! The scenario is built so that nothing depends on timing. The order socket is the local stand-in of
//! `crate::fake_deribit_ws` (its first connection dies after the auth handshake, so the idle probe
//! fails and the transport re-dials before the order frame exists), and its `hook` runs the fill
//! pump's part -- the venue's `user.trades` frame through the real mapper -- strictly BEFORE the fake
//! answers `private/sell`. The exec thread is the real `run_loop` over the real `DeribitRest`. The
//! events the core's lane received are then folded through a REAL `ExecutionEngine`.
//!
//! Two wirings run the same scenario. `fenced` is the exec start's (`vike_deribit::exec`'s `run`):
//! `FencedRest` on the exec thread, `SubmitFence::deliver` as the pump's sink. The control is what
//! v0.1.84 did: the bare `DeribitRest`, the pump sending straight to the lane.
//!
//! The third test is the other producer: the audit-A3 reconnect replay, through the REAL pump and
//! supervisor, while the order's submit is still open (both sockets dropped by one blip).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::exec_actor::{ExecCommand, run_loop};
use vike_deribit::client::DeribitRest;
use vike_deribit::event_mapper::map_deribit_private;
use vike_deribit::history::map_deribit_history;
use vike_deribit::submit_fence::{FencedRest, SubmitFence};
use vike_deribit::user_data::spawn_deribit_user_data_with_resync;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, Ingest, Outbox, RiskGate, event_channel,
};
use vike_model::events::{Event, PositionSide};
use vike_model::{ManagedOrder, OrderRequest, OrderStatus, OrderType, RiskLimits, now_ms};

use crate::fake_deribit_ws::{FakeDeribit, SYMBOL, Session, transport_against};

/// The incident's client order id.
const COID: &str = "25115f7a2";

/// The flatten of the incident: a reduce-only market SELL of the whole 10.
fn flatten() -> OrderRequest {
    OrderRequest {
        client_order_id: COID.to_string(),
        venue: "deribit".to_string(),
        symbol: SYMBOL.to_string(),
        side: -1,
        qty: 10.0,
        order_type: OrderType::Market,
        reduce_only: true,
        ts: 1_700_000_000_000,
        ..Default::default()
    }
}

/// One filled-trade row of `coid`: the shape of a `user.trades` push and of the history fetch alike.
fn fill_row(coid: &str, trade_id: &str, timestamp: i64) -> serde_json::Value {
    json!({
        "label": coid, "timestamp": timestamp, "state": "filled", "price": 50_000.0,
        "trade_id": trade_id, "direction": "sell", "instrument_name": SYMBOL,
        "amount": 10.0, "order_id": "146062", "fee": 0.001, "fee_currency": "BTC",
        "liquidity": "T",
    })
}

/// The venue's `user.trades` push for the filled flatten: the frame shape the real pump reads.
fn fill_frame() -> serde_json::Value {
    json!({"jsonrpc": "2.0", "method": "subscription", "params": {
        "channel": "user.trades.any.any.raw",
        "data": [fill_row(COID, "t-flatten-1", 1_700_000_000_005i64)],
    }})
}

/// Run the scenario and return what the core's lane received, in order.
fn scenario(fenced: bool) -> Vec<Event> {
    let (events, mut rx) = event_channel(64);
    let fence = Arc::new(SubmitFence::new(events.clone()));

    // The pump's sink, as `spawn_deribit_user_data_with_resync` wires it (fenced) or as it was (not).
    let sink: Box<dyn Fn(Event) + Send + Sync> = if fenced {
        let fence = Arc::clone(&fence);
        Box::new(move |event| {
            fence.deliver(event);
        })
    } else {
        let events = events.clone();
        Box::new(move |event| {
            let _ = events.blocking_send(event);
        })
    };
    let fake = FakeDeribit::spawn_with_hook(
        // the first connection dies right after its auth handshake: the order socket sits "dead
        // while idle", so the probe before the order fails and the transport re-dials
        vec![Session::CloseAfterAuth, Session::OkForever],
        Arc::new(move |method: &str, _params: &serde_json::Value| {
            if method == "private/sell" {
                for event in map_deribit_private(&fill_frame(), "deribit", SYMBOL) {
                    sink(event);
                }
            }
        }),
    );
    let transport = transport_against(&fake).with_idle_probe_after(Duration::ZERO);
    let rest = DeribitRest::new(transport, SYMBOL, vike_model::SymbolProperties::default(), "BTC");

    let (commands, rx_commands) = std::sync::mpsc::channel();
    commands.send(ExecCommand::Submit(Box::new(flatten()))).unwrap();
    commands.send(ExecCommand::Shutdown).unwrap();
    let never = Duration::from_secs(3600);
    if fenced {
        run_loop(&FencedRest::new(&rest, &fence), &events, rx_commands, Vec::<Event>::new, never);
    } else {
        run_loop(&rest, &events, rx_commands, Vec::<Event>::new, never);
    }

    assert_eq!(fake.connections(), 2, "the order socket re-dialed before the order was sent");
    assert_eq!(fake.methods(), vec!["private/sell".to_string()], "one order, never re-sent");
    let mut received = Vec::new();
    while let Ok(ingest) = rx.try_recv() {
        if let Ingest::Event(event) = ingest {
            received.push(event);
        }
    }
    received
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "Submitted",
            Event::OrderAccepted(_) => "Accepted",
            Event::Fill(_) => "Fill",
            Event::OrderFilled(_) => "OrderFilled",
            _ => "other",
        })
        .collect()
}

/// The order the four kinds these scenarios produce belong to.
fn coid_of(event: &Event) -> &str {
    match event {
        Event::OrderSubmitted(e) => &e.client_order_id,
        Event::OrderAccepted(e) => &e.client_order_id,
        Event::Fill(e) => &e.client_order_id,
        Event::OrderFilled(e) => &e.client_order_id,
        _ => "",
    }
}

/// An engine mounted on the instrument, holding the flatten as the core registers it (`Initialized`),
/// with `events` folded through it in arrival order.
fn fold(events: &[Event]) -> ExecutionEngine<RecordingClient> {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "deribit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        SYMBOL,
    );
    engine.registry.insert(COID.to_string(), ManagedOrder::new(flatten()));
    let mut outbox = Outbox::default();
    for event in events {
        engine.on_event(event, &mut outbox);
    }
    engine
}

/// THE FIX. The fill reaches the pump before the order is answered, and the core still sees the
/// legal order -- so the order ends `Filled`, the engine refuses nothing, and the position is flat of
/// the sold 10.
#[test]
fn a_fill_pushed_before_the_order_is_answered_still_follows_the_answer() {
    let received = scenario(true);
    assert_eq!(kinds(&received), ["Submitted", "Accepted", "Fill", "OrderFilled"]);

    let engine = fold(&received);
    let order = &engine.registry[COID];
    assert_eq!(order.status, OrderStatus::Filled, "the venue-filled order is filled in the engine");
    assert_eq!(order.filled_qty, 10.0);
    assert_eq!(engine.dropped_terminal_on_live, 0, "no terminal event refused on a live order");
    assert_eq!(engine.position_size(PositionSide::Both), -10.0);
}

/// THE CONTROL: v0.1.84's wiring on the same scenario. It documents what the fence cures, and it is
/// what proves this harness CAN produce the inversion (a scenario that could not would let the test
/// above pass for the wrong reason). The engine half pins today's engine: it defers the refused fill
/// and replays it only when a bare `OrderAccepted` lands on an `Initialized` order, so with
/// `OrderSubmitted` in between the order rests `Accepted` with nothing filled. If the engine ever
/// replays on the ordinary accept too, this half changes -- the event-order half must not.
#[test]
fn without_the_fence_the_fill_overtakes_the_answer_and_strands_the_order() {
    let received = scenario(false);
    assert_eq!(kinds(&received), ["Fill", "OrderFilled", "Submitted", "Accepted"]);

    let engine = fold(&received);
    let order = &engine.registry[COID];
    assert_eq!(order.status, OrderStatus::Accepted, "the incident: ACCEPTED ...");
    assert_eq!(order.filled_qty, 0.0, "... filled 0, for a venue-filled order");
    assert_eq!(engine.dropped_terminal_on_live, 1, "... and the terminal-dropped counter");
    assert_eq!(engine.position_size(PositionSide::Both), -10.0, "the account itself was right");
}

/// THE REPLAY'S DOOR. One network blip kills BOTH sockets: the order socket re-dials inside the
/// submit, and the fill socket re-opens, which bumps the generation the audit-A3 resync supervisor
/// watches; it replays the account's recent history one settle later, and that history already holds
/// the fill of the order whose submit is STILL OPEN. Nothing here is a stand-in for the replay's
/// path: the fill socket is the real `spawn_deribit_user_data_with_resync` against a local venue that
/// closes its first connection right after the subscribe ack, so the real supervisor calls the fetch
/// closure and emits what comes back.
///
/// No timing decides the order. The order venue holds its answer to `private/sell` until the test
/// opens a gate, and the test opens it only after a PROBE event -- the last row of the very batch
/// the replay returned, for another order, which no fence holds -- has reached the lane: so every
/// other row of that batch has been sent or held by then, and the exec thread has not yet seen its
/// answer. Without the fence's `hold` on the replay the in-flight order's fill is already on the
/// lane at that point and the order of events is the incident's.
#[test]
fn a_reconnect_replay_that_returns_the_in_flight_orders_fill_still_follows_the_answer() {
    const PROBE: &str = "replay-probe";
    let (events, mut rx) = event_channel(64);
    let fence = Arc::new(SubmitFence::new(events.clone()));

    // The order venue: the first connection dies after auth (the transport re-dials before the order
    // is sent), and the answer to the order waits for `gate`.
    let (in_flight_tx, in_flight_rx) = std::sync::mpsc::channel::<()>();
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let (in_flight_tx, gate_rx) = (Mutex::new(in_flight_tx), Mutex::new(gate_rx));
    let gate_opened_in_time = Arc::new(AtomicBool::new(false));
    let order_venue = FakeDeribit::spawn_with_hook(
        vec![Session::CloseAfterAuth, Session::OkForever],
        Arc::new({
            let gate_opened_in_time = Arc::clone(&gate_opened_in_time);
            move |method: &str, _params: &serde_json::Value| {
                if method == "private/sell" {
                    let _ = in_flight_tx.lock().unwrap().send(());
                    // under the transport's own 10 s reply wait, so the submit stays one plain round trip
                    let opened = gate_rx.lock().unwrap().recv_timeout(Duration::from_secs(9));
                    gate_opened_in_time.store(opened.is_ok(), Ordering::SeqCst);
                }
            }
        }),
    );
    // The fill-stream venue: closes its first connection right after the subscribe ack, so the pump
    // re-opens (bumping the generation) and the supervisor replays once.
    let fill_venue = FakeDeribit::spawn(vec![Session::OkThenClose(1), Session::OkForever]);

    let mut replayed = false;
    let feed = spawn_deribit_user_data_with_resync(
        fill_venue.url.clone(),
        "client-id".to_string(),
        "client-secret".to_string(),
        SYMBOL.to_string(),
        events.clone(),
        Arc::clone(&fence),
        move || {
            if std::mem::replace(&mut replayed, true) {
                return Vec::new();
            }
            // the replay runs while the submit is open: wait until the venue has the order
            in_flight_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the order never reached the venue");
            let trades = json!([
                fill_row(COID, "t-flatten-1", now_ms()),
                fill_row(PROBE, "t-probe-1", now_ms())
            ]);
            map_deribit_history(&json!([]), &trades, "deribit", SYMBOL)
        },
    );

    // The exec thread: the real `run_loop` over the real `DeribitRest`, the order socket stale.
    let transport = transport_against(&order_venue).with_idle_probe_after(Duration::ZERO);
    let rest = DeribitRest::new(transport, SYMBOL, vike_model::SymbolProperties::default(), "BTC");
    let (commands, rx_commands) = std::sync::mpsc::channel();
    commands.send(ExecCommand::Submit(Box::new(flatten()))).unwrap();
    let exec = {
        let (events, fence) = (events.clone(), Arc::clone(&fence));
        std::thread::spawn(move || {
            let never = Duration::from_secs(3600);
            run_loop(
                &FencedRest::new(&rest, &fence),
                &events,
                rx_commands,
                Vec::<Event>::new,
                never,
            );
        })
    };

    // Wait (on a positive condition only) for the PROBE to reach the lane, then let the venue answer.
    let mut received: Vec<Event> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !received.iter().any(|e| matches!(e, Event::Fill(f) if f.client_order_id == PROBE)) {
        assert!(Instant::now() < deadline, "the reconnect replay never reached the lane");
        match rx.try_recv() {
            Ok(Ingest::Event(event)) => received.push(event),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    gate_tx.send(()).unwrap();
    commands.send(ExecCommand::Shutdown).unwrap();
    exec.join().unwrap();
    while let Ok(ingest) = rx.try_recv() {
        if let Ingest::Event(event) = ingest {
            received.push(event);
        }
    }
    feed.shutdown().unwrap();

    assert!(gate_opened_in_time.load(Ordering::SeqCst), "the venue answered before the replay ran");
    assert_eq!(
        order_venue.connections(),
        2,
        "the order socket re-dialed before the order was sent"
    );
    assert_eq!(
        fill_venue.connections(),
        2,
        "the fill socket re-opened, which is what fires the replay"
    );
    assert_eq!(order_venue.methods(), vec!["private/sell".to_string()], "one order, never re-sent");

    let mine: Vec<Event> = received.into_iter().filter(|e| coid_of(e) == COID).collect();
    assert_eq!(kinds(&mine), ["Submitted", "Accepted", "Fill", "OrderFilled"]);
    let engine = fold(&mine);
    assert_eq!(engine.registry[COID].status, OrderStatus::Filled);
    assert_eq!(engine.dropped_terminal_on_live, 0, "no terminal event refused on a live order");
}

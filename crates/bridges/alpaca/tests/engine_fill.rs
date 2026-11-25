//! A fill reaches the ENGINE's account, not just the order's FSM.
//!
//! `Engine::on_event` drops a bare `Event::Fill` in silence (no log, no counter) unless the
//! fill's `symbol` is the engine's mounted symbol or the symbol of the order the fill names (a registry
//! hit on the order client id AND `request.symbol == fill.symbol`). Alpaca spells the coin `BTCUSD` as
//! `BTC/USD` on the wire (its kept row's spelling) and its trade-update stream names the symbol that way, so
//! a fill stamped with it moved the order (the lifecycle wrap) and never the position. These tests run
//! the order through a REAL engine and read the account's position.
//!
//! The client is the real actor with this venue's real hooks (`exec::with_submitted_symbols`: its `submit`
//! records the submitted symbol, `adopt_restored` teaches a restored one) over a fake command loop that
//! mirrors `exec::run` (a submit is answered `Accepted`; the fill arrives later on the SSE stream); the SSE
//! decode shares ONE submitted-symbol table (`submitted_symbols`) with the hooks, as `spawn` does.

use crate::common::kept_rows;

use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::execution_drive::drain;

use vike_alpaca::execution::with_submitted_symbols;
use vike_alpaca::{decode_trade_event, map_order_response, submitted_symbols};
use vike_bridge_core::exec_actor::{ExecutionClientActor, ExecutionClientActorCommand};
use vike_bridge_core::per_symbol::OrderClientIdSymbols;
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, EventSender, ExecutionClient,
    Ingest, RiskGate, event_channel,
};
use vike_model::events::{Event, OrderSubmitted, PositionSide};
use vike_model::{OrderRecord, OrderRequest, OrderStatus, OrderType, RiskLimits};

fn fake_run(events: EventSender, rx: &std::sync::mpsc::Receiver<ExecutionClientActorCommand>) {
    while let Ok(cmd) = rx.recv() {
        let req = match cmd {
            ExecutionClientActorCommand::Submit(req, _) => req,
            ExecutionClientActorCommand::Shutdown => break,
            _ => continue,
        };
        let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
            order_client_id: req.order_client_id.clone(),
            timestamp: req.timestamp,
        }));
        let response = serde_json::json!({"id": "v-1", "client_order_id": req.order_client_id});
        for event in map_order_response(&req.order_client_id, req.timestamp, &response) {
            let _ = events.blocking_send(event);
        }
    }
}

/// An engine mounted on `BTCUSD` over the real client hooks.
fn engine(
    symbols: &Arc<OrderClientIdSymbols>,
) -> (Engine<ExecutionClientActor>, tokio::sync::mpsc::Receiver<Ingest>) {
    vike_log::test_init();
    let (events, rx) = event_channel(64);
    let actor = ExecutionClientActor::spawn("alpaca-exec-fake", events.clone(), move |r| {
        fake_run(events, r)
    });
    let engine = Engine::new(
        Account::new(1.0, "alpaca", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        with_submitted_symbols(actor, symbols.clone()),
        "alpaca",
        "BTCUSD",
    );
    (engine, rx)
}

fn order(symbol: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: "c-1".into(),
        venue: "alpaca".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 0.5,
        order_type: OrderType::Market,
        timestamp: 42,
        ..Default::default()
    }
}

/// Submit a market buy of 0.5 `symbol`; fold Submitted + Accepted.
fn submit(
    engine: &mut Engine<ExecutionClientActor>,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    symbol: &str,
) -> EventBusOutbox {
    let mut outbox = EventBusOutbox::default();
    engine.submit_order(&order(symbol), 1, &mut outbox);
    let got = drain(|| rx.try_recv().ok(), 2, Duration::from_secs(3));
    assert_eq!(got.len(), 2, "Submitted and Accepted: {got:?}");
    for event in &got {
        engine.on_event(event, &mut outbox);
    }
    outbox
}

/// The SSE frame for `order_client_id`: Alpaca names the crypto pair with the slash. `event` is `fill` or
/// `partial_fill`, `exec_id` the per-execution identity the engine dedups on.
fn sse(event: &str, exec_id: &str, qty: &str, order_client_id: &str) -> serde_json::Value {
    serde_json::json!({"event": event, "execution_id": exec_id,
        "timestamp": "2026-07-14T05:39:31.4Z", "price": "60000", "qty": qty,
        "order": {"client_order_id": order_client_id, "id": "v-1", "symbol": "BTC/USD", "side": "buy"}})
}

/// `BTCUSD` was submitted, the venue answers `BTC/USD`. Before the submitted symbol was recorded the
/// order read Filled and the position stayed 0.
#[test]
fn a_crypto_fill_books_into_the_engine_for_the_submitted_btcusd() {
    let symbols = submitted_symbols();
    let (mut engine, mut rx) = engine(&symbols);
    let mut outbox = submit(&mut engine, &mut rx, "BTCUSD");
    for event in decode_trade_event(&sse("fill", "e-1", "0.5", "c-1"), &symbols) {
        engine.on_event(&event, &mut outbox);
    }
    assert_eq!(engine.registry["c-1"].status, OrderStatus::Filled);
    assert_eq!(engine.position_size(PositionSide::Both), 0.5);
    engine.client.detach();
}

/// A `partial_fill` does not end the order: the pieces that follow still book under the submitted symbol.
#[test]
fn every_piece_of_a_partly_filled_order_books_under_the_submitted_symbol() {
    let symbols = submitted_symbols();
    let (mut engine, mut rx) = engine(&symbols);
    let mut outbox = submit(&mut engine, &mut rx, "BTCUSD");
    for frame in [sse("partial_fill", "e-1", "0.2", "c-1"), sse("fill", "e-2", "0.3", "c-1")] {
        for event in decode_trade_event(&frame, &symbols) {
            engine.on_event(&event, &mut outbox);
        }
    }
    assert_eq!(engine.position_size(PositionSide::Both), 0.5);
    engine.client.detach();
}

/// A restored order (a resting limit a previous session placed): the restore step announces
/// `(order client id, symbol)` through `adopt_restored`, and the stream's fill then carries that symbol.
#[test]
fn a_restored_order_the_host_taught_books_its_stream_fill() {
    let symbols = submitted_symbols();
    let (mut engine, _rx) = engine(&symbols);
    engine.registry.insert("c-1".into(), OrderRecord::new(order("BTCUSD")));
    engine.client.adopt_restored(&[vike_execution::RestoredOrder::new("c-1", "BTCUSD")]);
    let mut outbox = EventBusOutbox::default();
    for event in decode_trade_event(&sse("fill", "e-1", "0.5", "c-1"), &symbols) {
        engine.on_event(&event, &mut outbox);
    }
    assert_eq!(engine.position_size(PositionSide::Both), 0.5);
    engine.client.detach();
}

/// A fill of an order client id this process never submitted carries the USER name of its kept row (`BTC/USD` is
/// `BTCUSD`, decision 0290), and the venue's spelling only when no kept row is that instrument.
#[test]
fn a_fill_of_an_unknown_order_client_id_carries_its_kept_rows_user_name() {
    kept_rows::hold_kept_rows();
    let evs = decode_trade_event(&sse("fill", "e-1", "0.5", "foreign"), &submitted_symbols());
    let Some(Event::Fill(fill)) = evs.first() else { panic!("a bare Fill first: {evs:?}") };
    assert_eq!(fill.symbol, "BTCUSD");

    let mut unlisted = sse("fill", "e-2", "0.5", "foreign");
    unlisted["order"]["symbol"] = "SOL/USD".into();
    let evs = decode_trade_event(&unlisted, &submitted_symbols());
    let Some(Event::Fill(fill)) = evs.first() else { panic!("a bare Fill first: {evs:?}") };
    assert_eq!(fill.symbol, "SOL/USD", "no kept row: the venue's spelling");
}

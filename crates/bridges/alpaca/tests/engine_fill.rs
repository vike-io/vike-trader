//! A fill reaches the ENGINE's account, not just the order's FSM.
//!
//! `ExecutionEngine::on_event` drops a bare `Event::Fill` in silence (no log, no counter) unless the
//! fill's `symbol` is the engine's mounted symbol or the symbol of the order the fill names (a registry
//! hit on the coid AND `request.symbol == fill.symbol`). Alpaca spells the crypto pair `BTCUSD` as
//! `BTC/USD` on the wire (`to_alpaca_symbol`) and its trade-update stream names the symbol that way, so
//! a fill stamped with it moved the order (the lifecycle wrap) and never the position. These tests run
//! the order through a REAL engine and read the account's position.
//!
//! The client is the real `AlpacaExecutionClient` wrapper (its `submit` records the submitted symbol,
//! `adopt_restored` teaches a restored one) over a fake command loop that mirrors `exec::run` (a submit
//! is answered `Accepted`; the fill arrives later on the SSE stream); the SSE decode shares ONE
//! `SubmittedSymbols` with the wrapper, as `spawn` does.

use std::time::{Duration, Instant};

use vike_alpaca::{
    AlpacaExecutionClient, SubmittedSymbols, decode_trade_event, map_order_response,
};
use vike_bridge_core::exec_actor::{ExecActor, ExecCommand};
use vike_exec::{
    Account, BalanceMode, EventHandler, EventSender, ExecutionClient, ExecutionEngine, Ingest,
    Outbox, RiskGate, event_channel,
};
use vike_model::events::{Event, OrderSubmitted, PositionSide};
use vike_model::{ManagedOrder, OrderRequest, OrderStatus, OrderType, RiskLimits};

fn fake_run(events: EventSender, rx: std::sync::mpsc::Receiver<ExecCommand>) {
    while let Ok(cmd) = rx.recv() {
        let req = match cmd {
            ExecCommand::Submit(req) => req,
            ExecCommand::Shutdown => break,
            _ => continue,
        };
        let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: req.client_order_id.clone(),
            ts: req.ts,
        }));
        let resp = serde_json::json!({"id": "v-1", "client_order_id": req.client_order_id});
        for ev in map_order_response(&req.client_order_id, req.ts, &resp) {
            let _ = events.blocking_send(ev);
        }
    }
}

fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, n: usize) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(3);
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

/// An engine mounted on `BTCUSD` over the real client wrapper.
fn engine(
    symbols: &SubmittedSymbols,
) -> (ExecutionEngine<AlpacaExecutionClient>, tokio::sync::mpsc::Receiver<Ingest>) {
    vike_log::test_init();
    let (events, rx) = event_channel(64);
    let actor = ExecActor::spawn("alpaca-exec-fake", events.clone(), move |r| fake_run(events, r));
    let engine = ExecutionEngine::new(
        Account::new(1.0, "alpaca", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        AlpacaExecutionClient::from_parts(actor, symbols.clone()),
        "alpaca",
        "BTCUSD",
    );
    (engine, rx)
}

fn order(symbol: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: "c-1".into(),
        venue: "alpaca".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 0.5,
        order_type: OrderType::Market,
        ts: 42,
        ..Default::default()
    }
}

/// Submit a market buy of 0.5 `symbol`; fold Submitted + Accepted.
fn submit(
    engine: &mut ExecutionEngine<AlpacaExecutionClient>,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    symbol: &str,
) -> Outbox {
    let mut outbox = Outbox::default();
    engine.submit_order(&order(symbol), 1, &mut outbox);
    let got = drain(rx, 2);
    assert_eq!(got.len(), 2, "Submitted and Accepted: {got:?}");
    for ev in &got {
        engine.on_event(ev, &mut outbox);
    }
    outbox
}

/// The SSE frame for `coid`: Alpaca names the crypto pair with the slash. `event` is `fill` or
/// `partial_fill`, `exec_id` the per-execution identity the engine dedups on.
fn sse(event: &str, exec_id: &str, qty: &str, coid: &str) -> serde_json::Value {
    serde_json::json!({"event": event, "execution_id": exec_id,
        "timestamp": "2026-07-14T05:39:31.4Z", "price": "60000", "qty": qty,
        "order": {"client_order_id": coid, "id": "v-1", "symbol": "BTC/USD", "side": "buy"}})
}

/// `BTCUSD` was submitted, the venue answers `BTC/USD`. Before the submitted symbol was recorded the
/// order read Filled and the position stayed 0.
#[test]
fn a_crypto_fill_books_into_the_engine_for_the_submitted_btcusd() {
    let symbols = SubmittedSymbols::default();
    let (mut engine, mut rx) = engine(&symbols);
    let mut outbox = submit(&mut engine, &mut rx, "BTCUSD");
    for ev in decode_trade_event(&sse("fill", "e-1", "0.5", "c-1"), &symbols) {
        engine.on_event(&ev, &mut outbox);
    }
    assert_eq!(engine.registry["c-1"].status, OrderStatus::Filled);
    assert_eq!(engine.position_size(PositionSide::Both), 0.5);
    engine.client.detach();
}

/// A `partial_fill` does not end the order: the pieces that follow still book under the submitted symbol.
#[test]
fn every_piece_of_a_partly_filled_order_books_under_the_submitted_symbol() {
    let symbols = SubmittedSymbols::default();
    let (mut engine, mut rx) = engine(&symbols);
    let mut outbox = submit(&mut engine, &mut rx, "BTCUSD");
    for frame in [sse("partial_fill", "e-1", "0.2", "c-1"), sse("fill", "e-2", "0.3", "c-1")] {
        for ev in decode_trade_event(&frame, &symbols) {
            engine.on_event(&ev, &mut outbox);
        }
    }
    assert_eq!(engine.position_size(PositionSide::Both), 0.5);
    engine.client.detach();
}

/// An order submitted already slashed fills slashed (the engine owns it by its coid).
#[test]
fn an_order_submitted_as_the_venue_spelling_books_under_that_spelling() {
    let symbols = SubmittedSymbols::default();
    let (mut engine, mut rx) = engine(&symbols);
    let mut outbox = submit(&mut engine, &mut rx, "BTC/USD");
    for ev in decode_trade_event(&sse("fill", "e-1", "0.5", "c-1"), &symbols) {
        engine.on_event(&ev, &mut outbox);
    }
    assert_eq!(engine.registry["c-1"].status, OrderStatus::Filled);
    assert_eq!(engine.position_size_of("BTC/USD", PositionSide::Both), 0.5);
    assert_eq!(engine.position_size(PositionSide::Both), 0.0, "and nothing under the other name");
    engine.client.detach();
}

/// A restored order (a resting limit a previous session placed): the restore step announces
/// `(coid, symbol)` through `adopt_restored`, and the stream's fill then carries that symbol.
#[test]
fn a_restored_order_the_host_taught_books_its_stream_fill() {
    let symbols = SubmittedSymbols::default();
    let (mut engine, _rx) = engine(&symbols);
    engine.registry.insert("c-1".into(), ManagedOrder::new(order("BTCUSD")));
    engine.client.adopt_restored(&[("c-1".to_string(), "BTCUSD".to_string())]);
    let mut outbox = Outbox::default();
    for ev in decode_trade_event(&sse("fill", "e-1", "0.5", "c-1"), &symbols) {
        engine.on_event(&ev, &mut outbox);
    }
    assert_eq!(engine.position_size(PositionSide::Both), 0.5);
    engine.client.detach();
}

/// A fill of a coid this process never submitted keeps the venue's spelling.
#[test]
fn a_fill_of_an_unknown_coid_keeps_the_venue_spelling() {
    let evs =
        decode_trade_event(&sse("fill", "e-1", "0.5", "foreign"), &SubmittedSymbols::default());
    let Some(Event::Fill(fill)) = evs.first() else { panic!("a bare Fill first: {evs:?}") };
    assert_eq!(fill.symbol, "BTC/USD");
}

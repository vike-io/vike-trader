//! A fill reaches the ENGINE's account, not just the order's FSM.
//!
//! `Engine::on_event` drops a bare `Event::Fill` in silence (no log, no counter) unless the
//! fill's `symbol` is the engine's mounted symbol (`accepts_symbol`) or the symbol of the order the fill
//! names (`owns_fill_symbol`: a registry hit on the order client id AND `request.symbol == fill.symbol`). OANDA
//! spells `EURUSD` as `EUR_USD` on the wire (`vike_catalog::to_oanda_instrument`) and its transactions
//! name the instrument the venue's way, so a fill stamped with that spelling moved the order (the
//! lifecycle wrap) and never the position. These tests run the order through a REAL engine and read the
//! account's position: the thing the pure-decoder tests could not see.
//!
//! The client is the shared actor with the venue's REAL hooks (`vike_oanda::with_symbol_hooks`: its submit
//! records the submitted symbol, `adopt_restored` teaches a restored one) over a fake command loop that
//! mirrors `exec::run` as in `execution_actor_wiring.rs`; the loop and the stream decode share ONE
//! submitted-symbol table (`submitted_symbols`), as `spawn_execution_client` does.

use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::execution_drive::drain;

use vike_bridge_core::exec_actor::{ExecutionClientActor, ExecutionClientActorCommand};
use vike_bridge_core::per_symbol::OrderClientIdSymbols;
use vike_execution::recon::{ReconcileDivergence, ReconcileLocalState, diff};
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, EventSender, ExecutionClient,
    Ingest, RiskGate, event_channel,
};
use vike_model::events::{Event, OrderSubmitted, PositionSide};
use vike_model::{OrderRecord, OrderRequest, OrderStatus, OrderType, RiskLimits};
use vike_oanda::reconcile::parse_fill_reports;
use vike_oanda::{
    decode_transaction_events, map_order_response, map_transactions_since, submitted_symbols,
    with_symbol_hooks,
};

/// A fake `exec::run`: `OrderSubmitted`, then the canned POST reply through the real mapper. A market
/// order fills inline (the venue's spelling of the instrument, `EUR_USD`); anything else rests.
fn fake_run(
    events: EventSender,
    rx: std::sync::mpsc::Receiver<ExecutionClientActorCommand>,
    symbols: Arc<OrderClientIdSymbols>,
) {
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
        let response = if matches!(req.order_type, OrderType::Market) {
            serde_json::json!({
                "orderCreateTransaction": {"id": "9001", "type": "MARKET_ORDER"},
                "orderFillTransaction": {"id": "9002", "time": "1478012400.000000000",
                    "instrument": "EUR_USD", "units": "1000", "price": "1.09000",
                    "commission": "0.01"},
                "lastTransactionID": "9002"
            })
        } else {
            serde_json::json!({"orderCreateTransaction": {"id": "9003", "type": "LIMIT_ORDER"}})
        };
        for event in map_order_response(&req.order_client_id, req.timestamp, &response, &symbols) {
            let _ = events.blocking_send(event);
        }
    }
}

fn order(order_client_id: &str, symbol: &str, order_type: OrderType) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "oanda".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1000.0,
        order_type,
        price: Some(1.09),
        timestamp: 42,
        ..Default::default()
    }
}

/// An engine mounted on `EURUSD` over the hooked actor; `symbols` is the table the stream decode below
/// reads, shared with the fake loop exactly as `vike_oanda::spawn_execution_client` shares it.
fn engine(
    symbols: &Arc<OrderClientIdSymbols>,
) -> (Engine<ExecutionClientActor>, tokio::sync::mpsc::Receiver<Ingest>) {
    vike_log::test_init();
    let (events, rx) = event_channel(64);
    let actor = ExecutionClientActor::spawn("oanda-exec-fake", events.clone(), {
        let symbols = symbols.clone();
        move |r| fake_run(events, r, symbols)
    });
    let engine = Engine::new(
        Account::new(1.0, "oanda", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        with_symbol_hooks(actor, symbols),
        "oanda",
        "EURUSD",
    );
    (engine, rx)
}

/// Submit `order` and fold the `n` events the bridge answers with.
fn submit(
    engine: &mut Engine<ExecutionClientActor>,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    order: &OrderRequest,
    n: usize,
) -> EventBusOutbox {
    let mut outbox = EventBusOutbox::default();
    engine.submit_order(order, 1, &mut outbox);
    let got = drain(|| rx.try_recv().ok(), n, Duration::from_secs(3));
    assert_eq!(got.len(), n, "the bridge answers with {n} events: {got:?}");
    for event in &got {
        engine.on_event(event, &mut outbox);
    }
    outbox
}

/// The fill of a resting order, as the transactions stream (and the sinceid backfill) names it.
fn stream_fill(order_client_id: &str) -> serde_json::Value {
    serde_json::json!({"type": "ORDER_FILL", "id": "9100", "time": "1478012500.000000000",
        "orderID": "9003", "instrument": "EUR_USD", "units": "1000", "price": "1.09000",
        "commission": "0.01", "clientExtensions": {"id": order_client_id}})
}

/// The inline fill of a market order: `EURUSD` was submitted, the venue answers `EUR_USD`. Before the
/// submitted symbol was recorded the order read Filled and the position stayed 0.
#[test]
fn an_inline_market_fill_books_into_the_engine_for_the_submitted_eurusd() {
    let symbols = submitted_symbols();
    let (mut engine, mut rx) = engine(&symbols);
    // Submitted, Accepted, the bare Fill, OrderFilled.
    submit(&mut engine, &mut rx, &order("c-mkt", "EURUSD", OrderType::Market), 4);
    assert_eq!(engine.registry["c-mkt"].status, OrderStatus::Filled);
    assert_eq!(engine.position_size(PositionSide::Both), 1000.0);
    engine.client.detach();
}

/// The delayed fill of a resting limit, off the transactions stream.
#[test]
fn a_stream_fill_books_into_the_engine_for_the_submitted_eurusd() {
    let symbols = submitted_symbols();
    let (mut engine, mut rx) = engine(&symbols);
    // Submitted, Accepted.
    let mut outbox = submit(&mut engine, &mut rx, &order("c-lim", "EURUSD", OrderType::Limit), 2);
    assert_eq!(engine.position_size(PositionSide::Both), 0.0);
    let evs = decode_transaction_events(&stream_fill("c-lim"), &symbols);
    for event in &evs {
        engine.on_event(event, &mut outbox);
    }
    assert_eq!(engine.registry["c-lim"].status, OrderStatus::Filled);
    assert_eq!(engine.position_size(PositionSide::Both), 1000.0);
    engine.client.detach();
}

/// The GUI may send the venue's own spelling: an order submitted as `EUR_USD` fills as `EUR_USD`
/// (the engine owns it by its order client id), whatever symbol the engine is mounted on.
#[test]
fn an_order_submitted_as_the_venue_spelling_books_under_that_spelling() {
    let symbols = submitted_symbols();
    let (mut engine, mut rx) = engine(&symbols);
    submit(&mut engine, &mut rx, &order("c-venue", "EUR_USD", OrderType::Market), 4);
    assert_eq!(engine.registry["c-venue"].status, OrderStatus::Filled);
    assert_eq!(engine.position_size_of("EUR_USD", PositionSide::Both), 1000.0);
    assert_eq!(engine.position_size(PositionSide::Both), 0.0, "and nothing under the other name");
    engine.client.detach();
}

/// A restored order (a resting limit a previous session placed) fills after a restart: the restore step
/// announces `(order client id, symbol)` through `adopt_restored`, and the stream's fill then carries that symbol.
#[test]
fn a_restored_order_the_host_taught_books_its_stream_fill() {
    let symbols = submitted_symbols();
    let (mut engine, _rx) = engine(&symbols);
    let restored = order("c-rested", "EURUSD", OrderType::Limit);
    engine.registry.insert("c-rested".into(), OrderRecord::new(restored));
    engine.client.adopt_restored(&[vike_execution::RestoredOrder::new("c-rested", "EURUSD")]);
    let mut outbox = EventBusOutbox::default();
    for event in &decode_transaction_events(&stream_fill("c-rested"), &symbols) {
        engine.on_event(event, &mut outbox);
    }
    assert_eq!(engine.position_size(PositionSide::Both), 1000.0);
    engine.client.detach();
}

/// **A backfill that overlaps a line already read books the fill once**, in either order (the reopened stream
/// delivers what the read-back just did; a re-read after a dropped stream repeats a line): one position, one
/// commission, one fill on the order. `crates/bridges/oanda/src/execution/user_data_tests.rs` shows the two copies leaving the lane.
#[test]
fn a_fill_the_backfill_and_the_line_both_carry_books_once() {
    let body =
        serde_json::json!({ "transactions": [stream_fill("c-lim")], "lastTransactionID": "9100" });
    for backfill_first in [true, false] {
        let symbols = submitted_symbols();
        let (mut engine, mut rx) = engine(&symbols);
        let mut outbox =
            submit(&mut engine, &mut rx, &order("c-lim", "EURUSD", OrderType::Limit), 2);
        let line = decode_transaction_events(&stream_fill("c-lim"), &symbols);
        let gap = map_transactions_since(&body, &symbols);
        let (first, second) = if backfill_first { (gap, line) } else { (line, gap) };
        for event in &first {
            engine.on_event(event, &mut outbox);
        }
        let fees_once = engine.account.fees_paid;
        assert!(fees_once > 0.0, "the commission was booked");
        assert_eq!(engine.registry["c-lim"].status, OrderStatus::Filled);
        for event in &second {
            engine.on_event(event, &mut outbox);
        }
        assert_eq!(engine.position_size(PositionSide::Both), 1000.0);
        assert_eq!(engine.account.fees_paid, fees_once);
        engine.client.detach();
    }
}

/// **A fill read again one pass late is no divergence.** The fill floor moves only when the next pass asks, so the first
/// pass after a restart reads again what the last pass read: an engine that holds the trade id (the live one, or one
/// restored with its `seen_trade_ids`) diffs that to nothing. Without the restored set it IS a `MissingFill`, which is
/// why `crates/bridges/oanda/CLAUDE.md` keeps this venue off `hybrid`.
#[test]
fn a_fill_read_again_after_a_restart_is_no_divergence() {
    let symbols = submitted_symbols();
    let (mut folded, mut rx) = engine(&symbols);
    let mut outbox = submit(&mut folded, &mut rx, &order("c-lim", "EURUSD", OrderType::Limit), 2);
    for event in &decode_transaction_events(&stream_fill("c-lim"), &symbols) {
        folded.on_event(event, &mut outbox);
    }
    let body =
        serde_json::json!({ "transactions": [stream_fill("c-lim")], "lastTransactionID": "9100" });
    let fills = parse_fill_reports(&body, "EUR_USD", "EURUSD", "USD");
    assert_eq!(fills.len(), 1);
    let missing = |local: &ReconcileLocalState| {
        diff(&[], &fills, &[], &local.as_view(), None)
            .iter()
            .filter(|d| matches!(d, ReconcileDivergence::MissingFill(_)))
            .count()
    };
    assert_eq!(missing(&folded.local_view()), 0, "the engine that folded it");

    let other = submitted_symbols();
    let (mut restarted, _rx) = engine(&other);
    assert_eq!(missing(&restarted.local_view()), 1, "the control: nothing restored");
    restarted.seed_seen_trade_ids(folded.seen_trade_ids().map(str::to_string));
    assert_eq!(missing(&restarted.local_view()), 0, "restored with its dedup set");
    folded.client.detach();
    restarted.client.detach();
}

/// A fill of an order client id this process never submitted (another client, the web platform, a backfill after a
/// restart) keeps the venue's spelling: nothing is known to relabel it by.
#[test]
fn a_fill_of_an_unknown_order_client_id_keeps_the_venue_spelling() {
    let evs = decode_transaction_events(&stream_fill("foreign"), &submitted_symbols());
    let Some(Event::Fill(fill)) = evs.first() else { panic!("a bare Fill first: {evs:?}") };
    assert_eq!(fill.symbol, "EUR_USD");
}

//! One cancel of a resting conditional order must reach the core as ONE terminal event.
//!
//! Measured on the v0.1.87 node (Bybit DEMO, 2026-10-24): cancelling a resting STOP printed
//! `outcome: OrderExpired`, because Bybit's private `order` topic sends a `Cancelled` row and then a
//! `Deactivated` row for a cancelled conditional order, and the mapper turns the first into
//! `OrderCanceled` and the second into `OrderExpired`. The engine already ignored the second
//! (`engine_drops_the_second_terminal_without_a_counter`), so the order row was right, but the
//! journal and the CLI's outcome line carried both.
//!
//! The cure is `vike_bybit::user_data::perp_frame_decoder`, the decoder the private-WS pump runs on
//! every frame: `map_bybit_perp` plus a bounded memory of the orders it already reported a cancel or
//! an expiry for. A genuine `Deactivated` (nothing before it) is still `OrderExpired`.
//!
//! The frames are scripted, as the pump would hand them over; nothing here opens a socket.

use serde_json::{Value, json};
use vike_bybit::event_mapper::map_bybit_perp;
use vike_bybit::user_data::{REPORTED_TERMINALS_KEPT, perp_frame_decoder};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate};
use vike_model::events::{Event, OrderSubmitted};
use vike_model::{OrderRequest, OrderStatus, RiskLimits};

/// One `order`-topic row in the shape the venue sends for a conditional (stop) order.
fn row(coid: &str, status: &str) -> Value {
    json!({
        "category": "linear", "symbol": "BTCUSDT", "orderId": "v-oid", "orderLinkId": coid,
        "orderStatus": status, "cancelType": "CancelByUser", "stopOrderType": "Stop",
        "updatedTime": "5"
    })
}

fn frame(rows: Vec<Value>) -> Value {
    json!({"topic": "order", "creationTime": 1, "data": rows})
}

/// What the core would be handed, as `Kind:coid` words.
fn words(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderCanceled(c) => format!("OrderCanceled:{}", c.client_order_id),
            Event::OrderExpired(x) => format!("OrderExpired:{}", x.client_order_id),
            Event::OrderAccepted(a) => format!("OrderAccepted:{}", a.client_order_id),
            Event::Fill(f) => format!("Fill:{}", f.client_order_id),
            Event::OrderFilled(w) => format!("OrderFilled:{}", w.fill.client_order_id),
            other => format!("{other:?}"),
        })
        .collect()
}

/// Run frames through ONE decoder, in order, and return everything it handed over.
fn decode_all(frames: &[Value]) -> Vec<String> {
    let decode = perp_frame_decoder("BTCUSDT");
    frames.iter().flat_map(|f| words(&decode(f))).collect()
}

#[test]
fn cancelled_then_deactivated_is_one_cancel() {
    // Two frames, as the live socket delivers them ...
    let got =
        decode_all(&[frame(vec![row("c1", "Cancelled")]), frame(vec![row("c1", "Deactivated")])]);
    assert_eq!(got, ["OrderCanceled:c1"], "the cancel is the only terminal");

    // ... and both rows in ONE frame, which the venue may also do.
    let got = decode_all(&[frame(vec![row("c1", "Cancelled"), row("c1", "Deactivated")])]);
    assert_eq!(got, ["OrderCanceled:c1"]);
}

#[test]
fn a_lone_deactivated_is_still_an_expiry() {
    // A stop that expired, or one the venue deactivated on its own: no `Cancelled` row before it.
    assert_eq!(decode_all(&[frame(vec![row("c1", "Deactivated")])]), ["OrderExpired:c1"]);
}

#[test]
fn deactivated_before_cancelled_is_still_one_terminal() {
    // The first terminal wins, exactly as the engine would have had it: one event, not two.
    let got =
        decode_all(&[frame(vec![row("c1", "Deactivated")]), frame(vec![row("c1", "Cancelled")])]);
    assert_eq!(got, ["OrderExpired:c1"]);
}

#[test]
fn only_the_order_that_ended_is_remembered() {
    // Another order's expiry, an order's acceptance and a fill's replay are not touched.
    let got = decode_all(&[
        frame(vec![row("c1", "Cancelled")]),
        frame(vec![row("c2", "Deactivated")]),
        frame(vec![row("c3", "New")]),
        frame(vec![row("c1", "Deactivated")]),
    ]);
    assert_eq!(got, ["OrderCanceled:c1", "OrderExpired:c2", "OrderAccepted:c3"]);

    // A row with no `orderLinkId` names no order, so two of them are two events.
    let got = decode_all(&[frame(vec![row("", "Deactivated"), row("", "Deactivated")])]);
    assert_eq!(got.len(), 2, "{got:?}");

    // The decoder leaves fills alone: a replayed execution row still gives [Fill, OrderFilled]
    // twice (the engine's execId dedup owns that), so the memory is only for cancel/expiry.
    let fill = json!({"topic": "execution", "data": [{
        "execType": "Trade", "symbol": "BTCUSDT", "orderLinkId": "c9", "execId": "E1",
        "side": "Buy", "execPrice": "100", "execQty": "1", "execFee": "0.06",
        "feeCurrency": "USDT", "isMaker": false, "cumExecQty": "1", "orderQty": "1",
        "leavesQty": "0"}]});
    let got = decode_all(&[fill.clone(), fill]);
    assert_eq!(got, ["Fill:c9", "OrderFilled:c9", "Fill:c9", "OrderFilled:c9"]);
}

#[test]
fn the_memory_is_bounded() {
    let decode = perp_frame_decoder("BTCUSDT");
    let coid = |n: usize| format!("c{n}");
    // One more cancelled order than the memory holds: the OLDEST id is forgotten, the rest stay.
    for n in 0..=REPORTED_TERMINALS_KEPT {
        assert_eq!(words(&decode(&frame(vec![row(&coid(n), "Cancelled")]))).len(), 1);
    }
    let again = |n: usize| words(&decode(&frame(vec![row(&coid(n), "Deactivated")])));
    assert!(
        again(REPORTED_TERMINALS_KEPT).is_empty(),
        "the newest cancel is still remembered, so its Deactivated is dropped"
    );
    assert_eq!(
        again(0),
        ["OrderExpired:c0"],
        "the oldest cancel fell out of the bounded memory (a ring, not a growing set)"
    );
}

/// The engine side, measured: the second terminal on an ended order was ALREADY dropped by the FSM
/// without a counter, so no order state was ever wrong. This is why the fix lives in the bridge
/// (the journal and the outcome line), not in the engine.
#[test]
fn engine_drops_the_second_terminal_without_a_counter() {
    let mut eng = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    let mut outbox = Outbox::default();
    let req = OrderRequest {
        client_order_id: "c1".into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    };
    eng.submit_order(&req, 1, &mut outbox);
    eng.on_event(
        &Event::OrderSubmitted(OrderSubmitted { client_order_id: "c1".to_string(), ts: 1 }),
        &mut outbox,
    );
    for ev in map_bybit_perp(&frame(vec![row("c1", "New")]), "bybit", "BTCUSDT") {
        eng.on_event(&ev, &mut outbox);
    }
    assert_eq!(eng.registry.get("c1").unwrap().status, OrderStatus::Accepted);

    // The RAW mapper output (no memory): a cancel, then the expiry.
    for status in ["Cancelled", "Deactivated"] {
        for ev in map_bybit_perp(&frame(vec![row("c1", status)]), "bybit", "BTCUSDT") {
            eng.on_event(&ev, &mut outbox);
        }
    }
    assert_eq!(
        eng.registry.get("c1").unwrap().status,
        OrderStatus::Canceled,
        "first terminal wins"
    );
    assert_eq!(eng.dropped_terminal_on_live, 0, "not a lost terminal: the order had already ended");
    assert_eq!(eng.stranded_terminal_drops, 0);
}

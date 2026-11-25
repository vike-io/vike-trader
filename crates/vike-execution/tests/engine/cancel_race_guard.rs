//! Cancel-vs-fill race guard (LEAN `CancelPendingOrders` semantics, reimplemented). Two halves:
//!
//! 1. LIVE-PATH PINS — `Engine::cancel_order` is fire-and-forget and never mutates
//!    status, so the pre-cancel status IS the LEAN "snapshot": a fill racing the in-flight cancel
//!    applies normally, a venue cancel-reject leaves the order in its live state (meanwhile-fills
//!    folded), and a cancel-ack terminalizes CANCELED. Pinned at the ENGINE level (the FSM-level
//!    advisory behavior is unit-tested in `order.rs`).
//!
//! 2. THE DELTA — an order CAN sit at PENDING_CANCEL via venue-status seeding
//!    (`reregister_orders`; binance recon passes the venue's `PENDING_CANCEL` through verbatim),
//!    and the r5 fixture-pinned FSM table rejects fills from that state and leaves a cancel-reject
//!    inert, which would drop venue truth (a reconcile reap then mislabels a filled order
//!    CANCELED). The engine fold restores the pre-cancel live status (recomputed from `filled_qty`
//!    — `OrderRecord::resolve_pending_cancel`) before the one `apply` site, ONLY for the events
//!    that prove the cancel lost or died (fill wraps, cancel-reject). The FSM transition table
//!    itself is untouched (golden fixture).

use vike_execution::testing::{EngineBuilder, RecordingClient, canceled, resting_limit};
use vike_execution::{Engine, EventBus, EventBusOutbox};
use vike_model::OrderStatus;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCancelRejected, OrderFilled, OrderPartiallyFilled,
    OrderSubmitted,
};

fn engine() -> Engine<RecordingClient> {
    EngineBuilder { venue: "binance".into(), ..Default::default() }.build()
}

fn fill(order_client_id: &str, trade_id: &'static str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        order_client_id: order_client_id.to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: Some(px),
        position_side: vike_model::events::PositionSide::Both,
    }
}

fn submitted(order_client_id: &str) -> Event {
    Event::OrderSubmitted(OrderSubmitted { order_client_id: order_client_id.into(), timestamp: 0 })
}

fn accepted(order_client_id: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.into(),
        venue_order_id: Some("v1".into()),
        timestamp: 0,
    })
}

fn partial(order_client_id: &str, trade_id: &'static str, qty: f64, px: f64) -> Event {
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        order_client_id: order_client_id.into(),
        fill: fill(order_client_id, trade_id, qty, px),
        timestamp: 0,
    })
}

fn filled(order_client_id: &str, trade_id: &'static str, qty: f64, px: f64) -> Event {
    Event::OrderFilled(OrderFilled {
        order_client_id: order_client_id.into(),
        fill: fill(order_client_id, trade_id, qty, px),
        timestamp: 0,
    })
}

fn cancel_rejected(order_client_id: &str) -> Event {
    Event::OrderCancelRejected(OrderCancelRejected {
        order_client_id: order_client_id.into(),
        reason: "order does not exist / too late to cancel".into(),
        timestamp: 0,
        refusal_id: None,
    })
}

/// Submit + venue-accept one resting order, then issue the cancel intent (fire-and-forget).
fn resting_with_cancel_in_flight(order_client_id: &str) -> Engine<RecordingClient> {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    let mut outbox = EventBusOutbox::default();
    engine_under_test.submit_order(&resting_limit(order_client_id), 1, &mut outbox);
    // The emitter split: the adapter emits OrderSubmitted, then the venue acks — replay both so
    // the order rests ACCEPTED before the cancel intent.
    bus.publish(submitted(order_client_id), &mut engine_under_test);
    bus.publish(accepted(order_client_id), &mut engine_under_test);
    engine_under_test.cancel_order(order_client_id);
    assert_eq!(
        engine_under_test.client.cancels.as_slice(),
        &[order_client_id.to_string()],
        "cancel intent reached the venue client"
    );
    // The LEAN pending-cancels "snapshot": vike keeps it implicitly — the status is untouched.
    assert_eq!(
        engine_under_test.registry[order_client_id].status,
        OrderStatus::Accepted,
        "cancel intent never clobbers"
    );
    engine_under_test
}

/// Seed one order at PENDING_CANCEL through the REAL production path: an operator-confirmed
/// `reregister_orders` recovery whose venue report carries the venue's own PENDING_CANCEL status.
fn seeded_pending_cancel(order_client_id: &str, filled_qty: f64) -> Engine<RecordingClient> {
    let mut engine_under_test = engine();
    let n = engine_under_test.reregister_orders(&[vike_model::OrderStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v1".into(),
        order_client_id: Some(order_client_id.into()),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 1.0,
        filled_qty,
        average_price: if filled_qty > 0.0 { 100.0 } else { 0.0 },
        status: vike_model::OrderStatus::PendingCancel,
        timestamp: 5,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    }]);
    assert_eq!(n, 1);
    assert_eq!(
        engine_under_test.registry[order_client_id].status,
        OrderStatus::PendingCancel,
        "seeded at PENDING_CANCEL"
    );
    engine_under_test
}

// --- 1. Live-path pins: cancel intent never clobbers, so LEAN's restore is implicit ---

/// cancel → fill-before-ack: the fill applies normally (NOT dropped as an invalid transition) and
/// the order completes FILLED; the late cancel-ack is dropped benignly, the terminal untouched.
#[test]
fn cancel_then_fill_before_ack_completes_filled() {
    let mut engine = resting_with_cancel_in_flight("c1");
    let mut bus = EventBus::new();

    bus.publish(filled("c1", "t1", 1.0, 100.0), &mut engine);
    assert_eq!(engine.registry["c1"].status, OrderStatus::Filled, "fill won the race");
    assert_eq!(engine.registry["c1"].filled_qty, 1.0);

    // The straggler ack is NOT counted as a lost terminal: the order IS terminal (a benign
    // out-of-order artifact).
    bus.publish(canceled("c1", "user", 0), &mut engine);
    assert_eq!(engine.registry["c1"].status, OrderStatus::Filled, "terminal stands");
    assert_eq!(engine.closing_events_dropped_order_still_live, 0);
    assert_eq!(engine.events_dropped_order_already_closed, 0);
}

/// cancel → reject: the venue refused the cancel. The order must be back (= still) in its
/// pre-cancel status, and a subsequent fill still lands.
#[test]
fn cancel_reject_restores_status_and_subsequent_fill_lands() {
    let mut engine = resting_with_cancel_in_flight("c2");
    let mut bus = EventBus::new();

    bus.publish(cancel_rejected("c2"), &mut engine);
    assert_eq!(
        engine.registry["c2"].status,
        OrderStatus::Accepted,
        "pre-cancel status restored (never clobbered)"
    );

    bus.publish(filled("c2", "t1", 1.0, 100.0), &mut engine);
    assert_eq!(engine.registry["c2"].status, OrderStatus::Filled, "fill after reject lands");
    assert_eq!(engine.registry["c2"].filled_qty, 1.0);
}

/// cancel → partial-fill → reject: fills that arrive while the cancel is pending fold into the
/// order, and the reject leaves it PARTIALLY_FILLED with the updated qty — after which the
/// remainder can still fill to completion.
#[test]
fn cancel_partial_fill_then_reject_keeps_updated_quantity() {
    let mut engine = resting_with_cancel_in_flight("c3");
    let mut bus = EventBus::new();

    bus.publish(partial("c3", "t1", 0.4, 100.5), &mut engine);
    assert_eq!(
        engine.registry["c3"].status,
        OrderStatus::PartiallyFilled,
        "mid-cancel fill applies"
    );
    assert_eq!(engine.registry["c3"].filled_qty, 0.4);

    bus.publish(cancel_rejected("c3"), &mut engine);
    assert_eq!(
        engine.registry["c3"].status,
        OrderStatus::PartiallyFilled,
        "restored WITH the meanwhile-fill folded in"
    );
    assert_eq!(engine.registry["c3"].filled_qty, 0.4, "fill accumulation survives the reject");

    bus.publish(filled("c3", "t2", 0.6, 101.0), &mut engine);
    assert_eq!(engine.registry["c3"].status, OrderStatus::Filled);
    assert_eq!(engine.registry["c3"].filled_qty, 1.0);
}

/// cancel → ack: the normal path — the venue's authoritative OrderCanceled terminalizes CANCELED.
#[test]
fn cancel_ack_terminalizes_canceled() {
    let mut engine = resting_with_cancel_in_flight("c4");
    let mut bus = EventBus::new();

    bus.publish(canceled("c4", "user", 0), &mut engine);
    assert_eq!(engine.registry["c4"].status, OrderStatus::Canceled);
    assert_eq!(engine.closing_events_dropped_order_still_live, 0);
}

// --- 2. The delta: venue-seeded PENDING_CANCEL orders ---

/// PENDING_CANCEL + full fill wrap: the cancel lost the race outright. The guard restores the
/// pre-cancel status and the fill applies to FILLED, instead of being dropped (a fixture-pinned
/// invalid transition) and counted as a lost terminal, stranding the order.
#[test]
fn pending_cancel_fill_wrap_applies_instead_of_stranding() {
    let mut engine = seeded_pending_cancel("p1", 0.0);
    let mut bus = EventBus::new();

    bus.publish(filled("p1", "t1", 1.0, 100.0), &mut engine);
    assert_eq!(engine.registry["p1"].status, OrderStatus::Filled, "venue truth applies");
    assert_eq!(engine.registry["p1"].filled_qty, 1.0);
    assert_eq!(
        engine.closing_events_dropped_order_still_live, 0,
        "nothing dropped — nothing to count"
    );
    assert_eq!(engine.events_dropped_order_already_closed, 0);
}

/// PENDING_CANCEL + partial fill, then the cancel-ack arrives: the mid-cancel execution folds in
/// and the ack still terminalizes CANCELED with the updated qty (partial-fill-then-cancel race).
#[test]
fn pending_cancel_partial_fill_then_ack_cancels_with_quantity() {
    let mut engine = seeded_pending_cancel("p2", 0.0);
    let mut bus = EventBus::new();

    bus.publish(partial("p2", "t1", 0.4, 100.5), &mut engine);
    assert_eq!(engine.registry["p2"].status, OrderStatus::PartiallyFilled, "restored, fill folded");
    assert_eq!(engine.registry["p2"].filled_qty, 0.4);

    bus.publish(canceled("p2", "user", 0), &mut engine);
    assert_eq!(engine.registry["p2"].status, OrderStatus::Canceled, "ack still wins the remainder");
    assert_eq!(engine.registry["p2"].filled_qty, 0.4, "executed qty preserved");
}

/// PENDING_CANCEL + cancel-reject, nothing filled: restored to ACCEPTED (not left stuck at
/// PENDING_CANCEL, where fills would have been dropped forever), and a later fill lands.
#[test]
fn pending_cancel_reject_restores_accepted_and_fill_lands() {
    let mut engine = seeded_pending_cancel("p3", 0.0);
    let mut bus = EventBus::new();

    bus.publish(cancel_rejected("p3"), &mut engine);
    assert_eq!(engine.registry["p3"].status, OrderStatus::Accepted, "restored, not stuck");

    bus.publish(filled("p3", "t1", 1.0, 100.0), &mut engine);
    assert_eq!(engine.registry["p3"].status, OrderStatus::Filled);
}

/// PENDING_CANCEL + cancel-reject with fills already folded (the report carried filled_qty): the
/// restore recomputes from the fill stream — PARTIALLY_FILLED, not ACCEPTED.
#[test]
fn pending_cancel_reject_restores_partially_filled_from_quantity() {
    let mut engine = seeded_pending_cancel("p4", 0.5);
    let mut bus = EventBus::new();

    bus.publish(cancel_rejected("p4"), &mut engine);
    assert_eq!(
        engine.registry["p4"].status,
        OrderStatus::PartiallyFilled,
        "restore recomputed from filled_qty"
    );
    assert_eq!(engine.registry["p4"].filled_qty, 0.5);
}

/// PENDING_CANCEL + cancel-ack: the guard must NOT interfere with the legal ack edge —
/// PENDING_CANCEL → CANCELED directly, no restore.
#[test]
fn pending_cancel_ack_path_needs_no_restore() {
    let mut engine = seeded_pending_cancel("p5", 0.0);
    let mut bus = EventBus::new();

    bus.publish(canceled("p5", "user", 0), &mut engine);
    assert_eq!(engine.registry["p5"].status, OrderStatus::Canceled);
    assert_eq!(engine.closing_events_dropped_order_still_live, 0);
}

/// Scope discipline: the guard resolves ONLY on fill wraps and cancel-reject. Any other event on
/// a PENDING_CANCEL order (e.g. a stray OrderAccepted replay) is still dropped — the order stays
/// PENDING_CANCEL awaiting its real resolution.
#[test]
fn pending_cancel_is_not_resolved_by_other_events() {
    let mut engine = seeded_pending_cancel("p6", 0.0);
    let mut bus = EventBus::new();

    bus.publish(accepted("p6"), &mut engine);
    assert_eq!(
        engine.registry["p6"].status,
        OrderStatus::PendingCancel,
        "a stray accept replay does not resolve the pending cancel"
    );
}

/// The guard adds NO state: an engine that just resolved a race snapshots and restores
/// byte-identically through the existing `EngineSnapshot` (state-hash determinism intact).
#[test]
fn resolved_race_round_trips_through_engine_snapshot() {
    let mut engine = seeded_pending_cancel("p7", 0.0);
    let mut bus = EventBus::new();
    bus.publish(partial("p7", "t1", 0.4, 100.5), &mut engine);

    let snap = engine.snapshot_state();
    let restored: Engine<RecordingClient> =
        Engine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(restored.registry["p7"].status, OrderStatus::PartiallyFilled);
    assert_eq!(restored.registry["p7"].filled_qty, 0.4);
    assert_eq!(
        vike_execution::state_hash(std::slice::from_ref(&snap)),
        vike_execution::state_hash(&[restored.snapshot_state()]),
        "state-hash determinism"
    );
}

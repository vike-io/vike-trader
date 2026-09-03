//! Cancel-vs-fill race guard (LEAN `CancelPendingOrders` semantics, reimplemented).
//!
//! Two halves, matching what vike ALREADY did vs the missing delta:
//!
//! 1. LIVE-PATH PINS — `ExecutionEngine::cancel_order` is fire-and-forget and never mutates
//!    status, so the pre-cancel status IS the LEAN "snapshot" (never clobbered): a fill racing
//!    the in-flight cancel applies normally, a venue cancel-reject leaves the order in its live
//!    state (with any meanwhile-fills already folded), and a cancel-ack terminalizes CANCELED.
//!    These tests pin that equivalence at the ENGINE level (the FSM-level advisory behavior was
//!    already unit-tested in `order.rs`).
//!
//! 2. THE DELTA — an order CAN sit at PENDING_CANCEL via venue-status seeding
//!    (`reregister_orders`; binance recon passes the venue's `PENDING_CANCEL` string through
//!    verbatim), and the r5 fixture-pinned FSM table rejects fills from that state and leaves a
//!    cancel-reject inert: venue truth was DROPPED (stranding the order until a reconcile reap
//!    mislabeled a filled order CANCELED). The engine fold now restores the pre-cancel live
//!    status (recomputed from `filled_qty` — `ManagedOrder::resolve_pending_cancel`) before the
//!    one `apply` site, ONLY for the events that prove the cancel lost or died (fill wraps,
//!    cancel-reject). The FSM transition table itself is untouched (golden fixture).

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventBus, ExecutionEngine, OrderStatus, Outbox, RiskGate, RiskLimits,
};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderFilled,
    OrderPartiallyFilled, OrderSubmitted,
};
use vike_model::OrderRequest;

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    )
}

fn req(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    }
}

fn fill(coid: &str, trade_id: &'static str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: coid.to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    }
}

fn submitted(coid: &str) -> Event {
    Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.into(), ts: 0 })
}

fn accepted(coid: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.into(),
        venue_order_id: Some("v1".into()),
        ts: 0,
    })
}

fn partial(coid: &str, trade_id: &'static str, qty: f64, px: f64) -> Event {
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        client_order_id: coid.into(),
        fill: fill(coid, trade_id, qty, px),
        ts: 0,
    })
}

fn filled(coid: &str, trade_id: &'static str, qty: f64, px: f64) -> Event {
    Event::OrderFilled(OrderFilled {
        client_order_id: coid.into(),
        fill: fill(coid, trade_id, qty, px),
        ts: 0,
    })
}

fn canceled(coid: &str) -> Event {
    Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.into(),
        reason: "user".to_string().into(),
        ts: 0,
    })
}

fn cancel_rejected(coid: &str) -> Event {
    Event::OrderCancelRejected(OrderCancelRejected {
        client_order_id: coid.into(),
        reason: "order does not exist / too late to cancel".into(),
        ts: 0,
    })
}

/// Submit + venue-accept one resting order, then issue the cancel intent (fire-and-forget).
fn resting_with_cancel_in_flight(coid: &str) -> ExecutionEngine<RecordingClient> {
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req(coid), 1, &mut outbox);
    // The venue-adapter lane's emitter split: the adapter emits OrderSubmitted at submit, then
    // the venue acks — replay both so the order rests ACCEPTED before the cancel intent.
    bus.publish(submitted(coid), &mut eng);
    bus.publish(accepted(coid), &mut eng);
    eng.cancel_order(coid);
    assert_eq!(
        eng.client.cancels.as_slice(),
        &[coid.to_string()],
        "cancel intent reached the venue client"
    );
    // The LEAN pending-cancels "snapshot": vike keeps it implicitly — the status is untouched.
    assert_eq!(eng.registry[coid].status, OrderStatus::Accepted, "cancel intent never clobbers");
    eng
}

/// Seed one order at PENDING_CANCEL through the REAL production path: an operator-confirmed
/// `reregister_orders` recovery whose venue report carries the venue's own PENDING_CANCEL status
/// (binance recon normalization passes it through verbatim).
fn seeded_pending_cancel(coid: &str, filled_qty: f64) -> ExecutionEngine<RecordingClient> {
    let mut eng = engine();
    let n = eng.reregister_orders(&[vike_model::OrderStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v1".into(),
        client_order_id: Some(coid.into()),
        side: 1,
        order_type: "limit".into(),
        qty: 1.0,
        filled_qty,
        avg_px: if filled_qty > 0.0 { 100.0 } else { 0.0 },
        status: "PENDING_CANCEL".into(),
        ts: 5,
    }]);
    assert_eq!(n, 1);
    assert_eq!(eng.registry[coid].status, OrderStatus::PendingCancel, "seeded at PENDING_CANCEL");
    eng
}

// ------------------------------------------------------------------------------------------
// 1. Live-path pins: cancel intent never clobbers, so LEAN's restore is implicit
// ------------------------------------------------------------------------------------------

/// cancel → fill-before-ack: the venue filled the order before honoring the cancel. The fill must
/// apply normally (NOT be dropped as an invalid transition) and the order completes FILLED; the
/// venue's late cancel-ack (now impossible) is dropped benignly without touching the terminal.
#[test]
fn cancel_then_fill_before_ack_completes_filled() {
    let mut eng = resting_with_cancel_in_flight("c1");
    let mut bus = EventBus::new();

    bus.publish(filled("c1", "t1", 1.0, 100.0), &mut eng);
    assert_eq!(eng.registry["c1"].status, OrderStatus::Filled, "fill won the race");
    assert_eq!(eng.registry["c1"].filled_qty, 1.0);

    // A straggler cancel-ack for the already-filled order: dropped, terminal untouched, and NOT
    // counted as a lost terminal (the order IS terminal — a benign out-of-order artifact).
    bus.publish(canceled("c1"), &mut eng);
    assert_eq!(eng.registry["c1"].status, OrderStatus::Filled, "terminal stands");
    assert_eq!(eng.dropped_terminal_on_live, 0);
    assert_eq!(eng.stranded_terminal_drops, 0);
}

/// cancel → reject: the venue refused the cancel. The order must be back (= still) in its
/// pre-cancel status, and a subsequent fill still lands.
#[test]
fn cancel_reject_restores_status_and_subsequent_fill_lands() {
    let mut eng = resting_with_cancel_in_flight("c2");
    let mut bus = EventBus::new();

    bus.publish(cancel_rejected("c2"), &mut eng);
    assert_eq!(
        eng.registry["c2"].status,
        OrderStatus::Accepted,
        "pre-cancel status restored (never clobbered)"
    );

    bus.publish(filled("c2", "t1", 1.0, 100.0), &mut eng);
    assert_eq!(eng.registry["c2"].status, OrderStatus::Filled, "fill after reject lands");
    assert_eq!(eng.registry["c2"].filled_qty, 1.0);
}

/// cancel → partial-fill → reject: fills that arrive while the cancel is pending fold into the
/// order, and the reject leaves it PARTIALLY_FILLED with the updated qty — after which the
/// remainder can still fill to completion.
#[test]
fn cancel_partial_fill_then_reject_keeps_updated_qty() {
    let mut eng = resting_with_cancel_in_flight("c3");
    let mut bus = EventBus::new();

    bus.publish(partial("c3", "t1", 0.4, 100.5), &mut eng);
    assert_eq!(eng.registry["c3"].status, OrderStatus::PartiallyFilled, "mid-cancel fill applies");
    assert_eq!(eng.registry["c3"].filled_qty, 0.4);

    bus.publish(cancel_rejected("c3"), &mut eng);
    assert_eq!(
        eng.registry["c3"].status,
        OrderStatus::PartiallyFilled,
        "restored WITH the meanwhile-fill folded in"
    );
    assert_eq!(eng.registry["c3"].filled_qty, 0.4, "fill accumulation survives the reject");

    bus.publish(filled("c3", "t2", 0.6, 101.0), &mut eng);
    assert_eq!(eng.registry["c3"].status, OrderStatus::Filled);
    assert_eq!(eng.registry["c3"].filled_qty, 1.0);
}

/// cancel → ack: the normal path — the venue's authoritative OrderCanceled terminalizes CANCELED.
#[test]
fn cancel_ack_terminalizes_canceled() {
    let mut eng = resting_with_cancel_in_flight("c4");
    let mut bus = EventBus::new();

    bus.publish(canceled("c4"), &mut eng);
    assert_eq!(eng.registry["c4"].status, OrderStatus::Canceled);
    assert_eq!(eng.dropped_terminal_on_live, 0);
}

// ------------------------------------------------------------------------------------------
// 2. The delta: venue-seeded PENDING_CANCEL orders (the previously-stranding race)
// ------------------------------------------------------------------------------------------

/// PENDING_CANCEL + full fill wrap: the cancel lost the race outright. Pre-guard this fill was
/// DROPPED (fixture-pinned invalid transition) and counted as a lost terminal, stranding the
/// order; now the guard restores the pre-cancel status and the fill applies to FILLED.
#[test]
fn pending_cancel_fill_wrap_applies_instead_of_stranding() {
    let mut eng = seeded_pending_cancel("p1", 0.0);
    let mut bus = EventBus::new();

    bus.publish(filled("p1", "t1", 1.0, 100.0), &mut eng);
    assert_eq!(eng.registry["p1"].status, OrderStatus::Filled, "venue truth applies");
    assert_eq!(eng.registry["p1"].filled_qty, 1.0);
    assert_eq!(eng.dropped_terminal_on_live, 0, "nothing dropped — nothing to count");
    assert_eq!(eng.stranded_terminal_drops, 0);
}

/// PENDING_CANCEL + partial fill, then the cancel-ack arrives: the mid-cancel execution folds in
/// and the ack still terminalizes CANCELED with the updated qty (partial-fill-then-cancel race).
#[test]
fn pending_cancel_partial_fill_then_ack_cancels_with_qty() {
    let mut eng = seeded_pending_cancel("p2", 0.0);
    let mut bus = EventBus::new();

    bus.publish(partial("p2", "t1", 0.4, 100.5), &mut eng);
    assert_eq!(eng.registry["p2"].status, OrderStatus::PartiallyFilled, "restored, fill folded");
    assert_eq!(eng.registry["p2"].filled_qty, 0.4);

    bus.publish(canceled("p2"), &mut eng);
    assert_eq!(eng.registry["p2"].status, OrderStatus::Canceled, "ack still wins the remainder");
    assert_eq!(eng.registry["p2"].filled_qty, 0.4, "executed qty preserved");
}

/// PENDING_CANCEL + cancel-reject, nothing filled: restored to ACCEPTED (not left stuck at
/// PENDING_CANCEL, where fills would have been dropped forever), and a later fill lands.
#[test]
fn pending_cancel_reject_restores_accepted_and_fill_lands() {
    let mut eng = seeded_pending_cancel("p3", 0.0);
    let mut bus = EventBus::new();

    bus.publish(cancel_rejected("p3"), &mut eng);
    assert_eq!(eng.registry["p3"].status, OrderStatus::Accepted, "restored, not stuck");

    bus.publish(filled("p3", "t1", 1.0, 100.0), &mut eng);
    assert_eq!(eng.registry["p3"].status, OrderStatus::Filled);
}

/// PENDING_CANCEL + cancel-reject with fills already folded (the report carried filled_qty): the
/// restore recomputes from the fill stream — PARTIALLY_FILLED, not ACCEPTED.
#[test]
fn pending_cancel_reject_restores_partially_filled_from_qty() {
    let mut eng = seeded_pending_cancel("p4", 0.5);
    let mut bus = EventBus::new();

    bus.publish(cancel_rejected("p4"), &mut eng);
    assert_eq!(
        eng.registry["p4"].status,
        OrderStatus::PartiallyFilled,
        "restore recomputed from filled_qty"
    );
    assert_eq!(eng.registry["p4"].filled_qty, 0.5);
}

/// PENDING_CANCEL + cancel-ack: the guard must NOT interfere with the legal ack edge —
/// PENDING_CANCEL → CANCELED directly, no restore.
#[test]
fn pending_cancel_ack_path_needs_no_restore() {
    let mut eng = seeded_pending_cancel("p5", 0.0);
    let mut bus = EventBus::new();

    bus.publish(canceled("p5"), &mut eng);
    assert_eq!(eng.registry["p5"].status, OrderStatus::Canceled);
    assert_eq!(eng.dropped_terminal_on_live, 0);
}

/// Scope discipline: the guard resolves ONLY on fill wraps and cancel-reject. Any other event on
/// a PENDING_CANCEL order (e.g. a stray OrderAccepted replay) keeps today's drop — the order
/// stays PENDING_CANCEL awaiting its real resolution.
#[test]
fn pending_cancel_is_not_resolved_by_other_events() {
    let mut eng = seeded_pending_cancel("p6", 0.0);
    let mut bus = EventBus::new();

    bus.publish(accepted("p6"), &mut eng);
    assert_eq!(
        eng.registry["p6"].status,
        OrderStatus::PendingCancel,
        "a stray accept replay does not resolve the pending cancel"
    );
}

/// The guard adds NO state: an engine that just resolved a race snapshots and restores
/// byte-identically through the existing `EngineSnapshot` (state-hash determinism intact).
#[test]
fn resolved_race_round_trips_through_engine_snapshot() {
    let mut eng = seeded_pending_cancel("p7", 0.0);
    let mut bus = EventBus::new();
    bus.publish(partial("p7", "t1", 0.4, 100.5), &mut eng);

    let snap = eng.snapshot_state();
    let restored: ExecutionEngine<RecordingClient> =
        ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(restored.registry["p7"].status, OrderStatus::PartiallyFilled);
    assert_eq!(restored.registry["p7"].filled_qty, 0.4);
    assert_eq!(
        vike_exec::state_hash(std::slice::from_ref(&snap)),
        vike_exec::state_hash(&[restored.snapshot_state()]),
        "state-hash determinism"
    );
}

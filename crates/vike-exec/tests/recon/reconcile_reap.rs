//! Continuous-reconcile REAP (the second reconcile gap): `ExecutionEngine::apply_snapshot` used to
//! be INSERT-ONLY — it seeded venue-reported open orders but never closed a local order the venue
//! had stopped reporting, so a lost cancel-ack left a phantom-live local order forever. The reap
//! now terminalizes any order WE still hold live whose client_order_id is absent from the venue's
//! open-order set, by synthesizing a local `OrderCanceled` and driving it through the SAME FSM path
//! the live fold uses (so a mounted strategy learns of the cancel via `order_events`). These tests
//! pin: (a) an absent live order IS reaped and the mount sees it, (b) a still-reported order is
//! untouched, (c) re-applying the same snapshot is idempotent, (d) a venue-only order still seeds
//! (the insert path is not regressed).

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, ExecutionEngine, ManagedOrder, OrderStatus, ReconcileSnapshot, RiskGate,
    RiskLimits,
};
use vike_model::OrderRequest;
use vike_model::strategy::OrderEventKind;

fn engine() -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    // A strategy is mounted → lifecycle transitions are captured into `order_events` (the same flag
    // the live runtime sets), so we can assert the reaped cancel is DELIVERED, not just applied.
    e.collect_applied_fills = true;
    e
}

/// An ACCEPTED (live) resting order on this engine's own venue, shaped like a venue open-order row.
fn open_order(coid: &str) -> ManagedOrder {
    let req: OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "sim", "symbol": "BTCUSDT",
        "side": 1, "qty": 1.0, "order_type": "limit", "price": 100.0
    }))
    .unwrap();
    let mut mo = ManagedOrder::new(req);
    mo.status = OrderStatus::Accepted;
    mo
}

fn status(e: &ExecutionEngine<RecordingClient>, coid: &str) -> Option<OrderStatus> {
    e.registry.get(coid).map(|mo| mo.status)
}

fn saw_cancel(e: &ExecutionEngine<RecordingClient>, coid: &str) -> bool {
    e.order_events.iter().any(|oe| {
        oe.event.client_order_id == coid && matches!(oe.event.kind, OrderEventKind::Canceled { .. })
    })
}

/// (a) A live local order that the venue no longer reports open is terminalized THROUGH the FSM
/// (status → CANCELED) and the mounted strategy is notified via `order_events`.
#[test]
fn absent_live_order_is_reaped_and_delivered() {
    let mut e = engine();
    // Seed the order as live via a first snapshot (the insert path).
    e.apply_snapshot(&ReconcileSnapshot {
        open_orders: vec![open_order("live1")],
        ..Default::default()
    });
    assert_eq!(status(&e, "live1"), Some(OrderStatus::Accepted), "seeded live");
    e.order_events.clear(); // ignore any seeding noise; only the reap should follow

    // Venue truth: the order is GONE (empty open_orders).
    e.apply_snapshot(&ReconcileSnapshot::default());

    assert_eq!(status(&e, "live1"), Some(OrderStatus::Canceled), "reaped via the FSM");
    assert!(saw_cancel(&e, "live1"), "the mount saw the synthesized cancel: {:?}", e.order_events);
}

/// (b) A live local order the venue STILL reports open is left untouched (venue truth = still live).
#[test]
fn still_reported_order_is_untouched() {
    let mut e = engine();
    e.apply_snapshot(&ReconcileSnapshot {
        open_orders: vec![open_order("keep1")],
        ..Default::default()
    });
    e.order_events.clear();

    // Same order still present in the next snapshot → NOT reaped.
    e.apply_snapshot(&ReconcileSnapshot {
        open_orders: vec![open_order("keep1")],
        ..Default::default()
    });

    assert_eq!(status(&e, "keep1"), Some(OrderStatus::Accepted), "still live");
    assert!(!saw_cancel(&e, "keep1"), "no cancel delivered for a still-open order");
}

/// (c) Re-applying the SAME (order-absent) snapshot is idempotent: the order is already terminal, so
/// it is no longer a reap candidate and no second cancel is delivered.
#[test]
fn re_applying_absent_snapshot_is_idempotent() {
    let mut e = engine();
    e.apply_snapshot(&ReconcileSnapshot {
        open_orders: vec![open_order("live2")],
        ..Default::default()
    });
    e.apply_snapshot(&ReconcileSnapshot::default()); // first reap → CANCELED
    assert_eq!(status(&e, "live2"), Some(OrderStatus::Canceled));
    e.order_events.clear();

    e.apply_snapshot(&ReconcileSnapshot::default()); // second application — no-op
    assert_eq!(status(&e, "live2"), Some(OrderStatus::Canceled), "stays terminal");
    assert!(!saw_cancel(&e, "live2"), "no second cancel — idempotent: {:?}", e.order_events);
}

/// (d) A snapshot carrying a venue-only order still SEEDS it (the insert path is not regressed by the
/// reap): the newly reported order lands ACCEPTED and is never reaped in the same pass.
#[test]
fn venue_only_order_is_still_seeded() {
    let mut e = engine();
    e.apply_snapshot(&ReconcileSnapshot {
        open_orders: vec![open_order("seed1")],
        ..Default::default()
    });
    assert_eq!(status(&e, "seed1"), Some(OrderStatus::Accepted), "venue-only order seeded live");
    assert!(!saw_cancel(&e, "seed1"), "a freshly-seeded order is not reaped in the same snapshot");
}

/// The reap only fires for orders in a CANCELABLE state — a still-`Submitted` (pre-ack) local order
/// is live but not cancelable, so the FSM drops the synthetic cancel and the order survives (the
/// pre-ack race self-guard). This proves the reap gate + FSM legality compose correctly.
#[test]
fn pre_ack_submitted_order_is_not_reaped() {
    let mut e = engine();
    let mut submitted = open_order("pending1");
    submitted.status = OrderStatus::Submitted; // live, but the venue has not acked it
    e.apply_snapshot(&ReconcileSnapshot { open_orders: vec![submitted], ..Default::default() });
    e.order_events.clear();

    // Venue reports nothing — but a Submitted order is not cancelable, so the FSM drops the cancel.
    e.apply_snapshot(&ReconcileSnapshot::default());
    assert_eq!(status(&e, "pending1"), Some(OrderStatus::Submitted), "pre-ack order survives");
    assert!(!saw_cancel(&e, "pending1"), "no cancel delivered for a dropped invalid transition");
}

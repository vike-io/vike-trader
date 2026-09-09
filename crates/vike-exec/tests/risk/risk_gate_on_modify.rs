//! The MODIFY path is risk-gated — the threat-model hole where `ExecutionEngine::modify_order`
//! never called the `RiskGate` at all.
//!
//! `self.gate.check(..)` appeared EXACTLY ONCE in `crates/vike-exec/src/execution_engine/mod.rs`,
//! on the submit path, so every per-order ceiling the gate enforces — `max_notional_per_order`
//! (the `policy.toml` ceiling), `max_total_exposure`, the halt/reduce-only trading state — was
//! simply absent from modify. The reachable attack from either remote write surface was two
//! commands: place a SMALL, in-cap order, then modify it up. Nothing in between ran.
//!
//! These tests drive the REAL `ExecutionEngine` through the REAL `RiskGate` and assert on what
//! reached the venue (`RecordingClient::modifies`), not on an internal call count — a gate that is
//! called but ignored fails here just as loudly as one that is never called.

use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, EventBus, ExecutionEngine, Outbox, RiskGate, RiskLimits};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};

/// Cap the per-order notional at 250 — the same shape `RiskLimits::from_max_notional` builds from
/// `policy.toml`'s `max_notional_per_order`.
fn engine_capped_at(max_notional: f64) -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits {
            max_notional_per_order: Some(max_notional),
            ..RiskLimits::new()
        }),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

fn limit_buy(coid: &str, qty: f64, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(price),
        ts: 1,
        ..Default::default()
    }
}

/// Submit `req` and drive it to ACCEPTED, so it is `is_modifiable()` and the modify path is
/// actually reachable (a SUBMITTED or terminal order is a documented no-op).
fn rest_an_accepted_order(e: &mut ExecutionEngine<RecordingClient>, req: &OrderRequest) {
    let mut outbox = Outbox::default();
    e.submit_order(req, 0, &mut outbox);
    assert_eq!(
        e.client.submissions.len(),
        1,
        "precondition: the in-cap order must itself be admitted, or the test proves nothing"
    );
    // The venue-adapter emitter split: the adapter emits OrderSubmitted at submit, then the venue
    // acks — replay both so the order rests ACCEPTED and is therefore `is_modifiable()`.
    let coid = req.client_order_id.clone();
    let mut bus = EventBus::new();
    bus.publish(Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.clone(), ts: 0 }), e);
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: Some("v1".into()),
            ts: 1,
        }),
        e,
    );
}

fn modify_rejection(outbox: &Outbox) -> Option<String> {
    outbox.0.iter().find_map(|e| match e {
        Event::OrderModifyRejected(r) => Some(r.reason.to_string()),
        _ => None,
    })
}

/// THE HOLE: a small in-cap order, modified UP past the per-order notional ceiling.
///
/// Before the fix this test's `modifies` assertion failed — the raise sailed through to the venue
/// with no verdict of any kind, because `modify_order` cloned the resting request and called
/// `client.modify` directly.
#[test]
fn a_qty_raise_past_the_notional_ceiling_is_denied() {
    let mut e = engine_capped_at(250.0);
    // 1 @ 100 = notional 100, comfortably inside the 250 cap.
    let req = limit_buy("c1", 1.0, 100.0);
    rest_an_accepted_order(&mut e, &req);

    // now raise it to 10 @ 100 = 1000, four times the ceiling
    let mut outbox = Outbox::default();
    e.modify_order("c1", Some(10.0), None, 2, &mut outbox);

    assert!(
        e.client.modifies.is_empty(),
        "an over-cap modify must NOT reach the venue, got {:?}",
        e.client.modifies
    );
    let reason = modify_rejection(&outbox)
        .expect("a denied modify must publish OrderModifyRejected, never vanish silently");
    assert!(
        reason.contains("over-max-notional"),
        "the rejection must name the lane that vetoed it, got {reason:?}"
    );
}

/// The PRICE half of the same hole: qty untouched, price raised past the ceiling.
#[test]
fn a_price_raise_past_the_notional_ceiling_is_denied() {
    let mut e = engine_capped_at(250.0);
    let req = limit_buy("c1", 2.0, 100.0); // notional 200, in cap
    rest_an_accepted_order(&mut e, &req);

    let mut outbox = Outbox::default();
    e.modify_order("c1", None, Some(500.0), 2, &mut outbox); // 2 @ 500 = 1000
    assert!(e.client.modifies.is_empty(), "an over-cap price raise must not reach the venue");
    assert!(modify_rejection(&outbox).is_some(), "and it must say so");
}

/// The projected notional is judged on the ORDER AS MODIFIED, not on the delta and not on the
/// resting terms. `new_qty` alone must be priced with the RESTING price — a gate that read only
/// the modify's own (absent) price would see notional 0 and admit anything.
#[test]
fn the_projected_order_is_judged_not_the_delta() {
    let mut e = engine_capped_at(250.0);
    let req = limit_buy("c1", 1.0, 100.0);
    rest_an_accepted_order(&mut e, &req);

    // The DELTA is +1 (notional 100 if judged as a delta — inside the cap). The PROJECTED order is
    // 2 @ 100 = 200, also inside. Both readings admit, so this case cannot discriminate; it is here
    // as the in-cap control for the discriminating case below.
    let mut ok = Outbox::default();
    e.modify_order("c1", Some(2.0), None, 2, &mut ok);
    assert_eq!(e.client.modifies.len(), 1, "an in-cap modify must still reach the venue");
    assert!(modify_rejection(&ok).is_none(), "and must not be rejected");

    // Now the discriminating case: delta +1 again (2 -> 3), which a delta-reading gate would admit
    // at notional 100, but the PROJECTED order is 3 @ 100 = 300 > 250 and must be denied.
    let mut denied = Outbox::default();
    e.modify_order("c1", Some(3.0), None, 3, &mut denied);
    assert_eq!(
        e.client.modifies.len(),
        1,
        "the projected-order reading must deny 3 @ 100; a delta reading would have admitted it"
    );
    assert!(modify_rejection(&denied).is_some());
}

/// A modify that stays inside every limit is untouched — the gate must not become a blanket
/// refusal, and an ACCEPTED modify still goes to the venue with the CALLER's values (the verdict is
/// a veto, not a rewrite).
#[test]
fn an_in_cap_modify_still_reaches_the_venue_verbatim() {
    let mut e = engine_capped_at(250.0);
    let req = limit_buy("c1", 1.0, 100.0);
    rest_an_accepted_order(&mut e, &req);

    let mut outbox = Outbox::default();
    e.modify_order("c1", Some(2.0), Some(110.0), 2, &mut outbox);
    assert_eq!(
        e.client.modifies,
        vec![("c1".to_string(), Some(2.0), Some(110.0))],
        "an admitted modify reaches the venue with the caller's own values"
    );
    assert!(modify_rejection(&outbox).is_none());
}

/// A denied modify leaves the order EXACTLY as it was: still live, still modifiable, still carrying
/// its original terms. `OrderModifyRejected` is non-terminal by contract, and the veto must not
/// strand the order the way a spurious terminal would.
#[test]
fn a_denied_modify_leaves_the_resting_order_live_and_unchanged() {
    let mut e = engine_capped_at(250.0);
    let req = limit_buy("c1", 1.0, 100.0);
    rest_an_accepted_order(&mut e, &req);

    let mut outbox = Outbox::default();
    e.modify_order("c1", Some(99.0), None, 2, &mut outbox);
    assert!(modify_rejection(&outbox).is_some(), "precondition: this modify is denied");

    // the order is still there, still live, still on its ORIGINAL terms
    let mo = e.registry.get("c1").expect("a denied modify must not evict the order");
    assert_eq!(mo.request.qty, 1.0, "the resting qty must be untouched by a denied modify");
    assert!(mo.status.is_modifiable(), "and the order must still be modifiable");

    // ...and a later IN-cap modify still works, so the veto did not wedge the order
    let mut ok = Outbox::default();
    e.modify_order("c1", Some(2.0), None, 3, &mut ok);
    assert_eq!(e.client.modifies.len(), 1, "a subsequent in-cap modify is admitted");
}

/// A modify must NOT consume a rate-limit slot: the resting order already paid one at submit, and
/// an amend-heavy maker (`vike_mm`'s `SpreadMaker` requotes continuously) would otherwise throttle
/// itself out of quoting for doing exactly what it exists to do. This pins the one deliberate
/// difference between `check_modify` and `check`.
#[test]
fn a_modify_does_not_consume_an_order_rate_slot() {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits {
            // exactly ONE order per window: the submit below consumes it
            max_orders_per_window: Some(1),
            window_ms: 60_000,
            ..RiskLimits::new()
        }),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let req = limit_buy("c1", 1.0, 100.0);
    rest_an_accepted_order(&mut e, &req);

    // the single slot is now spent; a modify must still be admitted
    for (i, qty) in [2.0, 3.0, 4.0].into_iter().enumerate() {
        let mut outbox = Outbox::default();
        e.modify_order("c1", Some(qty), None, 2 + i as i64, &mut outbox);
        assert!(
            modify_rejection(&outbox).is_none(),
            "modify {qty} must not be throttled — the resting order already paid its slot"
        );
    }
    assert_eq!(e.client.modifies.len(), 3, "all three amends reached the venue");
}

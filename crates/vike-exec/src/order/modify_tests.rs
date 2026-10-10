use super::*;
use vike_model::events::{OrderAccepted, OrderModified, OrderSubmitted};

fn limit_order() -> ManagedOrder {
    ManagedOrder::new(OrderRequest {
        client_order_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ..Default::default()
    })
}

fn submitted(coid: &str) -> Event {
    Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.into(), ts: 0 })
}
fn accepted(coid: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.into(),
        venue_order_id: None,
        ts: 0,
    })
}
fn modify(coid: &str, q: Option<f64>, p: Option<f64>) -> Event {
    Event::OrderModified(OrderModified {
        client_order_id: coid.into(),
        venue_order_id: None,
        new_qty: q,
        new_price: p,
        ts: 0,
    })
}

fn cancel_rejected(coid: &str) -> Event {
    Event::OrderCancelRejected(vike_model::events::OrderCancelRejected {
        client_order_id: coid.into(),
        reason: "network error".into(),
        ts: 0,
    })
}
fn modify_rejected(coid: &str) -> Event {
    Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
        client_order_id: coid.into(),
        reason: "venue error".into(),
        ts: 0,
    })
}

#[test]
fn cancel_rejected_is_advisory_and_keeps_status() {
    let mut o = limit_order();
    o.apply(&submitted("c")).unwrap();
    o.apply(&accepted("c")).unwrap();
    o.apply(&cancel_rejected("c")).unwrap();
    assert_eq!(
        o.status,
        OrderStatus::Accepted,
        "cancel-reject is non-terminal — the order stays live"
    );
}

#[test]
fn cancel_rejected_before_accept_is_error() {
    let mut o = limit_order();
    o.apply(&submitted("c")).unwrap(); // not cancelable at the venue yet
    assert!(o.apply(&cancel_rejected("c")).is_err());
}

#[test]
fn cancel_rejected_after_terminal_is_error() {
    let mut o = limit_order();
    o.apply(&submitted("c")).unwrap();
    o.apply(&Event::OrderRejected(vike_model::events::OrderRejected {
        client_order_id: "c".into(),
        reason: "x".into(),
        ts: 0,
    }))
    .unwrap();
    assert!(o.apply(&cancel_rejected("c")).is_err(), "no reject after terminal");
}

#[test]
fn modify_rejected_is_advisory_and_keeps_terms() {
    let mut o = limit_order();
    o.apply(&submitted("c")).unwrap();
    o.apply(&accepted("c")).unwrap();
    o.apply(&modify_rejected("c")).unwrap();
    assert_eq!(o.status, OrderStatus::Accepted, "modify-reject is non-terminal");
    assert_eq!(o.request.qty, 1.0, "terms untouched");
    assert_eq!(o.request.price, Some(100.0));
}

#[test]
fn modify_rewrites_resting_terms_and_keeps_status() {
    let mut o = limit_order();
    o.apply(&submitted("c")).unwrap();
    o.apply(&accepted("c")).unwrap();
    o.apply(&modify("c", Some(2.0), Some(101.0))).unwrap();
    assert_eq!(o.status, OrderStatus::Accepted, "modify is a self-transition");
    assert_eq!(o.request.qty, 2.0);
    assert_eq!(o.request.price, Some(101.0));
}

#[test]
fn modify_partial_none_fields_leave_terms_unchanged() {
    let mut o = limit_order();
    o.apply(&submitted("c")).unwrap();
    o.apply(&accepted("c")).unwrap();
    o.apply(&modify("c", None, Some(105.0))).unwrap(); // price-only re-quote
    assert_eq!(o.request.qty, 1.0, "qty untouched when new_qty is None");
    assert_eq!(o.request.price, Some(105.0));
}

#[test]
fn modify_rejected_before_accept() {
    let mut o = limit_order();
    o.apply(&submitted("c")).unwrap(); // SUBMITTED — not yet resting at the venue
    assert!(o.apply(&modify("c", Some(2.0), None)).is_err());
    assert_eq!(o.request.qty, 1.0, "rejected modify must not mutate terms");
}

#[test]
fn modify_rejected_when_terminal() {
    let mut o = limit_order();
    o.apply(&submitted("c")).unwrap();
    o.apply(&Event::OrderRejected(vike_model::events::OrderRejected {
        client_order_id: "c".into(),
        reason: "x".into(),
        ts: 0,
    }))
    .unwrap();
    assert_eq!(o.status, OrderStatus::Rejected);
    assert!(o.apply(&modify("c", Some(2.0), Some(9.0))).is_err());
}

#[test]
fn resolve_pending_cancel_restores_accepted_when_nothing_filled() {
    let mut o = limit_order();
    o.status = OrderStatus::PendingCancel; // venue-status seeded (reregister/recon)
    assert!(o.resolve_pending_cancel(), "PENDING_CANCEL is restored");
    assert_eq!(o.status, OrderStatus::Accepted, "no fills folded → ACCEPTED");
}

#[test]
fn resolve_pending_cancel_restores_partially_filled_when_qty_folded() {
    let mut o = limit_order();
    o.status = OrderStatus::PendingCancel;
    o.filled_qty = 0.5; // fills arrived while the cancel was pending
    o.avg_fill_px = 100.0;
    assert!(o.resolve_pending_cancel());
    assert_eq!(o.status, OrderStatus::PartiallyFilled, "recomputed from the folded fill stream");
    assert_eq!(o.filled_qty, 0.5, "fill accumulation untouched");
    assert_eq!(o.avg_fill_px, 100.0);
}

#[test]
fn resolve_pending_cancel_is_a_noop_on_any_other_status() {
    for status in [
        OrderStatus::Initialized,
        OrderStatus::Submitted,
        OrderStatus::Accepted,
        OrderStatus::Triggered,
        OrderStatus::PartiallyFilled,
        OrderStatus::Filled,
        OrderStatus::Canceled,
        OrderStatus::Rejected,
    ] {
        let mut o = limit_order();
        o.status = status;
        assert!(!o.resolve_pending_cancel(), "no restore from {}", status.as_str());
        assert_eq!(o.status, status, "status untouched for {}", status.as_str());
    }
}

#[test]
fn modify_of_stop_moves_the_trigger_not_the_limit() {
    let mut o = ManagedOrder::new(OrderRequest {
        client_order_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: 1,
        qty: 1.0,
        order_type: "stop".into(),
        trigger_price: Some(100.0),
        ..Default::default()
    });
    o.apply(&submitted("c")).unwrap();
    o.apply(&accepted("c")).unwrap();
    o.apply(&modify("c", None, Some(110.0))).unwrap();
    assert_eq!(o.request.trigger_price, Some(110.0));
    assert_eq!(o.request.price, None, "limit price stays unset for a stop");
}

use super::*;
use crate::events::{OrderAccepted, OrderModified, OrderSubmitted};

/// A resting buy limit of `qty` at 100 (`lost_prefix_tests` and `fill_accumulation_tests` build their orders with it too).
pub(super) fn limit_order(qty: f64) -> OrderRecord {
    OrderRecord::new(OrderRequest {
        order_client_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    })
}

fn submitted(order_client_id: &str) -> Event {
    Event::OrderSubmitted(OrderSubmitted { order_client_id: order_client_id.into(), timestamp: 0 })
}
fn accepted(order_client_id: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.into(),
        venue_order_id: None,
        timestamp: 0,
    })
}
fn modify(order_client_id: &str, q: Option<f64>, p: Option<f64>) -> Event {
    Event::OrderModified(OrderModified {
        order_client_id: order_client_id.into(),
        venue_order_id: None,
        new_qty: q,
        new_price: p,
        new_trigger_price: None,
        timestamp: 0,
    })
}

fn cancel_rejected(order_client_id: &str) -> Event {
    Event::OrderCancelRejected(crate::events::OrderCancelRejected {
        order_client_id: order_client_id.into(),
        reason: "network error".into(),
        timestamp: 0,
    })
}
fn modify_rejected(order_client_id: &str) -> Event {
    Event::OrderModifyRejected(crate::events::OrderModifyRejected {
        order_client_id: order_client_id.into(),
        reason: "venue error".into(),
        timestamp: 0,
    })
}

#[test]
fn cancel_rejected_is_advisory_and_keeps_status() {
    let mut o = limit_order(1.0);
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
    let mut o = limit_order(1.0);
    o.apply(&submitted("c")).unwrap(); // not cancelable at the venue yet
    assert!(o.apply(&cancel_rejected("c")).is_err());
}

#[test]
fn cancel_rejected_after_terminal_is_error() {
    let mut o = limit_order(1.0);
    o.apply(&submitted("c")).unwrap();
    o.apply(&Event::OrderRejected(crate::events::OrderRejected {
        cause: None,
        order_client_id: "c".into(),
        reason: "x".into(),
        timestamp: 0,
    }))
    .unwrap();
    assert!(o.apply(&cancel_rejected("c")).is_err(), "no reject after terminal");
}

#[test]
fn modify_rejected_is_advisory_and_keeps_terms() {
    let mut o = limit_order(1.0);
    o.apply(&submitted("c")).unwrap();
    o.apply(&accepted("c")).unwrap();
    o.apply(&modify_rejected("c")).unwrap();
    assert_eq!(o.status, OrderStatus::Accepted, "modify-reject is non-terminal");
    assert_eq!(o.request.qty, 1.0, "terms untouched");
    assert_eq!(o.request.price, Some(100.0));
}

#[test]
fn modify_rewrites_resting_terms_and_keeps_status() {
    let mut o = limit_order(1.0);
    o.apply(&submitted("c")).unwrap();
    o.apply(&accepted("c")).unwrap();
    o.apply(&modify("c", Some(2.0), Some(101.0))).unwrap();
    assert_eq!(o.status, OrderStatus::Accepted, "modify is a self-transition");
    assert_eq!(o.request.qty, 2.0);
    assert_eq!(o.request.price, Some(101.0));
}

#[test]
fn modify_partial_none_fields_leave_terms_unchanged() {
    let mut o = limit_order(1.0);
    o.apply(&submitted("c")).unwrap();
    o.apply(&accepted("c")).unwrap();
    o.apply(&modify("c", None, Some(105.0))).unwrap(); // price-only re-quote
    assert_eq!(o.request.qty, 1.0, "qty untouched when new_qty is None");
    assert_eq!(o.request.price, Some(105.0));
}

#[test]
fn modify_rejected_before_accept() {
    let mut o = limit_order(1.0);
    o.apply(&submitted("c")).unwrap(); // SUBMITTED — not yet resting at the venue
    assert!(o.apply(&modify("c", Some(2.0), None)).is_err());
    assert_eq!(o.request.qty, 1.0, "rejected modify must not mutate terms");
}

#[test]
fn modify_rejected_when_terminal() {
    let mut o = limit_order(1.0);
    o.apply(&submitted("c")).unwrap();
    o.apply(&Event::OrderRejected(crate::events::OrderRejected {
        cause: None,
        order_client_id: "c".into(),
        reason: "x".into(),
        timestamp: 0,
    }))
    .unwrap();
    assert_eq!(o.status, OrderStatus::Rejected);
    assert!(o.apply(&modify("c", Some(2.0), Some(9.0))).is_err());
}

#[test]
fn resolve_pending_cancel_restores_accepted_when_nothing_filled() {
    let mut o = limit_order(1.0);
    o.status = OrderStatus::PendingCancel; // venue-status seeded (reregister/recon)
    assert!(o.resolve_pending_cancel(), "PENDING_CANCEL is restored");
    assert_eq!(o.status, OrderStatus::Accepted, "no fills folded → ACCEPTED");
}

#[test]
fn resolve_pending_cancel_restores_partially_filled_when_quantity_folded() {
    let mut o = limit_order(1.0);
    o.status = OrderStatus::PendingCancel;
    o.filled_qty = 0.5; // fills arrived while the cancel was pending
    o.average_fill_price = 100.0;
    assert!(o.resolve_pending_cancel());
    assert_eq!(o.status, OrderStatus::PartiallyFilled, "recomputed from the folded fill stream");
    assert_eq!(o.filled_qty, 0.5, "fill accumulation untouched");
    assert_eq!(o.average_fill_price, 100.0);
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
        let mut o = limit_order(1.0);
        o.status = status;
        assert!(!o.resolve_pending_cancel(), "no restore from {}", status.as_str());
        assert_eq!(o.status, status, "status untouched for {}", status.as_str());
    }
}

#[test]
fn modify_of_stop_moves_the_trigger_not_the_limit() {
    let mut o = OrderRecord::new(OrderRequest {
        order_client_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Stop,
        trigger_price: Some(100.0),
        ..Default::default()
    });
    o.apply(&submitted("c")).unwrap();
    o.apply(&accepted("c")).unwrap();
    o.apply(&modify("c", None, Some(110.0))).unwrap();
    assert_eq!(o.request.trigger_price, Some(110.0));
    assert_eq!(o.request.price, None, "limit price stays unset for a stop");
}

/// An `Unrecognized` kind (an order adopted with the venue's own text) is not `Stop`, so a modify
/// moves its LIMIT price — exactly what the string comparison `== "stop"` did for any other word.
#[test]
fn modify_of_an_unrecognised_kind_moves_the_limit_price() {
    let mut o = limit_order(1.0);
    o.request.order_type = OrderType::Unrecognized("stop_market".into());
    o.apply(&submitted("c")).unwrap();
    o.apply(&accepted("c")).unwrap();
    o.apply(&modify("c", None, Some(110.0))).unwrap();
    assert_eq!(o.request.price, Some(110.0));
    assert_eq!(o.request.trigger_price, None);
    assert!(!modified_price_is_trigger(&OrderType::Unrecognized("STOP".into())));
}

/// A modify's two prices: `new_price` lands where `modified_price_is_trigger` says, an explicit
/// trigger price wins on a stop (whose one price it is), the two move apart on a stop-limit or a
/// take-profit, and a trigger price on a kind with no trigger lands nowhere. With no explicit
/// trigger every row is what the one-price routing did.
#[test]
fn an_explicit_trigger_price_wins_and_lands_only_on_a_trigger_kind() {
    let unknown = OrderType::Unrecognized("stop_market".into());
    #[rustfmt::skip]
    let rows = [
        // (kind, new_price, new_trigger_price, (limit, trigger))
        (OrderType::Stop, Some(95.0), None, (None, Some(95.0))),
        (OrderType::Stop, Some(95.0), Some(94.0), (None, Some(94.0))),
        (OrderType::Stop, None, Some(94.0), (None, Some(94.0))),
        (OrderType::StopLimit, Some(99.0), None, (Some(99.0), None)),
        (OrderType::StopLimit, Some(99.0), Some(100.0), (Some(99.0), Some(100.0))),
        (OrderType::StopLimit, None, Some(100.0), (None, Some(100.0))),
        (OrderType::TakeProfit, None, Some(105.0), (None, Some(105.0))),
        (OrderType::Limit, Some(101.0), None, (Some(101.0), None)),
        (OrderType::Limit, Some(101.0), Some(90.0), (Some(101.0), None)),
        (OrderType::MarketToLimit, None, Some(90.0), (None, None)),
        (unknown, Some(101.0), Some(90.0), (Some(101.0), None)),
        // a trailing kind is modified in its size only: neither a price nor its activation lands
        (OrderType::Trailing, Some(101.0), None, (None, None)),
        (OrderType::Trailing, Some(101.0), Some(90.0), (None, None)),
        (OrderType::TrailingLimit, None, Some(90.0), (None, None)),
    ];
    for (kind, price, trigger, want) in rows {
        assert_eq!(modified_prices(&kind, price, trigger), want, "{kind} {price:?} {trigger:?}");
    }
}

/// The venue's `OrderModified` folds both of its prices by [`modified_prices`]: a stop-limit's trigger
/// moves apart from its limit, and with no trigger in the event the trigger stays where it was.
#[test]
fn an_order_modified_with_a_trigger_moves_a_stop_limits_trigger_and_keeps_its_limit() {
    let mut o = OrderRecord::new(OrderRequest {
        order_client_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: -1,
        qty: 1.0,
        order_type: OrderType::StopLimit,
        price: Some(94.5),
        trigger_price: Some(95.0),
        ..Default::default()
    });
    o.apply(&submitted("c")).unwrap();
    o.apply(&accepted("c")).unwrap();
    o.apply(&modify("c", None, Some(94.0))).unwrap();
    assert_eq!((o.request.price, o.request.trigger_price), (Some(94.0), Some(95.0)));
    let Event::OrderModified(mut m) = modify("c", None, None) else { unreachable!() };
    m.new_trigger_price = Some(96.0);
    o.apply(&Event::OrderModified(m)).unwrap();
    assert_eq!((o.request.price, o.request.trigger_price), (Some(94.0), Some(96.0)));
    assert_eq!(o.status, OrderStatus::Accepted, "still a self-transition");
}

/// An `OrderModified` with no trigger serializes to the bytes it always had (the field is skipped), and
/// those bytes read back as no trigger: the journal and the gatewayhub wire carry it unchanged.
#[test]
fn an_order_modified_with_no_trigger_keeps_its_bytes() {
    let Event::OrderModified(m) = modify("c", Some(2.0), Some(101.0)) else { unreachable!() };
    let bytes = serde_json::to_string(&m).unwrap();
    assert_eq!(
        bytes,
        r#"{"order_client_id":"c","venue_order_id":null,"new_qty":2.0,"new_price":101.0,"timestamp":0}"#
    );
    let back: OrderModified = serde_json::from_str(&bytes).unwrap();
    assert_eq!(back.new_trigger_price, None);
    let with = OrderModified { new_trigger_price: Some(96.0), ..m };
    let text = serde_json::to_string(&with).unwrap();
    assert!(text.contains(r#""new_trigger_price":96.0"#), "{text}");
    assert_eq!(serde_json::from_str::<OrderModified>(&text).unwrap(), with);
}

/// `modified_order` is the projection the native amends re-send: the size, then the one price in the
/// column `modified_price_is_trigger` names, then an explicit trigger, which on a stop wins; every
/// other term of the resting order is kept.
#[test]
fn a_modified_order_moves_its_size_its_one_price_and_an_explicit_trigger() {
    let limit = limit_order(1.0).request;
    let moved = modified_order(&limit, Some(2.0), Some(101.0), None);
    assert_eq!((moved.qty, moved.price, moved.trigger_price), (2.0, Some(101.0), None));
    assert_eq!(OrderRequest { qty: 1.0, price: Some(100.0), ..moved }, limit, "nothing else moved");
    assert_eq!(modified_order(&limit, None, None, None), limit, "nothing asked, nothing moved");

    let stop = OrderRequest {
        order_type: OrderType::Stop,
        price: None,
        trigger_price: Some(95.0),
        ..limit.clone()
    };
    let s = modified_order(&stop, None, Some(94.0), None);
    assert_eq!((s.price, s.trigger_price), (None, Some(94.0)), "a stop's price is its trigger");
    let s = modified_order(&stop, None, Some(94.0), Some(93.0));
    assert_eq!(s.trigger_price, Some(93.0), "an explicit trigger wins on a stop");

    let stop_limit = OrderRequest {
        order_type: OrderType::StopLimit,
        price: Some(94.0),
        trigger_price: Some(95.0),
        ..limit
    };
    let modified_stop_limit = modified_order(&stop_limit, None, Some(93.5), Some(94.5));
    assert_eq!(
        (modified_stop_limit.price, modified_stop_limit.trigger_price),
        (Some(93.5), Some(94.5))
    );
}

/// The fill-progress status of a working row (ruling B-16): any fill is `PartiallyFilled`, its whole
/// size included, and nothing filled (or a `NaN`) is `Accepted`.
#[test]
fn a_working_rows_fill_progress_is_partially_filled_once_anything_filled() {
    assert_eq!(OrderStatus::from_fill_progress(0.0), OrderStatus::Accepted);
    assert_eq!(OrderStatus::from_fill_progress(f64::NAN), OrderStatus::Accepted);
    assert_eq!(OrderStatus::from_fill_progress(0.4), OrderStatus::PartiallyFilled);
    assert_eq!(OrderStatus::from_fill_progress(1.0), OrderStatus::PartiallyFilled, "a whole fill");
}

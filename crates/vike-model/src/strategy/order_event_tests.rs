use super::*;
use crate::events::{
    FillEvent, OrderAccepted, OrderCanceled, OrderDenied, OrderExpired, OrderFilled, OrderRejected,
    OrderSubmitted,
};

#[test]
fn from_event_maps_the_nonfill_transitions() {
    assert_eq!(
        OrderEvent::from_event(&Event::OrderAccepted(OrderAccepted {
            order_client_id: "c1".into(),
            venue_order_id: None,
            timestamp: 0,
        })),
        Some(OrderEvent {
            order_client_id: "c1".into(),
            tag: None,
            kind: OrderEventKind::Accepted
        }),
    );
    assert_eq!(
        OrderEvent::from_event(&Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: "c2".into(),
            reason: "too big".into(),
            timestamp: 0,
        })),
        Some(OrderEvent {
            order_client_id: "c2".into(),
            tag: None,
            kind: OrderEventKind::Rejected { reason: "too big".into() },
        }),
    );
    assert_eq!(
        OrderEvent::from_event(&Event::OrderDenied(OrderDenied {
            order_client_id: "c3".into(),
            reason: "risk".into(),
            timestamp: 0,
        })),
        Some(OrderEvent {
            order_client_id: "c3".into(),
            tag: None,
            kind: OrderEventKind::Denied { reason: "risk".into() },
        }),
    );
    assert_eq!(
        OrderEvent::from_event(&Event::OrderCanceled(OrderCanceled {
            order_client_id: "c4".into(),
            reason: "pulled".into(),
            timestamp: 0,
        })),
        Some(OrderEvent {
            order_client_id: "c4".into(),
            tag: None,
            kind: OrderEventKind::Canceled { reason: "pulled".into() },
        }),
    );
    assert_eq!(
        OrderEvent::from_event(&Event::OrderExpired(OrderExpired {
            order_client_id: "c5".into(),
            timestamp: 0,
        })),
        Some(OrderEvent { order_client_id: "c5".into(), tag: None, kind: OrderEventKind::Expired }),
    );
}

#[test]
fn from_event_ignores_fills_and_submit_echo() {
    // fills flow on_fill, not on_order_event
    let fe = FillEvent {
        trade_id: "t".into(),
        order_client_id: "c".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: 0,
        mark_price: None,
        position_side: crate::events::PositionSide::Both,
    };
    assert_eq!(
        OrderEvent::from_event(&Event::OrderFilled(OrderFilled {
            order_client_id: "c".into(),
            fill: fe.clone(),
            timestamp: 0,
        })),
        None,
    );
    assert_eq!(OrderEvent::from_event(&Event::Fill(fe)), None);
    // the local submit echo is not a venue outcome the strategy observes here
    assert_eq!(
        OrderEvent::from_event(&Event::OrderSubmitted(OrderSubmitted {
            order_client_id: "c".into(),
            timestamp: 0,
        })),
        None,
    );
}

#[test]
fn order_lifecycle_round_trips_serde() {
    let lc = OrderEvent {
        order_client_id: "c9".into(),
        tag: Some("bid".into()),
        kind: OrderEventKind::Canceled { reason: "refresh".into() },
    };
    let json = serde_json::to_string(&lc).unwrap();
    assert_eq!(lc, serde_json::from_str::<OrderEvent>(&json).unwrap());
}

#[test]
fn a_busy_refusal_is_recognized_only_in_its_own_words_under_its_own_tag() {
    let denied = |tag: Option<&str>, reason: String| OrderEvent {
        order_client_id: "r1".into(),
        tag: tag.map(str::to_string),
        kind: OrderEventKind::Denied { reason },
    };
    assert_eq!(tag_busy_reason("bid"), "bid is busy: amend it, or cancel it first");
    assert!(denied(Some("bid"), tag_busy_reason("bid")).is_tag_busy_refusal());
    // another tag's words, no tag, other words, or another kind: not this refusal
    assert!(!denied(Some("ask"), tag_busy_reason("bid")).is_tag_busy_refusal());
    assert!(!denied(None, tag_busy_reason("bid")).is_tag_busy_refusal());
    assert!(!denied(Some("bid"), "refused by the pre-trade gate".into()).is_tag_busy_refusal());
    let canceled = OrderEvent {
        kind: OrderEventKind::Canceled { reason: tag_busy_reason("bid") },
        ..denied(Some("bid"), String::new())
    };
    assert!(!canceled.is_tag_busy_refusal());
}

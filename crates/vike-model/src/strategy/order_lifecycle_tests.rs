use super::*;
use crate::events::{
    FillEvent, OrderAccepted, OrderCanceled, OrderDenied, OrderExpired, OrderFilled, OrderRejected,
    OrderSubmitted,
};

#[test]
fn from_event_maps_the_nonfill_transitions() {
    assert_eq!(
        OrderLifecycle::from_event(&Event::OrderAccepted(OrderAccepted {
            client_order_id: "c1".into(),
            venue_order_id: None,
            ts: 0,
        })),
        Some(OrderLifecycle {
            client_order_id: "c1".into(),
            tag: None,
            kind: OrderEventKind::Accepted
        }),
    );
    assert_eq!(
        OrderLifecycle::from_event(&Event::OrderRejected(OrderRejected {
            client_order_id: "c2".into(),
            reason: "too big".into(),
            ts: 0,
        })),
        Some(OrderLifecycle {
            client_order_id: "c2".into(),
            tag: None,
            kind: OrderEventKind::Rejected { reason: "too big".into() },
        }),
    );
    assert_eq!(
        OrderLifecycle::from_event(&Event::OrderDenied(OrderDenied {
            client_order_id: "c3".into(),
            reason: "risk".into(),
            ts: 0,
        })),
        Some(OrderLifecycle {
            client_order_id: "c3".into(),
            tag: None,
            kind: OrderEventKind::Denied { reason: "risk".into() },
        }),
    );
    assert_eq!(
        OrderLifecycle::from_event(&Event::OrderCanceled(OrderCanceled {
            client_order_id: "c4".into(),
            reason: "pulled".into(),
            ts: 0,
        })),
        Some(OrderLifecycle {
            client_order_id: "c4".into(),
            tag: None,
            kind: OrderEventKind::Canceled { reason: "pulled".into() },
        }),
    );
    assert_eq!(
        OrderLifecycle::from_event(&Event::OrderExpired(OrderExpired {
            client_order_id: "c5".into(),
            ts: 0,
        })),
        Some(OrderLifecycle {
            client_order_id: "c5".into(),
            tag: None,
            kind: OrderEventKind::Expired
        }),
    );
}

#[test]
fn from_event_ignores_fills_and_submit_echo() {
    // fills flow on_fill, not on_order_event
    let fe = FillEvent {
        trade_id: "t".into(),
        client_order_id: "c".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    };
    assert_eq!(
        OrderLifecycle::from_event(&Event::OrderFilled(OrderFilled {
            client_order_id: "c".into(),
            fill: fe.clone(),
            ts: 0,
        })),
        None,
    );
    assert_eq!(OrderLifecycle::from_event(&Event::Fill(fe)), None);
    // the local submit echo is not a venue outcome the strategy observes here
    assert_eq!(
        OrderLifecycle::from_event(&Event::OrderSubmitted(OrderSubmitted {
            client_order_id: "c".into(),
            ts: 0,
        })),
        None,
    );
}

#[test]
fn order_lifecycle_round_trips_serde() {
    let lc = OrderLifecycle {
        client_order_id: "c9".into(),
        tag: Some("bid".into()),
        kind: OrderEventKind::Canceled { reason: "refresh".into() },
    };
    let json = serde_json::to_string(&lc).unwrap();
    assert_eq!(lc, serde_json::from_str::<OrderLifecycle>(&json).unwrap());
}

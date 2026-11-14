//! A MODIFY the engine sent and the venue has not answered is remembered beside the order
//! (`Engine::pending_modify_price`, published as `OrderView::pending_price`): the registry
//! keeps the resting terms until the venue confirms, so a reader that sizes a qty-only modify (the
//! control door, known-limitations "The control door remembers the price of a modify only if the
//! modify went through that door") would otherwise price it at a stale limit. A side table: no
//! snapshot or `state_hash` input moves.

use vike_execution::testing::RecordingClient;
use vike_execution::{Engine, EventBus, EventBusOutbox};
use vike_model::OrderRequest;
use vike_model::OrderType;
use vike_model::events::{
    Event, OrderAccepted, OrderCanceled, OrderModified, OrderModifyRejected, OrderSubmitted,
};

use vike_execution::testing::EngineBuilder;

/// `c1`, a resting ACCEPTED order of `order_type` (a limit at 66 000, or a stop with that trigger).
fn resting(order_type: OrderType) -> Engine<RecordingClient> {
    let mut e = EngineBuilder::default().build();
    rest(&mut e, "c1", order_type);
    e
}

/// Submit `order_client_id` to `e` and walk it to ACCEPTED.
fn rest(e: &mut Engine<RecordingClient>, order_client_id: &str, order_type: OrderType) {
    let stop = order_type == OrderType::Stop;
    let req = OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        price: (!stop).then_some(66_000.0),
        trigger_price: stop.then_some(66_000.0),
        order_type,
        timestamp: 1,
        ..Default::default()
    };
    e.submit_order(&req, 0, &mut EventBusOutbox::default());
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order_client_id.into(),
            timestamp: 0,
        }),
        e,
    );
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.into(),
            venue_order_id: Some(format!("v-{order_client_id}").into()),
            timestamp: 1,
        }),
        e,
    );
}

fn modified(price: Option<f64>) -> Event {
    Event::OrderModified(OrderModified {
        order_client_id: "c1".into(),
        venue_order_id: None,
        new_qty: None,
        new_price: price,
        new_trigger_price: None,
        timestamp: 5,
    })
}

/// The price is pending from the moment the modify is sent until the venue answers, while the
/// registry still shows the old one; the answer clears it and the registry takes the new price.
#[test]
fn a_price_modify_is_pending_until_the_venue_answers() {
    let mut e = resting(OrderType::Limit);
    assert_eq!(e.pending_modify_price("c1"), None, "nothing in flight yet");

    e.modify_order("c1", None, Some(67_000.0), 2, &mut EventBusOutbox::default());
    assert_eq!(e.pending_modify_price("c1"), Some(67_000.0), "sent, not answered");
    assert_eq!(e.registry["c1"].request.price, Some(66_000.0), "the resting terms are unchanged");
    assert_eq!(e.pending_modify_price("other"), None, "per order");

    EventBus::new().publish(modified(Some(67_000.0)), &mut e);
    assert_eq!(e.pending_modify_price("c1"), None, "answered");
    assert_eq!(e.registry["c1"].request.price, Some(67_000.0), "and now the resting price");
}

/// A modify the venue REFUSES is answered too: the order keeps its terms and nothing is pending.
#[test]
fn a_refused_modify_is_no_longer_pending() {
    let mut e = resting(OrderType::Limit);
    e.modify_order("c1", None, Some(67_000.0), 2, &mut EventBusOutbox::default());
    assert_eq!(e.pending_modify_price("c1"), Some(67_000.0));

    EventBus::new().publish(
        Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id: "c1".into(),
            reason: "venue: too far from the mark".into(),
            timestamp: 3,
        }),
        &mut e,
    );
    assert_eq!(e.pending_modify_price("c1"), None);
    assert_eq!(e.registry["c1"].request.price, Some(66_000.0));
}

/// Only a modify of the LIMIT price is a pending price: a qty-only modify carries none, and a stop's
/// modify moves its TRIGGER (`vike_model::modified_price_is_trigger`), which the view's `price` does not.
#[test]
fn a_quantity_only_or_a_stop_modify_is_not_a_pending_price() {
    let mut e = resting(OrderType::Limit);
    e.modify_order("c1", Some(2.0), None, 2, &mut EventBusOutbox::default());
    assert_eq!(e.pending_modify_price("c1"), None, "a qty-only modify carries no price");

    let mut s = resting(OrderType::Stop);
    s.modify_order("c1", None, Some(65_000.0), 2, &mut EventBusOutbox::default());
    assert!(!s.client.modifies.is_empty(), "the modify did reach the client");
    assert_eq!(s.pending_modify_price("c1"), None, "a stop's modify moves the trigger");
}

/// An answer that never comes does not leave a price behind for good: it is read for 30 s of the
/// engine clock after the send and not after.
#[test]
fn a_pending_modify_that_is_never_answered_expires() {
    let mut e = resting(OrderType::Limit);
    e.modify_order("c1", None, Some(67_000.0), 1_000, &mut EventBusOutbox::default());

    e.now_ms = 1_000 + 30_000;
    assert_eq!(e.pending_modify_price("c1"), Some(67_000.0), "30 s after the send: still read");
    e.now_ms = 1_000 + 30_001;
    assert_eq!(e.pending_modify_price("c1"), None, "past it: gone");
    e.now_ms = 0;
    assert_eq!(
        e.pending_modify_price("c1"),
        Some(67_000.0),
        "a clock behind the send is not stale"
    );
}

/// An order that ENDS before the venue answers its modify leaves a dead entry; the next priced
/// modify of any order sweeps it, so the table is empty again once nothing is in flight (the
/// publish pays an `is_empty` test per order only while it is).
#[test]
fn a_modify_whose_order_ended_unanswered_is_swept_at_the_next_modify() {
    let mut e = resting(OrderType::Limit);
    rest(&mut e, "c2", OrderType::Limit);
    e.modify_order("c1", None, Some(67_000.0), 2, &mut EventBusOutbox::default());
    EventBus::new().publish(
        Event::OrderCanceled(OrderCanceled {
            order_client_id: "c1".into(),
            reason: "".into(),
            timestamp: 3,
        }),
        &mut e,
    );
    assert_eq!(e.pending_modify_price("c1"), Some(67_000.0), "nothing swept it yet");

    e.modify_order("c2", None, Some(66_500.0), 4, &mut EventBusOutbox::default());
    assert_eq!(e.pending_modify_price("c1"), None, "swept: its order is terminal");
    assert_eq!(e.pending_modify_price("c2"), Some(66_500.0), "the live one is kept");
}

/// The time an amend's answer is stamped with is read by nothing the fold keeps (the owner's
/// 2026-11-12 ruling: the venue's time from the reply, else the time it was received, never `0`):
/// the same `OrderModified` and `OrderModifyRejected` stamped `0` and stamped with a venue time leave
/// the same state, the same terms and the same reason. `0` is no request to stamp with the core's clock.
#[test]
fn the_fold_reads_no_amend_answers_time() {
    let fold = |ts: i64| {
        let mut e = resting(OrderType::Limit);
        let mut bus = EventBus::new();
        e.modify_order("c1", None, Some(67_000.0), 2, &mut EventBusOutbox::default());
        bus.publish(
            Event::OrderModified(OrderModified {
                order_client_id: "c1".into(),
                venue_order_id: Some("v-c1b".into()),
                new_qty: None,
                new_price: Some(67_000.0),
                new_trigger_price: None,
                timestamp: ts,
            }),
            &mut e,
        );
        e.modify_order("c1", None, Some(68_000.0), 3, &mut EventBusOutbox::default());
        bus.publish(
            Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: "c1".into(),
                reason: "venue: too far from the mark".into(),
                timestamp: ts,
            }),
            &mut e,
        );
        e
    };
    let (unstamped, stamped) = (fold(0), fold(1_794_000_000_123));
    assert_eq!(
        vike_execution::state_hash(&[unstamped.snapshot_state()]),
        vike_execution::state_hash(&[stamped.snapshot_state()])
    );
    for e in [&unstamped, &stamped] {
        assert_eq!(e.registry["c1"].request.price, Some(67_000.0), "the modify landed");
        assert_eq!(e.rejection_reason("c1"), Some("venue: too far from the mark"));
        assert_eq!(e.pending_modify_price("c1"), None, "both answers folded");
    }
}

/// With two modifies in flight the last one sent is the pending price.
#[test]
fn the_last_modify_sent_is_the_pending_price() {
    let mut e = resting(OrderType::Limit);
    e.modify_order("c1", None, Some(67_000.0), 2, &mut EventBusOutbox::default());
    e.modify_order("c1", None, Some(66_500.0), 3, &mut EventBusOutbox::default());
    assert_eq!(e.pending_modify_price("c1"), Some(66_500.0));
}

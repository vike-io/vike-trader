//! A CANCEL or a MODIFY the engine sent is remembered until the venue answers it
//! (`Engine::unanswered_commands`), with the engine clock of its first send: what the core's watchdog rung for
//! unanswered commands reads. A side table: no snapshot or `state_hash` input moves.

use vike_execution::testing::{EngineBuilder, RecordingClient};
use vike_execution::{Engine, EventBus, EventBusOutbox, UnansweredCommand};
use vike_model::OrderRequest;
use vike_model::events::{
    Event, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderModified, OrderModifyRejected,
    OrderSubmitted,
};

/// An engine holding `c1` and `c2`, both resting ACCEPTED limits.
fn resting() -> Engine<RecordingClient> {
    let mut e = EngineBuilder::default().build();
    for order_client_id in ["c1", "c2"] {
        let req = OrderRequest {
            order_client_id: order_client_id.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            price: Some(66_000.0),
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
            &mut e,
        );
        bus.publish(
            Event::OrderAccepted(OrderAccepted {
                order_client_id: order_client_id.into(),
                venue_order_id: Some(format!("v-{order_client_id}").into()),
                timestamp: 1,
            }),
            &mut e,
        );
    }
    e
}

fn unanswered(e: &Engine<RecordingClient>) -> Vec<(String, UnansweredCommand, i64)> {
    let mut all: Vec<_> =
        e.unanswered_commands().map(|(c, command, at)| (c.to_string(), command, at)).collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all
}

#[test]
fn a_cancel_is_unanswered_until_the_venue_cancels_or_refuses_it_and_a_repeat_keeps_its_first_clock()
{
    let mut e = resting();
    assert!(unanswered(&e).is_empty(), "nothing sent yet");
    e.now_ms = 10;
    assert!(e.cancel_order("c1"));
    e.now_ms = 20;
    assert!(e.cancel_order("c1"), "the dead-man repeats its sweep");
    assert_eq!(unanswered(&e), [("c1".to_string(), UnansweredCommand::Cancel, 10)]);

    EventBus::new().publish(
        Event::OrderCancelRejected(OrderCancelRejected {
            order_client_id: "c1".into(),
            reason: "venue: too late".into(),
            timestamp: 21,
            refusal_id: None,
        }),
        &mut e,
    );
    assert!(unanswered(&e).is_empty(), "a refusal answers the cancel");

    e.now_ms = 30;
    e.cancel_order_batch_with_intent(
        &["c1".to_string(), "c2".to_string()],
        vike_model::OrderCancelIntent::RiskOff,
    );
    EventBus::new().publish(
        Event::OrderCanceled(OrderCanceled {
            order_client_id: "c2".into(),
            reason: "venue".into(),
            timestamp: 31,
            post_only_would_cross: false,
            refusal_id: None,
        }),
        &mut e,
    );
    assert_eq!(
        unanswered(&e),
        [("c1".to_string(), UnansweredCommand::Cancel, 30)],
        "the cancel of c2 was answered, the cancel of c1 was not"
    );
    e.forget_unanswered("c1", UnansweredCommand::Cancel);
    assert!(unanswered(&e).is_empty());
}

#[test]
fn a_modify_is_unanswered_until_the_venue_modifies_or_refuses_it() {
    let mut e = resting();
    e.modify_order("c1", None, Some(67_000.0), 5, &mut EventBusOutbox::default());
    e.modify_order("c2", Some(0.5), None, 6, &mut EventBusOutbox::default());
    assert_eq!(
        unanswered(&e),
        [
            ("c1".to_string(), UnansweredCommand::Modify, 5),
            ("c2".to_string(), UnansweredCommand::Modify, 6)
        ]
    );
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderModified(OrderModified {
            order_client_id: "c1".into(),
            venue_order_id: None,
            new_qty: None,
            new_price: Some(67_000.0),
            new_trigger_price: None,
            timestamp: 7,
        }),
        &mut e,
    );
    bus.publish(
        Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id: "c2".into(),
            reason: "venue: too far from the mark".into(),
            timestamp: 8,
            refusal_id: None,
        }),
        &mut e,
    );
    assert!(unanswered(&e).is_empty(), "both answered: {:?}", unanswered(&e));
}

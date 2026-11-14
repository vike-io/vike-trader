//! Cancel INTENT plumbing at the engine level — [`vike_model::OrderCancelIntent`] travelling from an
//! `Engine` cancel verb down to the venue client, unchanged and never guessed at.
//!
//! The engine does nothing with the intent, on purpose; the property is that the classification
//! ARRIVES. A venue that meters its cancels (Polymarket's per-signer cancel bucket, the reason this
//! seam exists) can hold routine churn back only if it is still distinguishable at the adapter; a
//! verb dropping the intent would hand every venue `Unspecified`: safe, but with the protection
//! disarmed and nothing failing.
//!
//! The other half: the one plain cancel verb (`cancel_order`) means
//! [`vike_model::OrderCancelIntent::Unspecified`], the flatten-safe value no venue may shed.

use vike_execution::testing::{EngineBuilder, RecordingClient, resting_limit};
use vike_execution::{Engine, EventBus, EventBusOutbox};
use vike_model::OrderCancelIntent;
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};

fn engine() -> Engine<RecordingClient> {
    EngineBuilder { venue: "binance".into(), ..Default::default() }.build()
}

/// Rest `order_client_ids` ACCEPTED through the real emitter split (`OrderSubmitted` at submit, then the venue
/// ack): a cancel verb only reaches the client for a LIVE order.
fn engine_with_resting(order_client_ids: &[&str]) -> Engine<RecordingClient> {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    let mut outbox = EventBusOutbox::default();
    for order_client_id in order_client_ids {
        engine_under_test.submit_order(&resting_limit(order_client_id), 1, &mut outbox);
        bus.publish(
            Event::OrderSubmitted(OrderSubmitted {
                order_client_id: (*order_client_id).into(),
                timestamp: 1,
            }),
            &mut engine_under_test,
        );
        bus.publish(
            Event::OrderAccepted(OrderAccepted {
                order_client_id: (*order_client_id).into(),
                venue_order_id: None,
                timestamp: 1,
            }),
            &mut engine_under_test,
        );
        assert!(
            engine_under_test.registry.contains_key(*order_client_id),
            "fixture: {order_client_id} must be resting, or every assertion below passes vacuously"
        );
    }
    // The submits are done; only cancels are recorded from here on.
    engine_under_test.client.cancel_intents.clear();
    engine_under_test
}

/// The one-cancel expectation, spelled once.
fn recorded(order_client_id: &str, intent: OrderCancelIntent) -> Vec<(String, OrderCancelIntent)> {
    vec![(order_client_id.to_string(), intent)]
}

/// The one plain cancel verb, `cancel_order`, means "unclassified", which every venue must treat
/// as a flatten. (The batch and mass verbs take the intent as an argument; there is no plain twin.)
#[test]
fn the_unclassified_verb_still_sends_the_flatten_safe_default() {
    let mut one = engine_with_resting(&["c1"]);
    one.cancel_order("c1");
    assert_eq!(
        one.client.cancel_intents,
        recorded("c1", OrderCancelIntent::Unspecified),
        "an unnamed intent must be Unspecified — never Routine, which a venue may shed"
    );
}

/// A caller that CAN classify reaches the client with the classification intact, on all three
/// verbs. `mass_cancel_with_intent` matters most: it is how "get me out" is spelled.
///
/// ⚠ One engine PER VERB, not one engine driven three times: a cancel is fire-and-forget and never
/// mutates status (the venue owns the terminal), so an already-cancelled order is still LIVE to a
/// later `mass_cancel_with_intent` and would be swept again. A shared engine would make these
/// assertions about that re-sweep rather than about the verb under test.
#[test]
fn a_classified_cancel_reaches_the_client_unchanged() {
    let mut one = engine_with_resting(&["r1"]);
    one.cancel_order_with_intent("r1", OrderCancelIntent::Routine);
    assert_eq!(
        one.client.cancel_intents,
        recorded("r1", OrderCancelIntent::Routine),
        "the intent must survive every hop from verb to venue client"
    );

    let mut batch = engine_with_resting(&["r2"]);
    batch.cancel_order_batch_with_intent(&["r2".to_string()], OrderCancelIntent::Routine);
    assert_eq!(batch.client.cancel_intents, recorded("r2", OrderCancelIntent::Routine));

    let mut mass = engine_with_resting(&["r3"]);
    mass.mass_cancel_with_intent(OrderCancelIntent::RiskOff);
    assert_eq!(mass.client.cancel_intents, recorded("r3", OrderCancelIntent::RiskOff));
}

/// Both single-order verbs answer whether the cancel was ISSUED: `true` for a live order (and the client
/// heard it), `false` for the no-op of an order client id this registry does not hold, so a caller that must not
/// lose track of a resting order can tell the two apart.
#[test]
fn a_cancel_says_whether_it_was_issued() {
    let mut engine = engine_with_resting(&["live"]);
    assert!(engine.cancel_order("live"));
    assert!(engine.cancel_order_with_intent("live", OrderCancelIntent::RiskOff));
    assert_eq!(engine.client.cancel_intents.len(), 2, "both issued cancels reached the client");

    assert!(!engine.cancel_order("never-existed"));
    assert!(!engine.cancel_order_with_intent("never-existed", OrderCancelIntent::RiskOff));
    assert_eq!(engine.client.cancel_intents.len(), 2, "the no-ops reached no client");
}

/// The terminal guard is unchanged by the intent: an unknown order client id still reaches no client at all,
/// so a classification can never resurrect a cancel the engine would have suppressed.
#[test]
fn an_unknown_order_client_id_still_reaches_no_client_however_it_is_classified() {
    let mut engine = engine_with_resting(&["known"]);
    engine.cancel_order_with_intent("never-existed", OrderCancelIntent::RiskOff);
    engine
        .cancel_order_batch_with_intent(&["also-not-real".to_string()], OrderCancelIntent::Routine);
    assert!(
        engine.client.cancel_intents.is_empty(),
        "is_live still gates the cancel path: {:?}",
        engine.client.cancel_intents
    );
}

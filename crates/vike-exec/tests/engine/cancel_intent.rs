//! Cancel INTENT plumbing at the engine level — [`vike_exec::CancelIntent`] travelling from an
//! `ExecutionEngine` cancel verb down to the venue client, unchanged and never guessed at.
//!
//! The property under test is not "the engine does something with the intent" — it does nothing
//! with it, on purpose. It is that the classification ARRIVES: a venue that meters its cancels
//! (Polymarket's per-signer cancel bucket, the reason this seam exists) can only hold routine churn
//! back if the churn is still distinguishable by the time it reaches the adapter, and a verb that
//! silently dropped the intent would hand every venue an `Unspecified` — safe, but with the whole
//! protection disarmed and nothing failing.
//!
//! The other half is the backward-compatibility claim: every pre-existing cancel verb still means
//! [`vike_exec::CancelIntent::Unspecified`], the flatten-safe value no venue may shed.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, CancelIntent, EventBus, ExecutionEngine, Outbox, RiskGate, RiskLimits,
};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1_000_000.0, "binance", None, BalanceMode::Delta),
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

/// Rest `coids` ACCEPTED at the venue, through the real emitter-split lane (the adapter emits
/// `OrderSubmitted` at submit, the venue acks) — a cancel verb only reaches the client for a LIVE
/// order, so nothing below would be observable without this.
fn engine_with_resting(coids: &[&str]) -> ExecutionEngine<RecordingClient> {
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    for coid in coids {
        eng.submit_order(&req(coid), 1, &mut outbox);
        bus.publish(
            Event::OrderSubmitted(OrderSubmitted { client_order_id: (*coid).into(), ts: 1 }),
            &mut eng,
        );
        bus.publish(
            Event::OrderAccepted(OrderAccepted {
                client_order_id: (*coid).into(),
                venue_order_id: None,
                ts: 1,
            }),
            &mut eng,
        );
        assert!(
            eng.registry.contains_key(*coid),
            "fixture: {coid} must be resting, or every assertion below passes vacuously"
        );
    }
    // The submits are done; only cancels are recorded from here on.
    eng.client.cancel_intents.clear();
    eng
}

/// The one-cancel expectation, spelled once.
fn recorded(coid: &str, intent: CancelIntent) -> Vec<(String, CancelIntent)> {
    vec![(coid.to_string(), intent)]
}

/// Every cancel verb that existed before the intent did still means "unclassified" — which every
/// venue must treat as a flatten. This is the whole no-behavior-change claim for the eleven venues
/// that were not touched.
///
/// ⚠ One engine PER VERB, not one engine driven three times: `cancel_order` is fire-and-forget and
/// never mutates status (the venue owns the terminal), so an already-cancelled order is still LIVE
/// to a later `mass_cancel` and would be swept again. A shared engine would make these assertions
/// about that re-sweep rather than about the verb under test.
#[test]
fn the_unclassified_verbs_still_send_the_flatten_safe_default() {
    let mut one = engine_with_resting(&["c1"]);
    one.cancel_order("c1");
    assert_eq!(
        one.client.cancel_intents,
        recorded("c1", CancelIntent::Unspecified),
        "an unnamed intent must be Unspecified — never Routine, which a venue may shed"
    );

    let mut batch = engine_with_resting(&["c2"]);
    batch.cancel_order_batch(&["c2".to_string()]);
    assert_eq!(batch.client.cancel_intents, recorded("c2", CancelIntent::Unspecified));

    let mut mass = engine_with_resting(&["c3"]);
    mass.mass_cancel();
    assert_eq!(mass.client.cancel_intents, recorded("c3", CancelIntent::Unspecified));
}

/// A caller that CAN classify reaches the client with the classification intact, on all three
/// verbs. `mass_cancel_with_intent` matters most: it is how "get me out" is spelled.
#[test]
fn a_classified_cancel_reaches_the_client_unchanged() {
    let mut one = engine_with_resting(&["r1"]);
    one.cancel_order_with_intent("r1", CancelIntent::Routine);
    assert_eq!(
        one.client.cancel_intents,
        recorded("r1", CancelIntent::Routine),
        "the intent must survive every hop from verb to venue client"
    );

    let mut batch = engine_with_resting(&["r2"]);
    batch.cancel_order_batch_with_intent(&["r2".to_string()], CancelIntent::Routine);
    assert_eq!(batch.client.cancel_intents, recorded("r2", CancelIntent::Routine));

    let mut mass = engine_with_resting(&["r3"]);
    mass.mass_cancel_with_intent(CancelIntent::RiskOff);
    assert_eq!(mass.client.cancel_intents, recorded("r3", CancelIntent::RiskOff));
}

/// The terminal guard is unchanged by the intent: an unknown coid still reaches no client at all,
/// so a classification can never resurrect a cancel the engine would have suppressed.
#[test]
fn an_unknown_coid_still_reaches_no_client_however_it_is_classified() {
    let mut eng = engine_with_resting(&["known"]);
    eng.cancel_order_with_intent("never-existed", CancelIntent::RiskOff);
    eng.cancel_order_batch_with_intent(&["also-not-real".to_string()], CancelIntent::Routine);
    assert!(
        eng.client.cancel_intents.is_empty(),
        "is_live still gates the cancel path: {:?}",
        eng.client.cancel_intents
    );
}

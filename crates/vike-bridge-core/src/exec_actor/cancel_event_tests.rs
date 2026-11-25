//! `cancel_event`'s mapping of a cancel outcome to its canonical event: never a swallowed failure.

use super::{ExecutionClientActorCancelOutcome, cancel_event};
use vike_model::events::Event;

#[test]
fn confirmed_cancel_maps_to_terminal_order_canceled() {
    match cancel_event("c1", ExecutionClientActorCancelOutcome::Canceled) {
        Event::OrderCanceled(e) => assert_eq!(e.order_client_id, "c1"),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }
}

#[test]
fn failed_cancel_maps_to_nonterminal_reject_with_reason() {
    let not_sent = vike_model::OrderRefusalId::NotSent;
    match cancel_event(
        "c1",
        ExecutionClientActorCancelOutcome::Rejected("venue down".into(), not_sent),
    ) {
        Event::OrderCancelRejected(e) => {
            assert_eq!(e.order_client_id, "c1");
            assert_eq!(e.reason, "venue down");
            assert_eq!(
                e.refusal_id.as_deref(),
                Some(not_sent.word()),
                "the caller's id rides the event"
            );
        }
        other => panic!("expected OrderCancelRejected, got {other:?}"),
    }
}

//! `cancel_event`'s mapping of a cancel outcome to its canonical event: never a swallowed failure.

use super::{CancelOutcome, cancel_event};
use vike_model::events::Event;

#[test]
fn confirmed_cancel_maps_to_terminal_order_canceled() {
    match cancel_event("c1", CancelOutcome::Canceled) {
        Event::OrderCanceled(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }
}

#[test]
fn failed_cancel_maps_to_nonterminal_reject_with_reason() {
    match cancel_event("c1", CancelOutcome::Rejected("venue down".into())) {
        Event::OrderCancelRejected(e) => {
            assert_eq!(e.client_order_id, "c1");
            assert_eq!(e.reason, "venue down");
        }
        other => panic!("expected OrderCancelRejected, got {other:?}"),
    }
}

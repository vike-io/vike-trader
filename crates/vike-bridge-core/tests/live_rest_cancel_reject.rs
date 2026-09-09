//! A failed cancel through the shared `LiveRestClient` (Binance/Bybit/OKX/Deribit) must surface as
//! an `OrderCancelRejected` event, not vanish into the write-only `last_cancel_error` field.
//! Regression guard for audit finding A2.

use vike_bridge_core::rest::{LiveRestClient, VenueRest};
use vike_bridge_core::transport::VenueApiError;
use vike_exec::ExecutionClient;
use vike_model::OrderRequest;
use vike_model::events::Event;

/// A `VenueRest` whose cancel always fails with a venue error (e.g. venue down, not the benign
/// "unknown order" case which venues already swallow as idempotent success).
struct FailingCancelRest;

impl VenueRest for FailingCancelRest {
    fn submit_order(&self, _request: &OrderRequest) -> Vec<Event> {
        Vec::new()
    }
    fn cancel_order(&self, _client_order_id: &str) -> Result<(), VenueApiError> {
        Err(VenueApiError { code: -1001, msg: "venue unavailable".into() })
    }
}

#[test]
fn failed_cancel_emits_order_cancel_rejected() {
    let mut client = LiveRestClient::new(FailingCancelRest);

    client.cancel("c1");

    match client.poll_events() {
        Some(Event::OrderCancelRejected(r)) => {
            assert_eq!(r.client_order_id, "c1");
            assert!(
                r.reason.contains("venue unavailable"),
                "carries the venue reason: {}",
                r.reason
            );
        }
        other => panic!("expected OrderCancelRejected, got {other:?}"),
    }
}

#[test]
fn failed_cancel_batch_emits_reject_per_order() {
    let mut client = LiveRestClient::new(FailingCancelRest);

    client.cancel_batch(&["c1".to_string(), "c2".to_string()]);

    let mut rejected: Vec<String> = Vec::new();
    while let Some(ev) = client.poll_events() {
        match ev {
            Event::OrderCancelRejected(r) => rejected.push(r.client_order_id),
            other => panic!("expected only OrderCancelRejected, got {other:?}"),
        }
    }
    assert_eq!(rejected, vec!["c1".to_string(), "c2".to_string()]);
}

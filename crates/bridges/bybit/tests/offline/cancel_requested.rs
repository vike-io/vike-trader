//! A cancel the venue TOOK is `OrderCancelAck::Requested`, never `Canceled`, from the reply the DEMO host really
//! sends: `CANCEL_TAKEN` is the venue's envelope, byte for byte, to `POST /v5/order/cancel` by `orderLinkId` of a far
//! limit (account `bybit/DEMO`, 2026-11-23, `crates/bridges/bybit/tests/bybit_smokes/bybit_cancel_reply_smoke.rs`'s
//! `bybit_a_cancel_sent_before_the_stream_connects`). That run read the order `Cancelled` by `orderLinkId` at once,
//! and the private stream it connected AFTER the cancel said nothing about it in eight seconds: the grid wait
//! (`vike_bridge_core::venue_adapter::await_properties`, a cancel before the exec thread's stream exists) reads the
//! order back, and `the_grid_wait_reads_a_taken_cancel_back_by_order_client_id` drives that with the captured reply
//! and the captured row's fields.

use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use vike_bridge_core::exec_actor::ExecutionClientActorCommand;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::venue_adapter::await_properties;
use vike_bybit::execution::rest::VenueRestBybit;
use vike_execution::{Ingest, event_channel};
use vike_model::OrderCancelIntent;
use vike_model::events::Event;

use crate::r6_bybit_parity::{bybit_perp, fixture};

const ORDER_CLIENT_ID: &str = "vtcrb46212513";

/// The venue's answer to the cancel that rested no stream behind it (the smoke's `CAPTURED` line).
fn cancel_taken() -> Value {
    json!({"result": {"orderId": "6d515a66-0a5c-441d-a563-f5cbb40f3b01", "orderLinkId": ORDER_CLIENT_ID},
        "retCode": 0, "retExtInfo": {}, "retMsg": "OK", "time": 1795446213043_i64})
}

/// `/v5/order/realtime` by `orderLinkId` right after it: the fields the smoke printed of the row
/// (`orderStatus Cancelled`, `cancelType CancelByUser`, `updatedTime` the reply's `time`).
fn cancelled_row() -> Value {
    json!({"retCode": 0, "retMsg": "OK", "result": {"list": [{
        "orderId": "6d515a66-0a5c-441d-a563-f5cbb40f3b01", "orderLinkId": ORDER_CLIENT_ID, "symbol": "BTCUSDT",
        "orderStatus": "Cancelled", "cancelType": "CancelByUser", "updatedTime": "1795446213043"}]}})
}

/// Answers the cancel POST with [`cancel_taken`] and the realtime read with [`cancelled_row`], after checking each
/// names the book and the order client id.
struct Venue;

impl RestTransportJson for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let p: Vec<(&str, &Value)> = params.iter().map(|(k, v)| (*k, v)).collect();
        assert_eq!(
            p,
            [
                ("category", &json!("linear")),
                ("symbol", &json!("BTCUSDT")),
                ("orderLinkId", &json!(ORDER_CLIENT_ID))
            ]
        );
        match (method, path) {
            ("POST", "/v5/order/cancel") => Ok(cancel_taken()),
            ("GET", "/v5/order/realtime") => Ok(cancelled_row()),
            other => panic!("unexpected {other:?}"),
        }
    }
}

fn client() -> VenueRestBybit<Venue> {
    bybit_perp(&fixture(), Venue)
}

#[test]
fn a_taken_cancel_is_requested_never_canceled() {
    assert_eq!(client().cancel_order(ORDER_CLIENT_ID).ok(), Some(OrderCancelAck::Requested));
}

/// The grid wait sends the cancel through its grid-less client, hears `Requested`, and reads the order back by its
/// order client id: the read's `Cancelled` row is the one answer, an `OrderCanceled` stamped with the row's time.
#[test]
fn the_grid_wait_reads_a_taken_cancel_back_by_order_client_id() {
    let (sender, mut ingest) = event_channel(16);
    let (tx, rx) = mpsc::channel();
    let intent = OrderCancelIntent::Unspecified;
    tx.send(ExecutionClientActorCommand::Cancel {
        order_client_id: ORDER_CLIENT_ID.into(),
        intent,
    })
    .unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    let fetch = || -> Result<(), String> { Err("instruments-info: HTTP 503".to_string()) };
    let pause = Duration::from_secs(60);
    assert_eq!(await_properties("bybit", "BTCUSDT.P", &sender, &rx, pause, client, fetch), None);
    let mut seen = Vec::new();
    while let Ok(Ingest::Event(event)) = ingest.try_recv() {
        seen.push(event);
    }
    let [Event::OrderCanceled(canceled)] = seen.as_slice() else { panic!("{seen:?}") };
    assert_eq!(
        (canceled.order_client_id.as_str(), canceled.timestamp),
        (ORDER_CLIENT_ID, 1_795_446_213_043)
    );
}

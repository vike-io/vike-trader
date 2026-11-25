//! A `cancel-order` the venue TOOK (`sCode 0`) is `OrderCancelAck::Requested`, never `Canceled`, from the reply the
//! DEMO host really sends: `cancel_taken` is the venue's envelope, byte for byte, to `POST /api/v5/trade/cancel-order`
//! by `clOrdId` of a far limit (the store's default okx demo account, 2026-11-23,
//! `crates/bridges/okx/tests/okx_smokes/okx_cancel_reply_smoke.rs`'s `okx_a_cancel_sent_before_the_stream_connects`).
//! That run read the order `canceled` by `clOrdId` at once, and the private socket it connected AFTER the cancel said
//! nothing about it in eight seconds: the grid wait (`vike_bridge_core::venue_adapter::await_properties`, a cancel
//! before the exec thread's socket exists) reads the order back, and
//! `the_grid_wait_reads_a_taken_cancel_back_by_order_client_id` drives that with the captured reply and the captured
//! row's fields. A stop's cancel (`cancel-algos`) keeps its own answer: `crates/bridges/okx/tests/offline/algo_stop.rs`.

use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use vike_bridge_core::exec_actor::ExecutionClientActorCommand;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::venue_adapter::await_properties;
use vike_execution::{Ingest, event_channel};
use vike_model::OrderCancelIntent;
use vike_model::events::Event;
use vike_okx::execution::rest::VenueRestOkx;

use crate::r6_okx_parity::{fixture, okx_perp};

const ORDER_CLIENT_ID: &str = "vtcrb46507466";

/// The venue's answer to the cancel that had no socket behind it (the smoke's `CAPTURED` line).
fn cancel_taken() -> Value {
    json!({"code": "0", "data": [{"clOrdId": ORDER_CLIENT_ID, "ordId": "4125319710605488129", "sCode": "0",
        "sMsg": "", "ts": "1795446508254"}], "inTime": "1795446508253057", "msg": "", "outTime": "1795446508255156"})
}

/// `GET /api/v5/trade/order` by `clOrdId` right after it: the fields the smoke printed of the row (`state canceled`,
/// `cancelSource 1`, `uTime` the reply row's `ts`).
fn canceled_row() -> Value {
    json!({"code": "0", "msg": "", "data": [{"clOrdId": ORDER_CLIENT_ID, "ordId": "4125319710605488129",
        "instId": "BTC-USDT-SWAP", "state": "canceled", "cancelSource": "1", "uTime": "1795446508254"}]})
}

/// Answers the cancel POST with [`cancel_taken`] and the order read with [`canceled_row`], after checking each
/// names the instrument and the order client id.
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
        assert_eq!(p, [("instId", &json!("BTC-USDT-SWAP")), ("clOrdId", &json!(ORDER_CLIENT_ID))]);
        match (method, path) {
            ("POST", "/api/v5/trade/cancel-order") => Ok(cancel_taken()),
            ("GET", "/api/v5/trade/order") => Ok(canceled_row()),
            other => panic!("unexpected {other:?}"),
        }
    }
}

fn client() -> VenueRestOkx<Venue> {
    okx_perp(&fixture(), Venue)
}

#[test]
fn a_taken_cancel_is_requested_never_canceled() {
    assert_eq!(client().cancel_order(ORDER_CLIENT_ID).ok(), Some(OrderCancelAck::Requested));
}

/// The grid wait sends the cancel through its grid-less client, hears `Requested`, and reads the order back by its
/// order client id: the read's `canceled` row is the one answer, an `OrderCanceled` stamped with the row's time.
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
    let fetch = || -> Result<(), String> { Err("public/instruments: HTTP 503".to_string()) };
    let pause = Duration::from_secs(60);
    assert_eq!(await_properties("okx", "BTCUSDT.P", &sender, &rx, pause, client, fetch), None);
    let mut seen = Vec::new();
    while let Ok(Ingest::Event(event)) = ingest.try_recv() {
        seen.push(event);
    }
    let [Event::OrderCanceled(canceled)] = seen.as_slice() else { panic!("{seen:?}") };
    assert_eq!(
        (canceled.order_client_id.as_str(), canceled.timestamp),
        (ORDER_CLIENT_ID, 1_795_446_508_254)
    );
}

//! A 2xx that does not say what it should is not an answer. A body with no `retCode` read as success
//! accepted a submit under an empty id; a by-id read with no `list` read as "absent" rejected an order
//! that may rest. Wrong shape is `Err` and a 2xx with no id goes to the re-query. A REAL empty `list` stays
//! "absent" (bybit has no not-found code for a by-id miss, the crate page): pinned on a recorded envelope
//! (`tests/fixtures/classes/order_realtime_dated_far_stop.json`, captured on the demo host) with its rows
//! taken out, because the repo holds no captured empty reply.

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_bybit::execution::rest::VenueRestBybit;
use vike_bybit::transport::client::unwrap_envelope;
use vike_model::OrderRequest;
use vike_model::events::Event;

use crate::r6_bybit_parity::{bybit_perp, fixture};

/// `post` answers every POST, `get` every GET; `wire` records `(method, last path segment)`.
struct Script {
    post: Value,
    get: Value,
    wire: Mutex<Vec<String>>,
}

impl RestTransportJson for Script {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let tail = path.rsplit('/').next().unwrap_or(path);
        self.wire.lock().unwrap().push(format!("{method} {tail}"));
        Ok(if method.eq_ignore_ascii_case("POST") { self.post.clone() } else { self.get.clone() })
    }
}

fn client(post: Value, get: Value) -> VenueRestBybit<Script> {
    bybit_perp(&fixture(), Script { post, get, wire: Mutex::new(Vec::new()) })
}

fn order(order_client_id: &str) -> OrderRequest {
    serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "bybit", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.01, "order_type": "limit", "price": 50000.0
    }))
    .unwrap()
}

/// The recorded `order/realtime` envelope, with `list` as given.
fn recorded_envelope(list: Value) -> Value {
    let mut envelope: Value = serde_json::from_str(include_str!(
        "../fixtures/classes/order_realtime_dated_far_stop.json"
    ))
    .unwrap();
    envelope["result"]["list"] = list;
    envelope
}

fn found() -> Value {
    recorded_envelope(json!([{"orderId": "live-1", "orderLinkId": "c1"}]))
}

#[test]
fn an_envelope_with_no_ret_code_is_not_a_success() {
    for body in [json!({}), json!({"result": {"orderId": "x"}}), json!({"retCode": "0"})] {
        let e = unwrap_envelope(body.clone()).expect_err("no answer");
        assert_eq!(e.code, E_TIMEOUT_AMBIGUOUS, "{body}");
    }
    assert_eq!(unwrap_envelope(recorded_envelope(json!([]))).unwrap()["list"], json!([]));
    assert_eq!(unwrap_envelope(json!({"retCode": 10001, "retMsg": "m"})).unwrap_err().code, 10001);
}

/// A 2xx with no id is not an accept under an empty id: the order is asked for, and takes the venue's id.
#[test]
fn a_submit_answered_with_no_id_is_asked_for() {
    for post in [
        json!({"retCode": 0, "retMsg": "OK", "result": {}}),
        json!({"retCode": 0, "retMsg": "OK", "result": {"orderId": ""}}),
        json!({"retCode": 0, "retMsg": "OK", "result": {"orderId": null}}),
        json!({"retCode": 0, "retMsg": "OK"}),
        json!({"result": {"orderId": "not-an-answer"}}),
        json!({}),
    ] {
        let c = client(post.clone(), found());
        match &c.submit_order(&order("c1"))[..] {
            [Event::OrderSubmitted(_), Event::OrderAccepted(a)] => {
                assert_eq!(a.venue_order_id.as_deref(), Some("live-1"), "{post}");
            }
            other => panic!("{post}: {other:?}"),
        }
        assert_eq!(*c.transport.wire.lock().unwrap(), ["POST create", "GET realtime"], "{post}");

        let c = client(post.clone(), recorded_envelope(json!([])));
        let events = c.submit_order(&order("c1"));
        assert_eq!(events.len(), 1, "only OrderSubmitted, the ladder decides: {post}: {events:?}");
    }
}

/// A by-id read with no `list`, a row that names no order or a body with no `retCode` says nothing: the
/// active confirm stays silent and no resting order is rejected. A real empty `list` on BOTH endpoints is
/// the venue's "absent".
#[test]
fn a_status_read_that_names_no_order_says_nothing_and_an_empty_list_is_absent() {
    for get in [
        json!({"retCode": 0, "retMsg": "OK", "result": {}}),
        json!({"retCode": 0, "retMsg": "OK"}),
        json!({"retCode": 0, "retMsg": "OK", "result": {"list": {}}}),
        recorded_envelope(json!([{}])),
        recorded_envelope(json!([{"orderId": ""}])),
        recorded_envelope(json!([{"orderId": null}])),
        json!({"result": {"list": []}}),
        json!({}),
    ] {
        let events = client(json!({}), get.clone()).confirm_order("c1", 5);
        assert!(events.is_empty(), "{get}: {events:?}");
    }
    let c = client(json!({}), recorded_envelope(json!([])));
    assert!(matches!(c.confirm_order("c1", 5)[..], [Event::OrderRejected(_)]));
    assert_eq!(*c.transport.wire.lock().unwrap(), ["GET realtime", "GET history"]);
    assert!(matches!(
        client(json!({}), found()).confirm_order("c1", 5)[..],
        [Event::OrderAccepted(_)]
    ));
}

/// The batch: a body with no `retCode` and a success with no id are asked for, never an accept with no id.
#[test]
fn a_batch_answer_that_names_no_order_is_asked_for() {
    let ids = |post: Value| -> Vec<String> {
        let c = client(post, found());
        c.submit_batch(&[order("a"), order("b")])
            .iter()
            .skip(2)
            .map(|e| match e {
                Event::OrderAccepted(a) => {
                    a.venue_order_id.as_deref().unwrap_or("no id").to_string()
                }
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(ids(json!({})), ["live-1", "live-1"]);
    assert_eq!(
        ids(json!({"retCode": 0, "retMsg": "OK",
            "result": {"list": [{"orderId": "bb-1"}, {"orderId": ""}]},
            "retExtInfo": {"list": [{"code": 0}, {"code": 0}]}})),
        ["bb-1", "live-1"]
    );
}

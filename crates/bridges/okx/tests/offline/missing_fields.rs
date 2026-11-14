//! A 2xx that does not say what it should is not an answer. A body with no `code` read as success accepted
//! a submit under an empty id and let the orders read see no open orders; an empty `data` on a by-id read
//! read as "absent" rejected an order that may rest. Wrong shape is `Err`, a 2xx with no id goes to the
//! re-query, and only the venue's own code (`51603`) says absent. A REAL empty list stays a list: pinned on
//! a recorded envelope (`tests/fixtures/algo/rest_orders_history.json`, captured on the demo account) with
//! its rows taken out, because the repo holds no captured empty reply.

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_execution::recon::ReconcileClient;
use vike_model::OrderRequest;
use vike_model::events::Event;
use vike_okx::ReconcileClientOkx;
use vike_okx::execution::rest::VenueRestOkx;
use vike_okx::signer::OkxV5Signer;
use vike_okx::transport::client::unwrap_okx;
use vike_secrets::credentials::Credentials;

use crate::r6_okx_parity::{fixture, okx_perp};

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
    fn signed_json(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        _body: &str,
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        Ok(self.post.clone())
    }
    vike_bridge_core::no_public_call!();
}

fn client(post: Value, get: Value) -> VenueRestOkx<Script> {
    okx_perp(&fixture(), Script { post, get, wire: Mutex::new(Vec::new()) })
}

fn limit() -> OrderRequest {
    serde_json::from_value(json!({
        "order_client_id": "c1", "venue": "okx", "symbol": "BTC-USDT-SWAP",
        "side": 1, "qty": 0.01, "order_type": "limit", "price": 50000.0
    }))
    .unwrap()
}

fn stop() -> OrderRequest {
    serde_json::from_value(json!({
        "order_client_id": "sl1", "venue": "okx", "symbol": "BTC-USDT-SWAP", "side": -1,
        "qty": 0.01, "order_type": "stop", "trigger_price": 58000.0, "reduce_only": true
    }))
    .unwrap()
}

fn found() -> Value {
    json!({"code": "0", "msg": "", "data": [{"ordId": "live-1", "algoId": "algo-1"}]})
}

fn absent() -> Value {
    json!({"code": "51603", "msg": "Order does not exist", "data": []})
}

/// The recorded envelope, with `data` as given.
fn recorded_envelope(data: Value) -> Value {
    let mut envelope: Value =
        serde_json::from_str(include_str!("../fixtures/algo/rest_orders_history.json")).unwrap();
    envelope["data"] = data;
    envelope
}

#[test]
fn an_envelope_with_no_code_or_no_data_list_is_not_a_success() {
    for body in [
        json!({}),
        json!({"data": []}),
        json!({"data": [{"ordId": "x"}]}),
        json!({"code": true, "data": []}),
        json!({"code": "0"}),
        json!({"code": 0}),
        json!({"code": "0", "data": {}}),
        json!({"code": "0", "data": null}),
    ] {
        let e = unwrap_okx(body.clone()).expect_err("no answer");
        assert_eq!(e.code, E_TIMEOUT_AMBIGUOUS, "{body}");
    }
    assert_eq!(unwrap_okx(recorded_envelope(json!([]))).unwrap(), json!([]));
    assert_eq!(unwrap_okx(recorded_envelope(json!([{"a": 1}]))).unwrap(), json!([{"a": 1}]));
    assert_eq!(unwrap_okx(json!({"code": "51000", "msg": "m"})).unwrap_err().code, 51000);
    // a code that cannot be read is never the pre-send `0` ("it never left this host")
    assert_ne!(unwrap_okx(json!({"code": "oops", "msg": "m"})).unwrap_err().code, 0);
}

/// A 2xx with no id is not an accept under an empty id: the order is asked for, and takes the venue's id.
#[test]
fn a_submit_answered_with_no_id_is_asked_for() {
    for post in [
        json!({"code": "0", "msg": "", "data": [{"sCode": "0", "sMsg": ""}]}),
        json!({"code": "0", "msg": "", "data": [{"ordId": "", "sCode": "0"}]}),
        json!({"code": "0", "msg": "", "data": []}),
        json!({"code": "0", "msg": ""}),
        json!({}),
    ] {
        let c = client(post.clone(), found());
        match &c.submit_order(&limit())[..] {
            [Event::OrderSubmitted(_), Event::OrderAccepted(a)] => {
                assert_eq!(a.venue_order_id.as_deref(), Some("live-1"), "{post}");
            }
            other => panic!("{post}: {other:?}"),
        }
        assert_eq!(*c.transport.wire.lock().unwrap(), ["POST order", "GET order"], "{post}");

        let c = client(post.clone(), absent());
        let events = c.submit_order(&limit());
        assert_eq!(events.len(), 1, "only OrderSubmitted, the ladder decides: {post}: {events:?}");
    }
}

/// The same for a stop, on its own book: and a look that says nothing leaves it pre-ack, never ACCEPTED.
#[test]
fn a_stop_answered_with_no_id_is_asked_for_on_the_algo_book() {
    for post in [json!({"code": "0", "msg": "", "data": [{"sCode": "0"}]}), json!({})] {
        let c = client(post.clone(), found());
        match &c.submit_order(&stop())[..] {
            [Event::OrderSubmitted(_), Event::OrderAccepted(a)] => {
                assert_eq!(a.venue_order_id.as_deref(), Some("algo-1"), "{post}");
            }
            other => panic!("{post}: {other:?}"),
        }
        assert_eq!(*c.transport.wire.lock().unwrap(), ["POST order-algo", "GET order-algo"]);

        let c = client(post.clone(), json!({"code": "0", "msg": "", "data": []}));
        let events = c.submit_order(&stop());
        assert!(matches!(events[..], [Event::OrderSubmitted(_)]), "{post}: {events:?}");
    }
}

/// A by-id read: only the venue's code says absent. An empty `data`, a row with no id and a body with no
/// code say nothing, so the active confirm stays silent and no resting order is rejected.
#[test]
fn a_status_read_that_names_no_order_says_nothing() {
    for get in [
        json!({"code": "0", "msg": "", "data": []}),
        json!({"code": "0", "msg": "", "data": [{}]}),
        json!({"code": "0", "msg": "", "data": [{"ordId": null, "algoId": null}]}),
        json!({"code": "0", "msg": ""}),
        json!({}),
    ] {
        let events = client(json!({}), get.clone()).confirm_order("c1", 5);
        assert!(events.is_empty(), "{get}: {events:?}");
    }
    assert!(matches!(
        client(json!({}), absent()).confirm_order("c1", 5)[..],
        [Event::OrderRejected(_)]
    ));
    assert!(matches!(
        client(json!({}), found()).confirm_order("c1", 5)[..],
        [Event::OrderAccepted(_)]
    ));
}

/// The batch's rows: one with no `sCode`, a success with no id and a row that is not there are all asked
/// for, never a terminal "no batch ack".
#[test]
fn a_batch_row_that_says_nothing_is_asked_for() {
    let order = |order_client_id: &str| OrderRequest {
        order_client_id: order_client_id.to_string(),
        ..limit()
    };
    let post = json!({"code": "0", "msg": "", "data": [
        {"clOrdId": "a", "ordId": "o-1", "sCode": "0"},
        {"clOrdId": "b", "ordId": "", "sCode": "0"},
        {"clOrdId": "c", "ordId": "o-3"},
        {"clOrdId": "d", "sCode": "51008", "sMsg": "insufficient"}]});
    let c = client(post, found());
    let events = c.submit_batch(&[order("a"), order("b"), order("c"), order("d"), order("e")]);
    let answers: Vec<String> = events
        .iter()
        .skip(5)
        .map(|e| match e {
            Event::OrderAccepted(a) => {
                format!("{} {}", a.order_client_id, a.venue_order_id.as_deref().unwrap_or("no id"))
            }
            Event::OrderRejected(r) => format!("{} rejected", r.order_client_id),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(answers, ["a o-1", "b live-1", "c live-1", "d rejected", "e live-1"]);
}

fn recon(get: Value) -> ReconcileClientOkx<Script> {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    ReconcileClientOkx::new(
        OkxV5Signer::new(&creds, || 0),
        Script { post: json!({}), get, wire: Mutex::new(Vec::new()) },
        "https://unused.invalid",
        "BTC-USDT-SWAP",
        0.01,
    )
}

/// The orders read: the restore planner reads an order a successful report lacks as gone, so a body with
/// no `code` or no `data` must fail the report; a real empty list is a successful "nothing rests".
#[test]
fn the_orders_read_tells_no_answer_from_no_orders() {
    for body in [json!({}), json!({"code": "0", "msg": ""}), json!({"data": []})] {
        assert!(recon(body.clone()).fetch_order_status_reports(0).is_err(), "{body}");
    }
    assert!(recon(recorded_envelope(json!([]))).fetch_order_status_reports(0).unwrap().is_empty());
}

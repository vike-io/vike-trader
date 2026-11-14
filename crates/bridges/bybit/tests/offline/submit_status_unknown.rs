//! A bybit code the venue's OWN table files as "outcome unknown" (`ErrorKind::Timeout`), or that says
//! the server had trouble (`10016`), must take the
//! same re-query path the transport's ambiguous sentinel takes, in every submit arm (single, stop,
//! whole-batch, per-element) and read as "outcome unknown" on a modify. Bybit answers a business
//! failure with HTTP 200 + `{retCode,retMsg}`, so the stub hands the ENVELOPE back as `Ok` exactly
//! as the shared reply engine (`vike_bridge_core::transport::reply`'s `classify_rest_reply`) does and the client's `unwrap_envelope` raises the code. A definite
//! code (`110007` insufficient balance, `10006` rate limit) still ends in a terminal reject WITHOUT
//! touching the re-query endpoints.

use std::cell::Cell;

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_model::OrderRequest;
use vike_model::events::Event;

use crate::r6_bybit_parity::{bybit_perp, fixture};

/// `create` is the envelope every create/create-batch/amend POST answers; `live` is the
/// `/order/realtime` row list (history is always empty). `queries` counts re-query GETs.
struct Stub {
    create: Value,
    live: Value,
    queries: Cell<u32>,
    /// The re-query itself fails (the venue is restarting): `Err`, not an empty list.
    query_fails: Cell<bool>,
}

fn envelope(ret_code: i64) -> Value {
    json!({"retCode": ret_code, "retMsg": "venue words", "result": {}, "retExtInfo": {}})
}

impl RestTransportJson for Stub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        if method.eq_ignore_ascii_case("POST") {
            return Ok(self.create.clone());
        }
        self.queries.set(self.queries.get() + 1);
        if self.query_fails.get() {
            return Ok(envelope(10016));
        }
        let list = if path.ends_with("/order/realtime") { self.live.clone() } else { json!([]) };
        Ok(json!({"retCode": 0, "retMsg": "OK", "result": {"list": list}}))
    }
}

fn client(create: Value, live: Value) -> vike_bybit::execution::rest::VenueRestBybit<Stub> {
    bybit_perp(
        &fixture(),
        Stub { create, live, queries: Cell::new(0), query_fails: Cell::new(false) },
    )
}

fn req(order_client_id: &str, order_type: &str) -> OrderRequest {
    serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "bybit", "symbol": "BTCUSDT",
        "side": -1, "qty": 0.01, "order_type": order_type, "price": 50000.0,
        "trigger_price": 58000.0, "reduce_only": order_type == "stop"
    }))
    .unwrap()
}

fn live_rows() -> Value {
    json!([{"orderId": "live-1", "orderLinkId": "x"}])
}

fn accepted_id(e: &Event) -> Option<String> {
    match e {
        Event::OrderAccepted(a) => a.venue_order_id.as_ref().map(|v| v.to_string()),
        _ => None,
    }
}

/// The codes bybit's table files as `Timeout`, and the one that says the server had trouble.
const UNKNOWN: [i64; 4] = [10000, 170007, 170146, 10016];
/// Definite refusals: must stay terminal and must not re-query.
const DEFINITE: [i64; 2] = [110007, 10006];

#[test]
fn single_and_stop_submit_with_a_status_unknown_code_requery_and_find_the_live_order() {
    for code in UNKNOWN {
        for order_type in ["limit", "stop"] {
            let c = client(envelope(code), live_rows());
            let event = c.submit_order(&req("c1", order_type));
            assert_eq!(
                accepted_id(event.last().unwrap()).as_deref(),
                Some("live-1"),
                "{code} {order_type}"
            );
            assert_eq!(c.transport.queries.get(), 1, "{code} {order_type}: re-queried once");
        }
    }
}

#[test]
fn single_submit_status_unknown_and_absent_on_the_venue_says_nothing() {
    let c = client(envelope(10000), json!([]));
    let event = c.submit_order(&req("c1", "limit"));
    assert!(
        !event.iter().any(|e| matches!(e, Event::OrderRejected(_))),
        "the POST may land: {event:?}"
    );
    assert_eq!(event.len(), 1, "only OrderSubmitted, the stuck-order ladder decides: {event:?}");
    assert_eq!(c.transport.queries.get(), 2, "realtime then history");
}

/// "The service is restarting" and the re-query fails too: nothing is known, so nothing is claimed. No
/// terminal reject (the order may be live) and no id-less accept (a phantom no sweep ever finds): the
/// order stays pre-ack for the watchdog's confirm. The timeout codes say the same.
#[test]
fn a_restarting_venue_whose_requery_also_fails_claims_nothing() {
    for code in [10016, 10000] {
        let c = client(envelope(code), live_rows());
        c.transport.query_fails.set(true);
        let event = c.submit_order(&req("c1", "limit"));
        assert!(matches!(event[..], [Event::OrderSubmitted(_)]), "{code}: {event:?}");
    }
}

#[test]
fn definite_codes_still_reject_without_a_requery() {
    for code in DEFINITE {
        let c = client(envelope(code), live_rows());
        let event = c.submit_order(&req("c1", "limit"));
        match event.last().unwrap() {
            Event::OrderRejected(r) => assert_eq!(r.reason, "venue words", "{code}"),
            other => panic!("{code}: expected OrderRejected, got {other:?}"),
        }
        assert_eq!(c.transport.queries.get(), 0, "{code}: no re-query");
    }
}

#[test]
fn whole_batch_status_unknown_requeries_each_order() {
    for code in UNKNOWN {
        let c = client(envelope(code), live_rows());
        let event = c.submit_batch(&[req("a", "limit"), req("b", "limit")]);
        let tail: Vec<_> = event.iter().skip(2).map(accepted_id).collect();
        assert_eq!(tail, vec![Some("live-1".to_string()); 2], "{code}");
    }
    for code in DEFINITE {
        let c = client(envelope(code), live_rows());
        let event = c.submit_batch(&[req("a", "limit"), req("b", "limit")]);
        assert!(event.iter().skip(2).all(|e| matches!(e, Event::OrderRejected(_))), "{code}");
        assert_eq!(c.transport.queries.get(), 0, "{code}: no re-query");
    }
}

#[test]
fn batch_element_status_unknown_requeries_only_that_order() {
    let create = json!({"retCode": 0, "retMsg": "OK",
        "result": {"list": [{"orderId": "bb-1"}, {"orderId": ""}, {"orderId": ""}]},
        "retExtInfo": {"list": [
            {"code": 0, "msg": "OK"}, {"code": 170146, "msg": "Order creation timeout"},
            {"code": 110007, "msg": "insufficient"}]}});
    let c = client(create, live_rows());
    let event = c.submit_batch(&[req("a", "limit"), req("b", "limit"), req("c", "limit")]);
    assert_eq!(accepted_id(&event[3]).as_deref(), Some("bb-1"));
    assert_eq!(accepted_id(&event[4]).as_deref(), Some("live-1"), "unknown element re-queried");
    match &event[5] {
        Event::OrderRejected(r) => assert_eq!(r.reason, "insufficient"),
        other => panic!("definite element must reject, got {other:?}"),
    }
    assert_eq!(c.transport.queries.get(), 1, "only the unknown element is re-queried");
}

#[test]
fn modify_with_a_status_unknown_code_says_outcome_unknown() {
    let reason = |code| {
        let c = client(envelope(code), json!([]));
        match &c.modify_order("c1", Some(0.02), None)[0] {
            Event::OrderModifyRejected(m) => m.reason.to_string(),
            other => panic!("expected OrderModifyRejected, got {other:?}"),
        }
    };
    for code in UNKNOWN {
        assert!(reason(code).contains("outcome unknown"), "{code}");
    }
    assert_eq!(reason(110017), "venue words");
}

/// A one-shot loopback server answering `status` with `body`; its base URL.
fn answering(status: u16, body: &str) -> String {
    use std::io::{Read, Write};
    let reply = format!(
        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let _ = socket.read(&mut [0u8; 2048]);
            let _ = socket.write_all(reply.as_bytes());
        }
    });
    format!("http://{addr}")
}

/// **A write answered 5xx with no venue code is unknown, whatever the status**, through the REAL
/// transport: it was the status-as-code, a terminal reject of an order that may be live and an amend
/// worded as refused. The venue's own non-zero code stays its verdict, and a read keeps its status.
#[test]
fn a_write_answered_5xx_with_no_venue_code_is_unknown_and_a_read_keeps_its_status() {
    use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
    let creds = vike_secrets::credentials::Credentials::for_test("k", "s", None);
    let signer = BybitV5Signer::new(&creds, || 0);
    let t = rest_transport();
    let call = |status, body, method| {
        t.signed(&answering(status, body), "/v5/order/create", method, &[], &signer).unwrap_err()
    };
    for status in [500, 501, 520, 521, 522, 523, 525, 526, 527, 530] {
        for body in ["", "upstream failed", r#"{"retCode":0}"#] {
            assert_eq!(call(status, body, "POST").code, E_TIMEOUT_AMBIGUOUS, "{status} {body}");
        }
    }
    assert_eq!(call(500, r#"{"retCode":10001,"retMsg":"bad"}"#, "POST").code, 10001);
    assert_eq!(call(400, "", "POST").code, 400, "a 4xx is the venue's verdict");
    assert_eq!(call(500, "", "GET").code, 500, "a read has no outcome to be unsure of");

    let event =
        vike_bybit::transport::error_codes::modify_failed("c1", &call(500, "", "POST"), None);
    match event {
        Event::OrderModifyRejected(m) => assert!(m.reason.contains("outcome unknown"), "{m:?}"),
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
}

//! An okx code the venue's OWN table files as "outcome unknown" (`ErrorKind::Timeout`: `50004`,
//! "does not mean the request was successful or failed") must take the same path the transport's
//! ambiguous sentinel takes, in every submit arm: single (re-query by `clOrdId`), the algo stop
//! (re-query on `order-algo` by `algoClOrdId`), the whole batch and each batch
//! element (re-query), and read as "outcome unknown" on a modify. The code reaches the client in
//! three shapes, all pinned: an HTTP 200 envelope whose top-level `code` is it, an HTTP 200 envelope
//! whose `data[0].sCode` is it, and a non-2xx the transport turned into `Err` (`OKX_REPLY`'s refusal). A
//! definite code (`51008`, `51000`) still ends in a terminal reject without touching the re-query.

use std::cell::Cell;

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_model::OrderRequest;
use vike_model::events::Event;
use vike_okx::execution::rest::VenueRestOkx;
use vike_okx::signer::OkxV5Signer;
use vike_okx::transport::client::rest_transport;

use crate::r6_okx_parity::{fixture, okx_perp};

/// `post` answers every POST (order, amend, algo, batch); a GET on `/trade/order` is the re-query
/// (`live` = a row, or the venue's `51603` "does not exist"); `queries` counts those GETs.
struct Stub {
    post: Result<Value, VenueApiError>,
    live: bool,
    queries: Cell<u32>,
    /// The re-query itself fails (the venue is busy): `Err`, not "does not exist".
    query_fails: Cell<bool>,
}

impl RestTransportJson for Stub {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        method: &str,
        _params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        if method.eq_ignore_ascii_case("POST") {
            return self.post.clone();
        }
        self.queries.set(self.queries.get() + 1);
        if self.query_fails.get() {
            return Ok(json!({"code": "50013", "msg": "System is busy", "data": []}));
        }
        Ok(if self.live {
            json!({"code": "0", "msg": "", "data": [{"ordId": "live-1", "algoId": "algo-1"}]})
        } else {
            json!({"code": "51603", "msg": "Order does not exist", "data": []})
        })
    }
    fn signed_json(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        _body: &str,
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        self.post.clone()
    }
    vike_bridge_core::no_public_call!();
}

fn client(post: Result<Value, VenueApiError>, live: bool) -> VenueRestOkx<Stub> {
    okx_perp(&fixture(), Stub { post, live, queries: Cell::new(0), query_fails: Cell::new(false) })
}

fn req(order_client_id: &str, order_type: &str) -> OrderRequest {
    serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "okx", "symbol": "BTC-USDT-SWAP",
        "side": -1, "qty": 0.01, "order_type": order_type, "price": 50000.0,
        "trigger_price": 58000.0, "reduce_only": order_type == "stop"
    }))
    .unwrap()
}

/// The three shapes a single-order failure with `code` arrives in.
fn shapes(code: i64) -> Vec<(&'static str, Result<Value, VenueApiError>)> {
    let msg = "venue words";
    vec![
        ("envelope code", Ok(json!({"code": code.to_string(), "msg": msg, "data": []}))),
        (
            "data[0].sCode",
            Ok(json!({"code": "1", "msg": "All operations failed",
                "data": [{"sCode": code.to_string(), "sMsg": msg}]})),
        ),
        ("non-2xx", Err(VenueApiError { code, msg: msg.into() })),
    ]
}

fn accepted(e: &Event) -> Option<Option<String>> {
    match e {
        Event::OrderAccepted(a) => Some(a.venue_order_id.as_ref().map(|v| v.to_string())),
        _ => None,
    }
}

#[test]
fn single_submit_with_50004_requeries_and_finds_the_live_order() {
    for (shape, post) in shapes(50004) {
        let c = client(post, true);
        let event = c.submit_order(&req("c1", "limit"));
        assert_eq!(accepted(event.last().unwrap()), Some(Some("live-1".into())), "{shape}");
        assert_eq!(c.transport.queries.get(), 1, "{shape}");
    }
}

/// The codes that say the server had trouble: the order may be live, so they re-query like `50004`,
/// and the venue's "does not exist" is the terminal reject. Every shape, stop included.
#[test]
fn a_venue_in_trouble_is_requeried_and_not_read_as_a_refusal() {
    for code in [50001, 50013, 50026] {
        for (shape, post) in shapes(code) {
            let c = client(post.clone(), true);
            let event = c.submit_order(&req("c1", "limit"));
            assert_eq!(
                accepted(event.last().unwrap()),
                Some(Some("live-1".into())),
                "{code} {shape}"
            );
            let c = client(post.clone(), true);
            let event = c.submit_order(&req("sl1", "stop"));
            assert_eq!(
                accepted(event.last().unwrap()),
                Some(Some("algo-1".into())),
                "{code} {shape}"
            );
            let c = client(post, false);
            let event = c.submit_order(&req("c1", "limit"));
            assert_eq!(event.len(), 1, "only OrderSubmitted, the ladder decides: {code} {shape}");
        }
    }
}

/// The venue is busy and the re-query fails too: nothing is known, so nothing is claimed. No terminal
/// reject (the order may be live) and no id-less accept (a phantom no sweep ever finds): the order stays
/// pre-ack for the watchdog's confirm. `50004` says the same.
#[test]
fn a_busy_venue_whose_requery_also_fails_claims_nothing() {
    for code in [50001, 50013, 50026, 50004] {
        let c = client(shapes(code).remove(0).1, true);
        c.transport.query_fails.set(true);
        let event = c.submit_order(&req("c1", "limit"));
        assert!(matches!(event[..], [Event::OrderSubmitted(_)]), "{code}: {event:?}");
    }
}

#[test]
fn single_submit_50004_and_absent_on_the_venue_says_nothing() {
    let c = client(shapes(50004).remove(0).1, false);
    let event = c.submit_order(&req("c1", "limit"));
    assert_eq!(event.len(), 1, "only OrderSubmitted, the stuck-order ladder decides: {event:?}");
    assert_eq!(c.transport.queries.get(), 1);
}

#[test]
fn definite_codes_still_reject_without_a_requery() {
    for code in [51008, 51000] {
        for (shape, post) in shapes(code) {
            let c = client(post, true);
            let event = c.submit_order(&req("c1", "limit"));
            match event.last().unwrap() {
                Event::OrderRejected(r) => assert_eq!(r.reason, "venue words", "{code} {shape}"),
                other => panic!("{code} {shape}: expected OrderRejected, got {other:?}"),
            }
            assert_eq!(c.transport.queries.get(), 0, "{code} {shape}: no re-query");
        }
    }
}

#[test]
fn stop_algo_with_50004_asks_the_algo_book_and_a_definite_code_rejects() {
    for (shape, post) in shapes(50004) {
        let c = client(post.clone(), true);
        let event = c.submit_order(&req("sl1", "stop"));
        assert_eq!(accepted(event.last().unwrap()), Some(Some("algo-1".into())), "{shape}");
        assert_eq!(c.transport.queries.get(), 1, "{shape}: one look at the algo book");
        // Not listed yet: one read just after the POST says nothing, the watchdog's confirm asks again.
        let c = client(post, false);
        let event = c.submit_order(&req("sl1", "stop"));
        assert!(matches!(event[..], [Event::OrderSubmitted(_)]), "{shape}: {event:?}");
    }
    let c = client(shapes(51000).remove(0).1, true);
    let event = c.submit_order(&req("sl1", "stop"));
    assert!(matches!(event.last().unwrap(), Event::OrderRejected(_)), "{event:?}");
}

#[test]
fn whole_batch_50004_requeries_each_order() {
    let http200 = Ok(json!({"code": "50004", "msg": "timeout", "data": []}));
    let non2xx = Err(VenueApiError { code: 50004, msg: "timeout".into() });
    for (shape, post) in [("envelope code", http200), ("non-2xx", non2xx)] {
        let c = client(post, true);
        let event = c.submit_batch(&[req("a", "limit"), req("b", "limit")]);
        let tail: Vec<_> = event.iter().skip(2).map(accepted).collect();
        assert_eq!(tail, vec![Some(Some("live-1".to_string())); 2], "{shape}");
    }
    // a definite whole-request failure still rejects every order, with no re-query
    let c = client(Err(VenueApiError { code: 51000, msg: "bad".into() }), true);
    let event = c.submit_batch(&[req("a", "limit"), req("b", "limit")]);
    assert!(event.iter().skip(2).all(|e| matches!(e, Event::OrderRejected(_))), "{event:?}");
    assert_eq!(c.transport.queries.get(), 0);
}

#[test]
fn batch_element_50004_requeries_only_that_order() {
    let post = Ok(json!({"code": "2", "msg": "", "data": [
        {"clOrdId": "a", "ordId": "o-1", "sCode": "0", "sMsg": ""},
        {"clOrdId": "b", "ordId": "", "sCode": "50004", "sMsg": "timeout"},
        {"clOrdId": "c", "ordId": "", "sCode": "51008", "sMsg": "insufficient"}]}));
    let c = client(post, true);
    let event = c.submit_batch(&[req("a", "limit"), req("b", "limit"), req("c", "limit")]);
    assert_eq!(accepted(&event[3]), Some(Some("o-1".into())));
    assert_eq!(accepted(&event[4]), Some(Some("live-1".into())), "unknown element re-queried");
    match &event[5] {
        Event::OrderRejected(r) => assert_eq!(r.reason, "insufficient"),
        other => panic!("definite element must reject, got {other:?}"),
    }
    assert_eq!(c.transport.queries.get(), 1, "only the unknown element is re-queried");
}

#[test]
fn modify_with_50004_says_outcome_unknown() {
    let reason = |code| {
        let c = client(shapes(code).remove(0).1, false);
        match &c.modify_order("c1", Some(0.02), None)[0] {
            Event::OrderModifyRejected(m) => m.reason.to_string(),
            other => panic!("expected OrderModifyRejected, got {other:?}"),
        }
    };
    assert!(reason(50004).contains("outcome unknown"));
    assert_eq!(reason(51503), "venue words");
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
/// worded as refused. The venue's own non-zero code stays its verdict (`50004` then goes through this
/// crate's own table), and a read keeps its status.
#[test]
fn a_write_answered_5xx_with_no_venue_code_is_unknown_and_a_read_keeps_its_status() {
    use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
    let creds = vike_secrets::credentials::Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    let signer = OkxV5Signer::new(&creds, || 0);
    let t = rest_transport(true);
    let call = |status, body, method| {
        t.signed(&answering(status, body), "/api/v5/trade/order", method, &[], &signer).unwrap_err()
    };
    for status in [500, 501, 520, 521, 522, 523, 525, 526, 527, 530] {
        for body in ["", "upstream failed", r#"{"code":"0"}"#, r#"{"code":"not a number"}"#] {
            assert_eq!(call(status, body, "POST").code, E_TIMEOUT_AMBIGUOUS, "{status} {body}");
        }
    }
    assert_eq!(call(500, r#"{"code":"51000","msg":"bad"}"#, "POST").code, 51000);
    assert_eq!(call(400, "", "POST").code, 400, "a 4xx is the venue's verdict");
    assert_eq!(call(500, "", "GET").code, 500, "a read has no outcome to be unsure of");

    let event = vike_okx::transport::error_codes::modify_failed("c1", &call(500, "", "POST"), None);
    match event {
        Event::OrderModifyRejected(m) => assert!(m.reason.contains("outcome unknown"), "{m:?}"),
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
}

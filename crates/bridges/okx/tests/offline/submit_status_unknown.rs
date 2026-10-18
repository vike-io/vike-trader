//! An okx code the venue's OWN table files as "outcome unknown" (`ErrorKind::Timeout`: `50004`,
//! "does not mean the request was successful or failed") must take the same path the transport's
//! ambiguous sentinel takes, in every submit arm: single (re-query by `clOrdId`), the algo stop
//! (optimistic accept: an algo is not queryable by `clOrdId`), the whole batch and each batch
//! element (re-query), and read as "outcome unknown" on a modify. The code reaches the client in
//! three shapes, all pinned: an HTTP 200 envelope whose top-level `code` is it, an HTTP 200 envelope
//! whose `data[0].sCode` is it, and a non-2xx the transport turned into `Err` (`http_error`). A
//! definite code (`51008`, `51000`) still ends in a terminal reject without touching the re-query.

use std::cell::Cell;

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_model::OrderRequest;
use vike_model::events::Event;
use vike_okx::perp::OkxPerpRest;
use vike_okx::transport::OkxTransport;

use crate::r6_okx_parity::{fixture, okx_perp};

/// `post` answers every POST (order, amend, algo, batch); a GET on `/trade/order` is the re-query
/// (`live` = a row, or the venue's `51603` "does not exist"); `queries` counts those GETs.
struct Stub {
    post: Result<Value, VenueApiError>,
    live: bool,
    queries: Cell<u32>,
}

impl OkxTransport for Stub {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        method: &str,
        _params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        if method.eq_ignore_ascii_case("POST") {
            return self.post.clone();
        }
        self.queries.set(self.queries.get() + 1);
        Ok(if self.live {
            json!({"code": "0", "msg": "", "data": [{"ordId": "live-1"}]})
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
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        self.post.clone()
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

fn client(post: Result<Value, VenueApiError>, live: bool) -> OkxPerpRest<Stub> {
    okx_perp(&fixture(), Stub { post, live, queries: Cell::new(0) })
}

fn req(coid: &str, order_type: &str) -> OrderRequest {
    serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": "BTC-USDT-SWAP",
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
        let ev = c.submit_order(&req("c1", "limit"));
        assert_eq!(accepted(ev.last().unwrap()), Some(Some("live-1".into())), "{shape}");
        assert_eq!(c.transport.queries.get(), 1, "{shape}");
    }
}

#[test]
fn single_submit_50004_and_absent_on_the_venue_is_the_terminal_reject() {
    let c = client(shapes(50004).remove(0).1, false);
    let ev = c.submit_order(&req("c1", "limit"));
    assert!(matches!(ev.last().unwrap(), Event::OrderRejected(_)), "{ev:?}");
    assert_eq!(c.transport.queries.get(), 1);
}

#[test]
fn definite_codes_still_reject_without_a_requery() {
    for code in [51008, 51000] {
        for (shape, post) in shapes(code) {
            let c = client(post, true);
            let ev = c.submit_order(&req("c1", "limit"));
            match ev.last().unwrap() {
                Event::OrderRejected(r) => assert_eq!(r.reason, "venue words", "{code} {shape}"),
                other => panic!("{code} {shape}: expected OrderRejected, got {other:?}"),
            }
            assert_eq!(c.transport.queries.get(), 0, "{code} {shape}: no re-query");
        }
    }
}

#[test]
fn stop_algo_with_50004_is_accepted_optimistically_and_a_definite_code_rejects() {
    for (shape, post) in shapes(50004) {
        let c = client(post, false);
        let ev = c.submit_order(&req("sl1", "stop"));
        assert_eq!(accepted(ev.last().unwrap()), Some(None), "{shape}");
        assert_eq!(c.transport.queries.get(), 0, "{shape}: an algo is not queryable by clOrdId");
    }
    let c = client(shapes(51000).remove(0).1, true);
    let ev = c.submit_order(&req("sl1", "stop"));
    assert!(matches!(ev.last().unwrap(), Event::OrderRejected(_)), "{ev:?}");
}

#[test]
fn whole_batch_50004_requeries_each_order() {
    let http200 = Ok(json!({"code": "50004", "msg": "timeout", "data": []}));
    let non2xx = Err(VenueApiError { code: 50004, msg: "timeout".into() });
    for (shape, post) in [("envelope code", http200), ("non-2xx", non2xx)] {
        let c = client(post, true);
        let ev = c.submit_batch(&[req("a", "limit"), req("b", "limit")]);
        let tail: Vec<_> = ev.iter().skip(2).map(accepted).collect();
        assert_eq!(tail, vec![Some(Some("live-1".to_string())); 2], "{shape}");
    }
    // a definite whole-request failure still rejects every order, with no re-query
    let c = client(Err(VenueApiError { code: 51000, msg: "bad".into() }), true);
    let ev = c.submit_batch(&[req("a", "limit"), req("b", "limit")]);
    assert!(ev.iter().skip(2).all(|e| matches!(e, Event::OrderRejected(_))), "{ev:?}");
    assert_eq!(c.transport.queries.get(), 0);
}

#[test]
fn batch_element_50004_requeries_only_that_order() {
    let post = Ok(json!({"code": "2", "msg": "", "data": [
        {"clOrdId": "a", "ordId": "o-1", "sCode": "0", "sMsg": ""},
        {"clOrdId": "b", "ordId": "", "sCode": "50004", "sMsg": "timeout"},
        {"clOrdId": "c", "ordId": "", "sCode": "51008", "sMsg": "insufficient"}]}));
    let c = client(post, true);
    let ev = c.submit_batch(&[req("a", "limit"), req("b", "limit"), req("c", "limit")]);
    assert_eq!(accepted(&ev[3]), Some(Some("o-1".into())));
    assert_eq!(accepted(&ev[4]), Some(Some("live-1".into())), "unknown element re-queried");
    match &ev[5] {
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

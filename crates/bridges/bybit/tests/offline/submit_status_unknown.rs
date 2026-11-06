//! A bybit code the venue's OWN table files as "outcome unknown" (`ErrorKind::Timeout`) must take the
//! same re-query path the transport's ambiguous sentinel takes, in every submit arm (single, stop,
//! whole-batch, per-element) and read as "outcome unknown" on a modify. Bybit answers a business
//! failure with HTTP 200 + `{retCode,retMsg}`, so the stub hands the ENVELOPE back as `Ok` exactly
//! as `UreqBybitTransport::run` does and the client's `unwrap_envelope` raises the code. A definite
//! code (`110007` insufficient balance, `10006` rate limit) still ends in a terminal reject WITHOUT
//! touching the re-query endpoints.

use std::cell::Cell;

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::transport::BybitTransport;
use vike_model::OrderRequest;
use vike_model::events::Event;

use crate::r6_bybit_parity::{bybit_perp, fixture};

/// `create` is the envelope every create/create-batch/amend POST answers; `live` is the
/// `/order/realtime` row list (history is always empty). `queries` counts re-query GETs.
struct Stub {
    create: Value,
    live: Value,
    queries: Cell<u32>,
}

fn envelope(ret_code: i64) -> Value {
    json!({"retCode": ret_code, "retMsg": "venue words", "result": {}, "retExtInfo": {}})
}

impl BybitTransport for Stub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        if method.eq_ignore_ascii_case("POST") {
            return Ok(self.create.clone());
        }
        self.queries.set(self.queries.get() + 1);
        let list = if path.ends_with("/order/realtime") { self.live.clone() } else { json!([]) };
        Ok(json!({"retCode": 0, "retMsg": "OK", "result": {"list": list}}))
    }
}

fn client(create: Value, live: Value) -> vike_bybit::perp::BybitPerpRest<Stub> {
    bybit_perp(&fixture(), Stub { create, live, queries: Cell::new(0) })
}

fn req(coid: &str, order_type: &str) -> OrderRequest {
    serde_json::from_value(json!({
        "client_order_id": coid, "venue": "bybit", "symbol": "BTCUSDT",
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

/// The codes bybit's table files as `Timeout`.
const UNKNOWN: [i64; 3] = [10000, 170007, 170146];
/// Definite refusals: must stay terminal and must not re-query.
const DEFINITE: [i64; 2] = [110007, 10006];

#[test]
fn single_and_stop_submit_with_a_status_unknown_code_requery_and_find_the_live_order() {
    for code in UNKNOWN {
        for order_type in ["limit", "stop"] {
            let c = client(envelope(code), live_rows());
            let ev = c.submit_order(&req("c1", order_type));
            assert_eq!(
                accepted_id(ev.last().unwrap()).as_deref(),
                Some("live-1"),
                "{code} {order_type}"
            );
            assert_eq!(c.transport.queries.get(), 1, "{code} {order_type}: re-queried once");
        }
    }
}

#[test]
fn single_submit_status_unknown_and_absent_on_the_venue_is_the_terminal_reject() {
    let c = client(envelope(10000), json!([]));
    let ev = c.submit_order(&req("c1", "limit"));
    assert!(matches!(ev.last().unwrap(), Event::OrderRejected(_)), "{ev:?}");
    assert_eq!(c.transport.queries.get(), 2, "realtime then history");
}

#[test]
fn definite_codes_still_reject_without_a_requery() {
    for code in DEFINITE {
        let c = client(envelope(code), live_rows());
        let ev = c.submit_order(&req("c1", "limit"));
        match ev.last().unwrap() {
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
        let ev = c.submit_batch(&[req("a", "limit"), req("b", "limit")]);
        let tail: Vec<_> = ev.iter().skip(2).map(accepted_id).collect();
        assert_eq!(tail, vec![Some("live-1".to_string()); 2], "{code}");
    }
    for code in DEFINITE {
        let c = client(envelope(code), live_rows());
        let ev = c.submit_batch(&[req("a", "limit"), req("b", "limit")]);
        assert!(ev.iter().skip(2).all(|e| matches!(e, Event::OrderRejected(_))), "{code}");
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
    let ev = c.submit_batch(&[req("a", "limit"), req("b", "limit"), req("c", "limit")]);
    assert_eq!(accepted_id(&ev[3]).as_deref(), Some("bb-1"));
    assert_eq!(accepted_id(&ev[4]).as_deref(), Some("live-1"), "unknown element re-queried");
    match &ev[5] {
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

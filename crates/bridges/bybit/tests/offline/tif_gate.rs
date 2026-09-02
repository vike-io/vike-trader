//! tif step-2 submit-gate pins (offline): a TIF bybit cannot express (GTD/Day) yields the
//! emitter-split pair `[OrderSubmitted, OrderRejected]` and the wire is NEVER touched — on the
//! single submit and per-order inside the native create-batch (denied orders are partitioned out
//! of the wire chunk; supported ones still batch). The mapped GTC/IOC/FOK wire strings and the
//! default-TIF byte-identity are pinned in `r6_bybit_parity.rs` against the golden fixture.

use std::sync::Mutex;

use serde_json::{json, Value};
use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::perp::BybitPerpRest;
use vike_bybit::transport::BybitTransport;
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties, TimeInForce};

fn signer() -> BybitV5Signer {
    BybitV5Signer::new(
        &Credentials { api_key: "k".into(), api_secret: "s".into(), passphrase: None },
        || 0,
    )
}

/// A transport that PANICS on any wire touch — proves the deny gate returns before the POST.
struct NoWire;
impl BybitTransport for NoWire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        panic!("denied TIF must never reach the wire ({path})");
    }
}

/// Captures the signed params and acks every order — for the mixed-batch partition pin.
struct Capture {
    calls: Mutex<Vec<Vec<(String, Value)>>>,
}
impl BybitTransport for Capture {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
        let n = params
            .iter()
            .find(|(k, _)| *k == "request")
            .and_then(|(_, v)| v.as_array().map(Vec::len))
            .unwrap_or(1);
        let acks: Vec<Value> = (0..n).map(|i| json!({ "orderId": format!("oid-{i}") })).collect();
        let exts: Vec<Value> = (0..n).map(|_| json!({ "code": 0, "msg": "OK" })).collect();
        Ok(json!({
            "retCode": 0, "retMsg": "OK",
            "result": { "list": acks },
            "retExtInfo": { "list": exts },
        }))
    }
}

fn make<T: BybitTransport>(transport: T) -> BybitPerpRest<T> {
    BybitPerpRest {
        signer: signer(),
        transport,
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
    }
}

fn limit_req(coid: &str, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "bybit".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.01,
        order_type: "limit".to_string(),
        price: Some(50000.0),
        time_in_force: tif,
        ..Default::default()
    }
}

#[test]
fn submit_denies_gtd_and_day_without_touching_the_wire() {
    let client = make(NoWire);
    for (coid, tif) in [("c-gtd", TimeInForce::Gtd), ("c-day", TimeInForce::Day)] {
        let events = VenueRest::submit_order(&client, &limit_req(coid, tif));
        assert_eq!(events.len(), 2, "{coid}: exactly the emitter-split pair: {events:?}");
        assert!(matches!(&events[0], Event::OrderSubmitted(s) if s.client_order_id == coid));
        match &events[1] {
            Event::OrderRejected(r) => {
                assert_eq!(r.client_order_id, coid);
                assert!(r.reason.contains("not supported on bybit"), "loud deny: {}", r.reason);
            }
            other => panic!("{coid}: expected terminal OrderRejected, got {other:?}"),
        }
    }
}

/// A mixed batch partitions: the denied order is terminally rejected and never enters the wire
/// chunk; the supported orders still batch (ONE wire call whose request array carries only the
/// two supported coids, with their honored TIFs).
#[test]
fn batch_partitions_denied_tifs_out_of_the_wire_chunk() {
    let client = make(Capture { calls: Mutex::new(Vec::new()) });
    let requests = vec![
        limit_req("c-ok-1", TimeInForce::Gtc),
        limit_req("c-bad", TimeInForce::Day),
        limit_req("c-ok-2", TimeInForce::Fok),
    ];
    let events = client.submit_batch(&requests);
    let rejected: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.client_order_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(rejected, ["c-bad"], "only the denied TIF is rejected");
    let accepted: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderAccepted(a) => Some(a.client_order_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(accepted, ["c-ok-1", "c-ok-2"], "the supported orders still batch");

    let calls = client.transport.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "one wire chunk for the two supported orders");
    let batch = calls[0].iter().find(|(k, _)| k == "request").map(|(_, v)| v.clone()).unwrap();
    let coids: Vec<_> = batch
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o.get("orderLinkId").and_then(|v| v.as_str()).unwrap().to_string())
        .collect();
    assert_eq!(coids, ["c-ok-1", "c-ok-2"], "the denied order never reaches the wire");
    let tifs: Vec<_> = batch
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o.get("timeInForce").and_then(|v| v.as_str()).unwrap().to_string())
        .collect();
    assert_eq!(tifs, ["GTC", "FOK"], "honored TIFs ride the wire");
}

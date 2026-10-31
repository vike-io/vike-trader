//! tif step-2 submit-gate pins (offline): a TIF okx cannot express (GTD/Day — no such ordType
//! on V5) yields the emitter-split pair `[OrderSubmitted, OrderRejected]` and the wire is NEVER
//! touched — on the single submit and per-order inside the native batch (denied orders are
//! partitioned out of the ARRAY-body wire chunk; supported ones still batch, carrying their
//! honored ordTypes). The mapped `ioc`/`fok` ordTypes and the default-TIF byte-identity
//! (`ordType:"limit"` — the venue default) are pinned in `r6_okx_parity.rs` against the golden
//! fixture.

use std::assert_matches;
use std::sync::Mutex;

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties, TimeInForce};
use vike_okx::perp::OkxPerpRest;
use vike_okx::transport::OkxTransport;
use vike_secrets::credentials::Credentials;

fn signer() -> OkxV5Signer {
    OkxV5Signer::new(
        &Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: Some("p".to_string().into()),
        },
        || 0,
    )
}

/// A transport that PANICS on any wire touch — proves the deny gate returns before the POST.
struct NoWire;
impl OkxTransport for NoWire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        panic!("denied TIF must never reach the wire (signed {path})");
    }
    fn signed_json(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _body: &str,
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        panic!("denied TIF must never reach the wire (signed_json {path})");
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("denied TIF must never reach the wire (public {path})");
    }
}

/// Captures the ARRAY-body batch requests and acks every order by clOrdId — for the mixed-batch
/// partition pin.
struct Capture {
    bodies: Mutex<Vec<String>>,
}
impl OkxTransport for Capture {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        panic!("batch must use signed_json, not signed ({path})");
    }
    fn signed_json(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        body: &str,
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        assert!(path.ends_with("/batch-orders"), "unexpected signed_json path {path}");
        self.bodies.lock().unwrap().push(body.to_string());
        let orders: Vec<Value> = serde_json::from_str(body).expect("array body");
        let acks: Vec<Value> = orders
            .iter()
            .enumerate()
            .map(|(i, o)| {
                json!({
                    "clOrdId": o.get("clOrdId").cloned().unwrap_or_default(),
                    "ordId": format!("oid-{i}"),
                    "sCode": "0",
                    "sMsg": ""
                })
            })
            .collect();
        Ok(json!({ "code": "0", "msg": "", "data": acks }))
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("unexpected public {path}");
    }
}

fn make<T: OkxTransport>(transport: T) -> OkxPerpRest<T> {
    OkxPerpRest {
        signer: signer(),
        transport,
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTC-USDT-SWAP".to_string(),
        properties: SymbolProperties {
            tick_size: 0.1,
            step_size: 0.01,
            min_qty: 0.01,
            ..Default::default()
        },
        ct_val: 0.01,
        leverage: 2.0,
        broker_code: None,
    }
}

// Per venue: `crates/bridges/bybit/tests/offline/tif_gate.rs`'s `limit_req` (and binance's) use
// `BTCUSDT` at `qty` 0.01, not this `BTC-USDT-SWAP` at 0.0002.
fn limit_req(coid: &str, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "okx".to_string(),
        symbol: "BTC-USDT-SWAP".to_string(),
        side: 1,
        qty: 0.0002,
        order_type: vike_model::OrderType::Limit,
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
        assert_matches!(&events[0], Event::OrderSubmitted(s) if s.client_order_id == coid);
        match &events[1] {
            Event::OrderRejected(r) => {
                assert_eq!(r.client_order_id, coid);
                assert!(r.reason.contains("not supported on okx"), "loud deny: {}", r.reason);
            }
            other => panic!("{coid}: expected terminal OrderRejected, got {other:?}"),
        }
    }
}

/// A mixed batch partitions: the denied order is terminally rejected and never enters the
/// ARRAY-body wire chunk; the supported orders still batch (ONE wire call whose array carries
/// only the two supported coids, with their honored ordTypes — the default's `limit` and the
/// requested `fok`).
#[test]
fn batch_partitions_denied_tifs_out_of_the_wire_chunk() {
    let client = make(Capture { bodies: Mutex::new(Vec::new()) });
    let requests = vec![
        limit_req("c-ok-1", TimeInForce::Gtc),
        limit_req("c-bad", TimeInForce::Day),
        limit_req("c-ok-2", TimeInForce::Fok),
    ];
    let events = client.submit_batch(&requests);
    let submitted: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderSubmitted(s) => Some(s.client_order_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(submitted, ["c-ok-1", "c-bad", "c-ok-2"], "every order gets its Submitted");
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

    let bodies = client.transport.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1, "one wire chunk for the two supported orders");
    let batch: Vec<Value> = serde_json::from_str(&bodies[0]).expect("array body");
    let coids: Vec<_> = batch
        .iter()
        .map(|o| o.get("clOrdId").and_then(|v| v.as_str()).unwrap().to_string())
        .collect();
    assert_eq!(coids, ["c-ok-1", "c-ok-2"], "the denied order never reaches the wire");
    let ord_types: Vec<_> = batch
        .iter()
        .map(|o| o.get("ordType").and_then(|v| v.as_str()).unwrap().to_string())
        .collect();
    assert_eq!(ord_types, ["limit", "fok"], "honored TIFs ride the wire as ordTypes");
}

/// POST-ONLY rides OKX's own `ordType=post_only` (still a limit order: the price is sent), and a
/// post-only order that is not a LIMIT order is denied before the wire: the flag would be dropped and
/// the order would take liquidity.
#[test]
fn post_only_rides_the_wire_as_an_ord_type_and_a_market_one_is_denied() {
    let client = make(NoWire);
    let params = client
        .build_order_params(&limit_req("c-po", TimeInForce::PostOnly), None)
        .expect("a legal order");
    let get = |k: &str| params.iter().find(|(key, _)| *key == k).map(|(_, v)| v.clone());
    assert_eq!(get("ordType"), Some(json!("post_only")));
    assert!(get("px").is_some(), "post_only is a limit order: the price is sent");

    let mut market = limit_req("c-po-mkt", TimeInForce::PostOnly);
    market.order_type = vike_model::OrderType::Market;
    market.price = None;
    let events = VenueRest::submit_order(&client, &market);
    assert_eq!(events.len(), 2, "{events:?}");
    assert_matches!(
        &events[1], Event::OrderRejected(r) if r.reason.contains("PostOnly needs a limit order"),
        "{events:?}"
    );
}

/// The venue ACCEPTS a crossing post-only order and then cancels it with its own code
/// (`cancelSource` 31, MEASURED on the demo host): the order-stream row becomes the typed terminal,
/// an `OrderCanceled` (a reject is legal before the accept only). Any other cancel keeps its code.
#[test]
fn a_crossing_post_only_order_the_venue_cancels_is_a_typed_cancel() {
    let row = |source: &str| {
        json!({
            "clOrdId": "c-po", "ordId": "o1", "state": "canceled", "ordType": "post_only",
            "cancelSource": source, "uTime": "5", "accFillSz": "0", "fillSz": "0"
        })
    };
    let mapped = vike_okx::event_mapper::map_okx_order(&row("31"), "okx", "BTC-USDT-SWAP");
    assert_matches!(
        mapped.as_slice(),
        [Event::OrderCanceled(c)] if c.is_post_only_cross() && c.client_order_id == "c-po",
        "{mapped:?}"
    );
    let other = vike_okx::event_mapper::map_okx_order(&row("1"), "okx", "BTC-USDT-SWAP");
    assert_matches!(
        other.as_slice(),
        [Event::OrderCanceled(c)] if !c.is_post_only_cross() && c.reason == "1"
    );
}

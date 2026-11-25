//! A 2xx that names no order is not an answer. A by-order-client-id read with no `orderId` (or an algo read with no
//! `algoId`) read as "absent" rejected an order that may rest; a submit answered with no id was accepted
//! under an empty one. Wrong shape is `Err` (the confirm says nothing), and a 2xx with no id goes to the
//! re-query. Only the venue's own code says absent. The real empty list of this family's reads
//! (`openOrders` -> `[]`) is `crates/bridges/binance/tests/offline/reconcile_client_parse.rs`'s, untouched.

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_binance::execution::rest::perp::VenueRestBinancePerp;
use vike_binance::execution::rest::spot::VenueRestBinanceSpot;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, SymbolProperties};

use crate::time_in_force_gate::NullSigner;

/// `post` answers every POST; a GET answers `order` on the regular endpoint and `algo` on the algo one.
struct Script {
    post: Value,
    order: Result<Value, VenueApiError>,
    algo: Result<Value, VenueApiError>,
    wire: Mutex<Vec<String>>,
}

impl RestTransport for Script {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        self.wire.lock().unwrap().push(format!("{method} {path}"));
        match method {
            "POST" => Ok(self.post.clone()),
            _ if path.ends_with("algoOrder") => self.algo.clone(),
            _ => self.order.clone(),
        }
    }
    vike_bridge_core::no_public_call!();
}

fn unknown() -> Result<Value, VenueApiError> {
    Err(VenueApiError { code: -2013, msg: "Order does not exist.".to_string() })
}

fn found() -> Result<Value, VenueApiError> {
    Ok(json!({"orderId": 777, "status": "NEW"}))
}

fn algo_found() -> Result<Value, VenueApiError> {
    Ok(json!({"algoId": 888, "algoStatus": "NEW"}))
}

fn script(
    post: Value,
    order: Result<Value, VenueApiError>,
    algo: Result<Value, VenueApiError>,
) -> Script {
    Script { post, order, algo, wire: Mutex::new(Vec::new()) }
}

fn perp(transport: Script) -> VenueRestBinancePerp<NullSigner, Script> {
    VenueRestBinancePerp {
        link_id: None,
        signer: NullSigner,
        transport,
        base_url: "https://stub".into(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        algo_order_client_ids: Default::default(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    }
}

fn spot(transport: Script) -> VenueRestBinanceSpot<NullSigner, Script> {
    VenueRestBinanceSpot {
        link_id: None,
        signer: NullSigner,
        transport,
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        base_asset: "BTC".into(),
    }
}

fn order(order_client_id: &str) -> OrderRequest {
    serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.005, "order_type": "limit", "price": 60000.0
    }))
    .unwrap()
}

fn venue_id(event: Option<&Event>) -> Option<String> {
    match event {
        Some(Event::OrderAccepted(a)) => a.venue_order_id.as_ref().map(ToString::to_string),
        other => panic!("expected an accept, got {other:?}"),
    }
}

fn no_id() -> [Value; 3] {
    [json!({}), json!({"orderId": null}), json!({"orderId": "", "status": "NEW"})]
}

#[test]
fn a_submit_answered_with_no_id_is_asked_for() {
    for post in no_id() {
        let c = perp(script(post.clone(), found(), unknown()));
        assert_eq!(venue_id(c.submit_order(&order("c1")).last()).as_deref(), Some("777"), "{post}");
        let c = spot(script(post.clone(), found(), unknown()));
        assert_eq!(venue_id(c.submit_order(&order("c1")).last()).as_deref(), Some("777"), "{post}");
        // and the venue's own "no such order" is the terminal reject
        let c = spot(script(post.clone(), unknown(), unknown()));
        let events = c.submit_order(&order("c1"));
        assert_eq!(events.len(), 1, "only OrderSubmitted, the ladder decides: {post}: {events:?}");
    }
}

/// A stop's ack with no `algoId` is asked for as well, and a look that says nothing leaves it pre-ack.
#[test]
fn a_stop_answered_with_no_algo_id_is_asked_for() {
    let stop = OrderRequest {
        order_type: OrderType::Stop,
        trigger_price: Some(58_000.0),
        reduce_only: true,
        price: None,
        ..order("sl1")
    };
    let c = perp(script(json!({}), unknown(), algo_found()));
    assert_eq!(venue_id(c.submit_order(&stop).last()).as_deref(), Some("888"));
    let c = perp(script(json!({}), unknown(), Ok(json!({}))));
    let events = c.submit_order(&stop);
    assert!(matches!(events[..], [Event::OrderSubmitted(_)]), "{events:?}");
}

/// A by-order-client-id read that names no order says nothing: never the "venue confirms absent" reject.
#[test]
fn a_status_read_that_names_no_order_says_nothing() {
    for body in no_id() {
        let events = spot(script(json!({}), Ok(body.clone()), unknown())).confirm_order("c1", 5);
        assert!(events.is_empty(), "spot {body}: {events:?}");
        let events = perp(script(json!({}), Ok(body.clone()), unknown())).confirm_order("c1", 5);
        assert!(events.is_empty(), "perp {body}: {events:?}");
    }
    let c = spot(script(json!({}), unknown(), unknown()));
    assert!(matches!(c.confirm_order("c1", 5)[..], [Event::OrderRejected(_)]));
    let c = spot(script(json!({}), found(), unknown()));
    assert!(matches!(c.confirm_order("c1", 5)[..], [Event::OrderAccepted(_)]));
}

#[test]
fn an_algo_read_that_names_no_stop_says_nothing() {
    for body in [json!({}), json!({"algoId": null}), json!({"algoId": "", "algoStatus": "NEW"})] {
        let events = perp(script(json!({}), unknown(), Ok(body.clone()))).confirm_order("sl1", 5);
        assert!(events.is_empty(), "{body}: {events:?}");
    }
    let c = perp(script(json!({}), unknown(), algo_found()));
    assert!(matches!(c.confirm_order("sl1", 5)[..], [Event::OrderAccepted(_)]));
}

/// The batch: an element with neither an id nor a code, and an element that is not there, are asked for,
/// never a terminal reject of an order that may rest; a real code stays the refusal it was.
#[test]
fn a_batch_answer_that_says_nothing_about_an_order_is_asked_for() {
    let post = json!([
        {"orderId": 5, "status": "NEW"},
        {},
        {"code": -2010, "msg": "Account has insufficient balance for requested action."}
    ]);
    let c = perp(script(post, found(), unknown()));
    let events = c.submit_batch(&[order("a"), order("b"), order("c"), order("d")]);
    let answers: Vec<String> = events
        .iter()
        .skip(4)
        .map(|e| match e {
            Event::OrderAccepted(a) => {
                format!("{} {}", a.order_client_id, a.venue_order_id.as_deref().unwrap_or("no id"))
            }
            Event::OrderRejected(r) => format!("{} rejected", r.order_client_id),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(answers, ["a 5", "b 777", "c rejected", "d 777"]);
}

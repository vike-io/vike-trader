//! A 2xx that names no order is not an answer, on aster's fork as on binance: a by-order-client-id read with no
//! `orderId` read as "absent" rejected an order that may rest, and a submit answered with no id was
//! accepted under an empty one. Wrong shape is `Err` (the confirm says nothing), and a 2xx with no id goes
//! to the re-query. An order the venue names keeps its path untouched, so nothing that worked starts refusing.

use serde_json::{Value, json};
use vike_aster::execution::rest::perp::VenueRestAsterPerp;
use vike_aster::execution::rest::spot::VenueRestAsterSpot;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties};

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// `post` answers every POST, `get` every GET.
struct Script {
    post: Value,
    get: Result<Value, VenueApiError>,
}

impl RestTransport for Script {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        if method == "POST" { Ok(self.post.clone()) } else { self.get.clone() }
    }
    vike_bridge_core::no_public_call!();
}

fn found() -> Result<Value, VenueApiError> {
    Ok(json!({"orderId": 777, "status": "NEW"}))
}

fn unknown() -> Result<Value, VenueApiError> {
    Err(VenueApiError { code: -2013, msg: "Order does not exist.".to_string() })
}

fn perp(post: Value, get: Result<Value, VenueApiError>) -> VenueRestAsterPerp<NullSigner, Script> {
    VenueRestAsterPerp {
        builder: None,
        signer: NullSigner,
        transport: Script { post, get },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    }
}

fn spot(post: Value, get: Result<Value, VenueApiError>) -> VenueRestAsterSpot<NullSigner, Script> {
    VenueRestAsterSpot {
        builder: None,
        signer: NullSigner,
        transport: Script { post, get },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        base_asset: "BTC".into(),
    }
}

fn order(order_client_id: &str) -> OrderRequest {
    serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "aster", "symbol": "BTCUSDT",
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
        let events = perp(post.clone(), found()).submit_order(&order("c1"));
        assert_eq!(venue_id(events.last()).as_deref(), Some("777"), "{post}");
        let events = spot(post.clone(), found()).submit_order(&order("c1"));
        assert_eq!(venue_id(events.last()).as_deref(), Some("777"), "{post}");
        let events = perp(post.clone(), unknown()).submit_order(&order("c1"));
        assert_eq!(events.len(), 1, "only OrderSubmitted, the ladder decides: {post}: {events:?}");
    }
}

/// An id the venue states keeps the path it always took: nothing that worked starts asking.
#[test]
fn a_submit_answered_with_an_id_takes_it() {
    for post in [json!({"orderId": 5}), json!({"orderId": "5"})] {
        let events = perp(post.clone(), unknown()).submit_order(&order("c1"));
        assert_eq!(venue_id(events.last()).as_deref(), Some("5"), "{post}");
        let events = spot(post.clone(), unknown()).submit_order(&order("c1"));
        assert_eq!(venue_id(events.last()).as_deref(), Some("5"), "{post}");
    }
}

#[test]
fn a_status_read_that_names_no_order_says_nothing() {
    for body in no_id() {
        let events = perp(json!({}), Ok(body.clone())).confirm_order("c1", 5);
        assert!(events.is_empty(), "perp {body}: {events:?}");
        let events = spot(json!({}), Ok(body.clone())).confirm_order("c1", 5);
        assert!(events.is_empty(), "spot {body}: {events:?}");
    }
    assert!(matches!(
        perp(json!({}), unknown()).confirm_order("c1", 5)[..],
        [Event::OrderRejected(_)]
    ));
    assert!(matches!(
        spot(json!({}), found()).confirm_order("c1", 5)[..],
        [Event::OrderAccepted(_)]
    ));
}

/// The batch: an element with neither an id nor a code, and one that is not there, are asked for.
#[test]
fn a_batch_answer_that_says_nothing_about_an_order_is_asked_for() {
    let post = json!([{"orderId": 5}, {}, {"code": -2010, "msg": "no balance"}]);
    let c = perp(post, found());
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

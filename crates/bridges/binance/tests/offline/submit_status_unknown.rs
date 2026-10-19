//! A submit that fails with a venue code whose documented meaning is "execution status UNKNOWN"
//! (`-1007 TIMEOUT`, `-1006 UNEXPECTED_RESP`; the transport delivers them as the body's own code,
//! e.g. on an HTTP 503, so they are NOT `E_TIMEOUT_AMBIGUOUS`) must re-query by client order id and
//! never become a terminal `OrderRejected`: the order may be live. A DEFINITE refusal (`-2010`
//! insufficient balance) stays the terminal reject, with no re-query.

use std::sync::Mutex;

use vike_binance::perp::BinancePerpRest;
use vike_binance::spot::BinanceSpotRest;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::SymbolProperties;
use vike_model::events::Event;

use crate::tif_gate::NullSigner;

/// POST answers `post`; the by-coid `GET .../order` answers "found, live" (venue id 777) and is
/// counted, so a test can tell a re-query from a straight reject.
struct Stub {
    post: Result<serde_json::Value, i64>,
    gets: Mutex<usize>,
}
impl Stub {
    fn new(post: Result<serde_json::Value, i64>) -> Self {
        Self { post, gets: Mutex::new(0) }
    }
}
impl RestTransport for Stub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        if method.eq_ignore_ascii_case("GET") && path.ends_with("/order") {
            *self.gets.lock().unwrap() += 1;
            return Ok(serde_json::json!({"orderId": 777, "status": "NEW"}));
        }
        assert!(method.eq_ignore_ascii_case("POST"), "{method} {path}");
        self.post.clone().map_err(|code| VenueApiError { code, msg: format!("venue code {code}") })
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

fn perp(stub: Stub) -> BinancePerpRest<NullSigner, Stub> {
    BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport: stub,
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    }
}

fn spot(stub: Stub) -> BinanceSpotRest<NullSigner, Stub> {
    BinanceSpotRest {
        link_id: None,
        signer: NullSigner,
        transport: stub,
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        base_asset: "BTC".into(),
    }
}

fn order(coid: &str) -> vike_model::OrderRequest {
    serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.005, "order_type": "limit", "price": 60000.0
    }))
    .unwrap()
}

/// Submitted, then the re-query's verdict: the live order accepted WITH its venue id.
fn assert_requeried_and_accepted(events: &[Event], gets: &Mutex<usize>) {
    assert_eq!(*gets.lock().unwrap(), 1, "the order must be re-queried by client id: {events:?}");
    match events.last() {
        Some(Event::OrderAccepted(a)) => assert_eq!(a.venue_order_id.as_deref(), Some("777")),
        other => panic!("a status-unknown submit must not end terminal: {other:?}"),
    }
    assert!(!events.iter().any(|e| matches!(e, Event::OrderRejected(_))), "{events:?}");
}

#[test]
fn perp_status_unknown_codes_requery_instead_of_rejecting() {
    for code in [-1007, -1006] {
        let c = perp(Stub::new(Err(code)));
        let events = c.submit_order(&order("c1"));
        assert_requeried_and_accepted(&events, &c.transport.gets);
    }
}

#[test]
fn spot_status_unknown_codes_requery_instead_of_rejecting() {
    for code in [-1007, -1006] {
        let c = spot(Stub::new(Err(code)));
        let events = c.submit_order(&order("c1"));
        assert_requeried_and_accepted(&events, &c.transport.gets);
    }
}

#[test]
fn perp_batch_status_unknown_requeries_each_order() {
    for code in [-1007, -1006] {
        let c = perp(Stub::new(Err(code)));
        let events = c.submit_batch(&[order("b0"), order("b1")]);
        assert_eq!(*c.transport.gets.lock().unwrap(), 2, "{events:?}");
        assert!(!events.iter().any(|e| matches!(e, Event::OrderRejected(_))), "{events:?}");
    }
}

/// The same code as ONE element of a 200 batch answer: that order's fate is unknown too.
#[test]
fn perp_batch_element_with_a_status_unknown_code_is_requeried() {
    let answer = serde_json::json!([
        {"orderId": 5, "status": "NEW"},
        {"code": -1007, "msg": "Timeout waiting for response from backend server."},
        {"code": -2010, "msg": "Account has insufficient balance for requested action."}
    ]);
    let c = perp(Stub::new(Ok(answer)));
    let events = c.submit_batch(&[order("b0"), order("b1"), order("b2")]);
    assert_eq!(*c.transport.gets.lock().unwrap(), 1, "only the unknown element: {events:?}");
    let rejected: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.client_order_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(rejected, vec!["b2"], "only the definite refusal is terminal: {events:?}");
}

/// A DEFINITE refusal stays exactly as before: terminal reject, no re-query.
#[test]
fn a_definite_reject_code_still_rejects_without_a_requery() {
    for code in [-2010, -1013, -1021, -1111] {
        let p = perp(Stub::new(Err(code)));
        let events = p.submit_order(&order("c1"));
        assert!(matches!(events.last(), Some(Event::OrderRejected(_))), "{code}: {events:?}");
        assert_eq!(*p.transport.gets.lock().unwrap(), 0, "{code}");

        let s = spot(Stub::new(Err(code)));
        let events = s.submit_order(&order("c1"));
        assert!(matches!(events.last(), Some(Event::OrderRejected(_))), "{code}: {events:?}");
        assert_eq!(*s.transport.gets.lock().unwrap(), 0, "{code}");
    }
}

/// The perp MODIFY (`PUT /fapi/v1/order`) fails with `put`; nothing else may be touched.
struct PutStub {
    put: i64,
}
impl RestTransport for PutStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        assert!(method.eq_ignore_ascii_case("PUT"), "{method} {path}");
        Err(VenueApiError { code: self.put, msg: format!("venue code {}", self.put) })
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

fn modify_reason(code: i64) -> String {
    let c = BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport: PutStub { put: code },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    };
    match &c.modify_order(&order("c1"), None, Some(61_000.0))[..] {
        [Event::OrderModifyRejected(r)] => r.reason.to_string(),
        other => panic!("{code}: expected one OrderModifyRejected, got {other:?}"),
    }
}

/// A modify whose venue code says "status UNKNOWN" must not claim the order kept its terms: it is
/// worded exactly like the timeout sentinel's advisory. A DEFINITE refusal keeps the venue's words.
#[test]
fn perp_modify_status_unknown_codes_say_the_outcome_is_unknown() {
    for code in [-1007, -1006] {
        let reason = modify_reason(code);
        assert!(reason.contains("modify outcome unknown"), "{code}: {reason}");
        assert!(reason.contains(&format!("venue code {code}")), "{code} stays visible: {reason}");
    }
}

#[test]
fn perp_modify_definite_refusals_stay_plain_denials() {
    for code in [-2013, -1013, -1111] {
        let reason = modify_reason(code);
        assert!(!reason.contains("outcome unknown"), "{code}: {reason}");
    }
}

/// The POST fails with `-1007` AND the by-coid lookup fails too: nothing is known about the order, so
/// nothing is claimed. No id-less `OrderAccepted` (a phantom if the order never landed) and no
/// terminal reject (a stranded position if it did): the stuck-order watchdog's active `confirm`
/// (`COID_LOOKUP` is true for this venue) and the user-data stream decide.
struct DeadLookup;
impl RestTransport for DeadLookup {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        let code = if method.eq_ignore_ascii_case("GET") { -1003 } else { -1007 };
        Err(VenueApiError { code, msg: format!("venue code {code}") })
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

#[test]
fn a_status_unknown_submit_whose_lookup_also_fails_claims_nothing() {
    let p = BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport: DeadLookup,
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    };
    let s = BinanceSpotRest {
        link_id: None,
        signer: NullSigner,
        transport: DeadLookup,
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        base_asset: "BTC".into(),
    };
    for events in [p.submit_order(&order("c1")), s.submit_order(&order("c1"))] {
        assert!(
            !events.iter().any(|e| matches!(e, Event::OrderAccepted(_) | Event::OrderRejected(_))),
            "{events:?}"
        );
    }
}

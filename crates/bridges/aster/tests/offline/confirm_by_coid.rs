//! The active order-status confirm (`VenueRest::confirm_order`, the body behind
//! `ExecActor::with_confirm`), offline: a stub transport answers the by-coid `GET .../order`.
//! found -> `OrderAccepted` with the venue id (read by STATUS: a CANCELED order is not a live one);
//! -2013 -> the "venue confirms absent" `OrderRejected`, only once the exec thread finished
//! dispatching the coid; a failed query -> NOTHING (never an id-less accept, never a terminal). Aster stamps no
//! coid prefix (even with a builder pair configured), so the bare id is the `origClientOrderId`.
//! NO socket: the transport is a stub, and the only order-capable aster keys are LIVE (mainnet) ones.

use std::sync::{Arc, Mutex};

use vike_aster::perp::AsterPerpRest;
use vike_aster::spot::AsterSpotRest;
use vike_binance::family::confirm::{ConfirmByCoid, DispatchLedger, confirm_closure};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, RestTransport, VenueApiError};
use vike_model::SymbolProperties;
use vike_model::events::Event;

use vike_bridge_core::signer::PreparedRequest;

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// Answers only `GET .../order`, recording `(path, origClientOrderId)`; anything else panics.
struct LookupStub {
    answer: Result<serde_json::Value, i64>,
    seen: Mutex<Vec<(String, String)>>,
}
impl LookupStub {
    fn new(answer: Result<serde_json::Value, i64>) -> Self {
        Self { answer, seen: Mutex::new(Vec::new()) }
    }
}
impl RestTransport for LookupStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        assert!(method.eq_ignore_ascii_case("GET") && path.ends_with("/order"), "{method} {path}");
        let orig = params.iter().find(|(k, _)| *k == "origClientOrderId").unwrap().1.clone();
        self.seen.lock().unwrap().push((path.to_string(), orig));
        self.answer.clone().map_err(|code| VenueApiError { code, msg: "stub".into() })
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

fn perp(
    stub: LookupStub,
    builder: Option<(String, String)>,
) -> AsterPerpRest<NullSigner, LookupStub> {
    AsterPerpRest {
        builder,
        signer: NullSigner,
        transport: stub,
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    }
}

fn spot(stub: LookupStub) -> AsterSpotRest<NullSigner, LookupStub> {
    AsterSpotRest {
        builder: None,
        signer: NullSigner,
        transport: stub,
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        base_asset: "BTC".into(),
    }
}

fn found() -> LookupStub {
    LookupStub::new(Ok(serde_json::json!({"orderId": 777, "status": "NEW"})))
}

fn only(events: Vec<Event>) -> Event {
    assert_eq!(events.len(), 1, "{events:?}");
    events.into_iter().next().unwrap()
}

#[test]
fn perp_confirm_found_is_accepted_with_the_venue_id_and_hits_fapi_order() {
    let c = perp(found(), None);
    match only(c.confirm_order("c1", 5)) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.client_order_id, "c1");
            assert_eq!(a.venue_order_id.as_deref(), Some("777"));
            assert_eq!(a.ts, 5);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        *c.transport.seen.lock().unwrap(),
        vec![("/fapi/v3/order".to_string(), "c1".to_string())]
    );
}

#[test]
fn perp_confirm_absent_is_the_venue_confirms_absent_reject() {
    let c = perp(LookupStub::new(Err(-2013)), None);
    match only(c.confirm_order("c2", 5)) {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "c2");
            assert!(r.reason.to_string().contains("venue confirms order absent"));
        }
        other => panic!("{other:?}"),
    }
}

/// A FAILED query says nothing about the order: no id-less accept, no terminal.
#[test]
fn perp_confirm_failed_query_emits_nothing() {
    for code in [E_TIMEOUT_AMBIGUOUS, -2015, -1022, 418, 429, 503] {
        let c = perp(LookupStub::new(Err(code)), None);
        assert!(c.confirm_order("c3", 5).is_empty(), "code {code}");
    }
}

/// Aster sends the bare coid even when a builder pair is configured.
#[test]
fn perp_confirm_sends_the_bare_coid() {
    let c = perp(found(), Some(("0xbuilder".into(), "0.001".into())));
    only(c.confirm_order("c4", 5));
    assert_eq!(c.transport.seen.lock().unwrap()[0].1, "c4");
}

#[test]
fn spot_confirm_hits_api_order_found_and_absent() {
    let c = spot(found());
    assert!(matches!(only(c.confirm_order("s1", 5)), Event::OrderAccepted(_)));
    assert_eq!(c.transport.seen.lock().unwrap()[0].0, "/api/v3/order");
    let c = spot(LookupStub::new(Err(-2013)));
    assert!(matches!(only(c.confirm_order("s2", 5)), Event::OrderRejected(_)));
}

/// A found order is read by STATUS (a CANCELED order is `Accepted` then `Canceled`, a REJECTED one
/// only `Rejected`), and `-2013` is only trusted once the coid's submit was dispatched.
#[test]
fn spot_confirm_reads_status_and_trusts_absent_only_after_dispatch() {
    let status = |w: &str| LookupStub::new(Ok(serde_json::json!({"orderId": 9, "status": w})));
    let kinds = |w: &str| -> Vec<bool> {
        spot(status(w))
            .confirm_order("s3", 5)
            .iter()
            .map(|e| matches!(e, Event::OrderAccepted(_)))
            .collect()
    };
    assert_eq!(kinds("NEW"), [true]);
    assert_eq!(kinds("CANCELED"), [true, false]);
    assert_eq!(kinds("REJECTED"), [false]);

    let ledger = Arc::new(DispatchLedger::default());
    let confirm = confirm_closure(spot(LookupStub::new(Err(-2013))), ledger.clone(), |_| true);
    assert!(confirm("q1").is_empty(), "still queued: absent proves nothing");
    ledger.mark("q2");
    assert!(matches!(confirm("q2")[..], [Event::OrderRejected(_)]));
    assert!(spot(LookupStub::new(Err(-2013))).confirm_when("q3", 5, false).is_empty());
}

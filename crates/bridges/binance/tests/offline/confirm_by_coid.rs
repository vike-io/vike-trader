//! The active order-status confirm (`VenueRest::confirm_order`, the body behind
//! `ExecActor::with_confirm`), offline: a stub transport answers the by-coid `GET .../order`.
//! found -> `OrderAccepted` with the venue id, read by STATUS (a CANCELED / EXPIRED / REJECTED order is
//! not a live one); -2013 -> the "venue confirms absent" `OrderRejected`, but only once the exec thread
//! finished dispatching the coid; a failed query -> NOTHING (never an id-less accept, never a
//! terminal). The lookup key is `origClientOrderId` AS THE VENUE STORED IT, so a configured broker
//! link id must prefix it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vike_binance::family::confirm::{found_events, resolve_submit};
use vike_binance::family::rest_reads::FoundOrder;
use vike_binance::perp::BinancePerpRest;
use vike_binance::spot::BinanceSpotRest;
use vike_bridge_core::confirm::{ConfirmByCoid, DispatchLedger, confirm_closure};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, RestTransport, VenueApiError};
use vike_model::SymbolProperties;
use vike_model::events::Event;

use crate::tif_gate::NullSigner;

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

fn perp(stub: LookupStub, link_id: Option<&str>) -> BinancePerpRest<NullSigner, LookupStub> {
    BinancePerpRest {
        link_id: link_id.map(str::to_string),
        signer: NullSigner,
        transport: stub,
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    }
}

fn spot(stub: LookupStub) -> BinanceSpotRest<NullSigner, LookupStub> {
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
        vec![("/fapi/v1/order".to_string(), "c1".to_string())]
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

/// A FAILED query says nothing about the order: no id-less accept, no terminal (auth -2015,
/// signature -1022, a ban, a 5xx, a spot -1021 and a timeout alike) - the watchdog ladder and
/// reconcile decide.
#[test]
fn perp_confirm_failed_query_emits_nothing() {
    for code in [E_TIMEOUT_AMBIGUOUS, -2015, -1022, -1021, 418, 429, 503] {
        let c = perp(LookupStub::new(Err(code)), None);
        assert!(c.confirm_order("c3", 5).is_empty(), "code {code}");
        assert!(c.confirm_when("c3", 5, false).is_empty(), "code {code}, undispatched");
    }
}

/// The venue stored the PREFIXED coid, so the lookup must send it prefixed.
#[test]
fn perp_confirm_sends_the_broker_prefixed_coid() {
    let c = perp(found(), Some("LINK"));
    only(c.confirm_order("c4", 5));
    let seen = c.transport.seen.lock().unwrap();
    assert_eq!(seen[0].1, "x-LINK-c4");
}

#[test]
fn spot_confirm_hits_api_order_found_and_absent() {
    let c = spot(found());
    assert!(matches!(only(c.confirm_order("s1", 5)), Event::OrderAccepted(_)));
    assert_eq!(c.transport.seen.lock().unwrap()[0].0, "/api/v3/order");
    let c = spot(LookupStub::new(Err(-2013)));
    assert!(matches!(only(c.confirm_order("s2", 5)), Event::OrderRejected(_)));
}

fn status(word: &str) -> LookupStub {
    LookupStub::new(Ok(serde_json::json!({"orderId": 9, "status": word})))
}

/// A found order is read by STATUS: live words are one `OrderAccepted`; a closed one carries its
/// terminal after the accept; REJECTED is the terminal alone (it never rested).
#[test]
fn a_found_order_is_read_by_status_not_by_existing() {
    let kinds = |word: &str| -> Vec<&'static str> {
        perp(status(word), None)
            .confirm_order("s", 5)
            .iter()
            .map(|e| match e {
                Event::OrderAccepted(_) => "accepted",
                Event::OrderCanceled(_) => "canceled",
                Event::OrderExpired(_) => "expired",
                Event::OrderRejected(_) => "rejected",
                other => panic!("{other:?}"),
            })
            .collect()
    };
    for live in ["NEW", "PARTIALLY_FILLED", "FILLED", "PENDING_CANCEL", ""] {
        assert_eq!(kinds(live), ["accepted"], "{live}");
    }
    assert_eq!(kinds("CANCELED"), ["accepted", "canceled"]);
    assert_eq!(kinds("EXPIRED"), ["accepted", "expired"]);
    assert_eq!(kinds("EXPIRED_IN_MATCH"), ["accepted", "expired"]);
    assert_eq!(kinds("REJECTED"), ["rejected"]);
}

/// The ambiguous SUBMIT path reads the status too (its old "any status is live" turned a CANCELED
/// order into an `OrderAccepted`), and keeps its verdict for absent; a failed query says nothing.
#[test]
fn the_ambiguous_submit_path_reads_status_and_keeps_its_other_verdicts() {
    let found = |word: &str| FoundOrder { order_id: "9".into(), status: word.into() };
    let n = |q| resolve_submit("c", 5, q).len();
    assert_eq!(n(Ok(Some(found("CANCELED")))), 2);
    assert_eq!(n(Ok(Some(found("NEW")))), 1);
    assert!(matches!(resolve_submit("c", 5, Ok(None))[0], Event::OrderRejected(_)));
    // a failed lookup claims nothing, like the active confirm: an id-less accept would be a phantom
    // the watchdog never sweeps when the order never landed (the watchdog's confirm decides)
    let failed = Err(VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: "x".into() });
    assert!(resolve_submit("c", 5, failed).is_empty());
    assert_eq!(found_events("c", 5, &found("FILLED")).len(), 1);
}

/// `-2013` is only trusted once the exec thread FINISHED dispatching the coid: a submit still queued
/// behind a slow cancel batch is not on the venue yet. A repeat of the same coid inside the backoff
/// is skipped; a found order needs no ledger.
#[test]
fn the_confirm_closure_trusts_absent_only_after_dispatch_and_backs_off() {
    let ledger = Arc::new(DispatchLedger::default());
    let absent = confirm_closure(perp(LookupStub::new(Err(-2013)), None), ledger.clone(), |_| true);
    assert!(absent("q1").is_empty(), "still queued: absent proves nothing");
    ledger.mark("q2");
    assert!(matches!(absent("q2")[..], [Event::OrderRejected(_)]), "dispatched: absent is real");
    assert!(absent("q2").is_empty(), "the same coid inside the backoff is skipped");

    let live = confirm_closure(perp(found(), None), Arc::new(DispatchLedger::default()), |_| true);
    assert!(matches!(live("q3")[..], [Event::OrderAccepted(_)]), "found needs no ledger");
}

/// ONE client serves every confirm, and `prepare` (the spot server-time read) runs until it succeeds,
/// then never again.
#[test]
fn the_confirm_closure_prepares_its_client_until_it_succeeds_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let confirm = confirm_closure(spot(found()), Arc::new(DispatchLedger::default()), move |_| {
        seen.fetch_add(1, Ordering::SeqCst) >= 1 // fails the first time, succeeds the second
    });
    for coid in ["a", "b", "c", "d"] {
        assert_eq!(confirm(coid).len(), 1, "{coid}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

//! The active order-status confirm (`VenueRest::confirm_order`, the body behind
//! `ExecActor::with_confirm`), offline: a stub transport answers the by-`clOrdId` GET on
//! `/trade/order`. The shared form of `vike_bridge_core::confirm`: found ->
//! `OrderAccepted` with the venue id; `51603` "does not exist" -> the "venue confirms absent"
//! `OrderRejected`, but only for a coid the exec thread finished dispatching; a FAILED query ->
//! NOTHING (never an id-less accept, never a terminal).

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_bridge_core::confirm_pins;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_model::events::Event;
use vike_okx::perp::OkxPerpRest;
use vike_okx::transport::OkxTransport;

use crate::r6_okx_parity::{fixture, okx_perp};

/// What the by-clOrdId GET answers.
enum Answer {
    Live,
    /// The venue's `51603` "Order does not exist".
    Absent,
    /// The transport itself failed (timeout sentinel, a 5xx).
    TransportErr(i64),
    /// HTTP 200 with a venue top-level `code` (a bad key, a rate limit, an endpoint timeout).
    Envelope(i64),
}

/// Answers only `GET .../trade/order`, recording the `clOrdId`; anything else panics.
struct LookupStub {
    answer: Answer,
    seen: Mutex<Vec<String>>,
}

impl LookupStub {
    fn new(answer: Answer) -> Self {
        Self { answer, seen: Mutex::new(Vec::new()) }
    }
}

impl OkxTransport for LookupStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        // "absent" needs both endpoints: the algo half (`algo_stop.rs` pins it) is absent here too
        if path.ends_with("/trade/order-algo") {
            return Ok(json!({"code": "51603", "msg": "Order does not exist", "data": []}));
        }
        assert!(
            method.eq_ignore_ascii_case("GET") && path.ends_with("/trade/order"),
            "{method} {path}"
        );
        let coid = params.iter().find(|(k, _)| *k == "clOrdId").unwrap().1.clone();
        self.seen.lock().unwrap().push(coid.as_str().unwrap().to_string());
        match &self.answer {
            Answer::Live => Ok(json!({"code": "0", "msg": "", "data": [{"ordId": "live-1"}]})),
            Answer::Absent => {
                Ok(json!({"code": "51603", "msg": "Order does not exist", "data": []}))
            }
            Answer::TransportErr(code) => Err(VenueApiError { code: *code, msg: "stub".into() }),
            Answer::Envelope(code) => {
                Ok(json!({"code": code.to_string(), "msg": "venue words", "data": []}))
            }
        }
    }
    fn signed_json(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _body: &str,
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        panic!("unexpected signed_json {path}")
    }
    vike_bridge_core::no_public_call!();
}

fn client(answer: Answer) -> OkxPerpRest<LookupStub> {
    okx_perp(&fixture(), LookupStub::new(answer))
}

/// Every way a query fails: none of them says anything about the order.
fn failures() -> Vec<Answer> {
    vec![
        Answer::TransportErr(E_TIMEOUT_AMBIGUOUS),
        Answer::TransportErr(503),
        Answer::Envelope(50111),
        Answer::Envelope(50011),
        Answer::Envelope(50004),
    ]
}

#[test]
fn confirm_found_is_accepted_with_the_venue_id() {
    let c = client(Answer::Live);
    match &c.confirm_order("c1", 5)[..] {
        [Event::OrderAccepted(a)] => {
            assert_eq!(a.client_order_id, "c1");
            assert_eq!(a.venue_order_id.as_deref(), Some("live-1"));
            assert_eq!(a.ts, 5);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(*c.transport.seen.lock().unwrap(), vec!["c1".to_string()]);
}

/// `confirm_order` is the trait door that assumes the submit was dispatched: the venue's `51603` is
/// the "venue confirms absent" reject.
#[test]
fn confirm_absent_is_the_venue_confirms_absent_reject() {
    let c = client(Answer::Absent);
    match &c.confirm_order("c2", 5)[..] {
        [Event::OrderRejected(r)] => {
            assert_eq!(r.client_order_id, "c2");
            assert!(r.reason.to_string().contains("venue confirms order absent"));
        }
        other => panic!("{other:?}"),
    }
}

/// A FAILED query says nothing about the order: no id-less accept (the old weak form's answer for a
/// bad key, a ban, a 5xx or a timeout), no terminal. The watchdog ladder and reconcile decide.
#[test]
fn confirm_failed_query_emits_nothing() {
    confirm_pins::assert_failed_queries_emit_nothing(failures().into_iter().map(client));
}

/// "Absent" is only trusted once the exec thread FINISHED dispatching the coid: a submit still queued
/// behind a slow cancel batch is not on the venue yet, so the lookup finding nothing proves nothing.
/// (The old weak form rejected it at once and the order then filled with no engine order to own it.)
#[test]
fn confirm_when_absent_for_an_undispatched_coid_is_silence_and_found_needs_no_ledger() {
    confirm_pins::assert_absent_rejects_only_a_dispatched_coid(
        client(Answer::Absent),
        client(Answer::Live),
    );
}

/// The closure the exec client hands to `ExecActor::with_confirm`: it reads the dispatch ledger BEFORE
/// the query, so absent rejects a dispatched coid only, a failed query is silence even for a
/// dispatched one, and a repeat of the same coid inside the backoff is skipped.
#[test]
fn the_confirm_closure_rejects_absent_only_after_dispatch_and_stays_silent_on_a_failed_query() {
    confirm_pins::assert_the_closure_rejects_absent_only_after_dispatch(
        client(Answer::Absent),
        client(Answer::Live),
        failures().into_iter().map(client),
    );
}

/// `run` (the exec thread) is private and starts with network reads, so the wiring is pinned at the
/// source: the command loop's client must be wrapped in `Dispatched`. Lose it and the ledger is never
/// stamped, so every "absent" answer goes silent and the reject path is quietly disabled.
#[test]
fn the_command_loop_client_is_wrapped_in_dispatched() {
    confirm_pins::assert_the_command_loop_client_is_wrapped_in_dispatched(env!(
        "CARGO_MANIFEST_DIR"
    ));
}

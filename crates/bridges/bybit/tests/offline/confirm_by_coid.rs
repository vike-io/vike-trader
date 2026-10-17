//! The active order-status confirm (`VenueRest::confirm_order`, the body behind
//! `ExecActor::with_confirm`), offline: a stub transport answers the by-orderLinkId GETs
//! (`/v5/order/realtime`, then `/v5/order/history`). The shared form of
//! `vike_bridge_core::confirm`: found -> `OrderAccepted` with the venue id; absent
//! on BOTH endpoints -> the "venue confirms absent" `OrderRejected`, but only for a coid the exec
//! thread finished dispatching; a FAILED query -> NOTHING (never an id-less accept, never a terminal).

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::confirm::{ConfirmByCoid, DispatchLedger, confirm_closure};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_bybit::perp::BybitPerpRest;
use vike_bybit::transport::BybitTransport;
use vike_model::events::Event;

use crate::r6_bybit_parity::{bybit_perp, fixture};

/// What every GET answers.
enum Answer {
    /// `result.list` of `/order/realtime` and of `/order/history`.
    Rows { realtime: Value, history: Value },
    /// The transport itself failed (timeout sentinel, a 5xx).
    TransportErr(i64),
    /// HTTP 200 with a venue `retCode` (a bad key, a rate limit).
    Envelope(i64),
}

/// Answers only the two by-orderLinkId GETs, recording `(path, orderLinkId)`; anything else panics.
struct LookupStub {
    answer: Answer,
    seen: Mutex<Vec<(String, String)>>,
}

impl LookupStub {
    fn new(answer: Answer) -> Self {
        Self { answer, seen: Mutex::new(Vec::new()) }
    }
}

impl BybitTransport for LookupStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        assert!(
            method.eq_ignore_ascii_case("GET")
                && (path.ends_with("/order/realtime") || path.ends_with("/order/history")),
            "{method} {path}"
        );
        let link = params.iter().find(|(k, _)| *k == "orderLinkId").unwrap().1.clone();
        self.seen.lock().unwrap().push((path.to_string(), link.as_str().unwrap().to_string()));
        match &self.answer {
            Answer::Rows { realtime, history } => {
                let list = if path.ends_with("/order/realtime") { realtime } else { history };
                Ok(json!({"retCode": 0, "retMsg": "OK", "result": {"list": list}}))
            }
            Answer::TransportErr(code) => Err(VenueApiError { code: *code, msg: "stub".into() }),
            Answer::Envelope(code) => {
                Ok(json!({"retCode": code, "retMsg": "venue words", "result": {}}))
            }
        }
    }
}

fn client(answer: Answer) -> BybitPerpRest<LookupStub> {
    bybit_perp(&fixture(), LookupStub::new(answer))
}

fn live() -> Answer {
    Answer::Rows { realtime: json!([{"orderId": "live-1"}]), history: json!([]) }
}

fn absent() -> Answer {
    Answer::Rows { realtime: json!([]), history: json!([]) }
}

/// Every way a query fails: none of them says anything about the order.
fn failures() -> Vec<Answer> {
    vec![
        Answer::TransportErr(E_TIMEOUT_AMBIGUOUS),
        Answer::TransportErr(503),
        Answer::Envelope(10003),
        Answer::Envelope(10006),
        Answer::Envelope(10016),
    ]
}

#[test]
fn confirm_found_is_accepted_with_the_venue_id() {
    let c = client(live());
    match &c.confirm_order("c1", 5)[..] {
        [Event::OrderAccepted(a)] => {
            assert_eq!(a.client_order_id, "c1");
            assert_eq!(a.venue_order_id.as_deref(), Some("live-1"));
            assert_eq!(a.ts, 5);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(c.transport.seen.lock().unwrap().len(), 1, "realtime answered; history untouched");
}

/// A filled-and-closed order drops off `/order/realtime`: history must be asked before "absent".
#[test]
fn confirm_found_only_in_history_is_still_accepted() {
    let c = client(Answer::Rows { realtime: json!([]), history: json!([{"orderId": "old-1"}]) });
    assert!(
        matches!(&c.confirm_order("c2", 5)[..], [Event::OrderAccepted(a)] if a.venue_order_id.as_deref() == Some("old-1"))
    );
    let paths: Vec<_> = c.transport.seen.lock().unwrap().iter().map(|(p, _)| p.clone()).collect();
    assert!(paths[0].ends_with("/order/realtime") && paths[1].ends_with("/order/history"));
}

/// `confirm_order` is the trait door that assumes the submit was dispatched: absent on both endpoints
/// is the "venue confirms absent" reject.
#[test]
fn confirm_absent_on_both_endpoints_is_the_venue_confirms_absent_reject() {
    let c = client(absent());
    match &c.confirm_order("c3", 5)[..] {
        [Event::OrderRejected(r)] => {
            assert_eq!(r.client_order_id, "c3");
            assert!(r.reason.to_string().contains("venue confirms order absent"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(c.transport.seen.lock().unwrap().len(), 2, "realtime then history");
}

/// A FAILED query says nothing about the order: no id-less accept (the old weak form's answer for a
/// bad key, a ban, a 5xx or a timeout), no terminal. The watchdog ladder and reconcile decide.
#[test]
fn confirm_failed_query_emits_nothing() {
    for answer in failures() {
        let c = client(answer);
        assert!(c.confirm_order("c4", 5).is_empty());
    }
}

/// "Absent" is only trusted once the exec thread FINISHED dispatching the coid: a submit still queued
/// behind a slow cancel batch is not on the venue yet, so the lookup finding nothing proves nothing.
/// (The old weak form rejected it at once and the order then filled with no engine order to own it.)
#[test]
fn confirm_when_absent_for_an_undispatched_coid_is_silence_and_found_needs_no_ledger() {
    let c = client(absent());
    assert!(c.confirm_when("q1", 5, false).is_empty());
    assert!(matches!(c.confirm_when("q1", 5, true)[..], [Event::OrderRejected(_)]));
    let c = client(live());
    assert!(matches!(c.confirm_when("q2", 5, false)[..], [Event::OrderAccepted(_)]));
}

/// The closure the exec client hands to `ExecActor::with_confirm`: it reads the dispatch ledger BEFORE
/// the query, so absent rejects a dispatched coid only, a failed query is silence even for a
/// dispatched one, and a repeat of the same coid inside the backoff is skipped.
#[test]
fn the_confirm_closure_rejects_absent_only_after_dispatch_and_stays_silent_on_a_failed_query() {
    let ledger = Arc::new(DispatchLedger::default());
    let absent = confirm_closure(client(absent()), ledger.clone(), |_| true);
    assert!(absent("q1").is_empty(), "still queued: absent proves nothing");
    ledger.mark("q2");
    assert!(matches!(absent("q2")[..], [Event::OrderRejected(_)]), "dispatched: absent is real");
    assert!(absent("q2").is_empty(), "the same coid inside the backoff is skipped");

    for (i, answer) in failures().into_iter().enumerate() {
        let coid = format!("f{i}");
        ledger.mark(&coid);
        let failing = confirm_closure(client(answer), ledger.clone(), |_| true);
        assert!(failing(&coid).is_empty(), "a failed query is silence even once dispatched");
    }

    let found = confirm_closure(client(live()), Arc::new(DispatchLedger::default()), |_| true);
    assert!(matches!(found("q3")[..], [Event::OrderAccepted(_)]), "found needs no ledger");
}

/// `run` (the exec thread) is private and starts with network reads, so the wiring is pinned at the
/// source: the command loop's client must be wrapped in `Dispatched`. Lose it and the ledger is never
/// stamped, so every "absent" answer goes silent and the reject path is quietly disabled.
#[test]
fn the_command_loop_client_is_wrapped_in_dispatched() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/exec.rs");
    let src = std::fs::read_to_string(&path).expect("the exec source is readable");
    let wrapped = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .any(|l| l.contains("&Dispatched { rest, ledger }"));
    assert!(wrapped, "{} no longer wraps run_loop's client in Dispatched", path.display());
}

//! The active order-status confirm (`VenueRest::confirm_order`, the body behind
//! `ExecutionClientActor::with_confirm`), offline: a stub transport answers the by-orderLinkId GETs
//! (`/v5/order/realtime`, then `/v5/order/history`). The shared form of
//! `vike_bridge_core::confirm`: found is read by its STATE (decision 0171 item 2); absent
//! on BOTH endpoints -> the "venue confirms absent" `OrderRejected`, but only for an order client id the exec
//! thread finished dispatching; a FAILED query -> NOTHING (never an id-less accept, never a terminal).

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_bridge_core::confirm_pins;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_bybit::execution::rest::VenueRestBybit;
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

impl RestTransportJson for LookupStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
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

fn client(answer: Answer) -> VenueRestBybit<LookupStub> {
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
            assert_eq!(a.order_client_id, "c1");
            assert_eq!(a.venue_order_id.as_deref(), Some("live-1"));
            assert_eq!(a.timestamp, 5);
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

/// One `/v5/order/realtime` row, the fields the mapper reads in the shape recorded on the demo host
/// (`tests/fixtures/classes/order_realtime_dated_far_stop.json`).
fn row(status: &str, reject: &str) -> Value {
    json!({"orderId": "v-1", "orderLinkId": "c9", "orderStatus": status, "cancelType": "CancelByUser",
           "rejectReason": reject, "updatedTime": "1793390091915"})
}

/// A found order's events, each named `kind [id or reason]`, with the order client id and the accept's `ts` checked.
fn answer_to(found: Value) -> Vec<String> {
    let c = client(Answer::Rows { realtime: json!([found]), history: json!([]) });
    c.confirm_order("c9", 5)
        .iter()
        .map(|e| match e {
            Event::OrderAccepted(a) => {
                assert_eq!((a.order_client_id.as_str(), a.timestamp), ("c9", 5));
                format!("accepted {}", a.venue_order_id.as_deref().unwrap_or("no id"))
            }
            Event::OrderCanceled(x) => {
                assert_eq!(x.order_client_id, "c9");
                format!("canceled {}", x.reason)
            }
            Event::OrderRejected(x) => {
                assert_eq!(x.order_client_id, "c9");
                format!("rejected {}", x.reason)
            }
            Event::OrderExpired(x) => {
                assert_eq!(x.order_client_id, "c9");
                "expired".to_string()
            }
            other => panic!("{other:?}"),
        })
        .collect()
}

/// Decision 0171 item 2, the cells that stay: live, partly filled and filled are the accept alone (a
/// fill rides the stream), as is a word the mapper does not know.
#[test]
fn confirm_found_open_or_filled_is_the_accept_alone() {
    for status in
        ["New", "PartiallyFilled", "Filled", "Untriggered", "Triggered", "Created", "Later"]
    {
        assert_eq!(answer_to(row(status, "EC_NoError")), ["accepted v-1"], "{status}");
    }
}

/// Decision 0171 item 2, the closed cells: the accept, then the terminal, by the stream's own mapper.
#[test]
fn confirm_found_closed_is_the_accept_then_its_terminal() {
    // `Deactivated` is a conditional the venue cancelled before it fired: the stream's cancel, not an expiry
    for status in ["Cancelled", "PartiallyFilledCanceled", "Deactivated"] {
        assert_eq!(
            answer_to(row(status, "EC_NoError")),
            ["accepted v-1", "canceled CancelByUser"],
            "{status}"
        );
    }
    // the venue's post-only word is a typed cancel whichever status it came under, as on the stream
    for status in ["Cancelled", "Rejected"] {
        assert_eq!(
            answer_to(row(status, "EC_PostOnlyWillTakeLiquidity")),
            [
                "accepted v-1".to_string(),
                format!("canceled {}", vike_model::events::POST_ONLY_CANCEL_REASON)
            ],
            "{status}"
        );
    }
}

/// Decision 0171 item 2, the refused cell: the terminal alone (the FSM takes a reject only before an accept).
#[test]
fn confirm_found_rejected_is_the_reject_alone() {
    assert_eq!(answer_to(row("Rejected", "EC_InvalidQty")), ["rejected EC_InvalidQty"]);
}

/// An order closed past `/order/realtime`'s retention is read off `/order/history` the same way.
#[test]
fn confirm_found_closed_only_in_history_carries_its_terminal() {
    let c = client(Answer::Rows {
        realtime: json!([]),
        history: json!([row("Cancelled", "EC_NoError")]),
    });
    assert!(
        matches!(&c.confirm_order("c9", 5)[..], [Event::OrderAccepted(_), Event::OrderCanceled(_)]),
        "{:?}",
        c.confirm_order("c9", 5)
    );
}

/// The read asked by this order client id, so its events are filed under it whatever the row spells.
#[test]
fn confirm_files_a_closed_row_under_the_order_client_id_it_asked_about() {
    let mut nameless = row("Cancelled", "EC_NoError");
    nameless.as_object_mut().unwrap().remove("orderLinkId");
    assert_eq!(answer_to(nameless), ["accepted v-1", "canceled CancelByUser"]);
}

/// `confirm_order` is the trait door that assumes the submit was dispatched: absent on both endpoints
/// is the "venue confirms absent" reject.
#[test]
fn confirm_absent_on_both_endpoints_is_the_venue_confirms_absent_reject() {
    let c = client(absent());
    match &c.confirm_order("c3", 5)[..] {
        [Event::OrderRejected(r)] => {
            assert_eq!(r.order_client_id, "c3");
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
    confirm_pins::assert_failed_queries_emit_nothing(failures().into_iter().map(client));
}

/// "Absent" is only trusted once the exec thread FINISHED dispatching the order client id: a submit still queued
/// behind a slow cancel batch is not on the venue yet, so the lookup finding nothing proves nothing.
/// (The old weak form rejected it at once and the order then filled with no engine order to own it.)
#[test]
fn confirm_when_absent_for_an_undispatched_order_client_id_is_silence_and_found_needs_no_ledger() {
    confirm_pins::assert_absent_rejects_only_a_dispatched_order_client_id(
        client(absent()),
        client(live()),
    );
}

/// The closure the exec client hands to `ExecutionClientActor::with_confirm`: it reads the dispatch ledger BEFORE
/// the query, so absent rejects a dispatched order client id only, a failed query is silence even for a
/// dispatched one, and a repeat of the same order client id inside the backoff is skipped.
#[test]
fn the_confirm_closure_rejects_absent_only_after_dispatch_and_stays_silent_on_a_failed_query() {
    confirm_pins::assert_the_closure_rejects_absent_only_after_dispatch(
        client(absent()),
        client(live()),
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

//! The active order-status confirm (`VenueRest::confirm_order`, the body behind
//! `ExecutionClientActor::with_confirm`), offline: a stub transport answers the by-`clOrdId` GET on
//! `/trade/order`. The shared form of `vike_bridge_core::confirm`: found is read by its
//! STATE (decision 0171 item 2); `51603` "does not exist" -> the "venue confirms absent"
//! `OrderRejected`, but only for an order client id the exec thread finished dispatching; a FAILED query ->
//! NOTHING (never an id-less accept, never a terminal).

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_bridge_core::confirm_pins;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_model::events::Event;
use vike_okx::execution::rest::VenueRestOkx;

use crate::r6_okx_parity::{fixture, okx_perp};

/// What the by-clOrdId GET answers.
enum Answer {
    Live,
    /// The venue's row of the order.
    Row(Value),
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

impl RestTransportJson for LookupStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        // "absent" needs both endpoints: the algo half (`algo_stop.rs` pins it) is absent here too
        if path.ends_with("/trade/order-algo") {
            return Ok(json!({"code": "51603", "msg": "Order does not exist", "data": []}));
        }
        assert!(
            method.eq_ignore_ascii_case("GET") && path.ends_with("/trade/order"),
            "{method} {path}"
        );
        let order_client_id = params.iter().find(|(k, _)| *k == "clOrdId").unwrap().1.clone();
        self.seen.lock().unwrap().push(order_client_id.as_str().unwrap().to_string());
        match &self.answer {
            Answer::Live => Ok(json!({"code": "0", "msg": "", "data": [{"ordId": "live-1"}]})),
            Answer::Row(row) => Ok(json!({"code": "0", "msg": "", "data": [row]})),
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
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        panic!("unexpected signed_json {path}")
    }
    vike_bridge_core::no_public_call!();
}

fn client(answer: Answer) -> VenueRestOkx<LookupStub> {
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
            assert_eq!(a.order_client_id, "c1");
            assert_eq!(a.venue_order_id.as_deref(), Some("live-1"));
            assert_eq!(a.timestamp, 5);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(*c.transport.seen.lock().unwrap(), vec!["c1".to_string()]);
}

/// The order the by-order-client-id read found: a recorded `orders-history` row, whose `GET /trade/order` twin has
/// the documented same fields, with `over` laid on it. It carries the order's last fill (`fillSz` in
/// contracts, `tradeId`) and `fee`, never the stream's `fillFee`.
fn recorded_row(over: Value) -> Value {
    let recorded: Value =
        serde_json::from_str(include_str!("../fixtures/algo/rest_orders_history.json")).unwrap();
    let mut row = recorded["data"][1].clone();
    row["clOrdId"] = json!("c9");
    for (k, v) in over.as_object().unwrap() {
        row[k.as_str()] = v.clone();
    }
    row
}

const ORD_ID: &str = "4038738332804362241";

/// A found order's events, each named `kind [id or reason]`, with the order client id and the accept's `ts` checked.
fn answer_to(found: Value) -> Vec<String> {
    client(Answer::Row(found))
        .confirm_order("c9", 5)
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
            other => panic!("{other:?}"),
        })
        .collect()
}

/// Decision 0171 item 2, the cells that stay: live, partly filled and filled are the accept alone. ⚠ The
/// row's `fillSz` and `tradeId` must not reach the stream mapper's fill arm: it would book the row's size
/// in CONTRACTS with no fee (`fee` is not `fillFee`), a wrong-money fill the panic above refuses.
#[test]
fn confirm_found_open_or_filled_is_the_accept_alone_and_books_no_fill() {
    let accepted = [format!("accepted {ORD_ID}")];
    for over in [
        json!({"state": "live", "fillSz": "0", "tradeId": "", "accFillSz": "0"}),
        json!({"state": "partially_filled", "accFillSz": "0.005", "sz": "0.01"}),
        json!({"state": "filled"}),
        json!({"state": "a_later_word"}),
    ] {
        assert_eq!(answer_to(recorded_row(over.clone())), accepted, "{over}");
    }
}

/// Decision 0171 item 2, the closed cells: the accept, then the terminal, by the stream's own mapper. A
/// partly filled order that ended cancelled carries a fill on its row too: the cancel, no fill.
#[test]
fn confirm_found_closed_is_the_accept_then_its_terminal() {
    let accepted = format!("accepted {ORD_ID}");
    for (over, reason) in [
        (json!({"state": "canceled", "cancelSource": "1"}), "1"),
        (json!({"state": "mmp_canceled", "cancelSource": "20"}), "20"),
        (
            json!({"state": "canceled", "cancelSource": "31"}),
            vike_model::events::POST_ONLY_CANCEL_REASON,
        ),
    ] {
        assert_eq!(
            answer_to(recorded_row(over.clone())),
            [accepted.clone(), format!("canceled {reason}")],
            "{over}"
        );
    }
}

/// The read asked by this order client id, so its events are filed under it whatever the row spells.
#[test]
fn confirm_files_a_closed_row_under_the_order_client_id_it_asked_about() {
    let mut nameless = recorded_row(json!({"state": "canceled", "cancelSource": "1"}));
    nameless.as_object_mut().unwrap().remove("clOrdId");
    assert_eq!(answer_to(nameless), [format!("accepted {ORD_ID}"), "canceled 1".to_string()]);
}

/// `confirm_order` is the trait door that assumes the submit was dispatched: the venue's `51603` is
/// the "venue confirms absent" reject.
#[test]
fn confirm_absent_is_the_venue_confirms_absent_reject() {
    let c = client(Answer::Absent);
    match &c.confirm_order("c2", 5)[..] {
        [Event::OrderRejected(r)] => {
            assert_eq!(r.order_client_id, "c2");
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

/// "Absent" is only trusted once the exec thread FINISHED dispatching the order client id: a submit still queued
/// behind a slow cancel batch is not on the venue yet, so the lookup finding nothing proves nothing.
/// (The old weak form rejected it at once and the order then filled with no engine order to own it.)
#[test]
fn confirm_when_absent_for_an_undispatched_order_client_id_is_silence_and_found_needs_no_ledger() {
    confirm_pins::assert_absent_rejects_only_a_dispatched_order_client_id(
        client(Answer::Absent),
        client(Answer::Live),
    );
}

/// The closure the exec client hands to `ExecutionClientActor::with_confirm`: it reads the dispatch ledger BEFORE
/// the query, so absent rejects a dispatched order client id only, a failed query is silence even for a
/// dispatched one, and a repeat of the same order client id inside the backoff is skipped.
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

use super::*;
use crate::execution::user_data::DealRefs;
use crate::transport::IG_HTTP;
use serde_json::json;
use std::assert_matches;
use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};

fn order(order_type: OrderType, qty: f64) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        display_quantity: None,
        release: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        account: None,
        combo_legs: Vec::new(),
        order_client_id: "order-client-id-1".into(),
        venue: VENUE.into(),
        symbol: "CS.D.EURUSD.MINI.IP".into(),
        side: 1,
        qty,
        order_type,
        price: Some(1.09),
        trigger_price: Some(1.08),
        reduce_only: false,
        time_in_force: vike_model::TimeInForce::Gtc,
        gtd_expiry: None,
        timestamp: 222,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

fn err(status: u16, message: &str) -> VenueApiHttpError {
    IG_HTTP.error(status, message)
}

fn refs() -> OrderVenueIdRegistryDealReference {
    Arc::new(Mutex::new(DealRefs::default()))
}

// --- an order the ladder left unsettled --------------------------------------------------------

fn nothing_read(_: i64) -> Result<String, String> {
    panic!("an open reads no activity history")
}

fn nothing_listed() -> Result<String, String> {
    panic!("an open reads no working-order list")
}

/// Ask about the rows due at `at` until nothing more is due then, the way the exec thread's loop does.
fn ask_all(
    book: &mut Unsettled,
    at: Instant,
    refs: &OrderVenueIdRegistryDealReference,
    ids: &Arc<Mutex<OrderVenueIdRegistry<String>>>,
    confirm: impl Fn(&str) -> Result<Value, VenueApiHttpError>,
) -> Vec<Event> {
    let mut out = Vec::new();
    while book.next_due().is_some_and(|due| due <= at) {
        out.extend(book.ask_due(at, 7, refs, ids, &confirm, nothing_read, nothing_listed));
    }
    out
}

/// **A dead stop must not read as protection.** The POST was taken, all 7 `/confirms` failed, and the order went out
/// `Accepted` with no id. Asked again, IG says it refused the deal, and only `/confirms` carries that: the order ends as a
/// CANCEL in IG's words, because the FSM refuses `OrderRejected` from `Accepted` and the row would read live for ever.
#[test]
fn a_rejection_found_after_the_id_less_accept_ends_the_order_in_igs_words() {
    let (refs, ids, mut book) = (
        refs(),
        Arc::new(Mutex::new(OrderVenueIdRegistry::<String>::default())),
        Unsettled::default(),
    );
    refs.lock().unwrap().record("stop-1", "REF1", false);
    let t0 = Instant::now();
    book.note_confirm("stop-1", "REF1", false, true, true, t0);

    assert!(
        book.ask_due(t0, 7, &refs, &ids, |_| panic!("not due yet"), nothing_read, nothing_listed)
            .is_empty()
    );
    let t1 = t0 + ASK_AGAIN_AFTER[0];
    let not_yet = |_: &str| Err(err(404, "error.service.execution.find"));
    assert!(ask_all(&mut book, t1, &refs, &ids, not_yet).is_empty(), "a miss settles nothing");
    assert_eq!(book.next_due(), Some(t1 + ASK_AGAIN_AFTER[1]), "the next pause of the backoff");

    let t2 = t1 + ASK_AGAIN_AFTER[1];
    let events = ask_all(&mut book, t2, &refs, &ids, |deal_ref| {
        assert_eq!(deal_ref, "REF1");
        Ok(json!({ "dealStatus": "REJECTED", "reason": "MARKET_CLOSED", "dealId": "DIAAA9" }))
    });
    assert_matches!(
        events.as_slice(),
        [Event::OrderCanceled(c)] if c.order_client_id == "stop-1"
            && c.reason.starts_with("MARKET_CLOSED ")
    );
    assert_eq!(book.next_due(), None, "settled, not asked again");
    assert_eq!(refs.lock().unwrap().pending_for("stop-1"), None, "a refused deal is forgotten");
    assert_eq!(
        ids.lock().unwrap().venue_order_id_of("stop-1"),
        None,
        "and no cancel can address it"
    );
}

/// The late answer that accepts teaches the `dealId`: the order the ladder left uncancellable can now be cancelled.
#[test]
fn an_answer_that_accepts_teaches_the_deal_id() {
    let (refs, ids, mut book) = (
        refs(),
        Arc::new(Mutex::new(OrderVenueIdRegistry::<String>::default())),
        Unsettled::default(),
    );
    let t0 = Instant::now();
    book.note_confirm("stop-2", "REF2", false, true, true, t0);
    let events = ask_all(&mut book, t0 + ASK_AGAIN_AFTER[0], &refs, &ids, |_| {
        Ok(json!({ "dealStatus": "ACCEPTED", "dealId": "DIAAA2", "status": "OPEN" }))
    });
    assert_matches!(
        events.as_slice(),
        [Event::OrderAccepted(a)] if a.venue_order_id.as_deref() == Some("DIAAA2")
    );
    assert_eq!(ids.lock().unwrap().venue_order_id_of("stop-2").map(String::as_str), Some("DIAAA2"));
}

/// An unknown create was not answered, so only a reply naming its reference may speak for it; a rejection then IS
/// the order's `OrderRejected`, because it was never accepted.
#[test]
fn an_unknown_create_needs_an_echo_and_is_rejected_not_cancelled() {
    let (refs, ids, mut book) = (
        refs(),
        Arc::new(Mutex::new(OrderVenueIdRegistry::<String>::default())),
        Unsettled::default(),
    );
    let t0 = Instant::now();
    book.note_confirm("c-3", "REF3", true, false, false, t0);
    let t1 = t0 + ASK_AGAIN_AFTER[0];
    let other = |_: &str| Ok(json!({ "dealStatus": "REJECTED", "dealReference": "SOMEONE-ELSE" }));
    assert!(ask_all(&mut book, t1, &refs, &ids, other).is_empty(), "not ours: it says nothing");
    let t2 = t1 + ASK_AGAIN_AFTER[1];
    let events = ask_all(&mut book, t2, &refs, &ids, |_| {
        Ok(json!({ "dealStatus": "REJECTED", "dealReference": "REF3", "reason": "MARKET_CLOSED" }))
    });
    assert_matches!(events.as_slice(), [Event::OrderRejected(r)] if r.order_client_id == "c-3");
}

/// A reply that neither accepts nor rejects leaves the row asking.
#[test]
fn a_reply_that_says_nothing_keeps_the_order_asked_about() {
    let (refs, ids, mut book) = (
        refs(),
        Arc::new(Mutex::new(OrderVenueIdRegistry::<String>::default())),
        Unsettled::default(),
    );
    let t0 = Instant::now();
    book.note_confirm("c-4", "REF4", true, true, true, t0);
    let t1 = t0 + ASK_AGAIN_AFTER[0];
    let silent = |_: &str| Ok(json!({ "dealId": "DIAAA4" }));
    assert!(ask_all(&mut book, t1, &refs, &ids, silent).is_empty());
    assert!(book.next_due().is_some());
    assert_eq!(ids.lock().unwrap().venue_order_id_of("c-4"), None);
}

/// The asking is bounded, and the pauses grow to a ceiling: IG meters these reads.
#[test]
fn the_asking_backs_off_and_stops() {
    let (refs, ids, mut book) = (
        refs(),
        Arc::new(Mutex::new(OrderVenueIdRegistry::<String>::default())),
        Unsettled::default(),
    );
    let t0 = Instant::now();
    book.note_confirm("c-5", "REF5", true, true, true, t0);
    let not_yet = |_: &str| Err(err(404, "error.service.execution.find"));
    let mut at = t0;
    let mut pauses = Vec::new();
    let mut reads = 0;
    while let Some(due) = book.next_due() {
        pauses.push(due - at);
        at = due;
        assert!(ask_all(&mut book, at, &refs, &ids, not_yet).is_empty());
        reads += 1;
        assert!(reads < 100, "bounded");
    }
    assert_eq!(pauses[..3], ASK_AGAIN_AFTER[..3], "the first pauses are the backoff");
    assert_eq!(pauses.last(), Some(&ASK_AGAIN_AFTER[4]), "and it stays at its ceiling");
    assert!(at >= t0 + ASK_AGAIN_FOR, "it is left to the reconcile only past the bound");
    assert!(at < t0 + ASK_AGAIN_FOR + ASK_AGAIN_AFTER[4] + ASK_AGAIN_AFTER[4]);
}

// --- create: the symbol ------------------------------------------------------------------------

/// `EURUSD` was sent as the epic and answered HTTP 500 `error.service.create.working.order.generic`
/// — a code that names nothing. The refusal names the form to send.
#[test]
fn a_bare_pair_name_is_refused_naming_the_epic_form() {
    let why = non_epic_refusal("EURUSD").expect("a dotless symbol cannot be an epic");
    assert!(why.contains("CS.D.EURUSD.MINI.IP"), "{why}");
    assert!(why.contains("EURUSD"), "the refused symbol is named: {why}");
    assert!(non_epic_refusal("").is_some(), "an empty symbol is not an epic either");
    assert_eq!(non_epic_refusal("CS.D.EURUSD.MINI.IP"), None);
    assert_eq!(non_epic_refusal("IX.D.DAX.DAILY.IP"), None, "a non-FX epic is dotted too");
}

// --- cancel -------------------------------------------------------------------------------------

#[test]
fn only_an_accepted_deleted_confirm_is_a_cancel() {
    assert!(matches!(
        map_cancel_confirm(&json!({"dealStatus": "ACCEPTED", "status": "DELETED"})),
        ExecutionClientActorCancelOutcome::Canceled
    ));
    // The DELETE endpoint can only delete: an ACCEPTED confirm with no `status` is still the verdict.
    assert!(matches!(
        map_cancel_confirm(&json!({"dealStatus": "ACCEPTED"})),
        ExecutionClientActorCancelOutcome::Canceled
    ));
    match map_cancel_confirm(&json!({"dealStatus": "REJECTED", "reason": "ORDER_NOT_FOUND"})) {
        ExecutionClientActorCancelOutcome::Rejected(r) => {
            assert!(r.contains("ORDER_NOT_FOUND"), "{r}")
        }
        ExecutionClientActorCancelOutcome::Canceled => panic!("a REJECTED confirm is not a cancel"),
    }
    for odd in [
        json!({"dealStatus": "ACCEPTED", "status": "OPEN"}),
        json!({"dealStatus": "ACCEPTED", "status": "AMENDED"}),
        json!({"status": "DELETED"}),
        json!({}),
    ] {
        assert!(
            matches!(map_cancel_confirm(&odd), ExecutionClientActorCancelOutcome::Rejected(_)),
            "{odd}"
        );
    }
}

#[test]
fn a_cancel_is_reported_from_the_confirm_not_from_the_delete() {
    let paths = RefCell::new(Vec::new());
    let outcome = cancel_working_order(
        &refs(),
        "order-client-id-1",
        "DIAAA1",
        |path| {
            paths.borrow_mut().push(path.to_string());
            Ok(json!({"dealReference": "CANCELREF"}))
        },
        |r| {
            assert_eq!(r, "CANCELREF", "the DELETE's own reference is what gets confirmed");
            Ok(json!({"dealStatus": "ACCEPTED", "status": "DELETED", "dealId": "DIAAA1"}))
        },
    );
    assert!(matches!(outcome, Some(ExecutionClientActorCancelOutcome::Canceled)));
    assert_eq!(*paths.borrow(), vec!["/workingorders/otc/DIAAA1".to_string()]);
}

/// The 200 of the DELETE alone used to be `Canceled`. A REJECTED confirm must now be a
/// NON-terminal rejection carrying IG's reason — the order may still rest.
#[test]
fn a_rejected_cancel_confirm_is_a_cancel_rejection_with_the_reason() {
    let outcome = cancel_working_order(
        &refs(),
        "order-client-id-1",
        "DIAAA1",
        |_| Ok(json!({"dealReference": "R"})),
        |_| Ok(json!({"dealStatus": "REJECTED", "reason": "ORDER_NOT_FOUND"})),
    );
    match outcome {
        Some(ExecutionClientActorCancelOutcome::Rejected(r)) => {
            assert!(r.contains("ORDER_NOT_FOUND"), "{r}")
        }
        _ => panic!("a rejected cancel must not terminalize the order"),
    }
}

#[test]
fn a_refused_delete_carries_ig_error_code_and_never_asks_for_a_confirm() {
    let asked = Cell::new(false);
    let outcome = cancel_working_order(
        &refs(),
        "order-client-id-1",
        "DIAAA1",
        |_| Err(err(404, "error.service.order.not-found")),
        |_| {
            asked.set(true);
            Ok(json!({}))
        },
    );
    match outcome {
        Some(ExecutionClientActorCancelOutcome::Rejected(r)) => {
            assert!(r.contains("error.service.order.not-found"), "{r}");
        }
        _ => panic!("a refused DELETE is not a cancel"),
    }
    assert!(!asked.get());
}

#[test]
fn a_delete_without_a_reference_cannot_be_confirmed_so_it_is_not_a_cancel() {
    let outcome = cancel_working_order(
        &refs(),
        "order-client-id-1",
        "DIAAA1",
        |_| Ok(json!({})),
        |_| panic!("there is no reference to confirm"),
    );
    assert!(matches!(outcome, Some(ExecutionClientActorCancelOutcome::Rejected(_))));
}

/// The confirm never answering is NOT a cancel (non-terminal), and the cancel's reference is taught
/// to the STREAM so IG's streamed DELETED confirm — if the cancel landed — still terminalizes the
/// order. The submit-time `order_client_id -> dealReference` half (what the watchdog asks IG with) is left alone.
#[test]
fn an_unanswered_cancel_confirm_is_unconfirmed_and_teaches_the_stream_the_reference() {
    let deal_refs = refs();
    deal_refs.lock().unwrap().record("order-client-id-1", "SUBMITREF", false);
    let outcome = cancel_working_order(
        &deal_refs,
        "order-client-id-1",
        "DIAAA1",
        |_| Ok(json!({"dealReference": "CANCELREF"})),
        |_| Err(err(404, "error.service.execution.find")),
    );
    match outcome {
        Some(ExecutionClientActorCancelOutcome::Rejected(r)) => {
            assert!(r.contains("CANCELREF") && r.contains("unconfirmed"), "{r}");
        }
        _ => panic!("an unconfirmed cancel must not terminalize"),
    }
    let g = deal_refs.lock().unwrap();
    assert_eq!(g.order_client_id_for("CANCELREF").as_deref(), Some("order-client-id-1"));
    assert_eq!(
        g.pending_for("order-client-id-1").map(|p| p.deal_ref).as_deref(),
        Some("SUBMITREF"),
        "the watchdog's order client id -> dealReference half is untouched"
    );
}

/// ...and the streamed DELETED confirm for that reference decodes to the terminal.
#[test]
fn the_streamed_deleted_confirm_of_a_recorded_cancel_reference_terminalizes() {
    use crate::execution::event_mapper::decode_trade_confirm;
    let deal_refs = refs();
    let _ = cancel_working_order(
        &deal_refs,
        "order-client-id-1",
        "DIAAA1",
        |_| Ok(json!({"dealReference": "CANCELREF"})),
        |_| Err(err(0, "network error")),
    );
    let order_client_id =
        deal_refs.lock().unwrap().order_client_id_for("CANCELREF").expect("recorded");
    let events = decode_trade_confirm(
        &json!({"dealStatus": "ACCEPTED", "status": "DELETED", "dealReference": "CANCELREF"}),
        &order_client_id,
        1,
    );
    assert_matches!(&events[..], [Event::OrderCanceled(c)] if c.order_client_id == "order-client-id-1");
}

// --- amend --------------------------------------------------------------------------------------

fn amend(
    o: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    deal_id: Option<&str>,
    put: Result<serde_json::Value, VenueApiHttpError>,
    confirm: Result<serde_json::Value, VenueApiHttpError>,
) -> Event {
    amend_working_order(o, new_qty, new_price, deal_id, |_, _| put, |_| confirm).0
}

#[test]
fn the_amend_body_is_level_type_time_in_force_and_goodtilldate_with_no_size() {
    let limit = build_amend_body(&order(OrderType::Limit, 1.0), 1.0923);
    assert_eq!(
        limit,
        json!({
            "level": "1.09230",
            "type": "LIMIT",
            "timeInForce": "GOOD_TILL_CANCELLED",
            "goodTillDate": null,
        })
    );
    assert_eq!(build_amend_body(&order(OrderType::Stop, 1.0), 1.07)["type"], "STOP");
    assert!(limit.get("size").is_none(), "IG's PUT has no size field");
}

#[test]
fn a_price_amend_puts_to_the_deal_and_is_modified_on_an_amended_confirm() {
    let o = order(OrderType::Limit, 2.0);
    let seen = RefCell::new(None);
    let before = vike_model::now_ms();
    let (event, _) = amend_working_order(
        &o,
        None,
        Some(1.1),
        Some("DIAAA1"),
        |path, body| {
            *seen.borrow_mut() = Some((path.to_string(), body.clone()));
            Ok(json!({"dealReference": "AMENDREF"}))
        },
        |r| {
            assert_eq!(r, "AMENDREF");
            Ok(json!({"dealStatus": "ACCEPTED", "status": "AMENDED", "dealId": "DIAAA1"}))
        },
    );
    let (path, body) = seen.into_inner().expect("the PUT was sent");
    assert_eq!(path, "/workingorders/otc/DIAAA1");
    assert_eq!(body["level"], "1.10000");
    match event {
        Event::OrderModified(m) => {
            assert_eq!(m.order_client_id, "order-client-id-1");
            assert_eq!(m.venue_order_id.as_deref(), Some("DIAAA1"));
            assert_eq!(m.new_price, Some(1.1));
            assert_eq!(m.new_qty, None, "the size did not move, so none is claimed");
            assert!(m.timestamp >= before, "stamped when IG answered, not at the order's creation");
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
}

/// The trait-level modify may carry the order's OWN qty alongside the price; that is not a resize.
#[test]
fn restating_the_orders_own_size_is_not_a_size_change() {
    let o = order(OrderType::Limit, 2.0);
    let event = amend(
        &o,
        Some(2.0),
        Some(1.1),
        Some("DIAAA1"),
        Ok(json!({"dealReference": "R"})),
        Ok(json!({"dealStatus": "ACCEPTED", "status": "AMENDED"})),
    );
    assert_matches!(event, Event::OrderModified(_));
}

fn rejected_reason(event: Event) -> String {
    match event {
        Event::OrderModifyRejected(r) => r.reason.to_string(),
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
}

#[test]
fn a_size_change_is_an_explicit_modify_rejection_and_sends_nothing() {
    let o = order(OrderType::Limit, 2.0);
    let (event, _) = amend_working_order(
        &o,
        Some(3.0),
        Some(1.1),
        Some("DIAAA1"),
        |_, _| panic!("IG's PUT cannot change a size, so nothing is sent"),
        |_| panic!("nothing to confirm"),
    );
    let why = rejected_reason(event);
    assert!(why.contains("size cannot be amended"), "{why}");
}

#[test]
fn the_unamendable_cases_each_say_why() {
    let limit = order(OrderType::Limit, 1.0);
    // no price
    let why = rejected_reason(amend(&limit, None, None, Some("D"), Ok(json!({})), Ok(json!({}))));
    assert!(why.contains("no usable new level"), "{why}");
    // a non-finite / non-positive price
    for p in [f64::NAN, 0.0, -1.0] {
        let why =
            rejected_reason(amend(&limit, None, Some(p), Some("D"), Ok(json!({})), Ok(json!({}))));
        assert!(why.contains("no usable new level"), "{why}");
    }
    // a market order has nothing resting, nor has a kind this bridge builds no order for (never amended as a STOP)
    for kind in [
        OrderType::Market,
        OrderType::StopLimit,
        OrderType::TakeProfit,
        OrderType::Unrecognized("market_if_touched".into()),
    ] {
        let why = rejected_reason(amend(
            &order(kind.clone(), 1.0),
            None,
            Some(1.1),
            Some("D"),
            Ok(json!({})),
            Ok(json!({})),
        ));
        assert!(why.contains("only a resting LIMIT or STOP"), "{kind}: {why}");
    }
    // no dealId known to this process
    let why = rejected_reason(amend(&limit, None, Some(1.1), None, Ok(json!({})), Ok(json!({}))));
    assert!(why.contains("no working order for order-client-id-1"), "{why}");
}

#[test]
fn a_refused_put_carries_ig_reason() {
    let event = amend(
        &order(OrderType::Limit, 1.0),
        None,
        Some(1.1),
        Some("D"),
        Err(err(400, "error.service.update.working.order.level")),
        Ok(json!({})),
    );
    assert!(rejected_reason(event).contains("error.service.update.working.order.level"));
}

#[test]
fn a_rejected_amend_confirm_is_a_modify_rejection_with_the_reason() {
    let event = amend(
        &order(OrderType::Limit, 1.0),
        None,
        Some(1.1),
        Some("D"),
        Ok(json!({"dealReference": "R"})),
        Ok(json!({"dealStatus": "REJECTED", "reason": "ORDER_LEVEL_TOO_CLOSE"})),
    );
    assert!(rejected_reason(event).contains("ORDER_LEVEL_TOO_CLOSE"));
}

#[test]
fn an_accepted_confirm_that_is_not_an_amendment_is_not_a_modification() {
    for odd in [
        json!({"dealStatus": "ACCEPTED", "status": "OPEN"}),
        json!({"dealStatus": "ACCEPTED"}),
        json!({}),
    ] {
        let event = amend(
            &order(OrderType::Limit, 1.0),
            None,
            Some(1.1),
            Some("D"),
            Ok(json!({"dealReference": "R"})),
            Ok(odd.clone()),
        );
        assert_matches!(event, Event::OrderModifyRejected(_), "{odd}");
    }
}

/// The PUT's reply was lost after the write: IG may have moved the level, so the refusal must not read "keeps its terms".
/// A refusal IG answered, or a failure provably before the write, is what it always was.
#[test]
fn an_amend_whose_reply_was_lost_says_the_outcome_is_unknown() {
    let lost = amend(
        &order(OrderType::Limit, 1.0),
        None,
        Some(1.1),
        Some("D"),
        Err(err(VenueApiHttpError::STATUS_OUTCOME_UNKNOWN, "network error: timed out")),
        Ok(json!({})),
    );
    let reason = rejected_reason(lost);
    assert!(reason.contains("outcome is UNKNOWN") && reason.contains("may have moved"), "{reason}");
    let gateway = amend(
        &order(OrderType::Limit, 1.0),
        None,
        Some(1.1),
        Some("D"),
        Err(err(504, "gateway")),
        Ok(json!({})),
    );
    assert!(rejected_reason(gateway).contains("outcome is UNKNOWN"));
    for answered in [err(400, "invalid.level"), err(0, "network error: connection refused")] {
        let refused = amend(
            &order(OrderType::Limit, 1.0),
            None,
            Some(1.1),
            Some("D"),
            Err(answered),
            Ok(json!({})),
        );
        assert!(!rejected_reason(refused).contains("UNKNOWN"));
    }
}

/// The PUT landed but IG's answer was lost: the honest event says the level is UNCONFIRMED (and
/// non-terminal) rather than claiming a modification or staying silent.
#[test]
fn an_amend_whose_confirm_never_answers_says_it_is_unconfirmed() {
    let missing_ref = amend(
        &order(OrderType::Limit, 1.0),
        None,
        Some(1.1),
        Some("D"),
        Ok(json!({})),
        Ok(json!({})),
    );
    assert!(rejected_reason(missing_ref).contains("unconfirmed"));
    let lost = amend(
        &order(OrderType::Limit, 1.0),
        None,
        Some(1.1),
        Some("D"),
        Ok(json!({"dealReference": "AMENDREF"})),
        Err(err(404, "error.service.execution.find")),
    );
    let why = rejected_reason(lost);
    assert!(why.contains("AMENDREF") && why.contains("unconfirmed"), "{why}");
}

/// **What IG may have moved is read back, and nothing else.** An amend whose PUT answer, reference or confirm was lost
/// says so (`true`: the exec thread reads IG's working orders for it); one IG answered either way does not.
#[test]
fn only_an_amend_whose_answer_was_lost_is_read_back() {
    let asks = |put: Result<Value, VenueApiHttpError>,
                confirm: Result<Value, VenueApiHttpError>| {
        let o = order(OrderType::Limit, 1.0);
        amend_working_order(&o, None, Some(1.1), Some("D"), |_, _| put, |_| confirm).1
    };
    let unknown = VenueApiHttpError::STATUS_OUTCOME_UNKNOWN;
    assert!(asks(Err(err(unknown, "network error: timed out")), Ok(json!({}))));
    assert!(asks(Err(err(504, "gateway")), Ok(json!({}))));
    assert!(asks(Ok(json!({})), Ok(json!({}))), "a reply with no reference to confirm");
    let lost = Err(err(404, "error.service.execution.find"));
    assert!(asks(Ok(json!({"dealReference": "R"})), lost));
    let amended = Ok(json!({"dealStatus": "ACCEPTED", "status": "AMENDED"}));
    assert!(!asks(Ok(json!({"dealReference": "R"})), amended));
    let refused = Ok(json!({"dealStatus": "REJECTED", "reason": "ORDER_LEVEL_TOO_CLOSE"}));
    assert!(!asks(Ok(json!({"dealReference": "R"})), refused));
    assert!(!asks(Err(err(400, "invalid.level")), Ok(json!({}))));
}

/// **An amend nothing confirmed settles to the level IG lists** (IG5: a modify to 0.9196 lost its answer and the
/// platform kept 0.948 with nothing left to read it). Moved: `OrderModified` to IG's level, and the row ends. Kept (the
/// amend did not land) or gone (the order no longer rests): nothing to send, and the row ends too. A list that cannot
/// be read is asked again; a later amend's row replaces the first, and a confirmed amend drops it.
#[test]
fn an_amend_nothing_confirmed_settles_to_the_level_ig_lists() {
    let (refs, ids) = (refs(), Arc::new(Mutex::new(OrderVenueIdRegistry::<String>::default())));
    let stop = OrderRequest { trigger_price: Some(0.948), ..order(OrderType::Stop, 1.0) };
    let listed = |level: f64| {
        json!({ "workingOrders": [{ "workingOrderData": {
            "dealId": "DIAAA1", "epic": "CS.D.EURUSD.MINI.IP", "direction": "SELL",
            "orderType": "STOP", "orderSize": 1.0, "orderLevel": level } }] })
        .to_string()
    };
    let ask = |book: &mut Unsettled, at: Instant, list: Result<String, String>| {
        book.ask_due(
            at,
            7,
            &refs,
            &ids,
            |_| panic!("an amend reads no confirm"),
            nothing_read,
            || list,
        )
    };
    let t0 = Instant::now();
    let due = t0 + ASK_AGAIN_AFTER[0];

    let mut book = Unsettled::default();
    book.note_amend(&stop, "DIAAA1", t0);
    assert_matches!(
        ask(&mut book, due, Ok(listed(0.9196))).as_slice(),
        [Event::OrderModified(m)] if m.order_client_id == "order-client-id-1"
            && m.new_price == Some(0.9196) && m.venue_order_id.as_deref() == Some("DIAAA1")
    );
    assert_eq!(book.next_due(), None, "settled, not asked again");

    for list in [listed(0.948), json!({ "workingOrders": [] }).to_string()] {
        let mut book = Unsettled::default();
        book.note_amend(&stop, "DIAAA1", t0);
        assert!(ask(&mut book, due, Ok(list.clone())).is_empty(), "{list}");
        assert_eq!(book.next_due(), None, "settled with nothing to send: {list}");
    }

    let mut book = Unsettled::default();
    book.note_amend(&stop, "DIAAA1", t0);
    assert!(ask(&mut book, due, Err("IG error 1: timed out".into())).is_empty());
    assert_eq!(book.next_due(), Some(due + ASK_AGAIN_AFTER[1]), "asked again");
    book.note_amend(&stop, "DIAAA1", t0);
    assert_eq!(book.next_due(), Some(due), "one row per order: the later amend's");
    book.forget_amend("order-client-id-1");
    assert_eq!(book.next_due(), None);
}

/// **An amend IG never lists is given up in words that name what can read its level, not the reconcile**, which reports
/// a working order's level and corrects none.
#[test]
fn an_amend_given_up_points_at_the_venues_read_not_the_reconcile() {
    let (refs, ids, mut book) = (
        refs(),
        Arc::new(Mutex::new(OrderVenueIdRegistry::<String>::default())),
        Unsettled::default(),
    );
    let stop = OrderRequest { trigger_price: Some(0.948), ..order(OrderType::Stop, 1.0) };
    let t0 = Instant::now();
    book.note_amend(&stop, "DIAAA1", t0);
    let ((), logged) = vike_log::capture::captured(|| {
        let mut at = t0;
        while let Some(due) = book.next_due() {
            at = due;
            let settled = book.ask_due(
                at,
                7,
                &refs,
                &ids,
                |_| panic!("an amend reads no confirm"),
                nothing_read,
                || Err("IG error 1: timed out".to_string()),
            );
            assert!(settled.is_empty());
        }
        assert!(at >= t0 + ASK_AGAIN_FOR, "given up only past the bound");
    });
    let gave_up: Vec<&str> = logged
        .iter()
        .filter(|e| e.level == tracing::Level::ERROR)
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(gave_up.len(), 1, "{gave_up:?}");
    assert!(
        gave_up[0].contains("within 10 minutes")
            && gave_up[0].contains("trade venue ig open-orders"),
        "{gave_up:?}"
    );
    assert!(!gave_up[0].contains("reconcile"), "{gave_up:?}");
}

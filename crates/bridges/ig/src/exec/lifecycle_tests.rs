use super::*;
use crate::exec::stream::DealRefs;
use serde_json::json;
use std::assert_matches;
use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};

fn order(order_type: OrderType, qty: f64) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        account: None,
        combo_legs: Vec::new(),
        client_order_id: "coid-1".into(),
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
        ts: 222,
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

fn err(status: u16, message: &str) -> IgApiError {
    IgApiError { status, message: message.into() }
}

fn refs() -> DealRefMap {
    Arc::new(Mutex::new(DealRefs::default()))
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
        CancelOutcome::Canceled
    ));
    // The DELETE endpoint can only delete: an ACCEPTED confirm with no `status` is still the verdict.
    assert!(matches!(
        map_cancel_confirm(&json!({"dealStatus": "ACCEPTED"})),
        CancelOutcome::Canceled
    ));
    match map_cancel_confirm(&json!({"dealStatus": "REJECTED", "reason": "ORDER_NOT_FOUND"})) {
        CancelOutcome::Rejected(r) => assert!(r.contains("ORDER_NOT_FOUND"), "{r}"),
        CancelOutcome::Canceled => panic!("a REJECTED confirm is not a cancel"),
    }
    for odd in [
        json!({"dealStatus": "ACCEPTED", "status": "OPEN"}),
        json!({"dealStatus": "ACCEPTED", "status": "AMENDED"}),
        json!({"status": "DELETED"}),
        json!({}),
    ] {
        assert!(matches!(map_cancel_confirm(&odd), CancelOutcome::Rejected(_)), "{odd}");
    }
}

#[test]
fn a_cancel_is_reported_from_the_confirm_not_from_the_delete() {
    let paths = RefCell::new(Vec::new());
    let outcome = cancel_working_order(
        &refs(),
        "coid-1",
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
    assert!(matches!(outcome, CancelOutcome::Canceled));
    assert_eq!(*paths.borrow(), vec!["/workingorders/otc/DIAAA1".to_string()]);
}

/// The 200 of the DELETE alone used to be `Canceled`. A REJECTED confirm must now be a
/// NON-terminal rejection carrying IG's reason — the order may still rest.
#[test]
fn a_rejected_cancel_confirm_is_a_cancel_rejection_with_the_reason() {
    let outcome = cancel_working_order(
        &refs(),
        "coid-1",
        "DIAAA1",
        |_| Ok(json!({"dealReference": "R"})),
        |_| Ok(json!({"dealStatus": "REJECTED", "reason": "ORDER_NOT_FOUND"})),
    );
    match outcome {
        CancelOutcome::Rejected(r) => assert!(r.contains("ORDER_NOT_FOUND"), "{r}"),
        CancelOutcome::Canceled => panic!("a rejected cancel must not terminalize the order"),
    }
}

#[test]
fn a_refused_delete_carries_ig_error_code_and_never_asks_for_a_confirm() {
    let asked = Cell::new(false);
    let outcome = cancel_working_order(
        &refs(),
        "coid-1",
        "DIAAA1",
        |_| Err(err(404, "error.service.order.not-found")),
        |_| {
            asked.set(true);
            Ok(json!({}))
        },
    );
    match outcome {
        CancelOutcome::Rejected(r) => assert!(r.contains("error.service.order.not-found"), "{r}"),
        CancelOutcome::Canceled => panic!("a refused DELETE is not a cancel"),
    }
    assert!(!asked.get());
}

#[test]
fn a_delete_without_a_reference_cannot_be_confirmed_so_it_is_not_a_cancel() {
    let outcome = cancel_working_order(
        &refs(),
        "coid-1",
        "DIAAA1",
        |_| Ok(json!({})),
        |_| panic!("there is no reference to confirm"),
    );
    assert!(matches!(outcome, CancelOutcome::Rejected(_)));
}

/// The confirm never answering is NOT a cancel (non-terminal), and the cancel's reference is taught
/// to the STREAM so IG's streamed DELETED confirm — if the cancel landed — still terminalizes the
/// order. The submit-time `coid -> dealReference` half (what the watchdog asks IG with) is left alone.
#[test]
fn an_unanswered_cancel_confirm_is_unconfirmed_and_teaches_the_stream_the_reference() {
    let deal_refs = refs();
    deal_refs.lock().unwrap().record("coid-1", "SUBMITREF", false);
    let outcome = cancel_working_order(
        &deal_refs,
        "coid-1",
        "DIAAA1",
        |_| Ok(json!({"dealReference": "CANCELREF"})),
        |_| Err(err(404, "error.service.execution.find")),
    );
    match outcome {
        CancelOutcome::Rejected(r) => {
            assert!(r.contains("CANCELREF") && r.contains("unconfirmed"), "{r}");
        }
        CancelOutcome::Canceled => panic!("an unconfirmed cancel must not terminalize"),
    }
    let g = deal_refs.lock().unwrap();
    assert_eq!(g.coid_for("CANCELREF").as_deref(), Some("coid-1"));
    assert_eq!(
        g.pending_for("coid-1").map(|p| p.deal_ref).as_deref(),
        Some("SUBMITREF"),
        "the watchdog's coid -> dealReference half is untouched"
    );
}

/// ...and the streamed DELETED confirm for that reference decodes to the terminal.
#[test]
fn the_streamed_deleted_confirm_of_a_recorded_cancel_reference_terminalizes() {
    use crate::exec::event_mapper::decode_trade_confirm;
    let deal_refs = refs();
    let _ = cancel_working_order(
        &deal_refs,
        "coid-1",
        "DIAAA1",
        |_| Ok(json!({"dealReference": "CANCELREF"})),
        |_| Err(err(0, "network error")),
    );
    let coid = deal_refs.lock().unwrap().coid_for("CANCELREF").expect("recorded");
    let evs = decode_trade_confirm(
        &json!({"dealStatus": "ACCEPTED", "status": "DELETED", "dealReference": "CANCELREF"}),
        &coid,
        1,
    );
    assert_matches!(&evs[..], [Event::OrderCanceled(c)] if c.client_order_id == "coid-1");
}

// --- amend --------------------------------------------------------------------------------------

fn amend(
    o: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    deal_id: Option<&str>,
    put: Result<serde_json::Value, IgApiError>,
    confirm: Result<serde_json::Value, IgApiError>,
) -> Event {
    amend_working_order(o, new_qty, new_price, deal_id, 7, |_, _| put, |_| confirm)
}

#[test]
fn the_amend_body_is_level_type_tif_and_goodtilldate_with_no_size() {
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
    let ev = amend_working_order(
        &o,
        None,
        Some(1.1),
        Some("DIAAA1"),
        7,
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
    match ev {
        Event::OrderModified(m) => {
            assert_eq!(m.client_order_id, "coid-1");
            assert_eq!(m.venue_order_id.as_deref(), Some("DIAAA1"));
            assert_eq!(m.new_price, Some(1.1));
            assert_eq!(m.new_qty, None, "the size did not move, so none is claimed");
            assert_eq!(m.ts, 7);
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
}

/// The trait-level modify may carry the order's OWN qty alongside the price; that is not a resize.
#[test]
fn restating_the_orders_own_size_is_not_a_size_change() {
    let o = order(OrderType::Limit, 2.0);
    let ev = amend(
        &o,
        Some(2.0),
        Some(1.1),
        Some("DIAAA1"),
        Ok(json!({"dealReference": "R"})),
        Ok(json!({"dealStatus": "ACCEPTED", "status": "AMENDED"})),
    );
    assert_matches!(ev, Event::OrderModified(_));
}

fn rejected_reason(ev: Event) -> String {
    match ev {
        Event::OrderModifyRejected(r) => r.reason.to_string(),
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
}

#[test]
fn a_size_change_is_an_explicit_modify_rejection_and_sends_nothing() {
    let o = order(OrderType::Limit, 2.0);
    let ev = amend_working_order(
        &o,
        Some(3.0),
        Some(1.1),
        Some("DIAAA1"),
        7,
        |_, _| panic!("IG's PUT cannot change a size, so nothing is sent"),
        |_| panic!("nothing to confirm"),
    );
    let why = rejected_reason(ev);
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
    // a market order has nothing resting
    let why = rejected_reason(amend(
        &order(OrderType::Market, 1.0),
        None,
        Some(1.1),
        Some("D"),
        Ok(json!({})),
        Ok(json!({})),
    ));
    assert!(why.contains("only a resting LIMIT or STOP"), "{why}");
    // no dealId known to this process
    let why = rejected_reason(amend(&limit, None, Some(1.1), None, Ok(json!({})), Ok(json!({}))));
    assert!(why.contains("no working order for coid-1"), "{why}");
}

#[test]
fn a_refused_put_carries_ig_reason() {
    let ev = amend(
        &order(OrderType::Limit, 1.0),
        None,
        Some(1.1),
        Some("D"),
        Err(err(400, "error.service.update.working.order.level")),
        Ok(json!({})),
    );
    assert!(rejected_reason(ev).contains("error.service.update.working.order.level"));
}

#[test]
fn a_rejected_amend_confirm_is_a_modify_rejection_with_the_reason() {
    let ev = amend(
        &order(OrderType::Limit, 1.0),
        None,
        Some(1.1),
        Some("D"),
        Ok(json!({"dealReference": "R"})),
        Ok(json!({"dealStatus": "REJECTED", "reason": "ORDER_LEVEL_TOO_CLOSE"})),
    );
    assert!(rejected_reason(ev).contains("ORDER_LEVEL_TOO_CLOSE"));
}

#[test]
fn an_accepted_confirm_that_is_not_an_amendment_is_not_a_modification() {
    for odd in [
        json!({"dealStatus": "ACCEPTED", "status": "OPEN"}),
        json!({"dealStatus": "ACCEPTED"}),
        json!({}),
    ] {
        let ev = amend(
            &order(OrderType::Limit, 1.0),
            None,
            Some(1.1),
            Some("D"),
            Ok(json!({"dealReference": "R"})),
            Ok(odd.clone()),
        );
        assert_matches!(ev, Event::OrderModifyRejected(_), "{odd}");
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

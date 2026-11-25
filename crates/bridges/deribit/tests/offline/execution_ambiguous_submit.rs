//! The deribit ORDER socket dying MID-SUBMIT — audit T1, over the local WebSocket stand-in in
//! `crate::fake_deribit_websocket`. No venue, no credentials, no `#[ignore]`.
//!
//! **The defect these pin is live-money.** `crates/bridges/deribit/src/transport/client.rs`'s
//! `DeribitTransport::call` marked only an exact response TIMEOUT as
//! `E_TIMEOUT_AMBIGUOUS`; `closed mid-request` fell through with `code: 0` and reached
//! `crates/bridges/deribit/src/execution/rest.rs`'s definite arm as a terminal **`OrderRejected`**. But by
//! the time `recv_until_id` sees the peer's `Close`, the request frame is already on the wire —
//! Deribit may have accepted the order before closing. So a socket close mid-submit told the core
//! an order was rejected while it was live at the venue: a stranded phantom position, which is
//! exactly what audit T1 exists to prevent, wired only to the timeout door. And per the recon
//! incident, `closed mid-request` is the COMMON first error on a dying socket, not a corner case.
//!
//! The rule now matches `vike_bridge_core::transport`'s own `read_body_ambiguous`, which the REST
//! venues have always followed: **the read half is ALWAYS ambiguous; only the send half is
//! definite.**
//!
//! ⚠ The assertion that matters most in this file is `fake.count("private/buy")`. Healing the
//! socket must never become re-sending the order: Deribit does not dedupe on `label`, so a second
//! `private/buy` is a SECOND REAL ORDER. Every test here asserts exactly one.

use std::assert_matches;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
use vike_deribit::reconcile::venue_read::read_over;
use vike_deribit::refusals::DeribitVenueReadRefusal;
use vike_model::events::Event;
use vike_model::{OrderRequest, VenueRead, VenueReadKind};

use crate::fake_deribit_websocket::{
    FakeDeribit, ORDER_ID, SYMBOL, Session, outcome, rejection, rest_against, stays_unknown,
    transport_against,
};

const ORDER_CLIENT_ID: &str = "vike-1";
const LOOKUP: &str = "private/get_order_state_by_label";

fn buy() -> OrderRequest {
    OrderRequest {
        order_client_id: ORDER_CLIENT_ID.to_string(),
        venue: "deribit".to_string(),
        symbol: crate::user(SYMBOL),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(50_000.0),
        timestamp: 1_700_000_000_000,
        ..Default::default()
    }
}

/// THE LIVE-MONEY REGRESSION. The venue receives `private/buy` and closes without answering; the
/// order IS live at the venue. We must NOT emit `OrderRejected` — we must re-dial, re-query, and
/// adopt the order the venue is holding.
///
/// Before the fix this produced `OrderRejected { reason: "closed mid-request" }` while the venue
/// held a live order.
#[test]
fn a_close_mid_submit_adopts_the_order_the_venue_actually_holds() {
    let fake = FakeDeribit::spawn(vec![Session::CloseBeforeAnswering, Session::OkForever]);
    let rest = rest_against(&fake);

    let event = outcome(rest.submit_order(&buy()));

    match event {
        Event::OrderAccepted(a) => {
            assert_eq!(a.order_client_id, ORDER_CLIENT_ID);
            assert_eq!(
                a.venue_order_id.as_ref().map(|v| v.as_str()),
                Some(ORDER_ID),
                "the venue id comes from the re-query, so the order is addressable"
            );
        }
        other => panic!("a live order must never be reported rejected: {other:?}"),
    }

    // The re-dial happened, and it happened BEFORE the re-query — a re-query on the dead socket
    // would have failed and left us guessing.
    assert_eq!(fake.connections(), 2, "the order socket was re-dialed once");
    assert_eq!(
        fake.methods(),
        vec!["private/buy".to_string(), "private/get_order_state_by_label".to_string()],
        "exactly one order, then a READ — never a second order"
    );
}

/// ⚠ THE SAFETY ASSERTION, stated on its own so it cannot be diluted by a refactor of the test
/// above: healing the socket is a RE-DIAL, never a RE-SEND. Deribit does not dedupe on `label`,
/// so a resent `private/buy` would be a second real order.
#[test]
fn the_order_is_never_re_sent_over_the_new_socket() {
    let fake = FakeDeribit::spawn(vec![Session::CloseBeforeAnswering, Session::OkForever]);
    let rest = rest_against(&fake);

    let _ = rest.submit_order(&buy());

    assert_eq!(fake.count("private/buy"), 1, "the order reached the venue exactly ONCE");
    assert_eq!(fake.count("private/sell"), 0);
    assert_eq!(
        fake.count("private/get_order_state_by_label"),
        1,
        "the recovery is a re-query — an idempotent READ"
    );
}

/// The other half of the re-query: the venue lists nothing under the label, asked twice. Read seconds
/// after the frame that proves nothing, so nothing is said and the core's stuck-order ladder ends the
/// order (`vike_bridge_core::resolve_ambiguous_submit` consumed verbatim).
#[test]
fn a_close_mid_submit_says_nothing_when_the_venue_lists_no_order() {
    let fake =
        FakeDeribit::spawn(vec![Session::CloseBeforeAnswering, Session::OkForeverOrderAbsent]);
    let rest = rest_against(&fake);

    stays_unknown(rest.submit_order(&buy()));
    assert_eq!(fake.count("private/buy"), 1, "still exactly one order");
    assert_eq!(fake.count(LOOKUP), 2, "an empty answer is asked once more");
    assert_eq!(fake.connections(), 2);
}

/// **The frame may still be on its way.** The socket closes just after the frame and the re-dial is
/// quick, so the first lookup answers `[]` while the venue is taking the order. One empty answer used
/// to reject it terminally; it is asked once more after about 2 s, and the order is adopted.
#[test]
fn an_empty_lookup_is_asked_once_more_before_it_may_reject() {
    let fake = FakeDeribit::spawn(vec![
        Session::CloseBeforeAnswering,
        Session::LabelAnswers(&["[]", r#"[{"order_id":"146062","label":"vike-1"}]"#]),
    ]);
    let rest = rest_against(&fake);

    let before = std::time::Instant::now();
    match outcome(rest.submit_order(&buy())) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("the order landed after the first ask: {other:?}"),
    }
    assert!(before.elapsed() >= std::time::Duration::from_secs(2), "the second ask waited");
    assert_eq!(fake.count(LOOKUP), 2);
    assert_eq!(fake.count("private/buy"), 1, "the ORDER is never re-sent");
}

/// **The age is asked again just before the frame**: a foreign instrument's row or a combo's registration runs in front
/// of it, and that time counts. The order is refused by name and the venue hears nothing; a close, which has no age, goes.
#[test]
fn an_order_past_its_wait_at_the_frame_is_refused_and_the_venue_hears_nothing() {
    use vike_bridge_core::exec_actor::{MAX_SUBMIT_WAIT, serving_submit};
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_against(&fake);
    let old = std::time::Instant::now() - MAX_SUBMIT_WAIT - std::time::Duration::from_secs(1);

    let why = rejection(outcome(serving_submit(old, || rest.submit_order(&buy()))));
    assert!(why.contains("NOT sent"), "{why}");
    assert_eq!(fake.count("private/buy"), 0);

    let close = OrderRequest {
        order_type: vike_model::OrderType::Market,
        price: None,
        reduce_only: true,
        ..buy()
    };
    assert_matches!(
        outcome(serving_submit(old, || rest.submit_order(&close))),
        Event::OrderAccepted(_)
    );
    assert_eq!(fake.count("private/buy"), 1, "a close has no age");
}

/// **The second ask is paid from the submit's hold.** With no room left for it, the one empty answer is
/// the order's unknown (nothing is said, the stuck-order ladder ends it), never a reject.
#[test]
fn with_no_room_for_the_second_ask_one_empty_answer_stays_unknown() {
    let script = Session::NoAnswerOn { method: "private/buy", absent: true };
    let fake = FakeDeribit::spawn(vec![script]);
    // Room for one reply's wait (11 s), none for the wait and the reply behind it (13 s).
    let limit = std::time::Duration::from_secs(12);
    let rest = rest_with(&fake, |t| t).with_submit_hold_limit(limit);

    stays_unknown(rest.submit_order(&buy()));
    assert_eq!(fake.count(LOOKUP), 1);
    assert_eq!(fake.count("private/buy"), 1, "the ORDER is never re-sent");
}

/// **A spent hold asks nothing and dials nothing** (the end of the 64 s path), the answer is the
/// unknown, and the socket left behind costs the next order nothing: its idle probe replaces it before
/// its frame exists. `ZERO` on the transport is "already idle", as in the idle-death test above.
#[test]
fn a_spent_hold_stays_unknown_and_the_next_order_still_rides_a_healed_socket() {
    let fake = FakeDeribit::spawn(vec![Session::CloseOnBuy, Session::OkForever]);
    let rest = rest_with(&fake, |t| t.with_idle_probe_after(std::time::Duration::ZERO))
        .with_submit_hold_limit(std::time::Duration::ZERO);

    stays_unknown(rest.submit_order(&buy()));
    assert_eq!(fake.connections(), 1, "no re-dial");
    assert_eq!(fake.count(LOOKUP), 0, "no lookup");

    match outcome(rest.submit_order(&buy())) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("the next order rides the healed socket: {other:?}"),
    }
    assert_eq!(fake.connections(), 2, "healed before the frame");
    assert_eq!(fake.count("private/buy"), 2, "two orders, each sent once");
}

/// A venue that is GONE says NOTHING, never a false terminal and never an accept — the re-dial
/// fails, the re-query fails with it, and `resolve_ambiguous_submit`'s `Err` arm applies. The order
/// may be live at a venue we cannot currently reach, or may never have landed there; it stays
/// `Submitted`, where the core's stuck-order ladder ends it.
#[test]
fn an_unreachable_venue_says_nothing_and_never_a_false_reject() {
    // ONE session: the venue takes the order, closes, and the script is exhausted — so the
    // re-dial `submit_order` fires next has nothing to connect to (the listener is dropped the
    // instant that session ends, and anything already queued behind it is reset).
    let fake = FakeDeribit::spawn(vec![Session::CloseBeforeAnswering]);
    let rest = rest_against(&fake);

    stays_unknown(rest.submit_order(&buy()));
    assert_eq!(fake.count("private/buy"), 1);
}

/// A DEFINITE failure still rejects. Nothing ever reached the wire (the transport holds no socket
/// at all), so there is no order to strand and a terminal is the correct, honest answer —
/// ambiguity must not be over-applied.
#[test]
fn a_pre_send_failure_is_still_a_definite_reject() {
    // Two sessions: the second exists so the heal below can actually be OBSERVED as a connection.
    let fake = FakeDeribit::spawn(vec![Session::OkForever, Session::OkForever]);
    let rest = rest_against(&fake);
    rest.detach(); // `call` now fails with WEBSOCKET_NOT_CONNECTED, code 0 — nothing sent

    match outcome(rest.submit_order(&buy())) {
        Event::OrderRejected(_) => {}
        other => panic!("a pre-send failure is definite and must reject: {other:?}"),
    }
    assert_eq!(fake.count("private/buy"), 0, "nothing reached the venue");
    assert_eq!(
        fake.count("private/get_order_state_by_label"),
        0,
        "no re-query: there is nothing ambiguous to resolve"
    );
    // ...but the socket was still healed, so the NEXT order is not doomed by this one.
    assert_eq!(fake.connections(), 2, "the dead transport was re-dialed anyway");
}

/// An order adopted through the re-query must be CANCELLABLE. `cancel_order` resolves
/// order client id → venue order id through `order_ids` and returns `Ok(())` on a miss (the crate's
/// documented "silent success" trap), so an adoption that forgot to record the id would leave a
/// live order that silently ignores every cancel.
#[test]
fn an_adopted_order_is_cancellable_afterwards() {
    let fake = FakeDeribit::spawn(vec![Session::CloseBeforeAnswering, Session::OkForever]);
    let rest = rest_against(&fake);

    let _ = rest.submit_order(&buy());
    rest.cancel_order(ORDER_CLIENT_ID).expect("cancel of an adopted order");

    assert_eq!(fake.count("private/cancel"), 1, "the cancel actually reached the venue");
}

/// CANCEL over a dead socket: re-dial and re-send ONCE. Unlike a submit this is safe by
/// construction — `private/cancel` names one immutable venue `order_id`, so a re-send can only act
/// on that order, and an already-cancelled order comes back in the swallowed `NOT_FOUND` set.
/// Leaving a live order un-cancelled on a venue the process can reach again is the wrong direction
/// for a call whose whole purpose is removing risk.
#[test]
fn a_cancel_over_a_dead_socket_re_dials_and_completes() {
    let fake = FakeDeribit::spawn(vec![Session::OkThenClose(1), Session::OkForever]);
    let rest = rest_against(&fake);

    // The submit is served (so `order_ids` knows the venue id); the venue then closes.
    let _ = rest.submit_order(&buy());
    assert_eq!(fake.connections(), 1);

    rest.cancel_order(ORDER_CLIENT_ID).expect("the cancel survives a venue-side close");

    assert_eq!(fake.connections(), 2, "the order socket was re-dialed for the cancel");
    assert_eq!(fake.count("private/buy"), 1, "the ORDER is still never re-sent");
}

/// A cancel for an order THIS client never submitted -- the restart shape: `reregister_orders`
/// seats the order in the engine, but the fresh `VenueRestDeribit`'s order client id -> order_id map is empty --
/// finds the order BY LABEL (`private/get_order_state_by_label`, the label being our order client id) and
/// cancels it by the id the venue reports. Before this the cancel was a silent `Ok(())` and the
/// venue kept the order live.
#[test]
fn a_cancel_for_an_order_this_client_never_submitted_finds_it_by_label() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_against(&fake);

    rest.cancel_order("vtrestore1").expect("cancel of a restored order");

    assert_eq!(
        fake.methods(),
        vec!["private/get_order_state_by_label".to_string(), "private/cancel".to_string()],
        "one label READ, then one cancel by the id it returned"
    );
    let lookup = fake.params_of("private/get_order_state_by_label");
    assert_eq!(
        lookup[0]["label"], "vtrestore1",
        "the lookup carries the order client id as the label"
    );
    let cancel = fake.params_of("private/cancel");
    assert_eq!(cancel[0]["order_id"], ORDER_ID, "the cancel names the venue's id, not a label");
    assert_eq!(fake.connections(), 1);
}

/// The fallback is for a map MISS only: an order this client submitted is cancelled by the id it
/// remembers, with no lookup frame at all.
#[test]
fn a_cancel_for_an_order_this_client_submitted_never_looks_up_the_label() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_against(&fake);

    let _ = rest.submit_order(&buy());
    rest.cancel_order(ORDER_CLIENT_ID).expect("cancel of a submitted order");

    assert_eq!(fake.methods(), vec!["private/buy".to_string(), "private/cancel".to_string()]);
    assert_eq!(fake.count("private/get_order_state_by_label"), 0, "the map hit needs no lookup");
}

/// ⚠ An EMPTY order client id must never reach the label lookup: an empty `label` is no filter, and the first
/// order of the currency would be cancelled in the unknown order's place. It is refused, loudly (an
/// `Err` the `ExecutionClientRest` turns into a non-terminal `OrderCancelRejected`), with no frame sent.
#[test]
fn an_empty_order_client_id_is_refused_and_touches_nothing() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_against(&fake);

    assert!(rest.cancel_order("").is_err(), "an empty label is refused");
    assert!(rest.cancel_order("  ").is_err(), "so is a blank one");

    assert!(fake.methods().is_empty(), "no lookup, no cancel: {:?}", fake.methods());
}

/// **A submit answered with a reply naming no order takes the lost answer's path.** It used to read as accepted under
/// an EMPTY venue id. The venue holds it: accepted, with the id the label lookup found, and the order sent once.
#[test]
fn a_submit_reply_naming_no_order_takes_its_id_from_the_label_lookup() {
    let script = Session::NoAnswerOn { method: "private/buy", absent: false };
    let fake = FakeDeribit::spawn(vec![script]);
    let rest = rest_against(&fake);
    match outcome(rest.submit_order(&buy())) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("the venue holds it: {other:?}"),
    }
    assert_eq!(fake.count("private/buy"), 1, "the ORDER is never re-sent");
    assert_eq!(fake.count("private/get_order_state_by_label"), 1);
}

/// ...the venue lists no such order: nothing is said, and the stuck-order ladder ends it, so nothing
/// silently vanishes.
#[test]
fn a_submit_reply_naming_no_order_the_venue_does_not_list_says_nothing() {
    let script = Session::NoAnswerOn { method: "private/buy", absent: true };
    let fake = FakeDeribit::spawn(vec![script]);
    let rest = rest_against(&fake);
    stays_unknown(rest.submit_order(&buy()));
    assert_eq!(fake.count("private/buy"), 1, "the ORDER is never re-sent");
}

/// ...and the lookup says nothing either: the lost answer's own unknown (the stuck-order ladder ends it),
/// never a terminal and never an id that is the empty string.
#[test]
fn a_submit_reply_naming_no_order_whose_lookup_fails_stays_unknown() {
    let fake = FakeDeribit::spawn(vec![Session::NoAnswerForever]);
    let rest = rest_against(&fake);
    stays_unknown(rest.submit_order(&buy()));
    assert_eq!(fake.count("private/buy"), 1, "the ORDER is never re-sent");
}

/// A reply with neither a result nor an error is NO answer, whoever asked: not "the venue holds no such label" (which
/// would let a cancel report success and an ambiguous submit be rejected), not an empty history, not "nothing rests".
#[test]
fn a_reply_with_neither_a_result_nor_an_error_is_no_answer_for_any_caller() {
    let fake = FakeDeribit::spawn(vec![Session::NoAnswerForever]);
    let rest = rest_against(&fake);
    assert!(rest.cancel_order(ORDER_CLIENT_ID).is_err(), "the label lookup was not answered");
    assert_eq!(fake.count("private/cancel"), 0, "and nothing was cancelled");
    assert!(rest.get_user_trades(10).is_err());
    assert!(rest.get_order_history(10).is_err());
    // (`VenueRestDeribit::connect`, the legacy snapshot an arm never read, is gone; the position and
    // open-order reads below are the ones that must not read this as a flat account.)
    for kind in [VenueReadKind::Balance, VenueReadKind::OpenOrders, VenueReadKind::Positions] {
        let why = read_over(&rest, kind).unwrap_err();
        assert!(
            matches!(why, DeribitVenueReadRefusal::Unread { code: E_TIMEOUT_AMBIGUOUS, .. }),
            "an unanswered read is the ambiguous one: {why}"
        );
    }
}

/// ...and a REAL empty answer still is one: `[]`, or `{trades: []}`, reads as nothing there for every caller.
#[test]
fn a_real_empty_answer_is_still_an_answer() {
    let fake = FakeDeribit::spawn(vec![Session::EmptyBook]);
    let rest = rest_against(&fake);
    rest.cancel_order(ORDER_CLIENT_ID).expect("the venue holds nothing under the label");
    assert_eq!(fake.count("private/cancel"), 0);
    assert_eq!(rest.get_user_trades(10).unwrap(), serde_json::json!([]));
    assert_eq!(rest.get_order_history(10).unwrap(), serde_json::json!([]));
    assert_eq!(
        read_over(&rest, VenueReadKind::OpenOrders).unwrap(),
        VenueRead::OpenOrders { orders: Vec::new(), truncated: false }
    );
    assert_eq!(
        read_over(&rest, VenueReadKind::Positions).unwrap(),
        VenueRead::Positions { positions: Vec::new(), truncated: false }
    );
}

/// The venue confirming it holds no order under the label is the old "nothing live to cancel"
/// answer: `Ok(())`, and no cancel frame for an id that does not exist.
#[test]
fn a_cancel_for_a_label_the_venue_does_not_hold_is_a_no_op() {
    let fake = FakeDeribit::spawn(vec![Session::OkForeverOrderAbsent]);
    let rest = rest_against(&fake);

    rest.cancel_order("never-submitted").expect("an absent order is not an error");

    assert_eq!(fake.methods(), vec!["private/get_order_state_by_label".to_string()]);
}

/// A FAILED lookup must not read as "nothing to cancel": the venue answered with a refusal, so the
/// order may well be live, and the cancel reports the failure instead of claiming success.
#[test]
fn a_failed_label_lookup_surfaces_instead_of_claiming_success() {
    let fake = FakeDeribit::spawn(vec![Session::VenueErrorForever]);
    let rest = rest_against(&fake);

    let err = rest.cancel_order("vtrestore1").expect_err("a refused lookup is an error");

    assert_eq!(err.code, 10009, "the venue's own refusal rides through");
    assert_eq!(fake.count("private/cancel"), 0, "no id, so nothing was cancelled");
}

/// **A cancel the venue's error leaves open is not a refused cancel.** `private/cancel` answered 13888 or a code of
/// the server's trouble may have cancelled the order, so the error carries the sentinel and says the outcome is
/// unknown (`crates/bridges/deribit/src/execution/rest.rs`'s `cancel_failed`); it used to read as the venue
/// refusing the cancel. A definite refusal keeps the venue's code and words. Neither re-sends the cancel.
#[test]
fn a_cancel_the_venue_leaves_open_is_unknown_and_a_refused_one_keeps_its_words() {
    for (code, message, open) in [
        (11094, "internal_server_error", true),
        (13888, "timed_out", true),
        (10025, "invalid_order_id", false),
    ] {
        let fake = FakeDeribit::spawn(vec![Session::VenueErrorOn {
            method: "private/cancel",
            code,
            message,
            absent: false,
        }]);
        let rest = rest_against(&fake);
        let err = rest.cancel_order(ORDER_CLIENT_ID).expect_err("the venue did not cancel it");
        if open {
            assert_eq!(err.code, E_TIMEOUT_AMBIGUOUS, "{code}");
            // The shared cancel answer words the unknown outcome; the venue's own words stay.
            assert_eq!(err.msg, message);
        } else {
            assert_eq!((err.code, err.msg.as_str()), (code, message));
        }
        assert_eq!(fake.count("private/cancel"), 1, "{code}: one cancel frame");
    }
}

/// The lookup is a READ, so over a dead socket it re-dials and re-asks once (the same licence the
/// recon client has), and the cancel then goes out on the healed socket.
#[test]
fn the_label_lookup_re_dials_a_dead_socket_and_completes() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever, Session::OkForever]);
    let rest = rest_against(&fake);
    rest.detach(); // `call` now fails with WEBSOCKET_NOT_CONNECTED, code 0 -- nothing sent

    rest.cancel_order("vtrestore1").expect("lookup and cancel survive a dead socket");

    assert_eq!(fake.connections(), 2, "the order socket was re-dialed for the lookup");
    assert_eq!(
        fake.methods(),
        vec!["private/get_order_state_by_label".to_string(), "private/cancel".to_string()]
    );
}

/// A VENUE refusal of a submit is still a plain terminal reject: the socket answered, so there is
/// no ambiguity, no re-dial and no re-query.
#[test]
fn a_venue_refusal_rejects_without_re_dialing() {
    let fake = FakeDeribit::spawn(vec![Session::VenueErrorForever, Session::VenueErrorForever]);
    let rest = rest_against(&fake);

    match outcome(rest.submit_order(&buy())) {
        Event::OrderRejected(r) => {
            assert!(r.reason.as_str().contains("not_enough_funds"), "{:?}", r.reason)
        }
        other => panic!("a venue refusal is a definite reject: {other:?}"),
    }
    assert_eq!(fake.connections(), 1, "a live socket that answered is never re-dialed");
    assert_eq!(fake.count("private/get_order_state_by_label"), 0, "nothing to re-query");
}

/// **A lookup that names no order is no answer, not an absence.** Something other than a list, or a row
/// with no order id, used to read as "the venue never took it", a terminal reject (and a `null` id as an
/// order called `null`). The order stays unknown, and a cancel says it could not find out instead of
/// reporting nothing to cancel.
#[test]
fn a_label_answer_naming_no_order_is_no_answer_for_a_submit_or_a_cancel() {
    let answers: [&'static [&'static str]; 4] = [
        &[r#"{"orders":[]}"#],
        &[r#"[{"label":"vike-1","order_state":"open"}]"#],
        &[r#"[{"label":"vike-1","order_id":null}]"#],
        &[r#"[{"label":"vike-1","order_id":""}]"#],
    ];
    for answer in answers {
        let fake =
            FakeDeribit::spawn(vec![Session::CloseBeforeAnswering, Session::LabelAnswers(answer)]);
        let rest = rest_against(&fake);
        stays_unknown(rest.submit_order(&buy()));
        assert_eq!(fake.count(LOOKUP), 1, "{answer:?}: no answer is not asked for again");

        let fake = FakeDeribit::spawn(vec![Session::LabelAnswers(answer)]);
        let rest = rest_against(&fake);
        let err = rest.cancel_order("vtrestore1").expect_err("the lookup was not answered");
        // No cancel left: NOT SENT, never a cancel of unknown outcome (decision 0303).
        assert_eq!(err.code, 0, "{answer:?}");
        assert_eq!(fake.count("private/cancel"), 0, "{answer:?}");
    }
}

/// **An explicit client id reused after a restart.** The venue lists an OLD order under the label,
/// created 1000 s before `buy()`'s stamp and long gone. It is not this order, so the order is not
/// accepted under its dead id; with nothing else there, the venue holds no such order.
#[test]
fn an_old_order_under_a_reused_client_id_is_not_adopted() {
    let fake = FakeDeribit::spawn(vec![
        Session::CloseBeforeAnswering,
        Session::LabelAnswers(&[
            r#"[{"order_id":"999","label":"vike-1","order_state":"cancelled","creation_timestamp":1699999000000}]"#,
        ]),
    ]);
    let rest = rest_against(&fake);
    stays_unknown(rest.submit_order(&buy()));
    assert_eq!(fake.count(LOOKUP), 2);
}

/// ...and the new order listed behind it is found.
#[test]
fn the_new_order_is_found_past_an_old_one_under_the_same_id() {
    let fake = FakeDeribit::spawn(vec![
        Session::CloseBeforeAnswering,
        Session::LabelAnswers(&[concat!(
            r#"[{"order_id":"999","label":"vike-1","creation_timestamp":1699999000000},"#,
            r#"{"order_id":"146062","label":"vike-1","creation_timestamp":1700000000005}]"#,
        )]),
    ]);
    let rest = rest_against(&fake);
    match outcome(rest.submit_order(&buy())) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("the new order stands: {other:?}"),
    }
}

/// A row a few seconds older than the order's stamp IS the order: the stamp is this host's clock, the
/// row's the venue's, and a drifted clock must not turn a landed order into a terminal reject.
#[test]
fn a_row_a_clock_drift_older_than_the_order_is_still_the_order() {
    let fake = FakeDeribit::spawn(vec![
        Session::CloseBeforeAnswering,
        Session::LabelAnswers(&[
            r#"[{"order_id":"146062","label":"vike-1","creation_timestamp":1699999990000}]"#,
        ]),
    ]);
    let rest = rest_against(&fake);
    match outcome(rest.submit_order(&buy())) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("10 s of drift is not an absence: {other:?}"),
    }
}

/// Two rows inside that allowance: the newest is the order, whichever the venue lists first.
#[test]
fn the_newest_row_inside_the_allowance_is_the_order() {
    let fake = FakeDeribit::spawn(vec![
        Session::CloseBeforeAnswering,
        Session::LabelAnswers(&[concat!(
            r#"[{"order_id":"999","label":"vike-1","creation_timestamp":1699999990000},"#,
            r#"{"order_id":"146062","label":"vike-1","creation_timestamp":1700000000005}]"#,
        )]),
    ]);
    let rest = rest_against(&fake);
    match outcome(rest.submit_order(&buy())) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("the new order stands: {other:?}"),
    }
}

/// **A cancel under a reused client id.** The submit's answer stayed unknown, so the cancel asks by
/// label, and the venue lists only an OLD order under the id (here still open). It is not this order, so
/// it is not cancelled in this one's place; there is nothing of this order's to cancel.
#[test]
fn a_cancel_does_not_take_an_old_order_under_a_reused_client_id_for_this_one() {
    let fake = FakeDeribit::spawn(vec![
        Session::CloseBeforeAnswering,
        Session::LabelAnswers(&[
            r#"{"orders":[]}"#,
            r#"[{"order_id":"999","label":"vike-1","order_state":"open","creation_timestamp":1699999000000}]"#,
        ]),
    ]);
    let rest = rest_against(&fake);
    stays_unknown(rest.submit_order(&buy()));

    rest.cancel_order(ORDER_CLIENT_ID).expect("the venue lists nothing of this order's");
    assert_eq!(fake.count("private/cancel"), 0, "the old order is not cancelled in its place");
}

/// ...and the same 60 s allowance as a submit's: 10 s of clock drift is still this order.
#[test]
fn a_cancel_takes_a_row_a_clock_drift_older_than_the_order() {
    let fake = FakeDeribit::spawn(vec![
        Session::CloseBeforeAnswering,
        Session::LabelAnswers(&[
            r#"{"orders":[]}"#,
            r#"[{"order_id":"146062","label":"vike-1","creation_timestamp":1699999990000}]"#,
        ]),
    ]);
    let rest = rest_against(&fake);
    let _ = rest.submit_order(&buy());

    rest.cancel_order(ORDER_CLIENT_ID).expect("the order is found");
    assert_eq!(fake.params_of("private/cancel")[0]["order_id"], ORDER_ID);
}

/// **An order this client never sent has no stamp here** (a restart's adopted order: the seams carry the
/// order client id and the symbol), so it is told by the newest row under its id, never the first one listed.
#[test]
fn a_cancel_of_an_order_this_client_never_sent_takes_the_newest_row_under_its_id() {
    let fake = FakeDeribit::spawn(vec![Session::LabelAnswers(&[concat!(
        r#"[{"order_id":"999","label":"vike-1","creation_timestamp":1699999000000},"#,
        r#"{"order_id":"146062","label":"vike-1","creation_timestamp":1700000000005}]"#,
    )])]);
    let rest = rest_against(&fake);

    rest.cancel_order(ORDER_CLIENT_ID).expect("the order is found");
    assert_eq!(fake.params_of("private/cancel")[0]["order_id"], ORDER_ID);
}

/// **A venue error that leaves the order's fate open is not a refusal.** The venue RECEIVED the frame and
/// answered 13888, a code of the server's trouble (`crates/bridges/deribit/src/transport/error_codes.rs`'s
/// `TAXONOMY`: its `Timeout` row and its `server_had_trouble`) or a code its page does not name (unmapped), which
/// says nothing about whether the order stands. It used to be a terminal reject (10001, 11031 and 13008 until S4b,
/// an unmapped code until the S4b flip). The venue holds it: adopted, and the order sent once.
#[test]
fn a_venue_timeout_or_internal_error_adopts_the_order_the_venue_holds() {
    for (code, message) in [
        (13888, "timed_out"),
        (11094, "internal_server_error"),
        (13028, "temporarily_unavailable"),
        (10001, "error"),
        (11031, "other_error"),
        (13008, "request_failed"),
        (19999, "some_new_word"),
    ] {
        let method = "private/buy";
        let fake = FakeDeribit::spawn(vec![Session::VenueErrorOn {
            method,
            code,
            message,
            absent: false,
        }]);
        let rest = rest_against(&fake);
        match outcome(rest.submit_order(&buy())) {
            Event::OrderAccepted(a) => {
                assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID), "{code}");
            }
            other => panic!("{code} says nothing against the order: {other:?}"),
        }
        assert_eq!(fake.count("private/buy"), 1, "{code}: the ORDER is never re-sent");
        assert_eq!(fake.count(LOOKUP), 1, "{code}");
        assert_eq!(fake.connections(), 1, "{code}: a socket that answered is not re-dialed");
    }
}

/// ...and the venue listing nothing, asked twice, says nothing: the stuck-order ladder ends the order.
#[test]
fn a_venue_timeout_the_venue_does_not_list_says_nothing() {
    let fake = FakeDeribit::spawn(vec![Session::VenueErrorOn {
        method: "private/buy",
        code: 13888,
        message: "timed_out",
        absent: true,
    }]);
    let rest = rest_against(&fake);
    stays_unknown(rest.submit_order(&buy()));
    assert_eq!(fake.count(LOOKUP), 2);
}

/// A clear refusal (a row of the table) stays the plain reject carrying the venue's word: nothing is asked.
#[test]
fn a_clear_refusal_stays_a_reject() {
    for (code, message) in [(10009, "not_enough_funds"), (-32602, "Invalid params")] {
        let method = "private/buy";
        let fake = FakeDeribit::spawn(vec![Session::VenueErrorOn {
            method,
            code,
            message,
            absent: false,
        }]);
        let rest = rest_against(&fake);
        assert_eq!(rejection(outcome(rest.submit_order(&buy()))), message, "{code}");
        assert_eq!(fake.count(LOOKUP), 0, "{code}: nothing to ask");
    }
}

/// THE FIRST ORDER AFTER AN IDLE PERIOD (the CI box drill 2026-10-16). The socket died while idle; the
/// old transport's first write "succeeded" into it, the read failed, and the re-query then
/// (truthfully) confirmed the order absent: a REJECT of an order that was never sent. Now the idle
/// socket is probed with `public/test` BEFORE the order frame exists, fails, is re-dialed, and the
/// order rides the live socket: accepted, exactly one `private/buy`, no re-query.
#[test]
fn the_first_order_after_an_idle_death_rides_a_healed_socket() {
    let fake = FakeDeribit::spawn(vec![Session::CloseAfterAuth, Session::OkForever]);
    // `ZERO` = "already idle": a deterministic stand-in for the real 5 s of silence.
    let transport = crate::fake_deribit_websocket::transport_against(&fake)
        .with_idle_probe_after(std::time::Duration::ZERO);
    let rest = vike_deribit::execution::rest::VenueRestDeribit::new(
        transport,
        SYMBOL,
        vike_model::SymbolProperties::default(),
        "BTC",
    );

    match outcome(rest.submit_order(&buy())) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("the first order after an idle death must not be lost: {other:?}"),
    }
    assert_eq!(fake.connections(), 2, "the stale socket was replaced before the send");
    assert_eq!(
        fake.methods(),
        vec!["private/buy".to_string()],
        "the probe died on the old socket; the ONE order rode the new one; no re-query needed"
    );
}

fn rest_with(
    fake: &FakeDeribit,
    shape: impl FnOnce(
        vike_deribit::transport::client::DeribitTransport,
    ) -> vike_deribit::transport::client::DeribitTransport,
) -> vike_deribit::execution::rest::VenueRestDeribit {
    vike_deribit::execution::rest::VenueRestDeribit::new(
        shape(crate::fake_deribit_websocket::transport_against(fake)),
        SYMBOL,
        vike_model::SymbolProperties::default(),
        "BTC",
    )
}

/// THE EXPIRED TOKEN, caught BEFORE the send. `public/test` (the idle probe) does not check
/// authentication, and the order socket never refreshes its ~900 s token: a socket older than
/// `with_auth_max_age` is re-authenticated in place, so the venue never sees a 13009 at all.
#[test]
fn an_old_token_is_reauthenticated_before_the_order_is_sent() {
    let fake = FakeDeribit::spawn(vec![Session::TokenExpired]);
    // `ZERO` = "the token is already old"; idle probing stays at its 5 s default.
    let rest = rest_with(&fake, |t| t.with_auth_max_age(std::time::Duration::ZERO));

    assert_matches!(outcome(rest.submit_order(&buy())), Event::OrderAccepted(_));
    assert_eq!(fake.auths(), 2, "connect + the in-place re-auth, on the SAME socket");
    assert_eq!(fake.connections(), 1, "re-authenticated in place, not re-dialed");
    assert_eq!(fake.methods(), vec!["private/buy".to_string()], "the refusal never happened");
}

/// THE TOKEN EXPIRING BETWEEN the age check and the frame: the venue answers 13009. Whether it
/// executed the frame first is NOT known (the docs say only "Wrong or expired authorization token
/// or bad signature", and a label is not unique), so the order is NEVER re-sent: the socket is
/// re-authenticated in place and the submit resolves BY LABEL, like a lost reply.
#[test]
fn a_13009_on_an_order_frame_reauthenticates_but_never_resends_it() {
    let fake = FakeDeribit::spawn(vec![Session::TokenExpired]);
    let rest = rest_against(&fake);

    // The label lookup finds the order the fake holds, so the ambiguous submit is adopted.
    match outcome(rest.submit_order(&buy())) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("an unconfirmed order must resolve on the venue's evidence: {other:?}"),
    }
    assert_eq!(
        fake.auths(),
        2,
        "connect + the in-place re-auth, so the lookup rides a valid token"
    );
    assert_eq!(fake.connections(), 1);
    assert_eq!(fake.count("private/buy"), 1, "the order frame is written exactly ONCE");
    assert_eq!(
        fake.methods(),
        vec!["private/buy".to_string(), "private/get_order_state_by_label".to_string()],
        "refused with 13009, then a READ by label -- never a second order"
    );
}

/// A READ is the one thing a 13009 may re-send: a second copy of `private/get_*` changes nothing.
#[test]
fn a_13009_on_a_read_reauthenticates_and_asks_again() {
    let fake = FakeDeribit::spawn(vec![Session::TokenExpired]);
    let mut transport = transport_against(&fake);

    let reply = transport.call("private/get_positions", &serde_json::json!({})).expect("reply");
    assert!(reply.get("error").is_none_or(|e| e.is_null()), "{reply}");
    assert_eq!(fake.auths(), 2);
    assert_eq!(
        fake.methods(),
        vec!["private/get_positions".to_string(), "private/get_positions".to_string()]
    );
}

/// **A dead socket is a variant of the call, never its words** (`CallError::DeadSocket`): the peer closing
/// under a request (the frame was on the wire, so `E_TIMEOUT_AMBIGUOUS`) and a transport with no socket
/// (nothing written, `code: 0`) are dead sockets; the venue's own refusal and a reply stating nothing are
/// `CallError::Failed`, the socket alive.
#[test]
fn a_dead_socket_is_a_variant_of_the_call_and_a_venue_answer_is_not() {
    let fake = FakeDeribit::spawn(vec![Session::CloseBeforeAnswering]);
    let mut transport = transport_against(&fake);
    let closed = transport
        .call("private/get_positions", &serde_json::json!({}))
        .expect_err("closed under the request");
    assert!(closed.is_dead_socket(), "{closed:?}");
    assert_eq!(closed.error().code, E_TIMEOUT_AMBIGUOUS, "the frame was on the wire: {closed:?}");
    transport.close();
    let none =
        transport.call("private/get_positions", &serde_json::json!({})).expect_err("no socket");
    assert!(none.is_dead_socket() && none.error().code == 0, "nothing was written: {none:?}");

    for session in [Session::VenueErrorForever, Session::NoAnswerForever] {
        let fake = FakeDeribit::spawn(vec![session]);
        let rest = rest_against(&fake);
        let failed = rest.get_order_history(10).expect_err("no history");
        assert!(!failed.is_dead_socket(), "the venue answered, the socket lives: {failed:?}");
    }
}

/// The heal's re-dial fails (the venue is gone): a DEFINITE reject, whose reason says the request
/// was never sent and the re-dial failed -- not the dead-socket "send:" wording -- and nothing
/// reached the wire.
#[test]
fn a_failed_heal_redial_is_a_definite_reject_with_an_accurate_reason() {
    let fake = FakeDeribit::spawn(vec![Session::CloseAfterAuth]);
    let rest = rest_with(&fake, |t| t.with_idle_probe_after(std::time::Duration::ZERO));
    fake.await_listener_closed();

    match outcome(rest.submit_order(&buy())) {
        Event::OrderRejected(r) => {
            let reason = r.reason.to_string();
            assert!(
                reason.starts_with("request not sent: stale order-WS re-dial failed"),
                "{reason}"
            );
        }
        other => panic!("a heal that cannot re-dial must reject definitively: {other:?}"),
    }
    assert!(fake.methods().is_empty(), "no frame was ever written");
}

/// The one answer `run_loop` gave a cancel: its refusal id and its words.
fn cancel_answer(rest: &impl VenueRest) -> (String, String) {
    match vike_bridge_core::execution_drive::drive_cancel(rest, ORDER_CLIENT_ID).as_slice() {
        [Event::OrderCancelRejected(r)] => {
            (r.refusal_id.as_deref().unwrap_or_default().to_string(), r.reason.to_string())
        }
        other => panic!("expected one OrderCancelRejected, got {other:?}"),
    }
}

/// **A cancel frame whose reply stated nothing is OUTCOME UNKNOWN through the shared loop, never a refused cancel**
/// (decision 0303): it used to read as the venue refusing the cancel. The cancel is sent once; a clear refusal keeps
/// its words and its id.
#[test]
fn a_cancel_whose_reply_was_lost_is_of_unknown_outcome_and_sent_once() {
    let fake =
        FakeDeribit::spawn(vec![Session::NoAnswerOn { method: "private/cancel", absent: false }]);
    let (id, reason) = cancel_answer(&*rest_against(&fake));
    assert_eq!(id, vike_model::OrderRefusalId::OutcomeUnknown.word(), "{reason}");
    assert!(reason.starts_with("cancel outcome unknown"), "{reason}");
    assert_eq!(fake.count("private/cancel"), 1, "the cancel is never sent again");

    let fake = FakeDeribit::spawn(vec![Session::VenueErrorOn {
        method: "private/cancel",
        code: 10009,
        message: "not_enough_funds",
        absent: false,
    }]);
    let (id, reason) = cancel_answer(&*rest_against(&fake));
    assert_eq!(id, vike_model::OrderRefusalId::VenueRefused.word());
    assert_eq!(reason, "not_enough_funds");
}

/// A label lookup that was not answered sent no cancel: NOT SENT, never a cancel of unknown outcome.
#[test]
fn a_cancel_whose_label_lookup_was_not_answered_is_not_sent() {
    let fake = FakeDeribit::spawn(vec![Session::NoAnswerOn { method: LOOKUP, absent: false }]);
    let (id, _) = cancel_answer(&*rest_against(&fake));
    assert_eq!(id, vike_model::OrderRefusalId::NotSent.word());
    assert_eq!(fake.count("private/cancel"), 0);
}

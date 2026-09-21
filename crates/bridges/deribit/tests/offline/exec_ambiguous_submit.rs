//! The deribit ORDER socket dying MID-SUBMIT — audit T1, over the local WebSocket stand-in in
//! `crate::fake_deribit_ws`. No venue, no credentials, no `#[ignore]`.
//!
//! **The defect these pin is live-money.** `crates/bridges/deribit/src/transport.rs`'s
//! `DeribitOrderTransport::call` marked only an exact response TIMEOUT as
//! `E_TIMEOUT_AMBIGUOUS`; `closed mid-request` fell through with `code: 0` and reached
//! `crates/bridges/deribit/src/client.rs`'s definite arm as a terminal **`OrderRejected`**. But by
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

use vike_bridge_core::rest::VenueRest;
use vike_model::OrderRequest;
use vike_model::events::Event;

use crate::fake_deribit_ws::{FakeDeribit, ORDER_ID, SYMBOL, Session, rest_against};

const COID: &str = "vike-1";

fn buy() -> OrderRequest {
    OrderRequest {
        client_order_id: COID.to_string(),
        venue: "deribit".to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 10.0,
        order_type: "limit".to_string(),
        price: Some(50_000.0),
        ts: 1_700_000_000_000,
        ..Default::default()
    }
}

/// `[OrderSubmitted, X]` — the emitter split; returns X.
fn outcome(events: Vec<Event>) -> Event {
    assert_eq!(events.len(), 2, "the emitter split is [OrderSubmitted, terminal-or-accepted]");
    assert!(matches!(events[0], Event::OrderSubmitted(_)), "first event is always OrderSubmitted");
    events.into_iter().nth(1).expect("second event")
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
            assert_eq!(a.client_order_id, COID);
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

/// The other half of the re-query: the venue confirms it never took the order, so the reject is
/// TRUE and must still be emitted. Ambiguity is resolved on evidence, not by always assuming the
/// order landed — that would strand a phantom in the opposite direction.
#[test]
fn a_close_mid_submit_still_rejects_when_the_venue_confirms_the_order_absent() {
    let fake =
        FakeDeribit::spawn(vec![Session::CloseBeforeAnswering, Session::OkForeverOrderAbsent]);
    let rest = rest_against(&fake);

    match outcome(rest.submit_order(&buy())) {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, COID);
            // The reason is `vike_bridge_core::resolve_ambiguous_submit`'s, consumed verbatim.
            assert!(r.reason.as_str().contains("venue confirms order absent"), "{:?}", r.reason);
        }
        other => panic!("an order the venue never took must reject: {other:?}"),
    }
    assert_eq!(fake.count("private/buy"), 1, "still exactly one order");
    assert_eq!(fake.connections(), 2);
}

/// A venue that is GONE resolves to an OPTIMISTIC accept, never a false terminal — the re-dial
/// fails, the re-query fails with it, and `resolve_ambiguous_submit`'s `Err` arm applies. The
/// order may be live at a venue we cannot currently reach; reconcile is what settles it later.
#[test]
fn an_unreachable_venue_resolves_optimistically_never_to_a_false_reject() {
    // ONE session: the venue takes the order, closes, and the script is exhausted — so the
    // re-dial `submit_order` fires next has nothing to connect to (the listener is dropped the
    // instant that session ends, and anything already queued behind it is reset).
    let fake = FakeDeribit::spawn(vec![Session::CloseBeforeAnswering]);
    let rest = rest_against(&fake);

    match outcome(rest.submit_order(&buy())) {
        Event::OrderAccepted(a) => assert!(
            a.venue_order_id.is_none(),
            "no evidence of a venue id — optimistic, not a claim"
        ),
        other => panic!("an unreachable venue must not produce a terminal: {other:?}"),
    }
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
    rest.detach(); // `call` now fails with WS_NOT_CONNECTED, code 0 — nothing sent

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
/// coid → venue order id through `order_ids` and returns `Ok(())` on a miss (the crate's
/// documented "silent success" trap), so an adoption that forgot to record the id would leave a
/// live order that silently ignores every cancel.
#[test]
fn an_adopted_order_is_cancellable_afterwards() {
    let fake = FakeDeribit::spawn(vec![Session::CloseBeforeAnswering, Session::OkForever]);
    let rest = rest_against(&fake);

    let _ = rest.submit_order(&buy());
    rest.cancel_order(COID).expect("cancel of an adopted order");

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

    rest.cancel_order(COID).expect("the cancel survives a venue-side close");

    assert_eq!(fake.connections(), 2, "the order socket was re-dialed for the cancel");
    assert_eq!(fake.count("private/buy"), 1, "the ORDER is still never re-sent");
}

/// A cancel for an order this process never submitted stays a no-op that touches no socket — the
/// documented behaviour, and proof the re-dial did not turn every cancel into a dial.
#[test]
fn an_unknown_cancel_touches_nothing() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_against(&fake);

    rest.cancel_order("never-submitted").expect("unknown cancel is Ok");

    assert!(fake.methods().is_empty(), "no frame was sent");
    assert_eq!(fake.connections(), 1);
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

//! A crossing edit of a resting post-only order: `private/edit` under `reject_post_only` may END the order
//! (`cancel_reason` `edit_post_only_reject`), and how the venue answers is unmeasured
//! (`tests/deribit_post_only_smoke.rs` records it), so each answer here is scripted and the event must
//! state what the order IS. An `OrderModifyRejected` tells the engine it keeps its terms; said of a
//! cancelled limit it rests in the registry until a reconcile pass, while the venue holds nothing.

use vike_bridge_core::rest::VenueRest;
use vike_deribit::client::DeribitRest;
use vike_model::events::Event;
use vike_model::{
    ManagedOrder, OrderRequest, OrderStatus, OrderType, SymbolProperties, TimeInForce,
};

use crate::fake_deribit_ws::{FakeDeribit, SYMBOL, Session, transport_against};

fn resting(time_in_force: TimeInForce) -> OrderRequest {
    OrderRequest {
        client_order_id: "vike-1".to_string(),
        venue: "deribit".to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 10.0,
        order_type: OrderType::Limit,
        price: Some(50_000.0),
        time_in_force,
        ..Default::default()
    }
}

/// The events of one modify of a resting post-only order to a crossing price, and the venue that heard it.
fn crossing_modify(script: Vec<Session>, order: &OrderRequest) -> (Vec<Event>, FakeDeribit) {
    let fake = FakeDeribit::spawn(script);
    let grid = SymbolProperties { tick_size: 0.5, step_size: 10.0, ..Default::default() };
    let rest = DeribitRest::new(transport_against(&fake), SYMBOL, grid, "BTC");
    let events = rest.modify_order(order, None, Some(51_000.0));
    (events, fake)
}

/// `events` folded on the order as the engine holds it: resting, at its old price.
fn folded(events: &[Event]) -> ManagedOrder {
    let mut order = ManagedOrder::new(resting(TimeInForce::PostOnly));
    order.status = OrderStatus::Accepted;
    for event in events {
        order.apply(event).expect("the FSM takes it from a resting order");
    }
    order
}

fn single_modify_refusal(events: &[Event]) -> String {
    match events {
        [Event::OrderModifyRejected(r)] => r.reason.to_string(),
        other => panic!("one non-terminal refusal, got {other:?}"),
    }
}

/// **The venue refuses the edit and the order is gone: the engine hears it was cancelled**, with the venue's reason,
/// and the order ends in the FSM. Without the read-back this was "the order keeps its terms".
#[test]
fn a_refused_post_only_edit_that_ended_the_order_is_told_as_a_cancel() {
    let script = Session::CrossingEdit { refused: true, order_state: Some("cancelled") };
    let (events, fake) = crossing_modify(vec![script], &resting(TimeInForce::PostOnly));
    match events.as_slice() {
        [Event::OrderCanceled(c)] => {
            assert_eq!(c.reason.to_string(), "deribit cancelled the order: edit_post_only_reject");
        }
        other => panic!("one cancel, got {other:?}"),
    }
    assert_eq!(fake.count("private/get_order_state"), 1);
    assert_eq!(folded(&events).status, OrderStatus::Canceled);
}

/// **The venue refuses the edit and the order still rests: it keeps its terms**, in the venue's own word.
#[test]
fn a_refused_post_only_edit_that_left_the_order_resting_keeps_its_terms() {
    let script = Session::CrossingEdit { refused: true, order_state: Some("open") };
    let (events, _fake) = crossing_modify(vec![script], &resting(TimeInForce::PostOnly));
    assert_eq!(single_modify_refusal(&events), "post_only_reject");
    let order = folded(&events);
    assert_eq!(order.status, OrderStatus::Accepted);
    assert_eq!(order.request.price, Some(50_000.0));
}

/// **The venue answers with a success that says the order is cancelled** (the changelog's "`private/edit` will return
/// the cancel reason"): that is no modify. This was `OrderModified`: a registry resting at a price the venue never held.
#[test]
fn an_edit_answered_with_a_cancelled_order_is_a_cancel_and_no_modify() {
    let script = Session::CrossingEdit { refused: false, order_state: Some("cancelled") };
    let (events, fake) = crossing_modify(vec![script], &resting(TimeInForce::PostOnly));
    assert!(matches!(events.as_slice(), [Event::OrderCanceled(_)]), "{events:?}");
    assert_eq!(fake.count("private/get_order_state"), 0, "the reply said it");
    assert_eq!(folded(&events).status, OrderStatus::Canceled);
}

/// **The read-back fails: the doubt is named**, and nothing terminal is invented about an order that may still rest.
#[test]
fn a_refused_edit_whose_read_back_fails_names_the_doubt() {
    let script = Session::CrossingEdit { refused: true, order_state: None };
    let (events, _fake) = crossing_modify(vec![script], &resting(TimeInForce::PostOnly));
    let reason = single_modify_refusal(&events);
    assert!(reason.contains("post_only_reject") && reason.contains("UNKNOWN"), "{reason}");
}

/// **PRESERVE: a refused edit of any other order reads nothing back.** Only `reject_post_only` is documented to end an
/// order on an edit, and it is sent for a post-only order alone.
#[test]
fn a_refused_edit_of_an_order_that_is_not_post_only_sends_no_read_back() {
    let script = Session::CrossingEdit { refused: true, order_state: Some("open") };
    let (events, fake) = crossing_modify(vec![script], &resting(TimeInForce::Gtc));
    assert_eq!(single_modify_refusal(&events), "post_only_reject");
    assert_eq!(fake.count("private/get_order_state"), 0);
}

/// **A lost reply settles the same way**: the order read back is cancelled, so the engine hears that, whoever ended it.
#[test]
fn a_lost_edit_reply_that_finds_the_order_cancelled_is_told_as_a_cancel() {
    let after_redial = Session::CrossingEdit { refused: false, order_state: Some("cancelled") };
    let script = vec![Session::CloseOnEdit, after_redial];
    let (events, fake) = crossing_modify(script, &resting(TimeInForce::PostOnly));
    assert!(matches!(events.as_slice(), [Event::OrderCanceled(_)]), "{events:?}");
    assert_eq!(fake.count("private/edit"), 1, "an edit is never re-sent");
}

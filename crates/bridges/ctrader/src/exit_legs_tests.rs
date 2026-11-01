use super::*;

use vike_model::events::{
    OrderCancelRejected, OrderExpired, OrderModified, OrderModifyRejected, TradeId,
};

fn fill(coid: &str, qty: f64, id: &str) -> FillEvent {
    FillEvent {
        trade_id: TradeId::new(id).expect("a non-empty trade id"),
        client_order_id: coid.to_string(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side: -1,
        last_qty: qty,
        last_px: 1.05,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: Default::default(),
        ts: 1,
        mark_price: None,
        position_side: Default::default(),
    }
}

fn accepted(coid: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some("1".into()),
        ts: 1,
    })
}

fn partial(coid: &str, qty: f64, id: &str) -> Event {
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        client_order_id: coid.to_string(),
        fill: fill(coid, qty, id),
        ts: 1,
    })
}

fn filled(coid: &str, qty: f64, id: &str) -> Event {
    Event::OrderFilled(OrderFilled {
        client_order_id: coid.to_string(),
        fill: fill(coid, qty, id),
        ts: 1,
    })
}

fn canceled(coid: &str) -> Event {
    Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.to_string(),
        reason: "cancelled".into(),
        ts: 1,
    })
}

fn rejected(coid: &str) -> Event {
    Event::OrderRejected(OrderRejected {
        client_order_id: coid.to_string(),
        reason: "refused".into(),
        ts: 1,
        cause: None,
    })
}

/// The exit `P` with two legs `PL1` and `PL2`.
fn two_legs() -> ExitLegs {
    let mut legs = ExitLegs::default();
    legs.register("P", &[leg_coid("P", 1), leg_coid("P", 2)]);
    legs
}

/// A short name for each event, in order, with the coid it carries.
fn shape(events: &[Event]) -> Vec<(&'static str, String)> {
    events
        .iter()
        .map(|e| {
            let kind = match e {
                Event::Fill(_) => "Fill",
                Event::OrderAccepted(_) => "Accepted",
                Event::OrderRejected(_) => "Rejected",
                Event::OrderPartiallyFilled(_) => "Partial",
                Event::OrderFilled(_) => "Filled",
                Event::OrderCanceled(_) => "Canceled",
                Event::OrderExpired(_) => "Expired",
                Event::OrderModified(_) => "Modified",
                Event::OrderCancelRejected(_) => "CancelRejected",
                Event::OrderModifyRejected(_) => "ModifyRejected",
                _ => "other",
            };
            (kind, coid_of(e).unwrap_or("").to_string())
        })
        .collect()
}

fn s(kind: &'static str, coid: &str) -> (&'static str, String) {
    (kind, coid.to_string())
}

/// With no exit group, or for a coid that is no leg, every event comes back untouched.
#[test]
fn an_event_of_no_leg_passes_untouched() {
    let mut none = ExitLegs::default();
    assert_eq!(none.fold(accepted("X")), vec![accepted("X")]);
    let mut legs = two_legs();
    assert_eq!(legs.fold(accepted("X")), vec![accepted("X")]);
    assert_eq!(
        legs.fold(filled("P", 1.0, "t")),
        vec![filled("P", 1.0, "t")],
        "the parent's own coid"
    );
}

/// ONE `OrderAccepted` for the exit, in the parent's name: the first leg's. The FSM takes an accept only from
/// `Submitted`, so a second one would be a refused transition.
#[test]
fn the_exit_is_accepted_once_in_the_parents_name() {
    let mut legs = two_legs();
    assert_eq!(shape(&legs.fold(accepted("PL1"))), vec![s("Accepted", "P")]);
    assert!(
        legs.fold(accepted("PL2")).is_empty(),
        "the second leg's accept is not a second accept"
    );
}

/// Each leg's fill is the exit's: a bare `Fill` for the account fold and a wrap for the order. The exit is
/// `Filled` only when EVERY leg filled; before that it is partially filled.
#[test]
fn the_exit_fills_leg_by_leg_and_is_filled_when_every_leg_has() {
    let mut legs = two_legs();
    let _ = legs.fold(accepted("PL1"));
    let _ = legs.fold(accepted("PL2"));

    let first = legs.fold(Event::Fill(fill("PL1", 1000.0, "d1")));
    assert_eq!(shape(&first), vec![s("Fill", "P")]);
    let first = legs.fold(filled("PL1", 1000.0, "d1"));
    assert_eq!(shape(&first), vec![s("Partial", "P")], "one leg of two filled");
    let Event::OrderPartiallyFilled(w) = &first[0] else { panic!("a partial fill") };
    assert_eq!((w.fill.client_order_id.as_str(), w.fill.last_qty), ("P", 1000.0));

    assert_eq!(shape(&legs.fold(Event::Fill(fill("PL2", 500.0, "d2")))), vec![s("Fill", "P")]);
    let last = legs.fold(filled("PL2", 500.0, "d2"));
    assert_eq!(shape(&last), vec![s("Filled", "P")], "every leg filled");
    assert_eq!(legs.live_legs("P"), None, "the group ended and is forgotten");
    assert_eq!(legs.parent_of("PL1"), None);
}

/// A leg's own partial fill is the exit's partial fill.
#[test]
fn a_partial_fill_of_a_leg_is_a_partial_fill_of_the_exit() {
    let mut legs = two_legs();
    let _ = legs.fold(accepted("PL1"));
    assert_eq!(shape(&legs.fold(partial("PL1", 200.0, "d1"))), vec![s("Partial", "P")]);
}

/// Cancelling the exit cancels every leg; the engine hears ONE cancel, when the last leg is gone.
#[test]
fn the_exit_is_canceled_when_its_last_leg_is() {
    let mut legs = two_legs();
    let _ = legs.fold(accepted("PL1"));
    assert!(legs.fold(canceled("PL1")).is_empty(), "a sibling still rests: the exit is not over");
    assert_eq!(legs.live_legs("P"), Some(vec!["PL2".to_string()]));
    assert_eq!(shape(&legs.fold(canceled("PL2"))), vec![s("Canceled", "P")]);
    assert_eq!(legs.live_legs("P"), None);
}

/// A leg that filled and a sibling that was cancelled afterwards: the exit ends CANCELED (legal after a partial
/// fill), never `Filled`.
#[test]
fn a_fill_then_a_cancel_of_the_last_leg_ends_the_exit_canceled() {
    let mut legs = two_legs();
    let _ = legs.fold(accepted("PL1"));
    assert_eq!(shape(&legs.fold(filled("PL1", 1000.0, "d1"))), vec![s("Partial", "P")]);
    assert_eq!(shape(&legs.fold(canceled("PL2"))), vec![s("Canceled", "P")]);
}

/// …and the order of those two does not matter: a cancel first, the last leg's fill after, ends partially
/// filled and canceled.
#[test]
fn a_cancel_then_the_last_legs_fill_ends_the_exit_partly_filled_and_canceled() {
    let mut legs = two_legs();
    let _ = legs.fold(accepted("PL1"));
    assert!(legs.fold(canceled("PL1")).is_empty());
    assert_eq!(
        shape(&legs.fold(filled("PL2", 500.0, "d2"))),
        vec![s("Partial", "P"), s("Canceled", "P")]
    );
}

/// A refusal of EVERY leg before anything was accepted is the exit's rejection (legal from `Submitted`); one leg's
/// refusal while a sibling rests is not passed on; and a rejection after an accept can only end as a cancel.
#[test]
fn a_rejection_is_the_exits_only_when_nothing_was_accepted() {
    let mut legs = two_legs();
    assert!(legs.fold(rejected("PL1")).is_empty(), "the other leg is still in flight");
    assert_eq!(shape(&legs.fold(rejected("PL2"))), vec![s("Rejected", "P")]);

    let mut legs = two_legs();
    let _ = legs.fold(accepted("PL1"));
    assert!(legs.fold(rejected("PL2")).is_empty());
    assert_eq!(
        shape(&legs.fold(canceled("PL1"))),
        vec![s("Canceled", "P")],
        "an accepted exit cannot be rejected: it ends cancelled"
    );
}

/// An expiry is a leg ending unfilled, like a cancel.
#[test]
fn an_expired_last_leg_ends_the_exit() {
    let mut legs = two_legs();
    let _ = legs.fold(accepted("PL1"));
    assert!(
        legs.fold(Event::OrderExpired(OrderExpired { client_order_id: "PL1".to_string(), ts: 1 }))
            .is_empty()
    );
    assert_eq!(shape(&legs.fold(canceled("PL2"))), vec![s("Canceled", "P")]);
}

/// A leg amended at the venue is not reported (it would resize the engine's whole order to one leg's size); a
/// refused cancel or amend of a leg is the exit's non-terminal advisory.
#[test]
fn advisories_are_the_parents_and_a_leg_amend_is_dropped() {
    let mut legs = two_legs();
    let modified = Event::OrderModified(OrderModified {
        client_order_id: "PL1".to_string(),
        venue_order_id: None,
        new_qty: Some(500.0),
        new_price: Some(1.04),
        ts: 1,
    });
    assert!(legs.fold(modified).is_empty());
    let cancel_refused = Event::OrderCancelRejected(OrderCancelRejected {
        client_order_id: "PL2".to_string(),
        reason: "refused".into(),
        ts: 1,
    });
    assert_eq!(shape(&legs.fold(cancel_refused)), vec![s("CancelRejected", "P")]);
    let modify_refused = Event::OrderModifyRejected(OrderModifyRejected {
        client_order_id: "PL1".to_string(),
        reason: "refused".into(),
        ts: 1,
    });
    assert_eq!(shape(&legs.fold(modify_refused)), vec![s("ModifyRejected", "P")]);
}

/// A leg a reconnect found gone (no event will ever come for it) stops holding the exit open: the exit ends silently
/// when no leg rests, and a fill of the last resting leg then ends it partly filled and cancelled, never "filled".
#[test]
fn a_leg_retired_after_a_reconnect_does_not_hold_the_exit_open() {
    let mut legs = two_legs();
    legs.retire_legs_where(|leg| leg == "PL1");
    assert_eq!(legs.live_legs("P"), Some(vec!["PL2".to_string()]));
    assert_eq!(
        shape(&legs.fold(filled("PL2", 1.0, "t1"))),
        vec![s("Partial", "P"), s("Canceled", "P")],
        "a leg that vanished unseen means the exit cannot be called filled"
    );

    let mut legs = two_legs();
    legs.retire_legs_where(|_| true);
    assert_eq!(legs.live_legs("P"), None, "nothing rests: the exit is forgotten");
    assert_eq!(legs.parent_of("PL1"), None);
    assert_eq!(legs.fold(accepted("PL1")), vec![accepted("PL1")], "no longer a leg");

    let mut legs = two_legs();
    legs.retire_legs_where(|_| false);
    assert_eq!(legs.live_legs("P").map(|l| l.len()), Some(2), "nothing gone: nothing retired");
}

/// A submit that never reached the venue forgets its group whole, legs included.
#[test]
fn forgetting_an_exit_forgets_its_legs() {
    let mut legs = two_legs();
    assert_eq!(legs.live_legs("P").map(|l| l.len()), Some(2));
    assert_eq!(legs.forget("P"), vec!["PL1".to_string(), "PL2".to_string()]);
    assert_eq!(legs.live_legs("P"), None);
    assert_eq!(legs.parent_of("PL1"), None);
    assert_eq!(legs.fold(accepted("PL1")), vec![accepted("PL1")], "no longer a leg");
    assert!(legs.forget("P").is_empty());
}

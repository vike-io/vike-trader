use super::*;

use vike_model::events::{
    OrderCancelRejected, OrderExpired, OrderModified, OrderModifyRejected, TradeId,
};

fn fill(order_client_id: &str, qty: f64, id: &str) -> FillEvent {
    FillEvent {
        trade_id: TradeId::new(id).expect("a non-empty trade id"),
        order_client_id: order_client_id.to_string(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side: -1,
        last_qty: qty,
        last_price: 1.05,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: Default::default(),
        timestamp: 1,
        mark_price: None,
        position_side: Default::default(),
    }
}

fn accepted(order_client_id: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: Some("1".into()),
        timestamp: 1,
    })
}

fn partial(order_client_id: &str, qty: f64, id: &str) -> Event {
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        order_client_id: order_client_id.to_string(),
        fill: fill(order_client_id, qty, id),
        timestamp: 1,
    })
}

fn filled(order_client_id: &str, qty: f64, id: &str) -> Event {
    Event::OrderFilled(OrderFilled {
        order_client_id: order_client_id.to_string(),
        fill: fill(order_client_id, qty, id),
        timestamp: 1,
    })
}

fn canceled(order_client_id: &str) -> Event {
    Event::OrderCanceled(OrderCanceled {
        order_client_id: order_client_id.to_string(),
        reason: "cancelled".into(),
        timestamp: 1,
    })
}

fn rejected(order_client_id: &str) -> Event {
    Event::OrderRejected(OrderRejected {
        order_client_id: order_client_id.to_string(),
        reason: "refused".into(),
        timestamp: 1,
        cause: None,
    })
}

/// The exit `P` with two legs: `PL1` of 1000 units and `PL2` of 500 (centi-volumes).
fn two_legs() -> ExitLegs {
    let mut legs = ExitLegs::default();
    legs.register(
        "P",
        &[(leg_order_client_id("P", 1), 100_000), (leg_order_client_id("P", 2), 50_000)],
    );
    legs
}

/// The one `OrderModified` of `events`, as `(order client id, new_qty)`.
fn narrowed_to(events: &[Event]) -> (String, f64) {
    let [Event::OrderModified(m)] = events else { panic!("one OrderModified, got {events:?}") };
    assert_eq!((m.new_price, m.new_trigger_price), (None, None), "only the size is told");
    (m.order_client_id.clone(), m.new_qty.expect("a size"))
}

/// A short name for each event, in order, with the order client id it carries.
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
            (kind, order_client_id_of(e).unwrap_or("").to_string())
        })
        .collect()
}

fn s(kind: &'static str, order_client_id: &str) -> (&'static str, String) {
    (kind, order_client_id.to_string())
}

/// With no exit group, or for an order client id that is no leg, every event comes back untouched.
#[test]
fn an_event_of_no_leg_passes_untouched() {
    let mut none = ExitLegs::default();
    assert_eq!(none.fold(accepted("X")), vec![accepted("X")]);
    let mut legs = two_legs();
    assert_eq!(legs.fold(accepted("X")), vec![accepted("X")]);
    assert_eq!(
        legs.fold(filled("P", 1.0, "t")),
        vec![filled("P", 1.0, "t")],
        "the parent's own order client id"
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
/// `Filled` when its last leg fills; before that it is partially filled.
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
    assert_eq!((w.fill.order_client_id.as_str(), w.fill.last_qty), ("P", 1000.0));

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

/// Cancelling the exit cancels every leg; the engine hears ONE cancel, when the last leg is gone. A leg that goes
/// first narrows the exit to what still rests (it could be the last one standing, if its sibling's cancel is refused).
#[test]
fn the_exit_is_canceled_when_its_last_leg_is() {
    let mut legs = two_legs();
    let _ = legs.fold(accepted("PL1"));
    assert_eq!(
        narrowed_to(&legs.fold(canceled("PL1"))),
        ("P".to_string(), 500.0),
        "a sibling still rests: the exit is not over, it backs PL2's 500"
    );
    assert_eq!(legs.live_legs("P"), Some(vec!["PL2".to_string()]));
    assert_eq!(shape(&legs.fold(canceled("PL2"))), vec![s("Canceled", "P")]);
    assert_eq!(legs.live_legs("P"), None);
}

/// FXCM's `ExitGroup` rule: a leg that ends UNFILLED while a sibling rests (the orphan rule cancelled it, its position
/// closed another way) tells the engine `OrderModified` with the size the venue still backs: what filled plus what
/// still rests. A leg's partial fill before it ended counts; the rest of it does not.
#[test]
fn a_leg_that_ends_unfilled_while_a_sibling_rests_narrows_the_exit_to_what_the_venue_backs() {
    let mut legs = ExitLegs::default();
    legs.register(
        "P",
        &[
            (leg_order_client_id("P", 1), 100_000),
            (leg_order_client_id("P", 2), 50_000),
            (leg_order_client_id("P", 3), 30_000),
        ],
    );
    let _ = legs.fold(accepted("PL1"));
    assert_eq!(shape(&legs.fold(partial("PL1", 200.0, "d1"))), vec![s("Partial", "P")]);
    assert_eq!(
        narrowed_to(&legs.fold(canceled("PL1"))),
        ("P".to_string(), 200.0 + 500.0 + 300.0),
        "PL1's 200 filled, PL2 and PL3 rest"
    );
    assert_eq!(shape(&legs.fold(filled("PL2", 500.0, "d2"))), vec![s("Partial", "P")]);
    assert_eq!(
        shape(&legs.fold(Event::OrderExpired(OrderExpired {
            order_client_id: "PL3".to_string(),
            timestamp: 1
        }))),
        vec![s("Canceled", "P")],
        "the LAST leg ending unfilled is the exit's terminal, not a narrowing"
    );
}

/// A leg the venue refuses before the exit's first accept cannot be told then (the FSM takes no modify from
/// `Submitted`): it is told right behind that accept. The sibling's fill then ends the exit FILLED, because the order
/// the engine holds is the narrowed one.
#[test]
fn a_leg_refused_before_the_first_accept_is_told_with_that_accept() {
    let mut legs = two_legs();
    assert!(legs.fold(rejected("PL1")).is_empty(), "nothing accepted yet: nothing to narrow");
    let out = legs.fold(accepted("PL2"));
    assert_eq!(shape(&out), vec![s("Accepted", "P"), s("Modified", "P")]);
    assert_eq!(narrowed_to(&out[1..]), ("P".to_string(), 500.0), "only PL2 rests");
    assert_eq!(shape(&legs.fold(filled("PL2", 500.0, "d2"))), vec![s("Filled", "P")]);
}

/// A leg's real volume is what [`ExitLegs::set_volume`] last said (the clamp after the mapper, a venue amend), not the
/// planned one; an order client id that is no leg is ignored.
#[test]
fn the_size_told_follows_the_volume_the_leg_really_has() {
    let mut legs = two_legs();
    legs.set_volume("PL2", 40_000);
    legs.set_volume("X", 1);
    let _ = legs.fold(accepted("PL1"));
    assert_eq!(narrowed_to(&legs.fold(canceled("PL1"))), ("P".to_string(), 400.0));
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

/// …and a cancel first, the last leg's fill after: the cancel narrowed the exit to the last leg, so its fill fills
/// the order the engine now holds (FXCM's `ExitGroup` ends the same way).
#[test]
fn a_cancel_then_the_last_legs_fill_narrows_the_exit_and_ends_it_filled() {
    let mut legs = two_legs();
    let _ = legs.fold(accepted("PL1"));
    assert_eq!(narrowed_to(&legs.fold(canceled("PL1"))), ("P".to_string(), 500.0));
    assert_eq!(shape(&legs.fold(filled("PL2", 500.0, "d2"))), vec![s("Filled", "P")]);
}

/// A refusal of EVERY leg before anything was accepted is the exit's rejection (legal from `Submitted`); one leg's
/// refusal while a sibling rests narrows the exit (once accepted); and a rejection after an accept can only end as a
/// cancel.
#[test]
fn a_rejection_is_the_exits_only_when_nothing_was_accepted() {
    let mut legs = two_legs();
    assert!(legs.fold(rejected("PL1")).is_empty(), "the other leg is still in flight");
    assert_eq!(shape(&legs.fold(rejected("PL2"))), vec![s("Rejected", "P")]);

    let mut legs = two_legs();
    let _ = legs.fold(accepted("PL1"));
    assert_eq!(narrowed_to(&legs.fold(rejected("PL2"))), ("P".to_string(), 1000.0));
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
    assert_eq!(
        narrowed_to(&legs.fold(Event::OrderExpired(OrderExpired {
            order_client_id: "PL1".to_string(),
            timestamp: 1
        }))),
        ("P".to_string(), 500.0)
    );
    assert_eq!(shape(&legs.fold(canceled("PL2"))), vec![s("Canceled", "P")]);
}

/// A leg amended at the venue is not reported (it would resize the engine's whole order to one leg's size); a
/// refused cancel or amend of a leg is the exit's non-terminal advisory.
#[test]
fn advisories_are_the_parents_and_a_leg_amend_is_dropped() {
    let mut legs = two_legs();
    let modified = Event::OrderModified(OrderModified {
        order_client_id: "PL1".to_string(),
        venue_order_id: None,
        new_qty: Some(500.0),
        new_price: Some(1.04),
        new_trigger_price: None,
        timestamp: 1,
    });
    assert!(legs.fold(modified).is_empty());
    let cancel_refused = Event::OrderCancelRejected(OrderCancelRejected {
        order_client_id: "PL2".to_string(),
        reason: "refused".into(),
        timestamp: 1,
    });
    assert_eq!(shape(&legs.fold(cancel_refused)), vec![s("CancelRejected", "P")]);
    let modify_refused = Event::OrderModifyRejected(OrderModifyRejected {
        order_client_id: "PL1".to_string(),
        reason: "refused".into(),
        timestamp: 1,
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

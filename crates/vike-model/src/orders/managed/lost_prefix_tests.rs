//! `ManagedOrder::apply_over_lost_prefix` (W19-D1): the door for a venue's ANSWER to an order whose
//! `OrderSubmitted` never reached the fold. The TABLE stays as the r5 fixture pins it.

use super::*;
use crate::events::{OrderAccepted, OrderFilled, OrderPartiallyFilled};

const ALL: &[OrderStatus] = &[
    S::Initialized,
    S::Submitted,
    S::Accepted,
    S::Triggered,
    S::PartiallyFilled,
    S::Filled,
    S::Canceled,
    S::Rejected,
    S::Denied,
    S::Expired,
    S::PendingCancel,
    S::Liquidated,
    S::Emulated,
    S::Released,
];

fn order() -> ManagedOrder {
    ManagedOrder::new(OrderRequest {
        client_order_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: 1,
        qty: 2.0,
        order_type: OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    })
}

fn accepted(venue_id: Option<&str>) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: "c".into(),
        venue_order_id: venue_id.map(Into::into),
        ts: 7,
    })
}

fn fill(qty: f64) -> FillEvent {
    fill_as("t1", qty)
}

fn fill_as(trade_id: &'static str, qty: f64) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: None,
        position_side: crate::events::PositionSide::Both,
    }
}

fn partially_filled() -> Event {
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        client_order_id: "c".into(),
        fill: fill(1.0),
        ts: 7,
    })
}

fn filled() -> Event {
    Event::OrderFilled(OrderFilled { client_order_id: "c".into(), fill: fill_as("t2", 2.0), ts: 7 })
}

fn rejected() -> Event {
    Event::OrderRejected(crate::events::OrderRejected {
        client_order_id: "c".into(),
        reason: "x".into(),
        ts: 0,
    })
}

#[test]
fn the_table_still_refuses_a_bare_accepted_from_initialized() {
    // The r5 fixture's row, restated: the door below is separate, `apply` is untouched.
    let mut o = order();
    assert!(o.apply(&accepted(None)).is_err());
    assert_eq!(o.status, OrderStatus::Initialized);
}

#[test]
fn a_bare_accepted_lands_on_an_initialized_order_and_keeps_its_venue_id() {
    let mut o = order();
    o.apply_over_lost_prefix(&accepted(Some("v-1"))).unwrap();
    assert_eq!(o.status, OrderStatus::Accepted);
    assert_eq!(o.venue_order_id.as_deref(), Some("v-1"));
    // and the order then lives its ordinary life, fills included
    o.apply(&partially_filled()).unwrap();
    o.apply(&filled()).unwrap();
    assert_eq!(o.status, OrderStatus::Filled);
}

#[test]
fn every_other_status_and_event_answers_exactly_as_apply_does() {
    let events = [
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "c".into(), ts: 0 }),
        accepted(None),
        accepted(Some("v-1")),
        partially_filled(),
        filled(),
        rejected(),
        Event::OrderCanceled(crate::events::OrderCanceled {
            client_order_id: "c".into(),
            reason: String::new().into(),
            ts: 0,
        }),
        Event::OrderExpired(crate::events::OrderExpired { client_order_id: "c".into(), ts: 0 }),
        Event::OrderTriggered(crate::events::OrderTriggered { client_order_id: "c".into(), ts: 0 }),
        Event::OrderDenied(crate::events::OrderDenied {
            client_order_id: "c".into(),
            reason: "x".into(),
            ts: 0,
        }),
    ];
    for from in ALL {
        for event in &events {
            let mut via_apply = order();
            via_apply.status = *from;
            let mut via_door = via_apply.clone();
            let a = via_apply.apply(event);
            let d = via_door.apply_over_lost_prefix(event);
            let widened = *from == OrderStatus::Initialized
                && matches!(event, Event::OrderAccepted(ans) if ans.venue_order_id.is_some());
            let what = format!("{} + {}", from.as_str(), event_name(event));
            if widened {
                assert!(a.is_err() && d.is_ok(), "{what}");
            } else {
                assert_eq!(a, d, "{what}");
                assert_eq!(via_apply, via_door, "{what}");
            }
        }
    }
}

#[test]
fn a_fill_a_cancel_and_an_expiry_are_not_in_the_set_so_they_are_still_refused_from_initialized() {
    // The `out_of_order` scenario of `fixtures/r5/hub.json`: a fill arriving before `OrderSubmitted` is dropped.
    for event in [
        partially_filled(),
        filled(),
        Event::OrderCanceled(crate::events::OrderCanceled {
            client_order_id: "c".into(),
            reason: String::new().into(),
            ts: 0,
        }),
        Event::OrderExpired(crate::events::OrderExpired { client_order_id: "c".into(), ts: 0 }),
    ] {
        let mut o = order();
        assert!(o.apply_over_lost_prefix(&event).is_err(), "{}", event_name(&event));
        assert_eq!(o.status, OrderStatus::Initialized, "a refusal leaves the order as found");
        assert_eq!(o.filled_qty, 0.0);
    }
}

// ---- W19g ----

fn canceled() -> Event {
    Event::OrderCanceled(crate::events::OrderCanceled {
        client_order_id: "c".into(),
        reason: String::new().into(),
        ts: 0,
    })
}

#[test]
fn an_accept_with_no_venue_id_is_refused_and_leaves_the_order_initialized() {
    // The resolver's OPTIMISTIC answer (the re-query failed): no evidence the venue holds the order, so the
    // order stays `Initialized`, where the stuck-order watchdog still sweeps it.
    let mut o = order();
    let before = o.clone();
    assert!(o.apply_over_lost_prefix(&accepted(None)).is_err());
    assert_eq!(o, before, "a refusal leaves the order exactly as found");
}

#[test]
fn a_fill_a_cancel_and_an_expiry_found_on_an_initialized_order_are_kept_for_the_answer() {
    for event in [
        partially_filled(),
        filled(),
        canceled(),
        Event::OrderExpired(crate::events::OrderExpired { client_order_id: "c".into(), ts: 0 }),
    ] {
        let mut o = order();
        assert!(o.apply(&event).is_err());
        assert!(o.defer_refused(&event), "{}", event_name(&event));
        assert_eq!(o.deferred_events, vec![event.clone()]);
        assert!(!o.defer_refused(&event), "the same event is not held twice");
        assert_eq!(o.status, OrderStatus::Initialized);
        assert_eq!(o.filled_qty, 0.0, "kept, not applied");
    }
}

#[test]
fn only_what_the_venue_sends_for_an_order_past_acceptance_is_kept_and_only_while_initialized() {
    let mut o = order();
    for event in [
        accepted(None),
        accepted(Some("v-1")),
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "c".into(), ts: 0 }),
        rejected(),
        Event::OrderTriggered(crate::events::OrderTriggered { client_order_id: "c".into(), ts: 0 }),
    ] {
        assert!(!o.defer_refused(&event), "{}", event_name(&event));
    }
    assert!(o.deferred_events.is_empty());
    for status in ALL.iter().filter(|s| **s != OrderStatus::Initialized) {
        o.status = *status;
        assert!(!o.defer_refused(&filled()), "{}", status.as_str());
    }
    assert!(o.deferred_events.is_empty());
}

#[test]
fn the_buffer_is_bounded() {
    let mut o = order();
    for i in 0..(MAX_DEFERRED_EVENTS + 5) {
        let mut f = fill(0.1);
        let id: &'static str = Box::leak(format!("t{i}").into_boxed_str());
        f.trade_id = id.into();
        let ev = Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: "c".into(),
            fill: f,
            ts: 0,
        });
        assert_eq!(o.defer_refused(&ev), i < MAX_DEFERRED_EVENTS);
    }
    assert_eq!(o.deferred_events.len(), MAX_DEFERRED_EVENTS);
}

#[test]
fn the_answer_then_the_replay_applies_what_was_kept_in_arrival_order() {
    let mut o = order();
    assert!(o.defer_refused(&partially_filled()));
    assert!(o.defer_refused(&filled()));
    o.apply_over_lost_prefix(&accepted(Some("v-1"))).unwrap();
    assert_eq!(o.status, OrderStatus::Accepted, "the answer alone leaves the buffer untouched");
    assert_eq!(o.deferred_events.len(), 2);

    let applied = o.replay_deferred(|_| false);

    assert_eq!(applied, vec![partially_filled(), filled()]);
    assert_eq!(o.status, OrderStatus::Filled);
    assert_eq!(o.filled_qty, 3.0, "1.0 then 2.0, through the ordinary table");
    assert!(o.deferred_events.is_empty());
}

#[test]
fn a_kept_event_the_table_still_refuses_is_dropped_by_the_replay() {
    // A cancel and then a fill, both found before the answer: the cancel applies, the fill then finds a
    // terminal order, as it would have live.
    let mut o = order();
    assert!(o.defer_refused(&canceled()));
    assert!(o.defer_refused(&filled()));
    o.apply_over_lost_prefix(&accepted(Some("v-1"))).unwrap();

    assert_eq!(o.replay_deferred(|_| false), vec![canceled()]);
    assert_eq!(o.status, OrderStatus::Canceled);
    assert_eq!(o.filled_qty, 0.0);
}

#[test]
fn the_buffer_is_absent_from_an_orders_json_until_something_is_kept() {
    let mut o = order();
    let json = serde_json::to_string(&o).unwrap();
    assert!(!json.contains("deferred_events"), "{json}");
    assert_eq!(
        serde_json::from_str::<ManagedOrder>(&json).unwrap(),
        o,
        "and an older JSON still reads"
    );
    assert!(o.defer_refused(&filled()));
    let kept = serde_json::to_string(&o).unwrap();
    assert!(kept.contains("deferred_events"));
    assert_eq!(serde_json::from_str::<ManagedOrder>(&kept).unwrap(), o);
}

/// A copy of `event` as the bridge's reconnect-resync or a reconcile-synthesized fill redelivers it: the
/// SAME trade id, a different timestamp (so `==` does not see it).
fn redelivered(event: &Event) -> Event {
    let mut again = event.clone();
    match &mut again {
        Event::OrderPartiallyFilled(w) => {
            w.ts += 1000;
            w.fill.ts += 1000;
        }
        Event::OrderFilled(w) => {
            w.ts += 1000;
            w.fill.ts += 1000;
        }
        other => panic!("not a fill: {other:?}"),
    }
    again
}

#[test]
fn a_fill_redelivered_under_the_same_trade_id_is_not_kept_twice() {
    // W13c finding 4: event equality missed a redelivery (another `ts`), so both were held and both applied.
    let mut o = order();
    assert!(o.defer_refused(&partially_filled()));
    let again = redelivered(&partially_filled());
    assert_ne!(again, partially_filled());
    assert!(
        !o.defer_refused(&again),
        "the first arrival of a trade id wins, as in the ordinary fold"
    );
    assert!(o.defer_refused(&filled()), "another trade id is another fill");
    assert_eq!(o.deferred_events.len(), 2);
}

#[test]
fn the_replay_applies_one_fill_per_trade_id_and_never_doubles_the_filled_qty() {
    // Whatever got into the buffer (an older build's, a Snap's), the replay itself dedups: two held fills
    // that share a trade id are applied once.
    let mut o = order();
    o.deferred_events = vec![partially_filled(), redelivered(&partially_filled()), filled()];
    o.apply_over_lost_prefix(&accepted(Some("v-1"))).unwrap();

    let applied = o.replay_deferred(|_| false);

    assert_eq!(applied, vec![partially_filled(), filled()]);
    assert_eq!(o.filled_qty, 3.0, "1.0 once, then 2.0: the redelivery added nothing");
    assert_eq!(o.status, OrderStatus::Filled);
}

#[test]
fn the_replay_drops_a_fill_the_engine_has_already_seen() {
    // `already_seen` is the engine's `seen_fsm_trade_ids`: the ordinary arm drops such a fill before `apply`.
    let mut o = order();
    o.deferred_events = vec![partially_filled(), filled()];
    o.apply_over_lost_prefix(&accepted(Some("v-1"))).unwrap();

    let applied = o.replay_deferred(|tid| tid == "t1");

    assert_eq!(applied, vec![filled()]);
    assert_eq!(o.filled_qty, 2.0);
}

#[test]
fn a_fill_the_table_refused_in_the_replay_does_not_shadow_its_redelivery() {
    // The ordinary arm marks a trade id only after a SUCCESSFUL apply. A cancel first makes the fill refuse
    // (a terminal order), and its redelivery is refused the same way: nothing is counted either time.
    let mut o = order();
    o.deferred_events = vec![canceled(), filled(), redelivered(&filled())];
    o.apply_over_lost_prefix(&accepted(Some("v-1"))).unwrap();

    assert_eq!(o.replay_deferred(|_| false), vec![canceled()]);
    assert_eq!(o.filled_qty, 0.0);
}

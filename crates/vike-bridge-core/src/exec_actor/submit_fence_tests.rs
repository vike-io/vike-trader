//! The fence over a mock `VenueRest` (NO socket, NO thread): the pump's fill is delivered INSIDE the
//! submit, which is the venue pushing the fill before it answers the order. The real sockets, mappers
//! and pumps of each wired venue are `tests/bridge_conformance.rs`'s `SubmitFenceRow` scenario.
use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

use vike_execution::{Ingest, event_channel};
use vike_model::events::{
    FillEvent, LiquiditySide, OrderAccepted, OrderCanceled, OrderExpired, OrderRejected,
    OrderSubmitted, PositionSide,
};

fn req(order_client_id: &str) -> OrderRequest {
    OrderRequest { order_client_id: order_client_id.to_string(), ..Default::default() }
}

/// A lane and a closure that takes everything sent on it so far, in order (the receiver's type
/// belongs to a crate this one does not name).
fn lane() -> (EventSender, impl FnMut() -> Vec<Event>) {
    let (events, mut rx) = event_channel(16);
    let drain = move || {
        let mut out = Vec::new();
        while let Ok(ingest) = rx.try_recv() {
            if let Ingest::Event(event) = ingest {
                out.push(event);
            }
        }
        out
    };
    (events, drain)
}

/// What a fill stream mints for one filled trade of `order_client_id`: `[Fill, OrderFilled]`.
fn fill_of(order_client_id: &str, trade_id: &'static str) -> Vec<Event> {
    let fill = FillEvent {
        trade_id: trade_id.into(),
        order_client_id: order_client_id.to_string(),
        venue: "v".into(),
        symbol: "BTC-PERPETUAL".into(),
        side: -1,
        last_qty: 10.0,
        last_price: 50_000.0,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 1_700_000_000_005,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    crate::event_map::terminal_events(order_client_id.to_string(), fill, 1_700_000_000_005, true)
}

fn answer(order_client_id: &str) -> Vec<Event> {
    vec![
        Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order_client_id.to_string(),
            timestamp: 0,
        }),
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: Some("v-1".to_string().into()),
            timestamp: 0,
        }),
    ]
}

/// A venue that pushes `fill_of(order client id)` on the fill stream before it answers the order.
struct FillsFirst<'a>(&'a SubmitFence);

impl VenueRest for FillsFirst<'_> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        for event in fill_of(&request.order_client_id, "t1") {
            assert!(self.0.deliver(event));
        }
        answer(&request.order_client_id)
    }
    fn cancel_order(&self, _: &str) -> Result<crate::rest::OrderCancelAck, VenueApiError> {
        Ok(crate::rest::OrderCancelAck::Canceled)
    }
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "Submitted",
            Event::OrderAccepted(_) => "Accepted",
            Event::Fill(_) => "Fill",
            Event::OrderFilled(_) => "OrderFilled",
            Event::OrderCanceled(_) => "Canceled",
            Event::OrderRejected(_) => "Rejected",
            Event::OrderExpired(_) => "Expired",
            _ => "other",
        })
        .collect()
}

#[test]
fn a_fill_pushed_before_the_answer_follows_the_answer() {
    let (events, mut drain) = lane();
    let fence = SubmitFence::new(events);
    let rest = FillsFirst(&fence);

    let returned = VenueRestOrderSubmitFence::new(&rest, &fence).submit_order(&req("c1"));

    assert!(returned.is_empty(), "the fence sent them itself: {returned:?}");
    assert_eq!(kinds(&drain()), ["Submitted", "Accepted", "Fill", "OrderFilled"]);
}

#[test]
fn another_orders_fill_is_not_held() {
    let (events, mut drain) = lane();
    let fence = SubmitFence::new(events);

    fence.submit("c1", || {
        for event in fill_of("someone-else", "t9") {
            fence.deliver(event);
        }
        // already on the lane, while c1's submit is still open
        assert_eq!(kinds(&drain()), ["Fill", "OrderFilled"]);
        answer("c1")
    });

    assert_eq!(kinds(&drain()), ["Submitted", "Accepted"]);
}

#[test]
fn once_the_answer_is_out_the_stream_flows_straight_through() {
    let (events, mut drain) = lane();
    let fence = SubmitFence::new(events);
    fence.submit("c1", || answer("c1"));
    assert!(fence.state().held.is_empty(), "no entry outlives its submit");
    drain();

    for event in fill_of("c1", "t2") {
        fence.deliver(event);
    }

    assert_eq!(kinds(&drain()), ["Fill", "OrderFilled"], "a late fill is not held");
}

/// The A3 replay's door: `hold` parks every kind the replay mints for the in-flight order (a fill and
/// its wrap, a cancel, a reject and an expiry row) and hands everything else back to be sent by its
/// producer; `fenced_fetch` is that door around a venue's fetch.
#[test]
fn fenced_fetch_parks_a_replayed_fill_cancel_reject_and_expiry_of_the_held_order_only() {
    let (events, mut drain) = lane();
    let fence = Arc::new(SubmitFence::new(events));
    let history = || {
        let mut rows = fill_of("c1", "t7");
        rows.push(Event::OrderCanceled(OrderCanceled {
            order_client_id: "c1".to_string(),
            reason: "".into(),
            timestamp: 5,
            post_only_would_cross: false,
            refusal_id: None,
        }));
        rows.push(Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: "c1".to_string(),
            reason: "".into(),
            timestamp: 6,
            refusal_id: None,
        }));
        rows.push(Event::OrderExpired(OrderExpired {
            order_client_id: "c1".to_string(),
            timestamp: 7,
        }));
        rows.push(Event::OrderCanceled(OrderCanceled {
            order_client_id: "c2".to_string(),
            reason: "".into(),
            timestamp: 8,
            post_only_would_cross: false,
            refusal_id: None,
        }));
        rows
    };
    assert_eq!(
        kinds(&history()),
        ["Fill", "OrderFilled", "Canceled", "Rejected", "Expired", "Canceled"]
    );
    let mut fetch = fence.fenced_fetch(history);

    let mut handed_back = Vec::new();
    fence.submit("c1", || {
        handed_back = fetch();
        assert!(drain().is_empty(), "nothing was sent while the submit is open");
        answer("c1")
    });

    assert_eq!(kinds(&handed_back), ["Canceled"], "only c2's cancel is the producer's to send");
    assert_eq!(
        kinds(&drain()),
        ["Submitted", "Accepted", "Fill", "OrderFilled", "Canceled", "Rejected", "Expired"],
        "c1's replay follows its answer, in the order the replay returned it"
    );
}

#[test]
fn a_submit_that_panics_still_lets_the_held_fill_go() {
    let (events, mut drain) = lane();
    let fence = SubmitFence::new(events);

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        fence.submit("c1", || {
            for event in fill_of("c1", "t3") {
                fence.deliver(event);
            }
            panic!("the exec thread died mid-submit")
        })
    }));

    assert!(outcome.is_err());
    assert_eq!(kinds(&drain()), ["Fill", "OrderFilled"], "released, not lost");
    assert!(fence.state().held.is_empty());
}

/// What a submit returns when its POST got no answer and its re-query failed.
fn no_word(order_client_id: &str) -> Vec<Event> {
    vec![Event::OrderSubmitted(OrderSubmitted {
        order_client_id: order_client_id.to_string(),
        timestamp: 0,
    })]
}

/// **A fill on an order with no answer first accepts it**, whole or partial: a wrap alone is refused on a `Submitted`
/// order and not kept, the order would read REJECTED at the backstop and a bracket's entry would never release its
/// stop. The accept is true, and names no venue id because a trade id is none. It is said once.
#[test]
fn a_fill_after_a_submit_with_no_answer_goes_behind_the_accept_it_implies() {
    let (events, mut drain) = lane();
    let fence = SubmitFence::new(events);
    fence.submit("c1", || no_word("c1"));
    assert_eq!(kinds(&drain()), ["Submitted"], "nothing is said for a silent order");

    for event in fill_of("c1", "t1").into_iter().chain(fill_of("c1", "t2")) {
        fence.deliver(event);
    }

    let said = drain();
    assert_eq!(kinds(&said), ["Fill", "Accepted", "OrderFilled", "Fill", "OrderFilled"]);
    assert!(
        matches!(&said[1], Event::OrderAccepted(a)
            if a.order_client_id == "c1" && a.venue_order_id.is_none()),
        "{said:?}"
    );
}

/// The fill that arrived while the POST hung is parked, and goes out behind the same accept.
#[test]
fn a_fill_held_through_a_submit_with_no_answer_goes_behind_the_accept_it_implies() {
    let (events, mut drain) = lane();
    let fence = SubmitFence::new(events);

    fence.submit("c1", || {
        for event in fill_of("c1", "t1") {
            fence.deliver(event);
        }
        no_word("c1")
    });

    assert_eq!(kinds(&drain()), ["Submitted", "Fill", "Accepted", "OrderFilled"]);
    for event in fill_of("c1", "t2") {
        fence.deliver(event);
    }
    assert_eq!(kinds(&drain()), ["Fill", "OrderFilled"], "the accept is said once");
}

/// The A3 replay's door says it too, and only a fill does: a cancel row of a silent order is the table's to refuse, and
/// an order the venue answered gets nothing.
#[test]
fn only_a_fill_of_an_order_with_no_answer_brings_an_accept() {
    let (events, mut drain) = lane();
    let fence = Arc::new(SubmitFence::new(events));
    fence.submit("silent", || no_word("silent"));
    fence.submit("answered", || answer("answered"));
    drain();

    let mut fetch = fence.fenced_fetch(|| {
        let mut rows = fill_of("answered", "t1");
        rows.push(Event::OrderCanceled(OrderCanceled {
            order_client_id: "silent".to_string(),
            reason: "".into(),
            timestamp: 5,
            post_only_would_cross: false,
            refusal_id: None,
        }));
        rows.extend(fill_of("silent", "t2"));
        rows
    });
    let handed_back = fetch();

    assert_eq!(kinds(&drain()), ["Accepted"], "sent ahead of what the supervisor sends");
    assert_eq!(kinds(&handed_back), ["Fill", "OrderFilled", "Canceled", "Fill", "OrderFilled"]);
}

/// One unanswered submit keeps back at most `MAX_PARKED` events: the next goes back to its producer at once, and
/// what was kept still follows the order's own answer, in the order it arrived.
#[test]
fn past_the_park_cap_an_event_goes_at_once_rather_than_wait() {
    let (events, mut rx) = event_channel(MAX_PARKED + 8);
    let fence = SubmitFence::new(events);
    let row = |n: usize| {
        Event::OrderCanceled(OrderCanceled {
            order_client_id: "c1".to_string(),
            reason: n.to_string().into(),
            timestamp: 0,
            post_only_would_cross: false,
            refusal_id: None,
        })
    };
    fence.submit("c1", || {
        for n in 0..MAX_PARKED {
            assert!(fence.hold(row(n)).is_none(), "row {n} is under the cap and waits");
        }
        assert_eq!(fence.parked("c1"), MAX_PARKED);
        match fence.hold(row(MAX_PARKED)) {
            Some(Event::OrderCanceled(c)) => {
                assert_eq!(c.reason.to_string(), MAX_PARKED.to_string())
            }
            other => panic!("past the cap the row goes back to its producer, got {other:?}"),
        }
        answer("c1")
    });
    let sent: Vec<Event> = std::iter::from_fn(|| match rx.try_recv() {
        Ok(Ingest::Event(event)) => Some(event),
        _ => None,
    })
    .collect();
    assert_eq!(kinds(&sent[..2]), ["Submitted", "Accepted"]);
    let kept: Vec<String> = sent[2..]
        .iter()
        .map(|e| match e {
            Event::OrderCanceled(c) => c.reason.to_string(),
            other => panic!("only the kept rows follow, got {other:?}"),
        })
        .collect();
    assert_eq!(kept, (0..MAX_PARKED).map(|n| n.to_string()).collect::<Vec<_>>());
}

/// An own loop's arm sends its order's events itself, at its own pace: a fill the stream pushed meanwhile waits
/// for the guard to drop, then follows what the arm sent.
#[test]
fn an_entered_fence_lets_the_held_fill_go_after_what_the_arm_sent() {
    let (events, mut drain) = lane();
    let fence = SubmitFence::new(events.clone());
    {
        let _fenced = fence.enter("c1");
        let [submitted, accepted] = <[Event; 2]>::try_from(answer("c1")).unwrap();
        let _ = events.blocking_send(submitted);
        for event in fill_of("c1", "t1") {
            assert!(fence.deliver(event));
        }
        assert_eq!(fence.parked("c1"), 2, "the fill waits while the arm runs");
        let _ = events.blocking_send(accepted);
        assert_eq!(kinds(&drain()), ["Submitted", "Accepted"]);
    }
    assert_eq!(kinds(&drain()), ["Fill", "OrderFilled"]);
    assert!(fence.state().held.is_empty(), "no entry outlives its guard");
}

/// The fence never reads an entered arm's own events, so it implies no accept for an order the arm left with
/// `OrderSubmitted` alone: a later fill goes as it is, and saying more stays the arm's.
#[test]
fn an_entered_fence_implies_no_accept() {
    let (events, mut drain) = lane();
    let fence = SubmitFence::new(events.clone());
    {
        let _fenced = fence.enter("c1");
        let _ = events.blocking_send(no_word("c1").remove(0));
    }
    for event in fill_of("c1", "t1") {
        fence.deliver(event);
    }
    assert_eq!(kinds(&drain()), ["Submitted", "Fill", "OrderFilled"]);
    assert!(fence.state().silent.0.is_empty(), "nothing is remembered for it");
}

/// The list is bounded: the oldest order goes, and a fill on it is what it was without the list.
#[test]
fn the_list_of_orders_with_no_answer_is_bounded() {
    let (events, mut drain) = lane();
    let fence = SubmitFence::new(events);
    for n in 0..=crate::exec_actor::MAX_SILENT_ORDERS {
        fence.submit(&format!("c{n}"), Vec::new);
    }
    drain();
    assert_eq!(fence.state().silent.0.len(), crate::exec_actor::MAX_SILENT_ORDERS);
    for event in fill_of("c0", "t1") {
        fence.deliver(event);
    }
    assert_eq!(kinds(&drain()), ["Fill", "OrderFilled"]);
}

/// **`enter_with`'s hook sees every held event, in order, as it is let go and before it is sent**, and nothing the arm
/// sent itself nor anything that was never held: the seam OANDA keeps its per-order table through (B-17).
#[test]
fn enter_with_runs_its_hook_on_each_released_event_before_it_is_sent() {
    let (events, mut drain) = lane();
    let fence = SubmitFence::new(events.clone());
    let seen = std::sync::Mutex::new(Vec::<&'static str>::new());
    {
        let _fenced = fence.enter_with("c1", |event| {
            seen.lock().unwrap().push(kinds(std::slice::from_ref(event))[0]);
        });
        for event in fill_of("c1", "t1") {
            assert!(fence.deliver(event), "held");
        }
        assert!(fence.deliver(fill_of("c2", "t2").remove(0)), "another order's: straight through");
        for event in answer("c1") {
            let _ = events.blocking_send(event);
        }
        assert!(seen.lock().unwrap().is_empty(), "nothing is let go while the arm runs");
    }
    assert_eq!(*seen.lock().unwrap(), ["Fill", "OrderFilled"], "only the held ones");
    assert_eq!(kinds(&drain()), ["Fill", "Submitted", "Accepted", "Fill", "OrderFilled"]);
}

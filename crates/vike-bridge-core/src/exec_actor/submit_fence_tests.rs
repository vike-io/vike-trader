//! The fence over a mock `VenueRest` (NO socket, NO thread): the pump's fill is delivered INSIDE the
//! submit, which is the venue pushing the fill before it answers the order. The real sockets, mappers
//! and pumps of each wired venue are `tests/bridge_conformance.rs`'s `SubmitFenceRow` scenario.
use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

use vike_exec::{Ingest, event_channel};
use vike_model::events::{
    FillEvent, LiquiditySide, OrderAccepted, OrderCanceled, OrderExpired, OrderRejected,
    OrderSubmitted, PositionSide,
};

fn req(coid: &str) -> OrderRequest {
    OrderRequest { client_order_id: coid.to_string(), ..Default::default() }
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

/// What a fill stream mints for one filled trade of `coid`: `[Fill, OrderFilled]`.
fn fill_of(coid: &str, trade_id: &'static str) -> Vec<Event> {
    let fill = FillEvent {
        trade_id: trade_id.into(),
        client_order_id: coid.to_string(),
        venue: "v".into(),
        symbol: "BTC-PERPETUAL".into(),
        side: -1,
        last_qty: 10.0,
        last_px: 50_000.0,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 1_700_000_000_005,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    crate::event_map::terminal_events(coid.to_string(), fill, 1_700_000_000_005, true)
}

fn answer(coid: &str) -> Vec<Event> {
    vec![
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts: 0 }),
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: Some("v-1".to_string().into()),
            ts: 0,
        }),
    ]
}

/// A venue that pushes `fill_of(coid)` on the fill stream before it answers the order.
struct FillsFirst<'a>(&'a SubmitFence);

impl VenueRest for FillsFirst<'_> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        for event in fill_of(&request.client_order_id, "t1") {
            assert!(self.0.deliver(event));
        }
        answer(&request.client_order_id)
    }
    fn cancel_order(&self, _client_order_id: &str) -> Result<(), VenueApiError> {
        Ok(())
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

    let returned = FencedRest::new(&rest, &fence).submit_order(&req("c1"));

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
    assert!(fence.held().is_empty(), "no entry outlives its submit");
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
            client_order_id: "c1".to_string(),
            reason: "".into(),
            ts: 5,
        }));
        rows.push(Event::OrderRejected(OrderRejected {
            cause: None,
            client_order_id: "c1".to_string(),
            reason: "".into(),
            ts: 6,
        }));
        rows.push(Event::OrderExpired(OrderExpired { client_order_id: "c1".to_string(), ts: 7 }));
        rows.push(Event::OrderCanceled(OrderCanceled {
            client_order_id: "c2".to_string(),
            reason: "".into(),
            ts: 8,
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
    assert!(fence.held().is_empty());
}

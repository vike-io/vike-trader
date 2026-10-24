//! The fence over a mock `VenueRest` (NO socket, NO thread): the pump's fill is delivered INSIDE the
//! submit, which is the venue pushing the fill before it answers the order. The order's real
//! sockets, the re-dial and the engine fold are `tests/offline/order_event_order.rs`.
use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::json;
use vike_exec::{Ingest, event_channel};
use vike_model::events::{OrderAccepted, OrderSubmitted};

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

/// What the fill stream mints for one filled trade of `coid`: `[Fill, OrderFilled]`.
fn fill_of(coid: &str, trade_id: &str) -> Vec<Event> {
    crate::event_mapper::map_deribit_trade(
        &json!({
            "label": coid, "trade_id": trade_id, "state": "filled", "direction": "sell",
            "instrument_name": "BTC-PERPETUAL", "amount": 10.0, "price": 50_000.0,
            "fee": 0.0, "fee_currency": "BTC", "liquidity": "T", "timestamp": 1_700_000_000_005i64,
        }),
        "deribit",
        "BTC-PERPETUAL",
    )
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

/// The A3 replay's door: `hold` parks every kind the replay mints for the in-flight order (a fill
/// and its wrap, a cancel and a reject row) and hands everything else back to be sent by its producer.
#[test]
fn hold_parks_a_replayed_fill_cancel_and_reject_of_the_held_order_only() {
    let (events, mut drain) = lane();
    let fence = SubmitFence::new(events);
    let history = crate::history::map_deribit_history(
        &json!([
            {"label": "c1", "order_state": "cancelled", "last_update_timestamp": 5},
            {"label": "c1", "order_state": "rejected", "last_update_timestamp": 6},
            {"label": "c2", "order_state": "cancelled", "last_update_timestamp": 7},
        ]),
        &json!([{
            "label": "c1", "trade_id": "t7", "state": "filled", "direction": "sell",
            "instrument_name": "BTC-PERPETUAL", "amount": 10.0, "price": 50_000.0,
            "fee": 0.0, "fee_currency": "BTC", "liquidity": "T", "timestamp": 4,
        }]),
        "deribit",
        "BTC-PERPETUAL",
    );
    assert_eq!(kinds(&history), ["Fill", "OrderFilled", "Canceled", "Rejected", "Canceled"]);

    let mut handed_back = Vec::new();
    fence.submit("c1", || {
        handed_back = history.into_iter().filter_map(|event| fence.hold(event)).collect();
        assert!(drain().is_empty(), "nothing was sent while the submit is open");
        answer("c1")
    });

    assert_eq!(kinds(&handed_back), ["Canceled"], "only c2's cancel is the producer's to send");
    assert_eq!(
        kinds(&drain()),
        ["Submitted", "Accepted", "Fill", "OrderFilled", "Canceled", "Rejected"],
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

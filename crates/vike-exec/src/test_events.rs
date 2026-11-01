//! Event and request builders and finders that the test binaries of several crates each typed for
//! themselves: the plain resting limit order, what an always-accepting scripted client queues for a
//! submit, a cancel event, the first reject and the first bare fill among a batch of events, and
//! the seeded generator the random-walk property tests draw from. Compiled only under `cfg(test)`
//! or the `test-support` feature, and named from `vike_exec::testing` only (one name per symbol).

use std::collections::VecDeque;

use vike_model::events::{Event, FillEvent, OrderAccepted, OrderCanceled, OrderSubmitted};
use vike_model::{OrderRequest, OrderType};

/// The plain resting limit buy the cancel-path engine suites submit: binance `BTCUSDT`, 1 @ 100,
/// `ts` 1. A suite on another venue overrides `venue` with struct-update syntax.
pub fn resting_limit(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    }
}

/// Queues what an always-accepting scripted venue answers a submit with: `OrderSubmitted`, then
/// `OrderAccepted` with no venue id, both stamped with the request's `ts`.
pub fn accept_submit(events: &mut VecDeque<Event>, request: &OrderRequest) {
    events.push_back(Event::OrderSubmitted(OrderSubmitted {
        client_order_id: request.client_order_id.clone(),
        ts: request.ts,
    }));
    events.push_back(Event::OrderAccepted(OrderAccepted {
        client_order_id: request.client_order_id.clone(),
        venue_order_id: None,
        ts: request.ts,
    }));
}

/// An `OrderCanceled` for `coid` carrying `reason` at `ts`.
pub fn canceled(coid: &str, reason: &str, ts: i64) -> Event {
    Event::OrderCanceled(OrderCanceled { client_order_id: coid.into(), reason: reason.into(), ts })
}

/// The reason of the first `OrderRejected` among `events`, if there is one.
pub fn rejection(events: &[Event]) -> Option<String> {
    events.iter().find_map(|e| match e {
        Event::OrderRejected(r) => Some(r.reason.to_string()),
        _ => None,
    })
}

/// A tiny deterministic generator (an LCG) seeded by its field, so a failing walk replays from its
/// seed and needs no crate.
pub struct Lcg(pub u64);

impl Lcg {
    /// The next draw: the state's high 31 bits after one step (Knuth's constants).
    pub fn next_u64(&mut self) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    /// The next draw, in `0..bound`.
    pub fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
}

/// The first bare `Fill` among `events`; panics naming them when there is none.
pub fn bare_fill(events: &[Event]) -> &FillEvent {
    events
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a bare Fill among {events:?}"))
}

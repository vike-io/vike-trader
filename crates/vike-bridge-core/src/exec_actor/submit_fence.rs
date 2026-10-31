//! The fence between an order's own answer and the venue's fills for that order.
//!
//! ## The race it closes
//!
//! [`run_loop`](super::run_loop) sends an order's `[OrderSubmitted, OrderAccepted]` only AFTER
//! [`VenueRest::submit_order`] has returned: the whole venue round trip. The venue's private stream
//! (the user-data pump) runs on its own thread and its own socket and sends a fill the moment it
//! arrives. An order that fills at once (a market order, a flatten) can therefore have its fill pushed
//! by the venue before the order's reply is read, and the core then sees `Fill`, `OrderFilled`,
//! `OrderSubmitted`, `OrderAccepted`.
//!
//! The engine's order table refuses `OrderFilled` on an order that is not `Accepted` yet and keeps it
//! (`vike_model::ManagedOrder`'s `defer_refused`), but at the time of the incident replayed what it kept
//! only when a bare `OrderAccepted` landed on an order still `Initialized`
//! (`crates/vike-exec/src/execution_engine/fold.rs`'s `land_lost_prefix_answer`). With `OrderSubmitted`
//! in between, the order walks `Submitted` and `Accepted` the ordinary way and rested `ACCEPTED` with
//! nothing filled while the venue held it filled and the position was flat (the CI box 2026-10-24, a Deribit
//! market sell: `terminal event dropped on a live order`, then a held `OrphanLocalOrder` alert). The
//! engine now replays at the ordinary accept too, but only for a fill that came BEFORE `OrderSubmitted`:
//! a fill between the bridge's two sends is refused on a `Submitted` order and not kept. So the cure
//! stays here: the bridge never produces the inverted order.
//!
//! ## What it does
//!
//! [`SubmitFence::submit`] marks the order BEFORE its frame can leave, runs the submit, sends the
//! order's own events and only then lets the held ones go, in the order they arrived.
//! [`SubmitFence::hold`] is the door every OTHER producer goes through: an event of a held order waits,
//! every other event comes back to its producer to be sent. There are two producers beside the exec
//! thread, and a bridge wires BOTH (its `spawn_*_user_data_with_resync` takes the fence, so neither can
//! be forgotten):
//!
//! * the user-data pump's sink, [`SubmitFence::deliver`] (`hold`, then send what comes back);
//! * the audit-A3 reconnect replay, whose fetch is wrapped by [`SubmitFence::fenced_fetch`] so its output
//!   goes through `hold`. The replay is NOT "long after a submit has been answered": one network blip
//!   can kill the order socket and the fill socket together, and the supervisor replays the account's
//!   recent history (`Fill`/`OrderFilled` of the order that just filled, `OrderCanceled`/`OrderRejected`
//!   rows) one settle after the stream re-opens, while the exec thread is still inside the submit.
//!
//! So the core always sees `OrderSubmitted`, `OrderAccepted`, then the fills, whoever wins the race at
//! the venue. The same shape as `crates/bridges/oanda/src/symbols.rs`'s `SubmittedSymbols`
//! (`hold`/`park`/`release`), which closes this race for OANDA.
//!
//! ## Who wires it, and who does not
//!
//! Wired: deribit, bybit and okx (`vike_deribit::user_data::spawn_deribit_user_data_with_resync` and
//! its two siblings; `crates/vike-bridge-core/tests/bridge_conformance.rs`'s `SubmitFenceRow` is the
//! roster, with a reason for every venue that is not). Their private streams can deliver a fill with NO accept of their own ahead
//! of it (deribit's stream is fills only; bybit's `order` and `execution` are separate topics). okx's
//! `orders` channel is not that shape on the live pump: it pushes a `live` row for a MARKET order too,
//! then its `filled` row in the same millisecond (measured on the demo, eight market orders on 2026-10-28
//! and 2026-10-31, the `live` row first every time), so
//! the pump's own bare `OrderAccepted` usually lands first and the fence holds the fill behind it. What
//! keeps okx wired is the fill's race with the exec thread's own two sends, and the A3 replay, whose
//! history rows carry no `live` row at all. Binance and aster are NOT wired on purpose:
//! their stream pushes the `NEW` report of an order before its first trade, on one ordered socket, so
//! the pump's own bare `OrderAccepted` (which names the venue order id) lands first and the engine takes
//! it (`ManagedOrder::apply_over_lost_prefix`); holding their fills would only add latency.
//!
//! ## What it does not do
//!
//! * It does not change the engine, [`run_loop`](super::run_loop) or any shared pump: [`FencedRest`] is a
//!   plain `VenueRest` over the bridge's own client, and the replay's fetch is wrapped from outside.
//! * The run-loop fill sentinel is not behind it, and does not need to be: it runs on the exec thread
//!   itself, between commands, so it can never overlap a submit. The `confirm` worker
//!   ([`ExecActor::with_confirm`](super::ExecActor::with_confirm)) sends straight to the lane and is not
//!   behind it either.
//! * A held replay event skips the supervisor's restart-law floor (it is sent by the exec thread's
//!   release, not by the supervisor). That cannot lose anything: only an order THIS process has in flight
//!   is ever held, and nothing it placed can predate the process.
//! * A held event waits for the submit to return (a venue's reply wait, dial and re-query are each
//!   bounded by its transport); it is released even if the submit panics.
//! * The pump's own `OrderAccepted` is not held (it names the venue order id and the engine takes it
//!   ahead of `OrderSubmitted`, which is what happens today), nor is any event without a client order id.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vike_exec::EventSender;
use vike_model::OrderRequest;
use vike_model::events::Event;

use crate::rest::VenueRest;
use crate::transport::VenueApiError;

/// The client order id a held-able event belongs to: the kinds an order's fills and the A3 replay mint
/// AFTER an accept (a fill, its wrap, a terminal that closes the order). The engine defers exactly the
/// wraps and terminals among them (`ManagedOrder::defer_refused`).
fn held_coid(event: &Event) -> Option<&str> {
    match event {
        Event::Fill(f) => Some(f.client_order_id.as_str()),
        Event::OrderPartiallyFilled(w) => Some(w.client_order_id.as_str()),
        Event::OrderFilled(w) => Some(w.client_order_id.as_str()),
        Event::OrderCanceled(w) => Some(w.client_order_id.as_str()),
        Event::OrderRejected(w) => Some(w.client_order_id.as_str()),
        Event::OrderExpired(w) => Some(w.client_order_id.as_str()),
        // no producer beside the exec thread mints anything else for an order in flight, and the exec
        // thread's own events never pass through here
        _ => None,
    }
}

/// See the module doc. One per exec start, shared by the exec thread ([`FencedRest`]), the
/// user-data pump and the reconnect replay ([`SubmitFence::hold`]); `events` is the lane they feed.
pub struct SubmitFence {
    events: EventSender,
    /// coid -> the other producers' events for it while its submit is unanswered, oldest first. An
    /// entry exists exactly while [`SubmitFence::submit`] runs for that coid.
    held: Mutex<HashMap<String, Vec<Event>>>,
}

impl SubmitFence {
    pub fn new(events: EventSender) -> Self {
        Self { events, held: Mutex::new(HashMap::new()) }
    }

    fn held(&self) -> MutexGuard<'_, HashMap<String, Vec<Event>>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Park `event` if its order's submit is in flight (`None`: it will be sent after the order's own
    /// events), else hand it back (`Some`) for its producer to send. The door of every producer
    /// beside the exec thread; the lock is never held across a send, so a producer blocked on a full
    /// lane cannot stop the exec thread from marking the next order.
    pub fn hold(&self, event: Event) -> Option<Event> {
        let mut held = self.held();
        if let Some(parked) = held_coid(&event).and_then(|coid| held.get_mut(coid)) {
            parked.push(event);
            return None;
        }
        Some(event)
    }

    /// How many events of `coid` are parked right now: `0` unless its submit is in flight. A test's
    /// handshake ("the stream's fill has reached the fence") reads it; nothing in production does.
    pub fn parked(&self, coid: &str) -> usize {
        self.held().get(coid).map_or(0, Vec::len)
    }

    /// The pump's sink: [`Self::hold`], then send what comes back. `false` = the core has gone (the
    /// pump's stop signal), as `EventSender::blocking_send` answers it.
    pub fn deliver(&self, event: Event) -> bool {
        match self.hold(event) {
            Some(event) => self.events.blocking_send(event).is_ok(),
            None => true,
        }
    }

    /// `fetch` (a venue's A3 history replay) with its output run through [`Self::hold`]: what comes back
    /// is for the supervisor to send, what was parked goes out behind its order's own events.
    pub fn fenced_fetch(
        self: &Arc<Self>,
        mut fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    ) -> Box<dyn FnMut() -> Vec<Event> + Send> {
        let fence = Arc::clone(self);
        Box::new(move || fetch().into_iter().filter_map(|event| fence.hold(event)).collect())
    }

    /// Run `submit` (the venue round trip for `coid`) with the other producers' events for `coid`
    /// held, then send the order's own events and, after them, whatever was held. The held events are
    /// released even when `submit` panics (the order's own events are then lost with it, as before).
    pub fn submit(&self, coid: &str, submit: impl FnOnce() -> Vec<Event>) {
        self.held().insert(coid.to_string(), Vec::new());
        let mut release = Release { fence: self, coid, own: Vec::new() };
        release.own = submit();
        // `release` drops here: the order's own events first, then the held ones.
    }
}

/// Sends the order's own events, then drains what was held for it, in `Drop` so a panicking submit
/// still lets the held events go.
struct Release<'a> {
    fence: &'a SubmitFence,
    coid: &'a str,
    own: Vec<Event>,
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        // `CoreGone` (the core has exited) = shutting down: nothing to deliver to.
        for event in self.own.drain(..) {
            let _ = self.fence.events.blocking_send(event);
        }
        let mut released = 0usize;
        // The entry stays until a pass finds it empty, so an event held while a batch is being sent
        // is picked up by the next pass and none can overtake the batch.
        loop {
            let batch = {
                let mut held = self.fence.held();
                match held.get_mut(self.coid) {
                    Some(parked) if !parked.is_empty() => std::mem::take(parked),
                    _ => {
                        held.remove(self.coid);
                        break;
                    }
                }
            };
            released += batch.len();
            for event in batch {
                let _ = self.fence.events.blocking_send(event);
            }
        }
        if released > 0 {
            tracing::info!(
                target: "vike_bridge_core::submit_fence",
                coid = self.coid,
                released,
                "the venue's fill (or a replay row) reached the lane before the order's own answer; held it and sent it after OrderSubmitted/OrderAccepted"
            );
        }
    }
}

/// `R` (the exec thread's REST client) with every submit run through a [`SubmitFence`].
///
/// ⚠ **`submit_order` SENDS the order's events itself and returns none**: the fence has to send them
/// before it lets the held ones go, and `run_loop` would send what is returned only afterwards. Every
/// other method is `R`'s own; a method added to `VenueRest` and not forwarded here would silently
/// inherit the trait default, so each is forwarded by hand. `submit_batch` is the trait default (one
/// fenced `submit_order` per order, each sending its own events, so the batch returns none): `run_loop`
/// never calls it, and a native batch endpoint behind `R` would be bypassed by it.
pub struct FencedRest<'a, R> {
    rest: &'a R,
    fence: &'a SubmitFence,
}

impl<'a, R> FencedRest<'a, R> {
    pub fn new(rest: &'a R, fence: &'a SubmitFence) -> Self {
        Self { rest, fence }
    }
}

impl<R: VenueRest + Sync> VenueRest for FencedRest<'_, R> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        self.fence.submit(&request.client_order_id, || self.rest.submit_order(request));
        Vec::new()
    }
    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        self.rest.cancel_order(client_order_id)
    }
    fn confirm_order(&self, client_order_id: &str, ts: i64) -> Vec<Event> {
        self.rest.confirm_order(client_order_id, ts)
    }
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        self.rest.modify_order(order, new_qty, new_price)
    }
    fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        self.rest.cancel_batch(client_order_ids)
    }
}

#[cfg(test)]
#[path = "submit_fence_tests.rs"]
mod submit_fence_tests;

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
//! (`vike_model::OrderRecord`'s `defer_refused`), but at the time of the incident replayed what it kept
//! only when a bare `OrderAccepted` landed on an order still `Initialized`
//! (`crates/vike-execution/src/execution_engine/fold.rs`'s `land_lost_prefix_answer`). With `OrderSubmitted`
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
//! order's own events and only then lets the held ones go, in the order they arrived. An exec thread of a
//! venue's own that sends its order's events itself, from a submit arm that leaves by many paths (OANDA's and
//! IG's loops), holds [`SubmitFence::enter`]'s guard over the arm instead: the held ones go when it drops,
//! after whatever the arm sent. A venue that keeps per-order state the producer it held an event from would have
//! updated on sending it takes [`SubmitFence::enter_with`], whose hook runs on each held event as it goes (OANDA's
//! submitted-symbol table).
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
//! So the core sees the order's own answer, then the fills, whoever wins the race at the venue. It took over the
//! hand-written queue OANDA's submitted-symbol table (`crates/bridges/oanda/src/execution/submitted_symbols.rs`) kept (owner ruling B-17),
//! with its cap: one order keeps back at most `MAX_PARKED` events, and the next goes at once, out of order, because a
//! lost fill is worse than an early one.
//!
//! ## An order with no answer
//!
//! A submit whose POST got no answer and whose re-query failed or found nothing returns `OrderSubmitted` alone
//! (decision 0171's amendment), and its fill can still arrive: the venue took the order after all. The fence
//! remembers it ([`super::SilentOrders`]) and that first fill wrap, held or not, goes behind the accept it implies
//! ([`crate::event_map::accept_implied_by_fill`]), so a bracket's entry releases its stop. The run-loop sentinel's
//! replay asks the same list ([`VenueRest::accept_implied_by_replay`]). An order that never fills says nothing, and the
//! core's backstop ends it at 60 to 75 s.
//!
//! ## Who wires it, and who does not
//!
//! Wired: deribit, bybit, okx, binance and aster (`vike_deribit::execution::user_data::spawn_deribit_user_data_with_resync` and
//! its siblings; `crates/vike-bridge-core/tests/bridge_conformance.rs`'s `SubmitFenceRow` is the
//! roster, with a reason for every venue that is not). The first three's private streams can deliver a fill with NO accept of their own ahead
//! of it (deribit's stream is fills only; bybit's `order` and `execution` are separate topics). okx's
//! `orders` channel is not that shape on the live pump: it pushes a `live` row for a MARKET order too,
//! then its `filled` row in the same millisecond (measured on the demo, eight market orders on 2026-10-28
//! and 2026-10-31, the `live` row first every time), so
//! the pump's own bare `OrderAccepted` usually lands first and the fence holds the fill behind it. What
//! keeps okx wired is the fill's race with the exec thread's own two sends, and the A3 replay, whose
//! history rows carry no `live` row at all. Binance and aster are wired for the replay too:
//! their stream pushes the `NEW` report of an order before its first trade, on one ordered socket, so
//! the pump's own bare `OrderAccepted` (which names the venue order id) usually lands first and the engine takes
//! it (`OrderRecord::apply_over_lost_prefix`), but one outage can take the POST's answer, the re-query, `NEW`
//! and the sentinel's read, and then the replay's wrap needs the accept only the fence knows to imply. A fill that
//! beats the answer now waits for it, at most one POST; nothing on their wire changes.
//!
//! ## What it does not do
//!
//! * It does not change the engine, [`run_loop`](super::run_loop) or any shared pump: [`VenueRestOrderSubmitFence`] is a
//!   plain `VenueRest` over the bridge's own client, and the replay's fetch is wrapped from outside.
//! * The run-loop fill sentinel is not behind it, and does not need to be: it runs on the exec thread
//!   itself, between commands, so it can never overlap a submit. The `confirm` worker
//!   ([`ExecutionClientActor::with_confirm`](super::ExecutionClientActor::with_confirm)) sends straight to the lane and is not
//!   behind it either.
//! * A held replay event skips the supervisor's restart-law floor (it is sent by the exec thread's
//!   release, not by the supervisor). That cannot lose anything: only an order THIS process has in flight
//!   is ever held, and nothing it placed can predate the process.
//! * A held event waits for the submit to return (a venue's reply wait, dial and re-query are each
//!   bounded by its transport); it is released even if the submit panics.
//! * The pump's own `OrderAccepted` is not held (it names the venue order id and the engine takes it
//!   ahead of `OrderSubmitted`, which is what happens today), nor is any event without an order client id.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vike_execution::EventSender;
use vike_model::OrderRequest;
use vike_model::events::Event;

use crate::rest::VenueRest;
use crate::transport::VenueApiError;

/// The order client id a held-able event belongs to: the kinds an order's fills and the A3 replay mint
/// AFTER an accept (a fill, its wrap, a terminal that closes the order). The engine defers exactly the
/// wraps and terminals among them (`OrderRecord::defer_refused`).
fn held_order_client_id(event: &Event) -> Option<&str> {
    match event {
        Event::Fill(f) => Some(f.order_client_id.as_str()),
        Event::OrderPartiallyFilled(w) => Some(w.order_client_id.as_str()),
        Event::OrderFilled(w) => Some(w.order_client_id.as_str()),
        Event::OrderCanceled(w) => Some(w.order_client_id.as_str()),
        Event::OrderRejected(w) => Some(w.order_client_id.as_str()),
        Event::OrderExpired(w) => Some(w.order_client_id.as_str()),
        // no producer beside the exec thread mints anything else for an order in flight, and the exec
        // thread's own events never pass through here
        _ => None,
    }
}

/// The most events [`SubmitFence::hold`] keeps back for one unanswered submit (OANDA's own fence's cap, which the
/// owner ruled into the shared one with B-17). Past it an event goes back to its producer at once, out of order, rather
/// than wait: a lost fill is worse than an early one, and a fence that grows without bound is a leak.
const MAX_PARKED: usize = 256;

/// What the fence's one lock guards: a submit's end moves its order from one to the other in one step.
#[derive(Default)]
struct Fenced {
    /// order client id -> the other producers' events for it while its submit is unanswered, oldest first. An
    /// entry exists exactly while [`SubmitFence::submit`] runs for that order client id.
    held: HashMap<String, Vec<Event>>,
    silent: super::SilentOrders,
}

/// See the module doc. One per exec start, shared by the exec thread ([`VenueRestOrderSubmitFence`]), the
/// user-data pump and the reconnect replay ([`SubmitFence::hold`]); `events` is the lane they feed.
pub struct SubmitFence {
    events: EventSender,
    state: Mutex<Fenced>,
}

impl SubmitFence {
    pub fn new(events: EventSender) -> Self {
        Self { events, state: Mutex::new(Fenced::default()) }
    }

    fn state(&self) -> MutexGuard<'_, Fenced> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Park `event` if its order's submit is in flight (`None`: it will be sent after the order's own
    /// events), else hand it back (`Some`) for its producer to send, behind the accept it implies when it is the first
    /// fill of an order with no answer (sent here). An order already holding [`MAX_PARKED`] events parks no more. The
    /// door of every producer beside the exec thread; the lock is never held across a send, so a producer blocked on a
    /// full lane cannot stop the exec thread from marking the next order.
    pub fn hold(&self, event: Event) -> Option<Event> {
        let mut state = self.state();
        if let Some(parked) = held_order_client_id(&event)
            .and_then(|order_client_id| state.held.get_mut(order_client_id))
            && parked.len() < MAX_PARKED
        {
            parked.push(event);
            return None;
        }
        let accept = state.silent.accept_for(&event);
        drop(state);
        if let Some(accept) = accept {
            // `CoreGone` (the core has exited) = shutting down: nothing to deliver to.
            let _ = self.events.blocking_send(accept);
        }
        Some(event)
    }

    /// How many events of `order_client_id` are parked right now: `0` unless its submit is in flight. A test's
    /// handshake ("the stream's fill has reached the fence") reads it; nothing in production does.
    pub fn parked(&self, order_client_id: &str) -> usize {
        self.state().held.get(order_client_id).map_or(0, Vec::len)
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

    /// Run `submit` (the venue round trip for `order_client_id`) with the other producers' events for `order_client_id`
    /// held, then send the order's own events and, after them, whatever was held. The held events are
    /// released even when `submit` panics (the order's own events are then lost with it, as before).
    pub fn submit(&self, order_client_id: &str, submit: impl FnOnce() -> Vec<Event>) {
        let mut release = self.release_on_drop(order_client_id);
        release.own = submit();
        release.read_own = true;
        // `release` drops here: the order's own events first, then the held ones.
    }

    /// The fence for an exec thread that is not [`run_loop`](super::run_loop) and sends its order's own events
    /// itself, wherever its submit arm leaves (a refusal, an ambiguous outcome, the POST's answer, a panic): from
    /// here the other producers' events for `order_client_id` wait, and dropping what this returns lets them go, after
    /// whatever the arm sent. Bind it to a name for the arm's whole length (`let _fenced = fence.enter(order client id);`):
    /// `let _ = ...` drops it at once and fences nothing. The fence does not see the arm's own events, so it
    /// implies no accept for an order the arm left unanswered: saying so stays the arm's.
    #[must_use = "the fence lasts while this lives: bind it to a name for the whole submit arm"]
    pub fn enter<'a>(&'a self, order_client_id: &'a str) -> impl Sized + 'a {
        self.release_on_drop(order_client_id)
    }

    /// [`Self::enter`], with `on_release` run on each held event as it is let go, just before it is sent: the hook a
    /// venue keeps per-order state through (`crates/bridges/oanda/src/execution/submitted_symbols.rs`'s `settle` drops what it kept for an order that
    /// event ends, which the producer it held the event from would have run on sending it).
    #[must_use = "the fence lasts while this lives: bind it to a name for the whole submit arm"]
    pub fn enter_with<'a>(
        &'a self,
        order_client_id: &'a str,
        on_release: impl Fn(&Event) + 'a,
    ) -> impl Sized + 'a {
        let mut release = self.release_on_drop(order_client_id);
        release.on_release = Box::new(on_release);
        release
    }

    /// Mark `order_client_id` held, and hand back what lets its held events go when it drops.
    fn release_on_drop<'a>(&'a self, order_client_id: &'a str) -> Release<'a> {
        self.state().held.insert(order_client_id.to_string(), Vec::new());
        Release {
            fence: self,
            order_client_id,
            own: Vec::new(),
            read_own: false,
            on_release: Box::new(|_| {}),
        }
    }

    /// [`VenueRest::accept_implied_by_replay`] for the client this fence wraps.
    fn accept_implied_by_replay(&self, event: &Event) -> Option<Event> {
        self.state().silent.accept_for(event)
    }
}

/// Sends the order's own events, then drains what was held for it, in `Drop` so a panicking submit
/// still lets the held events go.
struct Release<'a> {
    fence: &'a SubmitFence,
    order_client_id: &'a str,
    own: Vec<Event>,
    /// The fence read the order's own events: [`SubmitFence::submit`]'s came back. A panic is no answer it can
    /// read (its own events are lost with it), and an arm under [`SubmitFence::enter`] sends its own.
    read_own: bool,
    /// [`SubmitFence::enter_with`]'s hook, run on each held event just before it is sent.
    on_release: Box<dyn Fn(&Event) + 'a>,
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        // Still without a word from the venue: `OrderSubmitted` alone came back and no fill has said otherwise.
        let mut silent = self.read_own && super::no_answer(&self.own);
        // `CoreGone` (the core has exited) = shutting down: nothing to deliver to.
        for event in self.own.drain(..) {
            let _ = self.fence.events.blocking_send(event);
        }
        let mut released = 0usize;
        // The entry stays until a pass finds it empty, so an event held while a batch is being sent
        // is picked up by the next pass and none can overtake the batch.
        loop {
            let batch = {
                let mut state = self.fence.state();
                match state.held.get_mut(self.order_client_id) {
                    Some(parked) if !parked.is_empty() => std::mem::take(parked),
                    _ => {
                        state.held.remove(self.order_client_id);
                        // One step with the removal: no fill finds the order neither held nor remembered.
                        if silent {
                            state.silent.note(self.order_client_id);
                        }
                        break;
                    }
                }
            };
            released += batch.len();
            for event in batch {
                let implied = silent
                    .then(|| crate::event_map::accept_implied_by_fill(&event, false))
                    .flatten();
                if let Some(accept) = implied {
                    silent = false;
                    let _ = self.fence.events.blocking_send(accept);
                }
                (self.on_release)(&event);
                let _ = self.fence.events.blocking_send(event);
            }
        }
        if released > 0 {
            tracing::info!(
                target: "vike_bridge_core::submit_fence",
                order_client_id = self.order_client_id,
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
pub struct VenueRestOrderSubmitFence<'a, R> {
    rest: &'a R,
    fence: &'a SubmitFence,
}

impl<'a, R> VenueRestOrderSubmitFence<'a, R> {
    pub fn new(rest: &'a R, fence: &'a SubmitFence) -> Self {
        Self { rest, fence }
    }
}

impl<R: VenueRest + Sync> VenueRest for VenueRestOrderSubmitFence<'_, R> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        self.fence.submit(&request.order_client_id, || self.rest.submit_order(request));
        Vec::new()
    }
    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<crate::rest::OrderCancelAck, VenueApiError> {
        self.rest.cancel_order(order_client_id)
    }
    fn confirm_order(&self, order_client_id: &str, ts: i64) -> Vec<Event> {
        self.rest.confirm_order(order_client_id, ts)
    }
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        self.rest.modify_order(order, new_qty, new_price)
    }
    fn modify_trigger(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Vec<Event> {
        self.rest.modify_trigger(order, new_qty, new_price, new_trigger_price)
    }
    fn cancel_batch(&self, order_client_ids: &[String]) -> Result<(), VenueApiError> {
        self.rest.cancel_batch(order_client_ids)
    }
    fn accept_implied_by_replay(&self, event: &Event) -> Option<Event> {
        self.fence.accept_implied_by_replay(event)
    }
}

#[cfg(test)]
#[path = "submit_fence_tests.rs"]
mod submit_fence_tests;

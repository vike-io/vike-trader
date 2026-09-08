//! EventBus — defer-and-deliver FIFO. Exact port of `exec/bus.py` re-entrancy semantics.
//!
//! Python: `publish` appends to a deque; if a drain is already running the call returns
//! (the event is DEFERRED to the back of the FIFO); otherwise it drains, delivering each
//! event to every subscriber before the next event. That ordering is load-bearing: OCO
//! auto-cancel publishes `OrderCanceled` while `OrderFilled` is being delivered, and the
//! cancel must land AFTER every subscriber saw the fill.
//!
//! Rust twin: the single-writer core owns bus + handler, so callback re-entrancy is
//! impossible by construction — instead a handler pushes follow-on events into an
//! [`Outbox`], and the drain loop appends them to the back of the same FIFO. Byte-identical
//! delivery order, borrow-checker-clean.

use std::collections::VecDeque;
use vike_model::events::Event;

/// Events a handler publishes while handling an event (Python: `bus.publish` inside a drain).
#[derive(Debug, Default)]
pub struct Outbox(pub VecDeque<Event>);

impl Outbox {
    pub fn publish(&mut self, event: Event) {
        self.0.push_back(event);
    }
}

/// What a handler DID with an event — the fold's verdict, reported to its caller.
///
/// "Invalid transitions are dropped" is a core law of the OMS, but before this existed the drop was
/// INVISIBLE to the caller: [`EventHandler::on_event`] returned `()`, so a downstream reaction keyed
/// off the EVENT rather than off the fold's acceptance of it, and therefore fired for events the
/// engine had refused. `vike_core`'s live OTO/OCO drive was exactly that — a fabricated
/// `OrderFilled` on an unknown coid cancelled its OCO sibling and armed its held OTO children even
/// though `ExecutionEngine::on_event` had dropped the event and counted it into
/// `dropped_unknown_coid`.
///
/// A handler answers [`Applied`](Self::Applied) only when the event actually changed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// The handler folded the event into its state — a legitimate transition.
    Applied,
    /// The handler DROPPED it and state is unchanged: an unknown client-order-id, an illegal FSM
    /// transition, a reconnect replay, an event for a symbol/venue this engine does not own, or a
    /// non-finite venue number. Nothing downstream may treat a dropped event as having happened.
    Dropped,
}

/// The single logical subscriber of the core loop (the engine; R7 adds the router in front).
pub trait EventHandler {
    /// Fold `event` into state, reporting whether it was [`Fold::Applied`] or [`Fold::Dropped`].
    fn on_event(&mut self, event: &Event, outbox: &mut Outbox) -> Fold;
}

/// FIFO event queue with Python's defer-and-deliver drain.
#[derive(Debug, Default)]
pub struct EventBus {
    queue: VecDeque<Event>,
    /// re-entrancy latch — structurally unreachable in the single-writer core, kept so the
    /// semantics stay explicit (and exercised by the parity test's chained publisher)
    draining: bool,
    /// every event delivered, in order — the GUI echo seam (R5b taps this into the
    /// lossy observer channel) and the fixtures' delivery-order assertion
    pub delivered: Vec<Event>,
    /// events that were mid-fold when a handler panicked (audit C4) — salvaged by the drain
    /// guard instead of being dropped, so the runtime can journal the lost terminal.
    poisoned: Vec<Event>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the delivery log (runtime folds it into the bounded recent-events ring each
    /// batch — the log itself must not grow unbounded on a live core).
    pub fn take_delivered(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.delivered)
    }

    /// Take the events salvaged from a mid-fold handler panic (audit C4). The runtime drains this
    /// after its `catch_unwind` fires and journals each as a lost terminal, so a panic can never
    /// silently strand an order. Empty on the happy path.
    pub fn take_poisoned(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.poisoned)
    }

    /// Publish + drain: delivers `event` (and everything it transitively publishes) to
    /// `handler` in FIFO order. Follow-on events from the handler's outbox are appended to
    /// the BACK of the queue — delivered after all already-queued events, never recursively.
    ///
    /// Returns the [`Fold`] verdict for **the event passed in**, so a caller that reacts to an
    /// event can condition on the engine having actually accepted it. Follow-on events drained by
    /// the same call have their own verdicts, which are not reported (no caller reacts to them; the
    /// site that publishes them is the one that would).
    ///
    /// ⚠ THE DEFERRED BRANCH answers [`Fold::Applied`]. It is structurally unreachable in the
    /// single-writer core — `draining` can only be set by a `publish` higher in the same call
    /// stack, and `ExecutionEngine::on_event` publishes into its `Outbox` rather than back into the
    /// bus — so the branch exists only for the parity test's chained publisher. Answering `Applied`
    /// there keeps that path byte-identical to its pre-verdict behavior; answering `Dropped` would
    /// invent a new way to orphan a legitimate bracket on a path nobody exercises. A caller that
    /// genuinely needs the verdict of a DEFERRED event must not use this seam.
    ///
    /// UNWIND-SAFE like Python's `try/finally`: if a handler panics mid-drain the latch is
    /// cleared on the way out (drop guard), so after the runtime's catch_unwind the next
    /// publish drains normally — undelivered events stay queued, exactly the Python
    /// semantics. Without this one in-drain panic would silently defer every later event
    /// forever (review finding, 2026-07-03).
    pub fn publish<H: EventHandler>(&mut self, event: Event, handler: &mut H) -> Fold {
        self.queue.push_back(event);
        if self.draining {
            return Fold::Applied; // deferred — the running drain will deliver it (see the ⚠ above)
        }
        self.draining = true;
        // The guard both clears the latch AND salvages the event being folded if `on_event` panics
        // (audit C4): `in_flight` holds the current event during the fold; on a clean return the
        // drain loop `take()`s it into `delivered`, so anything left in `in_flight` at Drop time was
        // lost to a panic and is pushed to `poisoned`.
        struct DrainGuard<'a> {
            draining: &'a mut bool,
            poisoned: &'a mut Vec<Event>,
            in_flight: Option<Event>,
        }
        impl Drop for DrainGuard<'_> {
            fn drop(&mut self) {
                *self.draining = false;
                if let Some(ev) = self.in_flight.take() {
                    self.poisoned.push(ev);
                }
            }
        }
        let mut guard = DrainGuard {
            draining: &mut self.draining,
            poisoned: &mut self.poisoned,
            in_flight: None,
        };
        // The verdict for the event this call was handed. It is the FIRST one popped (the queue was
        // empty — `draining` was false — so `push_back` above put it at the head), and only that
        // first verdict is kept; follow-on outbox events belong to their own publishers.
        let mut first: Option<Fold> = None;
        while let Some(ev) = self.queue.pop_front() {
            guard.in_flight = Some(ev);
            let mut outbox = Outbox::default();
            let fold = handler.on_event(guard.in_flight.as_ref().unwrap(), &mut outbox);
            first.get_or_insert(fold);
            // clean fold — move the delivered event out of in_flight so Drop won't poison it
            self.delivered.push(guard.in_flight.take().unwrap());
            self.queue.append(&mut outbox.0); // defer-and-deliver: handler publishes go to the back
        }
        // `None` is unreachable (the loop always pops at least the event just pushed); it would mean
        // nothing was folded, for which `Applied` would be the wrong answer.
        first.unwrap_or(Fold::Dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::OrderSubmitted;

    struct PanicOn(&'static str);
    impl EventHandler for PanicOn {
        fn on_event(&mut self, event: &Event, _outbox: &mut Outbox) -> Fold {
            if let Event::OrderSubmitted(e) = event {
                assert_ne!(e.client_order_id, self.0, "boom");
            }
            Fold::Applied
        }
    }

    /// Folds anything whose coid is not `"drop-me"`, so a caller can observe both verdicts.
    struct DropsOne;
    impl EventHandler for DropsOne {
        fn on_event(&mut self, event: &Event, _outbox: &mut Outbox) -> Fold {
            match event {
                Event::OrderSubmitted(e) if e.client_order_id == "drop-me" => Fold::Dropped,
                _ => Fold::Applied,
            }
        }
    }

    fn sub(coid: &str) -> Event {
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts: 0 })
    }

    /// `publish` reports the handler's verdict for the event it was HANDED, which is what lets a
    /// caller keep a downstream reaction (the live OTO/OCO drive) off a dropped event.
    #[test]
    fn publish_reports_the_handlers_verdict_for_the_published_event() {
        let mut bus = EventBus::new();
        let mut handler = DropsOne;
        assert_eq!(bus.publish(sub("keep"), &mut handler), Fold::Applied);
        assert_eq!(bus.publish(sub("drop-me"), &mut handler), Fold::Dropped);
    }

    /// The verdict belongs to the PUBLISHED event, never to a follow-on the handler queued: a
    /// handler that folds the first event and drops a chained one must still report `Applied`.
    #[test]
    fn the_verdict_is_the_published_events_not_a_follow_ons() {
        struct Chains(bool);
        impl EventHandler for Chains {
            fn on_event(&mut self, event: &Event, outbox: &mut Outbox) -> Fold {
                match event {
                    Event::OrderSubmitted(e) if e.client_order_id == "head" => {
                        outbox.publish(sub("tail"));
                        Fold::Applied
                    }
                    _ => {
                        self.0 = true; // the follow-on WAS delivered
                        Fold::Dropped
                    }
                }
            }
        }
        let mut bus = EventBus::new();
        let mut handler = Chains(false);
        assert_eq!(bus.publish(sub("head"), &mut handler), Fold::Applied);
        assert!(handler.0, "the chained follow-on must still be delivered");
        assert_eq!(bus.take_delivered().len(), 2, "both events delivered by the one drain");
    }

    /// A panic mid-fold must SALVAGE the in-flight event (audit C4) instead of dropping it — a
    /// lost terminal OrderFilled/Canceled/Rejected would otherwise strand the FSM forever. The
    /// event lands in `poisoned` for the runtime to journal, and the bus keeps delivering.
    #[test]
    fn panic_mid_fold_salvages_the_in_flight_event() {
        let mut bus = EventBus::new();
        let mut handler = PanicOn("boom");
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bus.publish(sub("boom"), &mut handler);
        }));
        assert!(res.is_err(), "handler panic must propagate");
        let poisoned: Vec<String> = bus
            .take_poisoned()
            .into_iter()
            .map(|ev| match ev {
                Event::OrderSubmitted(e) => e.client_order_id,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(poisoned, vec!["boom"], "the panicking event must be salvaged, not lost");
        // Bus keeps working, and the salvaged event is NOT re-folded (moved out of the queue).
        bus.publish(sub("after"), &mut handler);
        let delivered: Vec<String> = bus
            .take_delivered()
            .into_iter()
            .map(|ev| match ev {
                Event::OrderSubmitted(e) => e.client_order_id,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(delivered, vec!["after"]);
    }

    /// An in-drain handler panic must clear the latch (Python's finally) so the bus keeps
    /// delivering afterwards — the runtime's panic policy depends on it.
    #[test]
    fn draining_latch_survives_handler_panic() {
        let mut bus = EventBus::new();
        let mut handler = PanicOn("boom");
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bus.publish(sub("boom"), &mut handler);
        }));
        assert!(poisoned.is_err(), "handler panic must propagate to the runtime guard");
        bus.publish(sub("after"), &mut handler);
        let delivered: Vec<String> = bus
            .take_delivered()
            .into_iter()
            .map(|ev| match ev {
                Event::OrderSubmitted(e) => e.client_order_id,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(delivered, vec!["after"], "bus must keep delivering after a panic");
    }
}

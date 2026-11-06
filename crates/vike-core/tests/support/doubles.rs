//! Test doubles: execution clients and strategies, each with exactly the
//! behaviour its doc names.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vike_core::LiveBroker;
use vike_exec::ExecutionClient;
use vike_exec::testing::accept_submit;
use vike_marketdata::{Bar, QuoteTick};
use vike_model::events::{Event, OrderCanceled, OrderModified};
use vike_model::{Fill, OrderRequest, Strategy};

/// Accepts every order (so it rests, modifiable) and echoes `OrderModified` on modify — enough to
/// drive the runtime's tag→coid resolution, `engine.modify_order`, and the FSM self-transition
/// fold. A modify updates the registry in place, so the snapshot's order price/qty reflect a
/// re-price WITHOUT a cancel+resubmit. `cancel` is a no-op: nothing is ever terminalized.
#[derive(Default)]
pub(crate) struct ModifiableClient {
    events: VecDeque<Event>,
}
impl ExecutionClient for ModifiableClient {
    fn submit(&mut self, request: &OrderRequest) {
        accept_submit(&mut self.events, request);
    }
    fn cancel(&mut self, _client_order_id: &str) {}
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.events.push_back(Event::OrderModified(OrderModified {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: None,
            new_qty,
            new_price,
            ts: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

/// A resting-order client: `submit` acks (so the order reaches `Accepted` and RESTS — unlike
/// `TestExecutionClient`, which fills immediately), `cancel` terminalizes it with the venue's
/// authoritative `OrderCanceled` — the event that actually moves the FSM, and the one replay re-folds
/// from its own journaled record — AND counts the calls, so a test can prove the GTD sweep fires
/// exactly once per order. The cancel's reason is the caller's (`"gtd"`, `"test"`).
#[derive(Debug)]
pub(crate) struct RestingClient {
    pending: VecDeque<Event>,
    cancels: Arc<AtomicUsize>,
    cancel_reason: &'static str,
}

impl RestingClient {
    /// A client with nothing pending, counting its cancels into `cancels`.
    pub(crate) fn new(cancel_reason: &'static str, cancels: Arc<AtomicUsize>) -> Self {
        Self { pending: VecDeque::new(), cancels, cancel_reason }
    }
}

impl ExecutionClient for RestingClient {
    fn submit(&mut self, request: &OrderRequest) {
        // Initialized → Submitted → Accepted: the FSM only accepts the ack after the submit
        // transition, so emit both (a real adapter's venue does the same in two frames).
        accept_submit(&mut self.pending, request);
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.cancels.fetch_add(1, Ordering::Relaxed);
        self.pending.push_back(Event::OrderCanceled(OrderCanceled {
            client_order_id: client_order_id.to_string(),
            reason: self.cancel_reason.into(),
            ts: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.pending.pop_front()
    }
}

/// Calls `order_target_percent(0.5)` on the first bar only — half of whatever `ctx.equity()`
/// reports, once, through `LiveBroker::order_target_percent`, which folds through
/// `vike_model::units_from_percent`, the ONE sizing law. Built with `done: true`, it never trades.
pub(crate) struct TargetHalf {
    pub(crate) done: bool,
}
impl Strategy<LiveBroker> for TargetHalf {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        if !self.done {
            self.done = true;
            broker.order_target_percent(0.5);
        }
    }
}

/// A trading [`TargetHalf`] that also counts every `on_fill` delivery into `fills` — what a test of
/// fill ROUTING reads (which mount, which engine heard its own fill).
pub(crate) struct TargetHalfCountingFills {
    half: TargetHalf,
    fills: Arc<AtomicUsize>,
}
impl TargetHalfCountingFills {
    pub(crate) fn new(fills: &Arc<AtomicUsize>) -> Self {
        Self { half: TargetHalf { done: false }, fills: Arc::clone(fills) }
    }
}
impl Strategy<LiveBroker> for TargetHalfCountingFills {
    fn on_bar(&mut self, broker: &mut LiveBroker, bar: &Bar) {
        self.half.on_bar(broker, bar);
    }
    fn on_fill(&mut self, _broker: &mut LiveBroker, _fill: &Fill) {
        self.fills.fetch_add(1, Ordering::Relaxed);
    }
}

/// Rests ONE tagged limit (`"bid"`, a 1.0 buy at 100.0) on its first quote tick, then never trades
/// again — the one mount-minted, still-RESTING order a restart or a refold has to attribute. `done`
/// reads `true` once it has rested it.
pub(crate) struct RestOnce {
    pub(crate) done: Arc<AtomicBool>,
}
impl Strategy<LiveBroker> for RestOnce {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if !self.done.swap(true, Ordering::Relaxed) {
            broker.submit_limit_tagged("bid", 1, 1.0, 100.0);
        }
    }
}

/// A scripted strategy that persists a single counter `n`: `save_state` -> `{"n": n}`, `load_state`
/// restores it, and each closed bar increments it. Also mirrors the current value into `observed` so
/// a test can read it directly — the strategy itself is moved into the mount's `Box` and unreachable
/// after `spawn_core`.
pub(crate) struct CounterStrategy {
    pub(crate) n: i64,
    pub(crate) observed: Arc<Mutex<i64>>,
}
impl Strategy<LiveBroker> for CounterStrategy {
    fn on_bar(&mut self, _broker: &mut LiveBroker, _bar: &Bar) {
        self.n += 1;
        *self.observed.lock().unwrap() = self.n;
    }
    fn save_state(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "n": self.n }))
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(n) = state.get("n").and_then(|v| v.as_i64()) {
            self.n = n;
        }
        *self.observed.lock().unwrap() = self.n;
    }
}

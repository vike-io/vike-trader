//! Test doubles: execution clients, a reconcile client and strategies, each with exactly the
//! behaviour its doc names.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use vike_core::LiveBroker;
use vike_exec::ExecutionClient;
use vike_exec::recon::ReconClient;
use vike_model::events::{Event, OrderAccepted, OrderModified, OrderSubmitted};
use vike_model::{
    Bar, Broker, FillReport, OrderRequest, OrderStatusReport, PositionStatusReport, QuoteTick,
    Strategy,
};

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
        self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        self.events.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: None,
            ts: request.ts,
        }));
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

/// Counts `fetch_order_status_reports` calls — one per reconcile pass — and reports nothing at all,
/// so a test proves a pass RAN (or was suppressed) without depending on what a pass would find.
pub(crate) struct CountingReconClient {
    pub(crate) calls: Arc<AtomicU64>,
}

impl ReconClient for CountingReconClient {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(Vec::new())
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

/// Submits a 1-lot BTCUSDT market buy on EVERY quote tick and counts the ticks it saw. Rewritten
/// against only the public `vike_model`/`vike_core` surface from the runtime-internal
/// `AlwaysSubmitStrategy` unit-test helper (`runtime::tests`, not exported).
pub(crate) struct AlwaysSubmitStrategy {
    pub(crate) calls: Arc<AtomicUsize>,
}

impl Strategy<LiveBroker> for AlwaysSubmitStrategy {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        broker.submit_market("BTCUSDT", 1, 1.0);
    }
}

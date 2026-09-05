//! In-process `ExecutionClient` test doubles: `RecordingClient` (records what the engine sent) and
//! `TestExecutionClient` (fill-at-request-price stub for the headless end-to-end path). Split out of
//! the engine module; behavior byte-identical.

use vike_model::OrderRequest;
use vike_model::events::Event;

use super::{CancelIntent, ExecutionClient};

/// Test/fixture client: records what the engine sent, emits nothing.
#[derive(Debug, Default)]
pub struct RecordingClient {
    pub submissions: Vec<OrderRequest>,
    pub cancels: Vec<String>,
    /// `(coid, intent)` per cancel — the same sequence as `cancels`, with the classification the
    /// engine passed down. SEPARATE from `cancels` so every existing assertion over that field is
    /// untouched; this is the only way to see whether a caller classified its cancel at all, since
    /// an unclassified one is indistinguishable from a risk-off one at the venue.
    pub cancel_intents: Vec<(String, CancelIntent)>,
    /// coids the engine issued a `confirm` (audit ex1 confirm-order watchdog) for.
    pub confirms: Vec<String>,
    /// `(coid, new_qty, new_price)` per modify that actually REACHED the venue. Recorded because a
    /// RiskGate veto on the modify path is invisible otherwise: a denied modify and a modify that
    /// was never issued both leave `submissions`/`cancels` untouched, so without this a test cannot
    /// tell "the gate refused it" from "the test never asked". See
    /// `crates/vike-exec/tests/risk/risk_gate_on_modify.rs`.
    pub modifies: Vec<(String, Option<f64>, Option<f64>)>,
    pub detached: bool,
    /// What this double claims its `modify` does to a resting order's outstanding size — the
    /// [`ExecutionClient::amend_semantics`] override, as DATA so a test can stand in for a client
    /// that is not the venue it is mounted under (the paper exchange being the real one).
    /// `None` (the default) = decline, i.e. `ExecutionEngine::modify_order` falls through to
    /// `vike_model::amend_semantics` on the engine's venue string, exactly as every venue adapter
    /// does.
    pub declared_amend_semantics: Option<vike_model::AmendSemantics>,
    /// Closed bars this client was handed through [`ExecutionClient::on_bar`] — the paper
    /// exchange's fill clock, recorded because WHICH clients receive it is a routing question with
    /// no other observable: the trait's default `on_bar` is a no-op, so a client that never gets
    /// the bar is indistinguishable from one that got it and had nothing to fill. That is exactly
    /// how a second ACCOUNT's paper book came to be filled by nothing at all
    /// (`vike_core`'s `mount_account_tests`).
    pub bars: Vec<vike_model::Bar>,
}

impl ExecutionClient for RecordingClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.submissions.push(request.clone());
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.cancel_with_intent(client_order_id, CancelIntent::Unspecified);
    }
    /// Both cancel doors land here (the rule the trait doc states for any client that overrides
    /// this), so `cancels` stays the complete cancel log whichever door a test drives.
    fn cancel_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
        self.cancels.push(client_order_id.to_string());
        self.cancel_intents.push((client_order_id.to_string(), intent));
    }
    /// Fan out per id so the batch lanes record their intent too. Byte-identical to the trait
    /// default this double inherited before (`cancel_batch` fans out to `cancel`) — this double
    /// has no native batch endpoint to preserve.
    fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
        for c in client_order_ids {
            self.cancel_with_intent(c, intent);
        }
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.modifies.push((order.client_order_id.to_string(), new_qty, new_price));
    }
    fn confirm(&mut self, client_order_id: &str) {
        self.confirms.push(client_order_id.to_string());
    }
    fn detach(&mut self) {
        self.detached = true;
    }
    fn amend_semantics(&self) -> Option<vike_model::AmendSemantics> {
        self.declared_amend_semantics
    }
    /// Record only — this double fills nothing on a bar. See [`RecordingClient::bars`].
    fn on_bar(&mut self, bar: &vike_model::Bar) {
        self.bars.push(bar.clone());
    }
}

/// Fill-at-request-price stub for the headless end-to-end path (no venue, no network):
/// `submit` queues Submitted → Accepted → bare fill + Filled wrap at the request's
/// price (`trigger_price`, then `mark` for market orders); `cancel` queues OrderCanceled.
/// The runtime pumps `pending` through the bus exactly like WS events off the ingest channel.
#[derive(Debug)]
pub struct TestExecutionClient {
    pub venue: String,
    /// price used when the request carries none (market orders)
    pub mark: f64,
    pub commission_per_fill: f64,
    pub pending: std::collections::VecDeque<Event>,
    /// recording half (the Python fixture twin inherits the recording client)
    pub submissions: Vec<OrderRequest>,
    pub cancels: Vec<String>,
    pub detached: bool,
    seq: u64,
}

impl TestExecutionClient {
    pub fn new(venue: &str, mark: f64) -> Self {
        TestExecutionClient {
            venue: venue.to_string(),
            mark,
            commission_per_fill: 0.0,
            pending: Default::default(),
            submissions: Vec::new(),
            cancels: Vec::new(),
            detached: false,
            seq: 0,
        }
    }
}

impl ExecutionClient for TestExecutionClient {
    fn poll_events(&mut self) -> Option<Event> {
        self.pending.pop_front()
    }

    fn submit(&mut self, request: &OrderRequest) {
        self.submissions.push(request.clone());
        self.seq += 1;
        let n = self.seq;
        let px = request.price.or(request.trigger_price).unwrap_or(self.mark);
        let fill = vike_model::events::FillEvent {
            trade_id: vike_model::events::TradeId::prefixed("simt", n),
            client_order_id: request.client_order_id.clone(),
            venue: self.venue.as_str().into(),
            symbol: request.symbol.as_str().into(),
            side: request.side,
            last_qty: request.qty,
            last_px: px,
            commission: self.commission_per_fill,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            ts: request.ts,
            mark_price: Some(px),
            position_side: "BOTH".into(),
        };
        self.pending.push_back(Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        self.pending.push_back(Event::OrderAccepted(vike_model::events::OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: Some(format!("sim{n}").into()),
            ts: request.ts,
        }));
        self.pending.push_back(Event::Fill(fill.clone()));
        self.pending.push_back(Event::OrderFilled(vike_model::events::OrderFilled {
            client_order_id: request.client_order_id.clone(),
            fill,
            ts: request.ts,
        }));
    }

    fn cancel(&mut self, client_order_id: &str) {
        self.cancels.push(client_order_id.to_string());
        self.pending.push_back(Event::OrderCanceled(vike_model::events::OrderCanceled {
            client_order_id: client_order_id.to_string(),
            reason: "user".to_string().into(),
            ts: 0,
        }));
    }

    fn detach(&mut self) {
        self.detached = true;
    }
}

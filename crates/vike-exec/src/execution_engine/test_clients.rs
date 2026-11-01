//! In-process `ExecutionClient` test doubles: `RecordingClient` (records what the engine sent) and
//! `TestExecutionClient` (fill-at-request-price stub for the headless end-to-end path), plus
//! `EngineBuilder`, the one `ExecutionEngine<RecordingClient>` builder this crate's own test binaries
//! share (compiled once here instead of once per binary), and the market order and the denial
//! reader the unit and integration risk suites both use.

use vike_model::events::Event;
use vike_model::{OrderRequest, RiskLimits};

use super::{CancelIntent, ExecutionClient, ExecutionEngine};
use crate::{Account, BalanceMode, EventHandler, Outbox, RiskGate};

/// A binance `BTCUSDT` MARKET order of `qty` on `side`, coid `"t1"`, every other field default: the
/// request the risk-gate suites judge.
pub fn market_order(side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "t1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        order_type: vike_model::OrderType::Market,
        side,
        qty,
        ..Default::default()
    }
}

/// Drain the client's queued venue events through the engine's fold (what the core runtime does).
pub fn pump<C: ExecutionClient>(engine: &mut ExecutionEngine<C>, outbox: &mut Outbox) {
    while let Some(event) = engine.client.poll_events() {
        engine.on_event(&event, outbox);
    }
}

/// The reason of the first `OrderDenied` in `outbox`, `None` when nothing was denied.
pub fn denied_reason(outbox: &Outbox) -> Option<String> {
    outbox.0.iter().find_map(|e| match e {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    })
}

/// Test/fixture client: records what the engine sent, emits nothing.
#[derive(Debug, Default)]
pub struct RecordingClient {
    pub submissions: Vec<OrderRequest>,
    pub cancels: Vec<String>,
    /// `(coid, intent)` per cancel — the same sequence as `cancels`, with the classification the
    /// engine passed down: the only way to see whether a caller classified its cancel, since an
    /// unclassified one is indistinguishable from a risk-off one at the venue.
    pub cancel_intents: Vec<(String, CancelIntent)>,
    /// coids the engine issued a `confirm` (the confirm-order watchdog) for.
    pub confirms: Vec<String>,
    /// `(coid, new_qty, new_price)` per modify that actually REACHED the venue: without it a test
    /// cannot tell "the gate refused it" from "the test never asked" (see
    /// `crates/vike-exec/tests/risk/risk_gate_on_modify.rs`).
    pub modifies: Vec<(String, Option<f64>, Option<f64>)>,
    pub detached: bool,
    /// The [`ExecutionClient::amend_semantics`] override, as DATA, so a test can stand in for a
    /// client that is not the venue it is mounted under (the paper exchange). `None` (the
    /// default) = decline: `ExecutionEngine::modify_order` falls through to
    /// `vike_venues::amend_semantics` on the engine's venue string, as every venue adapter does.
    pub declared_amend_semantics: Option<vike_venues::AmendSemantics>,
    /// Closed bars this client was handed through [`ExecutionClient::on_bar`] — the paper
    /// exchange's fill clock. Recorded because WHICH clients receive it has no other observable:
    /// the default `on_bar` is a no-op, so a client that never gets the bar looks like one that
    /// had nothing to fill (`vike_core`'s `mount_account_tests`).
    pub bars: Vec<vike_marketdata::Bar>,
    /// The [`ExecutionClient::wire_id_for`] override, as DATA: `Some(prefix)` makes this double
    /// answer `format!("{prefix}{coid}")`, the stand-in for a venue whose reports name an order by
    /// a function of its coid (Hyperliquid's `keccak` cloid). `None` (the default) = the venue
    /// echoes the coid, as every adapter but one does.
    pub wire_prefix: Option<String>,
    /// Every `(coid, symbol)` the restore announced through [`ExecutionClient::adopt_restored`],
    /// in call order: WHETHER and WITH WHAT the core announced an adoption has no other
    /// observable, since the default is a no-op.
    pub adopted: Vec<(String, String)>,
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
    /// Fan out per id so the batch lanes record their intent too (this double has no native batch
    /// endpoint to preserve).
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
    fn amend_semantics(&self) -> Option<vike_venues::AmendSemantics> {
        self.declared_amend_semantics
    }
    /// Record only — this double fills nothing on a bar. See [`RecordingClient::bars`].
    fn on_bar(&mut self, bar: &vike_marketdata::Bar) {
        self.bars.push(bar.clone());
    }
    fn wire_id_for(&self, coid: &str) -> Option<String> {
        self.wire_prefix.as_ref().map(|p| format!("{p}{coid}"))
    }
    fn adopt_restored(&mut self, orders: &[(String, String)]) {
        self.adopted.extend(orders.iter().cloned());
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
    /// recording half
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
            position_side: vike_model::events::PositionSide::Both,
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

/// One `ExecutionEngine<RecordingClient>` builder shared by the `engine`/`recon`/`parity`/`risk`
/// test binaries. The fields the members' engines differ on, DEFAULTED to the most common value:
/// multiplier `1.0`, venue `"sim"`, symbol `"BTCUSDT"`, `RiskLimits::new()` (no limits), no extra
/// symbols. A member overrides with struct-update syntax (`EngineBuilder { venue: .., ..Default::default() }`),
/// so every departure stays visible at its own site. Plain fields, not setters: each binary sets a
/// different subset, and a setter one binary never calls is dead code there.
///
/// PINNED: no per-symbol multiplier map, `BalanceMode::Delta`, a fresh `RecordingClient`, and the
/// account labelled with the engine's own venue.
pub struct EngineBuilder {
    /// `Account::new`'s first argument: the DEFAULT contract multiplier, not an equity seed.
    pub multiplier: f64,
    pub venue: String,
    pub symbol: String,
    pub limits: RiskLimits,
    /// Assigned after construction (`ExecutionEngine::new` leaves it empty).
    pub extra_symbols: Vec<String>,
}

impl Default for EngineBuilder {
    fn default() -> Self {
        EngineBuilder {
            multiplier: 1.0,
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            limits: RiskLimits::new(),
            extra_symbols: Vec::new(),
        }
    }
}

impl EngineBuilder {
    pub fn build(self) -> ExecutionEngine<RecordingClient> {
        self.build_with(RecordingClient::default())
    }

    /// [`Self::build`] driving `client` instead of a fresh `RecordingClient`.
    pub fn build_with<C: ExecutionClient>(self, client: C) -> ExecutionEngine<C> {
        let mut e = ExecutionEngine::new(
            Account::new(self.multiplier, &self.venue, None, BalanceMode::Delta),
            RiskGate::new(self.limits),
            client,
            &self.venue,
            &self.symbol,
        );
        e.extra_symbols = self.extra_symbols;
        e
    }
}

/// An engine on `venue`/`symbol` driving `client` — the ONE spelling of the default engine shape
/// the core's white-box suites, its integration kit and the reconcile-manager suites build on.
///
/// DEFAULTS: those of [`EngineBuilder`]: contract multiplier `1.0` (`Account::new`'s first argument —
/// NOT a seed: `spawn_core` sets the equity seed from `CoreConfig::seed_cash`), `BalanceMode::Delta`,
/// `RiskLimits::new()` (no limits), and the account labelled with the engine's own `venue` (so
/// `route_key` is `venue` too). A test whose subject is one of those — an Authoritative wallet, a risk
/// ceiling, a multiplier, an overridden `route_key`, `extra_symbols` — builds its engine itself.
pub fn engine_on<C: ExecutionClient>(venue: &str, symbol: &str, client: C) -> ExecutionEngine<C> {
    EngineBuilder { venue: venue.into(), symbol: symbol.into(), ..Default::default() }
        .build_with(client)
}

/// [`engine_on`] over a BOXED client, so engines whose clients differ in type share one
/// `ExecutionEngine<Box<dyn ExecutionClient + Send>>` and can ride one multi-engine core. The
/// parameter's `dyn` type is what lets `Box::new(SomeClient)` coerce at the call site (a generic one
/// would infer `Box<SomeClient>`).
pub fn dyn_engine(
    venue: &str,
    symbol: &str,
    client: Box<dyn ExecutionClient + Send>,
) -> ExecutionEngine<Box<dyn ExecutionClient + Send>> {
    engine_on(venue, symbol, client)
}

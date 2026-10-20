//! The scripted TRADE lane: a `TradeLane` whose venue adapters are doubles a test drives.
//!
//! [`ScriptedTrade`] serves the venues it was built with. Every `start_adapter` hands back a
//! [`ScriptedAdapter`]'s exec client, which DOES nothing on its own: it records each call the session
//! makes of it ([`ExecCall`], in arrival order), runs an optional submit hook (the venue's acks and
//! fills a test plays through the `EventSender` the lane was given) and answers its reconcile fetches
//! from a script. The test keeps the [`ScriptedAdapter`] handle and plays any event from any thread
//! ([`ScriptedAdapter::emit`]), fires the reconcile trigger ([`ScriptedAdapter::fire_trigger`]) and reads
//! back what the daemon asked of the venue.
//!
//! # What it keeps true to the real thing
//!
//! * `venues()` is fixed at construction: a venue that is down is a refused start, never a missing
//!   advertisement (the lane contract in `crate::trade::lane`).
//! * The default tier of an account is `demo` and a default start is `Live` at the lease's tier with a
//!   reconcile client; every other answer is scripted per `(venue, account)`.
//! * Every lane call is recorded ([`TradeCall`]) before it is answered, a start included, so "the lane
//!   was never asked" is an empty slice.
//! * `begin_detach` and `detach` are recorded and idempotent, as the `ExecutionClient` contract says.
//!
//! The kit reads no store, environment or credential and links no bridge.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use vike_config::ArmingBlock;
use vike_exec::recon::ReconClient;
use vike_exec::{CancelIntent, EventSender, ExecLinkUpdate, ExecutionClient};
use vike_gatewayhub_client::proto::{
    AdapterStart, ArmingTable, LegResult, PreflightAnswer, PreflightStep, WireSymbolFacts,
};
use vike_model::events::Event;
use vike_model::{
    AccountTier, FeeSchedule, FillReport, MarginMode, OrderRequest, OrderStatusReport,
    PositionStatusReport, SymbolProperties,
};

use crate::trade::lane::{AdapterRefusal, LinkWatch, StartedAdapter, TradeLane, TradeTarget};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One call the lane saw, in the order it arrived.
#[derive(Debug, Clone, PartialEq)]
pub enum TradeCall {
    /// `account_tier(venue, account)`.
    AccountTier {
        /// The venue.
        venue: String,
        /// The account label's text.
        account: Option<String>,
    },
    /// `arming_table()`.
    ArmingTable,
    /// `preflight(venues, step)`.
    Preflight {
        /// The venues asked.
        venues: Vec<String>,
        /// The question.
        step: PreflightStep,
    },
    /// `start_adapter` (recorded before it is answered).
    Start {
        /// The target.
        target: TradeTarget,
        /// The start request.
        start: AdapterStart,
        /// Whether the lane was handed a reconcile trigger.
        trigger: bool,
    },
    /// `record_identity`.
    RecordIdentity {
        /// The target.
        target: TradeTarget,
        /// The tier the start bound.
        bound_tier: AccountTier,
        /// Whether a reconcile client was passed.
        had_recon: bool,
    },
    /// `symbol_facts`.
    SymbolFacts {
        /// The target.
        target: TradeTarget,
        /// The symbol asked.
        symbol: String,
    },
    /// `symbol_facts_quoted` (asked by a node that converts a quote currency).
    SymbolFactsQuoted {
        /// The target.
        target: TradeTarget,
        /// The symbol asked.
        symbol: String,
    },
    /// `symbol_leverage` (asked with a committed admission).
    SymbolLeverage {
        /// The target.
        target: TradeTarget,
        /// The running adapter's own start.
        start: AdapterStart,
        /// The symbol asked.
        symbol: String,
    },
}

/// One call the session made of a scripted exec client, in the order it arrived.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecCall {
    /// `submit`.
    Submit(OrderRequest),
    /// `submit_batch`.
    SubmitBatch(Vec<OrderRequest>),
    /// `cancel_with_intent` (`cancel` is recorded as this with `Unspecified`).
    Cancel {
        /// The client order id.
        coid: String,
        /// Why.
        intent: CancelIntent,
    },
    /// `cancel_batch_with_intent`.
    CancelBatch {
        /// The client order ids.
        coids: Vec<String>,
        /// Why.
        intent: CancelIntent,
    },
    /// `modify`.
    Modify {
        /// The whole resting order.
        order: OrderRequest,
        /// The requested quantity.
        new_qty: Option<f64>,
        /// The requested price.
        new_price: Option<f64>,
    },
    /// `confirm`.
    Confirm(String),
    /// `adopt_restored`.
    AdoptRestored(Vec<(String, String)>),
    /// `begin_detach`.
    BeginDetach,
    /// `detach`.
    Detach,
}

/// What a scripted start answers.
#[derive(Debug, Clone)]
pub struct ScriptedStart {
    outcome: Result<LiveScript, AdapterRefusal>,
    delay: Duration,
}

#[derive(Debug, Clone)]
struct LiveScript {
    bound_tier: Option<AccountTier>,
    grid: Option<SymbolProperties>,
    contract_value: Option<vike_model::ContractValue>,
    margin_mode: Option<MarginMode>,
    leg_grids: Vec<(String, SymbolProperties)>,
    recon: bool,
}

impl ScriptedStart {
    /// A start that comes up at the lease's tier with a reconcile client.
    pub fn live() -> Self {
        Self {
            outcome: Ok(LiveScript {
                bound_tier: None,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
                recon: true,
            }),
            delay: Duration::ZERO,
        }
    }

    /// A start the bridge refuses: the account is NOT RUNNING with `block` as the word and `text` as the
    /// bridge's own words.
    pub fn refuse(block: ArmingBlock, text: &str) -> Self {
        Self {
            outcome: Err(AdapterRefusal {
                block_word: block.word().to_string(),
                refusal_text: text.to_string(),
            }),
            delay: Duration::ZERO,
        }
    }

    fn live_mut(&mut self) -> &mut LiveScript {
        match &mut self.outcome {
            Ok(live) => live,
            Err(_) => panic!("a refusal has no live facts to set"),
        }
    }

    /// Bind this tier whatever the lease asked (the tier interlock's test).
    #[must_use]
    pub fn bound_tier(mut self, tier: AccountTier) -> Self {
        self.live_mut().bound_tier = Some(tier);
        self
    }

    /// The engine symbol's grid.
    #[must_use]
    pub fn grid(mut self, grid: SymbolProperties) -> Self {
        self.live_mut().grid = Some(grid);
        self
    }

    /// The engine symbol's contract value (kind plus per-unit factor).
    #[must_use]
    pub fn contract_value(mut self, value: vike_model::ContractValue) -> Self {
        self.live_mut().contract_value = Some(value);
        self
    }

    /// The ruling margin mode.
    #[must_use]
    pub fn margin_mode(mut self, mode: MarginMode) -> Self {
        self.live_mut().margin_mode = Some(mode);
        self
    }

    /// The declared legs' grids.
    #[must_use]
    pub fn leg_grids(mut self, grids: Vec<(String, SymbolProperties)>) -> Self {
        self.live_mut().leg_grids = grids;
        self
    }

    /// Build no reconcile client (a venue without one).
    #[must_use]
    pub fn no_recon(mut self) -> Self {
        self.live_mut().recon = false;
        self
    }

    /// Take this long to answer (a venue login).
    #[must_use]
    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

type SubmitHook = Arc<dyn Fn(&OrderRequest, &EventSender) + Send + Sync>;

/// The reconcile answers a scripted adapter gives. Each is what the venue returns; `Err` is a failed
/// fetch.
#[derive(Clone)]
struct ReconScript {
    orders: Result<Vec<OrderStatusReport>, String>,
    fills: Result<Vec<FillReport>, String>,
    positions: Result<Vec<PositionStatusReport>, String>,
    balance: Result<Option<f64>, String>,
    fee_rates: Result<Option<FeeSchedule>, String>,
    identity: Result<Option<String>, String>,
    delay: Duration,
}

impl Default for ReconScript {
    fn default() -> Self {
        Self {
            orders: Ok(Vec::new()),
            fills: Ok(Vec::new()),
            positions: Ok(Vec::new()),
            balance: Ok(None),
            fee_rates: Ok(None),
            identity: Ok(None),
            delay: Duration::ZERO,
        }
    }
}

struct AdapterState {
    calls: Vec<ExecCall>,
    detached: bool,
    wire_ids: HashMap<String, String>,
    submit_hook: Option<SubmitHook>,
    recon: ReconScript,
    /// The symbols the daemon told the reconcile client the engine admitted (`ReconClient::admit_symbol`).
    admitted: Vec<String>,
    /// The admitted symbols the client held at each `fetch_order_status_reports`, in order.
    admitted_at_fetch: Vec<Vec<String>>,
    events: Option<EventSender>,
    trigger: Option<Sender<()>>,
}

/// A handle on one started adapter: what the daemon asked of it, and the doors a test plays the venue
/// through. Cloning shares the adapter.
#[derive(Clone)]
pub struct ScriptedAdapter {
    state: Arc<Mutex<AdapterState>>,
}

impl ScriptedAdapter {
    fn new(events: EventSender, trigger: Option<Sender<()>>) -> Self {
        Self {
            state: Arc::new(Mutex::new(AdapterState {
                calls: Vec::new(),
                detached: false,
                wire_ids: HashMap::new(),
                submit_hook: None,
                recon: ReconScript::default(),
                admitted: Vec::new(),
                admitted_at_fetch: Vec::new(),
                events: Some(events),
                trigger,
            })),
        }
    }

    /// The symbols the daemon told the reconcile client the engine admitted, in arrival order.
    pub fn admitted_symbols(&self) -> Vec<String> {
        lock(&self.state).admitted.clone()
    }

    /// The admitted symbols the reconcile client held at each order fetch, in fetch order: what a pass
    /// saw, which `admitted_symbols` (polled whenever) cannot say.
    pub fn admitted_at_order_fetches(&self) -> Vec<Vec<String>> {
        lock(&self.state).admitted_at_fetch.clone()
    }

    /// Every call the daemon made of the exec client, in arrival order.
    pub fn calls(&self) -> Vec<ExecCall> {
        lock(&self.state).calls.clone()
    }

    /// The orders submitted so far (singles and batches, in arrival order).
    pub fn submitted(&self) -> Vec<OrderRequest> {
        let mut out = Vec::new();
        for call in lock(&self.state).calls.iter() {
            match call {
                ExecCall::Submit(o) => out.push(o.clone()),
                ExecCall::SubmitBatch(os) => out.extend(os.iter().cloned()),
                _ => {}
            }
        }
        out
    }

    /// Whether `detach` has run.
    pub fn is_detached(&self) -> bool {
        lock(&self.state).detached
    }

    /// Play a venue event through the lane the daemon gave the adapter (blocking, lossless, exactly as a
    /// bridge pump does). `false` = the daemon's side is gone.
    pub fn emit(&self, event: Event) -> bool {
        let events = lock(&self.state).events.clone();
        match events {
            Some(events) => events.blocking_send(event).is_ok(),
            None => false,
        }
    }

    /// Play a link transition through the lane the daemon gave the adapter, as a bridge that discloses
    /// its venue link does (`EventSender::exec_link`, stamped with the lane's own route key). `false` =
    /// the daemon's side is gone.
    pub fn emit_exec_link(&self, up: bool) -> bool {
        let Some(events) = lock(&self.state).events.clone() else { return false };
        let route_key = events.route_key.map_or_else(|| "ibkr".to_string(), |k| k.to_string());
        let update: ExecLinkUpdate = serde_json::from_value(serde_json::json!({
            "route_key": route_key,
            "venue": "ibkr",
            "up": up,
            "at_ts_ms": 0,
        }))
        .expect("an ExecLinkUpdate is these four fields");
        events.exec_link(update).is_ok()
    }

    /// Fire the reconcile trigger (a bridge asking for a reconcile pass). `false` = the venue takes none,
    /// or the daemon dropped it.
    pub fn fire_trigger(&self) -> bool {
        match &lock(&self.state).trigger {
            Some(tx) => tx.send(()).is_ok(),
            None => false,
        }
    }

    /// Run `hook` on every submit, inside the exec client's `submit` (the venue's ack or fill, played
    /// through the `EventSender` the lane was given).
    pub fn on_submit(&self, hook: impl Fn(&OrderRequest, &EventSender) + Send + Sync + 'static) {
        lock(&self.state).submit_hook = Some(Arc::new(hook));
    }

    /// Answer `wire_id_for(coid)` with `wire` (hyperliquid's `cloid`).
    pub fn set_wire_id(&self, coid: &str, wire: &str) {
        lock(&self.state).wire_ids.insert(coid.to_string(), wire.to_string());
    }

    /// Script `fetch_order_status_reports`.
    pub fn set_orders(&self, answer: Result<Vec<OrderStatusReport>, String>) {
        lock(&self.state).recon.orders = answer;
    }

    /// Script `fetch_fill_reports`.
    pub fn set_fills(&self, answer: Result<Vec<FillReport>, String>) {
        lock(&self.state).recon.fills = answer;
    }

    /// Script `fetch_position_status_reports`.
    pub fn set_positions(&self, answer: Result<Vec<PositionStatusReport>, String>) {
        lock(&self.state).recon.positions = answer;
    }

    /// Script `fetch_balance`.
    pub fn set_balance(&self, answer: Result<Option<f64>, String>) {
        lock(&self.state).recon.balance = answer;
    }

    /// Script `fetch_fee_rates`.
    pub fn set_fee_rates(&self, answer: Result<Option<FeeSchedule>, String>) {
        lock(&self.state).recon.fee_rates = answer;
    }

    /// Script `fetch_account_identity`.
    pub fn set_identity(&self, answer: Result<Option<String>, String>) {
        lock(&self.state).recon.identity = answer;
    }

    /// Make every reconcile fetch take this long (a slow venue).
    pub fn set_recon_delay(&self, delay: Duration) {
        lock(&self.state).recon.delay = delay;
    }

    fn record(&self, call: ExecCall) {
        lock(&self.state).calls.push(call);
    }
}

impl std::fmt::Debug for ScriptedAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedAdapter").finish_non_exhaustive()
    }
}

/// The exec client the lane hands back: records, never touches a venue.
struct ScriptedExec {
    adapter: ScriptedAdapter,
}

impl ExecutionClient for ScriptedExec {
    fn submit(&mut self, request: &OrderRequest) {
        self.adapter.record(ExecCall::Submit(request.clone()));
        let (hook, events) = {
            let s = lock(&self.adapter.state);
            (s.submit_hook.clone(), s.events.clone())
        };
        if let (Some(hook), Some(events)) = (hook, events) {
            hook(request, &events);
        }
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.cancel_with_intent(client_order_id, CancelIntent::Unspecified);
    }
    fn cancel_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
        self.adapter.record(ExecCall::Cancel { coid: client_order_id.to_string(), intent });
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.adapter.record(ExecCall::Modify { order: order.clone(), new_qty, new_price });
    }
    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        self.adapter.record(ExecCall::SubmitBatch(requests.to_vec()));
    }
    fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
        self.adapter.record(ExecCall::CancelBatch { coids: client_order_ids.to_vec(), intent });
    }
    fn confirm(&mut self, client_order_id: &str) {
        self.adapter.record(ExecCall::Confirm(client_order_id.to_string()));
    }
    fn begin_detach(&mut self) {
        self.adapter.record(ExecCall::BeginDetach);
    }
    fn detach(&mut self) {
        let mut s = lock(&self.adapter.state);
        s.calls.push(ExecCall::Detach);
        s.detached = true;
        // A detached adapter drops its lane, as a real bridge's pumps do.
        s.events = None;
        s.trigger = None;
    }
    fn wire_id_for(&self, coid: &str) -> Option<String> {
        lock(&self.adapter.state).wire_ids.get(coid).cloned()
    }
    fn adopt_restored(&mut self, orders: &[(String, String)]) {
        self.adapter.record(ExecCall::AdoptRestored(orders.to_vec()));
    }
}

/// The reconcile client the lane hands back: answers from the script.
struct ScriptedRecon {
    adapter: ScriptedAdapter,
}

impl ScriptedRecon {
    fn script(&self) -> ReconScript {
        let script = lock(&self.adapter.state).recon.clone();
        if !script.delay.is_zero() {
            std::thread::sleep(script.delay);
        }
        script
    }
}

impl ReconClient for ScriptedRecon {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        {
            let mut state = lock(&self.adapter.state);
            let held = state.admitted.clone();
            state.admitted_at_fetch.push(held);
        }
        self.script().orders
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        self.script().fills
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.script().positions
    }
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        self.script().balance
    }
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        self.script().fee_rates
    }
    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        self.script().identity
    }
    fn admit_symbol(&self, symbol: &str) {
        lock(&self.adapter.state).admitted.push(symbol.to_string());
    }
}

type AccountKey = (String, Option<String>);

/// What answers a preflight question: the venues asked and the step, as the real lane gets them.
type PreflightResponder = Arc<dyn Fn(&[String], PreflightStep) -> PreflightAnswer + Send + Sync>;

struct State {
    tiers: HashMap<AccountKey, Result<AccountTier, ArmingBlock>>,
    arming: ArmingTable,
    legs: Vec<LegResult>,
    preflight: Option<PreflightResponder>,
    starts: HashMap<AccountKey, ScriptedStart>,
    resolvers: HashSet<String>,
    degraded: HashMap<String, String>,
    leverage_caps: HashMap<String, Result<f64, String>>,
    /// `(venue, symbol)` -> what the venue confirms of the account's figure for a symbol admitted on demand.
    symbol_leverages: HashMap<(String, String), Result<f64, String>>,
    identity: Result<(), String>,
    facts: HashMap<String, Result<WireSymbolFacts, String>>,
    /// What `symbol_facts_quoted` answers (a symbol with none scripted answers what `facts` does).
    quoted_facts: HashMap<String, Result<WireSymbolFacts, String>>,
    /// Venue -> the currency its account states (`account_currency`).
    account_currencies: HashMap<String, String>,
    /// Venues whose currency read is paid out of a budget (`currency_read_is_budgeted`).
    budgeted_currency_reads: HashSet<String>,
    /// How many times `account_currency` was asked (a counter, not a `TradeCall`: it is not an ordered call).
    account_currency_asks: usize,
    calls: Vec<TradeCall>,
    adapters: Vec<(AccountKey, ScriptedAdapter)>,
    watches: HashMap<String, Arc<dyn LinkWatch>>,
}

struct Inner {
    venues: Vec<String>,
    state: Mutex<State>,
}

/// A scripted `TradeLane`. See the module doc. Cloning shares the script and the record.
#[derive(Clone)]
pub struct ScriptedTrade {
    inner: Arc<Inner>,
}

impl ScriptedTrade {
    /// A lane serving `venues`.
    pub fn new(venues: &[&str]) -> Self {
        Self {
            inner: Arc::new(Inner {
                venues: venues.iter().map(|v| (*v).to_string()).collect(),
                state: Mutex::new(State {
                    tiers: HashMap::new(),
                    arming: ArmingTable::default(),
                    legs: Vec::new(),
                    preflight: None,
                    starts: HashMap::new(),
                    resolvers: HashSet::new(),
                    degraded: HashMap::new(),
                    leverage_caps: HashMap::new(),
                    symbol_leverages: HashMap::new(),
                    identity: Ok(()),
                    facts: HashMap::new(),
                    quoted_facts: HashMap::new(),
                    account_currencies: HashMap::new(),
                    budgeted_currency_reads: HashSet::new(),
                    account_currency_asks: 0,
                    calls: Vec::new(),
                    adapters: Vec::new(),
                    watches: HashMap::new(),
                }),
            }),
        }
    }

    /// What `account_tier` answers for this account (default: `Ok(demo)`).
    #[must_use]
    pub fn with_tier(
        self,
        venue: &str,
        account: Option<&str>,
        tier: Result<AccountTier, ArmingBlock>,
    ) -> Self {
        lock(&self.inner.state)
            .tiers
            .insert((venue.to_string(), account.map(str::to_string)), tier);
        self
    }

    /// Change what `account_tier` answers for this account from now on (the store broke, or was fixed,
    /// while the daemon ran).
    pub fn set_tier(
        &self,
        venue: &str,
        account: Option<&str>,
        tier: Result<AccountTier, ArmingBlock>,
    ) {
        lock(&self.inner.state)
            .tiers
            .insert((venue.to_string(), account.map(str::to_string)), tier);
    }

    /// What `arming_table` answers.
    #[must_use]
    pub fn with_arming(self, table: ArmingTable) -> Self {
        lock(&self.inner.state).arming = table;
        self
    }

    /// What `preflight` answers (whatever venues are asked).
    #[must_use]
    pub fn with_legs(self, legs: Vec<LegResult>) -> Self {
        lock(&self.inner.state).legs = legs;
        self
    }

    /// What `preflight` answers, per question: `answer(venues, step)` runs on the server's connection
    /// thread for every `PreflightQuery`. The scripted lane's way to play a clock that moves between reads,
    /// as the real one's does. Beats [`ScriptedTrade::with_legs`].
    #[must_use]
    pub fn with_preflight(
        self,
        answer: impl Fn(&[String], PreflightStep) -> PreflightAnswer + Send + Sync + 'static,
    ) -> Self {
        lock(&self.inner.state).preflight = Some(Arc::new(answer));
        self
    }

    /// What `start_adapter` answers for this account (default: a `Live` start).
    #[must_use]
    pub fn with_start(self, venue: &str, account: Option<&str>, start: ScriptedStart) -> Self {
        lock(&self.inner.state)
            .starts
            .insert((venue.to_string(), account.map(str::to_string)), start);
        self
    }

    /// Say `venue`'s price feed is mid-gap (or, with `None`, healthy again): `Recon` requests for it are
    /// answered `ReconNotReady{reason}` while it is.
    pub fn set_degraded(&self, venue: &str, reason: Option<&str>) {
        let mut s = lock(&self.inner.state);
        match reason {
            Some(r) => s.degraded.insert(venue.to_string(), r.to_string()),
            None => s.degraded.remove(venue),
        };
    }

    /// What the venue confirms of an account's stated leverage on `venue` (decision 0181): the answer
    /// `TradeLane::leverage_cap` gives at every start. A venue with none scripted answers `None`.
    #[must_use]
    pub fn with_leverage_cap(self, venue: &str, answer: Result<f64, String>) -> Self {
        lock(&self.inner.state).leverage_caps.insert(venue.to_string(), answer);
        self
    }

    /// What the venue confirms of an account's figure for `symbol` admitted on demand on `venue`: the
    /// answer `TradeLane::symbol_leverage` gives. A symbol with none scripted answers `None` (nothing asked).
    #[must_use]
    pub fn with_symbol_leverage(
        self,
        venue: &str,
        symbol: &str,
        answer: Result<f64, String>,
    ) -> Self {
        lock(&self.inner.state)
            .symbol_leverages
            .insert((venue.to_string(), symbol.to_string()), answer);
        self
    }

    /// Say `venue` can look an order up by client order id.
    #[must_use]
    pub fn with_resolver(self, venue: &str) -> Self {
        lock(&self.inner.state).resolvers.insert(venue.to_string());
        self
    }

    /// Make `record_identity` fail with this warning.
    #[must_use]
    pub fn with_identity_failure(self, why: &str) -> Self {
        lock(&self.inner.state).identity = Err(why.to_string());
        self
    }

    /// What `symbol_facts` answers for `symbol` (default for an unscripted symbol: an `Err` naming it).
    #[must_use]
    pub fn with_symbol_facts(self, symbol: &str, facts: Result<WireSymbolFacts, String>) -> Self {
        lock(&self.inner.state).facts.insert(symbol.to_string(), facts);
        self
    }

    /// What `symbol_facts_quoted` answers for `symbol` (a node that converts a quote currency asked): a
    /// symbol with none scripted is answered as `symbol_facts` answers it.
    #[must_use]
    pub fn with_quoted_symbol_facts(
        self,
        symbol: &str,
        facts: Result<WireSymbolFacts, String>,
    ) -> Self {
        lock(&self.inner.state).quoted_facts.insert(symbol.to_string(), facts);
        self
    }

    /// The currency `venue`'s account states (`account_currency`; default: none stated).
    #[must_use]
    pub fn with_account_currency(self, venue: &str, currency: &str) -> Self {
        lock(&self.inner.state).account_currencies.insert(venue.to_string(), currency.to_string());
        self
    }

    /// `venue`'s currency read is paid out of a budget, like ibkr's and ig's (the scripted lane still states
    /// its currency to whatever start asks, but the session re-asks it only for a start that asks to read).
    #[must_use]
    pub fn with_budgeted_currency_read(self, venue: &str) -> Self {
        lock(&self.inner.state).budgeted_currency_reads.insert(venue.to_string());
        self
    }

    /// How many times the session asked `account_currency` (a start and a retry each count).
    #[must_use]
    pub fn account_currency_asks(&self) -> usize {
        lock(&self.inner.state).account_currency_asks
    }

    /// Supervise this venue's link through `watch` (every start of `venue` returns it from `link_watch`).
    #[must_use]
    pub fn with_link_watch(self, venue: &str, watch: Arc<dyn LinkWatch>) -> Self {
        lock(&self.inner.state).watches.insert(venue.to_string(), watch);
        self
    }

    /// Every lane call so far, in arrival order.
    pub fn calls(&self) -> Vec<TradeCall> {
        lock(&self.inner.state).calls.clone()
    }

    /// How many adapters were started.
    pub fn starts(&self) -> usize {
        lock(&self.inner.state).adapters.len()
    }

    /// The most recently started adapter of this account, if any.
    pub fn adapter(&self, venue: &str, account: Option<&str>) -> Option<ScriptedAdapter> {
        let key = (venue.to_string(), account.map(str::to_string));
        lock(&self.inner.state)
            .adapters
            .iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, a)| a.clone())
    }
}

impl std::fmt::Debug for ScriptedTrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedTrade").field("venues", &self.inner.venues).finish_non_exhaustive()
    }
}

impl TradeLane for ScriptedTrade {
    fn venues(&self) -> Vec<String> {
        self.inner.venues.clone()
    }

    fn account_tier(&self, venue: &str, account: Option<&str>) -> Result<AccountTier, ArmingBlock> {
        let mut s = lock(&self.inner.state);
        s.calls.push(TradeCall::AccountTier {
            venue: venue.to_string(),
            account: account.map(str::to_string),
        });
        s.tiers
            .get(&(venue.to_string(), account.map(str::to_string)))
            .cloned()
            .unwrap_or(Ok(AccountTier::Demo))
    }

    fn arming_table(&self) -> ArmingTable {
        let mut s = lock(&self.inner.state);
        s.calls.push(TradeCall::ArmingTable);
        s.arming.clone()
    }

    fn preflight(&self, venues: &[String], step: PreflightStep) -> PreflightAnswer {
        let responder = {
            let mut s = lock(&self.inner.state);
            s.calls.push(TradeCall::Preflight { venues: venues.to_vec(), step });
            if s.preflight.is_none() {
                return PreflightAnswer { legs: s.legs.clone(), plan: None };
            }
            s.preflight.clone()
        };
        // The responder runs OUTSIDE the state lock: it may take time and may call back into the lane.
        responder.map_or_else(PreflightAnswer::default, |answer| answer(venues, step))
    }

    fn start_adapter(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
        events: EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<StartedAdapter, AdapterRefusal> {
        let key = (target.venue.clone(), target.account.clone());
        let script = {
            let mut s = lock(&self.inner.state);
            s.calls.push(TradeCall::Start {
                target: target.clone(),
                start: start.clone(),
                trigger: trigger.is_some(),
            });
            s.starts.get(&key).cloned().unwrap_or_else(ScriptedStart::live)
        };
        if !script.delay.is_zero() {
            std::thread::sleep(script.delay);
        }
        let live = script.outcome?;
        let adapter = ScriptedAdapter::new(events, trigger);
        lock(&self.inner.state).adapters.push((key, adapter.clone()));
        let recon: Option<Box<dyn ReconClient>> =
            live.recon.then(|| Box::new(ScriptedRecon { adapter: adapter.clone() }) as _);
        Ok(StartedAdapter {
            bound_tier: live.bound_tier.unwrap_or(target.tier),
            grid: live.grid,
            contract_value: live.contract_value,
            margin_mode: live.margin_mode,
            leg_grids: live.leg_grids,
            client: Box::new(ScriptedExec { adapter }),
            recon,
        })
    }

    fn record_identity(
        &self,
        target: &TradeTarget,
        bound_tier: AccountTier,
        recon: Option<&dyn ReconClient>,
    ) -> Result<(), String> {
        let mut s = lock(&self.inner.state);
        s.calls.push(TradeCall::RecordIdentity {
            target: target.clone(),
            bound_tier,
            had_recon: recon.is_some(),
        });
        s.identity.clone()
    }

    fn resolves_by_coid(&self, venue: &str) -> bool {
        lock(&self.inner.state).resolvers.contains(venue)
    }

    fn symbol_facts(&self, target: &TradeTarget, symbol: &str) -> Result<WireSymbolFacts, String> {
        let mut s = lock(&self.inner.state);
        s.calls.push(TradeCall::SymbolFacts { target: target.clone(), symbol: symbol.to_string() });
        s.facts
            .get(symbol)
            .cloned()
            .unwrap_or_else(|| Err(format!("scripted: no facts for {symbol}")))
    }

    fn symbol_facts_quoted(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        let mut s = lock(&self.inner.state);
        s.calls.push(TradeCall::SymbolFactsQuoted {
            target: target.clone(),
            symbol: symbol.to_string(),
        });
        s.quoted_facts
            .get(symbol)
            .or_else(|| s.facts.get(symbol))
            .cloned()
            .unwrap_or_else(|| Err(format!("scripted: no facts for {symbol}")))
    }

    fn account_currency(&self, target: &TradeTarget, _start: &AdapterStart) -> Option<String> {
        let mut s = lock(&self.inner.state);
        s.account_currency_asks += 1;
        s.account_currencies.get(&target.venue).cloned()
    }

    fn currency_read_is_budgeted(&self, target: &TradeTarget) -> bool {
        lock(&self.inner.state).budgeted_currency_reads.contains(&target.venue)
    }

    fn link_watch(
        &self,
        target: &TradeTarget,
        _start: &AdapterStart,
    ) -> Option<Arc<dyn LinkWatch>> {
        lock(&self.inner.state).watches.get(&target.venue).cloned()
    }

    fn recon_degraded(&self, venue: &str) -> Option<String> {
        lock(&self.inner.state).degraded.get(venue).cloned()
    }

    fn leverage_cap(
        &self,
        target: &TradeTarget,
        _start: &AdapterStart,
    ) -> Option<Result<f64, String>> {
        lock(&self.inner.state).leverage_caps.get(&target.venue).cloned()
    }

    fn symbol_leverage(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
        symbol: &str,
    ) -> Option<Result<f64, String>> {
        let mut s = lock(&self.inner.state);
        s.calls.push(TradeCall::SymbolLeverage {
            target: target.clone(),
            start: start.clone(),
            symbol: symbol.to_string(),
        });
        s.symbol_leverages.get(&(target.venue.clone(), symbol.to_string())).cloned()
    }
}

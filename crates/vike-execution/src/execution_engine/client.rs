//! The `ExecutionClient` venue-adapter seam — the ONE trait every venue bridge implements
//! (and the boxed cross-venue impl). Re-exported by `mod.rs` as `vike_execution::ExecutionClient` and
//! `execution_engine::ExecutionClient`.

use vike_model::events::Event;
use vike_model::{OrderCancelIntent, OrderRequest};

/// The wait an engine runs (decision 0279) before ANY call that
/// sends an order effect out of the process through its client (`submit`, `submit_batch`, `cancel*`,
/// `modify*`, `confirm`). The live core installs one when it journals; it returns once every journal
/// record appended so far is in the kernel's page cache, so no order leaves before its write-ahead
/// record, or [`JournalCannotWrite`] when the journal did not take them within its bound. An engine
/// without one sends at once, as before.
#[derive(Clone)]
pub struct OrderSendFence(std::sync::Arc<dyn Fn() -> Result<(), JournalCannotWrite> + Send + Sync>);

impl OrderSendFence {
    pub fn new(wait: impl Fn() -> Result<(), JournalCannotWrite> + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(wait))
    }

    #[inline]
    pub fn wait(&self) -> Result<(), JournalCannotWrite> {
        (self.0)()
    }
}

/// [`OrderSendFence::wait`]'s refusal: the journal did not take the records within its bound (a full,
/// read-only or failing disk; `vike_journal`'s `WriteFence::wait`). What the engine then does is decision
/// 0279's addendum of 2026-11-23: an effect that can open risk is NOT SENT, and a cancel, a confirm or an exit
/// the position covers leaves unrecorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalCannotWrite;

/// The venue-client seam: `submit`/`cancel` are the ONLY order-mutating calls that leave the
/// core; every state change comes back as venue events through the ingest channel.
pub trait ExecutionClient {
    fn submit(&mut self, request: &OrderRequest);
    fn cancel(&mut self, order_client_id: &str);
    /// Cancel one order, saying WHY (see [`OrderCancelIntent`]). **DEFAULT: drop the intent and call
    /// [`ExecutionClient::cancel`]**, what every venue that does not meter its cancels wants. The
    /// engine calls THIS, never `cancel`, so an override sees every cancel the core issues.
    ///
    /// ⚠ **A venue that overrides this MUST keep [`ExecutionClient::cancel`] consistent with it** —
    /// implement `cancel` as `self.cancel_with_intent(order client id, OrderCancelIntent::Unspecified)` rather than
    /// leaving two independent cancel doors, or a direct `cancel` call silently skips the override.
    fn cancel_with_intent(&mut self, order_client_id: &str, intent: OrderCancelIntent) {
        let _ = intent;
        self.cancel(order_client_id);
    }
    /// Modify a resting order's qty/price in place (RUST-NATIVE HFT surface).
    /// Takes the WHOLE resting `order` (not just its id) so venues that need more than the order client id to
    /// build a modify can get it — Binance-futures modify requires `side` plus BOTH qty and price,
    /// with unchanged fields falling back to `order.qty`/`order.price`. `new_qty`/`new_price` are
    /// the requested changes (`None` = keep the resting value). Fire-and-forget like `cancel`; the
    /// authoritative `OrderModified` returns over the event stream. Default no-op: a client that
    /// cannot modify leaves the order unchanged — override to support native modify.
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        let _ = (order, new_qty, new_price);
    }
    /// Modify a resting order's TRIGGER in place, with its qty and limit as [`ExecutionClient::modify`]
    /// takes them. Reached only for a kind whose `new_price` is NOT its trigger (a stop-limit, a
    /// take-profit: `vike_model::modified_price_is_trigger` is false): a stop's trigger rides `modify`'s
    /// `new_price`, as it always has (`Engine::modify_order_with_trigger`). Fire-and-forget like
    /// `modify`: the venue's `OrderModified` (carrying `new_trigger_price`) or a non-terminal
    /// `OrderModifyRejected` returns over the event stream.
    ///
    /// ⚠ **DEFAULT: refuse, `Err(TRIGGER_AMEND_UNSUPPORTED_REASON)`, and send nothing.** The caller
    /// publishes the `Err` as a non-terminal `OrderModifyRejected` on the order, so a client that never
    /// heard of a trigger amend answers one in words and never moves the limit alone. A WRAPPER forwards
    /// this by hand like every other method, or the inner client's override is shadowed by the refusal.
    fn modify_trigger(
        &mut self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Result<(), &'static str> {
        let _ = (order, new_qty, new_price, new_trigger_price);
        Err(super::TRIGGER_AMEND_UNSUPPORTED_REASON)
    }
    /// Submit many orders at once (RUST-NATIVE HFT surface). Default: fan out to per-order
    /// `submit`, so batching works on every client immediately. Override to use a venue batch
    /// endpoint (Binance `POST /batchOrders`, OKX/Bybit batch). Each order still produces its own
    /// canonical event stream.
    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        for r in requests {
            self.submit(r);
        }
    }
    /// Cancel many orders at once. Default: fan out to per-order `cancel`. Override for a venue
    /// batch/mass-cancel endpoint.
    fn cancel_batch(&mut self, order_client_ids: &[String]) {
        for c in order_client_ids {
            self.cancel(c);
        }
    }
    /// Cancel many orders at once, saying WHY (see [`OrderCancelIntent`]). **DEFAULT: drop the intent
    /// and call [`ExecutionClient::cancel_batch`]**, so a venue that overrode `cancel_batch` with a
    /// native endpoint keeps it (fanning out per order here would silently un-batch it).
    ///
    /// A venue that wants the intent per id overrides THIS and fans out to
    /// [`ExecutionClient::cancel_with_intent`] itself, as `ExecutionClientActor` does for every venue
    /// without a bulk lane; one that DECLARED a bulk lane (`ExecutionClientActor::with_bulk_cancel`) queues
    /// the whole batch with this intent as a single command, so the venue's planner can choose a
    /// native mass-cancel over `n` round trips.
    fn cancel_batch_with_intent(&mut self, order_client_ids: &[String], intent: OrderCancelIntent) {
        let _ = intent;
        self.cancel_batch(order_client_ids);
    }
    /// ACTIVELY re-confirm one order's status with the venue by order-client-id (RUST-NATIVE).
    /// Fire-and-forget like `cancel`: the venue's authoritative terminal
    /// (`OrderAccepted`/`OrderFilled`/`OrderRejected`) returns over the ingest lane. The
    /// confirm-grace watchdog issues this so a WEDGED adapter is PRODDED rather than only waited
    /// on. HARD CONTRACT: the re-query MUST run OFF the core fold thread (on the adapter's own
    /// thread) — never block here. DEFAULT no-op (the watchdog's last-resort reject backstops it);
    /// override to run the venue's EXISTING status re-query (the one behind
    /// `resolve_ambiguous_submit`).
    fn confirm(&mut self, order_client_id: &str) {
        let _ = order_client_id;
    }
    /// Raise this client's stop flags and RETURN AT ONCE — join nothing, block on nothing. Phase
    /// one of a teardown across SEVERAL clients: run it on every client first, and each one's
    /// threads are already winding down by the time the first [`ExecutionClient::detach`] join is
    /// entered.
    ///
    /// **Why.** A venue's `detach` costs at least one stop-flag POLL on its user-data pump
    /// (`crates/vike-bridge-core/src/websocket_session/user.rs`'s `USER_DATA_POLL` is 1s, and
    /// `vike_bridge_core::websocket_session::user::run_user` re-checks the flag on that
    /// cadence), so detaching one engine at a time pays that wind-down PER ENGINE; raised on every
    /// client first, the whole mount costs about ONE. The EXACT twin of
    /// `vike_data::DataClient::begin_shutdown` (`crates/vike-recorder/src/runtime.rs`'s `stop_all`
    /// carries the shared cost model). The caller is `vike_core::runtime`'s `CoreThread::run`
    /// teardown, via [`crate::Engine::begin_shutdown`].
    ///
    /// **DEFAULT no-op** (the additive-growth rule of `modify` / `confirm` / `on_bar`): a REQUIRED
    /// method would redden `crates/vike-bridge-core/tests/bridge_conformance.rs` for every roster
    /// venue at once. A client that does not override it is still torn down correctly by
    /// [`ExecutionClient::detach`]; it just pays its wind-down serially — so a WRAPPER that does
    /// not delegate it silently inherits this no-op.
    ///
    /// ⚠ **Two rules an override MUST hold.** It may not JOIN anything (that hands the serial cost
    /// straight back), and it must be IDEMPOTENT with [`ExecutionClient::detach`] — `detach` runs
    /// after it on the same client, re-sending the same stop signal, and is also still called on
    /// its OWN (by `Engine::shutdown`'s other callers and by tests).
    ///
    /// ⚠ It deliberately does NOT cancel, flatten or touch resting orders. The one shutdown policy
    /// that reaches the venue, `CoreConfig::cancel_orders_on_shutdown`, runs BEFORE phase one while
    /// every client is still attached; order traffic here would hit a client whose stop flag is up.
    fn begin_detach(&mut self) {}
    /// symmetric shutdown half; default no-op
    fn detach(&mut self) {}
    /// In-process clients (TestExecutionClient) synthesize venue events; the core loop
    /// pumps them through the bus after each command/event. Real venue clients return
    /// None — their events arrive over the ingest channel.
    fn poll_events(&mut self) -> Option<Event> {
        None
    }

    /// PAPER-MODE seam: the core calls this on each closed bar of the strategy
    /// mount's series BEFORE the strategy runs — an in-process paper exchange fills its
    /// resting book here with the backtest engine's exact next-open semantics. Real
    /// venue clients ignore it.
    fn on_bar(&mut self, _bar: &vike_marketdata::Bar) {}

    /// [`ExecutionClient::on_bar`] with the bar's QUOTE SIDES, the verb the core calls: `Some` with
    /// the bid's and the ask's path over the bar's window when the node folded the venue's quotes
    /// beside it, `None` where none came. The paper exchange judges a buy on the ask's path and a
    /// sell on the bid's (`vike_fills::fill_model::side_bar`, owner 2026-11-16); the default drops the
    /// sides and calls `on_bar`. ⚠ A WRAPPER must forward THIS verb too, or its inner paper book gets
    /// the default and fills at the mid or the print again.
    fn on_bar_sides(
        &mut self,
        bar: &vike_marketdata::Bar,
        sides: Option<&vike_marketdata::BarSides>,
    ) {
        let _ = sides;
        self.on_bar(bar);
    }

    /// **The engine installed `value` as what one unit of `symbol` is worth**, from the venue's facts
    /// (`Engine::admit_symbol`'s mounted arm on a `paper` engine). DEFAULT: nothing, a venue books by its
    /// own unit. The paper exchange keeps it for a symbol whose name fixes no unit (an IG epic: the owner's
    /// ruling of 2026-11-16), so it takes that symbol's orders and its ledger and fills agree. ⚠ A WRAPPER
    /// must forward it, or its inner paper book keeps refusing the symbol.
    fn on_contract_value(&mut self, symbol: &str, value: vike_model::ContractValue) {
        let _ = (symbol, value);
    }

    /// PAPER-MODE seam: one QUOTE of `symbol` (the venue's bid and ask at `timestamp`; a side that is not a
    /// finite price above zero is one the quote does not state), the clock a paper book fills its resting
    /// orders on the moment the market reaches them (decision 0285's amendment of 2026-11-16), and the
    /// current market a paper market order meets or is refused by (its amendment of 2026-11-17). The core calls it for a stated `paper` engine only, so a node
    /// with none does nothing per quote. Real venue clients ignore it (the default). ⚠ A WRAPPER must
    /// forward it, as it forwards [`ExecutionClient::on_bar_sides`].
    fn on_quote(&mut self, symbol: &str, bid: f64, ask: f64, timestamp: i64) {
        let _ = (symbol, bid, ask, timestamp);
    }

    /// PAPER-MODE seam: fill the resting orders `order_client_ids` names at the CLOSE of the last bar
    /// the book was shown (its quote sides' closes where it had them), stamped with that bar's END: its
    /// window's start (what its sides state, else the bar's own stamp) plus `bar_width_ms`. They are the
    /// orders a strategy whose file declares `fill = "close"` placed on that bar. The core calls it
    /// right after that strategy's bar step drained, for a stated `paper` engine only. Real venue
    /// clients ignore it (the default). ⚠ A WRAPPER must forward it.
    fn fill_at_close(&mut self, order_client_ids: &[String], bar_width_ms: i64) {
        let _ = (order_client_ids, bar_width_ms);
    }

    /// PAPER-MODE seam: while `hold` is `true`, an order submitted is a `fill = "close"` strategy's bar
    /// order that [`ExecutionClient::fill_at_close`] fills at its bar's close next, so the book does not
    /// meet it at the standing quote, whatever quote came after that bar (a sandboxed strategy's reply
    /// arrives after some). The core raises it around such a drain, for a stated `paper` engine only.
    /// Real venue clients ignore it (the default). ⚠ A WRAPPER must forward it.
    fn hold_for_close(&mut self, hold: bool) {
        let _ = hold;
    }

    /// What THIS client's [`ExecutionClient::modify`] does to a resting order's outstanding size —
    /// what `Engine::modify_order` needs before it may net a partially filled order's
    /// executed lots out of the pre-trade projection.
    ///
    /// **DEFAULT `None` = "ask the venue table"** (`vike_venues::amend_semantics` on the engine's
    /// venue string), what every real venue adapter wants. Override ONLY when this client is not
    /// the venue it is mounted under.
    ///
    /// ⚠ **The one override is the paper exchange.** `vike_tradehub_core::make_engine` (and
    /// `vike_tradehub_core::build_paper_strategy_core_with`) label a `vike_paper::ExecutionClientPaper` mount with
    /// the REAL venue string, so a paper mount on binance/okx/bybit looks up `InPlaceTotal`, while
    /// the paper book's `modify` assigns the amend's quantity to the REMAINING size — netting there
    /// would let the gate assume `q − filled` while the book executes `q`. Declared on the CLIENT,
    /// the convention travels with the object that implements it and no mount can forget it.
    fn amend_semantics(&self) -> Option<vike_venues::AmendSemantics> {
        None
    }

    /// The id THIS venue's reports carry for the order submitted under `order_client_id`, when that id is a
    /// pure function of `order_client_id` and NOT `order_client_id` itself — Hyperliquid's `cloid` is `keccak(order client id)`.
    ///
    /// **DEFAULT `None` = "the venue echoes our order client id verbatim (or carries none)"**, which is every
    /// other roster venue: their reports already name the order client id, so nothing needs translating.
    ///
    /// **The contract.** `Some(w)` promises that an open-order report or a fill report of the
    /// venue names this order by exactly `w` in `order_client_id`, in this process and after a
    /// restart alike, and that `w` can be computed without any state the restart lost. It is the
    /// one input of the restore's translation step
    /// (`crates/vike-core/src/runtime/restore.rs`'s module doc): vike-core, which sits ABOVE every
    /// bridge and cannot compute a venue hash, asks the client for the wire id of each restored
    /// order client id, rewrites the pass's reports from wire id back to order client id, and only then plans. The bridge
    /// answers a pure question and never reads the ownership file (it is a lower layer than the
    /// core and knows no mounts).
    ///
    /// Pure and cheap, called at pass cadence only, never per market message. It must not block,
    /// touch the network or allocate registry state.
    fn wire_id_for(&self, order_client_id: &str) -> Option<String> {
        let _ = order_client_id;
        None
    }

    /// Tell this client that orders a PREVIOUS session left resting are about to be live again:
    /// each [`RestoredOrder`] names the order client id the engine registry now holds, the symbol it rests on
    /// and, when the core holds one, the id the venue acknowledged it under. Called by the restore
    /// step AFTER the planner adopted them, on the fold thread, and by the tradehub's re-teaching
    /// after a gatewayhub restart (`crates/vike-tradehub/src/venue_backend/gateway/reteach.rs`).
    ///
    /// **DEFAULT no-op**: a client whose cancel, modify and event paths need no state the restart
    /// lost (every venue whose cancel is keyed by the order client id alone) has nothing to learn. A client
    /// that keeps per-order state in memory (Hyperliquid's `OrderClientIdVenueRegistry` and its order client id -> asset
    /// map, fxcm's venue-order-id routes, IG's `dealId`s) overrides this to rebuild exactly that, so
    /// a later `cancel`, a WS event or a fill of the restored order resolves as it would have in the
    /// session that placed it. A client that needs no venue id ignores
    /// [`RestoredOrder::venue_order_id`]; one that needs it treats `None` as today's answer (nothing
    /// to rebuild by).
    ///
    /// ⚠ **Must return at once.** Like `cancel` it runs on the single-writer fold: hand the work to
    /// the adapter's own thread through the command lane it already has (FIFO with later
    /// commands, so a `cancel` sent after this one sees the state) and never touch the network
    /// here. Idempotent: the same order may be announced again by a later pass.
    fn adopt_restored(&mut self, orders: &[RestoredOrder]) {
        let _ = orders;
    }
}

/// One order a previous session left resting, as [`ExecutionClient::adopt_restored`] teaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredOrder {
    /// The order client id the engine registry holds it under.
    pub order_client_id: String,
    /// The unified symbol it rests on.
    pub symbol: String,
    /// The id the venue acknowledged it under, as the core holds it (the accept's, or the one a
    /// modify replaced it with); `None` when the core holds none (never acknowledged, or an older
    /// peer's frame). Only a client that keys its own state by the venue's id reads it.
    pub venue_order_id: Option<String>,
}

impl RestoredOrder {
    /// A restored order the core holds no venue order id for.
    pub fn new(order_client_id: impl Into<String>, symbol: impl Into<String>) -> Self {
        Self {
            order_client_id: order_client_id.into(),
            symbol: symbol.into(),
            venue_order_id: None,
        }
    }

    /// The order a venue report names by our order client id, with the report's venue order id (an empty one is
    /// none); `None` for a report that carries no order client id.
    pub fn of_report(report: &vike_model::OrderStatusReport) -> Option<Self> {
        Some(Self {
            order_client_id: report.order_client_id.clone()?,
            symbol: report.symbol.clone(),
            venue_order_id: (!report.venue_order_id.is_empty())
                .then(|| report.venue_order_id.to_string()),
        })
    }
}

/// Heterogeneous-venue support (cross-venue runtime): a boxed client IS a client, so one
/// `CoreThread<Box<dyn ExecutionClient + Send>>` can drive N different venue adapters.
/// Every method delegates explicitly — trait-object defaults would otherwise shadow a
/// concrete client's overrides (batch endpoints, paper on_bar).
impl ExecutionClient for Box<dyn ExecutionClient + Send> {
    fn submit(&mut self, request: &OrderRequest) {
        (**self).submit(request);
    }
    fn cancel(&mut self, order_client_id: &str) {
        (**self).cancel(order_client_id);
    }
    fn cancel_with_intent(&mut self, order_client_id: &str, intent: OrderCancelIntent) {
        (**self).cancel_with_intent(order_client_id, intent);
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        (**self).modify(order, new_qty, new_price);
    }
    fn modify_trigger(
        &mut self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Result<(), &'static str> {
        (**self).modify_trigger(order, new_qty, new_price, new_trigger_price)
    }
    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        (**self).submit_batch(requests);
    }
    fn cancel_batch(&mut self, order_client_ids: &[String]) {
        (**self).cancel_batch(order_client_ids);
    }
    fn cancel_batch_with_intent(&mut self, order_client_ids: &[String], intent: OrderCancelIntent) {
        (**self).cancel_batch_with_intent(order_client_ids, intent);
    }
    fn confirm(&mut self, order_client_id: &str) {
        (**self).confirm(order_client_id);
    }
    fn begin_detach(&mut self) {
        (**self).begin_detach();
    }
    fn detach(&mut self) {
        (**self).detach();
    }
    fn poll_events(&mut self) -> Option<Event> {
        (**self).poll_events()
    }
    fn on_bar(&mut self, bar: &vike_marketdata::Bar) {
        (**self).on_bar(bar);
    }
    fn on_bar_sides(
        &mut self,
        bar: &vike_marketdata::Bar,
        sides: Option<&vike_marketdata::BarSides>,
    ) {
        (**self).on_bar_sides(bar, sides);
    }
    fn on_contract_value(&mut self, symbol: &str, value: vike_model::ContractValue) {
        (**self).on_contract_value(symbol, value);
    }
    fn on_quote(&mut self, symbol: &str, bid: f64, ask: f64, timestamp: i64) {
        (**self).on_quote(symbol, bid, ask, timestamp);
    }
    fn fill_at_close(&mut self, order_client_ids: &[String], bar_width_ms: i64) {
        (**self).fill_at_close(order_client_ids, bar_width_ms);
    }
    fn hold_for_close(&mut self, hold: bool) {
        (**self).hold_for_close(hold);
    }
    fn amend_semantics(&self) -> Option<vike_venues::AmendSemantics> {
        (**self).amend_semantics()
    }
    fn wire_id_for(&self, order_client_id: &str) -> Option<String> {
        (**self).wire_id_for(order_client_id)
    }
    fn adopt_restored(&mut self, orders: &[RestoredOrder]) {
        (**self).adopt_restored(orders);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client of the shape EVERY venue bridge has today: it overrides `cancel` and knows nothing
    /// about intent. What the trait's defaults do to it IS the backward-compatibility claim.
    #[derive(Default)]
    struct LegacyClient {
        cancels: Vec<String>,
    }

    impl ExecutionClient for LegacyClient {
        fn submit(&mut self, _request: &OrderRequest) {}
        fn cancel(&mut self, order_client_id: &str) {
            self.cancels.push(order_client_id.to_string());
        }
    }

    /// A client that DOES classify (the Polymarket shape): both doors agree because `cancel` is
    /// written in terms of `cancel_with_intent`, as the trait doc requires. Records through a
    /// shared handle so it stays observable after it has been BOXED.
    #[derive(Default)]
    struct IntentClient {
        seen: std::sync::Arc<std::sync::Mutex<Vec<(String, OrderCancelIntent)>>>,
    }

    impl ExecutionClient for IntentClient {
        fn submit(&mut self, _request: &OrderRequest) {}
        fn cancel(&mut self, order_client_id: &str) {
            self.cancel_with_intent(order_client_id, OrderCancelIntent::Unspecified);
        }
        fn cancel_with_intent(&mut self, order_client_id: &str, intent: OrderCancelIntent) {
            self.seen.lock().expect("seen").push((order_client_id.to_string(), intent));
        }
        fn cancel_batch_with_intent(
            &mut self,
            order_client_ids: &[String],
            intent: OrderCancelIntent,
        ) {
            for c in order_client_ids {
                self.cancel_with_intent(c, intent);
            }
        }
    }

    /// A client that wires the phase-one seam, recording through a shared handle so it stays
    /// observable after it has been BOXED.
    #[derive(Default)]
    struct DetachClient {
        seen: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    impl ExecutionClient for DetachClient {
        fn submit(&mut self, _request: &OrderRequest) {}
        fn cancel(&mut self, _order_client_id: &str) {}
        fn begin_detach(&mut self) {
            self.seen.lock().expect("seen").push("begin");
        }
        fn detach(&mut self) {
            self.seen.lock().expect("seen").push("detach");
        }
    }

    /// The backward-compatibility claim for the teardown seam, stated as a test: a venue that never
    /// heard of `begin_detach` inherits a no-op, so phase one is free where it buys nothing and no
    /// bridge is forced to change. `LegacyClient` overrides neither method, and calling both must
    /// leave it exactly as it was.
    #[test]
    fn a_venue_that_wired_no_begin_detach_is_untouched_by_phase_one() {
        let mut c = LegacyClient::default();
        c.begin_detach();
        c.detach();
        assert!(c.cancels.is_empty(), "phase one must not place venue traffic of any kind");
    }

    /// The trait-object impl must DELEGATE `begin_detach`: a boxed client falling through to the
    /// default would be silently skipped by the core's raise-all phase — the shadowing hazard the
    /// `Box<dyn …>` impl's doc exists for, invisible without an assertion.
    #[test]
    fn a_boxed_client_keeps_its_begin_detach_override() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut boxed: Box<dyn ExecutionClient + Send> =
            Box::new(DetachClient { seen: std::sync::Arc::clone(&seen) });
        boxed.begin_detach();
        boxed.detach();
        assert_eq!(
            *seen.lock().expect("seen"),
            vec!["begin", "detach"],
            "the boxed impl must delegate BOTH halves, in order"
        );
    }

    /// Backward compatibility, stated as a test rather than as a comment: a venue that implements
    /// only `cancel` receives EVERY intent through it, unchanged and undropped.
    #[test]
    fn a_venue_that_knows_no_intent_still_receives_every_cancel() {
        let mut c = LegacyClient::default();
        c.cancel("plain");
        c.cancel_with_intent("routine", OrderCancelIntent::Routine);
        c.cancel_with_intent("riskoff", OrderCancelIntent::RiskOff);
        c.cancel_batch_with_intent(
            &["b1".to_string(), "b2".to_string()],
            OrderCancelIntent::Routine,
        );
        assert_eq!(c.cancels, vec!["plain", "routine", "riskoff", "b1", "b2"]);
    }

    /// The trait-object impl must DELEGATE the new methods: a boxed intent-aware client that fell
    /// through to the default would answer `cancel`, silently discarding the classification —
    /// exactly the shadowing hazard the `Box<dyn …>` impl's own doc was written for.
    #[test]
    fn a_boxed_client_keeps_its_intent_override() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut boxed: Box<dyn ExecutionClient + Send> =
            Box::new(IntentClient { seen: std::sync::Arc::clone(&seen) });
        boxed.cancel_with_intent("c1", OrderCancelIntent::Routine);
        boxed.cancel_batch_with_intent(&["c2".to_string()], OrderCancelIntent::RiskOff);
        boxed.cancel("c3");
        assert_eq!(
            *seen.lock().expect("seen"),
            vec![
                ("c1".to_string(), OrderCancelIntent::Routine),
                ("c2".to_string(), OrderCancelIntent::RiskOff),
                ("c3".to_string(), OrderCancelIntent::Unspecified),
            ],
            "the boxed impl must delegate BOTH new methods — a default that fell through to \
             `cancel` would report every one of these as Unspecified"
        );
    }

    /// A client that moves triggers, recording through a shared handle so it stays observable boxed.
    #[derive(Default)]
    struct TriggerClient {
        seen: std::sync::Arc<std::sync::Mutex<Vec<(String, f64)>>>,
    }

    impl ExecutionClient for TriggerClient {
        fn submit(&mut self, _request: &OrderRequest) {}
        fn cancel(&mut self, _order_client_id: &str) {}
        fn modify_trigger(
            &mut self,
            order: &OrderRequest,
            _new_quantity: Option<f64>,
            _new_price: Option<f64>,
            new_trigger_price: f64,
        ) -> Result<(), &'static str> {
            self.seen
                .lock()
                .expect("seen")
                .push((order.order_client_id.clone(), new_trigger_price));
            Ok(())
        }
    }

    /// A client that never heard of a trigger amend REFUSES one in words (the caller publishes them)
    /// and sends nothing, and a boxed client that moves triggers keeps its override.
    #[test]
    fn a_trigger_amend_is_refused_by_default_and_forwarded_when_boxed() {
        let order = OrderRequest { order_client_id: "c1".into(), ..Default::default() };
        let mut legacy = LegacyClient::default();
        assert_eq!(
            legacy.modify_trigger(&order, None, Some(94.0), 95.0),
            Err(super::super::TRIGGER_AMEND_UNSUPPORTED_REASON)
        );
        assert!(legacy.cancels.is_empty(), "the refusal places no venue traffic");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut boxed: Box<dyn ExecutionClient + Send> =
            Box::new(TriggerClient { seen: std::sync::Arc::clone(&seen) });
        assert_eq!(boxed.modify_trigger(&order, None, None, 96.0), Ok(()));
        assert_eq!(*seen.lock().expect("seen"), vec![("c1".to_string(), 96.0)]);
    }

    /// A client that translates its wire id and records what the restore announced, through a
    /// shared handle so it stays observable after it has been BOXED.
    #[derive(Default)]
    struct WireClient {
        adopted: std::sync::Arc<std::sync::Mutex<Vec<RestoredOrder>>>,
    }

    impl ExecutionClient for WireClient {
        fn submit(&mut self, _request: &OrderRequest) {}
        fn cancel(&mut self, _order_client_id: &str) {}
        fn wire_id_for(&self, order_client_id: &str) -> Option<String> {
            Some(format!("0x{order_client_id}"))
        }
        fn adopt_restored(&mut self, orders: &[RestoredOrder]) {
            self.adopted.lock().expect("adopted").extend(orders.iter().cloned());
        }
    }

    /// The backward-compatibility claim for the restore seam, stated as a test: a venue that never
    /// heard of `wire_id_for` / `adopt_restored` echoes its order client id (`None`) and learns nothing (a
    /// no-op). `LegacyClient` is the shape every roster bridge but one has; the lane compile of the
    /// whole workspace is the proof that every other implementor still builds by default.
    #[test]
    fn a_venue_that_wired_no_restore_seam_echoes_its_order_client_id_and_learns_nothing() {
        let mut c = LegacyClient::default();
        assert_eq!(
            c.wire_id_for("c1"),
            None,
            "the default is: the report names the order client id itself"
        );
        c.adopt_restored(&[RestoredOrder::new("c1", "BTCUSDT")]);
        assert!(c.cancels.is_empty(), "announcing a restored order must place no venue traffic");
    }

    /// The trait-object impl must DELEGATE both restore methods: a boxed Hyperliquid-shaped client
    /// that fell through to the defaults would silently restore nothing (the pass would find no wire
    /// id and the client would never learn the order) — the shadowing hazard the `Box<dyn …>`
    /// impl's doc exists for.
    #[test]
    fn a_boxed_client_keeps_its_restore_seam_overrides() {
        let adopted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut boxed: Box<dyn ExecutionClient + Send> =
            Box::new(WireClient { adopted: std::sync::Arc::clone(&adopted) });
        assert_eq!(boxed.wire_id_for("c1").as_deref(), Some("0xc1"));
        let taught =
            RestoredOrder { venue_order_id: Some("v1".into()), ..RestoredOrder::new("c1", "BTC") };
        boxed.adopt_restored(std::slice::from_ref(&taught));
        assert_eq!(
            *adopted.lock().expect("adopted"),
            vec![taught],
            "the boxed impl must delegate adopt_restored, venue order id and all"
        );
    }
}

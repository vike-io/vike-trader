//! The `ExecutionClient` venue-adapter seam — the ONE trait every venue bridge implements
//! (and the boxed cross-venue impl). Re-exported by `mod.rs` as `vike_exec::ExecutionClient` and
//! `execution_engine::ExecutionClient`.

use vike_model::OrderRequest;
use vike_model::events::Event;

/// WHY a cancel is being issued — the ONE cross-venue classification, so a venue that meters its
/// cancels can tell routine ladder churn from an emergency flatten. A venue-side cancel budget is a
/// liveness hazard: Polymarket meters cancel tokens per signer, and a maker that spends its bucket
/// on requote churn is rate-limit-LOCKED at the moment it needs to pull a book.
///
/// ⚠ **[`CancelIntent::Unspecified`] is the DEFAULT and it is the FLATTEN-SAFE value, not a
/// "don't care".** An unclassified cancel gets the emergency treatment — fire it, never hold it
/// back — because shedding a routine cancel costs one stale quote, while shedding an emergency one
/// leaves a live order resting where the operator wanted out. Only [`CancelIntent::Routine`] may be
/// held back, and [`CancelIntent::may_be_shed`] is the ONE place that says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CancelIntent {
    /// The caller could not classify this cancel (an operator's DOM click, a tradehub/CLI ticket,
    /// a book-maintenance sweep). Never shed, like [`CancelIntent::RiskOff`]; a SEPARATE variant so
    /// a venue can later act only on a DECLARED emergency (Polymarket's account-wide `cancel-all`,
    /// say) and tell "the operator said get me out" from "nobody said anything".
    #[default]
    Unspecified,
    /// Maker ladder churn / requote. The ONLY intent a venue may hold back under its own budget,
    /// and only ever by reporting a NON-terminal cancel-reject — a shed cancel must never vanish
    /// silently, because the order is still resting.
    Routine,
    /// A declared risk-off action: flatten, market-exit, dead-man trip, safe-state sweep. Never
    /// shed — being able to fire here is the whole point of any reserve a venue holds back.
    RiskOff,
}

impl CancelIntent {
    /// May a venue hold THIS cancel back under its own rate/credit budget? True for
    /// [`CancelIntent::Routine`] and nothing else — the flatten-safe default and a declared
    /// risk-off action both answer `false`.
    ///
    /// One function rather than a `match` per venue: `Unspecified` shedding like `RiskOff` and NOT
    /// like `Routine` is the whole safety property, one `_ =>` arm away from inverted.
    pub fn may_be_shed(self) -> bool {
        matches!(self, CancelIntent::Routine)
    }
}

/// The venue-client seam: `submit`/`cancel` are the ONLY order-mutating calls that leave the
/// core; every state change comes back as venue events through the ingest channel.
pub trait ExecutionClient {
    fn submit(&mut self, request: &OrderRequest);
    fn cancel(&mut self, client_order_id: &str);
    /// Cancel one order, saying WHY (see [`CancelIntent`]). **DEFAULT: drop the intent and call
    /// [`ExecutionClient::cancel`]**, what every venue that does not meter its cancels wants. The
    /// engine calls THIS, never `cancel`, so an override sees every cancel the core issues.
    ///
    /// ⚠ **A venue that overrides this MUST keep [`ExecutionClient::cancel`] consistent with it** —
    /// implement `cancel` as `self.cancel_with_intent(coid, CancelIntent::Unspecified)` rather than
    /// leaving two independent cancel doors, or a direct `cancel` call silently skips the override.
    fn cancel_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
        let _ = intent;
        self.cancel(client_order_id);
    }
    /// Modify a resting order's qty/price in place (RUST-NATIVE HFT surface).
    /// Takes the WHOLE resting `order` (not just its id) so venues that need more than the coid to
    /// build a modify can get it — Binance-futures modify requires `side` plus BOTH qty and price,
    /// with unchanged fields falling back to `order.qty`/`order.price`. `new_qty`/`new_price` are
    /// the requested changes (`None` = keep the resting value). Fire-and-forget like `cancel`; the
    /// authoritative `OrderModified` returns over the event stream. Default no-op: a client that
    /// cannot modify leaves the order unchanged — override to support native modify.
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        let _ = (order, new_qty, new_price);
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
    fn cancel_batch(&mut self, client_order_ids: &[String]) {
        for c in client_order_ids {
            self.cancel(c);
        }
    }
    /// Cancel many orders at once, saying WHY (see [`CancelIntent`]). **DEFAULT: drop the intent
    /// and call [`ExecutionClient::cancel_batch`]**, so a venue that overrode `cancel_batch` with a
    /// native endpoint keeps it (fanning out per order here would silently un-batch it).
    ///
    /// A venue that wants the intent per id overrides THIS and fans out to
    /// [`ExecutionClient::cancel_with_intent`] itself, as `ExecActor` does for every venue
    /// without a bulk lane; one that DECLARED a bulk lane (`ExecActor::with_bulk_cancel`) queues
    /// the whole batch with this intent as a single command, so the venue's planner can choose a
    /// native mass-cancel over `n` round trips.
    fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
        let _ = intent;
        self.cancel_batch(client_order_ids);
    }
    /// ACTIVELY re-confirm one order's status with the venue by client-order-id (RUST-NATIVE).
    /// Fire-and-forget like `cancel`: the venue's authoritative terminal
    /// (`OrderAccepted`/`OrderFilled`/`OrderRejected`) returns over the ingest lane. The
    /// confirm-grace watchdog issues this so a WEDGED adapter is PRODDED rather than only waited
    /// on. HARD CONTRACT: the re-query MUST run OFF the core fold thread (on the adapter's own
    /// thread) — never block here. DEFAULT no-op (the watchdog's last-resort reject backstops it);
    /// override to run the venue's EXISTING status re-query (the one behind
    /// `resolve_ambiguous_submit`).
    fn confirm(&mut self, client_order_id: &str) {
        let _ = client_order_id;
    }
    /// Raise this client's stop flags and RETURN AT ONCE — join nothing, block on nothing. Phase
    /// one of a teardown across SEVERAL clients: run it on every client first, and each one's
    /// threads are already winding down by the time the first [`ExecutionClient::detach`] join is
    /// entered.
    ///
    /// **Why.** A venue's `detach` costs at least one stop-flag POLL on its user-data pump
    /// (`crates/bridges/binance/src/family/listenkey.rs`'s `POLL` is 1s, and
    /// `vike_bridge_core::user_data::run_user_data_forever_with_idle` re-checks the flag on that
    /// cadence), so detaching one engine at a time pays that wind-down PER ENGINE; raised on every
    /// client first, the whole mount costs about ONE. The EXACT twin of
    /// `vike_data::DataClient::begin_shutdown` (`crates/vike-recorder/src/runtime.rs`'s `stop_all`
    /// carries the shared cost model). The caller is `vike_core::runtime`'s `CoreThread::run`
    /// teardown, via [`crate::ExecutionEngine::begin_shutdown`].
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
    /// its OWN (by `ExecutionEngine::shutdown`'s other callers and by tests).
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
    fn on_bar(&mut self, _bar: &vike_model::Bar) {}

    /// What THIS client's [`ExecutionClient::modify`] does to a resting order's outstanding size —
    /// what `ExecutionEngine::modify_order` needs before it may net a partially filled order's
    /// executed lots out of the pre-trade projection.
    ///
    /// **DEFAULT `None` = "ask the venue table"** (`vike_model::amend_semantics` on the engine's
    /// venue string), what every real venue adapter wants. Override ONLY when this client is not
    /// the venue it is mounted under.
    ///
    /// ⚠ **The one override is the paper exchange.** `vike_mount::make_engine` (and
    /// `vike_mount::build_paper_maker_core`) label a `vike_paper::PaperExecutionClient` mount with
    /// the REAL venue string, so a paper mount on binance/okx/bybit looks up `InPlaceTotal`, while
    /// the paper book's `modify` assigns the amend's quantity to the REMAINING size — netting there
    /// would let the gate assume `q − filled` while the book executes `q`. Declared on the CLIENT,
    /// the convention travels with the object that implements it and no mount can forget it.
    fn amend_semantics(&self) -> Option<vike_model::AmendSemantics> {
        None
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
    fn cancel(&mut self, client_order_id: &str) {
        (**self).cancel(client_order_id);
    }
    fn cancel_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
        (**self).cancel_with_intent(client_order_id, intent);
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        (**self).modify(order, new_qty, new_price);
    }
    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        (**self).submit_batch(requests);
    }
    fn cancel_batch(&mut self, client_order_ids: &[String]) {
        (**self).cancel_batch(client_order_ids);
    }
    fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
        (**self).cancel_batch_with_intent(client_order_ids, intent);
    }
    fn confirm(&mut self, client_order_id: &str) {
        (**self).confirm(client_order_id);
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
    fn on_bar(&mut self, bar: &vike_model::Bar) {
        (**self).on_bar(bar);
    }
    fn amend_semantics(&self) -> Option<vike_model::AmendSemantics> {
        (**self).amend_semantics()
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
        fn cancel(&mut self, client_order_id: &str) {
            self.cancels.push(client_order_id.to_string());
        }
    }

    /// A client that DOES classify (the Polymarket shape): both doors agree because `cancel` is
    /// written in terms of `cancel_with_intent`, as the trait doc requires. Records through a
    /// shared handle so it stays observable after it has been BOXED.
    #[derive(Default)]
    struct IntentClient {
        seen: std::sync::Arc<std::sync::Mutex<Vec<(String, CancelIntent)>>>,
    }

    impl ExecutionClient for IntentClient {
        fn submit(&mut self, _request: &OrderRequest) {}
        fn cancel(&mut self, client_order_id: &str) {
            self.cancel_with_intent(client_order_id, CancelIntent::Unspecified);
        }
        fn cancel_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
            self.seen.lock().expect("seen").push((client_order_id.to_string(), intent));
        }
        fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
            for c in client_order_ids {
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
        fn cancel(&mut self, _client_order_id: &str) {}
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

    #[test]
    fn the_default_intent_is_the_flatten_safe_one() {
        assert_eq!(CancelIntent::default(), CancelIntent::Unspecified);
        assert!(
            !CancelIntent::default().may_be_shed(),
            "an unclassified cancel is never held back"
        );
    }

    /// The whole safety property in one assertion: ROUTINE and nothing else may be shed.
    #[test]
    fn only_routine_may_be_shed() {
        assert!(CancelIntent::Routine.may_be_shed());
        assert!(!CancelIntent::Unspecified.may_be_shed());
        assert!(!CancelIntent::RiskOff.may_be_shed());
    }

    /// Backward compatibility, stated as a test rather than as a comment: a venue that implements
    /// only `cancel` receives EVERY intent through it, unchanged and undropped.
    #[test]
    fn a_venue_that_knows_no_intent_still_receives_every_cancel() {
        let mut c = LegacyClient::default();
        c.cancel("plain");
        c.cancel_with_intent("routine", CancelIntent::Routine);
        c.cancel_with_intent("riskoff", CancelIntent::RiskOff);
        c.cancel_batch_with_intent(&["b1".to_string(), "b2".to_string()], CancelIntent::Routine);
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
        boxed.cancel_with_intent("c1", CancelIntent::Routine);
        boxed.cancel_batch_with_intent(&["c2".to_string()], CancelIntent::RiskOff);
        boxed.cancel("c3");
        assert_eq!(
            *seen.lock().expect("seen"),
            vec![
                ("c1".to_string(), CancelIntent::Routine),
                ("c2".to_string(), CancelIntent::RiskOff),
                ("c3".to_string(), CancelIntent::Unspecified),
            ],
            "the boxed impl must delegate BOTH new methods — a default that fell through to \
             `cancel` would report every one of these as Unspecified"
        );
    }
}

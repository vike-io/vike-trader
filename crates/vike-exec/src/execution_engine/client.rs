//! The `ExecutionClient` venue-adapter seam — the ONE trait every venue bridge implements
//! (and the boxed cross-venue impl). Isolated from the engine so the contract reads on its own;
//! re-exported by `mod.rs` so `vike_exec::ExecutionClient` and `execution_engine::ExecutionClient`
//! resolve exactly as before.

use vike_model::events::Event;
use vike_model::OrderRequest;

/// WHY a cancel is being issued — the ONE cross-venue classification, so a venue that meters its
/// cancels can tell routine ladder churn from an emergency flatten.
///
/// It exists because a venue-side cancel budget is a real liveness hazard: Polymarket meters order
/// and cancel tokens per signer, and a maker that spends its whole cancel bucket on requote churn
/// is rate-limit-LOCKED at the moment it needs to pull a book. Holding a reserve back is only
/// possible if the shedding decision can tell the two apart, and the `ExecutionClient` seam carried
/// no such fact: every cancel looked identical to every venue.
///
/// ⚠ **[`CancelIntent::Unspecified`] is the DEFAULT and it is the FLATTEN-SAFE value, not a
/// "don't care".** A caller that cannot classify its cancel gets the treatment an emergency gets —
/// fire it, never hold it back — because the failure modes are not symmetric: shedding a routine
/// cancel costs one stale quote for one refill window, while shedding an emergency one leaves a
/// live order resting in the situation the operator was trying to get out of. Only
/// [`CancelIntent::Routine`] may ever be held back, and [`CancelIntent::may_be_shed`] is the ONE
/// place that says so, so a venue never re-derives the rule from a `match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CancelIntent {
    /// The caller could not classify this cancel — an operator's DOM click, a tradehub/CLI ticket,
    /// a book-maintenance sweep. Treated exactly as [`CancelIntent::RiskOff`] by every venue today
    /// (never shed); it is a SEPARATE variant so a venue that later wants to act only on a
    /// DECLARED emergency (Polymarket's account-wide `cancel-all`, say) can tell "the operator said
    /// get me out" from "nobody said anything".
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
    /// One function rather than a `match` per venue: the fact that `Unspecified` sheds like
    /// `RiskOff` and NOT like `Routine` is the whole safety property, and a venue re-deriving it
    /// is one `_ =>` arm away from inverting it.
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
    /// [`ExecutionClient::cancel`]** — which is what every venue that does not meter its cancels
    /// wants, and is byte-identical to having no such method.
    ///
    /// The engine calls THIS, never `cancel`, so a venue overriding it sees every cancel the core
    /// issues; a venue overriding only `cancel` (all of them but Polymarket today) still receives
    /// every cancel through that method exactly as before.
    ///
    /// ⚠ **A venue that overrides this MUST keep [`ExecutionClient::cancel`] consistent with it** —
    /// implement `cancel` as `self.cancel_with_intent(coid, CancelIntent::Unspecified)` rather than
    /// leaving two independent cancel doors, or a direct `cancel` call silently skips the override.
    /// The default direction is safe either way (an unclassified cancel is never held back), but
    /// the two doors must not disagree about anything ELSE the override does.
    fn cancel_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
        let _ = intent;
        self.cancel(client_order_id);
    }
    /// Modify a resting order's qty/price in place (RUST-NATIVE HFT surface — no Python twin).
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
    /// Cancel many orders at once, saying WHY (see [`CancelIntent`]). **DEFAULT: drop the intent and
    /// call [`ExecutionClient::cancel_batch`]**, so a venue that overrode `cancel_batch` with a
    /// native batch/mass-cancel endpoint keeps using it — fanning out per-order here instead would
    /// silently un-batch those venues, which is a behavior change, not a no-op.
    ///
    /// A venue that wants the intent per id overrides THIS and fans out to
    /// [`ExecutionClient::cancel_with_intent`] itself, which is what `ExecActor` does for every
    /// venue that has no bulk lane. A venue that DECLARED one (`ExecActor::with_bulk_cancel`;
    /// Polymarket alone today) instead queues the whole batch, with this intent, as a single
    /// command — so the batch survives the seam and the venue's own planner can choose a native
    /// mass-cancel over `n` round trips. Both shapes are reached through THIS method; the trait
    /// default above is untouched, and so is every venue that inherits it.
    fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
        let _ = intent;
        self.cancel_batch(client_order_ids);
    }
    /// ACTIVELY re-confirm one order's status with the venue by client-order-id (audit ex1 residual;
    /// RUST-NATIVE). Fire-and-forget like `cancel`: the venue's authoritative terminal
    /// (`OrderAccepted`/`OrderFilled`/`OrderRejected`) returns over the ingest lane — this returns
    /// nothing. Closes the ack-ladder residual: the confirm-grace watchdog issues this so a
    /// truly-WEDGED adapter is PRODDED into answering rather than only waited on. HARD CONTRACT: the
    /// re-query MUST run OFF the core fold thread (on the adapter's own thread) — never block here.
    /// DEFAULT no-op: a client with no order-status re-query does nothing, and the watchdog's
    /// last-resort reject still backstops it. Override to run the venue's EXISTING status re-query
    /// (the one behind `resolve_ambiguous_submit`).
    fn confirm(&mut self, client_order_id: &str) {
        let _ = client_order_id;
    }
    /// symmetric shutdown half (Python `detach`); default no-op
    fn detach(&mut self) {}
    /// In-process clients (TestExecutionClient) synthesize venue events; the core loop
    /// pumps them through the bus after each command/event. Real venue clients return
    /// None — their events arrive over the ingest channel.
    fn poll_events(&mut self) -> Option<Event> {
        None
    }

    /// PAPER-MODE seam (R7): the core calls this on each closed bar of the strategy
    /// mount's series BEFORE the strategy runs — an in-process paper exchange fills its
    /// resting book here with the backtest engine's exact next-open semantics. Real
    /// venue clients ignore it.
    fn on_bar(&mut self, _bar: &vike_model::Bar) {}

    /// What THIS client's [`ExecutionClient::modify`] does to a resting order's outstanding size —
    /// the fact `ExecutionEngine::modify_order` needs before it may net a partially filled order's
    /// executed lots out of the pre-trade projection.
    ///
    /// **DEFAULT `None` = "ask the venue table"** (`vike_model::amend_semantics` on the engine's
    /// venue string), which is what every real venue adapter wants and is byte-identical to having
    /// no such method. Override ONLY when this client is not the venue it is mounted under.
    ///
    /// ⚠ **The one override in this workspace is the reason the method exists.**
    /// `vike_mount::make_engine` builds the engine with the REAL venue string even when the
    /// absent-credentials gate fell back to `vike_paper::PaperExecutionClient` (and
    /// `vike_run::build_paper_maker_core` does the same), so a paper mount on binance/okx/bybit
    /// looks up an `InPlaceTotal` row. The paper book's `modify` assigns the amend's quantity to the
    /// order's REMAINING size, so netting there would let the gate assume `q − filled` while the
    /// book executes `q`. Declaring the convention on the CLIENT means it travels with the object
    /// that implements it and no mount can forget to pass it.
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

    /// A client that DOES classify — the Polymarket shape: both doors agree because `cancel` is
    /// written in terms of `cancel_with_intent`, which is the rule the trait doc states. Records
    /// through a shared handle so the same client is observable after it has been BOXED (a trait
    /// object cannot be read back otherwise, and a test that could not read it would assert
    /// nothing).
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

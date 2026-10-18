//! The `ExecutionClient` seam: submit, cancel, modify, amend_semantics, poll_events and on_bar.

use vike_exec::ExecutionClient;
use vike_fills::fill_model::{BarFillModel, FillModel};
use vike_fills::fill_resolution::resolve_intrabar_fills;
use vike_marketdata::Bar;
use vike_model::events::{Event, OrderAccepted, OrderCanceled, OrderModified, OrderSubmitted};
use vike_model::{OrderRequest, TimeInForce, WorkingOrder, order_request_to_working};

use super::PaperExecutionClient;
use super::combo::RestingCombo;
use super::expiry::Expiry;

impl ExecutionClient for PaperExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        // HALT kill-switch, when this book was mounted with one (`with_halt_path`; `None` on every
        // simulation path short-circuits above without a syscall). While the sentinel exists, refuse
        // a submit that OPENS or ADDS risk and synthesize the terminal rejection through the normal
        // event path, so the intent never silently vanishes — the same emitter-split contract a live
        // adapter honours, and checked here BEFORE the combo/contingency arms so a halted mount
        // refuses every shape of opening order identically.
        //
        // ⚠ A REDUCING SUBMIT IS ADMITTED (`vike_exec::halt::halt_admits_submit`, shared verbatim
        // with `ExecActor` and hyperliquid): a halt must never trap an operator in a position, and
        // `OrderIntent::Flatten` mints exactly this shape, so `market-exit` still works on a halted
        // paper mount. Cancel is never gated at all — it does not pass through here.
        if self.halt_engaged() && !vike_exec::halt::halt_admits_submit(request) {
            self.events.push_back(Event::OrderRejected(vike_model::events::OrderRejected {
                client_order_id: request.client_order_id.clone(),
                reason: vike_exec::halt::HALT_REJECT_REASON.to_string().into(),
                ts: request.ts,
            }));
            return;
        }
        // A COMBO carrying contingency links is REJECTED terminally, not accepted-and-ignored:
        // the OTO-hold check lives in the single-symbol fill pass only (`fill_combos` never
        // consults `contingency.active`, so a "held" combo child would fill immediately) and the
        // expire-cascade sweeps `pending` only. Silently recording the links while enforcing none
        // of them is the one thing this book must never do — the dead-path rule says reject loud.
        // (`Submitted -> Rejected` with no `OrderAccepted`, same shape as the multi-router's
        // no-book arm.) A combo may still act as an OTO PARENT of plain children — the parent
        // itself carries no link fields — and a PLAIN order may OCO-link a combo (enforced in
        // `apply_contingency`).
        if !request.combo_legs.is_empty()
            && (request.parent_order_id.is_some()
                || !request.linked_order_ids.is_empty()
                || request.contingency_type.is_some())
        {
            self.events.push_back(Event::OrderRejected(vike_model::events::OrderRejected {
                client_order_id: request.client_order_id.clone(),
                reason: "combo orders do not support contingency links (OTO/OCO)"
                    .to_string()
                    .into(),
                ts: request.ts,
            }));
            return;
        }
        self.events.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: Some(request.client_order_id.clone().into()),
            ts: request.ts,
        }));
        // A COMBO (non-empty `combo_legs`) rests in its own book, never in `pending`: its `symbol`
        // is empty until a venue resolves the combo instrument and its `price` is a signed NET, so
        // lowering it to a `WorkingOrder` would hand the single-symbol fill primitives a limit
        // order on no instrument at a possibly-negative price. Empty legs = not a combo = every
        // pre-existing order, which takes the original path below byte-identically.
        if !request.combo_legs.is_empty() {
            self.combos.push((
                request.client_order_id.clone(),
                RestingCombo {
                    side: request.side,
                    qty: request.qty,
                    // `price` carries the SIGNED net limit verbatim (`build_combo`); a market
                    // combo carries `None`. Neither is clamped or absolute-valued here.
                    net_limit: request.price,
                    legs: request.combo_legs.clone(),
                },
            ));
        } else {
            self.pending.push((request.client_order_id.clone(), order_request_to_working(request)));
        }
        // record a TIF deadline for an EXPIRING time-in-force only — `Gtc` (the default) records
        // nothing, so the resting book behaves exactly as it did before. See `expire_due`.
        if matches!(request.time_in_force, TimeInForce::Gtd | TimeInForce::Day) {
            self.expiry.insert(
                request.client_order_id.clone(),
                Expiry {
                    tif: request.time_in_force,
                    gtd_expiry: request.gtd_expiry,
                    // anchored on the first bar, NOT on `request.ts` (which nothing stamps).
                    day_anchor: None,
                },
            );
        }
        // record OTO/OCO linkage for a bracket leg — plain orders add nothing, so the fill path
        // stays byte-identical (contingency map empty ⇒ `apply_contingency` is a no-op).
        if request.parent_order_id.is_some()
            || !request.linked_order_ids.is_empty()
            || request.contingency_type.is_some()
        {
            // a held exit (has a parent) arms on the parent's fill; an entry is active now —
            // the shared resolver's own insert law (`ContingencyBook::insert`).
            self.contingency.insert(
                request.client_order_id.clone(),
                request.parent_order_id.clone(),
                request.linked_order_ids.clone(),
            );
        }
    }

    fn cancel(&mut self, client_order_id: &str) {
        let before = self.pending.len() + self.combos.len();
        self.pending.retain(|(coid, _)| coid != client_order_id);
        // a resting combo cancels as ONE order (all legs at once) — it never half-cancels.
        self.combos.retain(|(coid, _)| coid != client_order_id);
        self.drop_marks_if_idle(); // last combo gone => its marks go with it
        self.contingency.remove(client_order_id);
        self.expiry.shift_remove(client_order_id);
        if self.pending.len() + self.combos.len() != before {
            self.events.push_back(Event::OrderCanceled(OrderCanceled {
                client_order_id: client_order_id.to_string(),
                reason: "user".to_string().into(),
                ts: 0,
            }));
        }
    }

    /// Modify a resting order in place (RUST-NATIVE; no Python twin — `sim_exchange.py` has no
    /// modify). Updates the resting `WorkingOrder`'s size/price and emits `OrderModified`; the fill
    /// on the next bar then uses the new terms. No-op if the order already filled/canceled (it is
    /// no longer in `pending`).
    ///
    /// ⚠ **`resting.size = q` MAKES THE AMEND'S QTY THE ORDER'S REMAINING SIZE**, which is neither
    /// venue convention — see [`ExecutionClient::amend_semantics`] below and
    /// `vike_venues::AmendSemantics`. Whatever executed before the amend is not deducted from `q`
    /// here and `WorkingOrder` carries no execution history at all, so `q` is exactly what the next
    /// `on_bar` will fill. This is what the declaration on this client must keep saying.
    ///
    /// ⚠ **HALT BLOCKS A MODIFY OUTRIGHT — there is no reducing exemption here, unlike
    /// [`ExecutionClient::submit`].** A modify can add size or chase price, and "does this reduce?"
    /// cannot be inferred from the request: `order.reduce_only` describes the ORIGINAL order, and
    /// `new_qty` is neither reliably a delta nor reliably a remainder (see [`Self::amend_semantics`]
    /// — this book's own answer is `InPlaceRemaining`, and the venues disagree). So the exit path
    /// under a halt is CANCEL, which is never gated, and never modify.
    ///
    /// This arm exists because the arm in [`ExecutionClient::submit`] did and this one did not, and
    /// a paper mount that refused new risk while still admitting an amend that ADDS it is not the
    /// same kill switch a live venue has. `crates/vike-bridge-core/src/exec_actor.rs`'s `modify` is
    /// the copy this mirrors.
    ///
    /// ⚠ **It mirrored it down to the SILENCE, and that is what changed.** Both returned with no
    /// event at all, so a halted amend was byte-indistinguishable from one the adapter had lost —
    /// `docs/ops/kill-switches.md`'s gap 5, open on three clients of four. All four now emit the
    /// same NON-terminal [`Event::OrderModifyRejected`] carrying
    /// [`vike_exec::halt::HALT_REJECT_REASON`]. Non-terminal is load-bearing: the FSM maps it back
    /// to MODIFIABLE (`vike_model::ManagedOrder`), so the resting order still keeps its terms and the
    /// VERDICT — a modify is refused outright, with no reducing exemption on any client — is
    /// untouched. The fix landed for all three at once for the reason this doc already gave: the
    /// whole point of the arm is that paper and live agree.
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        if self.halt_engaged() {
            self.events.push_back(Event::OrderModifyRejected(
                vike_model::events::OrderModifyRejected {
                    client_order_id: order.client_order_id.clone(),
                    reason: vike_exec::halt::HALT_REJECT_REASON.into(),
                    ts: order.ts,
                },
            ));
            return;
        }
        let coid = order.client_order_id.as_str();
        if let Some((_, resting)) = self.pending.iter_mut().find(|(c, _)| c == coid) {
            if let Some(q) = new_qty {
                resting.size = q;
            }
            if let Some(p) = new_price {
                resting.price = Some(p); // WorkingOrder.price is the resting limit/stop level
            }
            self.events.push_back(Event::OrderModified(OrderModified {
                client_order_id: coid.to_string(),
                venue_order_id: Some(coid.to_string().into()),
                new_qty,
                new_price,
                ts: 0,
            }));
        }
    }

    /// ⚠ **THE PAPER BOOK IS NOT THE VENUE IT IS MOUNTED UNDER.** `vike_tradehub_core::make_engine` builds
    /// the `ExecutionEngine` with the REAL venue string even when the mount fell back to this client
    /// (a `paper` ceiling, a bridge's `ExecOutcome::Paper`, or a venue this build does not compile),
    /// and `vike_tradehub_core::build_paper_maker_core` does the same with its profile's venue — so without this declaration a paper mount on binance/okx/bybit would have its amends
    /// judged under `vike_venues::AmendSemantics::InPlaceTotal`, i.e. the gate would assume
    /// `q − filled_qty` can still execute while [`PaperExecutionClient::modify`] above puts the whole
    /// `q` back on the book. That under-states projected exposure and under-charges margin, in the
    /// crate whose whole job is that "backtest == paper == live" is checkable.
    ///
    /// It is inert TODAY only by accident — a partially filled paper order leaves `pending` and
    /// never re-rests, so `modify` finds nothing and no amend of a partially filled order exists to
    /// mis-judge. Accidents are not gates, and this crate is the wrong place to keep one.
    fn amend_semantics(&self) -> Option<vike_venues::AmendSemantics> {
        Some(vike_venues::AmendSemantics::InPlaceRemaining)
    }

    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// The engine's `fill_pending` twin over the resting book.
    fn on_bar(&mut self, bar: &Bar) {
        // simulated time advanced to `bar.ts` — retire any resting order whose TIF deadline has
        // passed BEFORE filling (an expired order must not fill on the bar that expires it).
        // No-op unless an expiring-TIF order is resting.
        self.expire_due(bar.ts);
        // Record this bar as the mark for its symbol — ONLY while a combo is resting, so a plain
        // run neither allocates nor changes behavior. A symbol-less bar marks this book's own
        // symbol (the single-book compatibility case `MultiPaperExecutionClient::on_bar` relies on).
        if !self.combos.is_empty() {
            let sym = bar.symbol.clone().unwrap_or_else(|| self.symbol.clone());
            self.leg_marks.insert(sym, bar.clone());
        }
        let mut triggered: Vec<(String, WorkingOrder, f64)> = Vec::new();
        let mut still: Vec<(String, WorkingOrder)> = Vec::new();
        for (coid, mut order) in std::mem::take(&mut self.pending) {
            // a held OTO exit (inactive) never fills until its parent arms it — keep it resting.
            if self.contingency.is_held(&coid) {
                still.push((coid, order));
                continue;
            }
            match BarFillModel.fill_price(&mut order, bar) {
                None => still.push((coid, order)),
                Some(px) => triggered.push((coid, order, px)),
            }
        }
        self.pending = still;
        // adverse-first intrabar resolution (the engine's exact path when >1 trigger)
        if triggered.len() > 1 {
            let orders: Vec<(WorkingOrder, f64)> =
                triggered.iter().map(|(_, o, p)| (o.clone(), *p)).collect();
            let (resolved, _both) = resolve_intrabar_fills(orders, self.position);
            // resolve preserves order objects; re-pair with coids by identity of the
            // (side, kind, size) — the engine keys by object identity, we key by index:
            // resolve_intrabar_fills returns the SAME orders (possibly size-capped),
            // in adverse-first order. Re-associate by matching original index.
            let mut used = vec![false; triggered.len()];
            for (order, px) in resolved {
                let idx = triggered
                    .iter()
                    .enumerate()
                    .position(|(i, (_, o, p))| {
                        !used[i]
                            && o.side == order.side
                            && o.kind == order.kind
                            && (*p - px).abs() < 1e-12
                    })
                    .expect("resolved order maps back");
                used[idx] = true;
                let (coid, original, _) = &triggered[idx];
                if order.size <= 1e-12 {
                    // capped to ~0 by the bracket guard: the order has already left `pending`
                    // (the triggered/still partition) and never re-rests, so drop its deadline
                    // too — the map must track `pending` exactly or `expire_due` loses its
                    // empty-map fast path for the rest of the run.
                    self.expiry.shift_remove(coid);
                    continue;
                }
                let requested = original.size;
                let coid = coid.clone();
                self.emit_fill(&coid, requested, &order, px, bar.ts);
                self.apply_contingency(&coid, bar.ts);
            }
        } else if let Some((coid, order, px)) = triggered.pop() {
            if order.size > 1e-12 {
                let requested = order.size;
                let coid = coid.clone();
                self.emit_fill(&coid, requested, &order, px, bar.ts);
                self.apply_contingency(&coid, bar.ts);
            } else {
                // size-capped to ~0: gone from `pending` without a fill — drop its deadline too
                // (see the same branch above).
                self.expiry.shift_remove(&coid);
            }
        }
        // the combo book, AFTER the single-symbol pass — so the existing path is untouched (and a
        // no-op early return when no combo rests, i.e. on every pre-combo run).
        self.fill_combos(bar.ts);
    }
}

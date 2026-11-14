//! The `ExecutionClient` seam: submit, cancel, modify, amend_semantics, poll_events and on_bar.

use vike_execution::ExecutionClient;
use vike_execution::execution_engine::ADJUSTABLE_AMEND_REASON;
use vike_fills::fill_model::{
    FillModel, FillModelBar, post_only_reference, post_only_would_cross, trailing_path_fill,
};
use vike_fills::fill_resolution::resolve_intrabar_fills_path_first;
use vike_marketdata::Bar;
use vike_model::events::{
    Event, OrderAccepted, OrderCanceled, OrderModified, OrderRejected, OrderSubmitted,
    OrderTriggered, RejectCause,
};
use vike_model::{
    OrderKind, OrderRequest, OrderResting, OrderStopAdjustTo, OrderType, TimeInForce,
    trailing_activation_reached,
};

use super::combo::RestingCombo;
use super::expiry::OrderExpiry;
use super::{ExecutionClientPaper, foreign_quote_refusal, paper_contract_value};

/// The `OrderCanceled` reason of an order left nothing to fill that is not an OCO sibling: the
/// bracket cap gave the position it reduces to an adverse fill in the same bar, or a modify took
/// its size (`ExecutionClientPaper::retire_unfilled`).
const NOTHING_TO_FILL: &str = "nothing-to-fill";

/// The `OrderCanceled` reason of what is left of an order the bracket cap (`resolve_intrabar_fills`)
/// filled only in part: it left `pending` with its bar and never re-rests, so its remainder ends.
const CAPPED_TO_POSITION: &str = "capped-to-position";

impl ExecutionClient for ExecutionClientPaper {
    fn submit(&mut self, request: &OrderRequest) {
        self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            order_client_id: request.order_client_id.clone(),
            timestamp: request.timestamp,
        }));
        // HALT kill-switch, when this book was mounted with one (`with_halt_path`; `None` on every
        // simulation path short-circuits above without a syscall). While the sentinel exists, refuse
        // a submit that OPENS or ADDS risk and synthesize the terminal rejection through the normal
        // event path, so the intent never silently vanishes — the same emitter-split contract a live
        // adapter honours, and checked here BEFORE the combo/contingency arms so a halted mount
        // refuses every shape of opening order identically.
        //
        // ⚠ A REDUCING SUBMIT IS ADMITTED (`vike_execution::halt::halt_admits_submit`, shared verbatim
        // with `ExecutionClientActor` and hyperliquid): a halt must never trap an operator in a position, and
        // `OrderIntent::Flatten` mints exactly this shape, so `market-exit` still works on a halted
        // paper mount. Cancel is never gated at all — it does not pass through here.
        if self.halt_engaged() && !vike_execution::halt::halt_admits_submit(request) {
            self.events.push_back(Event::OrderRejected(vike_model::events::OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: vike_execution::halt::HALT_REJECT_REASON.to_string().into(),
                timestamp: request.timestamp,
            }));
            return;
        }
        // A symbol whose NAME says it is quoted in another fiat currency is refused in words: this
        // book is one dollar ledger with no rate source, so an accepted order would book yen or
        // pounds as dollars at par (decision 0183, items 7 and 13). Same terminal shape as the HALT
        // refusal above, so the intent never silently vanishes. A coin-margined contract whose unit
        // only its instrument row states is refused the same way: this book holds no row
        // (`paper_contract_value`). So is an option, or a combo with an option leg, whose fee this
        // book could only charge on the premium (`option_fee_refusal`).
        let symbol = if request.symbol.trim().is_empty() { &self.symbol } else { &request.symbol };
        if let Some(reason) = foreign_quote_refusal(&self.venue, symbol)
            .or_else(|| paper_contract_value(&self.venue, symbol).err())
            .or_else(|| {
                std::iter::once(symbol.as_str())
                    .chain(request.combo_legs.iter().map(|leg| leg.symbol.as_str()))
                    .find_map(|s| self.option_fee_refusal(s, request.timestamp))
            })
        {
            self.events.push_back(Event::OrderRejected(vike_model::events::OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: reason.into(),
                timestamp: request.timestamp,
            }));
            return;
        }
        // An order with no size to fill (not a finite quantity above zero) is refused in words: it
        // could only rest, then leave the book at its trigger with nothing filled.
        if !(request.qty.is_finite() && request.qty > 0.0) {
            self.events.push_back(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: format!("the quantity {} is not a finite number above zero", request.qty)
                    .into(),
                timestamp: request.timestamp,
            }));
            return;
        }
        // A TRIGGER kind (stop, stop-limit, take-profit, the three trailing kinds) without the
        // terms it fires on is refused in the shared rule's words
        // (`vike_model::order_terms_refusal`), never rested: the fill law reads its trigger (a
        // trailing kind's trail) with an `expect`, so one without would panic the book at the
        // next bar. A market-to-limit stating a limit price is refused by the same rule, as the
        // lowering would drop that price, and so is a percent trail on a kind that does not trail.
        // A trailing kind this book cannot trail is refused next (`trailing_refusal`). A PEGGED
        // order follows the venue's book, which this book does not hold, so it is refused by name,
        // never lowered to the market order its kind would fall to. Every other kind skips this at
        // one match. An order attribute and a stop's adjustment are judged by their shared rules
        // after it (`vike_model::attributes_refusal`, `vike_model::stop_adjust_refusal`): the lowering
        // drops one on a kind it does not belong to, and the adjustment law moves the stop by it.
        let kind = &request.order_type;
        let refusal = if kind.is_trigger()
            || *kind == OrderType::MarketToLimit
            || request.trail_percent.is_some()
        {
            vike_model::order_terms_refusal(
                kind,
                request.price,
                request.trigger_price,
                request.trail,
                request.limit_offset,
                request.peg.as_ref(),
                request.trail_percent,
            )
            .or_else(|| self.trailing_refusal(request))
        } else if *kind == OrderType::Pegged {
            Some(
                "a paper book holds no pegged order (the venue keeps one priced off its book); \
                 the node emulates it, so a peg never reaches this book: order refused"
                    .to_string(),
            )
        } else {
            None
        }
        .or_else(|| {
            vike_model::attributes_refusal(kind, request.time_in_force, request.attributes.as_ref())
        })
        .or_else(|| {
            vike_model::stop_adjust_refusal(
                kind,
                request.side,
                request.trigger_price,
                request.adjust.as_ref(),
            )
        });
        if let Some(reason) = refusal {
            self.events.push_back(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: reason.into(),
                timestamp: request.timestamp,
            }));
            return;
        }
        // An AUCTION lifetime takes part in the auction that opens or closes a session, and this
        // book has no session calendar to find one: refused by name, never priced on the next bar
        // (an opening auction's market-to-limit too). A combo with a release is refused too: the
        // combo book fills on fresh marks and holds nothing back, so it would ignore the release.
        // So is a release whose price condition names another symbol, or any venue (a price read on
        // another venue): this book reads its own symbol's price only, and would hold it forever.
        let foreign_condition = request
            .release
            .and_then(|r| r.price)
            .filter(|c| c.venue.is_some() || c.symbol.as_str() != self.symbol);
        let terms_refusal = if request.time_in_force.is_auction() {
            Some(format!(
                "time in force `{}` waits for a session's auction, and a paper book has no session \
                 calendar to find one: refused, nothing rests",
                request.time_in_force.as_word()
            ))
        } else if !request.combo_legs.is_empty() && request.release.is_some() {
            Some(
                "a combo order with a release: the paper combo book holds no order back, \
                 refused, nothing rests"
                    .to_string(),
            )
        } else if !request.combo_legs.is_empty()
            && let Some(e) = combo_refusal(request)
        {
            Some(format!("{e}: refused, nothing rests"))
        } else if let Some(c) = foreign_condition {
            let (read, own) = match c.venue {
                Some(v) => (format!("{} on {v}", c.symbol), " on its own venue"),
                None => (c.symbol.to_string(), ""),
            };
            Some(format!(
                "the release's price condition reads {read}, and this paper book reads only {}'s \
                 price{own}: refused, nothing rests",
                self.symbol
            ))
        } else {
            self.ouo_combo_refusal(request)
        };
        if let Some(reason) = terms_refusal {
            self.events.push_back(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: reason.into(),
                timestamp: request.timestamp,
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
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: "combo orders do not support contingency links (OTO/OCO)"
                    .to_string()
                    .into(),
                timestamp: request.timestamp,
            }));
            return;
        }
        // A POST-ONLY (maker-only) order is judged ONCE, here, against what the market offered at the
        // last bar: a crossing one is refused and nothing rests (vike-fills' law, shared with the
        // simulator), exactly the answer a venue gives. One with a release is judged at the bar that
        // releases it instead (`on_bar`), against the market it then arrives in. Every other TIF
        // skips this at one enum compare.
        if request.time_in_force == TimeInForce::PostOnly
            && let Some(rejected) = self.post_only_refusal(request)
        {
            self.events.push_back(rejected);
            return;
        }
        self.events.push_back(Event::OrderAccepted(OrderAccepted {
            order_client_id: request.order_client_id.clone(),
            venue_order_id: Some(request.order_client_id.clone().into()),
            timestamp: request.timestamp,
        }));
        // A COMBO (non-empty `combo_legs`) rests in its own book, never in `pending`: its `symbol`
        // is empty until a venue resolves the combo instrument and its `price` is a signed NET, so
        // lowering it to a `OrderResting` would hand the single-symbol fill primitives a limit
        // order on no instrument at a possibly-negative price. Empty legs = not a combo = every
        // pre-existing order, which takes the original path below byte-identically.
        if !request.combo_legs.is_empty() {
            self.combos.push((
                request.order_client_id.clone(),
                RestingCombo {
                    side: request.side,
                    qty: request.qty,
                    // `price` carries the SIGNED net limit verbatim (`build_combo`); a market
                    // combo, or one priced by its legs (limits on `legs`), carries `None`.
                    // Neither is clamped or absolute-valued here.
                    net_limit: request.price,
                    legs: request.combo_legs.clone(),
                },
            ));
        } else {
            let mut order = OrderResting::from_request(request);
            // read ONLY to tell an order-list entry from an exit inside one bar
            // (`is_order_list_entry`, the simulator's rule); it caps and refuses nothing here
            order.reduce_only = request.reduce_only;
            if kind.is_trailing() {
                // it trails from the last price, as the simulator's trails from its mark at the
                // submit (`trailing_refusal` refused one before any price); one with an activation
                // is armed by that price only once it has reached the level, else not yet (`None`)
                order.extreme = match order.activation {
                    Some(level) => self.last_close.filter(|&px| {
                        px > 0.0 && trailing_activation_reached(order.side, px, level)
                    }),
                    None => self.last_close,
                };
            }
            self.pending.push((request.order_client_id.clone(), order));
            if request.time_in_force == TimeInForce::PostOnly && request.release.is_some() {
                self.post_only_at_release.push(request.order_client_id.clone());
            }
        }
        // record a TIF deadline for an EXPIRING time-in-force only — `Gtc` (the default) records
        // nothing, so the resting book behaves exactly as it did before. See `expire_due`.
        if matches!(request.time_in_force, TimeInForce::Gtd | TimeInForce::Day) {
            self.expiry.insert(
                request.order_client_id.clone(),
                OrderExpiry {
                    tif: request.time_in_force,
                    gtd_expiry: request.gtd_expiry,
                    // anchored on the first bar, NOT on `request.ts` (which nothing stamps); a
                    // `Day` order with a release takes its day from the release, so it can never
                    // expire before it is released.
                    day_anchor: request.release.map(|r| r.at),
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
                request.order_client_id.clone(),
                request.contingency_type,
                request.parent_order_id.clone(),
                request.linked_order_ids.clone(),
            );
        }
    }

    fn cancel(&mut self, order_client_id: &str) {
        let before = self.pending.len() + self.combos.len();
        self.pending.retain(|(resting, _)| resting != order_client_id);
        // a resting combo cancels as ONE order (all legs at once) — it never half-cancels.
        self.combos.retain(|(resting, _)| resting != order_client_id);
        self.drop_marks_if_idle(); // last combo gone => its marks go with it
        self.contingency.remove(order_client_id);
        self.expiry.shift_remove(order_client_id);
        if self.pending.len() + self.combos.len() != before {
            self.events.push_back(Event::OrderCanceled(OrderCanceled {
                order_client_id: order_client_id.to_string(),
                reason: "user".to_string().into(),
                timestamp: 0,
            }));
        }
    }

    /// Modify a resting order in place (RUST-NATIVE; no Python twin — `sim_exchange.py` has no
    /// modify). Updates the resting `OrderResting`'s size and the price the FSM moves (a stop's
    /// trigger, every other kind's limit; a stop-limit's or take-profit's trigger never moves here)
    /// and emits `OrderModified`; the fill on the next bar then uses the new terms. No-op if the
    /// order already filled/canceled (it is no longer in `pending`). An untriggered trailing order
    /// has no price, so a modify naming one is refused whole (`vike_execution::execution_engine::TRAILING_AMEND_REASON`); its size
    /// alone moves. So is an adjustable stop's (`vike_execution::execution_engine::ADJUSTABLE_AMEND_REASON`),
    /// whose price its adjustment may already have made its target's.
    ///
    /// ⚠ **`resting.size = q` MAKES THE AMEND'S QTY THE ORDER'S REMAINING SIZE**, which is neither
    /// venue convention — see [`ExecutionClient::amend_semantics`] below and
    /// `vike_venues::AmendSemantics`. Whatever executed before the amend is not deducted from `q`
    /// here and `OrderResting` carries no execution history at all, so `q` is exactly what the next
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
    /// [`vike_execution::halt::HALT_REJECT_REASON`]. Non-terminal is load-bearing: the FSM maps it back
    /// to MODIFIABLE (`vike_model::OrderRecord`), so the resting order still keeps its terms and the
    /// VERDICT — a modify is refused outright, with no reducing exemption on any client — is
    /// untouched. The fix landed for all three at once for the reason this doc already gave: the
    /// whole point of the arm is that paper and live agree.
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        if self.halt_engaged() {
            self.events.push_back(Event::OrderModifyRejected(
                vike_model::events::OrderModifyRejected {
                    order_client_id: order.order_client_id.clone(),
                    reason: vike_execution::halt::HALT_REJECT_REASON.into(),
                    timestamp: order.timestamp,
                },
            ));
            return;
        }
        let order_client_id = order.order_client_id.as_str();
        if let Some((_, resting)) = self.pending.iter_mut().find(|(c, _)| c == order_client_id) {
            // a trailing order has no price to move, and what an amend of its trail or its limit
            // offset moves is not named; an adjustable stop's price may already be its target's:
            // refused whole, non-terminal, so it keeps its terms
            let refused = if order.adjust.is_some() {
                Some(ADJUSTABLE_AMEND_REASON)
            } else {
                matches!(
                    resting.kind,
                    OrderKind::Trailing | OrderKind::TrailingLimit | OrderKind::TrailingTakeProfit
                )
                .then_some(vike_execution::execution_engine::TRAILING_AMEND_REASON)
            };
            if new_price.is_some()
                && let Some(reason) = refused
            {
                self.events.push_back(Event::OrderModifyRejected(
                    vike_model::events::OrderModifyRejected {
                        order_client_id: order_client_id.to_string(),
                        reason: reason.into(),
                        timestamp: order.timestamp,
                    },
                ));
                return;
            }
            if let Some(q) = new_qty {
                resting.size = q;
            }
            if let Some(p) = new_price {
                // The price the order's FSM moves too (`vike_model::modified_price_is_trigger`): a
                // stop's trigger, every other kind's limit. An untriggered stop-limit or take-profit
                // keeps its trigger in `price` and its limit in `limit` (so a modified
                // market-if-touched gains a limit, as the FSM's request does); one released as a
                // `Limit` keeps its limit in `price`.
                match resting.kind {
                    OrderKind::StopLimit | OrderKind::TakeProfit => resting.limit = Some(p),
                    _ => resting.price = Some(p),
                }
            }
            self.events.push_back(Event::OrderModified(OrderModified {
                order_client_id: order_client_id.to_string(),
                venue_order_id: Some(order_client_id.to_string().into()),
                new_qty,
                new_price,
                new_trigger_price: None,
                timestamp: 0,
            }));
        }
    }

    /// Move a resting order's TRIGGER in place, beside its size and limit, exactly where the FSM
    /// moves them (`vike_model::modified_prices`: an untriggered stop-limit's or take-profit's
    /// trigger in `price` and its limit in `limit`; a stop's one price is its trigger, so
    /// `new_price` is not read), and say so with `OrderModified { new_trigger_price }`. An order
    /// whose trigger has fired rests as the plain `Limit` it released, and every other kind has no
    /// trigger: refused in the engine's words (`TRIGGER_AMEND_NO_TRIGGER_REASON`), nothing moved; a
    /// trailing kind in its trailing words, and an adjustable stop in `ADJUSTABLE_AMEND_REASON`.
    /// HALT blocks it as it blocks [`ExecutionClient::modify`], with the same event; an order no
    /// longer resting is a no-op, as there.
    fn modify_trigger(
        &mut self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Result<(), &'static str> {
        if self.halt_engaged() {
            self.events.push_back(Event::OrderModifyRejected(
                vike_model::events::OrderModifyRejected {
                    order_client_id: order.order_client_id.clone(),
                    reason: vike_execution::halt::HALT_REJECT_REASON.into(),
                    timestamp: order.timestamp,
                },
            ));
            return Ok(());
        }
        let order_client_id = order.order_client_id.as_str();
        let Some((_, resting)) = self.pending.iter_mut().find(|(c, _)| c == order_client_id) else {
            return Ok(());
        };
        if order.adjust.is_some() {
            return Err(ADJUSTABLE_AMEND_REASON);
        }
        match resting.kind {
            OrderKind::StopLimit | OrderKind::TakeProfit => {
                if let Some(p) = new_price {
                    resting.limit = Some(p);
                }
            }
            OrderKind::Stop => {}
            OrderKind::Trailing | OrderKind::TrailingLimit | OrderKind::TrailingTakeProfit => {
                return Err(vike_execution::execution_engine::TRAILING_AMEND_REASON);
            }
            _ => return Err(vike_execution::execution_engine::TRIGGER_AMEND_NO_TRIGGER_REASON),
        }
        resting.price = Some(new_trigger_price);
        if let Some(q) = new_qty {
            resting.size = q;
        }
        self.events.push_back(Event::OrderModified(OrderModified {
            order_client_id: order_client_id.to_string(),
            venue_order_id: Some(order_client_id.to_string().into()),
            new_qty,
            new_price,
            new_trigger_price: Some(new_trigger_price),
            timestamp: 0,
        }));
        Ok(())
    }

    /// ⚠ **THE PAPER BOOK IS NOT THE VENUE IT IS MOUNTED UNDER.** `vike_tradehub_core::make_engine` builds
    /// the `Engine` with the REAL venue string even when the mount fell back to this client
    /// (a `paper` ceiling, a bridge's `VenueAdapterExecutionOutcome::NotRunning`, or a venue this build does not compile),
    /// and `vike_tradehub_core::build_paper_strategy_core_with` does the same with its profile's venue — so without this declaration a paper mount on binance/okx/bybit would have its amends
    /// judged under `vike_venues::AmendSemantics::InPlaceTotal`, i.e. the gate would assume
    /// `q − filled_qty` can still execute while [`ExecutionClientPaper::modify`] above puts the whole
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
        self.expire_due(bar.timestamp);
        // what the market offered as this bar arrived: a held post-only order released on it is
        // judged against this, the reference its submit would have met at that moment.
        let arrival = self.last_taker;
        // what the market offers now — the one reference a post-only `submit` is judged against.
        self.last_taker = Some((post_only_reference(1, bar), post_only_reference(-1, bar)));
        // Record this bar as the mark for its symbol — ONLY while a combo is resting, so a plain
        // run neither allocates nor changes behavior. A symbol-less bar marks this book's own
        // symbol (the single-book compatibility case `ExecutionClientPaperMulti::on_bar` relies on).
        if !self.combos.is_empty() {
            let symbol = bar.symbol.clone().unwrap_or_else(|| self.symbol.clone());
            self.leg_marks.insert(symbol, bar.clone());
        }
        let mut triggered: Vec<(String, OrderResting, f64)> = Vec::new();
        let mut still: Vec<(String, OrderResting)> = Vec::new();
        // whether this bar is this book's own symbol: the one price a release's condition reads here
        // and the one a trailing order submitted after it trails from
        let own_bar = bar.symbol.as_deref().is_none_or(|s| s == self.symbol);
        if own_bar {
            self.last_close = Some(bar.close);
        }
        for (order_client_id, mut order) in std::mem::take(&mut self.pending) {
            // a held OTO exit (inactive) never fills until its parent arms it, and an order whose
            // release does not hold at this bar (`OrderResting::is_held`: a release inside the bar
            // waits for the next one; a price condition reads this bar's open, the price as the bar
            // arrives, of this book's own symbol only) takes part in nothing yet — keep both resting.
            let price_of = |s: &str| (own_bar && s == self.symbol).then_some(bar.open);
            if self.contingency.is_held(&order_client_id) || order.is_held(bar.timestamp, price_of)
            {
                still.push((order_client_id, order));
                continue;
            }
            // a post-only order with a release is judged once, now that it is released; a
            // crossing one ends here (it is already accepted, so the FSM's edge is a cancel)
            if !self.post_only_at_release.is_empty()
                && let Some(i) =
                    self.post_only_at_release.iter().position(|c| *c == order_client_id)
            {
                self.post_only_at_release.swap_remove(i);
                if let Some(px) = order.price
                    && let Some(words) = post_only_cross(arrival, order.side, px)
                {
                    let reason = format!("{words}: canceled at its release");
                    self.retire_unfilled(&order_client_id, &reason, bar.timestamp);
                    continue;
                }
            }
            let kind = order.kind;
            // a trailing order with an activation is walked along every bar's path, the
            // simulator's bar lane (`vike_fills::fill_model::trailing_path_fill`); every other
            // order, a released trailing limit included, takes the bar law
            let walked = matches!(kind, OrderKind::Trailing | OrderKind::TrailingLimit)
                && order.activation.is_some();
            let adjust = order.adjust.as_deref().copied();
            let fill = if walked {
                trailing_path_fill(&mut order, bar)
            } else {
                FillModelBar.fill_price(&mut order, bar)
            };
            match fill {
                None => {
                    // a kind change to the `Limit` is a release: a stop-limit, limit-if-touched or
                    // trailing-limit triggered short of its limit rests on as the `Limit` at L
                    // (`OrderResting::rest_as_limit`), and the book says so, as a venue does
                    if order.kind != kind && order.kind == OrderKind::Limit {
                        self.events.push_back(Event::OrderTriggered(OrderTriggered {
                            order_client_id: order_client_id.clone(),
                            timestamp: bar.timestamp,
                        }));
                    } else if let Some(adj) = adjust
                        && order.adjust.is_none()
                    {
                        // a stop's adjustment fired (`vike_model::apply_stop_adjust`): the one event
                        // that carries prices says its new trigger (a stop-limit target's limit as
                        // `new_price`), so the FSM and every order view show it; its new KIND has
                        // no event field, so the FSM keeps the kind it was sent as
                        let new_price = match adj.to {
                            OrderStopAdjustTo::StopLimit { limit, .. } => Some(limit),
                            OrderStopAdjustTo::Stop { .. } | OrderStopAdjustTo::Trailing { .. } => {
                                None
                            }
                        };
                        self.events.push_back(Event::OrderModified(OrderModified {
                            order_client_id: order_client_id.clone(),
                            venue_order_id: Some(order_client_id.clone().into()),
                            new_qty: None,
                            new_price,
                            new_trigger_price: Some(adj.to.stop()),
                            timestamp: bar.timestamp,
                        }));
                    }
                    still.push((order_client_id, order));
                }
                Some(px) => triggered.push((order_client_id, order, px)),
            }
        }
        self.pending = still;
        // a held post-only order that left the book unreleased (cancelled, expired) is judged never
        if !self.post_only_at_release.is_empty() {
            let pending = &self.pending;
            self.post_only_at_release.retain(|c| pending.iter().any(|(p, _)| p == c));
        }
        // adverse-first intrabar resolution (the engine's exact path when >1 trigger)
        if triggered.len() > 1 {
            // each clone carries its slot in `triggered` (so its order client id) in the engine-local
            // `qid` handle, which the resolver hands back untouched; the book's orders keep 0
            let orders: Vec<(OrderResting, f64)> = triggered
                .iter()
                .enumerate()
                .map(|(i, (_, o, p))| (OrderResting { qid: i as u64, ..o.clone() }, *p))
                .collect();
            // the order-list entries take the bar's path order, and the cap in it (the simulator's
            // rule); with fewer than two this is `resolve_intrabar_fills` exactly
            let entry: Vec<bool> = triggered
                .iter()
                .map(|(order_client_id, o, _)| self.is_order_list_entry(order_client_id, o))
                .collect();
            let (resolved, _both) =
                resolve_intrabar_fills_path_first(orders, self.position, &entry, bar);
            // resolve returns the SAME orders (possibly size-capped) in its own order; each is
            // re-paired with its order client id by the slot it carries, never by its terms: two orders of
            // one side, kind and price (two all-or-none exits of different sizes) would swap.
            // `(order client id, qty, shares)` of each leg filled in this pass, `shares` its OUO fill's law
            // (`ContingencyBook::ouo_shares_its_fill`) as it filled
            let mut filled: Vec<(String, f64, bool)> = Vec::new();
            for (mut order, px) in resolved {
                let (order_client_id, original, _) = &triggered[order.qid as usize];
                order.qid = original.qid;
                let Some(requested) =
                    self.resolve_in_pass(order_client_id, original.size, &filled, bar.timestamp)
                else {
                    continue;
                };
                order.size = order.size.min(requested);
                if original.all_or_none && order.size < original.size - 1e-9 {
                    // an all-or-none order the cap (`vike_fills`' rule: size 0) or an OUO sibling
                    // would cut does not fill in this pass: it rests on, whole, at what its OUO
                    // group left it (already said with an `OrderModified`)
                    let rests = OrderResting { size: requested, ..original.clone() };
                    self.pending.push((order_client_id.clone(), rests));
                    continue;
                }
                if order.size <= 1e-12 {
                    // capped to ~0 by the bracket guard: the order has already left `pending`
                    // (the triggered/still partition) and never re-rests, so it ends here.
                    self.retire_unfilled(order_client_id, NOTHING_TO_FILL, bar.timestamp);
                    continue;
                }
                let order_client_id = order_client_id.clone();
                let shares = self.contingency.ouo_shares_its_fill(&order_client_id);
                self.emit_fill(&order_client_id, requested, &order, px, bar.timestamp);
                self.apply_contingency(&order_client_id, order.size, px, bar.timestamp);
                if order.size < requested - 1e-9 {
                    // the cap filled part of it, and it has left `pending` for good: the rest
                    // ends here in words, or its FSM would stay partially filled forever
                    self.retire_unfilled(&order_client_id, CAPPED_TO_POSITION, bar.timestamp);
                }
                filled.push((order_client_id, order.size, shares));
            }
        } else if let Some((order_client_id, order, px)) = triggered.pop() {
            if order.size > 1e-12 {
                let requested = order.size;
                let order_client_id = order_client_id.clone();
                self.emit_fill(&order_client_id, requested, &order, px, bar.timestamp);
                self.apply_contingency(&order_client_id, order.size, px, bar.timestamp);
            } else {
                // ~0 size, which no cap made (one order triggered): `submit` refuses a size-less
                // order, so only a modify to (about) nothing reaches here. It ends in words too.
                self.retire_unfilled(&order_client_id, NOTHING_TO_FILL, bar.timestamp);
            }
        }
        // the combo book, AFTER the single-symbol pass — so the existing path is untouched (and a
        // no-op early return when no combo rests, i.e. on every pre-combo run).
        self.fill_combos(bar.timestamp);
    }
}

impl ExecutionClientPaper {
    /// End an order that left `pending` with nothing to fill: a terminal `OrderCanceled` in
    /// `reason`'s words, its deadline and its links dropped as `cancel` drops them, so no order
    /// leaves the book silently and its FSM ends.
    fn retire_unfilled(&mut self, order_client_id: &str, reason: &str, ts: i64) {
        self.expiry.shift_remove(order_client_id);
        self.contingency.remove(order_client_id);
        self.events.push_back(Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: reason.to_string().into(),
            timestamp: ts,
        }));
    }

    /// Whether the resting order `order_client_id` is an order-list ENTRY (a straddle's leg): a leg of the
    /// contingency book with no parent that is not reduce-only, the simulator's own test (a plain
    /// order is no exit, as a plain stop can open the opposite position). Entries triggered in one
    /// bar fill in the bar's path order and take the cap in it
    /// (`vike_fills::fill_resolution::resolve_intrabar_fills_path_first`): an ASSUMPTION, as any
    /// order inside one bar is.
    fn is_order_list_entry(&self, order_client_id: &str, o: &OrderResting) -> bool {
        !o.reduce_only
            && self.contingency.contains(order_client_id)
            && self.contingency.parent_of(order_client_id).is_none()
    }

    /// What the legs `filled` earlier in this bar's fill pass (`(order client id, qty, shares)`) leave the leg
    /// `order_client_id`, of `size`, that the pass reaches next: the size it may fill, or `None` when it ends
    /// here unfilled (retired in words). The links `apply_contingency` owed it it could not act on,
    /// the leg being out of `pending` already:
    /// - an OCO sibling of an earlier fill is cancelled (`oco`) before it can fill, however much the
    ///   cap left it; so is a target of a ladder whose stop filled (the stop sorts first and sizes
    ///   the position, so the cap is the law there and leaves the target nothing);
    /// - a leg of any other OUO group fills what is left of it, its size less every earlier
    ///   sibling's fill, said with an `OrderModified`, or is cancelled (`ouo`) when nothing is (an
    ///   all-or-none leg it cut rests on at what is left instead: `on_bar` fills it in no pass that
    ///   cuts it).
    fn resolve_in_pass(
        &mut self,
        order_client_id: &str,
        size: f64,
        filled: &[(String, f64, bool)],
        ts: i64,
    ) -> Option<f64> {
        let siblings = self.contingency.siblings_of(order_client_id);
        let earlier: Vec<&(String, f64, bool)> =
            filled.iter().filter(|(c, ..)| siblings.contains(c)).collect();
        if earlier.is_empty() {
            return Some(size);
        }
        let ouo =
            self.contingency.kind_of(order_client_id) == Some(vike_model::ContingencyType::Ouo);
        if !ouo || earlier.iter().any(|(_, _, shares)| *shares) {
            self.retire_unfilled(order_client_id, "oco", ts);
            return None;
        }
        let by: f64 = earlier.iter().map(|(_, q, _)| q).sum();
        let Some(left) = vike_execution::reduced_quantity(size, by) else {
            self.retire_unfilled(order_client_id, "ouo", ts);
            return None;
        };
        self.events.push_back(Event::OrderModified(OrderModified {
            order_client_id: order_client_id.to_string(),
            venue_order_id: Some(order_client_id.to_string().into()),
            new_qty: Some(left),
            new_price: None,
            new_trigger_price: None,
            timestamp: ts,
        }));
        Some(left)
    }

    /// Why an OUO link between a plain order and a COMBO cannot rest here, or `None`: this book never
    /// resizes a combo (`apply_contingency` reduces `pending` only, and a combo counts combo units),
    /// so the link would never be enforced. Either side is refused, whichever arrives second: an OUO
    /// order linking a resting combo, or a combo a resting OUO order links.
    fn ouo_combo_refusal(&self, request: &OrderRequest) -> Option<String> {
        let ouo = Some(vike_model::ContingencyType::Ouo);
        let order_client_id = &request.order_client_id;
        let other = if request.combo_legs.is_empty() {
            if request.contingency_type != ouo {
                return None;
            }
            let mut linked = request.linked_order_ids.iter();
            linked.find(|id| self.combos.iter().any(|(c, _)| c == *id))?.clone()
        } else {
            if self.contingency.is_empty() {
                return None;
            }
            let mut legs = self.contingency.snapshot().into_iter();
            legs.find(|leg| leg.kind == ouo && leg.linked.contains(order_client_id))?
                .order_client_id
        };
        Some(format!(
            "an OUO link between {order_client_id} and {other}, one of them a combo order: this paper book \
             never resizes a combo, so the link would never be enforced: refused, nothing rests"
        ))
    }

    /// Why this book cannot rest a trailing kind whose terms the shared rule passed, or `None` (and
    /// `None` for every other kind). One with no activation (a trailing take-profit never has one)
    /// trails from the close of the last bar this book saw, by a price or a percent of it, as the
    /// simulator trails from its mark at the submit, so before the first bar there is nothing to
    /// trail from; one with an activation waits, unactivated, for the price.
    fn trailing_refusal(&self, request: &OrderRequest) -> Option<String> {
        let kind = &request.order_type;
        if !kind.is_trailing() || request.trigger_price.is_some() {
            return None;
        }
        self.last_close.is_none().then(|| {
            format!(
                "a {kind} order trails from the last price, and this paper book has seen no price \
                 yet: refused, nothing rests"
            )
        })
    }

    /// The terminal `OrderRejected` a POST-ONLY submit earns, or `None` when it may rest.
    ///
    /// Two refusals: an order that is not a LIMIT with a price (no cause: a malformed request, the
    /// same words every venue adapter gives), and a limit order that would TAKE liquidity against
    /// what the last bar offered (`RejectCause::PostOnlyWouldCross`, nothing rests and nothing
    /// fills). Before the first bar there is no price to judge against, so the order rests, as it
    /// would on a venue whose book this book has not been shown yet. An order with a release is
    /// not judged here at all: `on_bar` judges it at the bar that releases it.
    fn post_only_refusal(&self, request: &OrderRequest) -> Option<Event> {
        let reject = |cause: Option<RejectCause>, reason: String| {
            Some(Event::OrderRejected(OrderRejected {
                cause,
                order_client_id: request.order_client_id.clone(),
                reason: reason.into(),
                timestamp: request.timestamp,
            }))
        };
        let (OrderType::Limit, Some(limit_px)) = (&request.order_type, request.price) else {
            return reject(
                None,
                "post-only needs a limit order with a price (a market order would take \
                 liquidity)"
                    .to_string(),
            );
        };
        // one with a release meets the market at its release, not now (`on_bar`)
        if request.release.is_some() {
            return None;
        }
        let words = post_only_cross(self.last_taker, request.side, limit_px)?;
        reject(Some(RejectCause::PostOnlyWouldCross), format!("{words}: refused, nothing rests"))
    }
}

/// Whether a post-only `side` order at `limit_px` would TAKE liquidity against `reference` (what
/// the last bar offered: `(for a buy, for a sell)`), in words; `None` when it may rest, or when
/// there is no reference yet (before the first bar).
fn post_only_cross(reference: Option<(f64, f64)>, side: i32, limit_px: f64) -> Option<String> {
    let (buy_ref, sell_ref) = reference?;
    let (word, taker_price, taken) =
        if side > 0 { ("buy", buy_ref, "ask") } else { ("sell", sell_ref, "bid") };
    post_only_would_cross(side, limit_px, taker_price).then(|| {
        format!(
            "post-only {word} at {limit_px} would take liquidity (the {taken} is {taker_price})"
        )
    })
}

/// A combo request (a caller may build one by hand) judged by the model's ONE combo rule
/// (`OrderCombo::validate`: a leg-limit mix this book would read half of, one instrument in two legs)
/// and then by the books' lifetime rule (`OrderCombo::book_lifetime_refusal`: this book rests a GTC
/// combo only), both shared with the simulator; `None` when it may rest.
fn combo_refusal(request: &OrderRequest) -> Option<vike_model::ComboError> {
    let spec = vike_model::OrderCombo {
        venue: request.venue.clone(),
        side: request.side,
        qty: request.qty,
        legs: request.combo_legs.clone(),
        net_limit: request.price,
        time_in_force: request.time_in_force,
        account: None,
        order_client_id: None,
    };
    spec.validate().err().or_else(|| spec.book_lifetime_refusal())
}

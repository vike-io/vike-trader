//! `apply_intent_routed` — every `OrderIntent` arm lowered onto its engine. This file holds the
//! dispatcher and the small arms (`Cancel`, `CancelBatch`, `Modify`, `Confirm`, `Combo`,
//! `Exercise`); each large arm is a method in a sibling file — `submit.rs` (`lower_submit`,
//! `lower_submit_batch`, `lower_bracket`), `order_list.rs` (`lower_order_list`), `exit.rs`
//! (`lower_mass_cancel`, `lower_flatten`, `lower_market_exit`) and
//! `conditional.rs` (`arm_conditional`, `disarm_conditional`). Every recursion — SubmitBatch to
//! Submit, Flatten to Submit, MarketExit to MassCancel and Flatten — re-enters through this
//! dispatcher.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// [`Self::apply_intent`], classifying every cancel it lowers (see [`OrderCancelIntent`]) and naming
    /// WHICH ENGINE the intent is lowered onto ([`EngineRoute`]).
    ///
    /// The classification rides ALONGSIDE the [`OrderIntent`] rather than inside it, deliberately:
    /// `OrderIntent` is JOURNALED (`Ingest::Command`) and crosses the tradehub wire, so widening
    /// `Cancel`/`CancelBatch`/`MassCancel` would change a persisted shape for a fact replay does
    /// not need — the venue's authoritative `OrderCanceled` is journaled as its own event and the
    /// cancel itself publishes nothing local, so a replay that re-derived the intent would change
    /// no `state_hash`. It is a LIVE routing hint, and it is stored nowhere.
    ///
    /// ⚠ **`route` rides alongside for the same reason and one more — and the trap it avoids is the
    /// COMPOSITE STRING, not the payload.** A STRATEGY's account is not a property of the ORDER:
    /// putting it there would mean an `OrderRequest::venue` carrying `"binance#ALT"`, which is the
    /// trap `vike_execution::route_key`'s module doc names — it would route correctly and then be read
    /// as an EXCHANGE by `vike_venues::capabilities_for`, `preflight_order_at` and every adapter that
    /// forwards the request, whereupon `preflight_order_at`'s unknown-venue affordance answers
    /// `Ok(())` and every capability check is skipped in silence. That argument is unchanged, and
    /// it is why `venue` is still the canonical exchange id at this seam and nowhere decorated.
    ///
    /// **What it does not argue against is a TYPED SIBLING field**, which sidesteps the trap
    /// rather than walking into it. `vike_model::OrderRequest`'s `account` is an
    /// `Option<AccountLabel>` BESIDE `venue`, never inside it — the same split
    /// `vike_execution::Engine` already makes between `venue` and `route_key` — so no
    /// downstream reader can mistake it for an exchange: every capability lookup goes on reading
    /// `venue` and resolves the row it always resolved. This clause used to end *"a misroute
    /// cannot be written here because there is no string to write it in"*, and the second half of
    /// that is now false; the first half survives, and survives BECAUSE the field is typed.
    ///
    /// **So what routes an operator's order is the payload, and what routes a strategy's is still
    /// the caller.** The `Submit` arm resolves `req.account` to one engine before
    /// `CoreThread::ambiguous_accounts` is asked anything (see that resolution's own comment, and
    /// `CoreThread::route_for_payload_account`). **The strategy lane is untouched by
    /// construction**, twice over: it lowers through [`EngineRoute::Mount`], whose engine was
    /// resolved at assemble through `CoreThread::mount_engine`, and a route that already names a
    /// book on the payload's own exchange keeps it — and a strategy-minted request carries no
    /// account to consult in the first place.
    pub(crate) fn apply_intent_routed(
        &mut self,
        intent: OrderIntent,
        now: i64,
        cancel_intent: OrderCancelIntent,
        route: EngineRoute,
    ) -> Vec<String> {
        match intent {
            OrderIntent::Submit(req) => self.lower_submit(*req, now, route),
            OrderIntent::SubmitBatch(reqs) => {
                self.lower_submit_batch(reqs, now, cancel_intent, route)
            }
            OrderIntent::Cancel(order_client_id) => {
                // A HELD bracket exit is not at the venue and has no registered order, so a plain
                // `cancel_order` would silently no-op. Intercept it: drop it from `held_orders` + the
                // book (`cancel_held`). Otherwise cancel the live order as before (its authoritative
                // `OrderCanceled` drives the book via `drive_contingency_on_terminal`).
                // An operator's CORE-HELD stop is cancelled by its arm id the same way (`cancel_armed`),
                // and an order held for its release with no venue call (`cancel_held_release`).
                if !self.cancel_held(&order_client_id)
                    && !self.cancel_held_release(&order_client_id)
                    && !self.cancel_armed(&order_client_id, now)
                    && !self.cancel_emulated(&order_client_id)
                {
                    let engine_index =
                        self.engine_of_order_client_id(&order_client_id).unwrap_or(0);
                    // The operator's order client id names the LOGICAL order: after a cancel-then-new amend it
                    // reaches the live replacement, and an amend still in flight must not place
                    // anything after this (`amend.rs`). Both are one `is_empty` read on a cancel.
                    if !self.amends.is_empty() {
                        self.withdraw_amends(Some(&order_client_id));
                    }
                    let order_client_id =
                        self.amend_head(engine_index, &order_client_id).unwrap_or(order_client_id);
                    self.engine_mut(engine_index)
                        .cancel_order_with_intent(&order_client_id, cancel_intent);
                }
                self.pump_client();
                Vec::new()
            }
            OrderIntent::CancelBatch(order_client_ids) => {
                // Split HELD exits (canceled directly off the book) from live order client ids (batched to the
                // venue), so a batch that mixes both cancels every one instead of dropping the held
                // ones on the floor.
                let order_client_ids: Vec<String> = order_client_ids
                    .into_iter()
                    .filter(|c| {
                        !self.cancel_held(c)
                            && !self.cancel_held_release(c)
                            && !self.cancel_armed(c, now)
                            && !self.cancel_emulated(c)
                    })
                    .collect();
                if self.extra_engines.is_empty() {
                    self.engine.cancel_order_batch_with_intent(&order_client_ids, cancel_intent);
                } else {
                    let mut by_index: Vec<Vec<String>> =
                        vec![Vec::new(); self.extra_engines.len() + 1];
                    for order_client_id in order_client_ids {
                        let i = self.engine_of_order_client_id(&order_client_id).unwrap_or(0);
                        by_index[i].push(order_client_id);
                    }
                    for (i, group) in by_index.into_iter().enumerate() {
                        if group.is_empty() {
                            continue;
                        }
                        if i == 0 {
                            self.engine.cancel_order_batch_with_intent(&group, cancel_intent);
                        } else {
                            self.extra_engines[i - 1]
                                .1
                                .cancel_order_batch_with_intent(&group, cancel_intent);
                        }
                    }
                }
                self.pump_client();
                Vec::new()
            }
            OrderIntent::Modify { order_client_id, new_qty, new_price, new_trigger_price } => {
                // An EMULATED parent is never amended (decision 0275): cancel it and send it again.
                if self.refuse_emulated_modify(&order_client_id) {
                    self.pump_client();
                    return Vec::new();
                }
                // A HELD exit is modified IN PLACE on its stored request (it is not at the venue yet),
                // so the release later submits the updated terms. A live order takes the venue path.
                //
                // ⚠ The in-place edit is deliberately NOT risk-gated here, and that is not the
                // modify-bypass hole its shape resembles: a held exit carries NO exposure (it has
                // never reached a venue), and the terms written here are judged in full by the
                // RiskGate at RELEASE time — `drive_contingency_on_fill` releases through
                // `submit_resolved`, which calls `submit_order`, which gates. Gating the edit too
                // would judge an order that does not exist yet against a context that will have
                // moved by the time it does. The LIVE branch below is the one that had no gate at
                // all until the modify-bypass fix; see `Engine::modify_order`.
                if let Some(held) = self.held_orders.get_mut(&order_client_id) {
                    if let Some(q) = new_qty {
                        held.qty = q;
                    }
                    // The price an amend names is the TRIGGER of a stop and the limit price of every
                    // other kind, and a stated trigger price moves a trigger kind's trigger
                    // (`vike_model::modified_prices`), exactly as the venue's `OrderModified` is
                    // folded: a held stop used to take it as a limit price and was released at the
                    // trigger it was placed with.
                    let (price, trigger) =
                        vike_model::modified_prices(&held.order_type, new_price, new_trigger_price);
                    if let Some(p) = price {
                        held.price = Some(p);
                    }
                    if let Some(t) = trigger {
                        held.trigger_price = Some(t);
                    }
                    self.note(format!("held bracket exit {order_client_id} modified in place"));
                } else if !self.modify_held_release(
                    &order_client_id,
                    new_qty,
                    new_price,
                    new_trigger_price,
                ) {
                    // The venue path: native, or cancel-then-new where the venue cannot change this
                    // kind of resting order in place (`amend.rs`). The RiskGate judges either.
                    self.modify_resting(
                        order_client_id,
                        new_qty,
                        new_price,
                        new_trigger_price,
                        now,
                    );
                }
                self.pump_client();
                Vec::new()
            }
            OrderIntent::Confirm(order_client_id) => {
                let engine_index = self.engine_of_order_client_id(&order_client_id).unwrap_or(0);
                self.engine_mut(engine_index).confirm_order(&order_client_id);
                self.pump_client();
                Vec::new()
            }
            OrderIntent::MassCancel { venue, symbol, account } => {
                self.lower_mass_cancel(venue, symbol, account, cancel_intent, route)
            }
            OrderIntent::Flatten { venue, symbol, account } => {
                self.lower_flatten(venue, symbol, account, now, cancel_intent, route)
            }
            OrderIntent::MarketExit { venue, account } => {
                self.lower_market_exit(venue, account, now, cancel_intent, route)
            }
            OrderIntent::Bracket(spec) => self.lower_bracket(*spec, now, route),
            OrderIntent::ArmConditional(c) => self.arm_conditional(c, now, route),
            OrderIntent::DisarmConditional { arm_id } => self.disarm_conditional(arm_id, now),
            // The combo lowering (PR-2). The venue-capability decision is resolved HERE, at the
            // edge, off the static registry; `lower_combo` is then a pure function of it. That
            // split is deliberate — see the doc on `lower_combo`.
            OrderIntent::Combo(spec) => {
                // Caps row from the ROUTED engine, the twin of the Submit/Bracket arms and for the
                // same reason (`Self::capabilities_venue`): `spec.venue` is what `lower_combo` will route
                // the lowered legs by, and a routing key is not what `supports_combo` is a fact
                // about. Byte-identical today; an UNROUTED spec keeps its own string, so the
                // fail-closed `UNSUPPORTED` fallback still applies to it exactly as before.
                let supported = {
                    let routed = self.route_of(route, &spec.venue);
                    vike_venues::capabilities_for(self.capabilities_venue(routed, &spec.venue))
                        .supports_combo
                };
                self.lower_combo(*spec, now, supported, route)
            }
            OrderIntent::OrderList(spec) => self.lower_order_list(*spec, now, route),
            // An option exercise is an instruction, not an order: nothing is minted or sent. Its one
            // account resolves first (`lower_bracket`'s account step: an unheld one is refused by
            // name), a venue of several accounts naming none is refused as ambiguous, and then every
            // venue refuses it in its own words (`vike_venues::venue_caps::exercise_refusal`, keyed by
            // the routed engine's exchange id), the spec's own words first.
            OrderIntent::Exercise(spec) => {
                let route = match spec.account.as_ref() {
                    None => route,
                    Some(account) => {
                        match self.route_for_payload_account(route, &spec.venue, account) {
                            Some(resolved) => resolved,
                            None => {
                                self.refuse_unheld_account("exercise", &spec.venue, account);
                                self.pump_client();
                                return Vec::new();
                            }
                        }
                    }
                };
                if let Some(candidates) = self.ambiguous_accounts(route, &spec.venue) {
                    self.refuse_ambiguous("exercise", &spec.venue, &candidates);
                } else {
                    let venue =
                        self.capabilities_venue(self.route_of(route, &spec.venue), &spec.venue);
                    let why = spec.validate().err().unwrap_or_else(|| {
                        vike_venues::venue_caps::exercise_refusal(venue).unwrap_or_else(|| {
                            format!(
                                "{venue} is no roster venue, and nothing exercises an option there"
                            )
                        })
                    });
                    let note = format!(
                        "exercise of {} {} on {venue} REFUSED, nothing was sent: {why}",
                        spec.qty, spec.symbol
                    );
                    self.note(note);
                }
                self.pump_client();
                Vec::new()
            }
        }
    }
}

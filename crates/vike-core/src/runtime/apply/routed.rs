//! `apply_intent_routed` — every `OrderIntent` arm lowered onto its engine. This file holds the
//! dispatcher and the small arms (`Cancel`, `CancelBatch`, `Modify`, `Confirm`, `Combo`); each large
//! arm is a method in a sibling file — `submit.rs` (`lower_submit`, `lower_submit_batch`,
//! `lower_bracket`), `exit.rs` (`lower_mass_cancel`, `lower_flatten`, `lower_market_exit`) and
//! `conditional.rs` (`arm_conditional`, `disarm_conditional`). Every recursion — SubmitBatch to
//! Submit, Flatten to Submit, MarketExit to MassCancel and Flatten — re-enters through this
//! dispatcher.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// [`Self::apply_intent`], classifying every cancel it lowers (see [`CancelIntent`]) and naming
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
    /// trap `vike_exec::route_key`'s module doc names — it would route correctly and then be read
    /// as an EXCHANGE by `vike_model::caps_for`, `preflight_order_at` and every adapter that
    /// forwards the request, whereupon `preflight_order_at`'s unknown-venue affordance answers
    /// `Ok(())` and every capability check is skipped in silence. That argument is unchanged, and
    /// it is why `venue` is still the canonical exchange id at this seam and nowhere decorated.
    ///
    /// **What it does not argue against is a TYPED SIBLING field**, which sidesteps the trap
    /// rather than walking into it. `vike_model::OrderRequest`'s `account` is an
    /// `Option<AccountLabel>` BESIDE `venue`, never inside it — the same split
    /// `vike_exec::ExecutionEngine` already makes between `venue` and `route_key` — so no
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
        cancel_intent: CancelIntent,
        route: EngineRoute,
    ) -> Vec<String> {
        match intent {
            OrderIntent::Submit(req) => self.lower_submit(*req, now, route),
            OrderIntent::SubmitBatch(reqs) => {
                self.lower_submit_batch(reqs, now, cancel_intent, route)
            }
            OrderIntent::Cancel(coid) => {
                // A HELD bracket exit is not at the venue and has no registered order, so a plain
                // `cancel_order` would silently no-op. Intercept it: drop it from `held_orders` + the
                // book (`cancel_held`). Otherwise cancel the live order as before (its authoritative
                // `OrderCanceled` drives the book via `drive_contingency_on_terminal`).
                if !self.cancel_held(&coid) {
                    let eidx = self.coid_venue.get(&coid).copied().unwrap_or(0);
                    self.eng_mut(eidx).cancel_order_with_intent(&coid, cancel_intent);
                }
                self.pump_client();
                Vec::new()
            }
            OrderIntent::CancelBatch(coids) => {
                // Split HELD exits (canceled directly off the book) from live coids (batched to the
                // venue), so a batch that mixes both cancels every one instead of dropping the held
                // ones on the floor.
                let coids: Vec<String> =
                    coids.into_iter().filter(|c| !self.cancel_held(c)).collect();
                if self.extra_engines.is_empty() {
                    self.engine.cancel_order_batch_with_intent(&coids, cancel_intent);
                } else {
                    let mut by_idx: Vec<Vec<String>> =
                        vec![Vec::new(); self.extra_engines.len() + 1];
                    for coid in coids {
                        let i = self.coid_venue.get(&coid).copied().unwrap_or(0);
                        by_idx[i].push(coid);
                    }
                    for (i, group) in by_idx.into_iter().enumerate() {
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
            OrderIntent::Modify { client_order_id, new_qty, new_price } => {
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
                // all until the modify-bypass fix; see `ExecutionEngine::modify_order`.
                if let Some(held) = self.held_orders.get_mut(&client_order_id) {
                    if let Some(q) = new_qty {
                        held.qty = q;
                    }
                    if let Some(p) = new_price {
                        held.price = Some(p);
                    }
                    self.note(format!("held bracket exit {client_order_id} modified in place"));
                } else {
                    let eidx = self.coid_venue.get(&client_order_id).copied().unwrap_or(0);
                    self.eng_mut(eidx).now_ms = now;
                    let mut outbox = Outbox::default();
                    self.eng_mut(eidx).modify_order(
                        &client_order_id,
                        new_qty,
                        new_price,
                        now,
                        &mut outbox,
                    );
                    // A RiskGate veto publishes a NON-TERMINAL `OrderModifyRejected` (the order keeps
                    // its terms), so this drives the same outbox path every other intent does — the
                    // rejection reaches the event stream, the recent-events ring and any mounted
                    // strategy instead of being dropped on the floor.
                    self.publish_and_drive_outbox(eidx, outbox, now);
                }
                self.pump_client();
                Vec::new()
            }
            OrderIntent::Confirm(coid) => {
                let eidx = self.coid_venue.get(&coid).copied().unwrap_or(0);
                self.eng_mut(eidx).confirm_order(&coid);
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
                // same reason (`Self::caps_venue`): `spec.venue` is what `lower_combo` will route
                // the lowered legs by, and a routing key is not what `supports_combo` is a fact
                // about. Byte-identical today; an UNROUTED spec keeps its own string, so the
                // fail-closed `UNSUPPORTED` fallback still applies to it exactly as before.
                let supported = {
                    let routed = self.route_of(route, &spec.venue);
                    vike_model::caps_for(self.caps_venue(routed, &spec.venue)).supports_combo
                };
                self.lower_combo(*spec, now, supported, route)
            }
        }
    }
}

//! `apply_intent` — THE order-write lowering site: every OrderIntent from every origin (external
//! Command path, LiveBroker strategy drain, conditional FIRE, margin-call liquidation) is meant to
//! flow through here — mint → route → RiskGate (inside `submit_order`) → client — so order
//! submission has one gated, mint-consistent path. (A source-scan test in the control-boundary
//! tests enforces that no engine submit call lives outside this file.) `use super::*` re-exports
//! the parent fold module's imports.

use super::*;

/// Does this request's reserved contingency slots declare it a member of an OTO/OCO group? Plain
/// orders (the byte-identical path) answer `false` and are never entered into the contingency book.
/// The `build_bracket` lowering stamps `contingency_type = "OTO"` on the entry and
/// `parent_order_id`/`linked_order_ids` on the exits — any of the three marks a leg.
fn has_contingency_links(req: &OrderRequest) -> bool {
    req.parent_order_id.is_some()
        || !req.linked_order_ids.is_empty()
        || req.contingency_type.is_some()
}

/// Strip the OTO/OCO linkage from a request about to go to the VENUE. The core emulates OTO/OCO in
/// its own [`vike_exec::ContingencyBook`], so the venue — or an OCO-enforcing test client like the
/// paper exchange — must see a PLAIN order. Left intact, a client that also holds OTO children would
/// RE-hold a just-released exit (whose parent already filled and can never re-arm it there),
/// dead-locking the fill. No mounted venue enforces OCO/OTO natively today; a future native-OCO
/// venue would pass the linkage through here instead of stripping it.
fn strip_contingency_links(mut req: OrderRequest) -> OrderRequest {
    req.parent_order_id = None;
    req.linked_order_ids.clear();
    req.contingency_type = None;
    req.order_list_id = None;
    req
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The ONE [`OrderIntent::MassCancel`] leg of a [`OrderIntent::MarketExit`], scoped exactly as
    /// the exit is (`None` ⇒ every engine + every conditional book; `Some(v)` ⇒ that engine + that
    /// venue's books).
    pub(crate) fn market_exit_mass_cancel(venue: Option<&str>) -> OrderIntent {
        OrderIntent::MassCancel { venue: venue.map(|v| v.to_string()), symbol: None }
    }

    /// The FLATTEN legs of a [`OrderIntent::MarketExit`]: ONE [`OrderIntent::Flatten`] per non-flat
    /// position, walked engine by engine (primary first, then extras in registration order) and,
    /// within an engine, in the `Account` `positions` IndexMap's own insertion order. `Flatten`
    /// itself re-resolves the size and submits a `reduce_only` MARKET for `|position|` at apply
    /// time.
    ///
    /// This function READS ONLY (`&self`) — it mints nothing, sends nothing, and touches no
    /// engine. That is what makes the compound verb replay-deterministic without a journal record
    /// of its own: a replay that folded the same record prefix holds the same positions, so this
    /// returns the same intent list in the same order, so the same coids are minted downstream.
    ///
    /// **Called AFTER the mass-cancel has been applied**, never before — see the
    /// [`OrderIntent::MarketExit`] arm of [`Self::apply_intent`] for why (the mass-cancel's own
    /// `pump_client` can fold a fill that OPENS a position on a symbol that was flat a moment
    /// earlier; a list snapshotted before the cancel would have no leg for it and the exit would
    /// leave that position on).
    ///
    /// SCOPE NOTE (hedge mode): only `position_side == "BOTH"` legs are expanded, because
    /// `ExecutionEngine::position_size_of` — the size `Flatten` resolves through — is keyed
    /// `(venue, symbol, "BOTH")`. A hedge-mode LONG/SHORT leg would expand into a `Flatten` that
    /// resolves 0.0 and no-ops, so it is skipped here rather than emitted as a dead intent. Those
    /// venues need per-leg closing intents, which the primitive vocabulary does not carry yet.
    pub(crate) fn market_exit_flatten_legs(&self, venue: Option<&str>) -> Vec<OrderIntent> {
        let mut out = Vec::new();
        for idx in 0..=self.extra_engines.len() {
            let eng_venue = self.eng(idx).venue.clone();
            if let Some(v) = venue {
                if eng_venue != v {
                    continue;
                }
            }
            for ((pv, symbol, side), pos) in self.eng(idx).account.positions.iter() {
                if *side != vike_model::events::PositionSide::Both
                    || pv.as_str() != eng_venue
                    || pos.size == 0.0
                {
                    continue;
                }
                out.push(OrderIntent::Flatten {
                    venue: pv.to_string(),
                    symbol: symbol.to_string(),
                });
            }
        }
        out
    }

    /// PURE expansion of the compound [`OrderIntent::MarketExit`] ("get me out") into the existing
    /// primitive intents, in deterministic order: the mass-cancel leg
    /// ([`Self::market_exit_mass_cancel`]) followed by the flatten legs
    /// ([`Self::market_exit_flatten_legs`]) *as of right now*.
    ///
    /// INSPECTION/TEST HELPER ONLY. The live arm does NOT apply this list wholesale — it applies
    /// the mass-cancel, then RE-derives the flatten legs from the post-cancel state (see the
    /// `MarketExit` arm). Snapshotting the whole plan up front would miss a position opened by a
    /// fill the mass-cancel's own pump folded.
    #[cfg(test)]
    pub(crate) fn expand_market_exit(&self, venue: Option<&str>) -> Vec<OrderIntent> {
        let mut out = vec![Self::market_exit_mass_cancel(venue)];
        out.extend(self.market_exit_flatten_legs(venue));
        out
    }

    /// Lower one [`OrderIntent`] onto the engine apply surface. `now` stamps the target engine's
    /// clock. Returns coids of orders SUBMITTED by this intent (see module + call sites).
    ///
    /// Says nothing about WHY any cancel this intent lowers is being issued, so those cancels reach
    /// the venue as [`CancelIntent::Unspecified`] — the flatten-safe value, which no venue may hold
    /// back. That is the right answer for the EXTERNAL command path (a DOM click, a tradehub
    /// ticket, a CLI verb): the operator's intent is genuinely unknown here, and guessing "routine"
    /// on their behalf could shed a cancel they meant as an exit. A caller that DOES know uses
    /// [`Self::apply_intent_with_cancel_intent`].
    pub(crate) fn apply_intent(&mut self, intent: OrderIntent, now: i64) -> Vec<String> {
        self.apply_intent_with_cancel_intent(intent, now, CancelIntent::Unspecified)
    }

    /// [`Self::apply_intent_with_cancel_intent`] routed by the PAYLOAD's venue — the external
    /// command path, and the only routing an operator ticket, a DOM click or a CLI verb can carry.
    pub(crate) fn apply_intent_with_cancel_intent(
        &mut self,
        intent: OrderIntent,
        now: i64,
        cancel_intent: CancelIntent,
    ) -> Vec<String> {
        self.apply_intent_routed(intent, now, cancel_intent, EngineRoute::Payload)
    }

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
    /// ⚠ **`route` rides alongside for the same reason and one more.** A strategy's account is not
    /// a property of the ORDER — putting it on the payload would mean an `OrderRequest::venue`
    /// carrying `"binance#ALT"`, which is the trap `vike_exec::route_key`'s module doc names: it
    /// would route correctly and then be read as an EXCHANGE by `vike_model::caps_for`,
    /// `preflight_order_at` and every adapter that forwards the request. So the account stays a
    /// property of the CALLER, resolved through `CoreThread::mount_engine`, and a misroute cannot
    /// be written here because there is no string to write it in.
    pub(crate) fn apply_intent_routed(
        &mut self,
        intent: OrderIntent,
        now: i64,
        cancel_intent: CancelIntent,
        route: EngineRoute,
    ) -> Vec<String> {
        match intent {
            OrderIntent::Submit(req) => {
                let mut req = *req;
                let minted = req.client_order_id.is_empty();
                if minted {
                    req.client_order_id = self.coid_gen.generate();
                }
                // Minted-coid `exec_order` gap: the write-ahead `Cmd` that carried this submit had
                // an EMPTY coid (the mint just happened, AFTER that record), so journal the now-
                // RESOLVED request so the materializer can tie the minted coid to its terms even for
                // an order that terminalizes without ever filling. Only when we actually minted (an
                // explicit-coid submit is already fully resolved in its write-ahead `Cmd`). Cold-ish
                // relative to the p99 event fold — submits are orders, not market data — and gated on
                // journaling being on, so the desktop/no-journal path is byte-identical.
                if minted {
                    if let Some(journal) = self.journal.as_mut() {
                        journal.append_minted_submit(now, &req).expect("journal append");
                        self.journaled_since_snap += 1;
                    }
                }
                let coid = req.client_order_id.clone();
                let routed = self.route_of(route, &req.venue);
                let eidx = routed.unwrap_or(0);
                if eidx != 0 {
                    self.coid_venue.insert(coid.clone(), eidx);
                }
                self.eng_mut(eidx).now_ms = now;
                // Capability preflight (w2-task-5), the sibling of the Combo arm's supports_combo
                // gate: an order the venue's declared `VenueCaps` row cannot honor is refused HERE
                // — a synthesized terminal lifecycle with a machine-readable reason — instead of
                // reaching the adapter to be silently coerced (a "stop" firing as an immediate
                // market, a non-GTC TIF resting GTC). AFTER the journal write on purpose (a
                // refused order is exactly the audit trail worth keeping — the combo-arm
                // doctrine); the coid is still returned, matching this arm's RiskGate-denial
                // shape (the refusal events NAME the order).
                //
                // The caps row comes from the ROUTED ENGINE (`Self::caps_venue`), not from
                // `req.venue` — the request's venue string is what routed it, and routing is a
                // per-ACCOUNT question while a caps row is a per-EXCHANGE fact. Byte-identical
                // today (a routed request's venue IS the engine's), and the reason it is spelled
                // this way is in `caps_venue`'s own doc: reading the routing string would let a
                // per-account key silently skip every check below rather than fail closed.
                // Scoped so the `&self` borrow ends before the `&mut self` refusal path; no
                // allocation on the accepted path (the `Result` is `Ok(())`).
                let deny =
                    vike_model::preflight_order_at(&req, self.caps_venue(routed, &req.venue)).err();
                if let Some(deny) = deny {
                    self.synthesize_capability_reject(eidx, &req, &deny.to_string(), now);
                    self.pump_client();
                    return vec![coid];
                }
                // Live-runtime OTO/OCO submit-hold: a request that carries contingency linkage is
                // recorded in the shared book (byte-identical: plain orders are never inserted). If
                // it is a HELD exit (has a parent that has not filled), it must NOT go live to the
                // venue yet — store its resolved request and return; its parent's fill releases it
                // (`drive_contingency_on_fill`). An ENTRY / link-free leg (not held) falls through to
                // the normal submit below. `coid_venue` is already recorded above, so a later
                // release/cancel routes to the right engine.
                if has_contingency_links(&req) {
                    self.contingency.insert(
                        coid.clone(),
                        req.parent_order_id.clone(),
                        req.linked_order_ids.clone(),
                    );
                    if self.contingency.is_held(&coid) {
                        self.note(format!("OTO child {coid} held pending parent fill"));
                        self.held_orders.insert(coid.clone(), req);
                        return vec![coid];
                    }
                    // A non-held leg (an OTO entry) still goes to the venue — but as a PLAIN order:
                    // the core owns the OTO/OCO emulation in its book, so the venue must not
                    // re-interpret the linkage (an OCO-enforcing client would otherwise re-hold or
                    // double-manage it). See `strip_contingency_links`.
                    req = strip_contingency_links(req);
                }
                let mut outbox = Outbox::default();
                self.eng_mut(eidx).submit_order(&req, now, &mut outbox);
                // Drive the outbox contingency-aware: a linked ENTRY vetoed HERE by the RiskGate
                // (`OrderDenied`) must cascade-drop its already-held children, not orphan them.
                self.publish_and_drive_outbox(eidx, outbox, now);
                self.pump_client();
                vec![coid]
            }
            OrderIntent::SubmitBatch(mut reqs) => {
                let all_primary =
                    reqs.iter().all(|r| self.route_of(route, &r.venue).unwrap_or(0) == 0);
                if !all_primary {
                    // mixed venues: route each via the single-submit path (which mints + routes)
                    let mut minted = Vec::with_capacity(reqs.len());
                    for r in reqs {
                        minted.extend(self.apply_intent_routed(
                            OrderIntent::Submit(Box::new(r)),
                            now,
                            cancel_intent,
                            route,
                        ));
                    }
                    return minted;
                }
                let mut minted = Vec::with_capacity(reqs.len());
                for r in reqs.iter_mut() {
                    let was_minted = r.client_order_id.is_empty();
                    if was_minted {
                        r.client_order_id = self.coid_gen.generate();
                    }
                    // Same minted-coid gap closure as the single-submit arm: journal each server-
                    // minted request's resolved terms so its `exec_order` row can be seeded even when
                    // it never fills (the explicit-coid legs are already resolved in the write-ahead).
                    if was_minted {
                        if let Some(journal) = self.journal.as_mut() {
                            journal.append_minted_submit(now, r).expect("journal append");
                            self.journaled_since_snap += 1;
                        }
                    }
                    minted.push(r.client_order_id.clone());
                }
                self.engine.now_ms = now;
                // Capability preflight per leg (w2-task-5): a refused leg gets its synthesized
                // terminal lifecycle (same shape as the Submit arm); the survivors proceed as one
                // batch. The mixed-venue path above needs no twin of this — each of its legs
                // re-enters the Submit arm, which preflights it there.
                //
                // Caps row from the ROUTED engine, per leg, exactly as the Submit arm does — and
                // per LEG rather than hoisted to `self.engine.venue`, because `all_primary` above
                // is `unwrap_or(0)`, so a leg naming a venue no engine answers for counts as
                // primary while resolving NO engine. `caps_venue` keeps that leg on its own string
                // (the unknown-venue affordance), which is what it does today.
                let reqs: Vec<_> = reqs
                    .into_iter()
                    .filter(|r| {
                        let deny = {
                            let routed = self.route_of(route, &r.venue);
                            vike_model::preflight_order_at(r, self.caps_venue(routed, &r.venue))
                                .err()
                        };
                        match deny {
                            None => true,
                            Some(deny) => {
                                self.synthesize_capability_reject(0, r, &deny.to_string(), now);
                                false
                            }
                        }
                    })
                    .collect();
                let mut outbox = Outbox::default();
                self.engine.submit_order_batch(&reqs, now, &mut outbox);
                // Contingency-aware drain (all-primary, so idx 0): a batched linked leg vetoed here
                // drives the cascade too — same guarantee as the single-submit path. Inert unless a
                // leg names a book entry (empty book ⇒ classifier no-op).
                self.publish_and_drive_outbox(0, outbox, now);
                self.pump_client();
                minted
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
            OrderIntent::MassCancel { venue, symbol } => {
                match (venue, symbol) {
                    (None, None) => {
                        self.engine.mass_cancel_with_intent(cancel_intent);
                        for (_, e) in self.extra_engines.iter_mut() {
                            e.mass_cancel_with_intent(cancel_intent);
                        }
                        self.clear_conditional_scope(None, None);
                        // Drop every HELD bracket exit + the whole contingency book (the live legs are
                        // canceled at the venue above; clearing the book here stops their subsequent
                        // `OrderCanceled` events from re-alerting as "protective exit died").
                        self.held_orders.clear();
                        self.contingency.clear();
                    }
                    (Some(v), sym) => {
                        let eidx = self.route_of(route, &v).unwrap_or(0);
                        self.eng_mut(eidx).mass_cancel_with_intent(cancel_intent);
                        // Held exits in scope have no venue order, so clear them explicitly (live legs
                        // self-clean via their venue `OrderCanceled`).
                        self.clear_held_scope(Some(&v), sym.as_deref());
                        self.clear_conditional_scope(Some(&v), sym.as_deref());
                    }
                    (None, Some(_)) => {
                        self.note("MassCancel: symbol without venue is ignored".to_string());
                    }
                }
                self.pump_client();
                Vec::new()
            }
            OrderIntent::Flatten { venue, symbol } => {
                let eidx = self.route_of(route, &venue).unwrap_or(0);
                let pos = self.eng(eidx).position_size_of(&symbol, "BOTH");
                if pos.abs() <= 1e-12 {
                    return Vec::new();
                }
                let req = OrderRequest {
                    client_order_id: String::new(),
                    venue,
                    symbol,
                    side: vike_model::closing_side(pos),
                    qty: pos.abs(),
                    order_type: "market".to_string(),
                    reduce_only: true,
                    ts: now,
                    ..Default::default()
                };
                self.apply_intent_routed(
                    OrderIntent::Submit(Box::new(req)),
                    now,
                    cancel_intent,
                    route,
                )
            }
            OrderIntent::MarketExit { venue } => {
                // COMPOUND VERB, PURE EXPANSION. Nothing here talks to an engine directly: the
                // expansion ([`Self::expand_market_exit`]) is a pure read of live state into a
                // Vec of EXISTING primitive intents, each of which is then re-entered through this
                // same `apply_intent` — so mass-cancel, minting, routing and the RiskGate all keep
                // exactly the semantics they have on every other path.
                //
                // JOURNAL/REPLAY (why no new record kind): the compound intent is journaled by the
                // EXISTING write-ahead `Cmd`/`StrategySubmit` record that carried it, and the
                // expansion is re-derived on replay from the replayed state — the same contract
                // `OrderIntent::Flatten` (position read at apply time) and `MassCancel` already
                // rely on. Replay folds the identical prefix of records, so the `Account`
                // positions this reads are identical, so the expansion is identical, so the coids
                // minted for the flatten legs are identical. ZERO new journal record kinds.
                //
                // ORDERING IS LOAD-BEARING, and the flatten legs are derived AFTER the cancel:
                // `MassCancel`'s own arm ends in `pump_client()`, which can fold venue events —
                // including a FILL that OPENS a position on a symbol that was flat when the
                // operator hit the button. A plan snapshotted before the cancel would carry no leg
                // for that symbol and the "get me out" verb would hand back an OPEN position. So
                // the cancel is applied first and `market_exit_flatten_legs` reads the post-pump
                // `Account`. (Positions already open are safe either way — `Flatten` re-resolves
                // its size via `position_size_of` at apply time, so a fill folded in between
                // shrinks/zeroes the leg rather than double-flattening.)
                //
                // BEST-EFFORT, NOT ATOMIC: against a real venue `mass_cancel` is fire-and-forget
                // over the adapter's `ExecActor` thread — the cancel ACKs arrive asynchronously on
                // the ingest lane, potentially long after these flatten market orders are on the
                // wire. Ordering the legs is the strongest guarantee the core can give locally; it
                // does NOT prevent a resting order filling after a flatten leg on a live venue.
                // Re-issue the exit if the position board is not flat afterwards.
                //
                // `TradingState` — THE PANIC BUTTON WORKS FROM A HALTED CORE, and that is the whole
                // point of it. The flatten legs are ordinary `reduce_only` submits through the SAME
                // `RiskGate`, and its kill switch admits a POSITION-COVERED reduce
                // (`vike_model::is_covered_reduce`) under `Halted` — which is exactly the shape
                // `OrderIntent::Flatten` mints (side opposite the position, qty `|position|`). So
                // they pass under `Active`, `Reducing` AND `Halted`.
                //
                // ⚠ It used to be the opposite, and the reversal is deliberate. The gate denied
                // EVERY order under `Halted`, `reduce_only` included, so this verb ran its
                // mass-cancel and then had every flatten come back `OrderDenied` — disarmed in
                // precisely the situations that reach `Halted` on their own (`enter_safe_state`
                // after a fold panic, the dead-man's switch), which are the situations an operator
                // reaches for this verb. The documented cure was "SetTradingState(Active) first,
                // then re-issue" — i.e. un-halt the whole core, including the strategy that got you
                // here, from a phone, mid-incident. A kill switch must never trap you in a position.
                //
                // ⚠ ONE RESIDUAL, and it is a property of the GRID rather than of the halt: the gate
                // re-checks coverage against the LOT-ROUNDED size, so a position sitting OFF the lot
                // grid (`|position| = 1.8` on a `1.0` lot) rounds its own flatten UP to `2.0`, which
                // would flip the position and is refused — under `Halted` only, since `Reducing`
                // trusts the flag. That leg surfaces as an ordinary `OrderDenied` like any other
                // refusal. Every size this core mints is already lot-rounded, so it takes an
                // externally-sourced position (a venue liquidation, a reconcile fold) to reach.
                let mut coids = Vec::new();
                // RISK-OFF, declared: "get me out" is the one verb whose cancels a venue must
                // never hold back under its own rate/credit budget. Behaviourally identical to the
                // `Unspecified` default on every venue today — this states the classification
                // rather than leaving a venue to infer it from an unlabeled cancel.
                coids.extend(self.apply_intent_routed(
                    Self::market_exit_mass_cancel(venue.as_deref()),
                    now,
                    CancelIntent::RiskOff,
                    route,
                ));
                let halted = (0..=self.extra_engines.len())
                    .filter(|idx| venue.as_deref().is_none_or(|v| self.eng(*idx).venue == v))
                    .any(|idx| self.eng(idx).trading_state == TradingState::Halted);
                let legs = self.market_exit_flatten_legs(venue.as_deref());
                if halted && !legs.is_empty() {
                    // Say so POSITIVELY. An operator who knows the core is halted has every reason
                    // to expect the exit to be refused (it was, for this verb's whole life, and the
                    // runbook said so), so "it went through" is the useful thing to tell them —
                    // and it is what distinguishes a working exit from a silent no-op.
                    tracing::warn!(
                        target: "vike_core::core",
                        legs = legs.len(),
                        "MarketExit while HALTED: the mass-cancel ran and the flatten legs ARE \
                         being submitted — the kill switch admits position-covered reduces, so it \
                         cannot trap you in a position. No un-halt is needed."
                    );
                    self.note(format!(
                        "MarketExit: HALTED — flattening anyway ({} leg(s)); halt admits covered reduces",
                        legs.len()
                    ));
                }
                for it in legs {
                    coids.extend(self.apply_intent_routed(it, now, cancel_intent, route));
                }
                coids
            }
            OrderIntent::Bracket(spec) => {
                let spec = *spec;
                let routed = self.route_of(route, &spec.venue);
                let eidx = routed.unwrap_or(0);
                let (entry, sl, tp) =
                    (self.coid_gen.generate(), self.coid_gen.generate(), self.coid_gen.generate());
                if eidx != 0 {
                    self.coid_venue.insert(entry.clone(), eidx);
                    self.coid_venue.insert(sl.clone(), eidx);
                    self.coid_venue.insert(tp.clone(), eidx);
                }
                let mut orders = build_bracket(&spec, &entry, &sl, &tp);
                for o in orders.iter_mut() {
                    o.ts = now;
                }
                // Same minted-coid gap closure as the Submit/SubmitBatch arms: all THREE bracket
                // legs (entry, SL, TP) are ALWAYS server-minted here (unlike Submit, there is no
                // caller-provided coid to preserve), and the write-ahead `Cmd`/`StrategySubmit`
                // record for this intent carries the pre-mint `BracketSpec` with no coids at all —
                // so without this, a leg that terminalizes without ever filling (e.g. the SL/TP
                // sibling canceled by its OCO partner's fill) has no `exec_order` seed for the
                // materializer to find. Gated on journaling being on; borrowed serialize; counts
                // against the same `journaled_since_snap` cadence. Cold path (order submission, not
                // the market-event fold), so the p99 gate is untouched.
                if let Some(journal) = self.journal.as_mut() {
                    for o in orders.iter() {
                        journal.append_minted_submit(now, o).expect("journal append");
                        self.journaled_since_snap += 1;
                    }
                }
                self.eng_mut(eidx).now_ms = now;
                // Capability preflight (w2-task-5), ATOMIC over the bracket: a bracket with a
                // child the venue's declared row cannot hold is refused WHOLE — submitting the
                // entry while refusing its protective stop would strand a naked position, and
                // letting the child through invites the silent-coercion class (a "stop" firing
                // immediately as market on a venue with no native trigger). The culprit carries
                // its machine-readable reason; each sibling carries `BRACKET_ATOMIC_REFUSED:
                // culprit=<coid>` so the group refusal is traceable from any leg.
                // Caps row from the ROUTED engine, as in the Submit arm — every leg carries the
                // bracket's own `spec.venue`, so one resolution covers all three. Scoped so the
                // `&self` borrow ends before the `&mut self` refusal loop below.
                let refusal = {
                    let caps_venue = self.caps_venue(routed, &spec.venue);
                    orders.iter().enumerate().find_map(|(i, o)| {
                        vike_model::preflight_order_at(o, caps_venue).err().map(|d| (i, d))
                    })
                };
                if let Some((ci, deny)) = refusal {
                    let culprit = orders[ci].client_order_id.clone();
                    let reason = deny.to_string();
                    let sibling_reason = format!("BRACKET_ATOMIC_REFUSED: culprit={culprit}");
                    for (i, o) in orders.iter().enumerate() {
                        let r = if i == ci { &reason } else { &sibling_reason };
                        self.synthesize_capability_reject(eidx, o, r, now);
                    }
                    self.pump_client();
                    return vec![entry, sl, tp];
                }
                // Live-runtime OTO/OCO submit-hold (the WHOLE bracket): record all three legs'
                // linkage in the shared book, then submit ONLY the OTO entry and HOLD the protective
                // exits off the venue until the entry fills (`drive_contingency_on_fill` releases
                // them). This is the emulation the feature exists for — a stop-loss / take-profit
                // sitting live at the venue before the entry fills is a naked order that can trigger
                // and open an unwanted position; holding it is what makes the bracket an atomic OTO.
                // `build_bracket` stamps the entry active (no parent) and sl/tp held (parent =
                // entry), so the split falls straight out of `is_held`. `coid_venue` for all three is
                // already recorded above, so a later release/cancel routes correctly.
                self.eng_mut(eidx).now_ms = now;
                for o in &orders {
                    if has_contingency_links(o) {
                        self.contingency.insert(
                            o.client_order_id.clone(),
                            o.parent_order_id.clone(),
                            o.linked_order_ids.clone(),
                        );
                    }
                }
                // ORDERING IS LOAD-BEARING: hold EVERY exit into `held_orders` FIRST, THEN submit the
                // entry. The entry's `submit_order` can be vetoed by the RiskGate right here (an
                // `OrderDenied` in its outbox), and `publish_and_drive_outbox` then cascade-drops the
                // entry's held children. If the children were still being inserted AFTER the entry
                // submit, that cascade would find them gone from the book and the loop would go on to
                // submit them to the venue — a naked exit on a denied entry. So exits are fully held
                // before the entry can be denied.
                let mut entries = Vec::new();
                for o in orders {
                    let ocoid = o.client_order_id.clone();
                    if self.contingency.is_held(&ocoid) {
                        self.note(format!("bracket exit {ocoid} held pending entry fill"));
                        self.held_orders.insert(ocoid, o);
                    } else {
                        entries.push(o);
                    }
                }
                for o in entries {
                    // the OTO entry goes to the venue as a PLAIN order (the core owns the linkage in
                    // its book) — see `strip_contingency_links`. A RiskGate veto here drives the
                    // cascade via `publish_and_drive_outbox`, dropping the held exits.
                    let plain = strip_contingency_links(o);
                    let mut outbox = Outbox::default();
                    self.eng_mut(eidx).submit_order(&plain, now, &mut outbox);
                    self.publish_and_drive_outbox(eidx, outbox, now);
                }
                self.pump_client();
                vec![entry, sl, tp]
            }
            OrderIntent::ArmConditional(c) => {
                let ConditionalIntent { venue, symbol, side, qty, price, trail, trigger_by } = c;
                let eidx = self.route_of(route, &venue).unwrap_or(0);
                // Trigger-source gate (w2 trigger_by): the core has NO index lane, so an Index
                // arm could never fire — REFUSED here (before minting an id, like the
                // no-mark trailing refusal below), surfaced to recent-events, never armed
                // inert and never silently evaluated off a different series.
                if trigger_by == Some(vike_model::TriggerBy::Index) {
                    self.note(
                        "ArmConditional REFUSED: trigger_by Index — the core has no index lane"
                            .into(),
                    );
                    return Vec::new();
                }
                // The id is minted INSIDE each arm branch, only once the arm is known good (the
                // trailing branch can still refuse on a missing mark): a refused arm that burned an
                // id would leave gaps in the sequence, which is exactly what makes a gap
                // diagnostic — an id that exists but names no arm is indistinguishable from a lost
                // record. Each branch then journals its RESOLVED terms BEFORE mutating the book,
                // the same write-ahead discipline `submit_fired` uses for the FIRE: a crash between
                // the two loses an arm, never hides one.
                if let Some(trail) = trail {
                    let mark = self.eng(eidx).account.mark_of(&venue, &symbol).unwrap_or(0.0);
                    if mark <= 0.0 {
                        self.note("trailing-stop REFUSED: no mark to seed the extreme".to_string());
                        return Vec::new();
                    }
                    let arm_id = self.mint_arm_id();
                    self.journal_conditional_armed(
                        now,
                        &arm_id,
                        crate::journal::ConditionalRecord {
                            venue: venue.clone(),
                            symbol: symbol.clone(),
                            side,
                            qty,
                            price: None,
                            trail: Some(trail),
                            extreme: Some(mark),
                            trigger_by,
                        },
                    );
                    // A duplicate id cannot occur live (the counter is monotone and, on restart,
                    // resumed from the Snap's stamped `arm_seq`) — the book's structural
                    // uniqueness is the backstop, and a refusal here would mean that resume
                    // contract was violated upstream. Surface it, never panic in the fold.
                    if self
                        .conditional_books
                        .entry((venue, symbol))
                        .or_default()
                        .add_trailing(&arm_id, side, qty, trail, mark, trigger_by)
                    {
                        // WHICH ACCOUNT this arm protects, recorded at the one moment it is known —
                        // see `CoreThread::cond_engine`. Only for an arm that actually entered a
                        // book, so the map cannot outlive its arm.
                        self.cond_engine.insert(arm_id.clone(), eidx);
                    } else {
                        self.note(format!("ArmConditional: duplicate arm id {arm_id} — REFUSED"));
                    }
                } else if let Some(px) = price {
                    let arm_id = self.mint_arm_id();
                    self.journal_conditional_armed(
                        now,
                        &arm_id,
                        crate::journal::ConditionalRecord {
                            venue: venue.clone(),
                            symbol: symbol.clone(),
                            side,
                            qty,
                            price: Some(px),
                            trail: None,
                            extreme: None,
                            trigger_by,
                        },
                    );
                    if self
                        .conditional_books
                        .entry((venue, symbol))
                        .or_default()
                        .add_stop(&arm_id, side, qty, px, trigger_by)
                    {
                        // The stop twin of the trailing arm above — see `CoreThread::cond_engine`.
                        self.cond_engine.insert(arm_id.clone(), eidx);
                    } else {
                        self.note(format!("ArmConditional: duplicate arm id {arm_id} — REFUSED"));
                    }
                } else {
                    self.note("ArmConditional: neither price nor trail set — ignored".to_string());
                }
                Vec::new()
            }
            OrderIntent::DisarmConditional { arm_id } => {
                // The individual-cancel twin of the ARM (emulator PR-2). Route by PROBING the
                // books for the id — the intent deliberately carries only `arm_id` (the minted id
                // is globally unique per core: monotone counter, Snap-resumed across restarts, and
                // each book refuses duplicates structurally), so the caller does not need to
                // remember the (venue, symbol) it armed on. The probe walks the books map
                // (insertion order, deterministic); books are few (one per armed (venue, symbol))
                // and this is command cadence — never the per-message fold.
                let key = self
                    .conditional_books
                    .iter()
                    .find(|(_, b)| b.contains(&arm_id))
                    .map(|(k, _)| k.clone());
                match key {
                    Some(key) => {
                        // WRITE-AHEAD, the PR-1 ordering discipline: the record goes down BEFORE
                        // the book mutation — a crash between the two re-applies the disarm from
                        // its own write-ahead `Cmd`/`StrategySubmit` on restore, so it can be
                        // repeated, never lost. Journaled ONLY for an actual disarm (the refusal
                        // below writes nothing).
                        self.journal_conditional_disarmed(now, &arm_id);
                        let removed = self
                            .conditional_books
                            .get_mut(&key)
                            .map(|b| b.disarm(&arm_id))
                            .unwrap_or(false);
                        debug_assert!(removed, "probe found the arm; disarm must remove it");
                        // The arm left its book, so its account entry goes with it — see
                        // `CoreThread::cond_engine` for the bound this keeps.
                        self.cond_engine.remove(&arm_id);
                        // Confirm on the same surface the ARM's refusals use (recent-events):
                        // armed conditionals have no snapshot view yet, so a silent removal would
                        // leave the operator unable to tell a disarm happened at all.
                        self.note(format!("conditional {arm_id} DISARMED"));
                    }
                    None => {
                        // Unknown/stale id: a LOUD no-op — never a panic, never silent. Mirrors
                        // the ARM's refusal surfacing (`recent`), same stale-click tolerance as
                        // `ConditionalBook::disarm` itself. NOT journaled: no state changed.
                        self.note(format!("DisarmConditional: unknown arm id {arm_id} — ignored"));
                    }
                }
                Vec::new()
            }
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

    /// Submit ONE resolved request to its engine — the venue-facing tail shared by the plain
    /// `Submit`/`Bracket` paths and the OTO-child RELEASE: route by venue, stamp the clock,
    /// `submit_order` (gate → register → `client.submit`), drain the outbox to the bus. It does NOT
    /// touch the contingency book (the caller owns that) and does NOT re-run the capability preflight
    /// (a released child was preflighted atomically with its bracket before it was ever held). The
    /// coid is the caller's; nothing is returned.
    fn submit_resolved(&mut self, req: &OrderRequest, now: i64) {
        // Released to the venue as a PLAIN order — the core keeps the OTO/OCO linkage in its book,
        // so the venue (or an OCO-enforcing paper client) must not re-interpret it. Stripping is
        // what makes the release land as a normal resting order rather than being re-held by a
        // client that also enforces OTO. See `strip_contingency_links`.
        let req = strip_contingency_links(req.clone());
        // ⚠ THE ORDER'S OWN ENGINE FIRST, and only then its venue string. This order was ROUTED
        // once already, when it was submitted and held (`apply_intent_routed` recorded the index in
        // `coid_venue` for every non-primary engine), and re-deriving it from `req.venue` would
        // send a labelled account's held OTO child to that venue's DEFAULT account on release — the
        // one moment in an order's life when the caller that knew the account is long gone.
        let eidx = self
            .coid_venue
            .get(&req.client_order_id)
            .copied()
            .or_else(|| self.engine_idx_for_route_key(RouteKey::sole_account_of(&req.venue)))
            .unwrap_or(0);
        if eidx != 0 {
            self.coid_venue.insert(req.client_order_id.clone(), eidx);
        }
        self.eng_mut(eidx).now_ms = now;
        let mut outbox = Outbox::default();
        self.eng_mut(eidx).submit_order(&req, now, &mut outbox);
        // Drive the outbox contingency-aware: a released exit vetoed by the RiskGate at arm-time
        // (`OrderDenied`) must clean its own now-stale book entry (`drive_contingency_on_terminal`),
        // not linger active-but-dead. No children to cascade (an exit is a leaf); the surviving OCO
        // sibling is kept or canceled per `CoreConfig::oco_cancel_sibling_on_dead_exit` (keep is the
        // default — see `drive_contingency_on_terminal`).
        self.publish_and_drive_outbox(eidx, outbox, now);
    }

    /// Live-runtime OTO/OCO fill drive: after `filled_coid` fully fills and the engine has folded it,
    /// ARM this leg's held OTO children (submit each to the venue — the parent's fill is their
    /// trigger) and CANCEL its OCO siblings. The DECISION is the shared [`vike_exec::ContingencyBook`]
    /// resolver — the SAME `on_fill` the paper/backtest oracle drives, so the live path cannot
    /// diverge from it; this method owns only the book MECHANICS (release the held request, cancel
    /// the resting sibling). A no-op for a plain fill (not in the book). COLD path — per FILL, never
    /// the market-data fold.
    pub(crate) fn drive_contingency_on_fill(&mut self, filled_coid: &str, now: i64) {
        if self.contingency.is_empty() {
            return;
        }
        // `on_fill` arms every held child of the filled leg (flips it active) AND returns the leg's
        // OCO siblings (its `linked` ids that are NOT its own children) to cancel, in `linked` order.
        let siblings = self.contingency.on_fill(filled_coid);
        // RELEASE the just-armed OTO children: a child of the filled leg that is now active has its
        // held request drained + submitted to the venue. (A child still held — a deeper OTO tier not
        // armed by THIS fill — is left resting.)
        for child in self.contingency.children_of(filled_coid) {
            if !self.contingency.is_held(&child) {
                if let Some(req) = self.held_orders.shift_remove(&child) {
                    self.submit_resolved(&req, now);
                    self.note(format!("OTO child {child} released by {filled_coid} fill"));
                }
            }
        }
        // CANCEL the OCO siblings: one still HELD (never went live) is simply dropped from the held
        // map; one resting at the venue is canceled (`cancel_order` is `is_live`-guarded and the
        // venue emits the authoritative `OrderCanceled`). Each leaves the book.
        //
        // Unclassified (`CancelIntent::Unspecified`) on purpose: the sibling is removed from the
        // contingency book on the SAME pass, so nothing re-issues this cancel — a venue that held
        // it back to protect its own budget would strand a live protective leg whose OCO twin has
        // already filled. Book maintenance is not "routine churn" in the shed-and-re-offer sense.
        for sib in siblings {
            if self.held_orders.shift_remove(&sib).is_some() {
                self.note(format!("OCO sibling {sib} dropped (was held) on {filled_coid} fill"));
            } else {
                let eidx = self.coid_venue.get(&sib).copied().unwrap_or(0);
                self.eng_mut(eidx).cancel_order(&sib);
                self.note(format!("OCO sibling {sib} canceled on {filled_coid} fill"));
            }
            self.contingency.remove(&sib);
        }
        // The filled leg leaves the book (it terminalized). Its just-armed children STAY (active/live)
        // so a later child fill can still resolve its own OCO sibling.
        self.contingency.remove(filled_coid);
        // NB: no `pump_client` here on purpose. This runs from BOTH fold sites — the `Ingest::Event`
        // arm AND `pump_client`'s own drain loop (client-synthesized fills, e.g. the paper exchange).
        // Pumping here would re-enter `pump_client` from inside its own loop; instead each call site
        // pumps after driving, and the `pump_client` loop naturally drains the events this queued.
    }

    /// Live-runtime OTO/OCO terminal drive: a contingency leg that terminalized WITHOUT filling
    /// (canceled / rejected / DENIED / expired) can never arm its still-held OTO children —
    /// cascade-drop them (they were never sent to the venue, so there is nothing to cancel there) and
    /// drop the terminated leg's OWN book entry (stop the stale-entry leak). The paper oracle's
    /// `expire_children_of` on the live path, plus own-entry cleanup. Already-armed (live) children
    /// are LEFT — they are real resting orders now, cleaned by their own terminal event.
    ///
    /// A dying PROTECTIVE EXIT (a released leg with a parent — e.g. a stop-loss the venue rejected)
    /// is SURFACED (warn + recent-events note) so the operator knows a leg of the protection died.
    /// Its surviving OCO sibling's fate is the [`crate::CoreConfig::oco_cancel_sibling_on_dead_exit`]
    /// knob:
    ///
    /// - **OFF (default)** — the sibling is KEPT, not auto-canceled: a position that just lost one
    ///   protective leg retains whatever protection it still has (a naked position is the worse
    ///   default). Byte-identical to the feature as first shipped; only a FILL cancels a sibling.
    /// - **ON** — the surviving OCO sibling is ALSO canceled and cleaned from the book, through the
    ///   SAME mechanics the fill-driven OCO-cancel uses ([`Self::drive_contingency_on_fill`]): a
    ///   still-held sibling is dropped, a resting one is canceled at the venue (its authoritative
    ///   `OrderCanceled` re-enters here, but the book no longer holds it, so that is a no-op). A
    ///   deployment that prefers a fully flat book over partial protection opts into this.
    pub(crate) fn drive_contingency_on_terminal(&mut self, coid: &str) {
        if self.contingency.is_empty() || !self.contingency.contains(coid) {
            return;
        }
        // Snapshot whether this is a dead protective exit BEFORE the removals below borrow the book.
        let dead_exit_parent = self.contingency.parent_of(coid).map(str::to_string);
        // Opt-in sibling-cancel (`CoreConfig::oco_cancel_sibling_on_dead_exit`, default OFF): capture
        // the dead exit's surviving OCO siblings NOW, before its own book entry is removed below —
        // that removal drops the `linked` list `siblings_of` reads. OFF leaves this empty and the
        // cancel block inert, so the keep-protection default is byte-identical.
        let doomed_siblings: Vec<String> =
            if self.config.oco_cancel_sibling_on_dead_exit && dead_exit_parent.is_some() {
                self.contingency.siblings_of(coid)
            } else {
                Vec::new()
            };
        let mut queue = vec![coid.to_string()];
        while let Some(parent) = queue.pop() {
            for child in self.contingency.children_of(&parent) {
                if self.contingency.is_held(&child) {
                    self.held_orders.shift_remove(&child);
                    self.contingency.remove(&child);
                    self.note(format!("OTO child {child} dropped: parent {parent} terminated"));
                    queue.push(child);
                }
            }
        }
        self.contingency.remove(coid);
        let Some(parent) = dead_exit_parent else {
            return;
        };
        if doomed_siblings.is_empty() {
            // KNOB OFF (or nothing left to cancel): keep the surviving protection, surface the death.
            tracing::warn!(
                target: "vike_core::core",
                coid,
                parent = %parent,
                "protective exit leg terminated UNFILLED — the position keeps its remaining OCO \
                 protection (the surviving sibling is not auto-canceled)"
            );
            self.note(format!(
                "protective exit {coid} died unfilled (parent {parent}); surviving OCO sibling KEPT"
            ));
            return;
        }
        // KNOB ON: cancel every surviving OCO sibling too — the same mechanics as the fill-driven
        // cancel (`drive_contingency_on_fill`): a still-held sibling is dropped, a resting one is
        // canceled at the venue; each leaves the book.
        for sib in doomed_siblings {
            if self.held_orders.shift_remove(&sib).is_some() {
                self.note(format!(
                    "OCO sibling {sib} dropped (was held): protective exit {coid} died unfilled"
                ));
            } else {
                let eidx = self.coid_venue.get(&sib).copied().unwrap_or(0);
                self.eng_mut(eidx).cancel_order(&sib);
                self.note(format!(
                    "OCO sibling {sib} canceled: protective exit {coid} died unfilled"
                ));
            }
            self.contingency.remove(&sib);
        }
        tracing::warn!(
            target: "vike_core::core",
            coid,
            parent = %parent,
            "protective exit leg terminated UNFILLED — sibling-cancel knob ON: the surviving OCO \
             sibling was canceled (book left flat)"
        );
        self.note(format!(
            "protective exit {coid} died unfilled (parent {parent}); surviving OCO sibling CANCELED"
        ));
    }

    /// Cancel a HELD (not-yet-at-venue) contingency leg: drop it from `held_orders` and the book.
    /// Returns `true` iff `coid` was held (so a cancel caller skips the venue path). A held leg has no
    /// registered order to terminalize, so this NOTES the removal rather than emitting an
    /// `OrderCanceled` the FSM would drop as an unknown coid. Its OTO parent's `linked` list still
    /// names it, but `on_fill`/`children_of` resolve by the BOOK, from which it is now gone, so the
    /// parent's later fill neither releases nor references it.
    fn cancel_held(&mut self, coid: &str) -> bool {
        if self.held_orders.shift_remove(coid).is_some() {
            self.contingency.remove(coid);
            self.note(format!("held bracket exit {coid} canceled before its parent filled"));
            true
        } else {
            false
        }
    }

    /// Drop every HELD contingency leg matching a `(venue, symbol)` scope (`None` = any) from
    /// `held_orders` + the book — the `MassCancel` scoped twin of [`Self::cancel_held`]. Live legs are
    /// NOT touched here (the engine's `mass_cancel` cancels those at the venue; their `OrderCanceled`
    /// cleans the book).
    fn clear_held_scope(&mut self, venue: Option<&str>, symbol: Option<&str>) {
        let doomed: Vec<String> = self
            .held_orders
            .iter()
            .filter(|(_, r)| {
                venue.is_none_or(|v| r.venue == v) && symbol.is_none_or(|s| r.symbol == s)
            })
            .map(|(c, _)| c.clone())
            .collect();
        for c in doomed {
            self.held_orders.shift_remove(&c);
            self.contingency.remove(&c);
        }
    }

    /// Clear every ARMED conditional matching a `(venue, symbol)` scope (`None` = any) — the
    /// `MassCancel` twin of [`Self::clear_held_scope`], and the ONE spelling of "a book is being
    /// emptied", so an arm's account entry ([`CoreThread::cond_engine`]) cannot be left behind by a
    /// site that remembered to clear the book and forgot the map.
    ///
    /// Two passes rather than one because the ids are read from `conditional_books` and spent on
    /// `cond_engine`; the books are few (one per armed `(venue, symbol)`) and this is command
    /// cadence, never the per-message fold.
    fn clear_conditional_scope(&mut self, venue: Option<&str>, symbol: Option<&str>) {
        let in_scope = |(v, s): &(String, String)| {
            venue.is_none_or(|want| v == want) && symbol.is_none_or(|want| s == want)
        };
        let doomed: Vec<String> = self
            .conditional_books
            .iter()
            .filter(|(k, _)| in_scope(k))
            .flat_map(|(_, b)| b.iter().map(|(id, _)| id.to_string()))
            .collect();
        for id in doomed {
            self.cond_engine.remove(&id);
        }
        for (k, book) in self.conditional_books.iter_mut() {
            if in_scope(k) {
                book.clear();
            }
        }
    }

    /// Synthesize the TERMINAL refusal lifecycle for an order the capability preflight
    /// ([`vike_model::preflight_order`]) refused — the exact event shape of the Combo arm's
    /// `supports_combo` rejection: register the order so the FSM has an entry to advance, then
    /// `OrderSubmitted` → `OrderRejected` carrying the machine-readable `reason`
    /// (`CATEGORY_CONDITION: key=value`, e.g. `TIF_UNSUPPORTED: tif=Fok venue=ig`). No order may
    /// silently vanish, and there is no venue client on the other side of a refusal, so the core
    /// emits the whole lifecycle itself. Per-ORDER log + note — off the fold path.
    fn synthesize_capability_reject(
        &mut self,
        eidx: usize,
        req: &vike_model::OrderRequest,
        reason: &str,
        now: i64,
    ) {
        tracing::warn!(
            target: "vike_core::core",
            coid = %req.client_order_id,
            venue = %req.venue,
            reason,
            "order REFUSED by capability preflight"
        );
        let eng = self.eng_mut(eidx);
        let mut mo = vike_exec::ManagedOrder::new(req.clone());
        mo.created_ms = Some(now);
        eng.registry.insert(req.client_order_id.clone(), mo);
        self.publish_to(
            eidx,
            Event::OrderSubmitted(vike_model::events::OrderSubmitted {
                client_order_id: req.client_order_id.clone(),
                ts: now,
            }),
        );
        self.publish_to(
            eidx,
            Event::OrderRejected(OrderRejected {
                client_order_id: req.client_order_id.clone(),
                reason: reason.to_string().into(),
                ts: now,
            }),
        );
        self.note(format!("order {} REFUSED: {reason}", req.client_order_id));
    }

    /// Lower one [`OrderIntent::Combo`] onto the engine apply surface — the PR-2 half of combo
    /// orders (PR-1 shipped the vocabulary; `vike_exec::RiskGate::check_combo` shipped the gate).
    ///
    /// `venue_supports_combo` is passed IN rather than read here so the static-registry lookup
    /// stays at the call site and this function is a pure function of the answer.
    /// [`vike_model::build_combo`] deliberately leaves `symbol` EMPTY for the venue adapter to
    /// resolve at submit, so a `true` caps row is a PROMISE the adapter really does that step —
    /// the registry keeps every other venue's combo on the terminal-reject path below, the
    /// protection that stops an empty-symbol order reaching a live venue. DERIBIT is the one
    /// `true` row (pinned by `vike_model`'s `combo_support_is_deribit_only`): its `submit_order`
    /// routes a non-empty `combo_legs` through `private/create_combo` + the orientation solve
    /// (`vike_deribit::combo`), proven live by the gated `deribit_combo_smoke` order lifecycle.
    ///
    /// Shape, in order:
    /// 1. **Validate, THEN mint ONE coid for the whole combo** — never one per leg. A combo is ONE
    ///    order everywhere downstream (one coid, one `ManagedOrder`, one event stream) because the
    ///    VENUE is the group manager; `build_combo` carries every leg on that single request. An
    ///    invalid (hand-built) spec refuses BEFORE the mint — no coid-sequence gap.
    /// 2. **Write-ahead journal** of the resolved request (`append_minted_submit`), exactly as the
    ///    `Submit`/`Bracket` arms do: the `Cmd` record for this intent carries the PRE-mint
    ///    `ComboSpec` with no coid at all, so without this a combo that terminalizes without ever
    ///    filling has no `exec_order` seed for the materializer. Journaled BEFORE the capability
    ///    and risk refusals on purpose — a refused combo is precisely the audit trail worth
    ///    keeping. (The invalid-spec refusal precedes the mint, so there is nothing to journal.)
    /// 3. **Venue capability.** A venue that cannot take a combo gets a SYNTHESIZED TERMINAL
    ///    lifecycle — `OrderSubmitted` then `OrderRejected` — never a silent drop (the emitter-split
    ///    contract: no order may vanish, and a dead venue path must synthesize the terminal
    ///    itself). Registered first so the FSM has an entry for those two events to advance.
    /// 4. **Risk.** [`vike_exec::RiskGate::check_combo`] crosses every leg as a synthetic
    ///    single-leg order and ACCUMULATES the admitted legs' commitments, so N legs cannot each
    ///    fit inside the same unchanged free buying power. A veto surfaces as `OrderDenied` — the
    ///    same event a single-order veto produces, captured for `Strategy::on_order_event` exactly
    ///    as `gate_and_register` captures it — and the combo never enters the registry.
    /// 5. **Submit.** Admit every leg symbol into the engine's `extra_symbols` scope (leg fills
    ///    carry LEG symbols; without this the bare-`Fill` symbol filter drops them and the Account
    ///    stays flat), then register + hand the request to the client, mirroring
    ///    `ExecutionEngine::submit_order`'s tail. The venue emits the rest of the lifecycle.
    ///
    /// Returns the coid IFF the combo reached the venue; every refusal path returns empty, per the
    /// documented [`Self::apply_intent`] contract ("coids of orders SUBMITTED"). Validation runs
    /// BEFORE the mint (the `ArmConditional` mint-after-validation rule: a refused spec that
    /// burned a coid would leave a sequence gap, and a gap is only diagnostic while an id that
    /// exists but names no order stays impossible); the capability and risk refusals mint FIRST
    /// because their refusal events must NAME an order — the same trade-off the single-order path
    /// makes when it mints ahead of the gate.
    ///
    /// COLD PATH: this runs per ORDER, never per market message, so the per-leg context snapshot
    /// and its `Vec` are off the measured p99 core hop. No logging happens on the fold path.
    fn lower_combo(
        &mut self,
        spec: vike_model::ComboSpec,
        now: i64,
        venue_supports_combo: bool,
        route: EngineRoute,
    ) -> Vec<String> {
        let eidx = self.route_of(route, &spec.venue).unwrap_or(0);
        // Validate FIRST, mint SECOND. `ComboSpec` cannot normally be invalid (`validate` runs in
        // both its constructor and its `Deserialize`), so a failure here is a hand-built spec.
        // Refuse loudly, mint nothing, emit nothing: there is no order to name yet — and minting
        // one would burn a coid into a sequence gap (see the doc above). `pump_client` still runs,
        // for uniformity with every other arm's tail.
        if let Err(e) = spec.validate() {
            self.note(format!("combo REFUSED: {e}"));
            self.pump_client();
            return Vec::new();
        }
        let coid = self.coid_gen.generate();
        let Ok(mut req) = vike_model::build_combo(&spec, &coid) else {
            // Unreachable: `validate` passed just above and `build_combo`'s only failure IS
            // `validate` on the same immutable spec. Refuse gracefully anyway — never panic in
            // the fold.
            self.note("combo REFUSED: spec failed re-validation".to_string());
            self.pump_client();
            return Vec::new();
        };
        req.ts = now;
        if eidx != 0 {
            self.coid_venue.insert(coid.clone(), eidx);
        }
        if let Some(journal) = self.journal.as_mut() {
            journal.append_minted_submit(now, &req).expect("journal append");
            self.journaled_since_snap += 1;
        }
        // Stamp the engine clock BEFORE the gate, mirroring the single-order path (the `Submit`
        // arm stamps ahead of `submit_order`): a denied combo must not leave `now_ms` stale.
        self.eng_mut(eidx).now_ms = now;

        if !venue_supports_combo {
            // No order may silently vanish. There is no venue client on the other side of this to
            // emit the lifecycle, so synthesize the whole terminal sequence locally. Registered
            // FIRST so the FSM has an entry for `OrderSubmitted` (Initialized -> Submitted) and
            // then `OrderRejected` (-> Rejected) to advance through. Per-ORDER log, off the fold.
            tracing::warn!(
                target: "vike_core::core",
                coid = %coid,
                venue = %spec.venue,
                "combo REJECTED: venue declares no combo support"
            );
            let eng = self.eng_mut(eidx);
            let mut mo = vike_exec::ManagedOrder::new(req.clone());
            mo.created_ms = Some(now);
            eng.registry.insert(coid.clone(), mo);
            self.publish_to(
                eidx,
                Event::OrderSubmitted(vike_model::events::OrderSubmitted {
                    client_order_id: coid.clone(),
                    ts: now,
                }),
            );
            self.publish_to(
                eidx,
                Event::OrderRejected(OrderRejected {
                    client_order_id: coid,
                    reason: format!("venue {} does not support combo orders", spec.venue).into(),
                    ts: now,
                }),
            );
            self.note(format!("combo REJECTED: venue {} declares no combo support", spec.venue));
            self.pump_client();
            return Vec::new();
        }

        // Snapshot each leg's own risk facts BEFORE reaching for the gate: `check_combo` takes
        // `leg_ctx` by `Fn`, and a closure reading the engine's `account` live would borrow the
        // engine immutably while `gate` is borrowed mutably off that same struct. A `Vec` (not a
        // map) because a combo is 2..~6 legs — the linear probe is cheaper than hashing, and this
        // is a per-order path anyway.
        let mut leg_ctxs: Vec<(String, vike_exec::RiskContext)> =
            Vec::with_capacity(spec.legs.len());
        {
            let eng = self.eng(eidx);
            // Equity is an ACCOUNT-level fact — identical for every leg — folded ONCE, and only
            // when the margin knob is armed for at least one leg (`resolved_equity` is
            // O(positions); per-order path, still not free). One-price law: resolver-priced,
            // the SAME `config.price_cfg` source the snapshot/sampler/watchdog read.
            let armed_any = spec.legs.iter().any(|l| eng.gate.limits.im_for(&l.symbol).is_some());
            let equity = if armed_any {
                eng.resolved_equity(eng.equity_seed, &self.config.price_cfg)
            } else {
                0.0
            };
            for leg in &spec.legs {
                // Σ initial margin of the open book, priced PER LEG — the exact mirror of
                // `gate_and_register`'s Phase-B fold: an UNMARKED position contributes 0 (it
                // cannot be priced — LEAN's 0-rate skip), and a marked position with NO per-symbol
                // IM override falls back to the order symbol's own `im_req`. A combo has no single
                // order symbol, so the fallback here is the im_req of THE LEG BEING PRICED —
                // which is why this fold runs per leg rather than once: two legs with different
                // rates price the same no-override position differently, exactly as two naked
                // orders on those symbols would. (Silently SKIPPING a no-override position — this
                // block's original shape — understated `margin_used` whenever the global
                // `im_requirement` is unset and margin was armed per-symbol, which is exactly what
                // `Command::SetMargin` produces: it only writes `im_by_symbol`. That admitted
                // combos whose naked legs would be denied, violating the gate's own invariant.)
                // An unarmed leg (`im_for` None) gets the single path's unarmed tuple — the
                // buying-power check is off for it and never reads these fields. `check_combo`
                // threads each admitted leg's own margin on top of this baseline itself.
                let (leg_equity, margin_used) =
                    if let Some(leg_im) = eng.gate.limits.im_for(&leg.symbol) {
                        // THE shared margin-in-use fold, resolver-priced
                        // (`resolved_margin_in_use_by` under `config.price_cfg` — the same
                        // basis as the resolver `equity` above and the single-order gate's own
                        // fold), rated PER LEG: a no-override position falls back to THIS
                        // leg's `leg_im` (the combo has no single order symbol), exactly
                        // mirroring the single-order gate — including its POOL POLICY (the
                        // liquidation law's partition): only CROSS positions consume the
                        // shared equity the combo is admitted against; an Isolated position's
                        // wallet / a Cash position's full funding never backed it, so counting
                        // them would double-charge a mixed account. All-cross accounts: filter
                        // is a no-op.
                        let used = eng.resolved_margin_in_use_by(
                            &self.config.price_cfg,
                            |(_v, s, _side), p| {
                                p.margin_mode
                                    .is_cross()
                                    .then(|| eng.gate.limits.im_for(s).unwrap_or(leg_im))
                            },
                        );
                        (equity, used)
                    } else {
                        (0.0, 0.0)
                    };
                // The leg's own position size, reused for the resolver's side-aware quote choice
                // below exactly as the single-order gate feeds its `pos`.
                let pos = eng.position_size_of(&leg.symbol, "BOTH");
                leg_ctxs.push((
                    leg.symbol.clone(),
                    vike_exec::RiskContext {
                        position_size: pos,
                        // The leg's notional/exposure REFERENCE now shares the resolver with the
                        // per-leg equity/margin folded above — the one-price law extended to this
                        // gate's LAST split-basis site (the combo twin of #550's `gate_and_register`
                        // convergence). Priced through the position's OWN side chain under
                        // `config.price_cfg`, the SAME basis the snapshot/sampler/watchdog read,
                        // rather than off the raw untagged `Account.marks` scalar `mark_of` returned
                        // while the same call's equity/margin were board-priced. `check_combo` still
                        // DENIES a leg whose reference is missing, non-finite or <= 0
                        // (`leg <sym>: no-mark`) precisely because a zero reference makes every
                        // price-based limit (notional cap, exposure, buying power) vacuous at once —
                        // a `Missing` resolution maps to 0.0, exactly the empty raw-scalar read it
                        // replaces, so that refusal is preserved. A fresh venue mark prices both
                        // stores identically, so an ordinary marked leg is byte-identical to the
                        // pre-convergence `mark_of` read; only a STALE mark tightens the verdict.
                        mark_price: eng
                            .resolved_position_price(
                                &eng.venue,
                                &leg.symbol,
                                pos,
                                &self.config.price_cfg,
                            )
                            .unwrap_or(0.0),
                        trading_state: eng.trading_state,
                        now_ms: now,
                        equity: leg_equity,
                        margin_used,
                        // `check_combo` re-derives each leg's own reduce_only from the PROJECTED
                        // book, so the reversing-leg margin credit `gate_and_register` computes has
                        // no single-symbol meaning here. Left 0.0 so the crossing errs strictly
                        // HIGH — the gate's own stance ("a combo must never pass a gate its naked
                        // legs would fail").
                        closing_credit: 0.0,
                        multiplier: eng.account.multiplier_of(&leg.symbol),
                    },
                ));
            }
        }

        // Account-level ctx. `check_combo` takes `trading_state` + `now_ms` from HERE and lets them
        // OVERRIDE whatever `leg_ctx` returns, so a `Reducing`/`Halted` account can never be
        // laundered into `Active` by the per-leg snapshot.
        let ctx = vike_exec::RiskContext {
            trading_state: self.eng(eidx).trading_state,
            now_ms: now,
            ..Default::default()
        };
        let verdict = self.eng_mut(eidx).gate.check_combo(&req, &ctx, |sym| {
            // Total by contract. A symbol with no snapshot falls back to a default ctx whose mark
            // is 0.0, which `check_combo` DENIES as `no-mark` — the honest answer for a symbol the
            // runtime knows nothing about.
            leg_ctxs.iter().find(|(s, _)| s == sym).map(|(_, c)| *c).unwrap_or_default()
        });

        let Some(admitted) = verdict.request.filter(|_| verdict.ok) else {
            // Per-ORDER (not per-tick): a RiskGate veto is fault-adjacent, off the measured hop —
            // the same budget the single-order veto log takes in `gate_and_register`.
            tracing::warn!(
                target: "vike_core::risk",
                coid = %coid,
                reason = %verdict.reason,
                "combo denied by RiskGate"
            );
            self.note(format!("combo DENIED: {}", verdict.reason));
            // Mirror `gate_and_register`'s veto capture for `Strategy::on_order_event`: a denied
            // combo never enters the registry, so the FSM-apply capture site can't see it — without
            // this a strategy would never learn its combo was vetoed. Routed by (venue, FIRST leg
            // symbol), CONSISTENTLY: the combo's own `req.symbol` is EMPTY by design (`build_combo`
            // leaves it for the adapter to resolve at submit), and the first leg is the spec's
            // defining leg — the mount trading it is the one that armed the combo. Gated on the
            // same mount flag as `applied_fills`.
            {
                let eng = self.eng_mut(eidx);
                if eng.collect_applied_fills {
                    eng.order_events.push(vike_exec::execution_engine::OrderEventOut {
                        venue: req.venue.clone(),
                        symbol: req
                            .combo_legs
                            .first()
                            .map(|l| l.symbol.clone())
                            .unwrap_or_default(),
                        event: vike_model::strategy::OrderLifecycle {
                            client_order_id: coid.clone(),
                            // Stamped at DELIVERY (`dispatch_order_events`) if this coid holds a
                            // tag entry, never here — one spelling of the rule. A combo leg is
                            // not a tagged quote, so in practice this stays `None`.
                            tag: None,
                            kind: vike_model::strategy::OrderEventKind::Denied {
                                reason: verdict.reason.clone(),
                            },
                        },
                    });
                }
            }
            // A denied combo never enters the registry — mirroring `gate_and_register`, where a
            // vetoed order is published as denied and dropped without registration.
            self.publish_to(
                eidx,
                Event::OrderDenied(vike_model::events::OrderDenied {
                    client_order_id: coid,
                    reason: verdict.reason.into(),
                    ts: now,
                }),
            );
            self.pump_client();
            return Vec::new();
        };

        // Mirrors `ExecutionEngine::submit_order`'s tail (register + `client.submit`), open-coded
        // because the crossing above is `check_combo` — calling `submit_order` here would re-run
        // the SINGLE-order `check`, which prices the request off its SIGNED net (negative for a
        // credit structure, so every price-based limit inverts) and would burn a SECOND throttle
        // slot for one order.
        let eng = self.eng_mut(eidx);
        // ADMIT every leg symbol into this engine's venue-event scope BEFORE the order is on the
        // wire: `ExecutionEngine::on_event` gates a bare `Event::Fill` on `accepts_symbol` (the
        // account-wide-WS scoping filter), and a combo's leg fills arrive carrying LEG symbols —
        // the combo has no symbol of its own. Without this the `OrderFilled` wraps advance the
        // FSM (they route by coid) while the `Account` never folds the fills and
        // `Strategy::on_fill` never fires — the strategy is blind to its own filled combo.
        // `extra_symbols` (the Phase D multi-symbol mechanism) stays the PRIMARY admission
        // because it also lets Funding/PositionLiquidated events on the leg symbols fold — leg
        // positions are REAL positions with real funding, and a fill-only route cannot carry
        // those. Since combo gate 4, `ExecutionEngine::owns_fill_symbol` ALSO routes a bare fill
        // whose coid names a registered order and whose symbol is that order's own symbol or one
        // of its `combo_legs` — the safety net for fills, and the ONLY route that classifies the
        // venue's combo-instrument net-price print (deliberately NOT folded — a phantom position;
        // see that fn's doc + the deribit fill probe) — so the two mechanisms are complementary,
        // not alternatives. Replay-safe: the journaled `Cmd` re-runs this lowering on replay, and
        // `extra_symbols` rides the `EngineSnapshot`.
        for leg in &admitted.combo_legs {
            if !eng.accepts_symbol(&leg.symbol) {
                eng.extra_symbols.push(leg.symbol.clone());
            }
        }
        let mut mo = vike_exec::ManagedOrder::new(admitted.clone());
        mo.created_ms = Some(now);
        eng.registry.insert(coid.clone(), mo);
        eng.client.submit(&admitted);
        self.pump_client();
        vec![coid]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_exec::testing::RecordingClient;
    use vike_exec::MarkSource;
    use vike_exec::{
        Account, BalanceMode, ConditionalIntent, OrderIntent, RiskGate, RiskLimits, TradingState,
    };
    use vike_model::OrderRequest;

    fn test_core() -> CoreThread<RecordingClient> {
        test_core_with(RecordingClient::default(), Vec::new())
    }

    /// `test_core` over an arbitrary client + optional EXTRA engines — needed by the mid-expansion
    /// fill regression (a client that yields queued events on `poll_events`) and the multi-engine
    /// MarketExit walk.
    fn test_core_with<C: ExecutionClient>(
        client: C,
        extra: Vec<(f64, ExecutionEngine<C>)>,
    ) -> CoreThread<C> {
        let engine = ExecutionEngine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            client,
            "sim",
            "BTCUSDT",
        );
        let market = Arc::new(Conflated {
            state: Mutex::new(ConflatedState::default()),
            drops: AtomicU64::new(0),
        });
        let snapshot =
            Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
        assemble_core(
            engine,
            extra,
            CoreConfig::default(),
            market,
            snapshot,
            Arc::new(AtomicU64::new(0)),
        )
    }

    /// `test_core` over an explicit [`CoreConfig`] — the sibling-cancel knob test flips
    /// `oco_cancel_sibling_on_dead_exit` on; every other field stays at its inert default.
    fn test_core_cfg(config: CoreConfig) -> CoreThread<RecordingClient> {
        let engine = ExecutionEngine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        let market = Arc::new(Conflated {
            state: Mutex::new(ConflatedState::default()),
            drops: AtomicU64::new(0),
        });
        let snapshot =
            Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
        assemble_core(engine, Vec::new(), config, market, snapshot, Arc::new(AtomicU64::new(0)))
    }

    fn extra_engine(venue: &str, symbol: &str) -> ExecutionEngine<QueuedEventClient> {
        ExecutionEngine::new(
            Account::new(1.0, venue, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            QueuedEventClient::default(),
            venue,
            symbol,
        )
    }

    /// A `RecordingClient` whose `poll_events` drains a queue the TEST seeds. That is the whole
    /// point: it lets a fill be made to land INSIDE the mass-cancel's own `pump_client()`, i.e.
    /// after the operator hit the panic button and before the flatten legs are derived.
    #[derive(Debug, Default)]
    struct QueuedEventClient {
        submissions: Vec<OrderRequest>,
        cancels: Vec<String>,
        pending: std::collections::VecDeque<vike_model::events::Event>,
    }

    impl vike_exec::ExecutionClient for QueuedEventClient {
        fn poll_events(&mut self) -> Option<vike_model::events::Event> {
            self.pending.pop_front()
        }
        fn submit(&mut self, request: &OrderRequest) {
            self.submissions.push(request.clone());
        }
        fn cancel(&mut self, client_order_id: &str) {
            self.cancels.push(client_order_id.to_string());
        }
    }

    /// The venue-event sequence a real fill arrives as, for `coid` on (venue, symbol).
    ///
    /// ⚠ `OrderSubmitted` FIRST IS LOAD-BEARING, not decoration. The FSM's table is
    /// `Initialized --OrderSubmitted--> Submitted --OrderAccepted--> Accepted --OrderFilled-->
    /// Filled`, so without the first hop `OrderAccepted` is ILLEGAL from `Initialized`, the order
    /// never leaves `Initialized`, and the closing `OrderFilled` is illegal too — the whole
    /// sequence is refused and `dropped_terminal_on_live` moves.
    ///
    /// This helper used to omit it, and the bracket/OCO/OTO tests below still passed — because the
    /// contingency drive ran off the EVENT rather than off the fold's verdict, so it armed and
    /// cancelled legs for a sequence the engine had rejected end to end. Gating the drive on
    /// `Fold::Applied` turned all nine of them red at once, which is how the gap in this fixture was
    /// found. `RecordingClient` emits nothing of its own, so the sequence has to be spelled here in
    /// full; every REAL adapter emits `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously
    /// at submit (the emitter split — see `vike_binance::exec`/`vike_aster::spot` and the paper
    /// exchange's own `submit`), so this now matches what a venue actually delivers.
    fn fill_events(
        coid: &str,
        venue: &str,
        symbol: &str,
        side: i32,
        qty: f64,
        px: f64,
    ) -> Vec<vike_model::events::Event> {
        use vike_model::events::*;
        let fill = FillEvent {
            // minted by this helper, not read off a wire — same `t-<coid>` bytes as before
            trade_id: TradeId::prefixed("t-", coid),
            client_order_id: coid.to_string(),
            venue: venue.into(),
            symbol: symbol.into(),
            side,
            last_qty: qty,
            last_px: px,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "maker".to_string().into(),
            ts: 0,
            mark_price: Some(px),
            position_side: "BOTH".into(),
        };
        vec![
            // Initialized -> Submitted. See this fn's doc: omitting this made every later hop
            // illegal, and the bracket tests only passed because the drive ignored the verdict.
            Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts: 0 }),
            Event::OrderAccepted(OrderAccepted {
                client_order_id: coid.to_string(),
                venue_order_id: Some(format!("v-{coid}").into()),
                ts: 0,
            }),
            Event::Fill(fill.clone()),
            Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts: 0 }),
        ]
    }

    fn market_req(coid: &str) -> Box<OrderRequest> {
        Box::new(OrderRequest {
            client_order_id: coid.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "market".into(),
            ..Default::default()
        })
    }

    // Bar has no Default and carries funding/bid/ask/symbol beyond OHLCV — build it in full.
    fn mk_bar(ts: i64, low: f64, close: f64) -> vike_model::Bar {
        vike_model::Bar {
            ts,
            open: 100.0,
            high: 100.0,
            low,
            close,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    #[test]
    fn submit_empty_coid_is_minted_nonempty_respected() {
        let mut c = test_core();
        let minted = c.apply_intent(OrderIntent::Submit(market_req("")), 0);
        assert_eq!(minted.len(), 1);
        assert!(!minted[0].is_empty(), "empty coid must be minted");
        assert_eq!(c.engine.client.submissions[0].client_order_id, minted[0]);

        let kept = c.apply_intent(OrderIntent::Submit(market_req("mine")), 0);
        assert_eq!(kept, vec!["mine".to_string()]);
        assert_eq!(c.engine.client.submissions[1].client_order_id, "mine");
    }

    #[test]
    fn halted_gate_denies_submit_no_client_call() {
        let mut c = test_core();
        c.engine.trading_state = TradingState::Halted;
        c.apply_intent(OrderIntent::Submit(market_req("c1")), 0);
        assert!(
            c.engine.client.submissions.is_empty(),
            "RiskGate veto: nothing reaches the client"
        );
    }

    #[test]
    fn bracket_mints_three_linked_coids_but_holds_the_exits() {
        // Live-runtime OTO/OCO: a bracket still mints THREE linked coids, but only the OTO ENTRY
        // goes live to the venue — its protective stop-loss / take-profit are HELD off the venue
        // (the emulation) until the entry fills, so a naked exit can never trigger first.
        let mut c = test_core();
        let spec = vike_model::BracketSpec {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 2.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
        };
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
        assert_eq!(coids.len(), 3, "three coids: entry + stop-loss + take-profit");
        assert_eq!(c.engine.client.submissions.len(), 1, "only the OTO entry reaches the venue");
        let venue_entry = &c.engine.client.submissions[0];
        assert_eq!(venue_entry.client_order_id, coids[0]);
        // the venue sees a PLAIN order — the OTO/OCO linkage lives in the CORE's book, not the wire
        assert_eq!(venue_entry.contingency_type, None, "the entry reaches the venue link-free");
        assert!(venue_entry.linked_order_ids.is_empty() && venue_entry.parent_order_id.is_none());
        // the entry IS recorded as an active OTO leg in the core's contingency book
        assert!(!c.contingency.is_empty() && !c.contingency.is_held(&coids[0]));
        // the two exits are held pending the entry fill, not at the venue
        assert_eq!(c.held_orders.len(), 2, "stop-loss + take-profit held");
        assert!(c.held_orders.contains_key(&coids[1]) && c.held_orders.contains_key(&coids[2]));
        assert!(c.contingency.is_held(&coids[1]) && c.contingency.is_held(&coids[2]));
    }

    // ---- live-runtime OTO/OCO drive (submit-hold + fill-drive) ----

    fn bracket_spec() -> vike_model::BracketSpec {
        vike_model::BracketSpec {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 2.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
        }
    }

    /// Fold the [`OrderAccepted`, `Fill`, `OrderFilled`] sequence for `coid` into the core exactly as
    /// a venue delivers a full fill — the `OrderFilled` drives any contingency (`drive_contingency_
    /// on_fill`). Same helper shape the mid-expansion fill test uses (`fill_events`).
    fn feed_full_fill(
        c: &mut CoreThread<RecordingClient>,
        coid: &str,
        side: i32,
        qty: f64,
        px: f64,
    ) {
        for ev in fill_events(coid, "sim", "BTCUSDT", side, qty, px) {
            c.dispatch(Ingest::Event(ev));
        }
    }

    /// Walk `coid` from `Initialized` to `Accepted` exactly as a real adapter does — the emitter
    /// split's synchronous `[OrderSubmitted, OrderAccepted]` pair.
    ///
    /// ⚠ REQUIRED before cancelling/expiring a resting order in these tests. `OrderCanceled` is
    /// legal only from `Accepted`/`Triggered`/`PartiallyFilled`/`PendingCancel` — NEVER from
    /// `Initialized` — and `RecordingClient` emits nothing of its own, so an order it "submitted"
    /// sits at `Initialized` until a test says otherwise. Same fixture gap `fill_events` had: while
    /// the contingency drive ignored the fold's verdict, cancelling an `Initialized` order still
    /// drove the cascade, so the shortcut was invisible.
    fn accept(c: &mut CoreThread<RecordingClient>, coid: &str) {
        c.dispatch(Ingest::Event(Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            client_order_id: coid.to_string(),
            ts: 0,
        })));
        c.dispatch(Ingest::Event(Event::OrderAccepted(vike_model::events::OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: Some(format!("v-{coid}").into()),
            ts: 0,
        })));
    }

    #[test]
    fn oto_entry_fill_releases_both_held_exits() {
        let mut c = test_core();
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
        assert_eq!(c.engine.client.submissions.len(), 1, "only the entry is live pre-fill");
        // OTO: the entry's fill arms + releases both protective exits to the venue.
        feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
        let submitted: Vec<String> =
            c.engine.client.submissions.iter().map(|r| r.client_order_id.clone()).collect();
        assert!(
            submitted.contains(&sl) && submitted.contains(&tp),
            "both exits released: {submitted:?}"
        );
        assert!(c.held_orders.is_empty(), "nothing left held once the parent filled");
        assert!(
            !c.contingency.is_held(&sl) && !c.contingency.is_held(&tp),
            "the released exits are armed (active) in the book"
        );
    }

    #[test]
    fn oco_stop_fill_cancels_the_take_profit() {
        let mut c = test_core();
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
        feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms sl + tp
        feed_full_fill(&mut c, &sl, -1, 2.0, 95.0); // OCO direction 1: the stop fills
        assert!(c.engine.client.cancels.contains(&tp), "the stop's fill cancels the take-profit");
        assert!(!c.engine.client.cancels.contains(&sl), "the filled leg is never self-canceled");
        assert!(c.contingency.is_empty(), "the whole group resolved");
    }

    #[test]
    fn oco_take_profit_fill_cancels_the_stop() {
        let mut c = test_core();
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
        feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms sl + tp
        feed_full_fill(&mut c, &tp, -1, 2.0, 110.0); // OCO direction 2: the take-profit fills
        assert!(c.engine.client.cancels.contains(&sl), "the take-profit's fill cancels the stop");
        assert!(c.contingency.is_empty(), "the whole group resolved");
    }

    // ---- ADVERSARIAL: the contingency drive under a HOSTILE venue --------------------------
    //
    // The threat model and the conventions for this class of test are stated once, in
    // `crates/vike-exec/tests/engine/hostile_venue_fold.rs`'s module doc. In short: TLS is verified, so
    // this is not a man-in-the-middle — the actor is the VENUE ITSELF, returning well-formed frames
    // with fabricated contents.
    //
    // These are the adversarial twins of the legitimate drive tests directly above. The property is
    // narrow and total: an event the ENGINE DROPPED must drive NOTHING. Before the `Fold` verdict
    // gated the drive, `contingency_terminal` classified the event ALONE, so the core law "invalid
    // transitions are dropped" did not extend to the bracket/OCO/OTO machinery at all.

    /// The bare `FillEvent` out of `fill_events` for `coid` — the wrap's embedded copy.
    fn bare_fill(coid: &str, side: i32, qty: f64, px: f64) -> vike_model::events::FillEvent {
        fill_events(coid, "sim", "BTCUSDT", side, qty, px)
            .into_iter()
            .find_map(|e| match e {
                Event::Fill(f) => Some(f),
                _ => None,
            })
            .expect("fill_events yields a bare Fill")
    }

    // ⚠ WHERE THE ASYMMETRY ACTUALLY IS — this is what makes the drive reachable with a coid the
    // FSM refuses, and the first two attempts at these tests were VACUOUS for missing it.
    //
    // A bracket's protective exits are HELD: `apply_intent` puts them in `held_orders` + the
    // contingency book and deliberately never calls `submit_order` for them, so they are NOT in
    // `ExecutionEngine::registry`. That is the gap: the CONTINGENCY BOOK knows those coids while the
    // ORDER REGISTRY does not. A fabricated terminal naming a held exit is therefore dropped by the
    // fold (`dropped_unknown_coid` moves) AND was still driven by the contingency machinery.
    //
    // Forging a coid that exists in NEITHER (`"{tp}-FORGED"`) proves nothing — `on_fill` on an
    // unknown coid is a no-op whether or not the gate is there, so such a test passes with the fix
    // reverted. Always mutation-check an adversarial test; a green one may simply be inert.

    /// A fabricated `OrderFilled` naming the HELD take-profit. The FSM refuses it (that coid was
    /// never registered), but the contingency book knows it — so before the gate it ran the OCO
    /// sibling-cancel and **silently destroyed the held STOP-LOSS**, leaving the bracket with no
    /// downside protection to release when the entry eventually fills.
    #[test]
    fn a_fabricated_fill_on_a_held_exit_does_not_destroy_its_oco_sibling() {
        let mut c = test_core();
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        let (sl, tp) = (coids[1].clone(), coids[2].clone());
        assert_eq!(c.held_orders.len(), 2, "precondition: both exits held");
        assert!(!c.engine.registry.contains_key(&tp), "precondition: a held exit is UNREGISTERED");

        c.dispatch(Ingest::Event(Event::OrderFilled(vike_model::events::OrderFilled {
            client_order_id: tp.clone(),
            fill: bare_fill(&tp, -1, 2.0, 110.0),
            ts: 1,
        })));

        assert!(c.engine.dropped_unknown_coid > 0, "the fold must have REFUSED the forged fill");
        assert!(
            c.held_orders.contains_key(&sl),
            "the STOP-LOSS must survive a refused fill — losing it leaves the bracket unprotected"
        );
        assert!(c.held_orders.contains_key(&tp), "and the take-profit too");
        assert_eq!(c.held_orders.len(), 2, "the group is untouched");
        assert!(c.contingency.is_held(&sl) && c.contingency.is_held(&tp), "book untouched");
    }

    /// The terminal-without-fill half of the same hole: a fabricated `OrderCanceled` naming a held
    /// exit ran `drive_contingency_on_terminal`, which drops that leg from the held map AND the
    /// book — so the take-profit simply VANISHES and is never released when the entry fills.
    #[test]
    fn a_fabricated_cancel_on_a_held_exit_does_not_remove_it_from_the_bracket() {
        let mut c = test_core();
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());

        c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
            client_order_id: tp.clone(),
            reason: "forged".to_string().into(),
            ts: 1,
        })));

        assert!(c.engine.dropped_unknown_coid > 0, "the fold must have REFUSED the forged cancel");
        assert!(c.held_orders.contains_key(&tp), "the take-profit must NOT be dropped");
        assert!(c.contingency.is_held(&tp), "and must still be in the book");

        // ...and it is still there to be released when the entry genuinely fills.
        feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
        let submitted: Vec<String> =
            c.engine.client.submissions.iter().map(|r| r.client_order_id.clone()).collect();
        assert!(submitted.contains(&tp), "the take-profit still releases: {submitted:?}");
        assert!(submitted.contains(&sl), "and so does the stop-loss");
    }

    /// THE MUTATION SENTINEL for the three tests above: a gate that suppressed EVERYTHING would
    /// pass all of them and fail only this. An event the engine ACCEPTS must drive exactly what it
    /// drove before — the fix is "a dropped event drives nothing", never "drive less".
    #[test]
    fn an_accepted_terminal_still_drives_the_contingency_exactly_as_before() {
        let mut c = test_core();
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());

        // OTO: a REAL entry fill still releases both held exits to the venue.
        feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
        let submitted: Vec<String> =
            c.engine.client.submissions.iter().map(|r| r.client_order_id.clone()).collect();
        assert!(submitted.contains(&sl) && submitted.contains(&tp), "both released: {submitted:?}");
        assert!(c.held_orders.is_empty(), "nothing left held");

        // OCO: a REAL stop fill still cancels the take-profit.
        feed_full_fill(&mut c, &sl, -1, 2.0, 95.0);
        assert!(c.engine.client.cancels.contains(&tp), "the real fill still cancels the sibling");
        assert!(c.contingency.is_empty(), "the group still resolves");
    }

    #[test]
    fn oto_entry_terminating_unfilled_drops_the_held_exits() {
        let mut c = test_core();
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        let entry = coids[0].clone();
        assert_eq!(c.held_orders.len(), 2, "sl + tp held");
        // The entry is canceled before it ever fills — its held children can never arm, so the
        // cascade drops them (they were never at the venue, so nothing to cancel there). It has to
        // REACH the venue first: a cancel is only legal on an accepted order (see `accept`).
        accept(&mut c, &entry);
        c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
            client_order_id: entry,
            reason: "user".to_string().into(),
            ts: 0,
        })));
        assert!(c.held_orders.is_empty(), "held exits dropped when the parent terminates unfilled");
        assert!(c.contingency.is_empty(), "no orphaned linkage left behind");
    }

    #[test]
    fn plain_order_is_inert_no_contingency_state_or_cancels() {
        let mut c = test_core();
        // a link-free order: submitted straight through, nothing enters the contingency book.
        let coids = c.apply_intent(OrderIntent::Submit(market_req("p1")), 0);
        assert_eq!(coids, vec!["p1".to_string()]);
        assert_eq!(c.engine.client.submissions.len(), 1, "plain order goes live immediately");
        assert!(c.contingency.is_empty() && c.held_orders.is_empty(), "no contingency state");
        // its fill drives nothing (the byte-identical no-bracket path).
        feed_full_fill(&mut c, "p1", 1, 1.0, 100.0);
        assert!(c.engine.client.cancels.is_empty(), "a plain fill cancels nothing");
        assert!(c.contingency.is_empty());
    }

    #[test]
    fn denied_bracket_entry_drops_its_held_exits_not_orphans_them() {
        // The CRITICAL leak: a bracket entry vetoed by the RiskGate (synchronous `OrderDenied`)
        // must cascade-drop its held exits. Before the fix they were orphaned forever — never armed
        // (no fill ever comes), never removed, re-captured in every Snap.
        let mut c = test_core();
        c.engine.trading_state = TradingState::Halted; // the RiskGate vetoes every new order
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        assert_eq!(coids.len(), 3, "coids are still minted+returned");
        assert!(c.engine.client.submissions.is_empty(), "Halted: nothing reaches the venue");
        assert!(
            c.held_orders.is_empty(),
            "the denied entry's held exits are DROPPED, not orphaned"
        );
        assert!(c.contingency.is_empty(), "no orphaned contingency linkage survives the denial");
    }

    #[test]
    fn cancel_a_held_bracket_exit_drops_it_and_never_hits_the_venue() {
        let mut c = test_core();
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        let (sl, tp) = (coids[1].clone(), coids[2].clone());
        assert_eq!(c.held_orders.len(), 2);
        c.apply_intent(OrderIntent::Cancel(sl.clone()), 0);
        assert!(
            !c.held_orders.contains_key(&sl) && !c.contingency.contains(&sl),
            "the canceled held exit is gone from both the held map and the book"
        );
        assert!(c.held_orders.contains_key(&tp), "its sibling stays held");
        assert!(c.engine.client.cancels.is_empty(), "a held cancel never reaches the venue");
    }

    #[test]
    fn modify_a_held_bracket_exit_updates_the_terms_it_is_released_with() {
        let mut c = test_core();
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        let (entry, tp) = (coids[0].clone(), coids[2].clone());
        c.apply_intent(
            OrderIntent::Modify {
                client_order_id: tp.clone(),
                new_qty: Some(5.0),
                new_price: Some(115.0),
            },
            0,
        );
        let held = c.held_orders.get(&tp).expect("still held");
        assert_eq!(held.qty, 5.0);
        assert_eq!(held.price, Some(115.0));
        // and those updated terms are exactly what get released to the venue when the entry fills
        feed_full_fill(&mut c, &entry, 1, 2.0, 100.0);
        let released = c
            .engine
            .client
            .submissions
            .iter()
            .find(|r| r.client_order_id == tp)
            .expect("the take-profit was released");
        assert_eq!(released.qty, 5.0, "the release carries the modified qty");
        assert_eq!(released.price, Some(115.0), "and the modified price");
    }

    #[test]
    fn a_released_exit_dying_unfilled_keeps_the_surviving_sibling() {
        // KNOB OFF (the default): a protective exit that dies unfilled (venue reject/cancel/expire)
        // cleans its OWN stale book entry, but the surviving OCO sibling is KEPT — a position that
        // just lost one protective leg should retain whatever protection it still has. This is the
        // inert default of `CoreConfig::oco_cancel_sibling_on_dead_exit` (`test_core` builds a
        // default config); the knob-ON twin below flips it.
        let mut c = test_core();
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
        feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms + releases sl + tp
        accept(&mut c, &sl); // the released stop-loss reaches the venue and rests there
                             // the venue cancels the released stop-loss without it ever filling
        c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
            client_order_id: sl.clone(),
            reason: "venue".to_string().into(),
            ts: 0,
        })));
        assert!(!c.contingency.contains(&sl), "the dead exit's own book entry is cleaned");
        assert!(
            c.contingency.contains(&tp),
            "the surviving take-profit is KEPT (protection stays)"
        );
        assert!(!c.engine.client.cancels.contains(&tp), "the sibling is NOT auto-canceled");
    }

    #[test]
    fn a_released_exit_dying_unfilled_cancels_the_sibling_when_the_knob_is_on() {
        // KNOB ON (`CoreConfig::oco_cancel_sibling_on_dead_exit = true`): the same dead released
        // stop-loss now ALSO cancels its surviving OCO take-profit AND cleans its book entry — the
        // fully-flat-book deployment choice. Everything up to the death is identical to the OFF
        // twin above; only the sibling's fate differs.
        let mut c = test_core_cfg(CoreConfig {
            oco_cancel_sibling_on_dead_exit: true,
            ..Default::default()
        });
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(bracket_spec())), 0);
        let (entry, sl, tp) = (coids[0].clone(), coids[1].clone(), coids[2].clone());
        feed_full_fill(&mut c, &entry, 1, 2.0, 100.0); // arms + releases sl + tp to the venue
        accept(&mut c, &sl); // the released stop-loss reaches the venue and rests there
                             // the venue cancels the released stop-loss without it ever filling
        c.dispatch(Ingest::Event(Event::OrderCanceled(vike_model::events::OrderCanceled {
            client_order_id: sl.clone(),
            reason: "venue".to_string().into(),
            ts: 0,
        })));
        assert!(!c.contingency.contains(&sl), "the dead exit's own book entry is cleaned");
        assert!(
            !c.contingency.contains(&tp),
            "knob ON: the surviving take-profit is CANCELED and cleaned from the book"
        );
        assert!(
            c.engine.client.cancels.contains(&tp),
            "knob ON: the surviving OCO sibling is canceled at the venue"
        );
        assert!(c.contingency.is_empty(), "the whole group resolved — the book is left flat");
    }

    /// The two-leg call spread every combo test below is built from. `venue`/`symbol`s match the
    /// `test_core` engine ("sim") so the per-leg mark/position lookups actually resolve.
    fn combo_spec(qty: f64) -> vike_model::ComboSpec {
        vike_model::ComboSpec {
            venue: "sim".into(),
            side: 1,
            qty,
            legs: vec![
                vike_model::ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
                vike_model::ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
            ],
            net_limit: Some(-0.0125), // a CREDIT combo, the awkward case
            time_in_force: vike_model::TimeInForce::Gtc,
        }
    }

    /// Give both legs of [`combo_spec`] a real mark on the primary engine. `check_combo` DENIES a
    /// leg with no mark (`leg <sym>: no-mark`), so every admitted-path test needs this. The combo
    /// gate's per-leg reference is resolver-priced, so feed BOTH the `Account.marks` scalar and the
    /// price board — the same pair every live mark write-site writes; a fresh board price equal to
    /// the scalar keeps every admitted-path verdict byte-identical.
    fn mark_combo_legs<C: ExecutionClient>(c: &mut CoreThread<C>, px: f64) {
        for leg in combo_spec(1.0).legs {
            c.engine.account.set_mark_from("sim", &leg.symbol, px, MarkSource::VenueMark, 0);
            c.engine.price_board.set_mark("sim", &leg.symbol, px, 0);
        }
    }

    #[test]
    fn combo_on_unsupported_venue_gets_a_terminal_reject_never_a_silent_drop() {
        // The emitter-split contract: no order may vanish. A venue that cannot take a combo must
        // still produce a full terminal lifecycle locally, because no venue client will.
        let mut c = test_core();
        mark_combo_legs(&mut c, 100.0);
        let before_seq = c.coid_gen.state().1;

        let coids = c.lower_combo(combo_spec(2.0), 7, false, EngineRoute::Payload);

        assert!(coids.is_empty(), "nothing reached the venue, so no coid is returned");
        assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
        assert_eq!(c.coid_gen.state().1, before_seq + 1, "ONE coid is minted for the combo");
        // the order exists and is TERMINAL — not dropped
        assert_eq!(c.engine.registry.len(), 1);
        let mo = c.engine.registry.values().next().unwrap();
        assert_eq!(mo.status, vike_exec::OrderStatus::Rejected, "must reach a TERMINAL state");
        assert!(c.recent.back().unwrap().contains("no combo support"));
    }

    #[test]
    fn combo_passing_the_gate_mints_exactly_one_coid_and_submits() {
        // ONE coid for the WHOLE combo — not one per leg — carrying both legs on one request.
        let mut c = test_core();
        mark_combo_legs(&mut c, 100.0);
        let before_seq = c.coid_gen.state().1;

        let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);

        assert_eq!(coids.len(), 1, "ONE coid for the whole combo, never one per leg");
        assert_eq!(c.coid_gen.state().1, before_seq + 1, "exactly one mint");
        assert_eq!(c.engine.client.submissions.len(), 1, "ONE order reaches the venue");
        let sent = &c.engine.client.submissions[0];
        assert_eq!(sent.client_order_id, coids[0]);
        assert_eq!(sent.combo_legs.len(), 2, "both legs ride the one request");
        // the SIGNED net limit rides through verbatim — never absolute-valued, never clamped
        assert_eq!(sent.price, Some(-0.0125));
        assert!(c.engine.registry.contains_key(&coids[0]), "registered as ONE ManagedOrder");
    }

    #[test]
    fn combo_denied_by_the_gate_emits_order_denied_and_submits_nothing() {
        // A combo veto must surface exactly as a single-order veto does: an `OrderDenied` event,
        // nothing on the wire, and no registry entry.
        let mut c = test_core();
        mark_combo_legs(&mut c, 100.0);
        c.engine.trading_state = TradingState::Halted; // the gate's kill switch precedes all else
        let denied_before = c.engine.dropped_unknown_coid;

        let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);

        assert!(coids.is_empty(), "a denied combo returns no coid");
        assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
        assert!(c.engine.registry.is_empty(), "a denied combo never enters the registry");
        assert!(c.recent.back().unwrap().contains("combo DENIED"));
        // the OrderDenied really was published (an unregistered coid is counted as it routes)
        assert!(c.engine.dropped_unknown_coid > denied_before, "OrderDenied was published");
        // the denied path stamps the engine clock BEFORE the gate, like the single-order path
        // (adversarial review, minor #3)
        assert_eq!(c.engine.now_ms, 7, "a denied combo must not leave now_ms stale");
    }

    #[test]
    fn combo_legs_accumulate_so_individually_affordable_legs_are_collectively_denied() {
        // THE point of the whole PR: #453 built leg-by-leg ACCUMULATION (each admitted leg's
        // initial margin is threaded onto the next leg's `margin_used`) precisely so N legs cannot
        // each fit inside the same unchanged free buying power. That behavior only means something
        // once a production caller supplies real per-symbol facts — this test proves it does.
        //
        // Sized so ONE leg fits the account and TWO do not: equity 1000, 100% IM, mark 100,
        // multiplier 1, qty 6 ⇒ each leg needs 6 × 100 × 1 × 1.0 = 600. Leg 1 is admitted
        // (600 <= 1000 free); leg 2 then sees margin_used 600, i.e. only 400 free against another
        // 600 ⇒ the COMBO is denied. (`Account::new`'s first argument is the contract MULTIPLIER,
        // not cash — the account's equity comes from `equity_seed` below.)
        let mut limits = RiskLimits::new();
        limits.im_requirement = Some(1.0);
        let engine = ExecutionEngine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(limits),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        let market = Arc::new(Conflated {
            state: Mutex::new(ConflatedState::default()),
            drops: AtomicU64::new(0),
        });
        let snapshot =
            Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
        let mut c = assemble_core(
            engine,
            Vec::new(),
            CoreConfig::default(),
            market,
            snapshot,
            Arc::new(AtomicU64::new(0)),
        );
        c.engine.equity_seed = 1000.0;
        mark_combo_legs(&mut c, 100.0);

        let coids = c.lower_combo(combo_spec(6.0), 7, true, EngineRoute::Payload);

        assert!(coids.is_empty(), "legs that individually fit must COLLECTIVELY be denied");
        assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
        let reason = c.recent.back().unwrap();
        assert!(reason.contains("combo DENIED"), "unexpected refusal: {reason}");
        // the SECOND leg is the one that breaks the budget — proving leg 1 was admitted first and
        // its commitment was carried forward, which is the accumulation contract itself.
        assert!(
            reason.contains("BTC-27MAR26-120000-C"),
            "the SECOND leg must be the one denied (accumulation), got: {reason}"
        );

        // And the same combo at a size BOTH legs fit (qty 2 ⇒ 200 each, 400 total <= 1000) passes,
        // so the denial above is a budget verdict rather than a blanket combo refusal.
        let ok = c.lower_combo(combo_spec(2.0), 8, true, EngineRoute::Payload);
        assert_eq!(ok.len(), 1, "a combo that fits in aggregate is admitted");
    }

    /// REGRESSION (adversarial review, MAJOR #1). The `margin_used` baseline must price a MARKED
    /// open position with NO per-symbol IM override exactly as `gate_and_register` does — falling
    /// back to the priced symbol's own `im_req` — never silently skipping it. The divergence arms
    /// exactly when the global `im_requirement` is None and margin was armed per-symbol, which is
    /// precisely what `Command::SetMargin` produces (it only writes `im_by_symbol`): the old skip
    /// saw the whole equity as free and ADMITTED a combo whose naked legs would be DENIED —
    /// violating the gate's own invariant ("a combo must never pass a gate its naked legs would
    /// fail").
    #[test]
    fn combo_margin_baseline_counts_no_override_positions_like_the_single_path() {
        let mut c = test_core();
        c.engine.equity_seed = 1000.0;
        // margin armed PER-SYMBOL only (Command::SetMargin's exact shape): global im stays None
        for leg in combo_spec(1.0).legs {
            c.engine.gate.limits.im_by_symbol.insert(leg.symbol, 1.0);
        }
        mark_combo_legs(&mut c, 100.0);
        // a MARKED open position on a FOREIGN symbol with NO per-symbol IM override:
        // 9 × 100 × 1, priced at the order/leg symbol's fallback rate 1.0 ⇒ 900 of the 1000
        // equity is already spoken for (mark == avg_px, so equity stays exactly 1000)
        c.engine.account.positions.insert(
            ("sim".into(), "FOREIGN".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 9.0, avg_px: 100.0, ..Default::default() },
        );
        c.engine.account.set_mark_from("sim", "FOREIGN", 100.0, MarkSource::VenueMark, 0);
        // the margin fold is resolver-priced now — feed the board the same price the live
        // write-sites would store alongside `account.set_mark`
        c.engine.price_board.set_mark("sim", "FOREIGN", 100.0, 0);

        // the NAKED leg is denied by the single-order path: free = 1000 − 900 = 100 while the
        // leg needs 2 × 100 × 1.0 = 200 (explicit limit price: the single-symbol engine's
        // `mark()` prices off the MOUNTED symbol, which this test never marks)
        let leg1 = combo_spec(1.0).legs[0].symbol.clone();
        c.apply_intent(
            OrderIntent::Submit(Box::new(OrderRequest {
                client_order_id: "naked".into(),
                venue: "sim".into(),
                symbol: leg1.clone(),
                side: 1,
                qty: 2.0,
                order_type: "limit".into(),
                price: Some(100.0),
                ..Default::default()
            })),
            0,
        );
        assert!(
            c.engine.client.submissions.is_empty(),
            "precondition: the naked leg is DENIED by the single-order path"
        );

        // CONSISTENCY, asserted directly: the combo carrying that same leg must be denied too.
        // (The old skip priced FOREIGN at 0, saw 1000 free, and admitted BOTH legs.)
        let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
        assert!(coids.is_empty(), "the combo must fail exactly where its naked leg fails");
        assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
        let reason = c.recent.back().unwrap();
        assert!(
            reason.contains(&leg1) && reason.contains("insufficient-margin"),
            "the leg must fail the margin check against the foreign position, got: {reason}"
        );
    }

    /// MAJOR-3 (the liquidation law's partition in the COMBO leg baseline): an ISOLATED open
    /// position is backed by its own walled-off wallet, not the shared equity the combo is
    /// admitted against, so it must no longer inflate the per-leg `margin_used` baseline. The
    /// scenario is the test above with FOREIGN flipped Isolated: counted (the old fold) it
    /// spoke for 900 of the 1000 equity and DENIED the combo; excluded, the combo fits with
    /// room to spare and is ADMITTED. Cross books are untouched (the test above still denies).
    #[test]
    fn combo_margin_baseline_excludes_isolated_positions() {
        let mut c = test_core();
        c.engine.equity_seed = 1000.0;
        for leg in combo_spec(1.0).legs {
            c.engine.gate.limits.im_by_symbol.insert(leg.symbol, 1.0);
        }
        mark_combo_legs(&mut c, 100.0);
        // the SAME foreign position as the consistency test above — 9 × 100 × 1.0 = 900 if
        // counted — but ISOLATED with its own wallet: it never consumes the shared equity.
        c.engine.account.positions.insert(
            ("sim".into(), "FOREIGN".into(), "BOTH".into()),
            vike_exec::PositionEntry {
                size: 9.0,
                avg_px: 100.0,
                margin_mode: vike_model::MarginMode::Isolated,
                isolated_margin: Some(900.0),
            },
        );
        c.engine.account.set_mark_from("sim", "FOREIGN", 100.0, MarkSource::VenueMark, 0);

        // both legs need 2 × 100 × 1.0 = 200 each; baseline 0 + accumulation 200 → 400 of the
        // 1000 equity → ADMITTED. (Counted at 900, leg 1 alone would already fail: 100 free.)
        let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
        assert_eq!(
            coids.len(),
            1,
            "an isolated position must not consume the combo's shared margin baseline: {:?}",
            c.recent.back()
        );
        assert_eq!(c.engine.client.submissions.len(), 1, "the admitted combo reaches the venue");
    }

    /// The combo twin of #550's `gate_exposure_reads_the_resolver_not_the_stale_mark`: a combo
    /// leg's projected-exposure REFERENCE used to be the raw `Account.marks` scalar (`mark_of`),
    /// while the SAME crossing's per-leg equity/margin were already resolver-priced. That split let
    /// a stale-LOW scalar UNDER-measure a leg's projected exposure and admit a combo the fresh
    /// board denies — the risk-unsafe direction, and exactly what the single-order gate closed. The
    /// reference now shares the resolver, so the whole crossing speaks ONE price. Long 10 on the
    /// FIRST leg, board fresh at 200, stale `Account.marks` at 100, `max_total_exposure` 2500: a
    /// combo buy 3 projects (10+3)·200 = 2600 > 2500 on that leg → DENY. Under the split basis it
    /// was (10+3)·100 = 1300 → admitted.
    #[test]
    fn combo_exposure_reads_the_resolver_not_the_stale_mark() {
        let mut c = test_core();
        c.engine.gate.limits.max_total_exposure = Some(2500.0);
        let legs = combo_spec(1.0).legs;
        // pre-existing long on the FIRST leg — the one the exposure cap will trip
        c.engine.account.positions.insert(
            ("sim".into(), legs[0].symbol.as_str().into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
        );
        // stale-LOW scalar vs fresh-HIGH board, on BOTH legs (the second leg must also price, or it
        // would `no-mark`-deny under the OLD basis and mask the real verdict)
        for leg in &legs {
            c.engine.account.set_mark_from("sim", &leg.symbol, 100.0, MarkSource::VenueMark, 0);
            c.engine.price_board.set_mark("sim", &leg.symbol, 200.0, 1);
        }

        let coids = c.lower_combo(combo_spec(3.0), 7, true, EngineRoute::Payload);

        assert!(coids.is_empty(), "a fresh board must not be under-measured by a stale mark");
        assert!(c.engine.client.submissions.is_empty(), "nothing reaches the venue");
        let reason = c.recent.back().unwrap();
        assert!(
            reason.contains(&legs[0].symbol) && reason.contains("over-max-exposure"),
            "the first leg must trip the resolver-priced exposure cap, got: {reason}"
        );
    }

    /// Stated as the invariant: the combo exposure verdict is a function of the RESOLVED price
    /// alone — three wildly different `Account.marks` scalars over ONE fresh board all reach the
    /// identical DENY, so the raw scalar is no longer an input (the combo twin of
    /// `the_notional_lane_verdict_is_independent_of_the_mark_scalar`). Under the split basis, stale
    /// 0 and 100 ADMITTED while stale 5_000 denied — the verdict tracked the scalar, not the board.
    #[test]
    fn the_combo_exposure_verdict_is_independent_of_the_mark_scalar() {
        for stale in [0.0, 100.0, 5_000.0] {
            let mut c = test_core();
            c.engine.gate.limits.max_total_exposure = Some(2500.0);
            let legs = combo_spec(1.0).legs;
            c.engine.account.positions.insert(
                ("sim".into(), legs[0].symbol.as_str().into(), "BOTH".into()),
                vike_exec::PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
            );
            for leg in &legs {
                c.engine.account.set_mark_from("sim", &leg.symbol, stale, MarkSource::VenueMark, 0);
                c.engine.price_board.set_mark("sim", &leg.symbol, 200.0, 1);
            }

            let coids = c.lower_combo(combo_spec(3.0), 7, true, EngineRoute::Payload);

            assert!(
                coids.is_empty() && c.engine.client.submissions.is_empty(),
                "stale scalar {stale} changed a resolver-priced combo exposure verdict"
            );
            let reason = c.recent.back().unwrap();
            assert!(
                reason.contains("over-max-exposure"),
                "stale {stale}: expected the board-priced exposure DENY, got: {reason}"
            );
        }
    }

    /// REGRESSION (sibling #457 review, MAJOR — the fix belongs in the lowering). A combo's leg
    /// fills arrive as bare `Event::Fill`s carrying LEG symbols — neither the engine's mounted
    /// symbol nor (before this fix) in `extra_symbols` — so `on_event`'s account-wide-WS symbol
    /// filter DROPPED them: the FSM reached Filled (the wraps route by coid) while the Account
    /// stayed flat and `Strategy::on_fill` never fired. Registration must admit every leg symbol
    /// into the engine's scope.
    #[test]
    fn combo_leg_fills_fold_into_the_account_for_both_legs() {
        use vike_model::events::{Event, FillEvent, OrderAccepted, OrderFilled};
        let mut c = test_core_with(QueuedEventClient::default(), Vec::new());
        mark_combo_legs(&mut c, 100.0);
        c.engine.collect_applied_fills = true; // a strategy is mounted: on_fill delivery matters

        let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
        assert_eq!(coids.len(), 1, "precondition: the combo was gated + registered + submitted");
        let coid = coids[0].clone();

        // the venue's leg-fill stream: ONE coid, one bare Fill PER LEG carrying the LEG symbol
        // (distinct trade_ids — same-id fills are reconnect-deduped), then the terminal wrap
        let legs = combo_spec(2.0).legs;
        // `&'static str`: both call sites pass a source literal, so `TradeId: From<&'static str>`
        let mk_fill = |tid: &'static str, sym: &str, side: i32| FillEvent {
            trade_id: tid.into(),
            client_order_id: coid.clone(),
            venue: "sim".into(),
            symbol: sym.into(),
            side,
            last_qty: 2.0,
            last_px: 10.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            ts: 8,
            mark_price: Some(10.0),
            position_side: "BOTH".into(),
        };
        let f1 = mk_fill("t-leg1", &legs[0].symbol, 1); // ratio +1, combo bought ⇒ buy
        let f2 = mk_fill("t-leg2", &legs[1].symbol, -1); // ratio −1 ⇒ sell
        c.engine.client.pending.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.clone(),
            venue_order_id: Some("v-combo".to_string().into()),
            ts: 8,
        }));
        c.engine.client.pending.push_back(Event::Fill(f1));
        c.engine.client.pending.push_back(Event::Fill(f2.clone()));
        c.engine.client.pending.push_back(Event::OrderFilled(OrderFilled {
            client_order_id: coid,
            fill: f2,
            ts: 8,
        }));
        c.pump_client();

        // the Account gained BOTH leg positions — the whole point of the symbol admission
        assert_eq!(c.engine.position_size_of(&legs[0].symbol, "BOTH"), 2.0);
        assert_eq!(c.engine.position_size_of(&legs[1].symbol, "BOTH"), -2.0);
        // and the strategy actually HEARS its fills
        assert_eq!(c.engine.applied_fills.len(), 2, "on_fill delivery for both leg fills");

        // idempotent: re-registering the same legs must not grow extra_symbols again
        let n = c.engine.extra_symbols.len();
        let again = c.lower_combo(combo_spec(2.0), 9, true, EngineRoute::Payload);
        assert_eq!(again.len(), 1);
        assert_eq!(c.engine.extra_symbols.len(), n, "leg symbols are admitted exactly once");
    }

    /// A hand-built INVALID spec must refuse BEFORE the mint (adversarial review, minor #1): a
    /// burned coid would leave a sequence gap, and a gap is only diagnostic while ids that name
    /// no order stay impossible — the same mint-after-validation rule the `ArmConditional` arm
    /// documents.
    #[test]
    fn combo_invalid_spec_refuses_without_burning_a_coid() {
        let mut c = test_core();
        let before_seq = c.coid_gen.state().1;
        let mut spec = combo_spec(1.0);
        spec.legs.truncate(1); // < 2 legs: the constructor/Deserialize would refuse this
        let coids = c.lower_combo(spec, 7, true, EngineRoute::Payload);
        assert!(coids.is_empty());
        assert_eq!(c.coid_gen.state().1, before_seq, "a refused spec must not burn a coid");
        assert!(c.engine.client.submissions.is_empty());
        assert!(c.recent.back().unwrap().contains("combo REFUSED"));
    }

    /// A combo veto must reach `Strategy::on_order_event` exactly as a single-order veto does
    /// (`gate_and_register` pushes a Denied `OrderEventOut` at its veto site) — routed by the
    /// FIRST leg's symbol, because the combo request's own symbol is EMPTY by design
    /// (adversarial review, minor #2).
    #[test]
    fn combo_denied_captures_an_order_event_for_the_strategy() {
        let mut c = test_core();
        mark_combo_legs(&mut c, 100.0);
        c.engine.collect_applied_fills = true; // a strategy is mounted
        c.engine.trading_state = TradingState::Halted;
        let coids = c.lower_combo(combo_spec(2.0), 7, true, EngineRoute::Payload);
        assert!(coids.is_empty());
        assert_eq!(c.engine.order_events.len(), 1, "the veto must be captured for the strategy");
        let ev = &c.engine.order_events[0];
        assert_eq!(ev.venue, "sim");
        assert_eq!(ev.symbol, combo_spec(1.0).legs[0].symbol, "routed by the FIRST leg's symbol");
        assert!(matches!(
            &ev.event.kind,
            vike_model::strategy::OrderEventKind::Denied { reason } if reason == "halted"
        ));
    }

    #[test]
    fn confirm_routes_to_client_confirm() {
        let mut c = test_core();
        c.apply_intent(
            OrderIntent::Submit(Box::new(OrderRequest {
                client_order_id: "c1".into(),
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 1.0,
                order_type: "limit".into(),
                price: Some(100.0),
                ..Default::default()
            })),
            0,
        );
        c.apply_intent(OrderIntent::Confirm("c1".into()), 0);
        assert_eq!(c.engine.client.confirms, vec!["c1".to_string()]);
    }

    #[test]
    fn flatten_submits_reduce_only_opposite_of_position() {
        let mut c = test_core();
        // seed a +2 long directly (no instant-fill needed); key = (venue, symbol, position_side)
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        let coids = c.apply_intent(
            OrderIntent::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into() },
            0,
        );
        assert_eq!(coids.len(), 1);
        let o = &c.engine.client.submissions[0];
        assert_eq!(o.side, -1, "long +2 ⇒ sell to flatten");
        assert_eq!(o.qty, 2.0);
        assert!(o.reduce_only);
        assert_eq!(o.order_type, "market");

        // flat position ⇒ no order
        let mut c2 = test_core();
        let none = c2.apply_intent(
            OrderIntent::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into() },
            0,
        );
        assert!(none.is_empty());
    }

    #[test]
    fn market_exit_expands_to_mass_cancel_then_flatten_per_open_position() {
        let mut c = test_core();
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        c.engine.account.positions.insert(
            ("sim".into(), "ETHUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: -3.0, avg_px: 50.0, ..Default::default() },
        );
        // a FLAT leftover row (a closed position never leaves the map) must NOT expand
        c.engine.account.positions.insert(
            ("sim".into(), "SOLUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 0.0, avg_px: 10.0, ..Default::default() },
        );
        let intents = c.expand_market_exit(None);
        assert_eq!(intents.len(), 3, "mass-cancel + 2 flattens (the flat row is skipped)");
        assert!(matches!(&intents[0], OrderIntent::MassCancel { venue: None, symbol: None }));
        assert!(
            matches!(&intents[1], OrderIntent::Flatten { symbol, .. } if symbol == "BTCUSDT"),
            "Account insertion order is the expansion order"
        );
        assert!(matches!(&intents[2], OrderIntent::Flatten { symbol, .. } if symbol == "ETHUSDT"));
    }

    #[test]
    fn market_exit_submits_reduce_only_closing_orders() {
        let mut c = test_core();
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        c.engine.account.positions.insert(
            ("sim".into(), "ETHUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: -3.0, avg_px: 50.0, ..Default::default() },
        );
        let coids = c.apply_intent(OrderIntent::MarketExit { venue: None }, 0);
        assert_eq!(coids.len(), 2, "one closing order per open position");
        let subs = &c.engine.client.submissions;
        assert_eq!(subs.len(), 2);
        assert_eq!((subs[0].symbol.as_str(), subs[0].side, subs[0].qty), ("BTCUSDT", -1, 2.0));
        assert_eq!((subs[1].symbol.as_str(), subs[1].side, subs[1].qty), ("ETHUSDT", 1, 3.0));
        assert!(subs.iter().all(|o| o.reduce_only && o.order_type == "market"));
    }

    #[test]
    fn market_exit_is_a_noop_beyond_mass_cancel_when_flat() {
        let mut c = test_core();
        let coids = c.apply_intent(OrderIntent::MarketExit { venue: None }, 0);
        assert!(coids.is_empty());
        assert!(c.engine.client.submissions.is_empty());
    }

    #[test]
    fn market_exit_scoped_to_a_foreign_venue_flattens_nothing_here() {
        let mut c = test_core();
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        let intents = c.expand_market_exit(Some("binance"));
        assert_eq!(intents.len(), 1, "only the venue-scoped mass-cancel; sim is not the target");
        assert!(
            matches!(&intents[0], OrderIntent::MassCancel { venue: Some(v), symbol: None } if v == "binance")
        );
    }

    #[test]
    fn market_exit_expansion_is_replay_deterministic() {
        // The property the compound verb relies on to need NO journal record kind of its own: TWO
        // cores that folded the SAME record prefix (here: the same submits + the same venue fills,
        // applied in the same order) expand `MarketExit` into the same intent list — the "replay"
        // core is a second, independently-built core, not the same one asked twice.
        let build = || {
            let mut c = test_core_with(QueuedEventClient::default(), Vec::new());
            // the engine folds venue events only for symbols it accepts
            c.engine.extra_symbols = vec!["ETHUSDT".into(), "SOLUSDT".into()];
            for (coid, sym, side, qty) in
                [("a", "BTCUSDT", 1, 1.0), ("b", "ETHUSDT", -1, 2.0), ("c", "SOLUSDT", 1, 3.5)]
            {
                c.apply_intent(
                    OrderIntent::Submit(Box::new(OrderRequest {
                        client_order_id: coid.into(),
                        venue: "sim".into(),
                        symbol: sym.into(),
                        side,
                        qty,
                        order_type: "limit".into(),
                        price: Some(10.0),
                        ..Default::default()
                    })),
                    0,
                );
                c.engine.client.pending.extend(fill_events(coid, "sim", sym, side, qty, 10.0));
                c.pump_client();
            }
            c
        };
        let live = build();
        let replayed = build();
        assert_eq!(
            format!("{:?}", live.expand_market_exit(None)),
            format!("{:?}", replayed.expand_market_exit(None))
        );
        // and it is not vacuously equal — the fills really did open three positions
        assert_eq!(live.market_exit_flatten_legs(None).len(), 3);
    }

    /// REGRESSION (adversarial review, major #1). The flatten legs MUST be derived AFTER the
    /// mass-cancel, because `MassCancel`'s own arm ends in `pump_client()` — which can fold a FILL
    /// that opens a position on a symbol that was FLAT when the operator hit the panic button. A
    /// plan snapshotted up front carries no leg for it and the "get me out" verb hands back an
    /// open position.
    #[test]
    fn market_exit_flattens_a_position_opened_by_the_mass_cancels_own_pump() {
        let mut c = test_core_with(QueuedEventClient::default(), Vec::new());
        c.engine.extra_symbols = vec!["ETHUSDT".into()];
        // a resting BUY on ETHUSDT; ETH is FLAT at this point (no fill folded yet)
        c.apply_intent(
            OrderIntent::Submit(Box::new(OrderRequest {
                client_order_id: "e1".into(),
                venue: "sim".into(),
                symbol: "ETHUSDT".into(),
                side: 1,
                qty: 4.0,
                order_type: "limit".into(),
                price: Some(50.0),
                ..Default::default()
            })),
            0,
        );
        assert!(c.market_exit_flatten_legs(None).is_empty(), "precondition: flat at button-press");
        // the venue had already filled it — the events are queued and will surface on the NEXT
        // poll, i.e. inside the mass-cancel's pump
        c.engine.client.pending.extend(fill_events("e1", "sim", "ETHUSDT", 1, 4.0, 50.0));

        c.apply_intent(OrderIntent::MarketExit { venue: None }, 0);

        assert_eq!(
            c.engine.position_size_of("ETHUSDT", "BOTH"),
            4.0,
            "precondition: the mass-cancel pump really did open the position"
        );
        let close = c
            .engine
            .client
            .submissions
            .iter()
            .find(|o| o.symbol == "ETHUSDT" && o.reduce_only)
            .expect("a flatten leg for the position the mass-cancel pump opened");
        assert_eq!(
            (close.side, close.qty, close.order_type.as_str()),
            (-1, 4.0, "market"),
            "the exit must SEND the closing order; without the post-pump re-derivation the              operator is left long 4 ETH with nothing on the wire"
        );
    }

    /// THE PANIC BUTTON WORKS FROM A HALTED CORE — the guarantee this test now pins, and the exact
    /// REVERSAL of what it pinned before.
    ///
    /// It used to be `market_exit_under_halted_denies_every_flatten_leg_and_says_so`, asserting that
    /// `RiskGate`'s kill switch denied EVERY order under `Halted`, `reduce_only` included, so the
    /// exit was disarmed in precisely the safe-state / dead-man situations an operator reaches for
    /// it. That was pinned as a "deliberate non-bypass"; it was really a trap — halted WITH the
    /// position open and no way to close it, the escape being to un-halt the whole core (strategy
    /// included) and re-issue. The gate now admits a POSITION-COVERED reduce under `Halted`
    /// (`vike_model::is_covered_reduce`), which is exactly the shape `Flatten` mints.
    ///
    /// A kill switch must stop OPENING risk, never trap you in it.
    #[test]
    fn market_exit_under_halted_still_flattens_because_a_halt_must_not_trap_you() {
        let mut c = test_core();
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        c.engine.trading_state = TradingState::Halted;
        c.apply_intent(OrderIntent::MarketExit { venue: None }, 0);
        assert_eq!(
            c.engine.client.submissions.len(),
            1,
            "the flatten leg MUST reach the client while Halted — the operator has to be able to \
             get out: {:?}",
            c.engine.client.submissions
        );
        let leg = &c.engine.client.submissions[0];
        assert_eq!(
            (leg.side, leg.qty, leg.order_type.as_str(), leg.reduce_only),
            (-1, 2.0, "market", true),
            "and it is the closing leg: reduce_only market for the whole position"
        );
        assert!(
            c.recent.iter().any(|m| m.contains("HALTED")),
            "the operator is still TOLD the exit ran under a halt — silence would be worse now \
             that it works, not better: {:?}",
            c.recent
        );
    }

    // ── the opt-in shutdown sweep (`CoreConfig::cancel_orders_on_shutdown`) ──────────────────
    //
    // The wiring — that the core's TEARDOWN calls this when the flag is on and not when it is off —
    // is proved end to end through a real `spawn_core` in
    // `crates/vike-core/tests/wiring/shutdown_cancel_policy.rs`. These two prove what the sweep DOES, from
    // in-crate where the client's recorded cancels are directly readable.

    /// The sweep cancels every resting order, naming each one to the client.
    #[test]
    fn the_shutdown_sweep_cancels_every_resting_order() {
        let mut c = test_core();
        c.apply_intent(OrderIntent::Submit(market_req("rest-1")), 0);
        c.apply_intent(OrderIntent::Submit(market_req("rest-2")), 0);
        // `RecordingClient` emits nothing of its own, so both orders sit non-terminal — which is
        // exactly the state a resting order is in when a daemon is stopped.
        assert!(c.engine.client.cancels.is_empty(), "precondition: nothing cancelled yet");

        c.cancel_resting_on_shutdown();

        let mut cancelled = c.engine.client.cancels.clone();
        cancelled.sort();
        assert_eq!(cancelled, vec!["rest-1".to_string(), "rest-2".to_string()]);
        assert!(
            c.recent.iter().any(|m| m.contains("shutdown") && m.contains("positions untouched")),
            "the operator is told what the stop did — and what it did NOT do: {:?}",
            c.recent
        );
    }

    /// MUTATION SENTINEL: an empty book must not manufacture a cancel, and must not leave a note
    /// claiming one happened. A sweep that unconditionally logged would pass the test above.
    #[test]
    fn the_shutdown_sweep_is_a_no_op_when_nothing_is_resting() {
        let mut c = test_core();
        c.cancel_resting_on_shutdown();
        assert!(c.engine.client.cancels.is_empty(), "nothing rested, so nothing is cancelled");
        assert!(
            !c.recent.iter().any(|m| m.contains("shutdown: cancelled")),
            "and no note claims otherwise: {:?}",
            c.recent
        );
    }

    /// It CANCELS; it does not FLATTEN. A stop must not decide on its own to realize PnL — closing a
    /// position is `MarketExit`, an operator action. Pinned because "cancel on shutdown" is one
    /// short step from "go flat on shutdown" in a reader's head.
    #[test]
    fn the_shutdown_sweep_never_closes_a_position() {
        let mut c = test_core();
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        c.apply_intent(OrderIntent::Submit(market_req("rest-1")), 0);
        let submitted_before = c.engine.client.submissions.len();

        c.cancel_resting_on_shutdown();

        assert_eq!(c.engine.client.cancels, vec!["rest-1".to_string()], "the order is cancelled");
        assert_eq!(
            c.engine.client.submissions.len(),
            submitted_before,
            "but NO closing order is sent — the position survives the stop"
        );
        assert_eq!(c.engine.position_size_of("BTCUSDT", "BOTH"), 2.0, "the position is untouched");
    }

    /// THE OTHER HALF, and the mutation sentinel for the test above: admitting the flatten must not
    /// have turned `Halted` into a state that admits ORDERS generally. An ordinary opening order on
    /// the same halted core still dies at the gate and never reaches the venue.
    #[test]
    fn market_exit_flattening_under_halt_did_not_open_the_gate_to_opening_orders() {
        let mut c = test_core();
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        c.engine.trading_state = TradingState::Halted;
        // an opening BUY on the same symbol — risk-INCREASING, and the thing a halt exists to stop
        c.apply_intent(OrderIntent::Submit(market_req("open-me")), 0);
        assert!(
            c.engine.client.submissions.is_empty(),
            "a halt must still refuse an opening order: {:?}",
            c.engine.client.submissions
        );
        // ...and the exit still works on the very same core, in the same state.
        c.apply_intent(OrderIntent::MarketExit { venue: None }, 0);
        assert_eq!(c.engine.client.submissions.len(), 1, "the exit is still admitted");
        assert!(c.engine.client.submissions[0].reduce_only);
    }

    /// The documented counterpart: under `Reducing` the flatten legs ARE permitted (they are
    /// `reduce_only`). `lanes.rs` claims this; nothing pinned it before.
    #[test]
    fn market_exit_under_reducing_still_flattens() {
        let mut c = test_core();
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        c.engine.trading_state = TradingState::Reducing;
        c.apply_intent(OrderIntent::MarketExit { venue: None }, 0);
        assert_eq!(c.engine.client.submissions.len(), 1, "reduce_only passes the Reducing gate");
        assert!(c.engine.client.submissions[0].reduce_only);
        assert!(c.recent.iter().all(|m| !m.contains("HALTED")));
    }

    /// The cross-engine walk (`for idx in 0..=extra_engines.len()`), the `*pv == eng_venue` filter
    /// and `Flatten`'s venue routing were entirely unexercised (review test-gap #3).
    #[test]
    fn market_exit_walks_every_engine_and_routes_each_flatten_to_its_own() {
        let mut c = test_core_with(
            QueuedEventClient::default(),
            vec![(1.0, extra_engine("bin", "BTCUSDT"))],
        );
        c.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        c.extra_engines[0].1.account.positions.insert(
            ("bin".into(), "ETHUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: -3.0, avg_px: 50.0, ..Default::default() },
        );
        let legs = c.market_exit_flatten_legs(None);
        assert_eq!(legs.len(), 2, "primary engine first, then extras in registration order");
        assert!(
            matches!(&legs[0], OrderIntent::Flatten { venue, symbol } if venue == "sim" && symbol == "BTCUSDT")
        );
        assert!(
            matches!(&legs[1], OrderIntent::Flatten { venue, symbol } if venue == "bin" && symbol == "ETHUSDT")
        );

        c.apply_intent(OrderIntent::MarketExit { venue: None }, 0);
        assert_eq!(c.engine.client.submissions.len(), 1, "sim leg goes to the sim client");
        assert_eq!(c.engine.client.submissions[0].symbol, "BTCUSDT");
        assert_eq!(c.extra_engines[0].1.client.submissions.len(), 1, "bin leg goes to the bin one");
        assert_eq!(c.extra_engines[0].1.client.submissions[0].symbol, "ETHUSDT");

        // and a venue-scoped exit touches only that engine
        let scoped = c.market_exit_flatten_legs(Some("bin"));
        assert!(scoped
            .iter()
            .all(|l| matches!(l, OrderIntent::Flatten { venue, .. } if venue == "bin")));
    }

    /// Pins the documented hedge-mode scope note (review test-gap #6): non-`BOTH` position rows are
    /// SKIPPED, so a future change that starts expanding LONG/SHORT legs cannot land silently.
    #[test]
    fn market_exit_skips_hedge_mode_long_short_rows() {
        let mut c = test_core();
        for side in ["LONG", "SHORT"] {
            c.engine.account.positions.insert(
                ("sim".into(), "BTCUSDT".into(), side.into()),
                vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
            );
        }
        assert!(
            c.market_exit_flatten_legs(None).is_empty(),
            "hedge-mode legs are out of scope until the primitives carry per-leg closes"
        );
        let coids = c.apply_intent(OrderIntent::MarketExit { venue: None }, 0);
        assert!(coids.is_empty());
        assert!(c.engine.client.submissions.is_empty());
    }

    /// CHARACTERIZATION (this REVEALS the current verdict; it argues for no cure): a multi-position
    /// `MarketExit` METERS ITS OWN LEGS against one order-rate window and starves the tail of them.
    ///
    /// Three facts compose into it, each true on its own:
    ///
    ///   1. `Self::market_exit_flatten_legs` mints ONE `OrderIntent::Flatten` per non-flat
    ///      position, and each lowers to an ordinary `OrderIntent::Submit` — so each crosses
    ///      `vike_exec::RiskGate::check` with `consume_throttle: true`, like any opening order;
    ///   2. `RiskGate`'s sliding window is per-GATE, and `vike_mount::make_engine` builds ONE
    ///      engine — so ONE gate, so ONE window — per venue, shared by every symbol routed
    ///      through it (the field's own doc says so);
    ///   3. `Self::apply_intent`'s `MarketExit` arm applies every leg under the SAME `now`, so the
    ///      window cannot slide between them. `admit_throttle` evicts on `now_ms - window_ms`, and
    ///      all N stamps are identical.
    ///
    /// `RiskGate::check_inner`'s throttle lane carries no `covered_reduce` term (unlike the min
    /// floors, the price collar, the buying-power charge and the impact veto, which all bypass for
    /// a covered reduce, and unlike the `Halted` kill switch, which admits one). Neither does
    /// `max_notional_per_order`, which `vike_mount::require_live_risk_budget` makes MANDATORY on a
    /// live mount and is therefore the more reachable denial there.
    /// `crates/vike-exec/tests/risk/risk_lane_completion.rs`'s
    /// `a_position_covered_reduce_is_metered_by_the_shared_order_rate_window` and
    /// `the_mandatory_live_caps_have_no_covered_reduce_bypass_either` pin those two lanes directly.
    ///
    /// Why nothing caught it: `test_core`/`test_core_with` build the gate from `RiskLimits::new()`,
    /// whose `max_orders_per_window` is `None`, so EVERY other `MarketExit` test here — the
    /// halt-does-not-trap-you one included — runs with the throttle DISARMED.
    #[test]
    fn a_multi_position_market_exit_meters_its_own_legs_against_one_window() {
        let mut c = test_core();
        // Arm the window at 2 orders (`RiskLimits::new()` supplies `window_ms = 1000`) on the ONE
        // gate this engine owns, then open THREE positions for it to flatten.
        c.engine.gate.limits.max_orders_per_window = Some(2);
        for symbol in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
            c.engine.account.positions.insert(
                ("sim".into(), symbol.into(), "BOTH".into()),
                vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
            );
        }
        assert_eq!(c.market_exit_flatten_legs(None).len(), 3, "precondition: three legs to mint");

        let denied_before = c.engine.dropped_unknown_coid;
        let coids = c.apply_intent(OrderIntent::MarketExit { venue: None }, 0);

        // The verb still MINTS a leg per position — a denied submit returns its coid like any
        // other, so the coid list alone reports success.
        assert_eq!(coids.len(), 3, "one leg minted per open position");
        // ...but only the window's worth of them reaches the venue.
        let sent: Vec<&str> =
            c.engine.client.submissions.iter().map(|o| o.symbol.as_str()).collect();
        assert_eq!(
            sent,
            vec!["BTCUSDT", "ETHUSDT"],
            "the exit spends its own rate window on its first legs and the last one is refused"
        );
        // The refusal is PUBLISHED as an ordinary `OrderDenied`, observed the way
        // `combo_denied_by_the_gate_emits_order_denied_and_submits_nothing` observes it: a denied
        // order was never registered, so its coid is counted as unknown on the way out.
        //
        // ⚠ Deliberately NOT asserted through `c.recent`. An earlier draft did, and it was wrong
        // about the harness rather than the behaviour: `apply_intent` publishes the event but folds
        // nothing, so `recent` — which the combo paths above populate by pushing to it directly —
        // stays EMPTY here even though the leg really was refused. The first two assertions in this
        // test already prove the refusal happened (one leg short on the wire); this proves the
        // engine said so rather than dropping it silently.
        assert!(
            c.engine.dropped_unknown_coid > denied_before,
            "the starved leg's OrderDenied was published"
        );
        // and the starved position is still OPEN — the operator pressed the panic button and is
        // still short of flat by one symbol.
        assert_eq!(c.engine.position_size_of("SOLUSDT", "BOTH"), 2.0);

        // MUTATION SENTINEL: it was the WINDOW that refused it — not the symbol, and not some
        // later lane. The identical leg, on the same core, one window later (`admit_throttle`
        // evicts stamps at or before `now_ms - window_ms`) reaches the venue.
        c.apply_intent(
            OrderIntent::Flatten { venue: "sim".into(), symbol: "SOLUSDT".into() },
            1_002,
        );
        let sent: Vec<&str> =
            c.engine.client.submissions.iter().map(|o| o.symbol.as_str()).collect();
        assert_eq!(
            sent,
            vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"],
            "the starved leg must go out once the window slid — otherwise this pins the wrong lane"
        );
    }

    #[test]
    fn tagged_submit_registers_minted_coid_for_modify() {
        let mut c = test_core();
        // simulate what drain_broker does for a tagged limit: apply Submit(empty coid), then register
        let coids = c.apply_intent(
            OrderIntent::Submit(Box::new(OrderRequest {
                client_order_id: String::new(),
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 1.0,
                order_type: "limit".into(),
                price: Some(100.0),
                ts: 0,
                ..Default::default()
            })),
            0,
        );
        c.strategy_tags.insert("0|sim|BTCUSDT|q1".to_string(), coids[0].clone());
        // resolve + modify by tag (the drain's path)
        let coid = c.strategy_tags.get("0|sim|BTCUSDT|q1").cloned().unwrap();
        c.apply_intent(
            OrderIntent::Modify {
                client_order_id: coid.clone(),
                new_qty: Some(2.0),
                new_price: None,
            },
            0,
        );
        // the recording client saw exactly one submit; the modify targets the accepted order (no-op
        // pre-accept is fine — this asserts the tag→coid wiring, not the venue modify)
        assert_eq!(c.engine.client.submissions.len(), 1);
        assert_eq!(c.engine.client.submissions[0].client_order_id, coid);
    }

    #[test]
    fn conditional_fire_is_gated() {
        let mut c = test_core();
        c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
        // arm a stop that a downward bar crosses
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: Some(95.0),
                trail: None,
                trigger_by: None,
            }),
            0,
        );
        c.engine.trading_state = TradingState::Halted;
        // fire against a crossing bar via submit_fired's public entry (fire_conditionals_bar)
        let bar = mk_bar(1, 90.0, 92.0);
        c.fire_conditionals_bar("sim", "BTCUSDT", &bar);
        assert!(
            c.engine.client.submissions.is_empty(),
            "a fired conditional still crosses RiskGate"
        );
    }

    #[test]
    fn global_mass_cancel_clears_conditional_books() {
        let mut c = test_core();
        c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: Some(95.0),
                trail: None,
                trigger_by: None,
            }),
            0,
        );
        c.apply_intent(OrderIntent::MassCancel { venue: None, symbol: None }, 0);
        // a bar that WOULD have crossed the stop now fires nothing (book cleared)
        let bar = mk_bar(1, 90.0, 92.0);
        c.fire_conditionals_bar("sim", "BTCUSDT", &bar);
        assert!(
            c.engine.client.submissions.is_empty(),
            "global mass-cancel must clear armed conditionals"
        );
    }

    /// The disarm verb (emulator PR-2): removing an arm by its minted id means the crossing bar
    /// that would have fired it releases nothing, and the operator got a confirmation note.
    #[test]
    fn disarm_conditional_removes_the_arm_so_it_no_longer_fires() {
        let mut c = test_core();
        c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: Some(95.0),
                trail: None,
                trigger_by: None,
            }),
            0,
        );
        // the runtime minted `{coid_session}a0` for the first arm of this session
        let arm_id = format!("{}a0", c.coid_gen.state().0);
        c.apply_intent(OrderIntent::DisarmConditional { arm_id: arm_id.clone() }, 1);
        assert!(
            c.recent.back().unwrap().contains("DISARMED"),
            "the disarm is confirmed on the recent-events surface: {:?}",
            c.recent
        );
        let bar = mk_bar(2, 90.0, 92.0); // would have crossed the 95 stop
        c.fire_conditionals_bar("sim", "BTCUSDT", &bar);
        assert!(c.engine.client.submissions.is_empty(), "a disarmed conditional must not fire");
    }

    /// An unknown/stale arm id is a LOUD no-op — surfaced to recent-events, nothing disturbed,
    /// never a panic (the stale-click tolerance the book's own `disarm` documents).
    #[test]
    fn disarm_unknown_arm_id_is_a_loud_noop_that_disturbs_nothing() {
        let mut c = test_core();
        c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: Some(95.0),
                trail: None,
                trigger_by: None,
            }),
            0,
        );
        c.apply_intent(OrderIntent::DisarmConditional { arm_id: "nope".into() }, 1);
        assert!(
            c.recent.back().unwrap().contains("unknown arm id"),
            "the refusal must be loud: {:?}",
            c.recent
        );
        // and the REAL arm is untouched — the crossing bar still fires it
        c.fire_conditionals_bar("sim", "BTCUSDT", &mk_bar(2, 90.0, 92.0));
        assert_eq!(c.engine.client.submissions.len(), 1, "the resting arm still fires");
    }

    /// The disarm routes across (venue, symbol) books by PROBING for the id (the intent carries
    /// only `arm_id`), and targets exactly ONE arm — siblings on the same and other symbols keep
    /// firing.
    #[test]
    fn disarm_targets_exactly_one_arm_across_books() {
        let mut c = test_core();
        for (sym, px) in [("BTCUSDT", 95.0), ("BTCUSDT", 93.0), ("ETHUSDT", 95.0)] {
            c.apply_intent(
                OrderIntent::ArmConditional(ConditionalIntent {
                    venue: "sim".into(),
                    symbol: sym.into(),
                    side: -1,
                    qty: 1.0,
                    price: Some(px),
                    trail: None,
                    trigger_by: None,
                }),
                0,
            );
        }
        // arms minted a0 (BTC@95), a1 (BTC@93), a2 (ETH@95); disarm the FIRST BTC one
        let session = c.coid_gen.state().0;
        c.apply_intent(OrderIntent::DisarmConditional { arm_id: format!("{session}a0") }, 1);
        // a bar crossing BOTH BTC stops fires only the surviving a1
        c.fire_conditionals_bar("sim", "BTCUSDT", &mk_bar(2, 90.0, 92.0));
        assert_eq!(c.engine.client.submissions.len(), 1, "only the surviving BTC arm fires");
        // and the ETH book was never touched
        c.fire_conditionals_bar("sim", "ETHUSDT", &mk_bar(3, 90.0, 92.0));
        assert_eq!(c.engine.client.submissions.len(), 2, "the ETH arm still fires");
    }

    #[test]
    fn scoped_mass_cancel_clears_only_its_book() {
        let mut c = test_core();
        c.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
        c.apply_intent(
            OrderIntent::ArmConditional(ConditionalIntent {
                venue: "sim".into(),
                symbol: "ETHUSDT".into(),
                side: -1,
                qty: 1.0,
                price: Some(95.0),
                trail: None,
                trigger_by: None,
            }),
            0,
        );
        c.engine.account.set_mark_from("sim", "ETHUSDT", 100.0, MarkSource::VenueMark, 0);
        c.apply_intent(
            OrderIntent::MassCancel { venue: Some("sim".into()), symbol: Some("BTCUSDT".into()) },
            0,
        );
        let bar = mk_bar(1, 90.0, 92.0);
        c.fire_conditionals_bar("sim", "ETHUSDT", &bar);
        assert_eq!(
            c.engine.client.submissions.len(),
            1,
            "the untouched (sim,ETH) book still fires"
        );
    }

    // ── capability preflight (w2-task-5) ─────────────────────────────────────────────────────

    fn venue_req(venue: &str, order_type: &str, tif: vike_model::TimeInForce) -> Box<OrderRequest> {
        Box::new(OrderRequest {
            client_order_id: format!("pf-{venue}-{order_type}"),
            venue: venue.into(),
            symbol: "X".into(),
            side: 1,
            qty: 1.0,
            order_type: order_type.into(),
            price: Some(1.0),
            time_in_force: tif,
            ..Default::default()
        })
    }

    /// A refused submit follows the Combo arm's shape: NOTHING reaches the client, and the order
    /// terminalizes locally as `OrderSubmitted` → `OrderRejected` (status `Rejected`) with the
    /// machine-readable reason surfaced on the recent-events strip.
    #[test]
    fn preflight_refusal_synthesizes_terminal_reject_and_never_reaches_client() {
        let mut c = test_core();
        // deribit wires no trigger orders — a "stop" would coerce to an IMMEDIATE market there
        let coids = c.apply_intent(
            OrderIntent::Submit(venue_req("deribit", "stop", vike_model::TimeInForce::Gtc)),
            7,
        );
        assert_eq!(coids.len(), 1, "the refusal still names the order");
        assert!(c.engine.client.submissions.is_empty(), "refused order must not reach the client");
        let mo = c.engine.registry.get(&coids[0]).expect("registered for the FSM to advance");
        assert_eq!(mo.status, vike_exec::OrderStatus::Rejected, "terminal reject");
        assert!(
            c.recent.iter().any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=deribit")),
            "machine-readable reason surfaced: {:?}",
            c.recent
        );
    }

    /// The aster/ig flip this task ships: a non-GTC limit that would silently rest GTC is now a
    /// loud deny — while a GTC limit (what actually rests) still submits.
    #[test]
    fn preflight_flips_silent_tif_ignores_to_loud_denies() {
        let mut c = test_core();
        c.apply_intent(
            OrderIntent::Submit(venue_req("aster", "limit", vike_model::TimeInForce::Ioc)),
            0,
        );
        assert!(c.engine.client.submissions.is_empty(), "aster Ioc limit is refused");
        assert!(c.recent.iter().any(|n| n.contains("TIF_UNSUPPORTED: tif=Ioc venue=aster")));
        c.apply_intent(
            OrderIntent::Submit(venue_req("aster", "limit", vike_model::TimeInForce::Gtc)),
            0,
        );
        assert_eq!(c.engine.client.submissions.len(), 1, "aster GTC limit still submits");
    }

    /// THE COMPAT LAW at the core edge: everything venues accept-and-honor today still submits —
    /// binance's perp-lane GTD (lane union), the coercion venues' coerced TIFs, and every
    /// non-roster (sim/paper) venue id.
    #[test]
    fn preflight_passes_accepted_requests_through() {
        let mut c = test_core();
        for (venue, ot, tif) in [
            ("binance", "limit", vike_model::TimeInForce::Gtd), // perp lane wires native GTD
            ("polymarket", "limit", vike_model::TimeInForce::Ioc), // live Ioc→FOK coercion
            ("hyperliquid", "take_profit", vike_model::TimeInForce::Gtc), // native tpsl trigger
            ("sim", "limit", vike_model::TimeInForce::Ioc),     // non-roster venue: no row
        ] {
            let before = c.engine.client.submissions.len();
            c.apply_intent(OrderIntent::Submit(venue_req(venue, ot, tif)), 0);
            assert_eq!(
                c.engine.client.submissions.len(),
                before + 1,
                "{venue}/{ot}/{tif:?} must reach the client"
            );
        }
    }

    /// A bracket with a preflight-refused child is refused ATOMICALLY: no leg reaches the
    /// client, the culprit carries its own reason, the siblings carry the culprit's coid.
    #[test]
    fn bracket_with_unsupported_child_is_refused_whole() {
        let mut c = test_core();
        let spec = vike_model::BracketSpec {
            venue: "deribit".into(), // no native trigger orders → the SL "stop" child is refused
            symbol: "X".into(),
            side: 1,
            qty: 1.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
        };
        let coids = c.apply_intent(OrderIntent::Bracket(Box::new(spec)), 0);
        assert_eq!(coids.len(), 3);
        assert!(c.engine.client.submissions.is_empty(), "no bracket leg may reach the client");
        for coid in &coids {
            let mo = c.engine.registry.get(coid).expect("every leg registered");
            assert_eq!(mo.status, vike_exec::OrderStatus::Rejected, "{coid} terminal");
        }
        assert!(c
            .recent
            .iter()
            .any(|n| n.contains("TRIGGER_UNSUPPORTED: kind=stop venue=deribit")));
        assert!(c.recent.iter().any(|n| n.contains("BRACKET_ATOMIC_REFUSED: culprit=")));
    }

    /// SubmitBatch (single-engine path): a refused leg terminalizes locally while the healthy
    /// legs proceed as one batch.
    #[test]
    fn submit_batch_refuses_only_the_unsupported_leg() {
        let mut c = test_core();
        let good = *market_req("b-good");
        let bad = *venue_req("deribit", "stop", vike_model::TimeInForce::Gtc);
        c.apply_intent(OrderIntent::SubmitBatch(vec![good, bad]), 0);
        assert_eq!(c.engine.client.submissions.len(), 1, "only the healthy leg reaches the client");
        assert_eq!(c.engine.client.submissions[0].client_order_id, "b-good");
        let mo = c.engine.registry.get("pf-deribit-stop").expect("refused leg registered");
        assert_eq!(mo.status, vike_exec::OrderStatus::Rejected);
    }
}

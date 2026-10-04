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
    /// the exit is (`None` ⇒ every engine + every conditional book; `Some(v)` ⇒ every engine OF
    /// THAT EXCHANGE that the exit's own route reaches + that venue's books).
    ///
    /// ⚠ The second half said "that engine", singular, and it was the defect rather than the
    /// description: the arm resolved ONE engine through `CoreThread::route_of`, so a venue-scoped
    /// exit on a two-account node cancelled the default account's book and left the second
    /// account's resting orders live — free to fill straight after the flatten legs and put the
    /// operator back in. The arm fans over [`Self::exit_scope_engines`] now.
    ///
    /// `account` is the EXIT's own, carried onto the leg so the cancel narrows exactly as the exit
    /// does — including the core-held protection the `MassCancel` arm clears (held bracket exits,
    /// armed conditionals), which a leg carrying `None` would clear for every account of the venue
    /// even under a route that names one engine. The exit has already resolved (or refused) the
    /// account before this leg is built, so the leg's own resolution cannot refuse.
    pub(crate) fn market_exit_mass_cancel(
        venue: Option<&str>,
        account: Option<&vike_model::accounts::account_keys::AccountLabel>,
    ) -> OrderIntent {
        OrderIntent::MassCancel {
            venue: venue.map(|v| v.to_string()),
            symbol: None,
            account: account.cloned(),
        }
    }

    /// The FLATTEN legs of a [`OrderIntent::MarketExit`]: ONE [`OrderIntent::Flatten`] per non-flat
    /// position, PAIRED WITH THE ENGINE INDEX IT WAS READ FROM, walked engine by engine (primary
    /// first, then extras in registration order) and, within an engine, in the `Account`
    /// `positions` IndexMap's own insertion order. `Flatten` itself re-resolves the size and
    /// submits a `reduce_only` MARKET for `|position|` at apply time.
    ///
    /// ⚠ **THE INDEX IS HALF THE LEG, and it used to die on the line that built one.** A
    /// `Flatten` names a venue and a symbol and nothing else, so two accounts of one exchange
    /// holding a position in one symbol produced BYTE-IDENTICAL intents — and downstream
    /// `vike_exec::RouteKey::sole_account_of` resolves a venue string to that venue's DEFAULT
    /// account. Both legs landed on the default book: it was flattened twice (its own size
    /// re-resolved each time, so the second leg REVERSED it), and the second account was never
    /// flattened at all. The caller lowers each leg with
    /// [`super::EngineRoute::Engine`], so the order acts on the book the position was read out of.
    ///
    /// `route` is the exit's own route and it scopes the WALK through
    /// [`Self::exit_scope_engines`]: an unscoped exit reads every engine, a venue-scoped one
    /// reads every engine of that exchange, and an exit a labelled mount issued against its own
    /// venue reads that mount's engine alone — a strategy's exit has no business closing another
    /// account's position.
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
    pub(crate) fn market_exit_flatten_legs(
        &self,
        route: EngineRoute,
        venue: Option<&str>,
    ) -> Vec<(usize, OrderIntent)> {
        let mut out = Vec::new();
        for idx in self.exit_scope_engines(route, venue) {
            let eng_venue = self.eng(idx).venue.clone();
            if let Some(v) = venue
                && eng_venue != v
            {
                continue;
            }
            for ((pv, symbol, side), pos) in self.eng(idx).account.positions.iter() {
                if *side != vike_model::events::PositionSide::Both
                    || pv.as_str() != eng_venue
                    || pos.size == 0.0
                {
                    continue;
                }
                out.push((
                    idx,
                    OrderIntent::Flatten {
                        venue: pv.to_string(),
                        symbol: symbol.to_string(),
                        account: None,
                    },
                ));
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
    ///
    /// Expands the EXTERNAL command path ([`EngineRoute::Payload`]) — the operator's panic button,
    /// which is what the replay-determinism comparison folds — and drops each leg's engine index,
    /// because the property it exists to show is that two cores mint the same INTENT LIST. Which
    /// book each leg lands on is [`Self::market_exit_flatten_legs`]'s own assertion.
    #[cfg(test)]
    pub(crate) fn expand_market_exit(&self, venue: Option<&str>) -> Vec<OrderIntent> {
        let mut out = vec![Self::market_exit_mass_cancel(venue, None)];
        out.extend(
            self.market_exit_flatten_legs(EngineRoute::Payload, venue).into_iter().map(|(_, i)| i),
        );
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
            OrderIntent::Submit(req) => {
                let mut req = *req;
                // **THE PAYLOAD NAMES ITS ACCOUNT, AND THAT IS RESOLVED BEFORE THE GATE BELOW.**
                //
                // The ordering is the fix, not an optimisation. `ambiguous_accounts` asks
                // `routed_engine(route, venue).is_some()`, and for `EngineRoute::Payload` that is
                // `None` UNCONDITIONALLY — an `EngineRoute` carries no account, because until
                // `OrderRequest::account` existed a payload had nowhere to name one. So a wire
                // order on a two-account venue was refused as AMBIGUOUS before routing was
                // consulted at all, whatever it said: the sender had named a book and the refusal
                // told them to name a book. Resolving first turns the statement into an
                // `EngineRoute::Engine`, and everything downstream — the gate, the write-ahead
                // route key, `caps_venue`, `route_of` — is handed a determined destination and is
                // untouched.
                //
                // ⚠ **`None` short-circuits to `route` and reaches none of this.** That is the
                // no-regression anchor: every strategy order, every pre-field ticket and every
                // journal replay carries no account, so this arm is a discriminant test and
                // nothing else for them — no lookup, no allocation, no behaviour change.
                // `CoreThread::route_for_payload_account` carries the other two answers and why an
                // unheld account may not fall through to the venue's default book.
                let route = match req.account.as_ref() {
                    None => route,
                    Some(account) => {
                        match self.route_for_payload_account(route, &req.venue, account) {
                            Some(resolved) => resolved,
                            // Refused on the SAME terms as the ambiguity refusal below — before
                            // the mint, so no coid is burned and no sequence gap is left, and with
                            // an EMPTY return because nothing exists to name.
                            None => {
                                self.refuse_unheld_account("submit", &req.venue, account);
                                self.pump_client();
                                return Vec::new();
                            }
                        }
                    }
                };
                // **§4.2's TABLE, ROW 1 — REFUSE BEFORE ANYTHING IS MINTED OR JOURNALED.**
                //
                // §4.5's law: *"A risk-REDUCING venue verb that names no account fans out to EVERY
                // account of that venue. A risk-INCREASING one refuses."* A submit is the
                // risk-INCREASING side by definition — it is the verb that opens exposure — so a
                // request naming an exchange this process runs several accounts of, and no account,
                // is refused rather than routed to the venue's default book.
                //
                // ⚠ **The direction is not symmetric and inverting it is the whole hazard.** Fanning
                // a submit out would place N orders in N books, every one of them an account the
                // sender did not name — strictly worse than the misroute this replaces, which put
                // ONE order in ONE wrong book. §4.5's licence for a fan-out (*"A fan-out can never
                // reach an account the sender did not mean, because the sender meant all of them"*)
                // is true only of a reducing verb and does not transfer.
                //
                // BEFORE THE MINT, following `lower_combo`'s validate-then-mint rule: *"a refused
                // spec that burned a coid would leave a sequence gap, and a gap is only diagnostic
                // while an id that exists but names no order stays impossible."* Nothing was
                // minted, nothing was journaled, nothing reached an engine — so the return is EMPTY
                // (the documented contract is "coids of orders SUBMITTED") rather than the
                // `vec![coid]` the capability and RiskGate refusals below return, which NAME an
                // order because one exists by then.
                if let Some(candidates) = self.ambiguous_accounts(route, &req.venue) {
                    self.refuse_ambiguous("submit", &req.venue, &candidates);
                    self.pump_client();
                    return Vec::new();
                }
                let minted = req.client_order_id.is_empty();
                if minted {
                    req.client_order_id = self.coid_gen.generate();
                }
                // ⚠ ROUTED BEFORE THE JOURNAL WRITE, which is a MOVE rather than a new call (§9
                // item 12): the write-ahead record has to carry the RESOLVED route key, so the
                // resolution has to have happened. Pure `&self` reads, so nothing observable moves.
                let routed = self.route_of(route, &req.venue);
                let eidx = routed.unwrap_or(0);
                // Minted-coid `exec_order` gap: the write-ahead `Cmd` that carried this submit had
                // an EMPTY coid (the mint just happened, AFTER that record), so journal the now-
                // RESOLVED request so the materializer can tie the minted coid to its terms even for
                // an order that terminalizes without ever filling. Only when we actually minted (an
                // explicit-coid submit is already fully resolved in its write-ahead `Cmd`). Cold-ish
                // relative to the p99 event fold — submits are orders, not market data — and gated on
                // journaling being on, so the desktop/no-journal path is byte-identical.
                // ⚠ The route key is resolved BEFORE the journal borrow, not inside it: the
                // `&self` read of the routed engine and the `&mut self.journal` of the append
                // cannot overlap. `None` on every single-account box, so this allocates nothing
                // there whether journaling is on or off.
                let rk = self.journal_route_key(eidx, &req.venue);
                if minted && let Some(journal) = self.journal.as_mut() {
                    journal.append_minted_submit(now, &req, rk.as_deref()).expect("journal append");
                    self.journaled_since_snap += 1;
                }
                let coid = req.client_order_id.clone();
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
                // ⚠ **AN AMBIGUOUS LEG IS NOT `all_primary`, and that clause is load-bearing rather
                // than defensive.** `route_of` resolves an account-less leg to its venue's DEFAULT
                // engine, so on a two-account PRIMARY venue the comparison against `0` is satisfied
                // by the very leg §4.2 refuses — and the whole batch would then be submitted to
                // engine 0 without ever re-entering the single-submit arm where the refusal lives.
                // Answering `false` here routes every leg back through that arm, which refuses the
                // ambiguous ones BY NAME and submits the rest exactly as before.
                //
                // ⚠ **AND NEITHER IS AN UNROUTABLE ONE — which is why this asks `== Some(0)` and
                // not `unwrap_or(0) == 0`.** The two spellings differ on exactly one input: a leg
                // naming a venue NO engine of this process claims, where `route_of` answers `None`.
                // `unwrap_or(0)` turned that `None` into the primary engine, so a leg that resolved
                // NOTHING counted as resolving engine 0 and the whole batch — that leg included —
                // took the single-call path below. `all_primary` is consumed twice further down as
                // the statement *"every leg resolves engine 0"*, and for that leg the statement was
                // false; the arm was reading its own predicate wider than the predicate held.
                //
                // This changes WHICH PATH such a leg takes and NOT WHERE IT LANDS. §4.2's `N = 0`
                // cell is a decision, not an oversight — `CoreThread::ambiguous_accounts`' own doc
                // records it — so the single-submit arm still does `unwrap_or(0)` for an unknown
                // venue and still puts it on engine 0. What it stops doing is BYPASSING that arm:
                // an unroutable leg now passes through the same mint → journal → capability
                // preflight → contingency sequence as any other single submit, and whatever the
                // stage that widens §4.2's other two rows decides to do about `N = 0` will be
                // reached by a batched leg and not only by a lone one.
                // ⚠ **AND NEITHER IS A LEG THAT NAMES AN ACCOUNT — the same argument as the first
                // clause, one row further down the sender table.** That clause is about a leg that
                // said too LITTLE; this one is about a leg that said something this predicate
                // cannot see. `all_primary` asks only about `r.venue`, so on a core whose engine 0
                // is the leg's venue a `{venue: "binance", account: Some("NOSUCH")}` leg finds the
                // ambiguity gate silent (a single-account venue has nothing to be ambiguous about)
                // and `route_of` answering `Some(0)` through its `sole_account_of` fallback — and
                // the batch would then be minted, journaled under engine 0's key and submitted
                // into engine 0's book without ever re-entering the single-submit arm where
                // `CoreThread::route_for_payload_account` lives. That is the exact misroute the
                // `Submit` arm refuses, reached through its sibling intent.
                //
                // ⚠ The dangerous core here is the SINGLE-account one, not the multi-account one.
                // A labelled leg on a genuinely ambiguous venue is already forced down the slow
                // path by the first clause, so it was never the hole; the hole is the venue with
                // nothing to be ambiguous ABOUT, where the gate is silent by construction and the
                // fallback answers `Some(0)` — a client's typo trading the one book the node has,
                // with nothing anywhere saying so.
                //
                // Spelled as `is_none()` rather than as a resolution because this predicate's job
                // is to decide WHICH PATH a leg takes, not where it lands: every labelled leg goes
                // through the single-submit arm and that arm resolves it, refusing the unheld ones
                // BY NAME and routing the held ones to the account they name. A leg naming an
                // account that resolves to engine 0 anyway lands exactly where it would have —
                // this changes its path and not its destination, the same trade the unroutable
                // clause above already makes and argues. An account-LESS batch is byte-identical:
                // `r.account` is `None` for every leg any caller builds today, so the added clause
                // is a discriminant test that always passes and the predicate is the one it was.
                let all_primary = reqs.iter().all(|r| {
                    r.account.is_none()
                        && self.ambiguous_accounts(route, &r.venue).is_none()
                        && self.route_of(route, &r.venue) == Some(0)
                });
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
                    //
                    // The route key is engine 0's — `all_primary` above is exactly the statement
                    // that every leg resolves it, and since that predicate asks `== Some(0)` the
                    // statement now holds for every leg that reaches here rather than for every leg
                    // but an unroutable one — so `journal_route_key` answers `None` on every box
                    // whose engine 0 is its venue's default account, which is every box.
                    let rk = self.journal_route_key(0, &r.venue);
                    if was_minted && let Some(journal) = self.journal.as_mut() {
                        journal
                            .append_minted_submit(now, r, rk.as_deref())
                            .expect("journal append");
                        self.journaled_since_snap += 1;
                    }
                    minted.push(r.client_order_id.clone());
                }
                self.engine.now_ms = now;
                // Capability preflight per leg (w2-task-5): a refused leg gets its synthesized
                // terminal lifecycle (same shape as the Submit arm); the survivors proceed as one
                // batch. The mixed-venue path above needs no twin of this — each of its legs
                // re-enters the Submit arm, which preflights it there.
                //
                // Caps row from the ROUTED engine, per leg, exactly as the Submit arm does. It is
                // spelled per LEG rather than hoisted to `self.engine.venue` because the Submit arm
                // spells it that way and the two arms must not drift — `caps_venue`'s own doc is
                // the argument for the shape. It is NOT a hedge against a leg that resolves no
                // engine: `all_primary` above asks `== Some(0)`, so a leg naming a venue no engine
                // answers for never reaches this loop at all. It answers `false`, every leg
                // re-enters the Submit arm, and the identical `caps_venue(routed, …)` call THERE
                // keeps it on its own string (the unknown-venue affordance).
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
            OrderIntent::MassCancel { venue, symbol, account } => {
                // **THE PAYLOAD MAY NAME ITS ACCOUNT, and that is resolved before anything is
                // cancelled** — `reduce_route_for_account`'s four answers. `None` hands back
                // `route` untouched, so every account-less cancel below (the dead-man, the shutdown
                // sweep, a strategy's `mass_cancel`, the unscoped panic button) runs exactly the
                // arm it always ran. A named account the core does not hold, or one named with no
                // venue, is refused by name and touches nothing: returning here is the whole of
                // "never widened".
                let Some(route) = self.reduce_route_for_account(
                    "mass_cancel",
                    route,
                    venue.as_deref(),
                    account.as_ref(),
                ) else {
                    self.pump_client();
                    return Vec::new();
                };
                // A payload-NAMED account narrows the core-held protection too — see the clear
                // calls below. An account-less cancel keeps the venue-wide clear it always had.
                let narrowed = account.is_some();
                match (venue, symbol) {
                    (None, None) => {
                        self.engine.mass_cancel_with_intent(cancel_intent);
                        for (_, e) in self.extra_engines.iter_mut() {
                            e.mass_cancel_with_intent(cancel_intent);
                        }
                        self.clear_conditional_scope(None, None, None);
                        // Drop every HELD bracket exit + the whole contingency book (the live legs are
                        // canceled at the venue above; clearing the book here stops their subsequent
                        // `OrderCanceled` events from re-alerting as "protective exit died").
                        self.held_orders.clear();
                        self.contingency.clear();
                    }
                    (Some(v), sym) => {
                        // ⚠ A VENUE IS AN EXCHANGE, NOT AN ACCOUNT OF ONE. This resolved a single
                        // engine through `route_of`, whose fallback is
                        // `RouteKey::sole_account_of` — the venue's DEFAULT account — so on a
                        // process holding two accounts of one exchange an operator's
                        // "cancel everything on binance" emptied one book and left the other
                        // resting. `exit_scope_engines` keeps the ONE-engine answer wherever the
                        // caller actually named a book (a labelled mount's own intent, or
                        // `sweep_link_deadman`'s explicit per-account fan) and fans over every
                        // account of the exchange where it did not.
                        //
                        // A payload that NAMED an account arrives here with `route` already
                        // resolved to that account's engine, so `exit_scope_engines` answers with
                        // it alone — the operator's `mass-cancel binance ALT` is ALT's book, never
                        // the venue's.
                        let engines = self.exit_scope_engines(route, Some(&v));
                        for &eidx in &engines {
                            self.eng_mut(eidx).mass_cancel_with_intent(cancel_intent);
                        }
                        // Held exits in scope have no venue order, so clear them explicitly (live legs
                        // self-clean via their venue `OrderCanceled`).
                        //
                        // ⚠ NARROWED to the cancelled engines when the payload named an account:
                        // both stores are keyed by the EXCHANGE, so a venue-wide clear under a
                        // named account would strip the OTHER account's stop-losses and held
                        // bracket exits while leaving its positions and entries live. `None` keeps
                        // the venue-wide clear — the account-less shape, whose engines ARE every
                        // account of the venue.
                        let scope = narrowed.then_some(engines.as_slice());
                        self.clear_held_scope(Some(&v), sym.as_deref(), scope);
                        self.clear_conditional_scope(Some(&v), sym.as_deref(), scope);
                    }
                    (None, Some(_)) => {
                        self.note("MassCancel: symbol without venue is ignored".to_string());
                    }
                }
                self.pump_client();
                Vec::new()
            }
            OrderIntent::Flatten { venue, symbol, account } => {
                // **A NAMED account narrows the flatten to that account's book** — resolved before
                // any position is read, through `reduce_route_for_account` (an unheld account is
                // refused by name and closes nothing; `venue` is never absent on this verb). `None`
                // returns `route` untouched, and everything below is then exactly the account-less
                // fan-out it always was.
                let Some(route) =
                    self.reduce_route_for_account("flatten", route, Some(&venue), account.as_ref())
                else {
                    self.pump_client();
                    return Vec::new();
                };
                // **§4.5's LAW, the REDUCING side:** *"A risk-REDUCING venue verb that names no
                // account fans out to EVERY account of that venue. A risk-INCREASING one refuses."*
                //
                // A standalone `Flatten` names a venue and a symbol and no account, and it mints a
                // `reduce_only` MARKET sized to the position it read: it can only ever CLOSE, never
                // open. So it is the reducing side, and §4.5's licence applies in full — *"A fan-out
                // can never reach an account the sender did not mean, because the sender meant all
                // of them."*
                //
                // ⚠ **It resolved ONE engine, and that was a live defect rather than merely a
                // narrow reading.** `route_of(..).unwrap_or(0)` is the venue's DEFAULT account, so
                // on a two-account node "flatten my binance BTC" read the default account's
                // position, closed the default account's position, and left the second account's
                // position wide open — while `MarketExit` on the same venue, which is this verb plus
                // a mass-cancel, already fanned over `exit_scope_engines` and closed both. The two
                // spellings of one operator intention answered differently.
                //
                // ⚠ Each leg goes back to THE BOOK IT WAS READ OUT OF (`EngineRoute::Engine`),
                // exactly as `market_exit_flatten_legs` does and for the identical reason: two
                // accounts holding the same symbol mint BYTE-IDENTICAL requests, so a leg routed by
                // its payload would flatten the default book twice (the second re-resolving the
                // size the first was still closing, i.e. REVERSING it) and never touch the other.
                //
                // BYTE-IDENTICAL at one account: `exit_scope_engines` answers with exactly
                // `{ route_of(..).unwrap_or(0) }` whenever the venue has at most one engine — the
                // routed engine when the route names one, that venue's sole engine otherwise, and
                // `vec![0]` for a venue this core runs none of, which is the `unwrap_or(0)` this
                // replaces. `positions_size_of` is then read from the same engine as before and the
                // leg lowers onto the same engine as before.
                let mut coids = Vec::new();
                for eidx in self.exit_scope_engines(route, Some(&venue)) {
                    let pos = self.eng(eidx).position_size_of(&symbol, "BOTH");
                    if pos.abs() <= 1e-12 {
                        continue;
                    }
                    let req = OrderRequest {
                        client_order_id: String::new(),
                        venue: venue.clone(),
                        symbol: symbol.clone(),
                        side: vike_model::closing_side(pos),
                        qty: pos.abs(),
                        order_type: "market".to_string(),
                        reduce_only: true,
                        ts: now,
                        ..Default::default()
                    };
                    coids.extend(self.apply_intent_routed(
                        OrderIntent::Submit(Box::new(req)),
                        now,
                        cancel_intent,
                        EngineRoute::Engine(eidx),
                    ));
                }
                coids
            }
            OrderIntent::MarketExit { venue, account } => {
                // **A NAMED account narrows the whole exit to that account's book, and is resolved
                // ONCE, here, before either leg runs** (`reduce_route_for_account`). The resolved
                // route then scopes the mass-cancel leg, the halt notice and the flatten walk alike,
                // which is what keeps "which books does this exit cover" a single answer. An unheld
                // account, or one named with NO venue, is refused by name and neither leg runs —
                // the venue-less case matters most here, because the arm it would otherwise reach
                // is the global exit.
                //
                // ⚠ `account: None` returns `route` untouched: the venue-wide exit and the UNSCOPED
                // panic button (`venue: None, account: None`) take exactly the path they always
                // took, and no gate of the account family can refuse them.
                let Some(route) = self.reduce_route_for_account(
                    "market_exit",
                    route,
                    venue.as_deref(),
                    account.as_ref(),
                ) else {
                    self.pump_client();
                    return Vec::new();
                };
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
                    Self::market_exit_mass_cancel(venue.as_deref(), account.as_ref()),
                    now,
                    CancelIntent::RiskOff,
                    route,
                ));
                // ONE engine scope for the whole verb — the same set the mass-cancel leg above
                // reached, the set the halt notice speaks for, and the set the flatten legs are
                // read from. Deriving them separately is how the cancel and the flatten came to
                // disagree about which books an exit covers.
                let halted = self
                    .exit_scope_engines(route, venue.as_deref())
                    .into_iter()
                    .any(|idx| self.eng(idx).trading_state == TradingState::Halted);
                let legs = self.market_exit_flatten_legs(route, venue.as_deref());
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
                // ⚠ EACH LEG GOES BACK TO THE BOOK IT WAS READ OUT OF — `EngineRoute::Engine(idx)`,
                // not the exit's own `route`. A `Flatten` carries a venue and a symbol and no
                // account, so two accounts of one exchange holding the same symbol minted
                // byte-identical legs and both resolved the venue's DEFAULT engine: it was
                // flattened twice (the second leg re-resolving the size the first one was still
                // trying to close, i.e. REVERSING the position) while the second account was never
                // flattened at all. The index is the only thing that can say which, and it is
                // carried on the leg for exactly that reason.
                for (idx, it) in legs {
                    coids.extend(self.apply_intent_routed(
                        it,
                        now,
                        cancel_intent,
                        EngineRoute::Engine(idx),
                    ));
                }
                coids
            }
            OrderIntent::Bracket(spec) => {
                let spec = *spec;
                // §4.2 row 1, the risk-INCREASING side of §4.5's law — the `Submit` arm's twin, and
                // refused BEFORE the three coids are minted for the same sequence-gap reason. A
                // bracket opens exposure; it is not a reducing verb even though two of its three
                // legs are protective, because the ENTRY is what decides the direction.
                if let Some(candidates) = self.ambiguous_accounts(route, &spec.venue) {
                    self.refuse_ambiguous("bracket", &spec.venue, &candidates);
                    self.pump_client();
                    return Vec::new();
                }
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
                let rk = self.journal_route_key(eidx, &spec.venue);
                if let Some(journal) = self.journal.as_mut() {
                    for o in orders.iter() {
                        journal
                            .append_minted_submit(now, o, rk.as_deref())
                            .expect("journal append");
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
                // §4.2 row 1. An ARM is the risk-INCREASING side of §4.5's law even though a
                // particular arm may be protective: what it stores is a promise to SUBMIT into one
                // book later, and it names that book NOW. Fanning it out would arm N conditionals
                // that each fire an order; picking one would be the misroute this stage closes. It
                // is refused before the `arm_id` is minted, beside the `trigger_by Index` refusal
                // directly below — same shape, same reason ("never armed inert").
                if let Some(candidates) = self.ambiguous_accounts(route, &venue) {
                    self.refuse_ambiguous("ArmConditional", &venue, &candidates);
                    self.pump_client();
                    return Vec::new();
                }
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
                        vike_journal::ConditionalRecord {
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
                        vike_journal::ConditionalRecord {
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
        //
        // ⚠ **§5.1 of the account-routing spec names THIS fallback as a Class-A site, and Stage 1
        // deliberately leaves it in place. The argument, written here so the next reader does not
        // have to re-derive it:**
        //
        // * the fallback is REACHED — the insert below is gated on `eidx != 0`, so an order routed
        //   to engine 0 records nothing and comes back through the venue string on release. But
        //   `coid_venue` is never removed, retained or cleared anywhere in this crate, so an order
        //   routed to a NON-zero engine keeps its exact index for the life of the process and never
        //   reaches the fallback at all. A labelled account's held child is therefore routed by its
        //   recorded index, which is the hazard the paragraph above describes and it does not fire;
        // * what remains is an order whose routing WAS engine 0, re-deriving `sole_account_of` —
        //   which answers engine 0 too wherever engine 0 is its own venue's default account. That is
        //   the shape the mount fan-out produces: `vike_mount::accounts_to_mount` keeps the default
        //   account FIRST and its own doc calls that ordering load-bearing.
        //
        // **A refusal here would also point the wrong way under §4.5.** A released OTO child is the
        // protective leg of a bracket — risk-REDUCING — and dropping it would leave a filled entry
        // with no exit, which is the trading-halt failure that law exists to prevent. The §5.1
        // repair is to make the fallback IMPOSSIBLE rather than to refuse at it, and the honest
        // version of that (recording `coid_venue` for engine 0 as well) grows a map entry on every
        // single-account submit — a change to the single-account path, which is the one thing this
        // stage may not make.
        //
        // ⚠ **That last sentence used to end "It belongs with Stage 2's wire key", and that stage
        // HAS since arrived — by another route, and it brought the PERMISSION with it.** The ORDER
        // PAYLOAD names the account (`OrderRequest::account`) instead of the misroute spec's wire
        // key, and the order-payload spec's §2 quotes this very comment before answering it: *"This
        // is that stage, so it may make it."* So "the one thing this stage may not make" is no
        // longer the obstacle — the licence is granted. RE-MEASURED here on 2026-09-25, what is
        // left is a COST and a REACH problem rather than a rule:
        //
        // * the COST is unchanged, and paying it WOULD work for this function. `coid_venue` is
        //   INSERT-ONLY — nothing in this crate removes, clears or retains it — so dropping the
        //   `eidx != 0` guard turns a map that stays EMPTY for the life of every single-account
        //   process into one that grows an entry per submit and never sheds one. Every coid this
        //   function can be handed was minted by the `Submit` or `Bracket` arm above, so with the
        //   guard gone the lookup always hits and the tail below really does become unreachable.
        //   An unbounded per-submit allocation on the commonest deployment is the price, and it is
        //   why this is a decision rather than a tidy-up;
        // * gating the insert on [`CoreThread::multi_account`] instead costs nothing and keeps the
        //   single-account path byte-identical — but it buys a WEAKER property than stage 4 asks
        //   for. The fallback becomes unreached on a multi-account box and merely PROVABLY EXACT on
        //   a single-account one (one engine per venue there, so `sole_account_of` cannot answer
        //   wrong). The `unwrap_or(0)` still has to be spelled, so the guarantee stays a prose
        //   argument rather than a type — which is precisely what "structurally impossible" asks to
        //   stop relying on;
        // * a third route — resolving `req.account` here the way the `Submit` arm does — cannot
        //   work at all. A bracket's protective legs are built by `vike_model::build_bracket` from
        //   a `BracketSpec` that carries NO account field, so exactly the orders this fallback
        //   exists for arrive with `account: None` by construction, whatever the submitter named;
        // * and NONE of the three reaches the OTHER `coid_venue` fallbacks. The `Cancel` /
        //   `CancelBatch` / `Modify` / `Confirm` arms, and `dispatch_applied_fills`, take a bare
        //   coid off the wire or off a fill — a caller may always name one this process never
        //   placed — so no submit-time insert can make those total. Making them structural means a
        //   refusal, i.e. a behaviour change on four operator verbs, which stage 4's "safe to stop:
        //   yes" does not license.
        //
        // ⚠ What the payload DID change is the reach of the argument's first bullet above, and it
        // holds: a request naming an account is resolved by
        // [`CoreThread::route_for_payload_account`] BEFORE
        // the insert below, so a labelled book is a non-zero index and IS recorded. A labelled
        // account can never BE engine 0 — engine 0 is the primary engine, i.e. the first account of
        // the first venue, which `vike_mount::accounts_to_mount` keeps as that venue's DEFAULT — so
        // the payload opened no new path into this fallback.
        //
        // ⚠ **RULED 2026-09-26: LEAVE IT.** The argument above (re-measured 2026-09-25, #2170) is
        // the full case, and this is the verdict on it rather than another re-measurement — written
        // here so the next reader does not mistake a true argument for an open question.
        // The payload plane already closed the path that mattered: a labelled order is routed and
        // recorded correctly, never through this fallback. What paying the cost would still buy is
        // narrower than it looks — only the unlabelled bracket-leg residual, already exact on a
        // single-account box — against an unbounded, never-shed `coid_venue` entry per submit,
        // forever, on the path that keeps that map EMPTY today. Not worth it. **Reopens if
        // `coid_venue` ever gains bounded or evicted storage** (an entry retired on the order's own
        // terminal event, say) — that would make the same repair free instead of unbounded, and the
        // decision above would need re-arguing rather than re-affirming.
        //
        // The chain itself — `coid_venue`, then the venue's sole-account engine, then engine 0 — is
        // spelled ONCE, in `CoreThread::held_release_engine`, because the account-scoped
        // mass-cancel must attribute a held exit to exactly the book this line releases it onto.
        let eidx = self.held_release_engine(&req.client_order_id, &req.venue);
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
            if !self.contingency.is_held(&child)
                && let Some(req) = self.held_orders.shift_remove(&child)
            {
                self.submit_resolved(&req, now);
                self.note(format!("OTO child {child} released by {filled_coid} fill"));
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
    ///
    /// ⚠ **The scope is a VENUE, never an account, and that is now the right shape rather than an
    /// oversight.** A held exit is matched on its `OrderRequest::venue` — the canonical exchange id
    /// both accounts of a venue carry — so this drops both accounts' held exits. It was ASYMMETRIC
    /// while the arm above it cancelled one engine: the default account's live orders went, and
    /// BOTH accounts' protective held exits went with them, so the second account kept its resting
    /// orders and lost the exits that covered them. Now that the arm fans over
    /// [`Self::exit_scope_engines`], the operator path cancels exactly the books this empties.
    ///
    /// **`engines` NARROWS it to the held exits of those engines** — `Some` exactly when the
    /// payload NAMED an account (`OrderIntent::MassCancel`'s `account`), carrying the engine set
    /// the cancel itself reached. Each held exit is attributed to the engine its release would
    /// reach ([`Self::held_release_engine`], the one answer `submit_resolved` acts on), so
    /// `mass-cancel binance ALT` drops ALT's held exits and leaves the default account's, whose
    /// entries are still live and still need them. `None` is the venue-wide clear above,
    /// unchanged. The replay twin (`crate::replay`'s `fold_intent_scope`) has no engine to narrow
    /// by and needs none: it is single-engine by construction, and on one engine the narrowed set
    /// IS the venue scope.
    ///
    /// **Residual, declared:** where the caller named one engine by its ROUTE rather than by the
    /// payload (a labelled mount's own account-less venue-scoped mass-cancel), this still clears
    /// the other account's held exits. The mount's intent carries no account for it to narrow by,
    /// and narrowing on the route instead would change a strategy-lane behaviour this change is
    /// not about.
    fn clear_held_scope(
        &mut self,
        venue: Option<&str>,
        symbol: Option<&str>,
        engines: Option<&[usize]>,
    ) {
        let doomed: Vec<String> = self
            .held_orders
            .iter()
            .filter(|(c, r)| {
                venue.is_none_or(|v| r.venue == v)
                    && symbol.is_none_or(|s| r.symbol == s)
                    && engines
                        .is_none_or(|set| set.contains(&self.held_release_engine(c, &r.venue)))
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
    ///
    /// ⚠ **VENUE-KEYED, like the books themselves.** `conditional_books` is keyed `(venue, symbol)`
    /// — an EXCHANGE fact, which is why an arm needs `cond_engine` to remember which account armed
    /// it at all — so emptying one book empties both accounts' arms. Same story as
    /// [`Self::clear_held_scope`]: asymmetric while the `MassCancel` arm cancelled a single engine,
    /// matched to the cancel now that the arm fans over [`Self::exit_scope_engines`], and carrying
    /// the same declared residual for a caller that named ONE engine by its route.
    ///
    /// `engines` narrows it exactly as it narrows [`Self::clear_held_scope`]: `Some` when the
    /// payload NAMED an account, and then only the arms that would FIRE onto one of those engines
    /// ([`Self::armed_engine`]) leave the shared book — each through `ConditionalBook::disarm`, so
    /// the other account's stop keeps its place in fire order. An arm whose fire reaches NO engine
    /// (a restored arm on a venue with several accounts, which the Submit arm refuses as ambiguous)
    /// is in no named account's scope, so a narrowed clear leaves it. `None` empties the whole
    /// book, as it always did.
    fn clear_conditional_scope(
        &mut self,
        venue: Option<&str>,
        symbol: Option<&str>,
        engines: Option<&[usize]>,
    ) {
        let in_scope = |(v, s): &(String, String)| {
            venue.is_none_or(|want| v == want) && symbol.is_none_or(|want| s == want)
        };
        let Some(engines) = engines else {
            // The venue-wide clear, exactly as it always ran: every arm in scope leaves, and the
            // books are emptied whole.
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
            return;
        };
        // The NARROWED clear: only the arms that fire onto one of `engines`, each disarmed out of
        // the shared book by id so the survivors keep their fire order. An arm whose fire reaches
        // no engine (`armed_engine` answers `None`) is not in the named account's scope.
        let mut doomed: Vec<((String, String), String)> = Vec::new();
        for (k, book) in self.conditional_books.iter().filter(|(k, _)| in_scope(k)) {
            for (id, _) in book.iter() {
                if self.armed_engine(id, &k.0).is_some_and(|e| engines.contains(&e)) {
                    doomed.push((k.clone(), id.to_string()));
                }
            }
        }
        for (k, id) in &doomed {
            self.cond_engine.remove(id);
            if let Some(book) = self.conditional_books.get_mut(k) {
                let removed = book.disarm(id);
                debug_assert!(removed, "the walk above read this arm out of this book");
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
        // §4.2 row 1, the risk-INCREASING side of §4.5's law — refused FIRST, ahead of even the
        // spec validation, because a refusal that cannot name a book is not about the spec at all.
        // Empty return, matching every other refusal in this function.
        if let Some(candidates) = self.ambiguous_accounts(route, &spec.venue) {
            self.refuse_ambiguous("combo", &spec.venue, &candidates);
            self.pump_client();
            return Vec::new();
        }
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
        let rk = self.journal_route_key(eidx, &req.venue);
        if let Some(journal) = self.journal.as_mut() {
            journal.append_minted_submit(now, &req, rk.as_deref()).expect("journal append");
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
            // The ACCOUNT-aggregate ceiling is a per-ENGINE fact, so whether it is armed is
            // loop-invariant — read once here, beside the equity gate above and for the same
            // reason: the fold it guards is O(positions + live orders), and a deployment that wrote
            // no `max_account_exposure` must pay nothing for a lane it did not turn on. (The FOLD
            // itself is not loop-invariant; it excludes the leg's own symbol, so it runs per leg.)
            let account_ceiling_armed = eng.gate.limits.max_account_exposure.is_some();
            // ⚠ `sizing_equity`, not `resolved_equity` — the combo twin of `gate_and_register`'s
            // own line. This is the buying-power basis a combo is ADMITTED against, so
            // `RiskLimits::max_sizing_equity` applies and a lower figure can only refuse sooner.
            // Bit-identical when no ceiling is armed; the margin-CALL sweep keeps the uncapped
            // resolver, and `vike_exec::ExecutionEngine::sizing_equity`'s doc carries the asymmetry.
            let equity = if armed_any {
                eng.sizing_equity(eng.equity_seed, &self.config.price_cfg)
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
                        // The ACCOUNT-aggregate ceiling's other half, per leg — this account's
                        // gross exposure with THIS leg's symbol's POSITION left out, so
                        // `check_combo` can add each leg's own projection back on, plus every live
                        // resting order the account already has out. Folded only when the ceiling
                        // is armed, the same discipline `gate_and_register`'s single-order twin
                        // follows and for the same reason (it is O(positions + orders), and a
                        // deployment that wrote no `max_account_exposure` must pay nothing for it).
                        // ⚠ Per LEG rather than once for the combo: the excluded symbol differs.
                        // `check_combo` threads each admitted leg's own committed notional on top
                        // of this baseline itself, exactly as it does for margin.
                        // ⚠ `coid` is the JUDGED order — this combo's own, minted above and not yet
                        // registered, so it excludes nothing today. It is passed rather than `""`
                        // because the producer's contract is "leave out the order being judged",
                        // and a combo that ever became re-judgeable (an amend) would need exactly
                        // this and would have no other place to learn it.
                        account_exposure_excl_order: if account_ceiling_armed {
                            eng.resolved_account_exposure_excluding(
                                &leg.symbol,
                                &coid,
                                &self.config.price_cfg,
                            )
                        } else {
                            0.0
                        },
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

#[path = "apply_tests.rs"]
#[cfg(test)]
mod apply_tests;

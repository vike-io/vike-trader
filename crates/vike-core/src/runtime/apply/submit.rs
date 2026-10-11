//! The venue-facing submit tail (`submit_resolved`) and the synthesized capability refusal, plus
//! the submit arms of `apply_intent_routed` — `Submit`, `SubmitBatch` and `Bracket`, one method
//! each — and the minted-submit journal write three of the four minting sites share
//! (`journal_minted_submit`; `lower_combo` is the third).

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Submit ONE resolved request to its engine — the venue-facing tail shared by the plain
    /// `Submit`/`Bracket` paths and the OTO-child RELEASE: route by venue, stamp the clock,
    /// `submit_order` (gate → register → `client.submit`), drain the outbox to the bus. It does NOT
    /// touch the contingency book (the caller owns that) and does NOT re-run the capability preflight
    /// (a released child was preflighted atomically with its bracket before it was ever held). The
    /// coid is the caller's; nothing is returned.
    pub(crate) fn submit_resolved(&mut self, req: &OrderRequest, now: i64) {
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

    /// Synthesize the TERMINAL refusal lifecycle for an order the capability preflight
    /// ([`vike_model::preflight_order`]) refused — the exact event shape of the Combo arm's
    /// `supports_combo` rejection: register the order so the FSM has an entry to advance, then
    /// `OrderSubmitted` → `OrderRejected` carrying the machine-readable `reason`
    /// (`CATEGORY_CONDITION: key=value`, e.g. `TIF_UNSUPPORTED: tif=Fok venue=ig`). No order may
    /// silently vanish, and there is no venue client on the other side of a refusal, so the core
    /// emits the whole lifecycle itself. Per-ORDER log + note — off the fold path.
    pub(crate) fn synthesize_capability_reject(
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

    /// The [`OrderIntent::Submit`] arm of [`Self::apply_intent_routed`]: resolve the payload's
    /// account, refuse an ambiguous venue, mint, journal, capability-preflight, hold a contingent
    /// child, submit. Returns the order's coid, or nothing when it refused before the mint.
    pub(super) fn lower_submit(
        &mut self,
        mut req: OrderRequest,
        now: i64,
        route: EngineRoute,
    ) -> Vec<String> {
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
        if minted {
            self.journal_minted_submit(eidx, &req, now);
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
        let deny = vike_model::preflight_order_at(&req, self.caps_venue(routed, &req.venue)).err();
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

    /// The [`OrderIntent::SubmitBatch`] arm of [`Self::apply_intent_routed`]: a batch whose every
    /// leg resolves engine 0 is minted, journaled and preflighted per leg and submitted to engine 0
    /// as one batch; any other batch re-enters the dispatcher as one `Submit` per leg.
    pub(super) fn lower_submit_batch(
        &mut self,
        mut reqs: Vec<OrderRequest>,
        now: i64,
        cancel_intent: CancelIntent,
        route: EngineRoute,
    ) -> Vec<String> {
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
            if was_minted {
                self.journal_minted_submit(0, r, now);
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
                    vike_model::preflight_order_at(r, self.caps_venue(routed, &r.venue)).err()
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

    /// The [`OrderIntent::Bracket`] arm of [`Self::apply_intent_routed`]: refuse an ambiguous
    /// venue, mint the three coids, journal all three legs, preflight the bracket atomically, hold
    /// the exits and submit the entry.
    pub(super) fn lower_bracket(
        &mut self,
        spec: BracketSpec,
        now: i64,
        route: EngineRoute,
    ) -> Vec<String> {
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
                journal.append_minted_submit(now, o, rk.as_deref()).expect("journal append");
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

    /// Journal one SERVER-MINTED request's resolved terms (`append_minted_submit`) under the route
    /// key of the engine it was routed to — the write [`Self::lower_submit`],
    /// [`Self::lower_submit_batch`] and [`Self::lower_combo`] each make once they have minted a
    /// coid. Each caller keeps its own `if minted` gate.
    ///
    /// [`Self::lower_bracket`] keeps its own spelling of this write: it resolves ONE route key for
    /// all three legs and appends them inside one journal borrow, so calling this once per leg
    /// would resolve that key three times (and allocate it three times on a labelled engine).
    pub(super) fn journal_minted_submit(&mut self, eidx: usize, req: &OrderRequest, now: i64) {
        // ⚠ The route key is resolved BEFORE the journal borrow, not inside it: the
        // `&self` read of the routed engine and the `&mut self.journal` of the append
        // cannot overlap. `None` on every single-account box, so this allocates nothing
        // there whether journaling is on or off.
        let rk = self.journal_route_key(eidx, &req.venue);
        if let Some(journal) = self.journal.as_mut() {
            journal.append_minted_submit(now, req, rk.as_deref()).expect("journal append");
            self.journaled_since_snap += 1;
        }
    }
}

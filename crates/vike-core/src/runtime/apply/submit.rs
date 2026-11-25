//! The venue-facing submit tail (`submit_resolved`) and the synthesized capability refusal, plus
//! the submit arms of `apply_intent_routed` — `Submit`, `SubmitBatch` and `Bracket`, one method
//! each — and the minted-submit journal write three of the four minting sites share
//! (`journal_minted_submit`; `lower_combo` is the third).

use super::*;
use vike_model::orders::client_id::{OrderClientIdLegRole, OrderClientIdStem};

/// The three legs of a bracket in the order `build_bracket` returns them, as the refusal words
/// name them.
const BRACKET_LEGS: [&str; 3] = ["entry", "stop_loss", "take_profit"];

impl<C: ExecutionClient> CoreThread<C> {
    /// Submit ONE resolved request to its engine — the venue-facing tail shared by the plain
    /// `Submit`/`Bracket` paths and the OTO-child RELEASE: route by venue, stamp the clock,
    /// `submit_order` (gate → register → `client.submit`), drain the outbox to the bus. It does NOT
    /// touch the contingency book (the caller owns that) and does NOT re-run the capability preflight
    /// (a released child was preflighted atomically with its bracket before it was ever held). The
    /// order client id is the caller's; nothing is returned.
    pub(crate) fn submit_resolved(&mut self, req: &OrderRequest, now: i64) {
        // Released to the venue as a PLAIN order — the core keeps the OTO/OCO linkage in its book,
        // so the venue (or an OCO-enforcing paper client) must not re-interpret it. Stripping is
        // what makes the release land as a normal resting order rather than being re-held by a
        // client that also enforces OTO. See `strip_contingency_links`.
        let req = strip_contingency_links(req.clone());
        // ⚠ THE ORDER'S OWN ENGINE FIRST, and only then its venue string. This order was ROUTED
        // once already, when it was submitted and held (`apply_intent_routed` recorded the index in
        // `order_client_id_venue` for every non-primary engine), and re-deriving it from `req.venue` would
        // send a labelled account's held OTO child to that venue's DEFAULT account on release — the
        // one moment in an order's life when the caller that knew the account is long gone.
        //
        // ⚠ **§5.1 of the account-routing spec names THIS fallback as a Class-A site, and Stage 1
        // deliberately leaves it in place. The argument, written here so the next reader does not
        // have to re-derive it:**
        //
        // * the fallback is REACHED — the insert below is gated on `engine_index != 0`, so an order routed
        //   to engine 0 records nothing and comes back through the venue string on release. But a row of
        //   `order_client_id_venue` is retired only after its order ENDED (`prune_terminal_order_client_ids`), and a
        //   held child has not, so an order routed to a NON-zero engine keeps its exact index while it can
        //   be released and never reaches the fallback at all. A labelled account's held child is therefore routed by its
        //   recorded index, which is the hazard the paragraph above describes and it does not fire;
        // * what remains is an order whose routing WAS engine 0, re-deriving `sole_account_of` —
        //   which answers engine 0 too wherever engine 0 is its own venue's default account. That is
        //   the shape the mount fan-out produces: `vike_tradehub_core::accounts_to_run` keeps the default
        //   account FIRST and its own doc calls that ordering load-bearing.
        //
        // **A refusal here would also point the wrong way under §4.5.** A released OTO child is the
        // protective leg of a bracket — risk-REDUCING — and dropping it would leave a filled entry
        // with no exit, which is the trading-halt failure that law exists to prevent. The §5.1
        // repair is to make the fallback IMPOSSIBLE rather than to refuse at it, and the honest
        // version of that (recording `order_client_id_venue` for engine 0 as well) grows a map entry on every
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
        // * the COST is unchanged, and paying it WOULD work for this function. `order_client_id_venue` is
        //   INSERT-ONLY — nothing in this crate removes, clears or retains it — so dropping the
        //   `engine_index != 0` guard turns a map that stays EMPTY for the life of every single-account
        //   process into one that grows an entry per submit and never sheds one. Every order client id this
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
        //   work at all. A bracket's protective legs are built by `vike_model::build_bracket`, and a
        //   MOUNTED strategy's `OrderBracket` names no account (its route is the mount's engine), so
        //   exactly the orders this fallback exists for arrive with `account: None` by construction
        //   (a spec that names one is resolved in `lower_bracket` and recorded in `order_client_id_venue`);
        // * and NONE of the three reaches the OTHER `order_client_id_venue` fallbacks. The `Cancel` /
        //   `CancelBatch` / `Modify` / `Confirm` arms, and `dispatch_applied_fills`, take a bare
        //   order client id off the wire or off a fill — a caller may always name one this process never
        //   placed — so no submit-time insert can make those total. Making them structural means a
        //   refusal, i.e. a behaviour change on four operator verbs, which stage 4's "safe to stop:
        //   yes" does not license.
        //
        // ⚠ What the payload DID change is the reach of the argument's first bullet above, and it
        // holds: a request naming an account is resolved by
        // [`CoreThread::route_for_payload_account`] BEFORE
        // the insert below, so a labelled book is a non-zero index and IS recorded.
        //
        // ⚠ **CORRECTED: a labelled account CAN be engine 0**, which this paragraph used to deny
        // ("engine 0 is that venue's DEFAULT"). The node puts every running DEFAULT engine first and the
        // labelled ones after, so engine 0 is labelled exactly when no default account runs at boot
        // (decisions 0156 and 0172), and a default started later (`add_engine_hot`) lands at a higher
        // index. [`CoreThread::order_client_id_row_needed`] records an engine-0 order on such a node, so
        // `lower_submit`, the batch arm and `lower_combo` never reach this fallback for it.
        //
        // ⚠ **RULED 2026-09-26: LEAVE IT.** The argument above (re-measured 2026-09-25, #2170) is
        // the full case, and this is the verdict on it rather than another re-measurement — written
        // here so the next reader does not mistake a true argument for an open question.
        // The payload plane already closed the path that mattered: a labelled order is routed and
        // recorded correctly, never through this fallback. What paying the cost would still buy is
        // narrower than it looks — only the unlabelled bracket-leg residual, already exact on a
        // single-account box — against an unbounded, never-shed `order_client_id_venue` entry per submit,
        // forever, on the path that keeps that map EMPTY today. Not worth it. **Reopens if
        // `order_client_id_venue` ever gains bounded or evicted storage** (an entry retired on the order's own
        // terminal event, say) — that would make the same repair free instead of unbounded, and the
        // decision above would need re-arguing rather than re-affirming.
        //
        // ⚠ **That condition is MET since 2026-11-24**: a row retires `ORDER_CLIENT_ID_PRUNE_LINGER_MS` after its order
        // ended (`prune_terminal_order_client_ids`), so the "INSERT-ONLY" bullet above no longer holds and the
        // repair would cost a row per order in that window, not per order ever. Nothing on this path changed
        // with it, and the verdict is not re-argued here.
        //
        // The chain itself — `order_client_id_venue`, then the venue's sole-account engine, then engine 0 — is
        // spelled ONCE, in `CoreThread::held_release_engine`, because the account-scoped
        // mass-cancel must attribute a held exit to exactly the book this line releases it onto.
        let engine_index = self.held_release_engine(&req.order_client_id, &req.venue);
        if engine_index != 0 {
            self.order_client_id_venue.insert(req.order_client_id.clone(), engine_index);
        }
        self.engine_mut(engine_index).now_ms = now;
        let mut outbox = EventBusOutbox::default();
        self.engine_mut(engine_index).submit_order(&req, now, &mut outbox);
        // Drive the outbox contingency-aware: a released exit vetoed by the RiskGate at arm-time
        // (`OrderDenied`) must clean its own now-stale book entry (`drive_contingency_on_terminal`),
        // not linger active-but-dead. No children to cascade (an exit is a leaf); the surviving OCO
        // sibling is kept or canceled per `CoreConfig::oco_cancel_sibling_on_dead_exit` (keep is the
        // default — see `drive_contingency_on_terminal`).
        self.publish_and_drive_outbox(engine_index, outbox, now, std::slice::from_ref(&req));
    }

    /// Synthesize the TERMINAL refusal lifecycle for an order the capability preflight
    /// ([`vike_venues::preflight_order`]) refused — the exact event shape of the Combo arm's
    /// `supports_combo` rejection: register the order so the FSM has an entry to advance, then
    /// `OrderSubmitted` → `OrderRejected` carrying the machine-readable `reason`
    /// (`CATEGORY_CONDITION: key=value`, e.g. `TIF_UNSUPPORTED: tif=Fok venue=ig`). No order may
    /// silently vanish, and there is no venue client on the other side of a refusal, so the core
    /// emits the whole lifecycle itself. Per-ORDER log + note — off the fold path.
    ///
    /// `refusal_id` is the refusal's catalog id (`vike_model::events::refusal_id_of`): the venue capability for the
    /// preflight's own, the core's for a stopped account or a lapsed hold, the gate's lane for an emulated parent.
    pub(crate) fn synthesize_capability_reject(
        &mut self,
        engine_index: usize,
        req: &vike_model::OrderRequest,
        reason: &str,
        refusal_id: Option<compact_str::CompactString>,
        now: i64,
    ) {
        tracing::warn!(
            target: "vike_core::core",
            order_client_id = %req.order_client_id,
            venue = %req.venue,
            reason,
            "order REFUSED by capability preflight"
        );
        let engine = self.engine_mut(engine_index);
        let mut mo = vike_model::OrderRecord::new(req.clone());
        mo.created_ms = Some(now);
        engine.registry.insert(req.order_client_id.clone(), mo);
        self.publish_synthesized(
            engine_index,
            Event::OrderSubmitted(vike_model::events::OrderSubmitted {
                order_client_id: req.order_client_id.clone(),
                timestamp: now,
            }),
            None,
        );
        self.publish_synthesized(
            engine_index,
            Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: req.order_client_id.clone(),
                reason: reason.to_string().into(),
                timestamp: now,
                refusal_id,
            }),
            Some(req),
        );
        self.note(format!("order {} REFUSED: {reason}", req.order_client_id));
    }

    /// **Say, once per `(venue, kind)`, that a PAPER engine was let through where the venue's own row
    /// would have refused** a reduce-only order ([`vike_venues::preflight_order_at`] skips that one axis
    /// for a stated `paper` tier, decision 0201): `paper: the live <venue> adapter would refuse this
    /// (<kind>)`. The relaxation has a cost the operator must be able to see, because a strategy that
    /// works on a paper account through it (a bracket on IG or Dukascopy) is refused on the demo and
    /// live account of the same venue; the line is on the recent-events ring (never the hot fold),
    /// and `paper_noted` bounds it to one line per pair per run.
    ///
    /// Called for a request that PASSED the preflight, with the route the preflight judged it by.
    /// Costs one `bool` test for every order that is not reduce-only and one tier compare for one that
    /// is; the second preflight (with no tier, so the venue's whole row) runs only for a paper
    /// engine's reduce-only order. It reads the request and the capability table and nothing else, so
    /// the replay core says the same thing.
    pub(crate) fn note_paper_admit(&mut self, routed: Option<usize>, req: &OrderRequest) {
        if !req.reduce_only
            || self.engine(routed.unwrap_or(0)).stated_tier()
                != Some(vike_model::AccountTier::Paper)
        {
            return;
        }
        let live =
            vike_venues::preflight_order_at(req, self.capabilities_venue(routed, &req.venue), None)
                .err();
        if let Some(live) = live
            && let vike_venues::PreflightDeny::ReduceOnly { kind, venue } = &live
            && self.paper_noted.insert((venue.clone(), kind.clone()))
        {
            self.note(format!(
                "paper: the live {venue} adapter would refuse this ({kind}): a demo or live \
                 account refuses it ({live}), so what works here does not work there"
            ));
        }
    }

    /// **Whether engine `engine_index` is a STOPPED account's (decision 0169), and if so the order is refused**: its
    /// whole lifecycle is synthesized terminal with `ACCOUNT_NOT_RUNNING`. Asked of the FINAL index (the one
    /// the order would be handed to, `unwrap_or(0)` included), by the single order, the bracket and the combo
    /// alike: the client of a stopped account stays attached until its cancels are answered, so anything it
    /// is sent now would rest at the venue after the mass cancel and nothing would ever cancel it. One
    /// `contains` on an empty list on every core that never stopped an account.
    pub(super) fn refuse_for_stopped_account(
        &mut self,
        engine_index: usize,
        req: &vike_model::OrderRequest,
        now: i64,
    ) -> bool {
        if !self.is_retired(engine_index) {
            return false;
        }
        let refusal = crate::runtime::refusals::CoreOrderRefusal::AccountStopped {
            account: self.engine(engine_index).route_key.to_string(),
        };
        let refusal_id = vike_model::events::refusal_id_of(refusal.id().word());
        self.synthesize_capability_reject(engine_index, req, &refusal.to_string(), refusal_id, now);
        true
    }

    /// The [`OrderIntent::Submit`] arm of [`Self::apply_intent_routed`]: resolve the payload's
    /// account, refuse an ambiguous venue, mint, journal, refuse an explicit order client id the engine holds
    /// answered or finished ([`Self::answered_order_client_id_status`]), capability-preflight, hold a contingent
    /// child, submit. Returns the order's order client id, or nothing when it refused before the mint or because
    /// the explicit order client id is held.
    pub(super) fn lower_submit(
        &mut self,
        mut req: OrderRequest,
        now: i64,
        route: EngineRoute,
    ) -> Vec<String> {
        // A HELD order's release (`held_release.rs`): `apply_routed_order` took it out of the table
        // already, so every answer below (sent, refused, expired) ends the hold. One `bool` read.
        let releasing = std::mem::take(&mut self.releasing);
        // A strategy's submit whose gate denial may be counted (`denial_streaks.rs`): this lowering's alone.
        let candidate = self.denial_candidate.take();
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
        // route key, `capabilities_venue`, `route_of` — is handed a determined destination and is
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
                    // the mint, so no order client id is burned and no sequence gap is left, and with
                    // an EMPTY return because nothing exists to name.
                    None => {
                        self.refuse_unheld_account("submit", &req.venue, account);
                        self.pump_client();
                        return Vec::new();
                    }
                }
            }
        };
        // **AN ACCOUNT-LESS ORDER FOR A VENUE WHOSE DEFAULT ACCOUNT DOES NOT RUN** (decision 0156).
        //
        // Naming no account names the venue's DEFAULT one, and since 0156 that account may have no
        // engine (no paper engine stands in for it). Then no engine resolves the payload and the
        // `unwrap_or(0)` below would hand the order to ENGINE ZERO — another account, possibly
        // another exchange. So it is refused like any order naming an account this process runs
        // no engine for, with the reason the default account does not run, before the mint.
        //
        // ⚠ Cost: one `is_empty` on every account-less submit of a node whose accounts all run —
        // every node before 0156 — and the lookups only when something does not run. The `N = 0`
        // affordance for an id nobody named (a sim or test venue) is untouched: it is refused
        // only when the composition root NAMED the venue's default account as not running.
        if req.account.is_none()
            && !self.config.not_running.is_empty()
            && self.route_of(route, &req.venue).is_none()
            && self.config.not_running.iter().any(|nr| nr.route_key() == req.venue)
        {
            let default = vike_model::accounts::account_keys::AccountLabel::Default;
            self.refuse_unheld_account("submit", &req.venue, &default);
            self.pump_client();
            return Vec::new();
        }
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
        // spec that burned an order client id would leave a sequence gap, and a gap is only diagnostic
        // while an id that exists but names no order stays impossible."* Nothing was
        // minted, nothing was journaled, nothing reached an engine — so the return is EMPTY
        // (the documented contract is "order client ids of orders SUBMITTED") rather than the
        // `vec![order_client_id]` the capability and RiskGate refusals below return, which NAME an
        // order because one exists by then.
        if let Some(candidates) = self.ambiguous_accounts(route, &req.venue) {
            self.refuse_ambiguous("submit", &req.venue, &candidates);
            self.pump_client();
            return Vec::new();
        }
        // **A REDUCE-ONLY STOP ON A VENUE WITH NO NATIVE STOP IS HELD BY THE CORE, NOT REFUSED** (the
        // owner rule of 2026-10-26). Taken here, BEFORE the mint, so it burns no order client id and journals no
        // `MintedSubmit` (the same validate-then-mint rule as the refusals above); the refusal just
        // below stays for everything else. `arm_core_held_stop` has the contract. Costs two
        // discriminant tests on a submit that is not a stop; per order, never per message. An order
        // held for its release is not asked: the preflight below refuses what the row cannot rest.
        if req.release.is_none() && self.arm_core_held_stop(&req, now, route) {
            return Vec::new();
        }
        // ⚠ ROUTED BEFORE THE MINT AND THE JOURNAL WRITE, which is a MOVE rather than a new call
        // (§9 item 12): the write-ahead record has to carry the RESOLVED route key, and the mint
        // renders the ROUTED engine's plan (decision 0270), so the resolution has to have happened.
        // Pure `&self` reads, so nothing observable moves.
        let routed = self.route_of(route, &req.venue);
        let engine_index = routed.unwrap_or(0);
        let minted = req.order_client_id.is_empty();
        // the sequence a candidate's id is minted under, when the generator minted it
        let mut minted_at = None;
        if minted {
            let drawn = self.order_client_id_gen.sequence();
            // A cancel-then-new amend's pending successor (`R<n>`, `apply/amend.rs`) is this mint's id.
            req.order_client_id = match self.order_client_id_successor.take() {
                Some(next) => next,
                None => self.mint_order_client_id(route, engine_index, &req),
            };
            minted_at = (self.order_client_id_gen.sequence() == drawn + 1).then_some(drawn);
        }
        // Minted-order-client-id `execution_order` gap: the write-ahead `Command` that carried this submit had
        // an EMPTY order client id (the mint just happened, AFTER that record), so journal the now-
        // RESOLVED request so a reader of the journal can tie the minted order client id to its terms even for
        // an order that terminalizes without ever filling. Only when we actually minted (an
        // explicit-order-client-id submit is already fully resolved in its write-ahead `Command`). Cold-ish
        // relative to the p99 event fold — submits are orders, not market data — and gated on
        // journaling being on, so the desktop/no-journal path is byte-identical.
        if minted {
            self.journal_minted_submit(engine_index, &req, now);
        } else if self.refuse_explicit_order_client_id(
            engine_index,
            &req.order_client_id,
            releasing,
        ) {
            // **AN EXPLICIT ORDER CLIENT ID THE NODE ALREADY HOLDS OR REMEMBERS IS REFUSED** (decisions 0207 and
            // 0270), before anything below can REPLACE its registry entry: `synthesize_capability_reject`
            // and `gate_and_register` both insert under the order client id, which hides a resting order behind a
            // fresh `INITIALIZED` one nothing will ever answer. The door refuses the same reuse from the
            // PUBLISHED snapshot; this is the backstop for the window its coalesced publish leaves.
            //
            // Asked only for a submit that brought its own order client id (an operator's wire command): a minted
            // order client id (every strategy order, a fired stop, a flatten) skips this branch on the `minted`
            // bool already in hand. An UNANSWERED order (`INITIALIZED` or `SUBMITTED`) is not refused:
            // that is the in-flight retry the gatewayhub's dedup exists for, and `run_submit_hop`
            // depends on it (its probe never answers). `refuse_explicit_order_client_id` says what else refuses.
            self.pump_client();
            return Vec::new();
        }
        let order_client_id = req.order_client_id.clone();
        if self.order_client_id_row_needed(engine_index) {
            self.order_client_id_venue.insert(order_client_id.clone(), engine_index);
        }
        self.engine_mut(engine_index).now_ms = now;
        // A STOPPED account (decision 0169) refuses, whoever sent it (a strategy included).
        if self.refuse_for_stopped_account(engine_index, &req, now) {
            self.pump_client();
            return vec![order_client_id];
        }
        // A held order whose `gtd` lifetime ended while it was held EXPIRES instead of being sent (the
        // one expiry law, `time_in_force_expired`; a `day` order's day starts at this registration).
        if releasing
            && vike_model::time_in_force_expired(req.time_in_force, req.gtd_expiry, now, now)
        {
            let refusal = crate::runtime::refusals::CoreOrderRefusal::ExpiredBeforeRelease {
                gtd_expiry: req.gtd_expiry.unwrap_or_default(),
                now,
            };
            let refusal_id = vike_model::events::refusal_id_of(refusal.id().word());
            self.synthesize_capability_reject(
                engine_index,
                &req,
                &refusal.to_string(),
                refusal_id,
                now,
            );
            self.pump_client();
            return vec![order_client_id];
        }
        // Capability preflight (w2-task-5), the sibling of the Combo arm's supports_combo
        // gate: an order the venue's declared `VenueCapabilities` row cannot honor is refused HERE
        // — a synthesized terminal lifecycle with a machine-readable reason — instead of
        // reaching the adapter to be silently coerced (a "stop" firing as an immediate
        // market, a non-GTC TIF resting GTC). AFTER the journal write on purpose (a
        // refused order is exactly the audit trail worth keeping — the combo-arm
        // doctrine); the order client id is still returned, matching this arm's RiskGate-denial
        // shape (the refusal events NAME the order).
        //
        // The caps row comes from the ROUTED ENGINE (`Self::capabilities_venue`), not from
        // `req.venue` — the request's venue string is what routed it, and routing is a
        // per-ACCOUNT question while a caps row is a per-EXCHANGE fact. Byte-identical
        // today (a routed request's venue IS the engine's), and the reason it is spelled
        // this way is in `capabilities_venue`'s own doc: reading the routing string would let a
        // per-account key silently skip every check below rather than fail closed.
        // Scoped so the `&self` borrow ends before the `&mut self` refusal path; no
        // allocation on the accepted path (the `Result` is `Ok(())`).
        //
        // The engine's STATED tier rides along (`Engine::stated_tier`: two field loads and a
        // compare on a value this arm already reads the engine for): a `paper` book is not judged by
        // the row's reduce-only kinds, which are the LIVE adapter's refusals (decision 0201).
        let tier = self.engine(engine_index).stated_tier();
        // An order kind the venue does not hold that the engine EMULATES (decision 0275,
        // `emulated.rs`) is judged as its CHILD would be: a plain limit with its lifetime and flags. Two
        // compares on every other order.
        let capabilities_venue = self.capabilities_venue(routed, &req.venue);
        let emulation = vike_venues::emulation_of(&req, capabilities_venue, tier);
        let deny = match emulation {
            None => vike_venues::preflight_order_at(&req, capabilities_venue, tier).err(),
            Some(_) => vike_venues::preflight_order_at(
                &vike_venues::emulated_child(&req),
                capabilities_venue,
                tier,
            )
            .err(),
        };
        if let Some(deny) = deny {
            self.synthesize_capability_reject(
                engine_index,
                &req,
                &deny.to_string(),
                vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueCapability.word(),
                ),
                now,
            );
            self.pump_client();
            return vec![order_client_id];
        }
        // A lone OUO leg is judged as the list it belongs to would be (`lower_order_list`), and once
        // accepted it raises the journal's format as an accepted list does: one discriminant compare
        // on every other submit.
        if req.contingency_type == Some(vike_model::ContingencyType::Ouo) {
            let refusal = (tier != Some(vike_model::AccountTier::Paper))
                .then(|| {
                    vike_venues::order_list_refusal(
                        self.capabilities_venue(routed, &req.venue),
                        std::slice::from_ref(&req),
                    )
                })
                .flatten();
            if let Some(reason) = refusal {
                self.synthesize_capability_reject(
                    engine_index,
                    &req,
                    &reason,
                    vike_model::events::refusal_id_of(
                        vike_model::OrderRefusalId::VenueCapability.word(),
                    ),
                    now,
                );
                self.pump_client();
                return vec![order_client_id];
            }
            if let Some(journal) = self.journal.as_mut() {
                journal.stamp_order_list_version();
            }
        }
        self.note_paper_admit(routed, &req);
        // The order PASSED pre-trade: record the delay label of the data it was priced off
        // (decision 0140) with it. A RECORD, never a gate — nothing below reads it to refuse or
        // hold — and the core's own table is the authority, so a caller's value is overwritten.
        // One `is_empty` branch on a core whose feeds label nothing; one note per order priced off
        // data that is not real-time (`runtime/data_delay.rs`).
        self.stamp_data_delay(&mut req);
        // An order with a RELEASE is held by the core until it holds (`held_release.rs`): accepted and
        // preflighted above, sent at its release, unless its venue holds a time release itself (the row's
        // `release` word: then it goes now, with its release). One `Option` test on every other submit.
        if req.release.is_some() && !self.release_held_by_venue(routed, &req) {
            self.hold_for_release(engine_index, req);
            self.pump_client();
            return vec![order_client_id];
        }
        // An EMULATED order is armed in the core's own book instead of sent (`emulated.rs`): one
        // `Option` test on every other submit.
        if let Some(emulated) = emulation {
            // its records reach the journal before a child can leave, or it ends NOT SENT
            if candidate.is_some() && !self.frame_held_submits() {
                self.synthesize_capability_reject(
                    engine_index,
                    &req,
                    vike_execution::execution_engine::JOURNAL_CANNOT_WRITE_REASON,
                    vike_model::events::refusal_id_of(
                        vike_model::OrderRefusalId::JournalCannotWrite.word(),
                    ),
                    now,
                );
                self.pump_client();
                return vec![order_client_id];
            }
            self.arm_emulated(engine_index, route, req, emulated, now);
            self.pump_client();
            return vec![order_client_id];
        }
        // Live-runtime OTO/OCO submit-hold: a request that carries contingency linkage is
        // recorded in the shared book (byte-identical: plain orders are never inserted). If
        // it is a HELD exit (has a parent that has not filled), it must NOT go live to the
        // venue yet — store its resolved request and return; its parent's fill releases it
        // (`drive_contingency_on_fill`). An ENTRY / link-free leg (not held) falls through to
        // the normal submit below. `order_client_id_venue` is already recorded above, so a later
        // release/cancel routes to the right engine.
        if has_contingency_links(&req) {
            self.contingency.insert(
                order_client_id.clone(),
                req.contingency_type,
                req.parent_order_id.clone(),
                req.linked_order_ids.clone(),
            );
            if self.contingency.is_held(&order_client_id) {
                self.note(format!("OTO child {order_client_id} held pending parent fill"));
                self.held_orders.insert(order_client_id.clone(), req);
                return vec![order_client_id];
            }
            // A non-held leg (an OTO entry) still goes to the venue — but as a PLAIN order:
            // the core owns the OTO/OCO emulation in its book, so the venue must not
            // re-interpret the linkage (an OCO-enforcing client would otherwise re-hold or
            // double-manage it). See `strip_contingency_links`.
            req = strip_contingency_links(req);
        }
        let mut outbox = EventBusOutbox::default();
        match candidate {
            // a strategy's submit: its held records and a repeated denial's count decided at the verdict
            Some(c) => self.submit_strategy_order(
                (engine_index, route),
                &mut req,
                (c, minted_at),
                now,
                &mut outbox,
            ),
            None => self.engine_mut(engine_index).submit_order(&req, now, &mut outbox),
        }
        // Drive the outbox contingency-aware: a linked ENTRY vetoed HERE by the RiskGate
        // (`OrderDenied`) must cascade-drop its already-held children, not orphan them.
        self.publish_and_drive_outbox(engine_index, outbox, now, std::slice::from_ref(&req));
        self.coalesced_denial = None;
        self.pump_client();
        // a counted denial's id is the one its range gave it
        vec![req.order_client_id]
    }

    /// The [`OrderIntent::SubmitBatch`] arm of [`Self::apply_intent_routed`]: a batch whose every
    /// leg resolves engine 0 is minted (or its explicit id refused as a single submit's would be),
    /// journaled and preflighted per leg and submitted to engine 0 as one batch; any other batch
    /// re-enters the dispatcher as one `Submit` per leg.
    pub(super) fn lower_submit_batch(
        &mut self,
        mut reqs: Vec<OrderRequest>,
        now: i64,
        cancel_intent: OrderCancelIntent,
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
        // A stopped PRIMARY (decision 0169) takes no batch: each leg re-enters the single-submit arm, which refuses it.
        let all_primary = !self.is_retired(0)
            && reqs.iter().all(|r| {
                r.account.is_none()
                    // a leg with a release is held by the single-submit arm, never batched out now
                    && r.release.is_none()
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
        // Engine 0 is a LABELLED account only when no default account runs: then each leg is recorded
        // (`order_client_id_row_needed`), as the single-submit arm records it. False on every other node.
        let row_at_zero = self.order_client_id_row_needed(0);
        // A leg that brought its own order client id asks the single submit's question (`refuse_explicit_order_client_id`): one the
        // node holds answered or remembers from before its restart is refused in those words and leaves the
        // batch here, before its row or its journal record; the legs that pass go on as one batch.
        reqs.retain_mut(|r| {
            let was_minted = r.order_client_id.is_empty();
            if was_minted {
                r.order_client_id = self.mint_order_client_id(route, 0, r);
            } else if self.refuse_explicit_order_client_id(0, &r.order_client_id, false) {
                return false;
            }
            if row_at_zero {
                self.order_client_id_venue.insert(r.order_client_id.clone(), 0);
            }
            // Same minted-order-client-id gap closure as the single-submit arm: journal each server-
            // minted request's resolved terms so its `execution_order` row can be seeded even when
            // it never fills (the explicit-order-client-id legs are already resolved in the write-ahead).
            //
            // The route key is engine 0's — `all_primary` above is exactly the statement
            // that every leg resolves it, and since that predicate asks `== Some(0)` the
            // statement now holds for every leg that reaches here rather than for every leg
            // but an unroutable one — so `journal_route_key` answers `None` on every box
            // whose engine 0 is its venue's default account, which is every box.
            if was_minted {
                self.journal_minted_submit(0, r, now);
            }
            minted.push(r.order_client_id.clone());
            true
        });
        self.engine.now_ms = now;
        // Capability preflight per leg (w2-task-5): a refused leg gets its synthesized
        // terminal lifecycle (same shape as the Submit arm); the survivors proceed as one
        // batch. The mixed-venue path above needs no twin of this — each of its legs
        // re-enters the Submit arm, which preflights it there.
        //
        // Caps row from the ROUTED engine, per leg, exactly as the Submit arm does. It is
        // spelled per LEG rather than hoisted to `self.engine.venue` because the Submit arm
        // spells it that way and the two arms must not drift — `capabilities_venue`'s own doc is
        // the argument for the shape. It is NOT a hedge against a leg that resolves no
        // engine: `all_primary` above asks `== Some(0)`, so a leg naming a venue no engine
        // answers for never reaches this loop at all. It answers `false`, every leg
        // re-enters the Submit arm, and the identical `capabilities_venue(routed, …)` call THERE
        // keeps it on its own string (the unknown-venue affordance).
        let mut reqs: Vec<_> = reqs
            .into_iter()
            .filter(|r| {
                let routed = self.route_of(route, &r.venue);
                let deny = {
                    let tier = self.engine(routed.unwrap_or(0)).stated_tier();
                    vike_venues::preflight_order_at(
                        r,
                        self.capabilities_venue(routed, &r.venue),
                        tier,
                    )
                    .err()
                };
                match deny {
                    None => {
                        self.note_paper_admit(routed, r);
                        true
                    }
                    Some(deny) => {
                        self.synthesize_capability_reject(
                            0,
                            r,
                            &deny.to_string(),
                            vike_model::events::refusal_id_of(
                                vike_model::OrderRefusalId::VenueCapability.word(),
                            ),
                            now,
                        );
                        false
                    }
                }
            })
            .collect();
        // The survivors passed pre-trade: each records its delay label, as in the Submit arm.
        for r in &mut reqs {
            self.stamp_data_delay(r);
        }
        let mut outbox = EventBusOutbox::default();
        self.engine.submit_order_batch(&reqs, now, &mut outbox);
        // Contingency-aware drain (all-primary, so idx 0): a batched linked leg vetoed here
        // drives the cascade too — same guarantee as the single-submit path. Inert unless a
        // leg names a book entry (empty book ⇒ classifier no-op).
        self.publish_and_drive_outbox(0, outbox, now, &reqs);
        self.pump_client();
        minted
    }

    /// The [`OrderIntent::Bracket`] arm of [`Self::apply_intent_routed`]: resolve the spec's
    /// account, refuse an ambiguous venue, take the sender's stem (refused whole when it is in use) or
    /// mint one, derive the three legs' order client ids from it, journal all three legs, preflight
    /// the bracket atomically, hold the exits and submit the entry.
    pub(super) fn lower_bracket(
        &mut self,
        spec: OrderBracket,
        now: i64,
        route: EngineRoute,
    ) -> Vec<String> {
        // **THE SPEC NAMES ITS ACCOUNT, resolved BEFORE the ambiguity gate below** — `lower_submit`'s
        // step for the same reason, and its argument is not repeated here: `routed_engine` answers
        // `None` for `EngineRoute::Payload`, so a bracket on a venue that runs two engines (a lane
        // twin beside the spot engine, or a labelled account) was refused as ambiguous whatever it
        // named. Resolving first hands the gate, the journal's route key, `capabilities_venue` and `route_of`
        // an `EngineRoute::Engine`. An unheld account is refused BY NAME before anything is minted
        // and never falls through to the venue's default book (`route_for_payload_account`).
        //
        // ⚠ `account: None` short-circuits to `route` and reaches none of this: a mounted strategy's
        // bracket (its route is the mount's engine, which `route_for_payload_account` also lets win)
        // and every spec written before the field take the path they always took.
        let route = match spec.account.as_ref() {
            None => route,
            Some(account) => match self.route_for_payload_account(route, &spec.venue, account) {
                Some(resolved) => resolved,
                None => {
                    self.refuse_unheld_account("bracket", &spec.venue, account);
                    self.pump_client();
                    return Vec::new();
                }
            },
        };
        // §4.2 row 1, the risk-INCREASING side of §4.5's law — the `Submit` arm's twin, and
        // refused BEFORE the three order client ids are minted for the same sequence-gap reason. A
        // bracket opens exposure; it is not a reducing verb even though two of its three
        // legs are protective, because the ENTRY is what decides the direction.
        if let Some(candidates) = self.ambiguous_accounts(route, &spec.venue) {
            self.refuse_ambiguous("bracket", &spec.venue, &candidates);
            self.pump_client();
            return Vec::new();
        }
        let routed = self.route_of(route, &spec.venue);
        let engine_index = routed.unwrap_or(0);
        // ONE stem for the group (decision 0270): the SENDER's (`OrderBracket::order_client_id`, so a resend
        // names the same three legs), refused whole BEFORE anything is journaled when it is no v2 stem or a
        // leg of it is in use; else minted on the routed engine's plan, one sequence number (a v1 session's
        // stem mints each leg whole, entry, stop, take-profit, as before).
        let stem = match spec.order_client_id.as_deref().filter(|s| !s.is_empty()) {
            None => self.mint_stem(route, engine_index),
            Some(given) => match OrderClientIdStem::given(given) {
                None => {
                    let why = format!(
                        "its order client id {given} is not a group stem (a v2 id through its \
                         unique part)"
                    );
                    return self.refuse_bracket_stem(&spec, &why);
                }
                Some(stem) => {
                    if let Some(leg) =
                        self.stem_in_use(engine_index, &stem, &OrderClientIdLegRole::BRACKET)
                    {
                        self.refuse_order_id_in_use(engine_index, &leg);
                        return self.refuse_bracket_stem(
                            &spec,
                            &format!("its leg id {leg} is already used"),
                        );
                    }
                    stem
                }
            },
        };
        let entry_intent =
            self.order_client_id_intent(engine_index, spec.side, false, &spec.symbol);
        let exit_intent = self.order_client_id_intent(engine_index, -spec.side, true, &spec.symbol);
        let [en, stop_loss_role, take_profit_role] = OrderClientIdLegRole::BRACKET;
        let (entry, stop_loss, take_profit) = (
            stem.leg(&mut self.order_client_id_gen, entry_intent, en),
            stem.leg(&mut self.order_client_id_gen, exit_intent, stop_loss_role),
            stem.leg(&mut self.order_client_id_gen, exit_intent, take_profit_role),
        );
        // ⚠ The three order client ids get their `order_client_id_venue` row exactly when [`CoreThread::order_client_id_row_needed`] says so: a
        // non-primary engine, or an engine 0 that is a LABELLED account (route key `venue#LABEL`), however the
        // bracket reached it (it NAMED the account, or a mount routed it). The exits are released long after the
        // caller that knew the account is gone, and `held_release_engine`'s fallback re-derives the engine from
        // the VENUE: on a node whose engine 0 is a labelled account (the default one inactive at boot) that
        // later starts the default account at another index, the fallback is the DEFAULT engine, and both
        // reduce-only exits would go to a book that holds no position while the labelled one is left naked. A
        // default account at engine 0 keeps its cost of nothing; a row costs three map entries per bracket, a
        // command and not a message.
        if self.order_client_id_row_needed(engine_index) {
            self.order_client_id_venue.insert(entry.clone(), engine_index);
            self.order_client_id_venue.insert(stop_loss.clone(), engine_index);
            self.order_client_id_venue.insert(take_profit.clone(), engine_index);
        }
        let mut orders = build_bracket(&spec, &entry, &stop_loss, &take_profit);
        for o in orders.iter_mut() {
            o.timestamp = now;
        }
        // Same minted-order-client-id gap closure as the Submit/SubmitBatch arms: all THREE bracket
        // legs (entry, SL, TP) are ALWAYS node-made here (minted, or derived from a sender's
        // stem, never a caller's whole id), and the write-ahead `Command`/`StrategySubmit`
        // record for this intent carries the pre-mint `OrderBracket` with no order client ids at all —
        // so without this, a leg that terminalizes without ever filling (e.g. the SL/TP
        // sibling canceled by its OCO partner's fill) has no recorded terms for a reader of
        // the journal to find. Gated on journaling being on; borrowed serialize; counts
        // against the same `journaled_since_snap` cadence. Cold path (order submission, not
        // the market-event fold), so the p99 gate is untouched.
        let rk = self.journal_route_key(engine_index, &spec.venue);
        if let Some(journal) = self.journal.as_mut() {
            for o in orders.iter() {
                journal.append_minted_submit(now, o, rk.as_deref()).expect("journal append");
                self.journaled_since_snap += 1;
            }
        }
        self.engine_mut(engine_index).now_ms = now;
        // A STOPPED account refuses the WHOLE bracket (`refuse_for_stopped_account`): a mounted strategy
        // reaches it here through `EngineRoute::Mount`, past the edge's refusal.
        if self.is_retired(engine_index) {
            for o in &orders {
                self.refuse_for_stopped_account(engine_index, o, now);
            }
            self.pump_client();
            return vec![entry, stop_loss, take_profit];
        }
        // Capability preflight (w2-task-5), ATOMIC over the bracket: a bracket with a
        // child the venue's declared row cannot hold is refused WHOLE — submitting the
        // entry while refusing its protective stop would strand a naked position, and
        // letting the child through invites the silent-coercion class (a "stop" firing
        // immediately as market on a venue with no native trigger). The culprit carries
        // its machine-readable reason; each sibling carries `BRACKET_ATOMIC_REFUSED:
        // culprit=<order client id> leg=<name> venue=<venue>` so the group refusal is traceable from any leg.
        // The preflight also reads each leg's `reduce_only` against the row's `reduce_only_kinds`
        // (`PreflightDeny::ReduceOnly`): the exits are HELD until the entry fills and are never
        // preflighted again (`submit_resolved`), so this is the only place a venue that cannot
        // rest a reduce-only stop or limit (IG, Dukascopy's limit) can be told before the entry
        // is sent.
        // Caps row from the ROUTED engine, as in the Submit arm — every leg carries the
        // bracket's own `spec.venue`, so one resolution covers all three. Scoped so the
        // `&self` borrow ends before the `&mut self` refusal loop below.
        //
        // A `paper` engine's bracket is not judged by the row's reduce-only kinds (they are the LIVE
        // adapter's refusals; the simulator holds both exits), decision 0201: the engine's stated tier
        // rides along, and each admitted leg that the venue's row would have refused is said once.
        let tier = self.engine(engine_index).stated_tier();
        let refusal = {
            let capabilities_venue = self.capabilities_venue(routed, &spec.venue);
            orders.iter().enumerate().find_map(|(i, o)| {
                vike_venues::preflight_order_at(o, capabilities_venue, tier)
                    .err()
                    .map(|d| (i, d, capabilities_venue.to_string()))
            })
        };
        if let Some((ci, deny, venue)) = refusal {
            let culprit = orders[ci].order_client_id.clone();
            let reason = deny.to_string();
            // `build_bracket` fixes the leg order. The leg and the venue are on every sibling's reason
            // (the culprit's own names the kind and the venue), so whichever row an operator reads says
            // which exit the venue cannot hold, and where.
            let leg = BRACKET_LEGS[ci];
            let sibling_reason =
                format!("BRACKET_ATOMIC_REFUSED: culprit={culprit} leg={leg} venue={venue}");
            for (i, o) in orders.iter().enumerate() {
                let r = if i == ci { &reason } else { &sibling_reason };
                self.synthesize_capability_reject(
                    engine_index,
                    o,
                    r,
                    vike_model::events::refusal_id_of(
                        vike_model::OrderRefusalId::VenueCapability.word(),
                    ),
                    now,
                );
            }
            self.note(format!(
                "bracket for {venue} REFUSED WHOLE, nothing was sent: its {leg} leg is refused ({reason})"
            ));
            self.pump_client();
            return vec![entry, stop_loss, take_profit];
        }
        self.place_linked_group(engine_index, routed, Vec::from(orders), "bracket exit", now);
        vec![entry, stop_loss, take_profit]
    }

    /// Refuse a bracket whose SENDER's stem cannot be placed (`why`), whole and before anything is
    /// journaled or sent: one ring line, and nothing to name.
    #[cold]
    #[inline(never)]
    fn refuse_bracket_stem(&mut self, spec: &OrderBracket, why: &str) -> Vec<String> {
        self.note(format!(
            "bracket on {} {} REFUSED WHOLE, nothing was sent: {why}",
            spec.venue, spec.symbol
        ));
        self.pump_client();
        Vec::new()
    }

    /// The tail [`Self::lower_bracket`] and `lower_order_list` share once a linked group passed its
    /// atomic preflight WHOLE: say each paper admit, stamp each leg's delay label, record every leg's
    /// links in the book, HOLD every leg with an unfilled parent (`held` names it on the note: `<held>
    /// <order client id> held pending entry fill`, words `vike-cli`'s one-shot reads for a bracket), then submit
    /// the rest as plain orders.
    pub(super) fn place_linked_group(
        &mut self,
        engine_index: usize,
        routed: Option<usize>,
        mut orders: Vec<OrderRequest>,
        held: &str,
        now: i64,
    ) {
        for o in &orders {
            self.note_paper_admit(routed, o);
        }
        // Live-runtime OTO/OCO submit-hold (the WHOLE bracket): record all three legs'
        // linkage in the shared book, then submit ONLY the OTO entry and HOLD the protective
        // exits off the venue until the entry fills (`drive_contingency_on_fill` releases
        // them). This is the emulation the feature exists for — a stop-loss / take-profit
        // sitting live at the venue before the entry fills is a naked order that can trigger
        // and open an unwanted position; holding it is what makes the bracket an atomic OTO.
        // `build_bracket` stamps the entry active (no parent) and sl/tp held (parent =
        // entry), so the split falls straight out of `is_held`. `order_client_id_venue` for all three is
        // already recorded above, so a later release/cancel routes correctly.
        self.engine_mut(engine_index).now_ms = now;
        // The bracket passed pre-trade whole: every leg records its delay label (decision 0140),
        // as in the Submit arm, and each leg not real-time gets its own note.
        for o in &mut orders {
            self.stamp_data_delay(o);
        }
        for o in &orders {
            if has_contingency_links(o) {
                self.contingency.insert(
                    o.order_client_id.clone(),
                    o.contingency_type,
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
            let ocoid = o.order_client_id.clone();
            if self.contingency.is_held(&ocoid) {
                self.note(format!("{held} {ocoid} held pending entry fill"));
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
            // a leg live at once beside its linked legs (an OCO pair) is one claim on a spot holding
            let linked = self.contingency.linked_exits(&plain.order_client_id);
            self.engine_mut(engine_index).linked_exits = linked;
            let mut outbox = EventBusOutbox::default();
            self.engine_mut(engine_index).submit_order(&plain, now, &mut outbox);
            self.engine_mut(engine_index).linked_exits = None;
            self.publish_and_drive_outbox(engine_index, outbox, now, std::slice::from_ref(&plain));
        }
        self.pump_client();
    }

    /// Journal one SERVER-MINTED request's resolved terms (`append_minted_submit`) under the route
    /// key of the engine it was routed to — the write [`Self::lower_submit`],
    /// [`Self::lower_submit_batch`] and [`Self::lower_combo`] each make once they have minted a
    /// order client id. Each caller keeps its own `if minted` gate.
    ///
    /// [`Self::lower_bracket`] keeps its own spelling of this write: it resolves ONE route key for
    /// all three legs and appends them inside one journal borrow, so calling this once per leg
    /// would resolve that key three times (and allocate it three times on a labelled engine).
    pub(super) fn journal_minted_submit(
        &mut self,
        engine_index: usize,
        req: &OrderRequest,
        now: i64,
    ) {
        // ⚠ The route key is resolved BEFORE the journal borrow, not inside it: the
        // `&self` read of the routed engine and the `&mut self.journal` of the append
        // cannot overlap. `None` on every single-account box, so this allocates nothing
        // there whether journaling is on or off.
        let rk = self.journal_route_key(engine_index, &req.venue);
        // an amend's new order names the one it replaces (set around its submit by `place_replacement`),
        // so a restore gives it that order's owner with no `StrategySubmit` of its own
        let replaces = self.engine(engine_index).replacing.clone();
        if let Some(journal) = self.journal.as_mut() {
            // a HELD record counts toward the cadence when it is framed (`frame_held_submits`), never when dropped
            let held = journal.deferred_submits().is_some();
            journal
                .append_minted_submit_replacing(now, req, rk.as_deref(), replaces.as_deref())
                .expect("journal append");
            self.journaled_since_snap += u64::from(!held);
        }
    }
}

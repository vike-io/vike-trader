//! The emulated-conditional arms of `apply_intent_routed` — `ArmConditional` and
//! `DisarmConditional` — one method each.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// The [`OrderIntent::ArmConditional`] arm of [`Self::apply_intent_routed`]: refuse an
    /// ambiguous venue or an `Index` trigger, then mint an arm id, journal the arm and add it to its
    /// book. Arms nothing that reaches a venue, so it always returns an empty list.
    pub(super) fn arm_conditional(
        &mut self,
        c: ConditionalIntent,
        now: i64,
        route: EngineRoute,
    ) -> Vec<String> {
        self.arm_conditional_as(c, false, now, route);
        Vec::new()
    }

    /// [`Self::arm_conditional`] with the arm's `reduce_only` standing, and the minted arm id back
    /// (`None` = refused or ignored, as the note said). `reduce_only` is true for exactly two callers,
    /// [`Self::arm_core_held_stop`] (the operator's reduce-only stop on a venue with no native reduce-only
    /// stop) and the sweep that arms such a stop again after its close failed
    /// (`CoreThread::rearm_core_held_stop`, `core_held.rs`).
    /// It rides the journaled [`vike_journal::ConditionalRecord`] and the arm's [`ArmedBy`], and the
    /// fire reads it (`submit_fired`).
    pub(crate) fn arm_conditional_as(
        &mut self,
        c: ConditionalIntent,
        reduce_only: bool,
        now: i64,
        route: EngineRoute,
    ) -> Option<String> {
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
            return None;
        }
        let eidx = self.route_of(route, &venue).unwrap_or(0);
        // The mount whose strategy armed this, when a strategy did: the fire's order is THAT
        // strategy's own. An operator's arm (`Payload`) and a core-built one (`Engine`) have none.
        let mount = match route {
            EngineRoute::Mount(i) => Some(i),
            EngineRoute::Payload | EngineRoute::Engine(_) => None,
        };
        // Trigger-source gate (w2 trigger_by): the core has NO index lane, so an Index
        // arm could never fire — REFUSED here (before minting an id, like the
        // no-mark trailing refusal below), surfaced to recent-events, never armed
        // inert and never silently evaluated off a different series.
        if trigger_by == Some(vike_model::TriggerBy::Index) {
            self.note(
                "ArmConditional REFUSED: trigger_by Index — the core has no index lane".into(),
            );
            return None;
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
                return None;
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
                    reduce_only,
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
                // WHICH ACCOUNT this arm protects, and WHICH MOUNT armed it, recorded at the
                // one moment both are known — see `CoreThread::cond_engine`. Only for an arm
                // that actually entered a book, so the map cannot outlive its arm.
                self.cond_engine.insert(
                    arm_id.clone(),
                    ArmedBy { engine: eidx, mount, reduce_only, covered: false, rearms: 0 },
                );
                self.tick_conditionals |= reduce_only;
                return Some(arm_id);
            }
            self.note(format!("ArmConditional: duplicate arm id {arm_id} — REFUSED"));
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
                    reduce_only,
                },
            );
            if self
                .conditional_books
                .entry((venue, symbol))
                .or_default()
                .add_stop(&arm_id, side, qty, px, trigger_by)
            {
                // The stop twin of the trailing arm above — see `CoreThread::cond_engine`.
                self.cond_engine.insert(
                    arm_id.clone(),
                    ArmedBy { engine: eidx, mount, reduce_only, covered: false, rearms: 0 },
                );
                self.tick_conditionals |= reduce_only;
                return Some(arm_id);
            }
            self.note(format!("ArmConditional: duplicate arm id {arm_id} — REFUSED"));
        } else {
            self.note("ArmConditional: neither price nor trail set — ignored".to_string());
        }
        None
    }

    /// The [`OrderIntent::DisarmConditional`] arm of [`Self::apply_intent_routed`]: find the arm's
    /// book by probing for its id, journal the disarm, then remove it. Always returns an empty list.
    pub(super) fn disarm_conditional(&mut self, arm_id: String, now: i64) -> Vec<String> {
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
                self.refresh_tick_conditionals();
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

    /// Recompute [`CoreThread::tick_conditionals`]: the profile's `guards.conditionals_on_ticks`, or a
    /// core-held reduce-only stop still rests. Called wherever an arm leaves `cond_engine` (a raise
    /// is done inline at the arm); command cadence, a scan of the few armed rows.
    pub(crate) fn refresh_tick_conditionals(&mut self) {
        self.tick_conditionals =
            self.config.conditionals_on_ticks || self.cond_engine.values().any(|a| a.reduce_only);
    }

    /// **A reduce-only STOP on a venue with NO native reduce-only stop is held by the core
    /// instead of refused** (the owner rule of 2026-10-26: a stop-loss on every venue) — the operator's
    /// `--type stop --reduce-only` AND a mounted strategy's `LiveBroker::submit_stop_reduce`, which lower
    /// through the same arm and differ only in the route. Called by
    /// [`Self::lower_submit`] BEFORE it mints a coid. `true` = the submit was taken here (armed):
    /// nothing is minted and nothing reaches a venue. `false` = not this verb, or the arm was refused
    /// (its own note says why), and `lower_submit` goes on to its unchanged mint and capability
    /// refusal, so the operator still gets the terminal `OrderRejected`.
    ///
    /// Eligible: a `stop`, `reduce_only`, with a trigger price and quantity, no contingency links, and
    /// the ROUTED engine's capability row refuses exactly this order: the kind (`PreflightDeny::Trigger`,
    /// no `stop` in `supported_order_kinds`) or the kind AS a reduce-only one (`PreflightDeny::ReduceOnly`,
    /// `stop` is an entry kind but not in `reduce_only_kinds`: IG, decision 0199's amendment of 2026-10-27).
    /// So it is the capability map that decides, per venue: a venue whose row takes a reduce-only `stop`
    /// never gets here (it takes its native stop), and a venue that gains one retires this path with no
    /// other edit. A BRACKET's stop-loss never comes here (`lower_bracket` preflights its legs and refuses
    /// the whole bracket), so a bracket on such a venue stays refused whole.
    /// A stop that is NOT reduce-only (a breakout ENTRY) stays refused; a trailing one is strategy-only
    /// (`LiveBroker::submit_trailing`, plain: it has no reduce-only form).
    ///
    /// **A MOUNTED strategy's reduce-only stop is armed the same way** (`EngineRoute::Mount`): the arm
    /// records the mount (`ArmedBy::mount`), so the fire's market is that strategy's own order (its fills,
    /// its `on_order_event`), and the arm is journaled under the mount's `StrategySubmit` exactly as a
    /// strategy's `ArmConditional` is, so a restart gives it back to the same mount
    /// (`replay::fold_coid_origins`). The strategy is told nothing at the arm (there is no coid to hang an
    /// event on): the note names the mount, and a refusal reaches it as the `OrderRejected` the unchanged
    /// mint and capability refusal produce.
    ///
    /// The decision reads nothing but the request and the capability table, never a setting: this is
    /// on the write-ahead replay path (the operator's `Cmd` is re-applied through here), and a branch on
    /// a config value the replay core does not carry would arm live and refuse in replay.
    ///
    /// The terms are the existing arm's (`arm_conditional_as`): journaled `ConditionalArmed`, the
    /// engine and the flag in [`ArmedBy`]; the fire is `submit_fired`'s, capped to the position.
    pub(super) fn arm_core_held_stop(
        &mut self,
        req: &OrderRequest,
        now: i64,
        route: EngineRoute,
    ) -> bool {
        let Some(trigger) = req.trigger_price.filter(|t| t.is_finite() && *t > 0.0) else {
            return false;
        };
        if req.order_type != vike_model::OrderType::Stop
            || !req.reduce_only
            || !(req.qty.is_finite() && req.qty > 0.0)
            || req.parent_order_id.is_some()
            || !req.linked_order_ids.is_empty()
            || !req.combo_legs.is_empty()
        {
            return false;
        }
        let routed = self.route_of(route, &req.venue);
        // A STOPPED account (decision 0169) is refused `ACCOUNT_NOT_RUNNING` by the submit tail, whatever
        // the order is: do not arm onto it, let the caller's refusal say so.
        if self.is_retired(routed.unwrap_or(0)) {
            return false;
        }
        // The row refuses this reduce-only stop in one of TWO ways, and the core holds it in both: the row
        // has no `stop` at all (`Trigger`: polymarket), or it takes a stop as an ENTRY order but its adapter
        // places no reduce-only one (`ReduceOnly`: IG, whose close endpoint rests nothing,
        // `VenueCaps::reduce_only_kinds`). The fire below is a reduce-only MARKET, which every row lists
        // (`vike-venues`' `reduce_only_kinds_are_a_subset_of_the_kinds_and_keep_the_market_close`), so the
        // venue is never asked for what it refuses. The kind is `stop` for sure: `order_type` was tested above.
        // The engine's stated tier is passed as `lower_submit` passes it, so the two judgements stay one:
        // `ReduceOnly` is the tier-sensitive axis, and a `paper` engine is not refused it (its stop reaches
        // the simulator, which holds it: decision 0201), so it is not held by the core here.
        let tier = self.eng(routed.unwrap_or(0)).stated_tier();
        let deny = vike_venues::preflight_order_at(req, self.caps_venue(routed, &req.venue), tier);
        if !matches!(
            deny,
            Err(vike_venues::PreflightDeny::Trigger { .. }
                | vike_venues::PreflightDeny::ReduceOnly { .. })
        ) {
            return false;
        }
        let c = ConditionalIntent {
            venue: req.venue.clone(),
            symbol: req.symbol.clone(),
            side: req.side,
            qty: req.qty,
            price: Some(trigger),
            trail: None,
            trigger_by: req.trigger_by,
        };
        let Some(arm_id) = self.arm_conditional_as(c, true, now, route) else {
            return false;
        };
        let eidx = self.cond_engine.get(&arm_id).map_or(0, |a| a.engine);
        let position = self.eng(eidx).gate_position_size(&req.symbol);
        // A stop armed over a position it reduces is COVERED: when that position later goes away the
        // sweep clears the stop (`core_held.rs`). One armed on a flat book waits for its position.
        if vike_model::is_reducing_direction(req.side, position)
            && position.abs() > 1e-12
            && let Some(a) = self.cond_engine.get_mut(&arm_id)
        {
            a.covered = true;
        }
        // Who asked: a mounted strategy cannot cancel the arm by id (it has no coid to name), so say so and
        // what clears it instead.
        let by = match route {
            EngineRoute::Mount(i) => format!(
                " Armed by the strategy mount `{}`, which cancels it with its own mass-cancel.",
                self.mount_ids.get(i).map_or("?", String::as_str)
            ),
            EngineRoute::Payload | EngineRoute::Engine(_) => String::new(),
        };
        let note = format!(
            "CORE-HELD STOP {arm_id} armed: {} {} {} {} when the price crosses {trigger} \
             (reduce-only; the position now is {position}). {} has NO native reduce-only stop, so THIS \
             NODE holds it: it protects only while the node runs, NOT through an outage or a restart gap. At the \
             trigger it sends a market for at most the open position, or drops itself if there is \
             none. Cancel it by this id.{by}",
            if req.side > 0 { "BUY" } else { "SELL" },
            req.qty,
            req.venue,
            req.symbol,
            req.venue,
        );
        tracing::warn!(target: "vike_core::core", arm_id = %arm_id, venue = %req.venue, symbol = %req.symbol, "{note}");
        self.note(note);
        true
    }

    /// An operator's `Cancel` of a core-held stop by its arm id (the id the arm note named): disarm it,
    /// exactly as a held bracket exit is cancelled by [`Self::cancel_held`]. `true` iff `id` was one,
    /// so the caller skips the venue path. Only a core-held (`reduce_only`) row answers: a strategy's
    /// arm has its own verb (`DisarmConditional`). Per cancel, but on the maker's requote path too: an
    /// empty `cond_engine` (every node that arms nothing) costs one `is_empty`.
    pub(crate) fn cancel_armed(&mut self, id: &str, now: i64) -> bool {
        if self.cond_engine.is_empty() || !self.cond_engine.get(id).is_some_and(|a| a.reduce_only) {
            return false;
        }
        self.disarm_conditional(id.to_string(), now);
        true
    }
}

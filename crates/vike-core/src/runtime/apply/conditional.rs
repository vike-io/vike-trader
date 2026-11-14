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
    /// It rides the journaled [`vike_journal::OrderEmulationRecord`] and the arm's [`OrderEmulationArmRoute`], and the
    /// fire reads it (`submit_fired`).
    pub(crate) fn arm_conditional_as(
        &mut self,
        c: ConditionalIntent,
        reduce_only: bool,
        now: i64,
        route: EngineRoute,
    ) -> Option<String> {
        let ConditionalIntent { venue, symbol, side, qty, price, trail, trigger_by, kind_terms } =
            c;
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
        let engine_index = self.route_of(route, &venue).unwrap_or(0);
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
        // The id is minted only once the arm is known good (a trailing arm can still refuse on a
        // missing mark, and terms its kind's law cannot judge arm nothing): a refused arm that
        // burned an id would leave gaps in the sequence, which is exactly what makes a gap
        // diagnostic — an id that exists but names no arm is indistinguishable from a lost
        // record. The RESOLVED terms are journaled BEFORE the book is mutated, the same
        // write-ahead discipline `submit_fired` uses for the FIRE: a crash between the two loses
        // an arm, never hides one.
        // The arm's order: today's trailing stop (`trail`) or stop (`price`) from the legacy fields,
        // any other kind from its `kind_terms` (whose legacy fields are `None`).
        let mut order = match (kind_terms, trail, price) {
            (Some(terms), ..) => terms.working_order(side, qty),
            (None, Some(trail), _) => {
                let mut o =
                    vike_model::OrderResting::new(vike_model::OrderKind::Trailing, side, qty);
                o.trail = Some(trail);
                Some(o)
            }
            (None, None, Some(px)) => {
                let mut o = vike_model::OrderResting::new(vike_model::OrderKind::Stop, side, qty);
                o.price = Some(px);
                Some(o)
            }
            (None, None, None) => {
                self.note("ArmConditional: neither price nor trail set — ignored".to_string());
                return None;
            }
        };
        // A trailing arm with no activation level trails from the mark NOW; one with a level seeds its
        // extreme from the price that reaches it (`crate::emulator`'s module doc).
        if let Some(o) = order.as_mut()
            && (o.trail.is_some() || o.trail_percent.is_some())
            && o.extreme.is_none()
            && o.activation.is_none()
        {
            let mark = self.engine(engine_index).account.mark_of(&venue, &symbol).unwrap_or(0.0);
            if mark <= 0.0 {
                self.note("trailing-stop REFUSED: no mark to seed the extreme".to_string());
                return None;
            }
            o.extreme = Some(mark);
        }
        // The trigger rests where the gate would send it to a venue (the owner's G1): on this
        // engine's grid for the symbol, a stop away from the market, a stop-limit's and a
        // take-profit's as a limit. The journaled arm carries the level it fires at, so a restart
        // restores that level.
        if let Some(o) = order.as_mut() {
            let tick = self.engine(engine_index).gate.limits.grid_for(&symbol).tick_size;
            vike_execution::risk::working_trigger_on_grid(o, tick);
        }
        let Some(armed) = order.and_then(|o| crate::emulator::OrderEmulationArm::of(o, trigger_by))
        else {
            self.note(
                "ArmConditional REFUSED: an arm without a term its kind reads (a stop-limit's limit, \
                 a trailing-limit's offset), or of a kind the core does not hold"
                    .to_string(),
            );
            return None;
        };
        let record = vike_journal::OrderEmulationRecord::of_arm(
            venue.clone(),
            symbol.clone(),
            &armed.order,
            trigger_by,
            reduce_only,
        );
        // Never arm what the journal cannot state whole: a restart (and every `Snapshot`) re-arms from this
        // record, so one that drops a term (a percent trail, a stop's adjustment, a trailing take-profit's
        // kind) would come back as another order. Every arm before the attributes wave restates itself.
        if crate::emulator::OrderEmulationArm::from_record(&record).as_ref() != Some(&armed) {
            self.note(
                "ArmConditional REFUSED: the journal cannot state this arm whole (its kind or one of \
                 its terms), so a restart would re-arm another order"
                    .to_string(),
            );
            return None;
        }
        let arm_id = self.mint_arm_id();
        self.journal_conditional_armed(now, &arm_id, record);
        // A duplicate id cannot occur live (the counter is monotone and, on restart, resumed from
        // the Snapshot's stamped `arm_sequence`) — the book's structural uniqueness is the backstop, and a
        // refusal here would mean that resume contract was violated upstream. Surface it, never
        // panic in the fold.
        if self
            .emulated
            .order_emulation_arms
            .entry((venue, symbol))
            .or_default()
            .add(&arm_id, armed)
        {
            // WHICH ACCOUNT this arm protects, and WHICH MOUNT armed it, recorded at the one
            // moment both are known — see `OrderEmulationBook::cond_engine`. Only for an arm that
            // actually entered a book, so the map cannot outlive its arm.
            self.emulated.cond_engine.insert(
                arm_id.clone(),
                OrderEmulationArmRoute {
                    engine: engine_index,
                    mount,
                    reduce_only,
                    covered: false,
                    rearms: 0,
                },
            );
            self.tick_conditionals |= reduce_only;
            return Some(arm_id);
        }
        self.note(format!("ArmConditional: duplicate arm id {arm_id} — REFUSED"));
        None
    }

    /// The [`OrderIntent::DisarmConditional`] arm of [`Self::apply_intent_routed`]: find the arm's
    /// book by probing for its id, journal the disarm, then remove it. Always returns an empty list.
    pub(super) fn disarm_conditional(&mut self, arm_id: String, now: i64) -> Vec<String> {
        // The individual-cancel twin of the ARM (emulator PR-2). Route by PROBING the
        // books for the id — the intent deliberately carries only `arm_id` (the minted id
        // is globally unique per core: monotone counter, Snapshot-resumed across restarts, and
        // each book refuses duplicates structurally), so the caller does not need to
        // remember the (venue, symbol) it armed on. The probe walks the books map
        // (insertion order, deterministic); books are few (one per armed (venue, symbol))
        // and this is command cadence — never the per-message fold.
        let key = self
            .emulated
            .order_emulation_arms
            .iter()
            .find(|(_, b)| b.contains(&arm_id))
            .map(|(k, _)| k.clone());
        match key {
            Some(key) => {
                // WRITE-AHEAD, the PR-1 ordering discipline: the record goes down BEFORE
                // the book mutation — a crash between the two re-applies the disarm from
                // its own write-ahead `Command`/`StrategySubmit` on restore, so it can be
                // repeated, never lost. Journaled ONLY for an actual disarm (the refusal
                // below writes nothing).
                self.journal_conditional_disarmed(now, &arm_id);
                let removed = self
                    .emulated
                    .order_emulation_arms
                    .get_mut(&key)
                    .map(|b| b.disarm(&arm_id))
                    .unwrap_or(false);
                debug_assert!(removed, "probe found the arm; disarm must remove it");
                // The arm left its book, so its account entry goes with it — see
                // `OrderEmulationBook::cond_engine` for the bound this keeps.
                self.emulated.cond_engine.remove(&arm_id);
                self.refresh_tick_conditionals();
                // Confirm on the same surface the ARM's refusals use (recent-events):
                // armed conditionals have no snapshot view yet, so a silent removal would
                // leave the operator unable to tell a disarm happened at all.
                self.note(format!("conditional {arm_id} DISARMED"));
            }
            None => {
                // Unknown/stale id: a LOUD no-op — never a panic, never silent. Mirrors
                // the ARM's refusal surfacing (`recent`), same stale-click tolerance as
                // `ConditionalArms::disarm` itself. NOT journaled: no state changed.
                self.note(format!("DisarmConditional: unknown arm id {arm_id} — ignored"));
            }
        }
        Vec::new()
    }

    /// Recompute [`CoreThread::tick_conditionals`]: the profile's `guards.conditionals_on_ticks`, or a
    /// core-held reduce-only stop still rests. Called wherever an arm leaves `cond_engine` (a raise
    /// is done inline at the arm); command cadence, a scan of the few armed rows.
    pub(crate) fn refresh_tick_conditionals(&mut self) {
        self.tick_conditionals = self.config.conditionals_on_ticks
            || self.emulated.cond_engine.values().any(|a| a.reduce_only);
    }

    /// **A reduce-only STOP on a venue with NO native reduce-only stop is held by the core
    /// instead of refused** (the owner rule of 2026-10-26: a stop-loss on every venue) — the operator's
    /// `--type stop --reduce-only` AND a mounted strategy's `LiveBroker::submit_stop_reduce`, which lower
    /// through the same arm and differ only in the route. Called by
    /// [`Self::lower_submit`] BEFORE it mints an order client id. `true` = the submit was taken here (armed):
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
    /// A stop that is NOT reduce-only (a breakout ENTRY) stays refused.
    ///
    /// **The same holds for the other four trigger kinds, reduce-only** (wave 3, decision 0199's shape): a
    /// `stop_limit`, a `take_profit` (if-touched, with or without its limit), a `trailing` stop and a
    /// `trailing_limit` (the trail in `OrderRequest::trail`, an activation level in `trigger_price`, the
    /// offset in `OrderRequest::limit_offset`) whose terms `vike_model::order_terms_refusal` admits. The fire
    /// of a stop-limit, a limit-if-touched and a trailing-limit is a reduce-only LIMIT, so such an arm is
    /// held only where the row takes that limit as reduce-only (IG's does not: it stays refused, at accept,
    /// decision 0219's shape). Entries of every kind keep their refusal.
    ///
    /// **And for the attributes wave's three forms, reduce-only** (decision 0267, owner's question 3): a
    /// `trailing_take_profit` (the if-touched mirror of a trailing stop; with a `limit_offset` its fire is a
    /// limit off its trigger), a trailing kind with a PERCENT trail (`OrderRequest::trail_percent`), and a
    /// `stop` or `stop_limit` with an ADJUSTMENT (`OrderRequest::adjust`), each held exactly where the row
    /// refuses its kind, its percent (the row's `percent_trail_kinds`) or `adjust`, and judged by
    /// `vike_model::stop_adjust_refusal` beside the terms rule. An adjustment whose target is a stop-limit fires a
    /// limit, so it is held only where the row takes a reduce-only limit. ⚠ `arm_conditional_as` arms
    /// nothing its journal record cannot restate whole, so a form `vike_execution::ConditionalKindTerms` does not
    /// carry keeps the row's refusal instead of coming back from a restart as another order.
    ///
    /// **A MOUNTED strategy's reduce-only stop is armed the same way** (`EngineRoute::Mount`): the arm
    /// records the mount (`OrderEmulationArmRoute::mount`), so the fire's market is that strategy's own order (its fills,
    /// its `on_order_event`), and the arm is journaled under the mount's `StrategySubmit` exactly as a
    /// strategy's `ArmConditional` is, so a restart gives it back to the same mount
    /// (`replay::fold_order_client_id_origins`). The strategy is told nothing at the arm (there is no order client id to hang an
    /// event on): the note names the mount, and a refusal reaches it as the `OrderRejected` the unchanged
    /// mint and capability refusal produce.
    ///
    /// The decision reads nothing but the request and the capability table, never a setting: this is
    /// on the write-ahead replay path (the operator's `Command` is re-applied through here), and a branch on
    /// a config value the replay core does not carry would arm live and refuse in replay.
    ///
    /// The terms are the existing arm's (`arm_conditional_as`): journaled `OrderEmulationArmed`, the
    /// engine and the flag in [`OrderEmulationArmRoute`]; the fire is `submit_fired`'s, capped to the position.
    pub(super) fn arm_core_held_stop(
        &mut self,
        req: &OrderRequest,
        now: i64,
        route: EngineRoute,
    ) -> bool {
        use vike_model::{OrderKind, OrderType};
        if !req.reduce_only {
            return false;
        }
        let kind = match req.order_type {
            OrderType::Stop => OrderKind::Stop,
            OrderType::StopLimit => OrderKind::StopLimit,
            OrderType::TakeProfit => OrderKind::TakeProfit,
            OrderType::Trailing => OrderKind::Trailing,
            OrderType::TrailingLimit => OrderKind::TrailingLimit,
            OrderType::TrailingTakeProfit => OrderKind::TrailingTakeProfit,
            _ => return false,
        };
        // A stop is judged as it always was; the others by the one terms rule the door and the drain apply,
        // and a stop's adjustment by its own (`stop_adjust_refusal`: `None` when it has none).
        let terms_ok = if req.order_type == OrderType::Stop && req.trail_percent.is_none() {
            req.trigger_price.is_some_and(|t| t.is_finite() && t > 0.0)
        } else {
            vike_model::order_terms_refusal(
                &req.order_type,
                req.price,
                req.trigger_price,
                req.trail,
                req.limit_offset,
                req.peg.as_ref(),
                req.trail_percent,
            )
            .is_none()
        } && vike_model::stop_adjust_refusal(
            &req.order_type,
            req.side,
            req.trigger_price,
            req.adjust.as_ref(),
        )
        .is_none();
        if !terms_ok
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
        // `VenueCapabilities::reduce_only_kinds`). The fire below is a reduce-only MARKET, which every row lists
        // (`vike-venues`' `reduce_only_kinds_are_a_subset_of_the_kinds_and_keep_the_market_close`), so the
        // venue is never asked for what it refuses; a kind whose fire is a reduce-only LIMIT is asked about
        // that limit first. The engine's stated tier is passed as `lower_submit` passes it, so the two
        // judgements stay one: `ReduceOnly` is the tier-sensitive axis, and a `paper` engine is not refused
        // it (its stop reaches the simulator, which holds it: decision 0201), so it is not held by the core here.
        // The attributes wave's three forms are held the same way where the row refuses them: a trailing
        // take-profit's kind (`Trigger`), a percent trail (`Trigger`: the row's `percent_trail_kinds`, which
        // judge the reduce-only axis too) and a stop's adjustment (`Attribute`, the word `adjust`).
        let tier = self.engine(routed.unwrap_or(0)).stated_tier();
        let capabilities = self.capabilities_venue(routed, &req.venue);
        let deny = vike_venues::preflight_order_at(req, capabilities, tier);
        if !matches!(
            deny,
            Err(vike_venues::PreflightDeny::Trigger { .. }
                | vike_venues::PreflightDeny::ReduceOnly { .. }
                | vike_venues::PreflightDeny::Attribute { attribute: "adjust", .. })
        ) {
            return false;
        }
        let limit = match req.order_type {
            OrderType::StopLimit | OrderType::TakeProfit => req.price,
            _ => None,
        };
        // Whose fire is a reduce-only LIMIT: a stated limit, a trailing-limit, a trailing take-profit with an
        // offset (IB `TRAIL LIT`), and a stop that an adjustment turns into a stop-limit.
        let fires_a_limit = limit.is_some()
            || req.order_type == OrderType::TrailingLimit
            || (req.order_type == OrderType::TrailingTakeProfit && req.limit_offset.is_some())
            || req
                .adjust
                .is_some_and(|a| matches!(a.to, vike_model::OrderStopAdjustTo::StopLimit { .. }));
        if fires_a_limit {
            let release = OrderRequest {
                order_type: OrderType::Limit,
                reduce_only: true,
                ..Default::default()
            };
            if vike_venues::preflight_order_at(&release, capabilities, tier).is_err() {
                return false;
            }
        }
        // The arm's order: a trailing kind's trail, activation (`trigger_price`) and offset, or the
        // trigger and limit of the others.
        let mut order = vike_model::OrderResting::new(kind, req.side, req.qty);
        if req.order_type.is_trailing() {
            order.trail = req.trail;
            order.trail_percent = req.trail_percent;
            order.activation = req.trigger_price;
            order.limit_offset = req.limit_offset;
        } else {
            order.price = req.trigger_price;
            order.limit = limit;
            order.adjust = req.adjust.map(Box::new);
        }
        let c = crate::emulator::OrderEmulationArm { order, trigger_by: req.trigger_by }
            .intent(req.venue.clone(), req.symbol.clone());
        let Some(arm_id) = self.arm_conditional_as(c, true, now, route) else {
            return false;
        };
        let engine_index = self.emulated.cond_engine.get(&arm_id).map_or(0, |a| a.engine);
        let position = self.engine(engine_index).gate_position_size(&req.symbol);
        // A stop armed over a position it reduces is COVERED: when that position later goes away the
        // sweep clears the stop (`core_held.rs`). One armed on a flat book waits for its position.
        if vike_model::is_reducing_direction(req.side, position)
            && position.abs() > 1e-12
            && let Some(a) = self.emulated.cond_engine.get_mut(&arm_id)
        {
            a.covered = true;
        }
        // Who asked: a mounted strategy cannot cancel the arm by id (it has no order client id to name), so say so and
        // what clears it instead.
        let by = match route {
            EngineRoute::Mount(i) => format!(
                " Armed by the strategy mount `{}`, which cancels it with its own mass-cancel.",
                self.mount_ids.get(i).map_or("?", String::as_str)
            ),
            EngineRoute::Payload | EngineRoute::Engine(_) => String::new(),
        };
        // The words name the level the arm fires at: its trigger on the grid (`arm_conditional_as`).
        let tick = self.engine(engine_index).gate.limits.grid_for(&req.symbol).tick_size;
        let trigger = req.trigger_price.map_or(0.0, |t| {
            vike_execution::risk::trigger_on_grid(&req.order_type, req.side, t, tick)
        });
        let (when, sends) = core_held_terms(req, trigger, limit);
        // what the row lacks: the kind, or the kind with its percent or its adjustment
        let what = match (req.adjust.is_some(), req.trail_percent.is_some()) {
            (true, _) => format!("adjustable {}", req.order_type),
            (_, true) => format!("percent {}", req.order_type),
            _ => req.order_type.to_string(),
        };
        let note = format!(
            "CORE-HELD STOP {arm_id} armed: {} {} {} {} {when} \
             (reduce-only; the position now is {position}). {} has NO native reduce-only {what}, so THIS \
             NODE holds it: it protects only while the node runs, NOT through an outage or a restart gap. At the \
             trigger it sends {sends} for at most the open position, or drops itself if there is \
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
        if self.emulated.cond_engine.is_empty()
            || !self.emulated.cond_engine.get(id).is_some_and(|a| a.reduce_only)
        {
            return false;
        }
        self.disarm_conditional(id.to_string(), now);
        true
    }
}

/// The arm note's two phrases for the core-held arm of `req` (its `trigger` as armed, its fire's
/// `limit`, if any): when it fires, and what it sends. A stop's words are the words its note always had.
fn core_held_terms(req: &OrderRequest, trigger: f64, limit: Option<f64>) -> (String, String) {
    use vike_model::{OrderStopAdjustTo, OrderType};
    let distance =
        req.trail_percent.map_or_else(|| req.trail.unwrap_or(0.0).to_string(), |p| format!("{p}%"));
    let mut when = if req.order_type == OrderType::TrailingTakeProfit {
        let (moves, from) = if req.side < 0 { ("rises", "low") } else { ("falls", "high") };
        format!("when the price {moves} {distance} off its {from}")
    } else if req.order_type.is_trailing() {
        let since =
            req.trigger_price.map_or(String::new(), |a| format!(" once it has reached {a}"));
        format!("when the price comes back {distance} from its best{since}")
    } else if req.order_type == OrderType::TakeProfit {
        format!("when the price touches {trigger}")
    } else {
        format!("when the price crosses {trigger}")
    };
    if let Some(a) = req.adjust {
        let target = match a.to {
            OrderStopAdjustTo::Stop { stop } => format!("a stop at {stop}"),
            OrderStopAdjustTo::StopLimit { stop, limit } => {
                format!("a stop-limit at {stop}, limit {limit}")
            }
            OrderStopAdjustTo::Trailing { stop, trail, trail_percent } => {
                let d = trail_percent
                    .map_or_else(|| trail.unwrap_or(0.0).to_string(), |p| format!("{p}%"));
                format!("a trailing stop from {stop} by {d}")
            }
        };
        when.push_str(&format!(", and once the price reaches {} it becomes {target}", a.at));
    }
    let sends = match (limit, req.limit_offset) {
        (Some(l), _) => format!("a limit at {l}"),
        (None, Some(o)) => format!("a limit {o} past the stop level it fires at"),
        (None, None) => "a market".to_string(),
    };
    (when, sends)
}

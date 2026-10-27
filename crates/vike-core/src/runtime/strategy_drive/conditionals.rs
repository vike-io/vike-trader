//! Conditional-order firing: the bar, price and mark triggers, and the fired order's submit.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Check the ConditionalBook against a closed bar and submit fired orders as plain
    /// MARKETs through the ONE live path (mint → RiskGate → client) — oracle semantics:
    /// only the FIRE crosses the gate; a veto surfaces as OrderDenied.
    pub(crate) fn fire_conditionals_bar(&mut self, venue: &str, symbol: &str, bar: &Bar) {
        let fired = match self.conditional_books.get_mut(&(venue.to_string(), symbol.to_string())) {
            Some(book) if !book.is_empty() => book.check_bar(bar),
            _ => return,
        };
        self.submit_fired(venue, symbol, fired, bar.ts);
    }

    /// Tick-price twin (Rust-native upgrade, `CoreConfig::conditionals_on_ticks`, or a core-held
    /// reduce-only stop resting: `CoreThread::tick_conditionals`): the same book checked against a
    /// degenerate bar at the tick price — the Last lane. The price is the quote MID.
    ///
    /// `#[inline(never)]`, like the crate's other cold helpers a hot function calls: this body (the book
    /// walk, `submit_fired`) runs only when `tick_conditionals` is on, so it must not be absorbed into
    /// `drive_strategy_tick`, which every quote, trade and book message runs.
    #[inline(never)]
    pub(crate) fn fire_conditionals_at_price(
        &mut self,
        venue: &str,
        symbol: &str,
        px: f64,
        now: i64,
    ) {
        // THE PER-TICK PATH. The book is found by comparing the two names in place, so a tick that
        // finds nothing armed for its pair (the common case: no arm anywhere, or one on another
        // symbol) allocates NOTHING; the lookup key (two `String`s per tick) used to be built before
        // the book was known to be there. O(armed books), a handful; zero steps for an empty map.
        let Some(book) = self
            .conditional_books
            .iter_mut()
            .find_map(|((v, s), b)| (v == venue && s == symbol && !b.is_empty()).then_some(b))
        else {
            return;
        };
        let fired = book.check_price(px, now);
        self.submit_fired(venue, symbol, fired, now);
    }

    /// MARK-lane twin (w2 `trigger_by`): a mark tick evaluates ONLY the `Some(Mark)` arms of the
    /// book (their trailing extremes ratchet on this lane too). Called off the conflated mark
    /// drain for EVERY mark tick — deliberately NOT behind `conditionals_on_ticks` (that knob
    /// gates the Last lane's sub-bar upgrade over the oracle's bar-close law; the mark lane has
    /// no bar-close equivalent at all, so gating it would mean a Mark arm could never fire).
    ///
    /// Because it is ungated it sits on the mark drain, so the no-Mark-arm path must cost
    /// nothing: the `has_mark_arms` scan runs FIRST and the (venue, symbol) lookup key — two
    /// String allocations — is built ONLY once some book actually holds a Mark arm. The scan is
    /// O(books) over the (venue, symbol) pairs carrying conditionals (zero when none are armed,
    /// a handful otherwise), never a walk of the arms themselves.
    pub(crate) fn fire_conditionals_at_mark(
        &mut self,
        venue: &str,
        symbol: &str,
        px: f64,
        now: i64,
    ) {
        if !self.conditional_books.values().any(|b| b.has_mark_arms()) {
            return;
        }
        let fired = match self.conditional_books.get_mut(&(venue.to_string(), symbol.to_string())) {
            Some(book) if book.has_mark_arms() => book.check_mark(px, now),
            _ => return,
        };
        self.submit_fired(venue, symbol, fired, now);
    }

    /// Release each fired conditional as a plain MARKET through the ONE live path, WRITE-AHEAD
    /// journaled as a [`vike_journal::JournalRecord::ConditionalFire`] (emulator-journal PR-1).
    ///
    /// The write-ahead record is what makes an emulated trigger replayable at all. The FIRE is a
    /// runtime reaction to a bar/tick, and market data is deliberately never journaled — so before
    /// this record `replay.rs` re-folded a fired session WITHOUT its release and the determinism
    /// fence caught it as a `HashMismatch` (the residual its module doc named). Journaling the
    /// DECISION (not the tick behind it) is the same trick `apply_strategy_intent` plays for a
    /// mounted strategy's orders: replay re-applies the recorded request through `apply_intent`
    /// and never re-evaluates a trigger, so a fire can neither double nor vanish.
    ///
    /// The record goes down BEFORE `apply_intent` (write-ahead: a crash between the two loses the
    /// release, never hides it) and carries the request with its coid still EMPTY — the mint
    /// happens inside `apply_intent`, which then writes its own `MintedSubmit`, exactly as for any
    /// other server-minted submit. Gated on journaling being on, so the no-journal path is
    /// byte-identical; firing is order cadence, never the p99 per-message fold.
    fn submit_fired(
        &mut self,
        venue: &str,
        symbol: &str,
        fired: Vec<crate::emulator::FiredConditional>,
        now: i64,
    ) {
        for f in fired {
            // The arm's side record is spent FIRST (it used to be after the journal write, which
            // does not read it): a core-held reduce-only stop's request is the CAPPED one, and the
            // write-ahead record must carry exactly what is sent, because replay re-applies it.
            let armed = self.cond_engine.remove(&f.arm_id);
            let (qty, reduce_only) = match armed.filter(|a| a.reduce_only) {
                None => (f.qty, false),
                Some(a) => {
                    // A core-held arm left `cond_engine`: the tick lane it kept on may go back off.
                    self.refresh_tick_conditionals();
                    match self.core_held_fire_qty(a.engine, symbol, f.side, f.qty) {
                        Ok(qty) => (qty, true),
                        Err(why) => {
                            self.drop_core_held_fire(venue, symbol, &f, why, now);
                            continue;
                        }
                    }
                }
            };
            let req = OrderRequest {
                client_order_id: String::new(),
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                side: f.side,
                qty,
                order_type: vike_model::OrderType::Market,
                reduce_only,
                ts: now,
                ..Default::default()
            };
            if let Some(j) = self.journal.as_mut() {
                j.append_conditional_fire(now, &f.arm_id, f.trigger_px, &req)
                    .expect("journal append");
                self.journaled_since_snap += 1;
            }
            // ⚠ THE ARM'S OWN ACCOUNT, not the venue's default one. `conditional_books` is keyed by
            // `(venue, symbol)` — an EXCHANGE fact — so two accounts of one venue arm into one book
            // and a `FiredConditional` names no account; lowering this release through
            // `apply_intent` routed it by the payload's venue, which resolves the venue's DEFAULT
            // engine. A labelled mount's `Broker::submit_stop` therefore armed against its own book
            // and, on trigger, sold into the default account's: the protective exit OPENED a naked
            // position on an account that never asked for one while the position it was armed to
            // close stayed open — silently, both books wrong, and it is the exact shape the spread
            // configuration makes routine (long on one account, short on the other).
            //
            // `cond_engine` recorded the index at ARM time; a MISS keeps the historical payload
            // route, which is what a book seeded from a restored `Snap` still takes
            // (`CoreConfig::conditionals` carries `(venue, symbol)` and no route key — the declared
            // residual on that field).
            let route = match armed {
                Some(armed) => EngineRoute::Engine(armed.engine),
                None => EngineRoute::Payload,
            };
            let coids = self.apply_intent_routed(
                OrderIntent::Submit(Box::new(req)),
                now,
                CancelIntent::Unspecified,
                route,
            );
            if reduce_only {
                // The stop is spent whatever becomes of its close: say which order carries it.
                let eidx = armed.map_or(0, |a| a.engine);
                self.note_core_held_fire(venue, symbol, &f, qty, &coids, eidx);
            }
            // ⚠ THE ARMING MOUNT OWNS THE ORDER IT RELEASED. A stop is the strategy's own order
            // exactly as a `Submit` is, but it lowers through `apply_intent_routed` with no mount,
            // so `apply_strategy_intent`'s `coid_mount` write never saw it: the fill booked into no
            // ledger and `mount_for_coid` missed, which handed mount B's stop to the first mount
            // on the pair. The arm remembered its mount (`ArmedBy::mount`) for this write; an
            // operator's arm has none and stays unattributed. The same write
            // `replay::fold_coid_mounts` reproduces from the journal's `ConditionalArmed`.
            if let Some(mount) = armed.and_then(|a| a.mount) {
                for coid in coids {
                    self.own_coid(coid, mount, None);
                }
            }
            self.dirty = true;
        }
    }

    /// The quantity a core-held reduce-only stop sends when it fires, or why there is nothing to send:
    /// `min(armed qty, |position| less the reduce-only MARKET closes already out)` of `engine`'s book in
    /// `symbol`, and only while `side` really reduces it ([`vike_model::is_reducing_direction`]: a flat
    /// book, or a position on the side the stop would add to, reduces nothing).
    ///
    /// **The closes already in flight are taken off** ([`Self::closes_in_flight`], the netting the margin
    /// call and the budget latch's flatten do). The position is the node's FOLDED book, so a flatten, a
    /// margin call's liquidation or a first fired stop still waiting for its fill leaves it whole, and a
    /// second full-size close beside it is admitted (the gate nets in-flight closes only against a FLAT
    /// book); on a venue that sends no reduce-only flag (dukascopy, fxcm netting, polymarket) the two
    /// closes add up to more than the position and the position flips.
    ///
    /// **Why a cap and not the bare flag.** `vike_exec::RiskGate` trusts a bare `reduce_only` while an
    /// engine is `Reducing` (the drawdown latch), and six venues (fxcm, oanda, ibkr, alpaca, dukascopy,
    /// polymarket) put no such flag on the wire, so a flagged stop on a flat or smaller book would be
    /// admitted and then OPEN a position. With the quantity clamped to the book, the order IS a covered
    /// reduce (`vike_model::is_covered_reduce`) on merit under `Reducing` and `Halted`, whatever the
    /// venue does with the flag, and it is the figure the gate judges with
    /// (`ExecutionEngine::gate_position_size`). It also stops a STALE stop (the position was closed by
    /// another order) from opening one. The book is the arm's own account's: venue-agnostic.
    fn core_held_fire_qty(
        &self,
        engine: usize,
        symbol: &str,
        side: i32,
        armed_qty: f64,
    ) -> Result<f64, &'static str> {
        let position = self.eng(engine).gate_position_size(symbol);
        if !vike_model::is_reducing_direction(side, position) {
            return Err("there is no position on its side to reduce");
        }
        let room = position.abs() - self.closes_in_flight(engine, symbol, side, None);
        if room.is_nan() || room <= 1e-12 {
            return Err(
                "the market closes already in flight cover the whole position (if they fail the \
                 position is UNPROTECTED: re-arm the stop)",
            );
        }
        Ok(armed_qty.min(room))
    }

    /// A core-held reduce-only stop fired with nothing to send (`why`): nothing is sent. Journals the
    /// existing [`vike_journal::JournalRecord::ConditionalDisarmed`] (the arm already left its book at
    /// the fire, and `replay::fold_conditionals` removes an arm on that record, so a restart cannot
    /// resurrect it) and says so on the ring and the log, naming the arm.
    #[inline(never)]
    fn drop_core_held_fire(
        &mut self,
        venue: &str,
        symbol: &str,
        f: &crate::emulator::FiredConditional,
        why: &str,
        now: i64,
    ) {
        self.journal_conditional_disarmed(now, &f.arm_id);
        let note = format!(
            "REDUCE-ONLY STOP DROPPED: core-held stop {} on {venue} {symbol} fired at {} but {why}, \
             so NOTHING was sent",
            f.arm_id, f.trigger_px
        );
        tracing::warn!(
            target: "vike_core::core",
            arm_id = %f.arm_id,
            venue,
            symbol,
            "{note}"
        );
        self.note(note);
        self.dirty = true;
    }

    /// A core-held reduce-only stop FIRED and its capped market (`qty`) was handed to engine `eidx`:
    /// say so on the ring and the log, naming the arm and the order (`coids`), because the arm is spent
    /// from this instant and the order's own refusal line (a gate veto, a stopped account, the venue's
    /// reject) names only the coid. If the order is ALREADY terminal (refused synchronously) the line
    /// says the position is UNPROTECTED; a refusal that comes later is the coid's own line, and a
    /// partial fill leaves the rest bare in the same way. Per fire, never per message.
    #[inline(never)]
    fn note_core_held_fire(
        &mut self,
        venue: &str,
        symbol: &str,
        f: &crate::emulator::FiredConditional,
        qty: f64,
        coids: &[String],
        eidx: usize,
    ) {
        let registry = &self.eng(eidx).registry;
        let refused = coids.is_empty()
            || coids.iter().any(|c| registry.get(c.as_str()).is_some_and(|o| !o.status.is_live()));
        let sent = if coids.is_empty() { "no order".to_string() } else { coids.join(", ") };
        let verdict = if refused {
            "that order was REFUSED at once, so the position is UNPROTECTED: re-arm the stop"
        } else {
            "the stop is spent: if that order is refused or fills only in part, the rest of the \
             position is UNPROTECTED (re-arm the stop)"
        };
        let note = format!(
            "CORE-HELD STOP {} FIRED at {}: {} {qty} {venue} {symbol} reduce-only market sent as {sent}; \
             {verdict}",
            f.arm_id,
            f.trigger_px,
            if f.side > 0 { "BUY" } else { "SELL" },
        );
        tracing::warn!(
            target: "vike_core::core",
            arm_id = %f.arm_id,
            venue,
            symbol,
            "{note}"
        );
        self.note(note);
    }
}

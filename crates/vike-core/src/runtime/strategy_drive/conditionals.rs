//! Conditional-order firing: the bar, price and mark triggers, and the fired order's submit.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Check the ConditionalArms against a closed bar and submit fired orders as
    /// MARKETs through the ONE live path (mint → RiskGate → client) — oracle semantics:
    /// only the FIRE crosses the gate; a veto surfaces as OrderDenied.
    pub(crate) fn fire_conditionals_bar(&mut self, venue: &str, symbol: &str, bar: &Bar) {
        let fired = match self
            .emulated
            .order_emulation_arms
            .get_mut(&(venue.to_string(), symbol.to_string()))
        {
            Some(book) if !book.is_empty() => book.check_bar(bar),
            _ => return,
        };
        self.submit_fired(venue, symbol, fired, bar.timestamp);
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
            .emulated
            .order_emulation_arms
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
        if !self.emulated.order_emulation_arms.values().any(|b| b.has_mark_arms()) {
            return;
        }
        let fired = match self
            .emulated
            .order_emulation_arms
            .get_mut(&(venue.to_string(), symbol.to_string()))
        {
            Some(book) if book.has_mark_arms() => book.check_mark(px, now),
            _ => return,
        };
        self.submit_fired(venue, symbol, fired, now);
    }

    /// Release each fired conditional as a MARKET, or as the LIMIT a stop-limit, a limit-if-touched or
    /// a trailing-limit releases ([`fired_request`]), through the ONE live path, WRITE-AHEAD
    /// journaled as a [`vike_journal::JournalRecord::OrderEmulationFire`] (emulator-journal PR-1).
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
    /// release, never hides it) and carries the request with its order client id still EMPTY — the mint
    /// happens inside `apply_intent`, which then writes its own `MintedSubmit`, exactly as for any
    /// other server-minted submit. Gated on journaling being on, so the no-journal path is
    /// byte-identical; firing is order cadence, never the p99 per-message fold.
    fn submit_fired(
        &mut self,
        venue: &str,
        symbol: &str,
        fired: Vec<crate::emulator::OrderEmulationFire>,
        now: i64,
    ) {
        for f in fired {
            // A RESTORED core-held stop whose account has had no reconcile pass yet is put back in its
            // book, not fired (`core_held.rs`). One `is_empty` test per fire on every other node.
            if !self.stop_holds.is_empty() && self.hold_restored_stop(venue, symbol, &f) {
                continue;
            }
            // The arm's side record is spent FIRST (it used to be after the journal write, which
            // does not read it): a core-held reduce-only stop's request is the CAPPED one, and the
            // write-ahead record must carry exactly what is sent, because replay re-applies it.
            let armed = self.emulated.cond_engine.remove(&f.arm_id);
            let (qty, core_held) = match armed.filter(|a| a.reduce_only) {
                None => (f.qty, false),
                Some(a) => {
                    // A core-held arm left `cond_engine`: the tick lane it kept on may go back off.
                    self.refresh_tick_conditionals();
                    match self.core_held_fire_quantity(a.engine, symbol, f.side, f.qty) {
                        Ok(qty) => (qty, true),
                        Err(why) => {
                            self.drop_core_held_fire(venue, symbol, &f, why, now);
                            continue;
                        }
                    }
                }
            };
            // ⚠ THE ARM'S OWN ACCOUNT, not the venue's default one. `conditional_books` is keyed by
            // `(venue, symbol)` — an EXCHANGE fact — so two accounts of one venue arm into one book
            // and a `OrderEmulationFire` names no account; lowering this release through
            // `apply_intent` routed it by the payload's venue, which resolves the venue's DEFAULT
            // engine. A labelled mount's `LiveBroker::submit_stop` therefore armed against its own book
            // and, on trigger, sold into the default account's: the protective exit OPENED a naked
            // position on an account that never asked for one while the position it was armed to
            // close stayed open — silently, both books wrong, and it is the exact shape the spread
            // configuration makes routine (long on one account, short on the other).
            //
            // `cond_engine` recorded the index at ARM time; a MISS keeps the historical payload
            // route, which is what a book seeded from a restored `Snapshot` still takes
            // (`CoreConfig::order_emulation_arms` carries `(venue, symbol)` and no route key — the declared
            // residual on that field).
            let route = match armed {
                Some(armed) => EngineRoute::Engine(armed.engine),
                None => EngineRoute::Payload,
            };
            // ⚠ WHAT THE POSITION COVERS IS A CLOSE, and says so: the stop is spent at this fire and nothing
            // asks again, so a queue's age check (`vike_bridge_core::exec_actor`'s `closes_at_once`) refusing
            // it would leave the position open. A venue with the flag on its wire trims it to what is still
            // held. On a flat book or on the position's own side it can only open (an entry stop) and stays
            // plain. Larger than the position it is a close and an opening, and they go as two orders: the
            // gate reads a flagged order above the position as an overshoot, and it denied the plain whole
            // under a halt or for its size, with the position open. Per fire, never per message.
            //
            // ⚠ ONLY ONE PLAIN STOP CLOSES (owner ruling of 2026-11-08: "only 1 can close, other will open new
            // opposite position"; perps, futures, shortable stocks, CFDs and forex alike). What the closes
            // already in flight cover (`core_held_closes_in_flight`: market closes, crossing limits and the
            // limits a fire released) is no longer this stop's to close, so that part goes as the plain opening
            // order behind the close, judged as one: of two stops of the position's size the first closes it
            // and the second opens the opposite position, on a venue with the reduce-only flag and on one
            // without alike. The close stays flagged: on ig, an fxcm hedging and an oanda hedging account the
            // flag is what makes it CLOSE instead of opening a hedge.
            let engine_index = self.route_of(route, venue);
            let position = engine_index.map_or(0.0, |e| self.engine(e).gate_position_size(symbol));
            let (sent, past_position, reduce_only) = if core_held {
                (qty, 0.0, true)
            } else if vike_model::is_reducing_direction(f.side, position) {
                let in_flight = engine_index
                    .map_or(0.0, |e| self.core_held_closes_in_flight(e, symbol, f.side));
                let closing = qty.min((position.abs() - in_flight).max(0.0));
                (closing, qty - closing, true)
            } else {
                (qty, 0.0, false)
            };
            if sent <= 1e-12 {
                // The closes in flight cover the whole position: the stop is all opening order.
                if !self.release_past_position(
                    venue,
                    symbol,
                    &f,
                    armed,
                    route,
                    engine_index,
                    qty,
                    now,
                ) {
                    // Nothing was sent, so no `OrderEmulationFire` removes the arm: the disarm does, or a
                    // restart would resurrect a spent stop (and the replay fence would see it).
                    self.journal_conditional_disarmed(now, &f.arm_id);
                }
                self.dirty = true;
                continue;
            }
            let req = fired_request(venue, symbol, &f, sent, reduce_only, now);
            let order_client_ids = self.release_fired(&f, armed, route, req, now);
            if core_held {
                // The stop is spent whatever becomes of its close: say which order carries it.
                let engine_index = armed.map_or(0, |a| a.engine);
                self.note_core_held_fire(venue, symbol, &f, qty, &order_client_ids, engine_index);
                // …and the sweep reads how that order ended: a refused or part-filled close re-arms the
                // stop (`core_held.rs`).
                self.track_fired_stop(venue, symbol, &f, &order_client_ids, armed);
            }
            if past_position > 1e-12 {
                self.release_past_position(
                    venue,
                    symbol,
                    &f,
                    armed,
                    route,
                    engine_index,
                    past_position,
                    now,
                );
            }
            self.dirty = true;
        }
    }

    /// Send one order of a fire: the write-ahead record, which carries exactly what is sent because replay
    /// re-applies it, then the ONE live path, and the arming mount owns what it minted. The order client ids it minted.
    fn release_fired(
        &mut self,
        f: &crate::emulator::OrderEmulationFire,
        armed: Option<OrderEmulationArmRoute>,
        route: EngineRoute,
        req: OrderRequest,
        now: i64,
    ) -> Vec<String> {
        // The engine the arm fires onto (`None` for an arm with no recorded engine, which fires
        // through the payload route below and replays through it, and for no journal).
        let rk = if self.journal.is_some() {
            armed.and_then(|a| self.journal_replay_route_key(a.engine))
        } else {
            None
        };
        if let Some(j) = self.journal.as_mut() {
            j.append_conditional_fire(now, &f.arm_id, f.trigger_price, &req, rk.as_deref())
                .expect("journal append");
            self.journaled_since_snap += 1;
        }
        let limit_close = (req.reduce_only && req.order_type == vike_model::OrderType::Limit)
            .then(|| self.route_of(route, &req.venue))
            .flatten();
        // A strategy's fire is the strategy's order in its id too (`S` and its code, decision 0270), though it
        // lowers on its arm's engine: the arm's mount is the mint's owner for this one submit.
        self.order_client_id_owner = armed.and_then(|a| a.mount);
        let order_client_ids = self.apply_intent_routed(
            OrderIntent::Submit(Box::new(req)),
            now,
            OrderCancelIntent::Unspecified,
            route,
        );
        self.order_client_id_owner = None;
        // A reduce-only LIMIT this fire released is netted by the next core-held fire on its position
        // whether or not it crosses (`core_held_closes_in_flight`). Pruned to the live ones here.
        if let Some(engine_index) = limit_close {
            self.note_fired_limit_closes(engine_index, &order_client_ids);
        }
        // ⚠ THE ARMING MOUNT OWNS THE ORDER IT RELEASED. A stop is the strategy's own order
        // exactly as a `Submit` is, but it lowers through `apply_intent_routed` with no mount,
        // so `apply_strategy_intent`'s `order_client_id_mount` write never saw it: the fill booked into no
        // ledger and `mount_for_order_client_id` missed, which handed mount B's stop to the first mount
        // on the pair. The arm remembered its mount (`OrderEmulationArmRoute::mount`) for this write; an
        // operator's arm has none and stays unattributed. The same write
        // `replay::fold_order_client_id_mounts` reproduces from the journal's `OrderEmulationArmed`.
        if let Some(mount) = armed.and_then(|a| a.mount) {
            for order_client_id in &order_client_ids {
                self.own_order_client_id(order_client_id.clone(), mount, None);
            }
        }
        order_client_ids
    }

    /// Remember the `order_client_ids` of a reduce-only LIMIT a fire released on engine `engine_index`
    /// ([`CoreThread::fired_limit_closes`]), dropping the remembered ones that are no longer live. Per fire.
    #[cold]
    #[inline(never)]
    fn note_fired_limit_closes(&mut self, engine_index: usize, order_client_ids: &[String]) {
        let mut kept = std::mem::take(&mut self.fired_limit_closes);
        kept.retain(|(e, c)| {
            self.engine(*e).registry.get(c.as_str()).is_some_and(|o| o.status.is_live())
        });
        kept.extend(order_client_ids.iter().map(|c| (engine_index, c.clone())));
        self.fired_limit_closes = kept;
    }

    /// The part of a stop larger than its position, which opens the other way: a plain market behind the close,
    /// so a queue's age check can still refuse it, and alone. The gate at the send cannot judge it: it still reads the whole
    /// position (the close has not filled), so it takes this for a covered reduce up to the position's size and for too small
    /// a flip past it. So the engine asks the gate first, on the flat book the close leaves
    /// (`Engine::denial_past_close`): every ceiling of an opening order, a halted or reducing account among them. A
    /// refused part is left out and said. The close has already gone, and no record is written, so a replay, which never
    /// re-decides a fire, sends the close alone too. The same judgement takes a stop whose position the closes already in
    /// flight cover WHOLE (`submit_fired`: it sent no close, so the opening order is all of it). `true` = sent.
    #[inline(never)]
    fn release_past_position(
        &mut self,
        venue: &str,
        symbol: &str,
        f: &crate::emulator::OrderEmulationFire,
        armed: Option<OrderEmulationArmRoute>,
        route: EngineRoute,
        engine_index: Option<usize>,
        qty: f64,
        now: i64,
    ) -> bool {
        let req = fired_request(venue, symbol, f, qty, false, now);
        let Some(why) = engine_index.and_then(|e| self.engine_mut(e).denial_past_close(&req, now))
        else {
            self.release_fired(f, armed, route, req, now);
            return true;
        };
        let lead = if qty + 1e-12 >= f.qty {
            "fired with its position already closing (the closes in flight cover it)"
        } else {
            "fired larger than the position: the close went out"
        };
        let note = format!(
            "STOP {} on {venue} {symbol} {lead}, and the {qty} past it would open the other way, so it was NOT \
             sent ({why})",
            f.arm_id
        );
        tracing::warn!(target: "vike_core::core", arm_id = %f.arm_id, venue, symbol, "{note}");
        self.note(note);
        false
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
    /// **Why a cap and not the bare flag.** `vike_execution::RiskGate` trusts a bare `reduce_only` while an
    /// engine is `Reducing` (the drawdown latch), and six venues (fxcm, oanda, ibkr, alpaca, dukascopy,
    /// polymarket) put no such flag on the wire, so a flagged stop on a flat or smaller book would be
    /// admitted and then OPEN a position. With the quantity clamped to the book, the order IS a covered
    /// reduce (`vike_model::is_covered_reduce`) on merit under `Reducing` and `Halted`, whatever the
    /// venue does with the flag, and it is the figure the gate judges with
    /// (`Engine::gate_position_size`). It also stops a STALE stop (the position was closed by
    /// another order) from opening one. The book is the arm's own account's: venue-agnostic.
    fn core_held_fire_quantity(
        &self,
        engine: usize,
        symbol: &str,
        side: i32,
        armed_quantity: f64,
    ) -> Result<f64, &'static str> {
        let position = self.engine(engine).gate_position_size(symbol);
        if !vike_model::is_reducing_direction(side, position) {
            return Err("there is no position on its side to reduce");
        }
        let room = position.abs() - self.core_held_closes_in_flight(engine, symbol, side);
        if room.is_nan() || room <= 1e-12 {
            return Err(
                "the closes already in flight (market closes, crossing limits and the limits a fire \
                 released) cover the whole position (if they fail the position is UNPROTECTED: re-arm \
                 the stop)",
            );
        }
        Ok(armed_quantity.min(room))
    }

    /// A core-held reduce-only stop fired with nothing to send (`why`): nothing is sent. Journals the
    /// existing [`vike_journal::JournalRecord::OrderEmulationDisarmed`] (the arm already left its book at
    /// the fire, and `replay::fold_conditionals` removes an arm on that record, so a restart cannot
    /// resurrect it) and says so on the ring and the log, naming the arm.
    #[inline(never)]
    fn drop_core_held_fire(
        &mut self,
        venue: &str,
        symbol: &str,
        f: &crate::emulator::OrderEmulationFire,
        why: &str,
        now: i64,
    ) {
        self.journal_conditional_disarmed(now, &f.arm_id);
        let note = format!(
            "REDUCE-ONLY STOP DROPPED: core-held stop {} on {venue} {symbol} fired at {} but {why}, \
             so NOTHING was sent",
            f.arm_id, f.trigger_price
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

    /// A core-held reduce-only stop FIRED and its capped market (`qty`) was handed to engine `engine_index`:
    /// say so on the ring and the log, naming the arm and the order (`order_client_ids`), because the arm is spent
    /// from this instant and the order's own refusal line (a gate veto, a stopped account, the venue's
    /// reject) names only the order client id. If the order is ALREADY terminal (refused synchronously) the line
    /// says the position is UNPROTECTED; a refusal that comes later is the order client id's own line, and a
    /// partial fill leaves the rest bare in the same way. Per fire, never per message.
    #[inline(never)]
    fn note_core_held_fire(
        &mut self,
        venue: &str,
        symbol: &str,
        f: &crate::emulator::OrderEmulationFire,
        qty: f64,
        order_client_ids: &[String],
        engine_index: usize,
    ) {
        let registry = &self.engine(engine_index).registry;
        let refused = order_client_ids.is_empty()
            || order_client_ids
                .iter()
                .any(|c| registry.get(c.as_str()).is_some_and(|o| !o.status.is_live()));
        let sent = if order_client_ids.is_empty() {
            "no order".to_string()
        } else {
            order_client_ids.join(", ")
        };
        let verdict = if refused {
            "that order was REFUSED at once, so the position is UNPROTECTED: re-arm the stop"
        } else {
            "the stop is spent: if that order is refused or fills only in part, the rest of the \
             position is UNPROTECTED (re-arm the stop)"
        };
        let order = f.limit.map_or_else(|| "market".to_string(), |l| format!("limit at {l}"));
        let note = format!(
            "CORE-HELD STOP {} FIRED at {}: {} {qty} {venue} {symbol} reduce-only {order} sent as {sent}; \
             {verdict}",
            f.arm_id,
            f.trigger_price,
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

/// One order of a fire: a MARKET, or the LIMIT at [`crate::emulator::OrderEmulationFire::limit`] a
/// stop-limit, a limit-if-touched or a trailing-limit releases (never a market for those); its order client id
/// still empty because the mint is `apply_intent`'s.
fn fired_request(
    venue: &str,
    symbol: &str,
    f: &crate::emulator::OrderEmulationFire,
    qty: f64,
    reduce_only: bool,
    now: i64,
) -> OrderRequest {
    OrderRequest {
        order_client_id: String::new(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side: f.side,
        qty,
        order_type: if f.limit.is_some() {
            vike_model::OrderType::Limit
        } else {
            vike_model::OrderType::Market
        },
        price: f.limit,
        reduce_only,
        timestamp: now,
        ..Default::default()
    }
}

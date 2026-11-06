//! The hygiene of an operator's CORE-HELD reduce-only stop (decision 0199 and its amendments): what keeps a
//! stop that lives in the node's memory from outliving the position it protects, firing against a book it
//! has not been checked against, or going quiet after a refused close. Every path here is COLD: a restored
//! stop's fire (`submit_fired`, only when a hold exists), a reconcile pass, the stuck-order clock's sweep
//! (`TimerKind::StuckSweep`, the arm that also runs `hold_nothing_to_reduce`) and the publish overlay. Nothing
//! is added to `dispatch`, `run_submit_hop`, the fold or `snapshot::build`'s per-engine walk.
//!
//! - **HOLD a restored stop until its account's first reconcile pass** ([`StopHold`]). The restored book is
//!   the last snapshot's, not the venue's, so a stop crossed in the gap of a restart is capped against a
//!   position the venue may have closed. The stop STAYS in its book (the `Snap` writes it, a cancel
//!   disarms it, the listing shows it `held`); when a crossing fires it, `submit_fired` asks
//!   [`CoreThread::hold_restored_stop`], which puts it back instead. The pass for the stop's engine
//!   releases it ([`CoreThread::release_stop_holds`], called from `reconcile_reports`). ⚠ A hold that no
//!   pass releases LAPSES (`RESTORED_TAG_DENY_WINDOW_MS` after the first sweep, or after the first held
//!   crossing on a core with no sweep): an account with no reconcile leg must not go without its stop.
//! - **CLEAR a stop whose position went away** ([`CoreThread::clear_stale_core_held_stops`]). A native
//!   reduce-only stop is cancelled by the venue when its position closes; this one rests. The sweep clears
//!   it when its engine's position in the symbol is flat or on the other side AND the node has seen a
//!   position on the stop's side before (`ArmedBy::covered`: a stop armed before its entry fills is left).
//! - **RE-ARM a stop whose close failed** ([`CoreThread::rearm_failed_core_held_stops`]). The stop is spent
//!   at its fire; the sweep reads how the close ended, and if the position is still open (refused, or filled
//!   only in part) arms the stop again, at most [`MAX_STOP_REARMS`] times in a row, said by name.
//! - **LIST** the armed stops ([`CoreThread::core_held_stop_views`]), overlaid on every snapshot.
//!
//! None of it is journaled but what the existing records already carry: a re-arm is a `ConditionalArmed`
//! (the same record the operator's arm writes, `reduce_only` set), a clear is a `ConditionalDisarmed`, and
//! the journal format does not move.

use super::*;

use crate::emulator::FiredConditional;
use vike_venues::venue_restore::RESTORED_TAG_DENY_WINDOW_MS;

/// How many times in a row a stop is re-armed after its close failed before the node gives up and says the
/// position is unprotected: each round costs one sweep period, so a venue that keeps refusing the close is
/// asked three times, not for ever.
pub(crate) const MAX_STOP_REARMS: u8 = 3;

/// The most fired stops whose close the sweep is still judging; the oldest goes first.
const FIRED_STOPS_MAX: usize = 64;

/// How long (engine clock, ms) a fired stop's close is watched: past it the record is dropped unjudged (the
/// stuck-order ladder has long since decided the close).
const FIRED_STOP_WATCH_MS: i64 = 600_000;

/// A position at or under this is flat (the tolerance `core_held_fire_qty` uses).
const POSITION_TOL: f64 = 1e-12;

/// The terms of one resting core-held stop, read out of its book.
struct StopTerms {
    venue: String,
    symbol: String,
    side: i32,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The fire of restored stop `f` crossed while its account's first pass has not landed: put it back in its
    /// book with its own terms and fire nothing. `true` = held back (the caller `continue`s); `false` = not
    /// a held stop, or its hold LAPSED (it is released and the caller fires it).
    ///
    /// Said ONCE per stop, at the first crossing it holds back; each later crossing tick puts it back
    /// silently. Bounded by the hold itself: it ends at the pass or at the lapse.
    #[cold]
    #[inline(never)]
    pub(crate) fn hold_restored_stop(
        &mut self,
        venue: &str,
        symbol: &str,
        f: &FiredConditional,
    ) -> bool {
        let Some(hold) = self.stop_holds.get(&f.arm_id).copied() else {
            return false;
        };
        let Some(px) = f.armed_price else {
            return false;
        };
        let now = self.engine.now_ms;
        let first = hold.until.is_none();
        let until = hold.until.unwrap_or_else(|| now.saturating_add(RESTORED_TAG_DENY_WINDOW_MS));
        if now >= until {
            self.stop_holds.remove(&f.arm_id);
            let note = format!(
                "CORE-HELD STOP {} hold LAPSED: no reconcile pass reached its account in {} s (reconcile \
                 off, or its fetch keeps failing), so it is released and fires NOW against the book as it \
                 stands",
                f.arm_id,
                RESTORED_TAG_DENY_WINDOW_MS / 1_000
            );
            tracing::warn!(target: "vike_core::core", arm_id = %f.arm_id, "{note}");
            self.note(note);
            return false;
        }
        let back = self
            .conditional_books
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .add_stop(&f.arm_id, f.side, f.qty, px, f.armed_trigger_by);
        if !back {
            // The arm left this very book an instant ago, so a duplicate id cannot happen; if it ever
            // did, firing is the safe direction for a stop-loss.
            return false;
        }
        if first {
            if let Some(h) = self.stop_holds.get_mut(&f.arm_id) {
                h.until = Some(until);
            }
            let account = self.eng(hold.engine).route_key.clone();
            let note = format!(
                "CORE-HELD STOP {} on {venue} {symbol} crossed its trigger {px} while HELD: it was restored \
                 from the journal and account {account} has not had its first reconcile pass yet, so the \
                 book it would be capped against is the last snapshot's and not the venue's. It fires at \
                 the first tick after that pass, or after {} s if none comes.",
                f.arm_id,
                RESTORED_TAG_DENY_WINDOW_MS / 1_000
            );
            tracing::warn!(target: "vike_core::core", arm_id = %f.arm_id, venue, symbol, "{note}");
            self.note(note);
        }
        true
    }

    /// The first reconcile pass of engine `engine` has landed: release every restored stop waiting for it.
    /// Called by `reconcile_reports` after the pass is folded, only when a hold exists.
    #[cold]
    #[inline(never)]
    pub(crate) fn release_stop_holds(&mut self, engine: usize) {
        let before = self.stop_holds.len();
        self.stop_holds.retain(|_, h| h.engine != engine);
        let released = before - self.stop_holds.len();
        if released == 0 {
            return;
        }
        let account = self.eng(engine).route_key.clone();
        let note = format!(
            "CORE-HELD STOP(S) RELEASED: {released} restored stop(s) of {account} were waiting for its first \
             reconcile pass, which has landed; they fire on the next tick that crosses them."
        );
        tracing::info!(target: "vike_core::core", account = %account, "{note}");
        self.note(note);
    }

    /// Record the close a core-held stop just sent (`coids`, empty when it was refused before it was
    /// registered) so [`Self::rearm_failed_core_held_stops`] can read how it ended. Per fire.
    #[cold]
    #[inline(never)]
    pub(crate) fn track_fired_stop(
        &mut self,
        venue: &str,
        symbol: &str,
        f: &FiredConditional,
        coids: &[String],
        armed: Option<ArmedBy>,
    ) {
        let (Some(trigger), Some(armed)) = (f.armed_price, armed) else {
            return;
        };
        if self.fired_stops.len() >= FIRED_STOPS_MAX {
            self.fired_stops.remove(0);
        }
        self.fired_stops.push(FiredStop {
            arm_id: f.arm_id.clone(),
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            side: f.side,
            qty: f.qty,
            trigger,
            trigger_by: f.armed_trigger_by,
            engine: armed.engine,
            mount: armed.mount,
            coids: coids.to_vec(),
            rearms: armed.rearms,
            at_ms: self.engine.now_ms,
        });
    }

    /// An operator's mass-cancel or market-exit takes the fired stops of its scope out of the re-arm watch:
    /// the operator asked for the book to be emptied, and a stop re-armed behind that would undo it. Same
    /// scope rule as [`Self::clear_conditional_scope`], which calls this.
    pub(crate) fn forget_fired_stops(
        &mut self,
        venue: Option<&str>,
        symbol: Option<&str>,
        engines: Option<&[usize]>,
    ) {
        self.fired_stops.retain(|fs| {
            let in_scope = venue.is_none_or(|v| fs.venue == v)
                && symbol.is_none_or(|s| fs.symbol == s)
                && engines.is_none_or(|e| e.contains(&fs.engine));
            !in_scope
        });
    }

    /// The cold sweep of the core-held stops, run by the stuck-order clock: start the lapse clock of the
    /// holds, clear the stops whose position went away and re-arm the ones whose close failed. A pair of
    /// `is_empty` tests on a node that holds no core-held stop.
    #[cold]
    #[inline(never)]
    pub(crate) fn sweep_core_held_stops(&mut self, now: i64) {
        if self.cond_engine.is_empty() && self.fired_stops.is_empty() {
            return;
        }
        self.age_stop_holds(now);
        self.clear_stale_core_held_stops(now);
        self.rearm_failed_core_held_stops(now);
    }

    /// Start the lapse clock of every hold that has none (at the first sweep after the restart, so the
    /// window is counted from boot and not from a crossing that may be hours away), forget a hold whose arm
    /// is gone, and release the ones whose window ran out.
    fn age_stop_holds(&mut self, now: i64) {
        if self.stop_holds.is_empty() {
            return;
        }
        let live = &self.cond_engine;
        self.stop_holds.retain(|id, _| live.contains_key(id));
        let mut lapsed = 0usize;
        self.stop_holds.retain(|_, h| {
            let until = *h.until.get_or_insert(now.saturating_add(RESTORED_TAG_DENY_WINDOW_MS));
            let keep = now < until;
            lapsed += usize::from(!keep);
            keep
        });
        if lapsed > 0 {
            let note = format!(
                "CORE-HELD STOP(S) hold LAPSED: {lapsed} restored stop(s) waited {} s for a reconcile pass \
                 of their account and none came (reconcile off, or its fetch keeps failing); they are \
                 released and fire on the next tick that crosses them, against the book as it stands",
                RESTORED_TAG_DENY_WINDOW_MS / 1_000
            );
            tracing::warn!(target: "vike_core::core", "{note}");
            self.note(note);
        }
    }

    /// The terms of resting arm `arm_id`: its book's `(venue, symbol)` and its side. `None` when no book
    /// holds it.
    fn stop_terms(&self, arm_id: &str) -> Option<StopTerms> {
        self.conditional_books.iter().find_map(|((venue, symbol), book)| {
            book.iter().find(|(id, _)| *id == arm_id).map(|(_, a)| StopTerms {
                venue: venue.clone(),
                symbol: symbol.clone(),
                side: a.order.side,
            })
        })
    }

    /// Clear every core-held stop that was covered by a position and whose position is now flat or on the
    /// other side (another order closed it: a take-profit, a strategy's exit, a liquidation, a flatten).
    /// Also marks a stop covered the first time a sweep finds its position.
    fn clear_stale_core_held_stops(&mut self, now: i64) {
        let held: Vec<(String, usize, bool)> = self
            .cond_engine
            .iter()
            .filter(|(_, a)| a.reduce_only)
            .map(|(id, a)| (id.clone(), a.engine, a.covered))
            .collect();
        for (id, engine, covered) in held {
            // A stopped account's stops are the operator's to look at, not this sweep's to clear.
            if self.is_retired(engine) {
                continue;
            }
            let Some(terms) = self.stop_terms(&id) else {
                continue;
            };
            let position = self.eng(engine).gate_position_size(&terms.symbol);
            let reduces = vike_model::is_reducing_direction(terms.side, position)
                && position.abs() > POSITION_TOL;
            if reduces {
                if !covered && let Some(a) = self.cond_engine.get_mut(&id) {
                    a.covered = true;
                }
            } else if covered {
                self.clear_core_held_stop(&id, &terms, position, now);
            }
        }
    }

    /// Disarm core-held stop `id` because its position is gone: the journal record, the book, the side
    /// record, the hold and the tick lane, then ONE line saying so.
    fn clear_core_held_stop(&mut self, id: &str, terms: &StopTerms, position: f64, now: i64) {
        self.journal_conditional_disarmed(now, id);
        if let Some(book) =
            self.conditional_books.get_mut(&(terms.venue.clone(), terms.symbol.clone()))
        {
            book.disarm(id);
        }
        self.cond_engine.remove(id);
        self.stop_holds.remove(id);
        self.refresh_tick_conditionals();
        let now_state = if position.abs() <= POSITION_TOL {
            "flat".to_string()
        } else {
            format!("on the other side ({position})")
        };
        let note = format!(
            "CORE-HELD STOP {id} CLEARED: its position in {} {} is now {now_state}, closed by another \
             order, so a stop that outlived it would close the next position opened on the far side of its \
             trigger. Arm another if you want one.",
            terms.venue, terms.symbol
        );
        tracing::warn!(target: "vike_core::core", arm_id = %id, venue = %terms.venue, symbol = %terms.symbol, "{note}");
        self.note(note);
        self.dirty = true;
    }

    /// Judge the close of every fired core-held stop whose close has ended and arm the stop again when
    /// the position is still open (the close was refused, or filled only in part).
    fn rearm_failed_core_held_stops(&mut self, now: i64) {
        if self.fired_stops.is_empty() {
            return;
        }
        let mut keep: Vec<FiredStop> = Vec::new();
        for fs in std::mem::take(&mut self.fired_stops) {
            if self.is_retired(fs.engine) || now.saturating_sub(fs.at_ms) > FIRED_STOP_WATCH_MS {
                continue;
            }
            let live = {
                let registry = &self.eng(fs.engine).registry;
                fs.coids
                    .iter()
                    .any(|c| registry.get(c.as_str()).is_some_and(|o| o.status.is_live()))
            };
            if live {
                keep.push(fs);
                continue;
            }
            let position = self.eng(fs.engine).gate_position_size(&fs.symbol);
            if !vike_model::is_reducing_direction(fs.side, position)
                || position.abs() <= POSITION_TOL
            {
                // Flat, or on the other side: the close took everything, or something else did.
                continue;
            }
            let in_flight = self.closes_in_flight(fs.engine, &fs.symbol, fs.side, None);
            let room = position.abs() - in_flight;
            if room.is_nan() || room <= POSITION_TOL {
                // Other closes cover what is left; they may still fail, so keep watching.
                if in_flight > POSITION_TOL {
                    keep.push(fs);
                }
                continue;
            }
            self.rearm_core_held_stop(&fs, room, now);
        }
        self.fired_stops = keep;
    }

    /// Arm the stop of fired record `fs` again for `min(armed qty, room)` at its own trigger, or say that
    /// the node gave up.
    fn rearm_core_held_stop(&mut self, fs: &FiredStop, room: f64, now: i64) {
        let how = if fs.coids.is_empty() {
            "was refused before it was registered".to_string()
        } else {
            format!(
                "({}) ended with the position still open (refused, or filled only in part)",
                fs.coids.join(", ")
            )
        };
        if fs.rearms >= MAX_STOP_REARMS {
            let note = format!(
                "CORE-HELD STOP {} GAVE UP: the close it sent {how}, {MAX_STOP_REARMS} re-arms in a row did \
                 not take the position and {room} of {} {} is UNPROTECTED. Look at the account, then arm a \
                 stop again.",
                fs.arm_id, fs.venue, fs.symbol
            );
            tracing::warn!(target: "vike_core::core", arm_id = %fs.arm_id, "{note}");
            self.note(note);
            return;
        }
        let qty = fs.qty.min(room);
        let intent = ConditionalIntent {
            venue: fs.venue.clone(),
            symbol: fs.symbol.clone(),
            side: fs.side,
            qty,
            price: Some(fs.trigger),
            trail: None,
            trigger_by: fs.trigger_by,
        };
        match self.arm_conditional_as(intent, true, now, EngineRoute::Engine(fs.engine)) {
            Some(new_id) => {
                if let Some(a) = self.cond_engine.get_mut(&new_id) {
                    a.rearms = fs.rearms + 1;
                    a.covered = true;
                    a.mount = fs.mount;
                }
                let note = format!(
                    "CORE-HELD STOP {} RE-ARMED as {new_id}: the close it sent {how}, and {room} of {} {} \
                     is still open. Armed again for {qty} at {} (re-arm {} of {MAX_STOP_REARMS}).",
                    fs.arm_id,
                    fs.venue,
                    fs.symbol,
                    fs.trigger,
                    fs.rearms + 1
                );
                tracing::warn!(target: "vike_core::core", arm_id = %fs.arm_id, new_arm_id = %new_id, "{note}");
                self.note(note);
            }
            None => {
                let note = format!(
                    "CORE-HELD STOP {} could NOT be re-armed (its close {how}): {room} of {} {} is \
                     UNPROTECTED. Arm a stop again.",
                    fs.arm_id, fs.venue, fs.symbol
                );
                tracing::warn!(target: "vike_core::core", arm_id = %fs.arm_id, "{note}");
                self.note(note);
            }
        }
        self.dirty = true;
    }

    /// The armed core-held stops, for the snapshot (`CoreSnapshot::core_held_stops`). An empty `Vec` (no
    /// allocation) on a node with no conditional at all; otherwise one walk of the armed books with a hash
    /// probe per arm, and a few `String` clones per core-held stop.
    pub(crate) fn core_held_stop_views(&self) -> Vec<vike_exec::CoreHeldStopView> {
        if self.cond_engine.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for ((venue, symbol), book) in &self.conditional_books {
            for (id, arm) in book.iter() {
                let Some(a) = self.cond_engine.get(id).filter(|a| a.reduce_only) else {
                    continue;
                };
                out.push(vike_exec::CoreHeldStopView {
                    arm_id: id.to_string(),
                    venue: venue.clone(),
                    symbol: symbol.clone(),
                    route_key: self.eng(a.engine).route_key.clone(),
                    side: arm.order.side,
                    qty: arm.order.size,
                    trigger_price: arm.order.price.unwrap_or(0.0),
                    held: self.stop_holds.contains_key(id),
                    rearms: a.rearms,
                });
            }
        }
        out
    }
}

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
                // WHICH ACCOUNT this arm protects, and WHICH MOUNT armed it, recorded at the
                // one moment both are known — see `CoreThread::cond_engine`. Only for an arm
                // that actually entered a book, so the map cannot outlive its arm.
                self.cond_engine.insert(arm_id.clone(), ArmedBy { engine: eidx, mount });
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
                self.cond_engine.insert(arm_id.clone(), ArmedBy { engine: eidx, mount });
            } else {
                self.note(format!("ArmConditional: duplicate arm id {arm_id} — REFUSED"));
            }
        } else {
            self.note("ArmConditional: neither price nor trail set — ignored".to_string());
        }
        Vec::new()
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
}

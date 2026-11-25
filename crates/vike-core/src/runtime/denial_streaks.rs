//! **Repeated DENIALS of one strategy order are counted, not journaled one by one** (journal format 35, the owner's
//! yes of 2026-11-24). A strategy that re-sends an order the pre-trade gate keeps refusing (the CI box: a quote over
//! `max_account_exposure` every ~13 ms, 832,810 denials in 29 h, ~0.4 GB/h of journal) wrote three records per denial:
//! its `StrategySubmit`, its `MintedSubmit` and the `Denied` `ExecutionReport`. Now the first gate denial of a KEY (the
//! mount, the order's venue, symbol and side) in a process writes that triple, and each later submit of the key the
//! gate refuses for the same refusal (`OrderDenied::refusal_id`) is COUNTED: one `vike_journal::JournalRecord::DeniedRepeat`
//! stands for all of them, written when the count's window ends ([`DENIAL_WINDOW_MS`], at the next counted denial), at
//! the key's next order the gate admits or refuses for another reason, before every `Snapshot` (`write_snap`, so at
//! the stop too), and when the core reserves a new range of ids (below). A crash loses at most one window's count.
//!
//! **What the live core does with the order is unchanged.** It is lowered as before: minted, gated, its `OrderDenied`
//! published, heard by its strategy, listed for the alert (`note_denied`), counted by every counter. Only its journal
//! records are held (`vike_journal::Journal::defer_submits`, from its journal write to the gate's verdict in
//! `lower_submit`): dropped for a counted denial, framed for anything else, and always before the order can leave
//! the process (`submit_strategy_order`, and before `arm_emulated`). A core with no journal and a replay core count
//! nothing.
//!
//! **A counted denial's order client id is never minted again, a crash included.** It cannot keep the id the lowering
//! minted from the session's sequence: no record would say the sequence moved, so a restart could mint the id again
//! and a replay would mint every later order's id one off. So the core gives that id back
//! (`OrderClientIdGenerator::give_back`) and mints the denial's id from a RANGE of [`DENIAL_RANGE`] sequence numbers
//! it reserved for counted denials: the range starts at the id given back, the `DeniedRepeat` naming it is appended
//! BEFORE the range's first id is handed out, and the generator jumps past it at once and never mints inside it
//! (`OrderClientIdGenerator::reserve`). A boot restore floors the session's sequence at the end of every range the
//! journal names (`crates/vike-core/src/replay/boot.rs`'s `restore_from`), and a replay core skips each range when its
//! sequence reaches it, where the live core jumped (`CoreConfig::order_client_id_reserved`), so it mints every later
//! order's id byte for byte. A crash spends what is left of a range and never reuses an id: the reservation is a record
//! like a `MintedSubmit`, within the write thread's lag of the page cache (decision 0279).
//!
//! Order cadence (a strategy's submit), never the per-message fold: a core whose strategies were never denied pays
//! one `is_empty` per journaled strategy submit.

use super::*;
use vike_journal::JournalDeniedRepeat;
use vike_model::orders::client_id::OrderClientIdGenerator;

/// How long a key's denials are counted before the count is journaled (one record a minute while a strategy keeps
/// re-sending a refused order).
pub(crate) const DENIAL_WINDOW_MS: i64 = 60_000;

/// How many order client ids one range reserved for counted denials holds (a v2 session has `2^39` before it rolls).
pub(crate) const DENIAL_RANGE: u64 = 1 << 16;

/// One KEY's counted denials: its mount, the key and the count not journaled yet ([`JournalDeniedRepeat`]), and when
/// its last record was written.
pub(crate) struct DenialStreak {
    mount_index: usize,
    repeat: JournalDeniedRepeat,
    written_ms: i64,
}

/// A strategy submit being lowered whose denial may be counted: its mount, and whether the journal holds its records
/// (a key the gate denied before).
#[derive(Clone, Copy)]
pub(crate) struct DenialCandidate {
    pub(crate) mount_index: usize,
    held: bool,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// At the journal write of mount `mount_index`'s `intent`: a submit is a candidate, and one of a key the gate
    /// denied before has its records held. Every other intent, and a replay core, nothing.
    pub(crate) fn begin_denial_candidate(&mut self, mount_index: usize, intent: &OrderIntent) {
        #[cfg(test)]
        if self.journal_every_denial {
            return;
        }
        if self.config.replay_core {
            return;
        }
        let OrderIntent::Submit(req) = intent else { return };
        let held = self.streak_index(mount_index, req).is_some();
        if held && let Some(journal) = self.journal.as_mut() {
            journal.defer_submits();
        }
        self.denial_candidate = Some(DenialCandidate { mount_index, held });
    }

    /// Once the intent is lowered: frame whatever is still held (a submit refused before its gate, held for its
    /// release, an OTO child).
    pub(crate) fn end_denial_candidate(&mut self) {
        self.denial_candidate = None;
        self.frame_held_submits();
    }

    /// Frame the held records, `false` when the journal cannot: then the node halts at the drain boundary, as it
    /// does when an order's fence cannot pass (decision 0279's addendum), and the caller sends nothing.
    pub(super) fn frame_held_submits(&mut self) -> bool {
        let Some(journal) = self.journal.as_mut() else { return true };
        let held = journal.deferred_submits().unwrap_or(0) as u64;
        match journal.end_deferred_submits() {
            Ok(()) => {
                self.journaled_since_snap += held;
                true
            }
            Err(e) => {
                tracing::error!(target: "vike_core::core", error = %e,
                    "the journal cannot frame a strategy order's write-ahead records: the order is not sent");
                self.journal_refused.store(true, Ordering::Relaxed);
                false
            }
        }
    }

    /// [`Engine::submit_order`] for a strategy's submit, `candidate` its denial's candidacy and `minted` the sequence
    /// its id was minted under (`None`: an id the lowering did not mint). The gate's verdict first: admitted, the
    /// held records are framed and the key's count journaled before the order can leave; refused as the key was
    /// before, the denial is counted ([`Self::count_denial`]); refused otherwise, its records are framed (the key's
    /// example) and the key remembered.
    pub(super) fn submit_strategy_order(
        &mut self,
        (engine_index, route): (usize, EngineRoute),
        req: &mut OrderRequest,
        (candidate, minted): (DenialCandidate, Option<u64>),
        now: i64,
        outbox: &mut EventBusOutbox,
    ) {
        let verdict = self.engine_mut(engine_index).gate_verdict(req, now);
        let mut recorded = true;
        if verdict.ok {
            recorded = self.frame_held_submits();
            if let Some(i) = self.streak_index(candidate.mount_index, req) {
                self.write_streak(i, now);
            }
        } else {
            let refusal_id = verdict
                .denial
                .and_then(|d| vike_model::events::refusal_id_of(d.id().word()))
                .map(|id| id.to_string());
            let counted = candidate.held
                && self.count_denial(
                    (engine_index, route),
                    req,
                    (candidate.mount_index, minted),
                    (refusal_id.as_deref(), &verdict.reason),
                    now,
                );
            if !counted {
                self.frame_held_submits();
                self.remember_denial(candidate.mount_index, req, refusal_id, now);
            }
        }
        self.engine_mut(engine_index).submit_judged(req, verdict, recorded, now, outbox);
    }

    /// **Count this denial instead of journaling it**, when the key's last denial was for the same refusal, its two
    /// records are held and nothing else was journaled since, and its id was the last one minted: give the id back,
    /// drop the records, take the denial's id from the reserved range (reserving one, journaled, when none is left)
    /// and count it. `false`, with nothing changed, otherwise. `reason` is the gate's words.
    fn count_denial(
        &mut self,
        (engine_index, route): (usize, EngineRoute),
        req: &mut OrderRequest,
        (mount_index, minted): (usize, Option<u64>),
        (refusal_id, reason): (Option<&str>, &str),
        now: i64,
    ) -> bool {
        let Some(minted) = minted else { return false };
        let Some(i) = self.streak_index(mount_index, req) else { return false };
        // a ring line already names the id this would give back (`stamp_data_delay`)
        let noted = req.data_delay.is_some_and(|d| !d.is_real_time());
        if self.denial_streaks[i].repeat.refusal_id.as_deref() != refusal_id
            || noted
            || self.journal.as_ref().and_then(vike_journal::Journal::deferred_submits) != Some(2)
            || !self.order_client_id_gen.give_back(minted)
        {
            return false;
        }
        if let Some(journal) = self.journal.as_mut() {
            journal.drop_deferred_submits();
        }
        let repeat = &mut self.denial_streaks[i].repeat;
        if repeat.count == 0 {
            repeat.first_ms = now;
        }
        repeat.count += 1;
        repeat.last_ms = now;
        reason.clone_into(&mut repeat.reason);
        match self.denial_range {
            Some((from, next, end)) if next < end => {
                // the range's next id, under the labels the lowering minted with; the session's sequence stays
                let session = self.order_client_id_gen.state().0;
                let main = std::mem::replace(
                    &mut self.order_client_id_gen,
                    OrderClientIdGenerator::resume(session, next),
                );
                let id = self.mint_order_client_id(route, engine_index, req);
                self.order_client_id_gen = main;
                self.denial_range = Some((from, next + 1, end));
                if let Some(e) = self.order_client_id_venue.remove(&req.order_client_id) {
                    self.order_client_id_venue.insert(id.clone(), e);
                }
                req.order_client_id = id;
                if now - self.denial_streaks[i].written_ms >= DENIAL_WINDOW_MS {
                    self.write_streak(i, now);
                }
            }
            _ => {
                // A new range from the id just given back, which is its first (the denial keeps it): journaled now,
                // before the id is handed out, and the session's sequence past it.
                let end = minted + DENIAL_RANGE;
                self.order_client_id_gen.reserve(minted, end);
                self.denial_range = Some((minted, minted + 1, end));
                self.write_streak(i, now);
            }
        }
        self.coalesced_denial = Some(req.order_client_id.clone());
        true
    }

    /// Remember the key of a denial journaled whole (its example), for the refusal `refusal_id`: a key denied before
    /// for another refusal journals its count first.
    fn remember_denial(
        &mut self,
        mount_index: usize,
        req: &OrderRequest,
        refusal_id: Option<String>,
        now: i64,
    ) {
        if self.journal.is_none() || self.config.replay_core {
            return;
        }
        match self.streak_index(mount_index, req) {
            Some(i) if self.denial_streaks[i].repeat.refusal_id != refusal_id => {
                self.write_streak(i, now);
                self.denial_streaks[i].repeat.refusal_id = refusal_id;
            }
            Some(_) => {}
            None => self.denial_streaks.push(DenialStreak {
                mount_index,
                repeat: JournalDeniedRepeat {
                    mount_id: self.mount_ids.get(mount_index).cloned().unwrap_or_default(),
                    venue: req.venue.clone(),
                    symbol: req.symbol.clone(),
                    side: req.side,
                    refusal_id,
                    ..JournalDeniedRepeat::default()
                },
                written_ms: now,
            }),
        }
    }

    /// Journal every key's count not journaled yet: before a `Snapshot` (`write_snap`).
    pub(crate) fn write_denial_streaks(&mut self, now: i64) {
        for i in 0..self.denial_streaks.len() {
            self.write_streak(i, now);
        }
    }

    /// Journal key `i`'s count, with the range reserved now, unless it counts nothing. A failed append is said at
    /// ERROR and halts the node at the drain boundary, as a journal that cannot write does.
    fn write_streak(&mut self, i: usize, now: i64) {
        let Some((from, _, to)) = self.denial_range else { return };
        if self.denial_streaks[i].repeat.count == 0 {
            return;
        }
        let session = self.order_client_id_gen.state().0;
        let streak = &mut self.denial_streaks[i];
        streak.repeat.order_client_id_session = session;
        (streak.repeat.reserved_from, streak.repeat.reserved_to) = (from, to);
        if let Some(journal) = self.journal.as_mut() {
            match journal.append_denied_repeat(now, &streak.repeat) {
                Ok(sequence) => {
                    self.journaled_since_snap += 1;
                    // It rides the report run as a report would (`journal_execution_report`): it causes no fold, so the
                    // next report still names the record before it, the order's `MintedSubmit` or its venue event.
                    let (run_first, run_end) = self.report_run;
                    self.report_run = if sequence == run_end {
                        (run_first, sequence + 1)
                    } else {
                        (sequence, sequence + 1)
                    };
                }
                Err(e) => {
                    tracing::error!(target: "vike_core::core", error = %e,
                        "the journal cannot write a count of repeated denials");
                    self.journal_refused.store(true, Ordering::Relaxed);
                }
            }
        }
        streak.repeat.count = 0;
        streak.written_ms = now;
    }

    /// The streak of mount `mount_index`'s key of `req` (its venue, symbol and side). One `is_empty` on a core
    /// whose strategies were never denied.
    fn streak_index(&self, mount_index: usize, req: &OrderRequest) -> Option<usize> {
        if self.denial_streaks.is_empty() {
            return None;
        }
        self.denial_streaks.iter().position(|s| {
            s.mount_index == mount_index
                && s.repeat.side == req.side
                && s.repeat.venue == req.venue
                && s.repeat.symbol == req.symbol
        })
    }
}

//! Write-ahead journal helpers, arm/refusal id minting, the cadence snapshot, and the recent-events ring writers.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Write-ahead-journal a client-synthesized venue event as the `Ingest::Event` record a real
    /// venue event rides. Moves `ev` through a temporary `Ingest` wrapper so the append borrows it
    /// (no clone) and hands it straight back for the fold. No-op passthrough when journaling is off.
    pub(crate) fn journal_pumped_event(&mut self, now_ms: i64, ev: Event) -> Event {
        let Some(j) = self.journal.as_mut() else { return ev };
        let ingest = Ingest::Event(ev);
        j.append_cmd(now_ms, &ingest).expect("journal append");
        self.journaled_since_snap += 1;
        let Ingest::Event(ev) = ingest else { unreachable!("wrapped an Event just above") };
        ev
    }

    /// Mint the next emulated-conditional `arm_id`, `{coid_session}a{arm_seq}`. The coid-session
    /// prefix separates two concurrently-running cores; the suffix is the crate-local
    /// [`Self::arm_seq`] counter, NOT a coid — see that field for why.
    ///
    /// Uniqueness across a RESTART (which deliberately restores the same `coid_session`, and
    /// appends to the same journal directory) rests on `arm_seq` being resumed too, via
    /// [`CoreConfig::arm_seq`]/[`crate::replay::RestoredState::arm_seq`]. A caller that restores
    /// the coid session but leaves `arm_seq` at 0 WILL re-emit pre-restart ids — and `arm_id` IS
    /// the disarm key (emulator PR-2: [`vike_exec::OrderIntent::DisarmConditional`] targets the
    /// arm-id-keyed [`crate::emulator::ConditionalBook`]), so an ambiguous id would disarm the
    /// wrong arm. Restore both or neither.
    pub(crate) fn mint_arm_id(&mut self) -> String {
        let n = self.arm_seq;
        self.arm_seq += 1;
        format!("{}a{}", self.coid_gen.state().0, n)
    }

    /// Mint the next REFUSAL id, `{coid_session}r{refusal_seq}` — the handle an intent the drain
    /// refused is denied under. The [`Self::mint_arm_id`] idiom applied to a second non-order id
    /// sequence: session prefix (so two concurrently-running cores never collide) + its OWN
    /// crate-local counter ([`CoreThread::refusal_seq`]), never the coid generator.
    ///
    /// The separation is load-bearing, not cosmetic. A refused intent journals nothing and submits
    /// nothing, so a coid drawn here would be spent with no record of it existing: every later
    /// order's coid would run one ahead of the `coid_seq` a `Snap` records, and a restart resuming
    /// that journal would re-mint an already-used id. The `a`/`r` infixes also keep all three id
    /// spaces mutually unambiguous — a real coid is `{session}{digits}`.
    pub(crate) fn mint_refusal_id(&mut self) -> String {
        let n = self.refusal_seq;
        self.refusal_seq += 1;
        format!("{}r{}", self.coid_gen.state().0, n)
    }

    /// Journal one ARM's RESOLVED terms right after [`Self::mint_arm_id`] named it — the
    /// `append_minted_submit` precedent applied to `OrderIntent::ArmConditional` (the write-ahead
    /// `Cmd`/`StrategySubmit` for that intent cannot carry a TRAILING arm's extreme, which is
    /// seeded from a non-journaled mark). Counts against the same `journaled_since_snap` cadence
    /// as every other append — so an arming session reaches `snapshot_every` marginally sooner
    /// (one extra record per arm shifts the `Snap` cadence; arms are operator/strategy cadence, so
    /// this is a rounding effect on checkpoint spacing, never a write-amplification concern). NO-OP
    /// when journaling is off, so the desktop/no-journal path stays byte-identical; arming is
    /// order-cadence, never the per-market-message fold.
    pub(crate) fn journal_conditional_armed(
        &mut self,
        now_ms: i64,
        arm_id: &str,
        resolved: vike_journal::ConditionalRecord,
    ) {
        if let Some(j) = self.journal.as_mut() {
            j.append_conditional_armed(now_ms, arm_id, &resolved).expect("journal append");
            self.journaled_since_snap += 1;
        }
    }

    /// Journal one conditional's DISARM, write-ahead of the book mutation (the
    /// [`Self::journal_conditional_armed`] twin, emulator PR-2). Called ONLY for an actual disarm
    /// — an unknown-id refusal writes nothing. NO-OP when journaling is off; disarming is
    /// command cadence, never the per-market-message fold.
    pub(crate) fn journal_conditional_disarmed(&mut self, now_ms: i64, arm_id: &str) {
        if let Some(j) = self.journal.as_mut() {
            j.append_conditional_disarmed(now_ms, arm_id).expect("journal append");
            self.journaled_since_snap += 1;
        }
    }

    /// Cadence-snapshot check, shared with [`dispatch`](Self::dispatch)'s tail via the same
    /// `journaled_since_snap` counter: fire a full-state `Snap` once the write-ahead counter reaches
    /// `snapshot_every`. `pump_client` needs its own copy because its journaled events land AFTER
    /// `dispatch`'s tail check has already run (and, on the bar-close path, `dispatch`'s tail check
    /// is skipped entirely — that message is not exec-lane) — so without this, a paper-only workload
    /// would never checkpoint until shutdown. No-op when journaling is off.
    pub(crate) fn maybe_cadence_snap(&mut self, now_ms: i64) {
        if self.journal.is_some()
            && self.journaled_since_snap >= self.config.journal.as_ref().unwrap().snapshot_every
        {
            self.write_snap(now_ms);
        }
    }

    /// Push `msg` onto the bounded recent-events ring, trimming to `recent_events_cap`. Cold path
    /// only (sweeps, reconcile passes, commands, refusals, a contingency release on a fill) — never
    /// the per-message hot fold, so no hot-path logging budget applies. The per-message delivery
    /// drains (`drain_delivered`, `drain_poisoned`) do NOT come through here: they push through
    /// [`Self::note_event`] / [`Self::note_lost`], which render on the fold.
    pub(crate) fn note(&mut self, msg: String) {
        self.push_line(msg.into());
    }

    /// **The RESOLVED route key a write-ahead journal record must carry** (§9 item 12), or `None`
    /// when that engine IS the payload venue's sole/default account.
    ///
    /// # Why the journal records it at all
    ///
    /// The write-ahead records (`append_minted_submit`, `append_margin_call_liquidate`) carry an
    /// `OrderRequest` with a VENUE and no account. Replay re-applies them through the same routing
    /// path, so on a multi-account core a replay would RE-MAKE the routing decision from a venue
    /// string instead of REPRODUCING the one the live run made — and now that Stage 1 refuses an
    /// account-less risk-increasing intent, it would refuse rather than misroute, which breaks
    /// replay determinism instead of a book. Either way the durable fact has to be on the record.
    ///
    /// ⚠ **Recorded, and not yet CONSUMED — and that is a scope statement rather than a gap.**
    /// `crate::replay::replay_journal` refuses a multi-engine base outright
    /// (`ReplayError::Unsupported("multi-engine replay (v1: single-engine only)")`), so there is no
    /// multi-account replay for a stored key to steer today; honouring it needs a routed command
    /// shape that reaches past `Ingest::Command`, which is a wider change than this stage. What
    /// this buys now is that the journals being written from today forward carry the answer, so the
    /// replay half can be built against real data instead of against journals that never had it.
    ///
    /// # ⚠ `None` for the default account, so a single-account box's journal BYTES do not move
    ///
    /// Exactly `vike_exec::ReconcileReports::route_key`'s shape, and for the reason its own doc
    /// gives: *"keeping the absent field OFF the wire is what makes a single-account box's journal
    /// bytes identical to the ones written before this field existed."* `None` also allocates
    /// nothing on that box, which is every box in production.
    pub(crate) fn journal_route_key(&self, eidx: usize, payload_venue: &str) -> Option<String> {
        let rk = self.eng(eidx).route_key.as_str();
        (rk != payload_venue).then(|| rk.to_string())
    }

    /// The per-message twin of [`Self::note`]: push a DELIVERED event's rendered LINE onto the ring.
    ///
    /// ## Why this renders HERE, on the fold path (2026-07-29)
    ///
    /// #887 deferred rendering to publish, on the premise that "the ring is only ever READ at
    /// publish, which is coalesced to `snapshot_interval` (>= 16 ms), so rendering is pure waste on
    /// the fold thread". **That premise is false.** The runtime ALSO publishes whenever the core is
    /// about to go idle (`if self.dirty`, immediately before `blocking_recv`), so a core whose
    /// events arrive sporadically publishes PER EVENT. Deferring therefore never avoided the
    /// format — it relocated it, and added a capture plus per-publish `Arc` traffic on top of it.
    ///
    /// MEASURED on the latency box (shielded cores, SCHED_FIFO 50, `--test-threads=1`, 8 interleaved reps):
    /// rendering here instead of at publish takes the baseline core-hop p99 from **2,024 ns to
    /// 802 ns**, and p50 from 321 ns to 296 ns. [`crate::recent`] keeps the byte-identical
    /// rendering contract either way.
    pub(crate) fn note_event(&mut self, note: crate::recent::EventNote) {
        self.push_line(crate::recent::RecentNote::Event(note).render().into());
    }

    /// The salvaged-from-a-mid-fold-panic twin of [`Self::note_event`] (audit C4).
    pub(crate) fn note_lost(&mut self, note: crate::recent::EventNote) {
        self.push_line(crate::recent::RecentNote::Lost(note).render().into());
    }

    /// THE one ring writer: append, then trim to `recent_events_cap`. The ring holds RENDERED lines
    /// as `Arc<str>`, so publish clones a refcount per entry and never mutates the ring.
    fn push_line(&mut self, line: std::sync::Arc<str>) {
        self.recent.push_back(line);
        while self.recent.len() > self.config.recent_events_cap {
            self.recent.pop_front();
        }
    }
}

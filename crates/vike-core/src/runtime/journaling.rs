//! Write-ahead journal helpers, arm/refusal id minting, the cadence snapshot, and the recent-events ring writers.

use super::*;

/// **The most forecasts one mount's STEP may have journaled** (REVIEW-LIVE-MONEY M3); the rest of that step's are
/// refused ([`CoreThread::refuse_forecasts`]). A forecast is no order, so no order gate (`max_orders_per_window`,
/// the symbol check) bounds it, and every one is a journal frame written on the fold that serves every live
/// account: a sandboxed file's one reply (up to 64 MiB) could otherwise carry about a million. 512 holds a
/// cross-sectional strategy's one forecast per name over a few hundred names (about 75 KB of frames a step), and
/// counts toward the snapshot cadence as every record does, so the journal stays prunable.
pub(crate) const MAX_FORECASTS_PER_STEP: usize = 512;

/// Why a boot's position snapshot does not state a venue account's book although the restore folded the journal:
/// no journaled book of the account was put back on its engine (an account added since, a book refused for its tier),
/// so the engine holds a book it never folded the venue's history into.
pub(crate) const POSITIONS_WITHOUT_A_JOURNALED_BOOK: &str = "the journal holds no book of this account that the boot restored, so its venue positions are not known";

/// The core's `fault` while its journal cannot write ([`CoreThread::halt_for_unwritable_journal`]): what happens, and
/// what a restart does (a clean stop's restart does not restore this halt, a crash's does: [`JournalFenceHalt`]).
pub(crate) const JOURNAL_CANNOT_WRITE_FAULT: &str = "the command journal could not write for 1 s (the filesystem \
     stalled, or the disk is full, read-only or failing): orders that open risk are refused NOT SENT, cancels and \
     orders that close a position leave unrecorded, and every account is HALTED until the node restarts; once the \
     journal's disk writes again (free it if it is full), stop and start the node: a clean restart comes back in the \
     trading state each account held before this halt (a crash comes back halted)";

/// **What the node held before the journal fence halted it** ([`CoreThread::halt_for_unwritable_journal`]): the EXIT
/// `Snapshot` states THAT, never the fence's halt ([`CoreThread::write_exit_snap`],
/// [`CoreThread::journaled_trading_state`]), so a clean stop restarts in what the accounts held before the outage (the
/// owner's 2A of 2026-11-24, decision 0279's addendum of that day). Every other `Snapshot` states the engines as they
/// ARE, halted: a refold from one denies what the live core denied, so a CRASH comes back halted and regenerates no
/// order the halt refused. Every word or rule that would set an engine's state while it holds is raised in here too
/// (an operator's node-wide or account-scoped word, a hold's release, the silence dead-man, a panic, an account that
/// joins), so the exit `Snapshot` states what the node would hold without the fence. No journal format change: the
/// `Snapshot`'s own `trading_state` carries it. Core-local, cold.
#[derive(Debug, Clone)]
pub(crate) struct JournalFenceHalt {
    /// The node's state (`CoreThread::node_floor`): what an account started during the halt would have taken.
    node: TradingState,
    /// Each engine's, by engine index.
    engines: Vec<TradingState>,
    /// Set by [`CoreThread::write_exit_snap`] alone: the `Snapshot` being written is the last, so it states the priors.
    pub(crate) at_exit: bool,
}

impl JournalFenceHalt {
    /// The state engine `index` held before the halt (an engine added since: the node's).
    pub(crate) fn prior(&self, index: usize) -> TradingState {
        self.engines.get(index).copied().unwrap_or(self.node)
    }

    /// The node's state before the halt: what an account that joins now takes as its own before its book is restored.
    pub(crate) fn node(&self) -> TradingState {
        self.node
    }

    /// A kept-up twin's hold on engine `index` was released to `free` (`CoreThread::hold_account`): a prior at the
    /// hold's floor (`hold`) goes back to `free`, never below the node's, as the live release does.
    pub(crate) fn release_hold(&mut self, index: usize, hold: TradingState, free: TradingState) {
        if let Some(prior) = self.engines.get_mut(index)
            && *prior == hold
            && free.strictness() < hold.strictness()
        {
            *prior = if self.node.strictness() > free.strictness() { self.node } else { free };
        }
    }

    /// A node-wide word or a panic's halt: every engine and the node at least `st`.
    pub(crate) fn raise(&mut self, st: TradingState) {
        let raise = |s: &mut TradingState| {
            if st.strictness() > s.strictness() {
                *s = st;
            }
        };
        raise(&mut self.node);
        self.engines.iter_mut().for_each(raise);
    }

    /// An account-scoped word: engine `index` at least `st`. Whether that raised it (a looser word is clamped, as the
    /// live engine's is).
    pub(crate) fn raise_engine(&mut self, index: usize, st: TradingState) -> bool {
        if index >= self.engines.len() {
            self.engines.resize(index + 1, self.node);
        }
        let prior = &mut self.engines[index];
        let rose = st.strictness() > prior.strictness();
        if rose {
            *prior = st;
        }
        rose
    }
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The wait every engine runs before an order effect leaves the process (decision 0279) (`vike_execution::Engine::fence_send`) — it returns once every
    /// journal record appended so far (a lazy tail of execution reports excepted, `vike_journal`'s handoff) is in
    /// the segment file's page cache — or `None` without a
    /// journal. Installed on every engine at assembly and on every engine an account adds later. An answer of
    /// `vike_execution::JournalCannotWrite` raises [`CoreThread::journal_refused`] for the drain boundary.
    ///
    /// `None` on a REPLAY core (`CoreConfig::replay_core`) too: nothing it sends leaves the process (its client is
    /// `crates/vike-core/src/replay/client.rs`'s `ExecutionClientJournalReplay`) and its scratch journal is read only after it stopped
    /// (`crates/vike-core/src/replay/refold.rs`'s `run_replay_core`), so no send waits on it. A fence there would let
    /// a write thread more than the bound behind on a long boot refold refuse the replayed orders and HALT the state
    /// the boot applies.
    pub(crate) fn journal_send_fence(&self) -> Option<vike_execution::OrderSendFence> {
        if self.config.replay_core {
            return None;
        }
        let fence = self.journal.as_ref()?.write_fence();
        let refused = Arc::clone(&self.journal_refused);
        Some(vike_execution::OrderSendFence::new(move || {
            fence.wait().inspect_err(|_| refused.store(true, Ordering::Relaxed))
        }))
    }

    /// **The node HALTS when its journal cannot write** (decision 0279's addendum of 2026-11-23): an engine's fence
    /// answered `vike_execution::JournalCannotWrite`, so every engine is `Halted` and the node floor with it (a halt
    /// still admits a covered exit, and a cancel is never gated), the fault is published (`trade health` fails its
    /// `core` check; the desktop and Telegram show it, and `CoreSnapshot::journal_fence_halt` raises a
    /// `journal_fence_halt` alert) and the transition is said at ERROR, once per process, EVEN on a node every engine
    /// of which was halted already (then no state moves and no `Snapshot` is written, but the journal still cannot
    /// write). Nothing is cancelled: the resting book, its venue-held stops among it, keeps protecting the positions.
    /// A `Snapshot` of the halted engines follows, as after a panic (`journal_panic_halt`), so a refold starts from the
    /// refusals the live core made instead of sending what it refused; what each engine held BEFORE this halt is kept
    /// ([`JournalFenceHalt`]) for the exit `Snapshot` alone. Cold: called from the drain boundary only while the flag
    /// is raised.
    #[cold]
    #[inline(never)]
    pub(crate) fn halt_for_unwritable_journal(&mut self) {
        self.journal_refused.store(false, Ordering::Relaxed);
        if self.journal_fence_halt.is_none() {
            tracing::error!(target: "vike_core::core", "{JOURNAL_CANNOT_WRITE_FAULT}");
            let engines = (0..=self.extra_engines.len()).map(|i| self.engine(i).trading_state);
            self.journal_fence_halt = Some(JournalFenceHalt {
                node: self.node_floor,
                engines: engines.collect(),
                at_exit: false,
            });
            if self.fault.is_none() {
                self.fault = Some(JOURNAL_CANNOT_WRITE_FAULT.to_string());
            }
            self.note(format!("HALT: {JOURNAL_CANNOT_WRITE_FAULT}"));
            self.dirty = true;
        }
        let halted = |e: &Engine<C>| e.trading_state == TradingState::Halted;
        if self.node_floor == TradingState::Halted
            && halted(&self.engine)
            && self.extra_engines.iter().all(|(_, e)| halted(e))
        {
            return;
        }
        self.node_floor = TradingState::Halted;
        self.engine.trading_state = TradingState::Halted;
        for (_, e) in self.extra_engines.iter_mut() {
            e.trading_state = TradingState::Halted;
        }
        let now = self.engine.now_ms;
        self.write_snap(now);
        self.dirty = true;
    }

    /// **The EXIT `Snapshot`** (the teardown's, `run_loop.rs`): the last record, after which nothing replays, so the one
    /// `Snapshot` that states what each engine held before a journal fence halt ([`JournalFenceHalt`]) instead of the
    /// halt itself. A clean stop restarts in those states; a crash restores from an earlier `Snapshot`, which states
    /// the halt. Without a fence halt it is `write_snap`.
    pub(crate) fn write_exit_snap(&mut self, now_ms: i64) {
        if let Some(fence) = self.journal_fence_halt.as_mut() {
            fence.at_exit = true;
        }
        self.write_snap(now_ms);
    }

    /// Write-ahead-journal a client-synthesized venue event as the `Ingest::Event` record a real
    /// venue event rides. Moves `event` through a temporary `Ingest` wrapper so the append borrows it
    /// (no clone) and hands it straight back for the fold. No-op passthrough when journaling is off.
    pub(crate) fn journal_pumped_event(&mut self, now_ms: i64, event: Event) -> Event {
        let Some(j) = self.journal.as_mut() else { return event };
        let ingest = Ingest::Event(event);
        j.append_command(now_ms, &ingest).expect("journal append");
        self.journaled_since_snap += 1;
        let Ingest::Event(event) = ingest else { unreachable!("wrapped an Event just above") };
        event
    }

    /// **Journal what the OMS states after one fold** (`vike_journal::JournalRecord::ExecutionReport`): the report
    /// `cause` owes on engine `engine_index`, `ring` the account whose stream carried it, `now_ms` the fold's clock
    /// (`runtime/execution_report.rs`). Counts in `journaled_since_snap` like every record, and every caller
    /// writes it before any `Snapshot` its cause can be followed by. Cold and out of line: a fold reaches it only
    /// for an event naming an order. NO-OP without a journal. On a REPLAY core this is the collect arm: the report
    /// goes into the scratch journal under the scratch cause, and the refold reads it back and places it under the
    /// source journal's cause (`crates/vike-core/src/replay/refold.rs`'s `replayed_reports`). An order whose pre-trade
    /// decision read market data the journal does not hold (`vike_execution::Engine::market_dependent`, a mark the
    /// replay does not have) may have been decided otherwise live, so none of its reports is regenerated: the
    /// replay core journals an `ExecutionReportsUnverified` marker of it instead
    /// ([`crate::replay::MARKET_DATA_UNVERIFIED`]), placed like a report. The live core never asks.
    ///
    /// The CAUSE is read here, off the journal, so the fold stores nothing for it: the newest record that is not a
    /// report (the venue event's own `Command`, the pumped event's, the submit's `Command` or `StrategySubmit`, a
    /// minted order's `MintedSubmit`). `CoreThread::report_run` is the run of reports appended back to back, which
    /// is skipped.
    #[cold]
    #[inline(never)]
    pub(crate) fn journal_execution_report(
        &mut self,
        engine_index: Option<usize>,
        cause: super::execution_report::ExecutionReportCause<'_>,
        ring: Option<vike_marketdata::Symbol>,
        now_ms: i64,
    ) {
        if !self.execution_reports {
            return;
        }
        let Some(next) = self.journal.as_ref().map(vike_journal::Journal::next_sequence) else {
            return;
        };
        let (run_first, run_end) = self.report_run;
        let caused_by = if run_end == next { run_first } else { next }.saturating_sub(1);
        let Some(report) = self.execution_report(engine_index, &cause, ring, caused_by, now_ms)
        else {
            return;
        };
        let market_dependent = self.config.replay_core
            && (0..=self.extra_engines.len())
                .any(|i| self.engine(i).market_dependent.contains(&report.order_client_id));
        let Some(j) = self.journal.as_mut() else { return };
        // a marker rides the report run as a report would, so the next report's cause is unchanged
        let sequence = if market_dependent {
            j.append_execution_reports_unverified(
                now_ms,
                "",
                (caused_by, caused_by),
                (Some(now_ms), Some(now_ms)),
                std::slice::from_ref(&report.order_client_id),
                crate::replay::MARKET_DATA_UNVERIFIED,
            )
        } else {
            j.append_execution_report(now_ms, &report)
        }
        .expect("journal append");
        self.journaled_since_snap += 1;
        self.report_run =
            if sequence == run_end { (run_first, sequence + 1) } else { (sequence, sequence + 1) };
    }

    /// **Journal what the boot restore owes the execution stream** (`CoreConfig::owed_execution_reports`, package 2's
    /// C.1b), once, right after the journal opens, before the boot `Snapshot` and the first dispatch: the unverified
    /// window, said at ERROR, then each report a crash cut off, regenerated, under a sequence of its own and its
    /// cause's clock. Each counts in `journaled_since_snap`, and the reports are a report run, so the next report's
    /// cause is never one of them. Returns the ring lines that say so, which the caller puts after the restore's own.
    /// A journal-less core writes nothing. Cold: once per boot.
    pub(crate) fn journal_owed_execution_reports(&mut self) -> Vec<String> {
        let owed = std::mem::take(&mut self.config.owed_execution_reports);
        let Some(j) = self.journal.as_mut() else { return Vec::new() };
        if let Some(u) = &owed.unverified {
            j.append_execution_reports_unverified(
                u.now_ms,
                &u.journal_id,
                (u.after_sequence, u.through_sequence),
                (u.from_ms, u.to_ms),
                &u.order_client_ids,
                &u.reason,
            )
            .expect("journal append");
            self.journaled_since_snap += 1;
        }
        for (now_ms, report) in &owed.reports {
            let sequence = j.append_execution_report(*now_ms, report).expect("journal append");
            self.journaled_since_snap += 1;
            let (run_first, run_end) = self.report_run;
            self.report_run = if sequence == run_end {
                (run_first, sequence + 1)
            } else {
                (sequence, sequence + 1)
            };
        }
        let mut lines = Vec::new();
        if !owed.reports.is_empty() {
            let line = format!(
                "journal restore: {} execution report(s) a crash cut off are journaled again",
                owed.reports.len()
            );
            tracing::info!(target: "vike_core::replay", "{line}");
            lines.push(line);
        }
        if let Some(u) = owed.unverified {
            let line = format!(
                "EXECUTION REPORTS UNVERIFIED: the executions of journal `{}` records {}..={} (journal time {}..{}) \
                 may be incomplete: {}{}",
                u.journal_id,
                u.after_sequence + 1,
                u.through_sequence,
                u.from_ms.map_or_else(|| "-".to_string(), |t| t.to_string()),
                u.to_ms.map_or_else(|| "-".to_string(), |t| t.to_string()),
                u.reason,
                if u.order_client_ids.is_empty() {
                    String::new()
                } else {
                    format!("; orders {}", u.order_client_ids.join(", "))
                }
            );
            tracing::error!(target: "vike_core::replay", "{line}");
            lines.push(line);
        }
        lines
    }

    /// **Journal every account's open positions as this boot holds them** (`vike_journal::JournalRecord::PositionSnapshot`,
    /// the broker's start-of-day statement): once, right after [`Self::journal_owed_execution_reports`] and before
    /// the first dispatch, so a reader of the execution stream opens an account's window from the latest snapshot at or
    /// before it and that account's reports after it. A book is stated only where it IS the account's position:
    /// a PAPER book always (the core is its venue), a venue account's when the restore folded the journal
    /// (`CoreConfig::position_snapshot`) and put a journaled book back on its engine (`restored`, engine indices).
    /// Every other account, and every account the core runs no engine for (`CoreConfig::not_running`), says why its
    /// positions are unknown, never a guess. Nothing without a basis (a core no boot restore applied to) or a journal,
    /// and nothing on a replay core. An observation, so it does not count in `journaled_since_snap`, and a failed
    /// append is a warning (a reader then finds no snapshot and refuses in words). Cold: once per boot.
    pub(crate) fn journal_position_snapshot(&mut self, restored: &[usize]) {
        let Some(basis) = self.config.position_snapshot.take() else { return };
        if self.journal.is_none() || self.config.replay_core {
            return;
        }
        let mut accounts = Vec::with_capacity(self.extra_engines.len() + 1);
        for index in 0..=self.extra_engines.len() {
            let engine = self.engine(index);
            let route_key = engine.route_key.as_str();
            let label =
                vike_model::accounts::account_keys::label_of_route_key(&engine.venue, route_key);
            let Some(label) = label else {
                continue; // a route key that names no account: its reports name none either
            };
            let unknown = match &basis {
                _ if engine.tier == vike_model::AccountTier::Paper => None,
                crate::replay::PositionSnapshotBasis::NotFolded(why) => Some(why.clone()),
                crate::replay::PositionSnapshotBasis::Folded if !restored.contains(&index) => {
                    Some(POSITIONS_WITHOUT_A_JOURNALED_BOOK.to_string())
                }
                crate::replay::PositionSnapshotBasis::Folded => None,
            };
            let positions = match unknown {
                Some(_) => Vec::new(),
                None => engine
                    .account
                    .positions
                    .iter()
                    .filter(|(_, p)| p.size != 0.0)
                    .map(|((venue, symbol, side), p)| vike_model::AccountVenuePosition {
                        venue: venue.to_string(),
                        symbol: symbol.to_string(),
                        position_side: *side,
                        size: p.size,
                        average_price: p.average_price,
                    })
                    .collect(),
            };
            accounts.push(vike_journal::JournalPositionSnapshotAccount {
                venue: engine.venue.clone(),
                account: label.to_string(),
                positions,
                unknown,
            });
        }
        // shortcut: an account that joins LATE (`late_join.rs`) gets no snapshot of its own until the next boot, so its
        // windows refuse ("no engine runs the account") till then; journal one at its join the day that bites.
        for row in &self.config.not_running {
            if let vike_model::accounts::account_keys::AccountNamed::Known(account) = &row.account {
                accounts.push(vike_journal::JournalPositionSnapshotAccount {
                    venue: account.venue.to_string(),
                    account: account.label.to_string(),
                    positions: Vec::new(),
                    unknown: Some(format!("no engine runs the account ({})", row.reason)),
                });
            }
        }
        let now_ms = self.config.clock.now_ms();
        if let Some(journal) = self.journal.as_mut()
            && let Err(e) = journal.append_position_snapshot(now_ms, &accounts)
        {
            tracing::warn!(
                target: "vike_journal",
                error = %e,
                "failed to append the boot's position snapshot: an account report opening after this boot refuses"
            );
        }
    }

    /// Mint the next emulated-conditional `arm_id`, `{order_client_id_session}a{arm_sequence}`. The order-client-id-session
    /// prefix separates two concurrently-running cores; the suffix is the crate-local
    /// [`Self::arm_sequence`] counter, NOT an order client id — see that field for why.
    ///
    /// Uniqueness across a RESTART (which deliberately restores the same `order_client_id_session`, and
    /// appends to the same journal directory) rests on `arm_sequence` being resumed too, via
    /// [`CoreConfig::arm_sequence`]/[`crate::replay::RestoredState::arm_sequence`]. A caller that restores
    /// the order client id session but leaves `arm_sequence` at 0 WILL re-emit pre-restart ids — and `arm_id` IS
    /// the disarm key (emulator PR-2: [`vike_execution::OrderIntent::DisarmConditional`] targets the
    /// arm-id-keyed [`crate::emulator::ConditionalArms`]), so an ambiguous id would disarm the
    /// wrong arm. Restore both or neither.
    pub(crate) fn mint_arm_id(&mut self) -> String {
        let n = self.arm_sequence;
        self.arm_sequence += 1;
        format!("{}a{}", self.order_client_id_gen.state().0, n)
    }

    /// Mint the next REFUSAL id, `{order_client_id_session}r{refusal_sequence}` — the handle an intent the drain
    /// refused is denied under. The [`Self::mint_arm_id`] idiom applied to a second non-order id
    /// sequence: session prefix (so two concurrently-running cores never collide) + its OWN
    /// crate-local counter ([`CoreThread::refusal_sequence`]), never the order client id generator.
    ///
    /// The separation is load-bearing, not cosmetic. A refused intent journals nothing and submits
    /// nothing, so an order client id drawn here would be spent with no record of it existing: every later
    /// order's order client id would run one ahead of the `order_client_id_sequence` a `Snapshot` records, and a restart resuming
    /// that journal would re-mint an already-used id. The `a`/`r` infixes also keep all three id
    /// spaces mutually unambiguous — a real order client id is `{session}{digits}`.
    pub(crate) fn mint_refusal_id(&mut self) -> String {
        let n = self.refusal_sequence;
        self.refusal_sequence += 1;
        format!("{}r{}", self.order_client_id_gen.state().0, n)
    }

    /// Journal one ARM's RESOLVED terms right after [`Self::mint_arm_id`] named it — the
    /// `append_minted_submit` precedent applied to `OrderIntent::ArmConditional` (the write-ahead
    /// `Command`/`StrategySubmit` for that intent cannot carry a TRAILING arm's extreme, which is
    /// seeded from a non-journaled mark). Counts against the same `journaled_since_snap` cadence
    /// as every other append — so an arming session reaches `snapshot_every` marginally sooner
    /// (one extra record per arm shifts the `Snapshot` cadence; arms are operator/strategy cadence, so
    /// this is a rounding effect on checkpoint spacing, never a write-amplification concern). NO-OP
    /// when journaling is off, so the desktop/no-journal path stays byte-identical; arming is
    /// order-cadence, never the per-market-message fold.
    pub(crate) fn journal_conditional_armed(
        &mut self,
        now_ms: i64,
        arm_id: &str,
        resolved: vike_journal::OrderEmulationRecord,
    ) {
        if let Some(j) = self.journal.as_mut() {
            j.append_conditional_armed(now_ms, arm_id, &resolved).expect("journal append");
            self.journaled_since_snap += 1;
        }
    }

    /// Journal the strategy FILE mount slot `i` opened on ([`CoreConfig::mount_strategy_files`]), once, as
    /// the mount opens: at assembly for a boot mount, in the runtime mount arm for one mounted live. Never
    /// per event. NO-OP without a journal or for a mount no file names, so such a core writes and reads
    /// nothing it did not before.
    pub(crate) fn journal_strategy_file(&mut self, i: usize) {
        let Some(file) = self.config.mount_strategy_files.get(&self.mount_ids[i]) else { return };
        let Some(j) = self.journal.as_mut() else { return };
        let now_ms = self.config.clock.now_ms();
        j.append_strategy_file_mounted(now_ms, &self.mount_ids[i], file).expect("journal append");
        self.journaled_since_snap += 1;
    }

    /// **A BAR step's forecasts, journaled at the bar's CLOSE** (`ctx.now`, the bar's open, plus `interval`'s
    /// length): a bar step decides there (decision 0271), so that is when its forecasts were decided, and
    /// `StudyContext::forecasts_as_of` and the drift watch read `ts` as that instant. Only the forecasts move:
    /// `ctx.now`, by which the step's orders, refusals and lifetimes are judged, stays the bar's open, as the
    /// simulator's does. Taken out of `ctx`, so the drain journals none of them again; an interval with no
    /// length stamps the open. One `is_empty()` for a step that published none.
    pub(crate) fn journal_bar_forecasts(
        &mut self,
        mount_index: usize,
        venue: &str,
        symbol: &str,
        interval: &str,
        ctx: &mut LiveBroker,
    ) {
        // Off (`sinks.forecasts`, REVIEW-LIVE-MONEY M1): left in `ctx`, where the drain drops them unjournaled.
        if ctx.forecasts.is_empty() || !self.config.journal_forecasts {
            return;
        }
        let bar_ms = vike_marketdata::interval::interval_ms(interval).unwrap_or(0);
        let forecasts = std::mem::take(&mut ctx.forecasts);
        self.journal_forecasts(
            mount_index,
            venue,
            symbol,
            ctx.now.saturating_add(bar_ms),
            &forecasts,
        );
    }

    /// Journal the FORECASTS mount slot `mount_index`'s strategy published in one step (book W35), at `decided_ms`:
    /// its broker's `now`, or a bar step's close ([`Self::journal_bar_forecasts`]). One
    /// `vike_journal::JournalRecord::Forecast` each, on the mount's `venue`, a forecast that named no symbol on
    /// `symbol` (the dispatching series', as an order that names none). At most [`MAX_FORECASTS_PER_STEP`], in the
    /// order published; the rest are refused in words and counted (REVIEW-LIVE-MONEY M3), here so the bar step and
    /// the drain share the one cap. Called only for a step that published one, so cold: never the
    /// per-market-message fold. NO-OP without a journal.
    #[cold]
    #[inline(never)]
    pub(crate) fn journal_forecasts(
        &mut self,
        mount_index: usize,
        venue: &str,
        symbol: &str,
        decided_ms: i64,
        forecasts: &[(Option<String>, vike_model::Forecast)],
    ) {
        let kept = forecasts.len().min(MAX_FORECASTS_PER_STEP);
        if kept < forecasts.len() {
            self.refuse_forecasts(mount_index, forecasts.len() - kept);
        }
        let Some(j) = self.journal.as_mut() else { return };
        let mount_id = self.mount_ids.get(mount_index).map_or("", String::as_str);
        for (named, forecast) in &forecasts[..kept] {
            let symbol = named.as_deref().unwrap_or(symbol);
            j.append_forecast(decided_ms, mount_id, venue, symbol, *forecast)
                .expect("journal append");
            self.journaled_since_snap += 1;
        }
    }

    /// Refuse, in words, the `refused` forecasts mount slot `mount_index`'s step published past
    /// [`MAX_FORECASTS_PER_STEP`] (REVIEW-LIVE-MONEY M3): none of them is journaled, and they are counted in
    /// [`CoreThread::forecasts_refused`]. Said when the mount's running count crosses a power of two (its first
    /// refusal, then ever more rarely), so a strategy that overflows every step costs the log a few dozen lines in
    /// its whole life and the fold never one line a step. Cold: only a step over the cap reaches it.
    #[cold]
    #[inline(never)]
    pub(crate) fn refuse_forecasts(&mut self, mount_index: usize, refused: usize) {
        let mount_id = self.mount_ids.get(mount_index).cloned().unwrap_or_default();
        let total = self.forecasts_refused.entry(mount_id.clone()).or_insert(0);
        let before = *total;
        *total = total.saturating_add(refused as u64);
        if before == 0 || total.ilog2() > before.ilog2() {
            tracing::warn!(
                mount_id = mount_id.as_str(),
                refused,
                refused_in_all = *total,
                "a step published more than {MAX_FORECASTS_PER_STEP} forecasts: the {refused} past the cap \
                 were REFUSED and not journaled (the first {MAX_FORECASTS_PER_STEP} were)"
            );
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
    /// `journaled_since_snap` counter: fire a full-state `Snapshot` once the write-ahead counter reaches
    /// `snapshot_every`. `pump_client` needs its own copy because its journaled events land AFTER
    /// `dispatch`'s tail check has already run (and, on the bar-close path, `dispatch`'s tail check
    /// is skipped entirely — that message is not exec-lane) — so without this, a paper-only workload
    /// would never checkpoint until shutdown. No-op when journaling is off.
    ///
    /// ⚠ It fires INSIDE a pump, so a fill the pump folded may still wait for its mount and is then in the account and
    /// not in the mount's ledger ([`Self::write_snap_after_delivery`] cannot drain here). An in-process client's
    /// only (paper, a test's): a venue's events take `dispatch`'s cadence, which delivers first.
    pub(crate) fn maybe_cadence_snap(&mut self, now_ms: i64) {
        if self.journal.is_some()
            && self.journaled_since_snap
                >= self
                    .config
                    .journal
                    .as_ref()
                    .expect("journal is Some only when config.journal is (assemble_core)")
                    .snapshot_every
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
    /// ⚠ **Consumed by the boot restore's refold only** (`crate::replay`'s `refold_records`, which lowers a
    /// `MarginCallLiquidate` as `Command::OrderOn`); the two offline verifiers (`replay_offline`,
    /// `restore_from_journal`) still refuse a multi-engine base (`JournalReplayError::Unsupported`), so
    /// there is no multi-account verification for a stored key to steer. `None` is the venue's
    /// default account, which names the engine only on a core with one account per venue: the
    /// write-ahead records a replay RE-APPLIES (`StrategySubmit`, `OrderEmulationFire`) carry
    /// [`Self::journal_replay_route_key`] instead, which is `Some` for every engine of a core with
    /// several.
    ///
    /// # ⚠ `None` for the default account, so a single-account box's journal BYTES do not move
    ///
    /// Exactly `vike_execution::ReconcileReports::route_key`'s shape, and for the reason its own doc
    /// gives: *"keeping the absent field OFF the wire is what makes a single-account box's journal
    /// bytes identical to the ones written before this field existed."* `None` also allocates
    /// nothing on that box, which is every box in production.
    pub(crate) fn journal_route_key(
        &self,
        engine_index: usize,
        payload_venue: &str,
    ) -> Option<String> {
        let rk = self.engine(engine_index).route_key.as_str();
        (rk != payload_venue).then(|| rk.to_string())
    }

    /// **The route key a WRITE-AHEAD record of an order that lowers by an ENGINE (not by the payload's venue)
    /// must carry**, so a replay can lower it onto the same engine: `Some(key)` when `engine_index`'s engine is a
    /// labelled account (its key is not its venue) or this core holds two engines of one exchange (the
    /// payload's venue then names the default account or is refused as ambiguous), else `None`. The
    /// producer for `StrategySubmit` and `OrderEmulationFire`; [`Self::journal_route_key`] is the
    /// observation records' (`None` there means the venue's default account, which is not enough to
    /// replay by: a record that is RE-APPLIED must name the engine on a core with several).
    ///
    /// `None` on every single-account box, which allocates nothing and writes the bytes it always did.
    /// A labelled box pays one `String` per strategy intent or fired arm: order cadence, off the event
    /// fold. The replay half is `crate::replay`'s `refold_records` (`Command::OrderOn`).
    pub(crate) fn journal_replay_route_key(&self, engine_index: usize) -> Option<String> {
        let e = self.engine(engine_index);
        (self.multi_account || e.route_key != e.venue).then(|| e.route_key.clone())
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

    /// **List one denied order for `CoreSnapshot::denied_orders`**, the typed twin of its ring line: the newest
    /// [`DENIED_ORDERS_KEPT`], each with the venue and symbol of the order refused (`order`, the request in hand;
    /// else engine `engine_index`'s own) and a sequence a reader dedupes by. Called where every `OrderDenied` is
    /// published (`publish_reported`, `broker_drain.rs`'s `deny_intent_as`), never by a venue event.
    ///
    /// Per DENIAL, off the per-message fold: it rebuilds the shared list (one allocation, a refcount per kept entry)
    /// so a publish hands it on by one refcount. No log: the gate already warns once per veto.
    #[cold]
    #[inline(never)]
    pub(crate) fn note_denied(
        &mut self,
        engine_index: usize,
        denied: &vike_model::events::OrderDenied,
        order: Option<(&str, &str)>,
    ) {
        let engine = self.engine(engine_index);
        let listed = self.denied_orders.as_deref().unwrap_or_default();
        let (venue, symbol) = order.unwrap_or_default();
        let or_engine = |named: &str, own: &str| {
            if named.is_empty() { own.to_string() } else { named.to_string() }
        };
        let view = vike_execution::OrderDeniedView {
            sequence: listed.last().map_or(1, |d| d.sequence + 1),
            venue: or_engine(venue, engine.venue.as_str()),
            symbol: or_engine(symbol, engine.symbol.as_str()),
            event: denied.clone(),
        };
        let kept = listed.len().saturating_sub(DENIED_ORDERS_KEPT - 1);
        let next = listed[kept..].iter().cloned().chain(std::iter::once(std::sync::Arc::new(view)));
        self.denied_orders = Some(next.collect());
    }
}

/// How many denied orders the core keeps for `CoreSnapshot::denied_orders`
/// (`CoreThread::note_denied`): the alert mount reads them every summary tick, a few seconds apart.
pub(crate) const DENIED_ORDERS_KEPT: usize = 32;

/// **The write-ahead journal could not be opened: say why, in one line, and exit non-zero.** A
/// launch beside a live node (`io::ErrorKind::AddrInUse`, the single-writer interlock whose message
/// names the directory and the cure), a read-only or full disk: each used to surface as a panic and a
/// backtrace over the real sentence. A core that thinks it is journaling and is not is worse than one
/// that does not start (the reason this was never swallowed), so the start is refused.
///
/// ⚠ **It ends the PROCESS, from a library, on purpose**: `assemble_core` returns no `Result` (every
/// test and both spawn paths call it), and the caller is a root's start-up thread whose only
/// alternative is the panic this replaces. The line goes to the log and to stderr (where a unit's
/// output lands), because an exit does not flush a non-blocking log file. Start-up only, before the
/// first message; nothing on the fold. `tests/journal/double_launch.rs` holds the exit.
pub(super) fn refuse_start_journal_open(dir: &std::path::Path, e: &std::io::Error) -> ! {
    let line = format!(
        "vike-core: refusing to start: the write-ahead command journal at {} cannot be opened: {e}",
        dir.display()
    );
    tracing::error!(target: "vike_core::core", "{line}");
    eprintln!("{line}");
    std::process::exit(1)
}

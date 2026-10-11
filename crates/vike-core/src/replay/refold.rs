//! The shared re-fold engine (`replay_from`) and the state a restore hands back from it.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use vike_exec::{EngineSnapshot, ExecutionEngine, Ingest};
use vike_journal::{CommandJournal, JournalFileConfig, SnapConditional, SnapContingency};

use super::client::InertStrategy;
use super::{
    JournalRecord, QueueClock, ReplayClient, ReplayError, fold_coid_mounts, fold_conditionals,
};
use crate::{CoreConfig, JournalConfig, StrategyMount, spawn_core};

/// Which `Snap` in the journal supplies the restore base (Task 6 factoring).
pub(crate) enum BaseSelect {
    /// The FIRST `Snap` — re-folds the WHOLE post-checkpoint tail. `replay_offline`'s determinism
    /// fence needs a non-trivial tail to reproduce, so it restores from the earliest checkpoint.
    First,
    /// The LATEST `Snap` — re-folds only the crash tail after it (empty on a clean shutdown, where
    /// the exit Snap already reflects everything). The fast restart path (`restore_from_journal`).
    Latest,
}

/// The reproduced state a `replay_from` run ends on — the shared payoff of `replay_offline` (which
/// then fences `final_hash` against the source) and `restore_from_journal` (which just returns the
/// engines/coid). NO fence is applied here; that stays with the callers.
pub(crate) struct Replayed {
    pub(crate) engines: Vec<EngineSnapshot>,
    pub(crate) coid_session: String,
    pub(crate) coid_seq: u64,
    /// the replay core's final emulated-conditional arm-id counter (its exit Snap's stamped
    /// `arm_seq`, floored at `base + tail-armed-count` — see the module doc's undercount note) —
    /// the arm-side twin of `coid_seq`, seeded from the BASE Snap (or the pre-v7 fallback bound)
    /// and advanced by any tail arms, exactly as `coid_seq` is
    pub(crate) arm_seq: u64,
    /// the resting conditional books at the journal's end — the base `Snap`'s books folded
    /// forward through the tail's conditional records ([`fold_conditionals`]), in fire order
    pub(crate) conditionals: Vec<SnapConditional>,
    /// the resting OTO/OCO contingency book at the journal's end — read straight from the replay
    /// core's own exit `Snap` (the mark-less replay core reproduces it faithfully, so no record
    /// fold is needed — see the `contingencies` seed comment in `replay_from`)
    pub(crate) contingencies: Vec<SnapContingency>,
    /// the per-mount attribution ledgers captured on the BASE `Snap` (gap D)
    pub(crate) mount_attr: Vec<vike_journal::SnapMountAttr>,
    /// `(coid, mount_id)` for every order a mount minted, folded from the WHOLE readable record
    /// set ([`fold_coid_mounts`]) — not just the tail, so an order resting since before the base
    /// `Snap` still restores its origin
    pub(crate) coid_mounts: Vec<(String, String)>,
    /// source journal record count (for `ReplayOutcome::records`)
    pub(crate) records: u64,
    /// hash of the replayed exit Snap (the reproduced final state)
    pub(crate) final_hash: u64,
}

/// The parts of the `base`-selected `Snap` that [`replay_from`] restores from.
struct BaseSnap {
    engines: Vec<EngineSnapshot>,
    coid_session: String,
    coid_seq: u64,
    arm_seq: Option<u64>,
    conditionals: Vec<SnapConditional>,
    contingencies: Vec<SnapContingency>,
    mount_attr: Vec<vike_journal::SnapMountAttr>,
}

/// The parts of the replay core's exit `Snap` that [`replay_from`] hands back ([`read_exit_snap`]).
struct ExitSnap {
    final_hash: u64,
    engines: Vec<EngineSnapshot>,
    coid_session: String,
    coid_seq: u64,
    arm_seq: u64,
    contingencies: Vec<SnapContingency>,
}

/// The shared machinery of `replay_offline` + `restore_from_journal` (Task 6): restore the engine
/// from the `base`-selected `Snap`, spawn a real core (pure no-op [`ReplayClient`], [`QueueClock`]
/// replaying the tail's `now_ms`, temp journal with `snapshot_every = u64::MAX`), pump the post-base
/// `Cmd` tail IN ORDER, and read back the replayed exit Snap. Returns [`ReplayError::Empty`] when
/// the journal is empty or carries no `Snap`, [`ReplayError::Unsupported`] for a multi-engine base
/// or a watchdog-tick tail (v1 scope). NO fence — the caller adds it (`replay_offline`) or omits it
/// (`restore_from_journal`).
pub(crate) fn replay_from(dir: &Path, base: BaseSelect) -> Result<Replayed, ReplayError> {
    let records = CommandJournal::read_all(dir)?;
    if records.is_empty() {
        return Err(ReplayError::Empty);
    }
    let records_len = records.len() as u64;

    // Select the restore-base Snap per `base`: First ⇒ earliest checkpoint, Latest ⇒ newest.
    let base_idx = match base {
        BaseSelect::First => records.iter().position(|r| matches!(r, JournalRecord::Snap { .. })),
        BaseSelect::Latest => records.iter().rposition(|r| matches!(r, JournalRecord::Snap { .. })),
    };
    let Some(base_idx) = base_idx else {
        return Err(ReplayError::Empty); // no Snap ⇒ no engine config to restore
    };
    let BaseSnap {
        engines: base_engines,
        coid_session: base_session,
        coid_seq: base_seq,
        arm_seq: base_arm_seq,
        conditionals: base_conditionals,
        contingencies: base_contingencies,
        mount_attr: base_mount_attr,
    } = match &records[base_idx] {
        JournalRecord::Snap {
            engines,
            coid_session,
            coid_seq,
            arm_seq,
            conditionals,
            contingencies,
            mount_attr,
            ..
        } => BaseSnap {
            engines: engines.clone(),
            coid_session: coid_session.clone(),
            coid_seq: *coid_seq,
            arm_seq: *arm_seq,
            conditionals: conditionals.clone(),
            contingencies: contingencies.clone(),
            mount_attr: mount_attr.clone(),
        },
        // NOT a dispatch — `base_idx` came from a `Snap`-matching `position`/`rposition` two
        // statements up, so this arm is unreachable by construction and left as a catch-all
        // deliberately: it can only PANIC, never silently drop a future variant. (Contrast the
        // exhaustive folds below, where a missing arm WOULD be a silent drop.)
        _ => unreachable!("indexed a Snap"),
    };
    // Multi-mount durability (gap D): rebuild the coid -> mount ORIGIN map from the journal's own
    // `StrategySubmit` provenance. Folded over the WHOLE readable record set, not just the post-base
    // tail: an order minted long before the base `Snap` can still be RESTING, and it is exactly that
    // order whose post-restart fill used to land unattributed.
    let coid_mounts = fold_coid_mounts(&records);
    // Emulator PR-3: reconstruct the resting books at the journal's END — the base Snap's books
    // folded forward through the tail's conditional records. A PURE fold of records, deliberately
    // independent of the replay core's own book state (see the module doc's design point 1: the
    // replay core neither fires nor arms trailing stops faithfully, the records do). Also counts
    // the tail's `ConditionalArmed` records for the arm_seq undercount patch below.
    let (restored_conditionals, tail_armed_count) =
        fold_conditionals(base_conditionals.clone(), &records[base_idx + 1..]);
    // The emulated-conditional arm-id counter to seed the replay core with — the arm-side twin of
    // `base_seq`, and resumed for the same reason (see `RestoredState::arm_seq`). A v7+ Snap
    // STAMPS it (`Some`), so the replay core re-mints tail arms under the IDENTICAL ids the live
    // core minted — which is what lets a tail `DisarmConditional` (keyed by arm id) find its arm.
    // A pre-v7 Snap lacks the field (`None`): fall back to max-global-record-seq + 1, a
    // PRUNE-SAFE upper bound on the ids already spent (seqs are globally monotonic across
    // segments and survive pruning in the remaining records; every arm appended at least one
    // record, so ids-spent <= records-ever-written <= max-seq + 1). NEVER the readable record
    // COUNT — `prune_before_latest_snap` deletes whole early segments, so a count restarts a
    // restored session's counter BELOW ids already spent and re-mints duplicates. The overshoot
    // is harmless — but NOT because a pre-v7 journal cannot carry a `DisarmConditional`
    // (versions are per SEGMENT, so a mixed directory absolutely can: a v7+ binary resuming a v6
    // journal appends arms AND disarms into its tail before its first stamping Snap). It is
    // harmless because (a) a tail disarm the replay core misses — its tail arms re-mint under
    // the overshot ids, so a recorded id may not match — is a LOUD replay-neutral no-op inside
    // that core, whose book state is outside both the fence (books are not hashed) and the
    // restore surface, and (b) restore MEMBERSHIP comes from `fold_conditionals` above, which
    // resolves arms by their RECORDED ids and therefore consumes the disarm correctly anyway.
    // A too-HIGH counter only wastes ids; only a too-low one re-mints a spent (disarm-key) id.
    let arm_seq_seed = base_arm_seq
        .unwrap_or_else(|| records.iter().map(vike_journal::record_seq).max().map_or(0, |m| m + 1));

    // v1: single engine only. `spawn_core_multi` would accept pre-built extra engines, but their
    // per-engine seed_cash is not in the snapshot, so a faithful multi-engine restore is deferred.
    if base_engines.len() != 1 {
        return Err(ReplayError::Unsupported(
            "multi-engine replay (v1: single-engine only)".into(),
        ));
    }
    let base = &base_engines[0];

    let tail = replay_tail(records, base_idx);
    if tail.iter().any(|(_, m)| matches!(m, Ingest::Watchdog)) {
        return Err(ReplayError::Unsupported(
            "watchdog-enabled journal not replayable in v1".into(),
        ));
    }
    let stamps: Vec<i64> = tail.iter().map(|(now_ms, _)| *now_ms).collect();

    // Restore the engine and spawn a real core writing to a fresh temp journal. `snapshot_every =
    // u64::MAX` ⇒ no cadence Snaps fire — only the always-on exit Snap, which carries the replayed
    // final hash. `seed_cash` is restored from the base snapshot's `equity_seed` because
    // `spawn_core` OVERWRITES `engine.equity_seed` from `config.seed_cash`, and `equity_seed` is
    // part of `state_hash` — a default (0.0) would diverge for any non-zero-seed source.
    let engine = ExecutionEngine::from_snapshot(base, ReplayClient);
    let temp = unique_temp_dir();
    // `spawn_core` ALSO overwrites `engine.collect_applied_fills` from `config.strategy.is_some()`
    // (`spawn_core_multi`'s mount-presence derivation), clobbering whatever `from_snapshot` just
    // restored from `base`. Unlike `now_ms`/`marks`, `collect_applied_fills` is NOT excluded from
    // `state_hash` — so a mounted-strategy source session (`true`) would otherwise ALWAYS mismatch
    // a mount-free replay (`false`), regardless of how faithfully the tail folds (PR-5 T3's
    // `StrategySubmit` arm above is unrelated to this — it fixes the ORDER flow; this fixes a
    // second, orthogonal config-derived field). Mount an [`InertStrategy`] iff the base had one:
    // that flips the derivation back to `true`, matching `base`, with no other observable effect.
    let strategy = base.collect_applied_fills.then(|| StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        venue: base.venue.clone(),
        symbol: base.symbol.clone(),
        interval: String::new(),
        strategy: Box::new(InertStrategy),
        underlying_symbol: None,
    });
    let cfg = CoreConfig {
        seed_cash: base.equity_seed,
        clock: Box::new(QueueClock::new(stamps)),
        coid_session: Some((base_session, base_seq)),
        arm_seq: arm_seq_seed,
        // Seed the replay core's books from the base Snap (emulator PR-3), exactly as a live
        // restart does — so a tail `DisarmConditional` targeting an arm minted BEFORE the base
        // finds it (pre-PR-3 that disarm missed and refused loudly), and the replay core's own
        // exit Snap carries a meaningful book. Membership for the RESTORE still comes from the
        // record fold above, not from this core (module doc, design point 1).
        conditionals: base_conditionals,
        // Seed the replay core's OTO/OCO book from the base Snap (live-runtime OCO/OTO), exactly as
        // a live restart does — so a tail fill that RELEASES a held child / cancels an OCO sibling
        // of a bracket submitted BEFORE the base is reproduced (its held request came off the Snap,
        // not the tail), and the replay core's exit Snap carries the true contingency state.
        // UNLIKE conditionals, the RESTORE membership below reads straight from this core's exit
        // Snap (not a record fold): a contingency leg has no mark-seeded field the mark-less replay
        // core mishandles — it is driven purely by journaled fills — so the replay core reproduces
        // it faithfully.
        contingencies: base_contingencies,
        strategy,
        journal: Some(JournalConfig {
            dir: temp.to_path_buf(),
            file: JournalFileConfig::default(),
            snapshot_every: u64::MAX,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);

    // Pump the tail IN ORDER: events onto the lossless event lane, commands onto the same lane via
    // send_command. Both feed the ONE FIFO ingest queue, so dispatch order == journal order.
    let events = handle.event_sender();
    for (_, msg) in tail {
        match msg {
            Ingest::Event(e) => {
                // `CoreGone` = the replay core died mid-tail. The replayed state then diverges from
                // the source journal and the caller's hash fence (`HashMismatch`) refuses it.
                let _ = events.blocking_send(e);
            }
            Ingest::Command(c) => handle.send_command(c),
            Ingest::Watchdog => unreachable!("watchdog tail rejected above"),
            // Market/Bar*/Quote/Trade/Book are never journaled (Task 4 journals only the exec lane),
            // so they cannot appear here; skip defensively rather than panic on a future format.
            Ingest::Market
            | Ingest::BarSeed(_)
            | Ingest::BarClose(_)
            | Ingest::Quote(_)
            | Ingest::Trade(_)
            | Ingest::Book(_)
            | Ingest::StreamStatus(_)
            | Ingest::Flow(_) => {}
        }
    }
    handle.shutdown_and_join(); // delivers Shutdown losslessly after the tail, then joins

    let ExitSnap {
        final_hash,
        engines,
        coid_session,
        coid_seq,
        arm_seq,
        contingencies: restored_contingencies,
    } = read_exit_snap(&temp, arm_seq_seed)?;
    // (the replay journal directory is removed by `temp`'s guard when this function returns --
    // including on every `?` above and on a panic, which the old explicit removal here did not)

    // The arm-counter undercount patch (module doc, design point 1): a tail TRAILING arm is
    // refused by the mark-less replay core, skipping its mint — so the replayed exit `arm_seq`
    // can land BELOW the ids the live session actually spent. Every live mint since the base
    // wrote exactly one `ConditionalArmed` record, so `seed + tail-armed-count` is the true
    // spent bound; take the max so a restored session can never re-mint a spent id (the id is
    // the disarm key). For a pre-v7 base the seed is itself an overshoot bound — still sound.
    let arm_seq = arm_seq.max(arm_seq_seed + tail_armed_count);

    Ok(Replayed {
        engines,
        coid_session,
        coid_seq,
        arm_seq,
        conditionals: restored_conditionals,
        contingencies: restored_contingencies,
        mount_attr: base_mount_attr,
        coid_mounts,
        records: records_len,
        final_hash,
    })
}

/// [`replay_from`]'s tail extraction: the records after `base_idx` that replay re-applies, in file
/// order, each paired with its recorded `now_ms`.
fn replay_tail(records: Vec<JournalRecord>, base_idx: usize) -> Vec<(i64, Ingest)> {
    // tail = every Cmd/StrategySubmit positioned AFTER the base Snap, consumed by value (Ingest
    // moves out — no clone), keeping each record's recorded now_ms for the QueueClock seed (same
    // order). StrategySubmit (PR-5 T2/T3) carries a mounted strategy's ALREADY-RESOLVED OrderIntent
    // (mint/tag resolution already happened at the `drain_broker` boundary before it was journaled,
    // same as `apply_strategy_intent`'s doc describes) — replaying it as `Command::Order(intent)`
    // folds it through the SAME `apply_intent` site a `Cmd`-carried `Command::Order` would, so a
    // mounted-strategy session now replays deterministically instead of the tail silently dropping
    // its order flow (the module doc's old "out of v1 scope" note, now stale). `filter_map`
    // preserves file order, so a StrategySubmit interleaves with surrounding Cmd records correctly
    // without any extra merge step.
    records
        .into_iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            JournalRecord::Cmd { now_ms, msg, .. } if i > base_idx => Some((now_ms, msg)),
            JournalRecord::StrategySubmit { now_ms, intent, .. } if i > base_idx => {
                Some((now_ms, Ingest::Command(vike_exec::Command::Order(intent))))
            }
            // Emulator-journal PR-1: a conditional FIRE replays as the RELEASE it decided —
            // `Submit(req)` with the coid still empty, through the SAME `apply_intent` site the
            // live fire used, so the replay core re-mints the identical coid from the restored
            // generator. The trigger itself is NEVER re-evaluated (the bar/tick that crossed it is
            // not journaled and never will be): the DECISION is the record. `trigger_px` is
            // diagnostic only and does not participate.
            JournalRecord::ConditionalFire { now_ms, req, .. } if i > base_idx => Some((
                now_ms,
                Ingest::Command(vike_exec::Command::Order(vike_exec::OrderIntent::Submit(
                    Box::new(req),
                ))),
            )),
            // Emulator PR-4: a margin-call auto-liquidation replays as the RELEASE it decided —
            // `Submit(req)` with the coid still empty, through the SAME `apply_intent` site the live
            // sweep used, so the replay core re-mints the identical coid. The breach itself is NEVER
            // re-evaluated (the closed bar + equity that triggered it are not journaled and never
            // will be): the DECISION is the record, exactly as for `ConditionalFire`.
            JournalRecord::MarginCallLiquidate { now_ms, req, .. } if i > base_idx => Some((
                now_ms,
                Ingest::Command(vike_exec::Command::Order(vike_exec::OrderIntent::Submit(
                    Box::new(req),
                ))),
            )),

            // ── EXHAUSTIVE by design: NO `_` arm. A new `JournalRecord` variant must fail to
            // compile HERE until someone decides whether replay re-applies it — see the "Adding a
            // variant" contract on [`JournalRecord`]. Silently dropping one is the bug class this
            // shape exists to make impossible (PR #915's `MarginCallLiquidate` mount_id gap).
            //
            // (a) The four REPLAYABLE records above, at a PRE-BASE position (`i <= base_idx`):
            // already folded into the base `Snap`, so re-applying them would DOUBLE the state.
            // (The guarded arms above cannot cover a variant for exhaustiveness purposes, which is
            // exactly why this arm is spelled out rather than implied.)
            JournalRecord::Cmd { .. }
            | JournalRecord::StrategySubmit { .. }
            | JournalRecord::ConditionalFire { .. }
            | JournalRecord::MarginCallLiquidate { .. } => None,

            // (b) REPLAY-NEUTRAL at ANY position — checkpoints, observations and decision markers
            // that move no fenced state. Each variant's own doc carries the full argument:
            //   `Snap`                — the restore BASE itself, not a command to re-apply.
            //   `MintedSubmit`        — the mint is re-derived by re-applying the empty-coid submit
            //                           through the SAME `apply_intent` site; replaying the record
            //                           too would submit the order twice.
            //   `PortfolioSnap`       — a periodic observation; carries no engine state at all.
            //   `ConditionalArmed`    — the arm replays from its own write-ahead `Cmd`/
            //                           `StrategySubmit`; this record only RESOLVES its terms (and
            //                           `fold_conditionals` reads it for book membership).
            //   `ConditionalDisarmed` — ditto; the disarm intent already carries the whole `arm_id`.
            //   `GtdExpire`           — a DECISION MARKER: the sweep's `cancel_order` publishes
            //                           nothing locally, and the venue's authoritative
            //                           `OrderCanceled` replays as its own `Cmd`.
            //   `ScheduleFire`        — a DECISION MARKER: `on_schedule`'s orders each ride their
            //                           own `StrategySubmit`, so re-firing would double them.
            JournalRecord::Snap { .. }
            | JournalRecord::MintedSubmit { .. }
            | JournalRecord::PortfolioSnap { .. }
            | JournalRecord::ConditionalArmed { .. }
            | JournalRecord::ConditionalDisarmed { .. }
            | JournalRecord::GtdExpire { .. }
            | JournalRecord::ScheduleFire { .. } => None,
        })
        .collect()
}

/// Read back the replay core's exit `Snap` from its temp journal at `dir` (for [`replay_from`]).
fn read_exit_snap(dir: &Path, arm_seq_seed: u64) -> Result<ExitSnap, ReplayError> {
    // Read the replay journal's final Snap: it carries the reproduced hash / engines / coid state
    // + the replayed arm counter (this build always stamps `arm_seq`, so the `unwrap_or` below is
    // for shape only — the replay core's exit Snap is v7 by construction).
    let replayed = CommandJournal::read_all(dir)?;
    replayed
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap {
                hash,
                engines,
                coid_session,
                coid_seq,
                arm_seq,
                contingencies,
                ..
            } => Some(ExitSnap {
                final_hash: *hash,
                engines: engines.clone(),
                coid_session: coid_session.clone(),
                coid_seq: *coid_seq,
                arm_seq: arm_seq.unwrap_or(arm_seq_seed),
                contingencies: contingencies.clone(),
            }),
            // every other record: not this tap's business (audited 2026-10, I-21)
            _ => None,
        })
        .ok_or_else(|| ReplayError::Unsupported("replay produced no snapshot".into()))
}

/// The state a crash restart restores from the journal (Task 6): the reproduced engine
/// snapshot(s) + the coid session/seq to resume so post-restart order ids never collide with a
/// still-open prior-session id. Caller pattern: `from_snapshot(&engines[0], real_client)` →
/// re-attach `exec_db` → `spawn_core` with `coid_session: Some((coid_session, coid_seq))`,
/// `arm_seq` (resume BOTH or neither — see that field), `conditionals` (re-arm the books — see
/// that field) and
/// `journal: Some(same dir)` (append resumes — seq continues) → venue reconcile repairs any tail
/// the venue saw while down.
#[derive(Debug, Clone)]
pub struct RestoredState {
    pub engines: Vec<EngineSnapshot>,
    pub coid_session: String,
    pub coid_seq: u64,
    /// The emulated-conditional arm-id counter to resume ([`crate::CoreConfig::arm_seq`]) — the
    /// arm-side twin of `coid_seq`, and needed for the SAME reason: an arm id is
    /// `{coid_session}a{arm_seq}` and the session above is restored, so a counter restarting at 0
    /// would re-emit ids the pre-restart run already wrote into this very journal (resume APPENDS
    /// to the same directory) — and since emulator PR-2 the id is the DISARM key, so a duplicate
    /// would let a `DisarmConditional` target the wrong arm.
    ///
    /// The value is the journal's PERSISTED counter: the base `Snap` stamps `arm_seq` next to
    /// `coid_seq` (v7), advanced by any tail arms the restore re-folded — exactly the `coid_seq`
    /// mechanism. The pre-v7 shape (the restored journal's readable RECORD COUNT) was
    /// prune-UNSAFE: [`vike_journal::CommandJournal::prune_before_latest_snap`] deletes whole
    /// early segments (its own doc prescribes prune-then-restore at restart), so the count could
    /// land BELOW the ids already spent and a restored session re-minted duplicates. A pre-v7
    /// `Snap` (no stamped field) falls back to max-global-record-seq + 1 — still a sound upper
    /// bound under pruning, because record seqs are globally monotonic across segments and every
    /// arm appended at least one record.
    pub arm_seq: u64,
    /// The resting emulated-conditional books to re-arm ([`crate::CoreConfig::conditionals`]) —
    /// emulator PR-3's whole point: a live position's protective stop survives the restart. The
    /// latest `Snap`'s captured books (a trailing arm's ratcheted extreme included) folded
    /// forward through the crash tail's conditional records (arms added, fires/disarms/
    /// mass-cancels removed — [`fold_conditionals`]). Empty for a pre-v8 journal (its Snaps
    /// carry no books) and for a session with nothing armed — both restore to empty books,
    /// exactly the pre-PR-3 behavior, never an error. Seed it together with `coid_session` and
    /// `arm_seq`: the re-armed ids are these entries' `arm_id`s, and the counter must resume
    /// ABOVE them.
    pub conditionals: Vec<SnapConditional>,
    /// The resting OTO/OCO contingency book to re-arm ([`crate::CoreConfig::contingencies`]) — the
    /// live-runtime OCO/OTO twin of `conditionals`: a live position's protective bracket (its held
    /// take-profit / stop-loss) survives the restart. Reproduced by re-folding the crash tail
    /// through the replay core (base `Snap`'s book seeded, tail fills re-driving release/cancel) and
    /// read from that core's exit `Snap` — no separate record fold, since the contingency book has
    /// no mark-seeded field the mark-less replay core mishandles. Empty for a pre-v11 journal (its
    /// Snaps carry no contingency state) and for a session with no brackets — both restore empty,
    /// exactly the pre-OCO/OTO behavior, never an error.
    pub contingencies: Vec<SnapContingency>,
    /// The per-mount fill-ATTRIBUTION ledgers to re-seed ([`crate::CoreConfig::mount_attr`]) —
    /// multi-mount durability (gap D): without them a restart resumed every mount ledger at zero
    /// while `Account.realized_pnl` kept the same PnL, so `Σ mounts ≠ account` from the first
    /// restart onward. The LATEST `Snap`'s captured rows, keyed by `mount_id`. Empty for a pre-v13
    /// journal (its Snaps carry no ledgers) and for a mount-free session — both restore zeroed,
    /// exactly the pre-feature behavior, never an error.
    ///
    /// DOCUMENTED STALENESS, the same shape (and same safe direction) as `conditionals`: this is the
    /// base `Snap`'s value, NOT folded forward through the crash tail. A clean shutdown always writes
    /// an exit `Snap`, so the common path is exact; an unclean crash loses attribution for the fills
    /// between the last cadence `Snap` and the crash. Those fills are still in `Account` (they
    /// replay as journaled `Cmd` events), so the discrepancy shows up in the published RESIDUAL row
    /// rather than vanishing — measurable, not silent.
    pub mount_attr: Vec<vike_journal::SnapMountAttr>,
    /// The coid -> mount ORIGIN map to re-seed ([`crate::CoreConfig::coid_mounts`]) — one
    /// `(client_order_id, mount_id)` per order a mount minted, folded from the journal's existing
    /// `StrategySubmit`/`MintedSubmit` provenance ([`fold_coid_mounts`]) over the WHOLE readable
    /// record set. This is what lets an order still RESTING across the restart book its eventual fill
    /// into the mount that placed it. Empty for a journal with no mounted-strategy submits.
    pub coid_mounts: Vec<(String, String)>,
}

/// A fresh, collision-free working directory for one replay core's journal, OWNED — removed when
/// the returned guard drops, on every path including a panic.
///
/// Uniqueness is the process id + a nanosecond stamp + a per-call counter, so concurrent
/// `replay_offline` calls never share one.
///
/// ⚠ **This is a journal directory, which makes the ownership the expensive half.**
/// `crates/vike-journal/src/segment.rs`'s `reserve_blocks` calls `posix_fallocate` and `JournalConfig::at`
/// defaults to 64 MiB segments, so a segment is FULLY allocated the moment it is created rather
/// than sparse. A leaked replay directory is a real 64 MiB. `replay_from` did remove this at the
/// end — but only on the success path: every `?` above that line, and every panic, left one behind.
/// That is the same shape that put 215 GB of leaked scratch on the CI box, and
/// [`vike_model::scratch::ScratchDir`] is what closes it here.
///
/// ⚠ **The PARENT is still the system temp directory, and that is a KNOWN debt with a row in
/// `crates/vike-ops/tests/hygiene/system_temp_gate/pin.rs`'s `SYSTEM_TEMP_PIN`, not an oversight.** Production
/// scratch belongs in `<project>/tmp`, and a library may not resolve that itself — the root has to
/// arrive as a parameter from the composition root, which here means a new argument on
/// [`replay_offline`] and [`restore_from_journal`] and therefore on every one of their ~55 call
/// sites. That is a mechanical change, not a hard one, and it is the repair. Ownership was split
/// out and landed first because it needs no signature at all and removes the leak today.
fn unique_temp_dir() -> vike_model::scratch::ScratchDir {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tag = format!("vike-replay-{}-{nanos}-{n}", std::process::id());
    vike_model::scratch::ScratchDir::create_in(&std::env::temp_dir(), &tag)
        .expect("a replay working directory under the system temp directory")
}

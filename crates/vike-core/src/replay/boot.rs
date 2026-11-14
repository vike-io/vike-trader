//! The BOOT RESTORE (`docs/decisions/0127-the-live-daemon-restores-its-core-from-the-journal-at-boot.md`):
//! what a restarted daemon takes back from its write-ahead journal before either mount assembles a
//! core, and how deep the journal let it go.
//!
//! # The one call
//!
//! [`restore_for_boot`] runs ONCE per process, in the composition root (`vike-tradehub`'s `run`),
//! after the journal is resolved and BEFORE the live core opens it — so before
//! `vike_journal::Journal::open` closes a last segment that ends in a torn frame, and while no
//! writer holds the directory. It reads the journal without the lock (only the write path locks),
//! never touches the fold, and hands back a [`RestoreReport`]; the root logs its line
//! ([`RestoreReport::log_startup_line`]) and the mount that builds the core fills its `CoreConfig`
//! with [`RestoreReport::apply_to`] — one function for the paper and the live mount, so they cannot
//! disagree about what a journal restores.
//!
//! # How deep (the [`RestoreOutcome`])
//!
//! - **Full** — the latest `Snapshot` plus its crash tail, refolded through a replay core
//!   (`refold_records` under `JournalReplayRefoldOptions::restore`: several engines, waker records skipped,
//!   scratch under the caller's root: `<settings>/state/restore-scratch` for `vike-tradehub`).
//! - **SnapOnly** — the refold could not run (a record of a labelled or doubled venue engine that
//!   names no engine, which only a journal written before the route key can hold; a panic; a scratch
//!   failure): the latest `Snapshot` and the PURE folds of its tail (books, owners, minted and ended
//!   orders, the id counters as overshooting bounds). The account state is the `Snapshot`'s.
//! - **TornTail** — the read halted at a torn frame of the LAST written segment (an ordinary
//!   crash): that segment is COPIED to `<dir>/torn/` first (the live open closes it, and a later
//!   prune deletes it), the newest [`TORN_COPIES_KEPT`] copies are kept, and the readable prefix
//!   restores as above. A KILL mid-frame leaves no length, so its read ends clean: no copy, no note, and the
//!   live open closes that segment too (`crates/vike-journal/src/writer.rs`'s `close_dirty_tail`).
//! - **Broken** — the read halted with records written AFTER the corrupt frame, or at a frame
//!   another build wrote (`ParseFailed`): NOTHING is restored from it, the directory is moved aside
//!   to `<dir>.broken-<stamp>` and the live core starts a fresh journal (otherwise every later read
//!   stops at the same frame for good). The last readable `Snapshot` is still read, to name what is
//!   lost: one PROTECTION LOST line counts the conditional arms and held exits, and each lost held
//!   exit's mount hears a terminal `OrderRejected` ([`RESTORE_LOST_REASON`]) at boot.
//! - **Fresh** — no directory, no record, or no `Snapshot`.
//!
//! An unreadable directory or a journal written by a NEWER build is not an outcome: it is an `Err`
//! the root turns into a refused start — the live open would fail on the same directory a moment
//! later, and a core that thinks it is journaling but is not is worse than one that does not start.
//! After a Full or Fresh restore, segments the latest `Snapshot` superseded are pruned (never after a
//! snap-only, torn or broken read: those keep their evidence). The whole restore holds the journal's
//! single-writer lock (`vike_journal::lock::JournalLock`), released only for a move aside, so a second
//! daemon started beside a live one is refused and changes nothing.
//!
//! # Precedence (applied by `assemble_core`)
//!
//! Settings and the run profile set every LIMIT (the journal's `RiskLimits` are never applied). The
//! journal is the one source of the order client id session and counters, the conditional and contingency
//! books and the arms' owners, and each engine's account state (positions, PnL, fees, balances, all
//! three fill-dedup sets, the gate's rate window), matched to a live engine by ROUTE KEY. The
//! ownership file (`crate::order_owners`, decision 0113) decides who owns an order and every
//! ledger; the journal fills only what the file does not know. The venue's first successful
//! reconcile pass decides what still rests (decision 0120). A journaled trading state applies only
//! when it is STRICTER than the boot's.
//!
//! # The gateway cursors (journal v18)
//!
//! A tradehub that reaches its venues through a gateway daemon journals each venue event as an
//! `Ingest::GatewayEvent`, one record carrying the event and its position in the gateway's
//! per-account ring. The restore hands those positions back ([`RestoreReport::gateway_cursors`], and
//! `CoreConfig::gateway_cursors` so the next `Snapshot` keeps them). They are the resume point of the
//! trade session, never state the restore applies, and a cursor may only be as far along as the
//! engine state it comes back beside:
//!
//! - **Full** — per account, the latest `Snapshot`'s cursor advanced by every `GatewayEvent` of its
//!   tail, because the refold folded those events into the engines like any other venue event. A
//!   resume asks for the event AFTER the cursor and nothing is folded twice. Only an account the
//!   `Snapshot` holds an ENGINE for advances: the refold builds its replay core from the `Snapshot`'s engines
//!   alone, so the tail events of an account armed after the checkpoint reached no engine and keep the
//!   `Snapshot`'s cursor (or none: the session opens fresh and a reconcile recovers what it can).
//! - **SnapOnly** (and a torn tail that could not refold) — the latest `Snapshot`'s cursors UNADVANCED.
//!   The pure folds read the tail's events only for their facts (minted and ended orders); no tail
//!   event reaches any engine, so a cursor past the `Snapshot` would tell the gateway the tail's fills
//!   and funding were already applied and it would never send them again. An account the `Snapshot` has
//!   no cursor for opens its session with no cursor (a reconcile then recovers what it can).
//!
//! What a replayed event can still do to a book the
//! restore and a reconcile pass already settled is pinned by
//! `crates/vike-core/src/runtime/tests/boot_restore.rs`'s `a_fill_the_pass_booked_is_not_booked_again_by_the_ring_replay`
//! and its twin.
//!
//! # What it does not restore
//!
//! The per-mount budget latch, the PnL-curve peak and the held reconcile alerts are in no `Snapshot`;
//! a trailing stop's extreme is the last `Snapshot`'s; the stuck-order and dead-man sweeps of a skipped
//! waker tail are not reproduced (such an order can come back as a candidate, and the pass decides
//! it). No journaled order enters an engine registry at boot.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use vike_execution::EngineSnapshot;
use vike_journal::lock::JournalLock;
use vike_journal::{
    Journal, JournalCorruptCause, JournalReadHalt, JournalSnapshotContingency,
    JournalSnapshotMountAttribution, JournalSnapshotOrderEmulationArm, JournalStrategyFile,
    OrderHeldRelease,
};
use vike_model::EventRingCursor;

use super::fold::{
    JournalReplayTailFacts, TradeClosedTail, closed_tail, fold_gateway_cursors,
    fold_order_client_id_origins, resting_orders, tail_facts,
};
use super::refold::{JournalReplayBaseSelection, JournalReplayRefoldOptions, refold_records};
use super::{JournalRecord, fold_conditionals};
use crate::{CoreConfig, JournalConfig};

/// The reason a mount hears on the synthetic terminal `OrderRejected` of a HELD exit a broken
/// journal could not restore: the exit was kept off the venue by the core that died, so nothing
/// holds it any more.
pub const RESTORE_LOST_REASON: &str = "restart: held exit lost with a broken journal";

/// The reason a mount hears on the synthetic `Canceled` of a restored order whose engine is a
/// PAPER book: that book lives in the process's memory, so a restart emptied it.
pub const RESTORE_PAPER_BOOK_LOST_REASON: &str = "restart: a paper book does not survive a restart";

/// How many copies of a torn segment `<journal_dir>/torn/` keeps (the owner's ruling: the last 3).
pub const TORN_COPIES_KEPT: usize = 3;

/// One order the journal says a mount left resting — an OWNERSHIP CANDIDATE for
/// `crate::CoreConfig::journal_orders`. Ownership only supplements the ownership file (the journal's
/// price and trigger price do win over the line's, which a modify made stale): an order client id the
/// file knows keeps the file's owner. Never a registry entry: the first reconcile pass judges it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OrderOwnerCandidate {
    pub order_client_id: String,
    /// The minting mount's id (`crate::strategy_state::mount_id_with`).
    pub mount_id: String,
    pub venue: Option<String>,
    pub symbol: Option<String>,
    /// The account LABEL TEXT of the engine the order is on (`None` for the default account).
    pub account: Option<String>,
    /// The order's limit price, which the reconcile adoption puts back on the registry entry it
    /// rebuilds from a venue report that carries none (decision 0120's verdict 8, amended).
    pub price: Option<f64>,
    /// Whether the order was `reduce_only`: the reconcile adoption puts it back, because a venue
    /// report carries no such flag and an adopted stop that forgot it would no longer be a stop that
    /// only reduces.
    pub reduce_only: bool,
    /// The stop's trigger price, which no venue report carries either (see [`Self::reduce_only`]).
    pub trigger_price: Option<f64>,
    /// The order's lifetime, which no venue report carries either. `None`: the registry entry was itself
    /// rebuilt from a report (`vike_model::OrderRecord`'s `created_ms` is empty), so the journal's `GTC`
    /// there is the type's default and no knowledge; the ownership line says what it does remember.
    pub time_in_force: Option<vike_model::TimeInForce>,
    /// The good-till-date deadline that goes with [`Self::time_in_force`].
    pub gtd_expiry: Option<i64>,
    /// The order's kind as the registry held it (a venue report names its own kind, and the ownership
    /// file's `own` line carries it from the mint: this supplements a line that does not).
    pub order_type: Option<vike_model::OrderType>,
    /// The tier the snapshot's engine stated, IN MEMORY ONLY: the boot adopts an operator's order (a
    /// candidate under the pseudo mount) only on a `demo` or `live` book, never `paper` or unstated
    /// (`runtime/boot_restore.rs`'s `merge_journal_owners`). `None` for a candidate that is not an
    /// engine-registry one (a tail mint, a held exit) and for an engine that states none.
    pub tier: Option<vike_model::AccountTier>,
}

/// A HELD exit (an OTO child kept off the venue until its parent fills) that a broken journal took
/// with it: its mount is told with a terminal reject at boot.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreLostHeldExit {
    pub order_client_id: String,
    pub venue: String,
    pub symbol: String,
}

/// How deep a boot restore went; the module doc describes each.
#[derive(Debug, Clone, PartialEq)]
pub enum RestoreOutcome {
    /// No journal is configured: nothing is read, no line is logged, nothing is applied.
    Off,
    Fresh {
        why: String,
    },
    Full,
    SnapOnly {
        why: String,
    },
    /// `refolded`: the readable prefix restored through the replay core (else by the pure folds).
    TornTail {
        cause: JournalCorruptCause,
        kept_evidence: Option<PathBuf>,
        refolded: bool,
    },
    Broken {
        cause: JournalCorruptCause,
        moved_to: Option<PathBuf>,
    },
}

/// What the journal restores, as `CoreConfig` will hold it.
#[derive(Debug, Clone)]
struct BootState {
    order_client_id_session: (String, u64),
    arm_sequence: u64,
    order_emulation_arms: Vec<JournalSnapshotOrderEmulationArm>,
    /// `(arm_id, mount_id)` of every restored arm a mount armed.
    conditional_owners: Vec<(String, String)>,
    contingencies: Vec<JournalSnapshotContingency>,
    /// The orders held for their release (`Snapshot.held_releases`).
    held_releases: Vec<OrderHeldRelease>,
    /// The emulated parents (`Snapshot.emulated`, decision 0275).
    emulated: Vec<vike_journal::JournalSnapshotOrderEmulation>,
    engines: Vec<EngineSnapshot>,
    journal_orders: Vec<OrderOwnerCandidate>,
    ledgers: Vec<JournalSnapshotMountAttribution>,
    /// Where each gateway ring stood in the engine state this restore hands back (journal v18): the
    /// latest `Snapshot`'s cursors, advanced over its tail's `GatewayEvent` records only when the refold
    /// folded that tail (Full), one per route key.
    gateway_cursors: Vec<(String, EventRingCursor)>,
    /// Every mount's closed-trade ledger as the latest `Snapshot` holds it, the archive's rows put in front of its
    /// own (`with_archived_rows`).
    closed_trades: Vec<vike_journal::JournalSnapshotClosedTrades>,
    /// What the ledgers book from the tail (`closed_tail`): only when the refold folded that tail (Full), so
    /// the ledgers move with the engines' positions.
    closed_tail: Vec<TradeClosedTail>,
}

/// The counts the startup line reports.
#[derive(Debug, Clone, Default)]
struct Report {
    snap_sequence: Option<u64>,
    tail_records: usize,
    skipped_wakers: u64,
    /// Ring notes (and log lines) beyond the startup line, in order.
    notes: Vec<String>,
}

/// The result of [`restore_for_boot`]: the outcome, what it restores, and what it says.
#[derive(Debug, Clone)]
pub struct RestoreReport {
    outcome: RestoreOutcome,
    dir: Option<PathBuf>,
    state: Option<BootState>,
    lost_held_exits: Vec<RestoreLostHeldExit>,
    /// The late-retry memory ([`Self::recent_order_client_ids`]): kept beside `state`, because a journal with
    /// records and no checkpoint restores nothing else and still remembers the ids it used.
    recent_order_client_ids: Vec<(String, i64)>,
    /// Which strategy file each mount last ran ([`Self::strategy_files`]): a statement about the journal,
    /// never state a core applies, so it is kept beside `state` too.
    strategy_files: Vec<(String, JournalStrategyFile)>,
    report: Report,
}

impl RestoreReport {
    /// No journal: [`RestoreOutcome::Off`], nothing to apply and no line.
    pub fn off() -> Self {
        RestoreReport {
            outcome: RestoreOutcome::Off,
            dir: None,
            state: None,
            lost_held_exits: Vec::new(),
            recent_order_client_ids: Vec::new(),
            strategy_files: Vec::new(),
            report: Report::default(),
        }
    }

    fn fresh(dir: &Path, why: &str) -> Self {
        RestoreReport {
            outcome: RestoreOutcome::Fresh { why: why.to_string() },
            dir: Some(dir.to_path_buf()),
            ..Self::off()
        }
    }

    /// How deep the restore went.
    pub fn outcome(&self) -> &RestoreOutcome {
        &self.outcome
    }

    /// The restored order client id session and counter, `None` when nothing was restored.
    pub fn order_client_id_session(&self) -> Option<&(String, u64)> {
        self.state.as_ref().map(|s| &s.order_client_id_session)
    }

    /// The restored arm-id counter (0 when nothing was restored).
    pub fn arm_sequence(&self) -> u64 {
        self.state.as_ref().map_or(0, |s| s.arm_sequence)
    }

    /// The engine snapshots whose account state the core puts back by route key.
    pub fn restored_engines(&self) -> &[EngineSnapshot] {
        self.state.as_ref().map(|s| s.engines.as_slice()).unwrap_or(&[])
    }

    /// The resting-order ownership candidates.
    pub fn journal_orders(&self) -> &[OrderOwnerCandidate] {
        self.state.as_ref().map(|s| s.journal_orders.as_slice()).unwrap_or(&[])
    }

    /// The restored conditional books, in fire order.
    pub fn conditionals(&self) -> &[JournalSnapshotOrderEmulationArm] {
        self.state.as_ref().map(|s| s.order_emulation_arms.as_slice()).unwrap_or(&[])
    }

    /// The restored contingency book.
    pub fn contingencies(&self) -> &[JournalSnapshotContingency] {
        self.state.as_ref().map(|s| s.contingencies.as_slice()).unwrap_or(&[])
    }

    /// Where each gateway's per-account event ring stood in the engine state this restore hands
    /// back (journal v18), one `(route key, cursor)` per account: the resume point a trade session
    /// presents so the gateway replays what the tradehub missed while it was down. After a Full
    /// restore, per route key the cursor of the LAST `GatewayEvent` record, else the latest `Snapshot`'s;
    /// after a snap-only one the latest `Snapshot`'s, never advanced (the tail's events were not folded
    /// into any engine). Empty when nothing was restored (no journal, a fresh or broken one) or the
    /// journal never held a gateway event, and an account absent from it must open its session with
    /// NO cursor (a reconcile then recovers what it missed).
    pub fn gateway_cursors(&self) -> &[(String, EventRingCursor)] {
        self.state.as_ref().map(|s| s.gateway_cursors.as_slice()).unwrap_or(&[])
    }

    /// The held exits a broken journal lost (empty unless [`RestoreOutcome::Broken`]).
    pub fn lost_held_exits(&self) -> &[RestoreLostHeldExit] {
        &self.lost_held_exits
    }

    /// THE LATE-RETRY MEMORY (decision 0270, D7): `(order client id, created_ms)` of every id the journal
    /// says the node used, built ONCE here as the union of the base `Snapshot`'s carried memory, every order of
    /// its engine registries (ended ones too; one rebuilt from a venue report, which has no `created_ms`,
    /// at the `Snapshot`'s own time), and every id the tail used (minted, or named by a command, answered or
    /// not), pruned to the window at the restore's own clock read and capped (`prune_carried`). A journal
    /// with records and no checkpoint remembers its records' ids; a fresh, broken or absent one nothing.
    pub fn recent_order_client_ids(&self) -> &[(String, i64)] {
        &self.recent_order_client_ids
    }

    /// WHICH VERSION OF THE FILE TRADED: `(mount id, strategy file)` of every mount the journal names, in
    /// the order it first names them: the latest `Snapshot`'s own rows, then each
    /// `vike_journal::JournalRecord::StrategyFileMounted` after it (a later one replaces its mount's), or
    /// every such record of a journal with no `Snapshot`. Read from whatever was readable, so a broken journal
    /// still says it; each is also a note ([`Self::notes`]). Empty for a journal that never mounted a file.
    pub fn strategy_files(&self) -> &[(String, JournalStrategyFile)] {
        &self.strategy_files
    }

    /// The notes the restore puts on the core's recent-events ring (and logs), after its line.
    pub fn notes(&self) -> &[String] {
        &self.report.notes
    }

    /// THE startup line, `None` when the journal is off: `journal restore: <outcome> from <dir>
    /// (…counts…)`. One per boot.
    pub fn startup_line(&self) -> Option<String> {
        let dir = self.dir.as_deref()?;
        let what = match &self.outcome {
            RestoreOutcome::Off => return None,
            RestoreOutcome::Fresh { why } => format!("fresh ({why})"),
            RestoreOutcome::Full => "full".to_string(),
            RestoreOutcome::SnapOnly { why } => format!("snap-only ({why})"),
            RestoreOutcome::TornTail { cause, refolded, .. } => format!(
                "torn tail ({}, {})",
                cause.as_str(),
                if *refolded { "readable prefix refolded" } else { "readable prefix, snap-only" }
            ),
            RestoreOutcome::Broken { cause, moved_to } => format!(
                "BROKEN ({}), nothing restored, {}",
                cause.as_str(),
                moved_to.as_ref().map_or_else(
                    || "the directory could NOT be moved aside".to_string(),
                    |p| format!("moved aside to {}", p.display())
                )
            ),
        };
        let counts = match &self.state {
            None => String::new(),
            Some(s) => format!(
                " (snap seq {}, tail {} records{}, engines {}, resting orders {}, arms {}, held \
                 exits {})",
                self.report.snap_sequence.map_or_else(|| "-".to_string(), |n| n.to_string()),
                self.report.tail_records,
                if self.report.skipped_wakers > 0 {
                    format!(" ({} waker records skipped)", self.report.skipped_wakers)
                } else {
                    String::new()
                },
                s.engines.len(),
                s.journal_orders.len(),
                s.order_emulation_arms.len(),
                s.contingencies.iter().filter(|c| c.held_request.is_some()).count(),
            ),
        };
        Some(format!("journal restore: {what} from {}{counts}", dir.display()))
    }

    /// Log [`Self::startup_line`] and every note, once, at the outcome's level: INFO for a full or
    /// fresh restore, WARN for a snap-only or torn one, ERROR for a broken journal. Nothing when
    /// the journal is off.
    pub fn log_startup_line(&self) {
        let Some(line) = self.startup_line() else { return };
        let lines = std::iter::once(&line).chain(self.report.notes.iter());
        match &self.outcome {
            RestoreOutcome::Off => {}
            RestoreOutcome::Full | RestoreOutcome::Fresh { .. } => {
                for l in lines {
                    tracing::info!(target: "vike_core::replay", "{l}");
                }
            }
            RestoreOutcome::SnapOnly { .. } | RestoreOutcome::TornTail { .. } => {
                for l in lines {
                    tracing::warn!(target: "vike_core::replay", "{l}");
                }
            }
            RestoreOutcome::Broken { .. } => {
                for l in lines {
                    tracing::error!(target: "vike_core::replay", "{l}");
                }
            }
        }
    }

    /// Fill `config` with what the journal restores — the ONE writer of the restore fields, called
    /// by the paper and the live mount alike before the core is spawned. Off writes nothing; a
    /// broken journal writes only its notes and the held exits to tell; otherwise the order client id session,
    /// the arm counter, the books and their owners, the engine snapshots, the ownership candidates
    /// and the journal's ledgers (which `assemble_core` applies only where the ownership file has
    /// none), and the late-retry memory ([`Self::recent_order_client_ids`]). The risk limits are never touched.
    /// Setting `order_client_id_switch` with the session is what arms the core's one-time order client id session switch
    /// (`assemble_core`, decision 0270's D1).
    pub fn apply_to(self, config: &mut CoreConfig) {
        let line = self.startup_line();
        let snap_sequence = self.report.snap_sequence;
        config.restore_notes.extend(line);
        config.restore_notes.extend(self.report.notes);
        config.lost_held_exits = self.lost_held_exits;
        config.recent_order_client_ids = self.recent_order_client_ids;
        let Some(s) = self.state else { return };
        config.order_client_id_session = Some(s.order_client_id_session);
        config.order_client_id_switch = true;
        config.arm_sequence = s.arm_sequence;
        config.order_emulation_arms = s.order_emulation_arms;
        config.conditional_owners = s.conditional_owners;
        config.contingencies = s.contingencies;
        config.held_releases = s.held_releases;
        config.emulated = s.emulated;
        config.restored_engines = s.engines;
        config.restored_snap_sequence = snap_sequence;
        config.journal_orders = s.journal_orders;
        config.mount_attribution = s.ledgers;
        config.gateway_cursors = s.gateway_cursors;
        config.closed_trades = s.closed_trades;
        config.closed_tail = s.closed_tail;
    }
}

/// THE BOOT RESTORE: read `journal`'s directory and decide what the next core restores from it
/// (the module doc). `scratch_root` is the parent of the replay core's scratch journal
/// (`<settings>/state/restore-scratch`: a sandboxed daemon writes only its granted paths);
/// `now_ms` stamps the names of a torn copy and a moved-aside directory and is the clock the late-retry
/// memory is pruned at (this crate reads no clock). `Ok(RestoreReport::off())` for `None`; `Err` — a refused start — for a
/// directory that cannot be read or a journal a newer build wrote.
pub fn restore_for_boot(
    journal: Option<&JournalConfig>,
    scratch_root: &Path,
    now_ms: i64,
) -> Result<RestoreReport, String> {
    let Some(jc) = journal else {
        return Ok(RestoreReport::off());
    };
    let dir = jc.dir.as_path();
    if !dir.exists() {
        return Ok(RestoreReport::fresh(dir, "the journal directory does not exist yet"));
    }
    // ⚠ The journal's single-writer lock, held for the whole restore: the copy, the move aside and
    // the prune below CHANGE the directory, and a second daemon started beside a live one must not
    // touch the live one's journal. A held lock is a refused start (the live open would refuse the
    // same directory a moment later); it is released before a move aside (a directory with an open
    // handle inside cannot be renamed on Windows) and when this returns.
    let lock = JournalLock::acquire(dir).map_err(|e| busy(dir, &e))?;
    // The asynchronous journal (decision 0279): after a power cut a working segment may hold less
    // than its durable copy; repair every one BEFORE the read below, under the same lock.
    for note in vike_journal::repair_from_durable_copies(dir).map_err(|e| refusal(dir, &e))? {
        tracing::warn!(target: "vike_core::replay", dir = %dir.display(), "{note}");
    }
    let (records, halt) = Journal::read_all_located(dir).map_err(|e| refusal(dir, &e))?;
    let strategy_files = fold_strategy_files(&records);
    let stamp = format!("{:013}", now_ms.max(0));
    let mut br = match halt {
        Some(h) if h.cause == JournalCorruptCause::ParseFailed || !h.in_tail => {
            broken(dir, &records, &h, &stamp, lock)
        }
        Some(h) => torn(dir, records, &h, scratch_root, &stamp, now_ms),
        None => intact(dir, records, scratch_root, now_ms),
    };
    for (mount_id, file) in &strategy_files {
        br.report.notes.push(format!(
            "journal restore: mount `{mount_id}` last ran strategy file {} (sha256 {})",
            file.path, file.sha256
        ));
    }
    br.strategy_files = strategy_files;
    Ok(br)
}

/// [`RestoreReport::strategy_files`]: the latest `Snapshot`'s own mount rows, then every `StrategyFileMounted` after
/// it, a later one replacing its mount's (all of them when there is no `Snapshot`). A pure fold, read once at boot.
fn fold_strategy_files(records: &[JournalRecord]) -> Vec<(String, JournalStrategyFile)> {
    let mut files: Vec<(String, JournalStrategyFile)> = Vec::new();
    let mut put = |mount_id: &str, file: &JournalStrategyFile| match files
        .iter()
        .position(|(m, _)| m == mount_id)
    {
        Some(i) => files[i].1 = file.clone(),
        None => files.push((mount_id.to_string(), file.clone())),
    };
    for record in &records[latest_snap(records).unwrap_or(0)..] {
        match record {
            JournalRecord::Snapshot { mount_attribution: mount_attribute, .. } => {
                for row in mount_attribute.iter().filter(|r| r.venue.is_none()) {
                    if let Some(file) = &row.strategy_file {
                        put(&row.mount_id, file);
                    }
                }
            }
            JournalRecord::StrategyFileMounted { mount_id, strategy_file, .. } => {
                put(mount_id, strategy_file)
            }
            // every other record: not this lookup's business
            _ => {}
        }
    }
    files
}

/// The refused-start sentence for a journal whose single-writer lock cannot be taken: most often
/// another instance writing it (`AddrInUse`, whose own text names the cure).
fn busy(dir: &Path, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::AddrInUse {
        format!("journal restore: refused, nothing read or changed — {e}")
    } else {
        format!(
            "journal restore: the journal at {} cannot be locked: {e} — the live core could not \
             open it either",
            dir.display()
        )
    }
}

/// The refused-start sentence for a journal that cannot be read at all.
fn refusal(dir: &Path, e: &std::io::Error) -> String {
    let fix = if e.kind() == std::io::ErrorKind::InvalidData {
        "a segment carries a format version this build does not read — most often a journal a \
         NEWER build wrote, after a rollback: move the directory aside before starting this build \
         (a journal moved aside is kept, not deleted; the trade daemon's ops runbook, its \
         rollback section)"
    } else {
        "fix its permissions, or point the `config.journal_dir` row elsewhere — the live core \
         would fail to open this directory too"
    };
    format!("journal restore: the journal at {} cannot be read: {e} — {fix}", dir.display())
}

/// An intact read: restore from the latest `Snapshot`, or Fresh when there is none; prune after.
fn intact(
    dir: &Path,
    records: Vec<JournalRecord>,
    scratch_root: &Path,
    now_ms: i64,
) -> RestoreReport {
    let Some(base_index) = latest_snap(&records) else {
        let why = if records.is_empty() {
            "the journal is empty"
        } else {
            "the journal holds records but no checkpoint to restore from"
        };
        let mut br = RestoreReport::fresh(dir, why);
        br.recent_order_client_ids = boot_memory(None, &tail_facts(&records), now_ms);
        prune(dir, &mut br.report);
        return br;
    };
    let mut br = restore_from(dir, records, base_index, scratch_root, now_ms);
    if br.outcome == RestoreOutcome::Full {
        prune(dir, &mut br.report);
    }
    br
}

/// A torn LAST segment: keep a copy of it, then restore the readable prefix.
fn torn(
    dir: &Path,
    records: Vec<JournalRecord>,
    halt: &JournalReadHalt,
    scratch_root: &Path,
    stamp: &str,
    now_ms: i64,
) -> RestoreReport {
    let kept = Journal::preserve_torn_segment(dir, halt, stamp, TORN_COPIES_KEPT);
    let evidence = match &kept {
        Ok(p) => p.display().to_string(),
        Err(e) => format!("NOT kept ({e}) — the torn bytes stay only in the segment itself"),
    };
    let mut br = match latest_snap(&records) {
        Some(i) => restore_from(dir, records, i, scratch_root, now_ms),
        None => RestoreReport {
            recent_order_client_ids: boot_memory(None, &tail_facts(&records), now_ms),
            ..RestoreReport::fresh(dir, "no readable checkpoint")
        },
    };
    let refolded = br.outcome == RestoreOutcome::Full;
    let to = br.report.snap_sequence.map_or_else(|| "nothing".to_string(), |n| format!("seq {n}"));
    br.report.notes.insert(
        0,
        format!(
            "JOURNAL TAIL TORN: restored to {to}; nothing at or after offset {} of {} could be \
             read ({}); evidence {evidence}; the venue's first reconcile pass is the truth",
            halt.offset,
            halt.segment.display(),
            halt.cause.reading(),
        ),
    );
    if let RestoreOutcome::SnapOnly { why } = &br.outcome {
        br.report.notes.push(format!("journal restore: snap-only because {why}"));
    }
    br.outcome = RestoreOutcome::TornTail { cause: halt.cause, kept_evidence: kept.ok(), refolded };
    br
}

/// Corruption before the last written records (or a frame another build wrote): restore nothing,
/// name what is lost, move the directory aside.
fn broken(
    dir: &Path,
    records: &[JournalRecord],
    halt: &JournalReadHalt,
    stamp: &str,
    lock: JournalLock,
) -> RestoreReport {
    let mut report = Report::default();
    let lost = latest_snap(records).map(|i| lost_protection(records, i));
    drop(lock);
    let moved = Journal::set_aside(dir, stamp);
    report.notes.push(format!(
        "JOURNAL BROKEN: {} at offset {} of {}{} — nothing was restored from it; {}; the ownership \
         file still says who owns each order, and the venue's first reconcile pass is the truth",
        halt.cause.reading(),
        halt.offset,
        halt.segment.display(),
        if halt.in_tail { "" } else { ", with records written after it that no read can reach" },
        match &moved {
            Ok(to) => format!(
                "moved aside to {}, and a fresh journal starts at {}",
                to.display(),
                dir.display()
            ),
            Err(e) => format!(
                "it could NOT be moved aside ({e}): every later read stops at the same frame — \
                 move {} by hand",
                dir.display()
            ),
        },
    ));
    let lost_held_exits = match lost {
        None => {
            report.notes.push(
                "PROTECTION LOST: the journal holds no readable checkpoint, so the conditional \
                 arms and held exits it guarded cannot even be counted — check every open \
                 position's protection by hand"
                    .to_string(),
            );
            Vec::new()
        }
        Some((arms, held)) => {
            if arms + held.len() > 0 {
                report.notes.push(format!(
                    "PROTECTION LOST: {arms} conditional arm(s) and {} held exit(s) were not \
                     restored — the positions they guarded are unprotected{}",
                    held.len(),
                    if held.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; the mount of each held exit hears a terminal reject: {}",
                            held.iter()
                                .map(|h| h.order_client_id.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ));
            }
            held
        }
    };
    // The orders the core held for their release were never sent: lost with the journal, said by order client id.
    if let Some(i) = latest_snap(records)
        && let JournalRecord::Snapshot { held_releases, .. } = &records[i]
        && !held_releases.is_empty()
    {
        report.notes.push(format!(
            "HELD ORDERS LOST: {} order(s) held for their release were not restored and will not be \
             sent: {}",
            held_releases.len(),
            held_releases.iter().map(|h| h.request.order_client_id.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    RestoreReport {
        outcome: RestoreOutcome::Broken { cause: halt.cause, moved_to: moved.ok() },
        dir: Some(dir.to_path_buf()),
        state: None,
        lost_held_exits,
        recent_order_client_ids: Vec::new(),
        strategy_files: Vec::new(),
        report,
    }
}

/// What the last readable `Snapshot` (at `index`) and the tail after it held that a broken journal loses:
/// the count of armed conditionals and every held exit.
fn lost_protection(records: &[JournalRecord], index: usize) -> (usize, Vec<RestoreLostHeldExit>) {
    let JournalRecord::Snapshot { order_emulation_arms: conditionals, contingencies, .. } =
        &records[index]
    else {
        return (0, Vec::new());
    };
    let tail = &records[index + 1..];
    let (books, _) = fold_conditionals(conditionals.clone(), tail);
    let facts = tail_facts(tail);
    let held = live_contingencies(contingencies, &facts)
        .into_iter()
        .filter_map(|c| {
            let req = c.held_request?;
            Some(RestoreLostHeldExit {
                order_client_id: c.order_client_id,
                venue: req.venue,
                symbol: req.symbol,
            })
        })
        .collect();
    (books.len(), held)
}

/// `contingencies` minus every leg the tail ended or whose parent the tail ended (a filled parent
/// RELEASED its held child to the venue; a dead parent cascaded it away).
fn live_contingencies(
    contingencies: &[JournalSnapshotContingency],
    facts: &JournalReplayTailFacts,
) -> Vec<JournalSnapshotContingency> {
    contingencies
        .iter()
        .filter(|c| {
            !facts.ended(&c.order_client_id) && !c.parent.as_deref().is_some_and(|p| facts.ended(p))
        })
        .cloned()
        .collect()
}

/// The latest `Snapshot`'s emulated parents (`Snapshot.emulated`, decision 0275) minus every one its tail ENDED
/// (a journaled `Command::Emulated` end step): the restore when the refold cannot run. Each kept parent's
/// law awaits its child's confirmation, so a child the tail placed is adopted by the reconcile pass, never
/// placed twice. A pure fold of the records.
pub(super) fn live_emulated(
    snap: &[vike_journal::JournalSnapshotOrderEmulation],
    tail: &[JournalRecord],
) -> Vec<vike_journal::JournalSnapshotOrderEmulation> {
    use vike_execution::{Command, Ingest};
    let ended: HashSet<&str> = tail
        .iter()
        .filter_map(|r| match r {
            JournalRecord::Command {
                message: Ingest::Command(Command::Emulated(step)), ..
            } if matches!(step.verb, vike_execution::EmulatedVerb::End(_)) => {
                Some(step.parent.as_str())
            }
            _ => None,
        })
        .collect();
    snap.iter().filter(|e| !ended.contains(e.request.order_client_id.as_str())).cloned().collect()
}

/// The latest `Snapshot`'s held orders (`held_releases`) minus every one its tail released (a journaled
/// `OrderOn` of its order client id, `held_release.rs`; a plain submit reusing the order client id was refused, so it does not
/// count), cancelled by order client id, ended by a venue event, or ended by a mass-cancel or market exit of ITS
/// account: what a SNAP-ONLY restore holds again. An order names one account, so a venue-scoped cancel
/// reaches the held orders of the account it was lowered onto (named by the payload, else by the record's
/// route key, else the venue's default account), never the other accounts of that venue; only a cancel
/// naming no venue reaches every one. An order the tail held is lost with the tail like every tail mint
/// (its owning mount hears the reconcile pass's verdict on it). A pure fold of the records.
pub(super) fn live_held_releases(
    held: &[OrderHeldRelease],
    tail: &[JournalRecord],
) -> Vec<OrderHeldRelease> {
    use vike_execution::{Command, Ingest, OrderIntent};
    use vike_model::accounts::account_keys::route_key_of;
    let facts = tail_facts(tail);
    let mut gone: HashSet<&str> = HashSet::new();
    // `(route key, symbol)` scopes a tail mass-cancel or market exit emptied (`None` = any)
    let mut scopes: Vec<(Option<String>, Option<&str>)> = Vec::new();
    for rec in tail {
        let (intent, route_key, released) = match rec {
            JournalRecord::Command { message: Ingest::Command(Command::OrderOn(o)), .. } => {
                (&o.intent, Some(o.route_key.as_str()), true)
            }
            JournalRecord::Command { message: Ingest::Command(Command::Order(i)), .. } => {
                (i, None, false)
            }
            JournalRecord::StrategySubmit { intent, route_key, .. } => {
                (intent, route_key.as_deref(), false)
            }
            // every other record: not this fold's business (a single-purpose lookup)
            _ => continue,
        };
        // The account a venue-scoped cancel was lowered onto.
        let account_of =
            |venue: &str, label: &Option<vike_model::accounts::account_keys::AccountLabel>| {
                label
                    .as_ref()
                    .map(|l| route_key_of(venue, l))
                    .or_else(|| route_key.map(str::to_string))
                    .unwrap_or_else(|| venue.to_string())
            };
        match intent {
            OrderIntent::Submit(req) if released => {
                gone.insert(req.order_client_id.as_str());
            }
            OrderIntent::Cancel(order_client_id) => {
                gone.insert(order_client_id.as_str());
            }
            OrderIntent::CancelBatch(order_client_ids) => {
                gone.extend(order_client_ids.iter().map(String::as_str))
            }
            OrderIntent::MassCancel { venue, symbol, account } => {
                scopes.push((venue.as_deref().map(|v| account_of(v, account)), symbol.as_deref()));
            }
            OrderIntent::MarketExit { venue, account } => {
                scopes.push((venue.as_deref().map(|v| account_of(v, account)), None));
            }
            // every other intent: not this fold's business
            _ => {}
        }
    }
    held.iter()
        .filter(|h| {
            let r = &h.request;
            let c = r.order_client_id.as_str();
            let in_scope = scopes.iter().any(|(k, s)| {
                k.as_deref().is_none_or(|k| k == h.route_key) && s.is_none_or(|s| s == r.symbol)
            });
            !gone.contains(c) && !facts.ended(c) && !in_scope
        })
        .cloned()
        .collect()
}

/// The index of the latest `Snapshot` (a single-variant lookup).
fn latest_snap(records: &[JournalRecord]) -> Option<usize> {
    records.iter().rposition(|r| matches!(r, JournalRecord::Snapshot { .. }))
}

/// Restore from the `Snapshot` at `base_index`: the refold first, the pure folds when it cannot run.
fn restore_from(
    dir: &Path,
    records: Vec<JournalRecord>,
    base_index: usize,
    scratch_root: &Path,
    now_ms: i64,
) -> RestoreReport {
    // Everything the pure folds need is read BEFORE the refold, which consumes the records.
    let (minted_by, arm_owner) = fold_order_client_id_origins(&records);
    let owners: HashMap<String, String> = minted_by.into_iter().collect();
    let JournalRecord::Snapshot {
        sequence: seq,
        engines,
        order_client_id_session,
        order_client_id_sequence,
        arm_sequence: arm_sequence_value,
        order_emulation_arms: conditionals,
        contingencies,
        mount_attribution: mount_attribute,
        gateway_cursors,
        held_releases,
        emulated,
        closed_trades,
        ..
    } = &records[base_index]
    else {
        unreachable!("`latest_snap` indexed a Snapshot")
    };
    let snap_held = live_held_releases(held_releases, &records[base_index + 1..]);
    let snap_emulated = live_emulated(emulated, &records[base_index + 1..]);
    let tail = &records[base_index + 1..];
    let closed_trades = closed_trades.clone();
    let tail_closed = closed_tail(tail, engines, &owners);
    let mut archive_notes = Vec::new();
    let closed_trades = with_archived_rows(dir, closed_trades, &mut archive_notes);
    let (books, tail_armed) = fold_conditionals(conditionals.clone(), tail);
    let facts = tail_facts(tail);
    let recent_order_client_ids = boot_memory(Some(&records[base_index]), &facts, now_ms);
    // Two answers, because only the refold folds the tail's events into an engine: the cursors the
    // tail advances to (Full) and the `Snapshot`'s own (SnapOnly: its engines are the `Snapshot`'s, so a
    // cursor past it would make the gateway skip events no engine ever saw).
    let advanced_cursors = fold_gateway_cursors(gateway_cursors, engines, tail);
    let snap_cursors = gateway_cursors.clone();
    let arm_seed = arm_sequence_value.unwrap_or_else(|| {
        records.iter().map(vike_journal::record_sequence).max().map_or(0, |m| m + 1)
    });
    let mut report = Report {
        snap_sequence: Some(*seq),
        tail_records: tail.len(),
        notes: archive_notes,
        ..Report::default()
    };
    let snap_engines = engines.clone();
    let snap_session = (order_client_id_session.clone(), *order_client_id_sequence);
    let snap_contingencies = contingencies.clone();
    let ledgers = mount_attribute.clone();

    let refold = catch_unwind(AssertUnwindSafe(|| {
        refold_records(
            records,
            JournalReplayBaseSelection::Latest,
            &JournalReplayRefoldOptions::restore(scratch_root),
        )
    }));
    let mut held_after = snap_held;
    let mut emulated_after = snap_emulated;
    let (
        outcome,
        engines,
        order_client_id_session,
        arm_sequence_value,
        contingencies,
        orders,
        cursors,
    ) = match refold {
        Ok(Ok(r)) => {
            held_after = r.held_releases;
            emulated_after = r.emulated;
            report.skipped_wakers = r.skipped_wakers;
            let orders = resting_orders(&r.engines, None, &r.contingencies, &owners);
            // The replay core's own mint count, floored at the pure fold's (one `MintedSubmit` per
            // tail mint, the `SnapOnly` arm's bound): a record the replay core could not place mints
            // nothing there, and an undercount would re-mint an order client id the dead core had spent. Equal
            // whenever the refold reproduced the tail, so it moves nothing then.
            let order_client_id_sequence =
                r.order_client_id_sequence.max(snap_session.1 + facts.minted.len() as u64);
            (
                RestoreOutcome::Full,
                r.engines,
                (r.order_client_id_session, order_client_id_sequence),
                r.arm_sequence,
                r.contingencies,
                orders,
                advanced_cursors,
            )
        }
        failed => {
            let why = match failed {
                Ok(Err(e)) => e.to_string(),
                Err(payload) => {
                    format!("the replay core panicked: {}", crate::runtime::panic_text(payload))
                }
                Ok(Ok(_)) => unreachable!("matched above"),
            };
            // The pure folds. The order client id counter is the `Snapshot`'s plus one per tail mint (every mint
            // journals exactly one `MintedSubmit`), and the arm counter the `Snapshot`'s plus one per
            // tail arm: both may overshoot, which only wastes ids; neither may undercount.
            let live = live_contingencies(&snap_contingencies, &facts);
            if live.len() < snap_contingencies.len() {
                report.notes.push(format!(
                    "journal restore: {} contingency leg(s) of the checkpoint ended in its tail \
                     (filled, released or canceled) and were not re-armed",
                    snap_contingencies.len() - live.len()
                ));
            }
            let orders = resting_orders(&snap_engines, Some(&facts), &live, &owners);
            let session = (snap_session.0.clone(), snap_session.1 + facts.minted.len() as u64);
            (
                RestoreOutcome::SnapOnly { why },
                snap_engines,
                session,
                arm_seed + tail_armed,
                live,
                orders,
                snap_cursors,
            )
        }
    };
    let conditional_owners = books
        .iter()
        .filter_map(|c| arm_owner.get(&c.arm_id).map(|m| (c.arm_id.clone(), m.clone())))
        .collect();
    // The tail's fills reach the ledgers only where they reached the engines (the refold folded the tail).
    let closed_tail = if outcome == RestoreOutcome::Full { tail_closed } else { Vec::new() };
    RestoreReport {
        outcome,
        dir: Some(dir.to_path_buf()),
        state: Some(BootState {
            order_client_id_session,
            arm_sequence: arm_sequence_value,
            order_emulation_arms: books,
            conditional_owners,
            contingencies,
            held_releases: held_after,
            emulated: emulated_after,
            engines,
            closed_tail,
            journal_orders: orders,
            ledgers,
            gateway_cursors: cursors,
            closed_trades,
        }),
        lost_held_exits: Vec::new(),
        recent_order_client_ids,
        strategy_files: Vec::new(),
        report,
    }
}

/// **The `Snapshot`'s closed-trade ledgers with the ARCHIVE's rows put in front of their own**
/// (`crates/vike-core/src/runtime/closed_trades.rs`'s "A restart"): the archive beside the journal in `dir` is read
/// ONCE, here (`vike_journal::read_closed_archive`: a torn or duplicate frame dropped by its check and its number),
/// and each ledger that says the archive holds its rows `0..archived` gets them, then its own. A row the archive
/// lost (bytes gone from the disk under it, never a crash mid-append, which only loses rows a `Snapshot` still carries)
/// is an ERROR naming the mount: the list goes on without the rows from the first one missing to `archived`, and
/// the `Snapshot`'s own move up to the first number missing, under which the mount writes them to the archive again.
/// Not read at all while no ledger says the archive holds a row.
fn with_archived_rows(
    dir: &Path,
    mut ledgers: Vec<vike_journal::JournalSnapshotClosedTrades>,
    notes: &mut Vec<String>,
) -> Vec<vike_journal::JournalSnapshotClosedTrades> {
    if ledgers.iter().all(|c| c.archived == 0) {
        return ledgers;
    }
    let read = vike_journal::read_closed_archive(dir).unwrap_or_else(|e| {
        tracing::error!(
            target: "vike_core::replay",
            dir = %dir.display(),
            error = %e,
            "journal restore: the closed-trade archive cannot be read"
        );
        notes.push(format!(
            "journal restore: the closed-trade archive of {} cannot be read ({e}): the closed trades it held are not \
             restored",
            dir.display()
        ));
        vike_journal::TradeClosedArchiveRead::default()
    });
    if read.torn > 0 || read.duplicates > 0 {
        notes.push(format!(
            "journal restore: the closed-trade archive held {} torn stretch(es) (a crash mid-append) and {} row(s) \
             written again; both were dropped",
            read.torn, read.duplicates
        ));
    }
    for c in &mut ledgers {
        let by_n = read.trades.get(&c.mount_id);
        let mut rows = Vec::with_capacity(c.rows.len() + usize::try_from(c.archived).unwrap_or(0));
        rows.extend((0..c.archived).map_while(|n| by_n.and_then(|m| m.get(&n)).cloned()));
        let found = rows.len() as u64;
        if found < c.archived {
            tracing::error!(
                target: "vike_core::replay",
                mount_id = %c.mount_id,
                found,
                archived = c.archived,
                "journal restore: the closed-trade archive lost rows its Snapshot says it holds"
            );
            notes.push(format!(
                "journal restore: the closed-trade archive holds {found} of the {} closed trades of `{}` it should: \
                 its list resumes without the rest",
                c.archived, c.mount_id
            ));
        }
        rows.append(&mut c.rows);
        (c.rows, c.archived) = (rows, found);
    }
    ledgers
}

/// [`RestoreReport::recent_order_client_ids`]: the union of the `base` `Snapshot`'s carried memory and registries and the
/// ids its tail used, pruned at `now_ms` (the restore's own clock read). `base` is `None` for a journal
/// with no checkpoint, whose records are all tail. A pure fold of the records.
fn boot_memory(
    base: Option<&JournalRecord>,
    tail: &JournalReplayTailFacts,
    now_ms: i64,
) -> Vec<(String, i64)> {
    let mut carried: Vec<(String, i64)> = Vec::new();
    if let Some(JournalRecord::Snapshot {
        now_ms: snap_ms, engines, recent_order_client_ids, ..
    }) = base
    {
        carried.extend(recent_order_client_ids.iter().cloned());
        for e in engines {
            carried.extend(e.registry.iter().map(|(order_client_id, mo)| {
                (order_client_id.clone(), mo.created_ms.unwrap_or(*snap_ms))
            }));
        }
    }
    carried.extend(tail.used.iter().cloned());
    crate::runtime::recent_order_client_ids::prune_carried(&mut carried, now_ms);
    carried
}

/// Drop the segments the latest `Snapshot` superseded (a Full or Fresh restore only, before the live
/// core opens the directory); a failure is a note, never a refused start.
fn prune(dir: &Path, report: &mut Report) {
    if let Err(e) = Journal::prune_before_latest_snap(dir) {
        report.notes.push(format!(
            "journal restore: the superseded segments of {} could not be pruned: {e}",
            dir.display()
        ));
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use crate::runtime::recent_order_client_ids::RECENT_ORDER_CLIENT_ID_CAP;

    /// The boot memory is capped, newest kept, and one id used twice is carried once.
    #[test]
    fn the_boot_memory_is_capped_newest_first_and_one_pair_per_id() {
        let now = 1_000_000_000;
        let mut tail = tail_facts(&[]);
        tail.used = (0..RECENT_ORDER_CLIENT_ID_CAP as i64 + 2)
            .map(|i| (format!("t{i}"), now - i))
            .collect();
        tail.used.push(("t0".into(), now - 5));
        let memory = boot_memory(None, &tail, now);
        assert_eq!(memory.len(), RECENT_ORDER_CLIENT_ID_CAP);
        assert!(memory.iter().any(|(id, at)| id == "t0" && *at == now), "the newest of t0 is kept");
        assert!(!memory.iter().any(|(id, _)| id == "t65537"), "the oldest goes first");
    }
}

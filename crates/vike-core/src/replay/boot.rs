//! The BOOT RESTORE (`docs/decisions/0127-the-live-daemon-restores-its-core-from-the-journal-at-boot.md`):
//! what a restarted daemon takes back from its write-ahead journal before either mount assembles a
//! core, and how deep the journal let it go.
//!
//! # The one call
//!
//! [`restore_for_boot`] runs ONCE per process, in the composition root (`vike-tradehub`'s `run`),
//! after the journal is resolved and BEFORE the live core opens it — so before
//! `vike_journal::CommandJournal::open` resumes the last segment over a torn frame, and while no
//! writer holds the directory. It reads the journal without the lock (only the write path locks),
//! never touches the fold, and hands back a [`BootRestore`]; the root logs its line
//! ([`BootRestore::log_startup_line`]) and the mount that builds the core fills its `CoreConfig`
//! with [`BootRestore::apply_to`] — one function for the paper and the live mount, so they cannot
//! disagree about what a journal restores.
//!
//! # How deep (the [`RestoreOutcome`])
//!
//! - **Full** — the latest `Snap` plus its crash tail, refolded through a replay core
//!   (`refold_records` under `RefoldOpts::restore`: several engines, waker records skipped,
//!   scratch under the caller's root: `<settings>/state/restore-scratch` for `vike-tradehub`).
//! - **SnapOnly** — the refold could not run (a labelled or doubled venue engine, a panic, a scratch
//!   failure): the latest `Snap` and the PURE folds of its tail (books, owners, minted and ended
//!   orders, the id counters as overshooting bounds). The account state is the `Snap`'s.
//! - **TornTail** — the read halted at a torn frame of the LAST written segment (an ordinary
//!   crash): that segment is COPIED to `<dir>/torn/` first (the live open overwrites the torn bytes),
//!   the newest [`TORN_COPIES_KEPT`] copies are kept, and the readable prefix restores as above.
//! - **Broken** — the read halted with records written AFTER the corrupt frame, or at a frame
//!   another build wrote (`ParseFailed`): NOTHING is restored from it, the directory is moved aside
//!   to `<dir>.broken-<stamp>` and the live core starts a fresh journal (otherwise every later read
//!   stops at the same frame for good). The last readable `Snap` is still read, to name what is
//!   lost: one PROTECTION LOST line counts the conditional arms and held exits, and each lost held
//!   exit's mount hears a terminal `OrderRejected` ([`RESTORE_LOST_REASON`]) at boot.
//! - **Fresh** — no directory, no record, or no `Snap`.
//!
//! An unreadable directory or a journal written by a NEWER build is not an outcome: it is an `Err`
//! the root turns into a refused start — the live open would fail on the same directory a moment
//! later, and a core that thinks it is journaling but is not is worse than one that does not start.
//! After a Full or Fresh restore, segments the latest `Snap` superseded are pruned (never after a
//! snap-only, torn or broken read: those keep their evidence). The whole restore holds the journal's
//! single-writer lock (`vike_journal::lock::JournalLock`), released only for a move aside, so a second
//! daemon started beside a live one is refused and changes nothing.
//!
//! # Precedence (applied by `assemble_core`)
//!
//! Settings and the run profile set every LIMIT (the journal's `RiskLimits` are never applied). The
//! journal is the one source of the coid session and counters, the conditional and contingency
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
//! per-account ring. The restore hands those positions back ([`BootRestore::gateway_cursors`], and
//! `CoreConfig::gateway_cursors` so the next `Snap` keeps them). They are the resume point of the
//! trade session, never state the restore applies, and a cursor may only be as far along as the
//! engine state it comes back beside:
//!
//! - **Full** — per account, the latest `Snap`'s cursor advanced by every `GatewayEvent` of its
//!   tail, because the refold folded those events into the engines like any other venue event. A
//!   resume asks for the event AFTER the cursor and nothing is folded twice. Only an account the
//!   `Snap` holds an ENGINE for advances: the refold builds its replay core from the `Snap`'s engines
//!   alone, so the tail events of an account armed after the checkpoint reached no engine and keep the
//!   `Snap`'s cursor (or none: the session opens fresh and a reconcile recovers what it can).
//! - **SnapOnly** (and a torn tail that could not refold) — the latest `Snap`'s cursors UNADVANCED.
//!   The pure folds read the tail's events only for their facts (minted and ended orders); no tail
//!   event reaches any engine, so a cursor past the `Snap` would tell the gateway the tail's fills
//!   and funding were already applied and it would never send them again. An account the `Snap` has
//!   no cursor for opens its session with no cursor (a reconcile then recovers what it can).
//!
//! What a replayed event can still do to a book the
//! restore and a reconcile pass already settled is pinned by
//! `crates/vike-core/src/runtime/tests/boot_restore.rs`'s `a_fill_the_pass_booked_is_not_booked_again_by_the_ring_replay`
//! and its twin.
//!
//! # What it does not restore
//!
//! The per-mount budget latch, the PnL-curve peak and the held reconcile alerts are in no `Snap`;
//! a trailing stop's extreme is the last `Snap`'s; the stuck-order and dead-man sweeps of a skipped
//! waker tail are not reproduced (such an order can come back as a candidate, and the pass decides
//! it). No journaled order enters an engine registry at boot.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use vike_exec::{EngineSnapshot, GatewayCursor};
use vike_journal::lock::JournalLock;
use vike_journal::{
    CommandJournal, CorruptCause, ReadHalt, SnapConditional, SnapContingency, SnapMountAttr,
};

use super::fold::{TailFacts, fold_coid_origins, fold_gateway_cursors, resting_orders, tail_facts};
use super::refold::{BaseSelect, RefoldOpts, refold_records};
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
/// `crate::CoreConfig::journal_orders`. Ownership only supplements the ownership file: a coid the
/// file knows keeps the file's owner. Never a registry entry: the first reconcile pass judges it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JournalOrder {
    pub coid: String,
    /// The minting mount's id (`crate::strategy_state::mount_id_with`).
    pub mount_id: String,
    pub venue: Option<String>,
    pub symbol: Option<String>,
    /// The account LABEL TEXT of the engine the order is on (`None` for the default account).
    pub account: Option<String>,
    /// The order's limit price, which the reconcile adoption puts back on the registry entry it
    /// rebuilds from a venue report that carries none (decision 0120's verdict 8, amended).
    pub price: Option<f64>,
}

/// A HELD exit (an OTO child kept off the venue until its parent fills) that a broken journal took
/// with it: its mount is told with a terminal reject at boot.
#[derive(Debug, Clone, PartialEq)]
pub struct LostHeldExit {
    pub coid: String,
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
        cause: CorruptCause,
        kept_evidence: Option<PathBuf>,
        refolded: bool,
    },
    Broken {
        cause: CorruptCause,
        moved_to: Option<PathBuf>,
    },
}

/// What the journal restores, as `CoreConfig` will hold it.
#[derive(Debug, Clone)]
struct BootState {
    coid_session: (String, u64),
    arm_seq: u64,
    conditionals: Vec<SnapConditional>,
    /// `(arm_id, mount_id)` of every restored arm a mount armed.
    conditional_owners: Vec<(String, String)>,
    contingencies: Vec<SnapContingency>,
    engines: Vec<EngineSnapshot>,
    journal_orders: Vec<JournalOrder>,
    ledgers: Vec<SnapMountAttr>,
    /// Where each gateway ring stood in the engine state this restore hands back (journal v18): the
    /// latest `Snap`'s cursors, advanced over its tail's `GatewayEvent` records only when the refold
    /// folded that tail (Full), one per route key.
    gateway_cursors: Vec<(String, GatewayCursor)>,
}

/// The counts the startup line reports.
#[derive(Debug, Clone, Default)]
struct Report {
    snap_seq: Option<u64>,
    tail_records: usize,
    skipped_wakers: u64,
    /// Ring notes (and log lines) beyond the startup line, in order.
    notes: Vec<String>,
}

/// The result of [`restore_for_boot`]: the outcome, what it restores, and what it says.
#[derive(Debug, Clone)]
pub struct BootRestore {
    outcome: RestoreOutcome,
    dir: Option<PathBuf>,
    state: Option<BootState>,
    lost_held_exits: Vec<LostHeldExit>,
    report: Report,
}

impl BootRestore {
    /// No journal: [`RestoreOutcome::Off`], nothing to apply and no line.
    pub fn off() -> Self {
        BootRestore {
            outcome: RestoreOutcome::Off,
            dir: None,
            state: None,
            lost_held_exits: Vec::new(),
            report: Report::default(),
        }
    }

    fn fresh(dir: &Path, why: &str) -> Self {
        BootRestore {
            outcome: RestoreOutcome::Fresh { why: why.to_string() },
            dir: Some(dir.to_path_buf()),
            ..Self::off()
        }
    }

    /// How deep the restore went.
    pub fn outcome(&self) -> &RestoreOutcome {
        &self.outcome
    }

    /// The restored coid session and counter, `None` when nothing was restored.
    pub fn coid_session(&self) -> Option<&(String, u64)> {
        self.state.as_ref().map(|s| &s.coid_session)
    }

    /// The restored arm-id counter (0 when nothing was restored).
    pub fn arm_seq(&self) -> u64 {
        self.state.as_ref().map_or(0, |s| s.arm_seq)
    }

    /// The engine snapshots whose account state the core puts back by route key.
    pub fn restored_engines(&self) -> &[EngineSnapshot] {
        self.state.as_ref().map(|s| s.engines.as_slice()).unwrap_or(&[])
    }

    /// The resting-order ownership candidates.
    pub fn journal_orders(&self) -> &[JournalOrder] {
        self.state.as_ref().map(|s| s.journal_orders.as_slice()).unwrap_or(&[])
    }

    /// The restored conditional books, in fire order.
    pub fn conditionals(&self) -> &[SnapConditional] {
        self.state.as_ref().map(|s| s.conditionals.as_slice()).unwrap_or(&[])
    }

    /// The restored contingency book.
    pub fn contingencies(&self) -> &[SnapContingency] {
        self.state.as_ref().map(|s| s.contingencies.as_slice()).unwrap_or(&[])
    }

    /// Where each gateway's per-account event ring stood in the engine state this restore hands
    /// back (journal v18), one `(route key, cursor)` per account: the resume point a trade session
    /// presents so the gateway replays what the tradehub missed while it was down. After a Full
    /// restore, per route key the cursor of the LAST `GatewayEvent` record, else the latest `Snap`'s;
    /// after a snap-only one the latest `Snap`'s, never advanced (the tail's events were not folded
    /// into any engine). Empty when nothing was restored (no journal, a fresh or broken one) or the
    /// journal never held a gateway event, and an account absent from it must open its session with
    /// NO cursor (a reconcile then recovers what it missed).
    pub fn gateway_cursors(&self) -> &[(String, GatewayCursor)] {
        self.state.as_ref().map(|s| s.gateway_cursors.as_slice()).unwrap_or(&[])
    }

    /// The held exits a broken journal lost (empty unless [`RestoreOutcome::Broken`]).
    pub fn lost_held_exits(&self) -> &[LostHeldExit] {
        &self.lost_held_exits
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
                self.report.snap_seq.map_or_else(|| "-".to_string(), |n| n.to_string()),
                self.report.tail_records,
                if self.report.skipped_wakers > 0 {
                    format!(" ({} waker records skipped)", self.report.skipped_wakers)
                } else {
                    String::new()
                },
                s.engines.len(),
                s.journal_orders.len(),
                s.conditionals.len(),
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
    /// broken journal writes only its notes and the held exits to tell; otherwise the coid session,
    /// the arm counter, the books and their owners, the engine snapshots, the ownership candidates
    /// and the journal's ledgers (which `assemble_core` applies only where the ownership file has
    /// none). The risk limits are never touched.
    pub fn apply_to(self, config: &mut CoreConfig) {
        let line = self.startup_line();
        config.restore_notes.extend(line);
        config.restore_notes.extend(self.report.notes);
        config.lost_held_exits = self.lost_held_exits;
        let Some(s) = self.state else { return };
        config.coid_session = Some(s.coid_session);
        config.arm_seq = s.arm_seq;
        config.conditionals = s.conditionals;
        config.conditional_owners = s.conditional_owners;
        config.contingencies = s.contingencies;
        config.restored_engines = s.engines;
        config.journal_orders = s.journal_orders;
        config.mount_attr = s.ledgers;
        config.gateway_cursors = s.gateway_cursors;
    }
}

/// THE BOOT RESTORE: read `journal`'s directory and decide what the next core restores from it
/// (the module doc). `scratch_root` is the parent of the replay core's scratch journal
/// (`<settings>/state/restore-scratch`: a sandboxed daemon writes only its granted paths);
/// `now_ms` stamps the names of a torn copy and a moved-aside directory (this
/// crate reads no clock). `Ok(BootRestore::off())` for `None`; `Err` — a refused start — for a
/// directory that cannot be read or a journal a newer build wrote.
pub fn restore_for_boot(
    journal: Option<&JournalConfig>,
    scratch_root: &Path,
    now_ms: i64,
) -> Result<BootRestore, String> {
    let Some(jc) = journal else {
        return Ok(BootRestore::off());
    };
    let dir = jc.dir.as_path();
    if !dir.exists() {
        return Ok(BootRestore::fresh(dir, "the journal directory does not exist yet"));
    }
    // ⚠ The journal's single-writer lock, held for the whole restore: the copy, the move aside and
    // the prune below CHANGE the directory, and a second daemon started beside a live one must not
    // touch the live one's journal. A held lock is a refused start (the live open would refuse the
    // same directory a moment later); it is released before a move aside (a directory with an open
    // handle inside cannot be renamed on Windows) and when this returns.
    let lock = JournalLock::acquire(dir).map_err(|e| busy(dir, &e))?;
    let (records, halt) = CommandJournal::read_all_located(dir).map_err(|e| refusal(dir, &e))?;
    let stamp = format!("{:013}", now_ms.max(0));
    Ok(match halt {
        Some(h) if h.cause == CorruptCause::ParseFailed || !h.in_tail => {
            broken(dir, &records, &h, &stamp, lock)
        }
        Some(h) => torn(dir, records, &h, scratch_root, &stamp),
        None => intact(dir, records, scratch_root),
    })
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

/// An intact read: restore from the latest `Snap`, or Fresh when there is none; prune after.
fn intact(dir: &Path, records: Vec<JournalRecord>, scratch_root: &Path) -> BootRestore {
    let Some(base_idx) = latest_snap(&records) else {
        let why = if records.is_empty() {
            "the journal is empty"
        } else {
            "the journal holds records but no checkpoint to restore from"
        };
        let mut br = BootRestore::fresh(dir, why);
        prune(dir, &mut br.report);
        return br;
    };
    let mut br = restore_from(dir, records, base_idx, scratch_root);
    if br.outcome == RestoreOutcome::Full {
        prune(dir, &mut br.report);
    }
    br
}

/// A torn LAST segment: keep a copy of it, then restore the readable prefix.
fn torn(
    dir: &Path,
    records: Vec<JournalRecord>,
    halt: &ReadHalt,
    scratch_root: &Path,
    stamp: &str,
) -> BootRestore {
    let kept = CommandJournal::preserve_torn_segment(dir, halt, stamp, TORN_COPIES_KEPT);
    let evidence = match &kept {
        Ok(p) => p.display().to_string(),
        Err(e) => format!("NOT kept ({e}) — the first append overwrites the torn bytes"),
    };
    let restored = latest_snap(&records).map(|i| restore_from(dir, records, i, scratch_root));
    let refolded = restored.as_ref().is_some_and(|br| br.outcome == RestoreOutcome::Full);
    let mut br = restored.unwrap_or_else(|| BootRestore::fresh(dir, "no readable checkpoint"));
    let to = br.report.snap_seq.map_or_else(|| "nothing".to_string(), |n| format!("seq {n}"));
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
    halt: &ReadHalt,
    stamp: &str,
    lock: JournalLock,
) -> BootRestore {
    let mut report = Report::default();
    let lost = latest_snap(records).map(|i| lost_protection(records, i));
    drop(lock);
    let moved = CommandJournal::set_aside(dir, stamp);
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
                            held.iter().map(|h| h.coid.as_str()).collect::<Vec<_>>().join(", ")
                        )
                    }
                ));
            }
            held
        }
    };
    BootRestore {
        outcome: RestoreOutcome::Broken { cause: halt.cause, moved_to: moved.ok() },
        dir: Some(dir.to_path_buf()),
        state: None,
        lost_held_exits,
        report,
    }
}

/// What the last readable `Snap` (at `idx`) and the tail after it held that a broken journal loses:
/// the count of armed conditionals and every held exit.
fn lost_protection(records: &[JournalRecord], idx: usize) -> (usize, Vec<LostHeldExit>) {
    let JournalRecord::Snap { conditionals, contingencies, .. } = &records[idx] else {
        return (0, Vec::new());
    };
    let tail = &records[idx + 1..];
    let (books, _) = fold_conditionals(conditionals.clone(), tail);
    let facts = tail_facts(tail);
    let held = live_contingencies(contingencies, &facts)
        .into_iter()
        .filter_map(|c| {
            let req = c.held_request?;
            Some(LostHeldExit { coid: c.coid, venue: req.venue, symbol: req.symbol })
        })
        .collect();
    (books.len(), held)
}

/// `contingencies` minus every leg the tail ended or whose parent the tail ended (a filled parent
/// RELEASED its held child to the venue; a dead parent cascaded it away).
fn live_contingencies(
    contingencies: &[SnapContingency],
    facts: &TailFacts,
) -> Vec<SnapContingency> {
    contingencies
        .iter()
        .filter(|c| !facts.ended(&c.coid) && !c.parent.as_deref().is_some_and(|p| facts.ended(p)))
        .cloned()
        .collect()
}

/// The index of the latest `Snap` (a single-variant lookup).
fn latest_snap(records: &[JournalRecord]) -> Option<usize> {
    records.iter().rposition(|r| matches!(r, JournalRecord::Snap { .. }))
}

/// Restore from the `Snap` at `base_idx`: the refold first, the pure folds when it cannot run.
fn restore_from(
    dir: &Path,
    records: Vec<JournalRecord>,
    base_idx: usize,
    scratch_root: &Path,
) -> BootRestore {
    // Everything the pure folds need is read BEFORE the refold, which consumes the records.
    let (minted_by, arm_owner) = fold_coid_origins(&records);
    let owners: HashMap<String, String> = minted_by.into_iter().collect();
    let JournalRecord::Snap {
        seq,
        engines,
        coid_session,
        coid_seq,
        arm_seq,
        conditionals,
        contingencies,
        mount_attr,
        gateway_cursors,
        ..
    } = &records[base_idx]
    else {
        unreachable!("`latest_snap` indexed a Snap")
    };
    let tail = &records[base_idx + 1..];
    let (books, tail_armed) = fold_conditionals(conditionals.clone(), tail);
    let facts = tail_facts(tail);
    // Two answers, because only the refold folds the tail's events into an engine: the cursors the
    // tail advances to (Full) and the `Snap`'s own (SnapOnly: its engines are the `Snap`'s, so a
    // cursor past it would make the gateway skip events no engine ever saw).
    let advanced_cursors = fold_gateway_cursors(gateway_cursors, engines, tail);
    let snap_cursors = gateway_cursors.clone();
    let arm_seed = arm_seq
        .unwrap_or_else(|| records.iter().map(vike_journal::record_seq).max().map_or(0, |m| m + 1));
    let mut report = Report { snap_seq: Some(*seq), tail_records: tail.len(), ..Report::default() };
    let snap_engines = engines.clone();
    let snap_session = (coid_session.clone(), *coid_seq);
    let snap_contingencies = contingencies.clone();
    let ledgers = mount_attr.clone();

    let refold = catch_unwind(AssertUnwindSafe(|| {
        refold_records(records, BaseSelect::Latest, &RefoldOpts::restore(scratch_root))
    }));
    let (outcome, engines, coid_session, arm_seq, contingencies, orders, cursors) = match refold {
        Ok(Ok(r)) => {
            report.skipped_wakers = r.skipped_wakers;
            let orders = resting_orders(&r.engines, None, &r.contingencies, &owners);
            (
                RestoreOutcome::Full,
                r.engines,
                (r.coid_session, r.coid_seq),
                r.arm_seq,
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
            // The pure folds. The coid counter is the `Snap`'s plus one per tail mint (every mint
            // journals exactly one `MintedSubmit`), and the arm counter the `Snap`'s plus one per
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
    BootRestore {
        outcome,
        dir: Some(dir.to_path_buf()),
        state: Some(BootState {
            coid_session,
            arm_seq,
            conditionals: books,
            conditional_owners,
            contingencies,
            engines,
            journal_orders: orders,
            ledgers,
            gateway_cursors: cursors,
        }),
        lost_held_exits: Vec::new(),
        report,
    }
}

/// Drop the segments the latest `Snap` superseded (a Full or Fresh restore only, before the live
/// core opens the directory); a failure is a note, never a refused start.
fn prune(dir: &Path, report: &mut Report) {
    if let Err(e) = CommandJournal::prune_before_latest_snap(dir) {
        report.notes.push(format!(
            "journal restore: the superseded segments of {} could not be pruned: {e}",
            dir.display()
        ));
    }
}

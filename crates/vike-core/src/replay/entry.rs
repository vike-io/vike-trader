//! The two entry points (fenced offline replay, crash restore), their outcome, error and book hash.

use std::path::Path;

use vike_exec::EngineSnapshot;
use vike_journal::{
    CommandJournal, CorruptCause, SNAP_CONDITIONALS_VERSION as SNAP_BOOKS_VERSION, SnapConditional,
};

use super::refold::BaseSelect;
use super::{JournalRecord, RestoredState, fence_floor, replay_from};

/// The result of a successful replay: the source record count, the number of hashes compared
/// (always 1 in v1 — final-hash equality), the reproduced final state hash + engine snapshot(s),
/// and the coid session/seq the replay core ended on (Task 6/7 resume them).
#[derive(Debug, Clone)]
pub struct ReplayOutcome {
    pub records: u64,
    pub snaps_compared: usize,
    pub final_hash: u64,
    pub engines: Vec<EngineSnapshot>,
    pub coid_session: String,
    pub coid_seq: u64,
}

/// Why a replay could not be verified.
#[derive(Debug)]
pub enum ReplayError {
    /// The journal is empty or carries no `Snap` (v1 needs a Snap for the engine config; Task 4
    /// always writes a shutdown Snap, so this means a truncated/absent journal).
    Empty,
    /// The replayed final hash does not equal the source journal's last Snap hash — the fence
    /// caught nondeterminism or a corrupted journal. `expected` is the recorded source hash.
    ///
    /// ⚠ A journal whose READ was truncated reports [`Self::Truncated`] instead — see there.
    HashMismatch { expected: u64, got: u64 },
    /// The journal read stopped at a torn/corrupt frame before the end of the written data
    /// ([`CorruptCause`] says which condition fired), so the records the fences see are a PREFIX of
    /// what the live session wrote — and the fence target is therefore NOT that session's exit
    /// `Snap` but whichever mid-session checkpoint survived. Re-folding the tail past a mid-session
    /// checkpoint necessarily produces a different hash, so this is what a torn tail looks like from
    /// the fence's side.
    ///
    /// Returned INSTEAD OF [`Self::HashMismatch`], and only where that would have been returned: a
    /// truncated journal whose fences all PASS still succeeds exactly as before. The split is
    /// diagnostic — `HashMismatch` reads as "the engine diverged / this journal cannot be trusted",
    /// which for `OobLen` is a misreading of the ordinary consequence of a crash. The halt OFFSET
    /// and the SEGMENT are on the `warn!` that `CommandJournal::read_all` emits at the same halt;
    /// only the classification is carried into the error.
    Truncated { cause: CorruptCause },
    /// A journal shape v1 replay does not handle (multi-engine base, or a watchdog-tick tail).
    Unsupported(String),
    /// The WIDENED fence (emulator PR-6) caught a divergence in restored state that
    /// [`state_hash`](vike_exec::state_hash) does NOT cover — the reproduced resting conditional
    /// books, or an id counter reproduced BELOW the one the live session recorded. `field` names
    /// which; `expected` is the source journal's recorded value (a
    /// [`conditionals_hash`] for the books, the raw counter otherwise).
    ///
    /// Distinct from [`Self::HashMismatch`] on purpose: that one says "the ENGINE state diverged",
    /// this one says "the engine state matched but something else the restore hands back did not",
    /// and the two have completely different triage paths.
    RestoreMismatch { field: &'static str, expected: u64, got: u64 },
    /// The journal directory could not be read/written. (Additive to the spec's three variants:
    /// `CommandJournal::read_all` genuinely returns `io::Result`, and a determinism/durability tool
    /// must not silently fold a real disk fault into `Unsupported`.)
    Io(std::io::Error),
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayError::Empty => write!(f, "journal is empty or has no snapshot to restore from"),
            ReplayError::HashMismatch { expected, got } => write!(
                f,
                "determinism fence failed: replayed hash {got:#018x} != recorded {expected:#018x}"
            ),
            ReplayError::Truncated { cause } => write!(
                f,
                "journal read truncated at a corrupt frame (cause {cause}): {}. The fence target is \
                 the last READABLE Snap, not the session's exit Snap, so no hash comparison is \
                 meaningful — see the `journal read halted at a corrupt frame` warn for the segment \
                 and offset",
                cause.reading()
            ),
            ReplayError::Unsupported(why) => write!(f, "unsupported journal for v1 replay: {why}"),
            ReplayError::RestoreMismatch { field, expected, got } => write!(
                f,
                "restore fence failed on `{field}`: replayed {got:#018x} != recorded {expected:#018x}"
            ),
            ReplayError::Io(e) => write!(f, "journal io error: {e}"),
        }
    }
}

impl std::error::Error for ReplayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReplayError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ReplayError {
    fn from(e: std::io::Error) -> Self {
        ReplayError::Io(e)
    }
}

/// The conditional-book half of the determinism fence (emulator PR-6) — FNV-1a64 over the
/// canonical JSON of a book list, with ONE field cleared: each arm's `terms.extreme`.
///
/// Same algorithm and same clone-and-clear discipline as [`vike_exec::state_hash`], for the same
/// reason. `extreme` is a TRAILING arm's ratcheted high/low water mark: it moves on every
/// non-firing check against MARKET DATA, which is deliberately never journaled — so it is exactly
/// the books' analogue of the `now_ms`/`marks` pair `state_hash` excludes, and for exactly the
/// same argument. A replay/restore reconstructs a base arm's extreme as the last `Snap`'s value
/// and a crash-tail arm's as its `ConditionalArmed` seed (see [`RestoredState::conditionals`]'s
/// documented staleness residual); hashing it would turn that KNOWN, bounded, safe-direction
/// staleness into a spurious fence failure on a perfectly valid journal.
///
/// Everything else about a book IS hashed and IS reproducible from records alone: membership, the
/// `arm_id`s (the disarm keys), each arm's venue/symbol/side/qty, a fixed stop's trigger `price`,
/// and a trailing stop's `trail` distance — plus ORDER, because the books' insertion order is the
/// fire order and therefore state.
pub fn conditionals_hash(books: &[SnapConditional]) -> u64 {
    let mut hashable = books.to_vec();
    for c in &mut hashable {
        c.terms.extreme = None;
    }
    let bytes = serde_json::to_vec(&hashable).expect("SnapConditional serializes");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in &bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Replay the write-ahead command journal at `dir` through a real core and fence the reproduced
/// final state against the source journal's last Snap. See the module docs for the mechanism.
///
/// = [`replay_from`] from the FIRST `Snap` (so the whole tail is re-folded) + THREE fences over
/// the full reproduced restore surface:
/// 1. the final `state_hash` vs the last `Snap`'s recorded hash — the ENGINE state
///    ([`ReplayError::HashMismatch`]);
/// 2. [`conditionals_hash`] of the reproduced resting books vs the last `Snap`'s recorded books —
///    the state PR-3 made restorable but left unfenced ([`ReplayError::RestoreMismatch`]);
/// 3. the reproduced `coid_seq`/`arm_seq` vs the last `Snap`'s, as FLOORS ([`fence_floor`]).
///
/// (2) and (3) cover precisely the parts of [`RestoredState`] that `state_hash` does not: before
/// PR-6 a fold bug that resurrected a consumed arm, dropped a live one, or rewound an id counter
/// reproduced a bit-identical engine hash and the fence passed.
///
/// ⚠ **A TRUNCATED read fails fence 1 by construction, and is reported as such.** When the journal's
/// frame walk halts at a torn frame, the last readable `Snap` is a mid-session checkpoint, so fence
/// 1 compares the honestly re-folded tail against the wrong target and cannot pass. That used to
/// surface as [`ReplayError::HashMismatch`] — "the engine diverged" — for what is the ordinary
/// consequence of a crash; it now surfaces as [`ReplayError::Truncated`] carrying the
/// [`CorruptCause`], which distinguishes a benign torn tail from a segment written by a different
/// BUILD. Residual: a tear that removes EVERY `Snap` still lands as [`ReplayError::Empty`] from
/// [`replay_from`] before this check is reached — that variant already says "truncated/absent
/// journal" in its own doc, so it is not a misreading, merely a less specific one.
pub fn replay_offline(dir: &Path) -> Result<ReplayOutcome, ReplayError> {
    let replayed = replay_from(dir, BaseSelect::First)?;

    // Fence targets: the SOURCE journal's LAST Snap. `replay_from` succeeded ⇒ a Snap exists;
    // re-read (offline, not perf-sensitive; `dir` is untouched — replay wrote to a temp) to fetch
    // it, keeping `replay_from` fence-free.
    //
    // `read_all_reporting` is `read_all` with the walk's halt cause carried out instead of
    // discarded — the SAME records either way. It is used here for one reason: a torn tail makes
    // fence 1 fail with no fence target worth comparing (see the `truncated` arm below).
    let (source_records, truncated) = CommandJournal::read_all_reporting(dir)?;
    let (source_final_hash, source_books, source_coid_seq, source_arm_seq) = source_records
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { hash, conditionals, coid_seq, arm_seq, .. } => {
                Some((*hash, conditionals.clone(), *coid_seq, *arm_seq))
            }
            _ => None,
        })
        .ok_or(ReplayError::Empty)?;

    if replayed.final_hash != source_final_hash {
        // The journal's READ stopped at a torn frame, so `source_final_hash` is the last SURVIVING
        // `Snap` — a mid-session checkpoint — rather than the session's exit `Snap`, and the replay
        // has honestly re-folded the tail past it. There is nothing here for a determinism fence to
        // conclude, and calling it `HashMismatch` reads as "the engine diverged" when a torn tail is
        // the ordinary consequence of a crash. Report the truncation, carrying WHICH condition
        // fired: an `OobLen` tail is benign, a `ParseFailed` frame means a different BUILD wrote it
        // and is not.
        //
        // Deliberately checked HERE and not before the fences: a truncated journal whose fences all
        // PASS (a tear strictly after the exit `Snap`) still returns `Ok` exactly as it did before,
        // so this re-labels a failure and never converts a success into one.
        if let Some(cause) = truncated {
            return Err(ReplayError::Truncated { cause });
        }
        return Err(ReplayError::HashMismatch {
            expected: source_final_hash,
            got: replayed.final_hash,
        });
    }

    // Fence 2 — the resting conditional books. Gated on a v8+ journal: `Snap.conditionals` was
    // `#[serde(default)]`ed in at v8, so a pre-v8 Snap reads back EMPTY whether or not the live
    // session had arms, and comparing against that would reject a correctly-folded book. An older
    // journal keeps replaying exactly as it did (fence 1 only) rather than failing spuriously.
    let books_fenced =
        CommandJournal::latest_segment_version(dir)?.is_some_and(|v| v >= SNAP_BOOKS_VERSION);
    if books_fenced {
        let expected = conditionals_hash(&source_books);
        let got = conditionals_hash(&replayed.conditionals);
        if got != expected {
            return Err(ReplayError::RestoreMismatch { field: "conditionals", expected, got });
        }
    }

    // Fence 3 — the id counters, as floors (see `fence_floor` for why an overshoot is legal).
    // `arm_seq` is `Option` for the pre-v7 seam: an unstamped Snap records no counter to fence.
    fence_floor("coid_seq", replayed.coid_seq, source_coid_seq)?;
    if let Some(recorded) = source_arm_seq {
        fence_floor("arm_seq", replayed.arm_seq, recorded)?;
    }

    Ok(ReplayOutcome {
        records: replayed.records,
        snaps_compared: 1,
        final_hash: replayed.final_hash,
        engines: replayed.engines,
        coid_session: replayed.coid_session,
        coid_seq: replayed.coid_seq,
    })
}

/// Restore the newest checkpoint for a FAST crash restart (spec §A, Task 6) — the mirror of
/// `replay_offline`: restore from the LATEST `Snap` and re-fold only the crash tail after it (empty
/// on a clean shutdown, where the exit Snap already reflects everything; the tail after the last
/// cadence Snap on an unclean crash). This is a RESTORE, not a verification — there is NO fence.
///
/// Returns `Ok(None)` when the journal is empty or has no `Snap` (nothing to restore), `Ok(Some)`
/// otherwise. Same v1 limits as [`replay_offline`]: a multi-engine base or a watchdog-tick tail is
/// [`ReplayError::Unsupported`].
pub fn restore_from_journal(dir: &Path) -> Result<Option<RestoredState>, ReplayError> {
    match replay_from(dir, BaseSelect::Latest) {
        Ok(r) => Ok(Some(RestoredState {
            engines: r.engines,
            coid_session: r.coid_session,
            coid_seq: r.coid_seq,
            arm_seq: r.arm_seq,
            conditionals: r.conditionals,
            contingencies: r.contingencies,
            mount_attr: r.mount_attr,
            coid_mounts: r.coid_mounts,
        })),
        // Empty / no-Snap ⇒ nothing to restore (NOT an error for a restart caller).
        Err(ReplayError::Empty) => Ok(None),
        Err(e) => Err(e),
    }
}

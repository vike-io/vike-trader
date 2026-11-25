//! The two entry points (fenced offline replay, crash restore), their outcome, error and book hash.

use std::path::Path;

use vike_execution::EngineSnapshot;
use vike_journal::{
    Journal, JournalCorruptCause, JournalSnapshotOrderEmulationArm,
    SNAP_CONDITIONALS_VERSION as SNAP_BOOKS_VERSION,
};

use super::refold::JournalReplayBaseSelection;
use super::{JournalRecord, RestoredState, fence_floor, replay_from};

/// The result of a successful replay: the source record count, the number of hashes compared
/// (always 1 in v1 — final-hash equality), the reproduced final state hash + engine snapshot(s),
/// and the order client id session/seq the replay core ended on (Task 6/7 resume them).
#[derive(Debug, Clone)]
pub struct JournalReplayOutcome {
    pub records: u64,
    pub snaps_compared: usize,
    pub final_hash: u64,
    pub engines: Vec<EngineSnapshot>,
    pub order_client_id_session: String,
    pub order_client_id_sequence: u64,
}

/// Why a replay could not be verified.
#[derive(Debug)]
pub enum JournalReplayError {
    /// The journal is empty or carries no `Snapshot` (v1 needs a Snapshot for the engine config; Task 4
    /// always writes a shutdown Snapshot, so this means a truncated/absent journal).
    Empty,
    /// The replayed final hash does not equal the source journal's last Snapshot hash — the fence
    /// caught nondeterminism or a corrupted journal. `expected` is the recorded source hash.
    ///
    /// ⚠ A journal whose READ was truncated reports [`Self::Truncated`] instead — see there.
    HashMismatch { expected: u64, got: u64 },
    /// The journal read stopped at a torn/corrupt frame before the end of the written data
    /// ([`JournalCorruptCause`] says which condition fired), so the records the fences see are a PREFIX of
    /// what the live session wrote — and the fence target is therefore NOT that session's exit
    /// `Snapshot` but whichever mid-session checkpoint survived. Re-folding the tail past a mid-session
    /// checkpoint necessarily produces a different hash, so this is what a torn tail looks like from
    /// the fence's side.
    ///
    /// Returned INSTEAD OF [`Self::HashMismatch`], and only where that would have been returned: a
    /// truncated journal whose fences all PASS still succeeds exactly as before. The split is
    /// diagnostic — `HashMismatch` reads as "the engine diverged / this journal cannot be trusted",
    /// which for `OobLen` is a misreading of the ordinary consequence of a crash. The halt OFFSET
    /// and the SEGMENT are on the `warn!` that `Journal::read_all` emits at the same halt;
    /// only the classification is carried into the error.
    Truncated { cause: JournalCorruptCause },
    /// A journal shape v1 replay does not handle (multi-engine base, or a watchdog-tick tail).
    Unsupported(String),
    /// The WIDENED fence (emulator PR-6) caught a divergence in restored state that
    /// [`state_hash`](vike_execution::state_hash) does NOT cover — the reproduced resting conditional
    /// books, or an id counter reproduced BELOW the one the live session recorded. `field` names
    /// which; `expected` is the source journal's recorded value (a
    /// [`conditionals_hash`] for the books, the raw counter otherwise).
    ///
    /// Distinct from [`Self::HashMismatch`] on purpose: that one says "the ENGINE state diverged",
    /// this one says "the engine state matched but something else the restore hands back did not",
    /// and the two have completely different triage paths.
    RestoreMismatch { field: &'static str, expected: u64, got: u64 },
    /// The journal directory could not be read/written. (Additive to the spec's three variants:
    /// `Journal::read_all` genuinely returns `io::Result`, and a determinism/durability tool
    /// must not silently fold a real disk fault into `Unsupported`.)
    Io(std::io::Error),
}

impl std::fmt::Display for JournalReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JournalReplayError::Empty => {
                write!(f, "journal is empty or has no snapshot to restore from")
            }
            JournalReplayError::HashMismatch { expected, got } => write!(
                f,
                "determinism fence failed: replayed hash {got:#018x} != recorded {expected:#018x}"
            ),
            JournalReplayError::Truncated { cause } => write!(
                f,
                "journal read truncated at a corrupt frame (cause {cause}): {}. The fence target is \
                 the last READABLE Snapshot, not the session's exit Snapshot, so no hash comparison is \
                 meaningful — see the `journal read halted at a corrupt frame` warn for the segment \
                 and offset",
                cause.reading()
            ),
            JournalReplayError::Unsupported(why) => {
                write!(f, "unsupported journal for v1 replay: {why}")
            }
            JournalReplayError::RestoreMismatch { field, expected, got } => write!(
                f,
                "restore fence failed on `{field}`: replayed {got:#018x} != recorded {expected:#018x}"
            ),
            JournalReplayError::Io(e) => write!(f, "journal io error: {e}"),
        }
    }
}

impl std::error::Error for JournalReplayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JournalReplayError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for JournalReplayError {
    fn from(e: std::io::Error) -> Self {
        JournalReplayError::Io(e)
    }
}

/// The conditional-book half of the determinism fence (emulator PR-6) — FNV-1a64 over the
/// canonical JSON of a book list, with ONE field cleared: each arm's `terms.extreme`.
///
/// Same algorithm and same clone-and-clear discipline as [`vike_execution::state_hash`], for the same
/// reason. `extreme` is a TRAILING arm's ratcheted high/low water mark: it moves on every
/// non-firing check against MARKET DATA, which is deliberately never journaled — so it is exactly
/// the books' analogue of the `now_ms`/`marks` pair `state_hash` excludes, and for exactly the
/// same argument. A replay/restore reconstructs a base arm's extreme as the last `Snapshot`'s value
/// and a crash-tail arm's as its `OrderEmulationArmed` seed (see [`RestoredState::order_emulation_arms`]'s
/// documented staleness residual); hashing it would turn that KNOWN, bounded, safe-direction
/// staleness into a spurious fence failure on a perfectly valid journal.
///
/// A stop's ADJUSTMENT (decision 0267, a core-held adjustable stop) is the same kind of state: it
/// turns the arm into its target on a price that reaches its level, market data again, so a stop that
/// adjusted between two `Snapshot`s is the target in the live book and still the adjustable stop in the
/// reproduced one. Each arm that still carries one is hashed AS THE TARGET IT BECOMES (the law's own
/// mutation, `vike_model::apply_stop_adjust`, with its level taken as reached): the target's terms stay
/// fenced, and what is cleared is the pre-adjustment trigger and the level, which the target replaces.
///
/// Everything else about a book IS hashed and IS reproducible from records alone: membership, the
/// `arm_id`s (the disarm keys), each arm's venue/symbol/side/qty, a fixed stop's trigger `price`,
/// and a trailing stop's `trail` distance — plus ORDER, because the books' insertion order is the
/// fire order and therefore state.
pub fn conditionals_hash(books: &[JournalSnapshotOrderEmulationArm]) -> u64 {
    let mut hashable = books.to_vec();
    for c in &mut hashable {
        if let Some(mut armed) = crate::emulator::OrderEmulationArm::from_record(&c.terms)
            && armed.order.adjust.is_some()
        {
            vike_model::apply_stop_adjust(&mut armed.order, f64::INFINITY, f64::NEG_INFINITY);
            let t = &c.terms;
            c.terms = vike_journal::OrderEmulationRecord::of_arm(
                t.venue.clone(),
                t.symbol.clone(),
                &armed.order,
                t.trigger_by,
                t.reduce_only,
            );
        }
        c.terms.extreme = None;
        // The same extreme of an arm journaled under `kind_terms` (a trailing_limit, say).
        if let Some(t) = &mut c.terms.kind_terms {
            t.extreme = None;
        }
        // The arm's ACCOUNT (journal v17) is cleared too: a crash-tail arm re-derived by
        // `fold_conditionals` from its `OrderEmulationArmed` record carries none, so hashing it would
        // fail the fence on a valid multi-account journal. Cleared, the bytes are a v16 book's.
        c.route_key = None;
        // Its MOUNT (decision 0304) for the same reason: a crash-tail arm's row names none.
        c.mount_id = None;
    }
    let bytes = serde_json::to_vec(&hashable).expect("JournalSnapshotOrderEmulationArm serializes");
    vike_utility::fnv::fnv1a64(&bytes)
}

/// Replay the write-ahead command journal at `dir` through a real core and fence the reproduced
/// final state against the source journal's last Snapshot. See the module docs for the mechanism.
///
/// = [`replay_from`] from the FIRST `Snapshot` (so the whole tail is re-folded) + THREE fences over
/// the full reproduced restore surface:
/// 1. the final `state_hash` vs the last `Snapshot`'s recorded hash — the ENGINE state
///    ([`JournalReplayError::HashMismatch`]);
/// 2. [`conditionals_hash`] of the reproduced resting books vs the last `Snapshot`'s recorded books —
///    the state PR-3 made restorable but left unfenced ([`JournalReplayError::RestoreMismatch`]);
/// 3. the reproduced `order_client_id_sequence`/`arm_sequence` vs the last `Snapshot`'s, as FLOORS ([`fence_floor`]).
///
/// (2) and (3) cover precisely the parts of [`RestoredState`] that `state_hash` does not: before
/// PR-6 a fold bug that resurrected a consumed arm, dropped a live one, or rewound an id counter
/// reproduced a bit-identical engine hash and the fence passed.
///
/// ⚠ **A TRUNCATED read fails fence 1 by construction, and is reported as such.** When the journal's
/// frame walk halts at a torn frame, the last readable `Snapshot` is a mid-session checkpoint, so fence
/// 1 compares the honestly re-folded tail against the wrong target and cannot pass. That used to
/// surface as [`JournalReplayError::HashMismatch`] — "the engine diverged" — for what is the ordinary
/// consequence of a crash; it now surfaces as [`JournalReplayError::Truncated`] carrying the
/// [`JournalCorruptCause`], which distinguishes a benign torn tail from a segment written by a different
/// BUILD. Residual: a tear that removes EVERY `Snapshot` still lands as [`JournalReplayError::Empty`] from
/// [`replay_from`] before this check is reached — that variant already says "truncated/absent
/// journal" in its own doc, so it is not a misreading, merely a less specific one.
pub fn replay_offline(dir: &Path) -> Result<JournalReplayOutcome, JournalReplayError> {
    let replayed = replay_from(dir, JournalReplayBaseSelection::First)?;

    // Fence targets: the SOURCE journal's LAST Snapshot. `replay_from` succeeded ⇒ a Snapshot exists;
    // re-read (offline, not perf-sensitive; `dir` is untouched — replay wrote to a temp) to fetch
    // it, keeping `replay_from` fence-free.
    //
    // `read_all_reporting` is `read_all` with the walk's halt cause carried out instead of
    // discarded — the SAME records either way. It is used here for one reason: a torn tail makes
    // fence 1 fail with no fence target worth comparing (see the `truncated` arm below).
    let (source_records, truncated) = Journal::read_all_reporting(dir)?;
    let (source_final_hash, source_books, source_order_client_id_sequence, source_arm_sequence) =
        source_records
            .iter()
            .rev()
            .find_map(|r| match r {
                JournalRecord::Snapshot {
                    hash,
                    order_emulation_arms: conditionals,
                    order_client_id_sequence,
                    arm_sequence: arm_sequence_value,
                    ..
                } => Some((
                    *hash,
                    conditionals.clone(),
                    *order_client_id_sequence,
                    *arm_sequence_value,
                )),
                // every other record: not this tap's business (audited 2026-10, I-21)
                _ => None,
            })
            .ok_or(JournalReplayError::Empty)?;

    if replayed.final_hash != source_final_hash {
        // The journal's READ stopped at a torn frame, so `source_final_hash` is the last SURVIVING
        // `Snapshot` — a mid-session checkpoint — rather than the session's exit `Snapshot`, and the replay
        // has honestly re-folded the tail past it. There is nothing here for a determinism fence to
        // conclude, and calling it `HashMismatch` reads as "the engine diverged" when a torn tail is
        // the ordinary consequence of a crash. Report the truncation, carrying WHICH condition
        // fired: an `OobLen` tail is benign, a `ParseFailed` frame means a different BUILD wrote it
        // and is not.
        //
        // Deliberately checked HERE and not before the fences: a truncated journal whose fences all
        // PASS (a tear strictly after the exit `Snapshot`) still returns `Ok` exactly as it did before,
        // so this re-labels a failure and never converts a success into one.
        if let Some(cause) = truncated {
            return Err(JournalReplayError::Truncated { cause });
        }
        return Err(JournalReplayError::HashMismatch {
            expected: source_final_hash,
            got: replayed.final_hash,
        });
    }

    // Fence 2 — the resting conditional books. Gated on a v8+ journal: `Snapshot.order_emulation_arms` was
    // `#[serde(default)]`ed in at v8, so a pre-v8 Snapshot reads back EMPTY whether or not the live
    // session had arms, and comparing against that would reject a correctly-folded book. An older
    // journal keeps replaying exactly as it did (fence 1 only) rather than failing spuriously.
    let books_fenced =
        Journal::latest_segment_version(dir)?.is_some_and(|v| v >= SNAP_BOOKS_VERSION);
    if books_fenced {
        let expected = conditionals_hash(&source_books);
        let got = conditionals_hash(&replayed.order_emulation_arms);
        if got != expected {
            return Err(JournalReplayError::RestoreMismatch {
                field: "order_emulation_arms",
                expected,
                got,
            });
        }
    }

    // Fence 3 — the id counters, as floors (see `fence_floor` for why an overshoot is legal).
    // `arm_sequence` is `Option` for the pre-v7 seam: an unstamped Snapshot records no counter to fence.
    fence_floor(
        "order_client_id_sequence",
        replayed.order_client_id_sequence,
        source_order_client_id_sequence,
    )?;
    if let Some(recorded) = source_arm_sequence {
        fence_floor("arm_sequence", replayed.arm_sequence, recorded)?;
    }

    Ok(JournalReplayOutcome {
        records: replayed.records,
        snaps_compared: 1,
        final_hash: replayed.final_hash,
        engines: replayed.engines,
        order_client_id_session: replayed.order_client_id_session,
        order_client_id_sequence: replayed.order_client_id_sequence,
    })
}

/// Restore the newest checkpoint for a FAST crash restart (spec §A, Task 6) — the mirror of
/// `replay_offline`: restore from the LATEST `Snapshot` and re-fold only the crash tail after it (empty
/// on a clean shutdown, where the exit Snapshot already reflects everything; the tail after the last
/// cadence Snapshot on an unclean crash). This is a RESTORE, not a verification — there is NO fence.
///
/// Returns `Ok(None)` when the journal is empty or has no `Snapshot` (nothing to restore), `Ok(Some)`
/// otherwise. Same v1 limits as [`replay_offline`]: a multi-engine base or a watchdog-tick tail is
/// [`JournalReplayError::Unsupported`].
pub fn restore_from_journal(dir: &Path) -> Result<Option<RestoredState>, JournalReplayError> {
    match replay_from(dir, JournalReplayBaseSelection::Latest) {
        Ok(r) => Ok(Some(RestoredState {
            engines: r.engines,
            order_client_id_session: r.order_client_id_session,
            order_client_id_sequence: r.order_client_id_sequence,
            arm_sequence: r.arm_sequence,
            order_emulation_arms: r.order_emulation_arms,
            contingencies: r.contingencies,
            held_releases: r.held_releases,
            emulated: r.emulated,
            mount_attribution: r.mount_attribution,
            order_client_id_mounts: r.order_client_id_mounts,
        })),
        // Empty / no-Snapshot ⇒ nothing to restore (NOT an error for a restart caller).
        Err(JournalReplayError::Empty) => Ok(None),
        Err(e) => Err(e),
    }
}

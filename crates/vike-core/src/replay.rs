//! Deterministic OFFLINE replay through the REAL core + a state-hash determinism fence
//! (spec 2026-07-10-journal-replay-and-data-freshness.md §A, Task 5), plus the crash-restart RESTORE
//! path (Task 6). This is the payoff of the write-ahead command journal: a recorded session can be
//! re-derived through the same single-writer runtime and proven bit-identical, which both gates the
//! journal format (any nondeterminism in the fold surfaces as a mismatch) and is the substrate the
//! restart restore + Task 7 build on.
//!
//! **Two entry points, one shared engine ([`replay_from`]):**
//! - [`replay_offline`] — restore from the FIRST `Snap`, re-fold the WHOLE tail, FENCE the
//!   reproduced final hash against the source's LAST `Snap` hash. Proves determinism.
//! - [`restore_from_journal`] — restore from the LATEST `Snap`, re-fold only the (usually empty)
//!   crash tail, and return the reproduced engines + coid session. NO fence — it is a restore.
//!
//! **Mechanism — restore from the FIRST checkpoint; fence on the final hash.**
//! 1. Restore the engine from the journal's FIRST `Snap` (the earliest full-config checkpoint).
//!    Restoring from the LATEST would leave nothing to replay on a clean run (its exit Snap already
//!    reflects everything) — a worthless fence. Cadence Snaps are taken AFTER the fold (Task 4), so
//!    every `Cmd` before the first Snap is already folded into it and every `Cmd` after it is
//!    re-folded forward, re-deriving the whole post-checkpoint history.
//! 2. Spawn a real core from that engine with a PURE no-op [`ReplayClient`] (venue side-effects
//!    suppressed — the original venue events are already journaled as `Ingest::Event` records and
//!    get re-pumped; a client that ALSO emitted events would DOUBLE the state), a [`QueueClock`]
//!    replaying the tail's recorded `now_ms` so timestamp-derived state (e.g. `ManagedOrder
//!    .created_ms`) matches, the base Snap's coid session, and a temp journal with
//!    `snapshot_every = u64::MAX` (only the exit Snap fires).
//! 3. Pump every post-base `Cmd` through the handle IN ORDER, shut down, join.
//! 4. Read the replay temp journal's final Snap. **Fence:** its hash MUST equal the SOURCE
//!    journal's LAST Snap hash. The replay core's temp seqs differ from the source's, so per-seq
//!    comparison is impossible — final-hash equality is the essential guarantee (re-folding from
//!    the first checkpoint reproduces the exact final state).
//!
//! **v1 scope:** single-engine only. `spawn_core_multi` accepts pre-built extra engines, but their
//! per-engine `seed_cash` is not part of `EngineSnapshot`, so a faithful multi-engine restore is
//! deferred; a multi-engine base returns [`ReplayError::Unsupported`]. A watchdog-tick tail is also
//! `Unsupported` (the stuck-order / dead-man wall-clock sweeps it carries are not deterministic
//! functions of the journal; the GTD sweep no longer writes that record — see below).
//!
//! **Mounted-strategy sessions replay too (portfolio-observer PR-5).** A mounted strategy reacts to
//! NON-journaled bar/tick messages, but its order flow does NOT: `apply_strategy_intent` journals
//! each ALREADY-RESOLVED `OrderIntent` write-ahead of `apply_intent` (T2's [`JournalRecord
//! ::StrategySubmit`], mint/tag resolution already done — same shape a `Cmd`-carried
//! `Command::Order` would carry). The tail extraction below (T3) replays a `StrategySubmit` exactly
//! like a `Cmd`: `Ingest::Command(Command::Order(intent))` through the SAME `apply_intent` site.
//! Replay never re-runs the strategy's hooks at all — the replay core mounts no strategy (`cfg` below
//! sets none) — so it does not matter that the triggering ticks are non-journaled; only the
//! strategy's already-DECIDED orders need to reproduce, and those ride the journal now. (`now_ms`
//! and the mark cache remain excluded from `state_hash` for the same reason as ever: plain
//! non-journaled MARKET data — with no strategy reacting to it — still cannot move the fenced state.)
//!
//! **Emulated conditional FIRES replay too (emulator-journal PR-1).** A resting conditional
//! crossing on a bar/tick is a runtime reaction to NON-journaled market data, and used to be the
//! first-named replay residual. It no longer is: `submit_fired` journals the DECISION write-ahead
//! as [`JournalRecord::ConditionalFire`] (arm id + the released `OrderRequest`, coid still empty),
//! and the tail extraction below re-applies it as `Command::Order(Submit(req))` through the SAME
//! `apply_intent` site — never re-evaluating a trigger, so the fire can neither double nor vanish.
//! The companion [`JournalRecord::ConditionalArmed`] record (the ARM's resolved terms, including a
//! trailing arm's mark-seeded extreme) is REPLAY-NEUTRAL like `MintedSubmit`: the tail ignores it,
//! and the arm itself still replays from its own write-ahead `Cmd`/`StrategySubmit`.
//!
//! **Emulated conditional DISARMS replay too (emulator PR-2).** The arm-id-keyed disarm verb the
//! paragraph above's residual note used to ask for now exists: [`vike_exec::OrderIntent
//! ::DisarmConditional`] rides an ordinary write-ahead `Cmd`/`StrategySubmit` (the intent carries
//! the complete `arm_id` — unlike a trailing ARM there is nothing mark-seeded to resolve), so the
//! generic tail arms below re-apply it through the SAME `apply_intent` site and the replay core's
//! book drops the same arm the live one dropped — a restart cannot resurrect a disarmed
//! conditional. Identity is what makes that sound: the replay core's arm counter is seeded from
//! the base `Snap`'s stamped `arm_seq` (v7 — the prune-safe fix), so tail arms re-mint the
//! IDENTICAL ids the live core minted and a tail disarm finds its arm. The companion
//! [`JournalRecord::ConditionalDisarmed`] record (written write-ahead of the live book mutation)
//! is REPLAY-NEUTRAL like `ConditionalArmed`: the tail ignores it.
//!
//! **Armed conditionals survive a restart too (emulator PR-3, re-arm-on-restore).** The resting
//! books ride every `Snap` (`Snap.conditionals`: each arm's minted id + the terms the book held,
//! a TRAILING arm's CURRENT ratcheted extreme included — the one value neither the write-ahead
//! ARM `Cmd` nor the `ConditionalArmed` seed record can carry, because it moves on non-journaled
//! market data). Restore hands them back as [`RestoredState::conditionals`], which a restart
//! seeds via [`crate::CoreConfig::conditionals`] — a live position's protective stop no longer
//! silently vanishes with the process. Two deliberate design points:
//!
//! 1. **Crash-tail membership is a PURE FOLD of the journal's conditional records, never the
//!    replay core's own book state** ([`fold_conditionals`]): starting from the base `Snap`'s
//!    books, a tail `ConditionalArmed` ADDS (with its recorded seed terms), a tail
//!    `ConditionalFire`/`ConditionalDisarmed` REMOVES, and a tail `Cmd`/`StrategySubmit`-carried
//!    `MassCancel`/`MarketExit` clears its venue/symbol scope — the same semantics the live
//!    `apply_intent` applies. The replay core's book is NOT trusted for membership because it
//!    pumps no market data: a tail FIRE's arm would linger in it (the fire replays as a Submit,
//!    never a trigger), and a tail TRAILING arm is refused for lack of a mark — the fold gets
//!    both right from the records. (That refusal also skips a mint, so the replayed exit
//!    `arm_seq` can UNDERCOUNT; `replay_from` patches it with `base + tail-armed-count`, sound
//!    because every live mint wrote exactly one `ConditionalArmed` after the base.)
//! 2. **The books stay OUT of `state_hash`** (which remains `state_hash(&engines)` — engine
//!    state only). The fence is still sound: book contents can influence fenced state only
//!    through a FIRE, and every fire journals its own write-ahead `ConditionalFire` record that
//!    replays as the released order — a trigger is never re-evaluated, so no book divergence can
//!    move the replayed engines. The restored books are themselves deterministic (a pure
//!    function of the journal: Snap payload + record fold), just fenced by construction rather
//!    than by hash.
//!
//! Residual, documented: a trailing arm's restored extreme is the LAST-SNAPSHOT value (staleness
//! bounded by the snapshot cadence) for base arms, and the ARM-record seed for crash-tail arms —
//! never the true crash-instant extreme, which only existed in memory. The stop re-arms slightly
//! LOOSER than it died (the extreme only ever ratchets favorably), and re-tightens as live data
//! resumes; that is the best any design can do without journaling market data. Within the replay
//! core itself the pre-PR-3 sharp edge is now confined to that core's private book state and is
//! invisible to the fence, as before. One mixed-journal corner of that edge: versions are per
//! SEGMENT, so a v7+ binary resuming a pre-v7 journal can leave a tail with arms/disarms under a
//! base Snap that stamps no `arm_seq` — the replay core then re-mints tail arms under the
//! overshot fallback seed and a tail disarm may miss INSIDE that core (a loud, replay-neutral
//! no-op); the restore itself is unaffected, because [`fold_conditionals`] resolves membership by
//! the RECORDED ids and the counter bound above only ever overshoots.
//!
//! **Margin-call auto-liquidations replay too (emulator PR-4).** A margin-call sweep
//! (`sweep_margin_call_engine`) fires reduce-only MARKET liquidations off a CLOSED BAR when the
//! account's marks + equity breach maintenance — and neither the bar nor the equity is journaled,
//! so it used to be the class's remaining residual (a fired session surfaced as
//! `ReplayError::HashMismatch`). It no longer is: the sweep journals each released order write-ahead
//! as [`JournalRecord::MarginCallLiquidate`] (the `ConditionalFire` pattern applied to the
//! margin-call path), and the tail extraction below re-applies it as `Command::Order(Submit(req))`
//! through the SAME `apply_intent` site — never re-evaluating the breach (the replay core pumps no
//! market data, so its equity never moves), so a liquidation can neither double nor vanish. The
//! coid is re-minted identically from the restored generator, and the follow-on `MintedSubmit` the
//! release writes is replay-neutral as ever. Note the sweep runs on the per-closed-bar path, NOT a
//! wall-clock waker, so it enqueues no `Ingest::Watchdog` — unlike the GTD sweep below, a
//! margin-call session is genuinely replayable, not merely rejected.
//!
//! **The MANAGED GTD/Day EXPIRY sweep replays too (emulator PR-5) — via a MARKER, not a command.**
//! `sweep_gtd_expiry` (opt-in, `CoreConfig::gtd_sweep`) cancels a resting order off wall clock at
//! the drain-loop boundary; it used to be this scope's last documented residual, unreachable in
//! practice because enabling it armed the boundary waker whose `Ingest::Watchdog` records the tail
//! scan below rejects as `Unsupported`. It now journals each expiry write-ahead as
//! [`JournalRecord::GtdExpire`] — and, unlike `ConditionalFire`/`MarginCallLiquidate`, that record
//! is NOT re-applied here. The reason is that the sweep releases no order: it calls
//! `ExecutionEngine::cancel_order`, which publishes nothing locally, so it moves NO fenced state.
//! The state change is the venue's own `OrderCanceled`, which is journaled as an `Ingest::Event`
//! and replays independently through the normal tail. Re-issuing the cancel here would therefore
//! add nothing against [`ReplayClient`] (whose `cancel` is an explicit no-op) and would DOUBLE the
//! action against any future non-inert replay client. The marker instead removes the need to
//! re-DECIDE the expiry — which is what let `gtd_sweep` drop out of `journal_waker_records`, so a
//! `gtd_sweep`-only session now writes no waker record and replays/crash-restores normally.
//! `submit_ack_timeout` and `deadman` still arm that record and still refuse LOUDLY.
//!
//! **The fence now covers the WHOLE restored surface (emulator PR-6, the epic closer).** Every PR
//! above widened what REPLAYS; none widened what is CHECKED. [`replay_offline`] fenced exactly one
//! thing — `state_hash(&engines)` — while [`restore_from_journal`] hands a restart three more
//! values that no fence looked at: the resting conditional books PR-3 made restorable, and the
//! `coid_seq`/`arm_seq` counters. A fold bug that resurrected a consumed arm, dropped a live one,
//! mis-scoped a `MassCancel`, or rewound a counter reproduced a bit-identical ENGINE hash, so the
//! fence passed and the restart silently re-armed a taken exit or re-minted a spent disarm key.
//! `replay_offline` now adds two fences over the same oracle the hash fence uses — the SOURCE
//! journal's last `Snap`, which records the live books and counters at exit:
//!
//! - **books:** [`conditionals_hash`] of the reproduced books vs the recorded ones. Note what this
//!   does NOT change: the books still do not enter `state_hash`, and PR-3's argument for that is
//!   still exactly true after PR-5 (book contents move fenced state only through a FIRE, and every
//!   fire journals its own write-ahead `ConditionalFire`; PR-5 touched only the GTD sweep, which
//!   arms nothing). The books are now fenced ALONGSIDE the engine hash instead of by construction.
//! - **counters:** `coid_seq`/`arm_seq` as FLOORS, not equalities — an overshoot is legal and
//!   deliberately produced (a mark-refused tail trailing arm, a pre-v7 base), an undercount is the
//!   id-reuse bug. See [`fence_floor`].
//!
//! **The exclusions that REMAIN, each with its argument** (they are exclusions, not oversights):
//!
//! - `now_ms` + `AccountSnapshot::marks` — non-journaled market data mutates both every message;
//!   see [`vike_exec::state_hash`]'s doc. Unchanged.
//! - A trailing arm's `terms.extreme` — the books' exact analogue of `marks`: it ratchets on
//!   non-journaled market data, so `conditionals_hash` clears it, and the LAST-snapshot-value
//!   staleness stays the documented, safe-direction residual on [`RestoredState::conditionals`].
//! - `CoreThread::gtd_canceled` (the GTD sweep's fire-once set) — deliberately NOT snapshotted.
//!   It is a duplicate-SUPPRESSION set, not state: [`crate::journal::JournalRecord::GtdExpire`] is
//!   replay-neutral (the tail ignores it) and the sweep's `cancel_order` publishes nothing local,
//!   so the set cannot move `state_hash` in either direction, and the one observable consequence —
//!   a restart re-deciding a still-resting expired order and emitting a SECOND `GtdExpire` marker
//!   for one logical expiry — is CORRECT behavior (the order really is still resting and expired)
//!   rather than divergence. Snapshotting it would add a journal field to make an audit count
//!   tidier while changing no outcome; the caveat on that variant is the right call instead.
//! - `dirty`, `journaled_since_snap`, the bounded recent-events ring — publish/cadence/diagnostic
//!   bookkeeping. `dirty` and `journaled_since_snap` only decide WHEN a snapshot or GUI publish
//!   fires, never what it contains; the ring is read-only UI context. None is restored at all, so
//!   there is nothing for a fence to compare.
//!
//! **Per-mount ATTRIBUTION survives the restart too (multi-mount durability, gap D).** Two more
//! values ride the restore, both UNFENCED and both deliberately outside `state_hash` (they are a
//! read-only view over fills that are themselves journaled `Cmd` events — they drive no fold
//! decision and can move no fenced state): [`RestoredState::mount_attr`], the per-mount ledgers the
//! latest `Snap` captures, and [`RestoredState::coid_mounts`], the coid -> mount ORIGIN map folded
//! purely from the journal's existing `StrategySubmit`/`MintedSubmit` provenance
//! ([`fold_coid_mounts`]) — no new record type, the origin has always been on disk and simply was
//! never read back. Without them a restart resumed every mount ledger at zero and every pre-restart
//! resting order filled unattributed, while `Account.realized_pnl` kept the same PnL — a silent,
//! permanent `Σ mounts ≠ account` drift. Both residuals (the base-`Snap` staleness of the ledgers on
//! an unclean crash, and the budget-flatten order journaled without a mount id) are documented on
//! those two items and surface in the published RESIDUAL row rather than vanishing.
//!
//! **Residual scope: `apply_strategy_intent`** wraps only the `drain_broker` boundary — a mounted
//! strategy's OWN buffered submit/bracket/modify/cancel/conditional/mass_cancel — and every other
//! runtime-internal order path now has its own write-ahead site.
//!
//! Note the converse, so the `Unsupported` waker rule is not read as broader than it is: the waker
//! record is journaled ONLY when a state-mutating wall-clock sweep is configured (stuck-order
//! watchdog / dead-man — see `CoreThread::journal_waker_records`; GTD dropped OUT of that set in
//! v10, see above). Boundary features that
//! merely OBSERVE — the equity sampler, the periodic strategy-state save, and the periodic
//! `PortfolioSnap` record — also need the waker to reach an idle core, but write no waker record,
//! so a session running only those still replays and crash-restores normally.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use vike_exec::{EngineSnapshot, ExecutionClient, ExecutionEngine, Ingest};
use vike_model::{Clock, OrderRequest, Strategy};

use crate::journal::{
    CommandJournal, CorruptCause, JournalFileConfig, JournalRecord, SnapConditional,
    SnapContingency, SNAP_CONDITIONALS_VERSION as SNAP_BOOKS_VERSION,
};
use crate::{spawn_core, CoreConfig, JournalConfig, LiveBroker, StrategyMount};

/// A PURE no-op [`ExecutionClient`] for replay: it suppresses ALL venue side-effects and
/// synthesizes NO events. The original venue events are already in the journal as `Ingest::Event`
/// records and are re-pumped during replay; a client that ALSO emitted events would double the
/// state and (correctly) break the fence. This is the deliberate OPPOSITE of `TestExecutionClient`
/// (which DOES synthesize). `submit`/`cancel` are explicit no-ops; every other method (including
/// `poll_events`, the load-bearing one) uses the trait default, and the default `poll_events`
/// returns `None` — so no event is ever synthesized.
#[derive(Debug, Default)]
pub struct ReplayClient;

impl ExecutionClient for ReplayClient {
    fn submit(&mut self, _request: &OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
    // modify / submit_batch / cancel_batch / detach / on_bar: trait defaults are already no-ops.
    // poll_events: default returns None -> NO synthesized events (the load-bearing property).
}

/// A [`Strategy`] with every hook at its trait default (no-op) — mounted by [`replay_from`] SOLELY
/// to mirror the base snapshot's `EngineSnapshot::collect_applied_fills` flag (see the mount site
/// below for why). Its hooks are never actually invoked: the tail pump only ever sends
/// `Ingest::Event`/`Ingest::Command` (see the loop below), never the Quote/Trade/Book/BarClose
/// messages that would dispatch to a mount — so mounting it has NO other observable effect on the
/// replayed state.
#[derive(Debug)]
struct InertStrategy;

impl Strategy<LiveBroker> for InertStrategy {}

/// A [`Clock`] that replays a recorded sequence of `now_ms` stamps — the tail commands' journaled
/// dispatch timestamps, in order. The core reads the clock exactly once per dispatched message
/// (`runtime.rs`: `engine.now_ms = clock.now_ms()`), so popping the front per call hands message
/// `k` its exact recorded stamp. Once only the last stamp remains it is PEEKED, not popped, so any
/// further reads (the exit snapshot's single clock read, which is not part of `state_hash`) repeat
/// the last value harmlessly. Interior-mutable (`Mutex`) because [`Clock::now_ms`] takes `&self`.
pub struct QueueClock {
    stamps: Mutex<VecDeque<i64>>,
}

impl QueueClock {
    /// Seed with the tail commands' recorded `now_ms`, in journal order.
    pub fn new(stamps: Vec<i64>) -> Self {
        QueueClock { stamps: Mutex::new(stamps.into()) }
    }
}

impl Clock for QueueClock {
    fn now_ms(&self) -> i64 {
        let mut q = self.stamps.lock().unwrap();
        if q.len() > 1 {
            q.pop_front().unwrap()
        } else {
            // keep the last stamp resident so post-tail reads repeat it (empty ⇒ degenerate 0)
            q.front().copied().unwrap_or(0)
        }
    }
}

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

/// Which `Snap` in the journal supplies the restore base (Task 6 factoring).
enum BaseSelect {
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
struct Replayed {
    engines: Vec<EngineSnapshot>,
    coid_session: String,
    coid_seq: u64,
    /// the replay core's final emulated-conditional arm-id counter (its exit Snap's stamped
    /// `arm_seq`, floored at `base + tail-armed-count` — see the module doc's undercount note) —
    /// the arm-side twin of `coid_seq`, seeded from the BASE Snap (or the pre-v7 fallback bound)
    /// and advanced by any tail arms, exactly as `coid_seq` is
    arm_seq: u64,
    /// the resting conditional books at the journal's end — the base `Snap`'s books folded
    /// forward through the tail's conditional records ([`fold_conditionals`]), in fire order
    conditionals: Vec<SnapConditional>,
    /// the resting OTO/OCO contingency book at the journal's end — read straight from the replay
    /// core's own exit `Snap` (the mark-less replay core reproduces it faithfully, so no record
    /// fold is needed — see the `contingencies` seed comment in `replay_from`)
    contingencies: Vec<SnapContingency>,
    /// the per-mount attribution ledgers captured on the BASE `Snap` (gap D)
    mount_attr: Vec<crate::journal::SnapMountAttr>,
    /// `(coid, mount_id)` for every order a mount minted, folded from the WHOLE readable record
    /// set ([`fold_coid_mounts`]) — not just the tail, so an order resting since before the base
    /// `Snap` still restores its origin
    coid_mounts: Vec<(String, String)>,
    /// source journal record count (for `ReplayOutcome::records`)
    records: u64,
    /// hash of the replayed exit Snap (the reproduced final state)
    final_hash: u64,
}

/// The shared machinery of `replay_offline` + `restore_from_journal` (Task 6): restore the engine
/// from the `base`-selected `Snap`, spawn a real core (pure no-op [`ReplayClient`], [`QueueClock`]
/// replaying the tail's `now_ms`, temp journal with `snapshot_every = u64::MAX`), pump the post-base
/// `Cmd` tail IN ORDER, and read back the replayed exit Snap. Returns [`ReplayError::Empty`] when
/// the journal is empty or carries no `Snap`, [`ReplayError::Unsupported`] for a multi-engine base
/// or a watchdog-tick tail (v1 scope). NO fence — the caller adds it (`replay_offline`) or omits it
/// (`restore_from_journal`).
fn replay_from(dir: &Path, base: BaseSelect) -> Result<Replayed, ReplayError> {
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
    #[allow(clippy::type_complexity)]
    let (
        base_engines,
        base_session,
        base_seq,
        base_arm_seq,
        base_conditionals,
        base_contingencies,
        base_mount_attr,
    ) = match &records[base_idx] {
        JournalRecord::Snap {
            engines,
            coid_session,
            coid_seq,
            arm_seq,
            conditionals,
            contingencies,
            mount_attr,
            ..
        } => (
            engines.clone(),
            coid_session.clone(),
            *coid_seq,
            *arm_seq,
            conditionals.clone(),
            contingencies.clone(),
            mount_attr.clone(),
        ),
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
    let arm_seq_seed = base_arm_seq.unwrap_or_else(|| {
        records.iter().map(crate::journal::record_seq).max().map_or(0, |m| m + 1)
    });

    // v1: single engine only. `spawn_core_multi` would accept pre-built extra engines, but their
    // per-engine seed_cash is not in the snapshot, so a faithful multi-engine restore is deferred.
    if base_engines.len() != 1 {
        return Err(ReplayError::Unsupported(
            "multi-engine replay (v1: single-engine only)".into(),
        ));
    }
    let base = &base_engines[0];

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
    let tail: Vec<(i64, Ingest)> = records
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
        .collect();
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
                let _ = events.blocking_send(e);
            }
            Ingest::Command(c) => handle.send_command(c),
            Ingest::Watchdog => unreachable!("watchdog tail rejected above"),
            // Market/Bar*/Quote/Trade/Book are never journaled (Task 4 journals only the exec lane),
            // so they cannot appear here; skip defensively rather than panic on a future format.
            _ => {}
        }
    }
    handle.shutdown_and_join(); // delivers Shutdown losslessly after the tail, then joins

    // Read the replay journal's final Snap: it carries the reproduced hash / engines / coid state
    // + the replayed arm counter (this build always stamps `arm_seq`, so the `unwrap_or` below is
    // for shape only — the replay core's exit Snap is v7 by construction).
    let replayed = CommandJournal::read_all(&temp)?;
    let (final_hash, engines, coid_session, coid_seq, arm_seq, restored_contingencies) = replayed
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
            } => Some((
                *hash,
                engines.clone(),
                coid_session.clone(),
                *coid_seq,
                arm_seq.unwrap_or(arm_seq_seed),
                contingencies.clone(),
            )),
            _ => None,
        })
        .ok_or_else(|| ReplayError::Unsupported("replay produced no snapshot".into()))?;
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

/// The PURE coid -> mount-id fold behind attribution durability (gap D): which mount MINTED each
/// order, read back out of the journal's existing provenance records.
///
/// Nothing new is written to make this work. `apply_strategy_intent` already journals every mounted
/// strategy's intent as [`JournalRecord::StrategySubmit`] carrying its `mount_id`, write-ahead of the
/// `apply_intent` that mints the coid — and that mint writes its own [`JournalRecord::MintedSubmit`]
/// with the RESOLVED request immediately after, before any other record can be appended (the fold is
/// single-writer and single-threaded). So the pairing rule is positional and exact:
///
/// - a `StrategySubmit` OPENS ownership by its `mount_id` (and, for the rare intent that already
///   carries a client-supplied coid, maps that coid directly — no `MintedSubmit` follows one);
/// - every following `MintedSubmit` maps its minted coid to the open owner (a `Bracket` intent mints
///   three legs and writes three records — all three belong to the mount that armed them);
/// - a record that begins a NON-strategy write closes ownership: a `Cmd` (an operator/GUI command),
///   a `ConditionalFire`, an UNOWNED `MarginCallLiquidate` (the account-wide margin-call sweep), or
///   a `Snap`. Their `MintedSubmit`s are therefore correctly left UNATTRIBUTED.
/// - a `MarginCallLiquidate` that DOES name a `mount_id` (journal v14 — the per-mount budget latch's
///   flatten, `CoreThread::latch_mount`) OPENS ownership by that id, exactly like a `StrategySubmit`:
///   that order is minted by, and closes the attributed position of, one specific mount.
///
/// Kept a pure function over records (no replay core, no engine) for the same reason
/// [`fold_conditionals`] is: it must be able to reconstruct history the mark-less replay core cannot.
///
/// The `mount_id` arm closes what used to be a documented residual here. The budget latch's flatten
/// attributed its coid in memory but journaled it indistinguishably from an account-wide
/// liquidation, so on restore that one order came back unattributed and its fill landed in the
/// RESIDUAL row instead of the mount's ledger — under-reporting exactly the realized loss the budget
/// latch exists to bound. A PRE-v14 journal's latch flatten still restores unattributed (the field
/// reads back `None`), which is that journal's own recorded truth rather than a new loss.
fn fold_coid_mounts(records: &[JournalRecord]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut owner: Option<&str> = None;
    for rec in records {
        match rec {
            JournalRecord::StrategySubmit { mount_id, intent, .. } => {
                owner = Some(mount_id.as_str());
                // A client-supplied (non-empty) coid never reaches the minting path, so it has no
                // `MintedSubmit` to pair with — map it here or lose it.
                if let Some(coid) = client_supplied_coid(intent) {
                    out.push((coid, mount_id.clone()));
                }
            }
            JournalRecord::MintedSubmit { req, .. } => {
                let coid = req.client_order_id.as_str();
                if let Some(m) = owner.filter(|_| !coid.is_empty()) {
                    out.push((coid.to_string(), m.to_string()));
                }
            }
            // The per-mount budget latch's flatten NAMES its owner (v14): it OPENS ownership, so the
            // `MintedSubmit` that follows — the coid `apply_intent` mints for it — books into that
            // mount's ledger, identically to the live runtime's own `coid_mount` insert.
            JournalRecord::MarginCallLiquidate { mount_id: Some(mid), .. } => {
                owner = Some(mid.as_str());
            }
            JournalRecord::Cmd { .. }
            | JournalRecord::Snap { .. }
            | JournalRecord::ConditionalFire { .. }
            // ...whereas an UNOWNED `MarginCallLiquidate` is the account-wide margin-call sweep
            // (and every pre-v14 frame, which defaults to `None`) — a release no mount originated,
            // so it CLOSES.
            | JournalRecord::MarginCallLiquidate { mount_id: None, .. } => owner = None,

            // ── EXHAUSTIVE by design: NO `_` arm (see [`JournalRecord`]'s "Adding a variant"
            // contract). These five are observations/markers that MINT NOTHING, so they neither
            // open nor close ownership — an interleaved one must leave a `StrategySubmit` ->
            // `MintedSubmit` pair intact, which is why they are ignored rather than closing:
            //   `PortfolioSnap`       — a periodic portfolio observation.
            //   `ConditionalArmed`    — resolves an arm's terms; no order reaches a venue.
            //   `ConditionalDisarmed` — drops an arm; likewise no order.
            //   `GtdExpire`           — an expiry DECISION; issues a cancel, mints no coid.
            //   `ScheduleFire`        — an on_schedule DECISION; the orders it produces arrive as
            //                           their own `StrategySubmit` records, which open ownership.
            // A new variant that DOES mint a coid must be classified into one of the arms above,
            // not added here — the compiler will demand the decision.
            JournalRecord::PortfolioSnap { .. }
            | JournalRecord::ConditionalArmed { .. }
            | JournalRecord::ConditionalDisarmed { .. }
            | JournalRecord::GtdExpire { .. }
            | JournalRecord::ScheduleFire { .. } => {}
        }
    }
    out
}

/// The coid a journaled intent ALREADY carries, if any — the one case [`fold_coid_mounts`] cannot
/// pair with a following `MintedSubmit`, because a non-empty `client_order_id` skips the mint
/// entirely (and therefore writes no such record).
fn client_supplied_coid(intent: &vike_exec::OrderIntent) -> Option<String> {
    match intent {
        vike_exec::OrderIntent::Submit(req) if !req.client_order_id.is_empty() => {
            Some(req.client_order_id.clone())
        }
        _ => None,
    }
}

/// The PURE membership fold behind re-arm-on-restore (emulator PR-3): the resting conditional
/// books at the end of a record sequence, derived from the base `Snap`'s captured books plus the
/// tail's conditional records — never from re-evaluating a trigger, and never from the replay
/// core's book state (see the module doc's design point 1 for why that state is not trusted).
///
/// Fold rules, mirroring the live `apply_intent`/`submit_fired` semantics record-for-record:
/// - [`JournalRecord::ConditionalArmed`] ADDS the arm (its RESOLVED terms — a trailing arm's
///   mark-seeded extreme included; a refused arm never wrote this record, so refusals are
///   excluded by construction). Appended in record order, which IS live insertion (fire) order.
/// - [`JournalRecord::ConditionalFire`] / [`JournalRecord::ConditionalDisarmed`] REMOVE the arm
///   they name (both are written only when the live book actually changed).
/// - A `Cmd`/`StrategySubmit`-carried [`vike_exec::OrderIntent::MassCancel`] clears its scope
///   exactly as the live arm does (all books / one venue / one (venue, symbol); a symbol without
///   a venue is ignored, the live no-op), and [`vike_exec::OrderIntent::MarketExit`] clears its
///   venue scope (its lowering's mass-cancel leg is venue-scoped, symbol-`None`).
/// - The `Cmd`-carried `ArmConditional`/`DisarmConditional` intents themselves are IGNORED: the
///   `ConditionalArmed`/`ConditionalDisarmed` records above are their authoritative outcomes
///   (an intent can be refused; the record is only written for an applied mutation).
///
/// Returns the folded books plus the count of `ConditionalArmed` records seen (the arm-counter
/// bound `replay_from` patches with).
fn fold_conditionals(
    mut books: Vec<SnapConditional>,
    tail: &[JournalRecord],
) -> (Vec<SnapConditional>, u64) {
    let mut armed_count = 0u64;
    for rec in tail {
        match rec {
            JournalRecord::ConditionalArmed { arm_id, resolved, .. } => {
                armed_count += 1;
                if !books.iter().any(|c| c.arm_id == *arm_id) {
                    books.push(SnapConditional { arm_id: arm_id.clone(), terms: resolved.clone() });
                }
            }
            JournalRecord::ConditionalFire { arm_id, .. }
            | JournalRecord::ConditionalDisarmed { arm_id, .. } => {
                books.retain(|c| c.arm_id != *arm_id);
            }
            JournalRecord::Cmd {
                msg: Ingest::Command(vike_exec::Command::Order(intent)), ..
            } => fold_intent_scope(&mut books, intent),
            JournalRecord::StrategySubmit { intent, .. } => fold_intent_scope(&mut books, intent),

            // ── EXHAUSTIVE by design: NO `_` arm (see [`JournalRecord`]'s "Adding a variant"
            // contract). None of these can change book MEMBERSHIP:
            //   `Cmd` carrying anything but an order intent — a venue `Event`, a `Watchdog`, or a
            //     non-`Order` command. (The refined `Cmd` arm above catches the order-intent case;
            //     this one is its complement, and is what keeps the match exhaustive over `Cmd`.)
            //   `Snap`                — the fold's own STARTING books; never re-applied here.
            //   `MintedSubmit`        — a resolved order request; arms are not orders.
            //   `PortfolioSnap`       — a portfolio observation.
            //   `MarginCallLiquidate` — releases a reduce-only MARKET, arms/disarms nothing.
            //   `GtdExpire`           — expires a RESTING ORDER, not an arm (a mass-cancel-cleared
            //                           arm folds through its own intent, above).
            //   `ScheduleFire`        — an on_schedule DECISION; any arm it produced wrote its own
            //                           `ConditionalArmed`.
            JournalRecord::Cmd { .. }
            | JournalRecord::Snap { .. }
            | JournalRecord::MintedSubmit { .. }
            | JournalRecord::PortfolioSnap { .. }
            | JournalRecord::MarginCallLiquidate { .. }
            | JournalRecord::GtdExpire { .. }
            | JournalRecord::ScheduleFire { .. } => {}
        }
    }
    (books, armed_count)
}

/// The book-clearing half of [`fold_conditionals`]: apply one journaled intent's effect on book
/// MEMBERSHIP (`MassCancel`/`MarketExit` scoping — the live `apply_intent` arms' exact clear
/// semantics). Every other intent leaves the books untouched (arms/disarms fold from their own
/// records instead — see the caller's doc).
fn fold_intent_scope(books: &mut Vec<SnapConditional>, intent: &vike_exec::OrderIntent) {
    let clear = |books: &mut Vec<SnapConditional>, venue: Option<&str>, symbol: Option<&str>| {
        match (venue, symbol) {
            (None, None) => books.clear(),
            (Some(v), None) => books.retain(|c| c.terms.venue != v),
            (Some(v), Some(s)) => books.retain(|c| !(c.terms.venue == v && c.terms.symbol == s)),
            (None, Some(_)) => {} // the live arm ignores symbol-without-venue
        }
    };
    match intent {
        vike_exec::OrderIntent::MassCancel { venue, symbol } => {
            clear(books, venue.as_deref(), symbol.as_deref());
        }
        vike_exec::OrderIntent::MarketExit { venue } => clear(books, venue.as_deref(), None),
        _ => {}
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

/// The id-counter half of the widened fence: a reproduced counter may OVERSHOOT what the live
/// session recorded but must never land BELOW it.
///
/// The asymmetry is the whole point, and it is a safety property rather than a determinism one.
/// An overshoot only wastes ids (`replay_from` deliberately produces one for a mark-refused tail
/// trailing arm, and again for a pre-v7 base — both documented there). An UNDERCOUNT means a
/// restored session re-mints an id the pre-crash run already spent into this very journal — and
/// since emulator PR-2 an `arm_id` is the DISARM key, so a duplicate lets a stale
/// `DisarmConditional` silently remove a fresh stop-loss. Fencing the floor generalizes the one
/// hand-written scenario that class of bug had.
fn fence_floor(field: &'static str, got: u64, expected: u64) -> Result<(), ReplayError> {
    if got < expected {
        return Err(ReplayError::RestoreMismatch { field, expected, got });
    }
    Ok(())
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
    /// prune-UNSAFE: [`crate::journal::CommandJournal::prune_before_latest_snap`] deletes whole
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
    pub mount_attr: Vec<crate::journal::SnapMountAttr>,
    /// The coid -> mount ORIGIN map to re-seed ([`crate::CoreConfig::coid_mounts`]) — one
    /// `(client_order_id, mount_id)` per order a mount minted, folded from the journal's existing
    /// `StrategySubmit`/`MintedSubmit` provenance ([`fold_coid_mounts`]) over the WHOLE readable
    /// record set. This is what lets an order still RESTING across the restart book its eventual fill
    /// into the mount that placed it. Empty for a journal with no mounted-strategy submits.
    pub coid_mounts: Vec<(String, String)>,
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

/// A fresh, collision-free working directory for one replay core's journal, OWNED — removed when
/// the returned guard drops, on every path including a panic.
///
/// Uniqueness is the process id + a nanosecond stamp + a per-call counter, so concurrent
/// `replay_offline` calls never share one.
///
/// ⚠ **This is a journal directory, which makes the ownership the expensive half.**
/// [`crate::journal::segment`]'s `reserve_blocks` calls `posix_fallocate` and `JournalConfig::at`
/// defaults to 64 MiB segments, so a segment is FULLY allocated the moment it is created rather
/// than sparse. A leaked replay directory is a real 64 MiB. `replay_from` did remove this at the
/// end — but only on the success path: every `?` above that line, and every panic, left one behind.
/// That is the same shape that put 215 GB of leaked scratch on the CI box, and
/// [`vike_model::scratch::ScratchDir`] is what closes it here.
///
/// ⚠ **The PARENT is still the system temp directory, and that is a KNOWN debt with a row in
/// `crates/vike-ops/tests/system_temp_gate.rs`'s `SYSTEM_TEMP_PIN`, not an oversight.** Production
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

#[cfg(test)]
mod fold_tests {
    //! White-box tests of [`fold_conditionals`] — the pure membership fold behind
    //! re-arm-on-restore. The end-to-end restore is gated in `tests/conditional_journal.rs`;
    //! these pin the per-record semantics (add / remove / mass-cancel scoping) in isolation.

    use super::*;
    use crate::journal::ConditionalRecord;

    fn arm(arm_id: &str, venue: &str, symbol: &str, price: Option<f64>) -> SnapConditional {
        SnapConditional {
            arm_id: arm_id.into(),
            terms: ConditionalRecord {
                venue: venue.into(),
                symbol: symbol.into(),
                side: -1,
                qty: 1.0,
                price,
                trail: price.is_none().then_some(5.0),
                extreme: price.is_none().then_some(100.0),
                trigger_by: None,
            },
        }
    }

    fn armed_rec(seq: u64, c: &SnapConditional) -> JournalRecord {
        JournalRecord::ConditionalArmed {
            seq,
            now_ms: seq as i64,
            arm_id: c.arm_id.clone(),
            resolved: c.terms.clone(),
        }
    }

    fn fire_rec(seq: u64, arm_id: &str) -> JournalRecord {
        JournalRecord::ConditionalFire {
            seq,
            now_ms: seq as i64,
            arm_id: arm_id.into(),
            trigger_px: 0.0,
            req: OrderRequest::default(),
        }
    }

    fn disarm_rec(seq: u64, arm_id: &str) -> JournalRecord {
        JournalRecord::ConditionalDisarmed { seq, now_ms: seq as i64, arm_id: arm_id.into() }
    }

    fn cmd(seq: u64, intent: vike_exec::OrderIntent) -> JournalRecord {
        JournalRecord::Cmd {
            seq,
            now_ms: seq as i64,
            msg: Ingest::Command(vike_exec::Command::Order(intent)),
        }
    }

    fn ids(books: &[SnapConditional]) -> Vec<&str> {
        books.iter().map(|c| c.arm_id.as_str()).collect()
    }

    #[test]
    fn armed_adds_fire_and_disarm_remove_in_record_order() {
        let base = vec![arm("a0", "sim", "BTC", None), arm("a1", "sim", "BTC", Some(95.0))];
        let a2 = arm("a2", "sim", "BTC", Some(90.0));
        let tail = vec![
            fire_rec(10, "a0"),   // the live session consumed a0 (the replay core never would)
            armed_rec(11, &a2),   // armed after the base Snap
            disarm_rec(12, "a1"), // disarmed after the base Snap
        ];
        let (books, armed_count) = fold_conditionals(base, &tail);
        assert_eq!(ids(&books), vec!["a2"], "fire and disarm removed; the tail arm added");
        assert_eq!(books[0].terms.price, Some(90.0), "the ARM record's resolved terms carried");
        assert_eq!(armed_count, 1);
    }

    #[test]
    fn tail_arms_keep_record_order_which_is_fire_order() {
        let a2 = arm("a2", "sim", "BTC", Some(90.0));
        let a3 = arm("a3", "sim", "BTC", Some(85.0));
        let (books, _) = fold_conditionals(
            vec![arm("a0", "sim", "BTC", Some(95.0))],
            &[armed_rec(1, &a2), armed_rec(2, &a3)],
        );
        assert_eq!(ids(&books), vec!["a0", "a2", "a3"], "base first, tail in record order");
    }

    #[test]
    fn mass_cancel_scoping_matches_the_live_arm() {
        let base = || {
            vec![
                arm("a0", "sim", "BTC", Some(95.0)),
                arm("a1", "sim", "ETH", Some(90.0)),
                arm("a2", "other", "BTC", Some(85.0)),
            ]
        };
        // global: everything clears
        let (books, _) = fold_conditionals(
            base(),
            &[cmd(1, vike_exec::OrderIntent::MassCancel { venue: None, symbol: None })],
        );
        assert!(books.is_empty(), "global mass-cancel clears every book");
        // venue-scoped: only that venue's arms clear
        let (books, _) = fold_conditionals(
            base(),
            &[cmd(
                1,
                vike_exec::OrderIntent::MassCancel { venue: Some("sim".into()), symbol: None },
            )],
        );
        assert_eq!(ids(&books), vec!["a2"], "venue scope clears only sim's arms");
        // (venue, symbol)-scoped: only the one book clears
        let (books, _) = fold_conditionals(
            base(),
            &[cmd(
                1,
                vike_exec::OrderIntent::MassCancel {
                    venue: Some("sim".into()),
                    symbol: Some("BTC".into()),
                },
            )],
        );
        assert_eq!(ids(&books), vec!["a1", "a2"], "(venue, symbol) scope clears one book");
        // symbol without venue: the live no-op
        let (books, _) = fold_conditionals(
            base(),
            &[cmd(
                1,
                vike_exec::OrderIntent::MassCancel { venue: None, symbol: Some("BTC".into()) },
            )],
        );
        assert_eq!(ids(&books), vec!["a0", "a1", "a2"], "symbol-without-venue is ignored");
        // MarketExit: venue-scoped clear (its mass-cancel leg), None = all
        let (books, _) = fold_conditionals(
            base(),
            &[cmd(1, vike_exec::OrderIntent::MarketExit { venue: Some("sim".into()) })],
        );
        assert_eq!(ids(&books), vec!["a2"], "MarketExit clears its venue scope");
        let (books, _) = fold_conditionals(
            base(),
            &[cmd(1, vike_exec::OrderIntent::MarketExit { venue: None })],
        );
        assert!(books.is_empty(), "venue-less MarketExit clears everything");
    }
}

#[cfg(test)]
mod hash_tests {
    //! White-box tests of [`conditionals_hash`] — the book half of the widened determinism fence
    //! (emulator PR-6). One test proves the ONE documented exclusion (a trailing arm's ratcheted
    //! `extreme`) genuinely cannot move the hash; the rest prove every OTHER part of a book does,
    //! so the exclusion is a scalpel rather than a hole.

    use super::*;
    use crate::journal::ConditionalRecord;

    fn fixed(arm_id: &str) -> SnapConditional {
        SnapConditional {
            arm_id: arm_id.into(),
            terms: ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: Some(95.0),
                trail: None,
                extreme: None,
                trigger_by: None,
            },
        }
    }

    fn trailing(arm_id: &str, extreme: f64) -> SnapConditional {
        SnapConditional {
            arm_id: arm_id.into(),
            terms: ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: None,
                trail: Some(5.0),
                extreme: Some(extreme),
                trigger_by: None,
            },
        }
    }

    /// THE documented exclusion, proven: a trailing arm's `extreme` ratchets on non-journaled
    /// market data (the books' analogue of `AccountSnapshot::marks`), so restore reconstructs the
    /// last-snapshot value rather than the crash-instant one. Hashing it would fail a valid
    /// journal; clearing it must make the two indistinguishable — including the `Some`/`None`
    /// shape difference a mark-less reconstruction produces.
    #[test]
    fn the_ratcheted_extreme_is_excluded_from_the_hash() {
        let live = vec![trailing("a0", 110.0)];
        let restored_stale = vec![trailing("a0", 100.0)];
        let restored_markless = {
            let mut v = vec![trailing("a0", 100.0)];
            v[0].terms.extreme = None;
            v
        };
        assert_eq!(conditionals_hash(&live), conditionals_hash(&restored_stale));
        assert_eq!(conditionals_hash(&live), conditionals_hash(&restored_markless));
    }

    /// The scalpel's other edge: everything the journal CAN reproduce is fenced. Each tamper is
    /// one field, so a future `conditionals_hash` that over-clears is caught here.
    #[test]
    fn every_other_field_moves_the_hash() {
        let base = vec![trailing("a0", 110.0)];
        let h = conditionals_hash(&base);

        let mut t = base.clone();
        t[0].arm_id = "a1".into(); // the DISARM key — the most load-bearing field of all
        assert_ne!(h, conditionals_hash(&t), "arm_id (the disarm key) is fenced");

        let mut t = base.clone();
        t[0].terms.trail = Some(6.0);
        assert_ne!(
            h,
            conditionals_hash(&t),
            "the trail DISTANCE is fenced (only the extreme is not)"
        );

        let mut t = base.clone();
        t[0].terms.qty = 2.0;
        assert_ne!(h, conditionals_hash(&t), "qty is fenced");

        let mut t = base.clone();
        t[0].terms.side = 1;
        assert_ne!(h, conditionals_hash(&t), "side is fenced");

        let mut t = base.clone();
        t[0].terms.venue = "other".into();
        assert_ne!(h, conditionals_hash(&t), "venue is fenced");

        let mut t = base.clone();
        t[0].terms.symbol = "ETHUSDT".into();
        assert_ne!(h, conditionals_hash(&t), "symbol is fenced");

        let mut t = vec![fixed("a0")];
        let hf = conditionals_hash(&t);
        t[0].terms.price = Some(96.0);
        assert_ne!(hf, conditionals_hash(&t), "a fixed stop's trigger price is fenced");
    }

    /// MEMBERSHIP and ORDER are the two properties the fold can get wrong (a resurrected arm, a
    /// dropped one, a mis-ordered rebuild) — and books' insertion order IS fire order, so it is
    /// state, not presentation.
    #[test]
    fn membership_and_order_move_the_hash() {
        let two = vec![fixed("a0"), fixed("a1")];
        assert_ne!(
            conditionals_hash(&two),
            conditionals_hash(&[fixed("a0")]),
            "a resurrected/dropped arm moves the hash"
        );
        assert_ne!(
            conditionals_hash(&two),
            conditionals_hash(&[fixed("a1"), fixed("a0")]),
            "fire order is state, so order moves the hash"
        );
        assert_eq!(conditionals_hash(&[]), conditionals_hash(&[]), "empty books agree");
    }

    /// [`fence_floor`]'s asymmetry, pinned: an overshoot is legal (deliberately produced by a
    /// mark-refused tail trailing arm and by a pre-v7 base), an undercount is the id-reuse bug.
    #[test]
    fn the_counter_fence_is_a_floor_not_an_equality() {
        assert!(fence_floor("arm_seq", 7, 7).is_ok(), "equality passes");
        assert!(fence_floor("arm_seq", 9, 7).is_ok(), "an overshoot only wastes ids");
        match fence_floor("arm_seq", 6, 7) {
            Err(ReplayError::RestoreMismatch { field, expected, got }) => {
                assert_eq!((field, expected, got), ("arm_seq", 7, 6));
            }
            other => panic!("an undercount must be rejected, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod coid_mount_fold_tests {
    //! White-box tests of [`fold_coid_mounts`] — the pure coid -> mount-id fold behind attribution
    //! durability (gap D). The end-to-end restore is gated in `tests/mount_attr_durability.rs`;
    //! these pin the per-record pairing rule (open / pair / close) in isolation.

    use super::*;

    fn req(coid: &str) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ts: 1,
            ..Default::default()
        }
    }

    fn strategy_submit(seq: u64, mount_id: &str, coid: &str) -> JournalRecord {
        JournalRecord::StrategySubmit {
            seq,
            now_ms: seq as i64,
            mount_id: mount_id.into(),
            // the runtime always journals the resolved intent with an EMPTY coid (the mint happens
            // inside `apply_intent`, after this record) unless the caller supplied one
            intent: vike_exec::OrderIntent::Submit(Box::new(req(coid))),
        }
    }

    fn minted(seq: u64, coid: &str) -> JournalRecord {
        JournalRecord::MintedSubmit { seq, now_ms: seq as i64, req: req(coid) }
    }

    /// The pairing rule: each `StrategySubmit` OPENS ownership and every `MintedSubmit` that follows
    /// it (a bracket mints three) belongs to that mount — until a non-strategy write closes it.
    #[test]
    fn minted_submits_pair_with_the_open_strategy_submit() {
        let recs = vec![
            strategy_submit(1, "maker-a", ""),
            minted(2, "c1"),
            strategy_submit(3, "maker-b", ""),
            minted(4, "c2"),
            minted(5, "c3"), // a bracket's second leg — same owner
        ];
        assert_eq!(
            fold_coid_mounts(&recs),
            vec![
                ("c1".to_string(), "maker-a".to_string()),
                ("c2".to_string(), "maker-b".to_string()),
                ("c3".to_string(), "maker-b".to_string()),
            ]
        );
    }

    /// A NON-strategy write closes ownership, so an operator command's minted coid — and a
    /// runtime-decided liquidation's — stay UNATTRIBUTED rather than being credited to whichever
    /// mount happened to submit last.
    #[test]
    fn a_non_strategy_write_closes_ownership() {
        let recs = vec![
            strategy_submit(1, "maker-a", ""),
            minted(2, "c1"),
            cmd_order(3, vike_exec::OrderIntent::Submit(Box::new(req("")))),
            minted(4, "operator-coid"),
            JournalRecord::MarginCallLiquidate {
                seq: 5,
                now_ms: 5,
                req: req(""),
                mount_id: None, // the ACCOUNT-wide margin-call sweep — owned by no mount
            },
            minted(6, "liquidation-coid"),
        ];
        assert_eq!(
            fold_coid_mounts(&recs),
            vec![("c1".to_string(), "maker-a".to_string())],
            "only the mount-minted order is attributed"
        );
    }

    /// **THE v14 ARM.** A `MarginCallLiquidate` that NAMES a mount — the per-mount budget latch's
    /// flatten — OPENS ownership like a `StrategySubmit`, so the coid `apply_intent` mints for it
    /// restores into THAT mount's ledger.
    ///
    /// Without the id (every pre-v14 frame, and the account-wide sweep above) the flatten's coid came
    /// back unattributed and its fill landed in the RESIDUAL row — under-reporting the exact realized
    /// loss the budget latch exists to bound.
    #[test]
    fn an_owned_margin_call_liquidate_attributes_its_flatten_to_that_mount() {
        let recs = vec![
            strategy_submit(1, "maker-a", ""),
            minted(2, "c1"), // the mount's own resting order
            JournalRecord::MarginCallLiquidate {
                seq: 3,
                now_ms: 3,
                req: req(""),
                mount_id: Some("maker-a".into()), // the BUDGET LATCH's flatten
            },
            minted(4, "flatten-coid"),
        ];
        assert_eq!(
            fold_coid_mounts(&recs),
            vec![
                ("c1".to_string(), "maker-a".to_string()),
                ("flatten-coid".to_string(), "maker-a".to_string()),
            ],
            "the latch's flatten belongs to the mount whose budget breach released it"
        );
    }

    /// A client-SUPPLIED coid writes no `MintedSubmit` (it skips the mint), so the intent itself is
    /// the only place its origin exists.
    #[test]
    fn a_client_supplied_coid_maps_from_the_intent() {
        let recs = vec![strategy_submit(1, "maker-a", "given-coid")];
        assert_eq!(
            fold_coid_mounts(&recs),
            vec![("given-coid".to_string(), "maker-a".to_string())]
        );
    }

    fn cmd_order(seq: u64, intent: vike_exec::OrderIntent) -> JournalRecord {
        JournalRecord::Cmd {
            seq,
            now_ms: seq as i64,
            msg: Ingest::Command(vike_exec::Command::Order(intent)),
        }
    }
}

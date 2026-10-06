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
//!   It is a duplicate-SUPPRESSION set, not state: [`vike_journal::JournalRecord::GtdExpire`] is
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

mod client;
mod entry;
mod fold;
mod refold;

pub use client::{QueueClock, ReplayClient};
pub use entry::{
    ReplayError, ReplayOutcome, conditionals_hash, replay_offline, restore_from_journal,
};
use fold::{fence_floor, fold_coid_mounts, fold_conditionals};
pub use refold::RestoredState;
use refold::replay_from;
use vike_journal::JournalRecord;

#[cfg(test)]
mod fold_tests;

#[cfg(test)]
mod hash_tests;

#[cfg(test)]
mod coid_mount_fold_tests;

#[cfg(test)]
use vike_exec::Ingest;
#[cfg(test)]
use vike_journal::SnapConditional;
#[cfg(test)]
use vike_model::OrderRequest;

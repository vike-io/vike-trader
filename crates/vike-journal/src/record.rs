//! The journal's serde SCHEMA: [`JournalRecord`] (the externally-tagged wire enum every frame
//! carries), its payload structs, the borrowed write twin [`JournalRecordRef`], and [`record_seq`].
//!
//! Split out of `journal.rs` verbatim — the version-compatibility contract that governs adding a
//! variant here lives in the parent module's doc, and [`JournalRecord`]'s own "Adding a variant"
//! table is the checklist.

use serde::{Deserialize, Serialize};
use vike_exec::Ingest;

/// One frame of the write-ahead command journal.
///
/// # Adding a variant — the no-silent-drop contract
///
/// **Every variant must be EXPLICITLY NAMED by the replay fold and the materializer. A new variant
/// is a COMPILE ERROR at each of them, never a silent drop.** Three independent places interpret
/// this enum — the writer here, `vike_core::replay`'s fold (the crate ABOVE this one), and
/// [`crate::materialize`]'s materializer (this crate) — and both of the FOLD sites used to end in a
/// `_ =>` catch-all
/// (the writer's own [`record_seq`] was already exhaustive, which is exactly why it never caught
/// anything). A variant added to only the writer therefore compiled everywhere, was written to the
/// WAL, and was DROPPED at recovery: restored state silently diverged from pre-crash state. Not
/// hypothetical — PR #915's commit 3 fixed exactly this instance ([`Self::MarginCallLiquidate`]
/// carried no `mount_id`, so replay restored the order unattributed and its fill vanished into the
/// residual row), found by hand weeks after the variant was added.
///
/// So the catch-alls are gone from every site that makes a per-variant BEHAVIOR decision. Adding a
/// variant now fails to build until each of these names it:
///
/// | site | the decision it forces |
/// |---|---|
/// | [`record_seq`] (this file) | which field carries the global seq |
/// <!-- ⚠ A second row read `vike_journal::materialize::record_seq` | the same, in the
/// materializer's crate`. That site is GONE (2026-09-25): the materializer moved INTO this crate
/// and its private copy was deleted for this one. A new variant is therefore one fewer decision. -->
/// | `vike_core::replay`'s tail extraction (`replay_from`) | RE-APPLY on replay, or replay-neutral? |
/// | `vike_core::replay::fold_coid_mounts` | does it OPEN / CLOSE / not touch coid->mount ownership? |
/// | `vike_core::replay::fold_conditionals` | does it change conditional-book membership? |
/// | `vike_journal::materialize::OrderTracker::fold_record` | does it produce an exec-log row? |
///
/// Answering "nothing" is a perfectly good answer — write `JournalRecord::Foo { .. } => {}` with a
/// one-line reason, exactly as every already-ignored variant does there. The point is that the
/// answer is RECORDED rather than defaulted.
///
/// **Four things still will NOT break, so check them by hand:**
/// 1. [`JournalRecordRef`], the borrowed write twin — its variant/field names must match EXACTLY or
///    the frame will not deserialize back; add the mirror variant and an `append_*` method.
/// 2. [`super::VERSION`] — bump it, and extend the [`super::MIN_READABLE_VERSION`] rationale (a
///    purely additive variant leaves `MIN_READABLE_VERSION` where it is; see the module doc's
///    compatibility contract for when it must be raised instead).
/// 3. Single-variant LOOKUPS, deliberately left as catch-alls because they ask "is this THE `Snap`?"
///    (or "is this a fill?") rather than dispatching, and so are correct for any future variant by
///    construction: `replay_from`'s base-`Snap` `position`/`rposition` and its exit-`Snap`
///    `find_map`, `replay_offline`'s source-`Snap` `find_map`, `vike_mount::incident`'s
///    latest-`Snap` `find_map`, and `vike_report::fills_from_journal`.
/// 4. `vike_journal::materialize::materialize_once`'s FILL extraction is an `if let` on [`Self::Cmd`],
///    which no exhaustiveness check can reach. A new variant able to carry a
///    `vike_model::events::FillEvent` must be added there explicitly.
#[derive(Debug, Serialize, Deserialize)]
pub enum JournalRecord {
    /// One exec-lane ingest message, with the wall-clock the core stamped for its dispatch.
    Cmd { seq: u64, now_ms: i64, msg: Ingest },
    /// Full-state snapshot-as-command: replay starts at the LATEST of these.
    Snap {
        seq: u64,
        now_ms: i64,
        engines: Vec<vike_exec::EngineSnapshot>,
        coid_session: String,
        coid_seq: u64,
        /// The emulated-conditional arm-id counter at Snap time — the arm-side twin of
        /// `coid_seq`, stamped for the same reason: a restart resumes `coid_session`, so the
        /// counter behind `{coid_session}a{arm_seq}` must resume too or post-restart arms
        /// re-emit spent ids (and `arm_id` is the DISARM key since emulator PR-2, so an
        /// ambiguous id would disarm the wrong arm).
        ///
        /// `Option` + `#[serde(default)]` is the v6->v7 compat seam: a pre-v7 `Snap` frame has
        /// no such field and reads back `None` — meaning ABSENT, never "0" — and the restore
        /// path falls back to a prune-safe bound (max global record seq + 1, see
        /// `vike_core::replay`). Counting currently-READABLE records, the pre-v7 formula, is
        /// prune-UNSAFE: `prune_before_latest_snap` deletes whole early segments, so a restart
        /// after a prune undercounted the ids already spent and re-minted duplicates.
        #[serde(default)]
        arm_seq: Option<u64>,
        /// The RESTING emulated-conditional books at Snap time (emulator PR-3, re-arm-on-restore):
        /// one entry per armed conditional, in FIRE order (the books map's insertion order, then
        /// each book's own insertion order), each carrying its minted `arm_id` + the terms the
        /// book actually holds — for a TRAILING arm that includes the CURRENT ratcheted extreme,
        /// which is live state (it moves on every non-firing check against non-journaled market
        /// data) and therefore exists NOWHERE else: the `ConditionalArmed` record only captured
        /// the seed. Snapping the book is what lets a restart re-arm a protective stop instead of
        /// silently dropping it.
        ///
        /// `#[serde(default)]` is the v7->v8 compat seam: a pre-v8 `Snap` frame has no such field
        /// and reads back EMPTY — a pre-v8 journal restores with empty books (exactly its
        /// pre-PR-3 behavior), never an error. Deliberately NOT part of `state_hash` (which stays
        /// over the engine snapshots): book contents can only move fenced state through a FIRE,
        /// and every fire journals its own write-ahead [`Self::ConditionalFire`] record — see
        /// `vike_core::replay`'s module doc for the full fence argument.
        #[serde(default)]
        conditionals: Vec<SnapConditional>,
        /// The RESTING OTO/OCO contingency book at Snap time (live-runtime OCO/OTO): one entry per
        /// linked order — entries AND their held/armed exits — carrying the linkage the shared
        /// [`vike_exec::ContingencyBook`] holds (`parent`/`linked`/`active`) plus, for a HELD exit,
        /// the resolved [`vike_model::OrderRequest`] the runtime is holding back from the venue
        /// until its parent fills. It is the durable home for that held request: unlike an ACTIVE
        /// leg (already in the engine registry, so restored via `engines` above), a held exit lives
        /// ONLY in the runtime's side book and would be LOST on a restart — leaving a filled entry
        /// with no protective stop/take-profit — without this capture.
        ///
        /// `#[serde(default)]` is the v10->v11 compat seam: a pre-v11 `Snap` frame has no such field
        /// and reads back EMPTY (a pre-OCO/OTO journal restores with no contingency state, exactly
        /// its prior behavior, never an error). Deliberately NOT part of `state_hash` (which stays
        /// over the engine snapshots): the book moves fenced state only through a FILL that releases
        /// a held child / cancels a sibling, and every such fill is itself a journaled `Cmd` event
        /// that a replay re-folds through the re-seeded book — see `vike_core::replay`.
        #[serde(default)]
        contingencies: Vec<SnapContingency>,
        /// The per-mount fill ATTRIBUTION ledgers at Snap time (multi-mount durability, gap D): one
        /// row per mount slot — its `mount_id` plus the position/avg-px/realized/fees folded from
        /// exactly that mount's own fills. It is the ONLY durable home for them: attribution lives
        /// one layer up from `Account` (deliberately — `Account` is parity-gated and its snapshot
        /// wire is byte-identical-pinned), so nothing in `engines` carries it and a restart used to
        /// resume with every mount ledger silently back at zero while `Account.realized_pnl` kept
        /// the same PnL. Keyed by `mount_id` rather than by index so a re-ordered mount list
        /// restores onto the right mounts (an id that names no mounted slot is simply skipped).
        ///
        /// `#[serde(default)]` is the v12->v13 compat seam: a pre-v13 `Snap` frame has no such field
        /// and reads back EMPTY (a pre-feature journal restores with zeroed ledgers — exactly its
        /// prior behavior, never an error). Deliberately NOT part of `state_hash` (which stays over
        /// the engine snapshots), for the same reason `conditionals`/`contingencies` are not: the
        /// ledgers are a read-only VIEW folded from fills that are themselves journaled `Cmd`
        /// events — they drive no fold decision and can move no fenced state.
        #[serde(default)]
        mount_attr: Vec<SnapMountAttr>,
        hash: u64,
    },
    /// A mounted strategy's order intent, journaled write-ahead at the `drain_broker` boundary —
    /// the choke point where a strategy's buffered intents lower into `apply_intent` (PR-5). Today
    /// these bypass the journal entirely; this variant is the record of them. `mount_id` is
    /// provenance only (the `(venue, symbol, interval)` key the mount was registered under);
    /// replay re-derives coids from `intent` alone, same as a `Cmd`-carried `Command::Order`.
    StrategySubmit { seq: u64, now_ms: i64, mount_id: String, intent: vike_exec::OrderIntent },
    /// A server-minted submit's RESOLVED [`vike_model::OrderRequest`] — journaled from inside
    /// `apply_intent` immediately AFTER the coid is minted, for a submit whose incoming
    /// `client_order_id` was EMPTY. The write-ahead `Cmd`-carried `Command::Order(Submit)` recorded
    /// that same request with the coid still empty (the mint happens in the fold, after that
    /// record), so this variant is the durable tie between a server-minted coid and its
    /// `(venue, symbol, side, qty, type, price)` — the materializer's `exec_order` seed for orders
    /// that terminalize WITHOUT ever filling (the minted-coid gap, `journal_mat.rs`). It is
    /// REPLAY-NEUTRAL: replay re-mints from the empty-coid `Cmd` exactly as before and simply
    /// re-emits this record into its temp journal, so the determinism fence is unchanged (the tail
    /// extraction in `replay.rs` ignores this variant — it is not a command to re-apply).
    ///
    /// `route_key` (journal v16; `#[serde(default)]` ⇒ `None` on every pre-v16 frame) is WHICH
    /// ACCOUNT of `req.venue` this submit was lowered onto — absent meaning *"that venue's sole
    /// account"*, exactly `vike_exec::ReconcileReports::route_key`'s convention, so a
    /// single-account box writes no key and its journal bytes are identical to the ones written
    /// before the field existed.
    ///
    /// ⚠ **Why an OBSERVATION record carries a routing fact.** `req.venue` is an EXCHANGE, and on a
    /// core running several accounts of one exchange it does not say which book the order went to —
    /// so the materializer's `exec_order` seed, and anything reading this journal afterwards, had no
    /// way to attribute a minted order to an account. The account-routing spec's §9 item 12 names
    /// this as a Stage-1 requirement for the same reason it names the twin on
    /// [`Self::MarginCallLiquidate`]: the durable record has to REPRODUCE the live decision rather
    /// than let a reader re-make it from a venue string.
    MintedSubmit {
        seq: u64,
        now_ms: i64,
        req: vike_model::OrderRequest,
        #[serde(default)]
        route_key: Option<String>,
    },
    /// A periodic COMPACT PORTFOLIO OBSERVATION (core-ergonomics): per-venue equity/balance/
    /// realized plus every open position, appended on the opt-in
    /// `vike_core::CoreConfig::portfolio_snapshot_interval` cadence from the drain-loop boundary —
    /// never the per-message fold, and never at all unless that interval is configured.
    ///
    /// It is an OBSERVATION, not a command: like [`Self::MintedSubmit`] it is REPLAY-NEUTRAL —
    /// `replay.rs`'s tail extraction ignores it, so replaying a journal containing these records
    /// re-applies exactly the same command sequence as before and the determinism fence is
    /// unchanged. Unlike [`Self::Snap`] it is NOT a replay base: it carries no engine state,
    /// no coid session and no hash — only the numbers a report or an equity curve needs.
    PortfolioSnap { seq: u64, now_ms: i64, sample: PortfolioSample },
    /// An emulated conditional's RESOLVED terms, journaled from inside `apply_intent` immediately
    /// AFTER the runtime minted its `arm_id` — the `MintedSubmit` pattern applied to the ARM.
    ///
    /// The write-ahead `Cmd`/`StrategySubmit` that carried the `OrderIntent::ArmConditional`
    /// records the caller's INTENT, which for a TRAILING arm is incomplete: the extreme is seeded
    /// from the standing mark at apply time, and marks are market data — deliberately never
    /// journaled. This record is the durable tie between the minted `arm_id` and the terms the
    /// book actually holds (extreme included), so a future replay/restore can re-arm without a
    /// mark. It is REPLAY-NEUTRAL today (`replay.rs`'s tail extraction ignores it): the arm itself
    /// still replays from its own write-ahead record, exactly as before.
    ConditionalArmed { seq: u64, now_ms: i64, arm_id: String, resolved: ConditionalRecord },
    /// An emulated conditional's FIRE — the trigger decision, journaled write-ahead of the
    /// `apply_intent` release it causes (emulator-journal PR-1).
    ///
    /// This closes the determinism residual `replay.rs` documented: the FIRE is a runtime reaction
    /// to a bar/tick, and market data is never journaled, so before this record a session that
    /// fired a conditional could not be replayed (it surfaced as `ReplayError::HashMismatch`).
    /// The tick that caused the decision is still not replayed — the DECISION is, exactly as
    /// [`Self::StrategySubmit`] journals a strategy's decision. `req` is the released order with
    /// its coid still EMPTY (the mint happens inside `apply_intent`, after this record), so replay
    /// re-applies it as `Command::Order(Submit(req))` through the SAME one-write path and re-mints
    /// the identical coid from the restored generator. `trigger_px` is the oracle's diagnostic
    /// trigger price (never a fill promise); `arm_id` names the arm this fire consumed.
    ConditionalFire {
        seq: u64,
        now_ms: i64,
        arm_id: String,
        trigger_px: f64,
        req: vike_model::OrderRequest,
    },
    /// An emulated conditional's DISARM — the arm named by `arm_id` was dropped from the book by
    /// an [`vike_exec::OrderIntent::DisarmConditional`], journaled WRITE-AHEAD of the book
    /// mutation (emulator PR-2; the `ConditionalArmed`/`ConditionalFire` ordering discipline).
    ///
    /// Appended only for an ACTUAL disarm — an unknown/stale id is refused into recent-events
    /// and writes nothing. It completes the arm's record lifecycle (`ConditionalArmed` -> exactly
    /// one of `ConditionalFire`/`ConditionalDisarmed`/still-armed; a `MassCancel`-cleared arm is
    /// visible through that intent's own `Cmd` record instead — the coarse verb stays coarse), so
    /// a reader folding the conditional records can reconstruct the resting book at any point
    /// without re-evaluating a trigger. It is REPLAY-NEUTRAL, exactly like `ConditionalArmed`: the disarm itself replays
    /// from its own write-ahead `Cmd`/`StrategySubmit` record (the intent carries the complete
    /// `arm_id` — unlike a trailing ARM's mark-seeded extreme, nothing here needs resolving), and
    /// `replay.rs`'s tail extraction ignores this record.
    ConditionalDisarmed { seq: u64, now_ms: i64, arm_id: String },
    /// A margin-call auto-liquidation's RELEASED order — the reduce-only MARKET the sweep submits
    /// per breaching position, journaled write-ahead of the `apply_intent` release it causes
    /// (emulator PR-4; the `ConditionalFire` pattern applied to the margin-call sweep).
    ///
    /// This closes the last-but-one determinism residual `replay.rs` documented. The liquidation is
    /// a runtime reaction to ACCOUNT/MARKET state — the sweep runs on a CLOSED BAR against the live
    /// account's marks + equity, and neither the bar nor the equity is journaled — so before this
    /// record a session whose margin call fired could not be replayed (it surfaced as
    /// `ReplayError::HashMismatch`). The breach is NOT re-evaluated on replay (the replay core pumps
    /// no market data, so its equity never moves): the DECISION is the record, exactly as
    /// [`Self::ConditionalFire`] records a fired stop's release rather than the tick that crossed it.
    /// `req` is the released reduce-only MARKET with its coid still EMPTY (the mint happens inside
    /// `apply_intent`, after this record), so replay re-applies it as `Command::Order(Submit(req))`
    /// through the SAME one-write path and re-mints the identical coid from the restored generator —
    /// the very same shape a `ConditionalFire` replays. The follow-on `MintedSubmit` that
    /// `apply_intent` writes for this empty-coid submit ties the minted coid to its request for the
    /// materializer, so (like a fire) this variant needs no materializer handling of its own.
    ///
    /// `mount_id` (journal v14; `#[serde(default)]` ⇒ `None` on every pre-v14 frame) names the mount
    /// this liquidation BELONGS TO. It exists because ONE record carries TWO different releases:
    ///
    /// - `None` — the ACCOUNT-wide margin-call sweep (`vike_core::runtime`'s `sweep_margin_call`). It
    ///   liquidates a POSITION, not a strategy's book; no mount originated it and its fill correctly
    ///   lands in the unattributed residual row.
    /// - `Some(id)` — the PER-MOUNT budget latch's optional flatten (`CoreThread::latch_mount`),
    ///   the reduce-only MARKET that closes exactly ONE mount's attributed net position. That order
    ///   is minted BY a mount and its fill realizes THAT mount's loss.
    ///
    /// Before this field the two were indistinguishable on disk. The live runtime had the flatten
    /// right in memory (`latch_mount` does `coid_mount.insert(coid, i)`), but
    /// `vike_core::replay`'s `fold_coid_mounts` rebuilds attribution FROM THE JOURNAL and saw only an
    /// ownerless liquidation — so it CLOSED ownership and left the `MintedSubmit` that follows
    /// unattributed. After a restart the flatten's fill therefore booked into the RESIDUAL row
    /// instead of the mount whose budget breach released it, and that mount's ledger came back
    /// under-reporting precisely the realized loss the latch exists to bound: a restored core could
    /// re-latch the same mount against a loss it no longer remembered taking.
    ///
    /// `route_key` (journal v16; `#[serde(default)]` ⇒ `None` on every pre-v16 frame) is WHICH
    /// ACCOUNT of `req.venue` the BREACHING engine was — absent meaning *"that venue's sole
    /// account"*. It matters more here than on [`Self::MintedSubmit`] because this record IS
    /// re-applied on replay (`vike_core::replay`'s tail lowers it as `Command::Order(Submit(req))`), so
    /// a reader re-deriving the engine from `req.venue` would re-MAKE the routing decision instead
    /// of reproducing the one the live sweep made — and since the account-routing spec's Stage 1
    /// refuses an account-less risk-increasing intent on a multi-account venue, it would refuse
    /// rather than misroute, breaking replay determinism (§9 item 12).
    ///
    /// ⚠ **Recorded and not yet consumed**: `vike_core::replay::replay_journal` refuses a multi-engine
    /// base outright, so there is no multi-account replay for this key to steer today. It is
    /// written now so that journals being produced from v16 forward carry the answer — the replay
    /// half cannot be built against journals that never had it. `CoreThread::journal_route_key` is
    /// the one producer.
    MarginCallLiquidate {
        seq: u64,
        now_ms: i64,
        req: vike_model::OrderRequest,
        #[serde(default)]
        mount_id: Option<String>,
        #[serde(default)]
        route_key: Option<String>,
    },
    /// A MANAGED GTD/Day EXPIRY the wall-clock sweep decided — one record per order the sweep
    /// actually expires, journaled WRITE-AHEAD of the `cancel_order` it issues (emulator PR-5).
    ///
    /// **This is a DECISION MARKER, not a replayable command — and deliberately so.** Unlike
    /// [`Self::ConditionalFire`] / [`Self::MarginCallLiquidate`], which release an ORDER and
    /// therefore must re-apply through `apply_intent` on replay, the GTD sweep issues
    /// `ExecutionEngine::cancel_order`, which mutates NO local fenced state: it forwards to
    /// `ExecutionClient::cancel` and "publishes NOTHING — the venue stream emits the authoritative
    /// `OrderCanceled` that advances the FSM". That `OrderCanceled` is journaled on its own as an
    /// `Ingest::Event` `Cmd` record and replays independently, so the sweep's contribution to
    /// `state_hash` is exactly NIL. Re-issuing the cancel on replay would be a no-op against
    /// `ReplayClient` (whose `cancel` is an explicit no-op) and an outright DOUBLE-CANCEL against
    /// any future non-inert replay client — so `replay.rs`'s tail extraction ignores this variant,
    /// exactly like [`Self::MintedSubmit`] and [`Self::PortfolioSnap`].
    ///
    /// What the record buys is the thing the sweep actually lacked: an AUDIT of the local decision.
    /// Without it, a reader (or a materializer) cannot tell whether an `OrderCanceled` in the tail
    /// was a venue-side action or this runtime's own clock-driven expiry, and a reviewer cannot
    /// prove that a `gtd_sweep` session's replay reproduced the same set of expiries rather than
    /// silently re-deriving them from a different wall clock. With it, the expiry decision is on
    /// disk before its effect, the sweep never needs re-evaluating on replay, and `gtd_sweep` no
    /// longer has to arm the `Ingest::Watchdog` refusal (`CoreThread::journal_waker_records`) — a
    /// `gtd_sweep`-only session REPLAYS AND CRASH-RESTORES normally as of this variant.
    ///
    /// `coid` is the expiring order's client-order-id; `engine` is the engine index the order was
    /// found in (the sweep's own routing key — recorded so the audit trail survives a multi-engine
    /// core, where a coid alone does not identify the order).
    ///
    /// AUDIT-TRAIL CAVEAT (newly reachable now that a `gtd_sweep` core can crash-restore): the
    /// sweep's fire-once set (`CoreThread::gtd_canceled`) is runtime state — it is in neither
    /// `EngineSnapshot` nor the `Snap` payload, so it does NOT survive a restart. If a snapshot
    /// lands between the cancel and the venue's authoritative `OrderCanceled`, the restored core
    /// sees the order still live and still expired, re-decides it, and emits a SECOND `GtdExpire`
    /// for one logical expiry. That is benign — the order genuinely is still resting and expired,
    /// so re-cancelling is the correct action, `cancel_order` is `is_live`-guarded, and the record
    /// is replay-neutral so no hash can diverge — but a reader counting records must not assume
    /// one marker per expiry across a restart boundary.
    GtdExpire { seq: u64, now_ms: i64, coid: String, engine: usize },
    /// A WALL-CLOCK SCHEDULE FIRE the boundary poll decided (steal/core-live-scheduler) — one record
    /// per `(mount, tag)` the schedule crossed, journaled WRITE-AHEAD of the `Strategy::on_schedule`
    /// call it drives.
    ///
    /// **This is a DECISION MARKER, replay-neutral — exactly like [`Self::GtdExpire`].** The fire
    /// itself moves NO fenced state: the on_schedule hook's only durable effect is the ORDERS it
    /// buffers, and those lower through the SAME `drain_broker` → `apply_intent` path a strategy's
    /// `on_bar` orders take — each journaled on its own as a [`Self::StrategySubmit`] that replay
    /// re-applies. So the fire's contribution to `state_hash` is exactly the StrategySubmit records
    /// it produced (which replay owns), and re-running on_schedule on replay would DOUBLE them —
    /// `replay.rs`'s tail extraction therefore IGNORES this variant (an EXPLICIT replay-neutral
    /// arm — that match has no catch-all; see the "Adding a variant" contract on this enum),
    /// the same treatment [`Self::MintedSubmit`] / [`Self::PortfolioSnap`] / [`Self::GtdExpire`] get.
    ///
    /// What the record buys is the AUDIT of the local decision: without it a reader cannot tell
    /// whether a StrategySubmit came from a bar or from a scheduled recalibration, and a reviewer
    /// cannot prove a schedule session's replay reproduced the same fires. Because on_schedule is
    /// never re-decided on replay (the poll is wall-clock-driven, the replay core pumps no waker),
    /// enabling the schedule does NOT arm the `Ingest::Watchdog` refusal
    /// (`CoreThread::journal_waker_records`) — a schedule-only session REPLAYS AND CRASH-RESTORES
    /// normally, exactly as a `gtd_sweep`-only one does.
    ///
    /// `mount_id` is the `(venue, symbol, interval)` key the mount was registered under (provenance,
    /// mirroring [`Self::StrategySubmit`]); `tag` is the strategy-chosen schedule label passed to
    /// on_schedule.
    ScheduleFire { seq: u64, now_ms: i64, mount_id: String, tag: String },
}

/// One armed conditional captured in a [`JournalRecord::Snap`] (emulator PR-3): the minted
/// `arm_id` (the disarm key, resumed verbatim on restore so a post-restart
/// [`vike_exec::OrderIntent::DisarmConditional`] still finds its arm) plus the terms the book
/// holds — the SAME shape as a [`ConditionalRecord`], except `terms.extreme` here is the CURRENT
/// ratcheted extreme at Snap time, not the arm-time seed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapConditional {
    pub arm_id: String,
    pub terms: ConditionalRecord,
}

/// One resting OTO/OCO contingency leg captured in a [`JournalRecord::Snap`] (live-runtime
/// OCO/OTO): the leg's `coid` plus the linkage the shared [`vike_exec::ContingencyBook`] holds —
/// `parent` (the OTO entry that arms it, `None` for an entry), `linked` (its OCO siblings to cancel
/// when it fills), and `active` (an entry starts active; a held exit stays inactive until its
/// parent's fill arms it).
///
/// `held_request` is `Some` iff the leg is a HELD exit the runtime is keeping OFF the venue until
/// its parent fills — the resolved order to (re-)submit at arm time. It exists ONLY for held legs:
/// an active leg is already in the engine registry (restored via `Snap.engines`), so re-capturing
/// its request would double it. This is the durable home for a held exit — nothing else survives a
/// restart for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapContingency {
    pub coid: String,
    pub parent: Option<String>,
    pub linked: Vec<String>,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_request: Option<vike_model::OrderRequest>,
}

/// One mount's fill-ATTRIBUTION ledger captured in a [`JournalRecord::Snap`] (multi-mount
/// durability, gap D): the mount's identity (`vike_core::strategy_state::mount_id_with`) plus the four
/// numbers the runtime folds from that mount's OWN fills — the same weighted-average-cost fold
/// `Account::fold` runs, scoped to one mount.
///
/// `realized_pnl` is GROSS realized price PnL and `fees_paid` the cumulative commission (a maker
/// rebate is negative), mirroring `Account`'s own split — the published per-mount view nets them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SnapMountAttr {
    pub mount_id: String,
    #[serde(default)]
    pub size: f64,
    #[serde(default)]
    pub avg_px: f64,
    #[serde(default)]
    pub realized_pnl: f64,
    #[serde(default)]
    pub fees_paid: f64,
}

/// The RESOLVED terms of one armed conditional — the [`JournalRecord::ConditionalArmed`] payload
/// (and, via [`SnapConditional`], the per-arm shape a `Snap` carries).
/// `price` = a fixed stop's trigger; `trail` + `extreme` = a trailing stop's distance and the
/// extreme seeded from the standing mark (exactly one of `price`/`trail` is set, mirroring
/// `ConditionalIntent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionalRecord {
    pub venue: String,
    pub symbol: String,
    pub side: i32,
    pub qty: f64,
    pub price: Option<f64>,
    pub trail: Option<f64>,
    pub extreme: Option<f64>,
    /// Requested trigger price source (w2 `trigger_by`): `None`/`Last` = trade/bar lane (the
    /// pre-field law), `Mark` = the mark lane. Additive serde: `default` + `skip_serializing_if`
    /// `None`, so a pre-existing journal reads back as `None` and a `None` write is
    /// byte-identical — the replay fence sees unchanged frames for every source-less session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_by: Option<vike_model::TriggerBy>,
}

/// The [`JournalRecord::PortfolioSnap`] payload — one compact cross-venue portfolio observation.
/// Deliberately small and flat (no engine state, no order registry): this is a time series, so it
/// is written on a cadence and must stay cheap to append and cheap to scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortfolioSample {
    /// wall-clock ms the sample was taken (the boundary clock read that fired the timer).
    pub ts: i64,
    /// one row per engine, primary first then extras in registration order.
    pub venues: Vec<PortfolioVenueSample>,
    /// every OPEN (non-zero-size) position across every engine, in each `Account`'s own
    /// insertion order — the same order the expansion/snapshot paths walk.
    pub positions: Vec<PortfolioPositionSample>,
}

/// One venue row of a [`PortfolioSample`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortfolioVenueSample {
    pub venue: String,
    /// mode-aware, resolver-priced equity at this engine's own seed
    /// (`ExecutionEngine::resolved_equity` under `CoreConfig::price_cfg`) — the SAME source the
    /// equity sampler and `CoreSnapshot` use (the one-price law).
    pub equity: f64,
    pub balance: f64,
    pub realized_pnl: f64,
}

/// One open-position row of a [`PortfolioSample`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortfolioPositionSample {
    pub venue: String,
    pub symbol: String,
    pub position_side: String,
    pub size: f64,
    pub avg_px: f64,
}

/// Borrowed write-twin of [`JournalRecord`] — identical variant/field names ⇒ byte-identical
/// externally-tagged JSON, so `read_all` deserializes these back as owned `JournalRecord`.
/// Exists so appends serialize the hot-path `Ingest` and the snapshot `engines` slice BY
/// REFERENCE (no clone on the fold path).
#[derive(Serialize)]
pub(super) enum JournalRecordRef<'a> {
    Cmd {
        seq: u64,
        now_ms: i64,
        msg: &'a Ingest,
    },
    Snap {
        seq: u64,
        now_ms: i64,
        engines: &'a [vike_exec::EngineSnapshot],
        coid_session: &'a str,
        coid_seq: u64,
        // plain u64 (the writer ALWAYS stamps it); the owned twin is `Option<u64>` purely so a
        // pre-v7 frame's absence is distinguishable from a genuine 0 — `Some(n)` and `n`
        // serialize identically, so the write/read twins still match byte-for-byte.
        arm_seq: u64,
        // always stamped (empty slice for empty books); the owned twin's `#[serde(default)]`
        // exists only so a pre-v8 frame with NO such key reads back as empty.
        conditionals: &'a [SnapConditional],
        // always stamped (empty slice for no contingency legs); the owned twin's `#[serde(default)]`
        // exists only so a pre-v11 frame with NO such key reads back as empty.
        contingencies: &'a [SnapContingency],
        // always stamped (empty slice for a mount-free core); the owned twin's `#[serde(default)]`
        // exists only so a pre-v13 frame with NO such key reads back as empty.
        mount_attr: &'a [SnapMountAttr],
        hash: u64,
    },
    StrategySubmit {
        seq: u64,
        now_ms: i64,
        mount_id: &'a str,
        intent: &'a vike_exec::OrderIntent,
    },
    MintedSubmit {
        seq: u64,
        now_ms: i64,
        req: &'a vike_model::OrderRequest,
        #[serde(skip_serializing_if = "Option::is_none")]
        route_key: Option<&'a str>,
    },
    PortfolioSnap {
        seq: u64,
        now_ms: i64,
        sample: &'a PortfolioSample,
    },
    ConditionalArmed {
        seq: u64,
        now_ms: i64,
        arm_id: &'a str,
        resolved: &'a ConditionalRecord,
    },
    ConditionalFire {
        seq: u64,
        now_ms: i64,
        arm_id: &'a str,
        trigger_px: f64,
        req: &'a vike_model::OrderRequest,
    },
    ConditionalDisarmed {
        seq: u64,
        now_ms: i64,
        arm_id: &'a str,
    },
    MarginCallLiquidate {
        seq: u64,
        now_ms: i64,
        req: &'a vike_model::OrderRequest,
        // always stamped (`null` for the ACCOUNT-wide margin-call sweep, which no mount owns); the
        // owned twin's `#[serde(default)]` exists only so a pre-v14 frame with NO such key reads
        // back as `None`.
        mount_id: Option<&'a str>,
        // ⚠ SKIPPED when absent, unlike `mount_id` directly above, and the asymmetry is
        // deliberate. `mount_id` has been written on every frame since v14, so emitting `null`
        // costs nothing; this field is `None` on every single-account box, which is every box in
        // production, and emitting `"route_key": null` on each of their liquidation frames would
        // change their journal BYTES for a fact that was already true. Same rule and same reason as
        // `vike_exec::ReconcileReports::route_key`'s own `skip_serializing_if`.
        #[serde(skip_serializing_if = "Option::is_none")]
        route_key: Option<&'a str>,
    },
    GtdExpire {
        seq: u64,
        now_ms: i64,
        coid: &'a str,
        engine: usize,
    },
    ScheduleFire {
        seq: u64,
        now_ms: i64,
        mount_id: &'a str,
        tag: &'a str,
    },
}

/// The globally-monotonic `seq` carried by any [`JournalRecord`] variant. `pub`, and re-exported at
/// the crate root, because `vike_core::replay` needs it for the pre-v7 `arm_seq` fallback bound
/// (max global seq + 1); [`crate::read_since`] and the materializer here use it too. (This said
/// "`pub(crate)` because `crate::replay` needs it" until 2026-09-28 — both halves from before the
/// journal left vike-core.)
pub fn record_seq(rec: &JournalRecord) -> u64 {
    match rec {
        JournalRecord::Cmd { seq, .. }
        | JournalRecord::Snap { seq, .. }
        | JournalRecord::StrategySubmit { seq, .. }
        | JournalRecord::MintedSubmit { seq, .. }
        | JournalRecord::PortfolioSnap { seq, .. }
        | JournalRecord::ConditionalArmed { seq, .. }
        | JournalRecord::ConditionalFire { seq, .. }
        | JournalRecord::ConditionalDisarmed { seq, .. }
        | JournalRecord::MarginCallLiquidate { seq, .. }
        | JournalRecord::GtdExpire { seq, .. }
        | JournalRecord::ScheduleFire { seq, .. } => *seq,
    }
}

#[path = "record_tests.rs"]
#[cfg(test)]
mod record_tests;

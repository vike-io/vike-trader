//! The journal's serde SCHEMA: [`JournalRecord`] (the externally-tagged wire enum every frame
//! carries), its payload structs, the borrowed write twin [`JournalRecordRef`], and [`record_sequence`].
//!
//! Split out of `journal.rs` verbatim — the version-compatibility contract that governs adding a
//! variant here lives in the parent module's doc, and [`JournalRecord`]'s own "Adding a variant"
//! table is the checklist.

use serde::{Deserialize, Serialize};
use vike_execution::Ingest;

/// One frame of the write-ahead command journal.
///
/// # Adding a variant — the no-silent-drop contract
///
/// **Every variant must be EXPLICITLY NAMED by the replay fold and the execution stream's publisher. A
/// new variant is a COMPILE ERROR at each of them, never a silent drop.** Three independent places
/// interpret this enum — the writer here, `vike_core::replay`'s fold (the crate ABOVE this one), and
/// the tradehub's execution-stream publisher, which decides what the datahub stores
/// (`crates/vike-tradehub/src/execution_stream/read.rs`'s `body_of`; the journal materializer held that
/// role until 2026-11-18, decision 0295) — and both of the FOLD sites used to end in a `_ =>`
/// catch-all
/// (the writer's own [`record_sequence`] was already exhaustive, which is exactly why it never caught
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
/// | [`record_sequence`] (this file) | which field carries the global seq |
/// <!-- ⚠ A second row read "the materializer's own `record_sequence` | the same, in the
/// materializer's crate". That site is GONE (2026-09-25): its private copy was deleted for this one,
/// and the materializer called this until it was deleted (2026-11-18). One fewer decision. -->
/// | `vike_core::replay`'s tail extraction (`replay_from`'s `replay_tail`) | RE-APPLY on replay, or replay-neutral? |
/// | `crates/vike-journal/src/order_client_id_mounts.rs`'s `JournalOrderClientIdMountFold::fold_record` (this crate; vike-core's restore folds it) | does it OPEN / CLOSE / not touch order-client-id->mount ownership? |
/// | `vike_core::replay::fold_conditionals` | does it change conditional-book membership? |
/// | `crates/vike-tradehub/src/execution_stream/read.rs`'s `body_of` | is it sent to the datahub (an execution, a forecast), or is it no execution? |
///
/// Answering "nothing" is a perfectly good answer — write `JournalRecord::Foo { .. } => {}` with a
/// one-line reason, exactly as every already-ignored variant does there. The point is that the
/// answer is RECORDED rather than defaulted.
///
/// **Four things still will NOT break, so check them by hand:**
/// 1. [`JournalRecordRef`], the borrowed write twin — its variant/field names must match EXACTLY or
///    the frame will not deserialize back; add the mirror variant and an `append_*` method.
/// 2. [`super::format::VERSION`] — bump it, and extend the [`super::format::MIN_READABLE_VERSION`] rationale (a
///    purely additive variant leaves `MIN_READABLE_VERSION` where it is; see the module doc's
///    compatibility contract for when it must be raised instead).
/// 3. Single-variant LOOKUPS, deliberately left as catch-alls because they ask "is this THE `Snapshot`?"
///    (or "is this a fill?") rather than dispatching, and so are correct for any future variant by
///    construction: `replay_from`'s base-`Snapshot` `position`/`rposition` and `read_exit_snap`'s exit-`Snapshot`
///    `find_map`, `replay_offline`'s source-`Snapshot` `find_map`, `vike_tradehub_core::incident`'s
///    latest-`Snapshot` `find_map`, `vike_journal::fills_from_journal`, and the boot restore's
///    `latest_snap` and `tail_facts` (a `MintedSubmit`, and a `Command` carrying a terminal lifecycle
///    event).
///    ⚠ The lookups that ask "is this record a venue EVENT?" (`fills_from_journal`, `tail_facts`, the
///    publisher's foreign-execution read) must ask `vike_execution::Ingest::venue_event`, which
///    answers for `Ingest::Event` AND `Ingest::GatewayEvent` (journal v18); a hand-written
///    `Ingest::Event(..)` pattern there silently drops every event that came over a gateway.
/// 4. `crates/vike-tradehub/src/execution_stream/read.rs`'s `foreign_execution` reads a fill that names no
///    order out of a [`Self::Command`] alone, which no exhaustiveness check over this enum can reach. A new
///    variant able to carry a `vike_model::events::FillEvent` with no order must be added there explicitly.
#[derive(Debug, Serialize, Deserialize)]
pub enum JournalRecord {
    /// One exec-lane ingest message, with the wall-clock the core stamped for its dispatch.
    Command { sequence: u64, now_ms: i64, message: Ingest },
    /// Full-state snapshot-as-command: replay starts at the LATEST of these.
    Snapshot {
        sequence: u64,
        now_ms: i64,
        engines: Vec<vike_execution::EngineSnapshot>,
        order_client_id_session: String,
        order_client_id_sequence: u64,
        /// The emulated-conditional arm-id counter at Snapshot time — the arm-side twin of
        /// `order_client_id_sequence`, stamped for the same reason: a restart resumes `order_client_id_session`, so the
        /// counter behind `{order_client_id_session}a{arm_sequence}` must resume too or post-restart arms
        /// re-emit spent ids (and `arm_id` is the DISARM key since emulator PR-2, so an
        /// ambiguous id would disarm the wrong arm).
        ///
        /// `Option` + `#[serde(default)]` is the v6->v7 compat seam: a pre-v7 `Snapshot` frame has
        /// no such field and reads back `None` — meaning ABSENT, never "0" — and the restore
        /// path falls back to a prune-safe bound (max global record seq + 1, see
        /// `vike_core::replay`). Counting currently-READABLE records, the pre-v7 formula, is
        /// prune-UNSAFE: `prune_before_latest_snap` moves whole early segments to the archive, so a restart
        /// after a prune undercounted the ids already spent and re-minted duplicates.
        #[serde(default)]
        arm_sequence: Option<u64>,
        /// The RESTING emulated-conditional books at Snapshot time (emulator PR-3, re-arm-on-restore):
        /// one entry per armed conditional, in FIRE order (the books map's insertion order, then
        /// each book's own insertion order), each carrying its minted `arm_id` + the terms the
        /// book actually holds — for a TRAILING arm that includes the CURRENT ratcheted extreme,
        /// which is live state (it moves on every non-firing check against non-journaled market
        /// data) and therefore exists NOWHERE else: the `OrderEmulationArmed` record only captured
        /// the seed. Snapping the book is what lets a restart re-arm a protective stop instead of
        /// silently dropping it.
        ///
        /// `#[serde(default)]` is the v7->v8 compat seam: a pre-v8 `Snapshot` frame has no such field
        /// and reads back EMPTY — a pre-v8 journal restores with empty books (exactly its
        /// pre-PR-3 behavior), never an error. Deliberately NOT part of `state_hash` (which stays
        /// over the engine snapshots): book contents can only move fenced state through a FIRE,
        /// and every fire journals its own write-ahead [`Self::OrderEmulationFire`] record — see
        /// `vike_core::replay`'s module doc for the full fence argument.
        #[serde(default)]
        order_emulation_arms: Vec<JournalSnapshotOrderEmulationArm>,
        /// The RESTING OTO/OCO contingency book at Snapshot time (live-runtime OCO/OTO): one entry per
        /// linked order — entries AND their held/armed exits — carrying the linkage the shared
        /// [`vike_execution::ContingencyBook`] holds (`parent`/`linked`/`active`) plus, for a HELD exit,
        /// the resolved [`vike_model::OrderRequest`] the runtime is holding back from the venue
        /// until its parent fills. It is the durable home for that held request: unlike an ACTIVE
        /// leg (already in the engine registry, so restored via `engines` above), a held exit lives
        /// ONLY in the runtime's side book and would be LOST on a restart — leaving a filled entry
        /// with no protective stop/take-profit — without this capture.
        ///
        /// `#[serde(default)]` is the v10->v11 compat seam: a pre-v11 `Snapshot` frame has no such field
        /// and reads back EMPTY (a pre-OCO/OTO journal restores with no contingency state, exactly
        /// its prior behavior, never an error). Deliberately NOT part of `state_hash` (which stays
        /// over the engine snapshots): the book moves fenced state only through a FILL that releases
        /// a held child / cancels a sibling, and every such fill is itself a journaled `Command` event
        /// that a replay re-folds through the re-seeded book — see `vike_core::replay`.
        #[serde(default)]
        contingencies: Vec<JournalSnapshotContingency>,
        /// The per-mount fill ATTRIBUTION ledgers at Snapshot time (multi-mount durability, gap D): one
        /// row per mount slot — its `mount_id` plus the position/avg-px/realized/fees folded from
        /// exactly that mount's own fills. It is the ONLY durable home for them: attribution lives
        /// one layer up from `Account` (deliberately — `Account` is parity-gated and its snapshot
        /// wire is byte-identical-pinned), so nothing in `engines` carries it and a restart used to
        /// resume with every mount ledger silently back at zero while `Account.realized_pnl` kept
        /// the same PnL. Keyed by `mount_id` rather than by index so a re-ordered mount list
        /// restores onto the right mounts (an id that names no mounted slot is simply skipped).
        ///
        /// `#[serde(default)]` is the v12->v13 compat seam: a pre-v13 `Snapshot` frame has no such field
        /// and reads back EMPTY (a pre-feature journal restores with zeroed ledgers — exactly its
        /// prior behavior, never an error). Deliberately NOT part of `state_hash` (which stays over
        /// the engine snapshots), for the same reason `conditionals`/`contingencies` are not: the
        /// ledgers are a read-only VIEW folded from fills that are themselves journaled `Command`
        /// events — they drive no fold decision and can move no fenced state.
        #[serde(default)]
        mount_attribution: Vec<JournalSnapshotMountAttribution>,
        /// WHERE each gateway's per-account event ring stood at Snapshot time (journal v18): one
        /// `(route key, cursor)` pair per account the core has received a
        /// [`vike_execution::Ingest::GatewayEvent`] for, the cursor being the one the NEXT
        /// `TradeOpen.resume` asks the gateway to continue from.
        ///
        /// It rides the Snapshot for the same reason the ledgers above do: `prune_before_latest_snap`
        /// moves whole early segments to the archive, and an account that was quiet since the last `Snapshot` has no
        /// `GatewayEvent` record left in the surviving tail to read its cursor from. Without the
        /// field such an account would restore with NO cursor and fall back to a reconcile it did
        /// not need. A Full boot restore takes, per route key, the cursor of the LAST `GatewayEvent`
        /// in the tail, else this one (a snap-only restore takes this one: `vike_core::replay::fold_gateway_cursors`).
        ///
        /// `#[serde(default)]` is the v17->v18 compat seam: a pre-v18 `Snapshot` reads back EMPTY, and
        /// the write twin omits an empty list, so a run with no gateway events writes the `Snapshot`
        /// bytes it always did. NOT part of `state_hash` (which stays over the engine snapshots):
        /// a cursor is not engine state and moves nothing the replay fence compares.
        #[serde(default)]
        gateway_cursors: Vec<(String, vike_model::EventRingCursor)>,
        /// The orders the live core HOLDS until their release (`vike_model::OrderRelease`) at Snapshot time,
        /// in the order they were held: the only durable home for one, because it is in no engine
        /// registry and not at the venue until its release, so a restart without this key would
        /// lose it in silence. See [`OrderHeldRelease`].
        ///
        /// `#[serde(default)]` reads a `Snapshot` written before the key back EMPTY, and the write twin
        /// omits an empty list, so a core that holds nothing writes the `Snapshot` bytes it always did.
        /// NOT a format step: a build from before the key ignores it (the struct variant takes no
        /// `deny_unknown_fields`) and restores without the held orders, which then are never sent;
        /// the rollback runbook says to cancel them first. NOT in `state_hash`: a held order moves
        /// fenced state only at its release, which is journaled as its own write-ahead `Command`.
        /// `skip_serializing_if` here too, so a record read back and serialized again (a digest, a
        /// rewrite) is the bytes it was.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        held_releases: Vec<OrderHeldRelease>,
        /// The node's LATE-RETRY memory at Snapshot time (decision 0270, D7): `(order client id, created_ms)` of
        /// every id the node saw within the guard's window, so a retry of an ended order's id after a restart
        /// can be refused (the engine registries start EMPTY at boot). The boot restore builds it
        /// (`vike_core::replay`) and the core carries it (`vike_core`'s `CoreConfig::recent_order_client_ids`).
        ///
        /// `#[serde(default)]` reads a `Snapshot` written before the key back EMPTY, and the write twin omits an
        /// empty list, so a core with no memory writes the `Snapshot` bytes it always did. NOT a format step: a
        /// build from before the key ignores it and restores with no memory, which is what every restart did
        /// before it. NOT in `state_hash`, like the side books above.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        recent_order_client_ids: Vec<(String, i64)>,
        /// The EMULATED orders the live core works at Snapshot time (decision 0275), in the order they
        /// were accepted: the parent is in no registry and at no venue, so this is its only durable
        /// home. See [`JournalSnapshotOrderEmulation`]. `#[serde(default)]` reads a `Snapshot` from before the key back
        /// EMPTY and the write twin omits an empty list, so a core that emulates nothing writes the
        /// bytes it always did; a `Snapshot` that holds one needs `crate::format`'s `EMULATED_VERSION`.
        /// NOT in `state_hash`: a parent moves fenced state only through its children, each its own
        /// write-ahead `Command`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        emulated: Vec<JournalSnapshotOrderEmulation>,
        /// Every mount's CLOSED-TRADE ledger at Snapshot time (decision 0271 item 8, `vike_core`'s
        /// `runtime/closed_trades.rs`): per live mount, how far the closed-trade archive holds its rows
        /// (`crate::TradeClosedArchive`), the rows after that, its open lots exactly, and the bar clock its next
        /// fill's bar index counts from ([`JournalSnapshotClosedTrades`]). With the archive, the ONLY place a ledger's
        /// state is kept: the fills after the `Snapshot` are journaled as fills already, so a restart reads the
        /// archive, adds these rows, resumes the ledger and books each tail fill again through the same call
        /// (`vike_core::replay`'s `closed_tail`), and no fill can be folded without being booked.
        ///
        /// `#[serde(default)]` reads a `Snapshot` written before the key back EMPTY, and the write twin
        /// omits an empty list, so a mount-less core writes the `Snapshot` bytes it always did. NOT a format
        /// step: a build from before the key ignores it and restores a mount's position as one lot at its
        /// average price, which is what every restart did before. NOT in `state_hash`: a ledger is a view
        /// of fills that are journaled on their own.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        closed_trades: Vec<JournalSnapshotClosedTrades>,
        /// The journaled engines of the accounts that WAIT for their venue (owner 2026-11-15: a box whose accounts
        /// did not connect at the boot starts without them, and each joins when its venue connects): the book the
        /// boot restore read for an account the core runs no engine for yet, carried here so no `Snapshot` drops it,
        /// and restored onto the account's engine when it joins, as the boot would have (`vike_core`'s
        /// `runtime/late_join.rs`). NOT in `engines`: the refold builds its replay core from `engines`, and an
        /// account that does not run there must not route a record. `#[serde(default)]` reads a `Snapshot` written
        /// before the key back EMPTY, and the write twin omits an empty list, so a core whose accounts all run writes
        /// the bytes it always did. NOT a format step: a build from before the key ignores it and skips such a book
        /// at its boot, which is what every boot did before. NOT in `state_hash`: it is no engine of this core.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        awaiting_engines: Vec<vike_execution::EngineSnapshot>,
        /// The MOUNT of each order this `Snapshot` holds that a mount minted (decision 0304): `(order client id, mount
        /// id)` for every order of its engines' registries and its contingency book that the live core's ownership map
        /// (`vike_core`'s `CoreThread::order_client_id_mount`) names, so a boot that reads from this `Snapshot` on knows
        /// an order's owner without the `StrategySubmit` and `MintedSubmit` below it (a v1 id carries no strategy code to
        /// read one from). `#[serde(default)]` reads a `Snapshot` written before the key back EMPTY, and the write twin
        /// omits an empty list, so a mount-less core writes the bytes it always did. NOT a format step: a build from
        /// before the key ignores it and folds the owners from the records, as it always did. NOT in `state_hash`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        order_client_id_mounts: Vec<(String, String)>,
        /// This install's PAST order client id sessions as the core holds them (`vike_core`'s
        /// `CoreConfig::order_client_id_past_sessions`: `vike.db`'s rows and the session it switched from), `(session,
        /// last sequence)` (decision 0304): a boot that reads from this `Snapshot` on hands them to its replay cores, so a
        /// late fill on an id of a session only an OLDER `Snapshot` named regenerates as ours. The same compat as
        /// `order_client_id_mounts`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        order_client_id_past_sessions: Vec<(String, u64)>,
        hash: u64,
    },
    /// A mounted strategy's order intent, journaled write-ahead at the `drain_broker` boundary —
    /// the choke point where a strategy's buffered intents lower into `apply_intent` (PR-5). Today
    /// these bypass the journal entirely; this variant is the record of them. `mount_id` is
    /// provenance only (the `(venue, symbol, interval)` key the mount was registered under);
    /// replay re-derives order client ids from `intent` alone, same as a `Command`-carried `Command::Order`.
    ///
    /// `route_key` is the ROUTE KEY of the engine the mount lowers the intent onto, written only by a
    /// core whose engines cannot be told apart by an order's venue (a labelled account, or two
    /// engines of one exchange: `vike_core`'s `CoreThread::journal_replay_route_key`) and absent on
    /// every other core, so a single-account box writes the bytes it always did. A replay of such a
    /// core reads it to lower the intent onto that engine (`vike_execution::Command::OrderOn`); without it
    /// the payload's venue names the default account or none. `#[serde(default)]` reads every frame
    /// written before the field as `None`. Not a format step: a build from before the field ignores
    /// the key and refolds exactly what it refolded then (nothing, for such a core: it restores the
    /// latest `Snapshot` alone).
    StrategySubmit {
        sequence: u64,
        now_ms: i64,
        mount_id: String,
        intent: vike_execution::OrderIntent,
        #[serde(default)]
        route_key: Option<String>,
    },
    /// A server-minted submit's RESOLVED [`vike_model::OrderRequest`] — journaled from inside
    /// `apply_intent` immediately AFTER the order client id is minted, for a submit whose incoming
    /// `order_client_id` was EMPTY. The write-ahead `Command`-carried `Command::Order(Submit)` recorded
    /// that same request with the order client id still empty (the mint happens in the fold, after that
    /// record), so this variant is the durable tie between a server-minted order client id and its
    /// `(venue, symbol, side, qty, type, price)` — what a reader of this journal needs for an order that
    /// terminalizes WITHOUT ever filling (the minted-order-client-id gap, first met by the journal
    /// materializer). It is
    /// REPLAY-NEUTRAL: replay re-mints from the empty-order-client-id `Command` exactly as before and simply
    /// re-emits this record into its temp journal, so the determinism fence is unchanged (the tail
    /// extraction in `replay.rs` ignores this variant — it is not a command to re-apply). The one
    /// exception is an amend's replacement, which names what it `replaces` (below).
    ///
    /// `route_key` (journal v16; `#[serde(default)]` ⇒ `None` on every pre-v16 frame) is WHICH
    /// ACCOUNT of `req.venue` this submit was lowered onto — absent meaning *"that venue's sole
    /// account"*, exactly `vike_execution::ReconcileReports::route_key`'s convention, so a
    /// single-account box writes no key and its journal bytes are identical to the ones written
    /// before the field existed.
    ///
    /// ⚠ **Why an OBSERVATION record carries a routing fact.** `req.venue` is an EXCHANGE, and on a
    /// core running several accounts of one exchange it does not say which book the order went to —
    /// so anything reading this journal afterwards had no
    /// way to attribute a minted order to an account. The account-routing spec's §9 item 12 names
    /// this as a Stage-1 requirement for the same reason it names the twin on
    /// [`Self::MarginCallLiquidate`]: the durable record has to REPRODUCE the live decision rather
    /// than let a reader re-make it from a venue string.
    MintedSubmit {
        sequence: u64,
        now_ms: i64,
        request: vike_model::OrderRequest,
        #[serde(default)]
        route_key: Option<String>,
        /// The order client id of the order this mint REPLACES (a cancel-then-new amend's new order), absent on
        /// every other mint. A restore gives the replacement its predecessor's owner
        /// (`crates/vike-journal/src/order_client_id_mounts.rs`'s `JournalOrderClientIdMountFold`), and a replay places a cancel-first replacement
        /// HERE, the one record of where the live core placed it (`vike_core`'s `replay_tail`). Not a
        /// step: a build from before the key ignores it and leaves the order unattributed, which is all
        /// it could do for a replacement.
        #[serde(default)]
        replaces: Option<String>,
    },
    /// A periodic COMPACT PORTFOLIO OBSERVATION (core-ergonomics): per-venue equity/balance/
    /// realized plus every open position, appended on the opt-in
    /// `vike_core::CoreConfig::portfolio_snapshot_interval` cadence from the drain-loop boundary —
    /// never the per-message fold, and never at all unless that interval is configured.
    ///
    /// It is an OBSERVATION, not a command: like [`Self::MintedSubmit`] it is REPLAY-NEUTRAL —
    /// `replay.rs`'s tail extraction ignores it, so replaying a journal containing these records
    /// re-applies exactly the same command sequence as before and the determinism fence is
    /// unchanged. Unlike [`Self::Snapshot`] it is NOT a replay base: it carries no engine state,
    /// no order client id session and no hash — only the numbers a report or an equity curve needs.
    PortfolioSnapshot { sequence: u64, now_ms: i64, sample: PortfolioSample },
    /// An emulated conditional's RESOLVED terms, journaled from inside `apply_intent` immediately
    /// AFTER the runtime minted its `arm_id` — the `MintedSubmit` pattern applied to the ARM.
    ///
    /// The write-ahead `Command`/`StrategySubmit` that carried the `OrderIntent::ArmConditional`
    /// records the caller's INTENT, which for a TRAILING arm is incomplete: the extreme is seeded
    /// from the standing mark at apply time, and marks are market data — deliberately never
    /// journaled. This record is the durable tie between the minted `arm_id` and the terms the
    /// book actually holds (extreme included), so a future replay/restore can re-arm without a
    /// mark. It is REPLAY-NEUTRAL today (`replay.rs`'s tail extraction ignores it): the arm itself
    /// still replays from its own write-ahead record, exactly as before.
    OrderEmulationArmed {
        sequence: u64,
        now_ms: i64,
        arm_id: String,
        resolved: OrderEmulationRecord,
    },
    /// An emulated conditional's FIRE — the trigger decision, journaled write-ahead of the
    /// `apply_intent` release it causes (emulator-journal PR-1).
    ///
    /// This closes the determinism residual `replay.rs` documented: the FIRE is a runtime reaction
    /// to a bar/tick, and market data is never journaled, so before this record a session that
    /// fired a conditional could not be replayed (it surfaced as `JournalReplayError::HashMismatch`).
    /// The tick that caused the decision is still not replayed — the DECISION is, exactly as
    /// [`Self::StrategySubmit`] journals a strategy's decision. `req` is the released order with
    /// its order client id still EMPTY (the mint happens inside `apply_intent`, after this record), so replay
    /// re-applies it as `Command::Order(Submit(req))` through the SAME one-write path and re-mints
    /// the identical order client id from the restored generator. `trigger_price` is the oracle's diagnostic
    /// trigger price (never a fill promise); `arm_id` names the arm this fire consumed.
    ///
    /// `route_key` is [`Self::StrategySubmit`]'s: the engine the arm fires onto (`OrderEmulationBook::cond_engine`),
    /// written only by a core whose engines an order's venue cannot tell apart. `None` for the arm
    /// the core recorded no engine for (a book seeded from a restored `Snapshot`), which fires through
    /// the payload route live and replays through it.
    OrderEmulationFire {
        sequence: u64,
        now_ms: i64,
        arm_id: String,
        trigger_price: f64,
        request: vike_model::OrderRequest,
        #[serde(default)]
        route_key: Option<String>,
    },
    /// An emulated conditional's DISARM — the arm named by `arm_id` was dropped from the book by
    /// an [`vike_execution::OrderIntent::DisarmConditional`], journaled WRITE-AHEAD of the book
    /// mutation (emulator PR-2; the `OrderEmulationArmed`/`OrderEmulationFire` ordering discipline).
    ///
    /// Appended only for an ACTUAL disarm — an unknown/stale id is refused into recent-events
    /// and writes nothing. It completes the arm's record lifecycle (`OrderEmulationArmed` -> exactly
    /// one of `OrderEmulationFire`/`OrderEmulationDisarmed`/still-armed; a `MassCancel`-cleared arm is
    /// visible through that intent's own `Command` record instead — the coarse verb stays coarse), so
    /// a reader folding the conditional records can reconstruct the resting book at any point
    /// without re-evaluating a trigger. It is REPLAY-NEUTRAL, exactly like `OrderEmulationArmed`: the disarm itself replays
    /// from its own write-ahead `Command`/`StrategySubmit` record (the intent carries the complete
    /// `arm_id` — unlike a trailing ARM's mark-seeded extreme, nothing here needs resolving), and
    /// `replay.rs`'s tail extraction ignores this record.
    OrderEmulationDisarmed { sequence: u64, now_ms: i64, arm_id: String },
    /// A margin-call auto-liquidation's RELEASED order — the reduce-only MARKET the sweep submits
    /// per breaching position, journaled write-ahead of the `apply_intent` release it causes
    /// (emulator PR-4; the `OrderEmulationFire` pattern applied to the margin-call sweep).
    ///
    /// This closes the last-but-one determinism residual `replay.rs` documented. The liquidation is
    /// a runtime reaction to ACCOUNT/MARKET state — the sweep runs on a CLOSED BAR against the live
    /// account's marks + equity, and neither the bar nor the equity is journaled — so before this
    /// record a session whose margin call fired could not be replayed (it surfaced as
    /// `JournalReplayError::HashMismatch`). The breach is NOT re-evaluated on replay (the replay core pumps
    /// no market data, so its equity never moves): the DECISION is the record, exactly as
    /// [`Self::OrderEmulationFire`] records a fired stop's release rather than the tick that crossed it.
    /// `req` is the released reduce-only MARKET with its order client id still EMPTY (the mint happens inside
    /// `apply_intent`, after this record), so replay re-applies it as `Command::Order(Submit(req))`
    /// through the SAME one-write path and re-mints the identical order client id from the restored generator —
    /// the very same shape a `OrderEmulationFire` replays. The follow-on `MintedSubmit` that
    /// `apply_intent` writes for this empty-order-client-id submit ties the minted order client id to its request for a
    /// reader of the journal, so (like a fire) this variant needs no reader handling of its own.
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
    /// right in memory (`latch_mount` does `order_client_id_mount.insert(order client id, i)`), but
    /// `vike_core::replay`'s `fold_order_client_id_mounts` rebuilds attribution FROM THE JOURNAL and saw only an
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
    /// Consumed by the boot restore's refold of a base whose engines an order's venue cannot tell
    /// apart (`vike_core::replay`'s tail extraction lowers the release as `Command::OrderOn`); the
    /// two offline verifiers still refuse a multi-engine base. `CoreThread::journal_route_key` is
    /// the one producer: `None` is the payload venue's sole/default account.
    MarginCallLiquidate {
        sequence: u64,
        now_ms: i64,
        request: vike_model::OrderRequest,
        #[serde(default)]
        mount_id: Option<String>,
        #[serde(default)]
        route_key: Option<String>,
    },
    /// A MANAGED GTD/Day EXPIRY the wall-clock sweep decided — one record per order the sweep
    /// actually expires, journaled WRITE-AHEAD of the `cancel_order` it issues (emulator PR-5).
    ///
    /// **This is a DECISION MARKER, not a replayable command — and deliberately so.** Unlike
    /// [`Self::OrderEmulationFire`] / [`Self::MarginCallLiquidate`], which release an ORDER and
    /// therefore must re-apply through `apply_intent` on replay, the GTD sweep issues
    /// `Engine::cancel_order`, which mutates NO local fenced state: it forwards to
    /// `ExecutionClient::cancel` and "publishes NOTHING — the venue stream emits the authoritative
    /// `OrderCanceled` that advances the FSM". That `OrderCanceled` is journaled on its own as an
    /// `Ingest::Event` `Command` record and replays independently, so the sweep's contribution to
    /// `state_hash` is exactly NIL. Re-issuing the cancel on replay would be a no-op against
    /// `ExecutionClientJournalReplay` (whose `cancel` is an explicit no-op) and an outright DOUBLE-CANCEL against
    /// any future non-inert replay client — so `replay.rs`'s tail extraction ignores this variant,
    /// exactly like [`Self::MintedSubmit`] and [`Self::PortfolioSnapshot`].
    ///
    /// What the record buys is the thing the sweep actually lacked: an AUDIT of the local decision.
    /// Without it, a reader cannot tell whether an `OrderCanceled` in the tail
    /// was a venue-side action or this runtime's own clock-driven expiry, and a reviewer cannot
    /// prove that a `gtd_sweep` session's replay reproduced the same set of expiries rather than
    /// silently re-deriving them from a different wall clock. With it, the expiry decision is on
    /// disk before its effect, the sweep never needs re-evaluating on replay, and `gtd_sweep` no
    /// longer has to arm the `Ingest::Watchdog` refusal (`CoreThread::journal_waker_records`) — a
    /// `gtd_sweep`-only session REPLAYS AND CRASH-RESTORES normally as of this variant.
    ///
    /// `order_client_id` is the expiring order's order-client-id; `engine` is the engine index the order was
    /// found in (the sweep's own routing key — recorded so the audit trail survives a multi-engine
    /// core, where an order client id alone does not identify the order).
    ///
    /// AUDIT-TRAIL CAVEAT (newly reachable now that a `gtd_sweep` core can crash-restore): the
    /// sweep's fire-once set (`CoreThread::gtd_canceled`) is runtime state — it is in neither
    /// `EngineSnapshot` nor the `Snapshot` payload, so it does NOT survive a restart. If a snapshot
    /// lands between the cancel and the venue's authoritative `OrderCanceled`, the restored core
    /// sees the order still live and still expired, re-decides it, and emits a SECOND `GtdExpire`
    /// for one logical expiry. That is benign — the order genuinely is still resting and expired,
    /// so re-cancelling is the correct action, `cancel_order` is `is_live`-guarded, and the record
    /// is replay-neutral so no hash can diverge — but a reader counting records must not assume
    /// one marker per expiry across a restart boundary.
    GtdExpire { sequence: u64, now_ms: i64, order_client_id: String, engine: usize },
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
    /// the same treatment [`Self::MintedSubmit`] / [`Self::PortfolioSnapshot`] / [`Self::GtdExpire`] get.
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
    ScheduleFire { sequence: u64, now_ms: i64, mount_id: String, tag: String },
    /// A SHARED CLOSE whose grace ran out (decision 0271, its limit (a) closed): mount `mount_id`'s
    /// decision at `close_ms` waited for the bar of the input `input` that closes at the same instant,
    /// and it had not arrived within `vike_core`'s `SHARED_CLOSE_GRACE_MS`, so the decision went ahead on
    /// the input's bar before it. Journaled write-ahead of the strategy step it releases, at the
    /// boundary's clock (`now_ms`, when the grace fired).
    ///
    /// **A DECISION MARKER, replay-neutral, exactly like [`Self::ScheduleFire`]**: the step's orders
    /// ride their own [`Self::StrategySubmit`] records, which replay re-applies in journal order, and a
    /// replay core never re-decides a grace (it hears no bar). The marker is what tells a reader which
    /// decisions went ahead without their input's bar, and when.
    ///
    /// A FORMAT STEP, stamped lazily (`crate::format`'s `SHARED_CLOSE_VERSION`): a build from before the
    /// variant cannot parse the frame, which would end the segment as unreadable and restore nothing, so the
    /// first marker raises the header to the step that build refuses. A node whose shared closes never time
    /// out keeps its stamp.
    SharedCloseTimeout {
        sequence: u64,
        now_ms: i64,
        mount_id: String,
        close_ms: i64,
        input: String,
    },
    /// A mount OPENED on a strategy file: the file's path and the sha256 of the bytes the composition root
    /// read ([`JournalStrategyFile`]), under the id the core keys the mount by. Written ONCE per mount as it
    /// opens (`vike_core`'s assembly for a boot mount, its runtime mount arm for one mounted live), never per
    /// event, and only for a mount a file names, so a core with none writes the bytes it always did. With
    /// the mount's `Snapshot` row ([`JournalSnapshotMountAttribution::strategy_file`]) it answers "which version
    /// of the file traded" after a restart (`vike_core::replay::RestoreReport::strategy_files`).
    ///
    /// **An OBSERVATION, replay-neutral**: a mount is not journaled (a replay core mounts only inert
    /// strategies), so nothing re-applies it. A FORMAT STEP: a build from before the variant cannot parse
    /// the frame, so the first one raises the header to `crate::format`'s `STORED_WORDS_VERSION`.
    StrategyFileMounted {
        sequence: u64,
        now_ms: i64,
        mount_id: String,
        strategy_file: JournalStrategyFile,
    },
    /// A FORECAST mount `mount_id`'s strategy published (book W35, `vike_model::OrderBroker::publish_forecast`):
    /// what it expects of `symbol` on `venue` (the mount's own venue; `symbol` resolved, never `""`). `now_ms` is
    /// the time the strategy DECIDED (its broker's `now`, the bar or tick it heard), the time a backtest records
    /// the same forecast at. Journaled as the strategy step's orders drain, only for a step that published one, so a
    /// core whose strategies publish none writes the bytes it always did.
    ///
    /// **An OBSERVATION, replay-neutral**: a forecast moves no order and no position, and a replay core never
    /// re-decides. The tradehub's execution stream sends it to the datahub, which stores it as the
    /// `kind=exec_forecast` series. A FORMAT STEP, stamped
    /// lazily (`crate::format`'s `FORECAST_VERSION`): a build from before the variant cannot parse the frame.
    Forecast {
        sequence: u64,
        now_ms: i64,
        mount_id: String,
        venue: String,
        symbol: String,
        forecast: vike_model::Forecast,
    },
    /// What the OMS states about one execution or one step of an order's life, right after its fold
    /// (`vike_core`'s `CoreThread::journal_execution_report`): the FIX drop copy's ExecutionReport,
    /// [`vike_model::orders::ExecutionReport`], self-contained (account, mount, venue order id, the order after
    /// the fold), so a reader of the journal holds no per-order state. `report.caused_by_sequence` names the
    /// record whose fold it states, and every report is appended before any `Snapshot` that can follow that cause.
    ///
    /// **An OBSERVATION, replay-neutral**: derived from its cause and the folded state, so nothing re-applies it,
    /// and it is outside `state_hash`. A FORMAT STEP, stamped lazily (`crate::format`'s `EXECUTION_REPORT_VERSION`):
    /// a build from before the variant cannot parse the frame, so the first report raises the header to the step
    /// that build refuses.
    ExecutionReport { sequence: u64, now_ms: i64, report: vike_model::orders::ExecutionReport },
    /// The reports a boot could not regenerate for the records `(after_sequence, through_sequence]` of journal
    /// `journal_id` (the id the journal directory already has; a restore that refolded nothing, a waker record in
    /// the window, an order outside the restore's base), said once and loudly so no report is lost in silence.
    /// `from_ms` and `to_ms` are the `now_ms` of the window's first and last record (`None` when unreadable), the
    /// span whose executions may be incomplete. `order_client_ids` names the orders known to be affected (empty
    /// when the window could not be read). An OBSERVATION, replay-neutral, on the [`Self::ExecutionReport`] step.
    ExecutionReportsUnverified {
        sequence: u64,
        now_ms: i64,
        journal_id: String,
        after_sequence: u64,
        through_sequence: u64,
        #[serde(default)]
        from_ms: Option<i64>,
        #[serde(default)]
        to_ms: Option<i64>,
        #[serde(default)]
        order_client_ids: Vec<String>,
        reason: String,
    },
    /// Every account's OPEN POSITIONS as the core holds them at one instant, the broker's start-of-day statement:
    /// journaled once per boot, right after what the restore owes the stream and before the first dispatch
    /// (`vike_core`'s `CoreThread::journal_position_snapshot`). A reader of the execution stream takes an account's
    /// opening position from the latest one at or before its window, plus that account's reports after it, so it
    /// never needs a fill recorded before the stream. An account whose positions the core cannot know at that
    /// instant says why ([`JournalPositionSnapshotAccount::unknown`]), never a guess.
    ///
    /// **An OBSERVATION, replay-neutral**: it moves nothing and is outside `state_hash`. On the
    /// [`Self::ExecutionReport`] format step (`crate::format`'s `EXECUTION_REPORT_VERSION`), which had not shipped
    /// when it was added.
    PositionSnapshot { sequence: u64, now_ms: i64, accounts: Vec<JournalPositionSnapshotAccount> },
    /// **Repeated DENIALS of one strategy's orders, coalesced** (format 35, the owner's yes of 2026-11-24): the
    /// pre-trade gate refused `repeat.count` more submits of one mount, venue, symbol and side, for one refusal, each
    /// minted, heard by its strategy and listed for the alert as before, and none journaled on its own. The first
    /// denial of that key in a process writes its whole triple (`StrategySubmit`, `MintedSubmit`, the `Denied`
    /// `ExecutionReport`); the ones after it are counted here (`crates/vike-core/src/runtime/denial_streaks.rs`),
    /// the record written when the count's window ends, before a `Snapshot`, at the key's next order that is not
    /// such a denial, and when the core reserves a new range of ids ([`JournalDeniedRepeat::reserved_from`]).
    ///
    /// **An OBSERVATION, replay-neutral**: a denied order moves no state. What it does spend, an order client id, is
    /// what the range accounts for: the repeats are minted inside it, nothing else ever is, a restore floors the
    /// session's sequence at its end, and a replay core skips it as the live core did. A FORMAT STEP, stamped lazily
    /// (`crate::format`'s `DENIED_REPEAT_VERSION`): a build from before the variant cannot parse the frame.
    DeniedRepeat { sequence: u64, now_ms: i64, repeat: JournalDeniedRepeat },
}

/// What one [`JournalRecord::DeniedRepeat`] counts: the key (`mount_id`, `venue`, `symbol`, `side`, `refusal_id`),
/// how many denials since the key's previous record and when, and the order client id range the core reserved.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JournalDeniedRepeat {
    pub mount_id: String,
    pub venue: String,
    pub symbol: String,
    /// The order's side, `+1` buy or `-1` sell (`vike_model::OrderRequest::side`).
    pub side: i32,
    /// The gate's catalog id of the refusal (`vike_model::events::OrderDenied::refusal_id`); `None` for a lane with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_id: Option<String>,
    /// The words of the LAST denial counted (the figures in them move with the market).
    pub reason: String,
    /// The denials counted since this key's previous record.
    pub count: u64,
    /// The `now_ms` of the first and the last denial counted.
    pub first_ms: i64,
    pub last_ms: i64,
    /// The session the reserved range belongs to.
    pub order_client_id_session: String,
    /// `[reserved_from, reserved_to)`: the order client id sequence numbers the core had reserved for coalesced
    /// denials when it wrote this record. It reserved them BEFORE it minted the first of them and its generator never
    /// mints inside them, so a crash between two of these records spends ids and never mints one twice.
    pub reserved_from: u64,
    pub reserved_to: u64,
}

/// One account of a [`JournalRecord::PositionSnapshot`]: its venue, its `account` column spelling (`DEFAULT` for the
/// unlabelled account, as an execution report names it), and its open positions, or why they are not known.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalPositionSnapshotAccount {
    pub venue: String,
    pub account: String,
    /// Every open (non-zero) position of the account's book, in its own order. Empty with no
    /// [`Self::unknown`]: the account is flat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<vike_model::AccountVenuePosition>,
    /// Why the core cannot know this account's positions (a journal with no earlier book of it, a restore that did
    /// not fold its tail, an account no engine runs), in words; `None` when [`Self::positions`] is its book.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<String>,
}

/// The strategy FILE a mount runs, as the journal records it: its path under the node's strategies folder
/// (as the profile or the mount named it), the lowercase-hex sha256 of the bytes that were read, and the
/// SDK its build compiled against. The path alone cannot say which code traded: the file can be edited
/// between two restarts, and the same bytes build differently on two SDKs (decision 0296, verdict 3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalStrategyFile {
    pub path: String,
    pub sha256: String,
    /// The number of the SDK the mount's `.rs` file was built against; `None` for a `.rhai` file (it runs
    /// on the host's own code), a build in a git checkout (SDK dev) and every record written before the
    /// key. NOT a format step (`crates/vike-journal/CLAUDE.md`'s rule: a step is owed when ignoring a key
    /// makes an older build fold something wrongly): the record is replay-neutral, an older build ignores
    /// the key and reads the file as it always did, and an absent key writes the bytes it always wrote
    /// (`record_compat_tests.rs`'s `a_strategy_file_records_its_sdk_and_one_without_writes_no_key`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk: Option<u32>,
}

/// One mount's closed-trade ledger in a [`JournalRecord::Snapshot`] (`vike_core`'s `runtime/closed_trades.rs`):
/// how many of its closed rows the ARCHIVE holds on disk (`crate::TradeClosedArchive`, numbers `0..archived`), the
/// rows after those, its open lots exactly, and the bar clock a fill after the `Snapshot` counts its bar index from.
/// A restart reads the archive's rows, adds these, resumes the ledger (`TradeClosedLedger::resumed`) and books
/// the fills of the tail on it.
///
/// The rows carried are only the ones the archive's thread had not synced yet, so the entry's size does not grow
/// with the trades a mount closed; `crates/vike-journal/src/record_tests.rs`'s `a_closed_row_is_about_250_bytes`
/// holds the size of one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JournalSnapshotClosedTrades {
    pub mount_id: String,
    /// Archived through: the archive holds this mount's rows numbered `0..archived`, and [`Self::rows`] are the
    /// ones numbered from `archived` on. In a `RestoreReport`'s copy (`vike_core::replay`), [`Self::rows`] is the
    /// whole list, the archive's rows put in front.
    #[serde(default, skip_serializing_if = "none_archived")]
    pub archived: u64,
    /// The rows, as the ledger booked them: `vike_model::TradeClosedRow` itself, whose serde shape is this record's
    /// (an empty tag writes no key).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<vike_model::TradeClosedRow>,
    /// The open lots, exactly: `vike_model::TradeClosedOpenLot` itself (`vike_trade_ledger`'s `OpenLot::state`),
    /// whose serde shape is this record's. `crates/vike-journal/src/record_tests.rs`'s
    /// `a_snapshots_open_lots_keep_their_journal_bytes` pins its bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lots: Vec<vike_model::TradeClosedOpenLot>,
    /// The mount's newest closed bar at Snapshot time and its bar length, `None` before its first bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bar_clock: Option<JournalSnapshotBarClock>,
}

/// Where a mount's own series stood at a `Snapshot`: its newest closed bar's open `ts`, that bar's `index` in the
/// series, and one bar's length. A fill after the `Snapshot` falls in the bar `index + (ts' - ts) / bar_ms`, the
/// count the core's `bar_of` makes over a series with no gap in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalSnapshotBarClock {
    pub timestamp: i64,
    pub index: usize,
    pub bar_ms: i64,
}

/// One armed conditional captured in a [`JournalRecord::Snapshot`] (emulator PR-3): the minted
/// `arm_id` (the disarm key, resumed verbatim on restore so a post-restart
/// [`vike_execution::OrderIntent::DisarmConditional`] still finds its arm) plus the terms the book
/// holds — the SAME shape as a [`OrderEmulationRecord`], except `terms.extreme` here is the CURRENT
/// ratcheted extreme at Snapshot time, not the arm-time seed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalSnapshotOrderEmulationArm {
    pub arm_id: String,
    pub terms: OrderEmulationRecord,
    /// WHICH ACCOUNT's engine the arm fires onto (journal v17): the route key of the engine the
    /// live core recorded for it (`vike_core`'s `OrderEmulationBook::cond_engine`), written only on a core
    /// holding two engines of one exchange, where the payload route of a restored fire is
    /// ambiguous. `None` on a single-account core (the venue's one engine is the only book the
    /// fire can reach), for an arm the core recorded no engine for, and on every `Snapshot` written
    /// before v17 — so a single-account box's `Snapshot` bytes did not move.
    ///
    /// Additive: `#[serde(default)]` reads an older entry back `None`, and `skip_serializing_if`
    /// keeps an absent key off the wire. `vike_core::replay::conditionals_hash` clears it before
    /// hashing (the replay core re-derives no account for a crash-tail arm), so the books fence
    /// over a v16 and a v17 journal hashes the same bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_key: Option<String>,
    /// The MOUNT that armed it (`vike_core`'s `OrderEmulationArmRoute::mount`, by its mount id), so a boot that reads
    /// from this `Snapshot` on (decision 0304) books the restored stop's fire into that mount without the
    /// `OrderEmulationArmed` record below it (`crate::order_client_id_mounts`'s `fold_record` seeds its arm owners
    /// from it). `None` for an operator's arm, a crash-tail arm, and every `Snapshot` written before the key.
    ///
    /// NOT a format step: a build from before the key ignores it and folds the arm's owner from the records as it
    /// always did; `skip_serializing_if` keeps an operator's arm, and so every mount-less core's `Snapshot`, at the
    /// bytes it always had. `vike_core::replay::conditionals_hash` clears it with `route_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_id: Option<String>,
}

/// One resting OTO/OCO contingency leg captured in a [`JournalRecord::Snapshot`] (live-runtime
/// OCO/OTO): the leg's `order_client_id` plus the linkage the shared [`vike_execution::ContingencyBook`] holds —
/// `parent` (the OTO entry that arms it, `None` for an entry), `linked` (its OCO siblings to cancel
/// when it fills), and `active` (an entry starts active; a held exit stays inactive until its
/// parent's fill arms it).
///
/// `held_request` is `Some` iff the leg is a HELD exit the runtime is keeping OFF the venue until
/// its parent fills — the resolved order to (re-)submit at arm time. It exists ONLY for held legs:
/// an active leg is already in the engine registry (restored via `Snapshot.engines`), so re-capturing
/// its request would double it. This is the durable home for a held exit — nothing else survives a
/// restart for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalSnapshotContingency {
    pub order_client_id: String,
    pub parent: Option<String>,
    pub linked: Vec<String>,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_request: Option<vike_model::OrderRequest>,
    /// The leg's type where its shape does not give it (`vike_execution::contingency::ContingencyLegSnapshot::kind`:
    /// an OUO leg, an OCO leg with no parent), `None` where it does, so a bracket's `Snapshot` keeps the
    /// bytes it had. A FORMAT STEP, stamped lazily: a build from before the key ignores it and reads an
    /// OUO leg as OCO, so a fill would CANCEL the legs it should reduce
    /// (`a_build_from_before_the_key_reads_an_ouo_leg_as_oco`), so a `Snapshot` holding an OUO leg needs
    /// `crate::format`'s `ORDER_LIST_VERSION` (`snap_version_needed`), which that build refuses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contingency_type: Option<vike_model::ContingencyType>,
}

/// One order captured in [`JournalRecord::Snapshot`]'s `held_releases`: a request the live core accepted
/// (its order client id minted, its capability preflight passed) and keeps OFF the venue until its
/// `vike_model::OrderRelease` holds (`vike_core`'s `CoreThread::held_releases`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderHeldRelease {
    /// The ROUTE KEY of the engine the order was accepted onto and is released onto (the venue for
    /// a default account, `venue#LABEL` for a labelled one). Always written: the list is new, so no
    /// older byte shape is at stake, and the venue alone cannot name a labelled account.
    pub route_key: String,
    /// The held request, `order_client_id` and `release` set. A release whose price condition names
    /// a venue (`vike_model::OrderReleasePrice::venue`) carries a key a v23 build cannot parse, so a
    /// `Snapshot` holding one needs `crate::format`'s `RELEASE_VENUE_VERSION` (`snap_version_needed`).
    pub request: vike_model::OrderRequest,
}

/// One EMULATED parent captured in [`JournalRecord::Snapshot`]'s `emulated` (decision 0275,
/// `vike_core`'s `runtime/emulated.rs`): the order its owner sees, worked by the core through one plain
/// limit at a time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalSnapshotOrderEmulation {
    /// The ROUTE KEY of the engine it works on (`venue#LABEL` for a labelled account).
    pub route_key: String,
    /// The owning mount's id; `None` for an operator's order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_id: Option<String>,
    /// The parent as accepted (its order client id minted, `peg` or `display_quantity` set).
    pub request: vike_model::OrderRequest,
    /// The live child's order client id; `None` between two children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<String>,
    /// Every child order client id still mapped to the parent (the live one included), so a restarted core routes
    /// a late event of an older child to the parent, never to its owner as a stranger's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<String>,
    /// The parent's filled size (the sum of its children's fills) and their average price
    /// (`vike_model::ContractValue::average_price`, `0.0` while nothing filled).
    #[serde(default)]
    pub filled: f64,
    #[serde(default)]
    pub average_price: f64,
    /// The live child's size as placed and how much of it filled (its completion).
    #[serde(default)]
    pub child_qty: f64,
    #[serde(default)]
    pub child_filled: f64,
    /// The law's own state (`Strategy::save_state` of `vike_execution::emulated`'s law): its child slot and
    /// progress. A replay core runs no law, so its exit `Snapshot` carries the state of the base it started
    /// from; a restored law awaits its child's confirmation either way.
    #[serde(default)]
    pub law: serde_json::Value,
}

/// One mount's fill-ATTRIBUTION ledger captured in a [`JournalRecord::Snapshot`] (multi-mount
/// durability, gap D): the mount's identity (`vike_core::strategy_state::mount_id_with`) plus the four
/// numbers the runtime folds from that mount's OWN fills — the same weighted-average-cost fold
/// `Account::fold` runs, scoped to one mount.
///
/// `realized_pnl` is GROSS realized price PnL and `fees_paid` the cumulative commission (a maker
/// rebate is negative), mirroring `Account`'s own split — the published per-mount view nets them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JournalSnapshotMountAttribution {
    pub mount_id: String,
    #[serde(default)]
    pub size: f64,
    #[serde(default)]
    pub average_price: f64,
    #[serde(default)]
    pub realized_pnl: f64,
    #[serde(default)]
    pub fees_paid: f64,
    /// The mount's per-mount BUDGET LATCH was spent when this Snapshot was taken (`vike_core`'s
    /// `CoreThread::mount_latched`): the restart puts it back instead of leaving a latched mount free
    /// to trade until the next sweep re-fires it. Additive serde, the `trigger_by` precedent and NOT a
    /// format step (`crates/vike-journal/src/format.rs`'s `VERSION` doc): `default` +
    /// `skip_serializing_if` `false`, so a pre-existing journal reads back `false`, a `false` write is
    /// byte-identical, and a build from before the key ignores it (a latched mount then comes back
    /// unlatched, exactly as before). Like the rest of the row it is NOT in `state_hash`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub latched: bool,
    /// The mount's LOSS-BUDGET BASE (decision 0239): the net realized PnL (realized less fees) the operator's
    /// un-latch (`vike_execution::Command::UnlatchMount`) found the mount at, from which its `max_loss` counts afresh
    /// (`vike_core`'s sweep judges `-(realized net - loss_base + unrealized)`). Additive serde, the `latched`
    /// precedent and NOT a format step: `default` + `skip_serializing_if` zero, so a mount that was never
    /// un-latched writes no key (the bytes a build from before the key wrote), a pre-existing journal reads back
    /// `0.0` (the base every mount always had) and a build from before the key ignores it, then judges the
    /// cumulative loss again (a mount past its budget latches at its next sweep: the safe side of a rollback).
    /// Like the rest of the row it is NOT in `state_hash`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub loss_base: f64,
    /// The instrument of this row, when it is NOT the mount's own: a mount with legs writes one row per
    /// `(venue, symbol)` it traded, all under its `mount_id` (`vike_execution::MountAttribution`). Additive serde,
    /// the `latched` precedent and NOT a format step: `None` on the mount's own row, which is every row of a
    /// mount with no legs, so its bytes do not move and a pre-existing journal reads back the own row. A build
    /// from before the key reads each row as the whole ledger and the last one wins, so the own row is written
    /// last; no build from before the key can mount a leg. Not in `state_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue: Option<String>,
    /// See [`Self::venue`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The ACCOUNT of [`Self::venue`] a leg's row is held on (its wire spelling, `DEFAULT` for the unlabelled one),
    /// when it is not the book the mount's route resolves for the venue (`vike_execution::MountAttributionRow`, a leg
    /// named by another account, decision 0293): two accounts of one venue on one symbol are two rows. Additive
    /// serde, the `latched` precedent and NOT a format step: `None` on every other row, so their bytes do not move,
    /// and a build from before the key reads the row as the venue's. Not in `state_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The mount's STRATEGY CODE (decision 0270: the four `strat` characters of its order client ids, a vike.db
    /// `strategy_code` row), so the journal alone reproduces the ids the mount minted. Additive serde, the
    /// `latched` precedent and NOT a format step: `None` (a mount with no code, and every row written before the
    /// key) writes no key, and a build from before the key ignores it (it stamps no strategy code into an id).
    /// Not in `state_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_client_id_code: Option<vike_model::orders::client_id::OrderClientIdStrategyCode>,
    /// The strategy FILE the mount runs (its path and the sha256 of the bytes read), on the mount's own row,
    /// so a `Snapshot` carries forward what a pruned [`JournalRecord::StrategyFileMounted`] said. Additive serde,
    /// the `order_client_id_code` precedent: `None` (a mount no file names, a leg's row, every row written before the
    /// key) writes no key, and a build from before the key ignores it. Not in `state_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_file: Option<JournalStrategyFile>,
}

/// The RESOLVED terms of one armed conditional — the [`JournalRecord::OrderEmulationArmed`] payload
/// (and, via [`JournalSnapshotOrderEmulationArm`], the per-arm shape a `Snapshot` carries).
/// `price` = a fixed stop's trigger; `trail` + `extreme` = a trailing stop's distance and the
/// extreme seeded from the standing mark (exactly one of `price`/`trail` is set, mirroring
/// `ConditionalIntent`, or neither when [`Self::kind_terms`] states the arm).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderEmulationRecord {
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
    /// An operator's CORE-HELD reduce-only stop (`vike_core`'s `CoreThread::arm_conditional_as`, a
    /// `--type stop --reduce-only` submit on a venue with no native stop): at the fire its quantity
    /// is capped to the live position and the market goes out `reduce_only`, or is dropped when the
    /// position is gone. Additive serde, the `trigger_by` precedent and NOT a format step
    /// (`crates/vike-journal/src/format.rs`'s `VERSION` doc): `default` + `skip_serializing_if`
    /// `false`, so a pre-existing journal reads back `false` and a `false` write is byte-identical.
    /// A build from before the field ignores the key (no struct here takes `deny_unknown_fields`)
    /// and restores a PLAIN stop, which is what it did with every stop it ever armed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reduce_only: bool,
    /// The terms of an arm the legacy `price`/`trail`/`extreme` cannot state (wave 3 of the order
    /// kinds: a `stop_limit`, a `take_profit`, a `trailing_limit`, a trailing stop with an activation;
    /// [`vike_execution::ConditionalKindTerms::of`] is the one rule), with those three `None`. Additive
    /// serde, the `reduce_only` precedent and NOT a format step: a plain stop or trailing stop writes
    /// no key, so every pre-existing record keeps its bytes. A build from before the key ignores it
    /// and reads an arm with neither `price` nor `trail`: its restore SKIPS the arm with its warning
    /// ("conditional with neither price nor trail+extreme"), in the `Snapshot` and the `OrderEmulationArmed`
    /// alike, and never arms it as a plain stop. The rollback line: mass-cancel the arms first (0199's).
    /// The attributes wave's arms (decision 0267: a trailing take-profit, a percent trail, a stop's
    /// adjustment) ride here too, NOT a format step either: a build from before them reads a word it
    /// does not know, a trailing arm with no `trail`, or a stop with no trigger, and skips the arm
    /// the same way (`crates/vike-journal/src/record_compat_tests.rs`'s
    /// `a_build_from_before_the_attributes_wave_restores_none_of_its_arms_and_every_older_one`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_terms: Option<vike_execution::ConditionalKindTerms>,
}

impl OrderEmulationRecord {
    /// The record of a resting arm: the legacy shape for a plain stop or trailing stop (the bytes it
    /// always had), else [`Self::kind_terms`] with `price`/`trail`/`extreme` `None`. The ONE writer
    /// rule, for the `Snapshot` capture and the `OrderEmulationArmed` record alike: a legacy key set beside
    /// `kind_terms` is what an older build would arm as a plain stop.
    pub fn of_arm(
        venue: String,
        symbol: String,
        order: &vike_model::OrderResting,
        trigger_by: Option<vike_model::TriggerBy>,
        reduce_only: bool,
    ) -> Self {
        let kind_terms = vike_execution::ConditionalKindTerms::of(order);
        let legacy = kind_terms.is_none();
        Self {
            venue,
            symbol,
            side: order.side,
            qty: order.size,
            price: order.price.filter(|_| legacy),
            trail: order.trail.filter(|_| legacy),
            extreme: order.extreme.filter(|_| legacy),
            trigger_by,
            reduce_only,
            kind_terms,
        }
    }
}

/// `skip_serializing_if` for [`OrderEmulationRecord::reduce_only`] and [`JournalSnapshotMountAttribution::latched`]: serde hands
/// the predicate a `&bool`.
fn is_false(b: &bool) -> bool {
    !*b
}

/// `skip_serializing_if` for [`JournalSnapshotMountAttribution::loss_base`]: serde hands the predicate a `&f64`. A `-0.0` is zero too.
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// `skip_serializing_if` for [`JournalSnapshotClosedTrades::archived`].
fn none_archived(n: &u64) -> bool {
    *n == 0
}

/// The [`JournalRecord::PortfolioSnapshot`] payload — one compact cross-venue portfolio observation.
/// Deliberately small and flat (no engine state, no order registry): this is a time series, so it
/// is written on a cadence and must stay cheap to append and cheap to scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortfolioSample {
    /// wall-clock ms the sample was taken (the boundary clock read that fired the timer).
    pub timestamp: i64,
    /// one row per engine, primary first then extras in registration order.
    pub venues: Vec<PortfolioSampleVenue>,
    /// every OPEN (non-zero-size) position across every engine, in each `Account`'s own
    /// insertion order — the same order the expansion/snapshot paths walk.
    pub positions: Vec<vike_model::AccountVenuePosition>,
}

/// One venue row of a [`PortfolioSample`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortfolioSampleVenue {
    pub venue: String,
    /// mode-aware, resolver-priced equity at this engine's own seed
    /// (`Engine::resolved_equity` under `CoreConfig::price_config`) — the SAME source the
    /// equity sampler and `CoreSnapshot` use (the one-price law).
    pub equity: f64,
    pub balance: f64,
    pub realized_pnl: f64,
}

/// The side books a [`JournalRecord::Snapshot`] carries beside the engines and the id counters,
/// as [`crate::Journal::append_snap`] takes them (borrowed: no clone on the fold path).
///
/// A struct because the sixth, the gateway cursors (journal v18), took the call past the workspace's
/// ten-parameter ceiling (`clippy.toml`); and because a caller that has nothing to say for a book
/// writes `..JournalSnapshotParts::default()` and the next book costs it no edit. Every field is the borrowed
/// twin of the same-named [`JournalRecord::Snapshot`] field, whose doc says what it holds and why it
/// rides the `Snapshot`.
#[derive(Debug, Clone, Copy, Default)]
pub struct JournalSnapshotParts<'a> {
    pub order_emulation_arms: &'a [JournalSnapshotOrderEmulationArm],
    pub contingencies: &'a [JournalSnapshotContingency],
    pub mount_attribution: &'a [JournalSnapshotMountAttribution],
    pub gateway_cursors: &'a [(String, vike_model::EventRingCursor)],
    pub held_releases: &'a [OrderHeldRelease],
    pub recent_order_client_ids: &'a [(String, i64)],
    pub emulated: &'a [JournalSnapshotOrderEmulation],
    pub closed_trades: &'a [JournalSnapshotClosedTrades],
    pub awaiting_engines: &'a [vike_execution::EngineSnapshot],
    pub order_client_id_mounts: &'a [(String, String)],
    pub order_client_id_past_sessions: &'a [(String, u64)],
}

/// [`JournalSnapshotParts`] OWNED: what a core hands [`crate::Journal::append_snap_deferred`], whose write thread
/// encodes the `Snapshot` after the core has gone on (decision 0279, amended 2026-11-19). The same fields, by value.
#[derive(Debug, Clone, Default)]
pub struct JournalSnapshotBooks {
    pub order_emulation_arms: Vec<JournalSnapshotOrderEmulationArm>,
    pub contingencies: Vec<JournalSnapshotContingency>,
    pub mount_attribution: Vec<JournalSnapshotMountAttribution>,
    pub gateway_cursors: Vec<(String, vike_model::EventRingCursor)>,
    pub held_releases: Vec<OrderHeldRelease>,
    pub recent_order_client_ids: Vec<(String, i64)>,
    pub emulated: Vec<JournalSnapshotOrderEmulation>,
    pub closed_trades: Vec<JournalSnapshotClosedTrades>,
    pub awaiting_engines: Vec<vike_execution::EngineSnapshot>,
    pub order_client_id_mounts: Vec<(String, String)>,
    pub order_client_id_past_sessions: Vec<(String, u64)>,
}

impl JournalSnapshotBooks {
    /// The borrowed view the `Snapshot` record is written from.
    pub fn parts(&self) -> JournalSnapshotParts<'_> {
        JournalSnapshotParts {
            order_emulation_arms: &self.order_emulation_arms,
            contingencies: &self.contingencies,
            mount_attribution: &self.mount_attribution,
            gateway_cursors: &self.gateway_cursors,
            held_releases: &self.held_releases,
            recent_order_client_ids: &self.recent_order_client_ids,
            emulated: &self.emulated,
            closed_trades: &self.closed_trades,
            awaiting_engines: &self.awaiting_engines,
            order_client_id_mounts: &self.order_client_id_mounts,
            order_client_id_past_sessions: &self.order_client_id_past_sessions,
        }
    }
}

/// `skip_serializing_if` for [`JournalRecordRef::Snapshot`]'s `order_client_id_mounts` (see [`no_cursors`]).
fn no_order_client_id_mounts(mounts: &&[(String, String)]) -> bool {
    mounts.is_empty()
}

/// `skip_serializing_if` for [`JournalRecordRef::Snapshot`]'s `order_client_id_past_sessions` (see [`no_cursors`]).
fn no_past_sessions(sessions: &&[(String, u64)]) -> bool {
    sessions.is_empty()
}

/// `skip_serializing_if` for [`JournalRecordRef::Snapshot`]'s `closed_trades` (see [`no_cursors`]).
fn no_closed_trades(closed: &&[JournalSnapshotClosedTrades]) -> bool {
    closed.is_empty()
}

/// `skip_serializing_if` for [`JournalRecordRef::Snapshot`]'s `awaiting_engines` (see [`no_cursors`]).
fn no_awaiting_engines(engines: &&[vike_execution::EngineSnapshot]) -> bool {
    engines.is_empty()
}

/// `skip_serializing_if` for [`JournalRecordRef::Snapshot`]'s `gateway_cursors`: serde hands the
/// predicate a reference to the FIELD, and the field here is itself a slice reference.
fn no_cursors(cursors: &&[(String, vike_model::EventRingCursor)]) -> bool {
    cursors.is_empty()
}

/// `skip_serializing_if` for [`JournalRecordRef::Snapshot`]'s `held_releases` (see [`no_cursors`]).
fn no_held_releases(held: &&[OrderHeldRelease]) -> bool {
    held.is_empty()
}

/// `skip_serializing_if` for [`JournalRecordRef::Snapshot`]'s `recent_order_client_ids` (see [`no_cursors`]).
fn no_recent_order_client_ids(recent: &&[(String, i64)]) -> bool {
    recent.is_empty()
}

/// `skip_serializing_if` for [`JournalRecordRef::Snapshot`]'s `emulated` (see [`no_cursors`]).
fn no_emulated(emulated: &&[JournalSnapshotOrderEmulation]) -> bool {
    emulated.is_empty()
}

/// Borrowed write-twin of [`JournalRecord`] — identical variant/field names ⇒ byte-identical
/// externally-tagged JSON, so `read_all` deserializes these back as owned `JournalRecord`.
/// Exists so appends serialize the hot-path `Ingest` and the snapshot `engines` slice BY
/// REFERENCE (no clone on the fold path).
#[derive(Serialize)]
pub(super) enum JournalRecordRef<'a> {
    Command {
        sequence: u64,
        now_ms: i64,
        message: &'a Ingest,
    },
    Snapshot {
        sequence: u64,
        now_ms: i64,
        engines: &'a [vike_execution::EngineSnapshot],
        order_client_id_session: &'a str,
        order_client_id_sequence: u64,
        // plain u64 (the writer ALWAYS stamps it); the owned twin is `Option<u64>` purely so a
        // pre-v7 frame's absence is distinguishable from a genuine 0 — `Some(n)` and `n`
        // serialize identically, so the write/read twins still match byte-for-byte.
        arm_sequence: u64,
        // always stamped (empty slice for empty books); the owned twin's `#[serde(default)]`
        // exists only so a pre-v8 frame with NO such key reads back as empty.
        order_emulation_arms: &'a [JournalSnapshotOrderEmulationArm],
        // always stamped (empty slice for no contingency legs); the owned twin's `#[serde(default)]`
        // exists only so a pre-v11 frame with NO such key reads back as empty.
        contingencies: &'a [JournalSnapshotContingency],
        // always stamped (empty slice for a mount-free core); the owned twin's `#[serde(default)]`
        // exists only so a pre-v13 frame with NO such key reads back as empty.
        mount_attribution: &'a [JournalSnapshotMountAttribution],
        // omitted when empty (every run that receives no gateway event), so such a run writes the
        // `Snapshot` bytes it always did; the owned twin's `#[serde(default)]` reads the absence back.
        #[serde(skip_serializing_if = "no_cursors")]
        gateway_cursors: &'a [(String, vike_model::EventRingCursor)],
        // omitted when empty (every core that holds no order for a release), so such a core writes
        // the `Snapshot` bytes it always did; the owned twin's `#[serde(default)]` reads the absence back.
        #[serde(skip_serializing_if = "no_held_releases")]
        held_releases: &'a [OrderHeldRelease],
        // omitted when empty (a core with no late-retry memory), like `held_releases`.
        #[serde(skip_serializing_if = "no_recent_order_client_ids")]
        recent_order_client_ids: &'a [(String, i64)],
        // omitted when empty (a core that emulates nothing), like `held_releases`.
        #[serde(skip_serializing_if = "no_emulated")]
        emulated: &'a [JournalSnapshotOrderEmulation],
        // omitted when empty (a core whose mounts never filled), like `held_releases`.
        #[serde(skip_serializing_if = "no_closed_trades")]
        closed_trades: &'a [JournalSnapshotClosedTrades],
        // omitted when empty (a core whose accounts all run), like `held_releases`.
        #[serde(skip_serializing_if = "no_awaiting_engines")]
        awaiting_engines: &'a [vike_execution::EngineSnapshot],
        // omitted when empty (a mount-less core), like `held_releases`.
        #[serde(skip_serializing_if = "no_order_client_id_mounts")]
        order_client_id_mounts: &'a [(String, String)],
        // omitted when empty (a core that holds no past session), like `held_releases`.
        #[serde(skip_serializing_if = "no_past_sessions")]
        order_client_id_past_sessions: &'a [(String, u64)],
        hash: u64,
    },
    StrategySubmit {
        sequence: u64,
        now_ms: i64,
        mount_id: &'a str,
        intent: &'a vike_execution::OrderIntent,
        // omitted when absent (every core that routes by the payload's venue), so such a core writes
        // the frame bytes it always did; the owned twin's `#[serde(default)]` reads the absence back.
        #[serde(skip_serializing_if = "Option::is_none")]
        route_key: Option<&'a str>,
    },
    MintedSubmit {
        sequence: u64,
        now_ms: i64,
        request: &'a vike_model::OrderRequest,
        #[serde(skip_serializing_if = "Option::is_none")]
        route_key: Option<&'a str>,
        // omitted when absent (every mint but an amend's), so those frames keep their bytes
        #[serde(skip_serializing_if = "Option::is_none")]
        replaces: Option<&'a str>,
    },
    PortfolioSnapshot {
        sequence: u64,
        now_ms: i64,
        sample: &'a PortfolioSample,
    },
    OrderEmulationArmed {
        sequence: u64,
        now_ms: i64,
        arm_id: &'a str,
        resolved: &'a OrderEmulationRecord,
    },
    OrderEmulationFire {
        sequence: u64,
        now_ms: i64,
        arm_id: &'a str,
        trigger_price: f64,
        request: &'a vike_model::OrderRequest,
        // omitted when absent, like `StrategySubmit`'s.
        #[serde(skip_serializing_if = "Option::is_none")]
        route_key: Option<&'a str>,
    },
    OrderEmulationDisarmed {
        sequence: u64,
        now_ms: i64,
        arm_id: &'a str,
    },
    MarginCallLiquidate {
        sequence: u64,
        now_ms: i64,
        request: &'a vike_model::OrderRequest,
        // always stamped (`null` for the ACCOUNT-wide margin-call sweep, which no mount owns); the
        // owned twin's `#[serde(default)]` exists only so a pre-v14 frame with NO such key reads
        // back as `None`.
        mount_id: Option<&'a str>,
        // ⚠ SKIPPED when absent, unlike `mount_id` directly above, and the asymmetry is
        // deliberate. `mount_id` has been written on every frame since v14, so emitting `null`
        // costs nothing; this field is `None` on every single-account box, which is every box in
        // production, and emitting `"route_key": null` on each of their liquidation frames would
        // change their journal BYTES for a fact that was already true. Same rule and same reason as
        // `vike_execution::ReconcileReports::route_key`'s own `skip_serializing_if`.
        #[serde(skip_serializing_if = "Option::is_none")]
        route_key: Option<&'a str>,
    },
    GtdExpire {
        sequence: u64,
        now_ms: i64,
        order_client_id: &'a str,
        engine: usize,
    },
    ScheduleFire {
        sequence: u64,
        now_ms: i64,
        mount_id: &'a str,
        tag: &'a str,
    },
    SharedCloseTimeout {
        sequence: u64,
        now_ms: i64,
        mount_id: &'a str,
        close_ms: i64,
        input: &'a str,
    },
    StrategyFileMounted {
        sequence: u64,
        now_ms: i64,
        mount_id: &'a str,
        strategy_file: &'a JournalStrategyFile,
    },
    Forecast {
        sequence: u64,
        now_ms: i64,
        mount_id: &'a str,
        venue: &'a str,
        symbol: &'a str,
        forecast: vike_model::Forecast,
    },
    ExecutionReport {
        sequence: u64,
        now_ms: i64,
        report: &'a vike_model::orders::ExecutionReport,
    },
    ExecutionReportsUnverified {
        sequence: u64,
        now_ms: i64,
        journal_id: &'a str,
        after_sequence: u64,
        through_sequence: u64,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        order_client_ids: &'a [String],
        reason: &'a str,
    },
    PositionSnapshot {
        sequence: u64,
        now_ms: i64,
        accounts: &'a [JournalPositionSnapshotAccount],
    },
    DeniedRepeat {
        sequence: u64,
        now_ms: i64,
        repeat: &'a JournalDeniedRepeat,
    },
}

/// The globally-monotonic `seq` carried by any [`JournalRecord`] variant. `pub`, and re-exported at
/// the crate root, because `vike_core::replay` needs it for the pre-v7 `arm_sequence` fallback bound
/// (max global seq + 1); [`crate::read_since`] and the tradehub's execution-stream publisher use
/// it too. (This said
/// "`pub(crate)` because `crate::replay` needs it" until 2026-09-28 — both halves from before the
/// journal left vike-core.)
pub fn record_sequence(rec: &JournalRecord) -> u64 {
    match rec {
        JournalRecord::Command { sequence: seq, .. }
        | JournalRecord::Snapshot { sequence: seq, .. }
        | JournalRecord::StrategySubmit { sequence: seq, .. }
        | JournalRecord::MintedSubmit { sequence: seq, .. }
        | JournalRecord::PortfolioSnapshot { sequence: seq, .. }
        | JournalRecord::OrderEmulationArmed { sequence: seq, .. }
        | JournalRecord::OrderEmulationFire { sequence: seq, .. }
        | JournalRecord::OrderEmulationDisarmed { sequence: seq, .. }
        | JournalRecord::MarginCallLiquidate { sequence: seq, .. }
        | JournalRecord::GtdExpire { sequence: seq, .. }
        | JournalRecord::ScheduleFire { sequence: seq, .. }
        | JournalRecord::SharedCloseTimeout { sequence: seq, .. }
        | JournalRecord::StrategyFileMounted { sequence: seq, .. }
        | JournalRecord::Forecast { sequence: seq, .. }
        | JournalRecord::ExecutionReport { sequence: seq, .. }
        | JournalRecord::ExecutionReportsUnverified { sequence: seq, .. }
        | JournalRecord::PositionSnapshot { sequence: seq, .. }
        | JournalRecord::DeniedRepeat { sequence: seq, .. } => *seq,
    }
}

#[path = "record_tests.rs"]
#[cfg(test)]
mod record_tests;

#[path = "record_compat_tests.rs"]
#[cfg(test)]
mod record_compat_tests;

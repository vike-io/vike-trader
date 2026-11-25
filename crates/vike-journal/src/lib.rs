//! Write-ahead command journal — pre-allocated mmap segments (Chronicle-shaped; spec
//! 2026-07-10-journal-replay-and-data-freshness.md §A1 + A1.1 durability contract).
//! Frame: [len: u32 LE][check: u32 FNV-1a32(payload)][payload: msgpack(JournalRecord)] — msgpack
//! since format 34 (`crate::format`'s `BINARY_VERSION`, each struct a map of its field names, serde's
//! human-readable mode), `serde_json` in every format before it, which every reader still decodes
//! (`frame.rs`'s `decode`). len==0 ⇒ end of written data (segments are zero-filled at creation). A
//! record failing its check is a torn tail — reading stops at the last valid record, never panics.
//!
//! # Format 34: the binary payload, a floor step
//!
//! ⚠ **Since 34 the `JournalRecord is an externally-tagged serde_json enum` arguments below are about
//! the SCHEMA, not the bytes**: the schema rules (add only, `#[serde(default)]`) are unchanged, the
//! bytes are msgpack. No older build parses a v34 frame, so 34 is the write version, the base AND the
//! floor of what this build resumes and restores ([`MIN_READABLE_VERSION`]); a v31..=33 journal is
//! READ (the offline readers, the execution stream's drain of a journal moved aside, the tools), never
//! resumed, and the deploy moves it aside on the owner's `allow_journal_move`.
//!
//! # Version compatibility contract
//!
//! Each segment header stamps a format [`VERSION`]. This build WRITES up to `VERSION` (lazily, see below) and READS/RESUMES
//! anything in [`MIN_READABLE_VERSION`]`..=`[`VERSION`] (see [`check_version`]) — an older
//! supported journal directory is opened and appended to in place, NOT rejected, so upgrading the
//! binary never costs an operator their live journal. Two things are still hard errors: a version
//! BELOW `MIN_READABLE_VERSION` (a shape this build can no longer parse) and any version ABOVE
//! `VERSION` (written by a later build, possibly carrying variants that do not exist here).
//!
//! The rule that makes the accepted range safe: **a version step may only ADD a
//! [`JournalRecord`] variant, or add a `#[serde(default)]`-guarded field to an existing one** —
//! `JournalRecord` is an externally-tagged `serde_json` enum, so an older segment's frames are
//! byte-identical valid frames under the newer version; they simply contain none of the added
//! variants, and an added defaulted field reads back as its default (which the consumer must
//! treat as "absent" — see `Snapshot.arm_sequence`, an `Option` whose `None` means exactly that). The same
//! holds for the enums nested inside a record ([`vike_execution::Ingest`],
//! [`vike_execution::OrderIntent`]). Any step that RENAMES or REMOVES a variant, or changes an
//! existing field shape, must raise `MIN_READABLE_VERSION` to `VERSION` in the same commit —
//! that is the switch that turns a stale journal back into a loud cold-start error.
//!
//! DETERMINISM IS UNAFFECTED by reading an older journal. vike-core's replay re-applies a record
//! sequence; a v4 journal replayed by a v5 binary yields exactly the record sequence a v4 binary
//! saw, because the added variants are ones v4 never wrote — and the two additive records that DO
//! exist ([`JournalRecord::MintedSubmit`], [`JournalRecord::PortfolioSnapshot`],
//! [`JournalRecord::GtdExpire`], and now [`JournalRecord::ScheduleFire`]) are replay-NEUTRAL
//! observations that the tail extraction ignores. So `state_hash` over a v4 journal is identical
//! under either binary: the accepted-range widening changes which directories open, never which
//! commands fold.
//!
//! Current range — 4 (`MintedSubmit`) .. 18 (the `GatewayEvent` variant of the nested
//! `vike_execution::Ingest` enum — a venue event that came over a gateway daemon's trade link, journaled
//! as ONE record with its ring cursor — and the defaulted `Snapshot.gateway_cursors` field, each ring's
//! resume point per account, omitted when empty so a run that receives no gateway event writes
//! byte-identical records; 17 added the defaulted `EngineSnapshot.route_key` and
//! `JournalSnapshotOrderEmulationArm.route_key` fields — WHICH ACCOUNT a `Snapshot`'s engine and armed stop belong to,
//! the facts a boot restore matches them to a live engine by; both `skip_serializing_if`-omitted,
//! so a single-account box writes byte-identical records; 16 added the defaulted
//! `MintedSubmit.route_key` and
//! `MarginCallLiquidate.route_key` fields — WHICH ACCOUNT of a venue a WRITE-AHEAD record's order
//! was lowered onto, so a reader (and, once multi-engine replay exists, a replay) REPRODUCES the
//! live routing decision instead of re-making it from an exchange id; both are
//! `skip_serializing_if`-omitted when absent, so a single-account box writes byte-identical RECORDS
//! and only the header version moves — the bump exists so a DOWNGRADED build refuses such a segment
//! instead of dropping the key and re-routing a liquidation to a book it was not read from; 15 added
//! the defaulted `AccountState.route_key` field — WHICH
//! ACCOUNT of an exchange an account-wide balance snapshot belongs to, so a second account's
//! balances stop folding into the first account's book; it is `skip_serializing_if`-omitted when
//! absent, so a default-account box writes byte-identical RECORDS and only the header version
//! moves — the bump exists so a DOWNGRADED build refuses such a segment instead of silently
//! dropping the key and misrouting the balance; 14 added the defaulted
//! `MarginCallLiquidate.mount_id` field — WHICH of the two releases that record carries: the
//! ACCOUNT-wide margin-call sweep, owned by no mount, or the PER-MOUNT budget latch's flatten,
//! whose fill belongs in its mount's ledger; 13
//! added the defaulted `Snapshot.mount_attribution` field — the per-mount attribution ledgers ride the Snapshot so
//! a restart resumes them; 12 the `ScheduleFire` variant — the wall-clock schedule
//! poll's fire DECISION, journaled write-ahead of the `on_schedule` it drives so a scheduled
//! session is auditable and still replays; replay-NEUTRAL, see the variant's doc; 11 added
//! `Snapshot.contingencies`; 10 the `GtdExpire` variant — the managed GTD/Day expiry
//! sweep's DECISION, journaled write-ahead of the cancel it issues so a `gtd_sweep` session is
//! auditable and no longer has to refuse replay; replay-NEUTRAL, see the variant's doc; 9 added
//! `MarginCallLiquidate` — the margin-call
//! auto-liquidation's released reduce-only MARKET order, journaled write-ahead so a replay
//! reproduces the liquidation the same way a `OrderEmulationFire` reproduces a fired stop; 8 added the
//! defaulted `Snapshot.conditionals` field — the armed conditional books ride the Snapshot so a restart can
//! re-arm them; 7 added `OrderEmulationDisarmed` + the defaulted `Snapshot.arm_sequence` field; 6 added
//! `OrderEmulationArmed`/`OrderEmulationFire`; 5 added `PortfolioSnapshot`): every step purely additive,
//! hence 4 is still readable. 19 (`Command::AdmitSymbol`) is the first step stamped LAZILY, see below;
//! 20 (the quote-currency fields of `AdmitSymbol` and the sparse `fx` table of the account snapshot)
//! is stamped lazily too, and is the first step that gives an OLD record a new meaning.
//!
//! # The header is stamped lazily from v19: a rollback is safe until the first `AdmitSymbol`
//!
//! ⚠ **Since v30 this section is history.** v30 spelled the stored words out in full (the order client id
//! keys and the other phase-4 key words: a RENAME, not an addition), and v31 moved every symbol a record names to
//! its instrument's user name (decision 0290: a v30 name may name another instrument now), so
//! `MIN_READABLE_VERSION` and `BASE_VERSION` are 31: every segment is stamped 31 from its first byte, this build
//! refuses every older one, and an older build refuses this one (the deploy moves the journal aside). The lazy doors
//! below are all below the base now.
//!
//! [`VERSION`] is the newest format this build writes; a segment is stamped `BASE_VERSION` (18 then)
//! until a record only [`VERSION`] can read is appended. `Journal::open` restamps an older
//! resumed segment UP to 18 and no further, and `Journal::append_command` raises the live segment
//! to 19 (and a roll carries it) just before the first `Command::AdmitSymbol` frame. So starting
//! this build on a v18 journal changes nothing a v18 build minds, and a deploy lost before any
//! symbol is admitted rolls back without moving the journal aside; the first `AdmitSymbol` is the
//! one-way door (`docs/ops/tradehub-the CI box.md`'s "A rollback needs the command journal moved aside
//! first"). The raise's stamp reaches disk before any frame after it is synced: the core only sends
//! it, and the sync thread syncs it into the durable copy first (see `Journal::require_version`,
//! decision 0279), so the raise blocks nothing on the append path.
//!
//! The second door is v20: an `AdmitSymbol` that NAMES a quote currency or states the symbol's own
//! margin rate raises the segment to 20 (and a `Snapshot` holding the rate table does too), because a
//! v19 build would parse such an admission, ignore the keys it does not know and fold a
//! foreign-quoted symbol at par, or margin a 1x symbol at the engine's leverage. A box that never
//! trades one never crosses it.
//!
//! The third door is v21: the first `Command::SetAccountTradingState` (one account's engine halted,
//! reduced or resumed on its own) or `Command::HoldAccount` (the node's own closing-only hold of a lane
//! twin it keeps up to be closed, raised with no operator action; also what the startup path sends when a
//! profile re-halts a restored engine) raises the segment to 21, because a v20 build cannot parse either
//! variant. It is a stamp, not a migration, and a journal that never carries one stays put.
//!
//! The door of v28 (decision 0271; 27 is the emulated order's) is the first
//! [`JournalRecord::SharedCloseTimeout`] (a shared close whose grace ran out): a variant an older build cannot
//! parse, replay-NEUTRAL. A node whose shared closes never time out keeps its stamp. The `Snapshot`'s
//! `closed_trades` (each mount's closed-trade ledger, its rows kept in the archive beside the segments,
//! `closed_archive.rs`) is no step: an older build ignores the key and the file. Neither is its `awaiting_engines`
//! (the journaled books of the accounts that wait for their venue, decision 0283): an older build ignores the key and
//! skips such a book at its boot, as every boot did before.
//!
//! The door of v30 (the stored-words step) is also the first [`JournalRecord::StrategyFileMounted`] (a mount
//! opened on a strategy file, with the sha256 of the bytes read): a variant an older build cannot parse,
//! replay-NEUTRAL, written once per mount as it opens. A node that mounts no strategy file never crosses it.
//!
//! The door of v31 (book W35) is the first [`JournalRecord::Forecast`] (a forecast a live mount's strategy
//! published): additive, replay-NEUTRAL, the execution stream's input for `kind=exec_forecast`. A node none of
//! whose strategies publishes a forecast never crosses it and keeps its rollback.
//!
//! The door of v33 (package 2's execution stream) is the first [`JournalRecord::ExecutionReport`] (what the OMS
//! states right after a fold) or [`JournalRecord::ExecutionReportsUnverified`]: additive, replay-NEUTRAL, the
//! tradehub's stream to the datahub. A live core writes one at its first order event, so a node crosses it then; and
//! a journaled core writes a [`JournalRecord::PositionSnapshot`] (every account's positions) at every boot, on the
//! same step.
//!
//! The door of v35 is the first [`JournalRecord::DeniedRepeat`] (repeated gate denials of one strategy order,
//! counted, with the order client id range their ids came from): additive, replay-NEUTRAL. A node crosses it at the
//! second denial of a key the gate refuses for the same refusal; one whose strategies never repeat a denied order
//! keeps 34 and its rollback.
//!
//! # Where the blocking `msync` happens — NOT on the fold path
//!
//! Appending is a bounded memcpy into a mapped page, but forcing those pages to disk is an
//! `msync` whose cost is proportional to dirty bytes: measured on the latency box, 1MB ≈ 8.6ms, 16MB ≈ 100ms,
//! a dirty 64MB segment ≈ 292ms (max 432ms). This journal is appended to from the vike-core FOLD
//! thread — the one the `p99 < 10µs` core-hop gate protects — so **every blocking `msync` runs on a
//! dedicated syncer thread the appender never joins** ([`sync::JournalSyncer`], `run_syncer`). FOUR used
//! to run inline and none does now:
//!
//! 1. the per-chunk `sync_completed_region` in `Journal::write_framed` (now `frame_checked`; which put a single
//!    ~141ms stall into the measured hop, at hop #58 518 of 100 000) — #929;
//! 2. [`Journal::roll`]'s opening whole-segment flush — #929;
//! 3. the CHECKPOINT flush at the end of every cadence `Snapshot`, i.e. `CoreThread::write_snap`'s
//!    `journal.flush()` — the one #929 missed, because the `runtime_latency` harness pinned
//!    `snapshot_every = u64::MAX` and so never fired a snapshot inside a measurement while the
//!    production default (`vike_core::JournalConfig::at`, `[sinks.journal]`) is **1024**. That call
//!    site is now [`Journal::queue_sync`] — the same watermark post as (1), which is why
//!    there is one syncer mechanism here and not two;
//! 4. the HEADER page of a version raise (`Journal::require_version`, at most once per
//!    format step): a page `msync`, but a whole-file fsync on btrfs. The raise now only sends the
//!    stamp; the sync thread syncs it into the durable copy before any frame past it, so a frame that
//!    needs the stamp is never synced ahead of it (`sync::copy_and_sync`, decision 0279).
//!
//! [`Journal::flush`] — the blocking whole-mapping `msync` — survives for tools and tests
//! that genuinely want a barrier on their own thread, and its doc says in as many words that it is
//! not for the fold. Nothing on the fold path calls it.
//!
//! The split, and why it loses nothing:
//! - **The work is a watermark, not a queue.** The appender stores a monotonic byte offset into
//!   `JournalSyncShared::target` and posts a wake; the syncer reads the NEWEST target when it gets round
//!   to it. A wake that coalesces with an earlier one therefore drops no work — which is why the
//!   backlog cannot grow and the appender never has to block to apply back-pressure. That is also
//!   why folding the snapshot checkpoint into the SAME watermark costs nothing: a snap request and
//!   a chunk request differ only in which byte offset they publish, so the two callers cannot
//!   queue up behind each other no matter how far the disk falls behind.
//! - **Sending never blocks.** The channel is unbounded and carries at most one un-consumed wake
//!   (`JournalSyncShared::wake_pending`) plus one message per segment roll. Blocking the appender is the
//!   one thing that is NOT an acceptable full-queue policy: it would reintroduce exactly the defect.
//! - **A dropped/late sync is safe, by the spec's own durability contract.** Per
//!   `docs/superpowers/specs/2026-07-10-journal-replay-and-data-freshness.md` §A1.1, the mmap'd
//!   pages belong to the OS page cache, so a process crash / OOM-kill / memory-pressure eviction /
//!   clean reboot all lose NOTHING with zero `msync`. Only a hard power cut loses the un-synced
//!   tail, the spec explicitly puts the bounding `msync` "OFF the hot append path", and the lost
//!   tail is repaired by the post-replay venue reconcile (§A4). So this sync is LATENCY SHAPING —
//!   it bounds how much dirty data one `msync` ever has to move — not the durability mechanism.
//! - **A graceful stop still syncs the tail.** `Journal`'s `Drop` publishes the final
//!   watermark, closes the channel and JOINS; the syncer drains, does one whole-segment `msync` and
//!   only then exits. This is what carries the EXIT `Snapshot` — the record a clean restart restores
//!   from — to disk now that `write_snap` no longer flushes it itself: `CoreThread::run` takes
//!   `self` BY VALUE, so the teardown snap is appended and then the whole `CoreThread` (journal
//!   included) drops INSIDE the th-core thread, i.e. the syncer's final `msync` completes before
//!   that thread exits and `CoreHandle::shutdown_and_join` returns.
//!
//! # Single-writer interlock
//!
//! [`Journal::open`] takes an EXCLUSIVE advisory lock on the journal directory
//! (`<dir>/LOCK`, see [`lock`]) and holds it for the journal's lifetime, so a
//! double-launched instance fails fast instead of interleaving frames into a live WAL. The
//! sentinel is NOT a `journal-*.vjl` segment, so every reader here ignores it, and the read-only
//! entry points ([`Journal::read_all`], [`Journal::latest_segment_version`],
//! [`read_since`], [`Journal::prune_before_latest_snap`]) take no lock at all — offline
//! inspection/replay tooling is unchanged.
//!
//! # Module layout
//!
//! One job per file. (This read "This module is a DIRECTORY" with a `mod.rs` row until 2026-09-28:
//! it was vike-core's `journal` module directory before the move described below, and the table
//! kept that shape.)
//!
//! | file | what it owns |
//! |---|---|
//! | `lib.rs` (here) | the contract above, the wiring |
//! | `format.rs` | the format constants ([`VERSION`], [`MIN_READABLE_VERSION`], `HEADER`, `MAGIC`) and [`check_version`] |
//! | `record.rs` | [`JournalRecord`] + payloads + the borrowed write twin — the serde SCHEMA |
//! | `frame.rs` | the `[len][check][payload]` codec: `fnv1a32`, the payload's msgpack (`encode_into`, `decode`, which reads JSON too) and the one shared `walk_frames` |
//! | `segment.rs` | segment file naming (`journal-*.vjl` / `.prep`) and block reservation |
//! | `sync.rs` | **the sync policy + the syncer thread — the FOLD-PATH LATENCY CONTRACT** |
//! | `writer.rs` | [`Journal`]: open/roll/flush and its `Drop`; `writer/append.rs` holds the `append_*` verbs and `writer/offline.rs` the readers and prune |
//! | `archive.rs` | `<dir>/archive/`, where the prune MOVES a superseded segment: the move, its compression and the archive's limit (decision 0304) |
//! | `read.rs` | [`read_since`] + [`read_durable_since`] + [`read_closed_since`], the offline reader half, and [`JournalExecutionStream`] (the execution stream's identity and prune floor) |
//!
//! ⚠ `sync.rs` is the one to read before changing anything here. Every blocking `msync` this
//! module performs lives in that file, on a thread the appender never joins, because this journal
//! is appended to from the vike-core FOLD thread the `p99 < 10µs` core-hop gate protects (#929,
//! #932 — see "Where the blocking `msync` happens" above). A change that puts an `msync`, a wait,
//! or a bounded/blocking send back onto the append path is a latency regression the unit tests
//! here CANNOT catch — `tests/runtime_latency.rs`'s journal variant is the gate for that.

//! # ⚠ This was `vike_core::journal` until 2026-09-23
//!
//! It moved out whole, with `journal_lock.rs` beside it (now [`lock`]) because nothing outside the
//! journal ever named that module. The reason is not a rank — no crate was blocked by the old home,
//! and this one sits at 25 against a `vike-core` at 30. It is that a crate whose job is REPORTING
//! had to link the live trading core in order to read a log file: `vike-report` names `vike_core`
//! for exactly two things and both of them are this journal. The owner's second reason is the one
//! that decided it — the journal grows as execution volume grows, and a thing that will keep
//! growing is better off with its own boundary than inside a crate named for something else.
//!
//! ⚠ **The append path is still inside the fold `crates/vike-core/tests/runtime_latency.rs` gates**
//! — that harness has three variants and two of them are journal-on, with a `JOURNAL_P99_NS`
//! calibrated by taking the WORST journal-variant p99 across 98 CI attempts. The crate boundary is
//! new; the budget is not, and `lto = "thin"` is what keeps the compiler able to see across it.
//!
//! ⚠ **Scratch directories here are a SECOND guard's, and this paragraph first claimed the
//! opposite.** It said they were still `crates/vike-core/src/scratch.rs`'s `Scratch` "reached
//! through an upward DEV edge". That does not compile and never could: the module is declared
//! `#[cfg(test)] mod scratch;` and the type is `pub(crate)`, so no edge of any kind reaches it,
//! and the dev-dependency written to carry it has been deleted. These suites are why that guard
//! exists — they leaked 27,471 `/tmp/vjl-*` directories totalling 211 GB on the the CI box CI box
//! before it did — and they could not take it with them.
//! `crates/vike-journal/src/testutil.rs`'s `Scratch` is this crate's own, on the same
//! `tempfile::TempDir` and the same `vjl-` prefix; its doc carries the argument.
//! ⚠ What the move DOES change is which of that gate's two rules applies, and it is a degradation
//! rather than a hole. `crates/vike-ops/tests/hygiene/journal_scratch_gate.rs` holds a STRICT rule for
//! `crates/vike-core` — one guard file, every other site pinned — because that is where the
//! original lives and where the 211 GB happened, and its `tree_sources` skips that directory by
//! NAME. So this crate answers to the PROPERTY rule instead: show a self-deleting handle, or be
//! pinned. A guard owning a `TempDir` satisfies it; what is left behind is the stricter
//! one-guard-file discipline.

#![warn(clippy::undocumented_unsafe_blocks)]
#![warn(clippy::allow_attributes)]

#[cfg(doc)]
use crate::format::{MIN_READABLE_VERSION, VERSION, check_version};

// The segment ARCHIVE (decision 0304): `<dir>/archive/`, where the prune MOVES a superseded segment and
// compresses it, and the archive's limit.
mod archive;
// The closed-trade ARCHIVE beside the segments (decision 0271 item 8): every closed trade written once, by its
// own thread, so a `Snapshot` carries only what it has not written yet.
mod closed_archive;
mod fills;
mod format;
mod frame;
// The asynchronous journal (decision 0279): the durable copy of the live segment and the
// boot repair from it, the write thread of the asynchronous journal, and the measurement seam the
// `runtime_latency` harness reads.
mod durable;
mod handoff;
#[doc(hidden)]
pub mod measure;
// The SINGLE-WRITER directory lock. It was `vike_core::journal_lock` and is renamed on the way in:
// `vike_journal::journal_lock` would stutter, and the crate name already carries the noun.
pub mod lock;
// ⚠ The off-path MATERIALIZER stood here (`pub mod materialize;`, 2026-09-25 .. 2026-10-11), moved to
// the datahub because it WROTE the store
// (`docs/decisions/0084-only-the-datahub-touches-the-store.md` gives the store one writer process), and
// was deleted on 2026-11-18 for the execution stream (decision 0295), with its checkpoint. With it went
// this crate's `vike-data` and `indexmap` edges: the journal is a WAL and names no store. What stays here
// is the reader half (`read.rs`) and the execution stream's identity and prune floor (`JournalExecutionStream`).
// The order client id -> mount fold, one record at a time: vike-core's restore and refold fold the
// journal's provenance records through it.
mod order_client_id_mounts;
mod read;
mod record;
mod segment;
mod sync;
#[cfg(test)]
mod testutil;
mod writer;

// `JournalCorruptCause` is re-exported PUBLICLY (the rest of `frame` stays private): it is the payload of
// `crates/vike-core/src/replay/entry.rs`'s `JournalReplayError::Truncated`, so it is part of this crate's
// public error surface. ⚠ That consumer is one crate UP now, which is why the reference is a
// citation rather than an intra-doc link.
// `fnv1a32`, the frame check, is public for the same reason a consumer's byte surgery needs it
// (`crates/vike-core/tests/journal/boot_restore.rs` re-frames a doctored payload), and
// `end_of_frames` behind `test-support` for the same surgery.
pub use archive::{ARCHIVE_DIR, JournalArchiveLimit, JournalArchivePass, JournalArchivePolicy};
pub use closed_archive::{
    ARCHIVE_FILE, TradeClosedArchive, TradeClosedArchiveEntry, TradeClosedArchiveRead,
    read_closed_archive,
};
pub use durable::repair_from_durable_copies;
pub use fills::{JournalReadError, fills_from_journal};
pub use frame::{JournalCorruptCause, fnv1a32};
#[cfg(any(test, feature = "test-support"))]
pub use frame::{end_of_frames, payload_as_value, payload_of_value};
pub use handoff::WriteFence;
pub use order_client_id_mounts::JournalOrderClientIdMountFold;
pub use read::{
    JournalExecutionStream, JournalExecutionStreamPredecessor, read_closed_since,
    read_durable_since, read_since, record_now_ms,
};
pub use record::{
    JournalDeniedRepeat, JournalPositionSnapshotAccount, JournalRecord, JournalSnapshotBarClock,
    JournalSnapshotBooks, JournalSnapshotClosedTrades, JournalSnapshotContingency,
    JournalSnapshotMountAttribution, JournalSnapshotOrderEmulation,
    JournalSnapshotOrderEmulationArm, JournalSnapshotParts, JournalStrategyFile,
    OrderEmulationRecord, OrderHeldRelease, PortfolioSample, PortfolioSampleVenue,
};
pub use sync::{JournalDurableMark, JournalDurableWatch};
pub use writer::{EXECUTION_STREAM_HOLD_MS, Journal, JournalFileConfig, JournalReadHalt, TORN_DIR};

pub use record::record_sequence;

/// The [`VERSION`] at which `Snapshot.conditionals` appeared. A pre-v8 `Snapshot` frame has no such field
/// and `#[serde(default)]`s back EMPTY — indistinguishable from a genuinely empty book — so
/// `vike_core::replay::replay_offline`'s conditional-book fence gates itself on
/// [`Journal::latest_segment_version`] reaching this. Public API, so it stays at the crate
/// root while `VERSION` lives in `format.rs`; a version bump checks this one too.
pub const SNAP_CONDITIONALS_VERSION: u32 = 8;
// NOTE: `Snapshot.contingencies` appeared at VERSION 11 (the OTO/OCO twin of `SNAP_CONDITIONALS_VERSION`,
// `#[serde(default)]`-defaulted to EMPTY on a pre-v11 frame). No dedicated replay FENCE gates on it:
// the observable effect of the contingency book — released OTO children / canceled OCO siblings —
// lands in the engine registry, which `state_hash` (replay fence 1) already covers; the resting
// book itself is reproduced by the replay core (base Snapshot seeded + tail fills re-driven) and read
// from its exit Snapshot for restore. A book-level fence (the conditionals-fence analogue) is a future
// rigor step, hence no `SNAP_CONTINGENCIES_VERSION` constant yet.

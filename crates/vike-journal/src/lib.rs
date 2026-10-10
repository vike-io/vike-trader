//! Write-ahead command journal — pre-allocated mmap segments (Chronicle-shaped; spec
//! 2026-07-10-journal-replay-and-data-freshness.md §A1 + A1.1 durability contract).
//! Frame: [len: u32 LE][check: u32 FNV-1a32(payload)][payload: serde_json(JournalRecord)].
//! len==0 ⇒ end of written data (segments are zero-filled at creation). A record failing its
//! check is a torn tail — reading stops at the last valid record, never panics.
//!
//! # Version compatibility contract
//!
//! Each segment header stamps a format [`VERSION`]. This build WRITES `VERSION` and READS/RESUMES
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
//! treat as "absent" — see `Snap.arm_seq`, an `Option` whose `None` means exactly that). The same
//! holds for the enums nested inside a record ([`vike_exec::Ingest`],
//! [`vike_exec::OrderIntent`]). Any step that RENAMES or REMOVES a variant, or changes an
//! existing field shape, must raise `MIN_READABLE_VERSION` to `VERSION` in the same commit —
//! that is the switch that turns a stale journal back into a loud cold-start error.
//!
//! DETERMINISM IS UNAFFECTED by reading an older journal. vike-core's replay re-applies a record
//! sequence; a v4 journal replayed by a v5 binary yields exactly the record sequence a v4 binary
//! saw, because the added variants are ones v4 never wrote — and the two additive records that DO
//! exist ([`JournalRecord::MintedSubmit`], [`JournalRecord::PortfolioSnap`],
//! [`JournalRecord::GtdExpire`], and now [`JournalRecord::ScheduleFire`]) are replay-NEUTRAL
//! observations that the tail extraction ignores. So `state_hash` over a v4 journal is identical
//! under either binary: the accepted-range widening changes which directories open, never which
//! commands fold.
//!
//! Current range — 4 (`MintedSubmit`) .. 16 (the defaulted `MintedSubmit.route_key` and
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
//! added the defaulted `Snap.mount_attr` field — the per-mount attribution ledgers ride the Snap so
//! a restart resumes them; 12 the `ScheduleFire` variant — the wall-clock schedule
//! poll's fire DECISION, journaled write-ahead of the `on_schedule` it drives so a scheduled
//! session is auditable and still replays; replay-NEUTRAL, see the variant's doc; 11 added
//! `Snap.contingencies`; 10 the `GtdExpire` variant — the managed GTD/Day expiry
//! sweep's DECISION, journaled write-ahead of the cancel it issues so a `gtd_sweep` session is
//! auditable and no longer has to refuse replay; replay-NEUTRAL, see the variant's doc; 9 added
//! `MarginCallLiquidate` — the margin-call
//! auto-liquidation's released reduce-only MARKET order, journaled write-ahead so a replay
//! reproduces the liquidation the same way a `ConditionalFire` reproduces a fired stop; 8 added the
//! defaulted `Snap.conditionals` field — the armed conditional books ride the Snap so a restart can
//! re-arm them; 7 added `ConditionalDisarmed` + the defaulted `Snap.arm_seq` field; 6 added
//! `ConditionalArmed`/`ConditionalFire`; 5 added `PortfolioSnap`): every step purely additive,
//! hence 4 is still readable.
//!
//! # Where the blocking `msync` happens — NOT on the fold path
//!
//! Appending is a bounded memcpy into a mapped page, but forcing those pages to disk is an
//! `msync` whose cost is proportional to dirty bytes: measured on the latency box, 1MB ≈ 8.6ms, 16MB ≈ 100ms,
//! a dirty 64MB segment ≈ 292ms (max 432ms). This journal is appended to from the vike-core FOLD
//! thread — the one the `p99 < 10µs` core-hop gate protects — so **every blocking `msync` runs on a
//! dedicated syncer thread the appender never joins** ([`sync::Syncer`], `run_syncer`). THREE used
//! to run inline and none does now:
//!
//! 1. the per-chunk `sync_completed_region` in [`CommandJournal::write_framed`] (which put a single
//!    ~141ms stall into the measured hop, at hop #58 518 of 100 000) — #929;
//! 2. [`CommandJournal::roll`]'s opening whole-segment flush — #929;
//! 3. the CHECKPOINT flush at the end of every cadence `Snap`, i.e. `CoreThread::write_snap`'s
//!    `journal.flush()` — the one #929 missed, because the `runtime_latency` harness pinned
//!    `snapshot_every = u64::MAX` and so never fired a snapshot inside a measurement while the
//!    production default (`vike_core::JournalConfig::at`, `[sinks.journal]`) is **1024**. That call
//!    site is now [`CommandJournal::queue_sync`] — the same watermark post as (1), which is why
//!    there is one syncer mechanism here and not two.
//!
//! [`CommandJournal::flush`] — the blocking whole-mapping `msync` — survives for tools and tests
//! that genuinely want a barrier on their own thread, and its doc says in as many words that it is
//! not for the fold. Nothing on the fold path calls it.
//!
//! The split, and why it loses nothing:
//! - **The work is a watermark, not a queue.** The appender stores a monotonic byte offset into
//!   `SyncShared::target` and posts a wake; the syncer reads the NEWEST target when it gets round
//!   to it. A wake that coalesces with an earlier one therefore drops no work — which is why the
//!   backlog cannot grow and the appender never has to block to apply back-pressure. That is also
//!   why folding the snapshot checkpoint into the SAME watermark costs nothing: a snap request and
//!   a chunk request differ only in which byte offset they publish, so the two callers cannot
//!   queue up behind each other no matter how far the disk falls behind.
//! - **Sending never blocks.** The channel is unbounded and carries at most one un-consumed wake
//!   (`SyncShared::wake_pending`) plus one message per segment roll. Blocking the appender is the
//!   one thing that is NOT an acceptable full-queue policy: it would reintroduce exactly the defect.
//! - **A dropped/late sync is safe, by the spec's own durability contract.** Per
//!   `docs/superpowers/specs/2026-07-10-journal-replay-and-data-freshness.md` §A1.1, the mmap'd
//!   pages belong to the OS page cache, so a process crash / OOM-kill / memory-pressure eviction /
//!   clean reboot all lose NOTHING with zero `msync`. Only a hard power cut loses the un-synced
//!   tail, the spec explicitly puts the bounding `msync` "OFF the hot append path", and the lost
//!   tail is repaired by the post-replay venue reconcile (§A4). So this sync is LATENCY SHAPING —
//!   it bounds how much dirty data one `msync` ever has to move — not the durability mechanism.
//! - **A graceful stop still syncs the tail.** `CommandJournal`'s `Drop` publishes the final
//!   watermark, closes the channel and JOINS; the syncer drains, does one whole-segment `msync` and
//!   only then exits. This is what carries the EXIT `Snap` — the record a clean restart restores
//!   from — to disk now that `write_snap` no longer flushes it itself: `CoreThread::run` takes
//!   `self` BY VALUE, so the teardown snap is appended and then the whole `CoreThread` (journal
//!   included) drops INSIDE the vt-core thread, i.e. the syncer's final `msync` completes before
//!   that thread exits and `CoreHandle::shutdown_and_join` returns.
//!
//! # Single-writer interlock
//!
//! [`CommandJournal::open`] takes an EXCLUSIVE advisory lock on the journal directory
//! (`<dir>/LOCK`, see [`lock`]) and holds it for the journal's lifetime, so a
//! double-launched instance fails fast instead of interleaving frames into a live WAL. The
//! sentinel is NOT a `journal-*.vjl` segment, so every reader here ignores it, and the read-only
//! entry points ([`CommandJournal::read_all`], [`CommandJournal::latest_segment_version`],
//! [`read_since`], [`CommandJournal::prune_before_latest_snap`]) take no lock at all — offline
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
//! | `frame.rs` | the `[len][check][payload]` codec: `fnv1a32` and the one shared `walk_frames` |
//! | `segment.rs` | segment file naming (`journal-*.vjl` / `.prep`) and block reservation |
//! | `sync.rs` | **the sync policy + the syncer thread — the FOLD-PATH LATENCY CONTRACT** |
//! | `writer.rs` | [`CommandJournal`]: open/roll/flush and its `Drop`; `writer/append.rs` holds the `append_*` verbs and `writer/offline.rs` the readers and prune |
//! | `read.rs` | [`read_since`] + [`MaterializeCheckpoint`], the offline reader half |
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

mod format;
mod frame;
// The SINGLE-WRITER directory lock. It was `vike_core::journal_lock` and is renamed on the way in:
// `vike_journal::journal_lock` would stutter, and the crate name already carries the noun.
pub mod lock;
// The off-path MATERIALIZER: tail-follows this crate's own WAL and folds its fill/order records
// into the Tier-2 `kind=exec_fill` / `kind=exec_order` `HistStore` series `vike-report` reads.
// ⚠ It was `vike_ops::journal_mat` until 2026-09-25. It sat there because that crate could name
// BOTH the WAL and `vike-data`; the WAL is THIS crate now, so a module whose whole job is reading
// it belongs here and the `vike-ops` half of that reason is gone.
// ⚠ UNGATED. It arrived behind a `materialize` feature and lost it within the hour: every consumer
// of this crate already links `vike-data`, and the gate had silently switched this module's tests
// off in every test lane. `Cargo.toml` carries the measurement.
pub mod materialize;
mod read;
mod record;
mod segment;
mod sync;
#[cfg(test)]
mod testutil;
mod writer;

// `CorruptCause` is re-exported PUBLICLY (the rest of `frame` stays private): it is the payload of
// `crates/vike-core/src/replay/entry.rs`'s `ReplayError::Truncated`, so it is part of this crate's
// public error surface. ⚠ That consumer is one crate UP now, which is why the reference is a
// citation rather than an intra-doc link.
pub use frame::CorruptCause;
pub use read::{MaterializeCheckpoint, read_since};
pub use record::{
    ConditionalRecord, JournalRecord, PortfolioPositionSample, PortfolioSample,
    PortfolioVenueSample, SnapConditional, SnapContingency, SnapMountAttr,
};
pub use writer::{CommandJournal, JournalFileConfig};

pub use record::record_seq;

/// The [`VERSION`] at which `Snap.conditionals` appeared. A pre-v8 `Snap` frame has no such field
/// and `#[serde(default)]`s back EMPTY — indistinguishable from a genuinely empty book — so
/// `vike_core::replay::replay_offline`'s conditional-book fence gates itself on
/// [`CommandJournal::latest_segment_version`] reaching this. Public API, so it stays at the crate
/// root while `VERSION` lives in `format.rs`; a version bump checks this one too.
pub const SNAP_CONDITIONALS_VERSION: u32 = 8;
// NOTE: `Snap.contingencies` appeared at VERSION 11 (the OTO/OCO twin of `SNAP_CONDITIONALS_VERSION`,
// `#[serde(default)]`-defaulted to EMPTY on a pre-v11 frame). No dedicated replay FENCE gates on it:
// the observable effect of the contingency book — released OTO children / canceled OCO siblings —
// lands in the engine registry, which `state_hash` (replay fence 1) already covers; the resting
// book itself is reproduced by the replay core (base Snap seeded + tail fills re-driven) and read
// from its exit Snap for restore. A book-level fence (the conditionals-fence analogue) is a future
// rigor step, hence no `SNAP_CONTINGENCIES_VERSION` constant yet.

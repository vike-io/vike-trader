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
//! DETERMINISM IS UNAFFECTED by reading an older journal. `replay.rs` re-applies a record
//! sequence; a v4 journal replayed by a v5 binary yields exactly the record sequence a v4 binary
//! saw, because the added variants are ones v4 never wrote — and the two additive records that DO
//! exist ([`JournalRecord::MintedSubmit`], [`JournalRecord::PortfolioSnap`],
//! [`JournalRecord::GtdExpire`], and now [`JournalRecord::ScheduleFire`]) are replay-NEUTRAL
//! observations that the tail extraction ignores. So `state_hash` over a v4 journal is identical
//! under either binary: the accepted-range widening changes which directories open, never which
//! commands fold.
//!
//! Current range — 4 (`MintedSubmit`) .. 15 (the defaulted `AccountState.route_key` field — WHICH
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
//!    production default ([`crate::JournalConfig::at`], `[sinks.journal]`) is **1024**. That call
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
//! (`<dir>/LOCK`, see [`crate::journal_lock`]) and holds it for the journal's lifetime, so a
//! double-launched instance fails fast instead of interleaving frames into a live WAL. The
//! sentinel is NOT a `journal-*.vjl` segment, so every reader here ignores it, and the read-only
//! entry points ([`CommandJournal::read_all`], [`CommandJournal::latest_segment_version`],
//! [`read_since`], [`CommandJournal::prune_before_latest_snap`]) take no lock at all — offline
//! inspection/replay tooling is unchanged.
//!
//! # Module layout
//!
//! This module is a DIRECTORY (the repo's convention for a multi-part module — cf.
//! `vike-core/src/runtime/`, `vike-backtest/src/engine/`). One job per file:
//!
//! | file | what it owns |
//! |---|---|
//! | `mod.rs` (here) | the contract above, the format constants, [`check_version`], the wiring |
//! | `record.rs` | [`JournalRecord`] + payloads + the borrowed write twin — the serde SCHEMA |
//! | `frame.rs` | the `[len][check][payload]` codec: `fnv1a32` and the one shared `walk_frames` |
//! | `segment.rs` | segment file naming (`journal-*.vjl` / `.prep`) and block reservation |
//! | `sync.rs` | **the sync policy + the syncer thread — the FOLD-PATH LATENCY CONTRACT** |
//! | `writer.rs` | [`CommandJournal`]: open/append/roll/prune, and its `Drop` |
//! | `read.rs` | [`read_since`] + [`MaterializeCheckpoint`], the offline reader half |
//!
//! ⚠ `sync.rs` is the one to read before changing anything here. Every blocking `msync` this
//! module performs lives in that file, on a thread the appender never joins, because this journal
//! is appended to from the vike-core FOLD thread the `p99 < 10µs` core-hop gate protects (#929,
//! #932 — see "Where the blocking `msync` happens" above). A change that puts an `msync`, a wait,
//! or a bounded/blocking send back onto the append path is a latency regression the unit tests
//! here CANNOT catch — `tests/runtime_latency.rs`'s journal variant is the gate for that.

use std::io;

mod frame;
mod read;
mod record;
mod segment;
mod sync;
#[cfg(test)]
mod testutil;
mod writer;

// `CorruptCause` is re-exported PUBLICLY (the rest of `frame` stays private): it is the payload of
// `crate::replay::ReplayError::Truncated`, so it is part of this crate's public error surface.
pub use frame::CorruptCause;
pub use read::{read_since, MaterializeCheckpoint};
pub use record::{
    ConditionalRecord, JournalRecord, PortfolioPositionSample, PortfolioSample,
    PortfolioVenueSample, SnapConditional, SnapContingency, SnapMountAttr,
};
pub use writer::{CommandJournal, JournalFileConfig};

pub(crate) use record::record_seq;

const MAGIC: u32 = 0x314C_4A56; // "VJL1" LE
/// The format version THIS build stamps on every segment it creates. See
/// [`MIN_READABLE_VERSION`] for which older versions it still accepts.
const VERSION: u32 = 15; // added AccountState.route_key (14: MarginCallLiquidate.mount_id; 13: Snap.mount_attr; 12: ScheduleFire; 11: Snap.contingencies; 10: GtdExpire; 9: MarginCallLiquidate; 8: Snap.conditionals; 7: ConditionalDisarmed + Snap.arm_seq; 6: ConditionalArmed/Fire; 5: PortfolioSnap; 4: MintedSubmit; 3: StrategySubmit; 2 before)
/// The OLDEST on-disk format version this build can read and resume (see the module doc's
/// "Version compatibility" contract). Raise this — never silently — only when a version step stops
/// being purely additive (a renamed/removed variant, or a changed `Ingest`/`EngineSnapshot` field
/// shape); that is what turns a stale journal back into a loud cold-start error.
///
/// 4 -> 5 added ONE variant ([`JournalRecord::PortfolioSnap`]), 5 -> 6 added TWO
/// ([`JournalRecord::ConditionalArmed`], [`JournalRecord::ConditionalFire`]), 6 -> 7 added
/// ONE variant ([`JournalRecord::ConditionalDisarmed`]) plus ONE `#[serde(default)]` `Option`
/// field on `Snap` (`arm_seq`, `None` ⇔ absent), 7 -> 8 added ONE `#[serde(default)]` `Vec`
/// field on `Snap` (`conditionals`, empty ⇔ absent — a pre-v8 journal restores with EMPTY books,
/// today's behavior, never an error), 8 -> 9 added ONE variant
/// ([`JournalRecord::MarginCallLiquidate`]) to an externally-tagged serde_json enum and nothing
/// else, so every v4..v8 frame is byte-for-byte a valid v9 frame; 10 added `GtdExpire`; 10 -> 11
/// added ONE `#[serde(default)]` `Vec` field on `Snap` (`contingencies`, empty ⇔ absent — a pre-v11
/// journal restores with EMPTY held/linked contingency state, today's behavior, never an error); and
/// 11 -> 12 added ONE variant ([`JournalRecord::ScheduleFire`]) to the externally-tagged enum and
/// nothing else, so every v4..v11 frame is byte-for-byte a valid v12 frame; 12 -> 13 added ONE
/// `#[serde(default)]` `Vec` field on `Snap` (`mount_attr`, empty ⇔ absent — a pre-v13 journal
/// restores with EMPTY per-mount attribution ledgers, exactly its pre-feature behavior, never an
/// error), so every v4..v12 frame is still a valid v13 frame; and 13 -> 14 added ONE
/// `#[serde(default)]` `Option` field on [`JournalRecord::MarginCallLiquidate`] (`mount_id`, `None`
/// ⇔ absent — a pre-v14 frame reads back as the ACCOUNT-wide margin-call case, which is what every
/// such frame written before the per-mount budget latch existed actually was, and a pre-v14 latch
/// flatten restores unattributed exactly as it did before, never an error).
///
/// 14 -> 15 added ONE `#[serde(default)]` `Option` field to `vike_model::events::AccountState`
/// (`route_key`, `None` ⇔ absent — WHICH account of an exchange an account-wide balance snapshot
/// belongs to), reached through the nested [`vike_exec::Ingest`] enum. Additive in both directions
/// that matter: every v4..v14 frame is a valid v15 frame and reads back `None`, which is exactly
/// what every such frame meant — one account per venue.
///
/// ⚠ It is also the first step whose bump is load-bearing in the OTHER direction, which is worth
/// saying because the field is `skip_serializing_if`-omitted and a v14 build would therefore parse
/// a v15 frame without complaint. It would parse it WRONG: serde ignores the unknown key, so a
/// labelled account's balance snapshot would read back unrouted and overwrite the DEFAULT
/// account's balance. `check_version`'s "any version ABOVE `VERSION`" rejection is the only thing
/// that can catch that, and it can only catch it if this number moved.
const MIN_READABLE_VERSION: u32 = 4;

/// The [`VERSION`] at which `Snap.conditionals` appeared. A pre-v8 `Snap` frame has no such field
/// and `#[serde(default)]`s back EMPTY — indistinguishable from a genuinely empty book — so
/// [`crate::replay::replay_offline`]'s conditional-book fence gates itself on
/// [`CommandJournal::latest_segment_version`] reaching this. Named here, next to `VERSION`, so the
/// two move together.
pub(crate) const SNAP_CONDITIONALS_VERSION: u32 = 8;
// NOTE: `Snap.contingencies` appeared at VERSION 11 (the OTO/OCO twin of `SNAP_CONDITIONALS_VERSION`,
// `#[serde(default)]`-defaulted to EMPTY on a pre-v11 frame). No dedicated replay FENCE gates on it:
// the observable effect of the contingency book — released OTO children / canceled OCO siblings —
// lands in the engine registry, which `state_hash` (replay fence 1) already covers; the resting
// book itself is reproduced by the replay core (base Snap seeded + tail fills re-driven) and read
// from its exit Snap for restore. A book-level fence (the conditionals-fence analogue) is a future
// rigor step, hence no `SNAP_CONTINGENCIES_VERSION` constant yet.
const HEADER: usize = 16; // magic u32 | version u32 | first_seq u64

/// Gate an on-disk segment's stamped version against what this build understands.
///
/// ACCEPTS `MIN_READABLE_VERSION..=VERSION`. REJECTS anything older (a shape this build can no
/// longer parse) and anything NEWER (a forward version written by a later build, whose frames may
/// carry variants that do not exist here — a hard `InvalidData`, never a best-effort parse).
fn check_version(version: u32) -> io::Result<()> {
    if (MIN_READABLE_VERSION..=VERSION).contains(&version) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "journal format version mismatch: got {version}, this build reads \
             {MIN_READABLE_VERSION}..={VERSION}"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::segment::seg_path;
    use crate::journal::testutil::*;

    #[test]
    fn version_mismatch_on_read_is_an_explicit_error() {
        let dir = tmp_dir("version");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..5 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);
        // Overwrite the header VERSION (bytes[4..8]) with a future version. MAGIC still matches, so
        // this is NOT a foreign file (skipped) nor a torn tail (dropped) — it is OUR journal in an
        // unknown format, which must surface LOUDLY rather than silently mis-parse.
        // `seg_files` (not the first `read_dir` entry) — the dir also holds the `LOCK` sentinel.
        let seg = seg_files(&dir).remove(0);
        let mut bytes = std::fs::read(&seg).unwrap();
        bytes[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
        std::fs::write(&seg, bytes).unwrap();
        let err = CommandJournal::read_all(&dir).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidData,
            "a MAGIC-matching, VERSION-mismatching segment is an explicit InvalidData error"
        );
    }

    /// The WRITE-RESUME twin of `version_mismatch_on_read_is_an_explicit_error` above: that one
    /// exercises the READ path (`read_all`); this one exercises the branch a live restart actually
    /// hits FIRST — `CommandJournal::open` → `map_segment`'s "resuming an EXISTING segment" arm
    /// (~line 227) — which must refuse to append this build's frames into a segment stamped with an
    /// UNSUPPORTED format version, rather than silently reinterpreting/overwriting it. Uses a
    /// FORWARD version (`VERSION + 1`), the case that stays a hard error under the compat range;
    /// the supported-older case is covered by
    /// `resuming_an_older_supported_version_upgrades_the_stamp_and_appends`. Hand-write a segment
    /// header (MAGIC matches, version does not) with no `CommandJournal` in the loop, since going
    /// through `open`/`append_*` would only ever stamp the CURRENT `VERSION`.
    #[test]
    fn open_refuses_to_resume_a_segment_with_an_unsupported_version() {
        let dir = tmp_dir("write-resume-version");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
        let seg = seg_path(&dir, 0);
        let mut header = vec![0u8; HEADER];
        header[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        header[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
        header[8..16].copy_from_slice(&0u64.to_le_bytes());
        std::fs::write(&seg, &header).unwrap();

        // `CommandJournal` holds an `MmapMut` (no `Debug` impl), so `unwrap_err()` isn't available
        // here the way it is on `read_all`'s `io::Result<Vec<JournalRecord>>` above — match instead.
        match CommandJournal::open(&dir, cfg) {
            Ok(_) => panic!(
                "resuming a MAGIC-matching, VERSION-mismatching segment must fail loudly on open, \
                 not silently truncate/reinterpret it"
            ),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidData),
        }
    }

    #[test]
    fn version_is_15() {
        assert_eq!(super::VERSION, 15);
    }

    /// Pins the COMPATIBILITY RANGE, not just the write version (see the module doc's "Version
    /// compatibility contract"). If a future step stops being purely additive it must raise
    /// `MIN_READABLE_VERSION` — and this test is the tripwire that forces that to be a deliberate,
    /// reviewed edit rather than a silent widening.
    #[test]
    fn readable_version_range_is_4_to_15() {
        assert_eq!(super::MIN_READABLE_VERSION, 4);
        assert_eq!(super::VERSION, 15);
        // NB: the floor <= write-version invariant is pinned by the two assert_eq! above; a
        // direct `assert!(MIN_READABLE_VERSION <= VERSION)` is const-foldable and trips
        // clippy::assertions_on_constants under -D warnings.
        assert!(super::check_version(4).is_ok(), "v4 (MintedSubmit era) journals stay readable");
        assert!(super::check_version(5).is_ok(), "v5 (PortfolioSnap era) journals stay readable");
        assert!(super::check_version(6).is_ok(), "v6 (ConditionalArmed/Fire era) stays readable");
        assert!(super::check_version(7).is_ok(), "v7 (ConditionalDisarmed era) stays readable");
        assert!(super::check_version(8).is_ok(), "v8 (Snap.conditionals era) stays readable");
        assert!(super::check_version(9).is_ok(), "v9 (MarginCallLiquidate era) stays readable");
        assert!(super::check_version(10).is_ok(), "v10 (GtdExpire era) stays readable");
        assert!(super::check_version(11).is_ok(), "v11 (Snap.contingencies era) stays readable");
        assert!(super::check_version(12).is_ok(), "v12 (ScheduleFire era) stays readable");
        assert!(super::check_version(13).is_ok(), "v13 (Snap.mount_attr era) stays readable");
        assert!(
            super::check_version(14).is_ok(),
            "v14 (MarginCallLiquidate.mount_id era) stays readable"
        );
        assert!(
            super::check_version(15).is_ok(),
            "v15 (AccountState.route_key era) is what this build writes"
        );
        assert!(super::check_version(3).is_err(), "below the floor is a loud error");
        assert!(super::check_version(16).is_err(), "a FORWARD version is a loud error");
    }

    /// A v4 journal directory (written before `PortfolioSnap` existed) must READ BACK under this
    /// v5 build — the whole point of the additive-only rule. Before the compat range this returned
    /// `InvalidData` and an operator had to discard a live journal to take the upgrade.
    #[test]
    fn an_older_supported_version_journal_reads_back_intact() {
        let dir = tmp_dir("version-back-compat-read");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..5 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);
        // Restamp the header to the OLDEST supported version. The frames themselves are untouched
        // — which is exactly the real-world shape of a v4 segment: `Cmd` records only, since v4
        // could not write `PortfolioSnap`.
        let seg = seg_files(&dir).remove(0);
        let mut bytes = std::fs::read(&seg).unwrap();
        bytes[4..8].copy_from_slice(&super::MIN_READABLE_VERSION.to_le_bytes());
        std::fs::write(&seg, bytes).unwrap();

        let back = CommandJournal::read_all(&dir).unwrap();
        assert_eq!(back.len(), 5, "every v4 record is readable by this v5 build");
        for (i, r) in back.iter().enumerate() {
            assert!(matches!(r, JournalRecord::Cmd { seq, .. } if *seq == i as u64));
        }
    }

    /// The WRITE-RESUME twin: restarting a v5 binary on a v4 directory must APPEND to it (the live
    /// crash-restore path), upgrading the header stamp in place, and the merged old+new record
    /// stream must read back in seq order.
    #[test]
    fn resuming_an_older_supported_version_upgrades_the_stamp_and_appends() {
        let dir = tmp_dir("version-back-compat-resume");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
        let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
        for i in 0..3 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);
        let seg = seg_files(&dir).remove(0);
        let mut bytes = std::fs::read(&seg).unwrap();
        bytes[4..8].copy_from_slice(&super::MIN_READABLE_VERSION.to_le_bytes());
        std::fs::write(&seg, bytes).unwrap();

        // Restart onto the v4 directory: must open (not error), resume the seq, and append.
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 3..6 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        j.flush().unwrap();
        drop(j);

        let stamped = u32::from_le_bytes(std::fs::read(&seg).unwrap()[4..8].try_into().unwrap());
        assert_eq!(
            stamped,
            super::VERSION,
            "resuming a supported older segment restamps it to the version now required to read it"
        );
        let back = CommandJournal::read_all(&dir).unwrap();
        assert_eq!(back.len(), 6, "pre-upgrade and post-upgrade records coexist in one segment");
        for (i, r) in back.iter().enumerate() {
            assert!(
                matches!(r, JournalRecord::Cmd { seq, .. } if *seq == i as u64),
                "seq stays monotonic across the version upgrade"
            );
        }
    }

    /// The floor is still a hard wall in the other direction: a version BELOW
    /// `MIN_READABLE_VERSION` is a shape this build cannot parse, so it must fail loudly rather
    /// than drop records as a torn tail.
    #[test]
    fn a_version_below_the_supported_floor_is_still_an_explicit_error() {
        let dir = tmp_dir("version-below-floor");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        j.append_cmd(1_000, &ingest(0)).unwrap();
        drop(j);
        let seg = seg_files(&dir).remove(0);
        let mut bytes = std::fs::read(&seg).unwrap();
        bytes[4..8].copy_from_slice(&(super::MIN_READABLE_VERSION - 1).to_le_bytes());
        std::fs::write(&seg, bytes).unwrap();
        assert_eq!(
            CommandJournal::read_all(&dir).unwrap_err().kind(),
            io::ErrorKind::InvalidData,
            "a pre-floor segment is an explicit InvalidData error, not a silent mis-parse"
        );
    }
}

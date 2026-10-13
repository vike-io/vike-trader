//! The OFFLINE half of [`CommandJournal`]: the readers and the prune, all associated functions over
//! a journal DIRECTORY rather than methods on a live handle.
//!
//! Split out of `writer.rs` by concern, bodies verbatim. None of them takes the directory lock, none
//! touches the fold thread (they are startup / tooling paths), and none reads `self` — which is why
//! they sit apart from the append side. A journal-wide halt at a corrupt frame is warned about in
//! one place, `warn_read_halt`, exactly once per call: by `read_all_located` (behind
//! `read_all_reporting` and `read_all`) and by `crate::read`'s `read_since`, the one other walk that
//! halts journal-wide. Three of them CHANGE the directory and must only run while no live core is
//! journaling into it: `prune_before_latest_snap`, `preserve_torn_segment` (a copy under `torn/`,
//! plus its keep-the-newest-N trim) and `set_aside` (a rename of the whole directory) — the boot
//! restore calls them before the live core opens the journal.

use std::io;
use std::path::{Path, PathBuf};

use super::CommandJournal;
use crate::format::{HEADER, MAGIC, check_version};
use crate::frame::{CorruptCause, WalkEnd, walk_frames};
use crate::read::MaterializeCheckpoint;
use crate::record::JournalRecord;
use crate::segment::{seg_path, segment_indices};

impl CommandJournal {
    /// The format [`VERSION`](crate::format::VERSION) stamped on the HIGHEST-indexed segment — i.e. the shape the journal's
    /// LAST records (and therefore its last `Snap`) were written under. `None` when the directory
    /// holds no readable segment.
    ///
    /// Exists for the ONE consumer that must distinguish "the field was absent" from "the field
    /// was present and empty": `vike_core::replay::replay_offline`'s conditional-book fence, which
    /// compares the reproduced resting books against `Snap.conditionals` — a field `#[serde(
    /// default)]`ed in at v8, so a pre-v8 `Snap` reads back EMPTY and would spuriously mismatch a
    /// correctly-folded non-empty book. Gating the fence on `>= 8` keeps an old journal replaying
    /// exactly as it did.
    ///
    /// The newest segment is the right one to ask because that is where the last `Snap` lives, and
    /// because [`Self::open`] restamps a RESUMED segment to the current `VERSION` before appending
    /// — so a resumed old journal reports the version its newest frames (including the exit `Snap`
    /// every clean shutdown writes) were actually written under, which is what the gate wants.
    pub fn latest_segment_version(dir: &Path) -> io::Result<Option<u32>> {
        let mut idxs: Vec<u64> = std::fs::read_dir(dir)?
            .filter_map(|e| {
                let name = e.ok()?.file_name().to_string_lossy().into_owned();
                name.strip_prefix("journal-")?.strip_suffix(".vjl")?.parse().ok()
            })
            .collect();
        idxs.sort_unstable();
        for idx in idxs.into_iter().rev() {
            let bytes = std::fs::read(seg_path(dir, idx))?;
            if bytes.len() < HEADER || u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != MAGIC
            {
                continue;
            }
            return Ok(Some(u32::from_le_bytes(bytes[4..8].try_into().unwrap())));
        }
        Ok(None)
    }

    /// Read every valid record across all segments, in seq order. Stops at the first torn record.
    pub fn read_all(dir: &Path) -> io::Result<Vec<JournalRecord>> {
        Self::read_all_reporting(dir).map(|(records, _cause)| records)
    }

    /// [`read_all`](Self::read_all) plus WHY it stopped: `Some(cause)` when the walk halted at a
    /// torn/corrupt frame — in which case the records returned are the valid PREFIX of the journal
    /// and everything after that frame is NOT included — and `None` when every segment ended
    /// cleanly. The records are IDENTICAL either way; this is the same read, with the halt cause
    /// carried out instead of discarded.
    ///
    /// It exists for `vike_core::replay::replay_offline`, whose determinism fence otherwise reports a
    /// truncated read as a hash divergence: with the tail gone, the last READABLE `Snap` is a
    /// mid-session checkpoint rather than the session's exit `Snap`, so re-folding the tail past it
    /// necessarily produces a different hash. `read_all` stays the entry point for every other
    /// reader (nothing about their behaviour changes).
    ///
    /// LOGGING: the halt is warned about HERE, exactly once per call — the arm returns immediately,
    /// so a torn journal cannot produce a per-record (or even a per-segment) log storm. Nothing on
    /// this path runs on the latency-gated fold thread: `read_all` is an offline/startup reader, and
    /// the fold's only journal calls are `append_*`/`write_snap`.
    pub fn read_all_reporting(
        dir: &Path,
    ) -> io::Result<(Vec<JournalRecord>, Option<CorruptCause>)> {
        Self::read_all_located(dir).map(|(records, halt)| (records, halt.map(|h| h.cause)))
    }

    /// [`read_all_reporting`](Self::read_all_reporting) plus WHERE it stopped: the segment, the
    /// byte offset of the frame that did not read, and whether any LATER segment still holds a
    /// readable record ([`ReadHalt::in_tail`]). The records are identical to [`read_all`](Self::read_all)'s.
    ///
    /// It exists for the boot restore (`vike_core::replay::restore_for_boot`), which has to tell
    /// the two shapes a halt takes apart before the live core reopens the directory: a torn frame
    /// at the end of the LAST written segment is the ordinary consequence of a crash (and the
    /// next [`Self::open`] overwrites it in place, so a copy has to be taken first), while a halt
    /// with records written AFTER it stops every later read of this directory for good.
    ///
    /// LOGGING: the one `warn!` per halt (`warn_read_halt`) is emitted here, as
    /// [`read_all_reporting`](Self::read_all_reporting) always did.
    pub fn read_all_located(dir: &Path) -> io::Result<(Vec<JournalRecord>, Option<ReadHalt>)> {
        let idxs = segment_indices(dir)?;
        let mut out = Vec::new();
        for (pos, &idx) in idxs.iter().enumerate() {
            let path = seg_path(dir, idx);
            let bytes = std::fs::read(&path)?;
            if bytes.len() < HEADER || u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != MAGIC
            {
                continue;
            }
            // MAGIC matched ⇒ this IS one of our segments: gate the format VERSION before reading
            // any record. An UNSUPPORTED version means the on-disk `JournalRecord` serde shape may
            // differ from this build's — surface it LOUDLY as an io error rather than silently
            // mis-parsing (or dropping records as a torn tail). A SUPPORTED older version
            // (`MIN_READABLE_VERSION..`) reads normally: it simply contains none of the variants
            // added since. Raise `MIN_READABLE_VERSION` whenever a version step changes an existing
            // `EngineSnapshot`/`Ingest`/`JournalRecord` shape rather than merely adding a variant;
            // that turns a stale journal into a clear cold-start error instead of a silent
            // divergence.
            let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            check_version(version)?;
            let (halt_at, end) = walk_frames(&bytes, HEADER, |rec| out.push(rec));
            if let WalkEnd::Corrupt(cause) = end {
                // Halt-at-first-corruption ACROSS THE WHOLE JOURNAL: an out-of-bounds len, a CRC
                // mismatch, or a parse failure all stop replay here and NEVER resume in a later
                // segment past the corruption gap (see `walk_frames`' oob-len halt policy). A clean
                // end (`WalkEnd::Clean`: len==0 or buffer end) instead crosses to the next segment.
                //
                // ONE warn per call, at the single seam that ACTS on the halt. Until the cause was
                // carried out of `walk_frames` this decision was silent AND undifferentiated: a
                // benign post-crash truncation and a segment written by a different build read the
                // same. `cause.reading()` carries which of those an operator is looking at.
                Self::warn_read_halt(&path, halt_at, cause, out.len());
                let in_tail =
                    idxs[pos + 1..].iter().all(|&later| Self::holds_no_record(dir, later));
                let halt =
                    ReadHalt { cause, segment: path, segment_index: idx, offset: halt_at, in_tail };
                return Ok((out, Some(halt)));
            }
        }
        Ok((out, None))
    }

    /// The ONE warning a journal-wide read halt logs, shared by the two walks that halt
    /// journal-wide ([`read_all_located`](Self::read_all_located) and `crate::read`'s `read_since`)
    /// so both name the same fields in the same words. `records_kept` counts every record the walk
    /// kept before the halt, BEFORE any seq filter — `read_since` passes the count `read_all` would
    /// have, not the length of its own filtered tail.
    pub(crate) fn warn_read_halt(
        segment: &Path,
        offset: usize,
        cause: CorruptCause,
        records_kept: usize,
    ) {
        tracing::warn!(
            segment = %segment.display(),
            offset,
            cause = cause.as_str(),
            records_kept,
            reading = cause.reading(),
            "journal read halted at a corrupt frame — nothing at or after this offset is \
             replayed, in this segment or any later one"
        );
    }

    /// Delete whole segment files fully superseded by the latest checkpoint: those whose MAXIMUM
    /// record seq is strictly LESS than the latest `Snap`'s seq (spec §A; the method deferred from
    /// Task 3). NEVER touches the segment holding the latest `Snap` or any later segment — they
    /// carry the restore base + its replay tail, which restart restore folds forward. When there is
    /// no `Snap`, nothing is pruned.
    ///
    /// Safe by construction: seqs are globally monotonic across segments (each `roll` carries the
    /// seq forward), so the latest `Snap`'s segment and every later one have a max seq `>=` the
    /// latest snap seq and are kept, while every fully-earlier segment has a max seq `<` it and is
    /// dropped. `read_all` therefore still returns the latest `Snap` onward — a prune never costs a
    /// restore its base. Conservative: a segment whose records cannot be positively bounded below
    /// the latest snap seq (unreadable / no-magic / torn-from-the-start / empty) is KEPT.
    ///
    /// Call this only when NO live core is journaling into `dir` (e.g. at successful restart, before
    /// re-opening) — deleting a segment another handle has mmapped fails on Windows.
    pub fn prune_before_latest_snap(dir: &Path) -> io::Result<()> {
        // (idx, path) for every segment file present.
        let mut segs: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)?
            .filter_map(|e| {
                let name = e.ok()?.file_name().to_string_lossy().into_owned();
                let idx: u64 = name.strip_prefix("journal-")?.strip_suffix(".vjl")?.parse().ok()?;
                Some((idx, seg_path(dir, idx)))
            })
            .collect();
        segs.sort_unstable_by_key(|(i, _)| *i);

        // Per-segment max record seq + the journal-wide latest Snap seq.
        let mut per_seg_max: Vec<(PathBuf, Option<u64>)> = Vec::with_capacity(segs.len());
        let mut latest_snap_seq: Option<u64> = None;
        for (_idx, path) in &segs {
            let (max_seq, max_snap_seq) = Self::scan_segment_seqs(path)?;
            if let Some(s) = max_snap_seq {
                latest_snap_seq = Some(latest_snap_seq.map_or(s, |cur| cur.max(s)));
            }
            per_seg_max.push((path.clone(), max_seq));
        }
        let Some(latest_snap_seq) = latest_snap_seq else {
            return Ok(()); // no Snap anywhere ⇒ prune nothing
        };
        // Retention floor for the materialized-journal reader (#2): when a materializer is active
        // (its checkpoint file exists), NEVER drop a segment holding records it has not yet
        // consumed — so the parquet trade log can be fed from any surviving segment. Absent a
        // materializer, pruning stays snapshot-only (byte-identical to before).
        let mat_ckpt = MaterializeCheckpoint::exists(dir).then(|| MaterializeCheckpoint::load(dir));

        for (path, max_seq) in per_seg_max {
            // Delete ONLY when positively bounded: EVERY record in this segment is < the latest
            // snap seq AND (no materializer, or all of them already materialized). `None` (no
            // readable records) is the "when in doubt, keep it" case.
            if matches!(max_seq, Some(mx)
                if mx < latest_snap_seq && mat_ckpt.is_none_or(|c| mx <= c))
            {
                std::fs::remove_file(&path)?;
            }
        }
        Ok(())
    }

    /// Scan ONE segment file's framed records, returning `(max record seq, max Snap seq)` over the
    /// valid prefix (same halt-at-first-corruption walk as [`read_all`], but per file). `(None,
    /// None)` when the file is shorter than the header, lacks the magic, or holds no valid record.
    fn scan_segment_seqs(path: &Path) -> io::Result<(Option<u64>, Option<u64>)> {
        let bytes = std::fs::read(path)?;
        if bytes.len() < HEADER || u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != MAGIC {
            return Ok((None, None));
        }
        let mut max_seq: Option<u64> = None;
        let mut max_snap_seq: Option<u64> = None;
        // Per-file walk (prune's conservative, keep-when-in-doubt bounds): [`walk_frames`]' end
        // cause is intentionally IGNORED here — a torn tail simply ends THIS file's fold, and
        // prune's outer loop still scans later segments (unlike `read_all`'s journal-wide halt).
        walk_frames(&bytes, HEADER, |rec| match rec {
            JournalRecord::Cmd { seq, .. }
            | JournalRecord::StrategySubmit { seq, .. }
            | JournalRecord::MintedSubmit { seq, .. }
            | JournalRecord::PortfolioSnap { seq, .. }
            | JournalRecord::ConditionalArmed { seq, .. }
            | JournalRecord::ConditionalFire { seq, .. }
            | JournalRecord::ConditionalDisarmed { seq, .. }
            | JournalRecord::MarginCallLiquidate { seq, .. }
            | JournalRecord::GtdExpire { seq, .. }
            | JournalRecord::ScheduleFire { seq, .. } => {
                max_seq = Some(max_seq.map_or(seq, |m| m.max(seq)));
            }
            JournalRecord::Snap { seq, .. } => {
                max_seq = Some(max_seq.map_or(seq, |m| m.max(seq)));
                max_snap_seq = Some(max_snap_seq.map_or(seq, |m| m.max(seq)));
            }
        });
        Ok((max_seq, max_snap_seq))
    }

    /// Whether segment `idx` of `dir` holds NO readable record: missing, shorter than a header,
    /// not one of ours, or zero frames before a clean end. A read error counts as "holds one" —
    /// the cautious answer for [`ReadHalt::in_tail`], which a `false` turns into the BROKEN
    /// reading rather than the torn-tail one.
    fn holds_no_record(dir: &Path, idx: u64) -> bool {
        let Ok(bytes) = std::fs::read(seg_path(dir, idx)) else {
            return false;
        };
        if bytes.len() < HEADER || u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != MAGIC {
            return true;
        }
        let mut n = 0usize;
        let (_, end) = walk_frames(&bytes, HEADER, |_| n += 1);
        n == 0 && matches!(end, WalkEnd::Clean)
    }

    /// COPY the segment a torn-tail read halted in to `<dir>/torn/<stamp>-journal-<index>.vjl`,
    /// before anything reopens the directory, and keep only the `keep` newest copies there.
    ///
    /// Why a copy at all: [`Self::open`] resumes the last segment by scanning to the first frame
    /// that does not read and appends from THERE, so the first record a restarted core writes
    /// overwrites the torn bytes — the only evidence of what the crash interrupted. The copy is
    /// the whole file (a segment is pre-allocated, so this is `segment_bytes` on disk per copy);
    /// `stamp` leads the name so the names sort oldest first, and the caller passes a fixed-width
    /// clock value (this crate reads no clock). A name already taken gets a `-<n>` suffix rather
    /// than replacing the older copy.
    ///
    /// The pruning removes only files under `torn/` that this function wrote: they are copies of
    /// derived data the journal keeps no other use for, and an unbounded series of crash copies
    /// would grow the journal's volume without limit. A failed removal is not an error.
    pub fn preserve_torn_segment(
        dir: &Path,
        halt: &ReadHalt,
        stamp: &str,
        keep: usize,
    ) -> io::Result<PathBuf> {
        let torn = dir.join(TORN_DIR);
        std::fs::create_dir_all(&torn)?;
        let index = halt.segment_index;
        let mut dst = torn.join(format!("{stamp}-journal-{index:08}.vjl"));
        let mut n = 1u32;
        while dst.exists() {
            dst = torn.join(format!("{stamp}-{n}-journal-{index:08}.vjl"));
            n += 1;
        }
        std::fs::copy(&halt.segment, &dst)?;
        let mut copies: Vec<PathBuf> = std::fs::read_dir(&torn)?
            .filter_map(|e| {
                let p = e.ok()?.path();
                (p.extension().and_then(|x| x.to_str()) == Some("vjl")).then_some(p)
            })
            .collect();
        copies.sort();
        let excess = copies.len().saturating_sub(keep.max(1));
        for old in &copies[..excess] {
            if old != &dst {
                let _ = std::fs::remove_file(old);
            }
        }
        Ok(dst)
    }

    /// MOVE the whole journal directory aside, to `<dir>.broken-<stamp>` beside it, so the next
    /// [`Self::open`] starts a fresh journal at `dir`; returns where it went.
    ///
    /// For a journal whose read halts at corruption BEFORE its last written records
    /// ([`ReadHalt::in_tail`] `false`, or a parse failure anywhere): every later read of that
    /// directory would stop at the same frame for good while the writer kept appending behind it,
    /// so every later restore would come from an older state. Nothing is deleted. The destination
    /// is made unique with a `-<n>` suffix, because a directory rename onto an existing name
    /// fails on Windows. Call it only while nothing holds the directory open (before the live core
    /// opens it) — the same constraint as [`Self::prune_before_latest_snap`].
    pub fn set_aside(dir: &Path, stamp: &str) -> io::Result<PathBuf> {
        let Some(name) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("journal directory {} has no name to move aside", dir.display()),
            ));
        };
        let parent = dir.parent().unwrap_or_else(|| Path::new("."));
        let mut dst = parent.join(format!("{name}.broken-{stamp}"));
        let mut n = 1u32;
        while dst.exists() {
            dst = parent.join(format!("{name}.broken-{stamp}-{n}"));
            n += 1;
        }
        std::fs::rename(dir, &dst)?;
        Ok(dst)
    }
}

/// The subdirectory of a journal directory [`CommandJournal::preserve_torn_segment`] copies a torn
/// segment into. The readers and the prune look at `journal-*.vjl` files at the top level only, so
/// nothing in it is ever read back as part of the journal.
pub const TORN_DIR: &str = "torn";

/// Where [`CommandJournal::read_all_located`] stopped at a corrupt frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadHalt {
    /// Which condition fired (an `OobLen`/`CheckMismatch` is a torn write, a `ParseFailed` a
    /// segment another build wrote).
    pub cause: CorruptCause,
    /// The segment file the walk halted in.
    pub segment: PathBuf,
    /// That segment's index (`journal-<index>.vjl`).
    pub segment_index: u64,
    /// The byte offset, from the start of the file, of the frame that did not read: every record
    /// before it is in the returned records, nothing at or after it is.
    pub offset: usize,
    /// `true` when no LATER segment holds a readable record, i.e. the halt is at the end of what
    /// was written — the shape a crash mid-append leaves. `false` when records were written after
    /// the corrupt frame: they can never be read through it.
    pub in_tail: bool,
}

#[path = "offline_tests.rs"]
#[cfg(test)]
mod offline_tests;

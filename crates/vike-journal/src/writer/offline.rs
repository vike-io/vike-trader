//! The OFFLINE half of [`CommandJournal`]: the readers and the prune, all associated functions over
//! a journal DIRECTORY rather than methods on a live handle.
//!
//! Split out of `writer.rs` by concern, bodies verbatim. None of them takes the directory lock, none
//! touches the fold thread (they are startup / tooling paths), and none reads `self` — which is why
//! they sit apart from the append side. `read_all_reporting` is the one place a journal-wide halt at
//! a corrupt frame is warned about, exactly once per call; `prune_before_latest_snap` must only run
//! while no live core is journaling into the directory.

use std::io;
use std::path::{Path, PathBuf};

use super::CommandJournal;
use crate::frame::{CorruptCause, WalkEnd, walk_frames};
use crate::read::MaterializeCheckpoint;
use crate::record::JournalRecord;
use crate::segment::seg_path;
use crate::{HEADER, MAGIC, check_version};

impl CommandJournal {
    /// The format [`VERSION`](crate::VERSION) stamped on the HIGHEST-indexed segment — i.e. the shape the journal's
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
        let mut idxs: Vec<u64> = std::fs::read_dir(dir)?
            .filter_map(|e| {
                let name = e.ok()?.file_name().to_string_lossy().into_owned();
                name.strip_prefix("journal-")?.strip_suffix(".vjl")?.parse().ok()
            })
            .collect();
        idxs.sort_unstable();
        let mut out = Vec::new();
        for idx in idxs {
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
                tracing::warn!(
                    segment = %path.display(),
                    offset = halt_at,
                    cause = cause.as_str(),
                    records_kept = out.len(),
                    reading = cause.reading(),
                    "journal read halted at a corrupt frame — nothing at or after this offset is \
                     replayed, in this segment or any later one"
                );
                return Ok((out, Some(cause)));
            }
        }
        Ok((out, None))
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
}

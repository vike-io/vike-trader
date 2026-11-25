//! The OFFLINE half of [`Journal`]: the readers and the prune, all associated functions over
//! a journal DIRECTORY rather than methods on a live handle.
//!
//! Split out of `writer.rs` by concern, bodies verbatim. None of them takes the directory lock, none
//! touches the fold thread (they are startup / tooling paths), and none reads `self` — which is why
//! they sit apart from the append side. A journal-wide halt at a corrupt frame is warned about in
//! one place, `warn_read_halt`, exactly once per call: by `read_all_located` (behind
//! `read_all_reporting` and `read_all`) and by `crate::read`'s `read_since`, the one other walk that
//! halts journal-wide. Three of them CHANGE the directory. Two must only run while no live core is
//! journaling into it: `preserve_torn_segment` (a copy under `torn/`, plus its keep-the-newest-N trim)
//! and `set_aside` (a rename of the whole directory) — the boot restore calls them before the live core
//! opens the journal. The third, `prune_before_latest_snap`, MOVES superseded segments into the archive
//! (`crate::archive`) beside a live core, on the tradehub's archive thread: it says why that is safe.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use vike_model::JournalArchiveFull;

use super::Journal;
use crate::archive::{ARCHIVE_DIR, JournalArchivePass, JournalArchivePolicy, archive_segment};
use crate::format::{HEADER, MAGIC, check_read_only_version, check_version};
use crate::frame::{
    JournalCorruptCause, JournalWalkEnd, checked_payload, decode, is_snapshot_payload,
    walk_frame_payloads, walk_frames,
};
use crate::read::{JournalExecutionStream, record_now_ms};
use crate::record::{JournalRecord, record_sequence};
use crate::segment::{segment_indices, segment_path};
use crate::sync::JournalDurableMark;

/// How long the prune holds records past the execution stream's floor (`JournalExecutionStream`):
/// 7 days of journal time (owner 11-18). A datahub gone longer loses only the window past it, loudly.
pub const EXECUTION_STREAM_HOLD_MS: i64 = vike_model::MS_PER_WEEK;

/// What [`Journal::prune_before_latest_snap`] needs of one segment's valid prefix, for a segment the hold lets go.
#[derive(Debug, Default)]
struct SegmentScan {
    max_sequence: Option<u64>,
    /// The newest `now_ms` among its records: the hold measures the segment by it.
    newest_ms: Option<i64>,
    /// The first record past the execution stream's floor: `(sequence, now_ms)`.
    first_past_floor: Option<(u64, i64)>,
}

impl Journal {
    /// The format [`VERSION`](crate::format::VERSION) stamped on the HIGHEST-indexed segment — i.e. the shape the journal's
    /// LAST records (and therefore its last `Snapshot`) were written under. `None` when the directory
    /// holds no readable segment.
    ///
    /// Exists for the ONE consumer that must distinguish "the field was absent" from "the field
    /// was present and empty": `vike_core::replay::replay_offline`'s conditional-book fence, which
    /// compares the reproduced resting books against `Snapshot.conditionals` — a field `#[serde(
    /// default)]`ed in at v8, so a pre-v8 `Snapshot` reads back EMPTY and would spuriously mismatch a
    /// correctly-folded non-empty book. Gating the fence on `>= 8` keeps an old journal replaying
    /// exactly as it did.
    ///
    /// The newest segment is the right one to ask because that is where the last `Snapshot` lives, and
    /// because [`Self::open`] restamps a RESUMED segment to the base version
    /// ([`BASE_VERSION`](crate::format::BASE_VERSION)) before appending, and the first record that
    /// needs a newer one raises it — so a resumed old journal reports the version its newest frames
    /// (including the exit `Snapshot` every clean shutdown writes) were actually written under, which is
    /// what the gate wants.
    pub fn latest_segment_version(dir: &Path) -> io::Result<Option<u32>> {
        let mut idxs: Vec<u64> = std::fs::read_dir(dir)?
            .filter_map(|e| {
                let name = e.ok()?.file_name().to_string_lossy().into_owned();
                name.strip_prefix("journal-")?.strip_suffix(".vjl")?.parse().ok()
            })
            .collect();
        idxs.sort_unstable();
        for index in idxs.into_iter().rev() {
            let bytes = std::fs::read(segment_path(dir, index))?;
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
    /// truncated read as a hash divergence: with the tail gone, the last READABLE `Snapshot` is a
    /// mid-session checkpoint rather than the session's exit `Snapshot`, so re-folding the tail past it
    /// necessarily produces a different hash. `read_all` stays the entry point for every other
    /// reader (nothing about their behaviour changes).
    ///
    /// LOGGING: the halt is warned about HERE, exactly once per call — the arm returns immediately,
    /// so a torn journal cannot produce a per-record (or even a per-segment) log storm. Nothing on
    /// this path runs on the latency-gated fold thread: `read_all` is an offline/startup reader, and
    /// the fold's only journal calls are `append_*`/`write_snap`.
    ///
    /// It READS a JSON journal of formats 31..=33 too (`crate::format::MIN_READ_ONLY_VERSION`), as every offline reader
    /// does; only [`read_all_located`](Self::read_all_located) and the boot restore's read
    /// ([`read_from_latest_snap_located`](Self::read_from_latest_snap_located)) refuse one.
    pub fn read_all_reporting(
        dir: &Path,
    ) -> io::Result<(Vec<JournalRecord>, Option<JournalCorruptCause>)> {
        Self::read_all_gated(dir, check_read_only_version)
            .map(|(records, halt)| (records, halt.map(|h| h.cause)))
    }

    /// [`read_all_reporting`](Self::read_all_reporting) plus WHERE it stopped: the segment, the
    /// byte offset of the frame that did not read, and whether any LATER segment still holds a
    /// readable record ([`JournalReadHalt::in_tail`]). The records are identical to [`read_all`](Self::read_all)'s.
    ///
    /// The boot restore (`vike_core::replay::restore_for_boot`) reads the same way from its newest `Snapshot` on
    /// ([`Self::read_from_latest_snap_located`]), because it has to tell
    /// the two shapes a halt takes apart before the live core reopens the directory: a torn frame
    /// at the end of the LAST written segment is the ordinary consequence of a crash (and the
    /// next [`Self::open`] closes that segment there and goes on in a new one), while a halt
    /// with records written AFTER it stops every later read of this directory for good.
    ///
    /// LOGGING: the one `warn!` per halt (`warn_read_halt`) is emitted here, as
    /// [`read_all_reporting`](Self::read_all_reporting) always did.
    ///
    /// ⚠ It takes the WRITER's range (`crate::format::check_version`), not the readers': the boot restore refuses a
    /// journal this build would not resume (a JSON one, formats 31..=33), in the words the deploy's pre-flight reads to
    /// move it aside, before anything restores from it or prunes it.
    pub fn read_all_located(
        dir: &Path,
    ) -> io::Result<(Vec<JournalRecord>, Option<JournalReadHalt>)> {
        Self::read_all_gated(dir, check_version)
    }

    /// **THE BOOT RESTORE'S READ** (`docs/decisions/0304-a-boot-restores-from-the-newest-snapshot-and-its-tail-and-old-segments-are-archived-never-deleted.md`):
    /// [`read_all_located`](Self::read_all_located)'s records and halt, from the NEWEST `Snapshot` on instead of from
    /// the first segment. The segments are searched newest first, each walked with its frames' lengths and checks
    /// verified and nothing decoded (`crate::frame`'s `walk_frame_payloads`); the first holding a `Snapshot` frame is
    /// the base, read from that frame on, and every later segment whole, under the same version gate and the same
    /// journal-wide halt. So the read costs the segments from the newest down to the base (one, as a rule) plus the
    /// records decoded from the `Snapshot` on, whatever the journal's size.
    ///
    /// What it does not see, on purpose: anything BELOW the base frame, a corrupt frame there included (those records
    /// are superseded, so a restore from the `Snapshot` does not need them). A journal with no `Snapshot` in any
    /// segment's readable prefix is read whole, as [`read_all_located`](Self::read_all_located) reads it.
    pub fn read_from_latest_snap_located(
        dir: &Path,
    ) -> io::Result<(Vec<JournalRecord>, Option<JournalReadHalt>)> {
        let idxs = segment_indices(dir)?;
        let (from, offset) =
            Self::latest_snap_frame(dir, &idxs, check_version)?.unwrap_or((0, HEADER));
        Self::read_gated_from(dir, &idxs, from, offset, check_version)
    }

    /// Where the newest `Snapshot` frame of `dir` starts: `(position in idxs, byte offset)`, searched from the newest
    /// segment down without decoding a record, each segment's version judged by `gate` before its frames are walked.
    /// `None` when no segment's readable prefix holds one.
    fn latest_snap_frame(
        dir: &Path,
        idxs: &[u64],
        gate: fn(u32) -> io::Result<()>,
    ) -> io::Result<Option<(usize, usize)>> {
        for (pos, &index) in idxs.iter().enumerate().rev() {
            let bytes = std::fs::read(segment_path(dir, index))?;
            let Some((version, _)) = header_of(&bytes) else { continue };
            gate(version)?;
            let mut last = None;
            walk_frame_payloads(&bytes, HEADER, |offset, payload| {
                if is_snapshot_payload(payload) {
                    last = Some(offset);
                }
            });
            if let Some(offset) = last {
                return Ok(Some((pos, offset)));
            }
        }
        Ok(None)
    }

    /// The whole-journal walk behind [`read_all_located`](Self::read_all_located) and
    /// [`read_all_reporting`](Self::read_all_reporting), each segment's version judged by `gate`.
    fn read_all_gated(
        dir: &Path,
        gate: fn(u32) -> io::Result<()>,
    ) -> io::Result<(Vec<JournalRecord>, Option<JournalReadHalt>)> {
        Self::read_gated_from(dir, &segment_indices(dir)?, 0, HEADER, gate)
    }

    /// The walk of `idxs` from position `from` (its segment from byte `offset`, every later one whole), each segment's
    /// version judged by `gate`: every reader above. A halt is journal-wide and carries its place (`JournalReadHalt`).
    fn read_gated_from(
        dir: &Path,
        idxs: &[u64],
        from: usize,
        offset: usize,
        gate: fn(u32) -> io::Result<()>,
    ) -> io::Result<(Vec<JournalRecord>, Option<JournalReadHalt>)> {
        let mut out = Vec::new();
        for (pos, &index) in idxs.iter().enumerate().skip(from) {
            let path = segment_path(dir, index);
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
            gate(version)?;
            let start = if pos == from { offset } else { HEADER };
            let (halt_at, end) = walk_frames(&bytes, start, |rec| out.push(rec));
            if let JournalWalkEnd::Corrupt(cause) = end {
                // Halt-at-first-corruption ACROSS THE WHOLE JOURNAL: an out-of-bounds len, a CRC
                // mismatch, or a parse failure all stop replay here and NEVER resume in a later
                // segment past the corruption gap (see `walk_frames`' oob-len halt policy). A clean
                // end (`JournalWalkEnd::Clean`: len==0 or buffer end) instead crosses to the next segment.
                //
                // ONE warn per call, at the single seam that ACTS on the halt. Until the cause was
                // carried out of `walk_frames` this decision was silent AND undifferentiated: a
                // benign post-crash truncation and a segment written by a different build read the
                // same. `cause.reading()` carries which of those an operator is looking at.
                // A frame the write thread may still be writing (the last segment's end) is not a
                // fault: no warning, the halt is still returned (`crate::frame::is_pending_tail`).
                if !crate::frame::is_pending_tail(pos + 1 == idxs.len(), cause) {
                    Self::warn_read_halt(&path, halt_at, cause, out.len());
                }
                let in_tail =
                    idxs[pos + 1..].iter().all(|&later| Self::holds_no_record(dir, later));
                let halt = JournalReadHalt {
                    cause,
                    segment: path,
                    segment_index: index,
                    offset: halt_at,
                    in_tail,
                };
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
        cause: JournalCorruptCause,
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

    /// **MOVE whole segment files the newest durable checkpoint superseded into the ARCHIVE**
    /// (`<dir>/archive/`, `crate::archive`'s module doc; decision 0304's verdict 2: nothing is deleted in
    /// place): those whose every record is older than the newest `Snapshot` in a segment durable WHOLE, and
    /// whose newest record is at least `policy.keep_ms` older than `now_ms`. NEVER touches that `Snapshot`'s
    /// segment or any later one, the live segment above all — they carry the restore base + its replay tail,
    /// which a boot folds forward. When no durable segment holds a `Snapshot`, nothing moves. Returns what moved
    /// and the archive's limit when it was reached ([`crate::JournalArchivePass`]).
    ///
    /// **Durable whole** is below `durable.segment_index` (the journal's `JournalDurableWatch`, so the sync
    /// thread has sealed it): a `Snapshot` the live segment holds may be lost to a power cut, and the boot would
    /// then need the previous one's segment. A directory nothing writes passes a mark past every segment.
    ///
    /// Safe by construction: sequences are globally monotonic across segments (each `roll` carries the
    /// seq forward), so the base `Snapshot`'s segment and every later one have a max seq `>=` the
    /// base's and are kept, while every fully-earlier segment has a max seq `<` it and may go.
    /// `read_all` therefore still returns that `Snapshot` onward — an archive pass never costs a
    /// restore its base. Conservative: a segment whose records cannot be positively bounded below
    /// the base (unreadable / no-magic / torn-from-the-start / empty) is KEPT.
    ///
    /// The second floor is the execution stream's (`crate::read::JournalExecutionStream`): while
    /// `<dir>/execution_stream.json` exists, a segment holding a record past its
    /// `acknowledged_sequence` is kept, for at most [`EXECUTION_STREAM_HOLD_MS`]: a segment whose
    /// newest record is older than that (journal clock, against the journal's newest record) goes
    /// anyway, and the pass says at ERROR which sequences and times no datahub stored.
    /// `materializer.ckpt` is no floor any more.
    ///
    /// The third is the archive's own limit (`policy.max_bytes`, `policy.max_age_ms`), judged once at the start
    /// of the pass: reached, `policy.at_limit` decides. `keep`, the default, moves nothing and leaves every segment
    /// in the live directory; `drop_oldest`, the operator's opt-in, first deletes the oldest finished archived
    /// copies (`JournalArchivePolicy::drop_oldest`) and goes on once the archive is under its limits, else moves
    /// nothing either. A pass that starts under it may end past it by what it moved.
    ///
    /// It runs beside a LIVE core (the tradehub's `journal-archive` thread, a minute apart): a segment below
    /// the durable base is one no thread of the journal writes, syncs or maps any more, and a reader that listed
    /// it before the rename skips it (`crate::read`'s `read_window_reporting`). It never compresses
    /// (`Journal::compress_archive` does, after it).
    ///
    /// **It decodes no record of a segment it keeps or a stored one it drops** (decision 0304): the base segment (the
    /// one the newest `Snapshot` is in) is found as [`Self::read_from_latest_snap_located`] finds it, every segment
    /// below it holds only older records, and a segment's sequences are bounded by the NEXT segment's header
    /// `first_sequence` (a roll carries the sequence on), so the floor is judged on headers. A segment past the floor
    /// is judged by the hold on the next segment's first record's clock, which no record of it is newer than, so the
    /// hold runs out no earlier than it did; only a segment the hold lets go is decoded, for its ERROR line. The keep
    /// window is judged on that same first record, and first on the second segment's alone: while it is inside the
    /// window, so is every record above the first segment, and the pass ends before its `Snapshot` search (a read of
    /// a whole segment), which is what keeps a pass a minute cheap.
    pub fn prune_before_latest_snap(
        dir: &Path,
        policy: &JournalArchivePolicy,
        now_ms: i64,
        durable: JournalDurableMark,
    ) -> io::Result<JournalArchivePass> {
        let idxs: Vec<u64> =
            segment_indices(dir)?.into_iter().filter(|i| *i < durable.segment_index).collect();
        let mut pass =
            JournalArchivePass { moved: Vec::new(), limit: policy.limit_reached(dir, now_ms)? };
        if pass.limit.is_some() {
            match policy.at_limit {
                JournalArchiveFull::Keep => return Ok(pass),
                JournalArchiveFull::DropOldest => {
                    pass.limit = policy.drop_oldest(dir, now_ms)?;
                    if pass.limit.is_some() {
                        return Ok(pass);
                    }
                }
            }
        }
        let old_enough =
            |bound: Option<i64>| bound.is_some_and(|b| now_ms.saturating_sub(b) >= policy.keep_ms);
        match idxs.get(1) {
            None => return Ok(pass),
            Some(&second) => {
                if let Some(first) = Self::first_record(&segment_path(dir, second))?
                    && !old_enough(Some(record_now_ms(&first)))
                {
                    return Ok(pass);
                }
            }
        }

        // The execution stream's floor: while its file exists, NEVER drop a segment holding a
        // record the datahub has not acknowledged as stored (`Some(None)`: nothing acknowledged yet,
        // so every segment is held). Without the file (no publisher ever ran on this directory) the
        // prune is snapshot-only. A file that does not read fails the prune: nothing is dropped.
        let floor = JournalExecutionStream::load(dir)?.map(|s| s.acknowledged_sequence);

        let Some((base, base_offset)) = Self::latest_snap_frame(dir, &idxs, |_| Ok(()))? else {
            return Ok(pass); // no durable Snapshot ⇒ move nothing
        };
        let Some(base_first) = Self::header_first_sequence(&segment_path(dir, idxs[base]))? else {
            return Ok(pass);
        };
        // The journal's newest record clock, read (from the newest `Snapshot` on) only once a segment past the floor
        // needs it.
        let mut newest_ms: Option<Option<i64>> = None;

        // (first sequence, its time, last sequence, its time) of what the hold let go unstored.
        let mut unstored: Option<(u64, i64, u64, i64)> = None;
        let pruned = (|| {
            for (pos, &index) in idxs.iter().enumerate().take(base) {
                let path = segment_path(dir, index);
                // Move ONLY when positively bounded: EVERY record in this segment is < the base
                // snap seq AND (no stream file, or all of them acknowledged, or the hold ran out).
                // No readable first record is the "when in doubt, keep it" case, and so is a durable
                // copy beside it (a seal that failed: the next boot repairs the working file from it).
                if Self::first_record(&path)?.is_none()
                    || crate::durable::durable_path(dir, index).exists()
                {
                    continue;
                }
                // Every record of it is below the next stamped segment's first sequence, which is at most the base's.
                let mut next = None;
                for &later in &idxs[pos + 1..=base] {
                    let later = segment_path(dir, later);
                    if let Some(first) = Self::header_first_sequence(&later)? {
                        next = Some((first, later));
                        break;
                    }
                }
                let Some((next_first, next_path)) =
                    next.filter(|(f, _)| *f > 0 && *f <= base_first)
                else {
                    continue;
                };
                let mx = next_first - 1;
                // No record of this segment is newer than the next segment's first one.
                let bound = Self::first_record(&next_path)?.map(|r| record_now_ms(&r));
                if !old_enough(bound) {
                    continue; // inside the keep window: it stays in the live directory
                }
                let stored = floor.is_none_or(|acknowledged| acknowledged.is_some_and(|a| mx <= a));
                let lost = if stored {
                    None
                } else {
                    let newest = match newest_ms {
                        Some(n) => n,
                        None => {
                            let (records, _) =
                                Self::read_gated_from(dir, &idxs, base, base_offset, |_| Ok(()))?;
                            *newest_ms.insert(records.iter().map(record_now_ms).max())
                        }
                    };
                    let expired = |at: Option<i64>| {
                        newest
                            .zip(at)
                            .is_some_and(|(n, s)| n.saturating_sub(s) > EXECUTION_STREAM_HOLD_MS)
                    };
                    if !expired(bound) {
                        continue; // Held: the datahub has not stored it and the hold has not run out.
                    }
                    let scan = Self::scan_segment_sequences(&path, floor.flatten())?;
                    match (expired(scan.newest_ms), scan.first_past_floor, scan.newest_ms) {
                        (true, Some(first), Some(last_ms)) => {
                            Some((first, last_ms, scan.max_sequence.unwrap_or(mx)))
                        }
                        _ => continue,
                    }
                };
                archive_segment(dir, index)?;
                pass.moved.push(index);
                if let Some(((first, first_ms), last_ms, last)) = lost {
                    let (f, f_ms) = unstored.map_or((first, first_ms), |(f, f_ms, _, _)| (f, f_ms));
                    unstored = Some((f, f_ms, last, last_ms));
                }
            }
            io::Result::Ok(())
        })();
        if let Some((first_sequence, lost_from_ms, last_sequence, lost_to_ms)) = unstored {
            tracing::error!(
                journal = %dir.display(),
                first_sequence,
                last_sequence,
                lost_from_ms,
                lost_to_ms,
                hold_ms = EXECUTION_STREAM_HOLD_MS,
                "journal prune: records no datahub acknowledged are older than the execution \
                 stream's hold and are moved to the archive: the executions among them reach no datahub"
            );
        }
        // The renames' names survive a power cut once both directories are synced (a rename lost to one leaves
        // the segment where it was).
        if !pass.moved.is_empty() {
            vike_utility::atomic_file::sync_dir(dir)?;
            vike_utility::atomic_file::sync_dir(&dir.join(ARCHIVE_DIR))?;
        }
        pruned.map(|()| pass)
    }

    /// Scan ONE segment file's framed records over the valid prefix (same halt-at-first-corruption
    /// walk as [`read_all`], but per file): everything [`SegmentScan`] holds, the first record past
    /// `floor` (every record when `None`) included. All `None` when the file is shorter than the
    /// header, lacks the magic, or holds no valid record.
    fn scan_segment_sequences(path: &Path, floor: Option<u64>) -> io::Result<SegmentScan> {
        let bytes = std::fs::read(path)?;
        let mut scan = SegmentScan::default();
        if bytes.len() < HEADER || u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != MAGIC {
            return Ok(scan);
        }
        // Per-file walk (prune's conservative, keep-when-in-doubt bounds): [`walk_frames`]' end
        // cause is intentionally IGNORED here — a torn tail simply ends THIS file's fold, and
        // prune's outer loop still scans later segments (unlike `read_all`'s journal-wide halt).
        walk_frames(&bytes, HEADER, |rec| {
            let (seq, ms) = (record_sequence(&rec), record_now_ms(&rec));
            scan.max_sequence = Some(scan.max_sequence.map_or(seq, |m| m.max(seq)));
            scan.newest_ms = Some(scan.newest_ms.map_or(ms, |m| m.max(ms)));
            if scan.first_past_floor.is_none() && floor.is_none_or(|f| seq > f) {
                scan.first_past_floor = Some((seq, ms));
            }
        });
        Ok(scan)
    }

    /// Segment `path`'s header `first_sequence` (the sequence its first record takes, carried on by the roll that
    /// made it): `None` for a file shorter than a header or not one of ours.
    fn header_first_sequence(path: &Path) -> io::Result<Option<u64>> {
        let mut header = [0u8; HEADER];
        if std::fs::File::open(path)?.read_exact(&mut header).is_err() {
            return Ok(None);
        }
        Ok(header_of(&header).map(|(_, first_sequence)| first_sequence))
    }

    /// Segment `path`'s FIRST record, read alone (its header and one frame): `None` when the file is not one of ours,
    /// holds no frame, or its first frame is torn or does not parse.
    fn first_record(path: &Path) -> io::Result<Option<JournalRecord>> {
        let mut file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        let mut head = [0u8; HEADER + 8];
        if file.read_exact(&mut head).is_err() || header_of(&head).is_none() {
            return Ok(None);
        }
        let len = u64::from(u32::from_le_bytes([
            head[HEADER],
            head[HEADER + 1],
            head[HEADER + 2],
            head[HEADER + 3],
        ]));
        if len == 0 || (HEADER as u64 + 8 + len) > size {
            return Ok(None);
        }
        let mut frame = head[HEADER..].to_vec();
        frame.resize(8 + len as usize, 0);
        if file.read_exact(&mut frame[8..]).is_err() {
            return Ok(None);
        }
        Ok(checked_payload(&frame, 0).ok().flatten().and_then(decode))
    }

    /// The seq the HIGHEST segment `index` must start at if it carries no header yet, 0 when it does (its header
    /// says, and `map_segment` ignores this). `roll` names a new segment before it stamps it, so a kill between
    /// the two, or a power cut that lost the header page, leaves a segment of zeros. Stamped 0 it would run the
    /// sequences again from 0: `read_since` skips such records as read, and the prune, which trusts sequences to rise,
    /// deletes the newest segment as older than the latest `Snapshot`. So it goes on from the last record before it.
    pub(super) fn first_sequence_if_unstamped(dir: &Path, index: u64) -> io::Result<u64> {
        let mut magic = [0u8; 4];
        let stamped = std::fs::File::open(segment_path(dir, index))
            .and_then(|mut f| f.read_exact(&mut magic))
            .is_ok_and(|()| u32::from_le_bytes(magic) == MAGIC);
        if stamped {
            return Ok(0);
        }
        for earlier in segment_indices(dir)?.into_iter().rev().filter(|i| *i < index) {
            if let Some(max_sequence) =
                Self::scan_segment_sequences(&segment_path(dir, earlier), None)?.max_sequence
            {
                return Ok(max_sequence + 1);
            }
        }
        Ok(0)
    }

    /// Whether segment `index` of `dir` holds NO readable record: missing, shorter than a header,
    /// not one of ours, or zero frames before a clean end. A read error counts as "holds one" —
    /// the cautious answer for [`JournalReadHalt::in_tail`], which a `false` turns into the BROKEN
    /// reading rather than the torn-tail one.
    pub(crate) fn holds_no_record(dir: &Path, index: u64) -> bool {
        let Ok(bytes) = std::fs::read(segment_path(dir, index)) else {
            return false;
        };
        if bytes.len() < HEADER || u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != MAGIC {
            return true;
        }
        let mut n = 0usize;
        let (_, end) = walk_frames(&bytes, HEADER, |_| n += 1);
        n == 0 && matches!(end, JournalWalkEnd::Clean)
    }

    /// COPY the segment a torn-tail read halted in to `<dir>/torn/<stamp>-journal-<index>.vjl`,
    /// before anything reopens the directory, and keep only the `keep` newest copies there.
    ///
    /// Why a copy at all: [`Self::open`] zeroes the torn frame's length, and the prune deletes that
    /// segment once a later `Snapshot` has superseded it, so the torn bytes (the only evidence of what
    /// the crash interrupted) last no longer than this copy does. The copy is
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
        halt: &JournalReadHalt,
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
    /// ([`JournalReadHalt::in_tail`] `false`, or a parse failure anywhere): every later read of that
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

/// `(version, first_sequence)` of a segment's leading `bytes`: `None` for bytes shorter than a header or not one of
/// ours (no magic).
fn header_of(bytes: &[u8]) -> Option<(u32, u64)> {
    let b = bytes.get(..HEADER)?;
    let word = |at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    (word(0) == MAGIC).then(|| (word(4), u64::from(word(8)) | (u64::from(word(12)) << 32)))
}

/// The subdirectory of a journal directory [`Journal::preserve_torn_segment`] copies a torn
/// segment into. The readers and the prune look at `journal-*.vjl` files at the top level only, so
/// nothing in it is ever read back as part of the journal.
pub const TORN_DIR: &str = "torn";

/// Where [`Journal::read_all_located`] stopped at a corrupt frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalReadHalt {
    /// Which condition fired (an `OobLen`/`CheckMismatch` is a torn write, a `ParseFailed` a
    /// segment another build wrote).
    pub cause: JournalCorruptCause,
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

//! The offline reader half: [`read_since`] (the incremental tail-read a materializer resumes from)
//! and [`MaterializeCheckpoint`] (the tiny durable `<dir>/materializer.ckpt` it resumes WITH).
//! Neither takes the journal directory's writer lock. Split out of `journal.rs`.
//!
//! `read_since` is NOT `read_all` plus a filter any more, only equal to one: it walks the segments
//! itself so it can leave out what cannot change its answer — a CLOSED segment this process
//! already walked clean whose every seq is `<= after_seq` (`SegmentMemo`), and the zero-filled
//! unwritten end of every segment it does read (`walk_segment`). Its result, its errors and its
//! halt warning are `read_all`'s, filtered; the one exception is stated on `SegmentMemo`.
//!
//! The materializer itself is not in this crate: it writes the store, so it lives in the one store
//! writer process, `crates/vike-datahub/src/materialize.rs`'s `JournalMaterializer`
//! (`docs/decisions/0084-only-the-datahub-touches-the-store.md`). Both items stay HERE because the
//! journal needs them too: `writer/offline.rs`'s `prune_before_latest_snap` keeps every segment
//! holding records past the checkpoint while the checkpoint file exists, so the file must stay in
//! the journal directory and its name and format must stay this crate's.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use super::format::{HEADER, MAGIC, check_version};
use super::frame::{CorruptCause, WalkEnd, walk_frames};
use super::record::{JournalRecord, record_seq};
use super::segment::{seg_path, segment_indices};
use super::writer::CommandJournal;

/// Every valid record with `seq > after_seq`, in seq order — the incremental tail-read the
/// journal materializer (`crates/vike-datahub/src/materialize.rs`'s `materialize_once`) uses to
/// consume only records past where it last stopped. `after_seq` is the last seq already CONSUMED
/// and the bound is STRICT (exclusive), which is the resume semantic a checkpoint needs.
///
/// The RESULT is [`CommandJournal::read_all`] filtered by [`record_seq`] `> after_seq`, and so are
/// the failure modes: the same records in the same order, the same `Err` for a segment whose
/// format version this build does not read, the same journal-wide stop at the first corrupt frame
/// (the valid prefix, filtered, and the same one warning with the same `records_kept`). The one
/// case that can differ is stated on `SegmentMemo`: bytes of an already-verified CLOSED segment
/// changing on disk with its length, mtime and header unchanged.
///
/// Note the lower bound is strict: `after_seq == 0` returns every record with `seq > 0`. Because
/// this journal assigns the FIRST record seq 0, a cold-start materializer that has never
/// checkpointed (so [`MaterializeCheckpoint::load`] returns 0) skips that seq-0 record. In
/// practice the materializer is seeded from the latest `Snap` (whose seq is its exclusive base),
/// not from raw seq 0, so this is benign; a caller that must include seq 0 keys off a checkpoint
/// below the first seq rather than `read_since(dir, 0)`.
///
/// COST. It used to be `read_all` then a filter: every call read every segment file whole (each
/// pre-allocated, 64 MiB by default) and deserialized every record, and its doc called that
/// bounded because the prune compacts the journal. That stopped being true when the materializer
/// started calling it every few seconds for the trade daemon's whole uptime: the prune
/// ([`CommandJournal::prune_before_latest_snap`]) runs only before the trade daemon opens the
/// journal, so the segment count grows for as long as it runs. Now a call costs:
///
/// - one directory listing, and an open plus a 16-byte header read per segment;
/// - a full walk (read and deserialize) of every segment that can hold a record past `after_seq`
///   or has not been verified yet — in the steady state, the one live segment the writer is
///   appending to — and of a corrupt segment, every time, because a halt is never remembered;
/// - only the WRITTEN bytes of each segment walked, plus at most `READ_CHUNK`: the zero-filled
///   pre-allocated end is not read (`walk_segment`).
///
/// The first call in a process verifies every segment once, which is what every call cost before.
pub fn read_since(dir: &Path, after_seq: u64) -> io::Result<Vec<JournalRecord>> {
    let mut memo = SegmentMemo::take(dir);
    let out = read_since_with(dir, after_seq, &mut memo, READ_CHUNK);
    memo.put_back(dir);
    out
}

/// How far a [`walk_segment`] read reaches past what the walk already holds, per step: a walk
/// that stops at the end of its buffer reads at least this much more before it walks on.
const READ_CHUNK: usize = 1 << 20;

/// [`read_since`] against an explicit memo and read step — the whole of it; the public entry point
/// only fetches and returns this process's memo for `dir`. Tests drive it directly, with their own
/// memo and a step small enough to cross every frame boundary.
///
/// THE EQUALITY ARGUMENT, segment by segment, against `read_all_located`'s walk of the same
/// [`segment_indices`] listing:
///
/// - a file shorter than the header or without the MAGIC is skipped by both, and a header whose
///   version does not read fails both with the same `check_version` error, in the same order;
/// - a segment that is WALKED goes through the same [`walk_frames`] (see [`walk_segment`] for why
///   reading it in steps does not change what the walk returns), every record it yields is
///   counted, and the ones with `seq > after_seq` are kept in walk order: the filter of what
///   `read_all` collects from that segment;
/// - a segment that is SKIPPED is one `SegmentMemo` holds, verified clean, with every seq
///   `<= after_seq`: `read_all` would walk it to a clean end and the filter would drop everything
///   it yields, so leaving it out changes neither the records nor the halt;
/// - the first corrupt frame stops both journal-wide, after the same warning with the same
///   `records_kept` (skipped segments contribute their remembered record count).
fn read_since_with(
    dir: &Path,
    after_seq: u64,
    memo: &mut SegmentMemo,
    step: usize,
) -> io::Result<Vec<JournalRecord>> {
    let idxs = segment_indices(dir)?;
    // A row for a segment no longer listed (pruned, or the directory set aside) is never reused.
    memo.0.retain(|idx, _| idxs.binary_search(idx).is_ok());
    let mut out = Vec::new();
    // Every record `read_all` would have collected so far, unfiltered: the warning's count.
    let mut kept = 0usize;
    for (pos, &idx) in idxs.iter().enumerate() {
        // CLOSED: a later segment is listed. Only the writer creates a segment name, and it creates
        // segment k+1 only once it has written its last frame into k (`roll`; `open` resumes the
        // HIGHEST index), so a closed segment's bytes are final before this listing saw it closed.
        let closed = pos + 1 < idxs.len();
        let path = seg_path(dir, idx);
        let mut file = File::open(&path)?;
        // Taken BEFORE the bytes are read, so a write landing during the read can only make the
        // next call's stamp differ (a re-read), never let it match bytes this call did not see.
        let meta = file.metadata().ok().and_then(|m| m.modified().ok().map(|t| (m.len(), t)));
        let mut buf = Vec::new();
        fill_to(&mut file, &mut buf, HEADER)?;
        if buf.len() < HEADER || u32::from_le_bytes(buf[0..4].try_into().unwrap()) != MAGIC {
            memo.0.remove(&idx);
            continue;
        }
        check_version(u32::from_le_bytes(buf[4..8].try_into().unwrap()))?;
        let stamp = meta.map(|(len, modified)| SegmentStamp {
            len,
            modified,
            header: buf[..HEADER].try_into().unwrap(),
        });
        if closed
            && let Some(seen) = memo.0.get(&idx)
            && stamp.as_ref() == Some(&seen.stamp)
            && seen.max_seq.is_none_or(|m| m <= after_seq)
        {
            kept += seen.records;
            continue;
        }
        let mut records = 0usize;
        let mut max_seq: Option<u64> = None;
        let (halt_at, end) = walk_segment(&mut file, &mut buf, step, |rec| {
            let seq = record_seq(&rec);
            records += 1;
            max_seq = Some(max_seq.map_or(seq, |m| m.max(seq)));
            if seq > after_seq {
                out.push(rec);
            }
        })?;
        kept += records;
        if let WalkEnd::Corrupt(cause) = end {
            // Never remembered: the next call walks this segment again and halts here again,
            // exactly as `read_all` would.
            memo.0.remove(&idx);
            CommandJournal::warn_read_halt(&path, halt_at, cause, kept);
            return Ok(out);
        }
        match stamp {
            // Only a CLOSED segment is remembered: the live one is still being appended to, and an
            // append into an already-dirty mapped page need not move its mtime.
            Some(stamp) if closed => {
                memo.0.insert(idx, VerifiedSegment { stamp, records, max_seq });
            }
            _ => {
                memo.0.remove(&idx);
            }
        }
    }
    Ok(out)
}

/// The CLOSED segments of one journal directory this process has walked to a CLEAN end, by index,
/// with what the walk found — what lets [`read_since`] leave a segment out without changing its
/// answer. One per directory, held process-wide (`MEMOS`) because `read_since` is a free function
/// its caller calls every few seconds; a call takes its directory's memo out for the duration, so
/// two concurrent calls on one directory never share one (the loser simply walks everything).
///
/// A row is reused only while the segment is still closed, its length, mtime and 16-byte header
/// are what they were when it was walked, and every seq it held is `<= after_seq`. A segment the
/// walk found CORRUPT is never a row, so the halt `read_all` would report is reported on every
/// call — skipping a corrupt segment would otherwise return records `read_all` can never reach
/// (a crash mid-append that `open` then resumed and rolled past can leave such a frame behind
/// the last record of a closed segment).
///
/// ⚠ THE ONE DIFFERENCE from `read_all` then a filter: a closed segment whose bytes change on disk
/// AFTER it was verified, with length, mtime and header unchanged (bit rot, or a writer outside
/// this crate that restores the mtime), is still left out, so a corruption it acquired is not
/// reported and the records behind it stay readable. Nothing in this crate writes a closed segment;
/// [`CommandJournal::open`] resumes only the highest index. A change that starts rewriting closed
/// segments has to drop this memo with it.
#[derive(Debug, Default)]
struct SegmentMemo(BTreeMap<u64, VerifiedSegment>);

/// What one clean walk of a closed segment found.
#[derive(Debug, Clone)]
struct VerifiedSegment {
    stamp: SegmentStamp,
    /// Every record the walk yielded (for the halt warning's `records_kept`).
    records: usize,
    /// The highest seq among them; `None` when it held none.
    max_seq: Option<u64>,
}

/// What a segment file looked like when it was walked. `modified` is the file's own timestamp,
/// never a clock read; a filesystem that reports none gets no row at all.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SegmentStamp {
    len: u64,
    modified: SystemTime,
    header: [u8; HEADER],
}

/// Every directory's [`SegmentMemo`], by the path the caller passed. A process reading more than
/// [`MEMO_DIRS`] directories starts over rather than grow without bound: losing a memo costs one
/// full walk, never a different answer.
static MEMOS: Mutex<BTreeMap<PathBuf, SegmentMemo>> = Mutex::new(BTreeMap::new());
const MEMO_DIRS: usize = 16;

impl SegmentMemo {
    /// This process's memo for `dir`, taken out of [`MEMOS`] (empty when there is none).
    fn take(dir: &Path) -> Self {
        MEMOS.lock().unwrap_or_else(PoisonError::into_inner).remove(dir).unwrap_or_default()
    }

    /// Put the memo back for the next call on `dir`.
    fn put_back(self, dir: &Path) {
        let mut memos = MEMOS.lock().unwrap_or_else(PoisonError::into_inner);
        if memos.len() >= MEMO_DIRS && !memos.contains_key(dir) {
            memos.clear();
        }
        memos.insert(dir.to_path_buf(), self);
    }
}

/// [`walk_frames`] over one segment FILE, reading it only as far as the walk needs: `buf` starts
/// with what the caller already read (the header) and grows by at least `step` bytes whenever the
/// walk stops at the END OF THE BUFFER rather than at something in the bytes.
///
/// WHY THE RESULT EQUALS ONE WALK OF THE WHOLE FILE. `walk_frames` decides each frame from the
/// bytes at and after its start, and resumes at any frame boundary. Of the ways it stops, three are
/// decided entirely by bytes already in the buffer — a zero length prefix (the clean end of a
/// written segment), a checksum mismatch and a parse failure (both come only after the length
/// check has put the whole frame inside the buffer) — and stand. The other two — fewer than 8 bytes
/// left, and a length running past the buffer (`OobLen`) — depend on where the buffer ends, so
/// the walk resumes from the SAME frame once more of the file is in. Once the file is read to its
/// end the buffer is the file, and the stop is the whole-file walk's. Records before the stopping
/// frame are yielded once, in order. A file still being appended to is read in increasing offsets,
/// as one `std::fs::read` of it is.
fn walk_segment(
    file: &mut File,
    buf: &mut Vec<u8>,
    step: usize,
    mut on_record: impl FnMut(JournalRecord),
) -> io::Result<(usize, WalkEnd)> {
    let step = step.max(1);
    let mut at = HEADER;
    let mut whole = false;
    loop {
        let (stop, end) = walk_frames(buf.as_slice(), at, &mut on_record);
        at = stop;
        if whole {
            return Ok((stop, end));
        }
        let need = match end {
            WalkEnd::Clean if stop + 8 > buf.len() => stop + 8,
            WalkEnd::Corrupt(CorruptCause::OobLen) => {
                stop + 8 + u32::from_le_bytes(buf[stop..stop + 4].try_into().unwrap()) as usize
            }
            _ => return Ok((stop, end)),
        };
        whole = fill_to(file, buf, need.max(buf.len() + step))?;
    }
}

/// Read `file` on until `buf` holds `want` bytes. `true` when the file ended first, i.e. `buf` now
/// holds all of it.
fn fill_to(file: &mut File, buf: &mut Vec<u8>, want: usize) -> io::Result<bool> {
    let more = want.saturating_sub(buf.len()) as u64;
    let got = file.by_ref().take(more).read_to_end(buf)? as u64;
    Ok(got < more)
}

/// A tiny durable checkpoint file (`<dir>/materializer.ckpt`) recording the last seq a materializer
/// has consumed, so it can resume its incremental [`read_since`] tail across process restarts. The
/// checkpoint lives beside the journal segments but is entirely independent of them: it is never
/// read by replay/restore and pruning never touches it.
pub struct MaterializeCheckpoint;

impl MaterializeCheckpoint {
    const FILE: &'static str = "materializer.ckpt";
    const TMP: &'static str = "materializer.ckpt.tmp";

    /// The stored last-materialized seq, or `0` when the file is absent or unparseable — absent ⇒
    /// start from `0`. Never errors: a materializer that finds no (or a corrupt) checkpoint simply
    /// re-materializes from the beginning of the (snapshot-compacted) journal.
    pub fn load(dir: &Path) -> u64 {
        std::fs::read_to_string(dir.join(Self::FILE))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0)
    }

    /// Whether a checkpoint file exists — i.e. materialization has started. `load` returns 0 both
    /// for "absent" and for "materialized up to seq 0", so a cold-start consumer that must include
    /// the seq-0 record (which `read_since(dir, 0)` excludes) uses this to choose a full read.
    pub fn exists(dir: &Path) -> bool {
        dir.join(Self::FILE).exists()
    }

    /// Durably overwrite the checkpoint with `seq` using the standard crash-safe write-temp →
    /// fsync → atomic-rename pattern: a torn write leaves the old checkpoint intact, never a
    /// half-written seq. `sync_all` is the plain-file fsync twin of the journal's `map.flush`
    /// (msync). `std::fs::rename` replaces the destination atomically on both Unix and Windows
    /// (`MoveFileEx` with replace-existing).
    pub fn store(dir: &Path, seq: u64) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(Self::TMP);
        {
            let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
            use std::io::Write;
            f.write_all(seq.to_string().as_bytes())?;
            f.sync_all()?; // fsync the bytes before the rename makes them the live checkpoint
        }
        std::fs::rename(&tmp, dir.join(Self::FILE))
    }
}

#[path = "read_tests.rs"]
#[cfg(test)]
mod read_tests;

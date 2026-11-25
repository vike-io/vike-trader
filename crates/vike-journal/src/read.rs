//! The offline reader half: [`read_since`] (the incremental tail-read) and [`read_durable_since`] (the
//! same read bounded to what is durable, the execution stream's tap). Neither takes the journal
//! directory's writer lock. Split out of `journal.rs`.
//!
//! `read_since` is NOT `read_all` plus a filter any more, only equal to one: it walks the segments
//! itself so it can leave out what cannot change its answer — a CLOSED segment this process
//! already walked clean whose every seq is `<= after_sequence` (`SegmentMemo`), and the zero-filled
//! unwritten end of every segment it does read (`walk_segment`). Its result, its errors and its
//! halt warning are `read_all`'s, filtered; the one exception is stated on `SegmentMemo`.
//!
//! No reader here writes the store: the tradehub's publisher sends what [`read_durable_since`] returns
//! to the datahub, the store's one writer process
//! (`docs/decisions/0084-only-the-datahub-touches-the-store.md`, decision 0295).
//!
//! [`JournalExecutionStream`] (`<dir>/execution_stream.json`) is the execution stream's identity and
//! floor: `writer/offline.rs`'s `prune_before_latest_snap` keeps every segment holding records past
//! its `acknowledged_sequence` while the file exists, so the file stays in the journal directory and
//! its name and format stay this crate's.
//!
//! [`read_durable_since`] is the execution stream's tap: `read_since` bounded to what a
//! [`JournalDurableWatch`] says is durable, at most `limit` records a call, each walk resuming where
//! the last one stopped; its cursor is an `Option`, `None` reading from sequence 0.
//! [`read_closed_since`] is the same paged walk over a directory nothing writes any more (a journal
//! moved aside), with the halt it met.
//!
//! Every reader here READS a JSON journal of formats 31..=33 too (`crate::format`'s `MIN_READ_ONLY_VERSION`): the
//! step to the binary format moves such a journal aside, and the execution stream still drains it with this build.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use super::format::{HEADER, MAGIC, check_read_only_version};
use super::frame::{JournalCorruptCause, JournalWalkEnd, walk_frames_until};
use super::record::{JournalRecord, record_sequence};
use super::segment::{segment_indices, segment_path};
use super::sync::{JournalDurableMark, JournalDurableWatch};
use super::writer::{Journal, JournalReadHalt};

/// Every valid record with `seq > after_sequence`, in seq order — the incremental tail-read a reader
/// uses to consume only records past where it last stopped. `after_sequence` is the last seq already
/// CONSUMED and the bound is STRICT (exclusive), which is the resume semantic a cursor needs.
///
/// The RESULT is [`Journal::read_all`] filtered by [`record_sequence`] `> after_sequence`, and so are
/// the failure modes: the same records in the same order, the same `Err` for a segment whose
/// format version this build does not read, the same journal-wide stop at the first corrupt frame
/// (the valid prefix, filtered, and the same one warning with the same `records_kept`). The one
/// case that can differ is stated on `SegmentMemo`: bytes of an already-verified CLOSED segment
/// changing on disk with its length, mtime and header unchanged.
///
/// Note the lower bound is strict: `after_sequence == 0` returns every record with `seq > 0`. Because
/// this journal assigns the FIRST record seq 0, a caller that must include it uses
/// [`read_durable_since`]'s `None` cursor rather than `read_since(dir, 0)`.
///
/// COST. It used to be `read_all` then a filter: every call read every segment file whole (each
/// pre-allocated, 64 MiB by default) and deserialized every record, and its doc called that
/// bounded because the prune compacts the journal. That stopped being true when a reader (the journal
/// materializer, until 2026-11-18; the execution stream's publisher now) started calling it every few
/// seconds for the trade daemon's whole uptime: the prune
/// ([`Journal::prune_before_latest_snap`]) ran only before the trade daemon opened the
/// journal, so the segment count grew for as long as it ran (since decision 0304 it runs beside the live core and
/// keeps the live directory to about `config.journal_keep_hours` of segments). Now a call costs:
///
/// - one directory listing, and an open plus a 16-byte header read per segment;
/// - a full walk (read and deserialize) of every segment that can hold a record past `after_sequence`
///   or has not been verified yet — in the steady state, the one live segment the writer is
///   appending to — and of a corrupt segment, every time, because a halt is never remembered;
/// - only the WRITTEN bytes of each segment walked, plus at most `READ_CHUNK`: the zero-filled
///   pre-allocated end is not read (`walk_segment`).
///
/// The first call in a process verifies every segment once, which is what every call cost before.
pub fn read_since(dir: &Path, after_sequence: u64) -> io::Result<Vec<JournalRecord>> {
    let mut memo = SegmentMemo::take(dir);
    let out = read_since_with(dir, after_sequence, &mut memo, READ_CHUNK);
    memo.put_back(dir);
    out
}

/// The execution stream's tap (plan C.2): [`read_since`]'s records past `after_sequence`, at most
/// `limit` of them, stopping at the first frame outside what `watch` says is durable (a frame that
/// ENDS past its mark, or anything in a later segment). So a record it returns is one a power cut
/// cannot take back, and its sequence is never handed to another record.
///
/// THE CURSOR IS AN `Option`, unlike `read_since`'s: `None` (nothing acknowledged yet, the stream file's
/// `acknowledged_sequence` as it is) reads from the FIRST record, sequence 0 included, and `Some(n)` is exclusive
/// as `read_since`'s `n` is. A fresh journal's first record is sequence 0 and can be one the stream owes (a boot's
/// `ExecutionReportsUnverified` in the directory a broken journal was moved out of), so "since 0" would drop it.
///
/// It reads the journal's FILES, on the caller's thread, against a LIVE directory (no lock), and
/// never waits on the journal. Within the durable range its answer is `read_since`'s, filtered and
/// cut at `limit`, the halt and its warning included; a frame running past the mark is the end of
/// the range, said nothing about. A record the sync thread has made durable and the write thread
/// has not yet handed to the working file is simply returned by a later call.
///
/// COST. It shares `read_since`'s memo, so a closed segment whose records are all `<= after_sequence`
/// is not read again, and it remembers where its last walk stopped (`ResumePoint`): a call whose
/// `after_sequence` is at or past that point's records starts there instead of at the segment's
/// first frame. So a poll after a poll, and each step of a catch-up `limit` records at a time, walk
/// only what is new, and READ only that, the header and the 8-byte head that checks the point.
pub fn read_durable_since(
    dir: &Path,
    after_sequence: Option<u64>,
    watch: &JournalDurableWatch,
    limit: usize,
) -> io::Result<Vec<JournalRecord>> {
    let mut memo = SegmentMemo::take(dir);
    let out =
        read_window(dir, after_sequence, Some(watch.durable_mark()), limit, &mut memo, READ_CHUNK);
    memo.put_back(dir);
    out
}

/// **A journal directory nothing writes any more** (one the boot moved aside), a page at a time: the
/// records past `after_sequence` (`None`: from the first, sequence 0 included), at most `limit`, and
/// where the read HALTED when the page ends at a frame that does not read (`None` at a clean end or
/// at `limit`). It is [`read_durable_since`]'s walk with every segment durable, so each page resumes
/// where the last one stopped: paging through a journal reads it once and holds one page, never the
/// whole journal (`Journal::read_all_located` does, once per call).
pub fn read_closed_since(
    dir: &Path,
    after_sequence: Option<u64>,
    limit: usize,
) -> io::Result<(Vec<JournalRecord>, Option<JournalReadHalt>)> {
    let whole = JournalDurableMark { segment_index: u64::MAX, offset: usize::MAX };
    let mut memo = SegmentMemo::take(dir);
    let mut halt = None;
    let out = read_window_reporting(
        dir,
        after_sequence,
        Some(whole),
        limit,
        &mut memo,
        READ_CHUNK,
        Some(&mut halt),
    );
    memo.put_back(dir);
    Ok((out?, halt))
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
///   version does not read fails both with the same `check_read_only_version` error, in the same order (both READ a
///   JSON journal a format step moved aside);
/// - a segment that is WALKED goes through the same `walk_frames` (see [`walk_segment`] for why
///   reading it in steps does not change what the walk returns), every record it yields is
///   counted, and the ones with `seq > after_sequence` are kept in walk order: the filter of what
///   `read_all` collects from that segment;
/// - a segment that is SKIPPED is one `SegmentMemo` holds, verified clean, with every seq
///   `<= after_sequence`: `read_all` would walk it to a clean end and the filter would drop everything
///   it yields, so leaving it out changes neither the records nor the halt;
/// - the first corrupt frame stops both journal-wide, after the same warning with the same
///   `records_kept` (skipped segments contribute their remembered record count).
fn read_since_with(
    dir: &Path,
    after_sequence: u64,
    memo: &mut SegmentMemo,
    step: usize,
) -> io::Result<Vec<JournalRecord>> {
    read_window(dir, Some(after_sequence), None, usize::MAX, memo, step)
}

/// [`read_since_with`], bounded for [`read_durable_since`] when `until` is set: no frame that ends
/// past the mark and no later segment, at most `limit` records, and each segment's walk resumes at
/// the memo's [`ResumePoint`] when that point's records are all `<= after_sequence`. With `until`
/// `None` and `limit` `usize::MAX` it is exactly `read_since`'s walk: no resume point is read or
/// written, so the equality argument above stands as it was. `after_sequence` `None` keeps every record
/// (nothing is `<=` it); `read_since` always passes `Some`.
fn read_window(
    dir: &Path,
    after_sequence: Option<u64>,
    until: Option<JournalDurableMark>,
    limit: usize,
    memo: &mut SegmentMemo,
    step: usize,
) -> io::Result<Vec<JournalRecord>> {
    read_window_reporting(dir, after_sequence, until, limit, memo, step, None)
}

/// [`read_window`], and, when `halt` is given, the [`JournalReadHalt`] of the corrupt frame the walk
/// stopped at (left `None` at a clean end or at `limit`).
fn read_window_reporting(
    dir: &Path,
    after_sequence: Option<u64>,
    until: Option<JournalDurableMark>,
    limit: usize,
    memo: &mut SegmentMemo,
    step: usize,
    halt: Option<&mut Option<JournalReadHalt>>,
) -> io::Result<Vec<JournalRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let idxs = segment_indices(dir)?;
    // A row for a segment no longer listed (pruned, or the directory set aside) is never reused.
    memo.0.retain(|index, _| idxs.binary_search(index).is_ok());
    let mut out = Vec::new();
    // Every record `read_all` would have collected so far, unfiltered: the warning's count.
    let mut kept = 0usize;
    for (pos, &index) in idxs.iter().enumerate() {
        // The durable range: every segment below the mark's whole, its own up to the mark, no later
        // one. `usize::MAX`: the whole segment.
        let cap = match until {
            Some(mark) if index > mark.segment_index => break,
            Some(mark) if index == mark.segment_index => mark.offset,
            _ => usize::MAX,
        };
        // CLOSED: a later segment is listed. Only the writer creates a segment name, and it creates
        // segment k+1 only once it has written its last frame into k (`roll`; `open` resumes the
        // HIGHEST index), so a closed segment's bytes are final before this listing saw it closed.
        let closed = pos + 1 < idxs.len();
        let path = segment_path(dir, index);
        let mut file = match File::open(&path) {
            Ok(file) => file,
            // Moved to the archive after the listing (`Journal::prune_before_latest_snap`, beside a live core): a
            // segment below the newest durable `Snapshot`'s, as if it had not been listed.
            Err(e) if closed && e.kind() == io::ErrorKind::NotFound => {
                memo.0.remove(&index);
                continue;
            }
            Err(e) => return Err(e),
        };
        // Taken BEFORE the bytes are read, so a write landing during the read can only make the
        // next call's stamp differ (a re-read), never let it match bytes this call did not see.
        let meta = file.metadata().ok().and_then(|m| m.modified().ok().map(|t| (m.len(), t)));
        let mut buffer = Vec::new();
        fill_to(&mut file, &mut buffer, HEADER)?;
        if buffer.len() < HEADER || u32::from_le_bytes(buffer[0..4].try_into().unwrap()) != MAGIC {
            memo.0.remove(&index);
            continue;
        }
        check_read_only_version(u32::from_le_bytes(buffer[4..8].try_into().unwrap()))?;
        let stamp = meta.map(|(len, modified)| SegmentStamp {
            len,
            modified,
            header: buffer[..HEADER].try_into().unwrap(),
        });
        if closed
            && let Some(seen) = memo.0.get(&index)
            && stamp.as_ref() == Some(&seen.stamp)
            && seen.max_sequence.is_none_or(|m| after_sequence.is_some_and(|a| m <= a))
        {
            kept += seen.records;
            continue;
        }
        // Frames before the resume point hold nothing past `after_sequence`: start after them, once
        // the head of the last of them still reads as it did.
        let mut resume = memo.1.clone().filter(|r| {
            until.is_some()
                && r.index == index
                && after_sequence.is_some_and(|a| r.max_sequence <= a)
                && r.offset <= cap
        });
        // Only those 8 bytes are read, at their offset: copying the segment up to the resume point to
        // look at them was a whole read of the live segment on every poll.
        if let Some(r) = &resume {
            let mut head = Vec::new();
            file.seek(SeekFrom::Start(r.frame_start as u64))?;
            fill_to(&mut file, &mut head, 8)?;
            if head != r.frame_head {
                resume = None;
            }
        }
        // `buffer` holds the file from `base` on: from byte 0 (the header read above) for a walk from
        // the first frame, from the resume point (nothing of it read yet) for a resumed one.
        let base = resume.as_ref().map_or(0, |r| r.offset);
        if base > 0 {
            buffer.clear();
        }
        file.seek(SeekFrom::Start((base + buffer.len()) as u64))?;
        let (mut records, mut max_sequence, start) =
            resume.map_or((0, None, HEADER), |r| (r.records, Some(r.max_sequence), r.offset));
        // The last record's frame, `(start, end)`: the next resume point.
        let mut last: Option<(usize, usize)> = None;
        let mut frame_start = start;
        let (halt_at, end) =
            walk_segment(&mut file, &mut buffer, base, step, start, cap, |rec, ends| {
                let seq = record_sequence(&rec);
                records += 1;
                max_sequence = Some(max_sequence.map_or(seq, |m| m.max(seq)));
                last = Some((frame_start, ends));
                frame_start = ends;
                if after_sequence.is_none_or(|a| seq > a) {
                    out.push(rec);
                }
                out.len() < limit
            })?;
        kept += records;
        if until.is_some()
            && let (Some((frame_start, offset)), Some(max_sequence)) = (last, max_sequence)
        {
            memo.1 = Some(ResumePoint {
                index,
                frame_start,
                frame_head: buffer[frame_start - base..frame_start - base + 8].try_into().unwrap(),
                offset,
                records,
                max_sequence,
            });
        }
        if let JournalWalkEnd::Corrupt(cause) = end {
            // Never remembered: the next call walks this segment again and halts here again,
            // exactly as `read_all` would.
            memo.0.remove(&index);
            // The live segment's end may be a frame the write thread is still writing: stop before
            // it, silently (`crate::frame::is_pending_tail`). A frame running past the durable mark
            // is the end of the range, silently too.
            let past_the_mark = cap != usize::MAX && cause == JournalCorruptCause::OobLen;
            if !past_the_mark && !crate::frame::is_pending_tail(!closed, cause) {
                Journal::warn_read_halt(&path, halt_at, cause, kept);
            }
            if let Some(halt) = halt {
                let in_tail =
                    idxs[pos + 1..].iter().all(|&later| Journal::holds_no_record(dir, later));
                *halt = Some(JournalReadHalt {
                    cause,
                    segment: path,
                    segment_index: index,
                    offset: halt_at,
                    in_tail,
                });
            }
            return Ok(out);
        }
        if out.len() >= limit || cap != usize::MAX {
            // Not walked to its end (the limit, or the durable mark): nothing to remember of it,
            // and nothing after it is read.
            memo.0.remove(&index);
            return Ok(out);
        }
        match stamp {
            // Only a CLOSED segment is remembered: the live one is still being appended to, and an
            // append into an already-dirty mapped page need not move its mtime.
            Some(stamp) if closed => {
                memo.0.insert(index, VerifiedSegment { stamp, records, max_sequence });
            }
            _ => {
                memo.0.remove(&index);
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
/// are what they were when it was walked, and every seq it held is `<= after_sequence`. A segment the
/// walk found CORRUPT is never a row, so the halt `read_all` would report is reported on every
/// call — skipping a corrupt segment would otherwise return records `read_all` can never reach
/// (a build from before `crates/vike-journal/src/writer.rs`'s `close_dirty_tail` could resume over
/// a crash's bytes and roll past them, leaving such a frame behind the last record of a closed
/// segment).
///
/// ⚠ THE ONE DIFFERENCE from `read_all` then a filter: a closed segment whose bytes change on disk
/// AFTER it was verified, with length, mtime and header unchanged (bit rot, or a writer outside
/// this crate that restores the mtime), is still left out, so a corruption it acquired is not
/// reported and the records behind it stay readable. Nothing in this crate writes a closed segment;
/// [`Journal::open`] resumes only the highest index. A change that starts rewriting closed
/// segments has to drop this memo with it.
///
/// Its second half is [`read_durable_since`]'s alone: where that reader's last walk stopped.
#[derive(Debug, Default)]
struct SegmentMemo(BTreeMap<u64, VerifiedSegment>, Option<ResumePoint>);

/// The frame boundary after the last record a [`read_durable_since`] walk yielded, in segment
/// `index`, with what the walk had counted up to it. A later walk of that segment for records past
/// `max_sequence` (or later) starts there, unless the 8-byte head (length and check) of that last
/// frame no longer reads the same: a directory replaced under the same path, or a rewritten segment,
/// then walks from the first frame. Bytes before a frame the walk read are final while the journal
/// writes on (only bytes past the last frame are ever written).
#[derive(Debug, Clone)]
struct ResumePoint {
    index: u64,
    frame_start: usize,
    frame_head: [u8; 8],
    offset: usize,
    /// Records the walk yielded in this segment up to `offset`, and the highest seq among them.
    records: usize,
    max_sequence: u64,
}

/// What one clean walk of a closed segment found.
#[derive(Debug, Clone)]
struct VerifiedSegment {
    stamp: SegmentStamp,
    /// Every record the walk yielded (for the halt warning's `records_kept`).
    records: usize,
    /// The highest seq among them; `None` when it held none.
    max_sequence: Option<u64>,
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

/// [`walk_frames_until`] over one segment FILE, reading it only as far as the walk needs: `buffer` starts
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
///
/// For [`read_window`]: the walk starts at `start` (a frame boundary), sees the file only up to
/// `cap` (as if it ended there; `usize::MAX` for the whole file), and stops right after a record
/// `on_record` (handed the record and its frame's end) answers `false` for.
///
/// `buffer` holds the file from offset `base` on (`base <= start`, the file positioned at its end),
/// so a resumed walk reads only what lies past its resume point. Every offset in and out is the
/// FILE's; the walk itself sees `buffer` as the file with its first `base` bytes cut off, and a
/// frame walk decides each frame from its own bytes only, wherever it starts.
fn walk_segment(
    file: &mut File,
    buffer: &mut Vec<u8>,
    base: usize,
    step: usize,
    start: usize,
    cap: usize,
    mut on_record: impl FnMut(JournalRecord, usize) -> bool,
) -> io::Result<(usize, JournalWalkEnd)> {
    let step = step.max(1);
    let cap = cap.saturating_sub(base);
    let mut at = start - base;
    let mut whole = buffer.len() >= cap;
    let mut stopped = false;
    loop {
        let view = &buffer[..buffer.len().min(cap)];
        let (stop, end) = walk_frames_until(view, at, |rec, ends| {
            stopped = !on_record(rec, base + ends);
            !stopped
        });
        at = stop;
        if whole || stopped {
            return Ok((base + stop, end));
        }
        let need = match end {
            JournalWalkEnd::Clean if stop + 8 > view.len() => stop + 8,
            JournalWalkEnd::Corrupt(JournalCorruptCause::OobLen) => {
                stop + 8 + u32::from_le_bytes(view[stop..stop + 4].try_into().unwrap()) as usize
            }
            _ => return Ok((base + stop, end)),
        };
        whole =
            fill_to(file, buffer, need.max(buffer.len() + step).min(cap))? || buffer.len() >= cap;
    }
}

/// Read `file` on until `buffer` holds `want` bytes. `true` when the file ended first, i.e. `buffer` now
/// holds all of it.
fn fill_to(file: &mut File, buffer: &mut Vec<u8>, want: usize) -> io::Result<bool> {
    let more = want.saturating_sub(buffer.len()) as u64;
    let got = file.by_ref().take(more).read_to_end(buffer)? as u64;
    #[cfg(test)]
    BYTES_READ.with(|n| n.set(n.get() + got));
    Ok(got < more)
}

// Every byte `fill_to` read on this thread: what the tests measure a poll's cost by.
#[cfg(test)]
thread_local! { static BYTES_READ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }

/// The `now_ms` every record carries: the journal's clock when it was written. The prune's hold
/// (`EXECUTION_STREAM_HOLD_MS`) measures segments by it, and the execution stream sends it as a
/// record's `recorded_ms`.
pub fn record_now_ms(rec: &JournalRecord) -> i64 {
    match rec {
        JournalRecord::Command { now_ms, .. }
        | JournalRecord::Snapshot { now_ms, .. }
        | JournalRecord::StrategySubmit { now_ms, .. }
        | JournalRecord::MintedSubmit { now_ms, .. }
        | JournalRecord::PortfolioSnapshot { now_ms, .. }
        | JournalRecord::OrderEmulationArmed { now_ms, .. }
        | JournalRecord::OrderEmulationFire { now_ms, .. }
        | JournalRecord::OrderEmulationDisarmed { now_ms, .. }
        | JournalRecord::MarginCallLiquidate { now_ms, .. }
        | JournalRecord::GtdExpire { now_ms, .. }
        | JournalRecord::ScheduleFire { now_ms, .. }
        | JournalRecord::SharedCloseTimeout { now_ms, .. }
        | JournalRecord::StrategyFileMounted { now_ms, .. }
        | JournalRecord::Forecast { now_ms, .. }
        | JournalRecord::ExecutionReport { now_ms, .. }
        | JournalRecord::ExecutionReportsUnverified { now_ms, .. }
        | JournalRecord::PositionSnapshot { now_ms, .. }
        | JournalRecord::DeniedRepeat { now_ms, .. } => *now_ms,
    }
}

/// `<journal_dir>/execution_stream.json`: the journal's identity on the execution stream (the tradehub's
/// publisher sends this journal's executions to the datahub under `journal_id`) and its prune FLOOR, the last
/// sequence the datahub acknowledged as stored. The tradehub owns the file; the datahub never touches the journal
/// directory. It lives WITH the directory: a move aside takes it along and a copy carries it (the publisher's
/// divergence check catches the copy). `writer/offline.rs`'s `prune_before_latest_snap` keeps every segment holding
/// a record past `acknowledged_sequence` (`None` keeps them all), up to `EXECUTION_STREAM_HOLD_MS`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JournalExecutionStream {
    /// 32 lowercase hex, minted once by [`JournalExecutionStream::mint_journal_id`].
    pub journal_id: String,
    /// The last sequence the datahub said it stored; `None` until the first acknowledgement.
    pub acknowledged_sequence: Option<u64>,
    /// That record's `now_ms`; `None` until the first acknowledgement.
    pub acknowledged_ms: Option<i64>,
    /// Journals moved aside before this one that still owe the datahub their readable records.
    #[serde(default)]
    pub predecessors: Vec<JournalExecutionStreamPredecessor>,
}

/// A journal directory moved aside (`Journal::set_aside`) whose records past its floor the publisher still sends,
/// under its own id, before it marks it `done`. The moved directory keeps its own stream file too.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JournalExecutionStreamPredecessor {
    pub journal_id: String,
    /// Where the directory went.
    pub path: PathBuf,
    /// Its floor when it was moved.
    pub acknowledged_sequence: Option<u64>,
    #[serde(default)]
    pub done: bool,
}

impl JournalExecutionStream {
    pub const FILE: &'static str = "execution_stream.json";

    /// **The one journal-identity mint**: 16 random bytes as 32 lowercase hex, the shape
    /// `crates/vike-datahub-client/src/execution_stream.rs`'s `validate_journal_id` checks. The boot's move aside
    /// (vike-core) and the publisher (vike-tradehub, through [`Self::load_or_create`]) both call it. std only: two
    /// `RandomState` hashers, whose keys std draws from the OS once per thread and steps for each later one, so no
    /// RNG crate enters this crate or the determinism-critical core (`crates/vike-ops/tests/architecture/clock_pin.rs`'s
    /// `DETERMINISM_CRITICAL_CRATES`). Not crypto-grade, and it need not be: an identity is no secret, it only has to
    /// differ between journals.
    pub fn mint_journal_id() -> String {
        use std::hash::{BuildHasher, Hasher};
        let half = || {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u32(std::process::id());
            h.finish().to_le_bytes()
        };
        vike_utility::hex::hex_lower(&[half(), half()].concat())
    }

    /// The file in `dir`, or `None` when it (or the directory) does not exist. A file that exists and does not
    /// read is an ERROR, never "no stream": a fresh identity would hide the floor the datahub already holds.
    pub fn load(dir: &Path) -> io::Result<Option<Self>> {
        let path = dir.join(Self::FILE);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not a journal execution stream file: {e}", path.display()),
                )
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The file in `dir`; when there is none, a new one under the id `mint` returns, written before it is returned.
    pub fn load_or_create(dir: &Path, mint: impl FnOnce() -> String) -> io::Result<Self> {
        if let Some(stream) = Self::load(dir)? {
            return Ok(stream);
        }
        let stream = Self {
            journal_id: mint(),
            acknowledged_sequence: None,
            acknowledged_ms: None,
            predecessors: Vec::new(),
        };
        stream.store(dir)?;
        Ok(stream)
    }

    /// Durably replace the file through `vike_utility::atomic_file::write_atomic` (temp, fsync, replace: a plain
    /// `rename` onto an existing file fails on Windows), so a torn write leaves the old floor, never half a file.
    pub fn store(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        vike_utility::atomic_file::write_atomic(&dir.join(Self::FILE), &bytes)
    }
}

#[path = "read_tests.rs"]
#[cfg(test)]
mod read_tests;

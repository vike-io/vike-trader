//! [`Journal`] — the single-writer append side: open/resume, framing, the segment roll, the
//! flush barrier and the `Drop` that stops the journal's threads live in this file. The `append_*`
//! verbs are `writer/append.rs`'s and the offline readers and prune (`read_all`,
//! `latest_segment_version`, `prune_before_latest_snap`) are `writer/offline.rs`'s — each an
//! `impl Journal` block over the private state this file owns.
//!
//! ⚠ This is the FOLD-PATH type. The asynchronous journal (decision 0279): after [`Journal::open`] it touches NO segment file. Every frame goes into an
//! anonymous STAGE laid out like the segment, and the position is published with one Release store;
//! the WRITE thread (`handoff.rs`) hands the frames to the kernel with `pwrite` and the SYNC thread
//! (`sync.rs`) makes them durable. A roll swaps the stage and sends the write thread one message; a
//! header stamp is one message. The one wait the fold may run is an order's [`WriteFence::wait`],
//! from `vike_execution::Engine::fence_send`. [`Journal::flush`] is a barrier for tools and
//! tests, NOT for the fold.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use memmap2::{MmapMut, MmapRaw};

use super::frame::{JournalCorruptCause, JournalWalkEnd, fnv1a32, walk_frames};
use super::handoff::{Segment, ToWriter, WriteFence, WriteThread, pos};
use super::record::record_sequence;
use super::segment::{
    PREP_WATERMARK_PERCENT, prep_path, reserve_blocks, segment_path, warm_ahead_raw,
};
use super::sync::{JournalSyncShared, JournalSyncer, sync_chunk_bytes};
use crate::format::{BASE_VERSION, HEADER, MAGIC, check_version};
use crate::lock::JournalLock;

mod append;
mod offline;
pub use offline::{JournalReadHalt, TORN_DIR};

#[derive(Debug, Clone)]
pub struct JournalFileConfig {
    pub segment_bytes: u64,
    /// No longer read: it paced the per-`flush_every` `msync(MS_ASYNC)` of the mapped write, which
    /// the asynchronous journal does not have. Kept so every config literal still compiles.
    pub flush_every: u32,
}
impl Default for JournalFileConfig {
    fn default() -> Self {
        JournalFileConfig { segment_bytes: 64 * 1024 * 1024, flush_every: 256 }
    }
}

pub struct Journal {
    dir: PathBuf,
    config: JournalFileConfig,
    /// Where `write_framed` puts every frame: ANONYMOUS memory laid out like the live segment, offset
    /// for offset (a rolled segment's header included). Shared with the write and sync threads.
    stage: Arc<MmapRaw>,
    /// The live segment's ordinal in this process (0 = the one `open` resumed or made): the high half
    /// of every published position (`handoff::pos`).
    ordinal: u64,
    pub(super) segment_index: u64,
    pub(super) cursor: usize,
    /// The format version the live segment's header carries right now (see
    /// [`crate::format::BASE_VERSION`]): raised by `require_version`, carried across a roll, never
    /// lowered.
    version: u32,
    sequence: u64,
    /// Offset up to which a sync has been REQUESTED of the sync thread — deliberately NOT one that
    /// has completed (that is `JournalSyncShared::synced_to`, which the appender never reads and never
    /// waits on). `queued_to..cursor` is the region no sync has been asked for yet;
    /// [`sync_chunk_bytes`] bounds how large it is allowed to get before one is.
    pub(super) queued_to: usize,
    /// This segment's un-synced debt ceiling (see [`sync_chunk_bytes`]).
    pub(super) sync_chunk: usize,
    /// The duplicate-instance interlock on `dir`, held for this journal's whole lifetime and
    /// released on drop (see [`crate::lock`]), after `Drop` has stopped both threads.
    _lock: JournalLock,
    /// In-flight preparation of the NEXT segment (see `prepare_next`): its file, which the write
    /// thread renames into place, and the warmed stage the core goes on in. Joined by
    /// [`Self::roll`].
    prep: Option<std::thread::JoinHandle<Option<MmapRaw>>>,
    /// The watermarks the core, the write thread and the sync thread share.
    shared: Arc<JournalSyncShared>,
    /// The write thread. `Option` only so `Drop` can stop it; always `Some` while the journal lives.
    writer: Option<WriteThread>,
    /// The sync thread. `Option` only so `Drop` can stop it AFTER the write thread.
    syncer: Option<JournalSyncer>,
}

impl Journal {
    /// Open (or resume) the journal directory `dir` as its EXCLUSIVE writer.
    ///
    /// Takes the duplicate-instance lock FIRST — before any segment is mapped — so a second
    /// instance fails fast with a clear [`io::ErrorKind::AddrInUse`] error naming the directory
    /// instead of silently interleaving WAL frames into a live journal. The lock is held until
    /// this value drops. Read-only entry points (`read_all`, `latest_segment_version`,
    /// `read_since`, `prune_before_latest_snap`) are unaffected — they never lock.
    ///
    /// It resumes the highest segment at the end of its readable frames when nothing lies past them;
    /// otherwise it closes that segment and goes on in a new one (`close_dirty_tail`). This is the
    /// one place the core thread maps a segment file: session start, to read it.
    pub fn open(dir: &Path, config: JournalFileConfig) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        // Duplicate-instance interlock. Acquired before the segment scan/map so a refused second
        // instance never touches (let alone extends or restamps) the live journal's files. On the
        // error paths below the guard drops here and releases — a failed open leaves no lock held.
        let lock = JournalLock::acquire(dir)?;
        // A power cut may have left a working segment shorter than its durable copy: repair every
        // one BEFORE anything below reads a segment (`durable.rs`).
        for note in crate::durable::repair_from_durable_copies(dir)? {
            tracing::warn!(dir = %dir.display(), "{note}");
        }
        // resume: find the highest existing segment, else create segment 0
        let mut max_index: Option<u64> = None;
        for e in std::fs::read_dir(dir)? {
            let name = e?.file_name().to_string_lossy().into_owned();
            if let Some(n) = name.strip_prefix("journal-").and_then(|s| s.strip_suffix(".vjl"))
                && let Ok(i) = n.parse::<u64>()
            {
                max_index = Some(max_index.map_or(i, |m: u64| m.max(i)));
            }
        }
        let (segment_index, first_sequence) = match max_index {
            Some(index) => (index, Self::first_sequence_if_unstamped(dir, index)?),
            None => (0, 0),
        };
        let (mut map, file, version) =
            Self::map_segment(dir, segment_index, first_sequence, &config, BASE_VERSION)?;
        // scan existing records to find cursor + next seq (validates as it goes)
        let mut seq = u64::from_le_bytes(map[8..16].try_into().unwrap());
        let (cursor, end) = walk_frames(&map, HEADER, |rec| seq = record_sequence(&rec) + 1);
        // A directory with no segment has no tail to read.
        let dirty =
            max_index.is_some() && close_dirty_tail(&mut map, cursor, &end, dir, segment_index);
        let segment_len = map.len();
        // From here on the core holds no mapping of any segment file.
        drop(map);
        // The resumed bytes must be durable in the working file itself: the new durable copy holds
        // nothing below the resume point. Session start, once (cheap after a clean stop, which
        // synced it already).
        file.sync_all()?;
        let stage = Arc::new(MmapRaw::from(MmapMut::map_anon(segment_len)?));
        warm_ahead_raw(&stage, cursor);
        // Every watermark starts at the resumed cursor: those bytes are in the file already (and
        // durable), so no thread owes anything for them.
        let shared = Arc::new(JournalSyncShared::default());
        for w in [&shared.appended, &shared.written, &shared.target] {
            w.store(pos(0, cursor), Ordering::Release);
        }
        let syncer = JournalSyncer::spawn(
            dir.to_path_buf(),
            segment_index,
            Arc::clone(&stage),
            cursor,
            Arc::clone(&shared),
        )?;
        let writer = WriteThread::spawn(
            Segment {
                ordinal: 0,
                index: segment_index,
                file,
                stage: Arc::clone(&stage),
                off: cursor,
            },
            Arc::clone(&shared),
            syncer.sender(),
            dir.to_path_buf(),
            config.clone(),
        )?;
        let mut j = Journal {
            dir: dir.to_path_buf(),
            sync_chunk: sync_chunk_bytes(config.segment_bytes),
            config,
            stage,
            ordinal: 0,
            segment_index,
            cursor,
            version,
            sequence: seq,
            // Re-base the sync debt onto the RESUMED cursor: starting at HEADER would make this
            // session's first chunk sync re-cover the whole resumed prefix.
            queued_to: cursor,
            _lock: lock,
            prep: None,
            shared,
            writer: Some(writer),
            syncer: Some(syncer),
        };
        if dirty {
            j.roll()?;
        }
        Ok(j)
    }

    /// Map segment `index` for `open` to read (session start, never the append path), creating it
    /// sized and block-reserved when it is new, and stamp or restamp its header.
    ///
    /// `floor` is the format version the segment is stamped with at least: a fresh segment gets
    /// `floor`; a resumed one gets the higher of what it carries and `floor`.
    ///
    /// One of this journal's audited unsafe sites (`MmapMut::map_mut`): the crate lint is
    /// `unsafe_code = "deny"`, and this per-fn `#[expect(unsafe_code)]` is a named carve-out.
    #[expect(unsafe_code)]
    fn map_segment(
        dir: &Path,
        index: u64,
        first_sequence: u64,
        config: &JournalFileConfig,
        floor: u32,
    ) -> io::Result<(MmapMut, File, u32)> {
        // Invariant: `map[0..4]` (magic) and `map[8..16]` (first_sequence) below assume the segment
        // is at least HEADER bytes; a tiny misconfigured `segment_bytes` would otherwise panic
        // on an out-of-range slice. Fail loudly in debug/test rather than silently clamping.
        debug_assert!(
            config.segment_bytes >= HEADER as u64,
            "segment_bytes must be >= HEADER (16)"
        );
        let path = segment_path(dir, index);
        // truncate(false) is load-bearing: reopening an existing segment MUST keep its bytes so
        // the resume scan can find the prior records; truncating would wipe the journal.
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        if file.metadata()?.len() < config.segment_bytes {
            file.set_len(config.segment_bytes)?; // size it: appends never extend the file
        }
        // ...and materialize its BLOCKS (`reserve_blocks`), so the write thread's `pwrite`s never
        // pay a filesystem allocation either.
        reserve_blocks(&file, config.segment_bytes);
        // SAFETY: `MmapMut::map_mut` is unsafe by signature — the caller promises the file is not
        // resized/truncated by another handle while mapped. This journal owns `path` exclusively
        // (the directory lock), the mapping lives only for `open`'s scan, and nothing resizes the
        // file meanwhile, so the contract holds.
        let mut map = unsafe { MmapMut::map_mut(&file)? };
        let version = if u32::from_le_bytes(map[0..4].try_into().unwrap()) != MAGIC {
            // fresh (zero-filled) segment — stamp magic | version | first_sequence
            map[0..4].copy_from_slice(&MAGIC.to_le_bytes());
            map[4..8].copy_from_slice(&floor.to_le_bytes());
            map[8..16].copy_from_slice(&first_sequence.to_le_bytes());
            floor
        } else {
            // Resuming an EXISTING segment. A version this build cannot read (too old, or FORWARD
            // of it) still fails loudly — appending frames next to an incompatible serde shape
            // would interleave records no single reader can walk. A SUPPORTED-but-older version is
            // UPGRADED IN PLACE, up to `floor` and no further (see `crate::format::BASE_VERSION`).
            let found = u32::from_le_bytes(map[4..8].try_into().unwrap());
            check_version(found)?;
            let version = found.max(floor);
            if version != found {
                map[4..8].copy_from_slice(&version.to_le_bytes());
            }
            version
        };
        Ok((map, file, version))
    }

    /// ASK the sync thread to make everything appended so far durable, and return immediately. **This
    /// is the whole append-side cost of the sync policy**: one atomic store, one atomic swap, and at
    /// most one channel send — never a sync, never a wait.
    ///
    /// `queued_to` advances even when the send is skipped (a wake is already queued) or fails (the
    /// sync thread is gone). A failure is the documented degradation, not an append error.
    ///
    /// # The SECOND caller: the cadence-snapshot checkpoint
    ///
    /// `pub` for `crates/vike-core/src/runtime/publish.rs`'s `write_snap`, which ends every `Snapshot` by
    /// asking for the checkpoint to reach disk. What the sync thread does with the request is the
    /// live-segment policy (`crate::measure::LiveFlush`).
    #[inline]
    pub fn queue_sync(&mut self) {
        self.queued_to = self.cursor;
        if let Some(s) = &self.syncer {
            s.request(pos(self.ordinal, self.cursor));
        }
    }

    /// Build the NEXT segment ahead of time on a worker thread: its file, sized AND block-reserved
    /// (renamed into place by the write thread at the roll), and the anonymous stage the core will
    /// write it into, its first window warmed. One `std::thread::spawn` on ONE append per segment —
    /// the append that first crosses [`PREP_WATERMARK_PERCENT`].
    ///
    /// Best-effort throughout, like [`reserve_blocks`]: an I/O error leaves no `.prep` file (the
    /// write thread then makes the segment itself), and a failed stage leaves the roll to map one.
    fn prepare_next(&mut self) {
        if self.prep.is_some() {
            return;
        }
        let path = prep_path(&self.dir, self.segment_index + 1);
        let len = self.config.segment_bytes;
        self.prep = std::thread::Builder::new()
            .name("vjl-prep".into())
            .spawn(move || {
                // `create(true).truncate(true)`: a leftover `.prep` from a previous crash is
                // reused rather than tripping us up. Nothing reads this file until the write
                // thread renames it, so truncating is safe.
                if let Ok(f) = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&path)
                {
                    if f.set_len(len).is_err() {
                        let _ = std::fs::remove_file(&path); // never leave a short file
                    } else {
                        reserve_blocks(&f, len);
                    }
                }
                let stage = MmapRaw::from(MmapMut::map_anon(usize::try_from(len).ok()?).ok()?);
                warm_ahead_raw(&stage, 0);
                Some(stage)
            })
            .ok();
    }

    pub fn next_sequence(&self) -> u64 {
        self.sequence
    }

    /// The wait an order runs before it leaves the process (`handoff.rs`'s "The order path"): it
    /// returns once every record appended so far is in the segment file's page cache.
    pub fn write_fence(&self) -> WriteFence {
        self.writer.as_ref().expect("open spawns the write thread").fence()
    }

    /// Raise the live segment's header to `needed` BEFORE a record a reader of the old stamp cannot
    /// parse is written into it (see [`crate::format::BASE_VERSION`]); a no-op once it is there, so
    /// the append path pays one integer compare. The core sends the stamp and touches no file: the
    /// WRITE thread puts it in the working file before any byte of that record (no sync there), and
    /// the SYNC thread writes it into the durable copy and syncs it before any frame past it, so a
    /// hard power cut cannot keep the frame without the header that reads it. It fires at most once
    /// per format step (19 to 24, the doors in the crate doc); `self.version` latches.
    fn require_version(&mut self, needed: u32) {
        if needed > self.version {
            if let Some(w) = &self.writer {
                w.send(ToWriter::Stamp { at: pos(self.ordinal, self.cursor), version: needed });
                self.shared.stamps_sent.fetch_add(1, Ordering::Release);
            }
            // The durable copy carries it too, synced before any frame past it.
            if let Some(s) = &self.syncer {
                s.stamp(pos(self.ordinal, self.cursor), needed);
            }
            self.version = needed;
        }
    }

    /// Frame `[len][crc][payload]` into the stage at the cursor, rolling to a fresh segment first if it
    /// won't fit; bump the seq; publish the new end (one Release store). The write thread copies the
    /// published bytes into the file in one `pwrite` (`crate::handoff`'s `copy_span` says what a reader
    /// racing it may see, and why that is safe).
    fn write_framed(&mut self, payload: &[u8]) -> io::Result<()> {
        if self.cursor + 8 + payload.len() > self.stage.len() {
            self.roll()?;
        }
        let c = self.cursor;
        stage_write(&self.stage, c + 4, &fnv1a32(payload).to_le_bytes());
        stage_write(&self.stage, c + 8, payload);
        stage_write(&self.stage, c, &(payload.len() as u32).to_le_bytes());
        self.cursor = c + 8 + payload.len();
        self.sequence += 1;
        self.shared.appended.store(pos(self.ordinal, self.cursor), Ordering::Release);
        // Bound the un-synced debt. One integer compare per append; the REQUEST fires once per
        // `sync_chunk` and the sync itself happens on the sync thread.
        if self.cursor - self.queued_to >= self.sync_chunk {
            self.queue_sync();
        }
        // Once this segment is mostly full, start building its successor off-thread.
        if self.prep.is_none() && self.cursor * 100 >= self.stage.len() * PREP_WATERMARK_PERCENT {
            self.prepare_next();
        }
        Ok(())
    }

    /// Swap in the next segment: a new stage (the prepared one when `prepare_next` got it ready) with
    /// the new segment's header in its first bytes, and ONE message to the write thread, which ends
    /// the old file where the core's frames end, makes or renames the new one, and hands the old one
    /// to the sync thread for its seal. No file is touched here.
    fn roll(&mut self) -> io::Result<()> {
        let prepared = self.prep.take().and_then(|h| h.join().ok().flatten());
        let stage = match prepared {
            Some(s) => s,
            None => MmapRaw::from(MmapMut::map_anon(
                usize::try_from(self.config.segment_bytes).unwrap_or(usize::MAX),
            )?),
        };
        let stage = Arc::new(stage);
        // The fresh segment's header — magic | version | first_sequence — written into the file by the
        // write thread with the segment's first frames.
        stage_write(&stage, 0, &MAGIC.to_le_bytes());
        stage_write(&stage, 4, &self.version.to_le_bytes());
        stage_write(&stage, 8, &self.sequence.to_le_bytes());
        let previous_end = self.cursor;
        self.ordinal += 1;
        self.segment_index += 1;
        if let Some(w) = &self.writer {
            w.send(ToWriter::Roll {
                ordinal: self.ordinal,
                index: self.segment_index,
                stage: Arc::clone(&stage),
                previous_end,
            });
        }
        // The new segment's durable copy starts now, before any frame of it is asked for.
        if let Some(s) = &self.syncer {
            s.live(self.segment_index, self.ordinal, Arc::clone(&stage));
        }
        self.stage = stage;
        self.cursor = HEADER;
        self.queued_to = HEADER;
        Ok(())
    }

    /// Make everything appended so far durable, on the CALLER's thread's time: wait for the write
    /// thread, then for one full sync on the sync thread.
    ///
    /// The explicit "make it durable NOW" barrier for tools and tests, and therefore **not safe to call
    /// from the vike-core fold**. **NO fold-path caller may be added** — a checkpoint wants
    /// [`Self::queue_sync`].
    pub fn flush(&mut self) -> io::Result<()> {
        self.write_fence().wait();
        if let Some(s) = &self.syncer {
            s.flush_now()?;
        }
        // Everything up to the cursor is now on disk, so the appender owes the sync thread nothing
        // for it; without this the next chunk boundary would ask for an already-synced region.
        self.queued_to = self.cursor;
        Ok(())
    }

    /// Test seam: the shared cell, cloned so a test can OUTLIVE the journal and assert what the
    /// threads did — in particular the shutdown sync, which completes after the journal is gone.
    #[cfg(test)]
    pub(super) fn sync_shared(&self) -> Arc<JournalSyncShared> {
        Arc::clone(&self.shared)
    }
}

/// **Close a segment that holds anything past the resume cursor** (at `open`): `true` when the
/// records must go on in a new one.
///
/// What can lie there is what a crash left: a kill's check and part-written payload with NO length (the
/// length is written last, so the walk ends [`JournalWalkEnd::Clean`]), a power cut's frame whose length landed
/// and whose payload did not, or a dead run's whole frames past a lost page. Resuming over it is the
/// defect this fixes: a new record shorter than those bytes would leave them behind itself and every
/// reader would halt there for good. So the segment ends HERE: the one length no reader may follow is
/// zeroed (in the open-time mapping, sealed by the sync thread when the write thread leaves the segment),
/// which is the clean end every reader already knows, and the records go on in the next segment. A frame
/// that PASSED its check and does not parse is a record another build can read, so it keeps even its
/// length. A clean tail changes nothing, so an ordinary restart resumes in place.
fn close_dirty_tail(
    map: &mut MmapMut,
    cursor: usize,
    end: &JournalWalkEnd,
    dir: &Path,
    index: u64,
) -> bool {
    static ZEROS: [u8; 4096] = [0; 4096];
    let dirty = matches!(end, JournalWalkEnd::Corrupt(_))
        || map[cursor..].chunks(ZEROS.len()).any(|c| c != &ZEROS[..c.len()]);
    if !dirty {
        return false;
    }
    tracing::warn!(
        segment = %segment_path(dir, index).display(),
        offset = cursor,
        cause = match end {
            JournalWalkEnd::Clean => "no_length",
            JournalWalkEnd::Corrupt(cause) => cause.as_str(),
        },
        "journal open: bytes lie past the last readable frame (a crash mid-write): this segment is \
         closed where the frames end, those bytes stay in it, and the records go on in a new one"
    );
    if !matches!(end, JournalWalkEnd::Corrupt(JournalCorruptCause::ParseFailed))
        && cursor + 8 <= map.len()
    {
        map[cursor..cursor + 4].fill(0);
    }
    true
}

/// Write `bytes` into the stage at `off` (the core thread's only way into the stage).
///
/// An audited unsafe site of the asynchronous journal (decision 0279), pinned by the unsafe gate.
#[expect(unsafe_code)]
fn stage_write(stage: &MmapRaw, off: usize, bytes: &[u8]) {
    assert!(off + bytes.len() <= stage.len(), "a frame past the stage (roll first)");
    // SAFETY: in bounds (asserted above). The core thread is the stage's only writer and writes only
    // at and past its cursor, or into a stage no other thread has yet (a roll's new one); the write
    // and sync threads read only below a position the core published with a Release store AFTER
    // these writes, and never a byte past it.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), stage.as_mut_ptr().add(off), bytes.len());
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        // The write thread first: it drains everything appended into the files, hands the last
        // sealed segment to the sync thread and ends (dropping its sender). Then the sync thread:
        // it seals what it was handed, syncs the live segment once more and ends. The joins are the
        // guarantee that a graceful stop leaves the tail on disk.
        if let Some(w) = self.writer.take() {
            w.stop_and_join();
        }
        if let Some(s) = self.syncer.take() {
            s.shutdown_and_join();
        }
        // Join any in-flight segment preparation. Without this a worker could still be sizing a
        // `.prep` file after the journal (and, in tests, its whole directory) is gone.
        if let Some(h) = self.prep.take() {
            let _ = h.join();
        }
    }
}

#[path = "writer_tests.rs"]
#[cfg(test)]
mod writer_tests;

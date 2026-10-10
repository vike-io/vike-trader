//! [`CommandJournal`] — the single-writer append side: open/resume, framing, the segment roll, the
//! flush barrier and the `Drop` that stops the syncer live in this file. The `append_*` verbs are
//! `writer/append.rs`'s and the offline readers and prune (`read_all`, `latest_segment_version`,
//! `prune_before_latest_snap`) are `writer/offline.rs`'s — each an `impl CommandJournal` block over
//! the private state this file owns. Split out of `journal.rs` verbatim.
//!
//! ⚠ This is the FOLD-PATH type. Every blocking `msync` it needs lives in [`super::sync`], on a
//! thread this one never joins — [`CommandJournal::flush`] is the only barrier that runs on the
//! caller's thread and is explicitly NOT for the fold.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;

use memmap2::{MmapMut, MmapRaw};

use super::frame::{fnv1a32, walk_frames};
use super::record::record_seq;
use super::segment::{PREP_WATERMARK_PCT, prep_path, reserve_blocks, seg_path, warm_ahead};
#[cfg(test)]
use super::sync::SyncShared;
use super::sync::{Syncer, sync_chunk_bytes};
use super::{HEADER, MAGIC, VERSION, check_version};
use crate::lock::JournalLock;

mod append;
mod offline;

#[derive(Debug, Clone)]
pub struct JournalFileConfig {
    pub segment_bytes: u64,
    pub flush_every: u32,
}
impl Default for JournalFileConfig {
    fn default() -> Self {
        JournalFileConfig { segment_bytes: 64 * 1024 * 1024, flush_every: 256 }
    }
}

pub struct CommandJournal {
    dir: PathBuf,
    cfg: JournalFileConfig,
    map: MmapMut,
    pub(super) seg_idx: u64,
    pub(super) cursor: usize,
    seq: u64,
    since_flush: u32,
    /// Offset up to which a sync has been REQUESTED of the syncer thread — deliberately NOT one
    /// that has completed (that is `SyncShared::synced_to`, which the appender never reads and
    /// never waits on). `queued_to..cursor` is the region no sync has been asked for yet;
    /// [`sync_chunk_bytes`] bounds how large it is allowed to get before one is.
    pub(super) queued_to: usize,
    /// This segment's un-synced debt ceiling, resolved once at map time from `cfg.segment_bytes`
    /// (see [`sync_chunk_bytes`]) rather than recomputed on every append.
    pub(super) sync_chunk: usize,
    /// The duplicate-instance interlock on `dir`, held for this journal's whole lifetime and
    /// released on drop (see [`crate::lock`]). `Option` ONLY so the guard can be MOVED
    /// across a segment [`Self::roll`], which rebuilds the whole struct; [`Self::open`] always
    /// installs `Some`, and no other constructor is public.
    lock: Option<JournalLock>,
    /// In-flight preparation of the NEXT segment (see `prepare_next`), or `None` when none is
    /// scheduled. Joined by [`Self::roll`] before the successor is mapped — so the worker is never
    /// still sizing a file that `map_segment` is about to mmap, which would violate the
    /// `MmapMut::map_mut` contract that nothing resizes the file while it is mapped.
    prep: Option<std::thread::JoinHandle<()>>,
    /// The dedicated syncer thread — where every blocking `msync` runs. MOVED across each
    /// [`Self::roll`] like `lock`, so one thread serves the directory for its whole life.
    ///
    /// `None` reaches [`Drop`] on exactly ONE value: the `self` a `roll` just superseded, whose
    /// syncer has moved to the successor and is sealing that segment through its own mapping. `Drop`
    /// reads `None` as "do NOT flush here" for precisely that reason — flushing would put the
    /// whole-segment `msync` straight back on the fold path. (`open` always installs `Some`: a
    /// failed spawn fails the open.)
    syncer: Option<Syncer>,
}

impl CommandJournal {
    /// Open (or resume) the journal directory `dir` as its EXCLUSIVE writer.
    ///
    /// Takes the duplicate-instance lock FIRST — before any segment is mapped — so a second
    /// instance fails fast with a clear [`io::ErrorKind::AddrInUse`] error naming the directory
    /// instead of silently interleaving WAL frames into a live journal. The lock is held until
    /// this value drops. Read-only entry points (`read_all`, `latest_segment_version`,
    /// `read_since`, `prune_before_latest_snap`) are unaffected — they never lock.
    pub fn open(dir: &Path, cfg: JournalFileConfig) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        // Duplicate-instance interlock. Acquired before the segment scan/map so a refused second
        // instance never touches (let alone extends or restamps) the live journal's files. On the
        // error paths below the guard drops here and releases — a failed open leaves no lock held.
        let lock = JournalLock::acquire(dir)?;
        // resume: find the highest existing segment, else create segment 0
        let mut max_idx: Option<u64> = None;
        for e in std::fs::read_dir(dir)? {
            let name = e?.file_name().to_string_lossy().into_owned();
            if let Some(n) = name.strip_prefix("journal-").and_then(|s| s.strip_suffix(".vjl"))
                && let Ok(i) = n.parse::<u64>()
            {
                max_idx = Some(max_idx.map_or(i, |m: u64| m.max(i)));
            }
        }
        let (seg_idx, first_seq) = (max_idx.unwrap_or(0), 0u64);
        // `reserve = true`: session start is not a hot path, so buy the fault-free append run.
        let (mut j, sync_map) = Self::map_segment(dir, seg_idx, first_seq, &cfg, true)?;
        // Install the guard on the ONE public constructor: `map_segment` deliberately leaves it
        // `None` (see `roll`), so this is the single place a lock enters a `CommandJournal`.
        j.lock = Some(lock);
        // scan existing records to find cursor + next seq (validates as it goes)
        let (cursor, seq) = j.scan_written();
        j.cursor = cursor;
        j.seq = seq;
        // Re-base the sync debt onto the RESUMED cursor, on BOTH sides. Those bytes were already
        // written (and synced) by whoever appended them; starting either side at HEADER would make
        // this session's first chunk sync needlessly re-msync the entire resumed prefix — on a
        // nearly-full 64MB segment, exactly the multi-hundred-ms stall this bounding exists to
        // prevent, moved to startup (and now onto the syncer, where it would still delay the first
        // real sync behind a pointless one).
        j.queued_to = cursor;
        // Re-aim the warm window at the RESUMED cursor. `map_segment` warmed from HEADER, which is
        // right for a fresh segment and wrong for a resumed one — a nearly-full segment resumes
        // with its cursor far past the window, so without this the first appends of the session
        // fault exactly as they did before.
        warm_ahead(&j.map, cursor);
        // Start the syncer LAST, on the rebased cursor: from here on every blocking `msync` for
        // this directory happens on that thread.
        j.syncer = Some(Syncer::spawn(sync_map, cursor)?);
        Ok(j)
    }

    // One of this journal's TWO audited unsafe sites (the other is `segment::reserve_blocks`'s
    // `posix_fallocate`). The crate lint is `unsafe_code = "deny"` (see Cargo.toml); this per-fn
    // `#[allow(unsafe_code)]` is a named carve-out — every OTHER unsafe in the crate stays denied.
    //
    // NOTE: returns a `CommandJournal` with `lock: None`. This is a SEGMENT constructor, not a
    // directory-writer constructor — the directory interlock is taken once by `open` and then
    // MOVED across every `roll`; re-acquiring it here would self-conflict (this process already
    // holds the dir lock) and break the first segment roll.
    ///
    /// `reserve` asks for the segment's blocks to be materialized up front (see `reserve_blocks`).
    /// `open` passes `true` — session start, no hot path. **`roll` passes `false`, deliberately**:
    /// measured on the latency box ext4, reserving a 64MB segment costs a median 1.7ms (max 2.8ms) against
    /// ~160µs for the bare `set_len`+mmap, and `roll` runs INSIDE `write_framed` on the append hot
    /// path. Paying 2.8ms once per segment to avoid ~50 spikes of 100-220µs is a bad trade for a
    /// path whose whole purpose is bounded worst-case latency. The follow-up that would let rolled
    /// segments be reserved too is to create the NEXT segment ahead of time, off the hot path, so
    /// the roll becomes a pointer swap — not attempted here.
    ///
    /// Returns the journal AND a SECOND, flush-only mapping of the same segment for the syncer
    /// thread (see the returned `MmapRaw`'s construction below).
    #[expect(unsafe_code)]
    fn map_segment(
        dir: &Path,
        idx: u64,
        first_seq: u64,
        cfg: &JournalFileConfig,
        reserve: bool,
    ) -> io::Result<(Self, MmapRaw)> {
        // Invariant: `map[0..4]` (magic) and `map[8..16]` (first_seq) below assume the segment
        // is at least HEADER bytes; a tiny misconfigured `segment_bytes` would otherwise panic
        // on an out-of-range slice. Fail loudly in debug/test rather than silently clamping.
        debug_assert!(cfg.segment_bytes >= HEADER as u64, "segment_bytes must be >= HEADER (16)");
        let path = seg_path(dir, idx);
        // truncate(false) is load-bearing: reopening an existing segment MUST keep its bytes so
        // `scan_written` can resume from the prior records; truncating would wipe the journal.
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        if file.metadata()?.len() < cfg.segment_bytes {
            file.set_len(cfg.segment_bytes)?; // size it: appends never extend the file
        }
        // ...and, when the caller asks, materialize its BLOCKS. `set_len` moves only the size
        // marker, leaving a SPARSE file, so ext4 defers each block allocation to the first WRITE
        // FAULT on that page — i.e. onto the append path itself. Measured on the latency box against this
        // exact journal (256MB segment, flush_every 256, 100_000 appends): sparse takes 3-9 appends
        // over 100µs with a 220µs max; reserved, that is 0-1 and a ~40µs max.
        if reserve {
            reserve_blocks(&file, cfg.segment_bytes);
        }
        // SAFETY: `MmapMut::map_mut` is unsafe by signature — the caller promises the file is not
        // resized/truncated by another handle while mapped. This journal owns `path` exclusively
        // for the map's lifetime (pre-sized once above; only this process appends into the mapped
        // pages), so the safety contract holds.
        let mut map = unsafe { MmapMut::map_mut(&file)? };
        // ...and, with the mapping now in hand, its PAGE TABLES. `reserve_blocks` above removed the
        // filesystem allocation from inside the write fault; this removes the fault itself. Same
        // `reserve` flag, because both answer one question — "may this call buy a fault-free append
        // run" — and the two callers answer it identically: `open` yes (session start), `roll` no
        // (it runs ON the append path, and its successor was prepared ahead by `prepare_next`).
        //
        // ⚠ THE ROLL PATH IS NOT COVERED, and saying so is the point. `prepare_next` reserves the
        // successor's BLOCKS on its worker thread, but the successor is not MAPPED until `roll`, so
        // there is no mapping to warm ahead of time. A segment roll therefore still pays first-touch
        // faults as it fills. Closing that means moving the mapping itself into `prepare_next` and
        // handing the warmed `MmapMut` to `roll` — a lifecycle change, deliberately not folded into
        // the measurement that justified this one.
        if reserve {
            warm_ahead(&map, HEADER);
        }
        if u32::from_le_bytes(map[0..4].try_into().unwrap()) != MAGIC {
            // fresh (zero-filled) segment — stamp magic | version | first_seq
            map[0..4].copy_from_slice(&MAGIC.to_le_bytes());
            map[4..8].copy_from_slice(&VERSION.to_le_bytes());
            map[8..16].copy_from_slice(&first_seq.to_le_bytes());
        } else {
            // Resuming an EXISTING segment. A version this build cannot read (too old, or FORWARD
            // of it) still fails loudly — appending VERSION frames next to an incompatible serde
            // shape would interleave records no single reader can walk.
            //
            // A SUPPORTED-but-older version is instead UPGRADED IN PLACE: every frame already in
            // the segment is, by the additive-only rule on `MIN_READABLE_VERSION`, also a valid
            // VERSION frame, so restamping the header to VERSION is an honest statement of what it
            // now takes to read the segment (this build is about to append VERSION frames into it).
            // This is what lets a live v4 journal directory survive a v5 binary rollout instead of
            // having to be discarded.
            let version = u32::from_le_bytes(map[4..8].try_into().unwrap());
            check_version(version)?;
            if version != VERSION {
                map[4..8].copy_from_slice(&VERSION.to_le_bytes());
            }
        }
        // The SYNCER's own mapping of the same segment. `MmapRaw` is memmap2's safe half — it
        // exposes pointers, never a slice, so `map_raw` is NOT `unsafe` and this adds no third
        // unsafe carve-out — and `flush`/`flush_range` is the entire capability the syncer needs; it
        // never reads a byte. Both mappings are MAP_SHARED views of the same fd, so they address
        // the SAME page-cache pages: an `msync` issued through this one forces exactly the bytes the
        // appender wrote through the other, and it keeps working after the appender's mapping is
        // munmapped at a roll.
        let sync_map = MmapRaw::map_raw(&file)?;
        Ok((
            CommandJournal {
                dir: dir.to_path_buf(),
                cfg: cfg.clone(),
                map,
                seg_idx: idx,
                cursor: HEADER,
                seq: first_seq,
                since_flush: 0,
                // A resumed segment's existing bytes were synced by whoever wrote them (and, after
                // a crash, by the kernel's own writeback); `scan_written` moves `cursor` past them
                // right after this returns, so starting at HEADER would make the first chunk sync
                // re-cover the whole resumed prefix. `open` re-bases it once the cursor is known.
                queued_to: HEADER,
                sync_chunk: sync_chunk_bytes(cfg.segment_bytes),
                lock: None, // see the note above `map_segment`: `open`/`roll` own the guard
                prep: None, // a freshly-mapped segment has not scheduled its own successor yet
                syncer: None, // `open` spawns it; `roll` MOVES the existing one in
            },
            sync_map,
        ))
    }

    /// ASK the syncer thread to force everything written since the last request out to disk, and
    /// return immediately. **This is the whole append-side cost of the sync policy**: one atomic
    /// store, one atomic swap, and at most one channel send — never an `msync`, never a wait.
    ///
    /// This used to be `sync_completed_region`, a BLOCKING `flush_range` called inline from
    /// [`Self::write_framed`] i.e. from the vike-core fold thread. It bounded the seal (without it
    /// the dirty set grows until a roll has to write all of it at once — 339ms on a 64MB segment at
    /// full append rate) by paying ~36ms of it per chunk, on the hot path, which the `runtime_latency`
    /// harness measured landing as a single 141ms core hop. [`sync_chunk_bytes`] still bounds the
    /// depth of any one `msync`; what changed is who pays for it.
    ///
    /// `queued_to` advances even when the send is skipped (a wake is already queued) or fails (the
    /// syncer is gone). Skipping is correct — the watermark the syncer will read is already newer.
    /// A failure is the documented degradation, not an append error: per the spec's §A1.1
    /// durability contract the un-synced tail survives everything except a hard power cut, and
    /// failing an append here would take the live core down over a latency optimization.
    ///
    /// # The SECOND caller: the cadence-snapshot checkpoint
    ///
    /// `pub` for `crates/vike-core/src/runtime/publish.rs`'s `write_snap`, which ends every `Snap` by
    /// asking for the
    /// checkpoint to reach disk. It used to end with [`Self::flush`] — the blocking whole-mapping
    /// `msync`, on the fold thread, once per `snapshot_every` (1024) records. This is the
    /// non-blocking replacement, and reusing the existing watermark rather than adding a second
    /// mechanism is deliberate: `target` is monotonic, so "sync up to the snap" and "sync up to this
    /// chunk boundary" are the same statement about a different offset.
    ///
    /// Two things differ from the chunk caller and are worth stating rather than assuming:
    ///
    /// * **It fires ~14x more often at the production defaults.** 1024 records x ~287 B ≈ 287 KB
    ///   between snaps, against a 4 MiB `sync_chunk` on a 64 MiB segment. That makes the syncer's
    ///   `msync`s smaller and more frequent — which is the good direction (each one moves less dirty
    ///   data, so the seal at a roll has less debt to discover), and it cannot pile up: a syncer that
    ///   falls behind coalesces the extra wakes into one, exactly as it does for chunk requests.
    /// * **A snap is not more durable than any other record, and never was.** Per the spec's §A1.1
    ///   contract the mmap'd pages survive a process crash / OOM-kill / clean reboot with zero
    ///   `msync`; only a hard power cut loses the un-synced tail, and a torn tail already stops
    ///   replay at the last VALID record, whichever kind it is. The blocking flush bought ordering
    ///   against nothing (an `msync` of one range implies nothing about later pages) and readability
    ///   against nothing either — a materializer or a restart reading these segments with
    ///   `std::fs::read` sees the page cache, not the disk. What a clean shutdown needs is covered
    ///   by `Drop`'s join (see the module doc).
    #[inline]
    pub fn queue_sync(&mut self) {
        self.queued_to = self.cursor;
        if let Some(s) = &self.syncer {
            s.request(self.cursor);
        }
    }

    /// Build the NEXT segment — sized AND block-reserved — on a worker thread, so the roll that
    /// eventually consumes it is just a rename + mmap instead of a `set_len` + `fallocate`.
    ///
    /// This is what lets ROLLED segments be reserved at all. Reserving inline in [`Self::roll`]
    /// measured a median 1.7ms (max 2.8ms) for a 64MB segment against ~160µs for bare
    /// `set_len`+mmap — unacceptable on the append hot path (see `map_segment`). Doing it ahead of
    /// time off-thread keeps the reservation and pays ~nothing at the roll.
    ///
    /// Cost accounting, because this IS still touched from the hot path: one
    /// `std::thread::spawn` (tens of µs) on ONE append per segment — the append that first crosses
    /// [`PREP_WATERMARK`]. That is a far smaller spike than either the 1.7ms inline reservation or
    /// the ~50 fault spikes of 100-220µs it removes, but it is not free, which is why it fires once
    /// and only once per segment (`prep.is_some()` gates it).
    ///
    /// Best-effort throughout, like [`reserve_blocks`]: any I/O error leaves no `.prep` file and
    /// `roll` silently falls back to creating the segment itself — exactly today's behavior.
    fn prepare_next(&mut self) {
        if self.prep.is_some() {
            return;
        }
        let path = prep_path(&self.dir, self.seg_idx + 1);
        let len = self.cfg.segment_bytes;
        self.prep = std::thread::Builder::new()
            .name("vjl-prep".into())
            .spawn(move || {
                // `create(true).truncate(true)`: a leftover `.prep` from a previous crash is
                // reused rather than tripping us up. Nothing reads this file until `roll` renames
                // it, so truncating is safe.
                let Ok(f) = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&path)
                else {
                    return;
                };
                if f.set_len(len).is_err() {
                    let _ = std::fs::remove_file(&path); // never leave a short file for `roll`
                    return;
                }
                reserve_blocks(&f, len);
            })
            .ok();
    }

    /// Walk records from HEADER to the first len==0 / invalid record; return (cursor, next_seq).
    /// One segment (the freshly-mapped tail), so [`walk_frames`]' clean-vs-corrupt end is
    /// immaterial here — both stop the walk, and the resume cursor lands just before the unwritten
    /// / torn bytes so the next append overwrites them.
    fn scan_written(&self) -> (usize, u64) {
        let mut seq = u64::from_le_bytes(self.map[8..16].try_into().unwrap());
        let (cursor, _end) = walk_frames(&self.map, HEADER, |rec| seq = record_seq(&rec) + 1);
        (cursor, seq)
    }

    pub fn next_seq(&self) -> u64 {
        self.seq
    }

    /// Frame `[len][crc][payload]` at the cursor (len written LAST, so a reader never sees a
    /// length pointing at an unwritten payload), rolling to a fresh segment first if it won't
    /// fit; bump the seq; `flush_async` every `flush_every` records (never per-record — that
    /// syscall would blow the p99 gate).
    fn write_framed(&mut self, payload: &[u8]) -> io::Result<()> {
        if self.cursor + 8 + payload.len() > self.map.len() {
            self.roll()?;
        }
        let c = self.cursor;
        self.map[c + 4..c + 8].copy_from_slice(&fnv1a32(payload).to_le_bytes());
        self.map[c + 8..c + 8 + payload.len()].copy_from_slice(payload);
        self.map[c..c + 4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        self.cursor = c + 8 + payload.len();
        self.seq += 1;
        self.since_flush += 1;
        if self.since_flush >= self.cfg.flush_every {
            self.map.flush_async()?;
            self.since_flush = 0;
        }
        // Bound the un-synced debt. Without this the whole segment is still dirty when it is
        // sealed, and that one `msync` costs ~339ms on a 64MB segment at full rate; capped, ~36ms.
        // One integer compare per append; the REQUEST fires once per `sync_chunk` and the `msync`
        // itself happens on the syncer thread, so nothing here can block on the disk. A segment
        // smaller than `SYNC_CHUNK_MIN` never trips this at all (`sync_chunk` exceeds the segment),
        // so a journal that will never roll issues no chunk request either.
        if self.cursor - self.queued_to >= self.sync_chunk {
            self.queue_sync();
        }
        // Once this segment is mostly full, start building its successor off-thread so the roll
        // below is a rename+mmap. Cheap to test (one integer compare per append) and the spawn
        // itself fires exactly once per segment — see `prepare_next`.
        if self.prep.is_none() && self.cursor * 100 >= self.map.len() * PREP_WATERMARK_PCT {
            self.prepare_next();
        }
        Ok(())
    }

    /// Swap in the next segment.
    ///
    /// ⚠ Note what is NOT here any more: this used to open with `self.map.flush()`, a BLOCKING
    /// whole-segment `msync` — measured 292ms (max 432ms) on a dirty 64MB segment — running on the
    /// vike-core fold thread, since `roll` is called from inside `write_framed`. The same `msync`
    /// still happens, at the same moment, on the same bytes: it is now the syncer's
    /// `SyncMsg::Roll` seal (see [`Syncer::hand_over`] at the bottom of this function). The
    /// difference is the thread.
    fn roll(&mut self) -> io::Result<()> {
        // Consume the pre-built successor, if `prepare_next` got one ready. Joining FIRST is what
        // makes this sound: the worker may still be inside `set_len`/`fallocate` on that path, and
        // `map_segment` is about to mmap it — resizing a mapped file is exactly what
        // `MmapMut::map_mut`'s contract forbids. In the common case the worker finished long ago
        // (it started at PREP_WATERMARK_PCT of the segment) and this join is instant.
        if let Some(h) = self.prep.take() {
            let _ = h.join(); // a panicked worker just means no `.prep` file — fall through
            let (prep, live) =
                (prep_path(&self.dir, self.seg_idx + 1), seg_path(&self.dir, self.seg_idx + 1));
            // Rename only if it is actually the right size; a short/partial file would hand
            // `map_segment` a segment smaller than `segment_bytes`. On failure we simply leave it
            // and let `map_segment` build the segment the old way.
            if std::fs::metadata(&prep).is_ok_and(|m| m.len() >= self.cfg.segment_bytes) {
                let _ = std::fs::rename(&prep, &live);
            }
        }
        // `reserve = false`: this runs inside `write_framed`, on the append hot path — see
        // `map_segment`'s doc for the measured 1.7ms-vs-160µs reason. When `prepare_next` did its
        // job the blocks are ALREADY reserved (that is the whole point); when it did not, this
        // degrades to the previous sparse-roll behavior rather than stalling the hot path.
        let (mut next, sync_map) =
            Self::map_segment(&self.dir, self.seg_idx + 1, self.seq, &self.cfg, false)?;
        // CARRY the directory interlock across the swap. `map_segment` builds a fresh
        // `CommandJournal` with `lock: None`, and `*self = next` DROPS the old value — so without
        // this move the guard would be released on the first segment roll and a second instance
        // could slip in mid-session. `take()` runs only AFTER `map_segment` succeeded, so a failed
        // roll leaves the lock exactly where it was (still held by `self`).
        next.lock = self.lock.take();
        // CARRY the syncer the same way, and hand it the new segment's mapping — which is also the
        // instruction to seal the one it is holding. Two things ride on the `take()`:
        //   * ONE syncer thread serves the directory for its whole life, not one per segment;
        //   * it leaves `self.syncer == None`, which is exactly how the `Drop` about to run on the
        //     superseded value below knows NOT to flush this segment itself. Doing so would put the
        //     292ms whole-segment `msync` straight back on the fold path.
        let syncer = self.syncer.take();
        if let Some(s) = &syncer {
            s.hand_over(sync_map);
        }
        next.syncer = syncer;
        // Whole-value move (NOT `CommandJournal { cfg, ..next }` struct-update): `..next` would
        // partially move fields out of `next`, which E0509-rejects on a `Drop` type. `map_segment`
        // already set `next.cfg == self.cfg.clone()`, so this preserves the config; assigning a
        // complete value drops the old segment and installs the new one. NOTE the dropped value's
        // `Drop` deliberately does NOT flush (`syncer` is `None` on it — see the comment above and
        // `Drop`): the seal is the syncer's job now. Its `MmapMut` is munmapped here, which does not
        // discard the dirty page-cache pages the syncer's own mapping is about to force out.
        *self = next;
        Ok(())
    }

    /// Force the WHOLE live segment to disk on the CALLER's thread, and wait for it.
    ///
    /// The explicit "make it durable NOW" barrier for tools and tests. ⚠ Deliberately NOT what the
    /// append path uses — that posts a watermark to the syncer thread and returns (see
    /// [`Self::queue_sync`]) — and therefore **not safe to call from the vike-core fold**: on a
    /// dirty 64MB segment this is the 292ms `msync` the whole design exists to keep off that thread.
    ///
    /// That warning used to be contradicted by the crate's own code: `CoreThread::write_snap` called
    /// this, on the fold thread, every `snapshot_every` records. It calls [`Self::queue_sync`] now.
    /// **NO fold-path caller may be added back** — if a new one wants a checkpoint, it wants
    /// `queue_sync`.
    pub fn flush(&mut self) -> io::Result<()> {
        self.map.flush()?;
        // Everything up to the cursor is now on disk, so the appender owes the syncer nothing for
        // it; without this the next chunk boundary would ask for an already-synced region.
        self.queued_to = self.cursor;
        Ok(())
    }

    /// Test seam: the syncer's shared cell, cloned so a test can OUTLIVE the journal and assert
    /// what the syncer did — in particular the shutdown sync, which by definition completes after
    /// the journal is gone.
    #[cfg(test)]
    pub(super) fn sync_shared(&self) -> Arc<SyncShared> {
        Arc::clone(&self.syncer.as_ref().expect("open always installs a syncer").shared)
    }
}

impl Drop for CommandJournal {
    fn drop(&mut self) {
        // Stop the syncer and WAIT for its final whole-segment `msync`. The join is the guarantee:
        // a graceful stop that detached here would let the process exit with the tail sitting only
        // in the page cache. (Process death ALONE is safe — the kernel writes those pages back, per
        // the spec's §A1.1 contract — but a clean stop should not leave that to chance, and a
        // shutdown that is about to prune the directory or hand it to a reader needs the bytes on
        // disk.) This replaces the unconditional `self.map.flush()` that used to live here.
        if let Some(s) = self.syncer.take() {
            s.shutdown_and_join(self.cursor);
        }
        // `syncer == None` reaches here on exactly ONE value: the `self` a `roll` just superseded.
        // Its syncer moved to the successor and is sealing THIS segment through its own mapping, so
        // there is deliberately nothing to flush — doing it here would restore the 292ms
        // whole-segment stall on the fold path, which is the entire defect this design removes.
        //
        // Join any in-flight segment preparation. Without this a worker could still be sizing a
        // `.prep` file after the journal (and, in tests, its whole directory) is gone — recreating
        // the directory entry under a caller that just deleted it. `roll` already `take()`s the
        // handle, so this only fires when a journal drops mid-segment with a preparation pending.
        if let Some(h) = self.prep.take() {
            let _ = h.join();
        }
    }
}

#[path = "writer_tests.rs"]
#[cfg(test)]
mod writer_tests;

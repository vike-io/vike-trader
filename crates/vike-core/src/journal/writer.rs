//! [`CommandJournal`] — the single-writer append side: open/resume, the `append_*` verbs, framing,
//! the segment roll, the offline prune, and the `Drop` that stops the syncer. Split out of
//! `journal.rs` verbatim.
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
use vike_exec::Ingest;

use super::frame::{fnv1a32, walk_frames, CorruptCause, WalkEnd};
use super::read::MaterializeCheckpoint;
use super::record::{
    record_seq, ConditionalRecord, JournalRecord, JournalRecordRef, PortfolioSample,
    SnapConditional, SnapContingency, SnapMountAttr,
};
use super::segment::{prep_path, reserve_blocks, seg_path, warm_ahead, PREP_WATERMARK_PCT};
#[cfg(test)]
use super::sync::SyncShared;
use super::sync::{sync_chunk_bytes, Syncer};
use super::{check_version, HEADER, MAGIC, VERSION};
use crate::journal_lock::JournalLock;

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
    /// released on drop (see [`crate::journal_lock`]). `Option` ONLY so the guard can be MOVED
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
            if let Some(n) = name.strip_prefix("journal-").and_then(|s| s.strip_suffix(".vjl")) {
                if let Ok(i) = n.parse::<u64>() {
                    max_idx = Some(max_idx.map_or(i, |m: u64| m.max(i)));
                }
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
    #[allow(unsafe_code)]
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
    /// `pub(crate)` for [`crate::runtime`]'s `write_snap`, which ends every `Snap` by asking for the
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
    pub(crate) fn queue_sync(&mut self) {
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

    /// Journal one exec-lane ingest message (borrowed — no clone). Returns the assigned seq.
    pub fn append_cmd(&mut self, now_ms: i64, msg: &Ingest) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::Cmd { seq, now_ms, msg })
            .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one mounted strategy's order intent at the `drain_broker` boundary (borrowed — no
    /// clone), write-ahead of the `apply_intent` call it records (mirrors `append_cmd`). Returns
    /// the assigned seq.
    pub fn append_strategy_submit(
        &mut self,
        now_ms: i64,
        mount_id: &str,
        intent: &vike_exec::OrderIntent,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload =
            serde_json::to_vec(&JournalRecordRef::StrategySubmit { seq, now_ms, mount_id, intent })
                .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal a server-minted submit's RESOLVED request (borrowed — no clone), from inside
    /// `apply_intent` right AFTER the coid is minted for an incoming EMPTY-coid submit. This closes
    /// the minted-coid `exec_order` gap: the write-ahead `Cmd` for this order carried an empty coid,
    /// so without this record the materializer could never tie the minted coid to its
    /// `(venue, symbol, qty, …)` for an order that terminalizes without filling. See
    /// [`JournalRecord::MintedSubmit`]. Returns the assigned seq.
    pub fn append_minted_submit(
        &mut self,
        now_ms: i64,
        req: &vike_model::OrderRequest,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::MintedSubmit { seq, now_ms, req })
            .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one periodic compact portfolio observation (borrowed — no clone). See
    /// [`JournalRecord::PortfolioSnap`]. Returns the assigned seq.
    pub fn append_portfolio_snap(
        &mut self,
        now_ms: i64,
        sample: &PortfolioSample,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::PortfolioSnap { seq, now_ms, sample })
            .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one emulated conditional's RESOLVED terms right after its `arm_id` is minted
    /// (borrowed — no clone). See [`JournalRecord::ConditionalArmed`]. Returns the assigned seq.
    pub fn append_conditional_armed(
        &mut self,
        now_ms: i64,
        arm_id: &str,
        resolved: &ConditionalRecord,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::ConditionalArmed {
            seq,
            now_ms,
            arm_id,
            resolved,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one emulated conditional's FIRE, write-ahead of the release it causes (borrowed —
    /// no clone). See [`JournalRecord::ConditionalFire`]. Returns the assigned seq.
    pub fn append_conditional_fire(
        &mut self,
        now_ms: i64,
        arm_id: &str,
        trigger_px: f64,
        req: &vike_model::OrderRequest,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::ConditionalFire {
            seq,
            now_ms,
            arm_id,
            trigger_px,
            req,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one emulated conditional's DISARM, write-ahead of the book mutation it records.
    /// See [`JournalRecord::ConditionalDisarmed`]. Returns the assigned seq.
    pub fn append_conditional_disarmed(&mut self, now_ms: i64, arm_id: &str) -> io::Result<u64> {
        let seq = self.seq;
        let payload =
            serde_json::to_vec(&JournalRecordRef::ConditionalDisarmed { seq, now_ms, arm_id })
                .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one auto-liquidation's RELEASED reduce-only MARKET order (borrowed — no clone),
    /// write-ahead of the `apply_intent` release it records.
    ///
    /// `mount_id` is `None` for the ACCOUNT-wide margin-call sweep and `Some(id)` for the per-mount
    /// budget latch's flatten, which ONE mount owns — see [`JournalRecord::MarginCallLiquidate`] for
    /// why that distinction has to survive to disk. Returns the assigned seq.
    pub fn append_margin_call_liquidate(
        &mut self,
        now_ms: i64,
        req: &vike_model::OrderRequest,
        mount_id: Option<&str>,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::MarginCallLiquidate {
            seq,
            now_ms,
            req,
            mount_id,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one MANAGED GTD/Day expiry DECISION (borrowed coid — no clone), write-ahead of the
    /// `cancel_order` it records. See [`JournalRecord::GtdExpire`] for why this is an audit marker
    /// rather than a replayable command. Returns the assigned seq.
    pub fn append_gtd_expire(&mut self, now_ms: i64, coid: &str, engine: usize) -> io::Result<u64> {
        let seq = self.seq;
        let payload =
            serde_json::to_vec(&JournalRecordRef::GtdExpire { seq, now_ms, coid, engine })
                .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one WALL-CLOCK SCHEDULE FIRE DECISION (borrowed `mount_id`/`tag` — no clone),
    /// write-ahead of the `Strategy::on_schedule` it drives. See [`JournalRecord::ScheduleFire`] for
    /// why this is a replay-neutral audit marker (the on_schedule ORDERS journal on their own as
    /// `StrategySubmit` records that replay re-applies). Returns the assigned seq.
    pub fn append_schedule_fire(
        &mut self,
        now_ms: i64,
        mount_id: &str,
        tag: &str,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload =
            serde_json::to_vec(&JournalRecordRef::ScheduleFire { seq, now_ms, mount_id, tag })
                .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal a full-state snapshot-as-command (borrowed slices — no clone). `arm_seq` is the
    /// runtime's emulated-conditional arm-id counter, stamped next to `coid_seq`; `conditionals`
    /// is the resting conditional books, in fire order; `contingencies` is the resting OTO/OCO
    /// book (held exits carry their request); `mount_attr` is the per-mount attribution ledgers —
    /// see the [`JournalRecord::Snap`] field docs for why all four ride the Snap. Returns the seq.
    // Positional on purpose, mirroring the Snap record's own field list — a params struct would
    // just restate `JournalRecordRef::Snap` a third time.
    #[allow(clippy::too_many_arguments)]
    pub fn append_snap(
        &mut self,
        now_ms: i64,
        engines: &[vike_exec::EngineSnapshot],
        coid_session: &str,
        coid_seq: u64,
        arm_seq: u64,
        conditionals: &[SnapConditional],
        contingencies: &[SnapContingency],
        mount_attr: &[SnapMountAttr],
        hash: u64,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::Snap {
            seq,
            now_ms,
            engines,
            coid_session,
            coid_seq,
            arm_seq,
            conditionals,
            contingencies,
            mount_attr,
            hash,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
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

    /// The format [`VERSION`] stamped on the HIGHEST-indexed segment — i.e. the shape the journal's
    /// LAST records (and therefore its last `Snap`) were written under. `None` when the directory
    /// holds no readable segment.
    ///
    /// Exists for the ONE consumer that must distinguish "the field was absent" from "the field
    /// was present and empty": [`crate::replay::replay_offline`]'s conditional-book fence, which
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
    /// It exists for [`crate::replay::replay_offline`], whose determinism fence otherwise reports a
    /// truncated read as a hash divergence: with the tail gone, the last READABLE `Snap` is a
    /// mid-session checkpoint rather than the session's exit `Snap`, so re-folding the tail past it
    /// necessarily produces a different hash. `read_all` stays the entry point for every other
    /// reader (nothing about their behaviour changes).
    ///
    /// LOGGING: the halt is warned about HERE, exactly once per call — the arm returns immediately,
    /// so a torn journal cannot produce a per-record (or even a per-segment) log storm. Nothing on
    /// this path runs on the latency-gated fold thread: `read_all` is an offline/startup reader, and
    /// the fold's only journal calls are `append_*`/`write_snap`.
    pub(crate) fn read_all_reporting(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::testutil::*;

    /// MEASUREMENT, not a gate: how much of the journal append tail is FIRST-TOUCH PAGE FAULTS.
    ///
    /// [`reserve_blocks`] removes the filesystem ALLOCATION from inside the write fault. It does
    /// not remove the FAULT. A `MAP_SHARED` mapping starts with NO page-table entries, so the first
    /// store into each 4 KiB page still traps into the kernel — whether or not its block is already
    /// on disk.
    ///
    /// ⚠ This paragraph ended "Nothing in this crate populates those entries: there is no
    /// `MAP_POPULATE`, no `madvise`, no touch pass" until 2026-08-29, and that outlived the change
    /// which falsified it. [`crate::journal::segment`]'s `warm_ahead` DOES populate them, by
    /// `advise_range(memmap2::Advice::PopulateWrite, ..)`, and carries its own A/B measurement of
    /// the effect. So this harness now measures a mapping something else is warming, which is
    /// exactly the case its own "how to read it" note below calls the signal that "this whole line
    /// of attack is wrong" — read that note with the warming in mind rather than as a surprise.
    /// ⚠ The one place the original sentence still holds is the ROLL: `map_segment` declares "THE
    /// ROLL PATH IS NOT COVERED", so the appender's NEW mapping after a roll is warmed by nothing
    /// and pays first-touch faults as it fills.
    ///
    /// How to read what it prints: `faults` should land near `pages` if the tail really is
    /// first-touch work, and `over_20us` should be the same order. If `faults` sits far BELOW
    /// `pages`, something already warmed the mapping and this whole line of attack is wrong.
    ///
    /// Counted from `/proc/self/stat` rather than `getrusage` deliberately — the crate lint is
    /// `unsafe_code = "deny"` with exactly TWO audited carve-outs, and a measurement is not a good
    /// enough reason to open a third.
    ///
    /// Release-only, `--ignored`, filter `measure_first_touch`; run it the way
    /// `crates/vike-core/CLAUDE.md`'s A/B recipe runs the latency harness, on the dedicated cores.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "measurement, not a gate — see crates/vike-core/CLAUDE.md's A/B recipe"]
    fn measure_first_touch_faults_on_the_append_path() {
        /// Minor faults so far, field 10 of `/proc/self/stat`. Parsed after the LAST `)` because
        /// field 2 is the comm and may itself contain parentheses and spaces.
        fn minor_faults() -> u64 {
            let s = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");
            let tail = &s[s.rfind(')').expect("comm closes") + 1..];
            tail.split_whitespace().nth(7).expect("minflt").parse().expect("minflt parses")
        }

        const APPENDS: usize = 100_000;
        let dir = tmp_dir("measure-faults");
        // The latency gate's own shape: 256 MiB so no roll happens across the run, flush 256.
        let cfg = JournalFileConfig { segment_bytes: 256 * 1024 * 1024, flush_every: 256 };
        let t_open = std::time::Instant::now();
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        let open_us = t_open.elapsed().as_micros();

        let start_cursor = j.cursor;
        let f0 = minor_faults();
        let mut lat = Vec::with_capacity(APPENDS);
        for i in 0..APPENDS {
            let t = std::time::Instant::now();
            j.append_cmd(1_000 + i as i64, &ingest(i as u64)).unwrap();
            lat.push(t.elapsed().as_nanos() as u64);
        }
        let faults = minor_faults() - f0;
        let bytes = j.cursor - start_cursor;
        let pages = bytes.div_ceil(4096);

        lat.sort_unstable();
        let at = |q: f64| lat[((lat.len() as f64 * q) as usize).min(lat.len() - 1)];
        let over = |ns: u64| lat.iter().filter(|&&v| v > ns).count();

        println!(
            "MEASURE-FAULTS appends={APPENDS} bytes={bytes} pages={pages} faults={faults} open_us={open_us} \
             p50={} p99={} p999={} max={} over_20us={} over_100us={}",
            at(0.50),
            at(0.99),
            at(0.999),
            lat[lat.len() - 1],
            over(20_000),
            over(100_000),
        );
        drop(j);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// MEASUREMENT, not a gate: WHERE the journal append's time actually goes — serialization
    /// (`serde_json::to_vec`) versus framing (the memcpy into the mapped pages plus the periodic
    /// `flush_async`).
    ///
    /// # Why this exists
    ///
    /// [`warm_ahead`]'s doc carries the A/B that REFUTED the first-touch page-fault hypothesis:
    /// warming removed every fault and moved the append's own p99 from 3 697 ns to ~290 ns, while
    /// the CORE-HOP p99.9 the latency gate measures went 37 721 -> 37 300 ns — i.e. nothing.
    /// [`CommandJournal::map_segment`]'s doc names three causes for that residue; the third is now
    /// eliminated, which leaves serialization and the memcpy. This harness separates those two
    /// rather than assuming which one it is.
    ///
    /// # How to read what it prints
    ///
    /// Rows per mode, all in nanoseconds, plus a `clock` control row measuring an empty
    /// `Instant::now()` pair so the instrumentation's own cost is visible rather than assumed (it
    /// MATTERS at the p50 of a sub-µs append and is noise at the p99.9).
    ///
    ///   * `ser`   — building the payload bytes.
    ///   * `frame` — [`CommandJournal::write_framed`]: the three `copy_from_slice`s into the
    ///     mapping, the counters, and (on one append in `flush_every`) the `flush_async` syscall.
    ///   * `frame-flush` / `frame-noflush` — `frame` split on whether THAT append tripped
    ///     `since_flush >= flush_every`. `flush_every` is 256, so the flush appends are 0.39% of
    ///     the run: exactly the p99.9 population. If the syscall is the tail, it shows up here as
    ///     a `frame-flush` p50 far above the `frame-noflush` p99.9.
    ///   * `total` — the two summed, i.e. what [`CommandJournal::append_cmd`] costs.
    ///
    /// FOUR MODES are run, ALTERNATING (A B C D A B C D …) so a drifting box cannot masquerade as
    /// a difference, and the reported figure per statistic is the MEDIAN over the reps. They are
    /// the cross of two axes:
    ///
    ///   * message shape — `cancel` is [`ingest`]'s small `OrderIntent::Cancel`; `fill` is
    ///     [`fill_ingest`], which is `crates/vike-core/tests/runtime_latency.rs`'s `fill_with_coid`
    ///     field-for-field, i.e. the record the LATENCY GATE's journal variants actually write.
    ///     Read the `fill` rows for anything about the gate; the `cancel` rows are the floor.
    ///   * serializer — `to_vec` is today's code: a FRESH `Vec` allocated per append. `to_writer`
    ///     serializes the same bytes into a buffer the caller owns and reuses (`clear()` +
    ///     `serde_json::to_writer`). Byte-identical output; the ONLY difference is the per-append
    ///     allocation.
    ///
    /// …plus one CONTROL, `fill/to_vec+warmall`, which populates every page table at open instead
    /// of one `segment::warm_ahead_bytes` window. It re-runs the page-fault hypothesis at the APPEND level
    /// — the level the core-hop A/B could not isolate — and a tail that does not move under it is
    /// a tail that is not first-touch work.
    ///
    /// A `payload-bytes` row states the record size each table was priced on — a size, not a
    /// latency, so all four of its columns carry the same figure.
    ///
    /// Release-only, `--ignored`, filter `measure_append_cost_split`; run it the way
    /// `crates/vike-core/CLAUDE.md`'s A/B recipe runs the latency harness, on the dedicated cores.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "measurement, not a gate — see crates/vike-core/CLAUDE.md's A/B recipe"]
    fn measure_append_cost_split() {
        const APPENDS: usize = 100_000;
        const REPS: usize = 8;
        const FLUSH_EVERY: u32 = 256;

        /// One rep's labelled statistics: `(what, [p50, p99, p999, max])` per row.
        type Rows = Vec<(&'static str, [u64; 4])>;

        /// One configuration under measurement. Named configurations rather than a full cross of
        /// the axes: the cross would spend reps on combinations nothing asks about.
        #[derive(Clone, Copy)]
        struct Mode {
            /// The gate's `FillEvent` (275 B) rather than [`ingest`]'s cancel (85 B).
            fill_shape: bool,
            /// `serde_json::to_writer` into a reused buffer rather than a per-append `to_vec`.
            reuse_buffer: bool,
            /// Populate the WHOLE mapping's page tables at open, not just one window.
            warm_all: bool,
            /// The segment size, which is also what picks `sync_chunk_bytes` — and therefore how
            /// often the syncer re-aims the warm window. 256 MiB is the `journal` gate variant's;
            /// 64 MiB is `JournalConfig::at`'s, i.e. what a real node runs.
            segment_bytes: u64,
            name: &'static str,
        }

        /// p50 / p99 / p99.9 / max of a sample, in the order this harness prints them.
        fn quants(v: &mut [u64]) -> [u64; 4] {
            v.sort_unstable();
            let at = |q: f64| v[((v.len() as f64 * q) as usize).min(v.len() - 1)];
            [at(0.50), at(0.99), at(0.999), v[v.len() - 1]]
        }

        /// One rep of `APPENDS` appends in one mode. Returns the labelled samples, each already
        /// reduced to `[p50, p99, p999, max]`.
        fn rep(mode: Mode, verbose: bool, tag: &str) -> Rows {
            let Mode { fill_shape, reuse_buffer, warm_all, segment_bytes, .. } = mode;
            let dir = tmp_dir(tag);
            let cfg = JournalFileConfig { segment_bytes, flush_every: FLUSH_EVERY };
            let mut j = CommandJournal::open(&dir, cfg).unwrap();
            if warm_all {
                // The CONTROL for the page-fault hypothesis, re-run at the APPEND level. `open`
                // warms one `segment::warm_ahead_bytes` window and the syncer re-aims it after each chunk
                // `msync`, so a run whose WAL outgrows the window between refreshes faults over the
                // gap. This populates the whole mapping instead, which cannot leave a gap. If the
                // tail does not move, faults are refuted here as they already were at the core hop.
                let len = j.map.len();
                j.map.advise_range(memmap2::Advice::PopulateWrite, 0, len).unwrap();
            }
            let mut buf: Vec<u8> = Vec::with_capacity(4096);
            let mut ser = Vec::with_capacity(APPENDS);
            let mut frame_flush = Vec::new();
            let mut frame_noflush = Vec::with_capacity(APPENDS);
            let mut total = Vec::with_capacity(APPENDS);
            let mut clock = Vec::with_capacity(APPENDS);
            // WHERE in the run each cost lands, not just how large it is. `queued` records the
            // append index at which `queue_sync` last advanced the watermark — i.e. the moment the
            // SYNCER thread started `msync`ing a `sync_chunk` of pages this loop is still storing
            // into. If the tail is writeback contention rather than anything the append itself
            // does, the worst appends cluster immediately AFTER one of these indices.
            let mut chunk_at: Vec<usize> = Vec::new();
            let mut last_queued = j.queued_to;
            let mut by_index: Vec<(u64, usize)> = Vec::with_capacity(APPENDS);
            for i in 0..APPENDS {
                // The GATE's own message when `fill_shape`, the small cancel otherwise:
                // `runtime_latency.rs`'s journal variants journal a `FillEvent`, which serializes
                // to ~2.5x the cancel — so it is the shape whose cost the gate actually pays.
                let msg = if fill_shape {
                    fill_ingest(1_000_000_000 + i as i64)
                } else {
                    ingest(i as u64)
                };
                let now_ms = 1_000 + i as i64;
                let seq = j.next_seq();
                let is_flush = (i as u32 + 1).is_multiple_of(FLUSH_EVERY);
                let t0 = std::time::Instant::now();
                if reuse_buffer {
                    buf.clear();
                    serde_json::to_writer(
                        &mut buf,
                        &JournalRecordRef::Cmd { seq, now_ms, msg: &msg },
                    )
                    .expect("JournalRecordRef serializes");
                } else {
                    buf = serde_json::to_vec(&JournalRecordRef::Cmd { seq, now_ms, msg: &msg })
                        .expect("JournalRecordRef serializes");
                }
                let t1 = std::time::Instant::now();
                // Split the borrow through a length: `write_framed` takes `&mut self` while the
                // payload borrows `buf`, a LOCAL here. Not a copy the real append path would pay.
                let n = buf.len();
                j.write_framed(&buf[..n]).unwrap();
                let t2 = std::time::Instant::now();
                // The instrumentation's own cost, sampled with the same clock, same cadence.
                let c0 = std::time::Instant::now();
                let c1 = std::time::Instant::now();
                clock.push((c1 - c0).as_nanos() as u64);
                ser.push((t1 - t0).as_nanos() as u64);
                total.push((t2 - t0).as_nanos() as u64);
                let f = (t2 - t1).as_nanos() as u64;
                if is_flush {
                    frame_flush.push(f);
                } else {
                    frame_noflush.push(f);
                }
                // Both AFTER `t2`, so neither is inside a measured interval.
                by_index.push((f, i));
                if j.queued_to != last_queued {
                    last_queued = j.queued_to;
                    chunk_at.push(i);
                }
            }
            if verbose {
                // The ten worst FRAMES with their positions, against the chunk-sync positions.
                by_index.sort_unstable_by_key(|&(ns, _)| std::cmp::Reverse(ns));
                let worst: Vec<(usize, u64)> =
                    by_index.iter().take(10).map(|&(ns, i)| (i, ns)).collect();
                println!(
                    "SPLIT-TAIL {tag} chunk_sync_at={chunk_at:?} worst_frames_(i,ns)={worst:?}"
                );
            }
            let bytes = buf.len() as u64;
            let mut frame: Vec<u64> =
                frame_noflush.iter().chain(frame_flush.iter()).copied().collect();
            let out: Rows = vec![
                ("clock", quants(&mut clock)),
                ("ser", quants(&mut ser)),
                ("frame", quants(&mut frame)),
                ("frame-noflush", quants(&mut frame_noflush)),
                ("frame-flush", quants(&mut frame_flush)),
                ("total", quants(&mut total)),
                // A SIZE, not a latency — the payload length every row above was priced on,
                // carried in the same shape so one table states both.
                ("payload-bytes", [bytes; 4]),
            ];
            drop(j);
            out
        }

        // ALTERNATED (never mode-major) so box drift cannot pose as a mode difference.
        //
        //   * `cancel/*` is the FLOOR and doubles as a control: 85 B records put ~9.3 MB of WAL in
        //     the segment, which never reaches `sync_chunk_bytes` — so no chunk `msync` runs at all
        //     and its tail is first-touch faults alone.
        //   * `fill/*` is the shape the latency gate writes (275 B, ~28 MB, ONE chunk crossing).
        //   * `fill/to_vec+warmall` is `fill/to_vec` with every page table populated at open.
        const SEG_GATE: u64 = 256 * 1024 * 1024; // the `journal` latency variant's segment
        const SEG_PROD: u64 = 64 * 1024 * 1024; // `JournalConfig::at`'s — what a node runs
        let modes: [Mode; 6] = [
            Mode {
                fill_shape: false,
                reuse_buffer: false,
                warm_all: false,
                segment_bytes: SEG_GATE,
                name: "cancel/to_vec",
            },
            Mode {
                fill_shape: false,
                reuse_buffer: true,
                warm_all: false,
                segment_bytes: SEG_GATE,
                name: "cancel/to_writer",
            },
            Mode {
                fill_shape: true,
                reuse_buffer: false,
                warm_all: false,
                segment_bytes: SEG_GATE,
                name: "fill/to_vec",
            },
            Mode {
                fill_shape: true,
                reuse_buffer: true,
                warm_all: false,
                segment_bytes: SEG_GATE,
                name: "fill/to_writer",
            },
            Mode {
                fill_shape: true,
                reuse_buffer: false,
                warm_all: true,
                segment_bytes: SEG_GATE,
                name: "fill/to_vec+warmall",
            },
            Mode {
                fill_shape: true,
                reuse_buffer: false,
                warm_all: false,
                segment_bytes: SEG_PROD,
                name: "fill/to_vec/seg64",
            },
        ];
        let mut runs: Vec<(&str, Rows)> = Vec::new();
        for r in 0..REPS {
            for m in modes {
                // The positional detail only from the FIRST rep of each mode — it is one line per
                // rep and eight copies of it say nothing the first does not.
                let tag = format!("{}-{r}", m.name.replace(['/', '+'], "-"));
                runs.push((m.name, rep(m, r == 0, &tag)));
            }
        }

        for m in modes {
            let mode = m.name;
            let reps: Vec<&Rows> =
                runs.iter().filter(|(n, _)| *n == mode).map(|(_, v)| v).collect();
            for (row, (label, _)) in reps[0].iter().enumerate() {
                // MEDIAN of each statistic across the reps — never a best case (single runs lie on
                // this box; `crates/vike-core/CLAUDE.md`'s recipe is emphatic about it).
                let med = |col: usize| {
                    let mut xs: Vec<u64> = reps.iter().map(|r| r[row].1[col]).collect();
                    xs.sort_unstable();
                    xs[xs.len() / 2]
                };
                println!(
                    "SPLIT mode={mode:<20} what={label:<14} p50={:<8} p99={:<8} p999={:<8} max={}",
                    med(0),
                    med(1),
                    med(2),
                    med(3),
                );
            }
        }
    }

    #[test]
    fn append_reopen_read_back_in_order() {
        let dir = tmp_dir("roundtrip");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 4 };
        let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
        for i in 0..100 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j); // flush on drop
        let back = CommandJournal::read_all(&dir).unwrap();
        assert_eq!(back.len(), 100);
        for (i, r) in back.iter().enumerate() {
            match r {
                JournalRecord::Cmd { seq, .. } => assert_eq!(*seq, i as u64),
                _ => panic!("unexpected"),
            }
        }
        // reopen resumes the sequence
        let j2 = CommandJournal::open(&dir, cfg).unwrap();
        assert_eq!(j2.next_seq(), 100);
    }

    #[test]
    fn rolls_to_a_new_segment_when_full() {
        let dir = tmp_dir("roll");
        let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..200 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);
        // Count SEGMENTS, not directory entries: the dir also holds the interlock sentinel
        // (`LOCK`), so a raw `read_dir` count would read 2 with only ONE segment present.
        let segs = seg_files(&dir);
        assert!(segs.len() >= 2, "expected multiple segments, got {}", segs.len());
        assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 200);
    }

    #[test]
    fn prune_deletes_fully_superseded_segments_and_keeps_the_latest_snap_onward() {
        let dir = tmp_dir("prune");
        // small segments so the journal rolls; the Snap lands in a LATER segment
        let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..300 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        let snap_seq =
            j.append_snap(9_999, &snap_engines(), "sess", 7, 0, &[], &[], &[], 0xABCD).unwrap();
        for i in 300..360 {
            j.append_cmd(2_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);

        let files_before = seg_files(&dir).len();
        assert!(files_before >= 3, "scenario must span multiple segments (got {files_before})");
        let all_before = CommandJournal::read_all(&dir).unwrap();
        let max_seq_before = all_before.iter().map(record_seq).max().unwrap();

        CommandJournal::prune_before_latest_snap(&dir).unwrap();

        // Earliest, fully-superseded segment(s) are gone.
        let files_after = seg_files(&dir).len();
        assert!(files_after < files_before, "fully-superseded early segments must be deleted");
        assert!(
            !dir.join("journal-00000000.vjl").exists(),
            "segment 0 (all seqs < latest snap seq) must be pruned"
        );

        // The latest-Snap record + everything after it survive (restore base + its tail intact).
        let all_after = CommandJournal::read_all(&dir).unwrap();
        assert!(
            all_after
                .iter()
                .any(|r| matches!(r, JournalRecord::Snap { seq, .. } if *seq == snap_seq)),
            "the latest Snap must survive pruning (it is the restore base)"
        );
        let max_seq_after = all_after.iter().map(record_seq).max().unwrap();
        assert_eq!(max_seq_after, max_seq_before, "records after the snap must not be lost");
        let first_seq_after = record_seq(all_after.first().unwrap());
        assert!(first_seq_after > 0, "the pruned early segment held seq 0; it is gone");
        assert!(
            first_seq_after <= snap_seq,
            "the snap's own segment is kept WHOLE (earlier cmds too)"
        );
    }

    #[test]
    fn prune_respects_the_materializer_checkpoint_floor() {
        let dir = tmp_dir("prune_mat");
        let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..300 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        j.append_snap(9_999, &snap_engines(), "sess", 7, 0, &[], &[], &[], 0xABCD).unwrap();
        for i in 300..360 {
            j.append_cmd(2_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);
        let files_before = seg_files(&dir).len();
        assert!(files_before >= 3);

        // A materializer that has consumed only up to seq 0 → NOTHING past seq 0 may be pruned,
        // even the segments the snapshot alone would drop.
        MaterializeCheckpoint::store(&dir, 0).unwrap();
        CommandJournal::prune_before_latest_snap(&dir).unwrap();
        assert_eq!(
            seg_files(&dir).len(),
            files_before,
            "no segment is pruned while the materializer is behind"
        );
        assert!(
            dir.join("journal-00000000.vjl").exists(),
            "segment 0 is kept: not yet materialized"
        );

        // Once the materializer catches up (past every seq), pruning proceeds snapshot-only again.
        MaterializeCheckpoint::store(&dir, u64::MAX).unwrap();
        CommandJournal::prune_before_latest_snap(&dir).unwrap();
        assert!(
            seg_files(&dir).len() < files_before,
            "a caught-up materializer no longer blocks pruning"
        );
    }

    #[test]
    fn prune_never_deletes_the_latest_snap_segment_or_later() {
        // The Snap lands in the EARLIEST segment; every later segment holds only post-snap Cmds.
        // Nothing may be deleted — the snap's segment and all later ones are the restore base+tail.
        let dir = tmp_dir("prune-safe");
        let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..5 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        let snap_seq =
            j.append_snap(9_999, &snap_engines(), "sess", 0, 0, &[], &[], &[], 1).unwrap();
        for i in 5..300 {
            j.append_cmd(2_000 + i as i64, &ingest(i)).unwrap(); // roll into later segments
        }
        drop(j);

        let files_before = seg_files(&dir).len();
        assert!(files_before >= 2, "must span multiple segments (got {files_before})");
        let n_before = CommandJournal::read_all(&dir).unwrap().len();

        CommandJournal::prune_before_latest_snap(&dir).unwrap();

        assert_eq!(
            seg_files(&dir).len(),
            files_before,
            "no segment may be deleted: the snap is in the earliest segment, later segments are its tail"
        );
        let all_after = CommandJournal::read_all(&dir).unwrap();
        assert_eq!(all_after.len(), n_before, "no records may be lost");
        assert!(
            all_after
                .iter()
                .any(|r| matches!(r, JournalRecord::Snap { seq, .. } if *seq == snap_seq)),
            "the latest snap survives"
        );
    }

    #[test]
    fn prune_with_no_snap_is_a_noop() {
        let dir = tmp_dir("prune-nosnap");
        let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..300 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);
        let files_before = seg_files(&dir).len();
        let n_before = CommandJournal::read_all(&dir).unwrap().len();

        CommandJournal::prune_before_latest_snap(&dir).unwrap();

        assert_eq!(seg_files(&dir).len(), files_before, "no Snap -> prune nothing");
        assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), n_before, "no records lost");
    }

    // ---- duplicate-instance interlock (see `crate::journal_lock`) -------------------------------

    /// The small-segment config the interlock tests use — nothing here writes enough to roll, and
    /// the default 64 MiB pre-allocation is pointless for four extra directories.
    fn lock_cfg() -> JournalFileConfig {
        JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 }
    }

    /// (a) A FIRST open of a fresh directory succeeds and takes the directory lock.
    #[test]
    fn first_open_succeeds_and_takes_the_directory_lock() {
        let dir = tmp_dir("lock-first");
        let j = CommandJournal::open(&dir, lock_cfg()).unwrap();
        assert!(
            dir.join(crate::journal_lock::LOCK_FILE).exists(),
            "opening a journal materializes the interlock sentinel next to the segments"
        );
        assert_eq!(j.next_seq(), 0, "a fresh journal still starts at seq 0");
        drop(j);
    }

    /// (b) A SECOND open of the SAME directory while the first journal is ALIVE is refused — fast,
    /// with an actionable error naming the directory, never a silent second writer. This is the
    /// double-launch case: two writers interleave frames and break the replay determinism fence.
    #[test]
    fn a_second_open_of_the_same_dir_is_refused_while_the_first_lives() {
        let dir = tmp_dir("lock-second");
        let first = CommandJournal::open(&dir, lock_cfg()).unwrap();
        // `CommandJournal` holds an `MmapMut` (no `Debug`), so `unwrap_err()` isn't available —
        // match, exactly as `open_refuses_to_resume_a_segment_with_an_unsupported_version` does.
        match CommandJournal::open(&dir, lock_cfg()) {
            Ok(_) => panic!(
                "a second writer on one journal directory must be refused — two mmap writers \
                 interleave WAL frames and corrupt the determinism fence"
            ),
            Err(e) => {
                assert_eq!(
                    e.kind(),
                    io::ErrorKind::AddrInUse,
                    "the refusal is the distinctive AddrInUse, not a generic io error"
                );
                let msg = e.to_string();
                let want = dir.display().to_string();
                assert!(msg.contains(want.as_str()), "the error names the journal dir: {msg}");
            }
        }
        drop(first);
    }

    /// (c) After the first guard DROPS the lock is released, so a later open of the same directory
    /// succeeds and resumes the sequence. The leftover `LOCK` file is not a stale lock — which is
    /// exactly the crash-restart shape (a killed process's handle is closed by the OS).
    #[test]
    fn a_new_open_succeeds_after_the_first_journal_drops() {
        let dir = tmp_dir("lock-release");
        let mut first = CommandJournal::open(&dir, lock_cfg()).unwrap();
        first.append_cmd(1_000, &ingest(0)).unwrap();
        drop(first);
        assert!(
            dir.join(crate::journal_lock::LOCK_FILE).exists(),
            "the sentinel FILE survives the drop (only the lock is released)"
        );
        let second = CommandJournal::open(&dir, lock_cfg())
            .expect("a released directory re-opens — a leftover sentinel is never a stale lock");
        assert_eq!(second.next_seq(), 1, "the re-opened journal resumes the sequence");
        drop(second);
    }

    /// (d) Two DIFFERENT directories open fine at the same time — the interlock is per journal
    /// directory, not a process-global singleton (multi-core/multi-profile sessions still work).
    #[test]
    fn two_different_journal_dirs_open_concurrently() {
        let a = tmp_dir("lock-dir-a");
        let b = tmp_dir("lock-dir-b");
        let mut ja = CommandJournal::open(&a, lock_cfg()).unwrap();
        let mut jb = CommandJournal::open(&b, lock_cfg())
            .expect("a DIFFERENT journal dir is unaffected by the first one's lock");
        ja.append_cmd(1_000, &ingest(0)).unwrap();
        jb.append_cmd(1_000, &ingest(1)).unwrap();
        drop(ja);
        drop(jb);
        assert_eq!(CommandJournal::read_all(&a).unwrap().len(), 1);
        assert_eq!(CommandJournal::read_all(&b).unwrap().len(), 1);
    }

    /// The OFF/unchanged path: the interlock adds a sibling sentinel FILE and NOTHING else. No
    /// journal record, no segment byte, no new dir entry any reader sees — `read_all`,
    /// `latest_segment_version` and `prune_before_latest_snap` all key off the `journal-*.vjl`
    /// name, so the recorded stream (and therefore every `state_hash`) is byte-for-byte what it was
    /// before the lock existed. Pruning likewise never touches the sentinel.
    #[test]
    fn the_interlock_adds_no_record_and_is_invisible_to_every_reader() {
        let dir = tmp_dir("lock-inert");
        let mut j = CommandJournal::open(&dir, lock_cfg()).unwrap();
        for i in 0..5 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);

        assert!(dir.join(crate::journal_lock::LOCK_FILE).exists(), "the sentinel is present");
        assert_eq!(seg_files(&dir).len(), 1, "the sentinel is NOT counted as a segment");
        let back = CommandJournal::read_all(&dir).unwrap();
        assert_eq!(back.len(), 5, "the interlock appends no record of its own");
        let seqs: Vec<u64> = back.iter().map(record_seq).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3, 4], "seqs are exactly what they were before the lock");
        assert_eq!(
            CommandJournal::latest_segment_version(&dir).unwrap(),
            Some(VERSION),
            "the sentinel is skipped by the version probe (it carries no MAGIC)"
        );

        // No Snap anywhere ⇒ prune is a no-op, and it never removes the sentinel.
        CommandJournal::prune_before_latest_snap(&dir).unwrap();
        assert_eq!(seg_files(&dir).len(), 1);
        assert!(dir.join(crate::journal_lock::LOCK_FILE).exists(), "prune leaves the sentinel");
        assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 5);
    }

    /// A segment ROLL must CARRY the directory lock, not drop it: `roll` rebuilds the whole
    /// `CommandJournal` and assigns it over `*self`, so a naive swap would release the interlock
    /// mid-session and let a second instance in. Roll the journal, then prove a concurrent open is
    /// STILL refused while the rolled journal lives.
    #[test]
    fn the_lock_survives_a_segment_roll() {
        let dir = tmp_dir("lock-roll");
        let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
        let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
        for i in 0..200 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        assert!(seg_files(&dir).len() >= 2, "precondition: the journal actually rolled");
        match CommandJournal::open(&dir, cfg.clone()) {
            Ok(_) => panic!("the interlock must still be held after a segment roll"),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::AddrInUse),
        }
        drop(j);
        // and it IS released once the rolled journal drops
        let reopened = CommandJournal::open(&dir, cfg).expect("released after the rolled drop");
        drop(reopened);
    }
}

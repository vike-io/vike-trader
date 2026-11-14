//! **The sync policy, the SYNC thread and the cell the journal's threads share.**
//!
//! The asynchronous journal with a durable copy
//! (decision 0279, option C, the owner, 2026-11-12). The core thread frames into an anonymous stage and publishes how
//! far it got; the WRITE thread (`handoff.rs`) is the only one that writes the WORKING segment
//! (`pwrite`), and nothing syncs that file while it is live, so no sync can hold up the write thread,
//! and with it an order waiting on [`crate::WriteFence`]. The SYNC thread here makes the bytes durable
//! through a second file, the segment's DURABLE COPY (`durable.rs`): on each chunk or snap request it
//! appends the stage bytes the copy lacks and syncs the copy — the power-cut window of before. When the
//! write thread leaves a segment (a roll, the shutdown) the sync thread syncs the working file whole
//! and deletes its copy. It also keeps the stage's warm window ahead of the core.
//!
//! ⚠ Adding a wait, a file write or a bounded/blocking send to the APPEND side, or a sync of the live
//! working file anywhere, reintroduces the defect this design removes, and the unit tests at the bottom
//! of this file cannot catch it: they pin that each sync still happens and happens HERE, never the
//! latency. The gates for the latency claim are `tests/runtime_latency.rs`'s journal variants.

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use memmap2::MmapRaw;

use crate::durable::{create_durable, remove_durable};
use crate::handoff::{copy_span, off_of, ord_of, write_at};

/// How much freshly-appended data may sit un-SYNCED before the writer asks the syncer thread to
/// force it to disk ([`super::Journal::queue_sync`]).
///
/// This bounds how much dirty data any ONE `msync` ever has to move — which is otherwise set by
/// how much has piled up when a segment is sealed at [`super::Journal::roll`]. That cost is
/// proportional to dirty bytes — measured on the latency box: 1MB dirty ≈ 8.6ms, 16MB ≈ 100ms, a full 64MB
/// segment ≈ 292ms (max 432ms). Note `flush_async` is ~2µs and is ALREADY issued every
/// `flush_every` records, so the kernel already knows about every dirty page; only forcing
/// COMPLETION earlier bounds the seal. Measured over 1.48M appends at max rate (64MB segments),
/// back when this sync still ran INLINE on the append path:
///
/// ```text
///   chunk    worst append   appends >1ms
///   off        339.6 ms          2
///   4MB         36.3 ms         32
///   1MB         25.2 ms        128
/// ```
///
/// ⚠ **Read that table as history, not as today's cost.** Every one of those numbers is an APPEND
/// stall, and appends no longer pay for syncs at all: the whole column moved to the syncer thread
/// (see the module doc). What the divisor still buys is depth-per-`msync` — a syncer that moves
/// 4MiB at a time keeps up with a burst, where one that discovers 64MiB of debt at the seal does
/// not — plus the un-synced tail a hard power cut can lose. The old trade the table encodes (depth
/// vs frequency, and a settled-rate roll going 7.4ms -> ~11.9ms because the writer pays for syncs
/// the kernel's 30s flusher would have done for free) is now entirely off-fold.
///
/// **The chunk is a FRACTION OF THE SEGMENT, not a fixed 4MiB.** A fixed 4MiB made every journal
/// pay, including ones that will never roll at all and so have no seal to bound — and back when the
/// sync was inline that cost was charged straight to the measured core hop: main run for e2b21632
/// put ~3 forced syncs of **~30ms each** into `runtime_latency` (`max=30235675ns at hop #29241`,
/// and 32.3ms at #29219 on the next attempt — 29241 x ~143B ≈ 4.18MB, i.e. exactly the chunk
/// boundary, deterministic where contention spikes land at random indices). `segment/16` keeps a
/// CONSTANT ~16 syncs per segment whatever the segment size, so a seal stays bounded to 1/16th of a
/// segment while a journal that never fills one issues no chunk sync at all. Keep it a fraction.
///
/// ⚠ **CORRECTED — the claim this doc used to make about the harness ("a journal that never fills a
/// segment pays NOTHING") stopped being true, silently, when the record shape grew.** The
/// arithmetic, which is what to re-check rather than the conclusion: `runtime_latency` writes
/// `HOP_SAMPLES`-many records (100 000) into a **256MiB** segment, so `sync_chunk_bytes` =
/// max(256MiB/16, 4MiB) = **16MiB**. When the doc was written a framed record was ~143B, so the run
/// wrote 100 000 x 143B ≈ 14.3MB = **13.6MiB — under 16MiB, zero boundaries, zero syncs**. Records
/// are now ~287B framed (the run's own summary line closes it: `max_at_hop=58518` with a 16MiB
/// chunk ⇒ 16 777 216 / 58 518 ≈ **287B/record**, consistent with the reported
/// `wal_bytes_min=23800000` ⇒ 238B/record excluding the `JournalRecordRef::Command` wrapper). So the run
/// now writes 100 000 x 287B ≈ 28.7MB = **27.4MiB — it crosses 16MiB exactly once** and never
/// reaches 32MiB. One crossing, one forced sync, one ~141ms hop, at #58 518 of 100 000 (the
/// reference run's #58 293 and the 58 510 / 58 517 of later runs are the same crossing). That is
/// why the crossing is no longer paid inline; `a_journal_that_never_rolls_never_forces_a_sync` pins
/// the fraction rule with a record size small enough to still cross nothing.
///
/// Why not "defer until the segment is nearly full, then catch up": deferring does not remove the
/// work, it CONCENTRATES it. Reaching a 75% watermark with 48MB un-synced means the syncer must
/// move all 48MB in the last quarter — a burst of back-to-back multi-ms syncs it can fall behind
/// on, strictly worse than spreading them.
const SYNC_CHUNK_DIVISOR: u64 = 16;

/// Floor for [`SYNC_CHUNK_DIVISOR`]: below this a "chunk" is too small to be worth a syscall, and
/// tiny segments (the unit tests use KB-sized ones) roll cheaply enough to need no bounding at all
/// — a segment smaller than this floor therefore never forces a sync.
const SYNC_CHUNK_MIN: usize = 4 * 1024 * 1024;

/// The un-synced debt ceiling for a segment of `segment_bytes` — see [`SYNC_CHUNK_DIVISOR`].
pub(super) fn sync_chunk_bytes(segment_bytes: u64) -> usize {
    usize::try_from(segment_bytes / SYNC_CHUNK_DIVISOR).unwrap_or(usize::MAX).max(SYNC_CHUNK_MIN)
}

/// WHERE the syncer aims the warm window after a chunk `msync`: at the APPENDER, not at the
/// watermark it just advanced.
///
/// One line, extracted, because the one-word difference between `appender` and `synced` here is the
/// whole of a 37 µs append tail and nothing in a single-threaded test can see it — the appender is
/// idle while the syncer runs, so `appender == synced` in every unit test and the wrong version
/// passes them all. Making it a function at least lets [`tests::the_warm_window_is_aimed_at_the_appender_not_the_watermark`]
/// pin that `appender` participates. `max` rather than a bare `appender` because the watermark is
/// the floor: a rebased or clamped target must never aim the window BACKWARDS.
///
/// The measurement behind it is on `super::segment::warm_ahead_bytes`: the flush this follows takes
/// 36-141 ms and the appender runs at ~430 MB/s throughout, so by the time it returns the cursor is
/// megabytes past `synced`.
fn warm_from(appender: usize, synced: usize) -> usize {
    appender.max(synced)
}

// ================================================================================================
// The sync thread. Every sync this journal performs happens below this line, on a thread neither
// the core nor the write thread ever waits for (only `Drop` joins it, after the write thread).
// ================================================================================================

/// The cell the core, the write thread and the sync thread share. Watermarks, not work queues: each is
/// MONOTONIC, so a wake that coalesces with an earlier one loses nothing and no backlog can grow
/// however far behind the disk falls. That is what keeps every hand-off from the core non-blocking.
#[derive(Debug, Default)]
pub(crate) struct JournalSyncShared {
    /// Packed position (`handoff::pos`) the core asked the sync thread to make durable (a chunk
    /// boundary or a snap).
    pub(crate) target: AtomicU64,
    /// Packed position of the end of the core's last frame in the stage: stored on EVERY append.
    pub(crate) appended: AtomicU64,
    /// Packed position up to which the write thread has handed the stage to the working file.
    pub(crate) written: AtomicU64,
    /// Header stamps the core has sent the write thread, and how many it has written: a fence waits
    /// for both counts to meet, so a stamp with no frame after it is covered too.
    pub(crate) stamps_sent: AtomicU64,
    pub(crate) stamps_done: AtomicU64,
    /// Set by `Drop`: the write thread drains everything appended and ends.
    pub(crate) stop_writer: AtomicBool,
    /// Set when the write thread has ended (a fence then has no one to wait for).
    pub(crate) writer_gone: AtomicBool,
    /// True while a [`SyncMessage::Wake`] the sync thread has not consumed yet sits in the channel. Bounds
    /// the wake backlog to ONE — see [`JournalSyncer::request`] for the ordering argument.
    wake_pending: AtomicBool,
    /// Set the first time a send fails (the sync thread is gone), so the warning fires once.
    lost: AtomicBool,
    /// The offset of the core's live segment that is durable (in its copy, or in the working file
    /// once sealed). Written only by the sync thread.
    pub(crate) synced_to: AtomicUsize,
    /// Completed copy syncs. Written only by the sync thread: the seam the unit tests use to assert a
    /// sync happened AT ALL and happened THERE (coalesced wakes make it lower than the number of
    /// requests, by design).
    pub(crate) ranges: AtomicU64,
    /// Warm-window refreshes of the stage, by the sync thread.
    pub(crate) warms: AtomicU64,
    /// Completed seals (the full sync of a working segment the write thread has left at a roll).
    pub(crate) seals: AtomicU64,
    /// Completed shutdown syncs. Exactly 1 for any journal that was opened and dropped.
    pub(crate) finals: AtomicU64,
}

/// What the core and the write thread hand the sync thread. Every variant is non-blocking to send
/// (the channel is unbounded), so a slow disk can never back-pressure either of them.
pub(crate) enum SyncMessage {
    /// (core) "The sync target moved." Carries no work; the work is `JournalSyncShared::target`.
    Wake,
    /// (core) Its frames go on in segment `index` (ordinal `ordinal`), framed into `stage` from `base`
    /// on: keep that segment's durable copy from now on.
    Live { index: u64, ordinal: u64, stage: Arc<MmapRaw>, base: usize },
    /// (core) The header version rises to `version` before the frame at `at`: into the copy, synced,
    /// before any byte at or past `at` is.
    Stamp { at: u64, version: u32 },
    /// (write thread) It left segment `index` at a roll: sync the working file whole, then delete the
    /// segment's copy.
    Seal { work: File, index: u64 },
    /// (write thread) It drained everything and stops: the same for the live segment, last.
    Final { work: File, index: u64 },
    /// A sync of everything appended so far, NOW, answered when done (`Journal::flush`, tools
    /// and tests only).
    FlushNow(Sender<()>),
}

/// The core's handle on the sync thread. ONE thread serves a journal directory for its whole life.
pub(crate) struct JournalSyncer {
    tx: Sender<SyncMessage>,
    pub(crate) shared: Arc<JournalSyncShared>,
    /// `Option` only so [`Self::shutdown_and_join`] can move the handle out.
    handle: Option<std::thread::JoinHandle<()>>,
}

/// One segment's durable copy, as the sync thread holds it.
struct Copy {
    index: u64,
    ordinal: u64,
    stage: Arc<MmapRaw>,
    /// `None` when the copy could not be made, or once the segment was sealed.
    file: Option<File>,
    /// How far the copy holds the stage's bytes.
    off: usize,
}

impl JournalSyncer {
    /// Start the thread on the live segment `index` (ordinal 0), whose bytes below `base` are already
    /// durable in its working file (the resume point, `Journal::open`). Its durable copy is
    /// made HERE, so a copy that cannot be made refuses the open rather than silently losing the
    /// power-cut guarantee.
    pub(crate) fn spawn(
        dir: PathBuf,
        index: u64,
        stage: Arc<MmapRaw>,
        base: usize,
        shared: Arc<JournalSyncShared>,
    ) -> io::Result<Self> {
        let file = create_durable(&dir, index)?;
        shared.synced_to.store(base, Ordering::Release);
        let live = Copy { index, ordinal: 0, stage, file: Some(file), off: base };
        let (tx, rx) = mpsc::channel();
        let s = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("vjl-sync".into())
            .spawn(move || run_syncer(rx, &s, &dir, live))?;
        Ok(JournalSyncer { tx, shared, handle: Some(handle) })
    }

    /// A sender for the write thread's `Seal` and `Final`.
    pub(crate) fn sender(&self) -> Sender<SyncMessage> {
        self.tx.clone()
    }

    fn send(&self, m: SyncMessage) {
        if self.tx.send(m).is_err() {
            self.note_lost();
        }
    }

    /// The core rolled to segment `index` (`ordinal`), framed into `stage`.
    pub(crate) fn live(&self, index: u64, ordinal: u64, stage: Arc<MmapRaw>) {
        self.send(SyncMessage::Live { index, ordinal, stage, base: 0 });
    }

    /// The core raised the header to `version` before the frame at `at`.
    pub(crate) fn stamp(&self, at: u64, version: u32) {
        self.send(SyncMessage::Stamp { at, version });
    }

    /// Ask for everything up to the packed position `upto` to be made durable, and RETURN. Never
    /// blocks, never waits, never fails the append.
    ///
    /// The `wake_pending` dance is the backlog bound. The sync thread clears the flag BEFORE it reads
    /// `target`, so `swap` returning `true` here means "a wake not consumed yet is queued, and when it
    /// is consumed it reads a target at least as new as the one just stored": skipping the send loses
    /// nothing and the channel holds at most one wake.
    pub(crate) fn request(&self, upto: u64) {
        self.shared.target.fetch_max(upto, Ordering::AcqRel);
        if !self.shared.wake_pending.swap(true, Ordering::AcqRel)
            && self.tx.send(SyncMessage::Wake).is_err()
        {
            self.note_lost();
        }
    }

    /// Make everything appended so far durable on the sync thread, and wait for it (tools and tests
    /// only: never from the fold).
    pub(crate) fn flush_now(&self) -> io::Result<()> {
        let (ack, done) = mpsc::channel();
        self.tx
            .send(SyncMessage::FlushNow(ack))
            .map_err(|_| io::Error::other("the journal sync thread is gone"))?;
        done.recv().map_err(|_| io::Error::other("the journal sync thread is gone"))
    }

    /// Graceful stop, AFTER the write thread has drained, sent its `Final` and ended: close the
    /// channel and WAIT. `recv` returns `Err` only once the queue is DRAINED and every sender is gone,
    /// so the thread has synced the live working file and dropped its copy before this returns — "a
    /// clean shutdown does not lose the tail".
    pub(crate) fn shutdown_and_join(self) {
        let JournalSyncer { tx, shared: _, handle } = self;
        drop(tx);
        if let Some(h) = handle {
            let _ = h.join();
        }
    }

    /// Report a vanished sync thread ONCE. Reached only if the thread is gone, which `run_syncer`
    /// makes impossible short of process teardown (it is panic-free by construction).
    fn note_lost(&self) {
        if !self.shared.lost.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "journal sync thread is gone — WAL durability degrades to kernel writeback"
            );
        }
    }
}

/// The sync thread body. Panic-free by construction: a failed write or sync is logged and retried on
/// the next request, never unwrapped.
fn run_syncer(
    rx: Receiver<SyncMessage>,
    sh: &JournalSyncShared,
    dir: &std::path::Path,
    mut live: Copy,
) {
    // Copies of segments the core has left whose working file the write thread has not sealed yet.
    let mut retired: Vec<Copy> = Vec::new();
    // Seals that arrived before the core's `Live` moved past their segment (two senders, no order).
    let mut sealed_early: Vec<u64> = Vec::new();
    let mut stamps: VecDeque<(u64, u32)> = VecDeque::new();
    loop {
        match rx.recv() {
            Ok(SyncMessage::Wake) => {
                // Clear BEFORE reading the target — see `JournalSyncer::request`.
                sh.wake_pending.store(false, Ordering::Release);
                let target = sh.target.load(Ordering::Acquire);
                copy_and_sync(&mut live, &mut stamps, target, sh);
                // Keep the stage's warm window ahead of the core (anonymous memory: no file).
                let a = sh.appended.load(Ordering::Acquire);
                if ord_of(a) == live.ordinal {
                    crate::segment::warm_ahead_raw(&live.stage, warm_from(off_of(a), live.off));
                    sh.warms.fetch_add(1, Ordering::Relaxed);
                }
            }
            Ok(SyncMessage::Live { index, ordinal, stage, base }) => {
                let file = create_durable(dir, index)
                    .map_err(|e| {
                        tracing::error!(error = %e, segment = index, "journal durable copy not made: \
                            this segment is durable only at its seal");
                    })
                    .ok();
                let old =
                    std::mem::replace(&mut live, Copy { index, ordinal, stage, file, off: base });
                if let Some(k) = sealed_early.iter().position(|&i| i == old.index) {
                    sealed_early.swap_remove(k);
                } else {
                    retired.push(old);
                }
                sh.synced_to.store(base, Ordering::Release);
                // The core may already have asked for bytes of the new segment.
                copy_and_sync(&mut live, &mut stamps, sh.target.load(Ordering::Acquire), sh);
            }
            Ok(SyncMessage::Stamp { at, version }) => stamps.push_back((at, version)),
            Ok(SyncMessage::Seal { work, index }) => {
                seal(dir, &work, index, &mut live, &mut retired, &mut sealed_early);
                sh.seals.fetch_add(1, Ordering::Relaxed);
            }
            Ok(SyncMessage::Final { work, index }) => {
                if seal(dir, &work, index, &mut live, &mut retired, &mut sealed_early) {
                    let w = sh.written.load(Ordering::Acquire);
                    if ord_of(w) == live.ordinal {
                        sh.synced_to.store(off_of(w), Ordering::Release);
                    }
                }
                sh.finals.fetch_add(1, Ordering::Relaxed);
            }
            Ok(SyncMessage::FlushNow(ack)) => {
                let a = sh.appended.load(Ordering::Acquire);
                copy_and_sync(&mut live, &mut stamps, a, sh);
                let _ = ack.send(());
            }
            // Empty AND every sender dropped: `Drop` stopped the write thread (its `Final` is above).
            Err(_) => return,
        }
    }
}

/// Bring the live segment's copy up to the packed position `upto` (a target of an earlier segment is
/// already covered by that segment's seal), applying the version stamps that fall below it first, and
/// sync it.
fn copy_and_sync(
    live: &mut Copy,
    stamps: &mut VecDeque<(u64, u32)>,
    upto: u64,
    sh: &JournalSyncShared,
) {
    while let Some(&(at, _)) = stamps.front()
        && ord_of(at) < live.ordinal
    {
        stamps.pop_front(); // an earlier segment's: its seal synced the working file's header
    }
    if ord_of(upto) != live.ordinal {
        return;
    }
    let Some(file) = &live.file else { return };
    let end = off_of(upto).min(live.stage.len());
    let mut step = || -> io::Result<()> {
        while let Some(&(at, version)) = stamps.front() {
            if ord_of(at) != live.ordinal || off_of(at) > end {
                break;
            }
            copy_span(&live.stage, file, live.off, off_of(at))?;
            live.off = live.off.max(off_of(at));
            write_at(file, &version.to_le_bytes(), 4)?;
            file.sync_data()?;
            stamps.pop_front();
        }
        if end > live.off {
            copy_span(&live.stage, file, live.off, end)?;
            file.sync_data()?;
            live.off = end;
            sh.synced_to.store(end, Ordering::Release);
            sh.ranges.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    };
    // Do NOT advance on failure: the bytes stay owed and the next request retries them.
    if let Err(e) = step() {
        tracing::warn!(error = %e, "journal durable copy sync failed");
    }
}

/// Sync the working file of segment `index` whole, then delete its durable copy (never before: a power
/// cut between the two must still find the bytes somewhere). `true` when the sync succeeded.
fn seal(
    dir: &std::path::Path,
    work: &File,
    index: u64,
    live: &mut Copy,
    retired: &mut Vec<Copy>,
    sealed_early: &mut Vec<u64>,
) -> bool {
    if let Err(e) = work.sync_all() {
        // The copy stays: the next boot repairs from it.
        tracing::warn!(error = %e, segment = index, "journal segment seal failed; its durable copy is kept");
        return false;
    }
    if let Some(k) = retired.iter().position(|c| c.index == index) {
        retired.swap_remove(k);
    } else if live.index == index {
        // Sealed before the core's `Live` moved past it (or at the shutdown): nothing more goes in.
        live.file = None;
        sealed_early.push(index);
    }
    if let Err(e) = remove_durable(dir, index) {
        tracing::warn!(error = %e, segment = index, "journal durable copy not deleted (the next boot does)");
    }
    true
}

#[path = "sync_tests.rs"]
#[cfg(test)]
mod sync_tests;

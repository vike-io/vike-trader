//! **The sync policy and the syncer thread — the fold-path latency contract.**
//!
//! EVERY blocking `msync` this journal performs happens in this file, on a thread the appender
//! never joins. That is not an implementation detail: `CommandJournal` is appended to from the
//! vike-core FOLD thread, the one the `p99 < 10µs` core-hop gate protects, and an `msync` costs
//! time proportional to dirty bytes (measured on the latency box: 1MB ≈ 8.6ms, 16MB ≈ 100ms, a dirty 64MB
//! segment ≈ 292ms, max 432ms). #929 and #932 moved the three inline `msync` sites here; the
//! parent module's "Where the blocking `msync` happens" section is the full argument, including
//! why a watermark (not a queue) is what lets the hand-off be unconditionally non-blocking.
//!
//! ⚠ Adding an `msync`, a wait, or a bounded/blocking send to the APPEND side reintroduces exactly
//! the defect those PRs removed, and the unit tests at the bottom of this file cannot catch it —
//! they pin that each `msync` still happens and happens HERE, never the latency. The gate for the
//! latency claim is `tests/runtime_latency.rs`'s journal variant.
//!
//! Split out of `journal.rs` verbatim.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use memmap2::MmapRaw;

use crate::format::HEADER;

/// How much freshly-appended data may sit un-SYNCED before the writer asks the syncer thread to
/// force it to disk ([`super::CommandJournal::queue_sync`]).
///
/// This bounds how much dirty data any ONE `msync` ever has to move — which is otherwise set by
/// how much has piled up when a segment is sealed at [`super::CommandJournal::roll`]. That cost is
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
/// `wal_bytes_min=23800000` ⇒ 238B/record excluding the `JournalRecordRef::Cmd` wrapper). So the run
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
// The syncer thread. EVERY blocking `msync` this module performs happens below this line, on a
// thread the appender never joins — see the module doc's "Where the blocking `msync` happens".
// ================================================================================================

/// The cell the appender publishes into and the syncer reads back.
///
/// Deliberately a watermark rather than a work queue: `target` is a MONOTONIC byte offset, so a
/// wake that coalesces with an earlier one loses nothing (the syncer always reads the newest), and
/// the backlog therefore cannot grow no matter how far behind the disk falls. That is what lets the
/// appender's hand-off be unconditionally non-blocking.
#[derive(Debug, Default)]
pub(crate) struct SyncShared {
    /// Byte offset in the CURRENT segment the appender wants forced to disk. Monotonic **within a
    /// segment**, and re-based to [`HEADER`] by [`Syncer::hand_over`] BEFORE a rolled segment's
    /// mapping is handed over — that ordering is the whole reason the syncer can never carry a
    /// stale (old-segment) offset onto the fresh mapping and skip over live bytes.
    target: AtomicUsize,
    /// True while a [`SyncMsg::Wake`] the syncer has not consumed yet sits in the channel. Bounds
    /// the wake backlog to ONE — see [`Syncer::request`] for the ordering argument.
    wake_pending: AtomicBool,
    /// Set the first time a send fails (the syncer thread is gone), so the warning fires once
    /// rather than once per chunk.
    lost: AtomicBool,
    /// The offset the syncer has actually forced to disk in the current segment. Written ONLY by
    /// the syncer; the appender never reads it, and specifically never waits on it.
    synced_to: AtomicUsize,
    /// Completed chunk range-syncs. Written only by the syncer thread, so it costs the fold path
    /// nothing, and it is the seam the unit tests use to assert an `msync` happened AT ALL and
    /// happened THERE. `ranges` can be LOWER than the number of chunk boundaries crossed — that is
    /// coalescing working as designed, not a lost sync.
    ranges: AtomicU64,
    /// Warm-window refreshes performed by the syncer. Written only by the syncer thread, like
    /// [`Self::ranges`], and it exists for the same reason: it is the only seam a test has on work
    /// whose EFFECT (populated page tables) is invisible from Rust. A chunk sync must bump this
    /// TWICE — once before the blocking `msync` and once after — and the "before" one is the half
    /// no timing test can catch, because its whole purpose is to have already happened by the time
    /// the appender needs it. See `segment::warm_ahead_bytes`.
    warms: AtomicU64,
    /// Completed segment seals (the whole-segment `msync` a roll used to pay inline).
    seals: AtomicU64,
    /// Completed shutdown syncs. Exactly 1 for any journal that was opened and dropped.
    finals: AtomicU64,
}

/// What the appender hands the syncer. **Both variants are non-blocking to send** — the channel is
/// unbounded — so a slow disk can never back-pressure the fold path.
enum SyncMsg {
    /// "The watermark moved." Carries no work; the work is `SyncShared::target`.
    Wake,
    /// A segment roll: seal the segment the syncer currently holds — ONE whole-segment `msync`, the
    /// blocking flush [`super::CommandJournal::roll`] used to run inline on the fold path — then
    /// adopt the carried mapping as the new live segment.
    Roll(MmapRaw),
}

/// The appender's handle on the syncer thread. Owned by [`super::CommandJournal`] and MOVED across
/// every [`super::CommandJournal::roll`] (exactly like the directory lock), so ONE thread serves a
/// journal directory for its whole life rather than one per segment.
pub(super) struct Syncer {
    tx: Sender<SyncMsg>,
    pub(super) shared: Arc<SyncShared>,
    /// `Option` only so [`Self::shutdown_and_join`] can move the handle out; always `Some` while
    /// the journal is live.
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Syncer {
    /// Start the thread on `map` — the syncer's OWN mapping of the live segment — treating
    /// everything below `synced_from` as already durable (the resume rebase, see
    /// [`super::CommandJournal::open`]).
    ///
    /// A failed spawn is propagated rather than degraded-to-`None`: a journal with no syncer would
    /// silently issue no `msync` at all until it is dropped, and the ONE other way `syncer` is
    /// `None` (a `roll`-superseded value) carries a specific meaning `CommandJournal`'s `Drop` reads.
    pub(super) fn spawn(map: MmapRaw, synced_from: usize) -> io::Result<Self> {
        let shared = Arc::new(SyncShared::default());
        shared.target.store(synced_from, Ordering::Release);
        shared.synced_to.store(synced_from, Ordering::Release);
        let (tx, rx) = mpsc::channel();
        let s = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("vjl-sync".into())
            .spawn(move || run_syncer(rx, &s, map, synced_from))?;
        Ok(Syncer { tx, shared, handle: Some(handle) })
    }

    /// Ask for everything up to `upto` to be forced to disk, and RETURN. Never blocks, never waits,
    /// never fails the append — this is the entirety of what a chunk boundary costs the fold path.
    ///
    /// The `wake_pending` dance is the backlog bound. The syncer clears the flag BEFORE it reads
    /// `target`, so `swap` returning `true` here means "a wake the syncer has not consumed yet is
    /// queued, and when it consumes it, it will read a target at least as new as the one just
    /// stored" — i.e. skipping the send loses nothing and the channel holds at most one wake.
    pub(super) fn request(&self, upto: usize) {
        self.shared.target.store(upto, Ordering::Release);
        if !self.shared.wake_pending.swap(true, Ordering::AcqRel)
            && self.tx.send(SyncMsg::Wake).is_err()
        {
            self.note_lost();
        }
    }

    /// Hand over the segment [`super::CommandJournal::roll`] just rolled TO, which also tells the
    /// syncer to seal the one it currently holds.
    ///
    /// Re-basing `target` to [`HEADER`] first is load-bearing: without it a syncer that has already
    /// installed the new mapping could read a watermark left over from the OLD segment, mark that
    /// much of the FRESH segment as synced, and then never range-sync the live bytes underneath it.
    /// Storing `HEADER` first makes the only stale value it can read a no-op.
    pub(super) fn hand_over(&self, map: MmapRaw) {
        self.shared.target.store(HEADER, Ordering::Release);
        if self.tx.send(SyncMsg::Roll(map)).is_err() {
            self.note_lost();
        }
    }

    /// Graceful stop: publish the final watermark, close the channel, and WAIT.
    ///
    /// The join is the point. `recv` returns `Err` only once the queue is DRAINED and every sender
    /// is gone, so the syncer processes whatever is still pending, does one final whole-segment
    /// `msync`, and only then exits — which is what makes "a clean shutdown does not lose the tail"
    /// true. Detaching here would let the process exit with the tail only in the page cache.
    pub(super) fn shutdown_and_join(self, upto: usize) {
        let Syncer { tx, shared, handle } = self;
        shared.target.store(upto, Ordering::Release);
        drop(tx);
        if let Some(h) = handle {
            let _ = h.join();
        }
    }

    /// Report a vanished syncer ONCE. Reached only if the thread is gone, which `run_syncer` makes
    /// impossible short of process teardown (it is panic-free by construction — every fallible call
    /// is matched and logged, never unwrapped).
    fn note_lost(&self) {
        if !self.shared.lost.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "journal syncer thread is gone — WAL durability degrades to kernel writeback"
            );
        }
    }
}

/// The syncer thread body: **every blocking `msync` in this module**.
///
/// Panic-free by construction — an `msync` failure is logged and retried on the next wake, never
/// unwrapped — so the appender's `send` can only fail during process teardown.
fn run_syncer(rx: Receiver<SyncMsg>, shared: &SyncShared, mut map: MmapRaw, mut synced: usize) {
    loop {
        match rx.recv() {
            Ok(SyncMsg::Wake) => {
                // Clear BEFORE reading the watermark — see `Syncer::request` for why that order is
                // what makes a skipped send safe.
                shared.wake_pending.store(false, Ordering::Release);
                let target = shared.target.load(Ordering::Acquire).min(map.len());
                if target > synced {
                    // ⚠ WARM BEFORE THE BLOCKING FLUSH, AND AIM AT THE APPENDER. `flush_range`
                    // below is a blocking `msync` measured at ~36 ms (4 MiB chunk) to ~141 ms
                    // (16 MiB), and the appender runs at ~430 MB/s throughout — so this is the last
                    // moment the window can be refreshed before it has to carry the fold thread
                    // unaided. It is aimed at `target`, the appender's own cursor, because the
                    // post-flush refresh below used to aim at `synced` and therefore populated
                    // pages the appender had ALREADY faulted on. See `segment::warm_ahead_bytes`
                    // for the measurement that found this.
                    crate::segment::warm_ahead_raw(&map, target);
                    shared.warms.fetch_add(1, Ordering::Relaxed);
                    match map.flush_range(synced, target - synced) {
                        Ok(()) => {
                            synced = target;
                            shared.synced_to.store(synced, Ordering::Release);
                            shared.ranges.fetch_add(1, Ordering::Relaxed);
                            // ...and pull the WARM WINDOW along, aimed at WHERE THE APPENDER IS
                            // NOW rather than at the watermark. The flush above took tens to
                            // hundreds of milliseconds and the appender did not stop, so `synced`
                            // is behind it — re-reading `target` is what keeps this refresh in
                            // front of the cursor instead of behind it. Costs the fold path
                            // nothing: it runs here, on the syncer.
                            let ahead = shared.target.load(Ordering::Acquire).min(map.len());
                            crate::segment::warm_ahead_raw(&map, warm_from(ahead, synced));
                            shared.warms.fetch_add(1, Ordering::Relaxed);
                        }
                        // Do NOT advance `synced` on failure: the region stays owed and the next
                        // wake retries it. A failing `msync` means the WAL's bytes are not reaching
                        // disk, which is exactly the condition the journal exists to surface.
                        Err(e) => tracing::warn!(error = %e, "journal chunk sync failed"),
                    }
                }
            }
            Ok(SyncMsg::Roll(next)) => {
                // The seal — measured 292ms (max 432ms) on a dirty 64MB segment, which `roll` used
                // to pay INLINE on the fold thread.
                //
                // The appender's own mapping of this segment is already gone (`roll`'s
                // `*self = next` munmapped it). That is harmless and worth stating: `munmap` does
                // not discard dirty page-cache pages, and this is a MAP_SHARED mapping of the same
                // file, so it addresses exactly those pages.
                if let Err(e) = map.flush() {
                    tracing::warn!(error = %e, "journal segment seal failed");
                }
                shared.seals.fetch_add(1, Ordering::Relaxed);
                map = next;
                synced = HEADER;
                // A ROLLED segment is warmed HERE rather than in `roll`, which runs on the append
                // path — `map_segment(reserve = false)` deliberately warms nothing for it.
                crate::segment::warm_ahead_raw(&map, synced);
                shared.synced_to.store(synced, Ordering::Release);
            }
            // Empty AND every sender dropped — `CommandJournal::drop` is blocked on our join. Force
            // the whole live segment out before returning: this is the "a graceful stop must not
            // lose the tail" guarantee.
            Err(_) => {
                match map.flush() {
                    Ok(()) => shared.synced_to.store(
                        shared.target.load(Ordering::Acquire).min(map.len()),
                        Ordering::Release,
                    ),
                    Err(e) => tracing::warn!(error = %e, "journal shutdown sync failed"),
                }
                shared.finals.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
    }
}

#[path = "sync_tests.rs"]
#[cfg(test)]
mod sync_tests;

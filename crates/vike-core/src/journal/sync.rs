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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use memmap2::MmapRaw;

use super::HEADER;

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
                    crate::journal::segment::warm_ahead_raw(&map, target);
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
                            crate::journal::segment::warm_ahead_raw(&map, warm_from(ahead, synced));
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
                crate::journal::segment::warm_ahead_raw(&map, synced);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::testutil::*;
    use crate::journal::{CommandJournal, JournalFileConfig};

    /// GATE: [`warm_from`] tracks the APPENDER once it is past the watermark.
    ///
    /// Small, and deliberately kept anyway. The alternative is nothing at all: the aim only matters
    /// while the appender is running concurrently with a blocking `msync`, which no unit test in
    /// this file arranges, so the version that aims at `synced` — the one that cost 37 µs at the
    /// append's p99.9 — is green against every other test here.
    ///
    /// ⚠ Declared residual: this pins the DECISION, not the CALL SITE. A future edit that stops
    /// calling `warm_from` at all, or passes it `synced` twice, leaves this green. The measurement
    /// harness (`journal::writer`'s `measure_append_cost_split`) is what would catch that, and it
    /// is `--ignored`.
    #[test]
    fn the_warm_window_is_aimed_at_the_appender_not_the_watermark() {
        // The real case: the appender ran on for the whole msync and is megabytes ahead.
        assert_eq!(warm_from(20 * 1024 * 1024, 16 * 1024 * 1024), 20 * 1024 * 1024);
        // ...and the watermark is the FLOOR, so a clamped/rebased target cannot aim backwards.
        assert_eq!(warm_from(HEADER, 16 * 1024 * 1024), 16 * 1024 * 1024);
        assert_eq!(warm_from(4096, 4096), 4096);
    }

    /// The un-REQUESTED debt must stay bounded by the segment's chunk — that bound is what keeps
    /// any single `msync` (including the seal, which costs time proportional to whatever is still
    /// dirty when it runs) from having to move a whole segment at once.
    #[test]
    fn unrequested_debt_stays_bounded_while_appending() {
        let dir = tmp_dir("syncdebt");
        // A segment several chunks wide, so the bound is actually exercised rather than trivially
        // satisfied by the segment being smaller than one chunk.
        let seg = (SYNC_CHUNK_MIN * SYNC_CHUNK_DIVISOR as usize * 3) as u64;
        let chunk = sync_chunk_bytes(seg);
        let cfg = JournalFileConfig { segment_bytes: seg, flush_every: 256 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        let mut worst = 0usize;
        // FAT records (~4KiB framed) so the run crosses the 12MiB chunk in 5 000 appends rather
        // than the ~140 000 the ~90B `ingest` would need. `crossed` below PROVES the crossing
        // happened rather than leaving it to record-size arithmetic that can silently drift — which
        // it did, exactly once, and cost the p99 gate 141ms: see `SYNC_CHUNK_DIVISOR`'s corrected
        // harness note.
        let mut crossed = 0usize;
        for i in 0..5_000u64 {
            let before = j.queued_to;
            j.append_cmd(1_000 + i as i64, &fat_ingest(i)).unwrap();
            if j.queued_to != before {
                crossed += 1;
            }
            worst = worst.max(j.cursor.saturating_sub(j.queued_to));
        }
        assert!(crossed > 0, "test appended too little to cross a {chunk}-byte chunk at all");
        assert!(
            worst < chunk + 8192,
            "un-requested debt reached {worst} bytes, over the {chunk}-byte bound"
        );
        drop(j);
        assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 5_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A journal whose segment is far from full must issue no chunk sync at all — the property that
    /// keeps `SYNC_CHUNK_DIVISOR` a FRACTION of the segment rather than the fixed 4MiB that reached
    /// main in #789 and reddened the p99 gate (`max=30235675ns at hop #29241`, 4.18MB in — exactly
    /// that fixed chunk's boundary).
    ///
    /// ⚠ This is NO LONGER the `runtime_latency` harness's shape, and the difference is the whole
    /// reason the forced sync moved off the append path. The harness writes 100 000 records into a
    /// 256MiB segment (chunk = 16MiB): at the ~143B/record of the era this test was written that was
    /// 13.6MiB and crossed nothing, but records are now ~287B, so it writes ~27.4MiB and crosses the
    /// boundary ONCE — one ~141ms `msync`, on the fold thread, at hop #58 518 of 100 000. Sizing the
    /// chunk as a fraction cured the COUNT (3 syncs became 1), never the depth. So this test keeps
    /// the small ~90B `ingest` deliberately (≈8.9MiB over 100 000 appends, still short of the 16MiB
    /// chunk) and the two `precondition:` asserts below bracket that rather than trusting the
    /// estimate: what it pins is the FRACTION RULE, not a claim about the harness.
    #[test]
    fn a_journal_that_never_rolls_never_forces_a_sync() {
        let dir = tmp_dir("norollsync");
        let cfg = JournalFileConfig { segment_bytes: 256 * 1024 * 1024, flush_every: 256 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        let shared = j.sync_shared();
        for i in 0..100_000u64 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        assert!(j.cursor > 4 * 1024 * 1024, "precondition: wrote past a fixed 4MiB chunk");
        assert!(j.cursor < j.sync_chunk, "precondition: stayed under the FRACTIONAL chunk");
        assert_eq!(
            j.queued_to, HEADER,
            "a journal far from rolling must not have REQUESTED any sync (cursor={}, chunk={})",
            j.cursor, j.sync_chunk
        );
        drop(j);
        assert_eq!(
            shared.ranges.load(Ordering::Acquire),
            0,
            "...and the syncer must therefore have performed none"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Resuming must not re-sync the whole existing prefix (that would move the very stall this
    /// bounding removes to startup instead — and now onto the syncer, where it would delay the
    /// first REAL sync behind a pointless one).
    #[test]
    fn resume_rebases_the_sync_debt_onto_the_existing_cursor() {
        let dir = tmp_dir("syncresume");
        let cfg = JournalFileConfig { segment_bytes: 8 * 1024 * 1024, flush_every: 256 };
        let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
        for i in 0..20_000u64 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);
        let j2 = CommandJournal::open(&dir, cfg).unwrap();
        let shared = j2.sync_shared();
        assert!(j2.cursor > HEADER, "precondition: the resumed segment has records");
        assert_eq!(j2.queued_to, j2.cursor, "resume must owe nothing for already-written bytes");
        assert_eq!(
            shared.synced_to.load(Ordering::Acquire),
            j2.cursor,
            "the syncer must start on the same rebased cursor, not at HEADER"
        );
        drop(j2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- the syncer thread ------------------------------------------------------------------
    //
    // What these can and cannot prove. They pin that each of the three `msync` sites (chunk / seal /
    // shutdown) still happens, happens ON THE SYNCER (the counters are written by that thread and
    // nowhere else), and that the data survives — plus the shutdown-drain guarantee, which is the
    // one whose failure mode is silent data loss. What NO unit test here proves is the LATENCY
    // claim: "the appender does not wait" needs a slow disk and a measured hop, and the gate for
    // that is `tests/runtime_latency.rs`'s journal variant (JOURNAL_MAX_NS / JOURNAL_HOPS_OVER_100US
    // / JOURNAL_P999_NS), not this module.

    /// Chunk syncs must be performed BY THE SYNCER, and the appender must not wait for them: it
    /// advances `queued_to` at the boundary and keeps going.
    ///
    /// Note the deliberately loose upper bound on `ranges`: wakes COALESCE (one un-consumed wake at
    /// a time, the work being a monotonic watermark), so a syncer that falls behind legitimately
    /// covers several boundaries in one `msync`. Fewer syncs than boundaries is the design working,
    /// not a lost sync — which is exactly why the appender never has to block to bound the backlog.
    #[test]
    fn chunk_syncs_are_performed_by_the_syncer_thread() {
        let dir = tmp_dir("syncthread");
        // 32MiB segment ⇒ chunk = max(2MiB, SYNC_CHUNK_MIN) = 4MiB. ~4KiB records x 5 000 ≈ 19.4MiB
        // ⇒ 4 boundaries crossed, and no roll (19.4MiB < 32MiB).
        let cfg = JournalFileConfig { segment_bytes: 32 * 1024 * 1024, flush_every: 256 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        let shared = j.sync_shared();
        let mut crossed = 0usize;
        for i in 0..5_000u64 {
            let before = j.queued_to;
            j.append_cmd(1_000 + i as i64, &fat_ingest(i)).unwrap();
            if j.queued_to != before {
                assert_eq!(j.queued_to, j.cursor, "the appender must advance without waiting");
                crossed += 1;
            }
        }
        assert!(crossed >= 3, "precondition: expected several chunk crossings, got {crossed}");
        assert_eq!(j.seg_idx, 0, "precondition: this test must not roll");
        let cursor = j.cursor;
        drop(j); // drains + joins the syncer

        let ranges = shared.ranges.load(Ordering::Acquire);
        assert!(
            (1..=crossed as u64).contains(&ranges),
            "expected 1..={crossed} syncer-side chunk syncs, got {ranges}"
        );
        assert_eq!(shared.seals.load(Ordering::Acquire), 0, "no roll happened, so no seal");
        assert_eq!(shared.finals.load(Ordering::Acquire), 1, "exactly one shutdown sync");
        assert_eq!(
            shared.synced_to.load(Ordering::Acquire),
            cursor,
            "the syncer must have caught up to the final watermark before the join returned"
        );
        assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 5_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE: a chunk sync must warm the window TWICE — once BEFORE the blocking `msync` and once
    /// after — and the "before" half is the one that carries the fold thread.
    ///
    /// # Why a counter and not a timing test
    ///
    /// The effect of a warm is populated page tables, which no Rust-visible state reports; the only
    /// honest observation is a fault count, and that is a whole-process figure a unit test cannot
    /// isolate. So [`SyncShared::warms`] records the ACT. What it defends is structural and was
    /// missing for real: the refresh used to happen only after `flush_range`, a blocking `msync`
    /// measured at 36-141 ms, during which the appender keeps running at ~430 MB/s and leaves the
    /// window. Warming after the flush therefore populated pages the appender had ALREADY faulted
    /// on. `segment::warm_ahead_bytes` carries the measurement; this asserts the ordering survives.
    #[test]
    fn a_chunk_sync_warms_the_window_before_the_blocking_msync_as_well_as_after() {
        let dir = tmp_dir("syncwarm");
        // Same shape as `chunk_syncs_are_performed_by_the_syncer_thread`: 32MiB segment ⇒ 4MiB
        // chunk, ~4KiB records x 5 000 ≈ 19.4MiB ⇒ several boundaries, no roll.
        let cfg = JournalFileConfig { segment_bytes: 32 * 1024 * 1024, flush_every: 256 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        let shared = j.sync_shared();
        for i in 0..5_000u64 {
            j.append_cmd(1_000 + i as i64, &fat_ingest(i)).unwrap();
        }
        assert_eq!(j.seg_idx, 0, "precondition: this test must not roll");
        drop(j); // drains + joins the syncer

        let ranges = shared.ranges.load(Ordering::Acquire);
        assert!(ranges >= 1, "precondition: expected at least one chunk sync, got {ranges}");
        let warms = shared.warms.load(Ordering::Acquire);
        assert_eq!(
            warms,
            2 * ranges,
            "every chunk sync must warm the window twice — once before `flush_range` and once \
             after — got {warms} warms for {ranges} chunk syncs"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The segment SEAL — `roll`'s old opening `self.map.flush()`, a 292ms whole-segment `msync` on
    /// a dirty 64MB segment — must happen on the syncer, and the rolled-to segment must come back
    /// with a re-based watermark rather than inheriting the old segment's offset.
    #[test]
    fn a_roll_seals_the_old_segment_on_the_syncer_thread() {
        let dir = tmp_dir("syncseal");
        // 8MiB segment ⇒ chunk = SYNC_CHUNK_MIN = 4MiB. ~4KiB x 3 000 ≈ 11.6MiB ⇒ exactly one roll.
        let cfg = JournalFileConfig { segment_bytes: 8 * 1024 * 1024, flush_every: 256 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        let shared = j.sync_shared();
        for i in 0..3_000u64 {
            j.append_cmd(1_000 + i as i64, &fat_ingest(i)).unwrap();
        }
        assert!(j.seg_idx >= 1, "precondition: expected a roll, still on segment {}", j.seg_idx);
        assert!(
            j.queued_to < 8 * 1024 * 1024,
            "the rolled-to segment's watermark must be re-based, not carried over"
        );
        let rolls = j.seg_idx;
        drop(j);

        assert_eq!(
            shared.seals.load(Ordering::Acquire),
            rolls,
            "every roll's whole-segment msync must have run on the syncer"
        );
        assert_eq!(shared.finals.load(Ordering::Acquire), 1, "exactly one shutdown sync");
        assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 3_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The shutdown-drain guarantee.** A journal whose tail never reaches a chunk boundary has had
    /// NO sync requested for it at all, so the final sync on `Drop` is the only thing that puts it on
    /// disk. Getting this wrong turns a graceful stop into silent data loss, so it is pinned
    /// directly: zero chunk syncs, zero seals, exactly one final sync — and the records read back.
    #[test]
    fn a_graceful_shutdown_syncs_the_untouched_tail() {
        let dir = tmp_dir("synctail");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 4 };
        let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
        let shared = j.sync_shared();
        for i in 0..50u64 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        let cursor = j.cursor;
        assert_eq!(j.queued_to, HEADER, "precondition: the tail must be entirely un-requested");
        assert_eq!(shared.finals.load(Ordering::Acquire), 0, "precondition: not stopped yet");
        drop(j);

        assert_eq!(shared.ranges.load(Ordering::Acquire), 0, "no chunk boundary was crossed");
        assert_eq!(shared.seals.load(Ordering::Acquire), 0, "no roll happened");
        assert_eq!(
            shared.finals.load(Ordering::Acquire),
            1,
            "Drop must drain and sync the tail exactly once"
        );
        assert_eq!(
            shared.synced_to.load(Ordering::Acquire),
            cursor,
            "the final sync must cover everything the appender wrote"
        );
        // ...and the records are actually there.
        let j2 = CommandJournal::open(&dir, cfg).unwrap();
        assert_eq!(j2.next_seq(), 50);
        drop(j2);
        assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 50);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ONE syncer thread serves the directory for its whole life: `roll` MOVES the handle to the
    /// successor instead of stopping one thread and starting another. Pinned through the shared
    /// cell's identity — if `roll` re-spawned, the successor would carry a fresh `SyncShared` and
    /// the counters observed here would be zero.
    #[test]
    fn the_syncer_is_carried_across_a_roll_not_respawned() {
        let dir = tmp_dir("synccarry");
        let cfg = JournalFileConfig { segment_bytes: 8 * 1024 * 1024, flush_every: 256 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        let before = j.sync_shared();
        for i in 0..3_000u64 {
            j.append_cmd(1_000 + i as i64, &fat_ingest(i)).unwrap();
        }
        assert!(j.seg_idx >= 1, "precondition: expected a roll, still on segment {}", j.seg_idx);
        let after = j.sync_shared();
        assert!(Arc::ptr_eq(&before, &after), "the roll must carry the SAME syncer, not respawn");
        drop(j);
        assert_eq!(
            before.finals.load(Ordering::Acquire),
            1,
            "one thread for the journal's life ⇒ exactly one shutdown sync"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

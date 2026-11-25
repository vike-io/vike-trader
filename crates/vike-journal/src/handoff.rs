//! **The WRITE thread: the one thread that hands the journal's frames to the kernel.**
//!
//! The asynchronous journal (decision 0279). The core thread frames every record into an
//! anonymous STAGE laid out like the segment (`writer.rs`'s `stage_frame`) and publishes how far it
//! got (`JournalSyncShared::appended`, one Release store per frame). This thread copies what was published
//! into the segment FILE with `pwrite`, in order, and publishes how far the file's page cache holds
//! it (`JournalSyncShared::written`). It is the only thread that writes a segment file's data pages, so the
//! core thread never takes a fault on a file page and never waits on a filesystem lock.
//!
//! # The order path
//!
//! A command whose effect LEAVES the process (submit, cancel, amend, confirm) is sent only after
//! [`WriteFence::wait`] returned: every record appended so far — the command's own write-ahead
//! record included, a lazy tail of execution reports excepted (below) — is in the page cache, so it survives a crash of this process (a power cut
//! loses what the page cache held, as before). The wait is a few microseconds because this thread
//! SPINS for [`SPIN_AFTER_WORK`] after each hand-off before it parks; a fence that finds it parked
//! wakes it (`Thread::unpark`, one futex wake) and spins on the watermark.
//!
//! A write thread whose hand-offs keep failing (a full or read-only disk) retries without end, so the wait
//! is BOUNDED: past [`FENCE_BOUND`] it answers `JournalCannotWrite`, and the order path refuses what can
//! open risk and lets a cancel or a covered exit leave unrecorded (decision 0279's addendum of 2026-11-23).
//!
//! # Everything else is asynchronous
//!
//! Fills, market data, status, snapshots: the core appends and goes on. A parked thread wakes on its
//! [`WRITE_TICK`], so a frame nobody waits for reaches the page cache within about a millisecond —
//! the bound on what a reader of the files (the execution stream, `read_since`, a restart after a crash)
//! sees later than before.
//!
//! # Lazy frames: the execution reports
//!
//! An execution report (`Journal::append_execution_report`) is the one LAZY frame: the core publishes its
//! end as `JournalSyncShared::staged`, not `appended`. This thread writes a lazy tail only once nothing eager
//! has come for [`SPIN_AFTER_WORK`] (or at its tick, or at the stop), and an eager frame appended after it
//! carries it in the same `pwrite`. So a fence never waits for a report that no other record follows, and the
//! report written right after an order left (its registration) rides the NEXT order's `pwrite` instead of
//! costing that order's fence a second one (measured on the submit hop: the fence's wait doubled). The file
//! still holds a prefix of the stage, in order, byte for byte; a report reaches the page cache within the same
//! millisecond bound, and a crash that cuts it off leaves it owed to the next boot like any cut-off report.
//!
//! # Positions
//!
//! Watermarks are packed `(segment ordinal, offset)` pairs ([`pos`]): the ordinal counts the segments
//! this process wrote (0 = the one `open` resumed or made), so a position stays monotonic across rolls.
//!
//! # The cadence `Snapshot` is ENCODED here (decision 0279, amended 2026-11-19)
//!
//! A live core's cadence `Snapshot` (`Journal::append_snap_deferred`) reaches this thread as a job in the
//! [`SnapshotSlot`]: its parts, owned. Once the frames published before it are in the file, this thread runs
//! it (`vike_execution::state_hash`, the msgpack, the frame check) and leaves the payload in the slot; the core
//! copies it into its stage at its next append, or at its idle point. Nothing else of the frame moves here: the
//! core is still the stage's only writer and this thread still writes the file in stage order.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant};

use memmap2::MmapRaw;

use crate::measure;
use crate::segment::{prep_path, reserve_blocks, segment_path};
use crate::sync::{JournalSyncShared, SyncMessage};
use crate::writer::JournalFileConfig;

/// Bits of a packed position that hold the offset: segments up to 1 TiB.
const OFF_BITS: u32 = 40;

/// The packed position of offset `off` in the segment of ordinal `ordinal`.
#[inline]
pub(crate) fn pos(ordinal: u64, off: usize) -> u64 {
    (ordinal << OFF_BITS) | off as u64
}

#[inline]
pub(crate) fn ord_of(p: u64) -> u64 {
    p >> OFF_BITS
}

#[inline]
pub(crate) fn off_of(p: u64) -> usize {
    (p & ((1u64 << OFF_BITS) - 1)) as usize
}

/// How long the write thread keeps spinning after its last hand-off before it parks. While it spins a
/// fence costs the copy alone; a parked thread costs the fence a wake-up first.
const SPIN_AFTER_WORK: Duration = Duration::from_micros(200);

/// At the stop, how long a failing hand-off is still retried with no progress before the write thread gives up
/// (`run_writer`): a disk that recovers inside it gets everything; one that does not cannot hold the stop forever.
const STOP_RETRY_FOR: Duration = Duration::from_secs(2);

/// How long a parked write thread sleeps before it looks again: the bound on how late an
/// asynchronous frame (one no fence waits for) reaches the page cache.
pub(crate) const WRITE_TICK: Duration = Duration::from_millis(1);

/// A write-thread step at least this long is slow ([`note_step`]): a quarter of the fence's [`FENCE_BOUND`], and about
/// 180 times the worst healthy hand-off decision 0279 measured (1.4 ms). On the CI box on 2026-11-24 a btrfs commit paused
/// every write on the filesystem for about 5 s, the fence gave up and nothing said where this thread had waited.
const SLOW_STEP: Duration = Duration::from_millis(250);

/// A run of slow steps ends once the write thread's steps have been fast this long: a disk that stays slow says two
/// lines, never one per step.
const SLOW_RUN_QUIET: Duration = Duration::from_secs(60);

/// An open run of slow steps on this write thread: when it began, its last slow step and when, how many, the slowest.
#[derive(Clone, Copy)]
struct SlowRun {
    began: Instant,
    last: (&'static str, Instant),
    count: u64,
    slowest: Duration,
}

thread_local! {
    /// The write thread's open run of slow steps (each journal's write thread has its own).
    static SLOW_RUN: std::cell::Cell<Option<SlowRun>> = const { std::cell::Cell::new(None) };
}

/// **Say where the write thread waited**, once per RUN of slow steps: one WARN when a step takes [`SLOW_STEP`] or
/// more (its name and how long), and one when the steps have been fast for [`SLOW_RUN_QUIET`] again (how long the run
/// lasted, how many slow steps, the slowest), so the next stall tells a filesystem that held a `pwrite` from an encode
/// that ran long, and a disk that stays slow costs two lines. A fast step outside a run is one compare and one
/// thread-local read; the clock is read only inside a run. Write thread only, never the core's.
#[inline]
fn note_step(step: &'static str, took: Duration) {
    note_step_at(step, took, Instant::now);
}

/// [`note_step`] on the clock `now`, read only when a step is slow or a run is open.
#[inline]
fn note_step_at(step: &'static str, took: Duration, now: impl FnOnce() -> Instant) {
    if took >= SLOW_STEP {
        slow_step(step, took, now());
    } else if SLOW_RUN.with(|run| run.get().is_some()) {
        fast_step(now());
    }
}

#[cold]
#[inline(never)]
fn slow_step(step: &'static str, took: Duration, now: Instant) {
    SLOW_RUN.with(|run| match run.get() {
        None => {
            tracing::warn!(
                step,
                took_ms = u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
                "journal write thread: one {step} took {took:?} (an order's fence gives up at {FENCE_BOUND:?}): \
                 the filesystem held it, or the disk is slow; said again once its steps are fast for \
                 {SLOW_RUN_QUIET:?}"
            );
            let began = now.checked_sub(took).unwrap_or(now);
            run.set(Some(SlowRun { began, last: (step, now), count: 1, slowest: took }));
        }
        Some(open) => run.set(Some(SlowRun {
            last: (step, now),
            count: open.count + 1,
            slowest: open.slowest.max(took),
            ..open
        })),
    });
}

#[cold]
#[inline(never)]
fn fast_step(now: Instant) {
    SLOW_RUN.with(|run| {
        let Some(open) = run.get() else { return };
        if now.saturating_duration_since(open.last.1) < SLOW_RUN_QUIET {
            return;
        }
        let (step, last) = open.last;
        tracing::warn!(
            slow_steps = open.count,
            "journal write thread: fast again: {} slow step(s) over {:?}, the slowest {:?}, the last a {step}",
            open.count,
            last.saturating_duration_since(open.began),
            open.slowest
        );
        run.set(None);
    });
}

/// What the core thread tells the write thread, in the order of the positions they concern.
pub(crate) enum ToWriter {
    /// The core's frames go on in a NEW segment (`ordinal`, file index `index`), whose header the core
    /// already put at the start of `stage`; the previous segment's frames end at `previous_end`.
    Roll { ordinal: u64, index: u64, stage: Arc<MmapRaw>, previous_end: usize },
    /// Raise the live segment's header to `version` — written AND synced — before any byte at or
    /// past `at` reaches the file (a power cut must not keep a frame whose header cannot read it).
    Stamp { at: u64, version: u32 },
}

/// The segment the write thread is filling.
pub(crate) struct Segment {
    pub(crate) ordinal: u64,
    /// The segment's file index (`journal-{idx}.vjl`), which names its durable copy too.
    pub(crate) index: u64,
    pub(crate) file: File,
    pub(crate) stage: Arc<MmapRaw>,
    /// How far the file holds the stage's bytes.
    pub(crate) off: usize,
}

/// The wait an order runs before it leaves the process: see the module doc's "The order path".
#[derive(Clone)]
pub struct WriteFence {
    shared: Arc<JournalSyncShared>,
    writer: Thread,
}

impl WriteFence {
    /// Return once every frame the core appended so far (a lazy tail of execution reports excepted: the module
    /// doc) is in the file's page cache, and every
    /// header stamp it sent so far is written and synced. Called on the core thread only, the one that
    /// appends and stamps, so "so far" is exact.
    ///
    /// `Err` once `FENCE_BOUND` (1 s) has passed with the write thread behind (a stalled filesystem, or a full,
    /// read-only or failing disk: it retries without end, `run_writer`), and AT ONCE while it has written nothing since the last wait that
    /// ran out, so a sweep of cancels does not pay the bound per cancel. What the order path does then is
    /// decision 0279's addendum of 2026-11-23. The bound is read only once the write thread is behind.
    pub fn wait(&self) -> Result<(), vike_execution::JournalCannotWrite> {
        let sh = &*self.shared;
        let (target, stamps) =
            (sh.appended.load(Ordering::Relaxed), sh.stamps_sent.load(Ordering::Relaxed));
        let done = || {
            sh.written.load(Ordering::Acquire) >= target
                && sh.stamps_done.load(Ordering::Acquire) >= stamps
        };
        if done() {
            return Ok(());
        }
        if sh.fence_stall.0.load(Ordering::Relaxed) == sh.written.load(Ordering::Acquire) {
            return Err(vike_execution::JournalCannotWrite);
        }
        let started = Instant::now();
        self.writer.unpark();
        let mut spins: u32 = 0;
        while !done() {
            if self.shared.writer_gone.load(Ordering::Acquire) {
                // Reached only if the write thread ended without draining (it is panic-free by
                // construction and drains before it exits). Say it; there is no one left to wait for.
                tracing::error!(
                    "journal write thread is gone: an order leaves before its record reached the file"
                );
                break;
            }
            std::hint::spin_loop();
            spins = spins.wrapping_add(1);
            // A write thread that shares this CPU must get it.
            if spins.is_multiple_of(4096) {
                std::thread::yield_now();
                if started.elapsed() >= FENCE_BOUND {
                    return Err(self.gave_up(started));
                }
            }
        }
        measure::note_fence_wait(started.elapsed());
        Ok(())
    }

    /// A wait that ran out [`FENCE_BOUND`]: remember where the write thread stood, so the next wait answers at once
    /// until it moves, and say it. Cold: a journal outage.
    #[cold]
    #[inline(never)]
    fn gave_up(&self, started: Instant) -> vike_execution::JournalCannotWrite {
        let written = self.shared.written.load(Ordering::Acquire);
        self.shared.fence_stall.0.store(written, Ordering::Relaxed);
        measure::note_fence_wait(started.elapsed());
        tracing::error!(
            written_segment_ordinal = ord_of(written),
            written_offset = off_of(written),
            "journal write fence: the write thread wrote nothing for {FENCE_BOUND:?}; the order effect waiting \
             on it is not recorded, and every wait answers at once until the write thread moves again"
        );
        vike_execution::JournalCannotWrite
    }
}

/// How long an order's [`WriteFence::wait`] waits for a write thread that is behind before it answers that the journal
/// cannot write. Decision 0279 measured the fence on the latency box's btrfs at a p999 of 38-53 µs and a max of 0.27-1.4 ms, and
/// the worst stall of the design before it at 28.5 ms: a second is about 700 times the worst healthy wait and 35 times
/// that stall, so it never fires on a slow disk that works, and the core holds still for one second, once, on one
/// that does not. Below [`STOP_RETRY_FOR`], the stop's own patience with the same failure.
pub(crate) const FENCE_BOUND: Duration = Duration::from_secs(1);

/// Where the write thread stood when a [`WriteFence::wait`] last ran out [`FENCE_BOUND`]; `u64::MAX` while none did.
#[derive(Debug)]
pub(crate) struct FenceStall(AtomicU64);

impl Default for FenceStall {
    fn default() -> Self {
        Self(AtomicU64::new(u64::MAX))
    }
}

/// What encodes one cadence `Snapshot`: its parts, owned, and the encode (the hash, the msgpack), returning the
/// payload. Built on the core (`Journal::append_snap_deferred`), run where the [`SnapshotSlot`] says.
pub(crate) type SnapshotJob = Box<dyn FnOnce() -> Vec<u8> + Send>;

/// No job.
const SLOT_EMPTY: u8 = 0;
/// A job is waiting for the write thread.
const SLOT_QUEUED: u8 = 1;
/// The write thread is running it.
const SLOT_TAKEN: u8 = 2;
/// Its payload and check are in [`SnapshotSlot::done`].
const SLOT_DONE: u8 = 3;

/// **The one cadence `Snapshot` in flight between the core and the write thread** (this module's doc). ONE at a
/// time: the core settles a pending one before it hands another over. Whoever moves `state` from QUEUED takes the
/// job: the write thread (QUEUED -> TAKEN, then DONE with the payload), or the core settling it before the write
/// thread got to it (QUEUED -> EMPTY, and the core encodes it itself, what every `Snapshot` cost before). The two
/// mutexes are taken only by the side that won that move, so neither ever waits on the other's lock; the core waits
/// only for a job the write thread is RUNNING, which is CPU and no I/O.
#[derive(Default)]
pub(crate) struct SnapshotSlot {
    state: AtomicU8,
    job: Mutex<Option<SnapshotJob>>,
    /// The encoded payload and its frame check.
    done: Mutex<Option<(u32, Vec<u8>)>>,
}

impl std::fmt::Debug for SnapshotSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotSlot").field("state", &self.state.load(Ordering::Relaxed)).finish()
    }
}

impl SnapshotSlot {
    /// The core: put `job` in the (empty) slot.
    pub(crate) fn hand(&self, job: SnapshotJob) {
        *self.job.lock().unwrap_or_else(PoisonError::into_inner) = Some(job);
        self.state.store(SLOT_QUEUED, Ordering::Release);
    }

    /// The write thread: run a queued job, if there is one; `true` when it did.
    fn encode_queued(&self) -> bool {
        // A plain load first: the spinning write thread asks this every pass, and a failed swap would take the line.
        if self.state.load(Ordering::Relaxed) != SLOT_QUEUED
            || self
                .state
                .compare_exchange(SLOT_QUEUED, SLOT_TAKEN, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
        {
            return false;
        }
        let job = self.job.lock().unwrap_or_else(PoisonError::into_inner).take();
        let started = Instant::now();
        // An encode that panics leaves `None`: the core's settle fails then, on the core thread, where an
        // unencodable `Snapshot` always failed (`.expect("journal snap")`). This thread lives on.
        let done = job
            .and_then(|job| std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).ok())
            .map(|payload| (crate::frame::fnv1a32(&payload), payload));
        let took = started.elapsed();
        measure::note_snapshot_encode(took);
        note_step("snapshot encode", took);
        *self.done.lock().unwrap_or_else(PoisonError::into_inner) = done;
        self.state.store(SLOT_DONE, Ordering::Release);
        true
    }

    /// The core: whether the write thread has encoded the job it handed over (a settle now only copies it).
    pub(crate) fn is_done(&self) -> bool {
        self.state.load(Ordering::Acquire) == SLOT_DONE
    }

    /// The core: whether the write thread is running the job it handed over (CPU, it ends).
    pub(crate) fn is_taken(&self) -> bool {
        self.state.load(Ordering::Acquire) == SLOT_TAKEN
    }

    /// The core: the payload and check of the job it handed over: encoded here when the write thread has not
    /// started it, waited for while the write thread runs it. `None` when the write thread's encode panicked.
    pub(crate) fn settle(&self) -> Option<(u32, Vec<u8>)> {
        let started = Instant::now();
        let (done, waited) = if self
            .state
            .compare_exchange(SLOT_QUEUED, SLOT_EMPTY, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            let job = self.job.lock().unwrap_or_else(PoisonError::into_inner).take();
            let payload = job
                .and_then(|job| std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).ok());
            (payload.map(|payload| (crate::frame::fnv1a32(&payload), payload)), false)
        } else {
            let mut spins: u32 = 0;
            let mut waited = false;
            while self.state.load(Ordering::Acquire) != SLOT_DONE {
                waited = true;
                std::hint::spin_loop();
                spins = spins.wrapping_add(1);
                // A write thread that shares this CPU must get it.
                if spins.is_multiple_of(4096) {
                    std::thread::yield_now();
                }
            }
            let done = self.done.lock().unwrap_or_else(PoisonError::into_inner).take();
            self.state.store(SLOT_EMPTY, Ordering::Release);
            (done, waited)
        };
        measure::note_snapshot_settle(started.elapsed(), waited);
        done
    }
}

/// The core's handle on the write thread.
pub(crate) struct WriteThread {
    tx: Sender<ToWriter>,
    shared: Arc<JournalSyncShared>,
    thread: Thread,
    handle: Option<JoinHandle<()>>,
}

impl WriteThread {
    pub(crate) fn spawn(
        seg: Segment,
        shared: Arc<JournalSyncShared>,
        to_syncer: Sender<SyncMessage>,
        dir: PathBuf,
        config: JournalFileConfig,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let sh = Arc::clone(&shared);
        let handle = std::thread::Builder::new().name("vjl-write".into()).spawn(move || {
            run_writer(rx, &sh, seg, &to_syncer, &dir, &config);
            sh.writer_gone.store(true, Ordering::Release);
        })?;
        let thread = handle.thread().clone();
        Ok(Self { tx, shared, thread, handle: Some(handle) })
    }

    /// Hand a roll or a stamp over. Never blocks (the channel is unbounded).
    pub(crate) fn send(&self, m: ToWriter) {
        if self.tx.send(m).is_err() {
            tracing::error!(
                "journal write thread is gone: a roll or a header stamp was not handed over"
            );
        }
    }

    pub(crate) fn fence(&self) -> WriteFence {
        WriteFence { shared: Arc::clone(&self.shared), writer: self.thread.clone() }
    }

    /// Wake a parked write thread (a `Snapshot` was handed over): one futex wake when it is parked, none when
    /// it spins.
    pub(crate) fn wake(&self) {
        self.thread.unpark();
    }

    /// Drain everything appended, then stop. Waits for the thread, which gives up on a hand-off still failing
    /// [`STOP_RETRY_FOR`] after its last progress (`run_writer`): a broken disk cannot hold the stop forever.
    pub(crate) fn stop_and_join(mut self) {
        self.shared.stop_writer.store(true, Ordering::Release);
        self.thread.unpark();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// The thread body. Panic-free by construction: every fallible call is matched and logged, and a
/// failed hand-off is retried (the watermark does not move, so an order's fence waits, for at most
/// [`FENCE_BOUND`]); at the stop only for [`STOP_RETRY_FOR`] without progress (`Failing`).
fn run_writer(
    rx: Receiver<ToWriter>,
    sh: &JournalSyncShared,
    mut current: Segment,
    to_syncer: &Sender<SyncMessage>,
    dir: &std::path::Path,
    config: &JournalFileConfig,
) {
    let mut pending: VecDeque<ToWriter> = VecDeque::new();
    let mut last_work = Instant::now();
    let mut failing = Failing::default();
    let gave_up = loop {
        // `stop` BEFORE `appended`: a stop seen here covers every frame appended before it.
        let stop = sh.stop_writer.load(Ordering::Acquire);
        let appended = sh.appended.load(Ordering::Acquire);
        let staged = sh.staged.load(Ordering::Acquire);
        // A roll or stamp the core sent before it published `appended` (or `staged`) past it is visible now.
        while let Ok(m) = rx.try_recv() {
            pending.push_back(m);
        }
        match advance(&mut current, &mut pending, appended, sh, to_syncer, dir, config) {
            Ok(true) => {
                last_work = Instant::now();
                failing.progressed(dir);
                continue;
            }
            Ok(false) => {}
            Err(e) => match failing.failed(e, stop, dir) {
                None => continue,
                Some(e) => break Some(e),
            },
        }
        // A cadence `Snapshot` the core handed over, once the frames published before it are in the file (an
        // order's fence waits for those, never for this): encoded here, framed by the core.
        if sh.snapshot.encode_queued() {
            last_work = Instant::now();
            continue;
        }
        // The lazy tail, once nothing eager has come for the spin window (or at the stop): written without
        // restarting that window, so the thread parks after it.
        if staged > appended && (stop || last_work.elapsed() >= SPIN_AFTER_WORK) {
            match advance(&mut current, &mut pending, staged, sh, to_syncer, dir, config) {
                Ok(true) => {
                    failing.progressed(dir);
                    continue;
                }
                Ok(false) => {}
                Err(e) => match failing.failed(e, stop, dir) {
                    None => continue,
                    Some(e) => break Some(e),
                },
            }
        }
        if stop {
            break None;
        }
        if last_work.elapsed() < SPIN_AFTER_WORK {
            std::hint::spin_loop();
        } else {
            std::thread::park_timeout(WRITE_TICK);
        }
    };
    match gave_up {
        // Everything appended is in the working file: the sync thread syncs it whole and drops its copy.
        None => {
            let _ = to_syncer.send(SyncMessage::Final { work: current.file, index: current.index });
        }
        // No `Final`: it would delete a durable copy holding frames the working file lacks. The copies stay for
        // the next boot's repair (`crate::durable`), every one brought up to its segment's end first (`GaveUp`); a
        // segment whole in its working file (the roll after it failed) is sealed like any left segment.
        Some(e) => {
            let end = sh.staged_end();
            tracing::error!(
                error = %e,
                dir = %dir.display(),
                failures = failing.count,
                written_segment = current.index,
                written_offset = current.off,
                end_segment = current.index + (ord_of(end) - current.ordinal),
                end_offset = off_of(end),
                "journal stop: the write thread gave up after {STOP_RETRY_FOR:?} of failed hand-offs; the frames \
                 from the written position to the end are not in the working segment files (their durable copies \
                 are kept, brought up to their ends, for the next boot's repair)"
            );
            if matches!(pending.front(), Some(ToWriter::Roll { previous_end, .. }) if current.off >= *previous_end)
            {
                let _ =
                    to_syncer.send(SyncMessage::Seal { work: current.file, index: current.index });
            }
            let _ = to_syncer.send(SyncMessage::GaveUp);
        }
    }
}

/// The longest the write thread stays silent while its hand-offs keep failing (it retries every [`WRITE_TICK`]): a
/// broken or full disk must not flood a log that likely lives on it.
const FAILING_SAID_EVERY: Duration = Duration::from_secs(10);

/// A run of failed hand-offs: said in full at its first failure, summed up at most every [`FAILING_SAID_EVERY`], its
/// end said once; and at the stop, the deadline after which the write thread gives up.
#[derive(Default)]
struct Failing {
    /// Failures in this run (0: the hand-offs work).
    count: u64,
    /// When the run began, and when a line last said it.
    began: Option<Instant>,
    said: Option<Instant>,
    give_up_at: Option<Instant>,
}

impl Failing {
    /// A failed hand-off: retried after a tick, but at the stop only until [`STOP_RETRY_FOR`] has passed since the
    /// last progress. `Some` hands the error back: give up.
    fn failed(&mut self, e: io::Error, stop: bool, dir: &std::path::Path) -> Option<io::Error> {
        let now = Instant::now();
        if stop && now >= *self.give_up_at.get_or_insert(now + STOP_RETRY_FOR) {
            return Some(e);
        }
        self.count += 1;
        let began = *self.began.get_or_insert(now);
        let first = self.said.is_none();
        if first || self.said.is_some_and(|said| now - said >= FAILING_SAID_EVERY) {
            self.said = Some(now);
            if first {
                tracing::warn!(error = %e, dir = %dir.display(),
                    "journal hand-off to the kernel failed; retrying every {WRITE_TICK:?}, said again at most \
                     every {FAILING_SAID_EVERY:?}");
            } else {
                // ERROR: a run this long is past every order fence's bound (`FENCE_BOUND`), so orders are refused.
                tracing::error!(last_error = %e, dir = %dir.display(), failures = self.count,
                    failing_for = ?(now - began), "journal hand-offs to the kernel still failing; retrying");
            }
        }
        std::thread::sleep(WRITE_TICK);
        None
    }

    /// A hand-off moved something: a run of failures ends, said once.
    #[inline]
    fn progressed(&mut self, dir: &std::path::Path) {
        self.give_up_at = None;
        if self.count > 0 {
            self.recovered(dir);
        }
    }

    #[cold]
    #[inline(never)]
    fn recovered(&mut self, dir: &std::path::Path) {
        let failing_for = self.began.map(|t| t.elapsed()).unwrap_or_default();
        tracing::warn!(dir = %dir.display(), failures = self.count, failing_for = ?failing_for,
            "journal hand-offs to the kernel recovered");
        *self = Self::default();
    }
}

/// Move the file up to `appended`, applying rolls and stamps where they fall. `Ok(true)` when
/// anything moved.
fn advance(
    current: &mut Segment,
    pending: &mut VecDeque<ToWriter>,
    appended: u64,
    sh: &JournalSyncShared,
    to_syncer: &Sender<SyncMessage>,
    dir: &std::path::Path,
    config: &JournalFileConfig,
) -> io::Result<bool> {
    let mut moved = false;
    loop {
        match pending.front() {
            Some(ToWriter::Stamp { at, version }) if pos(current.ordinal, current.off) >= *at => {
                // Into the working file's page cache only: the sync thread writes the stamp into
                // the durable copy and syncs it before any frame past `at` (no sync here, ever).
                let started = Instant::now();
                write_at(&current.file, &version.to_le_bytes(), 4)?;
                let took = started.elapsed();
                measure::note_write(took);
                note_step("header stamp write", took);
                pending.pop_front();
                sh.stamps_done.fetch_add(1, Ordering::Release);
                moved = true;
                continue;
            }
            Some(ToWriter::Roll { previous_end, .. }) if current.off >= *previous_end => {
                let Some(ToWriter::Roll { ordinal, index, stage, .. }) = pending.pop_front() else {
                    unreachable!("the front was a roll");
                };
                let started = Instant::now();
                let file = match open_rolled(dir, index, config, stage.len() as u64) {
                    Ok(f) => {
                        note_step("segment roll", started.elapsed());
                        f
                    }
                    Err(e) => {
                        pending.push_front(ToWriter::Roll {
                            ordinal,
                            index,
                            stage,
                            previous_end: current.off,
                        });
                        return Err(e);
                    }
                };
                let old =
                    std::mem::replace(current, Segment { ordinal, index, file, stage, off: 0 });
                // The old segment is whole in its working file: the sync thread seals it (the full
                // sync) and only then drops its durable copy. The new segment's copy is the sync
                // thread's already: the core told it at the roll.
                let _ = to_syncer.send(SyncMessage::Seal { work: old.file, index: old.index });
                sh.written.store(pos(current.ordinal, 0), Ordering::Release);
                moved = true;
                continue;
            }
            _ => {}
        }
        // The end of the span this segment can take now: the published end in this segment, or, when
        // the core is already in a later one, where this one ends (the next roll's `previous_end`).
        let mut limit = if ord_of(appended) == current.ordinal {
            off_of(appended)
        } else if ord_of(appended) > current.ordinal {
            pending
                .iter()
                .find_map(|m| match m {
                    ToWriter::Roll { previous_end, .. } => Some(*previous_end),
                    ToWriter::Stamp { .. } => None,
                })
                .unwrap_or(current.off)
        } else {
            current.off
        };
        if let Some(ToWriter::Stamp { at, .. }) = pending.front()
            && ord_of(*at) == current.ordinal
        {
            limit = limit.min(off_of(*at));
        }
        if limit <= current.off {
            return Ok(moved);
        }
        let started = Instant::now();
        copy_span(&current.stage, &current.file, current.off, limit)?;
        let took = started.elapsed();
        measure::note_write(took);
        note_step("write", took);
        current.off = limit;
        sh.written.store(pos(current.ordinal, current.off), Ordering::Release);
        moved = true;
    }
}

/// The file of rolled segment `index`: the one `prepare_next` built (sized and reserved) when it is
/// there, else made here (on this thread, never the core's), and at least `stage_len` long before any
/// byte is written: a segment the core sized up for one frame larger than `segment_bytes`
/// (`crates/vike-journal/src/writer.rs`'s `roll`) is sized up here too, so a reader never meets a frame
/// running past its file (`crate::frame::is_pending_tail`).
fn open_rolled(
    dir: &std::path::Path,
    index: u64,
    config: &JournalFileConfig,
    stage_len: u64,
) -> io::Result<File> {
    let (prep, live) = (prep_path(dir, index), segment_path(dir, index));
    if std::fs::metadata(&prep).is_ok_and(|m| m.len() >= config.segment_bytes) {
        let _ = std::fs::rename(&prep, &live);
    }
    let file =
        OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&live)?;
    let len = config.segment_bytes.max(stage_len);
    if file.metadata()?.len() < len {
        file.set_len(len)?;
        reserve_blocks(&file, len);
    }
    Ok(file)
}

#[cfg(unix)]
pub(crate) fn write_at(file: &File, buffer: &[u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buffer, off)
}

#[cfg(not(unix))]
pub(crate) fn write_at(file: &File, mut buffer: &[u8], mut off: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        let n = std::os::windows::fs::FileExt::seek_write(file, buffer, off)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "journal hand-off wrote nothing"));
        }
        buffer = &buffer[n..];
        off += n as u64;
    }
    Ok(())
}

/// Copy the stage's `[from, to)` (whole frames the core published) into `file` at the same offsets,
/// in ONE `pwrite` (the owner, 2026-11-12: the order path waits for this call, and on btrfs each
/// buffered write costs several µs).
///
/// ⚠ So a reader racing this call CAN see a frame's length before its payload and check are all
/// there. Every reader already stops before a frame that fails its check, never accepts it and
/// never errors (`crate::frame::walk_frames`); at the end of the LAST segment that frame is "not yet
/// written", and the readers stay silent about it (`crate::frame::is_pending_tail`).
/// `durable_tests.rs`'s `a_reader_racing_the_write_thread_never_reads_a_partial_frame` holds it.
#[expect(unsafe_code)]
pub(crate) fn copy_span(stage: &MmapRaw, file: &File, from: usize, to: usize) -> io::Result<()> {
    let to = to.min(stage.len());
    if to <= from {
        return Ok(());
    }
    // SAFETY: `[from, to)` lies inside the stage (clamped above) and below a position the core
    // published with a Release store (read behind an Acquire load) AFTER writing those bytes, and the
    // core never writes a byte below a published position again: no write races this read.
    let span = unsafe { std::slice::from_raw_parts(stage.as_ptr().add(from), to - from) };
    write_at(file, span, from as u64)
}

#[path = "handoff_tests.rs"]
#[cfg(test)]
mod handoff_tests;

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
//! record included — is in the page cache, so it survives a crash of this process (a power cut
//! loses what the page cache held, as before). The wait is a few microseconds because this thread
//! SPINS for [`SPIN_AFTER_WORK`] after each hand-off before it parks; a fence that finds it parked
//! wakes it (`Thread::unpark`, one futex wake) and spins on the watermark.
//!
//! # Everything else is asynchronous
//!
//! Fills, market data, status, snapshots: the core appends and goes on. A parked thread wakes on its
//! [`WRITE_TICK`], so a frame nobody waits for reaches the page cache within about a millisecond —
//! the bound on what a reader of the files (the materializer, `read_since`, a restart after a crash)
//! sees later than before.
//!
//! # Positions
//!
//! Watermarks are packed `(segment ordinal, offset)` pairs ([`pos`]): the ordinal counts the segments
//! this process wrote (0 = the one `open` resumed or made), so a position stays monotonic across rolls.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
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

/// How long a parked write thread sleeps before it looks again: the bound on how late an
/// asynchronous frame (one no fence waits for) reaches the page cache.
pub(crate) const WRITE_TICK: Duration = Duration::from_millis(1);

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
    /// Return once every frame the core appended so far is in the file's page cache, and every
    /// header stamp it sent so far is written and synced. Called on the core thread only, the one that
    /// appends and stamps, so "so far" is exact.
    pub fn wait(&self) {
        let sh = &*self.shared;
        let (target, stamps) =
            (sh.appended.load(Ordering::Relaxed), sh.stamps_sent.load(Ordering::Relaxed));
        let done = || {
            sh.written.load(Ordering::Acquire) >= target
                && sh.stamps_done.load(Ordering::Acquire) >= stamps
        };
        if done() {
            return;
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
            }
        }
        measure::note_fence_wait(started.elapsed());
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

    /// Drain everything appended, then stop. Waits for the thread.
    pub(crate) fn stop_and_join(mut self) {
        self.shared.stop_writer.store(true, Ordering::Release);
        self.thread.unpark();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// The thread body. Panic-free by construction: every fallible call is matched and logged, and a
/// failed hand-off is retried (the watermark does not move, so an order waits rather than leave
/// without its record).
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
    loop {
        // `stop` BEFORE `appended`: a stop seen here covers every frame appended before it.
        let stop = sh.stop_writer.load(Ordering::Acquire);
        let appended = sh.appended.load(Ordering::Acquire);
        // A roll or stamp the core sent before it published `appended` past it is visible now.
        while let Ok(m) = rx.try_recv() {
            pending.push_back(m);
        }
        match advance(&mut current, &mut pending, appended, sh, to_syncer, dir, config) {
            Ok(true) => {
                last_work = Instant::now();
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, "journal hand-off to the kernel failed; retrying");
                std::thread::sleep(WRITE_TICK);
                continue;
            }
        }
        if stop {
            break;
        }
        if last_work.elapsed() < SPIN_AFTER_WORK {
            std::hint::spin_loop();
        } else {
            std::thread::park_timeout(WRITE_TICK);
        }
    }
    // Everything appended is in the working file: the sync thread syncs it whole and drops its copy.
    let _ = to_syncer.send(SyncMessage::Final { work: current.file, index: current.index });
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
                measure::note_write(started.elapsed());
                pending.pop_front();
                sh.stamps_done.fetch_add(1, Ordering::Release);
                moved = true;
                continue;
            }
            Some(ToWriter::Roll { previous_end, .. }) if current.off >= *previous_end => {
                let Some(ToWriter::Roll { ordinal, index, stage, .. }) = pending.pop_front() else {
                    unreachable!("the front was a roll");
                };
                let file = match open_rolled(dir, index, config) {
                    Ok(f) => f,
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
        measure::note_write(started.elapsed());
        current.off = limit;
        sh.written.store(pos(current.ordinal, current.off), Ordering::Release);
        moved = true;
    }
}

/// The file of rolled segment `index`: the one `prepare_next` built (sized and reserved) when it is
/// there, else made here (on this thread, never the core's).
fn open_rolled(dir: &std::path::Path, index: u64, config: &JournalFileConfig) -> io::Result<File> {
    let (prep, live) = (prep_path(dir, index), segment_path(dir, index));
    if std::fs::metadata(&prep).is_ok_and(|m| m.len() >= config.segment_bytes) {
        let _ = std::fs::rename(&prep, &live);
    }
    let file =
        OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&live)?;
    if file.metadata()?.len() < config.segment_bytes {
        file.set_len(config.segment_bytes)?;
        reserve_blocks(&file, config.segment_bytes);
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

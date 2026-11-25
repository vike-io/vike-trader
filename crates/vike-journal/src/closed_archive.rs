//! **The CLOSED-TRADE ARCHIVE**: the append-only file beside the journal's segments that keeps every closed trade a
//! live mount booked (decision 0271 item 8, `vike_core`'s `runtime/closed_trades.rs`), so a `Snapshot` carries only
//! "archived through #N", the few rows not yet archived and the open lots, and its size does not grow with the
//! trades a mount closed.
//!
//! # The file
//!
//! `<journal dir>/`[`ARCHIVE_FILE`], never a second SQLite file: the journal's own frame,
//! `[len: u32 LE][check: u32 FNV-1a32(payload)][payload: serde_json(TradeClosedArchiveEntry)]`, one per closed trade, each
//! carrying its mount and its NUMBER (its index in that mount's whole list). No segment scanner names it
//! (`journal-{idx}.vjl` is a segment), so the prune never deletes it, and a journal moved aside takes it along.
//!
//! # Written once, off the fold
//!
//! The core only ENQUEUES a trade ([`TradeClosedArchive::enqueue`], an unbounded channel send that never blocks); the
//! thread `th-closed-archive` encodes it, appends a batch with one write, forces it to disk (`sync_data`) and only
//! then publishes how far it got ([`TradeClosedArchive::durable`]). So "archived" means ON DISK: a `Snapshot` counts a row as
//! archived only once its write was synced, and a row the thread had not synced when the process died is still in
//! that `Snapshot`, or in the journal's tail as the fill it came from. A write that fails is said once (an ERROR) and the
//! thread publishes nothing more, so the rows stay in every `Snapshot` until the core restarts.
//!
//! # Read once, at a restart
//!
//! [`read_closed_archive`] walks the whole file. A torn frame (a crash mid-append: a length past the end, a payload
//! that fails its check or does not parse) is DROPPED, and the walk resynchronizes a byte at a time to the next frame
//! that passes its check, so the frames a later process appended after the torn bytes are read. A DUPLICATE (one
//! mount's number written twice: a restart writes again the rows its `Snapshot` and tail rebuilt) is dropped too, by
//! number: the LAST copy stands, the one the newest process wrote. Cold: once per boot, O(file).

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use serde::{Deserialize, Serialize};

use crate::frame::fnv1a32;
use vike_model::TradeClosedRow;

/// The archive's file name in the journal's directory.
pub const ARCHIVE_FILE: &str = "closed-trades.vja";

/// One frame of the archive: mount `mount_id`'s closed trade number `n` (its index in the mount's whole list).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeClosedArchiveEntry {
    pub mount_id: String,
    pub n: u64,
    pub row: TradeClosedRow,
}

/// **The archive's writer**, held by the core: [`Self::enqueue`] on the fold, the thread does the rest. Closing
/// ([`Self::close`]) or dropping it lets the thread write what is queued and joins it.
pub struct TradeClosedArchive {
    tx: Option<mpsc::Sender<TradeClosedArchiveEntry>>,
    /// The last sequence number handed out (`1` is the first trade enqueued).
    sent: u64,
    durable: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for TradeClosedArchive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TradeClosedArchive")
            .field("sent", &self.sent)
            .field("durable", &self.durable())
            .finish()
    }
}

impl TradeClosedArchive {
    /// Open (or create) `dir`'s archive for appending and start its thread. Session start, never the fold.
    pub fn open(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new().create(true).append(true).open(dir.join(ARCHIVE_FILE))?;
        let (tx, rx) = mpsc::channel();
        let durable = Arc::new(AtomicU64::new(0));
        let published = Arc::clone(&durable);
        let path = dir.join(ARCHIVE_FILE);
        let thread = std::thread::Builder::new()
            .name("th-closed-archive".into())
            .spawn(move || run_writer(file, &rx, &published, &path))?;
        Ok(TradeClosedArchive { tx: Some(tx), sent: 0, durable, thread: Some(thread) })
    }

    /// Queue mount `mount_id`'s closed trade number `n` for the archive, and return its sequence number (the
    /// trade is on disk once [`Self::durable`] reaches it). Never blocks and touches no disk: the fold's one cost
    /// is the send.
    pub fn enqueue(&mut self, mount_id: &str, n: u64, row: TradeClosedRow) -> u64 {
        self.sent += 1;
        if let Some(tx) = &self.tx {
            // A send fails only once the thread is gone (it panicked): nothing then reaches `durable`, so every
            // later row stays in the `Snapshot`.
            let _ = tx.send(TradeClosedArchiveEntry { mount_id: mount_id.to_string(), n, row });
        }
        self.sent
    }

    /// The highest sequence number whose trade is on disk (synced): every trade enqueued at or before it is.
    pub fn durable(&self) -> u64 {
        self.durable.load(Ordering::Acquire)
    }

    /// **Let the thread write everything queued, and join it**: [`Self::durable`] is then FINAL (every trade, unless a
    /// write failed), and a later [`Self::enqueue`] reaches no disk. No time bound: a clean stop joined this thread
    /// anyway, and a bound only let the exit `Snapshot` race the thread's last `sync_data` (a loaded disk lost it).
    /// Cold: a clean stop's exit `Snapshot`, and the drop.
    pub fn close(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for TradeClosedArchive {
    fn drop(&mut self) {
        self.close();
    }
}

/// The thread: each batch the channel holds, encoded, appended with one write, synced, then published.
fn run_writer(
    mut file: File,
    rx: &mpsc::Receiver<TradeClosedArchiveEntry>,
    durable: &AtomicU64,
    path: &Path,
) {
    let mut written: u64 = 0;
    let mut buffer: Vec<u8> = Vec::new();
    while let Ok(first) = rx.recv() {
        buffer.clear();
        let mut batch = 0u64;
        for t in std::iter::once(first).chain(rx.try_iter()) {
            push_frame(&mut buffer, &t);
            batch += 1;
        }
        if let Err(e) = file.write_all(&buffer).and_then(|()| file.sync_data()) {
            tracing::error!(
                file = %path.display(),
                error = %e,
                "closed-trade archive: a write failed; the rows stay in every Snapshot until the core restarts"
            );
            // Publish nothing more, ever: a later success would publish numbers past the lost batch, and a `Snapshot`
            // would then count rows the file does not hold. The rest is drained so the core's sends cost nothing.
            while rx.recv().is_ok() {}
            return;
        }
        written += batch;
        durable.store(written, Ordering::Release);
    }
}

/// Append `t`'s frame to `buffer`.
fn push_frame(buffer: &mut Vec<u8>, t: &TradeClosedArchiveEntry) {
    let payload = serde_json::to_vec(t).expect("an archived trade serializes");
    let len = u32::try_from(payload.len()).expect("a closed row is far below 4 GiB");
    buffer.extend_from_slice(&len.to_le_bytes());
    buffer.extend_from_slice(&fnv1a32(&payload).to_le_bytes());
    buffer.extend_from_slice(&payload);
}

/// What [`read_closed_archive`] found: each mount's trades by number, and what it dropped.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TradeClosedArchiveRead {
    pub trades: HashMap<String, BTreeMap<u64, TradeClosedRow>>,
    /// Stretches of torn bytes skipped (a crash mid-append leaves one).
    pub torn: usize,
    /// Copies of a mount's number dropped for a later one.
    pub duplicates: usize,
}

/// **Read `dir`'s archive once** (the module doc's "Read once"): every frame that passes its check and parses, the
/// last copy of each mount's number standing. A directory with no archive reads empty; one that cannot be read is
/// an error.
pub fn read_closed_archive(dir: &Path) -> io::Result<TradeClosedArchiveRead> {
    let bytes = match std::fs::read(dir.join(ARCHIVE_FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(TradeClosedArchiveRead::default());
        }
        Err(e) => return Err(e),
    };
    let mut out = TradeClosedArchiveRead::default();
    let (mut at, mut in_torn) = (0usize, false);
    while at + 8 <= bytes.len() {
        match frame_at(&bytes, at) {
            Some((t, next)) => {
                let by_n = out.trades.entry(t.mount_id).or_default();
                if by_n.insert(t.n, t.row).is_some() {
                    out.duplicates += 1;
                }
                (at, in_torn) = (next, false);
            }
            None => {
                out.torn += usize::from(!in_torn);
                (at, in_torn) = (at + 1, true);
            }
        }
    }
    out.torn += usize::from(!in_torn && at < bytes.len());
    Ok(out)
}

/// The frame at `at` and the offset after it, when one starts there whole and passes its check.
fn frame_at(bytes: &[u8], at: usize) -> Option<(TradeClosedArchiveEntry, usize)> {
    let len = u32::from_le_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
    let end = at.checked_add(8)?.checked_add(len)?;
    if len == 0 || end > bytes.len() {
        return None;
    }
    let check = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?);
    let payload = &bytes[at + 8..end];
    if fnv1a32(payload) != check {
        return None;
    }
    serde_json::from_slice(payload).ok().map(|t| (t, end))
}

#[path = "closed_archive_tests.rs"]
#[cfg(test)]
mod closed_archive_tests;

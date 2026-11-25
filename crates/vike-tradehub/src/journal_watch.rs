//! **The journal's size and growth watch** (journal retention, point 3; the owner's yes of 2026-11-24). The summary
//! thread asks it on every tick ([`JournalWatch::tick`]); at most once a [`MEASURE_EVERY_MS`] it sums the bytes of every
//! file under the journal directory (`config.journal_dir`) but its `archive/`, counts its segments, keeps the totals of the last
//! [`GROWTH_WINDOW_MS`], and says ONCE when the journal goes over `config.journal_size_alert_mib` or
//! `config.journal_growth_alert_mib_per_hour`: one WARN line here, and the [`JournalReading`] it returns, which the
//! alert mount tells as one `journal_growth` alert (`crate::alerts::AlertMount::on_journal_growth`). Back under both,
//! it re-arms (an INFO line). Why: a the CI box strategy flooded the journal at ~0.4 GB an hour (832k denied submits) and
//! nothing said so for 29 hours, until a deploy's boot probe hung on the restore.
//!
//! Every measure also lands in a cell the publisher shares ([`JournalWatch::reading`]), which puts it on every frame
//! (`vike_tradehub_client::wire::CoreSnapshotWire::journal`, `trade health`'s `journal-size` row): `None` until the
//! first measure and while the directory does not read.
//!
//! Off the fold: a directory listing on the summary thread, never on the core thread and never per message. The core
//! writes the journal; this only stats it.
//!
//! ⚠ **A segment is PREALLOCATED** (64 MiB at the daemon's default, `vike_core::JournalConfig`), so the total moves in
//! whole segments, and so does the rate. The rate is judged only once the window spans [`GROWTH_MIN_SPAN_MS`], so one
//! roll just after the start is no rate. A total that SHRANK (the boot prune, an archive) or a clock that went back
//! starts the window over.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use vike_config::config::{
    DEFAULT_JOURNAL_GROWTH_ALERT_MIB_PER_HOUR, DEFAULT_JOURNAL_SIZE_ALERT_MIB,
};
use vike_tradehub_client::wire::JournalWire;

/// How often the watch measures, at most: the summary thread ticks every few seconds.
pub const MEASURE_EVERY_MS: i64 = 60_000;
/// The window the growth is measured over.
pub const GROWTH_WINDOW_MS: i64 = 3_600_000;
/// The shortest window a growth is judged over: four whole segments at the default row.
pub const GROWTH_MIN_SPAN_MS: i64 = 15 * 60_000;

const MIB: u64 = 1024 * 1024;
const HOUR_MS: u128 = 3_600_000;

/// What the watch measured when the journal went over ([`JournalWatch::tick`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalReading {
    /// The bytes of every file under the journal directory.
    pub total_bytes: u64,
    /// Its top-level segments (`journal-{index}.vjl`, `vike_journal`'s naming).
    pub segments: u64,
    /// How fast the total grew over the window, in bytes an hour; `None` while the window spans less than
    /// [`GROWTH_MIN_SPAN_MS`].
    pub bytes_per_hour: Option<u64>,
}

/// The watch over one journal directory, owned by the summary thread.
pub struct JournalWatch {
    dir: PathBuf,
    size_alert_bytes: u64,
    growth_alert_bytes_per_hour: u64,
    /// `(epoch ms, total bytes)` of each measure inside the window, oldest first.
    samples: VecDeque<(i64, u64)>,
    last_measure_ms: Option<i64>,
    /// Whether the last measure read the directory, so a failure is said once, not every minute.
    readable: bool,
    /// Whether the journal is over either row: the latch that makes a crossing one alert.
    over: bool,
    /// The last measure, shared with the publisher ([`Self::reading`]).
    shown: Arc<Mutex<Option<JournalWire>>>,
}

impl JournalWatch {
    /// A watch over `dir` with the two rows' values; an absent row is its default.
    pub fn new(
        dir: PathBuf,
        size_alert_mib: Option<u32>,
        growth_alert_mib_per_hour: Option<u32>,
    ) -> Self {
        let bytes = |mib: u32| u64::from(mib) * MIB;
        JournalWatch {
            dir,
            size_alert_bytes: bytes(size_alert_mib.unwrap_or(DEFAULT_JOURNAL_SIZE_ALERT_MIB)),
            growth_alert_bytes_per_hour: bytes(
                growth_alert_mib_per_hour.unwrap_or(DEFAULT_JOURNAL_GROWTH_ALERT_MIB_PER_HOUR),
            ),
            samples: VecDeque::new(),
            last_measure_ms: None,
            readable: true,
            over: false,
            shown: Arc::default(),
        }
    }

    /// The cell each measure lands in, for the publisher to put on every frame
    /// (`crate::publish::PublisherHandle::show_journal`).
    pub fn reading(&self) -> Arc<Mutex<Option<JournalWire>>> {
        Arc::clone(&self.shown)
    }

    fn show(&self, reading: Option<JournalWire>) {
        *self.shown.lock().unwrap_or_else(PoisonError::into_inner) = reading;
    }

    /// Measure when one is due, and return the reading on the measure that finds the journal newly over either
    /// row; `None` on every other tick.
    pub fn tick(&mut self, now_ms: i64) -> Option<JournalReading> {
        if self.last_measure_ms.is_some_and(|at| (at..at + MEASURE_EVERY_MS).contains(&now_ms)) {
            return None;
        }
        self.last_measure_ms = Some(now_ms);
        match measure(&self.dir) {
            Ok((total_bytes, segments)) => {
                self.readable = true;
                self.judge(now_ms, total_bytes, segments)
            }
            Err(error) => {
                self.show(None);
                if std::mem::replace(&mut self.readable, false) {
                    tracing::warn!(
                        dir = %self.dir.display(), %error,
                        "the journal watch cannot read the journal directory: its size and growth are not \
                         measured until it can"
                    );
                }
                None
            }
        }
    }

    fn judge(&mut self, now_ms: i64, total_bytes: u64, segments: u64) -> Option<JournalReading> {
        if self.samples.back().is_some_and(|&(at, bytes)| total_bytes < bytes || now_ms < at) {
            self.samples.clear();
        }
        self.samples.push_back((now_ms, total_bytes));
        while self.samples.front().is_some_and(|&(at, _)| now_ms - at > GROWTH_WINDOW_MS) {
            self.samples.pop_front();
        }
        let (first_ms, first_bytes) =
            self.samples.front().copied().unwrap_or((now_ms, total_bytes));
        let span = now_ms - first_ms;
        let bytes_per_hour = (span >= GROWTH_MIN_SPAN_MS).then(|| {
            let grown = u128::from(total_bytes.saturating_sub(first_bytes));
            u64::try_from(grown * HOUR_MS / u128::from(span.unsigned_abs())).unwrap_or(u64::MAX)
        });
        let over = total_bytes > self.size_alert_bytes
            || bytes_per_hour.is_some_and(|rate| rate > self.growth_alert_bytes_per_hour);
        self.show(Some(JournalWire { total_bytes, segments, bytes_per_hour, over }));
        if over == self.over {
            return None;
        }
        self.over = over;
        let (size_mib, rate_mib) = (total_bytes / MIB, bytes_per_hour.map(|rate| rate / MIB));
        let (size_alert_mib, growth_alert_mib) =
            (self.size_alert_bytes / MIB, self.growth_alert_bytes_per_hour / MIB);
        if !over {
            tracing::info!(
                dir = %self.dir.display(), size_mib, segments, rate_mib,
                "the journal is back under its alert rows ({size_alert_mib} MiB, {growth_alert_mib} MiB an hour)"
            );
            return None;
        }
        tracing::warn!(
            dir = %self.dir.display(), size_mib, segments, rate_mib,
            "the journal is OVER its alert rows: {size_mib} MiB in {segments} segment(s), growing {} \
             (`config.journal_size_alert_mib` {size_alert_mib}, `config.journal_growth_alert_mib_per_hour` \
             {growth_alert_mib}); every record past the last snapshot is replayed at the next start",
            rate_mib.map_or("at a rate not measured yet".to_string(), |rate| format!("{rate} MiB an hour"))
        );
        Some(JournalReading { total_bytes, segments, bytes_per_hour })
    }
}

/// The bytes of every file under `dir` but its archive, and how many of its top-level files are segments. Only `dir`
/// itself must read: an entry the writer renamed or the prune removed between the listing and its stat is skipped.
/// The archive (`vike_journal::ARCHIVE_DIR`, decision 0304) is not the live journal: no boot reads it, and its own
/// size and age limit say when it is full (`crate::journal_archive`).
fn measure(dir: &Path) -> std::io::Result<(u64, u64)> {
    let (mut bytes, mut segments) = (0, 0);
    let mut pending = vec![(std::fs::read_dir(dir)?, true)];
    while let Some((entries, top)) = pending.pop() {
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                if top && entry.file_name() == vike_journal::ARCHIVE_DIR {
                    continue;
                }
                if let Ok(inner) = std::fs::read_dir(entry.path()) {
                    pending.push((inner, false));
                }
                continue;
            }
            bytes += meta.len();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            segments += u64::from(top && name.starts_with("journal-") && name.ends_with(".vjl"));
        }
    }
    Ok((bytes, segments))
}

#[cfg(test)]
#[path = "journal_watch_tests.rs"]
mod journal_watch_tests;

//! **The journal ARCHIVE thread** (`journal-archive`;
//! `docs/decisions/0304-a-boot-restores-from-the-newest-snapshot-and-its-tail-and-old-segments-are-archived-never-deleted.md`,
//! verdict 2): every [`ARCHIVE_INTERVAL`], beside the running core, it MOVES the segments the newest durable
//! `Snapshot` superseded and the keep window let go out of the live journal directory into its `archive/`
//! (`vike_journal::Journal::prune_before_latest_snap`), then compresses them there (`Journal::compress_archive`). Its
//! first pass runs as it starts, so it is the boot's prune too: the boot restore prunes nothing any more.
//!
//! - It never names the core: it reads the journal's FILES and the sync thread's durable mark
//!   (`vike_journal::JournalDurableWatch`), as the execution stream does. A pass with nothing to move reads two
//!   segment heads and lists the archive; a moved 64 MiB segment costs about a second of one core to compress.
//! - Under `config.journal_archive_full = keep`, the default, it deletes nothing: at the archive's limit a pass moves
//!   nothing, said at ERROR once, and at INFO once the archive is under its limit again. Under `drop_oldest`, the
//!   operator's opt-in, the pass first deletes the oldest finished archived copies until the archive is under its
//!   limits, each at INFO (`vike_journal::JournalArchivePolicy`'s `drop_oldest`), and moves on; ERROR only when what
//!   no deletion may take keeps it at the limit.
//! - A failed step is said at WARN once per distinct error (repeats at DEBUG) and tried again at the next pass.
//! - It is joined at the stop as a bounded task: it looks at the stop flag between short slices of its wait.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_journal::{Journal, JournalArchiveLimit, JournalArchivePolicy, JournalDurableWatch};
use vike_model::JournalArchiveFull;

/// How often the thread looks: a segment leaves the live directory within this of its keep window ending.
pub const ARCHIVE_INTERVAL: Duration = Duration::from_secs(60);

/// How long the thread sleeps between two looks at its stop flag.
const STOP_SLICE: Duration = Duration::from_millis(250);

/// The running archive thread, joined at the stop.
pub struct JournalArchiver {
    join: JoinHandle<()>,
}

impl JournalArchiver {
    /// Start the `journal-archive` thread over the open journal at `dir`, whose durable mark `watch` reads, under
    /// `policy` (the four archive rows). It runs until `stop` is raised.
    ///
    /// # Errors
    /// The thread could not be spawned (nothing is archived; trading is untouched).
    pub fn spawn(
        dir: PathBuf,
        watch: JournalDurableWatch,
        policy: JournalArchivePolicy,
        stop: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let join = std::thread::Builder::new().name("journal-archive".into()).spawn(move || {
            let mut said = Said::default();
            while !stop.load(Ordering::Relaxed) {
                said.pass(&dir, &watch, &policy);
                let until = Instant::now() + ARCHIVE_INTERVAL;
                while !stop.load(Ordering::Relaxed) && Instant::now() < until {
                    std::thread::sleep(STOP_SLICE);
                }
            }
        })?;
        Ok(JournalArchiver { join })
    }

    /// Wait for the thread, once the caller raised its stop flag: bounded by one pass.
    pub fn join(self) {
        // A `join` Err is a panic its hook already reported; the teardown goes on.
        let _ = self.join.join();
    }
}

/// What the thread has said, so each state is said once, at its transition.
#[derive(Debug, Default)]
struct Said {
    at_limit: bool,
    move_failure: Option<String>,
    compress_failure: Option<String>,
}

impl Said {
    /// One pass: the move, then the compression of whatever the archive holds plain.
    fn pass(&mut self, dir: &Path, watch: &JournalDurableWatch, policy: &JournalArchivePolicy) {
        let moved = Journal::prune_before_latest_snap(
            dir,
            policy,
            vike_model::now_ms(),
            watch.durable_mark(),
        );
        match moved {
            Ok(pass) => {
                self.move_failure = None;
                if !pass.moved.is_empty() {
                    tracing::info!(
                        journal = %dir.display(),
                        segments = ?pass.moved,
                        "journal archive: superseded segments moved to the journal's archive/ (kept, never deleted)"
                    );
                }
                self.limit(dir, pass.limit, policy);
            }
            Err(e) => {
                say_failure(&mut self.move_failure, dir, format!("moving superseded segments: {e}"))
            }
        }
        match Journal::compress_archive(dir) {
            Ok(_) => self.compress_failure = None,
            Err(e) => say_failure(
                &mut self.compress_failure,
                dir,
                format!("compressing the archive: {e}"),
            ),
        }
    }

    /// Say the archive's limit at its transitions.
    fn limit(
        &mut self,
        dir: &Path,
        limit: Option<JournalArchiveLimit>,
        policy: &JournalArchivePolicy,
    ) {
        match (limit, self.at_limit) {
            (Some(limit), false) => {
                let what = match limit {
                    JournalArchiveLimit::Size { bytes } => format!(
                        "it holds {} MiB, at its limit of {} MiB (config.journal_archive_max_gib)",
                        bytes >> 20,
                        policy.max_bytes >> 20
                    ),
                    JournalArchiveLimit::Age { age_ms } => format!(
                        "its oldest file is {} days old, past its limit of {} days (config.journal_archive_max_days)",
                        age_ms / vike_model::MS_PER_DAY,
                        policy.max_age_ms / vike_model::MS_PER_DAY
                    ),
                };
                let action = match policy.at_limit {
                    JournalArchiveFull::Keep => {
                        "config.journal_archive_full = keep: nothing more is archived, every newer segment stays in the \
                         live journal directory, and nothing in the archive is touched; raise the limit or move \
                         archived files off this box"
                    }
                    JournalArchiveFull::DropOldest => {
                        "config.journal_archive_full = drop_oldest: the archive is still at its limit after its \
                         oldest compressed files went, held there by files not compressed yet, which no deletion \
                         takes: nothing more is archived until it is under; raise the limit"
                    }
                };
                tracing::error!(journal = %dir.display(), "journal archive: {what}; {action}");
                self.at_limit = true;
            }
            (None, true) => {
                tracing::info!(journal = %dir.display(), "journal archive: under its limits again; archiving goes on");
                self.at_limit = false;
            }
            _ => {}
        }
    }
}

/// A failed step at WARN the first time its words are seen, at DEBUG while they repeat.
fn say_failure(last: &mut Option<String>, dir: &Path, why: String) {
    if last.as_deref() == Some(why.as_str()) {
        tracing::debug!(journal = %dir.display(), "journal archive: {why}");
    } else {
        tracing::warn!(
            journal = %dir.display(),
            "journal archive: {why}; every segment stays where it is, and the next pass tries again"
        );
        *last = Some(why);
    }
}

#[cfg(test)]
#[path = "journal_archive_tests.rs"]
mod journal_archive_tests;

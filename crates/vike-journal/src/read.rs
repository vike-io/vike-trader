//! The offline reader half: [`read_since`] (the incremental tail-read a materializer resumes from)
//! and [`MaterializeCheckpoint`] (the tiny durable `<dir>/materializer.ckpt` it resumes WITH).
//! Neither takes the journal directory's writer lock. Split out of `journal.rs` verbatim.

use std::fs::OpenOptions;
use std::io;
use std::path::Path;

use super::record::{JournalRecord, record_seq};
use super::writer::CommandJournal;

/// Every valid record with `seq > after_seq`, in seq order — the incremental tail-read a future
/// materializer uses to consume only records past where it last stopped. `after_seq` is the last
/// seq already CONSUMED and the bound is STRICT (exclusive), which is the resume semantic a
/// checkpoint needs. Simplest correct impl: [`CommandJournal::read_all`] then filter by
/// [`record_seq`].
///
/// Note the lower bound is strict: `after_seq == 0` returns every record with `seq > 0`. Because
/// this journal assigns the FIRST record seq 0, a cold-start materializer that has never
/// checkpointed (so [`MaterializeCheckpoint::load`] returns 0) skips that seq-0 record. In
/// practice the materializer is seeded from the latest `Snap` (whose seq is its exclusive base),
/// not from raw seq 0, so this is benign; a caller that must include seq 0 keys off a checkpoint
/// below the first seq rather than `read_since(dir, 0)`.
///
/// This is O(journal): it reads and deserializes every segment. That is deliberate and stays
/// bounded because the WAL is snapshot-compacted ([`CommandJournal::prune_before_latest_snap`]
/// drops fully-superseded segments), so the live journal never grows without limit. A streaming
/// per-segment reader that seeks to `after_seq` and skips earlier records is a deliberate later
/// optimization, not needed for correctness.
pub fn read_since(dir: &Path, after_seq: u64) -> io::Result<Vec<JournalRecord>> {
    let mut all = CommandJournal::read_all(dir)?;
    all.retain(|r| record_seq(r) > after_seq);
    Ok(all)
}

/// A tiny durable checkpoint file (`<dir>/materializer.ckpt`) recording the last seq a materializer
/// has consumed, so it can resume its incremental [`read_since`] tail across process restarts. The
/// checkpoint lives beside the journal segments but is entirely independent of them: it is never
/// read by replay/restore and pruning never touches it.
pub struct MaterializeCheckpoint;

impl MaterializeCheckpoint {
    const FILE: &'static str = "materializer.ckpt";
    const TMP: &'static str = "materializer.ckpt.tmp";

    /// The stored last-materialized seq, or `0` when the file is absent or unparseable — absent ⇒
    /// start from `0`. Never errors: a materializer that finds no (or a corrupt) checkpoint simply
    /// re-materializes from the beginning of the (snapshot-compacted) journal.
    pub fn load(dir: &Path) -> u64 {
        std::fs::read_to_string(dir.join(Self::FILE))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0)
    }

    /// Whether a checkpoint file exists — i.e. materialization has started. `load` returns 0 both
    /// for "absent" and for "materialized up to seq 0", so a cold-start consumer that must include
    /// the seq-0 record (which `read_since(dir, 0)` excludes) uses this to choose a full read.
    pub fn exists(dir: &Path) -> bool {
        dir.join(Self::FILE).exists()
    }

    /// Durably overwrite the checkpoint with `seq` using the standard crash-safe write-temp →
    /// fsync → atomic-rename pattern: a torn write leaves the old checkpoint intact, never a
    /// half-written seq. `sync_all` is the plain-file fsync twin of the journal's `map.flush`
    /// (msync). `std::fs::rename` replaces the destination atomically on both Unix and Windows
    /// (`MoveFileEx` with replace-existing).
    pub fn store(dir: &Path, seq: u64) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(Self::TMP);
        {
            let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
            use std::io::Write;
            f.write_all(seq.to_string().as_bytes())?;
            f.sync_all()?; // fsync the bytes before the rename makes them the live checkpoint
        }
        std::fs::rename(&tmp, dir.join(Self::FILE))
    }
}

#[path = "read_tests.rs"]
#[cfg(test)]
mod read_tests;

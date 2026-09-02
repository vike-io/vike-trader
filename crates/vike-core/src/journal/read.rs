//! The offline reader half: [`read_since`] (the incremental tail-read a materializer resumes from)
//! and [`MaterializeCheckpoint`] (the tiny durable `<dir>/materializer.ckpt` it resumes WITH).
//! Neither takes the journal directory's writer lock. Split out of `journal.rs` verbatim.

use std::fs::OpenOptions;
use std::io;
use std::path::Path;

use super::record::{record_seq, JournalRecord};
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::testutil::*;
    use crate::journal::JournalFileConfig;

    #[test]
    fn read_since_returns_only_records_strictly_after_the_cutoff() {
        let dir = tmp_dir("read-since");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 4 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..10 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap(); // seqs 0..=9
        }
        drop(j);

        // `after_seq == 0` ⇒ everything strictly after the null checkpoint. This journal's first
        // record carries seq 0, and the lower bound is STRICT (`seq > after_seq`, the exact resume
        // semantic the materializer needs: a checkpoint is the last seq CONSUMED, exclusive), so
        // seq 0 is excluded and seqs 1..=9 (9 records) are returned in seq order.
        let all = read_since(&dir, 0).unwrap();
        let all_seqs: Vec<u64> = all.iter().map(record_seq).collect();
        assert_eq!(all_seqs, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);

        // strictly-greater cutoff: seq 4 excluded, seqs 5..=9 returned in order.
        let tail = read_since(&dir, 4).unwrap();
        let seqs: Vec<u64> = tail.iter().map(record_seq).collect();
        assert_eq!(seqs, vec![5, 6, 7, 8, 9]);

        // a cutoff at/after the last seq ⇒ empty.
        assert!(read_since(&dir, 9).unwrap().is_empty());
        assert!(read_since(&dir, 100).unwrap().is_empty());
    }

    #[test]
    fn checkpoint_store_load_roundtrips_and_overwrites() {
        let dir = tmp_dir("ckpt");
        // absent file ⇒ 0, never errors.
        assert_eq!(MaterializeCheckpoint::load(&dir), 0);

        MaterializeCheckpoint::store(&dir, 42).unwrap();
        assert_eq!(MaterializeCheckpoint::load(&dir), 42);

        // a second store overwrites.
        MaterializeCheckpoint::store(&dir, 1_000_000).unwrap();
        assert_eq!(MaterializeCheckpoint::load(&dir), 1_000_000);
    }

    #[test]
    fn checkpoint_load_on_absent_dir_is_zero() {
        // A directory that does not exist at all ⇒ 0 (absent ⇒ start from 0, never errors).
        // `reserved` is the point: it names a path and creates nothing.
        let dir = crate::scratch::Scratch::reserved("test-ckpt-absent");
        assert_eq!(MaterializeCheckpoint::load(&dir), 0);
    }

    #[test]
    fn checkpoint_load_on_garbage_is_zero() {
        let dir = tmp_dir("ckpt-garbage");
        std::fs::write(dir.join("materializer.ckpt"), b"not-a-number\n").unwrap();
        assert_eq!(MaterializeCheckpoint::load(&dir), 0);
    }
}

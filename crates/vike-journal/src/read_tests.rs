//! Tests of the offline reader half: `read_since`'s strict cutoff and the checkpoint file.

use super::*;
use crate::JournalFileConfig;
use crate::testutil::*;

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
    let dir = Scratch::reserved("test-ckpt-absent");
    assert_eq!(MaterializeCheckpoint::load(&dir), 0);
}

#[test]
fn checkpoint_load_on_garbage_is_zero() {
    let dir = tmp_dir("ckpt-garbage");
    std::fs::write(dir.join("materializer.ckpt"), b"not-a-number\n").unwrap();
    assert_eq!(MaterializeCheckpoint::load(&dir), 0);
}

//! Tests of the offline prune: `prune_before_latest_snap` against the snapshot and checkpoint floors.

use crate::read::MaterializeCheckpoint;
use crate::record::{JournalRecord, record_seq};
use crate::testutil::*;
use crate::{CommandJournal, JournalFileConfig};

#[test]
fn prune_deletes_fully_superseded_segments_and_keeps_the_latest_snap_onward() {
    let dir = tmp_dir("prune");
    // small segments so the journal rolls; the Snap lands in a LATER segment
    let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    for i in 0..300 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    let snap_seq =
        j.append_snap(9_999, &snap_engines(), "sess", 7, 0, &[], &[], &[], 0xABCD).unwrap();
    for i in 300..360 {
        j.append_cmd(2_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);

    let files_before = seg_files(&dir).len();
    assert!(files_before >= 3, "scenario must span multiple segments (got {files_before})");
    let all_before = CommandJournal::read_all(&dir).unwrap();
    let max_seq_before = all_before.iter().map(record_seq).max().unwrap();

    CommandJournal::prune_before_latest_snap(&dir).unwrap();

    // Earliest, fully-superseded segment(s) are gone.
    let files_after = seg_files(&dir).len();
    assert!(files_after < files_before, "fully-superseded early segments must be deleted");
    assert!(
        !dir.join("journal-00000000.vjl").exists(),
        "segment 0 (all seqs < latest snap seq) must be pruned"
    );

    // The latest-Snap record + everything after it survive (restore base + its tail intact).
    let all_after = CommandJournal::read_all(&dir).unwrap();
    assert!(
        all_after.iter().any(|r| matches!(r, JournalRecord::Snap { seq, .. } if *seq == snap_seq)),
        "the latest Snap must survive pruning (it is the restore base)"
    );
    let max_seq_after = all_after.iter().map(record_seq).max().unwrap();
    assert_eq!(max_seq_after, max_seq_before, "records after the snap must not be lost");
    let first_seq_after = record_seq(all_after.first().unwrap());
    assert!(first_seq_after > 0, "the pruned early segment held seq 0; it is gone");
    assert!(first_seq_after <= snap_seq, "the snap's own segment is kept WHOLE (earlier cmds too)");
}

#[test]
fn prune_respects_the_materializer_checkpoint_floor() {
    let dir = tmp_dir("prune_mat");
    let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    for i in 0..300 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    j.append_snap(9_999, &snap_engines(), "sess", 7, 0, &[], &[], &[], 0xABCD).unwrap();
    for i in 300..360 {
        j.append_cmd(2_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let files_before = seg_files(&dir).len();
    assert!(files_before >= 3);

    // A materializer that has consumed only up to seq 0 → NOTHING past seq 0 may be pruned,
    // even the segments the snapshot alone would drop.
    MaterializeCheckpoint::store(&dir, 0).unwrap();
    CommandJournal::prune_before_latest_snap(&dir).unwrap();
    assert_eq!(
        seg_files(&dir).len(),
        files_before,
        "no segment is pruned while the materializer is behind"
    );
    assert!(dir.join("journal-00000000.vjl").exists(), "segment 0 is kept: not yet materialized");

    // Once the materializer catches up (past every seq), pruning proceeds snapshot-only again.
    MaterializeCheckpoint::store(&dir, u64::MAX).unwrap();
    CommandJournal::prune_before_latest_snap(&dir).unwrap();
    assert!(
        seg_files(&dir).len() < files_before,
        "a caught-up materializer no longer blocks pruning"
    );
}

#[test]
fn prune_never_deletes_the_latest_snap_segment_or_later() {
    // The Snap lands in the EARLIEST segment; every later segment holds only post-snap Cmds.
    // Nothing may be deleted — the snap's segment and all later ones are the restore base+tail.
    let dir = tmp_dir("prune-safe");
    let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    for i in 0..5 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    let snap_seq = j.append_snap(9_999, &snap_engines(), "sess", 0, 0, &[], &[], &[], 1).unwrap();
    for i in 5..300 {
        j.append_cmd(2_000 + i as i64, &ingest(i)).unwrap(); // roll into later segments
    }
    drop(j);

    let files_before = seg_files(&dir).len();
    assert!(files_before >= 2, "must span multiple segments (got {files_before})");
    let n_before = CommandJournal::read_all(&dir).unwrap().len();

    CommandJournal::prune_before_latest_snap(&dir).unwrap();

    assert_eq!(
        seg_files(&dir).len(),
        files_before,
        "no segment may be deleted: the snap is in the earliest segment, later segments are its tail"
    );
    let all_after = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(all_after.len(), n_before, "no records may be lost");
    assert!(
        all_after.iter().any(|r| matches!(r, JournalRecord::Snap { seq, .. } if *seq == snap_seq)),
        "the latest snap survives"
    );
}

#[test]
fn prune_with_no_snap_is_a_noop() {
    let dir = tmp_dir("prune-nosnap");
    let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    for i in 0..300 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let files_before = seg_files(&dir).len();
    let n_before = CommandJournal::read_all(&dir).unwrap().len();

    CommandJournal::prune_before_latest_snap(&dir).unwrap();

    assert_eq!(seg_files(&dir).len(), files_before, "no Snap -> prune nothing");
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), n_before, "no records lost");
}

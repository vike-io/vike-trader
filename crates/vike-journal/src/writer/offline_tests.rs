//! Tests of the offline prune: `prune_before_latest_snap` against the snapshot and checkpoint floors.

use crate::read::MaterializeCheckpoint;
use crate::record::{JournalRecord, SnapBooks, record_seq};
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
        j.append_snap(9_999, &snap_engines(), "sess", 7, 0, SnapBooks::default(), 0xABCD).unwrap();
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
    j.append_snap(9_999, &snap_engines(), "sess", 7, 0, SnapBooks::default(), 0xABCD).unwrap();
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
    let snap_seq =
        j.append_snap(9_999, &snap_engines(), "sess", 0, 0, SnapBooks::default(), 1).unwrap();
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

// ---------------------------------------------------------------------------------------------
// The boot restore's reader and its two directory moves: `read_all_located`,
// `preserve_torn_segment`, `set_aside`.
// ---------------------------------------------------------------------------------------------

// `end_of_frames` and `plant` are `crate::testutil`'s (the `read_since` suite plants too).

/// A torn length prefix at the end of the ONLY segment: the halt names that segment, the offset
/// of the torn frame and `in_tail`, and the records before it are all returned.
#[test]
fn read_all_located_reports_segment_offset_and_last_segment() {
    let dir = tmp_dir("located-tail");
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    for i in 0..5 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let seg = seg_files(&dir).remove(0);
    let end = end_of_frames(&seg);
    plant(&seg, end, &u32::MAX.to_le_bytes());

    let (records, halt) = CommandJournal::read_all_located(&dir).unwrap();
    assert_eq!(records.len(), 5, "every record before the torn frame reads");
    let halt = halt.expect("the torn frame halts the read");
    assert_eq!(halt.cause, crate::CorruptCause::OobLen);
    assert_eq!(halt.segment, seg);
    assert_eq!(halt.segment_index, 0);
    assert_eq!(halt.offset, end, "the offset of the frame that did not read");
    assert!(halt.in_tail, "nothing was written after it");
    let (_, cause) = CommandJournal::read_all_reporting(&dir).unwrap();
    assert_eq!(cause, Some(crate::CorruptCause::OobLen), "the reporting reader agrees");
}

/// A corrupt frame in an EARLIER segment, with records written in later ones: not the tail.
#[test]
fn a_halt_before_later_records_is_not_in_the_tail() {
    let dir = tmp_dir("located-middle");
    let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    for i in 0..300 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    assert!(seg_files(&dir).len() >= 2, "the scenario must roll");
    // Flip a byte inside the FIRST frame's payload of segment 0: its checksum no longer matches.
    let seg0 = dir.join("journal-00000000.vjl");
    let at = crate::format::HEADER + 8;
    let byte = std::fs::read(&seg0).unwrap()[at];
    plant(&seg0, at, &[byte ^ 0xFF]);

    let (records, halt) = CommandJournal::read_all_located(&dir).unwrap();
    assert!(records.is_empty(), "the halt is at the very first frame");
    let halt = halt.expect("halted");
    assert_eq!(halt.cause, crate::CorruptCause::CheckMismatch);
    assert_eq!(halt.segment_index, 0);
    assert!(!halt.in_tail, "later segments hold records the halt makes unreadable");
}

/// The torn segment is COPIED (the original stays for the live core to resume), the copies are
/// named by the caller's stamp, and only the newest `keep` survive.
#[test]
fn preserve_torn_segment_copies_the_segment_and_keeps_the_newest_three() {
    let dir = tmp_dir("torn-copy");
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    j.append_cmd(1_000, &ingest(0)).unwrap();
    drop(j);
    let seg = seg_files(&dir).remove(0);
    let end = end_of_frames(&seg);
    plant(&seg, end, &u32::MAX.to_le_bytes());
    let (_, halt) = CommandJournal::read_all_located(&dir).unwrap();
    let halt = halt.unwrap();

    let mut made = Vec::new();
    for stamp in ["0000000000001", "0000000000002", "0000000000003", "0000000000004"] {
        made.push(CommandJournal::preserve_torn_segment(&dir, &halt, stamp, 3).unwrap());
    }
    assert_eq!(
        std::fs::read(&made[3]).unwrap(),
        std::fs::read(&seg).unwrap(),
        "the copy is the segment, byte for byte"
    );
    assert!(seg.exists(), "the original is left for the live core");
    let torn = dir.join(crate::TORN_DIR);
    let mut left: Vec<String> = std::fs::read_dir(&torn)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        vec![
            "0000000000002-journal-00000000.vjl".to_string(),
            "0000000000003-journal-00000000.vjl".to_string(),
            "0000000000004-journal-00000000.vjl".to_string(),
        ],
        "the oldest copy went, three stay"
    );
    assert_eq!(seg_files(&dir).len(), 1, "a copy under torn/ is not a segment of the journal");
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 1, "and is never read back");
}

/// `set_aside` RENAMES the directory beside itself under a unique name and deletes nothing; a
/// second move on the same stamp does not collide with the first.
#[test]
fn set_aside_moves_the_directory_to_a_unique_name_and_deletes_nothing() {
    let root = tmp_dir("set-aside");
    let dir = root.join("journal");
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    j.append_cmd(1_000, &ingest(0)).unwrap();
    drop(j);

    let first = CommandJournal::set_aside(&dir, "0000000000007").unwrap();
    assert_eq!(first, root.join("journal.broken-0000000000007"));
    assert!(!dir.exists(), "the journal directory is free for a fresh one");
    assert_eq!(CommandJournal::read_all(&first).unwrap().len(), 1, "every record moved with it");

    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    j.append_cmd(2_000, &ingest(1)).unwrap();
    drop(j);
    let second = CommandJournal::set_aside(&dir, "0000000000007").unwrap();
    assert_eq!(second, root.join("journal.broken-0000000000007-1"), "a taken name is not reused");
    assert!(first.exists() && second.exists());
}

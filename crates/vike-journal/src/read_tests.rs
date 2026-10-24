//! Tests of the offline reader half: `read_since`'s strict cutoff, its equality with `read_all`
//! filtered (segments left out, segments read in steps, torn and corrupt segments), and the
//! checkpoint file.

use std::time::Duration;

use tracing_test::traced_test;

use super::*;
use crate::JournalFileConfig;
use crate::testutil::*;

/// Each record's serde bytes: `JournalRecord` has no `PartialEq`, and two reads agree exactly when
/// these do.
fn bytes(records: &[JournalRecord]) -> Vec<Vec<u8>> {
    records.iter().map(|r| serde_json::to_vec(r).unwrap()).collect()
}

/// THE ORACLE: `read_all` then the filter `read_since` used to be.
fn filtered(dir: &Path, after: u64) -> Vec<Vec<u8>> {
    let mut all = CommandJournal::read_all(dir).unwrap();
    all.retain(|r| record_seq(r) > after);
    bytes(&all)
}

/// A journal of `n` cancels over 4 KiB segments: several segments, every one but the last CLOSED.
fn rolled(tag: &str, n: u64) -> Scratch {
    let dir = tmp_dir(tag);
    let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    for i in 0..n {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    assert!(segment_indices(&dir).unwrap().len() >= 3, "the scenario must span several segments");
    dir
}

/// Segment `idx`'s header `first_seq`: the seq of its first record.
fn first_seq(dir: &Path, idx: u64) -> u64 {
    let b = std::fs::read(seg_path(dir, idx)).unwrap();
    u64::from_le_bytes(b[8..16].try_into().unwrap())
}

/// The highest segment index (the one the writer would resume).
fn last_idx(dir: &Path) -> u64 {
    *segment_indices(dir).unwrap().last().unwrap()
}

/// Corrupt the LAST frame of segment `idx` (one payload byte flipped: a check mismatch AFTER every
/// other record of the segment) and set its mtime to `stamp`. Returns that frame's offset.
fn corrupt_last_frame(dir: &Path, idx: u64, stamp: SystemTime) -> usize {
    let seg = seg_path(dir, idx);
    let at = *frame_starts(&seg).last().unwrap();
    let byte = std::fs::read(&seg).unwrap()[at + 8];
    plant(&seg, at + 8, &[byte ^ 0xFF]);
    std::fs::File::options().write(true).open(&seg).unwrap().set_modified(stamp).unwrap();
    at
}

fn mtime(dir: &Path, idx: u64) -> SystemTime {
    std::fs::metadata(seg_path(dir, idx)).unwrap().modified().unwrap()
}

/// The cutoff inside the LAST segment: the closed segments are left out once remembered, and the
/// answer is `read_all` filtered whatever the read step and whether the memo is cold or warm.
#[test]
fn read_since_equals_read_all_filtered_with_the_cutoff_in_the_last_segment() {
    let dir = rolled("since-last", 300);
    let after = first_seq(&dir, last_idx(&dir)) + 2;
    let want = filtered(&dir, after);
    assert!(!want.is_empty(), "the last segment holds records past the cutoff");

    for step in [1, 7, 4096, READ_CHUNK] {
        let got = read_since_with(&dir, after, &mut SegmentMemo::default(), step).unwrap();
        assert_eq!(bytes(&got), want, "cold, read step {step}");
    }
    let mut memo = SegmentMemo::default();
    assert_eq!(bytes(&read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap()), want);
    let closed = segment_indices(&dir).unwrap().len() - 1;
    assert_eq!(memo.0.len(), closed, "every closed segment is remembered, the live one is not");
    assert!(!memo.0.contains_key(&last_idx(&dir)));
    assert_eq!(bytes(&read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap()), want, "warm");

    // The public entry point, twice: the second call runs on the memo the first left behind.
    assert_eq!(bytes(&read_since(&dir, after).unwrap()), want);
    assert_eq!(bytes(&read_since(&dir, after).unwrap()), want);
}

/// `after_seq == 0`: everything but seq 0, cold and warm.
#[test]
fn read_since_zero_returns_every_record_but_seq_zero() {
    let dir = rolled("since-zero", 300);
    let all = CommandJournal::read_all(&dir).unwrap();
    let want = filtered(&dir, 0);
    assert_eq!(want, bytes(&all[1..]), "only seq 0 is dropped");
    let mut memo = SegmentMemo::default();
    for pass in ["cold", "warm"] {
        assert_eq!(
            bytes(&read_since_with(&dir, 0, &mut memo, READ_CHUNK).unwrap()),
            want,
            "{pass}"
        );
    }
}

/// A cutoff at or past the head: nothing, cold and warm — and the warm call leaves every closed
/// segment out.
#[test]
fn read_since_past_the_head_returns_nothing() {
    let dir = rolled("since-head", 300);
    let head = CommandJournal::read_all(&dir).unwrap().iter().map(record_seq).max().unwrap();
    let mut memo = SegmentMemo::default();
    for after in [head, head + 1, u64::MAX] {
        for pass in ["cold", "warm"] {
            let got = read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap();
            assert!(got.is_empty(), "after {after}, {pass}");
        }
    }
}

/// A torn frame at the end of the last segment (an out-of-bounds length, then a payload that fails
/// its check) stops the walk at the same frame whatever the read step: a step boundary inside the
/// frame must not turn either into a clean end or move the halt.
#[test]
fn a_torn_tail_reads_the_same_at_every_read_step() {
    let garbage: [&[u8]; 2] =
        [&u32::MAX.to_le_bytes(), &[5, 0, 0, 0, 0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4, 5]];
    for (n, torn) in garbage.into_iter().enumerate() {
        let dir = rolled(&format!("since-torn-{n}"), 300);
        let last = seg_path(&dir, last_idx(&dir));
        let end = end_of_frames(&last);
        assert!(end + torn.len() <= 4 * 1024, "room for the torn frame");
        plant(&last, end, torn);
        for after in [0, first_seq(&dir, last_idx(&dir))] {
            let want = filtered(&dir, after);
            for step in [1, 3, 7, 64, READ_CHUNK] {
                let got = read_since_with(&dir, after, &mut SegmentMemo::default(), step).unwrap();
                assert_eq!(bytes(&got), want, "torn {n}, after {after}, step {step}");
            }
        }
    }
}

/// THE CORRUPTION DECISION, cold: a corrupt frame behind the last record of a CLOSED segment whose
/// every record is `<= after` (what a crash mid-append that `open` then resumed and rolled past can
/// leave). Skipping that segment would return the records behind it, which `read_all` can never
/// reach; `read_since` reads it, halts there, and returns what `read_all` filtered does: nothing.
/// It does so on every call — a halt is never remembered.
#[test]
fn a_corrupt_closed_segment_below_the_cutoff_still_halts_the_read() {
    let dir = rolled("since-corrupt-cold", 300);
    let after = first_seq(&dir, 1) - 1; // segment 0's last seq: all of segment 0 is <= after
    corrupt_last_frame(&dir, 0, mtime(&dir, 0));
    let want = filtered(&dir, after);
    assert!(want.is_empty(), "read_all halts in segment 0, before every record past the cutoff");
    let mut memo = SegmentMemo::default();
    for pass in ["first", "second"] {
        assert_eq!(
            bytes(&read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap()),
            want,
            "{pass}"
        );
        assert!(!memo.0.contains_key(&0), "the corrupt segment is never remembered");
    }
}

/// The same corruption arriving AFTER segment 0 was verified, by a write that moves its mtime: the
/// stamp no longer matches, so the segment is walked again and the read halts as `read_all` does.
#[test]
fn a_corruption_after_verification_is_seen_once_the_segment_file_changes() {
    let dir = rolled("since-corrupt-warm", 300);
    let after = first_seq(&dir, 1) - 1;
    let mut memo = SegmentMemo::default();
    assert!(!read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap().is_empty());
    assert!(memo.0.contains_key(&0), "segment 0 is verified and remembered");

    corrupt_last_frame(&dir, 0, mtime(&dir, 0) + Duration::from_secs(2));
    let want = filtered(&dir, after);
    assert!(want.is_empty());
    assert_eq!(bytes(&read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap()), want);
    assert!(!memo.0.contains_key(&0), "the halt dropped the row");
}

/// ⚠ THE ONE DIFFERENCE, pinned so it cannot widen unnoticed (`SegmentMemo`'s doc): a verified
/// closed segment rewritten in place under its OLD length, mtime and header is still left out, so
/// the corruption it acquired is not seen and the records behind it stay readable. Nothing in this
/// crate writes a closed segment; this test has to forge the mtime to get here.
#[test]
fn a_closed_segment_rewritten_under_its_old_stamp_is_not_read_again() {
    let dir = rolled("since-corrupt-forged", 300);
    let after = first_seq(&dir, 1) - 1;
    let mut memo = SegmentMemo::default();
    let before = bytes(&read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap());
    assert!(!before.is_empty());

    corrupt_last_frame(&dir, 0, mtime(&dir, 0));
    assert!(filtered(&dir, after).is_empty(), "read_all now halts in segment 0");
    let got = bytes(&read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap());
    assert_eq!(got, before, "the remembered segment is left out: the answer from before");
}

/// A halt BEHIND remembered segments warns exactly as `read_all` would: the same segment, offset
/// and cause, and a `records_kept` that counts the left-out segments' records too.
#[traced_test]
#[test]
fn a_halt_behind_remembered_segments_warns_with_read_alls_count() {
    let dir = rolled("since-warn", 300);
    let after = first_seq(&dir, last_idx(&dir));
    let mut memo = SegmentMemo::default();
    read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap();
    assert!(memo.0.contains_key(&0) && memo.0.contains_key(&1));

    let kept = frame_starts(&seg_path(&dir, 0)).len() + frame_starts(&seg_path(&dir, 1)).len() - 1;
    let at = corrupt_last_frame(&dir, 1, mtime(&dir, 1) + Duration::from_secs(2));
    assert!(read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap().is_empty());
    assert!(logs_contain("journal read halted at a corrupt frame"));
    assert!(logs_contain("check_mismatch"));
    assert!(logs_contain(&format!("offset={at}")));
    assert!(logs_contain(&format!("records_kept={kept}")), "the count read_all would report");
    assert!(logs_contain("journal-00000001.vjl"), "the segment that halted");
}

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

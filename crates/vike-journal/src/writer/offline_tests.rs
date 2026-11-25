//! Tests of the offline prune: `prune_before_latest_snap` against the snapshot floor and the
//! execution stream's.

use std::path::Path;

use crate::end_of_frames;
use crate::read::JournalExecutionStream;
use crate::record::{JournalRecord, JournalSnapshotParts, record_sequence};
use crate::testutil::*;
use crate::{EXECUTION_STREAM_HOLD_MS, Journal, JournalFileConfig};
use vike_model::test_support::file::plant;

#[test]
fn prune_moves_fully_superseded_segments_to_the_archive_and_keeps_the_latest_snap_onward() {
    let dir = tmp_dir("prune");
    // small segments so the journal rolls; the Snapshot lands in a LATER segment
    let config = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..300 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    let snap_sequence = j
        .append_snap(9_999, &snap_engines(), "sess", 7, 0, JournalSnapshotParts::default(), 0xABCD)
        .unwrap();
    for i in 300..360 {
        j.append_command(2_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);

    let files_before = segment_files(&dir).len();
    assert!(files_before >= 3, "scenario must span multiple segments (got {files_before})");
    let all_before = Journal::read_all(&dir).unwrap();
    let max_sequence_before = all_before.iter().map(record_sequence).max().unwrap();

    let pass = prune_closed(&dir).unwrap();

    // Earliest, fully-superseded segment(s) left the live directory, into the archive, whole.
    let files_after = segment_files(&dir).len();
    assert!(files_after < files_before, "fully-superseded early segments must leave");
    assert_eq!(pass.moved.len(), files_before - files_after, "{pass:?}");
    assert!(
        !dir.join("journal-00000000.vjl").exists(),
        "segment 0 (all seqs < latest snap seq) must be pruned"
    );
    let archived = dir.join(crate::ARCHIVE_DIR).join("journal-00000000.vjl");
    assert!(archived.exists(), "moved to the archive, never deleted");

    // The latest-Snapshot record + everything after it survive (restore base + its tail intact).
    let all_after = Journal::read_all(&dir).unwrap();
    assert!(
        all_after.iter().any(
            |r| matches!(r, JournalRecord::Snapshot { sequence: seq, .. } if *seq == snap_sequence)
        ),
        "the latest Snapshot must survive pruning (it is the restore base)"
    );
    let max_sequence_after = all_after.iter().map(record_sequence).max().unwrap();
    assert_eq!(max_sequence_after, max_sequence_before, "records after the snap must not be lost");
    let first_sequence_after = record_sequence(all_after.first().unwrap());
    assert!(first_sequence_after > 0, "the pruned early segment held seq 0; it is gone");
    assert!(
        first_sequence_after <= snap_sequence,
        "the snap's own segment is kept WHOLE (earlier cmds too)"
    );
}

/// 300 commands over 4 KiB segments (`now_ms` from `t0`), a `Snapshot`, then 60 more: the snapshot
/// alone supersedes several early segments. Deterministic, so two calls lay out the same segments.
fn snapped(tag: &str, t0: i64, t1: i64) -> Scratch {
    let dir = tmp_dir(tag);
    let config = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..300 {
        j.append_command(t0 + i as i64, &ingest(i)).unwrap();
    }
    j.append_snap(t1, &snap_engines(), "sess", 7, 0, JournalSnapshotParts::default(), 0xABCD)
        .unwrap();
    for i in 300..360 {
        j.append_command(t1 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    assert!(segment_files(&dir).len() >= 3, "the scenario spans several segments");
    dir
}

/// The segment file NAMES in `dir`, sorted: what a prune left.
fn segment_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = segment_files(dir)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The segments a snapshot-only prune keeps of [`snapped`]'s journal (a twin pruned with no
/// stream file).
fn snapshot_only(tag: &str, t0: i64, t1: i64) -> Vec<String> {
    let twin = snapped(tag, t0, t1);
    prune_closed(&twin).unwrap();
    segment_names(&twin)
}

/// The stream file with this floor.
fn store_floor(dir: &Path, acknowledged_sequence: Option<u64>) {
    let stream = JournalExecutionStream {
        journal_id: "0".repeat(32),
        acknowledged_sequence,
        acknowledged_ms: None,
        predecessors: Vec::new(),
    };
    stream.store(dir).unwrap();
}

/// Segment `index`'s header `first_sequence`.
fn first_sequence(dir: &Path, index: u64) -> u64 {
    let b = std::fs::read(crate::segment::segment_path(dir, index)).unwrap();
    u64::from_le_bytes(b[8..16].try_into().unwrap())
}

#[test]
fn the_prune_keeps_every_segment_past_the_acknowledged_floor() {
    let dir = snapped("prune-floor", 1_000, 9_999);
    let before = segment_names(&dir);

    // Nothing acknowledged yet: even the segments the snapshot alone would drop are kept.
    store_floor(&dir, None);
    prune_closed(&dir).unwrap();
    assert_eq!(segment_names(&dir), before, "a null floor keeps every segment");

    // Acknowledged through segment 0's last record: segment 0 goes, segment 1 (past it) stays.
    store_floor(&dir, Some(first_sequence(&dir, 1) - 1));
    prune_closed(&dir).unwrap();
    assert!(!dir.join("journal-00000000.vjl").exists(), "every record of segment 0 is stored");
    assert!(dir.join("journal-00000001.vjl").exists(), "segment 1 holds unacknowledged records");

    // Acknowledged past every record: the snapshot's floor alone decides.
    store_floor(&dir, Some(u64::MAX));
    prune_closed(&dir).unwrap();
    assert_eq!(segment_names(&dir), snapshot_only("prune-floor-twin", 1_000, 9_999));
}

/// BEHAVIOUR CHANGE: the retired materializer's checkpoint is no floor. With no stream file the
/// prune is snapshot-only, a leftover `materializer.ckpt` (here at seq 0, which used to keep every
/// segment) included.
#[test]
fn without_a_stream_file_the_prune_is_snapshot_only() {
    let dir = snapped("prune-no-stream", 1_000, 9_999);
    std::fs::write(dir.join("materializer.ckpt"), "0").unwrap();
    prune_closed(&dir).unwrap();
    assert!(!dir.join("journal-00000000.vjl").exists(), "the checkpoint no longer holds it");
    assert_eq!(segment_names(&dir), snapshot_only("prune-no-stream-twin", 1_000, 9_999));
}

/// BEHAVIOUR CHANGE, the hold: a segment past the floor whose newest record is older than
/// `EXECUTION_STREAM_HOLD_MS` (against the journal's newest record) is pruned anyway, and the prune
/// says at ERROR which sequences and times went unstored. Within the hold it is kept, silently.
#[tracing_test::traced_test]
#[test]
fn a_segment_past_the_floor_older_than_the_hold_is_pruned_and_said() {
    const SAID: &str = "older than the execution stream's hold";
    // The newest record is `t1 + 359`, the early segments' at most `1_299`.
    let within = 1_000 + EXECUTION_STREAM_HOLD_MS - 10_000;
    let held = snapped("prune-hold-within", 1_000, within);
    let before = segment_names(&held);
    store_floor(&held, None);
    prune_closed(&held).unwrap();
    assert_eq!(segment_names(&held), before, "within the hold every unacknowledged segment stays");
    assert!(!logs_contain(SAID));

    let past = 1_000 + EXECUTION_STREAM_HOLD_MS + 10_000;
    let dir = snapped("prune-hold-past", 1_000, past);
    store_floor(&dir, Some(4)); // command `i` is seq `i`, at `1_000 + i`
    prune_closed(&dir).unwrap();
    let kept = segment_names(&dir);
    assert_eq!(kept, snapshot_only("prune-hold-past-twin", 1_000, past), "the snapshot decides");
    let first_kept: u64 = kept[0]["journal-".len()..kept[0].len() - ".vjl".len()].parse().unwrap();
    let last_lost = first_sequence(&dir, first_kept) - 1;
    assert!(logs_contain(SAID));
    assert!(logs_contain("ERROR"));
    assert!(logs_contain("first_sequence=5"), "the first record past the floor");
    assert!(logs_contain(&format!("last_sequence={last_lost}")));
    assert!(logs_contain("lost_from_ms=1005"));
    assert!(logs_contain(&format!("lost_to_ms={}", 1_000 + last_lost)));
}

/// A stream file that does not read fails the prune, and nothing is dropped.
#[test]
fn an_unreadable_stream_file_prunes_nothing() {
    let dir = snapped("prune-bad-stream", 1_000, 9_999);
    let before = segment_names(&dir);
    std::fs::write(dir.join(JournalExecutionStream::FILE), "{").unwrap();
    assert!(prune_closed(&dir).is_err());
    assert_eq!(segment_names(&dir), before);
}

#[test]
fn prune_never_deletes_the_latest_snap_segment_or_later() {
    // The Snapshot lands in the EARLIEST segment; every later segment holds only post-snap Cmds.
    // Nothing may be deleted — the snap's segment and all later ones are the restore base+tail.
    let dir = tmp_dir("prune-safe");
    let config = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..5 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    let snap_sequence = j
        .append_snap(9_999, &snap_engines(), "sess", 0, 0, JournalSnapshotParts::default(), 1)
        .unwrap();
    for i in 5..300 {
        j.append_command(2_000 + i as i64, &ingest(i)).unwrap(); // roll into later segments
    }
    drop(j);

    let files_before = segment_files(&dir).len();
    assert!(files_before >= 2, "must span multiple segments (got {files_before})");
    let n_before = Journal::read_all(&dir).unwrap().len();

    prune_closed(&dir).unwrap();

    assert_eq!(
        segment_files(&dir).len(),
        files_before,
        "no segment may be deleted: the snap is in the earliest segment, later segments are its tail"
    );
    let all_after = Journal::read_all(&dir).unwrap();
    assert_eq!(all_after.len(), n_before, "no records may be lost");
    assert!(
        all_after.iter().any(
            |r| matches!(r, JournalRecord::Snapshot { sequence: seq, .. } if *seq == snap_sequence)
        ),
        "the latest snap survives"
    );
}

#[test]
fn prune_with_no_snap_is_a_noop() {
    let dir = tmp_dir("prune-nosnap");
    let config = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..300 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let files_before = segment_files(&dir).len();
    let n_before = Journal::read_all(&dir).unwrap().len();

    prune_closed(&dir).unwrap();

    assert_eq!(segment_files(&dir).len(), files_before, "no Snapshot -> prune nothing");
    assert_eq!(Journal::read_all(&dir).unwrap().len(), n_before, "no records lost");
}

// ---------------------------------------------------------------------------------------------
// The boot restore's reader and its two directory moves: `read_all_located`,
// `preserve_torn_segment`, `set_aside`.
// ---------------------------------------------------------------------------------------------

// `plant` is `vike_model::test_support::file`'s (the `read_since` suite plants too), `end_of_frames`
// the crate's test-support surface (vike-core's byte surgery takes it too).

/// A torn length prefix at the end of the ONLY segment: the halt names that segment, the offset
/// of the torn frame and `in_tail`, and the records before it are all returned.
#[test]
fn read_all_located_reports_segment_offset_and_last_segment() {
    let dir = tmp_dir("located-tail");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    for i in 0..5 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let seg = segment_files(&dir).remove(0);
    let end = end_of_frames(&seg);
    plant(&seg, end, &u32::MAX.to_le_bytes());

    let (records, halt) = Journal::read_all_located(&dir).unwrap();
    assert_eq!(records.len(), 5, "every record before the torn frame reads");
    let halt = halt.expect("the torn frame halts the read");
    assert_eq!(halt.cause, crate::JournalCorruptCause::OobLen);
    assert_eq!(halt.segment, seg);
    assert_eq!(halt.segment_index, 0);
    assert_eq!(halt.offset, end, "the offset of the frame that did not read");
    assert!(halt.in_tail, "nothing was written after it");
    let (_, cause) = Journal::read_all_reporting(&dir).unwrap();
    assert_eq!(cause, Some(crate::JournalCorruptCause::OobLen), "the reporting reader agrees");
}

/// A corrupt frame in an EARLIER segment, with records written in later ones: not the tail.
#[test]
fn a_halt_before_later_records_is_not_in_the_tail() {
    let dir = tmp_dir("located-middle");
    let config = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..300 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    assert!(segment_files(&dir).len() >= 2, "the scenario must roll");
    // Flip a byte inside the FIRST frame's payload of segment 0: its checksum no longer matches.
    let segment0 = dir.join("journal-00000000.vjl");
    let at = crate::format::HEADER + 8;
    let byte = std::fs::read(&segment0).unwrap()[at];
    plant(&segment0, at, &[byte ^ 0xFF]);

    let (records, halt) = Journal::read_all_located(&dir).unwrap();
    assert!(records.is_empty(), "the halt is at the very first frame");
    let halt = halt.expect("halted");
    assert_eq!(halt.cause, crate::JournalCorruptCause::CheckMismatch);
    assert_eq!(halt.segment_index, 0);
    assert!(!halt.in_tail, "later segments hold records the halt makes unreadable");
}

/// [`snapped`]'s journal with a SECOND `Snapshot` several segments later and 40 records after it.
fn snapped_twice(tag: &str) -> Scratch {
    let dir = snapped(tag, 1_000, 9_999);
    let mut j = Journal::open(&dir, JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 })
        .unwrap();
    for i in 360..600 {
        j.append_command(20_000 + i as i64, &ingest(i)).unwrap();
    }
    j.append_snap(30_000, &snap_engines(), "sess", 9, 0, JournalSnapshotParts::default(), 0xBEEF)
        .unwrap();
    for i in 600..640 {
        j.append_command(30_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    dir
}

/// What [`Journal::read_all_located`] returns from its LAST `Snapshot` on.
fn from_last_snapshot(records: Vec<JournalRecord>) -> Vec<u64> {
    let at = records.iter().rposition(|r| matches!(r, JournalRecord::Snapshot { .. })).unwrap();
    records[at..].iter().map(record_sequence).collect()
}

/// **The boot's read is the whole read from its newest `Snapshot` on** (decision 0304): the same records, the
/// `Snapshot` first, though the journal holds two and spans many segments.
#[test]
fn the_boot_read_returns_the_records_from_the_newest_snapshot_on() {
    let dir = snapped_twice("latest-snap-read");
    let (every, halt) = Journal::read_all_located(&dir).unwrap();
    assert!(halt.is_none());
    let (read, halt) = Journal::read_from_latest_snap_located(&dir).unwrap();
    assert!(halt.is_none());
    assert!(matches!(read[0], JournalRecord::Snapshot { .. }), "the base comes first");
    assert_eq!(read.iter().map(record_sequence).collect::<Vec<_>>(), from_last_snapshot(every));
    assert_eq!(read.len(), 41, "the newest `Snapshot` and its 40 records");
}

/// A torn frame at the end of the last segment halts the boot's read exactly as it halts the whole read.
#[test]
fn the_boot_read_halts_at_a_torn_tail_as_the_whole_read_does() {
    let dir = snapped_twice("latest-snap-torn");
    let last = segment_files(&dir).into_iter().max().unwrap();
    plant(&last, end_of_frames(&last), &u32::MAX.to_le_bytes());
    let (every, every_halt) = Journal::read_all_located(&dir).unwrap();
    let (read, halt) = Journal::read_from_latest_snap_located(&dir).unwrap();
    assert_eq!(halt, every_halt);
    assert!(halt.expect("halted").in_tail);
    assert_eq!(read.iter().map(record_sequence).collect::<Vec<_>>(), from_last_snapshot(every));
}

/// **A corrupt frame BELOW the newest `Snapshot`'s segment is not read** (decision 0304): the whole read halts there
/// and calls the rest unreadable, the boot's read starts above it and does not see it.
#[test]
fn the_boot_read_does_not_read_below_the_newest_snapshot() {
    let dir = snapped_twice("latest-snap-below");
    let segment0 = dir.join("journal-00000000.vjl");
    let at = crate::format::HEADER + 8;
    let byte = std::fs::read(&segment0).unwrap()[at];
    plant(&segment0, at, &[byte ^ 0xFF]);
    let (every, halt) = Journal::read_all_located(&dir).unwrap();
    assert!(every.is_empty() && !halt.expect("halted").in_tail, "premise: the whole read halts");
    let (read, halt) = Journal::read_from_latest_snap_located(&dir).unwrap();
    assert!(halt.is_none());
    assert_eq!(read.len(), 41);
}

/// A journal with no `Snapshot` is read whole.
#[test]
fn the_boot_read_of_a_journal_with_no_snapshot_is_the_whole_read() {
    let dir = tmp_dir("latest-snap-none");
    let mut j = Journal::open(&dir, JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 })
        .unwrap();
    for i in 0..300 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let (read, halt) = Journal::read_from_latest_snap_located(&dir).unwrap();
    assert!(halt.is_none());
    assert_eq!(read.len(), 300);
}

/// The header-bounded prune drops exactly the segments below the newest `Snapshot`'s, as the decoding prune did.
#[test]
fn the_prune_drops_every_segment_below_the_newest_snapshots() {
    let dir = snapped_twice("prune-two-snaps");
    let (read, _) = Journal::read_from_latest_snap_located(&dir).unwrap();
    let base = record_sequence(&read[0]);
    prune_closed(&dir).unwrap();
    let kept = segment_names(&dir);
    let first_kept: u64 = kept[0]["journal-".len()..kept[0].len() - ".vjl".len()].parse().unwrap();
    assert!(first_sequence(&dir, first_kept) <= base, "the newest `Snapshot`'s segment is kept");
    let (after, _) = Journal::read_all_located(&dir).unwrap();
    let first_after = record_sequence(&after[0]);
    assert!(first_after <= base && after.iter().any(|r| record_sequence(r) == base));
    // the segment right above the first kept one starts past the `Snapshot`, so the first kept one holds it
    let later: Vec<u64> = kept[1..]
        .iter()
        .map(|n| n["journal-".len()..n.len() - ".vjl".len()].parse().unwrap())
        .collect();
    assert!(later.iter().all(|i| first_sequence(&dir, *i) > base));
}

/// The torn segment is COPIED (the original stays for the live core to resume), the copies are
/// named by the caller's stamp, and only the newest `keep` survive.
#[test]
fn preserve_torn_segment_copies_the_segment_and_keeps_the_newest_three() {
    let dir = tmp_dir("torn-copy");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_command(1_000, &ingest(0)).unwrap();
    drop(j);
    let seg = segment_files(&dir).remove(0);
    let end = end_of_frames(&seg);
    plant(&seg, end, &u32::MAX.to_le_bytes());
    let (_, halt) = Journal::read_all_located(&dir).unwrap();
    let halt = halt.unwrap();

    let mut made = Vec::new();
    for stamp in ["0000000000001", "0000000000002", "0000000000003", "0000000000004"] {
        made.push(Journal::preserve_torn_segment(&dir, &halt, stamp, 3).unwrap());
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
    assert_eq!(segment_files(&dir).len(), 1, "a copy under torn/ is not a segment of the journal");
    assert_eq!(Journal::read_all(&dir).unwrap().len(), 1, "and is never read back");
}

/// `set_aside` RENAMES the directory beside itself under a unique name and deletes nothing; a
/// second move on the same stamp does not collide with the first.
#[test]
fn set_aside_moves_the_directory_to_a_unique_name_and_deletes_nothing() {
    let root = tmp_dir("set-aside");
    let dir = root.join("journal");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_command(1_000, &ingest(0)).unwrap();
    drop(j);

    let first = Journal::set_aside(&dir, "0000000000007").unwrap();
    assert_eq!(first, root.join("journal.broken-0000000000007"));
    assert!(!dir.exists(), "the journal directory is free for a fresh one");
    assert_eq!(Journal::read_all(&first).unwrap().len(), 1, "every record moved with it");

    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_command(2_000, &ingest(1)).unwrap();
    drop(j);
    let second = Journal::set_aside(&dir, "0000000000007").unwrap();
    assert_eq!(second, root.join("journal.broken-0000000000007-1"), "a taken name is not reused");
    assert!(first.exists() && second.exists());
}

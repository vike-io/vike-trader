//! Tests of the offline reader half: `read_since`'s strict cutoff, its equality with `read_all`
//! filtered (segments left out, segments read in steps, torn and corrupt segments), and the
//! checkpoint file.

use std::time::Duration;

use tracing_test::traced_test;

use super::*;
use crate::JournalFileConfig;
use crate::end_of_frames;
use crate::testutil::*;
use vike_model::test_support::file::plant;

/// Each record's serde bytes: `JournalRecord` has no `PartialEq`, and two reads agree exactly when
/// these do.
fn bytes(records: &[JournalRecord]) -> Vec<Vec<u8>> {
    records.iter().map(|r| serde_json::to_vec(r).unwrap()).collect()
}

/// THE ORACLE: `read_all` then the filter `read_since` used to be.
fn filtered(dir: &Path, after: u64) -> Vec<Vec<u8>> {
    let mut all = Journal::read_all(dir).unwrap();
    all.retain(|r| record_sequence(r) > after);
    bytes(&all)
}

/// A journal of `n` cancels over 4 KiB segments: several segments, every one but the last CLOSED.
fn rolled(tag: &str, n: u64) -> Scratch {
    let dir = tmp_dir(tag);
    let config = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..n {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    assert!(segment_indices(&dir).unwrap().len() >= 3, "the scenario must span several segments");
    dir
}

/// Segment `index`'s header `first_sequence`: the seq of its first record.
fn first_sequence(dir: &Path, index: u64) -> u64 {
    let b = std::fs::read(segment_path(dir, index)).unwrap();
    u64::from_le_bytes(b[8..16].try_into().unwrap())
}

/// The highest segment index (the one the writer would resume).
fn last_index(dir: &Path) -> u64 {
    *segment_indices(dir).unwrap().last().unwrap()
}

/// Corrupt the LAST frame of segment `index` (one payload byte flipped: a check mismatch AFTER every
/// other record of the segment) and set its mtime to `stamp`. Returns that frame's offset.
fn corrupt_last_frame(dir: &Path, index: u64, stamp: SystemTime) -> usize {
    let seg = segment_path(dir, index);
    let at = *frame_starts(&seg).last().unwrap();
    let byte = std::fs::read(&seg).unwrap()[at + 8];
    plant(&seg, at + 8, &[byte ^ 0xFF]);
    std::fs::File::options().write(true).open(&seg).unwrap().set_modified(stamp).unwrap();
    at
}

fn mtime(dir: &Path, index: u64) -> SystemTime {
    std::fs::metadata(segment_path(dir, index)).unwrap().modified().unwrap()
}

/// The cutoff inside the LAST segment: the closed segments are left out once remembered, and the
/// answer is `read_all` filtered whatever the read step and whether the memo is cold or warm.
#[test]
fn read_since_equals_read_all_filtered_with_the_cutoff_in_the_last_segment() {
    // Where the segments roll moves with a record's byte size (its key names), so the scenario takes
    // the first record count whose last segment holds a record past the cutoff.
    let dir = (300..340)
        .map(|n| rolled(&format!("since-last-{n}"), n))
        .find(|d| !filtered(d, first_sequence(d, last_index(d)) + 2).is_empty())
        .expect("some count from 300 leaves three records in the last segment");
    let after = first_sequence(&dir, last_index(&dir)) + 2;
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
    assert!(!memo.0.contains_key(&last_index(&dir)));
    assert_eq!(bytes(&read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap()), want, "warm");

    // The public entry point, twice: the second call runs on the memo the first left behind.
    assert_eq!(bytes(&read_since(&dir, after).unwrap()), want);
    assert_eq!(bytes(&read_since(&dir, after).unwrap()), want);
}

/// `after_sequence == 0`: everything but seq 0, cold and warm.
#[test]
fn read_since_zero_returns_every_record_but_sequence_zero() {
    let dir = rolled("since-zero", 300);
    let all = Journal::read_all(&dir).unwrap();
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
    let head = Journal::read_all(&dir).unwrap().iter().map(record_sequence).max().unwrap();
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
        let last = segment_path(&dir, last_index(&dir));
        let end = end_of_frames(&last);
        assert!(end + torn.len() <= 4 * 1024, "room for the torn frame");
        plant(&last, end, torn);
        for after in [0, first_sequence(&dir, last_index(&dir))] {
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
    let after = first_sequence(&dir, 1) - 1; // segment 0's last seq: all of segment 0 is <= after
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
    let after = first_sequence(&dir, 1) - 1;
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
    let after = first_sequence(&dir, 1) - 1;
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
    let after = first_sequence(&dir, last_index(&dir));
    let mut memo = SegmentMemo::default();
    read_since_with(&dir, after, &mut memo, READ_CHUNK).unwrap();
    assert!(memo.0.contains_key(&0) && memo.0.contains_key(&1));

    let kept =
        frame_starts(&segment_path(&dir, 0)).len() + frame_starts(&segment_path(&dir, 1)).len() - 1;
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
    let config = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 4 };
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..10 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap(); // sequences 0..=9
    }
    drop(j);

    // `after_sequence == 0` ⇒ everything strictly after the null cursor. This journal's first
    // record carries seq 0, and the lower bound is STRICT (`seq > after_sequence`, the exact resume
    // semantic a reader needs: a cursor is the last seq CONSUMED, exclusive), so
    // seq 0 is excluded and sequences 1..=9 (9 records) are returned in seq order.
    let all = read_since(&dir, 0).unwrap();
    let all_sequences: Vec<u64> = all.iter().map(record_sequence).collect();
    assert_eq!(all_sequences, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);

    // strictly-greater cutoff: seq 4 excluded, sequences 5..=9 returned in order.
    let tail = read_since(&dir, 4).unwrap();
    let sequences: Vec<u64> = tail.iter().map(record_sequence).collect();
    assert_eq!(sequences, vec![5, 6, 7, 8, 9]);

    // a cutoff at/after the last seq ⇒ empty.
    assert!(read_since(&dir, 9).unwrap().is_empty());
    assert!(read_since(&dir, 100).unwrap().is_empty());
}

fn sequences(records: &[JournalRecord]) -> Vec<u64> {
    records.iter().map(record_sequence).collect()
}

/// Every record `read_window` returns past `after` under `mark`, `limit` at a time, each call
/// resuming after the last record the one before returned (the publisher's catch-up).
fn drain(
    dir: &Path,
    after: u64,
    mark: JournalDurableMark,
    limit: usize,
    memo: &mut SegmentMemo,
    step: usize,
) -> Vec<Vec<u8>> {
    let (mut after, mut all) = (after, Vec::new());
    loop {
        let got = read_window(dir, Some(after), Some(mark), limit, memo, step).unwrap();
        assert!(got.len() <= limit, "at most `limit` a call");
        let Some(last) = got.last() else { return bytes(&all) };
        after = record_sequence(last);
        all.extend(got);
    }
}

/// Live, against the real sync thread: a record the working file holds and no sync has reached is
/// not returned; once the publisher's `request_sync` lands it is, `limit` at a time.
#[test]
fn read_durable_since_stops_at_the_synced_mark() {
    let dir = tmp_dir("durable-live");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let watch = j.durable_watch();
    for i in 0..5 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    j.flush().unwrap();
    for i in 5..8 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    j.write_fence().wait().unwrap();
    assert_eq!(sequences(&read_since(&dir, 0).unwrap()), (1..8).collect::<Vec<_>>());
    let durable = read_durable_since(&dir, Some(0), &watch, 512).unwrap();
    assert_eq!(sequences(&durable), [1, 2, 3, 4], "nothing past the synced mark");
    let from_the_first = read_durable_since(&dir, None, &watch, 512).unwrap();
    assert_eq!(sequences(&from_the_first), [0, 1, 2, 3, 4], "`None`: sequence 0 too");

    watch.request_sync();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while watch.durable_mark().offset < j.cursor {
        assert!(std::time::Instant::now() < deadline, "the sync never landed");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(sequences(&read_durable_since(&dir, Some(0), &watch, 3).unwrap()), [1, 2, 3]);
    assert_eq!(sequences(&read_durable_since(&dir, Some(3), &watch, 3).unwrap()), [4, 5, 6]);
    assert_eq!(sequences(&read_durable_since(&dir, Some(6), &watch, 3).unwrap()), [7]);
    assert!(read_durable_since(&dir, Some(7), &watch, 3).unwrap().is_empty());
    assert!(
        read_durable_since(&dir, None, &watch, 0).unwrap().is_empty(),
        "a zero limit reads nothing"
    );
}

/// Sequence 0 is streamed: a fresh journal whose FIRST record is a boot's `ExecutionReportsUnverified` (the
/// directory a broken journal was moved out of) hands it out to a cursor read off a fresh stream file
/// (`acknowledged_sequence` `None`), where "since 0" would have dropped it.
#[test]
fn a_fresh_journals_first_record_is_streamed_to_a_cursor_with_nothing_acknowledged() {
    let dir = tmp_dir("durable-first");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let watch = j.durable_watch();
    let stream =
        JournalExecutionStream::load_or_create(&dir, JournalExecutionStream::mint_journal_id)
            .unwrap();
    let first = j
        .append_execution_reports_unverified(
            1_000,
            &stream.journal_id,
            (0, 0),
            (None, None),
            &[],
            "the journal was moved aside",
        )
        .unwrap();
    assert_eq!(first, 0, "a fresh journal's first record");
    j.append_command(1_001, &ingest(1)).unwrap();
    j.flush().unwrap();

    assert_eq!(stream.acknowledged_sequence, None);
    let durable = read_durable_since(&dir, stream.acknowledged_sequence, &watch, 512).unwrap();
    assert!(
        matches!(durable.first(), Some(JournalRecord::ExecutionReportsUnverified { sequence: 0, reason, .. })
            if reason == "the journal was moved aside"),
        "{durable:?}"
    );
    assert_eq!(sequences(&durable), [0, 1]);
    assert_eq!(sequences(&read_durable_since(&dir, Some(0), &watch, 512).unwrap()), [1]);
    assert_eq!(sequences(&read_since(&dir, 0).unwrap()), [1], "read_since keeps its contract");
}

/// Within the mark the answer is `read_since`'s, whatever the read step, the limit and whether the
/// memo (closed segments and the resume point) is cold or warm: a mark at a frame boundary keeps
/// every frame before it, a mark inside a frame leaves that frame out WITHOUT a warning, and a mark
/// at the start of a segment leaves that segment and every later one out.
#[traced_test]
#[test]
fn read_durable_since_equals_read_since_within_the_mark() {
    let dir = rolled("durable-window", 300);
    let last = last_index(&dir);
    let starts = frame_starts(&segment_path(&dir, 1));
    let k = starts.len() / 2;
    let frame_k = first_sequence(&dir, 1) + k as u64;
    let at = |segment_index: u64, offset: usize| JournalDurableMark { segment_index, offset };
    // (the mark, the first sequence it leaves out)
    let cases = [
        (at(last, end_of_frames(&segment_path(&dir, last))), u64::MAX),
        (at(1, starts[k]), frame_k),
        (at(1, starts[k] + 5), frame_k), // not even frame k's length and check are inside
        (at(1, starts[k] + 12), frame_k), // its payload runs past the mark (an out-of-bounds length)
        (at(1, 0), first_sequence(&dir, 1)),
    ];
    for (mark, first_past) in cases {
        for after in [0, first_sequence(&dir, 1) - 3] {
            let mut want = Journal::read_all(&dir).unwrap();
            want.retain(|r| (after + 1..first_past).contains(&record_sequence(r)));
            let want = bytes(&want);
            let mut warm = SegmentMemo::default();
            for step in [1, 7, 4096, READ_CHUNK] {
                for limit in [1, 7, usize::MAX] {
                    let cold = drain(&dir, after, mark, limit, &mut SegmentMemo::default(), step);
                    assert_eq!(
                        cold, want,
                        "{mark:?}, after {after}, step {step}, limit {limit}, cold"
                    );
                    let again = drain(&dir, after, mark, limit, &mut warm, step);
                    assert_eq!(
                        again, want,
                        "{mark:?}, after {after}, step {step}, limit {limit}, warm"
                    );
                }
            }
        }
    }
    assert!(!logs_contain("journal read halted"), "a frame past the mark is no fault");
}

/// The resume point is USED: a corruption planted behind it (in a frame already read, its head kept)
/// is not walked again, while a cold memo halts at it. So a poll reads only what is new.
#[test]
fn a_walk_resumes_after_the_frames_it_already_read() {
    let dir = tmp_dir("durable-resume");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    for i in 0..10 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let seg = segment_path(&dir, 0);
    let mark = |dir: &Path| JournalDurableMark {
        segment_index: 0,
        offset: end_of_frames(&segment_path(dir, 0)),
    };
    let mut memo = SegmentMemo::default();
    let first =
        read_window(&dir, Some(0), Some(mark(&dir)), usize::MAX, &mut memo, READ_CHUNK).unwrap();
    assert_eq!(sequences(&first), (1..10).collect::<Vec<_>>());

    let mut j = Journal::open(&dir, small_config()).unwrap();
    for i in 10..15 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let at = frame_starts(&seg)[2];
    let byte = std::fs::read(&seg).unwrap()[at + 8];
    plant(&seg, at + 8, &[byte ^ 0xFF]);

    let resumed =
        read_window(&dir, Some(9), Some(mark(&dir)), usize::MAX, &mut memo, READ_CHUNK).unwrap();
    assert_eq!(sequences(&resumed), (10..15).collect::<Vec<_>>(), "only the new frames are walked");
    let cold = read_window(
        &dir,
        Some(9),
        Some(mark(&dir)),
        usize::MAX,
        &mut SegmentMemo::default(),
        READ_CHUNK,
    )
    .unwrap();
    assert!(cold.is_empty(), "a cold walk halts at the planted frame");
}

/// A resume point whose last frame no longer reads the same (a journal replaced under the same
/// path) is not used: the walk starts from the first frame and the answer is the new journal's.
#[test]
fn a_resume_point_that_no_longer_reads_the_same_is_not_used() {
    let dir = tmp_dir("durable-replaced");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    for i in 0..10 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let mark = |dir: &Path| JournalDurableMark {
        segment_index: 0,
        offset: end_of_frames(&segment_path(dir, 0)),
    };
    let mut memo = SegmentMemo::default();
    read_window(&dir, Some(4), Some(mark(&dir)), usize::MAX, &mut memo, READ_CHUNK).unwrap();
    assert!(memo.1.is_some(), "the walk left a resume point");

    for f in segment_files(&dir) {
        std::fs::remove_file(f).unwrap();
    }
    let mut j = Journal::open(&dir, small_config()).unwrap();
    for i in 0..12 {
        j.append_command(2_000 + i as i64, &fat_ingest(i)).unwrap();
    }
    drop(j);
    let got =
        read_window(&dir, Some(9), Some(mark(&dir)), usize::MAX, &mut memo, READ_CHUNK).unwrap();
    assert_eq!(bytes(&got), filtered(&dir, 9), "the new journal's records past 9");
}

/// Poll `dir` as the execution stream does, on ONE memo, `limit` records a call from `cursor` until a call
/// returns none, every call checked against a COLD one (a fresh memo: no resume point, the walk from the
/// first frame); what it returns is appended to `streamed`.
fn poll(
    dir: &Path,
    cursor: &mut Option<u64>,
    memo: &mut SegmentMemo,
    limit: usize,
    streamed: &mut Vec<JournalRecord>,
) {
    // Every segment whole: the durable range of a journal whose every byte is synced.
    let all = JournalDurableMark { segment_index: u64::MAX, offset: usize::MAX };
    loop {
        let got = read_window(dir, *cursor, Some(all), limit, memo, 7).unwrap();
        let cold =
            read_window(dir, *cursor, Some(all), limit, &mut SegmentMemo::default(), READ_CHUNK)
                .unwrap();
        assert_eq!(bytes(&got), bytes(&cold), "after {cursor:?}: a resumed poll is a cold one");
        let Some(last) = got.last() else { return };
        *cursor = Some(record_sequence(last));
        streamed.extend(got);
    }
}

/// **A poll after a poll streams the journal record for record, across rolls and a torn tail met
/// mid-poll.** A journal written in rounds over 4 KiB segments; between two rounds the live segment
/// ends in a frame half there (its length and check written, its payload not: what a reader racing
/// the write thread's `pwrite` meets), which a poll stops before, silently; the next round reopens
/// the journal, which ends that segment there (`close_dirty_tail`) and goes on in a new one. Each
/// poll resumes where the last stopped and answers what a cold walk does; together they are
/// `read_all`'s records, each once, in order.
#[traced_test]
#[test]
fn polls_stream_the_journal_once_across_rolls_and_a_torn_tail() {
    let dir = tmp_dir("durable-stream");
    let config = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut next = 0u64;
    let mut write = |n: u64| {
        let mut j = Journal::open(&dir, config.clone()).unwrap();
        for _ in 0..n {
            j.append_command(1_000 + next as i64, &ingest(next)).unwrap();
            next += 1;
        }
    };
    let (mut cursor, mut memo, mut streamed) = (None, SegmentMemo::default(), Vec::new());
    write(150);
    poll(&dir, &mut cursor, &mut memo, 7, &mut streamed);
    write(150);
    // The torn frame below needs room at the live segment's end: one record more (a roll, at worst)
    // until it has it.
    while end_of_frames(&segment_path(&dir, last_index(&dir))) + 8 + 16 > 4 * 1024 {
        write(1);
    }
    poll(&dir, &mut cursor, &mut memo, 1, &mut streamed);
    let rolled = last_index(&dir);
    assert!(rolled >= 2, "the rounds rolled segments");

    let live = segment_path(&dir, rolled);
    // Its length and check are written, three bytes of its 16-byte payload: the check fails.
    plant(&live, end_of_frames(&live), &[16, 0, 0, 0, 0xEF, 0xBE, 0xAD, 0xDE, 1, 2, 3]);
    let before = streamed.len();
    poll(&dir, &mut cursor, &mut memo, 512, &mut streamed);
    assert_eq!(streamed.len(), before, "nothing past the last whole frame");
    assert!(!logs_contain("journal read halted"), "a frame in flight is no fault");

    write(50);
    assert!(last_index(&dir) > rolled, "the reopen went on in a new segment");
    poll(&dir, &mut cursor, &mut memo, 512, &mut streamed);
    let all = Journal::read_all(&dir).unwrap();
    assert_eq!(all.len() as u64, next, "the journal holds every record written");
    assert_eq!(bytes(&streamed), bytes(&all), "every record once, in order");
}

/// **A poll reads what is new, never the segment again.** Once a walk has read the live segment, each
/// later poll reads the header, the 8-byte head that checks its resume point and the bytes written
/// since, however long the segment has grown (it once copied the segment from byte 0 every poll).
#[test]
fn a_poll_reads_only_the_bytes_written_since_the_last() {
    let dir = tmp_dir("durable-cost");
    let mut j =
        Journal::open(&dir, JournalFileConfig { segment_bytes: 1 << 20, flush_every: 64 }).unwrap();
    let mark =
        || JournalDurableMark { segment_index: 0, offset: end_of_frames(&segment_path(&dir, 0)) };
    let mut memo = SegmentMemo::default();
    let mut read = |after: Option<u64>| {
        let before = BYTES_READ.with(|n| n.get());
        let got =
            read_window(&dir, after, Some(mark()), usize::MAX, &mut memo, READ_CHUNK).unwrap();
        (got, BYTES_READ.with(|n| n.get()) - before)
    };
    for i in 0..2_000 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    j.flush().unwrap();
    let (first, cold) = read(None);
    let written = mark().offset as u64;
    assert_eq!(first.len(), 2_000);
    assert!(cold >= written, "the first walk reads the segment: {cold} B of {written} B");

    let mut after = record_sequence(first.last().unwrap());
    for round in 0..5u64 {
        let from = mark().offset;
        for i in 0..3 {
            j.append_command(5_000 + i, &ingest(2_000 + round * 3 + i as u64)).unwrap();
        }
        j.flush().unwrap();
        let new = (mark().offset - from) as u64;
        let (got, cost) = read(Some(after));
        assert_eq!(sequences(&got), (after + 1..=after + 3).collect::<Vec<_>>(), "round {round}");
        assert_eq!(cost, (HEADER + 8) as u64 + new, "round {round}: {new} B new of {written}+ B");
        after += 3;
    }
    let (nothing, idle) = read(Some(after));
    assert!(nothing.is_empty());
    assert_eq!(idle, (HEADER + 8) as u64, "nothing new: the header and the head");
}

/// **A journal moved aside is read a page at a time** (the execution stream's drain of a predecessor): every page
/// holds at most `limit` records, the pages together are `read_all_located`'s records, and the halt is
/// `read_all_located`'s, reported on every page that ends at it and on no full page.
#[test]
fn read_closed_since_pages_a_moved_aside_journal_and_reports_its_halt() {
    let dir = rolled("closed-pages", 300);
    corrupt_last_frame(&dir, 1, mtime(&dir, 1));
    let (want, want_halt) = Journal::read_all_located(&dir).unwrap();
    let want_halt = want_halt.expect("the planted frame halts the read");
    assert!(!want.is_empty() && !want_halt.in_tail, "a broken journal, records before the halt");
    for limit in [1, 7, 512] {
        let (mut after, mut got) = (None, Vec::new());
        loop {
            let (page, halt) = read_closed_since(&dir, after, limit).unwrap();
            assert!(page.len() <= limit, "at most `limit` a page");
            assert_eq!(halt.is_some(), page.len() < limit, "the halt ends a page short: {halt:?}");
            if let Some(halt) = halt {
                assert_eq!(halt, want_halt);
            }
            let Some(last) = page.last() else { break };
            after = Some(record_sequence(last));
            got.extend(page);
        }
        assert_eq!(bytes(&got), bytes(&want), "limit {limit}");
    }
    let (clean, halt) = read_closed_since(&rolled("closed-clean", 300), Some(5), 4).unwrap();
    assert_eq!((sequences(&clean), halt), (vec![6, 7, 8, 9], None), "a full page halts nothing");
}

/// The stream file's first start mints its id once; every later load reads it back, floor and
/// predecessors included, and the bytes are the plan's shape: the floor fields are `null` until the
/// first acknowledgement.
#[test]
fn the_execution_stream_file_is_created_once_and_read_back() {
    let dir = tmp_dir("stream-file");
    let id = "0123456789abcdef0123456789abcdef";
    let created = JournalExecutionStream::load_or_create(&dir, || id.to_string()).unwrap();
    assert_eq!(created.journal_id, id);
    assert_eq!((created.acknowledged_sequence, created.acknowledged_ms), (None, None));
    let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join(JournalExecutionStream::FILE)).unwrap())
            .unwrap();
    let shape = serde_json::json!({
        "journal_id": id, "acknowledged_sequence": null, "acknowledged_ms": null, "predecessors": []
    });
    assert_eq!(raw, shape);

    let again =
        JournalExecutionStream::load_or_create(&dir, || panic!("an existing file mints nothing"))
            .unwrap();
    assert_eq!(again, created);

    let mut acknowledged = again;
    acknowledged.acknowledged_sequence = Some(42);
    acknowledged.acknowledged_ms = Some(1_700_000_000_000);
    acknowledged.predecessors.push(JournalExecutionStreamPredecessor {
        journal_id: "fedcba9876543210fedcba9876543210".into(),
        path: dir.join("journal.broken-1"),
        acknowledged_sequence: Some(7),
        done: false,
    });
    acknowledged.store(&dir).unwrap();
    // A replace over the existing file: a plain `rename` onto it fails on Windows.
    acknowledged.acknowledged_sequence = Some(43);
    acknowledged.store(&dir).unwrap();
    assert_eq!(JournalExecutionStream::load(&dir).unwrap(), Some(acknowledged));
}

/// No file, or no directory at all: `None`. A file that does not read: an error, never a fresh
/// identity.
#[test]
fn an_absent_stream_file_is_none_and_an_unreadable_one_is_an_error() {
    assert_eq!(JournalExecutionStream::load(&Scratch::reserved("stream-absent")).unwrap(), None);
    let dir = tmp_dir("stream-empty");
    assert_eq!(JournalExecutionStream::load(&dir).unwrap(), None);
    std::fs::write(dir.join(JournalExecutionStream::FILE), b"not json\n").unwrap();
    assert_eq!(JournalExecutionStream::load(&dir).unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert!(JournalExecutionStream::load_or_create(&dir, || unreachable!()).is_err());
}

/// The one journal-identity mint: 32 lowercase hex digits every time (the stream's `validate_journal_id` shape), a
/// fresh one each call, here and on another thread.
#[test]
fn the_journal_id_mint_is_32_lowercase_hex_and_fresh_each_call() {
    let mut ids: Vec<String> = (0..64).map(|_| JournalExecutionStream::mint_journal_id()).collect();
    ids.push(std::thread::spawn(JournalExecutionStream::mint_journal_id).join().unwrap());
    for id in &ids {
        assert_eq!(id.len(), 32, "{id}");
        assert!(id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{id}");
    }
    let distinct: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(distinct.len(), ids.len(), "every mint is fresh: {ids:?}");
}

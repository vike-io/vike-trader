//! **The datahub's cursor, checked at every open** (package 2's execution stream, plan C.3): the publisher keeps no
//! resume point of its own and starts from the datahub's `stored_through`, after two checks.
//!
//! - A datahub that follows a COPY of the journal (its cursor past the journal's head, or the journal's record at its
//!   cursor recorded at another time: a directory restored from a copy carries the id and diverges) makes the journal
//!   take a NEW identity, and the journal is sent again from its first record.
//! - A cursor the journal has pruned past is sent as a `Gap` (the executions in between are lost from the datahub, and
//!   it says so), and the stream goes on from the journal's oldest record.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use vike_journal::{Journal, JournalExecutionStream, JournalSnapshotParts, record_sequence};

use crate::support::{
    FakeDatahub, FakeDatahubMode, fill_report, open_journal, start_execution_stream,
};

/// A stream file naming `dir`'s journal by a fresh id, nothing acknowledged: the id it returns.
fn identify(dir: &std::path::Path) -> String {
    let journal_id = JournalExecutionStream::mint_journal_id();
    JournalExecutionStream {
        journal_id: journal_id.clone(),
        acknowledged_sequence: None,
        acknowledged_ms: None,
        predecessors: Vec::new(),
    }
    .store(dir)
    .expect("the stream file");
    journal_id
}

#[test]
fn a_journal_restored_from_a_copy_takes_a_new_identity() {
    // the datahub's cursor names a record this journal recorded at another time; or one past its head
    for (stored_through, recorded_ms) in [(2, Some(999)), (50, None)] {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = open_journal(dir.path());
        for i in 0..5 {
            journal
                .append_execution_report(100 + i, &fill_report("op1", &format!("t{i}")))
                .expect("append");
        }
        journal.flush().expect("flush");
        let copied = identify(dir.path());
        let datahub = FakeDatahub::start(None, FakeDatahubMode::Serve);
        datahub.set_cursor(&copied, stored_through, recorded_ms);
        let stop = Arc::new(AtomicBool::new(false));
        let publisher = start_execution_stream(
            dir.path(),
            journal.durable_watch(),
            datahub.addr,
            None,
            Arc::clone(&stop),
        );
        datahub.wait_until("the journal is sent again under a new identity", |d| {
            d.opens().iter().any(|(id, _)| *id != copied && d.stream(id).records.len() == 5)
        });
        stop.store(true, Ordering::Relaxed);
        publisher.join();
        let opens: Vec<String> = datahub.opens().into_iter().map(|(id, _)| id).collect();
        let file = JournalExecutionStream::load(dir.path()).expect("reads").expect("written");
        assert_eq!(
            opens,
            [copied.clone(), file.journal_id.clone()],
            "the copy's id, then the new one"
        );
        assert_ne!(file.journal_id, copied);
        assert_eq!(
            datahub.stream(&file.journal_id).records.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4],
            "sent again from the first record"
        );
        assert!(datahub.stream(&copied).records.is_empty(), "nothing went to the copy's cursor");
    }
}

#[test]
fn a_gap_is_sent_when_the_journal_no_longer_holds_what_the_datahub_lacks() {
    let dir = tempfile::tempdir().expect("temp");
    {
        // small segments: twenty reports span several, then a `Snapshot` and two more reports
        let mut journal = open_journal(dir.path());
        for i in 0..20 {
            journal
                .append_execution_report(100 + i, &fill_report("op1", &format!("t{i}")))
                .expect("append");
        }
        journal
            .append_snap(120, &[], "", 0, 0, JournalSnapshotParts::default(), 0)
            .expect("snapshot");
        for i in 21..23 {
            journal
                .append_execution_report(100 + i, &fill_report("op1", &format!("t{i}")))
                .expect("append");
        }
    }
    // no stream file yet: the snapshot-only archive pass moves the segments before the `Snapshot`'s
    let everything = vike_journal::JournalArchivePolicy {
        keep_ms: 0,
        max_bytes: u64::MAX,
        max_age_ms: i64::MAX,
        at_limit: vike_model::JournalArchiveFull::Keep,
    };
    let closed = vike_journal::JournalDurableMark { segment_index: u64::MAX, offset: usize::MAX };
    Journal::prune_before_latest_snap(dir.path(), &everything, i64::MAX, closed).expect("prune");
    let oldest = record_sequence(&Journal::read_all(dir.path()).expect("reads")[0]);
    assert!(oldest > 2, "the prune left records 0 to 2 behind: oldest {oldest}");
    let journal_id = identify(dir.path());
    let journal = open_journal(dir.path());
    let datahub = FakeDatahub::start(None, FakeDatahubMode::Serve);
    datahub.set_cursor(&journal_id, 1, Some(101));
    let stop = Arc::new(AtomicBool::new(false));
    let publisher = start_execution_stream(
        dir.path(),
        journal.durable_watch(),
        datahub.addr,
        None,
        Arc::clone(&stop),
    );
    datahub
        .wait_until("the last report lands", |d| d.stream(&journal_id).stored_through == Some(22));
    stop.store(true, Ordering::Relaxed);
    publisher.join();
    let stream = datahub.stream(&journal_id);
    assert_eq!(
        stream.gaps,
        [(1, oldest)],
        "the gap: after the datahub's cursor, up to the oldest record"
    );
    let first = &stream.records[0];
    assert_eq!(
        (first.sequence, first.previous_sequence),
        (oldest, Some(1)),
        "the stream chains on from the gap"
    );
    let expected: Vec<u64> = (oldest..23).filter(|s| *s != 20).collect();
    assert_eq!(
        stream.records.iter().map(|r| r.sequence).collect::<Vec<_>>(),
        expected,
        "every report kept"
    );
}

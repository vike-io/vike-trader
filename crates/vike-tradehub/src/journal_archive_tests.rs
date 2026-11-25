//! The archive thread's wiring: its FIRST pass runs as it starts, beside an open journal, and it ends at the stop.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_journal::{Journal, JournalArchivePolicy, JournalFileConfig, JournalSnapshotParts};
use vike_model::JournalArchiveFull;

use super::JournalArchiver;

#[test]
fn the_first_pass_archives_beside_the_open_journal_and_the_thread_ends_at_the_stop() {
    let scratch = tempfile::tempdir().expect("a journal dir");
    let dir = scratch.path().join("journal");
    let mut journal =
        Journal::open(&dir, JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 })
            .expect("open");
    let command = |n: u64| {
        vike_execution::Ingest::Command(vike_execution::Command::Order(
            vike_execution::OrderIntent::Cancel(format!("c{n}")),
        ))
    };
    for n in 0..300 {
        journal.append_command(n as i64, &command(n)).expect("append");
    }
    journal.append_snap(300, &[], "", 0, 0, JournalSnapshotParts::default(), 0).expect("snapshot");
    for n in 300..400 {
        journal.append_command(n as i64, &command(n)).expect("append");
    }
    journal.flush().expect("flush");
    // the sync thread seals a segment the write thread left on its own time: wait for the mark to reach the live one
    let live = std::fs::read_dir(&dir)
        .expect("the journal dir")
        .filter_map(|e| {
            let name = e.ok()?.file_name().to_string_lossy().into_owned();
            name.strip_prefix("journal-")?.strip_suffix(".vjl")?.parse::<u64>().ok()
        })
        .max()
        .expect("segments");
    let watch = journal.durable_watch();
    let until = Instant::now() + Duration::from_secs(10);
    while watch.durable_mark().segment_index < live {
        assert!(Instant::now() < until, "the seals never landed");
        std::thread::sleep(Duration::from_millis(1));
    }

    let policy = JournalArchivePolicy {
        keep_ms: 0,
        max_bytes: u64::MAX,
        max_age_ms: i64::MAX,
        at_limit: JournalArchiveFull::Keep,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let archiver =
        JournalArchiver::spawn(dir.clone(), watch, policy, Arc::clone(&stop)).expect("spawn");
    let archive = dir.join(vike_journal::ARCHIVE_DIR);
    let until = Instant::now() + Duration::from_secs(10);
    let compressed = || {
        std::fs::read_dir(&archive).is_ok_and(|entries| {
            entries
                .filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().ends_with(".vjl.gz"))
        })
    };
    while !compressed() {
        assert!(Instant::now() < until, "the first pass archived nothing within 10 s");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!dir.join("journal-00000000.vjl").exists(), "segment 0 left the live directory");

    stop.store(true, Ordering::Relaxed);
    let joined = Instant::now();
    archiver.join();
    assert!(
        joined.elapsed() < Duration::from_secs(5),
        "the thread ends within a slice of the stop"
    );
    journal.append_command(500, &command(500)).expect("the journal goes on");
}

//! The cadence `Snapshot` encoded on the write thread (decision 0279, amended 2026-11-19): the slot's hand-over
//! (one encode, whoever runs it; a panicking encode fails the settle, never the thread), and the journal's deferred
//! `Snapshot` against the synchronous one: the same bytes at the same place, framed by the next append, a flush,
//! the idle settle or the stop, across a roll too. And the stop of a write thread whose hand-offs keep failing.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::{
    FENCE_BOUND, SLOW_RUN_QUIET, SLOW_STEP, STOP_RETRY_FOR, SnapshotSlot, WRITE_TICK, note_step_at,
};
use crate::testutil::*;
use crate::{
    Journal, JournalFileConfig, JournalRecord, JournalSnapshotBooks,
    JournalSnapshotMountAttribution, fnv1a32,
};

/// **A job the write thread has not taken is encoded by the core that settles it**, once: the core never waits
/// for a write thread that is busy elsewhere (a long `pwrite`), it does what every `Snapshot` cost before.
#[test]
fn a_job_the_write_thread_has_not_taken_is_encoded_by_the_settle() {
    let slot = SnapshotSlot::default();
    slot.hand(Box::new(|| vec![1, 2, 3]));
    assert_eq!(slot.settle(), Some((fnv1a32(&[1, 2, 3]), vec![1, 2, 3])));
    assert!(!slot.encode_queued(), "nothing is left for the write thread");
}

/// **Raced, the job runs exactly once and the settle gets its payload**, whichever side moved first: the write
/// thread (the settle then waits for it) or the settle (the write thread then finds nothing).
#[test]
fn a_raced_job_runs_once_and_the_settle_gets_its_payload() {
    for i in 0..2_000u32 {
        let slot = Arc::new(SnapshotSlot::default());
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        slot.hand(Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            i.to_le_bytes().repeat(64)
        }));
        let writer = {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || slot.encode_queued())
        };
        let (check, payload) = slot.settle().expect("encoded");
        let _ = writer.join().expect("the write thread side never panics");
        assert_eq!(payload, i.to_le_bytes().repeat(64), "rep {i}");
        assert_eq!(check, fnv1a32(&payload), "rep {i}");
        assert_eq!(runs.load(Ordering::SeqCst), 1, "rep {i}: one encode, whoever ran it");
        assert!(!slot.encode_queued(), "rep {i}: the slot is empty again");
    }
}

/// **An encode that panics fails the settle, on the core thread, and the write thread lives on**; so does one the
/// settle runs itself.
#[test]
fn a_panicking_encode_fails_the_settle_and_not_the_write_thread() {
    let slot = SnapshotSlot::default();
    slot.hand(Box::new(|| -> Vec<u8> { panic!("an unencodable Snapshot") }));
    assert!(slot.encode_queued(), "the write thread took it");
    assert_eq!(slot.settle(), None);
    slot.hand(Box::new(|| -> Vec<u8> { panic!("an unencodable Snapshot") }));
    assert_eq!(
        slot.settle(),
        None,
        "reclaimed and encoded by the settle, which fails the same way"
    );
    slot.hand(Box::new(|| vec![7]));
    assert_eq!(slot.settle(), Some((fnv1a32(&[7]), vec![7])), "and the slot works on");
}

/// The books a `Snapshot` of these tests carries: a mount row and the late-retry memory, so its bytes are not
/// the empty record's.
fn books() -> JournalSnapshotBooks {
    JournalSnapshotBooks {
        mount_attribution: vec![JournalSnapshotMountAttribution {
            mount_id: "m".into(),
            size: 1.5,
            ..Default::default()
        }],
        recent_order_client_ids: vec![("c1".into(), 5)],
        ..Default::default()
    }
}

/// The written bytes of segment `seg`: its header and frames, up to its first zero length.
fn written(seg: &std::path::Path) -> Vec<u8> {
    let end = crate::end_of_frames(seg);
    std::fs::read(seg).unwrap()[..end].to_vec()
}

/// **A deferred `Snapshot` writes the bytes the synchronous one writes, at its place**: it holds the sequence it
/// was handed while the write thread encodes it, the record appended after it follows it, and its hash is the
/// engines' `state_hash`. So a restore reads the same journal either way.
#[test]
fn a_deferred_snapshot_writes_the_synchronous_bytes_at_its_place() {
    let engines = snap_engines();
    let hash = vike_execution::state_hash(&engines);
    let sync = tmp_dir("snap-sync");
    let mut j = Journal::open(&sync, small_config()).unwrap();
    j.append_command(1, &ingest(0)).unwrap();
    j.append_snap(2, &engines, "sess", 7, 9, books().parts(), hash).unwrap();
    j.append_command(3, &ingest(1)).unwrap();
    drop(j);

    let deferred = tmp_dir("snap-deferred");
    let mut j = Journal::open(&deferred, small_config()).unwrap();
    j.append_command(1, &ingest(0)).unwrap();
    let sequence =
        j.append_snap_deferred(2, engines.clone(), "sess".into(), (7, 9), books()).unwrap();
    assert_eq!((sequence, j.next_sequence()), (1, 2), "it holds its sequence while it is encoded");
    assert_eq!(j.append_command(3, &ingest(1)).unwrap(), 2, "the record after it follows it");
    drop(j);

    assert_eq!(
        written(&segment_files(&deferred).remove(0)),
        written(&segment_files(&sync).remove(0)),
        "the same header and frames, byte for byte"
    );
    match &Journal::read_all(&deferred).unwrap()[1] {
        JournalRecord::Snapshot { sequence: 1, hash: read, .. } => assert_eq!(*read, hash),
        other => panic!("expected the Snapshot second, got {other:?}"),
    }
}

/// **Nothing leaves a deferred `Snapshot` unframed**: a flush (the tools' barrier) frames it, so does the core's
/// idle settle, and so does the stop, where the exit `Snapshot` a clean restart restores from is one.
#[test]
fn a_deferred_snapshot_is_framed_by_a_flush_the_idle_settle_and_the_stop() {
    let engines = snap_engines();
    let snapshots = |dir: &std::path::Path| {
        Journal::read_all(dir)
            .unwrap()
            .iter()
            .filter(|r| matches!(r, JournalRecord::Snapshot { .. }))
            .count()
    };
    let dir = tmp_dir("snap-deferred-settles");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    j.append_snap_deferred(1, engines.clone(), "sess".into(), (0, 0), books()).unwrap();
    j.flush().unwrap();
    assert_eq!(snapshots(&dir), 1, "the flush framed it");

    // The idle settle frames it once the write thread has encoded it, and never waits for or runs the encode.
    j.append_snap_deferred(2, engines.clone(), "sess".into(), (0, 0), books()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        j.settle_pending_snapshot().unwrap();
        j.write_fence().wait().unwrap(); // a framed `Snapshot` is an eager frame: in the file after this
        if snapshots(&dir) == 2 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the write thread never encoded it");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(j.next_sequence(), 2, "its sequence is spent");

    j.append_snap_deferred(3, engines, "sess".into(), (0, 0), books()).unwrap();
    drop(j);
    let read = Journal::read_all(&dir).unwrap();
    assert_eq!(read.len(), 3);
    assert!(
        matches!(read[2], JournalRecord::Snapshot { sequence: 2, now_ms: 3, .. }),
        "the stop's"
    );
}

/// **A deferred `Snapshot` that does not fit what is left of its segment rolls like any frame**: it is the next
/// segment's first record, that segment's header names its sequence, and the sequences run on unbroken.
#[test]
fn a_deferred_snapshot_that_does_not_fit_rolls_and_heads_the_next_segment() {
    let dir = tmp_dir("snap-deferred-roll");
    let config = JournalFileConfig { segment_bytes: 16 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config).unwrap();
    let mut n = 0u64;
    while crate::end_of_frames(&segment_files(&dir).remove(0)) < 15 * 1024 || n == 0 {
        j.append_command(n as i64, &ingest(n)).unwrap();
        j.write_fence().wait().unwrap();
        n += 1;
    }
    let mut big = books();
    big.recent_order_client_ids = (0..64).map(|i| (format!("order-{i:04}"), i)).collect();
    let sequence =
        j.append_snap_deferred(9_000, snap_engines(), "sess".into(), (0, 0), big).unwrap();
    assert_eq!(sequence, n);
    j.append_command(9_001, &ingest(99)).unwrap();
    drop(j);

    let read = Journal::read_all(&dir).unwrap();
    let sequences: Vec<u64> = read.iter().map(crate::record_sequence).collect();
    assert_eq!(sequences, (0..n + 2).collect::<Vec<u64>>(), "unbroken, the Snapshot at {n}");
    let mut segments = segment_files(&dir);
    segments.sort();
    assert!(segments.len() >= 2, "the Snapshot rolled");
    let head = |seg: &std::path::Path| {
        u64::from_le_bytes(std::fs::read(seg).unwrap()[8..16].try_into().unwrap())
    };
    assert!(
        segments.iter().any(|s| head(s.as_path()) == n),
        "a segment begins at the Snapshot's sequence: {:?}",
        segments.iter().map(|s| head(s.as_path())).collect::<Vec<_>>()
    );
}

/// The records [`journal_rolled_into_a_blocked_segment`] appends: ~4 KiB each into 16 KiB segments, so two rolls
/// (0 -> 1 -> 2) and a retired segment between the one the write thread finished and the live one.
const RECORDS: u64 = 9;

/// [`RECORDS`] records into a journal whose segment 1 cannot be opened: a directory holds its name, so every
/// hand-off past the first roll fails. Returns the journal and that directory.
fn journal_rolled_into_a_blocked_segment(dir: &std::path::Path) -> (Journal, std::path::PathBuf) {
    let mut j =
        Journal::open(dir, JournalFileConfig { segment_bytes: 16 * 1024, flush_every: 1 }).unwrap();
    let blocker = dir.join("journal-00000001.vjl");
    std::fs::create_dir(&blocker).unwrap();
    for n in 0..RECORDS {
        j.append_command(n as i64, &fat_ingest(n)).unwrap();
    }
    (j, blocker)
}

/// The sequences a journal's working files read back.
fn sequences(dir: &std::path::Path) -> Vec<u64> {
    Journal::read_all(dir).unwrap().iter().map(crate::record_sequence).collect()
}

/// **A stop whose hand-offs keep failing gives up within its bound and says what it did not write**, and those
/// frames are not lost to it: the segment whole in its working file is sealed, the other copies stay, every one
/// brought up to its end (the retired segment 1 as well as the live 2), so the next boot's repair makes the working
/// files whole.
#[tracing_test::traced_test]
#[test]
fn a_stop_gives_up_on_failing_hand_offs_within_its_bound_and_the_next_boot_repairs() {
    let dir = tmp_dir("stop-failing-writer");
    let (j, blocker) = journal_rolled_into_a_blocked_segment(&dir);
    let started = Instant::now();
    drop(j);
    let took = started.elapsed();
    assert!(
        took >= STOP_RETRY_FOR && took < STOP_RETRY_FOR + Duration::from_secs(5),
        "the stop took {took:?}"
    );
    let logs =
        String::from_utf8_lossy(&tracing_test::internal::global_buf().lock().unwrap()).into_owned();
    assert!(
        logs.lines().any(|l| l.contains("ERROR")
            && l.contains("the write thread gave up")
            && l.contains(&*dir.to_string_lossy())),
        "no give-up error names {}",
        dir.display()
    );
    let copy = |index: u64| dir.join(format!("journal-{index:08}.vjl.dur")).exists();
    assert_eq!((copy(0), copy(1), copy(2)), (false, true, true), "segment 0 sealed, 1 and 2 kept");
    std::fs::remove_dir(&blocker).unwrap();
    crate::repair_from_durable_copies(&dir).unwrap();
    assert_eq!(
        sequences(&dir),
        (0..RECORDS).collect::<Vec<u64>>(),
        "every record, after the repair"
    );
}

/// The captured log lines that name `dir` (every journal of the test binary logs into one buffer).
fn lines_naming(dir: &std::path::Path) -> Vec<String> {
    let buffer = tracing_test::internal::global_buf().lock().unwrap();
    let name = dir.to_string_lossy();
    String::from_utf8_lossy(&buffer)
        .lines()
        .filter(|l| l.contains(&*name))
        .map(String::from)
        .collect()
}

/// **Hand-offs that keep failing do not flood the log** (it likely lives on the failing disk): about a thousand
/// failures across a blocked second are said in a line or two, and their recovery in one line that counts them.
#[tracing_test::traced_test]
#[test]
fn failing_hand_offs_are_said_once_and_their_recovery_once() {
    let dir = tmp_dir("failing-writer-log");
    let (j, blocker) = journal_rolled_into_a_blocked_segment(&dir);
    std::thread::sleep(Duration::from_secs(1)); // the write thread retries every tick meanwhile
    let blocked = lines_naming(&dir);
    assert!(blocked.len() <= 3, "{} lines over a blocked second: {blocked:#?}", blocked.len());
    std::fs::remove_dir(&blocker).unwrap();
    drop(j); // joins the write thread, which recovered and wrote everything
    let recovered: Vec<String> =
        lines_naming(&dir).into_iter().filter(|l| l.contains("recovered")).collect();
    let [line] = recovered.as_slice() else { panic!("one recovery line: {recovered:#?}") };
    let failures: u64 = line
        .split("failures=")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("the recovery line counts the failures: {line}"));
    assert!(failures >= 100, "the second was blocked ({failures} failures counted)");
    assert_eq!(sequences(&dir), (0..RECORDS).collect::<Vec<u64>>());
}

/// **A run of slow write-thread steps is said twice: when it starts (the step and how long it took) and once the steps
/// have been fast for `SLOW_RUN_QUIET` (how many, the slowest)**, never once per step: a quick step, a second slow one
/// and a quick one too soon after it add no line (the CI box, 2026-11-24: a filesystem pause held the thread for about 5 s
/// and nothing said where; a disk that stays slow must not write a line per step).
#[tracing_test::traced_test]
#[test]
fn a_run_of_slow_write_thread_steps_is_said_when_it_starts_and_when_it_ends() {
    let t0 = Instant::now();
    let step = "test-only step";
    note_step_at(step, Duration::from_millis(1), || t0);
    note_step_at(step, SLOW_STEP + Duration::from_millis(50), || t0);
    note_step_at(step, SLOW_STEP + Duration::from_millis(100), || t0 + Duration::from_secs(1));
    note_step_at(step, Duration::from_millis(1), || t0 + Duration::from_secs(2));
    note_step_at(step, Duration::from_millis(1), || t0 + Duration::from_secs(1) + SLOW_RUN_QUIET);
    let buffer = tracing_test::internal::global_buf().lock().unwrap();
    let logs = String::from_utf8_lossy(&buffer);
    let said: Vec<&str> = logs.lines().filter(|l| l.contains(step)).collect();
    let [started, ended] = said.as_slice() else { panic!("two lines: {said:#?}") };
    assert!(started.contains("WARN") && started.contains("test-only step took 300ms"), "{started}");
    assert!(
        ended.contains("WARN")
            && ended.contains("2 slow step(s)")
            && ended.contains("the slowest 350ms"),
        "{ended}"
    );
}

/// **An order's fence is bounded** (decision 0279's addendum of 2026-11-23): behind a write thread whose hand-offs keep
/// failing it answers `JournalCannotWrite` once [`FENCE_BOUND`] has passed, at once on the next wait while the write
/// thread has written nothing, and `Ok` again once the disk takes the frames.
#[test]
fn a_fence_behind_a_failing_write_thread_gives_up_at_its_bound_and_answers_again_once_it_recovers()
{
    let dir = tmp_dir("fence-bound");
    let (j, blocker) = journal_rolled_into_a_blocked_segment(&dir);
    let fence = j.write_fence();
    let started = Instant::now();
    assert_eq!(fence.wait(), Err(vike_execution::JournalCannotWrite));
    let took = started.elapsed();
    assert!(took >= FENCE_BOUND && took < FENCE_BOUND * 3, "the first wait took {took:?}");
    let started = Instant::now();
    assert_eq!(fence.wait(), Err(vike_execution::JournalCannotWrite));
    assert!(started.elapsed() < FENCE_BOUND / 10, "the next wait answers at once");
    std::fs::remove_dir(&blocker).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while fence.wait().is_err() {
        assert!(Instant::now() < deadline, "the fence never answered after the disk recovered");
        std::thread::sleep(WRITE_TICK);
    }
    drop(j);
    assert_eq!(sequences(&dir), (0..RECORDS).collect::<Vec<u64>>());
}

/// **A disk that recovers inside the bound gets everything at the stop**: the working files hold every record and
/// were sealed (no durable copy is left), so nothing was given up.
#[test]
fn a_stop_whose_disk_recovers_inside_the_bound_writes_everything() {
    let dir = tmp_dir("stop-recovering-writer");
    let (j, blocker) = journal_rolled_into_a_blocked_segment(&dir);
    let stop = std::thread::spawn(move || drop(j));
    std::thread::sleep(Duration::from_millis(200)); // the stop is retrying the failing roll
    std::fs::remove_dir(&blocker).unwrap();
    stop.join().unwrap();
    let copies: Vec<_> = std::fs::read_dir(&*dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| name.ends_with(".dur"))
        .collect();
    assert!(copies.is_empty(), "every segment sealed: {copies:?}");
    assert_eq!(sequences(&dir), (0..RECORDS).collect::<Vec<u64>>());
}

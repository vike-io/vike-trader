//! The ARCHIVE pass (decision 0304's verdict 2): what `Journal::prune_before_latest_snap` moves beside a live core
//! and what it never moves, what a crash at each step leaves, the archive's limit, and the compression's round trip.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::*;
use crate::record::{JournalRecord, JournalSnapshotParts, record_sequence};
use crate::segment::{segment_indices, segment_path};
use crate::testutil::*;
use crate::{JournalDurableMark, JournalFileConfig};

const HOUR_MS: i64 = 3_600_000;

/// 4 KiB segments, so a few hundred commands span several.
fn rolling() -> JournalFileConfig {
    JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 }
}

fn keep_for(keep_ms: i64) -> JournalArchivePolicy {
    JournalArchivePolicy { keep_ms, ..keep_nothing() }
}

/// The index of every top-level segment of `dir`, ascending.
fn indices(dir: &Path) -> Vec<u64> {
    segment_indices(dir).unwrap()
}

/// Each segment's header `first_sequence`.
fn first_sequences(dir: &Path) -> BTreeMap<u64, u64> {
    indices(dir)
        .into_iter()
        .map(|i| {
            let b = std::fs::read(segment_path(dir, i)).unwrap();
            (i, u64::from_le_bytes(b[8..16].try_into().unwrap()))
        })
        .collect()
}

/// The files under `<dir>/archive/`, by name, sorted.
fn archived(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(dir.join(ARCHIVE_DIR)) {
        Ok(entries) => {
            entries.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect()
        }
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// Flush `j` and wait until every segment below the live one is sealed (the sync thread seals a left segment on
/// its own time): the mark a running node's archive thread reads.
fn sealed_mark(j: &mut Journal, dir: &Path) -> JournalDurableMark {
    j.flush().unwrap();
    let watch = j.durable_watch();
    let live = *indices(dir).last().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while watch.durable_mark().segment_index < live {
        assert!(Instant::now() < deadline, "the seals never landed");
        std::thread::sleep(Duration::from_millis(1));
    }
    watch.durable_mark()
}

/// The newest `Snapshot`'s segment: where the boot's read starts.
fn snapshot_segment(dir: &Path) -> u64 {
    let (records, _) = Journal::read_from_latest_snap_located(dir).unwrap();
    let base = record_sequence(&records[0]);
    *first_sequences(dir).iter().rev().find(|(_, first)| **first <= base).unwrap().0
}

/// A LIVE journal: command `i` (sequence `i`) at hour `i` for 300 hours, a `Snapshot` at hour 300, then 100
/// commands more (several segments past the `Snapshot`'s), flushed and sealed. The journal stays open.
fn live_with_a_sealed_snapshot(dir: &Path) -> (Journal, JournalDurableMark) {
    let mut j = Journal::open(dir, rolling()).unwrap();
    for i in 0..300 {
        j.append_command(i as i64 * HOUR_MS, &ingest(i)).unwrap();
    }
    j.append_snap(300 * HOUR_MS, &snap_engines(), "sess", 7, 0, JournalSnapshotParts::default(), 1)
        .unwrap();
    for i in 300..400 {
        j.append_command(300 * HOUR_MS + i as i64, &ingest(i)).unwrap();
    }
    let mark = sealed_mark(&mut j, dir);
    assert!(
        snapshot_segment(dir) < mark.segment_index,
        "premise: the snapshot's segment is sealed"
    );
    (j, mark)
}

/// **While running**: after a durable `Snapshot`, the segments whose newest record is older than the keep window
/// move to the archive, the ones inside it stay, and the journal goes on appending.
#[test]
fn segments_older_than_the_window_move_to_the_archive_while_the_journal_runs() {
    let dir = tmp_dir("archive-live-window");
    let (mut j, mark) = live_with_a_sealed_snapshot(&dir);
    let firsts = first_sequences(&dir);
    let base = snapshot_segment(&dir);
    assert!(base >= 3, "premise: several segments below the snapshot's ({base})");

    // At hour 300 with a 1000-hour window, nothing is old enough.
    let now = 300 * HOUR_MS;
    let none =
        Journal::prune_before_latest_snap(&dir, &keep_for(1_000 * HOUR_MS), now, mark).unwrap();
    assert_eq!(none, JournalArchivePass::default());
    assert_eq!(indices(&dir), firsts.keys().copied().collect::<Vec<_>>());

    // A 200-hour window at hour 300: a segment goes once its NEXT segment's first record (sequence n at hour n)
    // is at hour 100 or before.
    let pass =
        Journal::prune_before_latest_snap(&dir, &keep_for(200 * HOUR_MS), now, mark).unwrap();
    let old: Vec<u64> =
        (0..base).filter(|i| firsts[&(i + 1)] as i64 * HOUR_MS <= now - 200 * HOUR_MS).collect();
    assert!(!old.is_empty() && old.len() < base as usize, "premise: the window splits them");
    assert_eq!(pass.moved, old, "exactly the segments outside the window");
    for index in &old {
        assert!(dir.join(ARCHIVE_DIR).join(format!("journal-{index:08}.vjl")).exists());
    }
    assert_eq!(indices(&dir), firsts.keys().copied().skip(old.len()).collect::<Vec<_>>());

    // The journal goes on: it appends, and the boot's read from the snapshot on reads to its last record.
    j.append_command(400 * HOUR_MS, &ingest(9_999)).unwrap();
    j.flush().unwrap();
    let (records, halt) = Journal::read_from_latest_snap_located(&dir).unwrap();
    assert!(halt.is_none() && matches!(records[0], JournalRecord::Snapshot { .. }));
    assert_eq!(records.last().map(record_sequence), Some(j.next_sequence() - 1));
}

/// **Never** the live segment, the newest durable `Snapshot`'s segment or anything between them, however old.
#[test]
fn the_live_segment_and_the_snapshots_segment_are_never_moved() {
    let dir = tmp_dir("archive-live-never");
    let (j, mark) = live_with_a_sealed_snapshot(&dir);
    let base = snapshot_segment(&dir);
    let live = *indices(&dir).last().unwrap();
    let pass = Journal::prune_before_latest_snap(&dir, &keep_nothing(), i64::MAX, mark).unwrap();
    assert_eq!(pass.moved, (0..base).collect::<Vec<_>>(), "every segment below the snapshot's");
    assert_eq!(indices(&dir), (base..=live).collect::<Vec<_>>(), "and no other");
    drop(j);
}

/// A `Snapshot` in the LIVE segment is not durable whole, so it is not the base: the pass judges by the newest
/// `Snapshot` in a sealed segment, here the first one, and moves nothing.
#[test]
fn a_snapshot_only_the_live_segment_holds_supersedes_nothing() {
    let dir = tmp_dir("archive-live-unsealed");
    let mut j = Journal::open(&dir, rolling()).unwrap();
    j.append_snap(0, &snap_engines(), "sess", 7, 0, JournalSnapshotParts::default(), 1).unwrap();
    for i in 0..300 {
        j.append_command(i as i64 * HOUR_MS, &ingest(i)).unwrap();
    }
    j.append_snap(300 * HOUR_MS, &snap_engines(), "sess", 7, 0, JournalSnapshotParts::default(), 2)
        .unwrap();
    let mark = sealed_mark(&mut j, &dir);
    let live = *indices(&dir).last().unwrap();
    assert_eq!(snapshot_segment(&dir), live, "premise: the second snapshot is in the live segment");

    let pass = Journal::prune_before_latest_snap(&dir, &keep_nothing(), i64::MAX, mark).unwrap();
    assert!(pass.moved.is_empty(), "the newest durable snapshot is the first: {pass:?}");
    assert_eq!(indices(&dir), (0..=live).collect::<Vec<_>>());
    drop(j);
    // Closed, every segment is durable, and the second snapshot supersedes the ones below it.
    assert_eq!(prune_closed(&dir).unwrap().moved, (0..live).collect::<Vec<_>>());
}

/// 300 commands, a `Snapshot`, 60 more, over 4 KiB segments, closed.
fn snapped_closed(tag: &str) -> Scratch {
    let dir = tmp_dir(tag);
    let mut j = Journal::open(&dir, rolling()).unwrap();
    for i in 0..300 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    j.append_snap(9_999, &snap_engines(), "sess", 7, 0, JournalSnapshotParts::default(), 1)
        .unwrap();
    for i in 300..360 {
        j.append_command(10_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    dir
}

/// **The archive's limit** applies its action: at the size or the age limit the pass moves nothing (`keep`), says
/// which limit, and touches nothing in the archive.
#[test]
fn at_the_archives_limit_the_pass_keeps_everything_where_it_is() {
    let dir = snapped_closed("archive-limit");
    let archive = dir.join(ARCHIVE_DIR);
    std::fs::create_dir_all(&archive).unwrap();
    let held = archive.join("journal-99999999.vjl.gz");
    std::fs::write(&held, vec![7u8; 1_000]).unwrap();
    let before = indices(&dir);

    let full = JournalArchivePolicy { max_bytes: 1_000, ..keep_nothing() };
    let pass =
        Journal::prune_before_latest_snap(&dir, &full, i64::MAX, EVERY_SEGMENT_DURABLE).unwrap();
    let size = JournalArchivePass {
        moved: Vec::new(),
        limit: Some(JournalArchiveLimit::Size { bytes: 1_000 }),
    };
    assert_eq!(pass, size);
    assert_eq!(indices(&dir), before, "nothing moved");

    let ten_days = Duration::from_secs(10 * 24 * 3_600);
    std::fs::File::options()
        .write(true)
        .open(&held)
        .unwrap()
        .set_modified(SystemTime::now() - ten_days)
        .unwrap();
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
    let aged = JournalArchivePolicy { max_age_ms: 9 * 24 * HOUR_MS, ..keep_nothing() };
    let pass =
        Journal::prune_before_latest_snap(&dir, &aged, now_ms, EVERY_SEGMENT_DURABLE).unwrap();
    assert!(pass.moved.is_empty(), "{pass:?}");
    assert!(
        matches!(pass.limit, Some(JournalArchiveLimit::Age { age_ms }) if age_ms >= 10 * 24 * HOUR_MS - 60_000),
        "{pass:?}"
    );
    assert_eq!(indices(&dir), before, "nothing moved");
    assert_eq!(archived(&dir), ["journal-99999999.vjl.gz"], "nothing in the archive was touched");

    // Under both limits, the same journal moves.
    let pass =
        Journal::prune_before_latest_snap(&dir, &keep_nothing(), now_ms, EVERY_SEGMENT_DURABLE)
            .unwrap();
    assert!(pass.limit.is_none() && !pass.moved.is_empty(), "{pass:?}");
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

/// The archive the `drop_oldest` tests start from, 1 000 bytes a file, 7 000 in all: three finished copies (40, 30
/// and 20 days old) and four files no deletion may take, older than most of them: a plain segment not compressed
/// yet, a `.part` being written, and a copy beside the plain file it was made from (a crash before that removal).
fn fill_archive(dir: &Path) {
    let archive = dir.join(ARCHIVE_DIR);
    std::fs::create_dir_all(&archive).unwrap();
    for (name, days) in [
        ("journal-90000000.vjl", 50),
        ("journal-90000001.vjl.gz", 40),
        ("journal-90000002.vjl.gz", 30),
        ("journal-90000003.vjl.gz", 20),
        ("journal-90000004.vjl.gz.part", 45),
        ("journal-90000005.vjl", 35),
        ("journal-90000005.vjl.gz", 35),
    ] {
        let path = archive.join(name);
        std::fs::write(&path, vec![7u8; 1_000]).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(days * 24 * 3_600))
            .unwrap();
    }
}

/// **At the limit, `keep` (the default) deletes nothing, and the opt-in `drop_oldest` deletes exactly the oldest
/// finished copies that bring the archive under it, then archives as usual**: never a file not compressed yet, a
/// `.part` or a copy beside its plain file, however old, and never a segment of the live directory.
#[test]
fn at_the_limit_drop_oldest_deletes_exactly_the_oldest_copies_needed_and_archiving_goes_on() {
    let dir = snapped_closed("archive-drop-oldest");
    fill_archive(&dir);
    let filled = archived(&dir);
    let base = snapshot_segment(&dir);
    let kept: Vec<(u64, Vec<u8>)> = indices(&dir)
        .into_iter()
        .filter(|i| *i >= base)
        .map(|i| (i, std::fs::read(segment_path(&dir, i)).unwrap()))
        .collect();

    let at = JournalArchivePolicy { max_bytes: 5_500, ..keep_nothing() };
    let pass =
        Journal::prune_before_latest_snap(&dir, &at, now_ms(), EVERY_SEGMENT_DURABLE).unwrap();
    let full = JournalArchivePass {
        moved: Vec::new(),
        limit: Some(JournalArchiveLimit::Size { bytes: 7_000 }),
    };
    assert_eq!(pass, full, "keep moves nothing");
    assert_eq!(archived(&dir), filled, "and deletes nothing");

    // 7 000 bytes against 5 500: the 40- and 30-day copies go (5 000 left), the 20-day one stays.
    let opted_in = JournalArchivePolicy { at_limit: JournalArchiveFull::DropOldest, ..at };
    let pass = Journal::prune_before_latest_snap(&dir, &opted_in, now_ms(), EVERY_SEGMENT_DURABLE)
        .unwrap();
    assert_eq!(pass.limit, None, "under its limit");
    assert_eq!(pass.moved, (0..base).collect::<Vec<_>>(), "and archiving went on");
    let deleted = ["journal-90000001.vjl.gz", "journal-90000002.vjl.gz"];
    let mut left: Vec<String> = filled
        .into_iter()
        .filter(|name| !deleted.contains(&name.as_str()))
        .chain(pass.moved.iter().map(|i| format!("journal-{i:08}.vjl")))
        .collect();
    left.sort();
    assert_eq!(archived(&dir), left, "exactly the two oldest copies went");
    // The newest snapshot's segment and every later one stay where they were, whole.
    assert_eq!(indices(&dir), kept.iter().map(|(i, _)| *i).collect::<Vec<_>>());
    for (index, bytes) in &kept {
        assert_eq!(&std::fs::read(segment_path(&dir, *index)).unwrap(), bytes, "segment {index}");
    }
}

/// `drop_oldest` at the AGE limit deletes every copy past it and no other, and the limit stays when a file no
/// deletion may take is past it too; at the SIZE limit it deletes no copy when those files fill it on their own.
#[test]
fn drop_oldest_never_deletes_what_cannot_bring_the_archive_under_its_limit() {
    let dir = tmp_dir("archive-drop-oldest-limits");
    fill_archive(&dir);
    let opted_in =
        JournalArchivePolicy { at_limit: JournalArchiveFull::DropOldest, ..keep_nothing() };

    // The four files no deletion may take hold 4 000 bytes, past a 3 000-byte limit: no copy goes.
    let size = JournalArchivePolicy { max_bytes: 3_000, ..opted_in };
    let limit = size.drop_oldest(&dir, now_ms()).unwrap();
    assert_eq!(limit, Some(JournalArchiveLimit::Size { bytes: 7_000 }));
    assert_eq!(archived(&dir).len(), 7, "nothing deleted");

    // 25 days: the 40- and 30-day copies go, the 20-day one stays, and the 50-day plain segment keeps the limit.
    let age = JournalArchivePolicy { max_age_ms: 25 * 24 * HOUR_MS, ..opted_in };
    let limit = age.drop_oldest(&dir, now_ms()).unwrap();
    assert!(
        matches!(limit, Some(JournalArchiveLimit::Age { age_ms }) if age_ms >= 50 * 24 * HOUR_MS - 60_000),
        "{limit:?}"
    );
    assert_eq!(
        archived(&dir),
        [
            "journal-90000000.vjl",
            "journal-90000003.vjl.gz",
            "journal-90000004.vjl.gz.part",
            "journal-90000005.vjl",
            "journal-90000005.vjl.gz",
        ]
    );
}

/// gunzip of `path`.
fn gunzip(path: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap()).read_to_end(&mut out).unwrap();
    out
}

/// **Nothing is lost**: every archived segment is compressed, its plain file goes only after its copy, and the
/// copies gunzipped back beside the live directory's segments read as the journal did before the pass.
#[test]
fn the_archive_compressed_and_unpacked_reads_as_the_journal_did() {
    let dir = snapped_closed("archive-roundtrip");
    let before = format!("{:?}", Journal::read_all(&dir).unwrap());
    let pass = prune_closed(&dir).unwrap();
    assert!(!pass.moved.is_empty());
    let packed = Journal::compress_archive(&dir).unwrap();
    assert_eq!(packed.len(), pass.moved.len());
    let names: Vec<String> = pass.moved.iter().map(|i| format!("journal-{i:08}.vjl.gz")).collect();
    assert_eq!(archived(&dir), names, "every plain file went, only after its copy");

    let unpacked = tmp_dir("archive-roundtrip-unpacked");
    for index in &pass.moved {
        let copy = gunzip(&dir.join(ARCHIVE_DIR).join(format!("journal-{index:08}.vjl.gz")));
        std::fs::write(segment_path(&unpacked, *index), copy).unwrap();
    }
    for index in indices(&dir) {
        std::fs::copy(segment_path(&dir, index), segment_path(&unpacked, index)).unwrap();
    }
    assert_eq!(format!("{:?}", Journal::read_all(&unpacked).unwrap()), before);
    assert!(Journal::compress_archive(&dir).unwrap().is_empty(), "a second call has nothing to do");
}

/// **A crash at any step loses nothing.** The move is one rename (the segment is in one place or the other); a
/// crash mid-compression leaves a `.part` (the next call writes it again and finishes), a crash between the copy's
/// rename and the plain file's removal leaves both (the next call removes the plain one), and a name the archive
/// already holds is never written over.
#[test]
fn a_crash_at_any_step_of_the_archive_leaves_every_segment_somewhere() {
    let dir = snapped_closed("archive-crash");
    let originals: BTreeMap<u64, Vec<u8>> = indices(&dir)
        .into_iter()
        .map(|i| (i, std::fs::read(segment_path(&dir, i)).unwrap()))
        .collect();
    let pass = prune_closed(&dir).unwrap();
    let archive = dir.join(ARCHIVE_DIR);
    let plain = |i: u64| archive.join(format!("journal-{i:08}.vjl"));
    let packed = |i: u64| archive.join(format!("journal-{i:08}.vjl.gz"));
    for index in &pass.moved {
        assert!(plain(*index).exists(), "segment {index} is in the archive");
        assert!(!segment_path(&dir, *index).exists(), "and only there");
    }
    let [first, second, ..] = pass.moved[..] else {
        panic!("premise: two segments move: {pass:?}")
    };

    // a crash mid-gzip of `first`: a torn `.part`
    let part = archive.join(format!("journal-{first:08}.vjl.gz.part"));
    std::fs::write(&part, b"torn").unwrap();
    // a crash after `second`'s copy was named, before its plain file went
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut encoder, &originals[&second]).unwrap();
    std::fs::write(packed(second), encoder.finish().unwrap()).unwrap();

    Journal::compress_archive(&dir).unwrap();
    for index in &pass.moved {
        assert!(!plain(*index).exists(), "segment {index}'s plain file went after its copy");
        assert_eq!(&gunzip(&packed(*index)), &originals[index], "segment {index} whole");
    }
    assert!(!part.exists(), "the torn copy was written again and named");

    // A name the archive holds is not reused: a second segment 0 (a journal started over) goes beside it.
    std::fs::write(segment_path(&dir, 0), &originals[&0]).unwrap();
    assert_eq!(archive_segment(&dir, 0).unwrap(), archive.join("journal-00000000-1.vjl"));
    assert_eq!(&gunzip(&packed(0)), &originals[&0], "the archived one is untouched");
}

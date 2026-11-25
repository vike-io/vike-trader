//! The version-compat tests: the stamped version, the readable range and both refusal edges.

use super::*;
use crate::record::JournalSnapshotParts;
use crate::segment::segment_path;
use crate::testutil::*;
use crate::{Journal, JournalRecord};
use std::assert_matches;

#[test]
fn version_mismatch_on_read_is_an_explicit_error() {
    let dir = tmp_dir("version");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..5 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
        j.write_fence().wait().unwrap();
    }
    drop(j);
    // Overwrite the header VERSION (bytes[4..8]) with a future version. MAGIC still matches, so
    // this is NOT a foreign file (skipped) nor a torn tail (dropped) — it is OUR journal in an
    // unknown format, which must surface LOUDLY rather than silently mis-parse.
    // `segment_files` (not the first `read_dir` entry) — the dir also holds the `LOCK` sentinel.
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();
    let err = Journal::read_all(&dir).unwrap_err();
    assert_eq!(
        err.kind(),
        io::ErrorKind::InvalidData,
        "a MAGIC-matching, VERSION-mismatching segment is an explicit InvalidData error"
    );
}

/// The WRITE-RESUME twin of `version_mismatch_on_read_is_an_explicit_error` above: that one
/// exercises the READ path (`read_all`); this one exercises the branch a live restart actually
/// hits FIRST — `Journal::open` → `map_segment`'s "resuming an EXISTING segment" arm
/// (~line 227) — which must refuse to append this build's frames into a segment stamped with an
/// UNSUPPORTED format version, rather than silently reinterpreting/overwriting it. Uses a
/// FORWARD version (`VERSION + 1`), the case that stays a hard error under the compat range;
/// the supported-older case is covered by
/// `resuming_an_older_supported_version_upgrades_the_stamp_and_appends`. Hand-write a segment
/// header (MAGIC matches, version does not) with no `Journal` in the loop, since going
/// through `open`/`append_*` would only ever stamp the CURRENT `VERSION`.
#[test]
fn open_refuses_to_resume_a_segment_with_an_unsupported_version() {
    let dir = tmp_dir("write-resume-version");
    let config = small_config();
    let seg = segment_path(&dir, 0);
    let mut header = vec![0u8; HEADER];
    header[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    header[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
    header[8..16].copy_from_slice(&0u64.to_le_bytes());
    std::fs::write(&seg, &header).unwrap();

    // `Journal` holds an `MmapMut` (no `Debug` impl), so `unwrap_err()` isn't available
    // here the way it is on `read_all`'s `io::Result<Vec<JournalRecord>>` above — match instead.
    match Journal::open(&dir, config) {
        Ok(_) => panic!(
            "resuming a MAGIC-matching, VERSION-mismatching segment must fail loudly on open, \
                 not silently truncate/reinterpret it"
        ),
        Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidData),
    }
}

#[test]
fn version_is_35() {
    assert_eq!(super::VERSION, 35);
}

/// Pins the COMPATIBILITY RANGES, not just the write version (see the module doc's "Version
/// compatibility contract"): what this build RESUMES and restores from (34 and 35: the binary step is a floor
/// step, the denied-repeat step is additive) and what it only READS (31..=35: a JSON journal the binary step moved
/// aside). This test is the tripwire that forces either to move by a deliberate, reviewed edit rather than a silent
/// widening.
#[test]
fn the_resumable_range_is_34_to_35_and_the_readable_one_31_to_35() {
    assert_eq!(super::MIN_READABLE_VERSION, 34);
    assert_eq!(super::MIN_READ_ONLY_VERSION, 31);
    assert_eq!(super::VERSION, 35);
    // NB: the floor <= write-version invariant is pinned by the assert_eq!s above; a
    // direct `assert!(MIN_READABLE_VERSION <= VERSION)` is const-foldable and trips
    // clippy::assertions_on_constants under -D warnings.
    assert!(super::check_version(34).is_ok(), "v34 (the binary payload) is the floor and the base");
    assert!(super::check_version(35).is_ok(), "v35 (a count of repeated denials) is additive");
    for v in 31..=33 {
        assert!(super::check_version(v).is_err(), "a v{v} JSON journal is never resumed");
        assert!(super::check_read_only_version(v).is_ok(), "a v{v} JSON journal is read");
    }
    assert!(
        super::check_read_only_version(30).is_err(),
        "a v30 journal names instruments under the old grammar (bybit `BTCUSDT` was the perpetual): a loud error, never a silent mis-read"
    );
    assert!(super::check_read_only_version(4).is_err(), "far below the floor is a loud error too");
    for check in [super::check_version, super::check_read_only_version] {
        assert!(check(36).is_err(), "a FORWARD version is a loud error");
    }
    // The words `deploy/docker/vikectl`'s pre-flight reads to move a journal aside.
    let words = super::check_version(33).unwrap_err().to_string();
    assert_eq!(words, "journal format version mismatch: got 33, this build reads 34..=35");
}

/// **The denied-repeat step is LAZY**: a journal that never coalesces a denial keeps the base, 34, from its first
/// byte, so the build before it still reads it; its first `DeniedRepeat` raises the header to 35, and the record
/// reads back whole.
#[test]
fn the_header_rises_to_35_only_with_a_denied_repeat() {
    let dir = tmp_dir("denied-repeat-stamp");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let seg = segment_files(&dir).remove(0);
    j.append_command(1, &ingest(0)).unwrap();
    j.write_fence().wait().unwrap();
    assert_eq!(stamp(&seg), 34, "no denial coalesced: the base");
    let repeat = crate::JournalDeniedRepeat {
        mount_id: "m".into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        refusal_id: Some("halted".into()),
        reason: "halted".into(),
        count: 3,
        first_ms: 2,
        last_ms: 4,
        order_client_id_session: "cafef00d".into(),
        reserved_from: 7,
        reserved_to: 7 + 65_536,
    };
    let seq = j.append_denied_repeat(5, &repeat).unwrap();
    j.write_fence().wait().unwrap();
    assert_eq!(stamp(&seg), super::DENIED_REPEAT_VERSION);
    drop(j);
    let back = Journal::read_all(&dir).unwrap();
    assert_matches!(
        &back[1],
        JournalRecord::DeniedRepeat { sequence: s, now_ms: 5, repeat: r } if *s == seq && *r == repeat
    );
}

/// **A v33 journal (JSON frames, the format before the binary step) READS BACK under this build** — the readers a
/// journal moved aside meets (`read_all`, `read_since`, `read_closed_since`) — and is REFUSED by everything that
/// would resume or restore from it: `Journal::open` and the boot restore's `read_all_located`, in `check_version`'s
/// words, before either touches it.
#[test]
fn a_v33_json_journal_reads_back_and_is_never_resumed() {
    let dir = tmp_dir("version-json-read");
    let records: Vec<JournalRecord> = (0..5)
        .map(|i| JournalRecord::Command {
            sequence: i,
            now_ms: 1_000 + i as i64,
            message: ingest(i),
        })
        .collect();
    write_json_segment(&dir, 0, 33, &records, 64 * 1024);
    let seg = segment_files(&dir).remove(0);
    let before = std::fs::read(&seg).unwrap();

    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 5, "every v33 record is readable by this build");
    for (i, r) in back.iter().enumerate() {
        assert_matches!(r, JournalRecord::Command { sequence: seq, .. } if *seq == i as u64);
    }
    assert_eq!(crate::read_since(&dir, 1).unwrap().len(), 3, "the incremental read too");
    let (page, halt) = crate::read_closed_since(&dir, None, 10).unwrap();
    assert_eq!((page.len(), halt), (5, None), "and the moved-aside journal's paged read");

    let refused = Journal::read_all_located(&dir).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
    assert!(refused.to_string().contains("got 33, this build reads 34..=35"), "{refused}");
    match Journal::open(&dir, small_config()) {
        Ok(_) => {
            panic!("a v33 segment must not be resumed with msgpack frames behind its JSON ones")
        }
        Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidData),
    }
    assert_eq!(std::fs::read(&seg).unwrap(), before, "neither refusal touched the segment");
}

#[test]
fn base_version_is_34() {
    assert_eq!(super::BASE_VERSION, 34);
}

/// The header version of a segment file.
fn stamp(seg: &std::path::Path) -> u32 {
    u32::from_le_bytes(std::fs::read(seg).unwrap()[4..8].try_into().unwrap())
}

/// A mount's strategy file (2b's J30) rides the stored-words step: every segment is stamped
/// `STORED_WORDS_VERSION` from its first byte (it is the base), a `Snapshot` whose mount row names no file writes no
/// key of it, and the record reads back whole.
#[test]
fn a_strategy_file_mount_rides_the_stored_words_step_and_a_row_without_one_writes_no_key() {
    let dir = tmp_dir("stored-words-mount");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let seg = segment_files(&dir).remove(0);
    let rows = vec![crate::JournalSnapshotMountAttribution {
        mount_id: "m".into(),
        size: 1.0,
        ..Default::default()
    }];
    let books =
        JournalSnapshotParts { mount_attribution: &rows, ..JournalSnapshotParts::default() };
    j.append_snap(1, &snap_engines(), "sess", 0, 0, books, 1).unwrap();
    j.write_fence().wait().unwrap();
    assert_eq!(stamp(&seg), super::BASE_VERSION, "every segment is the base from its first byte");
    let written = String::from_utf8_lossy(&std::fs::read(&seg).unwrap()).into_owned();
    assert!(!written.contains("strategy_file"), "a row naming no file writes no key");

    let file =
        crate::JournalStrategyFile { path: "grid.rhai".into(), sha256: "ab".repeat(32), sdk: None };
    let seq = j.append_strategy_file_mounted(2, "m", &file).unwrap();
    j.write_fence().wait().unwrap();
    assert_eq!(
        stamp(&seg),
        super::BASE_VERSION,
        "the stored-words record needs no raise: the base is above it"
    );
    assert_eq!(super::STORED_WORDS_VERSION, 30, "the stored-words step is history, below the base");
    drop(j);
    assert_eq!(Journal::latest_segment_version(&dir).unwrap(), Some(34));
    let back = Journal::read_all(&dir).unwrap();
    assert_matches!(
        &back[1],
        JournalRecord::StrategyFileMounted { sequence: s, now_ms: 2, mount_id, strategy_file }
            if *s == seq && mount_id == "m" && *strategy_file == file
    );
}

/// The floor is still a hard wall in the other direction: a version BELOW
/// `MIN_READ_ONLY_VERSION` is a shape this build cannot parse, so it must fail loudly rather
/// than drop records as a torn tail.
#[test]
fn a_version_below_the_supported_floor_is_still_an_explicit_error() {
    let dir = tmp_dir("version-below-floor");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    j.append_command(1_000, &ingest(0)).unwrap();
    j.write_fence().wait().unwrap();
    drop(j);
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&(super::MIN_READ_ONLY_VERSION - 1).to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();
    assert_eq!(
        Journal::read_all(&dir).unwrap_err().kind(),
        io::ErrorKind::InvalidData,
        "a pre-floor segment is an explicit InvalidData error, not a silent mis-parse"
    );
}

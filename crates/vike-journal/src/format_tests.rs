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
        j.write_fence().wait();
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
fn version_is_30() {
    assert_eq!(super::VERSION, 30);
}

/// Pins the COMPATIBILITY RANGE, not just the write version (see the module doc's "Version
/// compatibility contract"). If a future step stops being purely additive it must raise
/// `MIN_READABLE_VERSION` — and this test is the tripwire that forces that to be a deliberate,
/// reviewed edit rather than a silent widening.
#[test]
fn readable_version_range_is_30_to_30() {
    assert_eq!(super::MIN_READABLE_VERSION, 30);
    assert_eq!(super::VERSION, 30);
    // NB: the floor <= write-version invariant is pinned by the two assert_eq! above; a
    // direct `assert!(MIN_READABLE_VERSION <= VERSION)` is const-foldable and trips
    // clippy::assertions_on_constants under -D warnings.
    assert!(
        super::check_version(30).is_ok(),
        "v30 (the order client id keys spelled out) is what this build writes"
    );
    assert!(
        super::check_version(29).is_err(),
        "a v29 journal spells the keys this build renamed: a loud error, never a silent mis-read"
    );
    assert!(super::check_version(4).is_err(), "far below the floor is a loud error too");
    assert!(super::check_version(31).is_err(), "a FORWARD version is a loud error");
}

/// A v4 journal directory (written before `PortfolioSnapshot` existed) must READ BACK under this
/// v5 build — the whole point of the additive-only rule. Before the compat range this returned
/// `InvalidData` and an operator had to discard a live journal to take the upgrade.
#[test]
fn an_older_supported_version_journal_reads_back_intact() {
    let dir = tmp_dir("version-back-compat-read");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..5 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
        j.write_fence().wait();
    }
    drop(j);
    // Restamp the header to the OLDEST supported version. The frames themselves are untouched
    // — which is exactly the real-world shape of a v4 segment: `Command` records only, since v4
    // could not write `PortfolioSnapshot`.
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&super::MIN_READABLE_VERSION.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();

    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 5, "every v4 record is readable by this v5 build");
    for (i, r) in back.iter().enumerate() {
        assert_matches!(r, JournalRecord::Command { sequence: seq, .. } if *seq == i as u64);
    }
}

/// The WRITE-RESUME twin: restarting a v5 binary on a v4 directory must APPEND to it (the live
/// crash-restore path), upgrading the header stamp in place, and the merged old+new record
/// stream must read back in seq order.
#[test]
fn resuming_an_older_supported_version_upgrades_the_stamp_and_appends() {
    let dir = tmp_dir("version-back-compat-resume");
    let config = small_config();
    let mut j = Journal::open(&dir, config.clone()).unwrap();
    for i in 0..3 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
        j.write_fence().wait();
    }
    drop(j);
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&super::MIN_READABLE_VERSION.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();

    // Restart onto the v4 directory: must open (not error), resume the seq, and append.
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 3..6 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
        j.write_fence().wait();
    }
    j.flush().unwrap();
    drop(j);

    let stamped = u32::from_le_bytes(std::fs::read(&seg).unwrap()[4..8].try_into().unwrap());
    assert_eq!(
        stamped,
        super::BASE_VERSION,
        "resuming a supported older segment restamps it to the base version, not past it"
    );
    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 6, "pre-upgrade and post-upgrade records coexist in one segment");
    for (i, r) in back.iter().enumerate() {
        assert_matches!(
            r, JournalRecord::Command { sequence: seq, .. } if *seq == i as u64,
            "seq stays monotonic across the version upgrade"
        );
    }
}

#[test]
fn base_version_is_30() {
    assert_eq!(super::BASE_VERSION, 30);
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
    j.write_fence().wait();
    assert_eq!(stamp(&seg), super::BASE_VERSION, "every segment is the base from its first byte");
    let written = String::from_utf8_lossy(&std::fs::read(&seg).unwrap()).into_owned();
    assert!(!written.contains("strategy_file"), "a row naming no file writes no key");

    let file = crate::JournalStrategyFile { path: "grid.rhai".into(), sha256: "ab".repeat(32) };
    let seq = j.append_strategy_file_mounted(2, "m", &file).unwrap();
    j.write_fence().wait();
    assert_eq!(stamp(&seg), super::STORED_WORDS_VERSION);
    assert_eq!(super::STORED_WORDS_VERSION, 30);
    drop(j);
    assert_eq!(Journal::latest_segment_version(&dir).unwrap(), Some(30));
    let back = Journal::read_all(&dir).unwrap();
    assert_matches!(
        &back[1],
        JournalRecord::StrategyFileMounted { sequence: s, now_ms: 2, mount_id, strategy_file }
            if *s == seq && mount_id == "m" && *strategy_file == file
    );
}

/// The floor is still a hard wall in the other direction: a version BELOW
/// `MIN_READABLE_VERSION` is a shape this build cannot parse, so it must fail loudly rather
/// than drop records as a torn tail.
#[test]
fn a_version_below_the_supported_floor_is_still_an_explicit_error() {
    let dir = tmp_dir("version-below-floor");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    j.append_command(1_000, &ingest(0)).unwrap();
    j.write_fence().wait();
    drop(j);
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&(super::MIN_READABLE_VERSION - 1).to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();
    assert_eq!(
        Journal::read_all(&dir).unwrap_err().kind(),
        io::ErrorKind::InvalidData,
        "a pre-floor segment is an explicit InvalidData error, not a silent mis-parse"
    );
}

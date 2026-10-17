//! The version-compat tests: the stamped version, the readable range and both refusal edges.

use super::*;
use crate::segment::seg_path;
use crate::testutil::*;
use crate::{CommandJournal, JournalFileConfig, JournalRecord};
use std::assert_matches;

#[test]
fn version_mismatch_on_read_is_an_explicit_error() {
    let dir = tmp_dir("version");
    let cfg = small_cfg();
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    for i in 0..5 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    // Overwrite the header VERSION (bytes[4..8]) with a future version. MAGIC still matches, so
    // this is NOT a foreign file (skipped) nor a torn tail (dropped) — it is OUR journal in an
    // unknown format, which must surface LOUDLY rather than silently mis-parse.
    // `seg_files` (not the first `read_dir` entry) — the dir also holds the `LOCK` sentinel.
    let seg = seg_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();
    let err = CommandJournal::read_all(&dir).unwrap_err();
    assert_eq!(
        err.kind(),
        io::ErrorKind::InvalidData,
        "a MAGIC-matching, VERSION-mismatching segment is an explicit InvalidData error"
    );
}

/// The WRITE-RESUME twin of `version_mismatch_on_read_is_an_explicit_error` above: that one
/// exercises the READ path (`read_all`); this one exercises the branch a live restart actually
/// hits FIRST — `CommandJournal::open` → `map_segment`'s "resuming an EXISTING segment" arm
/// (~line 227) — which must refuse to append this build's frames into a segment stamped with an
/// UNSUPPORTED format version, rather than silently reinterpreting/overwriting it. Uses a
/// FORWARD version (`VERSION + 1`), the case that stays a hard error under the compat range;
/// the supported-older case is covered by
/// `resuming_an_older_supported_version_upgrades_the_stamp_and_appends`. Hand-write a segment
/// header (MAGIC matches, version does not) with no `CommandJournal` in the loop, since going
/// through `open`/`append_*` would only ever stamp the CURRENT `VERSION`.
#[test]
fn open_refuses_to_resume_a_segment_with_an_unsupported_version() {
    let dir = tmp_dir("write-resume-version");
    let cfg = small_cfg();
    let seg = seg_path(&dir, 0);
    let mut header = vec![0u8; HEADER];
    header[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    header[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
    header[8..16].copy_from_slice(&0u64.to_le_bytes());
    std::fs::write(&seg, &header).unwrap();

    // `CommandJournal` holds an `MmapMut` (no `Debug` impl), so `unwrap_err()` isn't available
    // here the way it is on `read_all`'s `io::Result<Vec<JournalRecord>>` above — match instead.
    match CommandJournal::open(&dir, cfg) {
        Ok(_) => panic!(
            "resuming a MAGIC-matching, VERSION-mismatching segment must fail loudly on open, \
                 not silently truncate/reinterpret it"
        ),
        Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidData),
    }
}

#[test]
fn version_is_19() {
    assert_eq!(super::VERSION, 19);
}

/// Pins the COMPATIBILITY RANGE, not just the write version (see the module doc's "Version
/// compatibility contract"). If a future step stops being purely additive it must raise
/// `MIN_READABLE_VERSION` — and this test is the tripwire that forces that to be a deliberate,
/// reviewed edit rather than a silent widening.
#[test]
fn readable_version_range_is_4_to_19() {
    assert_eq!(super::MIN_READABLE_VERSION, 4);
    assert_eq!(super::VERSION, 19);
    // NB: the floor <= write-version invariant is pinned by the two assert_eq! above; a
    // direct `assert!(MIN_READABLE_VERSION <= VERSION)` is const-foldable and trips
    // clippy::assertions_on_constants under -D warnings.
    assert!(super::check_version(4).is_ok(), "v4 (MintedSubmit era) journals stay readable");
    assert!(super::check_version(5).is_ok(), "v5 (PortfolioSnap era) journals stay readable");
    assert!(super::check_version(6).is_ok(), "v6 (ConditionalArmed/Fire era) stays readable");
    assert!(super::check_version(7).is_ok(), "v7 (ConditionalDisarmed era) stays readable");
    assert!(super::check_version(8).is_ok(), "v8 (Snap.conditionals era) stays readable");
    assert!(super::check_version(9).is_ok(), "v9 (MarginCallLiquidate era) stays readable");
    assert!(super::check_version(10).is_ok(), "v10 (GtdExpire era) stays readable");
    assert!(super::check_version(11).is_ok(), "v11 (Snap.contingencies era) stays readable");
    assert!(super::check_version(12).is_ok(), "v12 (ScheduleFire era) stays readable");
    assert!(super::check_version(13).is_ok(), "v13 (Snap.mount_attr era) stays readable");
    assert!(
        super::check_version(14).is_ok(),
        "v14 (MarginCallLiquidate.mount_id era) stays readable"
    );
    assert!(super::check_version(15).is_ok(), "v15 (AccountState.route_key era) stays readable");
    assert!(
        super::check_version(16).is_ok(),
        "v16 (the write-ahead records' own route_key) journals stay readable"
    );
    assert!(
        super::check_version(17).is_ok(),
        "v17 (the Snap's engine and arm route keys) journals stay readable"
    );
    assert!(
        super::check_version(18).is_ok(),
        "v18 (Ingest::GatewayEvent and Snap.gateway_cursors) journals stay readable"
    );
    assert!(
        super::check_version(19).is_ok(),
        "v19 (Command::AdmitSymbol) is what this build writes"
    );
    assert!(super::check_version(3).is_err(), "below the floor is a loud error");
    assert!(super::check_version(20).is_err(), "a FORWARD version is a loud error");
}

/// A v4 journal directory (written before `PortfolioSnap` existed) must READ BACK under this
/// v5 build — the whole point of the additive-only rule. Before the compat range this returned
/// `InvalidData` and an operator had to discard a live journal to take the upgrade.
#[test]
fn an_older_supported_version_journal_reads_back_intact() {
    let dir = tmp_dir("version-back-compat-read");
    let cfg = small_cfg();
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    for i in 0..5 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    // Restamp the header to the OLDEST supported version. The frames themselves are untouched
    // — which is exactly the real-world shape of a v4 segment: `Cmd` records only, since v4
    // could not write `PortfolioSnap`.
    let seg = seg_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&super::MIN_READABLE_VERSION.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();

    let back = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 5, "every v4 record is readable by this v5 build");
    for (i, r) in back.iter().enumerate() {
        assert_matches!(r, JournalRecord::Cmd { seq, .. } if *seq == i as u64);
    }
}

/// The WRITE-RESUME twin: restarting a v5 binary on a v4 directory must APPEND to it (the live
/// crash-restore path), upgrading the header stamp in place, and the merged old+new record
/// stream must read back in seq order.
#[test]
fn resuming_an_older_supported_version_upgrades_the_stamp_and_appends() {
    let dir = tmp_dir("version-back-compat-resume");
    let cfg = small_cfg();
    let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
    for i in 0..3 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let seg = seg_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&super::MIN_READABLE_VERSION.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();

    // Restart onto the v4 directory: must open (not error), resume the seq, and append.
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    for i in 3..6 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    j.flush().unwrap();
    drop(j);

    let stamped = u32::from_le_bytes(std::fs::read(&seg).unwrap()[4..8].try_into().unwrap());
    assert_eq!(
        stamped,
        super::BASE_VERSION,
        "resuming a supported older segment restamps it to the base version, not past it"
    );
    let back = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 6, "pre-upgrade and post-upgrade records coexist in one segment");
    for (i, r) in back.iter().enumerate() {
        assert_matches!(
            r, JournalRecord::Cmd { seq, .. } if *seq == i as u64,
            "seq stays monotonic across the version upgrade"
        );
    }
}

#[test]
fn base_version_is_18() {
    assert_eq!(super::BASE_VERSION, 18);
}

/// The header version of a segment file.
fn stamp(seg: &std::path::Path) -> u32 {
    u32::from_le_bytes(std::fs::read(seg).unwrap()[4..8].try_into().unwrap())
}

/// What the build this one replaces (reads `MIN_READABLE_VERSION..=BASE_VERSION`) does with a
/// header: the rollback target's own `check_version`.
fn base_build_reads(version: u32) -> bool {
    (super::MIN_READABLE_VERSION..=super::BASE_VERSION).contains(&version)
}

fn admit_symbol() -> vike_exec::Ingest {
    vike_exec::Ingest::Command(vike_exec::Command::AdmitSymbol(Box::new(
        vike_exec::SymbolAdmission {
            route_key: "binance#alt".into(),
            symbol: "ETHUSDT".into(),
            grid: vike_model::SymbolGrid {
                tick_size: Some(0.01),
                lot_size: Some(0.0001),
                ..vike_model::SymbolGrid::default()
            },
            contract_value: None,
            initial_mark: None,
        },
    )))
}

/// The rollback promise: starting this build on a v18 journal (and appending everything but an
/// `AdmitSymbol`) leaves the header at 18, so the build it replaced still reads the directory. The
/// header rises to 19 on the first `AdmitSymbol`, survives a restart, and from then on the older
/// build refuses the directory (the one-way door).
#[test]
fn a_v18_journal_stays_18_until_the_first_admit_symbol() {
    let dir = tmp_dir("lazy-stamp");
    let cfg = small_cfg();
    // A v18 journal as the build before this one left it, then a restamp-from-17 for good measure.
    let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
    j.append_cmd(1_000, &ingest(0)).unwrap();
    drop(j);
    let seg = seg_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&17u32.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();

    let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
    assert_eq!(stamp(&seg), 18, "a v17 header is raised to the base version at open, no further");
    for i in 1..4 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    assert_eq!(stamp(&seg), 18, "ordinary appends never raise the header");
    assert!(base_build_reads(stamp(&seg)), "so a rollback onto the previous build still reads it");

    let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
    assert_eq!(stamp(&seg), 18, "a second start changes nothing either");
    j.append_cmd(2_000, &admit_symbol()).unwrap();
    assert_eq!(stamp(&seg), 19, "the stamp is in the file BEFORE the AdmitSymbol returns");
    drop(j);
    assert!(!base_build_reads(stamp(&seg)), "the previous build refuses a journal holding one");

    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_cmd(3_000, &ingest(9)).unwrap();
    drop(j);
    assert_eq!(stamp(&seg), 19, "a restart never lowers the header");
    assert_eq!(CommandJournal::latest_segment_version(&dir).unwrap(), Some(19));
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 6, "every record reads back");
}

/// A segment rolled after the raise is v19 from its first byte (a `Snap` carrying admitted-symbol
/// rows lands in a later segment once the `AdmitSymbol` frame has been pruned), and the segments
/// before the raise stay as they were.
#[test]
fn a_roll_after_the_first_admit_symbol_stamps_the_new_segment_19() {
    let dir = tmp_dir("lazy-stamp-roll");
    let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    for i in 0..200 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    j.append_cmd(5_000, &admit_symbol()).unwrap();
    for i in 0..200 {
        j.append_cmd(6_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let mut segs = seg_files(&dir);
    segs.sort(); // names are zero-padded, `read_dir` order is not
    let stamps: Vec<u32> = segs.iter().map(|s| stamp(s)).collect();
    assert!(stamps.len() >= 3, "the test needs several segments, got {stamps:?}");
    assert_eq!(stamps[0], 18, "a segment closed before the raise keeps its stamp: {stamps:?}");
    assert_eq!(*stamps.last().unwrap(), 19, "{stamps:?}");
    assert!(stamps.windows(2).all(|w| w[0] <= w[1]), "never lowered along the chain: {stamps:?}");
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 401);
}

/// The floor is still a hard wall in the other direction: a version BELOW
/// `MIN_READABLE_VERSION` is a shape this build cannot parse, so it must fail loudly rather
/// than drop records as a torn tail.
#[test]
fn a_version_below_the_supported_floor_is_still_an_explicit_error() {
    let dir = tmp_dir("version-below-floor");
    let cfg = small_cfg();
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    j.append_cmd(1_000, &ingest(0)).unwrap();
    drop(j);
    let seg = seg_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[4..8].copy_from_slice(&(super::MIN_READABLE_VERSION - 1).to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();
    assert_eq!(
        CommandJournal::read_all(&dir).unwrap_err().kind(),
        io::ErrorKind::InvalidData,
        "a pre-floor segment is an explicit InvalidData error, not a silent mis-parse"
    );
}

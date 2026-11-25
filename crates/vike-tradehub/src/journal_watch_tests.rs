//! [`JournalWatch`]: a journal growing past the growth row fires ONE alert, a quiet one none, one past the size row
//! at its first measure and again only after it went back under; the two rows set the thresholds; every measure lands
//! in the frame's cell, and a directory that does not read leaves it empty.

use super::*;
use std::fs::File;
use vike_alerting::{AlertRule, AlertRuleSet, FiredAlert, RuleTrigger};

const SEGMENT: u64 = 64 * MIB;
const MINUTE_MS: i64 = 60_000;

/// Put the `index`th 64 MiB segment in `dir`, sparse, so the test writes no data.
fn segment(dir: &Path, index: u64) {
    File::create(dir.join(format!("journal-{index:08}.vjl"))).unwrap().set_len(SEGMENT).unwrap();
}

fn journal_rule_mount() -> crate::alerts::AlertMount {
    let rules = AlertRuleSet {
        rules: vec![AlertRule::new("journal", RuleTrigger::JournalGrowth)],
        ..Default::default()
    };
    crate::alerts::maybe_mount_with_sinks(Some(rules), Vec::new()).expect("an enabled rule mounts")
}

/// Tick once a minute for `minutes` minutes, a segment added every `every` minutes before the tick, and hand
/// each crossing to `mount`: the minute of each reading, and what fired.
fn run(
    watch: &mut JournalWatch,
    dir: &Path,
    minutes: i64,
    every: i64,
    mount: &mut crate::alerts::AlertMount,
) -> (Vec<(i64, JournalReading)>, Vec<FiredAlert>) {
    let (mut readings, mut fired) = (Vec::new(), Vec::new());
    segment(dir, 0);
    for minute in 0..=minutes {
        if minute > 0 && minute % every == 0 {
            segment(dir, minute.unsigned_abs());
        }
        let now = minute * MINUTE_MS;
        if let Some(reading) = watch.tick(now) {
            fired.extend(mount.on_journal_growth(&reading, now));
            readings.push((minute, reading));
        }
    }
    (readings, fired)
}

/// A segment a minute (3840 MiB an hour) is told once, at the first measure whose window is long enough, while the
/// size is still under its row; the ticks after it, still growing, tell nothing.
#[test]
fn a_journal_growing_past_the_growth_row_fires_one_alert() {
    let dir = tempfile::tempdir().unwrap();
    let mut watch = JournalWatch::new(dir.path().to_path_buf(), None, None);
    let mut mount = journal_rule_mount();
    let (readings, fired) = run(&mut watch, dir.path(), 20, 1, &mut mount);
    let reading = JournalReading {
        total_bytes: 16 * SEGMENT,
        segments: 16,
        bytes_per_hour: Some(3_840 * MIB),
    };
    assert_eq!(readings, [(GROWTH_MIN_SPAN_MS / MINUTE_MS, reading)]);
    let bodies: Vec<&str> = fired.iter().map(|alert| alert.body.as_str()).collect();
    assert_eq!(bodies, ["journal at 1024.0 MiB in 16 segment(s), growing 3840.0 MiB an hour"]);
}

/// A segment every 20 minutes (192 MiB an hour) over three hours stays under the default growth row, and its
/// 640 MiB under the size row: nothing is told.
#[test]
fn a_journal_under_both_rows_is_never_told() {
    let dir = tempfile::tempdir().unwrap();
    let mut watch = JournalWatch::new(dir.path().to_path_buf(), None, None);
    let mut mount = journal_rule_mount();
    let (readings, fired) = run(&mut watch, dir.path(), 180, 20, &mut mount);
    assert!(readings.is_empty(), "{readings:?}");
    assert!(fired.is_empty());
}

/// The size row is read: a 128 MiB journal is over a 100 MiB row at its first measure (no rate yet) and under the
/// default one. Still over, it tells nothing more; back under, it re-arms, and the next crossing is told again.
#[test]
fn the_size_row_is_read_and_a_crossing_is_told_again_only_after_going_back_under() {
    let dir = tempfile::tempdir().unwrap();
    segment(dir.path(), 0);
    segment(dir.path(), 1);
    let mut default_rows = JournalWatch::new(dir.path().to_path_buf(), None, None);
    assert_eq!(default_rows.tick(0), None);

    let mut watch = JournalWatch::new(dir.path().to_path_buf(), Some(100), None);
    let cell = watch.reading();
    let shown = || *cell.lock().unwrap();
    let over = Some(JournalReading { total_bytes: 2 * SEGMENT, segments: 2, bytes_per_hour: None });
    assert_eq!(watch.tick(0), over);
    let frame_over =
        JournalWire { total_bytes: 2 * SEGMENT, segments: 2, bytes_per_hour: None, over: true };
    assert_eq!(shown(), Some(frame_over), "every measure is on the frame");
    assert_eq!(watch.tick(MINUTE_MS), None, "still over: told once");
    assert_eq!(shown(), Some(frame_over), "and stays over on the frame");
    std::fs::remove_file(dir.path().join("journal-00000001.vjl")).unwrap();
    assert_eq!(watch.tick(2 * MINUTE_MS), None, "back under");
    let frame_under =
        JournalWire { total_bytes: SEGMENT, segments: 1, bytes_per_hour: None, over: false };
    assert_eq!(shown(), Some(frame_under));
    segment(dir.path(), 1);
    assert_eq!(watch.tick(3 * MINUTE_MS), over, "a new crossing");
}

/// The journal's archive (decision 0304) is not the live journal: a full archive beside one segment is no crossing.
#[test]
fn the_archive_does_not_count_toward_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    segment(dir.path(), 5);
    let archive = dir.path().join(vike_journal::ARCHIVE_DIR);
    std::fs::create_dir(&archive).unwrap();
    for index in 0..4 {
        File::create(archive.join(format!("journal-{index:08}.vjl.gz")))
            .unwrap()
            .set_len(SEGMENT)
            .unwrap();
    }
    let mut watch = JournalWatch::new(dir.path().to_path_buf(), Some(100), None);
    assert_eq!(
        watch.tick(0),
        None,
        "64 MiB live, under the 100 MiB row; the 256 MiB archive is not counted"
    );
}

/// A journal directory that does not read is no reading, and no panic.
#[test]
fn a_directory_that_does_not_read_is_no_reading() {
    let dir = tempfile::tempdir().unwrap();
    let mut watch = JournalWatch::new(dir.path().join("missing"), Some(1), Some(1));
    assert_eq!(watch.tick(0), None);
    assert_eq!(watch.tick(MINUTE_MS), None);
    assert_eq!(*watch.reading().lock().unwrap(), None, "nothing on the frame");
}

/// A directory that stops reading takes its last measure off the frame: a stale size is not a reading.
#[test]
fn a_directory_that_stops_reading_takes_its_measure_off_the_frame() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    segment(&journal, 0);
    let mut watch = JournalWatch::new(journal.clone(), None, None);
    assert_eq!(*watch.reading().lock().unwrap(), None, "not measured yet");
    watch.tick(0);
    let measured =
        JournalWire { total_bytes: SEGMENT, segments: 1, bytes_per_hour: None, over: false };
    assert_eq!(*watch.reading().lock().unwrap(), Some(measured));
    std::fs::remove_dir_all(&journal).unwrap();
    watch.tick(MINUTE_MS);
    assert_eq!(*watch.reading().lock().unwrap(), None);
}

//! **The data plane's per-box tuning rows, through the doors an operator and a daemon use** —
//! `vike-cli config set` (`vike_config::write_setting_row`) and the loader a daemon boots through
//! (`load_with_source`), over a real settings database for the first and hand-built rows for the
//! second. The unit tests in `crates/vike-config/src/config_tests.rs` hold the bounds one layer
//! down; this file holds what only the doors can show:
//!
//! * [`each_row_written_by_config_set_lands_loads_and_shows_from_the_db`] — the round trip: a write
//!   lands one row, the loader resolves it, and `config show` attributes it to `db`;
//! * [`a_write_outside_its_range_is_refused_and_leaves_the_database_byte_identical`] and
//!   [`a_stored_row_outside_its_range_marks_the_load`] — the same bound at the WRITE and at the
//!   LOAD, both naming the key and the range, neither clamping;
//! * [`a_non_numeric_row_is_refused_by_name_at_the_write_and_at_the_load`] — a word, a fraction, a
//!   negative number or a boolean is no count of seconds, and nothing reads it as one;
//! * [`a_box_without_the_rows_shows_every_one_unset_from_the_default`] — the box that never heard of
//!   the keys resolves them to nothing, which is what each reader reads as its compiled-in value;
//! * [`every_row_has_a_reader_the_consumption_gate_can_check`] — `config show` says the key is READ.

use std::ops::RangeInclusive;
use std::time::Duration;

use vike_config::config::{
    DATAHUB_CATALOG_TTL_SECS, DATAHUB_CHAIN_CADENCE_SECS, DATAHUB_OPTIONS_POLL_SECS,
    RECORDER_FLUSH_MAX_AGE_SECS, RECORDER_FLUSH_MAX_ROWS, RECORDER_SILENT_SECS, RECORDER_TICK_SECS,
};
use vike_config::consumed::{ReadStatus, read_status};
use vike_config::{
    CliOverrides, Config, Origin, RowPlanError, StoreLayer, describe_with_source, load_with_source,
    write_setting_row,
};
use vike_secrets::{Adoption, SettingRow, StoredSettings};

/// How a test reads a field back out of a resolved [`Config`].
type Getter = fn(&Config) -> Option<u32>;

/// One row under test: its dotted key, its range, a value inside it, and the field it fills.
struct Row {
    key: &'static str,
    range: RangeInclusive<u32>,
    legal: u32,
    get: Getter,
}

impl Row {
    /// The key without its section, as the loader's refusals spell it.
    fn leaf(&self) -> &'static str {
        self.key.strip_prefix("config.").expect("a config key")
    }
}

/// A function, because its arguments are where a closure becomes the `fn` pointer [`Row`] holds.
fn row(key: &'static str, range: RangeInclusive<u32>, legal: u32, get: Getter) -> Row {
    Row { key, range, legal, get }
}

/// The seven rows. A row added for the data plane belongs here.
fn rows() -> Vec<Row> {
    vec![
        row("config.datahub_catalog_ttl_secs", DATAHUB_CATALOG_TTL_SECS, 3_600, |c| {
            c.datahub_catalog_ttl_secs
        }),
        row("config.recorder_silent_secs", RECORDER_SILENT_SECS, 120, |c| c.recorder_silent_secs),
        row("config.recorder_tick_secs", RECORDER_TICK_SECS, 15, |c| c.recorder_tick_secs),
        row("config.recorder_flush_max_rows", RECORDER_FLUSH_MAX_ROWS, 10_000, |c| {
            c.recorder_flush_max_rows
        }),
        row("config.recorder_flush_max_age_secs", RECORDER_FLUSH_MAX_AGE_SECS, 10, |c| {
            c.recorder_flush_max_age_secs
        }),
        row("config.datahub_chain_cadence_secs", DATAHUB_CHAIN_CADENCE_SECS, 120, |c| {
            c.datahub_chain_cadence_secs
        }),
        row("config.datahub_options_poll_secs", DATAHUB_OPTIONS_POLL_SECS, 20, |c| {
            c.datahub_options_poll_secs
        }),
    ]
}

const BUDGET: Duration = Duration::from_millis(500);

/// A store holding one unrelated row, so a candidate resolves and an ordinary write can land.
fn seeded() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temp settings dir");
    vike_secrets::plant_settings_rows(
        dir.path(),
        &StoredSettings {
            settings: vec![SettingRow {
                section: "preferences".to_string(),
                key: "log_level".to_string(),
                value: "\"info\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("a fresh store plants");
    dir
}

/// One `config.<leaf>` row holding the JSON scalar `value`, as the writer would store it.
fn stored(row: &Row, value: &str) -> StoredSettings {
    StoredSettings {
        settings: vec![SettingRow {
            section: "config".to_string(),
            key: row.leaf().to_string(),
            value: value.to_string(),
        }],
        ..Default::default()
    }
}

/// The seal a real write would have left on `rows`: the counts are taken from the rows, because
/// that is exactly what the writer does.
fn seal(rows: &StoredSettings) -> Adoption {
    Adoption {
        adopted_at: "2026-10-13T00:00:00Z".to_string(),
        tool_version: "test".to_string(),
        files_present: String::new(),
        setting_rows: rows.settings.len(),
    }
}

/// The loader's mark for `rows`, or `None` when they load clean.
fn load_mark(dir: &std::path::Path, rows: &StoredSettings) -> (Option<String>, Config) {
    let seal = seal(rows);
    let layer = StoreLayer::Rows { rows, adopted: Some(&seal) };
    let settings = load_with_source(Some(dir), layer, &CliOverrides::default())
        .expect("a row problem MARKS the load, it never returns Err");
    (settings.seal_refusal, settings.config)
}

/// **The round trip**: `config set` lands the row, a daemon's loader resolves it to the value
/// written, and `config show` attributes it to the database with no default beside it.
#[test]
fn each_row_written_by_config_set_lands_loads_and_shows_from_the_db() {
    let dir = seeded();
    for r in rows() {
        let report = write_setting_row(dir.path(), r.key, &r.legal.to_string(), BUDGET)
            .unwrap_or_else(|e| panic!("{}: {e}", r.key));
        assert_eq!(report.new_value, r.legal.to_string(), "{}", r.key);
    }

    let read = vike_secrets::read_settings_in(dir.path());
    let mut scratch = String::new();
    let layer = StoreLayer::of(Some(&read), &mut scratch);

    let settings = load_with_source(Some(dir.path()), layer, &CliOverrides::default())
        .expect("the written store loads");
    assert_eq!(settings.seal_refusal, None, "a store the writer built loads clean");
    for r in rows() {
        assert_eq!(
            (r.get)(&settings.config),
            Some(r.legal),
            "{}: the loader resolves the value written",
            r.key
        );
    }

    let described =
        describe_with_source(Some(dir.path()), layer).expect("config show describes it");
    for r in rows() {
        let shown = described
            .rows
            .iter()
            .find(|s| s.key == r.key)
            .unwrap_or_else(|| panic!("config show has no row for {}", r.key));
        assert_eq!(shown.origin, Origin::Db, "{}: written, so it comes from the database", r.key);
        assert_eq!(shown.value, Some(r.legal.to_string()), "{}", r.key);
        assert_eq!(shown.default, None, "{}: the default is 'unset', not a number", r.key);
        assert!(!shown.adjusted, "{}: no later rule moved it", r.key);
    }
}

/// **The bound at the WRITE**: both ends land, one step outside either is refused with the key and
/// the range named, and a refusal leaves the database byte-identical (it is never clamped).
#[test]
fn a_write_outside_its_range_is_refused_and_leaves_the_database_byte_identical() {
    let dir = seeded();
    let db = vike_secrets::db_path_in(dir.path());
    for r in rows() {
        let (lo, hi) = (*r.range.start(), *r.range.end());
        for edge in [lo, hi] {
            write_setting_row(dir.path(), r.key, &edge.to_string(), BUDGET)
                .unwrap_or_else(|e| panic!("{} = {edge} is inside its range: {e}", r.key));
        }
        let before = std::fs::read(&db).expect("the database");
        for outside in [lo - 1, hi + 1] {
            let err = write_setting_row(dir.path(), r.key, &outside.to_string(), BUDGET)
                .expect_err("a value outside the range must not land");
            assert!(matches!(err, RowPlanError::Refused(_)), "{}: {err:?}", r.key);
            let msg = err.to_string();
            assert!(msg.contains(r.leaf()), "{}: the refusal names the key: {msg}", r.key);
            assert!(
                msg.contains(&format!("allowed range {lo} to {hi}")),
                "{}: …and the range: {msg}",
                r.key
            );
            assert_eq!(std::fs::read(&db).expect("the database"), before, "{}", r.key);
        }
    }
}

/// **The bound at the LOAD**: a stored row outside its range marks the load (which `vike-boot`
/// turns into a refusal to start) naming the key and the range, and the edges load clean.
#[test]
fn a_stored_row_outside_its_range_marks_the_load() {
    let dir = tempfile::tempdir().expect("a temp dir");
    for r in rows() {
        let (lo, hi) = (*r.range.start(), *r.range.end());
        for outside in [lo - 1, hi + 1] {
            let (mark, config) = load_mark(dir.path(), &stored(&r, &outside.to_string()));
            let mark = mark.unwrap_or_else(|| panic!("{} = {outside} must mark the load", r.key));
            assert!(mark.contains(r.leaf()), "{}: the mark names the key: {mark}", r.key);
            assert!(
                mark.contains(&format!("allowed range {lo} to {hi}")),
                "{}: …and the range: {mark}",
                r.key
            );
            assert_eq!((r.get)(&config), None, "{}: a refused row is never applied", r.key);
        }
        for edge in [lo, hi] {
            let (mark, config) = load_mark(dir.path(), &stored(&r, &edge.to_string()));
            assert_eq!(mark, None, "{} = {edge} is inside its range", r.key);
            assert_eq!((r.get)(&config), Some(edge), "{}", r.key);
        }
    }
}

/// **`0` is the silence threshold's one value from outside its range** — the flag's own "watchdog
/// off" — at both doors, and the gap between it and the floor is refused at both.
#[test]
fn the_silence_threshold_takes_zero_at_both_doors() {
    let dir = seeded();
    let r = row("config.recorder_silent_secs", RECORDER_SILENT_SECS, 0, |c| c.recorder_silent_secs);
    write_setting_row(dir.path(), r.key, "0", BUDGET).expect("0 is the watchdog's off switch");
    let (mark, config) = load_mark(dir.path(), &stored(&r, "0"));
    assert_eq!(mark, None);
    assert_eq!(config.recorder_silent_secs, Some(0));

    let db = vike_secrets::db_path_in(dir.path());
    let before = std::fs::read(&db).expect("the database");
    for gap in ["1", "59"] {
        write_setting_row(dir.path(), r.key, gap, BUDGET)
            .expect_err("between 0 and the floor is neither off nor a threshold");
        let (mark, _) = load_mark(dir.path(), &stored(&r, gap));
        assert!(mark.is_some_and(|m| m.contains("recorder_silent_secs")), "{gap}");
    }
    assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");
}

/// **A value that is no count of seconds is refused BY NAME at both doors** — a word, a fraction,
/// a negative number, a boolean — rather than read as zero or as the default.
#[test]
fn a_non_numeric_row_is_refused_by_name_at_the_write_and_at_the_load() {
    let dir = seeded();
    let db = vike_secrets::db_path_in(dir.path());
    let before = std::fs::read(&db).expect("the database");
    for r in rows() {
        // (what the operator types, the JSON scalar it is stored as)
        for (typed, json) in [
            ("abc", "\"abc\""),
            ("1.5", "1.5"),
            ("-5", "-5"),
            ("true", "true"),
            ("\"30\"", "\"30\""),
        ] {
            let err = write_setting_row(dir.path(), r.key, typed, BUDGET)
                .expect_err("a non-numeric value must not land");
            let msg = err.to_string();
            assert!(
                msg.contains(r.leaf()),
                "{} = {typed}: the refusal names the key: {msg}",
                r.key
            );
            assert!(msg.contains("u32"), "{} = {typed}: …and the type it wanted: {msg}", r.key);

            let (mark, config) = load_mark(dir.path(), &stored(&r, json));
            let mark = mark.unwrap_or_else(|| panic!("{} = {json} must mark the load", r.key));
            assert!(mark.contains(r.leaf()), "{} = {json}: the mark names the key: {mark}", r.key);
            assert_eq!((r.get)(&config), None, "{}: never read as a number", r.key);
        }
    }
    assert_eq!(std::fs::read(&db).expect("the database"), before, "no refusal wrote anything");
}

/// **A box that never heard of the keys resolves every one to nothing**, `config show` says so, and
/// that is the whole of "no behaviour change without the row": each reader reads an unset row as
/// its compiled-in value.
#[test]
fn a_box_without_the_rows_shows_every_one_unset_from_the_default() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let described = describe_with_source(Some(dir.path()), StoreLayer::NoDatabase)
        .expect("a box with no database describes");
    for r in rows() {
        let shown = described
            .rows
            .iter()
            .find(|s| s.key == r.key)
            .unwrap_or_else(|| panic!("config show has no row for {}", r.key));
        assert_eq!((&shown.origin, &shown.value, &shown.default), (&Origin::Default, &None, &None));
        assert_eq!((r.get)(&described.settings.config), None, "{}", r.key);
    }
}

/// **Every row has a reader `config show` can name.** The consumption gate
/// (`tests/settings_are_consumed.rs`) opens the file each row cites and finds the needle in it;
/// this is the disclosure side of the same row — never `Unknown`, never admitted unread.
#[test]
fn every_row_has_a_reader_the_consumption_gate_can_check() {
    for r in rows() {
        assert!(
            matches!(read_status(r.key), ReadStatus::Read { .. }),
            "{}: {:?}",
            r.key,
            read_status(r.key)
        );
    }
}

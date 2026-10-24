//! **`config.gatewayhub_connect_wait_ms`, through the doors an operator and a daemon use** —
//! `vike-cli config set` (`vike_config::write_setting_row`) and the loader the tradehub boots
//! through (`load_with_source`), over a real settings database for the first and hand-built rows for
//! the second. `crates/vike-config/src/config_tests.rs` holds the apply and the range one layer
//! down; this file holds what only the doors show, for the one number row that decides how long a
//! tradehub waits for the gatewayhub (decision 0164, D7):
//!
//! * the round trip — a write lands one row, the loader resolves it, `config show` attributes it to
//!   `db`;
//! * a value outside the range is refused at the WRITE (database byte-identical) and marks the
//!   LOAD, naming the key and the bound — and the ends of the range land at both;
//! * a box without the row shows it unset from the default;
//! * the consumption table names a reader for it.

use std::time::Duration;

use vike_config::config::{GATEWAYHUB_CONNECT_WAIT_MAX_MS, GATEWAYHUB_CONNECT_WAIT_MIN_MS};
use vike_config::consumed::{ReadStatus, read_status};
use vike_config::{
    CliOverrides, Origin, RowPlanError, StoreLayer, describe_with_source, load_with_source,
    write_setting_row,
};
use vike_secrets::{Adoption, SettingRow, StoredSettings};

const KEY: &str = "config.gatewayhub_connect_wait_ms";
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

/// The loader's mark for one stored row (`json` is the scalar as stored), and the number it resolved.
fn load_stored(dir: &std::path::Path, json: &str) -> (Option<String>, Option<u32>) {
    let rows = StoredSettings {
        settings: vec![SettingRow {
            section: "config".to_string(),
            key: "gatewayhub_connect_wait_ms".to_string(),
            value: json.to_string(),
        }],
        ..Default::default()
    };
    let seal = Adoption {
        adopted_at: "2026-10-15T00:00:00Z".to_string(),
        tool_version: "test".to_string(),
        files_present: String::new(),
        setting_rows: rows.settings.len(),
    };
    let layer = StoreLayer::Rows { rows: &rows, adopted: Some(&seal) };
    let settings = load_with_source(Some(dir), layer, &CliOverrides::default())
        .expect("a row problem MARKS the load, it never returns Err");
    (settings.seal_refusal, settings.config.gatewayhub_connect_wait_ms)
}

/// **The round trip**: `config set` lands the row, the loader resolves the value written, and
/// `config show` attributes it to the database with no default beside it.
#[test]
fn the_row_written_by_config_set_lands_loads_and_shows_from_the_db() {
    let dir = seeded();
    let report = write_setting_row(dir.path(), KEY, "90000", BUDGET)
        .unwrap_or_else(|e| panic!("{KEY}: {e}"));
    assert_eq!(report.new_value, "90000");

    let read = vike_secrets::read_settings_in(dir.path());
    let mut scratch = String::new();
    let layer = StoreLayer::of(Some(&read), &mut scratch);
    let settings = load_with_source(Some(dir.path()), layer, &CliOverrides::default())
        .expect("the written store loads");
    assert_eq!(settings.seal_refusal, None, "a store the writer built loads clean");
    assert_eq!(settings.config.gatewayhub_connect_wait_ms, Some(90_000));

    let described =
        describe_with_source(Some(dir.path()), layer).expect("config show describes it");
    let shown = described
        .rows
        .iter()
        .find(|s| s.key == KEY)
        .unwrap_or_else(|| panic!("config show has no row for {KEY}"));
    assert_eq!(shown.origin, Origin::Db, "written, so it comes from the database");
    assert_eq!(shown.value.as_deref(), Some("90000"));
    assert_eq!(shown.default, None, "the default is 'unset' — the tradehub's constant answers it");
}

/// **A value outside the range is refused at BOTH doors, by name and bound**, a refused write leaves
/// the database byte-identical, and the two ends land at both.
#[test]
fn a_value_outside_the_range_is_refused_at_the_write_and_the_load() {
    let dir = seeded();
    let db = vike_secrets::db_path_in(dir.path());
    let (min, max) = (GATEWAYHUB_CONNECT_WAIT_MIN_MS, GATEWAYHUB_CONNECT_WAIT_MAX_MS);
    for end in [min, max] {
        write_setting_row(dir.path(), KEY, &end.to_string(), BUDGET)
            .unwrap_or_else(|e| panic!("{KEY} = {end}: the end of the range lands: {e}"));
        let (mark, resolved) = load_stored(dir.path(), &end.to_string());
        assert_eq!(mark, None, "{end}: a legal row loads clean");
        assert_eq!(resolved, Some(end));
    }
    let before = std::fs::read(&db).expect("the database");
    for (outside, bound_word) in [
        (0, format!("minimum {min}")),
        (min - 1, format!("minimum {min}")),
        (max + 1, format!("maximum {max}")),
    ] {
        let err = write_setting_row(dir.path(), KEY, &outside.to_string(), BUDGET)
            .expect_err("a value outside the range must not land");
        assert!(matches!(err, RowPlanError::Refused(_)), "{outside}: {err:?}");
        let text = err.to_string();
        assert!(text.contains("gatewayhub_connect_wait_ms"), "{outside}: names the key: {text}");
        assert!(text.contains(&bound_word), "{outside}: names the bound: {text}");

        let (mark, resolved) = load_stored(dir.path(), &outside.to_string());
        let mark = mark.unwrap_or_else(|| panic!("{outside} must mark the load"));
        assert!(mark.contains("gatewayhub_connect_wait_ms"), "{outside}: {mark}");
        assert!(mark.contains(&bound_word), "{outside}: {mark}");
        assert_eq!(resolved, None, "{outside}: a refused row is never applied");
    }
    assert_eq!(std::fs::read(&db).expect("the database"), before, "no refusal wrote anything");
}

/// **A box that never heard of the key resolves it to nothing**, `config show` says so, and that is
/// the whole of "no behaviour change without the row": the tradehub reads an unset row as the
/// default.
#[test]
fn a_box_without_the_row_shows_it_unset_from_the_default() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let described = describe_with_source(Some(dir.path()), StoreLayer::NoDatabase)
        .expect("a box with no database describes");
    let shown = described
        .rows
        .iter()
        .find(|s| s.key == KEY)
        .unwrap_or_else(|| panic!("config show has no row for {KEY}"));
    assert_eq!((&shown.origin, &shown.value, &shown.default), (&Origin::Default, &None, &None));
    assert_eq!(described.settings.config.gatewayhub_connect_wait_ms, None);
}

/// **The row has a reader `config show` can name.** The consumption gate
/// (`tests/settings_are_consumed.rs`) opens the file the row cites and finds the needle in it; this
/// is the disclosure side of the same row — never `Unknown`, never admitted unread.
#[test]
fn the_row_has_a_reader_the_consumption_gate_can_check() {
    assert!(matches!(read_status(KEY), ReadStatus::Read { .. }), "{:?}", read_status(KEY));
}

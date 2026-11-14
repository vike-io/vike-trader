//! **`config.datahub_options_focus_underlyings` and `config.datahub_options_focus_strikes`, through
//! the doors an operator and a daemon use** — `vike-cli config set` (`vike_config::write_setting_row`)
//! and the loader the data daemon boots through (`load_with_source`). The unit tests in
//! `crates/vike-config/src/config_tests.rs` hold the grammar and the pair rule one layer down; this
//! file holds what only the doors show:
//!
//! * the defaults: a box with neither row resolves both unset, which the options lane's mount reads
//!   as no underlying (the list has no default: owner, 2026-11-13) and
//!   [`DEFAULT_DATAHUB_OPTIONS_FOCUS_STRIKES`];
//! * a set pair round-trips: written, loaded, shown from the database;
//! * the cap: a pair whose instruments exceed [`DATAHUB_OPTIONS_FOCUS_MAX_INSTRUMENTS`] is refused at
//!   the WRITE (the database byte-identical) and marks the LOAD, in words naming the arithmetic;
//! * the consumption table names a reader for both.

use std::time::Duration;

use vike_config::config::{
    DATAHUB_OPTIONS_FOCUS_MAX_INSTRUMENTS, DEFAULT_DATAHUB_OPTIONS_FOCUS_STRIKES,
    parse_options_focus_underlyings,
};
use vike_config::consumed::{ReadStatus, read_status};
use vike_config::write::synthetic_seal;
use vike_config::{
    CliOverrides, Config, Origin, RowPlanError, StoreLayer, describe_with_source, load_with_source,
    write_setting_row,
};
use vike_secrets::{SettingRow, StoredSettings};

use crate::seeded_store::seeded;

const UNDERLYINGS: &str = "config.datahub_options_focus_underlyings";
const STRIKES: &str = "config.datahub_options_focus_strikes";
const BUDGET: Duration = Duration::from_millis(500);

/// The loader's mark for the stored `(leaf, json)` rows, and the config it resolved.
fn load_stored(dir: &std::path::Path, rows: &[(&str, &str)]) -> (Option<String>, Config) {
    let rows = StoredSettings {
        settings: rows
            .iter()
            .map(|(key, value)| SettingRow {
                section: "config".to_string(),
                key: (*key).to_string(),
                value: (*value).to_string(),
            })
            .collect(),
        ..Default::default()
    };
    let seal = synthetic_seal(&rows);
    let layer = StoreLayer::Rows { rows: &rows, adopted: Some(&seal) };
    let settings = load_with_source(Some(dir), layer, &CliOverrides::default())
        .expect("a row problem MARKS the load, it never returns Err");
    (settings.seal_refusal, settings.config)
}

/// **The defaults**: a box with neither row loads clean with both unset; the list has no default, and
/// the one the deploy writes (`docs/ops/datahub-the CI box.md`, `BTC,ETH`) parses by the row's own grammar
/// and fits one ticker feed at the default strikes.
#[test]
fn a_box_without_the_rows_has_both_unset_and_the_defaults_fit_the_cap() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let (mark, config) = load_stored(dir.path(), &[]);
    assert_eq!(mark, None);
    assert_eq!(config.datahub_options_focus_underlyings, None);
    assert_eq!(config.datahub_options_focus_strikes, None);
    let names = parse_options_focus_underlyings("BTC,ETH").expect("the deploy's row parses");
    assert_eq!(names, ["BTC", "ETH"]);
    assert_eq!(DEFAULT_DATAHUB_OPTIONS_FOCUS_STRIKES, 10);
    assert!(
        names.len() as u32 * 4 * DEFAULT_DATAHUB_OPTIONS_FOCUS_STRIKES
            <= DATAHUB_OPTIONS_FOCUS_MAX_INSTRUMENTS
    );
}

/// **A set pair**: `config set` lands both rows, the loader resolves them as written, and `config
/// show` attributes them to the database.
#[test]
fn a_set_pair_is_written_loaded_and_shown_from_the_db() {
    let dir = seeded();
    write_setting_row(dir.path(), UNDERLYINGS, "BTC,ETH,SOL", BUDGET).expect("3 × 4 × 10 = 120");
    write_setting_row(dir.path(), STRIKES, "16", BUDGET).expect("3 × 4 × 16 = 192");

    let read = vike_secrets::read_settings_in(dir.path());
    let mut scratch = String::new();
    let layer = StoreLayer::of(Some(&read), &mut scratch);
    let settings = load_with_source(Some(dir.path()), layer, &CliOverrides::default())
        .expect("the written store loads");
    assert_eq!(settings.seal_refusal, None, "a store the writer built loads clean");
    assert_eq!(settings.config.datahub_options_focus_underlyings.as_deref(), Some("BTC,ETH,SOL"));
    assert_eq!(settings.config.datahub_options_focus_strikes, Some(16));

    let described =
        describe_with_source(Some(dir.path()), layer).expect("config show describes it");
    for key in [UNDERLYINGS, STRIKES] {
        let shown = described.rows.iter().find(|s| s.key == key).expect("a shown row");
        assert_eq!(shown.origin, Origin::Db, "{key}: written, so it comes from the database");
    }
}

/// **The cap at the WRITE**: a pair over one feed's instruments is refused naming the arithmetic, and
/// the database is left byte-identical; the key not written counts at its stored value, so the
/// refusal can fall on either key.
#[test]
fn a_pair_over_the_cap_is_refused_at_the_write_in_words() {
    let dir = seeded();
    let db = vike_secrets::db_path_in(dir.path());
    write_setting_row(dir.path(), UNDERLYINGS, "BTC,ETH", BUDGET).expect("2 × 4 × 10 = 80");
    // 2 (the stored underlyings) × 4 × 33 = 264.
    let before = std::fs::read(&db).expect("the database");
    let err = write_setting_row(dir.path(), STRIKES, "33", BUDGET).expect_err("over the cap");
    assert!(matches!(err, RowPlanError::Refused(_)), "{err:?}");
    let msg = err.to_string();
    assert!(
        msg.contains("datahub_options_focus_strikes")
            && msg.contains("264 instruments")
            && msg.contains(&format!("above the {DATAHUB_OPTIONS_FOCUS_MAX_INSTRUMENTS}")),
        "{msg}"
    );
    assert_eq!(std::fs::read(&db).expect("the database"), before, "nothing landed");

    // At the cap exactly it lands; then a third underlying would be 3 × 4 × 32 = 384.
    write_setting_row(dir.path(), STRIKES, "32", BUDGET).expect("2 × 4 × 32 = 256, the cap");
    let before = std::fs::read(&db).expect("the database");
    let err = write_setting_row(dir.path(), UNDERLYINGS, "BTC,ETH,SOL", BUDGET).expect_err("over");
    let msg = err.to_string();
    assert!(
        msg.contains("datahub_options_focus_underlyings") && msg.contains("384 instruments"),
        "{msg}"
    );
    assert_eq!(std::fs::read(&db).expect("the database"), before, "nothing landed");
}

/// **The cap at the LOAD**: a stored pair over the cap marks the load (which `vike-boot` turns into a
/// refusal to start) naming the arithmetic, and neither row is applied; a malformed list marks it
/// too.
#[test]
fn a_stored_pair_over_the_cap_marks_the_load() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let pair = [
        ("datahub_options_focus_underlyings", "\"BTC,ETH,SOL,XRP\""),
        ("datahub_options_focus_strikes", "20"),
    ];
    let (mark, config) = load_stored(dir.path(), &pair);
    let mark = mark.expect("4 × 4 × 20 = 320 must mark the load");
    assert!(mark.contains("320 instruments") && mark.contains("ticker feed"), "{mark}");
    assert_eq!(config.datahub_options_focus_strikes, None, "a refused pair is never applied");

    let (mark, _) =
        load_stored(dir.path(), &[("datahub_options_focus_underlyings", "\"btc,eth\"")]);
    assert!(mark.is_some_and(|m| m.contains("not an underlying name")), "a lower-case name");
    let (mark, config) =
        load_stored(dir.path(), &[("datahub_options_focus_underlyings", "\"SOL\"")]);
    assert_eq!(mark, None, "one underlying at the default strikes fits");
    assert_eq!(config.datahub_options_focus_underlyings.as_deref(), Some("SOL"));
}

/// Both rows have a reader the consumption gate checks: `config show` says they are READ.
#[test]
fn both_rows_have_a_reader() {
    for key in [UNDERLYINGS, STRIKES] {
        assert!(
            matches!(read_status(key), ReadStatus::Read { .. }),
            "{key}: {:?}",
            read_status(key)
        );
    }
}

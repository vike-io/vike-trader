//! **`config.gatewayhub_addr`, through the doors an operator and a daemon use** —
//! `vike-cli config set` (`vike_config::write_setting_row`) and the loader the gatewayhub boots
//! through (`load_with_source`), over a real settings database for the first and hand-built rows
//! for the second. `crates/vike-config/src/config_tests.rs` holds the apply and the default's
//! separation one layer down; this file holds what only the doors show, for the one address row
//! this crate carries for a daemon whose bind guard refuses a non-loopback value:
//!
//! * the round trip — a write lands one row, the loader resolves it, `config show` attributes it to
//!   `db`;
//! * a value that is no `host:port` is refused at the WRITE (database byte-identical) and marks the
//!   LOAD, naming the key — and a NON-LOOPBACK `host:port` is accepted by both, because the
//!   loopback rule is the daemon's bind guard and not this crate's (the classification is
//!   `vike_hub_proto::bind::bind_exposure`'s, a crate above this one);
//! * a box without the row shows it unset from the default, which is the daemon's
//!   [`DEFAULT_GATEWAYHUB_ADDR`];
//! * the consumption table names a reader for it.

use std::time::Duration;

use vike_config::config::DEFAULT_GATEWAYHUB_ADDR;
use vike_config::consumed::{ReadStatus, read_status};
use vike_config::{
    CliOverrides, Origin, RowPlanError, StoreLayer, describe_with_source, load_with_source,
    write_setting_row,
};
use vike_secrets::{Adoption, SettingRow, StoredSettings};

const KEY: &str = "config.gatewayhub_addr";
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

/// The loader's mark for one stored `config.gatewayhub_addr` row (`json` is the scalar as stored),
/// and the address it resolved.
fn load_stored(dir: &std::path::Path, json: &str) -> (Option<String>, Option<String>) {
    let rows = StoredSettings {
        settings: vec![SettingRow {
            section: "config".to_string(),
            key: "gatewayhub_addr".to_string(),
            value: json.to_string(),
        }],
        ..Default::default()
    };
    let seal = Adoption {
        adopted_at: "2026-10-13T00:00:00Z".to_string(),
        tool_version: "test".to_string(),
        files_present: String::new(),
        setting_rows: rows.settings.len(),
    };
    let layer = StoreLayer::Rows { rows: &rows, adopted: Some(&seal) };
    let settings = load_with_source(Some(dir), layer, &CliOverrides::default())
        .expect("a row problem MARKS the load, it never returns Err");
    (settings.seal_refusal, settings.config.gatewayhub_addr)
}

/// **The round trip**: `config set` lands the row, the loader resolves the value written, and
/// `config show` attributes it to the database with no default beside it.
#[test]
fn the_row_written_by_config_set_lands_loads_and_shows_from_the_db() {
    let dir = seeded();
    let report = write_setting_row(dir.path(), KEY, DEFAULT_GATEWAYHUB_ADDR, BUDGET)
        .unwrap_or_else(|e| panic!("{KEY}: {e}"));
    assert!(report.new_value.contains(DEFAULT_GATEWAYHUB_ADDR), "{}", report.new_value);

    let read = vike_secrets::read_settings_in(dir.path());
    let mut scratch = String::new();
    let layer = StoreLayer::of(Some(&read), &mut scratch);
    let settings = load_with_source(Some(dir.path()), layer, &CliOverrides::default())
        .expect("the written store loads");
    assert_eq!(settings.seal_refusal, None, "a store the writer built loads clean");
    assert_eq!(settings.config.gatewayhub_addr.as_deref(), Some(DEFAULT_GATEWAYHUB_ADDR));

    let described =
        describe_with_source(Some(dir.path()), layer).expect("config show describes it");
    let shown = described
        .rows
        .iter()
        .find(|s| s.key == KEY)
        .unwrap_or_else(|| panic!("config show has no row for {KEY}"));
    assert_eq!(shown.origin, Origin::Db, "written, so it comes from the database");
    assert!(
        shown.value.as_deref().is_some_and(|v| v.contains(DEFAULT_GATEWAYHUB_ADDR)),
        "{:?}",
        shown.value
    );
    assert_eq!(shown.default, None, "the default is 'unset' — the daemon's constant answers it");
}

/// **A value that is no `host:port` is refused at BOTH doors, by name**, and a refused write leaves
/// the database byte-identical. A non-loopback `host:port` is NOT refused here: that rule is the
/// daemon's bind guard.
#[test]
fn a_value_that_is_no_host_and_port_is_refused_at_the_write_and_the_load() {
    let dir = seeded();
    let db = vike_secrets::db_path_in(dir.path());
    let before = std::fs::read(&db).expect("the database");
    for typed in ["localhost", "7883"] {
        let err = write_setting_row(dir.path(), KEY, typed, BUDGET)
            .expect_err("a value that is no host:port must not land");
        assert!(matches!(err, RowPlanError::Refused(_)), "{typed}: {err:?}");
        assert!(err.to_string().contains("gatewayhub_addr"), "{typed}: {err}");

        let (mark, resolved) = load_stored(dir.path(), &format!("\"{typed}\""));
        let mark = mark.unwrap_or_else(|| panic!("{typed} must mark the load"));
        assert!(mark.contains("gatewayhub_addr"), "{typed}: the mark names the key: {mark}");
        assert_eq!(resolved, None, "{typed}: a refused row is never applied");
    }
    assert_eq!(std::fs::read(&db).expect("the database"), before, "no refusal wrote anything");

    // The loopback rule is the daemon's: a public `host:port` is a well-formed ROW.
    let (mark, resolved) = load_stored(dir.path(), "\"0.0.0.0:7883\"");
    assert_eq!(mark, None, "this crate does not classify exposure");
    assert_eq!(resolved.as_deref(), Some("0.0.0.0:7883"));
}

/// **A box that never heard of the key resolves it to nothing**, `config show` says so, and that is
/// the whole of "no behaviour change without the row": the daemon reads an unset row as
/// [`DEFAULT_GATEWAYHUB_ADDR`].
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
    assert_eq!(described.settings.config.gatewayhub_addr, None);
}

/// **The row has a reader `config show` can name.** The consumption gate
/// (`tests/settings_are_consumed.rs`) opens the file the row cites and finds the needle in it; this
/// is the disclosure side of the same row — never `Unknown`, never admitted unread.
#[test]
fn the_row_has_a_reader_the_consumption_gate_can_check() {
    assert!(matches!(read_status(KEY), ReadStatus::Read { .. }), "{:?}", read_status(KEY));
}

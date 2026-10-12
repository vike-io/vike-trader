//! The one-row writer and the whole-table replace under test, plus the helpers its siblings share.

use super::*;
use std::assert_matches;

/// A database with the schema on it and the version stamped — what `secrets init` leaves
/// behind, minus the credentials, so these tests never touch a credential path at all.
pub(super) fn planted(dir: &Path) -> PathBuf {
    let db = crate::store_locator::db_path_in(dir);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
    conn.execute_batch(crate::schema::DDL).unwrap();
    conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
    db
}

pub(super) fn rows() -> StoredSettings {
    StoredSettings {
        venue: Vec::new(),
        settings: vec![
            SettingRow {
                section: "policy".into(),
                key: "max_leverage".into(),
                value: "3.0".into(),
            },
            SettingRow {
                section: "preferences".into(),
                key: "log_file_level".into(),
                value: "\"warn\"".into(),
            },
        ],
    }
}

// -----------------------------------------------------------------------------------------
// `write_setting_row_in` — the one-row writer (0086)
// -----------------------------------------------------------------------------------------

/// The validator every test below hands the writer that wants no opinion of its own: it accepts
/// whatever candidate it is offered. A real caller (`vike-config`'s planner) is what actually
/// checks `apply_rows`/`differing_keys`; these tests are about the PRIMITIVE's own
/// mechanics (the transaction, the seal, the refusals it owns), not the validator's.
fn accept_everything(
    _current: &StoredSettings,
    _adoption: Option<&Adoption>,
    _candidate: &StoredSettings,
) -> Result<(), String> {
    Ok(())
}

#[test]
fn a_new_setting_row_is_inserted_and_reported_as_having_no_old_value() {
    let tmp = tempfile::tempdir().unwrap();
    planted(tmp.path());
    let written = write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "policy".into(),
            key: "max_notional_per_order".into(),
            value: "250".into(),
        },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    assert_eq!(written.old_value, None);
    assert_eq!(written.rows.settings.len(), 1);
    assert_eq!(written.rows.settings[0].value, "250");

    // Read back through the ordinary reader: the row really landed.
    let source = read_settings_in(tmp.path()).unwrap();
    let rows = source.rows().unwrap();
    assert_eq!(rows.settings.len(), 1);
    assert_eq!(rows.settings[0].section, "policy");
    assert_eq!(rows.settings[0].key, "max_notional_per_order");
    assert_eq!(rows.settings[0].value, "250");
}

#[test]
fn an_existing_setting_row_is_updated_in_place_and_reports_its_old_value() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();

    let written = write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "5.0".into(),
        },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    assert_eq!(written.old_value.as_deref(), Some("3.0"));

    // The OTHER settings row is untouched — one row changed, and only one: read the whole store back and compare against the fixture with one value patched.
    let mut expected = rows();
    expected.settings[0].value = "5.0".into();
    let source = read_settings_in(tmp.path()).unwrap();
    assert_eq!(source.rows().unwrap(), &expected.sorted());
}

#[test]
fn a_validator_refusal_leaves_the_database_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();
    let before = std::fs::read(&db).unwrap();

    let err = write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "50.0".into(),
        },
        std::time::Duration::from_millis(200),
        |_current, _adoption, _candidate| Err("this candidate does not boot clean".to_string()),
    )
    .unwrap_err();
    assert_matches!(err, RowWriteError::Rejected(_), "{err}");

    let after = std::fs::read(&db).unwrap();
    assert_eq!(before, after, "a refused write must not change one byte on disk");
}

#[test]
fn the_validator_sees_none_before_the_first_seal_and_the_real_seal_after() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();

    write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "flags".into(),
            key: "tradehub_control".into(),
            value: "true".into(),
        },
        std::time::Duration::from_millis(200),
        |_current, adoption, _candidate| {
            assert!(adoption.is_none(), "no seal exists yet");
            Ok(())
        },
    )
    .unwrap();

    write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "flags".into(),
            key: "tradehub_control".into(),
            value: "false".into(),
        },
        std::time::Duration::from_millis(200),
        |_current, adoption, _candidate| {
            let seal = adoption.expect("the previous write must have sealed the store");
            assert_eq!(seal.setting_rows, 3);
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn a_fresh_write_seals_a_store_that_had_no_seal_and_counts_its_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();
    // `write_settings` moves the seal's counts only when a seal already exists (see its own
    // doc) — a freshly planted store has none, so this is the state `write_setting_row_in`
    // seals for the first time.
    assert!(read_settings_in(tmp.path()).unwrap().adoption().is_none());

    write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "flags".into(),
            key: "tradehub_control".into(),
            value: "true".into(),
        },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();

    let source = read_settings_in(tmp.path()).unwrap();
    let seal = source.adoption().expect("the write must have sealed the store");
    assert_eq!(seal.setting_rows, 3); // the two fixture rows plus this one
}

/// `venues_declared` and `arming_rows` are dead since decision 0119: a fresh seal fills them with
/// `0` (they are `NOT NULL` without a default), and a later write never moves them, so a store an
/// older binary sealed keeps the counts that binary wrote.
#[test]
fn the_dead_seal_columns_are_inserted_as_zero_and_never_updated() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    let write = |value: &str| {
        write_setting_row_in(
            tmp.path(),
            RowChange::Setting {
                section: "flags".into(),
                key: "tradehub_control".into(),
                value: value.into(),
            },
            std::time::Duration::from_millis(200),
            accept_everything,
        )
        .unwrap();
    };
    let dead = || -> (i64, i64) {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.query_row(
            "SELECT venues_declared, arming_rows FROM settings_adoption WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };

    write("true");
    assert_eq!(dead(), (0, 0), "a fresh seal fills the dead columns with 0");

    // An older binary's seal: it counted arming rows. The next write must leave them alone.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("UPDATE settings_adoption SET venues_declared = 1, arming_rows = 14", [])
        .unwrap();
    write("false");
    assert_eq!(dead(), (1, 14), "a write moved a dead seal column");
    let seal = read_settings_in(tmp.path()).unwrap().adoption().cloned().unwrap();
    assert_eq!(seal.setting_rows, 1, "the live count still moves");
}

#[test]
fn plant_settings_rows_creates_a_store_from_nothing_and_plants_exactly_the_given_rows() {
    let tmp = tempfile::tempdir().unwrap();
    assert_matches!(read_settings_in(tmp.path()).unwrap(), SettingsSource::NoDatabase { .. });

    plant_settings_rows(tmp.path(), &rows()).unwrap();

    let source = read_settings_in(tmp.path()).unwrap();
    assert_eq!(source.rows().unwrap(), &rows().sorted());
}

#[test]
fn plant_settings_rows_on_an_existing_store_replaces_its_rows() {
    let tmp = tempfile::tempdir().unwrap();
    planted(tmp.path());
    plant_settings_rows(tmp.path(), &rows()).unwrap();
    plant_settings_rows(tmp.path(), &StoredSettings::default()).unwrap();

    let source = read_settings_in(tmp.path()).unwrap();
    assert!(source.rows().unwrap().is_empty());
}

#[test]
fn another_writer_holding_the_store_is_reported_as_busy_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();
    let before = std::fs::read(&db).unwrap();

    let _holder = hold_write_lock(tmp.path());
    let err = write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "9.0".into(),
        },
        std::time::Duration::from_millis(50),
        accept_everything,
    )
    .unwrap_err();
    assert_matches!(err, RowWriteError::Busy, "{err}");
    drop(_holder);

    let after = std::fs::read(&db).unwrap();
    assert_eq!(before, after, "a busy refusal must not change one byte on disk");
}

#[test]
fn no_database_is_refused_without_creating_one() {
    let tmp = tempfile::tempdir().unwrap();
    let err = write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "3.0".into(),
        },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap_err();
    assert_matches!(err, RowWriteError::NoDatabase { .. }, "{err}");
    assert!(!crate::store_locator::db_path_in(tmp.path()).exists());
}

#[test]
fn a_project_with_no_database_reads_as_no_database_and_refuses_a_write() {
    let tmp = tempfile::tempdir().unwrap();
    let found = read_settings_in(tmp.path()).unwrap();
    assert_matches!(found, SettingsSource::NoDatabase { .. }, "{found:?}");
    assert!(found.rows().is_none());

    let err = write_settings_in(tmp.path(), &rows()).unwrap_err();
    assert_matches!(err.kind, DbErrorKind::NoSettingsDatabase, "{err}");
    // ...and it did not create one on the way past. This is the assertion that matters: a
    // settings write that created the store would take every venue to paper.
    assert!(
        !crate::store_locator::db_path_in(tmp.path()).exists(),
        "the refusal must leave NO database behind — its existence is the credential backend's \
             whole choice"
    );
}

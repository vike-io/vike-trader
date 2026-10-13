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

// -----------------------------------------------------------------------------------------
// `RowChange::Remove` — the one DELETE (decision 0161)
// -----------------------------------------------------------------------------------------

fn remove(section: &str, key: &str) -> RowChange {
    RowChange::Remove { section: section.into(), key: key.into() }
}

/// A SEALED store holding the two fixture rows plus `flags.tradehub_control`, its seal at three.
fn sealed_with_three_rows(dir: &Path) -> PathBuf {
    let db = planted(dir);
    write_settings(&db, &rows()).unwrap();
    write_setting_row_in(
        dir,
        RowChange::Setting {
            section: "flags".into(),
            key: "tradehub_control".into(),
            value: "true".into(),
        },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    db
}

/// The raw `setting_rows` column — NOT [`Adoption::setting_rows`], whose reader clamps a negative
/// count to zero and would hide a count driven below it.
fn raw_seal_count(db: &Path) -> i64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row("SELECT setting_rows FROM settings_adoption WHERE id = 1", [], |r| r.get(0))
        .unwrap()
}

/// One text dump of every table a removal of a `setting` row must NOT reach — the node keys, the
/// venue roster, the credentials and the venue settings — so a before/after comparison covers them
/// all at once. A table with no rows dumps as an empty line, which is why the caller plants rows.
fn other_tables(db: &Path) -> String {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut out = String::new();
    for sql in [
        "SELECT group_concat(id || ':' || name || ':' || value, ';') FROM node_key",
        "SELECT group_concat(id || ':' || name, ';') FROM venue",
        "SELECT group_concat(id || ':' || venue_id || ':' || field || ':' || value || ':' || name, ';') \
         FROM credential",
        "SELECT group_concat(id || ':' || venue_id || ':' || tier || ':' || field || ':' || value, ';') \
         FROM venue_setting",
    ] {
        let part: Option<String> = conn.query_row(sql, [], |r| r.get(0)).unwrap();
        out.push_str(&part.unwrap_or_default());
        out.push('\n');
    }
    out
}

/// **A removal deletes exactly the row it names, hands back what it held, moves the seal DOWN by
/// one in the same transaction and touches no other table.**
#[test]
fn a_removal_deletes_exactly_the_named_row_and_moves_the_seal_down_by_one() {
    let tmp = tempfile::tempdir().unwrap();
    let db = sealed_with_three_rows(tmp.path());
    // A row in each table a removal of a `setting` row may not reach — planted AFTER the first
    // write, which is what seeds the venue roster the credential and venue-setting rows point at.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "INSERT INTO node_key (name, value) VALUES ('a-node-key', 'a-node-value'); \
             INSERT INTO credential (venue_id, field, value, name) \
                 VALUES ((SELECT min(id) FROM venue), 'API_KEY', 'a-fake-credential', 'FAKE_API_KEY'); \
             INSERT INTO venue_setting (venue_id, tier, field, value) \
                 VALUES ((SELECT min(id) FROM venue), 'demo', 'SERVER', 'a-server');",
        )
        .unwrap();
    let others_before = other_tables(&db);
    assert!(
        others_before.contains("a-fake-credential") && others_before.contains("a-server"),
        "the fixture must hold a row in each protected table: {others_before}"
    );

    let written = write_setting_row_in(
        tmp.path(),
        remove("policy", "max_leverage"),
        std::time::Duration::from_millis(200),
        |current, adoption, candidate| {
            assert_eq!(adoption.map(|a| a.setting_rows), Some(3), "the validator sees the seal");
            assert_eq!(current.settings.len(), 3);
            assert_eq!(candidate.settings.len(), 2, "the candidate is the store WITHOUT the row");
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(written.old_value.as_deref(), Some("3.0"), "the removed row's value is handed back");
    assert_eq!(written.rows.settings.len(), 2);

    let source = read_settings_in(tmp.path()).unwrap();
    let rows = source.rows().unwrap();
    let left: Vec<(&str, &str)> =
        rows.settings.iter().map(|r| (r.section.as_str(), r.key.as_str())).collect();
    assert_eq!(left, vec![("flags", "tradehub_control"), ("preferences", "log_file_level")]);
    assert_eq!(other_tables(&db), others_before, "a removal changed another table");
    assert_eq!(source.adoption().unwrap().setting_rows, 2, "the seal moved with the table");
    assert_eq!(raw_seal_count(&db), 2);
}

/// A row that is not there is refused by name — and refused BEFORE the validator is asked, with the
/// database byte-identical.
#[test]
fn removing_a_row_the_table_does_not_hold_is_refused_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = sealed_with_three_rows(tmp.path());
    let before = std::fs::read(&db).unwrap();

    let err = write_setting_row_in(
        tmp.path(),
        remove("policy", "no_such_key"),
        std::time::Duration::from_millis(200),
        |_current, _adoption, _candidate| {
            panic!("a removal with no row has nothing for the validator to judge")
        },
    )
    .unwrap_err();
    assert_matches!(err, RowWriteError::NoSuchRow { .. }, "{err}");
    let message = err.to_string();
    assert!(message.contains("policy.no_such_key"), "the refusal names the key: {message}");
    assert!(message.contains("nothing was removed"), "{message}");

    assert_eq!(before, std::fs::read(&db).unwrap(), "a refused removal must not change a byte");
}

/// A validator that refuses stops the removal with the database byte-identical — the row is still
/// there and the seal still says three.
#[test]
fn a_validator_refusal_of_a_removal_leaves_the_database_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let db = sealed_with_three_rows(tmp.path());
    let before = std::fs::read(&db).unwrap();

    let err = write_setting_row_in(
        tmp.path(),
        remove("policy", "max_leverage"),
        std::time::Duration::from_millis(200),
        |_current, _adoption, _candidate| Err("this removal is not allowed".to_string()),
    )
    .unwrap_err();
    assert_matches!(err, RowWriteError::Rejected(_), "{err}");

    assert_eq!(before, std::fs::read(&db).unwrap(), "a refused removal must not change a byte");
    assert_eq!(raw_seal_count(&db), 3);
}

/// **A failure AFTER the DELETE undoes the DELETE.** A trigger planted on the seal makes the
/// seal's `UPDATE` — the statement that runs after the row is gone — fail; the whole transaction
/// must roll back, leaving the row and the seal exactly as they were. This is the atomicity the
/// seal's count depends on: a row gone with the count unmoved is the very state the erase detector
/// refuses a daemon over.
#[test]
fn a_failure_after_the_delete_rolls_the_row_back_with_the_seal() {
    let tmp = tempfile::tempdir().unwrap();
    let db = sealed_with_three_rows(tmp.path());
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER injected_failure BEFORE UPDATE ON settings_adoption \
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .unwrap();

    let err = write_setting_row_in(
        tmp.path(),
        remove("policy", "max_leverage"),
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap_err();
    assert_matches!(err, RowWriteError::Sql(_), "{err}");

    let source = read_settings_in(tmp.path()).unwrap();
    assert_eq!(source.rows().unwrap().settings.len(), 3, "the DELETE was not rolled back");
    assert!(
        source
            .rows()
            .unwrap()
            .settings
            .iter()
            .any(|r| r.section == "policy" && r.key == "max_leverage" && r.value == "3.0"),
        "the removed row did not come back whole"
    );
    assert_eq!(raw_seal_count(&db), 3, "the seal moved although the write failed");
}

/// A store that was never sealed stays unsealed: a removal is an `UPDATE` of the seal, never an
/// upsert, so it does not seal a store whose rows nobody has vouched for.
#[test]
fn a_removal_on_a_never_sealed_store_creates_no_seal() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();
    assert!(read_settings_in(tmp.path()).unwrap().adoption().is_none());

    write_setting_row_in(
        tmp.path(),
        remove("policy", "max_leverage"),
        std::time::Duration::from_millis(200),
        |_current, adoption, _candidate| {
            assert!(adoption.is_none(), "no seal exists to show the validator");
            Ok(())
        },
    )
    .unwrap();

    let source = read_settings_in(tmp.path()).unwrap();
    assert_eq!(source.rows().unwrap().settings.len(), 1);
    assert!(source.adoption().is_none(), "a removal sealed a store that had no seal");
}

/// **The LAST row of a sealed store takes the seal with it**, so the table is the ordinary empty
/// store again — not a seal over zero rows, which `vike-cli config check` reads as "rows erased
/// after the seal". And the next sanctioned write seals the store afresh.
#[test]
fn removing_the_last_row_of_a_sealed_store_leaves_the_ordinary_empty_store() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
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
    assert_eq!(raw_seal_count(&db), 1);

    write_setting_row_in(
        tmp.path(),
        remove("flags", "tradehub_control"),
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    let source = read_settings_in(tmp.path()).unwrap();
    assert!(source.rows().unwrap().settings.is_empty());
    assert!(source.adoption().is_none(), "a seal over zero rows outlived the last row");

    write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "flags".into(),
            key: "tradehub_control".into(),
            value: "false".into(),
        },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    assert_eq!(raw_seal_count(&db), 1, "the next write seals the store afresh");
}

/// …but only when the count agrees: a seal that still says more than the table holds survives the
/// last row's removal, so an emptied table cannot launder a discrepancy into a clean empty store.
#[test]
fn the_last_row_does_not_take_a_seal_whose_count_is_off() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
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
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("UPDATE settings_adoption SET setting_rows = 4", [])
        .unwrap();

    write_setting_row_in(
        tmp.path(),
        remove("flags", "tradehub_control"),
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    assert_eq!(raw_seal_count(&db), 3, "the discrepancy (four over one) is three over none");
    assert!(read_settings_in(tmp.path()).unwrap().adoption().is_some());
}

/// **A removal moves the count by a DELTA, never a recompute, so it cannot launder an unsound
/// seal.** A seal at five over three rows (rows erased by some other route) becomes four over two
/// — still off by two, still refused at the next boot — rather than two over two, which would look
/// sound and bless the erase. And a count already at zero is left at zero, never driven negative.
#[test]
fn a_removal_preserves_a_discrepancy_and_never_drives_the_count_negative() {
    let tmp = tempfile::tempdir().unwrap();
    let db = sealed_with_three_rows(tmp.path());
    let conn = rusqlite::Connection::open(&db).unwrap();

    conn.execute("UPDATE settings_adoption SET setting_rows = 5", []).unwrap();
    write_setting_row_in(
        tmp.path(),
        remove("policy", "max_leverage"),
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    assert_eq!(raw_seal_count(&db), 4, "the seal was recomputed instead of moved by the delta");
    assert_eq!(read_settings_in(tmp.path()).unwrap().rows().unwrap().settings.len(), 2);

    conn.execute("UPDATE settings_adoption SET setting_rows = 0", []).unwrap();
    write_setting_row_in(
        tmp.path(),
        remove("flags", "tradehub_control"),
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    assert_eq!(raw_seal_count(&db), 0, "a count at zero was driven below it");
}

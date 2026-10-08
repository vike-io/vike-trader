//! The `profile_risk` rows.

use super::row_write_tests::planted;
use super::*;

// -----------------------------------------------------------------------------------------
// `profile_risk` — 0057 Phase 2
// -----------------------------------------------------------------------------------------

fn live_profile() -> StoredProfileRisk {
    StoredProfileRisk {
        profile: "run-live.toml".into(),
        rows: vec![
            ProfileRiskRow { key: "max_leverage".into(), value: "3.0".into() },
            ProfileRiskRow { key: "max_notional_per_order".into(), value: "250.0".into() },
            ProfileRiskRow { key: "max_total_exposure".into(), value: "1000.0".into() },
        ],
    }
}

#[test]
fn a_project_with_no_database_reads_as_no_database_and_refuses_a_profile_write() {
    let tmp = tempfile::tempdir().unwrap();
    let found = read_profile_risk_in(tmp.path()).unwrap();
    assert!(matches!(found, ProfileRiskSource::NoDatabase { .. }), "{found:?}");
    assert!(found.profiles().is_none());

    let err = write_profile_risk_in(tmp.path(), &live_profile()).unwrap_err();
    assert!(matches!(err.kind, DbErrorKind::NoSettingsDatabase), "{err}");
    assert!(
        !crate::dotenv::db_path_in(tmp.path()).exists(),
        "the refusal must leave NO database behind — its existence is the credential backend's \
             whole choice"
    );
}

#[test]
fn a_profile_round_trips_and_a_re_mirror_replaces_only_its_own_rows() {
    let tmp = tempfile::tempdir().unwrap();
    planted(tmp.path());

    let live = live_profile();
    let paper = StoredProfileRisk {
        profile: "run-paper.toml".into(),
        rows: vec![ProfileRiskRow { key: "max_leverage".into(), value: "10.0".into() }],
    };
    let first = write_profile_risk_in(tmp.path(), &live).unwrap();
    assert_eq!((first.rows, first.replaced), (3, 0));
    write_profile_risk_in(tmp.path(), &paper).unwrap();

    let found = read_profile_risk_in(tmp.path()).unwrap();
    assert_eq!(found.profiles(), Some(&[live.clone(), paper.clone()][..]));

    // Re-mirroring `run-live.toml` with FEWER keys drops its removed rows...
    let shrunk = StoredProfileRisk {
        profile: "run-live.toml".into(),
        rows: vec![ProfileRiskRow { key: "max_leverage".into(), value: "2.0".into() }],
    };
    let again = write_profile_risk_in(tmp.path(), &shrunk).unwrap();
    assert_eq!((again.rows, again.replaced), (1, 3));
    // ...and leaves the OTHER profile exactly where it was, which is the whole reason this
    // writer is scoped to one profile rather than replacing the table.
    let found = read_profile_risk_in(tmp.path()).unwrap();
    assert_eq!(found.profiles(), Some(&[shrunk, paper][..]));
}

/// A store migrated before Phase 2 says so BY NAME rather than reading as "this profile sets
/// no ceilings", which is the distinction the enum exists for.
#[test]
fn a_store_without_the_table_reads_as_table_absent_rather_than_as_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let db = crate::dotenv::db_path_in(tmp.path());
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
    conn.execute_batch(
        "CREATE TABLE node_key (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
    drop(conn);

    let found = read_profile_risk_in(tmp.path()).unwrap();
    assert!(matches!(found, ProfileRiskSource::TableAbsent { .. }), "{found:?}");
    assert!(found.profiles().is_none());
    assert!(found.to_string().contains("config mirror --profile"), "{found}");

    // ...and a write CREATES it, on the same store, without a schema-version bump.
    let written = write_profile_risk_in(tmp.path(), &live_profile()).unwrap();
    assert!(written.table_created);
    assert_eq!(read_profile_risk_in(tmp.path()).unwrap().profiles().unwrap().len(), 1);
}

/// `UNIQUE (profile, key)` is a gate, not a comment: two rows for one key of one profile are
/// refused by the engine, which is the half a Rust-side replace can never cover.
#[test]
fn the_schema_refuses_a_second_row_for_one_key_of_one_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    let conn = rusqlite::Connection::open(&db).unwrap();
    let insert = |profile: &str, key: &str, value: &str| {
        conn.execute(
            "INSERT INTO profile_risk (profile, key, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![profile, key, value],
        )
    };
    insert("run-live.toml", "max_leverage", "3.0").unwrap();
    assert!(insert("run-live.toml", "max_leverage", "9.0").is_err(), "one row per key");
    // A DIFFERENT profile is a different file and is allowed to carry the same key.
    insert("run-paper.toml", "max_leverage", "9.0").unwrap();
}

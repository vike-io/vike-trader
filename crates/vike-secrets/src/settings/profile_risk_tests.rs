//! The `profile_risk` table's DDL.

use super::row_write_tests::planted;

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

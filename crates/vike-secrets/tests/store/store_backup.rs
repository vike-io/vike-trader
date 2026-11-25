//! **The settings database's nightly copy and its integrity check** (`vike_secrets::back_up_store_in`,
//! `vike_secrets::check_store_in`; the owner's ruling of 2026-11-23).
//!
//! Over a real store: the day's copy is a whole store (its rows and its schema version) with the store's own
//! permissions; a second look the same day writes nothing; the copy past the newest seven is removed while every
//! other file in the folder (an operator's hand copy, an older script's) stays and the store is never touched; and a
//! damaged store fails the check naming the newest copy, and is not copied.

use std::fs;
use std::path::{Path, PathBuf};

use vike_secrets::{
    STORE_BACKUPS_KEPT, SettingRow, StoreBackup, StoreIntegrity, StoredSettings, back_up_store_in,
    check_store_in, db_path_in,
};

/// An instant inside the UTC day `y-m-d`.
fn on(y: i64, m: u32, d: u32) -> i64 {
    vike_utility::calendar::days_from_civil(y, m, d) * vike_model::MS_PER_DAY + 3_600_000
}

/// A project's settings directory holding a store with one settings row and a table of filler pages (so a damaged
/// page is a page of a table, not the header).
fn store() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    vike_secrets::plant_settings_rows(
        &settings,
        &StoredSettings {
            settings: vec![SettingRow {
                section: "config".to_string(),
                key: "tradehub_addr".to_string(),
                value: "\"127.0.0.1:17879\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("a store with one settings row");
    rusqlite::Connection::open(db_path_in(&settings))
        .and_then(|conn| {
            conn.execute_batch(
                "CREATE TABLE filler (id INTEGER PRIMARY KEY, body BLOB); \
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 400) \
                 INSERT INTO filler (body) SELECT randomblob(1000) FROM n;",
            )
        })
        .expect("filler pages");
    (dir, settings)
}

fn backups(settings: &Path) -> PathBuf {
    settings.join("db").join("backups")
}

fn made(outcome: StoreBackup) -> (PathBuf, Vec<PathBuf>) {
    match outcome {
        StoreBackup::Made { copy, removed } => (copy, removed),
        other => panic!("a copy was expected: {other:?}"),
    }
}

/// The store's bytes and its modification time: what "never touched" is measured by.
fn fingerprint(db: &Path) -> (Vec<u8>, std::time::SystemTime) {
    (
        fs::read(db).expect("the store"),
        fs::metadata(db).and_then(|m| m.modified()).expect("its time"),
    )
}

#[test]
fn the_days_copy_is_a_whole_store_and_a_second_look_writes_nothing() {
    let (_dir, settings) = store();
    let (copy, removed) = made(back_up_store_in(&settings, on(2026, 11, 23)).expect("the copy"));
    assert_eq!(copy, backups(&settings).join("vike.db.2026-11-23"));
    assert!(removed.is_empty(), "{removed:?}");
    let conn = rusqlite::Connection::open(&copy).expect("the copy opens");
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("version");
    assert_eq!(version, vike_secrets::SCHEMA_VERSION);
    let value: String = conn
        .query_row("SELECT value FROM setting WHERE key = 'tradehub_addr'", [], |r| r.get(0))
        .expect("the row");
    assert_eq!(value, "\"127.0.0.1:17879\"");
    let fillers: i64 =
        conn.query_row("SELECT count(*) FROM filler", [], |r| r.get(0)).expect("count");
    assert_eq!(fillers, 400);
    drop(conn);
    assert!(!backups(&settings).join("vike.db.partial").exists(), "no copy in progress is left");

    let later_that_day = on(2026, 11, 23) + 20 * 3_600_000;
    assert_eq!(
        back_up_store_in(&settings, later_that_day).expect("a second look"),
        StoreBackup::AlreadyMade(copy)
    );
    assert_eq!(check_store_in(&settings).expect("the check"), StoreIntegrity::Sound);
}

#[test]
fn the_copy_past_the_newest_seven_is_removed_and_the_store_is_never_touched() {
    let (_dir, settings) = store();
    let folder = backups(&settings);
    fs::create_dir_all(&folder).expect("the folder");
    let older: Vec<PathBuf> =
        (15..=21).map(|day| folder.join(format!("vike.db.2026-11-{day}"))).collect();
    for path in &older {
        fs::write(path, b"an older copy").expect("plant");
    }
    // Names that are not this code's copies: an operator's hand copy, an older script's, a stray file.
    let others = ["vike.db.pre-mode-delete-20261013", "vike.db.20261003T000233Z", "notes.txt"]
        .map(|name| folder.join(name));
    for path in &others {
        fs::write(path, b"not a nightly copy").expect("plant");
    }
    let db = db_path_in(&settings);
    let before = fingerprint(&db);

    let (copy, removed) = made(back_up_store_in(&settings, on(2026, 11, 23)).expect("the copy"));
    assert_eq!(removed, vec![older[0].clone()], "the eighth, the oldest, goes");
    assert!(!older[0].exists());
    for kept in older[1..].iter().chain([&copy]) {
        assert!(kept.is_file(), "{} stays", kept.display());
    }
    assert_eq!(older.len(), STORE_BACKUPS_KEPT, "seven planted, so the new one makes eight");
    for other in &others {
        assert_eq!(fs::read(other).expect("untouched"), b"not a nightly copy");
    }
    assert_eq!(fingerprint(&db), before, "the store is never touched");
}

#[test]
fn a_damaged_store_fails_the_check_naming_the_newest_copy_and_is_not_copied() {
    let (_dir, settings) = store();
    let (copy, _) = made(back_up_store_in(&settings, on(2026, 11, 22)).expect("yesterday's copy"));
    let copy_bytes = fs::read(&copy).expect("the copy");

    // The store's last page (a page of `filler`) overwritten, as a torn write or a bad sector leaves it.
    let db = db_path_in(&settings);
    let mut bytes = fs::read(&db).expect("the store");
    let page: i64 = rusqlite::Connection::open(&db)
        .and_then(|c| c.query_row("PRAGMA page_size", [], |r| r.get(0)))
        .expect("page size");
    let end = bytes.len();
    bytes[end - usize::try_from(page).expect("a page size")..].fill(0xFF);
    fs::write(&db, bytes).expect("damage the store");

    let StoreIntegrity::Damaged(damage) = check_store_in(&settings).expect("the check runs") else {
        panic!("a damaged store passed its check");
    };
    assert!(!damage.findings.is_empty());
    assert_eq!(damage.restore_from.as_ref(), Some(&copy), "the copy to restore from is named");

    let StoreBackup::Damaged(again) =
        back_up_store_in(&settings, on(2026, 11, 23)).expect("a look")
    else {
        panic!("a damaged store was copied");
    };
    assert_eq!(again.restore_from.as_ref(), Some(&copy));
    assert!(!backups(&settings).join("vike.db.2026-11-23").exists(), "no copy of a damaged store");
    assert_eq!(fs::read(&copy).expect("the copy"), copy_bytes, "the sound copy stays as it was");
}

#[test]
fn a_store_that_is_no_database_fails_the_check_and_names_no_copy_before_the_first() {
    let (_dir, settings) = store();
    let db = db_path_in(&settings);
    let mut bytes = fs::read(&db).expect("the store");
    bytes[..100].fill(0x5A);
    fs::write(&db, bytes).expect("damage the header");
    let StoreIntegrity::Damaged(damage) = check_store_in(&settings).expect("the check runs") else {
        panic!("a file that is no database passed its check");
    };
    assert_eq!(damage.restore_from, None, "no copy exists yet");
}

#[cfg(unix)]
#[test]
fn the_copy_carries_the_stores_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, settings) = store();
    let db = db_path_in(&settings);
    fs::set_permissions(&db, fs::Permissions::from_mode(0o640)).expect("chmod");
    let (copy, _) = made(back_up_store_in(&settings, on(2026, 11, 23)).expect("the copy"));
    let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&copy), mode(&db), "the store's own mode");
    assert_eq!(mode(&backups(&settings)), 0o700, "the folder is private");
}

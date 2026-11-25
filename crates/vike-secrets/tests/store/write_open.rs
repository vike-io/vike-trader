//! **The store's FILE under its writers** (unit D7 of the error-handling plan): a store the
//! filesystem will not stat is an ERROR, never "no store"; a write against a store that is not there
//! creates nothing, so nothing is ever removed; and a write waits for a lock another connection
//! holds, because every write transaction takes the lock at `BEGIN` (`IMMEDIATE`).

use crate::support::{self, Fixture};

use std::assert_matches;
use std::path::Path;
use std::time::Duration;

use vike_secrets::{CredentialOwner, CredentialUpsertIntent, StoreErrorKind, StoreTable};
use vike_venues::venue_fields::VenueFieldName;

/// No store: each writer refuses with its own word, and the settings directory holds no database and
/// no `db/` directory afterwards. `set_venue_setting_in_unjournalled` opened with SQLite's create
/// flag and ignored what it made, so it CREATED a store here before.
#[test]
fn a_write_against_a_missing_store_creates_nothing() {
    let fx = Fixture::empty();
    let dir = fx.dir();

    let refused = vike_secrets::set_venue_setting_in_unjournalled(
        dir,
        "binance",
        None,
        &VenueFieldName::normalised("server"),
        "demo.example.test",
    )
    .expect_err("no store: the venue-setting write is refused");
    assert_matches!(refused.kind, StoreErrorKind::NoDatabase);
    let refused = vike_secrets::strategy_code::strategy_code_for(dir, "mount", "strategy")
        .expect_err("no store: no code is minted");
    assert_matches!(refused.kind, StoreErrorKind::NoSettingsDatabase);
    let refused = vike_secrets::write_settings_in(dir, &vike_secrets::StoredSettings::default())
        .expect_err("no store: the settings rows are refused");
    assert_matches!(refused.kind, StoreErrorKind::NoSettingsDatabase);
    vike_secrets::order_client_id_sessions::raise_order_client_id_session(dir, "s", 1, 0)
        .expect("no store: nothing to record, and no error");

    assert!(!fx.db().exists(), "a write created the settings database");
    assert!(!dir.join("db").exists(), "a write created the store's directory");
}

/// A store behind a directory this process may not search: `database_present` says it cannot tell,
/// the backend answers `Database` (never `Absent`), and the credential and settings reads refuse
/// LOUDLY instead of answering "not configured". Unix only, and it skips loudly as root, where mode
/// bits do not bite.
#[cfg(unix)]
#[test]
fn a_store_this_process_cannot_stat_is_an_error_not_an_absent_store() {
    use std::os::unix::fs::PermissionsExt;

    /// Gives the directory its search bit back, so the temporary directory can be removed.
    struct Searchable<'a>(&'a Path);
    impl Drop for Searchable<'_> {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
        }
    }

    let fx = Fixture::seeded();
    let db_dir = fx.dir().join("db");
    std::fs::set_permissions(&db_dir, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
    let _searchable = Searchable(&db_dir);
    if std::fs::metadata(fx.db()).is_ok() {
        eprintln!(
            "SKIP: a stat through a 0000 directory SUCCEEDED, so this process ignores mode bits (root)"
        );
        return;
    }

    let refused =
        vike_secrets::database_present(&fx.db()).expect_err("the stat failed: not absent");
    assert_matches!(refused.kind, StoreErrorKind::Io(_));
    assert_eq!(vike_secrets::backend_in(fx.dir()), vike_secrets::StoreBackend::Database(fx.db()));
    support::store_in(fx.dir())
        .read_all()
        .expect_err("an unreachable store must not read as an empty credential map");
    vike_secrets::read_settings_in(fx.dir())
        .expect_err("an unreachable store must not read as `NoDatabase`");
}

/// How long the other connection holds the write lock: well inside the writers' five-second wait.
const HELD: Duration = Duration::from_millis(400);

/// Run `write` while another connection holds the write lock for [`HELD`], and answer what it said.
/// A writer that is still running when the lock is let go WAITED for it; a DEFERRED read-then-write
/// fails busy at once instead (its promotion to the write lock never asks the busy handler).
fn against_a_held_lock<T: Send + 'static>(
    settings: &Path,
    write: impl FnOnce() -> T + Send + 'static,
) -> T {
    let holder = vike_secrets::hold_write_lock(settings);
    let writer = std::thread::spawn(write);
    std::thread::sleep(HELD);
    assert!(!writer.is_finished(), "the write finished while the lock was held: it did not wait");
    drop(holder);
    writer.join().expect("the writer thread")
}

/// **The five read-then-write transactions that were DEFERRED wait for a held lock and then land.**
/// (The sixth, the store's creation, has no store to contend for.)
#[test]
fn a_write_waits_for_a_held_lock_and_lands() {
    let fx = Fixture::seeded();
    let dir = fx.dir().to_path_buf();
    let id = fx.id_of("binance", "demo");

    let at = dir.clone();
    against_a_held_lock(&dir, move || {
        vike_secrets::save_credentials_to_store_unjournalled(
            &at,
            StoreTable::Credential,
            &[("DEPLOYMENT_PAGER_TOKEN".to_string(), "a-token".to_string())],
            CredentialUpsertIntent::Add,
        )
    })
    .expect("the name-addressed upsert waited and landed");

    let at = dir.clone();
    against_a_held_lock(&dir, move || {
        vike_secrets::save_owned_credentials_unjournalled(
            &at,
            &CredentialOwner::Account(id),
            &[("API_KEY".to_string(), "rotated".to_string())],
            CredentialUpsertIntent::Replace,
        )
    })
    .expect("the owner-addressed upsert waited and landed");

    let at = dir.clone();
    against_a_held_lock(&dir, move || {
        vike_secrets::set_venue_setting_in_unjournalled(
            &at,
            "binance",
            None,
            &VenueFieldName::normalised("server"),
            "demo.example.test",
        )
    })
    .expect("the venue-setting upsert waited and landed");

    let at = dir.clone();
    against_a_held_lock(&dir, move || vike_secrets::venue_url::seed_venue_urls_in(&at))
        .expect("the venue address seeding waited and landed");

    let at = dir.clone();
    against_a_held_lock(&dir, move || {
        vike_secrets::write_settings_in(&at, &vike_secrets::StoredSettings::default())
    })
    .expect("the settings rows waited and landed");
}

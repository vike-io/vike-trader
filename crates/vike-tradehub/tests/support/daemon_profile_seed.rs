//! Shared spawn-test scaffolding: seed an ACTIVE daemon-profile ROW from the SAME profile TOML text
//! a spawn test has always written for `--config` — since
//! decision 0086 ("settings live only in the database") verdict 1 retired that file rung, the
//! real binary no longer reads it at all, and the ROW is the only thing that can tell it what to
//! mount.
//!
//! `#[path]`-included by each spawn-test binary that needs it (a bare `mod support;` would resolve
//! against `tests/`, the crate root for a `tests/<name>.rs` binary, not `tests/support/` — the exact
//! reason `tests/daemon.rs`'s own module list is `#[path = "daemon/…"]` rather than a bare `mod`).
//!
//! Not every spawn test needs this: a test that only exercises `--help`/`--version` or a deliberate
//! MISSING-profile refusal never reaches profile resolution and stays as it was. A fixture that ALSO
//! needs node keys seeded into the same store has exactly one consumer
//! (`daemon/audit_reaches_disk.rs`) and seeds its own store inline rather than growing this shared
//! module with a second public entry point only one binary would ever call.

use std::path::Path;

use vike_secrets::profile_store::{OperatorWrite, ProfileKind, set_active, store_profile};
use vike_tradehub::config::DaemonProfile;
use vike_tradehub::profile_rows::daemon_profile_to_rows;

/// Parse `profile_toml` through the SAME production path the daemon takes at boot for a stored
/// profile (`DaemonProfile::from_toml_str` → `profile_rows::daemon_profile_to_rows`), store it under
/// `name` and make it the ACTIVE daemon profile — in a FRESH store under `settings_dir`, created
/// here because every profile-store writer refuses to create one itself
/// (`vike_secrets::profile_store`'s `refuse_absent_store`).
///
/// `settings_dir` need not exist yet. The daemon TOML text a test already wrote for `--config` is
/// exactly what this takes — nothing about the profile's SHAPE changes, only how it reaches the
/// daemon.
///
/// A fixture that also needs node keys seeds them into the SAME store through the one credential
/// writer (`vike_secrets::save_credentials_to_store`), which is what `daemon/audit_reaches_disk.rs`
/// does inline.
pub fn seed_active_daemon_profile(settings_dir: &Path, name: &str, profile_toml: &str) {
    std::fs::create_dir_all(settings_dir).expect("create the settings dir");
    let db = vike_secrets::db_path_in(settings_dir);
    vike_secrets::create_empty_store_for_test(&db).expect("create an empty settings store");

    let profile = DaemonProfile::from_toml_str(profile_toml)
        .unwrap_or_else(|e| panic!("the seeded profile TOML must parse: {e}\n{profile_toml}"));
    let stored = daemon_profile_to_rows(name, &profile)
        .unwrap_or_else(|e| panic!("the seeded profile must lower to rows: {e}"));

    let write = OperatorWrite::claim("vike-tradehub test seed");
    let now_utc = 0i64;
    store_profile(&db, &stored, &write, now_utc, vike_model::AssetClass::SQL_WORDS)
        .unwrap_or_else(|e| panic!("store the seeded profile: {e}"));
    set_active(&db, ProfileKind::Daemon, name, &write, now_utc, vike_model::AssetClass::SQL_WORDS)
        .unwrap_or_else(|e| panic!("activate the seeded profile: {e}"));
}

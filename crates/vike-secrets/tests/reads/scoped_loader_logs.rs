//! **The scoped LOADERS** — `load_workspace_secrets_scoped_at`, `try_load_workspace_secrets_scoped_at`,
//! `load_workspace_secrets_scoped_from_env` and the `warn_once` they log through.
//!
//! `scoped_read.rs` holds the DATA half (the finding survives the narrowing). This file holds the
//! half that LOGS, which lived in `vike_bridge_core::credentials` until the loaders moved down here:
//! a scoped path that skipped the log call would lose the store's permission finding silently, for
//! every binary that scopes.
//!
//! | test | the property |
//! |---|---|
//! | [`an_exposed_store_is_logged_once_by_the_scoped_loaders_and_the_shared_dedupe`] | the permission finding reaches the log from a scoped read, exactly once per process, and `warn_once` (the call `vike_secrets::credentials`' whole-map reads make) shares that one dedupe set |
//! | [`a_clean_store_logs_nothing`] | a 0600 store is silent: no finding, no line |
//! | [`an_unreadable_store_is_loud_in_the_log_and_the_live_gate_in_the_answer`] | a store that EXISTS and will not open is an ERROR line and an empty scope whose declared names read as the live gate, never a silent empty answer |
//! | [`the_env_form_takes_the_settings_directory_from_the_map_and_nothing_else`] | `*_from_env` resolves the store `VIKE_SETTINGS_DIR` names, the same one the `_at` form resolves |
//!
//! ⚠ **Key NAMES only, and no credential name is spelled in this file**: every name comes from
//! `support::FIXTURE_KEYS`, so `crates/vike-model/src/scan.rs` finds no literal here to read as a
//! lookup (the reason `scoped_read.rs` gives for its own bindings). Every value is
//! `support::fake_value`'s derived string, and no assertion prints one.
//!
//! The capture is `vike_log::capture::captured`, the workspace's one scoped `tracing` capture
//! (`crates/vike-ops/tests/hygiene/tracing_capture_gate.rs`). The dedupe set is process-wide, which
//! is why each test seeds its OWN store: the message embeds the store's path, so a sibling test's
//! line cannot swallow this one's.

use std::collections::HashMap;

use tracing::Level;
use vike_log::capture::captured;
use vike_secrets::{KeyScope, Lookup, Source};

use crate::support::{self, FIXTURE_KEYS, Fixture};

/// The first two fixture names: one venue's key and secret, both in the seeded store.
fn scope() -> KeyScope {
    KeyScope::of([FIXTURE_KEYS[0], FIXTURE_KEYS[1]])
}

/// Make the store readable beyond its owner — the one finding there is. Unix only: the mode bits do
/// not exist elsewhere, and `permission_warning` answers `None` there by design.
#[cfg(unix)]
fn widen(fx: &Fixture) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(fx.db(), std::fs::Permissions::from_mode(0o644))
        .expect("widen the store's mode");
}

/// **The permission finding of a scoped read is logged, once per process, through the one dedupe.**
///
/// Three reads of the same exposed store in one capture, by every door the finding has: the
/// infallible loader, the fallible one, and a direct `warn_once` of the finding's text — which is
/// what `vike_secrets::credentials`' whole-map reads do. ONE line comes out of the three. Two
/// dedupe sets (one here, one up there) would print it twice, and a loader that dropped the call
/// would print it never; both reddens this test.
#[cfg(unix)]
#[test]
fn an_exposed_store_is_logged_once_by_the_scoped_loaders_and_the_shared_dedupe() {
    let fx = Fixture::seeded();
    widen(&fx);
    let scope = scope();

    let (finding, events) = captured(|| {
        let first = vike_secrets::load_workspace_secrets_scoped_at(fx.arg(), &scope);
        let second = vike_secrets::try_load_workspace_secrets_scoped_at(fx.arg(), &scope)
            .expect("the store opens");
        assert_eq!(first.warning, second.warning, "both doors carry the same finding as data");
        let finding = first.warning.as_ref().expect("a 0644 store is reported").to_string();
        // The call a whole-map read makes with the same finding.
        vike_secrets::warn_once(&finding);
        finding
    });

    assert_eq!(
        events.len(),
        1,
        "one finding, three doors, ONE line — a second dedupe set prints it twice and a loader \
         that skipped the log call prints it never: {events:?}"
    );
    let line = &events[0];
    assert_eq!(line.level, Level::WARN);
    assert_eq!(line.message, finding, "the line is the finding's own text");
    assert!(line.message.contains("readable beyond its owner"), "{}", line.message);
    assert!(line.message.contains("vike.db"), "it names the store: {}", line.message);
    assert!(!line.message.contains("value-for-"), "paths and a mode, never a value");
}

/// A store only its owner can read has no finding, so a scoped read of it logs nothing at all.
#[test]
fn a_clean_store_logs_nothing() {
    let fx = Fixture::seeded();
    let scope = scope();
    let (_, events) = captured(|| {
        let _ = vike_secrets::load_workspace_secrets_scoped_at(fx.arg(), &scope);
        let _ = vike_secrets::try_load_workspace_secrets_scoped_at(fx.arg(), &scope)
            .expect("the store opens");
    });
    assert!(events.is_empty(), "a clean store is silent: {events:?}");
}

/// **A store that EXISTS and will not open is loud, and its scope reads as the live gate.**
///
/// The infallible loader degrades to an EMPTY scope that keeps its declaration: a declared name
/// answers `AbsentFromStore` (every venue stays paper) while an undeclared one still answers
/// `NotDeclared`, so the two failures stay distinguishable. The ERROR line is what makes the
/// degradation visible; the fallible loader hands the error back and logs nothing itself.
#[test]
fn an_unreadable_store_is_loud_in_the_log_and_the_live_gate_in_the_answer() {
    let fx = Fixture::seeded();
    // A database this code will not read: the schema stamp is not `SCHEMA_VERSION`.
    let conn = fx.conn();
    support::sql::stamp_version(&conn, vike_secrets::SCHEMA_VERSION + 1);
    drop(conn);
    let scope = scope();

    let (scoped, events) =
        captured(|| vike_secrets::load_workspace_secrets_scoped_at(fx.arg(), &scope));
    assert_eq!(scoped.source, Source::None, "nothing answered");
    assert!(scoped.is_empty(), "and nothing is held");
    assert_eq!(scoped.get(FIXTURE_KEYS[0]), Lookup::AbsentFromStore, "declared: the live gate");
    assert!(scoped.get(FIXTURE_KEYS[2]).is_undeclared(), "undeclared stays a scope defect");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].level, Level::ERROR);
    assert!(
        events[0].message.contains("credential store could not be opened")
            && events[0].message.contains("every venue stays paper"),
        "{}",
        events[0].message
    );
    assert!(!events[0].message.contains("value-for-"), "{}", events[0].message);

    let (refused, events) =
        captured(|| vike_secrets::try_load_workspace_secrets_scoped_at(fx.arg(), &scope));
    assert!(refused.is_err(), "the fallible form hands the error back");
    assert!(events.is_empty(), "and logs no error line of its own: {events:?}");
}

/// `*_from_env` takes ONE fact out of the map — the settings directory — and resolves the same
/// store, with the same answer, as the `_at` form handed that directory directly.
#[test]
fn the_env_form_takes_the_settings_directory_from_the_map_and_nothing_else() {
    let fx = Fixture::seeded();
    let env: HashMap<String, String> = [(
        vike_secrets::SETTINGS_DIR_ENV.to_string(),
        fx.dir().to_str().expect("utf-8 temp path").to_string(),
    )]
    .into_iter()
    .collect();
    let scope = scope();

    let via_env = vike_secrets::load_workspace_secrets_scoped_from_env(&env, &scope);
    let via_arg = vike_secrets::load_workspace_secrets_scoped_at(fx.arg(), &scope);
    assert_eq!(via_env.source, Source::Database(fx.db()), "the map named the store");
    assert_eq!(via_env.source, via_arg.source);
    assert_eq!(via_env.len(), 2, "both declared names are in the seeded store");
    assert_eq!(via_env.into_map(), via_arg.into_map(), "the same store answers both forms");
}

//! The store readers: the entry points agreeing with each other and with `vike_secrets`' silent
//! reader, and `StoreHealth`.
//!
//! The silent reader's own unit tests live in `vike_secrets`; what is asserted here is this
//! module's own wrappers and that they answer what `vike_secrets` answers.

use super::*;

#[test]
fn the_secrets_sibling_returns_the_same_shape_and_never_panics() {
    // A CI checkout has no store, so this exercises the absent arm.
    let vars: HashMap<String, String> = load_workspace_secrets_at(None);
    let _n: usize = vars.len();
}

/// The two entry points a binary can take are the SAME resolution: the settings-directory override
/// is the only thing `..._from_env` adds, so an environment that names none is `None`.
#[test]
fn the_env_entry_point_is_the_no_override_entry_point_when_nothing_names_a_directory() {
    assert_eq!(load_workspace_secrets_from_env(&HashMap::new()), load_workspace_secrets_at(None));
}

/// …and so is `vike_secrets`' SILENT reader: the same map, only the findings go unlogged.
#[test]
fn the_logging_reader_is_the_silent_one_when_nothing_names_a_directory() {
    assert_eq!(load_workspace_secrets_at(None), vike_secrets::load_project_secrets(None));
}

/// ⚠ **THE TWO EMPTY MAPS ARE TELLABLE APART.** An ABSENT store and a store that EXISTS and will
/// not open both return zero credentials; [`StoreHealth`] distinguishes them, asserted over the
/// REAL resolver. The unreadable store is bytes that are not a database at the database's path —
/// no permission bit a Windows box (or root) would ignore.
///
/// Reddens on `load_workspace_secrets_at_checked` folding its error arm into
/// `StoreHealth::Readable`, the shape that lets a UI render a measured `0 set` for a store it never
/// opened.
#[test]
fn an_unreadable_store_is_distinguishable_from_an_absent_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    let sd = settings.to_str().expect("utf-8 temp path");

    // ABSENT: no settings database at all. Empty map, and the store ANSWERED.
    let (map, health) = load_workspace_secrets_at_checked(Some(sd));
    assert!(map.is_empty(), "an absent store holds nothing");
    assert_eq!(health, StoreHealth::Readable, "absent is an ANSWER, not a fault");
    assert!(health.is_readable());

    // PRESENT AND UNOPENABLE: same empty map, opposite verdict.
    let db = vike_secrets::db_path_in(&settings);
    std::fs::create_dir_all(db.parent().expect("db dir")).expect("db dir");
    std::fs::write(&db, b"not a sqlite database, and not empty either").expect("plant");
    let (map, health) = load_workspace_secrets_at_checked(Some(sd));
    assert!(map.is_empty(), "the degradation is unchanged — an empty map either way");
    assert!(!health.is_readable(), "…and THAT is the whole point: {health:?}");
    let StoreHealth::Unreadable(why) = &health else { panic!("{health:?}") };
    assert!(
        why.contains(vike_secrets::DB_FILE),
        "the fault is quotable and names the store: {why}"
    );
}

/// The checked reader is the ONE implementation: the infallible wrapper is its `.0`.
#[test]
fn the_infallible_reader_is_the_checked_one_without_its_verdict() {
    assert_eq!(load_workspace_secrets_at(None), load_workspace_secrets_at_checked(None).0);
    let env = HashMap::new();
    assert_eq!(
        load_workspace_secrets_from_env(&env),
        load_workspace_secrets_from_env_checked(&env).0
    );
}

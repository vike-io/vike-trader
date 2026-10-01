use super::*;

#[test]
fn no_env_and_no_store_is_exactly_the_code_defaults() {
    let s = load(None, &HashMap::new()).unwrap();
    assert_eq!(s.policy, Policy::default());
    assert_eq!(s.config, Config::default());
    assert_eq!(s.preferences, Preferences::default());
    assert_eq!(s.flags, Flags::default());
    assert_eq!(s.warnings, vec![NO_SETTINGS_DIRECTORY_WARNING.to_string()]);
}

/// A load handed NO settings directory says so, and a load handed one does not.
///
/// The pair is the whole point: a warning that fires on every load is noise an operator learns
/// to skip, and one that fires on no load is the silence this closes. `docs/ops/kill-switches.md`
/// carried it as a register entry — a process that resolved no project runs on
/// `Policy::default()`, which is no ceiling anywhere, and nothing anywhere said so.
#[test]
fn a_load_with_no_settings_directory_says_so_and_one_with_a_directory_does_not() {
    let none = load(None, &HashMap::new()).unwrap();
    assert!(
        none.warnings.iter().any(|w| w == NO_SETTINGS_DIRECTORY_WARNING),
        "a load that resolved no project must SAY that its ceilings are defaults: {none:?}"
    );

    // A real directory, empty of everything: the distinction is "I found no project", never
    // "your project wrote no ceilings", so a settings directory with no rows must still be
    // silent here.
    let tmp = tempfile::tempdir().unwrap();
    let some = load(Some(tmp.path()), &HashMap::new()).unwrap();
    assert!(
        some.warnings.is_empty(),
        "a settings directory that exists resolves the project, whatever it holds: {some:?}"
    );
}

/// **A DEAD flag's two spellings get two DIFFERENT answers, and both are answers.**
///
/// The row key is a hard refusal naming the key (`Flags::apply`, tested at its own site); the
/// variable is a WARNING and the load succeeds.
#[test]
fn a_dead_flag_refuses_its_row_key_and_warns_about_its_variable() {
    let tmp = tempfile::tempdir().unwrap();
    let rows = vike_secrets::StoredSettings {
        settings: vec![vike_secrets::SettingRow {
            section: "flags".to_string(),
            key: "record_dvol".to_string(),
            value: "true".to_string(),
        }],
        ..Default::default()
    };
    let resolved = load_with_source(
        Some(tmp.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &HashMap::new(),
        &CliOverrides::default(),
    )
    .expect("a row problem MARKS rather than hard-erroring — see `crate::mirror::apply_rows`");
    let refusal = resolved.seal_refusal.expect("the ROW key must be marked illegal");
    assert!(refusal.contains("record_dvol"), "{refusal}");

    // …and the variable, on a store with no such row: the load SUCCEEDS and says so once.
    let clean = tempfile::tempdir().unwrap();
    let env = HashMap::from([(crate::flags::RECORD_DVOL_ENV.to_string(), "1".to_string())]);
    let s = load(Some(clean.path()), &env).expect("the VARIABLE must not fail a load");
    assert!(
        s.warnings.iter().any(|w| w.contains(crate::flags::RECORD_DVOL_ENV)),
        "a set dead variable must be named once: {:?}",
        s.warnings
    );
    // An UNSET variable says nothing — a warning on every load is a warning nobody reads.
    let quiet = load(Some(clean.path()), &HashMap::new()).unwrap();
    assert!(quiet.warnings.is_empty(), "{:?}", quiet.warnings);
}

/// **The venue-catalog flip warns the operator whose value meant OFF, and ONLY that one.**
#[test]
fn the_old_venue_catalog_arming_warns_only_where_its_value_meant_off() {
    let dir = tempfile::tempdir().unwrap();
    let load_with = |v: Option<&str>| {
        let env = v.map_or_else(HashMap::new, |v| {
            HashMap::from([(crate::flags::VENUE_CATALOG_ENV.to_string(), v.to_string())])
        });
        load(Some(dir.path()), &env).expect("a stale variable must never fail a load")
    };
    let warned = |s: &Settings| s.warnings.iter().any(|w| w.contains("venue_catalog_off"));

    // The one operator who is WRONG: `0` turned the lane off and now turns nothing off.
    let off = load_with(Some("0"));
    assert!(warned(&off), "a `0` must be told: {:?}", off.warnings);
    assert!(
        off.warnings.iter().any(|w| w.contains(crate::flags::VENUE_CATALOG_ENV)),
        "…and the warning must name where they wrote it: {:?}",
        off.warnings
    );
    // Any other non-`1` value is the same case.
    assert!(warned(&load_with(Some("true"))), "a truthy typo also meant OFF under the old reader");

    // The operator who is RIGHT: `=1` asked for a served lane and has one.
    let on = load_with(Some("1"));
    assert!(!warned(&on), "a `=1` must be silent: {:?}", on.warnings);
    // A blank is unset, the rule `crate::layers::get` and `refuse_removed_env` both follow.
    let blank = load_with(Some(""));
    assert!(!warned(&blank), "a blank configured nothing then either: {:?}", blank.warnings);
    // …and an unset variable is the ordinary state.
    assert!(!warned(&load_with(None)));
}

/// **The REFUSAL resolves from the environment, and defaults to serving.**
#[test]
fn the_venue_catalog_refusal_resolves_from_the_environment_and_defaults_off() {
    let clean = tempfile::tempdir().unwrap();
    assert!(
        !load(Some(clean.path()), &HashMap::new()).unwrap().flags.venue_catalog_off,
        "the guarded state is `false` — the lane SERVES unless refused"
    );

    let env = HashMap::from([(crate::flags::VENUE_CATALOG_OFF_ENV.to_string(), "1".to_string())]);
    assert!(load(Some(clean.path()), &env).unwrap().flags.venue_catalog_off);
}

/// **This crate resolves no directory of its own** for settings resolution: with no store
/// consulted, a settings directory changes nothing about the resolved ceilings — it is consulted
/// only for the removed-project-file refusal.
#[test]
fn a_settings_directory_alone_resolves_nothing_without_a_store() {
    let tmp = tempfile::tempdir().unwrap();
    let s = load(Some(tmp.path()), &HashMap::new()).unwrap();
    assert_eq!(s.policy.max_notional_per_order, Policy::default().max_notional_per_order);
}

/// **The removed layer is refused by the LOADER, not by each root.**
///
/// The check is layer 0 of `load_with_cli`, so there is no entry point into this crate that
/// resolves a value while a `<project>/vike.toml` sits unread beside the settings directory.
#[test]
fn a_present_project_file_fails_the_load_rather_than_being_ignored() {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join("settings");
    std::fs::create_dir(&dir).unwrap();

    // Without it, the settings load exactly as they always did.
    assert!(load(Some(&dir), &HashMap::new()).is_ok());

    std::fs::write(
        project.path().join(crate::removed::REMOVED_PROJECT_FILE),
        "[config]\nlog_dir = \"/from/project\"\n",
    )
    .unwrap();

    let err = load(Some(&dir), &HashMap::new()).unwrap_err();
    assert!(matches!(err, ConfigError::RemovedProjectFile { .. }), "{err}");
    // …and the CLI entry point, which is a different function, refuses identically.
    assert!(load_with_cli(Some(&dir), &HashMap::new(), &CliOverrides::default()).is_err());
}

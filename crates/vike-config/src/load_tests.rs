use super::*;
use std::assert_matches;

#[test]
fn no_store_is_exactly_the_code_defaults() {
    let s = load(None).unwrap();
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
    let none = load(None).unwrap();
    assert!(
        none.warnings.iter().any(|w| w == NO_SETTINGS_DIRECTORY_WARNING),
        "a load that resolved no project must SAY that its ceilings are defaults: {none:?}"
    );

    // A real directory, empty of everything: the distinction is "I found no project", never
    // "your project wrote no ceilings", so a settings directory with no rows must still be
    // silent here.
    let tmp = tempfile::tempdir().unwrap();
    let some = load(Some(tmp.path())).unwrap();
    assert!(
        some.warnings.is_empty(),
        "a settings directory that exists resolves the project, whatever it holds: {some:?}"
    );
}

/// **A DEAD flag's row key is a hard refusal naming the key** (`Flags::apply`, tested at its own
/// site); its variable is a `crate::REMOVED_ENV` refusal, which `removed_tests.rs` covers.
#[test]
fn a_dead_flag_refuses_its_row_key() {
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
        &CliOverrides::default(),
    )
    .expect("a row problem MARKS rather than hard-erroring — see `crate::mirror::apply_rows`");
    let refusal = resolved.seal_refusal.expect("the ROW key must be marked illegal");
    assert!(refusal.contains("record_dvol"), "{refusal}");
}

/// **The venue-catalog REFUSAL resolves from its row, and defaults to serving.**
#[test]
fn the_venue_catalog_refusal_resolves_from_its_row_and_defaults_off() {
    let clean = tempfile::tempdir().unwrap();
    assert!(
        !load(Some(clean.path())).unwrap().flags.venue_catalog_off,
        "the guarded state is `false` — the lane SERVES unless refused"
    );

    let rows = vike_secrets::StoredSettings {
        settings: vec![vike_secrets::SettingRow {
            section: "flags".to_string(),
            key: "venue_catalog_off".to_string(),
            value: "true".to_string(),
        }],
        ..Default::default()
    };
    let resolved = load_with_source(
        Some(clean.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides::default(),
    )
    .unwrap();
    assert!(resolved.flags.venue_catalog_off);
}

/// **This crate resolves no directory of its own** for settings resolution: with no store
/// consulted, a settings directory changes nothing about the resolved ceilings — it is consulted
/// only for the removed-project-file refusal.
#[test]
fn a_settings_directory_alone_resolves_nothing_without_a_store() {
    let tmp = tempfile::tempdir().unwrap();
    let s = load(Some(tmp.path())).unwrap();
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
    assert!(load(Some(&dir)).is_ok());

    std::fs::write(
        project.path().join(crate::removed::REMOVED_PROJECT_FILE),
        "[config]\nlog_dir = \"/from/project\"\n",
    )
    .unwrap();

    let err = load(Some(&dir)).unwrap_err();
    assert_matches!(err, ConfigError::RemovedProjectFile { .. }, "{err}");
    // …and the CLI entry point, which is a different function, refuses identically.
    assert!(load_with_cli(Some(&dir), &CliOverrides::default()).is_err());
}

/// **A `config.node_ack_wait_ms` row shorter than the control door's price hold loads, is honoured, and is
/// WARNED about** (A54): a client that hangs up inside the hold never reads the refusal the hold ends in. A row
/// at the hold or above, and a box with no row (the default outlasts the hold), load silently.
#[test]
fn an_ack_wait_row_shorter_than_the_doors_hold_loads_with_a_warning() {
    use crate::config::{DEFAULT_NODE_ACK_WAIT_MS, DOOR_PRICE_WAIT_MS};
    let dir = tempfile::tempdir().unwrap();
    let load_row = |ms: u32| {
        let rows = vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "node_ack_wait_ms".to_string(),
                value: ms.to_string(),
            }],
            ..Default::default()
        };
        load_with_source(
            Some(dir.path()),
            StoreLayer::Rows { rows: &rows, adopted: None },
            &CliOverrides::default(),
        )
        .unwrap()
    };

    let short = load_row(DOOR_PRICE_WAIT_MS - 1);
    assert_eq!(
        short.config.node_ack_wait_ms,
        DOOR_PRICE_WAIT_MS - 1,
        "the row is honoured, never clamped"
    );
    let [warning] = &short.warnings[..] else { panic!("one warning: {:?}", short.warnings) };
    assert_eq!(*warning, crate::config::node_ack_wait_below_hold_warning(DOOR_PRICE_WAIT_MS - 1));
    assert!(
        warning.contains(&format!("{} ms", DOOR_PRICE_WAIT_MS - 1))
            && warning.contains(&format!("{DOOR_PRICE_WAIT_MS} ms price hold"))
            && warning.contains(&format!("default is {DEFAULT_NODE_ACK_WAIT_MS} ms")),
        "the warning states the row, the hold and the default: {warning}"
    );

    for ms in [DOOR_PRICE_WAIT_MS, DEFAULT_NODE_ACK_WAIT_MS, 12_345] {
        assert!(load_row(ms).warnings.is_empty(), "{ms} ms is not shorter than the hold");
    }
    assert!(
        load(Some(dir.path())).unwrap().warnings.is_empty(),
        "no row: the default outlasts the hold"
    );
}

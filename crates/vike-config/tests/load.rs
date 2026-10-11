//! The load contract, end to end: pure defaults, the removed `<project>/vike.toml` project file,
//! the CLI layer that sits ABOVE the settings database's rows, and the variables that used to set a
//! key, which refuse startup (decision 0111).
//!
//! There is no settings-DIRECTORY layer (`docs/decisions/0086`): `load` and
//! `load_with_cli` hand [`load_with_source`] a [`StoreLayer::NotConsulted`] unconditionally and
//! open no store of their own — only a composition root's OWN `load_with_source` call (through
//! `vike_boot::boot`, which reads the real rows) ever sees a [`StoreLayer::Rows`]. So a test of a
//! stored value is one of:
//!
//! * a property of the ROWS layer itself, which is `crates/vike-config/tests/mirror.rs`'s job
//!   (refusal by name, a bound still biting, the four sections, the seal) — not restated here, or
//! * below, driving [`load_with_source`] directly with a hand-built
//!   [`vike_secrets::StoredSettings`], because THIS file's remaining job is the layers
//!   `load_with_source` composes ABOVE the rows — the CLI — which `mirror.rs` never exercises with
//!   a non-default `CliOverrides`.
//!
//! What is new here: proof that `load`/`load_with_cli` truly consult NO store — even a real one
//! planted at the exact directory named — because no other test in the tree states that property.
//!
//! Every test builds a throwaway settings directory in a `tempfile::TempDir`. Nothing here reads a
//! real `~`, a real `std::env`, or the workspace `.env` — which is the point of `load` taking its
//! root as a parameter (see the crate doc's "I/O ownership").
//!
//! ⚠ Every env-var NAME spelled here is one that already carries a row in
//! `vike_ops::settings::SETTINGS`. That gate harvests env-shaped string literals from the whole
//! `crates/` tree, `tests/` included, and fails on an undeclared one — so a placeholder name
//! invented for a test would break CI in a completely unrelated crate.

use std::assert_matches;
use std::collections::HashMap;
use std::path::Path;

use vike_config::{
    CliOverrides, ConfigError, Settings, StoreLayer, load, load_with_cli, load_with_source,
};
use vike_secrets::{SettingRow, StoredSettings};

/// One `settings` row, the shape every rows-based fixture below wants.
fn row(section: &str, key: &str, value: &str) -> SettingRow {
    SettingRow { section: section.to_string(), key: key.to_string(), value: value.to_string() }
}

fn store(rows: Vec<SettingRow>) -> StoredSettings {
    StoredSettings { settings: rows, ..Default::default() }
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// ---------------------------------------------------------------------------------------------
// Layer 1 — defaults, and an absent store is not an error
// ---------------------------------------------------------------------------------------------

#[test]
fn missing_files_yield_the_code_defaults() {
    let settings = tempfile::tempdir().unwrap();
    // The directory exists and is completely empty — no database in it at all.
    let s = load(Some(settings.path())).unwrap();
    assert_eq!(s, Settings::default());
}

#[test]
fn absent_roots_yield_the_code_defaults() {
    let s = load(None).unwrap();
    // ⚠ NOT `assert_eq!(s, Settings::default())` any more, and the difference is ONE field. Every
    // resolved VALUE is still the code default — the four asserts below are that claim, stated
    // per-field so it cannot weaken — and `warnings` now carries the one line saying WHY they are
    // defaults, which `Settings::default()` cannot carry because it describes no load at all.
    assert_eq!(
        s.warnings,
        vec![vike_config::NO_SETTINGS_DIRECTORY_WARNING.to_string()],
        "no project resolved ⇒ say so"
    );
    assert_eq!(Settings { warnings: Vec::new(), ..s.clone() }, Settings::default());
    assert_eq!(s.policy.max_leverage, 1.0);
    assert_eq!(s.preferences.log_level, "info");
    assert_eq!(s.config.datahub_addr, None);
}

#[test]
fn a_settings_directory_that_does_not_exist_is_simply_an_absent_layer() {
    let settings = tempfile::tempdir().unwrap();
    let missing = settings.path().join("no-such-dir");
    let s = load(Some(&missing)).unwrap();
    assert_eq!(s, Settings::default());
}

// ---------------------------------------------------------------------------------------------
// `load`/`load_with_cli` consult NO store — only `load_with_source` does
// ---------------------------------------------------------------------------------------------

/// **The property this file exists to prove now.** `load`/`load_with_cli` hand `load_with_source`
/// `StoreLayer::NotConsulted` unconditionally, so even a settings ROW that is genuinely THERE, at
/// the exact directory named, changes nothing through these two entry points — only a composition
/// root that resolved its own `StoreLayer::Rows` (through `vike_boot::boot`) ever sees it.
#[test]
fn load_and_load_with_cli_never_consult_the_rows_even_when_a_real_store_is_planted_there() {
    let settings = tempfile::tempdir().unwrap();
    vike_secrets::plant_settings_rows(
        settings.path(),
        &store(vec![row("config", "store_root", "\"/from/db\"")]),
    )
    .unwrap();

    let s = load(Some(settings.path())).unwrap();
    assert_eq!(
        s.config.store_root, None,
        "a real row at this exact directory must still not apply through `load`"
    );

    let s = load_with_cli(Some(settings.path()), &CliOverrides::default()).unwrap();
    assert_eq!(s.config.store_root, None, "…nor through `load_with_cli`");
}

/// …and the SAME shape of row, handed to [`load_with_source`] explicitly, DOES apply — the one call
/// that consults the rows at all.
#[test]
fn load_with_source_is_the_one_call_that_consults_the_rows() {
    let settings = tempfile::tempdir().unwrap();
    let rows = store(vec![row("config", "store_root", "\"/from/db\"")]);
    let s = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(s.config.store_root.as_deref(), Some(Path::new("/from/db")));
}

// ---------------------------------------------------------------------------------------------
// The REMOVED layer — `<project>/vike.toml`, refused rather than ignored
// ---------------------------------------------------------------------------------------------

/// The per-project override file is removed, and the operator who has one on disk gets a refusal
/// naming it and both destinations — never a silent no-op, which would leave a belief ("my log
/// level is capped") quietly false. `vike-config` applies exactly this rule to a removed KEY and a
/// removed VARIABLE; a removed FILE is the same argument with a bigger blast radius, because a
/// whole table stops applying at once.
#[test]
fn a_present_project_file_refuses_the_load_naming_it_and_where_its_keys_go() {
    let project = tempfile::tempdir().unwrap();
    let settings = project.path().join("settings");
    let file = project.path().join("vike.toml");
    std::fs::write(&file, "[config]\nstore_root = \"/project/store\"\n").unwrap();

    let err = load(Some(&settings)).unwrap_err();
    let msg = err.to_string();
    assert_matches!(err, ConfigError::RemovedProjectFile { .. }, "{msg}");
    assert!(msg.contains(&file.display().to_string()), "the exact path, not a name: {msg}");
    assert!(msg.contains("NO LONGER READ"), "{msg}");
    // ⚠ There are no settings FILES to move its keys into any more either (`docs/decisions/0086`):
    // the way out is a `config set` of the row, not a destination file.
    assert!(msg.contains("vike-cli config set config."), "where [config] keys go now: {msg}");
    assert!(
        msg.contains("vike-cli config set preferences."),
        "where [preferences] keys go now: {msg}"
    );

    // ⚠ Refuse and instruct. The file is the operator's only copy of whatever they wrote in it.
    assert!(file.is_file(), "the loader must never delete or move it");
    assert!(std::fs::read_to_string(&file).unwrap().contains("/project/store"), "nor rewrite it");
}

/// Whatever it contains. It was `[config]`/`[preferences]` only — `[policy]` and `[flags]` were
/// refused by name — so the removal has no "some tables still work" middle ground to explain.
#[test]
fn every_project_file_refuses_whatever_table_it_carries() {
    for body in [
        "[config]\nstore_root = \"/project/store\"\n",
        "[preferences]\nlog_file_level = \"warn\"\n",
        "[policy]\nmax_leverage = 50.0\n",
        "[flags]\nreconcile = true\n",
        "",
    ] {
        let project = tempfile::tempdir().unwrap();
        let settings = project.path().join("settings");
        std::fs::create_dir(&settings).unwrap();
        std::fs::write(project.path().join("vike.toml"), body).unwrap();
        assert!(load(Some(&settings)).is_err(), "accepted {body:?}");
    }
}

/// The complement, and the one that makes the refusal a fact about a FILE rather than about a
/// directory layout: no project file ⇒ nothing changes, for the same rows.
#[test]
fn no_project_file_loads_exactly_as_before() {
    let project = tempfile::tempdir().unwrap();
    let settings = project.path().join("settings");
    let rows = store(vec![
        row("config", "store_root", "\"/home/store\""),
        row("config", "log_dir", "\"/home/logs\""),
    ]);

    let s = load_with_source(
        Some(&settings),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(s.config.store_root.as_deref(), Some(Path::new("/home/store")));
    assert_eq!(s.config.log_dir.as_deref(), Some(Path::new("/home/logs")));
}

// ---------------------------------------------------------------------------------------------
// No environment layer (decision 0111). A variable that used to set a key is REFUSED at startup.
// ---------------------------------------------------------------------------------------------

/// The test the whole taxonomy exists for, restated now that no type has an environment layer.
///
/// `VIKE_MAX_ORDER_NOTIONAL` and `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` (the headless daemon's copy
/// of the same idea) are read by nothing, and a set value is refused at startup — and so is every
/// variable that used to set a `config`/`flags`/`preferences` key. The loader takes no environment
/// map at all, so nothing in a process's environment can move a resolved value: the regression net
/// is that each such variable REFUSES rather than configuring nothing in silence.
#[test]
fn every_variable_that_used_to_set_a_key_is_refused_and_moves_nothing() {
    let settings = tempfile::tempdir().unwrap();
    let rows = store(vec![
        row("policy", "max_leverage", "2.0"),
        row("policy", "max_notional_per_order", "1000.0"),
        row("config", "store_root", "\"/home/store\""),
        row("flags", "reconcile", "false"),
    ]);
    for (var, value) in [
        ("VIKE_MAX_ORDER_NOTIONAL", "9999999"),
        ("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "9999999"),
        ("VIKE_RECONCILE", "1"),
        ("VIKE_TRADEHUB_LIVE", "1"),
        ("VIKE_LOG", "trace"),
    ] {
        assert!(
            vike_config::refuse_removed_env(&env(&[(var, value)])).is_err(),
            "{var} must refuse startup"
        );
    }

    let s = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(s.policy.max_leverage, 2.0);
    assert_eq!(s.policy.max_notional_per_order, Some(1000.0));
    assert_eq!(s.config.store_root.as_deref(), Some(Path::new("/home/store")));
    assert!(!s.flags.reconcile);
}

// ---------------------------------------------------------------------------------------------
// The WRITTEN REFUSAL S2 no longer honours — `reconcile = false`
//
// After S2 an unset `reconcile` and an explicitly-false one get the SAME answer from
// `vike_tradehub::reconcile_config::reconcile_gate` (a live mount reconciles), because the resolved
// field is a `bool`. The loader is the last frame that can still tell them apart, so it is where
// the operator is told their written setting stopped meaning what it says.
// ---------------------------------------------------------------------------------------------

/// The stable marker of every `flags::reconcile_refusal_ignored` message, whatever its origin —
/// so a test can count ITS OWN producer on a channel that now has more than one.
fn reconcile_refusals(warnings: &[String]) -> Vec<&String> {
    warnings.iter().filter(|m| m.contains("no longer turns reconciliation OFF")).collect()
}

/// Every origin an operator can write a refusal in produces exactly one warning that names the
/// replacement.
///
/// The `reconcile_off` half of each assertion is what keeps this test honest: a warning that said
/// only "this is ignored" would leave the reader with no way out, and the whole complaint being
/// answered is that a person wrote a refusal and got silence.
///
/// ⚠ **This counts the RECONCILE warnings, not the warnings.** It asserted `warnings.len() == 1`
/// when it was written, which was true while S2 was this channel's only producer and false the
/// moment it merged beside S3: the two loads below resolve NO settings directory, so
/// [`vike_config::NO_SETTINGS_DIRECTORY_WARNING`] fires alongside the refusal. The whole-channel
/// count is the wrong property anyway — it makes this test fail on any UNRELATED warning a later
/// branch adds, which is a false alarm rather than a finding. Count your own producer.
#[test]
fn a_written_reconcile_false_is_reported_with_the_switch_that_replaced_it() {
    let settings = tempfile::tempdir().unwrap();
    let rows = store(vec![row("flags", "reconcile", "false")]);

    // A settings directory WAS resolved here, so the refusal is this load's only warning at all.
    let from_row = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(from_row.warnings.len(), 1, "one warning, once: {:?}", from_row.warnings);
    let row_refusals = reconcile_refusals(&from_row.warnings);
    assert_eq!(row_refusals.len(), 1, "{:?}", from_row.warnings);
    // ⚠ THE DEFECT this class fixed: naming a settings FILE as somewhere to write sends the
    // operator to write it, restart, and get this identical warning back. There is no file any
    // more — the way out is a `config set` of the row.
    assert!(
        row_refusals[0].contains("vike-cli config set flags.reconcile_off true"),
        "the way out must reach the ROWS, not a file: {:?}",
        from_row.warnings
    );
    assert!(!row_refusals[0].contains(".toml"), "{:?}", from_row.warnings);
    assert!(row_refusals[0].contains("reconcile_off"), "{:?}", from_row.warnings);

    // ...and this one passes `None`, so it ALSO carries the no-settings-directory warning. The
    // refusal must still appear exactly once, and the other producer must still fire — asserted
    // rather than tolerated, so that silencing it would fail here too.
    let from_cli =
        load_with_cli(None, &CliOverrides { reconcile: Some(false), ..Default::default() })
            .unwrap();
    let cli_refusals = reconcile_refusals(&from_cli.warnings);
    assert_eq!(cli_refusals.len(), 1, "{:?}", from_cli.warnings);
    assert!(cli_refusals[0].contains("--reconcile"), "{:?}", from_cli.warnings);
    assert!(
        from_cli.warnings.contains(&vike_config::NO_SETTINGS_DIRECTORY_WARNING.to_string()),
        "the other producer must still fire: {:?}",
        from_cli.warnings
    );
}

/// ...and the silence the warning must NOT break: an operator who wrote nothing, who wrote the
/// FORCE-ON, or who wrote the actual refusal has said nothing contradictory and hears nothing.
///
/// The `reconcile_off = true` row is the load-bearing one. It is the answer the warning tells
/// people to write, so warning about it would be the loader arguing with its own advice — and it
/// is a different key, so nothing about `reconcile` is being ignored there.
#[test]
fn nothing_warns_when_the_operator_wrote_no_contradiction() {
    let settings = tempfile::tempdir().unwrap();

    let reconcile_true = store(vec![row("flags", "reconcile", "true")]);
    let ok = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &reconcile_true, adopted: None },
        &CliOverrides::default(),
    )
    .unwrap();
    assert!(ok.warnings.is_empty(), "{:?}", ok.warnings);

    let reconcile_off = store(vec![row("flags", "reconcile_off", "true")]);
    let refused = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &reconcile_off, adopted: None },
        &CliOverrides::default(),
    )
    .unwrap();
    assert!(refused.flags.reconcile_off, "the refusal really did apply");
    assert!(refused.warnings.is_empty(), "{:?}", refused.warnings);

    // ⚠ This one resolves NO settings directory, so it carries NO_SETTINGS_DIRECTORY_WARNING —
    // the channel's OTHER producer, nothing to do with reconcile. What must stay silent is the
    // REFUSAL warning, so that is what is counted.
    let nothing_written = load(None).unwrap();
    assert!(
        reconcile_refusals(&nothing_written.warnings).is_empty(),
        "{:?}",
        nothing_written.warnings
    );
}

// ---------------------------------------------------------------------------------------------
// Layer 2 — CLI, the top of the chain
// ---------------------------------------------------------------------------------------------

#[test]
fn cli_outranks_every_layer_below_it() {
    let settings = tempfile::tempdir().unwrap();
    let rows = store(vec![row("config", "store_root", "\"/home/store\"")]);

    let s = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides {
            store_root: Some("/cli/store".to_string()),
            reconcile: Some(true),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(s.config.store_root.as_deref(), Some(Path::new("/cli/store")));
    assert!(s.flags.reconcile);
}

// ---------------------------------------------------------------------------------------------
// The REMOVED pair: `policy.rate.max_utilization` and `preferences.rate_utilization`
//
// Both fields are tombstones: the preference was read by NOTHING (every pacer takes its fraction
// from the compiled-in `vike_model::rate_limits::DEFAULT_UTILIZATION`), so the ceiling bounded a
// dead value. What is gated is the removal itself, END TO END through the real loader — a ROW that
// sets either key must FAIL, by name, with somewhere to go.
// ---------------------------------------------------------------------------------------------

/// ⚠ **Not a hard load error.** `docs/decisions/0086`: a row that will not apply —
/// including a TOMBSTONE key `Policy::apply` refuses by name — is `Settings::seal_refusal`, a MARK,
/// never a `Result::Err` (`crate::mirror::apply_rows`'s own doc: a refusal here would take
/// `config show` and every `secrets` verb down with the box they exist to diagnose).
#[test]
fn a_policy_row_still_setting_the_removed_rate_ceiling_is_refused_by_name() {
    let settings = tempfile::tempdir().unwrap();
    let rows = store(vec![row("policy", "rate.max_utilization", "0.6")]);

    let resolved = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides::default(),
    )
    .expect("a row problem MARKS rather than hard-erroring");
    let msg = resolved.seal_refusal.expect("a ceiling that bounded nothing must be marked");
    assert!(msg.contains("rate.max_utilization"), "names the key: {msg}");
    assert!(msg.contains("no longer a policy key"), "{msg}");
    assert!(msg.contains("DEFAULT_UTILIZATION"), "says where the number lives now: {msg}");
}

#[test]
fn a_preferences_row_still_setting_the_removed_utilization_is_refused_by_name() {
    let settings = tempfile::tempdir().unwrap();
    let rows = store(vec![row("preferences", "rate_utilization", "0.5")]);

    let resolved = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides::default(),
    )
    .expect("a row problem MARKS rather than hard-erroring");
    let msg = resolved.seal_refusal.expect("a preference nothing reads must be marked");
    assert!(msg.contains("rate_utilization"), "names the key: {msg}");
    assert!(msg.contains("NOTHING read it"), "{msg}");
}

/// The refusal must be a MARK that sticks, not a fall-through that quietly applies one of the two
/// rows: the FIRST one `apply_section_values` reaches decides, and nothing downstream sees a
/// half-applied `Settings` treated as sound. Asserted with BOTH rows present, the state an operator
/// who followed the old docs is actually in.
#[test]
fn both_halves_present_still_marks_rather_than_loading_one_of_them() {
    let settings = tempfile::tempdir().unwrap();
    let rows = store(vec![
        row("policy", "rate.max_utilization", "0.6"),
        row("preferences", "rate_utilization", "0.5"),
    ]);
    let resolved = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides::default(),
    )
    .expect("a row problem MARKS rather than hard-erroring");
    assert!(resolved.seal_refusal.is_some(), "{resolved:?}");

    // …and with neither row the load is clean and warns about nothing, which is the state the
    // operator reaches by deleting both rows as the errors instruct.
    let empty = StoredSettings::default();
    let s = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &empty, adopted: None },
        &CliOverrides::default(),
    )
    .unwrap();
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
}

// ---------------------------------------------------------------------------------------------
// The load is a pure function of its arguments
// ---------------------------------------------------------------------------------------------

#[test]
fn the_same_inputs_always_produce_the_same_settings() {
    let settings = tempfile::tempdir().unwrap();
    let rows = store(vec![
        row("policy", "max_leverage", "5.0"),
        row("flags", "poly_exec", "true"),
        row("preferences", "log_file_level", "\"warn\""),
    ]);

    let a = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides::default(),
    )
    .unwrap();
    let b = load_with_source(
        Some(settings.path()),
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides::default(),
    )
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(a.preferences.log_file_level, "warn");
    assert!(a.flags.poly_exec);
    assert_eq!(a.policy.max_leverage, 5.0);
}

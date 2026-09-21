//! The load contract, end to end: precedence, the policy exclusion, clamping, and the four ways
//! a bad file is rejected loudly.
//!
//! Every test builds a throwaway settings directory in a `tempfile::TempDir` and passes an env
//! `HashMap` it constructed itself. Nothing here reads a real `~`, a real `std::env`, or the
//! workspace `.env` — which is the point of `load` taking both roots as parameters (see the crate
//! doc's "I/O ownership").
//!
//! ⚠ Every env-var NAME spelled here is one that already carries a row in
//! `vike_ops::settings::SETTINGS`. That gate harvests env-shaped string literals from the whole
//! `crates/` tree, `tests/` included, and fails on an undeclared one — so a placeholder name
//! invented for a test would break CI in a completely unrelated crate.

use std::collections::HashMap;
use std::path::Path;

use vike_config::{CliOverrides, ConfigError, Settings, load, load_with_cli};

/// Write one file into `dir`, creating parents as needed.
fn write(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), body).unwrap();
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// ---------------------------------------------------------------------------------------------
// Layer 1 — defaults, and a missing file is not an error
// ---------------------------------------------------------------------------------------------

#[test]
fn missing_files_yield_the_code_defaults() {
    let settings = tempfile::tempdir().unwrap();
    // The directory exists and is completely empty.
    let s = load(Some(settings.path()), &HashMap::new()).unwrap();
    assert_eq!(s, Settings::default());
}

#[test]
fn absent_roots_yield_the_code_defaults() {
    let s = load(None, &HashMap::new()).unwrap();
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
    let s = load(Some(&missing), &HashMap::new()).unwrap();
    assert_eq!(s, Settings::default());
}

// ---------------------------------------------------------------------------------------------
// Layer 2 — the settings directory
// ---------------------------------------------------------------------------------------------

#[test]
fn the_settings_files_override_the_code_defaults() {
    let settings = tempfile::tempdir().unwrap();
    write(
        settings.path(),
        "policy.toml",
        "max_leverage = 10.0\nmax_notional_per_order = 250000.0\n",
    );
    write(settings.path(), "config.toml", "store_root = \"/market_data/hist\"\n");
    write(settings.path(), "preferences.toml", "log_level = \"debug\"\n");
    write(settings.path(), "flags.toml", "reconcile = true\n");

    let s = load(Some(settings.path()), &HashMap::new()).unwrap();
    assert_eq!(s.policy.max_leverage, 10.0);
    assert_eq!(s.policy.max_notional_per_order, Some(250_000.0));
    assert_eq!(s.config.store_root.as_deref(), Some(Path::new("/market_data/hist")));
    assert_eq!(s.preferences.log_level, "debug");
    assert!(s.flags.reconcile);
    // Untouched keys keep their code defaults rather than being reset to nothing.
    assert_eq!(s.config.datahub_addr, None);
    assert_eq!(s.preferences.log_file_level, "trace");
    assert!(!s.flags.poly_exec);
}

// ---------------------------------------------------------------------------------------------
// The REMOVED layer — `<project>/vike.toml`, refused rather than ignored
// ---------------------------------------------------------------------------------------------

/// A per-project override file sat between the settings directory and the environment. It is gone,
/// and the operator who has one on disk gets a refusal naming it and both destinations — never a
/// silent no-op, which would leave a belief ("my log level is capped") quietly false. `vike-config`
/// applies exactly this rule to a removed KEY and a removed VARIABLE; a removed FILE is the same
/// argument with a bigger blast radius, because a whole table stops applying at once.
#[test]
fn a_present_project_file_refuses_the_load_naming_it_and_where_its_keys_go() {
    let project = tempfile::tempdir().unwrap();
    let settings = project.path().join("settings");
    write(&settings, "config.toml", "store_root = \"/home/store\"\n");
    let file = project.path().join("vike.toml");
    std::fs::write(&file, "[config]\nstore_root = \"/project/store\"\n").unwrap();

    let err = load(Some(&settings), &HashMap::new()).unwrap_err();
    let msg = err.to_string();
    assert!(matches!(err, ConfigError::RemovedProjectFile { .. }), "{msg}");
    assert!(msg.contains(&file.display().to_string()), "the exact path, not a name: {msg}");
    assert!(msg.contains("NO LONGER READ"), "{msg}");
    assert!(msg.contains("settings/config.toml"), "where [config] goes: {msg}");
    assert!(msg.contains("settings/preferences.toml"), "where [preferences] goes: {msg}");

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
        assert!(load(Some(&settings), &HashMap::new()).is_err(), "accepted {body:?}");
    }
}

/// The complement, and the one that makes the refusal a fact about a FILE rather than about a
/// directory layout: no project file ⇒ nothing changes, for the same tree.
#[test]
fn no_project_file_loads_exactly_as_before() {
    let project = tempfile::tempdir().unwrap();
    let settings = project.path().join("settings");
    write(&settings, "config.toml", "store_root = \"/home/store\"\nlog_dir = \"/home/logs\"\n");

    let s = load(Some(&settings), &HashMap::new()).unwrap();
    assert_eq!(s.config.store_root.as_deref(), Some(Path::new("/home/store")));
    assert_eq!(s.config.log_dir.as_deref(), Some(Path::new("/home/logs")));
}

// ---------------------------------------------------------------------------------------------
// Layer 3 — env. THE load-bearing one: policy does not have this layer.
// ---------------------------------------------------------------------------------------------

/// The test the whole taxonomy exists for.
///
/// `VIKE_MAX_ORDER_NOTIONAL` is not a hypothetical: it is read today by `vike-app`'s `main.rs`
/// and `vike-cli`'s `cmd/verbs.rs`, and `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` is the headless
/// daemon's copy of the same idea. Under this crate they are `Policy` fields, and `Policy` has no
/// env layer — so exporting either changes nothing.
///
/// Note what makes this test weak on its own and strong in context: it asserts a runtime
/// behaviour, but the REAL guarantee is that the code which would break it cannot be written —
/// `settings.policy.apply_env(&env)` does not compile, because `Policy` implements neither
/// `EnvOverride` nor any inherent `apply_env`. This test is the regression net under that.
#[test]
fn policy_ignores_the_environment_entirely() {
    let settings = tempfile::tempdir().unwrap();
    write(
        settings.path(),
        "policy.toml",
        "max_leverage = 2.0\nmax_notional_per_order = 1000.0\nmarket_slippage = 0.002\n",
    );

    let hostile = env(&[
        ("VIKE_MAX_ORDER_NOTIONAL", "9999999"),
        ("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "9999999"),
        // …and, for good measure, names shaped like the other policy fields.
        ("VIKE_MAX_ORDER_QTY", "9999999"),
    ]);

    let s = load(Some(settings.path()), &hostile).unwrap();
    assert_eq!(s.policy.max_leverage, 2.0, "leverage ceiling moved");
    assert_eq!(s.policy.max_notional_per_order, Some(1000.0), "order-notional ceiling moved");
    assert_eq!(s.policy.market_slippage, Some(0.002), "slippage band moved");

    // And the same env map with no policy file at all leaves the code defaults standing.
    let s = load(None, &hostile).unwrap();
    assert_eq!(s.policy, vike_config::Policy::default());
}

#[test]
fn env_overrides_the_file_layers_for_the_three_types_that_have_it() {
    let settings = tempfile::tempdir().unwrap();
    write(settings.path(), "config.toml", "store_root = \"/home/store\"\n");
    write(settings.path(), "preferences.toml", "log_level = \"warn\"\n");
    write(settings.path(), "flags.toml", "reconcile = false\n");

    let s = load(
        Some(settings.path()),
        &env(&[("VIKE_HIST_STORE", "/env/store"), ("RUST_LOG", "trace"), ("VIKE_RECONCILE", "1")]),
    )
    .unwrap();

    assert_eq!(s.config.store_root.as_deref(), Some(Path::new("/env/store")));
    assert_eq!(s.preferences.log_level, "trace");
    assert!(s.flags.reconcile);
}

#[test]
fn a_flag_typo_in_the_environment_is_an_error_naming_variable_and_value() {
    let err = load(None, &env(&[("POLY_EXEC", "true")])).unwrap_err();
    let msg = err.to_string();
    assert!(msg.starts_with("POLY_EXEC=true: "), "{msg}");
    assert!(msg.contains("\"1\""), "{msg}");
}

// ---------------------------------------------------------------------------------------------
// The WRITTEN REFUSAL S2 no longer honours — `reconcile = false` / `VIKE_RECONCILE=0`
//
// After S2 an unset `reconcile` and an explicitly-false one get the SAME answer from
// `vike_ops::reconcile_config::reconcile_gate` (a live mount reconciles), because the resolved
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
    write(settings.path(), "flags.toml", "reconcile = false\n");

    // A settings directory WAS resolved here, so the refusal is this load's only warning at all.
    let from_file = load(Some(settings.path()), &HashMap::new()).unwrap();
    assert_eq!(from_file.warnings.len(), 1, "one warning, once: {:?}", from_file.warnings);
    let file_refusals = reconcile_refusals(&from_file.warnings);
    assert_eq!(file_refusals.len(), 1, "{:?}", from_file.warnings);
    assert!(file_refusals[0].contains("flags.toml"), "{:?}", from_file.warnings);
    assert!(file_refusals[0].contains("reconcile_off"), "{:?}", from_file.warnings);

    // ...and these two pass `None`, so each ALSO carries the no-settings-directory warning. The
    // refusal must still appear exactly once, and the other producer must still fire — asserted
    // rather than tolerated, so that silencing it would fail here too.
    let from_env = load(None, &env(&[("VIKE_RECONCILE", "0")])).unwrap();
    let env_refusals = reconcile_refusals(&from_env.warnings);
    assert_eq!(env_refusals.len(), 1, "{:?}", from_env.warnings);
    assert!(env_refusals[0].contains("VIKE_RECONCILE=0"), "{:?}", from_env.warnings);
    assert!(env_refusals[0].contains("VIKE_RECONCILE_OFF=1"), "{:?}", from_env.warnings);
    assert!(
        from_env.warnings.contains(&vike_config::NO_SETTINGS_DIRECTORY_WARNING.to_string()),
        "the other producer must still fire: {:?}",
        from_env.warnings
    );

    let from_cli = load_with_cli(
        None,
        &HashMap::new(),
        &CliOverrides { reconcile: Some(false), ..Default::default() },
    )
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

    write(settings.path(), "flags.toml", "reconcile = true\n");
    assert!(load(Some(settings.path()), &HashMap::new()).unwrap().warnings.is_empty());

    write(settings.path(), "flags.toml", "reconcile_off = true\n");
    let refused = load(Some(settings.path()), &HashMap::new()).unwrap();
    assert!(refused.flags.reconcile_off, "the refusal really did apply");
    assert!(refused.warnings.is_empty(), "{:?}", refused.warnings);

    // An EMPTY value configured nothing under the old reader either, so nobody can believe a
    // refusal was armed by one — the same rule `vike_config::refuse_removed_env` applies.
    // ⚠ These two resolve NO settings directory, so each carries NO_SETTINGS_DIRECTORY_WARNING —
    // the channel's OTHER producer, nothing to do with reconcile. What must stay silent is the
    // REFUSAL warning, so that is what is counted; asserting the channel is empty would be
    // asserting something this loader has never promised.
    let empty_value = load(None, &env(&[("VIKE_RECONCILE", "")])).unwrap();
    assert!(reconcile_refusals(&empty_value.warnings).is_empty(), "{:?}", empty_value.warnings);
    let nothing_written = load(None, &HashMap::new()).unwrap();
    assert!(
        reconcile_refusals(&nothing_written.warnings).is_empty(),
        "{:?}",
        nothing_written.warnings
    );
}

// ---------------------------------------------------------------------------------------------
// Layer 4 — CLI, the top of the chain
// ---------------------------------------------------------------------------------------------

#[test]
fn cli_outranks_every_layer_below_it() {
    let settings = tempfile::tempdir().unwrap();
    write(settings.path(), "config.toml", "store_root = \"/home/store\"\n");

    let s = load_with_cli(
        Some(settings.path()),
        &env(&[("VIKE_HIST_STORE", "/env/store"), ("VIKE_RECONCILE", "0")]),
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
// These three tests used to prove the clamp — the model's only policy-binds-preference edge. Both
// of its fields are gone: the preference was read by NOTHING (every pacer takes its fraction from
// the compiled-in `vike_model::rate_limits::DEFAULT_UTILIZATION`), so the ceiling bounded a dead
// value. What is gated now is the removal itself, END TO END through the real loader — a file that
// sets either key must FAIL, by name, with somewhere to go.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_policy_file_still_setting_the_removed_rate_ceiling_is_refused_by_name() {
    let settings = tempfile::tempdir().unwrap();
    write(settings.path(), "policy.toml", "[rate]\nmax_utilization = 0.6\n");

    let msg = load(Some(settings.path()), &HashMap::new())
        .expect_err("a ceiling that bounded nothing must not load silently")
        .to_string();
    assert!(msg.contains("policy.toml"), "names the file: {msg}");
    assert!(msg.contains("rate.max_utilization"), "names the key: {msg}");
    assert!(msg.contains("no longer a policy key"), "{msg}");
    assert!(msg.contains("DEFAULT_UTILIZATION"), "says where the number lives now: {msg}");
}

#[test]
fn a_preferences_file_still_setting_the_removed_utilization_is_refused_by_name() {
    let settings = tempfile::tempdir().unwrap();
    write(settings.path(), "preferences.toml", "rate_utilization = 0.5\n");

    let msg = load(Some(settings.path()), &HashMap::new())
        .expect_err("a preference nothing reads must not load silently")
        .to_string();
    assert!(msg.contains("preferences.toml"), "names the file: {msg}");
    assert!(msg.contains("rate_utilization"), "names the key: {msg}");
    assert!(msg.contains("NOTHING read it"), "{msg}");
}

/// The refusal must be a REFUSAL, not a fall-through: the load fails, so nothing downstream ever
/// sees a half-applied `Settings`. Asserted with BOTH files present, the state an operator who
/// followed the old docs is actually in.
#[test]
fn both_halves_present_still_fails_rather_than_loading_one_of_them() {
    let settings = tempfile::tempdir().unwrap();
    write(settings.path(), "policy.toml", "[rate]\nmax_utilization = 0.6\n");
    write(settings.path(), "preferences.toml", "rate_utilization = 0.5\n");
    assert!(load(Some(settings.path()), &HashMap::new()).is_err());

    // …and with neither file the load is clean and warns about nothing, which is the state the
    // operator reaches by deleting both lines as the errors instruct.
    let empty = tempfile::tempdir().unwrap();
    let s = load(Some(empty.path()), &HashMap::new()).unwrap();
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
}

// ---------------------------------------------------------------------------------------------
// Rejections — every one names the file and, where the parser knows it, the key
// ---------------------------------------------------------------------------------------------

#[test]
fn an_unknown_key_is_rejected_by_name() {
    let settings = tempfile::tempdir().unwrap();
    write(settings.path(), "policy.toml", "max_leverag = 10.0\n");

    let err = load(Some(settings.path()), &HashMap::new()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("policy.toml"), "the file must be named: {msg}");
    assert!(msg.contains("max_leverag"), "the offending key must be named: {msg}");
    match err {
        ConfigError::Parse { key, .. } => assert_eq!(key.as_deref(), Some("max_leverag")),
        other => panic!("expected a Parse error, got {other:?}"),
    }
}

#[test]
fn an_unknown_key_in_a_nested_table_is_rejected_by_name() {
    let settings = tempfile::tempdir().unwrap();
    write(settings.path(), "policy.toml", "[rate]\nmax_utilisation = 0.5\n");

    let err = load(Some(settings.path()), &HashMap::new()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("policy.toml"), "{msg}");
    assert!(msg.contains("max_utilisation"), "{msg}");
}

#[test]
fn an_unknown_key_in_every_one_of_the_four_files_is_rejected() {
    for (file, body, key) in [
        ("policy.toml", "nope = 1\n", "nope"),
        ("config.toml", "nope = 1\n", "nope"),
        ("preferences.toml", "nope = 1\n", "nope"),
        ("flags.toml", "nope = true\n", "nope"),
    ] {
        let settings = tempfile::tempdir().unwrap();
        write(settings.path(), file, body);
        let err = load(Some(settings.path()), &HashMap::new()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(file), "{file}: {msg}");
        assert!(msg.contains(key), "{file}: {msg}");
    }
}

#[test]
fn malformed_toml_names_the_file_and_where_in_it() {
    let settings = tempfile::tempdir().unwrap();
    write(settings.path(), "config.toml", "store_root = \n");

    let err = load(Some(settings.path()), &HashMap::new()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("config.toml"), "the file must be named: {msg}");
    // A pure syntax error names no KEY — the parser reports a line/column instead, and the
    // message is carried through verbatim rather than replaced with "invalid config".
    assert!(msg.contains("line 1"), "the location must survive: {msg}");
}

#[test]
fn a_wrong_type_names_the_file() {
    let settings = tempfile::tempdir().unwrap();
    write(settings.path(), "policy.toml", "max_leverage = \"ten\"\n");

    let err = load(Some(settings.path()), &HashMap::new()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("policy.toml"), "{msg}");
    assert!(msg.contains("expected"), "{msg}");
}

#[test]
fn an_out_of_range_ceiling_names_file_key_value_and_bound() {
    let settings = tempfile::tempdir().unwrap();
    write(settings.path(), "policy.toml", "market_slippage = 0.9\n");

    let err = load(Some(settings.path()), &HashMap::new()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("market_slippage = 0.9 exceeds the allowed maximum"), "{msg}");
    assert!(msg.contains("policy.toml"), "{msg}");
}

#[test]
fn a_directory_where_a_file_belongs_is_a_read_error_not_a_silent_default() {
    let settings = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(settings.path().join("config.toml")).unwrap();

    let err = load(Some(settings.path()), &HashMap::new()).unwrap_err();
    assert!(matches!(err, ConfigError::Read { .. }), "{err}");
    assert!(err.to_string().contains("config.toml"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// The load is a pure function of its arguments
// ---------------------------------------------------------------------------------------------

#[test]
fn the_same_inputs_always_produce_the_same_settings() {
    let settings = tempfile::tempdir().unwrap();
    write(settings.path(), "policy.toml", "max_leverage = 5.0\n");
    write(settings.path(), "flags.toml", "poly_exec = true\n");
    let e = env(&[("VIKE_LOG_FILE_LEVEL", "warn")]);

    let a = load(Some(settings.path()), &e).unwrap();
    let b = load(Some(settings.path()), &e).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.preferences.log_file_level, "warn");
    assert!(a.flags.poly_exec);
    assert_eq!(a.policy.max_leverage, 5.0);
}

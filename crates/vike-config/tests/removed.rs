//! The removed-variable contract, end to end: the per-order notional ceiling comes from
//! `<project>/settings/policy.toml` and from nowhere else, and a process that finds one of the
//! REMOVED environment variables set refuses to start with a message naming its replacement.
//!
//! The four properties under test are the four ways this can go wrong:
//!
//! 1. **the refusal** — a set-but-removed variable ERRORS, naming file + key. Silently ignoring a
//!    ceiling someone believes is active is worse than either keeping it or erroring.
//! 2. **the refusal is not a leak** — a removed variable whose VALUE is itself a secret is refused
//!    without printing it.
//! 3. **the value flows** — a `policy.toml` naming `max_notional_per_order` reaches
//!    [`vike_config::Policy`], so the ceiling an operator writes is the ceiling that applies.
//! 4. **absent `policy.toml` is the default** — no file means `None` (uncapped).
//!
//! Every test builds a throwaway settings directory in a `tempfile::TempDir` and passes an env
//! `HashMap` it constructed itself — nothing here reads a real directory or a real `std::env`.
//!
//! ⚠ Every env-var NAME spelled here carries a row in `vike_ops::settings::SETTINGS` (the
//! `vike-config` rows — see that table). The registry gate harvests env-shaped string literals from
//! the whole `crates/` tree, `tests/` included, and fails on an undeclared one.

use std::collections::HashMap;

use vike_config::{load, refuse_removed_env, REMOVED_ENV};

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// ---------------------------------------------------------------------------------------------
// 1 — the refusal
// ---------------------------------------------------------------------------------------------

/// The exact operator experience: set the old variable, get told which file and key to use.
#[test]
fn a_removed_variable_refuses_startup_and_names_its_replacement() {
    let err = refuse_removed_env(&env(&[("VIKE_MAX_ORDER_NOTIONAL", "250")])).unwrap_err();
    assert!(err.contains("VIKE_MAX_ORDER_NOTIONAL"), "names the variable: {err}");
    assert!(err.contains("settings/policy.toml"), "names the file: {err}");
    assert!(err.contains("max_notional_per_order = 250"), "names the key AND the line: {err}");
}

#[test]
fn the_daemon_variable_is_refused_identically() {
    let err =
        refuse_removed_env(&env(&[("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "1000")])).unwrap_err();
    assert!(err.contains("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL"), "{err}");
    assert!(err.contains("settings/policy.toml"), "{err}");
    assert!(err.contains("max_notional_per_order = 1000"), "{err}");
}

/// The common case — an environment with none of them — must start, including one carrying
/// unrelated vike variables.
#[test]
fn an_ordinary_environment_starts() {
    assert_eq!(refuse_removed_env(&HashMap::new()), Ok(()));
    assert_eq!(
        refuse_removed_env(&env(&[("VIKE_RECONCILE", "1"), ("VIKE_TRADEHUB_CONTROL", "1")])),
        Ok(())
    );
}

/// The table is not empty — a refusal harness with no rows would make every test above vacuous the
/// day someone trims it.
#[test]
fn the_removed_table_covers_every_variable_that_was_deleted() {
    let vars: Vec<&str> = REMOVED_ENV.iter().map(|r| r.var).collect();
    for name in
        ["VIKE_MAX_ORDER_NOTIONAL", "VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "VIKE_SECRETS_PASSPHRASE"]
    {
        assert!(vars.contains(&name), "{name} is not refused: {vars:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// 2 — the refusal is a diagnostic, never a leak
// ---------------------------------------------------------------------------------------------

/// **A removed variable that HELD a secret is refused without printing it.**
///
/// Someone who set it believes it configures how credentials are opened, so starting anyway is not
/// an option — but the refusal lands on stderr, which every service manager and CI job captures, so
/// echoing the value would trade a misconfiguration for a leaked credential. The message says where
/// credentials come from instead.
#[test]
fn a_secret_valued_removed_variable_is_refused_without_being_printed() {
    let err = refuse_removed_env(&env(&[("VIKE_SECRETS_PASSPHRASE", "hunter2-correct-horse")]))
        .unwrap_err();
    assert!(err.contains("VIKE_SECRETS_PASSPHRASE"), "names the variable: {err}");
    assert!(err.contains("NO LONGER READ"), "{err}");
    assert!(err.contains("settings/secrets.env"), "names where credentials come from: {err}");
    assert!(!err.contains("hunter2-correct-horse"), "the value reached the message: {err}");
}

/// One pass, every offender — not one restart per variable.
#[test]
fn every_offender_is_reported_together() {
    let err = refuse_removed_env(&env(&[
        ("VIKE_MAX_ORDER_NOTIONAL", "250"),
        ("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "1000"),
        ("VIKE_SECRETS_PASSPHRASE", "pw"),
    ]))
    .unwrap_err();
    for name in
        ["VIKE_MAX_ORDER_NOTIONAL", "VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "VIKE_SECRETS_PASSPHRASE"]
    {
        assert!(err.contains(name), "{name} missing from the combined report: {err}");
    }
}

// ---------------------------------------------------------------------------------------------
// 3 — the value flows from policy.toml
// ---------------------------------------------------------------------------------------------

/// The replacement path the refusal message tells the operator to take, taken: write the suggested
/// line into `<project>/settings/policy.toml` and the ceiling is the one that loads.
#[test]
fn the_ceiling_the_refusal_suggests_is_the_ceiling_that_loads() {
    let settings = tempfile::tempdir().unwrap();
    std::fs::write(settings.path().join("policy.toml"), "max_notional_per_order = 250\n").unwrap();

    let s = load(Some(settings.path()), &HashMap::new()).unwrap();
    assert_eq!(s.policy.max_notional_per_order, Some(250.0));
}

/// …and the environment cannot move it. This is the whole point, so it is asserted directly rather
/// than left to the type system's word: `Policy` implements neither `EnvOverride` nor
/// `CliOverride`, but a future refactor could only break that in a way a test would catch.
#[test]
fn the_environment_cannot_change_the_loaded_ceiling() {
    let settings = tempfile::tempdir().unwrap();
    std::fs::write(settings.path().join("policy.toml"), "max_notional_per_order = 250\n").unwrap();

    // Note these are exactly the removed variables — a build that still honoured either would
    // report 9_999_999 here. (`refuse_removed_env` is what a BINARY calls; `load` itself does not
    // refuse, because the two answer different questions: "is the environment stale?" and "what is
    // the effective configuration?")
    let hostile = env(&[
        ("VIKE_MAX_ORDER_NOTIONAL", "9999999"),
        ("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "9999999"),
    ]);
    let s = load(Some(settings.path()), &hostile).unwrap();
    assert_eq!(s.policy.max_notional_per_order, Some(250.0), "env must not raise a ceiling");
}

// ---------------------------------------------------------------------------------------------
// 4 — no policy.toml is the permissive default
// ---------------------------------------------------------------------------------------------

/// An install with no `policy.toml` gets `None` — uncapped.
#[test]
fn an_absent_policy_file_leaves_the_ceiling_uncapped() {
    let settings = tempfile::tempdir().unwrap();
    let s = load(Some(settings.path()), &HashMap::new()).unwrap();
    assert_eq!(s.policy.max_notional_per_order, None);

    // …and so does no settings directory at all (no project above the working directory).
    let s = load(None, &HashMap::new()).unwrap();
    assert_eq!(s.policy.max_notional_per_order, None);
}

/// The binaries' whole startup sequence, in the order they run it: refuse, then load from the
/// settings directory the binary resolved. Proves the two pieces compose.
#[test]
fn the_startup_sequence_composes() {
    let project = tempfile::tempdir().unwrap();
    let settings = project.path().join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::write(settings.join("policy.toml"), "max_notional_per_order = 500\n").unwrap();

    let vars = env(&[("VIKE_RECONCILE", "1")]);
    refuse_removed_env(&vars).expect("a clean environment starts");
    let s = load(Some(&settings), &vars).unwrap();
    assert_eq!(s.policy.max_notional_per_order, Some(500.0));
    assert!(s.flags.reconcile, "the env layer still applies to flags");
}

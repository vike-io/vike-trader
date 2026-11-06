//! `vike-backend strategy-builder` reaches REAL code, proven by running the compiled multicall
//! binary as a subprocess rather than by reading `TOOLS` and trusting it.
//!
//! ⚠ Whole-file gated on the `vike-strategy-builder` feature, the same switch
//! `crates/vike/Cargo.toml`'s row is behind: a default (feature-off) build of `vike-backend` has
//! no `strategy-builder` row at all, so this file compiling unconditionally would either fail to
//! build against a stale `CARGO_BIN_EXE_vike-backend` shape or assert a refusal message a
//! default binary can never print. `cargo test -p vike-backend --features vike-strategy-builder`
//! (or any lane that turns the feature on, `full` included) is what runs it.
#![cfg(feature = "vike-strategy-builder")]

use std::process::Command;

/// The multicall's `--help`/`-h` output NAMES the verb — the same text an operator reads before
/// ever typing it.
#[test]
fn the_tool_list_names_strategy_builder() {
    let bin = env!("CARGO_BIN_EXE_vike-backend");
    let out = Command::new(bin).arg("--help").output().expect("failed to exec vike-backend --help");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("strategy-builder"),
        "vike-backend --help does not list strategy-builder — TOOLS table row missing or the \
         feature was not compiled in for this binary. Full output:\n{stdout}"
    );
}

/// `vike-backend strategy-builder`, run with NO configuration at all, reaches the REAL
/// `vike_strategy_builder::builder::run` and prints ITS refusal — not the dispatcher's own
/// `Route::Usage(2)` tool list, which is what a misnamed or missing row would produce instead.
/// Distinguishing the two is the whole point: both exit non-zero, but only one proves the verb
/// actually dispatches to this crate's code.
#[test]
fn the_verb_dispatches_to_the_real_builder_and_refuses_without_a_key() {
    let bin = env!("CARGO_BIN_EXE_vike-backend");
    // `env_remove`, not `env_clear` + a re-supplied `PATH`: the latter reads
    // `std::env::var("PATH")` at a resolved call site, which is exactly the kind of raw
    // process-environment read `crates/vike-ops/tests/settings_secrets/settings_registry.rs` exists to catch in
    // PRODUCTION code — and a `tests/*.rs` file is not exempt from being a real call site the
    // scanner resolves. `env_remove` proves the same thing (no key, no workspace root reach the
    // child) without adding one.
    //
    // The key lives ONLY in the settings database, so "no configuration" is a settings directory with
    // no database in it: an explicit empty one, rather than the working directory's project walk,
    // which on a developer's box could find a real store holding a real key.
    let no_store = tempfile::tempdir().expect("an empty settings directory");
    let out = Command::new(bin)
        .arg("strategy-builder")
        .env("VIKE_SETTINGS_DIR", no_store.path())
        .env_remove("VIKE_STRATEGY_BUILDER_KEY")
        .env_remove("VIKE_STRATEGY_BUILDER_KEY_FILE")
        .env_remove("VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT")
        .output()
        .expect("failed to exec vike-backend strategy-builder");
    // Printed unconditionally (not just on failure) — this is the one test in this file whose
    // whole point is to be READ, with `--nocapture`, as evidence the verb reaches real code.
    println!("exit status: {:?}", out.status);
    println!("stdout:\n{}", String::from_utf8_lossy(&out.stdout));
    println!("stderr:\n{}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "vike-backend strategy-builder exited 0 with no key configured — it should refuse to \
         start. stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("vike-cli secrets set VIKE_STRATEGY_BUILDER_KEY"),
        "vike-backend strategy-builder did not print the builder's own refusal, naming the key's \
         row and the one command that sets it — this is either `Route::Usage(2)` (the row is not \
         really reaching the dispatcher's TOOLS table) or some other failure. \
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("vike-backend — one executable"),
        "vike-backend strategy-builder printed the DISPATCHER'S OWN usage text — that is \
         Route::Usage(2), meaning the link did not resolve to a real tool. stdout:\n{stdout}"
    );
}

/// The retired key homes — a variable carrying a key, and a variable naming a key FILE (what the
/// unit's `LoadCredential=` line used to hand the daemon) — are read by NOTHING, through the
/// multicall exactly as through the crate's own binary: the settings database holds no key, so the
/// start is refused, naming the row and the one command that sets it, and neither planted value
/// reaches stderr. No fallback, no migration (the owner's ruling of 2026-10-31).
#[test]
fn the_verb_takes_no_key_from_a_variable_or_a_file() {
    let bin = env!("CARGO_BIN_EXE_vike-backend");
    let no_store = tempfile::tempdir().expect("an empty settings directory");
    let retired_file = no_store.path().join("retired-builder.key");
    std::fs::write(&retired_file, "verb-test-file-form\n").expect("plant the retired key file");
    let out = Command::new(bin)
        .arg("strategy-builder")
        .env("VIKE_SETTINGS_DIR", no_store.path())
        .env("VIKE_STRATEGY_BUILDER_KEY", "verb-test-value-form")
        .env("VIKE_STRATEGY_BUILDER_KEY_FILE", &retired_file)
        .env_remove("VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT")
        .output()
        .expect("failed to exec vike-backend strategy-builder");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a retired key home must not start the daemon. stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("vike-cli secrets set VIKE_STRATEGY_BUILDER_KEY"),
        "the refusal must name the row and the one command that sets it. stderr:\n{stderr}"
    );
    for planted in ["verb-test-value-form", "verb-test-file-form"] {
        assert!(!stderr.contains(planted), "a planted key value reached stderr:\n{stderr}");
    }
}

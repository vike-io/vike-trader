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
    // process-environment read `crates/vike-ops/tests/settings_registry.rs` exists to catch in
    // PRODUCTION code — and a `tests/*.rs` file is not exempt from being a real call site the
    // scanner resolves. `env_remove` proves the same thing (no key, no workspace root reach the
    // child) without adding one.
    let out = Command::new(bin)
        .arg("strategy-builder")
        .env_remove("VIKE_STRATEGY_BUILDER_KEY")
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
        stderr.contains("VIKE_STRATEGY_BUILDER_KEY"),
        "vike-backend strategy-builder did not print the builder's own refusal — this is either \
         `Route::Usage(2)` (the row is not really reaching the dispatcher's TOOLS table) or some \
         other failure. stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("vike-backend — one executable"),
        "vike-backend strategy-builder printed the DISPATCHER'S OWN usage text — that is \
         Route::Usage(2), meaning the link did not resolve to a real tool. stdout:\n{stdout}"
    );
}

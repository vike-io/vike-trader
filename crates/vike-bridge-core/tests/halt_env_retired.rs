//! **`VIKE_HALT_FILE` decides nothing** (decision 0099) — the sentinel's path is the composition
//! root's declared project, `<project>/settings/state/HALT`, and no process variable moves it.
//!
//! # Why a variable that is merely IGNORED is the dangerous half
//!
//! The startup refusal (`vike_config::REMOVED_ENV`) stops a process that still carries the variable.
//! This file holds what that refusal relies on: the resolver, reached by a binary that does NOT
//! refuse (a test, a tool, a root that skips the boot), does not honour it. A resolver that did
//! would watch a file the refusal says is no longer read, and an operator would `touch` the default
//! path instead.
//!
//! # Mechanism
//!
//! `halt_path_from_env` memoizes in a `OnceLock`, so one process cannot be asked twice, and this
//! workspace does not mutate the environment under threads (`std::env::set_var` is `unsafe` since
//! edition 2024 and the tree forbids unsafe code). So the property is stated as a plain test
//! ([`the_declared_project_decides_whatever_the_environment_says`]) that holds in ANY environment,
//! and a second test re-runs that very test in a CHILD process whose environment names an
//! override — `Command::env` on the child, never `set_var` here. The plain test passing in the
//! parent proves the default; the child passing proves the variable changed nothing.

use std::process::Command;

use vike_bridge_core::halt::{HALT_FILE, declare_project_state_dir, halt_path_from_env};

/// The variable this file proves is ignored. Spelled here rather than imported: the resolver
/// exports no name for it, which is the point.
const RETIRED_OVERRIDE: &str = "VIKE_HALT_FILE";

/// **The declared project's state directory decides the sentinel, whatever the environment says.**
///
/// True in a clean environment and, re-run by [`a_set_halt_file_variable_decides_nothing`], true in
/// one that names an override.
#[test]
fn the_declared_project_decides_whatever_the_environment_says() {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let state = scratch.path().join("project").join("settings").join("state");
    std::fs::create_dir_all(&state).expect("the declared state directory");

    declare_project_state_dir(Some(state.clone())).expect("the first declaration is accepted");

    assert_eq!(
        halt_path_from_env(),
        state.join(HALT_FILE),
        "the sentinel must be the declared project's `settings/state/HALT` — a variable in this \
         process's environment must not be able to name another file (decision 0099)"
    );
}

/// The variable, set in a child's environment to a path that is NOT the declared one, moves
/// nothing: the child's resolution is still the declared project's.
#[test]
fn a_set_halt_file_variable_decides_nothing() {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let elsewhere = scratch.path().join("elsewhere").join(HALT_FILE);

    let exe = std::env::current_exe().expect("this test binary's own path");
    let out = Command::new(exe)
        .args(["--exact", "the_declared_project_decides_whatever_the_environment_says"])
        .env(RETIRED_OVERRIDE, &elsewhere)
        .output()
        .expect("re-run this test binary");

    assert!(
        out.status.success(),
        "with {RETIRED_OVERRIDE} naming {} the sentinel moved: the resolver still honours the \
         retired variable.\n--- child stdout ---\n{}\n--- child stderr ---\n{}",
        elsewhere.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

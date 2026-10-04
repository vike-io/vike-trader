//! **`VIKE_HALT_FILE` decides nothing any more** (decision 0099) — the sentinel's path is the
//! composition root's declared project, `<project>/settings/state/HALT`, and no process variable
//! moves it.
//!
//! # Why a variable that is merely IGNORED is the dangerous half
//!
//! The retirement has two halves, and this file is the second. The first is the startup refusal
//! (`vike_config::REMOVED_ENV`): a process that still carries the variable does not start, so the
//! operator who wrote it into a unit is told on the spot. This file holds what the refusal
//! relies on — that the resolver, reached by a binary that does NOT refuse (a test, a tool, an
//! older composition root), has stopped honouring it. A resolver that still honoured it would be
//! worse than either half alone: the refusal would say "no longer read" while a file named by it
//! was in fact being watched, and the next operator to read that sentence would `touch` the
//! default path instead.
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

use std::path::{Path, PathBuf};
use std::process::Command;

use vike_bridge_core::halt::{HALT_FILE, declare_project_state_dir, halt_path_from_env};

/// The variable this file proves is retired. Spelled here rather than imported: the resolver no
/// longer exports a name for it, which is the point.
const RETIRED_OVERRIDE: &str = "VIKE_HALT_FILE";

/// A private scratch directory whose `Drop` removes it — `halt_declaration.rs`'s `Scratch`, for the
/// same reason (this crate has no `tempfile` dev-dependency) and with the same property: it is
/// removed on the unwinding path too, which a trailing `remove_dir_all` is not.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-halt-retired-{tag}-{nanos}"));
        std::fs::create_dir_all(&p).expect("scratch dir");
        Self(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **The declared project's state directory decides the sentinel, whatever the environment says.**
///
/// True in a clean environment (the default the units now rely on) and, re-run by
/// [`a_set_halt_file_variable_decides_nothing`], true in one that names an override.
#[test]
fn the_declared_project_decides_whatever_the_environment_says() {
    let scratch = Scratch::new("declared");
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

/// The retired variable, set in a child's environment to a path that is NOT the declared one, moves
/// nothing: the child's resolution is still the declared project's.
#[test]
fn a_set_halt_file_variable_decides_nothing() {
    let scratch = Scratch::new("elsewhere");
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

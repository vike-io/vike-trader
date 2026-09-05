//! **The DECLARATION reaches the memoized process-wide sentinel** — the one link
//! `crates/vike-bridge-core/tests/halt_default_path.rs` cannot cover.
//!
//! That file drives the pure resolution (`crates/vike-bridge-core/src/halt.rs`'s `halt_path_for`,
//! with `crates/vike-boot/src/lib.rs`'s `boot` supplying rung 2's project) and proves the ANSWER is
//! right. What it cannot reach is `halt_path_from_env`, which is the function every mount actually
//! calls: it memoizes in a `OnceLock`, so a `declare_project_state_dir` that stored its value where
//! nothing read it would leave every assertion over there green while the running daemon kept
//! resolving off its working directory.
//!
//! # Why a file of its own
//!
//! `halt_path_from_env` is resolved ONCE PER PROCESS, and cargo runs one integration-test file's
//! tests as threads in ONE process. So the declaration and the resolution below poison that process
//! for any sibling test — which is exactly why they get a process nobody else is in, the same
//! reasoning `crates/vike-paper/tests/paper_halt_process_wide.rs` is split out for.
//!
//! # What it deliberately does not do
//!
//! It never calls `std::env::set_var`. `VIKE_HALT_FILE` outranks the declaration by design, so a
//! process that already has one set cannot prove anything here — the test SKIPS loudly rather than
//! passing vacuously (this workspace does not mutate the environment under threads; see
//! `crates/vike-bridge-core/src/credentials.rs`).

use std::path::{Path, PathBuf};

use vike_bridge_core::halt::{
    HALT_FILE, HALT_FILE_ENV, declare_project_state_dir, halt_path_from_env,
};

/// A private scratch directory whose `Drop` removes it. `vike-bridge-core` has no `tempfile`
/// dev-dependency and this test adds none, so the guard is hand-rolled — it is
/// `crates/vike-bridge-core/tests/halt_default_path.rs`'s `Scratch` verbatim, minus the unix
/// permission restore that file's own negative control needs.
///
/// ⚠ **It used to be a bare `fn scratch(tag) -> PathBuf` plus a `remove_dir_all` at the END of the
/// test, and the doc comment above it NAMED `tempfile` while using none.** That mattered beyond
/// style. `crates/vike-ops/tests/journal_scratch_gate.rs`'s tree rule matched
/// `s.contains("tempfile")` over the RAW file text, so the mention in this file's PROSE — prose
/// saying the crate does not have the dependency — was what exempted it from the rule. This file is
/// one of the two measured escapes that made that gate strip comments and strings before matching.
/// The repair is the property the exemption had been claiming: a trailing `remove_dir_all` cleans
/// up only when the test PASSES, while a `Drop` runs on the unwinding path too — which is where the
/// 211 GB that gate exists for accumulated.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-halt-decl-{tag}-{nanos}"));
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

/// **The process-wide sentinel is the DECLARED project's, and a second declaration cannot move it.**
///
/// One test rather than three, because all three facts are about the same one-shot resolution and
/// splitting them would make the file's outcome depend on thread scheduling.
#[test]
fn the_declared_project_decides_the_process_wide_sentinel() {
    if std::env::var(HALT_FILE_ENV).is_ok_and(|v| !v.trim().is_empty()) {
        eprintln!(
            "SKIPPED: {HALT_FILE_ENV} is set in this process's environment, and it outranks the \
             declaration by design — nothing here would be proven"
        );
        return;
    }

    // BOUND for the whole test: dropping the guard removes the tree, so a failing assertion below
    // cleans up exactly like a passing one.
    let scratch = Scratch::new("declared");
    let root = scratch.path();
    let state = root.join("named-project").join("settings").join("state");
    std::fs::create_dir_all(&state).expect("the declared state directory");

    declare_project_state_dir(Some(state.clone())).expect("the first declaration is accepted");

    // THE PROPERTY: the memoized resolver every mount calls uses the declaration.
    assert_eq!(
        halt_path_from_env(),
        state.join(HALT_FILE),
        "`halt_path_from_env` must resolve rung 2 from the composition root's declaration — this \
         is the call `ExecActor::spawn`, `vike_mount`'s paper arm and the tradehub's startup \
         advisory all make"
    );

    // A SECOND, DIFFERENT declaration is refused and changes nothing. One process has one sentinel:
    // a kill switch whose path could differ between two submits is not a kill switch.
    let other = root.join("other-project").join("settings").join("state");
    let err = declare_project_state_dir(Some(other.clone()))
        .expect_err("a second, different declaration must be reported");
    assert!(
        err.contains(&state.display().to_string()) && err.contains(&other.display().to_string()),
        "the refusal must name BOTH directories so an operator can tell which one is in force: \
         {err}"
    );
    assert_eq!(halt_path_from_env(), state.join(HALT_FILE), "…and the answer is unchanged");

    // Repeating the declaration ALREADY IN FORCE is not a fault, even after the path resolved: it
    // changed nothing, so reporting it would teach a root to stop reporting the errors that matter.
    declare_project_state_dir(Some(state.clone()))
        .expect("re-declaring the directory already in force is a no-op, not a failure");

    // …and the scratch tree carried no sentinel through any of it: resolving a path must never
    // create one (that is `halt_path_arming_error`'s probe's rule, and it probes a sibling name).
    assert!(
        !state.join(HALT_FILE).exists(),
        "resolving the sentinel must not ARM it — an operator's `touch` is the only thing that does"
    );

    // …and no `remove_dir_all` here on purpose: `Scratch`'s `Drop` removes the tree, and it does so
    // on the path a trailing call cannot reach — the one where an assertion above failed.
}

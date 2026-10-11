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
//! It never calls `std::env::set_var` (this workspace does not mutate the environment under
//! threads).

use vike_bridge_core::halt::{HALT_FILE, declare_project_state_dir, halt_path_from_env};

/// **The process-wide sentinel is the DECLARED project's, and a second declaration cannot move it.**
///
/// One test rather than three, because all three facts are about the same one-shot resolution and
/// splitting them would make the file's outcome depend on thread scheduling.
#[test]
fn the_declared_project_decides_the_process_wide_sentinel() {
    // BOUND for the whole test: dropping the guard removes the tree, so a failing assertion below
    // cleans up exactly like a passing one.
    let scratch = tempfile::tempdir().expect("scratch dir");
    let root = scratch.path();
    let state = root.join("named-project").join("settings").join("state");
    std::fs::create_dir_all(&state).expect("the declared state directory");

    declare_project_state_dir(Some(state.clone())).expect("the first declaration is accepted");

    // THE PROPERTY: the memoized resolver every mount calls uses the declaration.
    assert_eq!(
        halt_path_from_env(),
        state.join(HALT_FILE),
        "`halt_path_from_env` must resolve rung 2 from the composition root's declaration — this \
         is the call `vike_mount`'s `process_facts`, its paper arm and the tradehub's startup \
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
}

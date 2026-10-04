// Shared across independently-compiled integration-test binaries (mount_scripted.rs,
// xemm_scripted.rs); each uses only a subset, so per-binary dead-code analysis flags the rest.
#![allow(dead_code)]
//! The one thing every mount-driving test binary in this crate needs: proof that its verdict does
//! not depend on whether an operator's HALT sentinel happens to exist on the box running it.
//!
//! A paper MOUNT is HALT-armed by design (`vike_mount::PaperHalt` carries the argument), so a test
//! that drives one and expects fills inherits the operator kill switch unless it pins its own
//! sentinel. That is not a hypothetical: MEASURED on the CI box, `VIKE_HALT_FILE=<an existing file>
//! cargo nextest run` over the CI roster turned 22 tests red — 5 of them in this crate — while the
//! same command without it ran 7147/7147 green. (That variable is retired, decision 0099; a box now
//! engages the same sentinel with `touch <project>/settings/state/HALT`, and this harness engages
//! one the same way — see below.)

/// The substring every halt-indifference test's NAME must contain, and the `--skip` filter the
/// child run below is given. Spelled once so the two cannot disagree — a mismatch would make the
/// child re-run the indifference test, which re-runs the binary, forever.
pub const HALT_INDIFFERENCE: &str = "indifferent_to_an_engaged_halt_sentinel";

/// Re-run THIS test binary with an operator HALT sentinel ENGAGED and require the SAME verdict.
///
/// ⚠ **This is the equality the [`vike_mount::PaperHalt`] pinning exists to produce, expressed as a
/// test so it cannot silently regress.** Pinning each mount is correct but is per-call-site
/// discipline: one future test that reaches for `build_paper_maker_core`/`build_paper_xemm_core`
/// re-inherits the process-wide sentinel, passes on every clean box, and fails only where the file
/// happens to exist. This asks the question directly — *does anything in this binary change verdict
/// when the sentinel is on?* — and asks it on EVERY box, because the sentinel it engages is one
/// this function writes rather than one the machine happened to have. Same lesson
/// `crates/vike-paper/tests/paper_halt_process_wide.rs` records: a proof that holds only where a
/// file already exists is not a proof.
///
/// Mechanism, deliberately with no new API and no `set_var`: libtest binaries accept their own
/// arguments, so `current_exe()` plus a child process is the whole of it. The child runs with its
/// WORKING DIRECTORY inside a synthetic project (`<tmp>/settings/state/HALT` already on disk), and
/// since a test binary declares no composition root's project, `halt_path_from_env` WALKS UP from
/// that directory and lands on exactly that file — the process-wide sentinel, engaged, whatever
/// else the box has. (Until decision 0099 this was `VIKE_HALT_FILE=<a file>`, which outranked the
/// walk; the override is retired, and the walk is what the sentinel's default always was.)
/// `--skip HALT_INDIFFERENCE` so the child does not re-enter here; `--test-threads=1` so a failure
/// is the sentinel's doing and not scheduling. `set_var` is unavailable for the usual reason: cargo
/// runs a binary's tests as threads in ONE process and `halt_path_from_env` memoizes in a
/// `OnceLock`.
///
/// ⚠ The project lives in a `tempfile::TempDir` BOUND for the rest of this function — it must
/// outlive `Command::output()`, which is what blocks until the child has finished reading it — and
/// its `Drop` removes the tree, on the unwind path too. That replaces
/// `env::temp_dir().join(format!("vike-run-halt-indifference-{pid}"))` plus a hand `remove_file`/
/// `remove_dir` pair: those ran only on the success path, so every failing run leaked the
/// directory, and the PID name could collide with an entry the other the CI box user owns, at which
/// point `create_dir_all` succeeds (it exists) and the write fails `PermissionDenied` — an
/// intermittent red on a box where CI runs as `the CI user` and the verification lanes as
/// `the operator`.
pub fn assert_indifferent_to_an_engaged_halt_sentinel() {
    // We ARE the child. `--skip` should already have excluded this test; this is the belt to that
    // braces, because unbounded recursion is a far worse failure mode than a red assert.
    if std::env::args().any(|a| a == HALT_INDIFFERENCE) {
        return;
    }
    let dir = tempfile::Builder::new()
        .prefix("vike-mount-halt-indifference-")
        .tempdir()
        .expect("temp project dir");
    // A `settings/` directory is the deployed-shape project marker (`vike_model::paths::state_path`), so
    // the walk from `dir` stops here and the sentinel is `<dir>/settings/state/HALT`.
    let state = dir.path().join("settings").join("state");
    std::fs::create_dir_all(&state).expect("the project's state directory");
    let sentinel = state.join("HALT");
    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    assert!(
        sentinel.exists(),
        "the sentinel must be ON DISK before the child runs, or this test passes vacuously — the \
         exact defect it exists to prevent"
    );

    let exe = std::env::current_exe().expect("this test binary's own path");
    let out = std::process::Command::new(&exe)
        .current_dir(dir.path())
        .args(["--test-threads=1", "--skip", HALT_INDIFFERENCE])
        .output()
        .expect("re-run this test binary");

    assert!(
        out.status.success(),
        "this binary's verdict CHANGED when an operator HALT sentinel was engaged. Some test here \
         stands up a paper mount without pinning its sentinel, so it inherits the operator kill \
         switch off the box — pass `PaperMountOpts {{ halt: PaperHalt::Pinned(..), .. }}` (or \
         `build_paper_xemm_core_with(.., &PaperHalt::Pinned(..))`) instead.\n--- child stdout \
         ---\n{}\n--- child stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

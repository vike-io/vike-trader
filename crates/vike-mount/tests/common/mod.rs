// Shared by several integration-test binaries; each uses a subset.
#![allow(dead_code)]
//! What every mount-driving test binary here needs: proof that its verdict does not depend on
//! whether an operator's HALT sentinel exists on the box running it.
//!
//! A paper MOUNT is HALT-armed by design (`vike_mount::PaperHalt`), so a test expecting fills
//! inherits the operator kill switch unless it pins its own sentinel. MEASURED on the CI box: an engaged
//! sentinel over the CI roster turned 22 tests red (5 in this crate) against 7147/7147 green
//! without it. A box engages it with `touch <project>/settings/state/HALT` (decision 0099); this
//! harness does the same. Also the one spelling of `no_halt` and `wait_until`.

use std::time::{Duration, Instant};

use vike_mount::PaperHalt;

/// The substring every halt-indifference test's NAME contains AND the child run's `--skip` filter:
/// spelled once, since a mismatch would make the child re-run the binary forever.
pub const HALT_INDIFFERENCE: &str = "indifferent_to_an_engaged_halt_sentinel";

/// Re-run THIS test binary with an operator HALT sentinel ENGAGED and require the SAME verdict.
///
/// ⚠ **The equality [`vike_mount::PaperHalt`] pinning exists to produce, as a test so it cannot
/// silently regress.** Pinning is per-call-site discipline: one future test reaching for
/// `build_paper_maker_core`/`build_paper_xemm_core` re-inherits the sentinel and fails only where
/// the file exists. This asks on EVERY box, with a sentinel it writes itself — a proof that holds
/// only where a file already exists is not one
/// (`crates/vike-paper/tests/paper_halt_process_wide.rs`).
///
/// Mechanism (no new API, no `set_var`: tests are threads of ONE process and `halt_path_from_env`
/// memoizes in a `OnceLock`): re-run `current_exe()` as a child whose WORKING DIRECTORY is a
/// synthetic project with `settings/state/HALT` on disk; a test binary declares no project, so
/// `halt_path_from_env` walks up to exactly that file. `--skip HALT_INDIFFERENCE` so the child does
/// not re-enter; `--test-threads=1` so a failure is the sentinel's, not scheduling's.
///
/// ⚠ The project is a `tempfile::TempDir` BOUND to the end of this function (it must outlive
/// `Command::output()`), whose `Drop` removes it on unwind too. The old PID-named `/tmp` dir with
/// hand removal leaked on every failing run and could collide with the other the CI box user's entry
/// (`PermissionDenied`: an intermittent red, CI runs as `the CI user`, lanes as `the operator`).
pub fn assert_indifferent_to_an_engaged_halt_sentinel() {
    // We ARE the child. Belt to `--skip`'s braces: unbounded recursion is worse than a red assert.
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

/// A test binary's OWN operator-HALT sentinel: a path it NAMES and never CREATES, under a fresh
/// root whose name starts with `prefix` (each binary passes its own).
///
/// ⚠ The sentinel is a child of a `tempfile::TempDir` returned ALONGSIDE it; the caller must BIND
/// that guard for the test's duration. A fixed `/tmp` name made "this path does not exist" a claim
/// about everything else on a shared box; a fresh random root makes it a property of THIS run, so
/// no leftover can hand a mount a kill switch.
pub fn no_halt(prefix: &str) -> (tempfile::TempDir, PaperHalt) {
    let root = tempfile::Builder::new().prefix(prefix).tempdir().expect("temp sentinel root");
    let sentinel = root.path().join("HALT");
    (root, PaperHalt::Pinned(sentinel))
}

// Same as `crates/vike-tradehub/tests/daemon/support.rs`'s `wait_until` (`secs`, 10 ms poll); NOT
// `crates/vike-cli/tests/common/mod.rs`'s (20 ms) nor the `Duration`-taking copies.
/// Poll `cond` up to `secs`, returning whether it became true (the core folds on its own thread;
/// scripted sends are lossless and ordered, so this only waits for the coalesced snapshot/fill).
pub fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

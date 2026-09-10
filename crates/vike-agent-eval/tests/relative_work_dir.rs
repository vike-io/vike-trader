//! The shape the shipped binary actually runs in: a RELATIVE work directory.
//!
//! ⚠ THIS FILE EXISTS BECAUSE THE OTHER SUITE STRUCTURALLY CANNOT REACH THIS SHAPE.
//! `crates/vike-agent-eval/tests/scripted_pipeline.rs` hands `run_case` a `tempfile::tempdir()`
//! path, which is always ABSOLUTE — while `crates/vike-agent-eval/src/main.rs`'s default
//! `--work-dir` is the relative `agent-eval-work`. Those are not the same input, and the difference
//! was a total failure of the binary that nothing red: `PaperNode::spawn` sets the daemon's
//! `current_dir` to the project root, so every relative path handed to that child — `--config`,
//! `VIKE_SETTINGS_DIR`, `VIKE_STATE_ROOT` — was re-resolved against the child's NEW directory. The
//! daemon exited in milliseconds with `bad profile …: No such file or directory` and could not see
//! the credential store holding its own node keys, so all five node-bearing cases failed while the
//! test twin passed. `Project::create` absolutizes at that one boundary now, and this is what holds
//! it there.
//!
//! ⚠ A SEPARATE TEST BINARY, deliberately: this test changes the process's working directory, which
//! is process-global state. `tests/` files compile to one binary each, so it can never be running in
//! the same process as the suite next door, whether the runner is nextest (a process per test) or a
//! bare `cargo test` (a thread per test).

use std::path::PathBuf;

use vike_agent_eval::cases::by_name;
use vike_agent_eval::driver::Scripted;
use vike_agent_eval::harness::{Binaries, run_case};
use vike_agent_eval::locate_binary;

/// Restores the working directory however the test leaves — a panic included. Without it a failure
/// here would leave the process pointed inside a directory that is about to be deleted, and the
/// tempdir's own cleanup would then fail for a second, unrelated reason.
struct Cwd(PathBuf);

impl Drop for Cwd {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

#[test]
fn a_relative_work_dir_still_stands_a_node_up() {
    // Resolved BEFORE the directory changes: `locate_binary`'s last resort is the relative
    // `target/debug`, and the point of this test is to move the ground under relative paths.
    let bins = match (locate_binary("vike-cli", None), locate_binary("vike-tradehub", None)) {
        (Ok(vike_cli), Ok(vike_tradehub)) => Binaries { vike_cli, vike_tradehub },
        (cli, tradehub) => panic!(
            "this test drives the SHIPPED binaries and one is not built in this tree.\n  \
             vike-cli:       {}\n  vike-tradehub:  {}\n\
             Build them first:\n  \
             cargo build -p vike-cli -p vike-tradehub --bin vike-cli --bin vike-tradehub",
            cli.map_or_else(|e| e, |p| p.display().to_string()),
            tradehub.map_or_else(|e| e, |p| p.display().to_string()),
        ),
    };
    assert!(bins.vike_cli.is_absolute(), "the binary paths must survive the chdir below");
    assert!(bins.vike_tradehub.is_absolute(), "the binary paths must survive the chdir below");

    let home = tempfile::tempdir().expect("a throwaway directory to run inside");
    let _restore = Cwd(std::env::current_dir().expect("a current directory"));
    std::env::set_current_dir(home.path()).expect("enter the throwaway directory");

    // The binary's own default, verbatim — the whole subject of this test.
    let work = PathBuf::from("agent-eval-work");
    assert!(work.is_relative(), "the input must be relative or this test proves nothing");

    // `halt-trading` is the cheapest case that exercises the whole chain: a paper node is spawned,
    // a write goes through the two-call gate, and the verdict is read back OUT of the node's own
    // published state — so a daemon that never started cannot produce a pass.
    let case = by_name("halt-trading").expect("the halt-trading case is in the suite");
    let mut driver = Scripted::new((case.script)());
    let verdict = run_case(case, &bins, &mut driver, &work, &[]);

    assert!(
        verdict.pass,
        "a relative work directory must stand a node up exactly as an absolute one does.\n\
         error:  {:?}\nchecks: {:#?}",
        verdict.error, verdict.checks
    );
    assert!(!verdict.checks.is_empty(), "an expectation-free case would pass for free");

    // ...and the child really was handed an absolute project root, which is the property that makes
    // the pass above hold rather than a coincidence of this box's working directory.
    let project = work.join(case.name).join("project");
    assert!(
        project.is_dir(),
        "the case's project directory should sit under the relative work dir: {}",
        project.display()
    );
}

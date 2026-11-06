//! Running a SHIPPED hub binary from its own integration tests and REQUIRING it to exit: the deadlined
//! spawn the gatewayhub's and the datahub's `help_cli` suites share (each carried a copy until
//! 2026-10-31). A hub binary that answers a question about its command line by starting to serve would
//! park `Command::output()` forever holding a port, so every spawn here is killed at its deadline and
//! reported as the defect it is.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Run `bin` (a `CARGO_BIN_EXE_*` path) with `args`, the extra environment `env` and without the names in
/// `remove`, and REQUIRE it to exit within `deadline`. A child still alive then is killed, and the panic
/// says `still_running`: what a live child means for that binary and that test.
pub fn run_deadlined(
    bin: &str,
    args: &[&str],
    env: &[(&str, &str)],
    remove: &[&str],
    deadline: Duration,
    still_running: &str,
) -> Output {
    let name = Path::new(bin).file_stem().map_or(bin.into(), |s| s.to_string_lossy());
    let mut cmd = Command::new(bin);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    for var in remove {
        cmd.env_remove(var);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap_or_else(|e| panic!("spawn {name} {args:?}: {e}"));

    let until = Instant::now() + deadline;
    loop {
        match child.try_wait().expect("poll the child") {
            Some(_) => break,
            None if Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`{name} {}` did not exit within {deadline:?} — {still_running}",
                    args.join(" ")
                );
            }
            // Polling rather than a blocking wait: `wait_with_output` cannot be deadlined, and the
            // output here is far too small to fill a pipe while we sleep.
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    child.wait_with_output().expect("collect the exited child's output")
}

/// The negative half of a `--help` suite, so "exit 0 on --help" is never bought by making everything
/// exit 0: the run `out` of an unrecognised `flag` is a usage error on stderr, naming the flag, with
/// nothing on stdout.
pub fn assert_an_unknown_flag_is_a_usage_error(out: &Output, flag: &str) {
    assert!(!out.status.success(), "an unknown flag must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(flag), "the error names the offending argument: {stderr:?}");
    assert!(stderr.contains("Usage:"), "and explains itself with the usage: {stderr:?}");
    assert!(out.stdout.is_empty(), "nothing on stdout: {:?}", out.stdout);
}

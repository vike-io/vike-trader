//! `vike-datahub --help` is a SUCCESS whose text is STDOUT, and `--version` exists — asserted over
//! the SHIPPED binary, because only a real run shows a status and a stream.
//!
//! This binary read no argv at all. Under `serve-datafusion` that meant `vike-datahub --help`
//! **opened a hist store and bound a listener** — help STARTED A SERVER; without the feature every
//! invocation, `--help` included, printed the missing-backend message to stderr and exited 2, which
//! reads as a first-class feature error for what was simply an unimplemented flag.
//!
//! ⚠ **Both build configurations are asserted by the SAME test**, deliberately: the whole point is
//! that a caller's `--help` does not depend on which features this binary was compiled with. The
//! feature only decides whether it can SERVE. `cargo test -p vike-datahub` runs this on the default
//! (DataFusion-free) build and `cargo test -p vike-datahub --features serve-datafusion` on the
//! serving one; a fix that landed in only one `main` fails in the other lane.
//!
//! ⚠ **Every spawn is DEADLINED, and that is not defensive decoration.** Verified on the pre-fix
//! tree: `vike-datahub --help` under `serve-datafusion` opened the store, bound `127.0.0.1:7878`
//! and served — so `Command::output()`, which blocks until the child exits, HUNG. A regression of
//! this fix must fail in 30 s with a diagnosis, not park a CI job forever holding a port.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a `--help`/`--version`/usage-error invocation may take. Enormous for what it measures
/// (three of these finish in milliseconds) because it is not a performance budget — it is the line
/// between "exited" and "is serving".
const EXIT_DEADLINE: Duration = Duration::from_secs(30);

/// Run the shipped binary and REQUIRE it to exit. A child still alive at [`EXIT_DEADLINE`] is
/// killed and reported as the defect it is: this binary answering a question about its command line
/// by starting a server.
fn run(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-datahub"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn vike-datahub {args:?}: {e}"));

    let deadline = Instant::now() + EXIT_DEADLINE;
    loop {
        match child.try_wait().expect("poll the child") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`vike-datahub {}` did not exit within {EXIT_DEADLINE:?} — it is still \
                     RUNNING, which for this binary means it opened a store and bound a listener \
                     instead of answering. That is the exact pre-fix behaviour of `--help` under \
                     `serve-datafusion`.",
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

/// Both spellings: exit 0, usage on stdout, nothing on stderr — and, load-bearing, **no mention of
/// the missing feature**. A build that cannot serve can still describe its own command line, and
/// answering "rebuild with --features serve-datafusion" told a caller the wrong thing about the
/// wrong question.
#[test]
fn help_exits_zero_with_usage_on_stdout_in_every_build() {
    for flag in ["--help", "-h"] {
        let out = run(&[flag]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`vike-datahub {flag}` must exit 0 — a non-zero --help breaks `set -e` and every \
             packaging smoke test. status: {:?}, stderr: {stderr}",
            out.status.code()
        );
        assert!(
            stdout.contains("usage:"),
            "`vike-datahub {flag}` must print its usage to STDOUT; stdout: {stdout:?}, \
             stderr: {stderr:?}"
        );
        assert!(
            stderr.trim().is_empty(),
            "…and nothing on stderr: a successful --help produces no diagnostics; \
             stderr: {stderr:?}"
        );
    }
}

/// `--version`/`-V`: the crate version on stdout, exit 0. It was unrecognised in both builds.
#[test]
fn version_prints_the_crate_version_on_stdout() {
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`vike-datahub {flag}` must exit 0; status: {:?}, stderr: {stderr}",
            out.status.code()
        );
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "`vike-datahub {flag}` must print the version {:?} on stdout; stdout: {stdout:?}",
            env!("CARGO_PKG_VERSION")
        );
    }
}

/// Spawn with EXTRA ENVIRONMENT, deadlined exactly as [`run`] is.
///
/// Separate rather than a parameter on `run` because every existing caller passes none, and a
/// `&[]` at four call sites reads as noise. The child inherits this process's environment plus
/// `env`; nothing is removed, so a variable the parent happens to carry is still visible — which is
/// why the one caller below pins `VIKE_SETTINGS_DIR` at an EMPTY directory rather than trusting the
/// box not to have a real credential store above the test's working directory.
#[cfg(feature = "serve-datafusion")]
fn run_with_env(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-datahub"));
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap_or_else(|e| panic!("spawn vike-datahub {args:?}: {e}"));

    let deadline = Instant::now() + EXIT_DEADLINE;
    loop {
        match child.try_wait().expect("poll the child") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`vike-datahub {}` did not exit within {EXIT_DEADLINE:?} — it is STILL SERVING. \
                     For this test that is the whole defect: a non-loopback bind with no node keys \
                     must refuse, not bind.",
                    args.join(" ")
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    child.wait_with_output().expect("collect the exited child's output")
}

/// THE ONE END-TO-END ASSERTION OF THIS BRANCH: a non-loopback bind with no node keys REFUSES.
///
/// ⚠ It exists because the change nearly shipped without it, on a stated reason that was false —
/// that a match arm inside `main` is unreachable from a test. This file is the refutation: it has
/// spawned the shipped binary and asserted status and streams since it was written. The unit tests
/// beside `bind_decision` cover the pure POLICY; only a real run covers the bin actually exiting
/// instead of binding.
///
/// ⚠ **Feature-gated, and the gate is load-bearing rather than tidy.** Without `serve-datafusion`
/// this binary exits 2 for an entirely different reason (no serving backend compiled in), so an
/// ungated version of this test would PASS on the default build while proving nothing — the
/// vacuous-pass shape. The stderr assertions below are what separate the two exits, and the gate is
/// what stops the wrong one being accepted.
///
/// `VIKE_SETTINGS_DIR` points at an empty directory so the credential store is genuinely absent:
/// without it the project walk climbs out of the test's working directory and can find a real
/// `secrets.env` on a developer's box, which would key the server and silently invert the test.
#[cfg(feature = "serve-datafusion")]
#[test]
fn a_non_loopback_bind_with_no_keys_refuses_instead_of_serving() {
    // ⚠ A `TempDir` HELD FOR THE WHOLE SCOPE, not `env::temp_dir()` plus a trailing
    // `remove_dir_all`. The first draft did the latter and
    // `crates/vike-ops/tests/journal_scratch_gate.rs`'s `no_new_unguarded_temp_paths_outside_vike_core`
    // rejected it, correctly: cleanup that is not in a `Drop` does not run when the work fails, and
    // every assertion below can panic. That gate exists because a leak of exactly this shape reached
    // 211 GB. Binding to `_` would drop it immediately and guard nothing — hence the name.
    let empty = tempfile::TempDir::new().expect("create the empty settings dir");

    let out = run_with_env(
        &[],
        &[
            ("VIKE_SETTINGS_DIR", empty.path().to_str().expect("utf-8 temp path")),
            ("VIKE_DATAHUB_ADDR", "0.0.0.0:7878"),
            ("VIKE_DATAHUB_ALLOW_PUBLIC_BIND", "1"),
        ],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a keyless non-loopback bind must exit 2, not serve. stderr: {stderr:?}"
    );
    // Distinguishes this refusal from the missing-backend exit, which shares the status code.
    assert!(
        stderr.contains("VIKE_DATAHUB_OBSERVE_KEY") || stderr.contains("authenticat"),
        "the refusal must explain itself in terms of AUTH, not merely fail: {stderr:?}"
    );
    assert!(
        !stderr.contains("serve-datafusion"),
        "this must be the auth refusal, not the missing-backend exit: {stderr:?}"
    );
}

/// The negative half, so "exit 0 on --help" is never bought by making everything exit 0: an
/// unrecognised flag is a usage error on stderr. It used to be SILENTLY IGNORED — this binary
/// parsed no argv, so a typo in a systemd `ExecStart=` started the server as if nothing were wrong.
#[test]
fn an_unknown_argument_fails_on_stderr_rather_than_being_ignored() {
    let out = run(&["--adr=127.0.0.1:7878"]);
    assert!(!out.status.success(), "an unknown flag must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--adr"), "the error names the offending argument: {stderr:?}");
    assert!(stderr.contains("usage:"), "and explains itself with the usage: {stderr:?}");
    assert!(out.stdout.is_empty(), "nothing on stdout: {:?}", out.stdout);
}

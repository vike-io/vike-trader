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
//!
//! Two `serve-datafusion` tests also live here because they need the server to BOOT and then EXIT,
//! and the keyless non-loopback refusal is the path that does both: the refusal itself, and the
//! file-log level read from the settings row.

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
/// Separate rather than a parameter on `run` because most callers pass none, and a `&[]` at four
/// call sites reads as noise. The child inherits this process's environment plus `env`, minus the
/// names in `remove`; anything not removed that the parent happens to carry is still visible — which
/// is why both callers pin `VIKE_SETTINGS_DIR` at a directory of their own rather than trusting the
/// box not to have a real credential store above the test's working directory.
#[cfg(feature = "serve-datafusion")]
fn run_with_env(args: &[&str], env: &[(&str, &str)], remove: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-datahub"));
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    for name in remove {
        cmd.env_remove(name);
    }
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
/// settings database on a developer's box, which would key the server and silently invert the
/// test.
#[cfg(feature = "serve-datafusion")]
#[test]
fn a_non_loopback_bind_with_no_keys_refuses_instead_of_serving() {
    // ⚠ A `TempDir` HELD FOR THE WHOLE SCOPE, not `env::temp_dir()` plus a trailing
    // `remove_dir_all`. The first draft did the latter and
    // `crates/vike-ops/tests/hygiene/journal_scratch_gate/tree_rule.rs`'s `no_new_unguarded_temp_paths_outside_vike_core`
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
        &[],
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

/// **The rolling file's level is the `preferences.log_file_level` ROW**
/// (`docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md`, verdict 3), read
/// by the shipped binary out of its settings database before its log subscriber exists.
///
/// The run is the refusal above, because it is the one path that boots, logs and EXITS: the
/// identity line reaches the file at `info`, the bind refusal at `error`, and returning from `run`
/// drops the appender guards, which flushes the file. With the row at `error` the file keeps the
/// refusal and loses the identity line. The control run has no database at all and keeps both, so
/// the missing line is the row's doing, not a file layer that never wrote.
///
/// `VIKE_LOG_FILE_LEVEL` and `VIKE_LOG_DIR` are REMOVED from the child: the first still beats the
/// row by design, and the second would move the file out of `<settings>/state/logs`.
#[cfg(feature = "serve-datafusion")]
#[test]
fn the_file_log_level_is_the_settings_row() {
    let identity = format!("vike-datahub {}", env!("CARGO_PKG_VERSION"));
    let refusal = "refusing to start";

    let control = file_log_after_a_refused_bind(None);
    assert!(
        control.contains(&identity) && control.contains(refusal),
        "with no row the file is at its compiled default and holds both lines: {control}"
    );

    let quiet = file_log_after_a_refused_bind(Some("error"));
    assert!(quiet.contains(refusal), "the `error` refusal still reaches the file: {quiet}");
    assert!(
        !quiet.contains(&identity),
        "an `info` line reached a file whose row says `error`: the daemon did not read the row. \
         File: {quiet}"
    );
}

/// Boot the shipped binary into the keyless non-loopback refusal over a settings directory of its
/// own — holding a `preferences.log_file_level` row when `level` is given, no database otherwise —
/// and return every rolling file it wrote under `<settings>/state/logs`.
#[cfg(feature = "serve-datafusion")]
fn file_log_after_a_refused_bind(level: Option<&str>) -> String {
    // Held for the whole scope: the `Drop` is the cleanup (see the test above for why).
    let settings = tempfile::TempDir::new().expect("create the settings dir");
    if let Some(level) = level {
        vike_secrets::plant_settings_rows(
            settings.path(),
            &vike_secrets::StoredSettings {
                settings: vec![vike_secrets::SettingRow {
                    section: "preferences".to_string(),
                    key: "log_file_level".to_string(),
                    value: format!("\"{level}\""),
                }],
                ..Default::default()
            },
        )
        .expect("plant the preferences.log_file_level row");
    }
    let out = run_with_env(
        &[],
        &[
            ("VIKE_SETTINGS_DIR", settings.path().to_str().expect("utf-8 temp path")),
            ("VIKE_DATAHUB_ADDR", "0.0.0.0:7878"),
            ("VIKE_DATAHUB_ALLOW_PUBLIC_BIND", "1"),
        ],
        &["VIKE_LOG_FILE_LEVEL", "VIKE_LOG_DIR"],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "the keyless non-loopback bind must refuse; stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    let logs = settings.path().join("state").join("logs");
    let mut text = String::new();
    for entry in std::fs::read_dir(&logs).unwrap_or_else(|e| panic!("{}: {e}", logs.display())) {
        let path = entry.expect("a log directory entry").path();
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("vike-datahub"))
        {
            text.push_str(&std::fs::read_to_string(&path).expect("read a rolling log file"));
        }
    }
    text
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

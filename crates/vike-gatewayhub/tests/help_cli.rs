//! The SHIPPED `vike-gatewayhub` binary: `--help` is a success whose text is STDOUT, `--version`
//! exists, a typo is refused, and a real boot over a settings directory of its own ends the way it
//! must — asserted over the binary itself, because only a real run shows a status and a stream.
//!
//! ⚠ **Every spawn is DEADLINED, and that is not decoration.** A regression that made `--help`
//! start a daemon (the datahub's binary once did exactly that, binding its port) must fail in 30 s
//! with a diagnosis rather than park a CI job forever.
//!
//! ⚠ **Every boot test pins `VIKE_SETTINGS_DIR` at a directory of its own**, so the project walk
//! cannot climb out of the test's working directory into a real settings database on a developer's
//! box and silently invert the test, and it REMOVES `VIKE_LOG_DIR` and `VIKE_LOG_FILE_LEVEL` from
//! the child (the second is a retired variable the boot refuses, the first would move the log file
//! out of `<settings>/state/logs`).

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a `--help`/`--version`/usage-error/refusal invocation may take. Enormous for what it
/// measures — it is the line between "exited" and "is serving".
const EXIT_DEADLINE: Duration = Duration::from_secs(30);

/// Run the shipped binary with `args` and EXTRA ENVIRONMENT, and REQUIRE it to exit.
fn run_with_env(args: &[&str], env: &[(&str, &str)], remove: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-gatewayhub"));
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    for name in remove {
        cmd.env_remove(name);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap_or_else(|e| panic!("spawn vike-gatewayhub {args:?}: {e}"));

    let deadline = Instant::now() + EXIT_DEADLINE;
    loop {
        match child.try_wait().expect("poll the child") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`vike-gatewayhub {}` did not exit within {EXIT_DEADLINE:?} — it is still \
                     RUNNING, which for this binary means it started serving instead of answering",
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

/// No extra environment.
fn run(args: &[&str]) -> Output {
    run_with_env(args, &[], &[])
}

/// A boot over `settings` as the project's settings directory.
fn boot(settings: &std::path::Path) -> Output {
    run_with_env(
        &[],
        &[("VIKE_SETTINGS_DIR", settings.to_str().expect("utf-8 temp path"))],
        &["VIKE_LOG_DIR", "VIKE_LOG_FILE_LEVEL"],
    )
}

/// A settings database holding `config.gatewayhub_addr = addr`, or no database when `addr` is
/// `None`. Held by the caller for the whole scope: the `Drop` is the cleanup.
fn settings_with_addr(addr: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("create the settings dir");
    if let Some(addr) = addr {
        vike_secrets::plant_settings_rows(
            dir.path(),
            &vike_secrets::StoredSettings {
                settings: vec![vike_secrets::SettingRow {
                    section: "config".to_string(),
                    key: "gatewayhub_addr".to_string(),
                    value: format!("\"{addr}\""),
                }],
                ..Default::default()
            },
        )
        .expect("plant the address row");
    }
    dir
}

/// Every rolling file the child wrote under `<settings>/state/logs`, joined.
fn log_files(settings: &std::path::Path) -> String {
    let logs = settings.join("state").join("logs");
    let mut text = String::new();
    for entry in std::fs::read_dir(&logs).unwrap_or_else(|e| panic!("{}: {e}", logs.display())) {
        let path = entry.expect("a log directory entry").path();
        let ours = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("vike-gatewayhub"));
        if ours {
            text.push_str(&std::fs::read_to_string(&path).expect("read a rolling log file"));
        }
    }
    text
}

/// Both spellings: exit 0, usage on stdout, nothing on stderr. A daemon whose `--help` started a
/// server would answer a question about its command line by serving.
#[test]
fn help_exits_zero_with_usage_on_stdout() {
    for flag in ["--help", "-h"] {
        let out = run(&[flag]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "`{flag}` must exit 0; stderr: {stderr}");
        assert!(stdout.contains("Usage:"), "`{flag}` prints usage to STDOUT: {stdout:?}");
        assert!(stdout.contains("config.gatewayhub_addr"), "…and the row it reads: {stdout:?}");
        assert!(stderr.trim().is_empty(), "a successful --help prints no diagnostics: {stderr:?}");
    }
}

/// `--version`/`-V`: the crate version on stdout, exit 0.
#[test]
fn version_prints_the_crate_version_on_stdout() {
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "`{flag}` must exit 0");
        assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "`{flag}`: {stdout:?}");
    }
}

/// The negative half, so "exit 0 on --help" is never bought by making everything exit 0: an
/// unrecognised flag is a usage error on stderr, never silently ignored.
#[test]
fn an_unknown_argument_fails_on_stderr_rather_than_being_ignored() {
    let out = run(&["--adr=127.0.0.1:7883"]);
    assert!(!out.status.success(), "an unknown flag must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--adr"), "the error names the offending argument: {stderr:?}");
    assert!(stderr.contains("Usage:"), "and explains itself with the usage: {stderr:?}");
    assert!(out.stdout.is_empty(), "nothing on stdout: {:?}", out.stdout);
}

/// A clean boot runs the whole order up to the wire hook and the hook REFUSES to serve: exit 2, in
/// the words of the build — without the hub keys (this project holds none) the daemon is always
/// keyed and does not start; a build without the `data` feature links
/// no bridge and has no venue lane. The identity line and the bind guard's acceptance are in the file
/// log under the daemon's own prefix, so the refusal came AFTER the boot, not instead of it. A
/// daemon that returned 0 here, or sat waiting, would look like a gateway to the unit that starts it.
#[test]
fn a_clean_boot_refuses_to_serve_and_exits_two() {
    let settings = settings_with_addr(None);
    let out = boot(settings.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr:?}");
    let why = if cfg!(feature = "data") {
        "the gatewayhub hub keys are not provisioned"
    } else {
        "built without the `data` feature: no venue lane"
    };
    assert!(stderr.contains(why), "it says why it stopped ({why}): {stderr:?}");
    assert!(
        !stderr.contains("wire is not implemented"),
        "the wire exists now; the old refusal is gone: {stderr:?}"
    );

    let log = log_files(settings.path());
    assert!(
        log.contains(&format!("vike-gatewayhub {}", env!("CARGO_PKG_VERSION"))),
        "the identity line is the file's first: {log}"
    );
    assert!(log.contains("is loopback"), "the bind guard's acceptance is logged: {log}");
    assert!(log.contains(why), "the refusal is in the file log too: {log}");
}

/// THE END-TO-END ASSERTION OF THE BIND RULE: a non-loopback `config.gatewayhub_addr` row REFUSES —
/// exit 2, naming the row and saying nothing lifts it — and the process never reaches the wire.
/// The unit tests beside `resolve_bind` cover the policy; only a real run covers the binary
/// exiting instead of binding.
#[test]
fn a_non_loopback_row_refuses_instead_of_serving() {
    let settings = settings_with_addr(Some("0.0.0.0:7883"));
    let out = boot(settings.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr:?}");
    assert!(stderr.contains("config.gatewayhub_addr"), "the refusal names the row: {stderr:?}");
    assert!(stderr.contains("NO flag or key"), "…and that nothing lifts it: {stderr:?}");
    for wire_refusal in
        ["the gatewayhub hub keys are not provisioned", "built without the `data` feature"]
    {
        assert!(
            !stderr.contains(wire_refusal),
            "this must be the bind refusal, not the wire hook's exit (they share a status): \
             {stderr:?}"
        );
    }
}

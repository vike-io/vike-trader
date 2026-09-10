//! `vike-recorder --help` is a SUCCESS whose text is STDOUT, and `--version` exists.
//!
//! The daemon's parser answered `-h`/`--help` with `Err("")` — an EMPTY error string, chosen so the
//! caller's `eprintln!("{e}\n\n{USAGE}")` would print the usage without a message above it. The
//! effect on a caller is the same class of defect the sibling binaries had: exit **2**, the whole
//! help text on **stderr**, and a leading blank line. Only a run of the shipped binary shows any of
//! that, which is why this is a spawn test.

use std::process::Command;

/// Both spellings: exit 0, usage on stdout, stderr quiet.
#[test]
fn help_exits_zero_with_usage_on_stdout() {
    for flag in ["--help", "-h"] {
        let out = Command::new(env!("CARGO_BIN_EXE_vike-recorder"))
            .arg(flag)
            .output()
            .expect("run vike-recorder --help");

        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`vike-recorder {flag}` must exit 0 — a non-zero --help breaks `set -e` and every \
             packaging smoke test. status: {:?}, stderr: {stderr}",
            out.status.code()
        );
        assert!(
            stdout.contains("usage:"),
            "`vike-recorder {flag}` must print its usage to STDOUT; stdout: {stdout:?}, \
             stderr: {stderr:?}"
        );
        assert!(
            stderr.trim().is_empty(),
            "…and nothing on stderr: a successful --help produces no diagnostics; \
             stderr: {stderr:?}"
        );
    }
}

/// `--version`/`-V`: the crate version on STDOUT, exit 0.
///
/// It was unrecognised, so it hit `Args::parse`'s `unknown argument` arm and exited **2** on
/// stderr. `-V`, never `-v`: lowercase is verbosity everywhere else on the box.
#[test]
fn version_prints_the_crate_version_on_stdout() {
    for flag in ["--version", "-V"] {
        let out = Command::new(env!("CARGO_BIN_EXE_vike-recorder"))
            .arg(flag)
            .output()
            .expect("run vike-recorder --version");

        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`vike-recorder {flag}` must exit 0; status: {:?}, stderr: {stderr}",
            out.status.code()
        );
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "`vike-recorder {flag}` must print the version {:?} on stdout; stdout: {stdout:?}",
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            !stderr.contains("unknown argument"),
            "…and must be RECOGNISED, not routed to the unknown-argument arm; stderr: {stderr:?}"
        );
    }
}

/// The negative half: a genuine usage error still fails, and still explains itself on stderr.
#[test]
fn a_missing_profile_still_exits_non_zero_on_stderr() {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-recorder"))
        .output()
        .expect("run vike-recorder with no args");
    assert!(!out.status.success(), "a missing --profile must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage:"), "the usage error explains itself on stderr: {stderr:?}");
    assert!(out.stdout.is_empty(), "and prints nothing on stdout: {:?}", out.stdout);
}

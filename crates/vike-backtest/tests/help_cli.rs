//! `backtest --help` is a SUCCESS whose text is STDOUT — the shipped-binary gate for the `backtest`
//! bin, the sibling of `crates/vike-cli/tests/help_cli.rs`.
//!
//! `--help` was not recognised AT ALL. The flag fell straight through to the required-argument
//! check, so `backtest --help` printed
//!
//! ```text
//! backtest: --profile <path> is required (or --list to show strategies)
//! ```
//!
//! to **stderr** and exited **2** — the worst of the four surfaces this PR fixes, because it does
//! not merely fail: it tells the user they forgot a flag they never meant to pass, while the help
//! text they asked for demonstrably exists (`--list` already printed to stdout and exited 0).
//!
//! `#![cfg(feature = "datafusion-store")]` because the `backtest` bin carries
//! `required-features = ["datafusion-store"]`: without it the binary is not built, and
//! `env!("CARGO_BIN_EXE_backtest")` would not COMPILE. CI runs this in the `datafusion-store` lane
//! (`scripts/ci_feature_suite.sh`). Its own test binary rather than a `tests/parity.rs`-style group
//! member, per this crate's grouping rule: a file carrying a `#![cfg]` feature gate stays separate.
#![cfg(feature = "datafusion-store")]

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_backtest"))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run backtest {args:?}: {e}"))
}

/// Both spellings: exit 0, usage on stdout, stderr quiet — and specifically NOT the
/// missing-`--profile` diagnostic, which is the wrong answer to this question.
#[test]
fn help_exits_zero_with_usage_on_stdout() {
    for flag in ["--help", "-h"] {
        let out = run(&[flag]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`backtest {flag}` must exit 0 — a non-zero --help breaks `set -e` and every packaging \
             smoke test. status: {:?}, stderr: {stderr}",
            out.status.code()
        );
        assert!(
            stdout.contains("usage:"),
            "`backtest {flag}` must print its usage to STDOUT; stdout: {stdout:?}, \
             stderr: {stderr:?}"
        );
        assert!(
            !stderr.contains("is required"),
            "`backtest {flag}` must not answer a help request by naming a missing argument — that \
             is the defect this gate exists for; stderr: {stderr:?}"
        );
        assert!(
            stderr.trim().is_empty(),
            "…and nothing on stderr at all: a successful --help produces no diagnostics; \
             stderr: {stderr:?}"
        );
    }
}

/// `--version`/`-V`: the crate version on stdout, exit 0. It used to hit the same
/// missing-`--profile` arm as `--help`.
#[test]
fn version_prints_the_crate_version_on_stdout() {
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`backtest {flag}` must exit 0; status: {:?}, stderr: {stderr}",
            out.status.code()
        );
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "`backtest {flag}` must print the version {:?} on stdout; stdout: {stdout:?}",
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            stdout.starts_with("backtest "),
            "…named as the BINARY was invoked (`backtest`), not as the package \
             (`vike-backtest`) — a bug report quotes what it ran; stdout: {stdout:?}"
        );
    }
}

/// The negative half, so "exit 0 on --help" is never bought by making everything exit 0: a bare
/// invocation still fails, still on stderr, and still says what is missing.
#[test]
fn a_missing_profile_still_exits_non_zero_on_stderr() {
    let out = run(&[]);
    assert!(!out.status.success(), "a missing --profile must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--profile"), "the usage error still names it: {stderr:?}");
    assert!(out.stdout.is_empty(), "and prints nothing on stdout: {:?}", out.stdout);
}

/// `--list` was already correct (stdout, exit 0) and is the reason the broken `--help` was so
/// misleading — the help text existed in spirit. Pinned so the fix cannot regress its reference.
#[test]
fn list_stays_a_success_on_stdout() {
    let out = run(&["--list"]);
    assert!(out.status.success(), "`backtest --list` must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.trim().is_empty(), "…and print the registered strategies: {stdout:?}");
}

//! The numeric exit ladder, asserted over the SHIPPED binary.
//!
//! A script branches on these numbers, so they are a public interface: a rung's meaning may be
//! added to, never repurposed. `crates/vike-cli/src/exit.rs` argues each rung; this file is the
//! proof that the binary actually exits on it, which no unit test of a parser can see — every one
//! of these codes is produced by the `main` shim collapsing an `ExitCode`, several layers below
//! the function that decided it.
//!
//! ⚠ **This file asserts FOUR of the six rungs, and the two it omits are omitted because they do
//! not exist yet.** `Exit::Refused` (`4`) and `Exit::Venue` (`5`) are RESERVED — nothing in the
//! crate constructs either, so there is no invocation to write a case for, and a case that could
//! not fail would be worse than the gap. `crates/vike-cli/src/exit.rs` carries what has to exist
//! before they become live (a non-interactive order-write verb; the two write surfaces today are a
//! REPL and a JSON-RPC server, neither of which exits the process on a per-order decision). Wiring
//! either one is the same PR that adds its case here.
//!
//! ⚠ Every case pins the CHILD's environment (`Command::env` / `env_remove`, never
//! `std::env::set_var`, which is unsafe under threads and leaks across this binary's parallel
//! cases): without the settings redirect a run on a developer box resolves the REPO's settings
//! directory and reads a real credential store into a test's assertions.

use std::process::{Command, Output};

/// Run the shipped binary against an EMPTY settings directory and return its exit code.
///
/// The two removed-variable removals are not decoration: `vike_config::refuse_removed_env` runs
/// before any verb is routed, so an exported `VIKE_MAX_ORDER_NOTIONAL` on the developer's box
/// would make every case below exit on the startup-refusal path instead of the rung it is testing.
fn run(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let empty = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
    cmd.args(args)
        .env("VIKE_SETTINGS_DIR", empty.path())
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

/// The exit code alone, for the cases whose message is not the point.
fn code(args: &[&str]) -> i32 {
    run(args, &[]).status.code().expect("the process exited rather than being signalled")
}

#[test]
fn help_is_zero() {
    assert_eq!(code(&["--help"]), 0);
}

/// A missing required flag is a USAGE error, not a run failure — a script RETRIES a run failure and
/// FIXES a usage error, so they must not share a code.
#[test]
fn a_usage_error_is_two() {
    assert_eq!(code(&["backtest"]), 2, "backtest with no --profile is a usage error");
    assert_eq!(code(&["backtest", "--profile"]), 2, "a flag with no value is a usage error");
    assert_eq!(code(&["strategy-status"]), 2, "strategy-status with no --node is a usage error");
    assert_eq!(code(&["sweep"]), 2, "sweep with no --profile is a usage error");
    assert_eq!(code(&["walkforward"]), 2, "walkforward with no --profile is a usage error");
    assert_eq!(code(&["secrets", "--nope"]), 2, "an unknown flag is a usage error");
    // `trade` parses its own command line rather than sharing `exit_for_parse_error` (it is a REPL
    // with a `Config`, not an `Args`), so its rung is asserted separately — it is exactly the sort
    // of surface that drifted before the decision was shared.
    assert_eq!(code(&["trade"]), 2, "trade with no --node is a usage error");
}

/// An unknown VERB is the same class as an unknown flag: the command line was wrong.
#[test]
fn an_unknown_command_is_two() {
    assert_eq!(code(&["definitely-not-a-verb"]), 2);
}

/// …and so is no verb at all. It used to exit 1, which a script could not tell from a run that
/// tried and failed.
#[test]
fn no_subcommand_is_two() {
    assert_eq!(code(&[]), 2);
}

/// A refused connection is distinguishable from a bad command line: a script WAITS on this one.
/// Port 1 on loopback refuses immediately on every platform this ships to.
///
/// ⚠ The profile is a REAL file, written into a temp directory: it is read BEFORE the socket is
/// opened, so a missing one would exit on the read's rung instead and this test would pass for the
/// wrong reason once — and then never notice the connect classification regressing.
#[test]
fn a_connect_failure_is_three() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = dir.path().join("run.toml");
    std::fs::write(
        &profile,
        "[strategy]
name = \"buy_hold\"
",
    )
    .expect("write profile");
    let out = run(
        &[
            "backtest",
            "--profile",
            profile.to_str().expect("utf-8 temp path"),
            "--addr",
            "127.0.0.1:1",
        ],
        &[],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "no datahub is a connect-class failure; stderr: {err}");
}

/// The same rung through a DIFFERENT client crate — `strategy-status` speaks to a vike-tradehub
/// node over `vike_tradehub_client`, not to a datahub over `vike_datahub_client`, so the two
/// classifications are genuinely separate code.
///
/// ⚠ The observe key is supplied on the child, because a node-facing verb refuses BEFORE it opens
/// a socket when no key resolves anywhere — that path is a different failure with a different rung,
/// and testing the connect rung requires getting past it.
#[test]
fn a_node_connect_failure_is_three() {
    let out = run(
        &["strategy-status", "--node", "127.0.0.1:1"],
        &[("VIKE_TRADEHUB_OBSERVE_KEY", "not-a-real-key")],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "a refused node connection is rung 3; stderr: {err}");
}

/// The pre-existing catch-all keeps its number, so every script written against the old
/// two-value behaviour still reads correctly.
#[test]
fn a_run_failure_is_still_one() {
    let out = run(&["backtest", "--profile", "no-such-profile.toml"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an unreadable profile is the ordinary run failure; stderr: {err}"
    );
    assert!(err.contains("no-such-profile.toml"), "and it names the file: {err}");
}

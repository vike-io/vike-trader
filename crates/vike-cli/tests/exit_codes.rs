//! The numeric exit ladder, asserted over the SHIPPED binary.
//!
//! A script branches on these numbers, so they are a public interface: a rung's meaning may be
//! added to, never repurposed. `crates/vike-cli/src/exit.rs` argues each rung; this file is the
//! proof that the binary actually exits on it, which no unit test of a parser can see — every one
//! of these codes is produced by the `main` shim collapsing an `ExitCode`, several layers below
//! the function that decided it.
//!
//! ⚠ **This file asserts SIX of the eight rungs, and the two it omits are omitted because they do
//! not exist yet.** `Exit::Refused` (`4`) and `Exit::Venue` (`5`) are RESERVED — nothing in the
//! crate constructs either, so there is no invocation to write a case for, and a case that could
//! not fail would be worse than the gap. `crates/vike-cli/src/exit.rs` carries what has to exist
//! before they become live (a non-interactive order-write verb; the two write surfaces today are a
//! REPL and a JSON-RPC server, neither of which exits the process on a per-order decision). Wiring
//! either one is the same PR that adds its case here.
//!
//! `Exit::Breach` (`6`) and `Exit::Empty` (`7`) were the fifth and sixth to become live, and they
//! arrived under exactly that rule: the PR that wired `vike-cli backtest gate` is the PR that added
//! their cases below.
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

/// [`run`]'s twin for the cases that need a POPULATED project — the two gate rungs below.
///
/// ⚠ `run` above points `VIKE_SETTINGS_DIR` at an EMPTY directory and sets nothing else, and its
/// emptiness is what every other case here relies on. So this is a second helper rather than an
/// edit: it takes both directories and adds `VIKE_USER_DATA_DIR`, keeping every `env_remove`.
fn run_in(settings: &std::path::Path, user_data: &std::path::Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
    cmd.args(args)
        .env("VIKE_SETTINGS_DIR", settings)
        .env("VIKE_USER_DATA_DIR", user_data)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY");
    cmd.output().unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

/// The smallest runs tree a gate can be pointed at: a baseline, a worse run, and a mark on the
/// first. Nine lines, kept INSIDE this file rather than in a shared `tests/` helper module — two
/// integration binaries sharing one would need a `mod` file both include, which is more machinery
/// than the thing it shares.
mod gate_fixture {
    use std::path::{Path, PathBuf};

    fn plant(user_data: &Path, run_id: &str, sharpe: f64) {
        let dir = user_data.join("runs").join(run_id);
        std::fs::create_dir_all(&dir).expect("run dir");
        std::fs::write(dir.join("report.json"), format!("{{\"sharpe\":{sharpe}}}\n"))
            .expect("report");
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                "{{\"run_id\":\"{run_id}\",\"kind\":\"backtest\",\"produced_by\":\"backtest\",\
                 \"started_at\":\"2025-08-24T01:46:40Z\",\"finished_at\":\"2025-08-24T01:47:00Z\",\
                 \"git_sha\":null,\"config\":{{\"path\":\"m.toml\",\"name\":null}},\
                 \"detail\":null}}\n"
            ),
        )
        .expect("manifest");
    }

    /// `(settings, user_data)`, with `@last` naming a run whose sharpe is well below the mark's.
    pub(super) fn plant_worse_than_baseline(scratch: &Path) -> (PathBuf, PathBuf) {
        let settings = scratch.join("settings");
        let user_data = scratch.join("user_data");
        std::fs::create_dir_all(&settings).expect("settings dir");
        std::fs::create_dir_all(user_data.join("runs")).expect("runs dir");
        plant(&user_data, "1755900000-1-0", 1.82);
        plant(&user_data, "1756000000-1-0", 1.40);
        let out = super::run_in(
            &settings,
            &user_data,
            &["backtest", "tag", "1755900000-1-0", "--as", "baseline/m"],
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "the fixture's own mark must be set, or both cases below would test the wrong \
             failure: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (settings, user_data)
    }
}

/// A DECLARED THRESHOLD WAS BREACHED. Its own rung because the command WORKED — it evaluated every
/// criterion it was given and the answer was no — so a wrapper escalates it where it would RETRY a
/// run failure and FIX a usage error.
///
/// ⚠ The invocation has to be a real one. This file's rule is that a rung nothing produces gets no
/// case, because a case that could not fail would be worse than the gap; so this one plants a runs
/// tree, marks a baseline and gates a worse run against it.
#[test]
fn a_breached_threshold_is_six() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let (settings, user_data) = gate_fixture::plant_worse_than_baseline(scratch.path());
    let out = run_in(
        &settings,
        &user_data,
        &["backtest", "gate", "@last", "--against", "@baseline/m", "--fail-if", "sharpe:-5%"],
    );
    assert_eq!(out.status.code(), Some(6), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

/// NOTHING WAS EVALUATED — and it is NOT a zero. A gate whose criteria named no key the document
/// carries has checked nothing, and from a `0` a CI step cannot tell that from a pass.
#[test]
fn an_unevaluated_gate_is_seven() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let (settings, user_data) = gate_fixture::plant_worse_than_baseline(scratch.path());
    let out = run_in(
        &settings,
        &user_data,
        &["backtest", "gate", "@last", "--against", "@baseline/m", "--fail-if", "nonsuch:-5%"],
    );
    assert_eq!(out.status.code(), Some(7), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn help_is_zero() {
    assert_eq!(code(&["--help"]), 0);
}

/// A missing required flag is a USAGE error, not a run failure — a script RETRIES a run failure and
/// FIXES a usage error, so they must not share a code.
#[test]
fn a_usage_error_is_two() {
    // ⚠ TWO different refusals on ONE rung, which is why both rows are here. A bare `backtest` is
    // now a missing SUB-VERB (decision 11 of the backtest-CLI-surface design: there is no bare
    // form); `backtest run` with nothing is stage 2's "nothing to run" — `--profile` is OPTIONAL
    // since then, so what is refused is a command line carrying NEITHER a file NOR anything to
    // build one from. The codes cannot tell them apart; the messages are pinned in
    // `crates/vike-cli/tests/backtest_cli.rs`.
    assert_eq!(code(&["backtest"]), 2, "backtest with no SUBCOMMAND is a usage error");
    assert_eq!(
        code(&["backtest", "run"]),
        2,
        "…and `run` with no profile and no flags is a usage error on the same rung"
    );
    assert_eq!(code(&["backtest", "run", "--profile"]), 2, "a flag with no value is a usage error");
    assert_eq!(code(&["trade", "status"]), 2, "trade status with no --node is a usage error");
    // The two one-shot WRITE verbs own the same rung, and they own it through their own parser
    // rather than `exit_for_parse_error` (`--node` is checked after the flags are drained, so its
    // absence is not a parse error) — which is exactly why each is asserted here rather than
    // assumed from its sibling.
    assert_eq!(code(&["trade", "halt"]), 2, "trade halt with no --node is a usage error");
    assert_eq!(code(&["trade", "resume"]), 2, "trade resume with no --node is a usage error");
    // ⚠ **A RETIRED verb rides the SAME rung as an unknown one, so these rows cannot tell them
    // apart** — which is why each says what it is asserting rather than naming a flag the verb no
    // longer has. The messages are pinned in `crates/vike-cli/tests/help_cli.rs`'s
    // `the_retired_sweep_verb_fails_naming_its_replacement` and
    // `the_retired_walkforward_verb_fails_naming_its_replacement`; if either test goes, the
    // matching row here silently stops testing anything.
    assert_eq!(code(&["sweep"]), 2, "a RETIRED verb is a usage error, like an unknown one");
    assert_eq!(code(&["walkforward"]), 2, "…and so is the second one (decision 3)");
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
            "run",
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

/// The same rung through a DIFFERENT client crate — `trade status` speaks to a vike-tradehub
/// node over `vike_tradehub_client`, not to a datahub over `vike_datahub_client`, so the two
/// classifications are genuinely separate code.
///
/// ⚠ The observe key is supplied on the child, because a node-facing verb refuses BEFORE it opens
/// a socket when no key resolves anywhere — that path is a different failure with a different rung,
/// and testing the connect rung requires getting past it.
#[test]
fn a_node_connect_failure_is_three() {
    let out = run(
        &["trade", "status", "--node", "127.0.0.1:1"],
        &[("VIKE_TRADEHUB_OBSERVE_KEY", "not-a-real-key")],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "a refused node connection is rung 3; stderr: {err}");
}

/// ⚠ An UNPARSEABLE `--profile` is rung **2**, and a MISSING one is rung **1**. Those are different
/// questions and stage 2 of the backtest-CLI-surface design MOVED the first of them.
///
/// Before the inversion the profile text crossed the wire unparsed, so malformed TOML was a
/// far-side failure — rung 1, after a dial. Now `build_profile_toml` parses it HERE, to apply
/// `--set` onto it, and `execute` classifies that with `CliError::usage`. A script should FIX a
/// profile it typed wrong and RETRY a profile it could not read, so the split is the right one.
///
/// It is pinned because nothing else notices the move: `a_run_failure_is_still_one` below uses a
/// file that does not EXIST, and that read still fails on rung 1 through a plain `?`. The two tests
/// are a pair and the difference between them is the whole point — change one and read the other.
#[test]
fn an_unparseable_profile_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = dir.path().join("malformed.toml");
    std::fs::write(&profile, "this is [not valid").expect("write profile");
    let out =
        run(&["backtest", "run", "--profile", profile.to_str().expect("utf-8 temp path")], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a profile that is not TOML is a command line the user must FIX; stderr: {err}"
    );
    assert!(err.contains("profile"), "and it says which input was wrong: {err}");
}

/// The pre-existing catch-all keeps its number, so every script written against the old
/// two-value behaviour still reads correctly.
///
/// ⚠ The file is MISSING, not malformed — see `an_unparseable_profile_is_a_usage_error` directly
/// above for why that distinction carries a different rung since stage 2.
#[test]
fn a_run_failure_is_still_one() {
    let out = run(&["backtest", "run", "--profile", "no-such-profile.toml"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an unreadable profile is the ordinary run failure; stderr: {err}"
    );
    assert!(err.contains("no-such-profile.toml"), "and it names the file: {err}");
}

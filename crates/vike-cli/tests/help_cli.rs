//! `--help` is a SUCCESS whose text is STDOUT — the shipped-binary gate for `vike-cli`.
//!
//! Every one of these ran the REAL binary (`CARGO_BIN_EXE_vike-cli`) rather than the parser,
//! because the defect this pins is not in the parsing: `--help` short-circuits the parser with an
//! `Err`, and each command's `run` then decided for itself what that `Err` meant. Four of them
//! decided "usage error": exit **1**, with the internal short-circuit token `"help requested"`
//! printed to **stderr** as though it were a diagnostic. That breaks `set -e`, every packaging
//! smoke test and every wrapper that checks a status — and it is invisible to a unit test of
//! `parse_args`, which sees the same `Err` either way. The three properties asserted here are
//! therefore exactly the three a user sees: the STATUS, the STREAM, and that no internal token
//! leaks.
//!
//! `vike-cli mcp` was deliberately absent from the first round — same defect, same one-line fix,
//! but `crates/vike-cli/src/cmd/mcp.rs` was owned by a concurrent branch at the time. That branch
//! merged, so its row is here now and asserts the real behaviour instead of the deferral.
//!
//! Hermetic: `VIKE_SETTINGS_DIR` points every child at an empty directory, so the dispatcher's
//! `resolve_policy` (which runs before EVERY subcommand) reads no real `policy.toml`, and the two
//! removed risk variables are cleared in case the harness inherited them.

use std::process::{Command, Output};

/// The internal short-circuit token `crate::cmd::args::help_requested` carries. It is control flow,
/// never a diagnostic, so it must never reach a user's terminal.
const SENTINEL: &str = "help requested";

/// Run the shipped binary with `args`, in an environment that resolves no project settings.
fn run(args: &[&str]) -> Output {
    // A path that need not exist: `vike_config::load` treats an absent file as "nothing
    // configured", which is exactly the neutral state these assertions want.
    let empty_settings = std::env::temp_dir().join(format!(
        "vike_cli_help_no_settings_{}_{}",
        std::process::id(),
        args.join("_").replace(['-', ' '], "")
    ));
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(args)
        .env("VIKE_SETTINGS_DIR", &empty_settings)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

/// The three properties a `--help` invocation must have, asserted together because a fix that
/// delivers only one of them is not a fix: exit 0, help on stdout, no internal token anywhere.
fn assert_help_is_clean(args: &[&str]) {
    let out = run(args);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "`vike-cli {}` must exit 0 — a non-zero --help breaks `set -e`, packaging smoke tests and \
         any wrapper that checks a status. status: {:?}, stderr: {stderr}",
        args.join(" "),
        out.status.code()
    );
    assert!(
        stdout.contains("usage:"),
        "`vike-cli {}` must print its usage to STDOUT (help is normal output, not a diagnostic); \
         stdout: {stdout:?}, stderr: {stderr:?}",
        args.join(" ")
    );
    assert!(
        !stdout.contains(SENTINEL) && !stderr.contains(SENTINEL),
        "`vike-cli {}` leaked the internal {SENTINEL:?} short-circuit token to a user; \
         stdout: {stdout:?}, stderr: {stderr:?}",
        args.join(" ")
    );
}

/// The four non-interactive remote commands plus the two settings commands, in both spellings.
/// One test per surface so a failure names the offending command instead of the first one.
#[test]
fn backtest_help_exits_zero_on_stdout() {
    assert_help_is_clean(&["backtest", "--help"]);
    assert_help_is_clean(&["backtest", "-h"]);
}

#[test]
fn sweep_help_exits_zero_on_stdout() {
    assert_help_is_clean(&["sweep", "--help"]);
    assert_help_is_clean(&["sweep", "-h"]);
}

#[test]
fn walkforward_help_exits_zero_on_stdout() {
    assert_help_is_clean(&["walkforward", "--help"]);
    assert_help_is_clean(&["walkforward", "-h"]);
}

/// `strategy-status` rides the same shared short-circuit as its siblings, and its `--help` must
/// work with NO node and NO key — help is how an operator discovers what the command needs.
#[test]
fn strategy_status_help_exits_zero_on_stdout() {
    assert_help_is_clean(&["strategy-status", "--help"]);
    assert_help_is_clean(&["strategy-status", "-h"]);
}

/// `config` has THREE help paths now and two of them once disagreed: the bare verb
/// (`config --help`) was already correct, while `config show --help` went through the shared parser
/// short-circuit and hit the same exit-1-with-sentinel arm as `backtest`. All three are asserted so
/// the set cannot drift again — and `check`'s matters most of the three, because it is the verb a
/// shipped `deploy/*.service` unit runs, where a `--help` probe that exits non-zero reads as a
/// broken binary.
#[test]
fn config_help_exits_zero_on_stdout_at_every_level() {
    assert_help_is_clean(&["config", "--help"]);
    assert_help_is_clean(&["config", "show", "--help"]);
    assert_help_is_clean(&["config", "show", "-h"]);
    assert_help_is_clean(&["config", "check", "--help"]);
    assert_help_is_clean(&["config", "check", "-h"]);
}

/// The verb list is the only discovery surface `config` has, so `config --help` must NAME every
/// verb. A verb that exists and is not in the list is one an operator can only find by reading
/// source — which is how `check` would have shipped invisible.
#[test]
fn the_config_help_lists_every_verb() {
    let out = run(&["config", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    for verb in ["show", "check"] {
        assert!(stdout.contains(verb), "`config --help` must list `{verb}`: {stdout}");
    }
}

/// …and the top-level list must too, for the same reason one level up.
#[test]
fn the_top_level_help_lists_every_command() {
    let out = run(&["--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    for command in
        ["backtest", "config", "secrets", "trade", "strategy-status", "init", "indicators"]
    {
        assert!(stdout.contains(command), "`vike-cli --help` must list `{command}`: {stdout}");
    }
}

/// `secrets` already exited 0 — the defect here is the STREAM: it printed its usage with
/// `eprintln!`, so `vike-cli secrets --help | less` showed an empty page.
#[test]
fn secrets_help_exits_zero_on_stdout_at_both_levels() {
    assert_help_is_clean(&["secrets", "--help"]);
    assert_help_is_clean(&["secrets", "list", "-h"]);
}

/// `mcp` is the stdio MCP server, so it is the one surface where a broken `--help` is worst: an
/// agent host that probes a server with `--help` before wiring it up reads exit 1 + a bare
/// `help requested` on stderr as "this server is broken". It inlined the error arm rather than
/// sharing `args::exit_for_parse_error`, which is exactly how the spellings drifted the first time.
#[test]
fn mcp_help_exits_zero_on_stdout() {
    assert_help_is_clean(&["mcp", "--help"]);
    assert_help_is_clean(&["mcp", "-h"]);
}

/// `indicators` prints the callable indicator roster, so a broken `--help` on it is the same class
/// of defect as on `mcp`: the command exists to be DISCOVERED — the shipped
/// `user_data/strategies/rhai/README.md` points a user straight at it — and a `--help` that exits 1
/// with a bare token reads as "this command is broken" to the person following that pointer.
#[test]
fn indicators_help_exits_zero_on_stdout() {
    assert_help_is_clean(&["indicators", "--help"]);
    assert_help_is_clean(&["indicators", "-h"]);
}

/// The top-level dispatcher and the interactive `trade` command were the two that were already
/// right. They are pinned so the fix cannot regress the reference behaviour it was modelled on.
#[test]
fn the_already_correct_surfaces_stay_correct() {
    assert_help_is_clean(&["--help"]);
    assert_help_is_clean(&["-h"]);
    assert_help_is_clean(&["help"]);
    assert_help_is_clean(&["trade", "--help"]);
}

/// `--version`/`-V`: the version on stdout, exit 0. It was unrecognised, so it fell into the
/// unknown-command arm — an exit 1 with `unknown command '--version'` on stderr.
#[test]
fn version_prints_the_crate_version_on_stdout() {
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`vike-cli {flag}` must exit 0; status: {:?}, stderr: {stderr}",
            out.status.code()
        );
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "`vike-cli {flag}` must print the crate version {:?} on stdout; stdout: {stdout:?}",
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            !stderr.contains("unknown command"),
            "`vike-cli {flag}` must be recognised, not routed to the unknown-command arm; \
             stderr: {stderr:?}"
        );
    }
}

/// The negative half, so "exit 0 on --help" is never bought by making everything exit 0: a real
/// usage error still fails, and still explains itself on stderr.
#[test]
fn a_real_usage_error_still_fails_on_stderr() {
    let out = run(&["backtest", "--not-a-flag"]);
    assert!(!out.status.success(), "an unknown flag must still exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage:"), "a usage error keeps printing usage to stderr: {stderr:?}");
    assert!(!stderr.contains(SENTINEL), "and it must not carry the help token either: {stderr:?}");

    let out = run(&["no-such-command"]);
    assert!(!out.status.success(), "an unknown command must still exit non-zero");
}

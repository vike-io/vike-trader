//! `vike-cli data` — the store-filling verb, driven as the SHIPPED binary.
//!
//! The grammar is unit-tested beside the module; what needs a real process is the part no parser
//! test can see: which exit code reaches a caller, and that the verb genuinely SPAWNS an engine
//! rather than pretending to.
//!
//! ⚠ **Every case names the engine with `--engine`, and that is not laziness.** The search's third
//! rung looks beside THIS executable, and a lane that has also built
//! `-p vike-backtest --features datafusion-store` into the same `target/` really does leave a
//! `backtest` binary sitting there. A test that relied on the search finding nothing would pass on
//! a developer box and fail in the full CI matrix, for a reason having nothing to do with this
//! code — so the tests that care about the MISS name a path that is not there, and the tests that
//! care about the SPAWN name a stand-in they wrote themselves.
//!
//! The child's environment is pinned on the child (`Command::env` / `env_remove`, never
//! `std::env::set_var`): without the settings redirect a run on a developer box resolves the REPO's
//! settings directory, and an exported removed variable would make every case exit on the startup
//! refusal instead of the rung it is testing.

use std::path::Path;
use std::process::{Command, Output};

fn run(settings_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(args)
        .env("VIKE_SETTINGS_DIR", settings_dir)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The verb's help is the only place its subcommands are named — it takes no default action — so a
/// help that did not list both would leave "get some market data" undiscoverable.
#[test]
fn help_names_both_subcommands_and_exits_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for sub in ["fetch", "seed-demo"] {
        assert!(text.contains(sub), "`data --help` must list `{sub}`: {text}");
    }
}

/// A command line the user can fix exits on the USAGE rung, and every one of these is caught HERE —
/// before a process is spawned, so the diagnostic comes from the binary they typed.
#[test]
fn a_bad_command_line_is_the_usage_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for (args, needle) in [
        (vec!["data"], "subcommand"),
        (vec!["data", "frobnicate"], "unknown `data` subcommand"),
        (vec!["data", "fetch"], "VENUE:SYMBOL:INTERVAL"),
        (vec!["data", "fetch", "binance:BTCUSDT"], "VENUE:SYMBOL:INTERVAL"),
        (vec!["data", "fetch", "binance:BTCUSDT:1h"], "--days"),
        (vec!["data", "fetch", "binance:BTCUSDT:1h", "--days", "x"], "--days"),
        (vec!["data", "fetch", "binance:BTCUSDT:1h", "--from", "0"], "--to"),
        (vec!["data", "seed-demo", "--days", "30"], "--days"),
        (vec!["data", "seed-demo", "--nope"], "unknown option"),
    ] {
        let out = run(scratch.path(), &args);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?} must be a usage error: {err}");
        assert!(err.contains(needle), "{args:?} must say {needle:?}: {err}");
    }
}

/// A missing engine is a CONNECT-class failure naming what is missing and how to point at one —
/// the same disposition an unreachable datahub gets, because it is the same kind of problem: the
/// command line was right and the thing it needs is not there.
#[test]
fn a_missing_engine_is_the_connect_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let absent = scratch.path().join("no-such-engine");
    let out = run(
        scratch.path(),
        &[
            "data",
            "fetch",
            "binance:BTCUSDT:1m",
            "--days",
            "1",
            "--engine",
            absent.to_str().expect("utf-8 temp path"),
        ],
    );
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(3), "a missing engine is connect-class: {err}");
    assert!(err.contains("backtest"), "the message must name the engine: {err}");
    assert!(err.contains("--engine"), "…and how to point at one: {err}");
}

/// THE plumbing: the verb really does spawn the engine, with the flags its own arguments translate
/// into, and folds the child's exit status rather than inventing one.
///
/// ⚠ A SCRIPT stands in for the engine, which is what keeps this test out of a DataFusion build —
/// and it is unix-only for exactly that reason: a `#!` line is what makes a text file executable,
/// and Windows has no equivalent `Command::new` will run.
#[cfg(unix)]
#[test]
fn fetch_and_seed_demo_reach_the_engine_as_its_own_flags() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir().expect("tempdir");
    // Echoes its argv and exits 0.
    let engine = scratch.path().join("fake-engine");
    std::fs::write(&engine, "#!/bin/sh\necho \"argv: $*\"\n").expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let engine = engine.to_str().expect("utf-8 temp path");

    let out = run(
        scratch.path(),
        &[
            "data",
            "fetch",
            "binance:BTCUSDT:1h",
            "--days",
            "180",
            "--store",
            "/s",
            "--engine",
            engine,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "argv: --fetch binance:BTCUSDT:1h --days 180 --store /s",
        "the verb's product is the engine's argv"
    );

    let out = run(scratch.path(), &["data", "seed-demo", "--engine", engine]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "argv: --seed-demo");
}

/// ⚠ The engine's `2` is NOT re-published as this binary's `2`, and that is the point of this case
/// rather than an accident of it. The engine returns `2` for a bad command line AND for a failed
/// venue fetch, an unopenable store and a failed demo seed — so folding it onto the usage rung
/// would tell a wrapper that a geoblocked `data fetch` "cannot succeed if re-run unchanged", which
/// is false and is the exact retry-vs-fix inversion the ladder exists to remove.
/// `crates/vike-cli/src/cmd/engine.rs`'s `fold_status` carries the argument. What a caller DOES
/// get is the child's own diagnostic, uncaptured, plus the code itself in this binary's line.
#[cfg(unix)]
#[test]
fn an_overloaded_engine_code_lands_on_the_unclassified_rung() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = tempfile::tempdir().expect("tempdir");
    let engine = scratch.path().join("refusing-engine");
    // 2 is the engine's own usage/pre-flight/runtime code — the build without `venue-fetch`
    // answers with it, and so does a fetch the venue refused. One code, two dispositions, which is
    // why this side may not read a cause into it.
    std::fs::write(&engine, "#!/bin/sh\necho 'no network fetch in this build' >&2\nexit 2\n")
        .expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let out = run(
        scratch.path(),
        &[
            "data",
            "fetch",
            "binance:BTCUSDT:1h",
            "--days",
            "1",
            "--engine",
            engine.to_str().expect("utf-8 temp path"),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "an overloaded engine code is the UNCLASSIFIED rung, never the usage one: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("no network fetch"),
        "the child's own diagnostic reaches the user's stderr, uncaptured: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("exited 2"),
        "…and this side names the code it saw rather than asserting a cause for it: {}",
        stderr(&out)
    );
}

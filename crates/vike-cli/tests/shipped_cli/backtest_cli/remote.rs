//! The REMOTE verb against a loopback backtesthub, and the offline refusals and `params`.

use std::process::Command;

use super::{MINIMAL_BAR_PROFILE, PROFILE_NAME, spawn_server, write_temp_profile};

/// A valid profile under `--whole`: `vike-cli backtest` exits 0 and prints the JSON report carrying the
/// profile name — the answer every single backtest gave before decision 0078's amendment of 2026-11-18.
#[test]
fn valid_profile_prints_report_json_with_name() {
    let addr = spawn_server(vike_strategy_lab::stand_in);
    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "valid");

    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .arg("backtest")
        .arg("run")
        .arg("--profile")
        .arg(&profile)
        .arg("--addr")
        .arg(addr.to_string())
        .arg("--whole")
        .output()
        .expect("run vike-cli backtest");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "vike-cli backtest must exit 0; stderr: {stderr}");

    // stdout is the (pretty-printed by default) report JSON — parse it and assert the name plumbed
    // through the profile -> engine -> report -> wire -> print path.
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON");
    assert!(value.is_object(), "report JSON must be an object");
    assert_eq!(
        value.get("name").and_then(|n| n.as_str()),
        Some(PROFILE_NAME),
        "report must carry the profile's name; stdout: {stdout}"
    );

    let _ = std::fs::remove_file(&profile);
}

/// `--json --whole` prints the report verbatim; it still parses as JSON with the same `name`.
#[test]
fn json_flag_prints_valid_report() {
    let addr = spawn_server(vike_strategy_lab::stand_in);
    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "json");

    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .arg("backtest")
        .arg("run")
        .arg("--profile")
        .arg(&profile)
        .arg("--addr")
        .arg(addr.to_string())
        .arg("--json")
        .arg("--whole")
        .output()
        .expect("run vike-cli backtest run --json");

    assert!(out.status.success(), "vike-cli backtest run --json must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json stdout must be valid JSON");
    assert_eq!(value.get("name").and_then(|n| n.as_str()), Some(PROFILE_NAME));

    let _ = std::fs::remove_file(&profile);
}

/// **A single backtest answers its SUMMARY by default** (decision 0078's amendment of 2026-11-18): under
/// `--json` the summary document — the run the backtesthub holds its trades and whole curve under, every
/// metric row rendered, the curve's length — and no report; without it the rows and one line saying where
/// the rest of the run is. The `--whole` cases above are the report it replaces, over the same profile.
#[test]
fn a_single_backtest_answers_its_summary_by_default() {
    let addr = spawn_server(vike_strategy_lab::stand_in);
    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "summary");
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_vike-cli"))
            .arg("backtest")
            .arg("run")
            .arg("--profile")
            .arg(&profile)
            .arg("--addr")
            .arg(addr.to_string())
            .args(extra)
            .output()
            .expect("run vike-cli backtest run")
    };

    let out = run(&["--json"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the summary run must exit 0; stderr: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json stdout must be one JSON document");
    assert!(
        value["run"].as_str().is_some_and(|run| !run.is_empty()),
        "names its held run: {stdout}"
    );
    assert!(
        value["rows"].as_array().is_some_and(|rows| !rows.is_empty()),
        "every metric: {stdout}"
    );
    assert!(value["curve_len"].is_u64() && value["n_trades"].is_u64(), "{stdout}");
    assert!(value.get("name").is_none(), "a summary, not the report: {stdout}");

    let out = run(&[]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("final_equity"), "the rendered rows: {stdout}");
    assert!(stdout.contains("stay on the backtesthub") && stdout.contains("--whole"), "{stdout}");
}

/// **The owner's perp command, flags only**: no `--profile`, no `--cash`, `--set
/// engine.attach_funding=false`. Refused on the CI box 2026-11-21 (`missing field cash` at `[engine]`);
/// it runs, and the report books no funding. The stub store holds no bars, so `funding_paid` 0 is the
/// wire's word for the switch, the profile half is `a_flags_only_profile_loads` and its siblings.
#[test]
fn a_flags_only_perp_run_with_funding_off_runs_and_pays_no_funding() {
    let addr = spawn_server(vike_strategy_lab::stand_in);
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["backtest", "run", "--venue", "binance", "--symbol", "BTCUSDT.P"])
        .args(["--interval", "1h", "--from", "2026-08-20", "--to", "2026-11-20"])
        .args(["--strategy", "trend/buy_hold.rs", "--set", "engine.attach_funding=false"])
        .args(["--addr", &addr.to_string(), "--json", "--whole"])
        .output()
        .expect("run vike-cli backtest run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "must run; stderr: {stderr}");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json --whole prints the report");
    assert_eq!(value["funding_paid"].as_f64(), Some(0.0), "{stdout}");
}

/// A malformed profile (`from > to`, caught by the server's validation): exit non-zero, the error on
/// stderr, nothing on stdout.
#[test]
fn invalid_profile_exits_1_with_stderr() {
    let addr = spawn_server(vike_strategy_lab::stand_in);
    let invalid = MINIMAL_BAR_PROFILE.replace("from = \"0\"", "from = \"999999\"");
    let profile = write_temp_profile(&invalid, "invalid");

    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .arg("backtest")
        .arg("run")
        .arg("--profile")
        .arg(&profile)
        .arg("--addr")
        .arg(addr.to_string())
        .output()
        .expect("run vike-cli backtest");

    assert!(!out.status.success(), "an invalid profile must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.trim().is_empty(), "a failure must carry a message on stderr");
    assert!(out.stdout.is_empty(), "no report on stdout for a failed run");

    let _ = std::fs::remove_file(&profile);
}

/// A bare `backtest run` has nothing to run and says so, naming BOTH routes in.
///
/// ⚠ It is no longer "missing required --profile": stage 2 made that flag optional, and a command
/// line carrying neither a file nor a flag to build one from is what is refused. The usage still
/// goes to STDERR on the usage rung (help alone goes to stdout — `crates/vike-cli/src/cmd/args.rs`'s
/// `args::finish`).
#[test]
fn a_bare_backtest_run_is_a_usage_error_naming_both_routes() {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .arg("backtest")
        .arg("run")
        .output()
        .expect("run vike-cli backtest run");
    assert_eq!(out.status.code(), Some(2), "a bare backtest run is a usage error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nothing to run"), "says what is missing; stderr: {stderr}");
    assert!(stderr.contains("--profile"), "names the file route; stderr: {stderr}");
    assert!(stderr.contains("--set"), "…and the flag route; stderr: {stderr}");
}

/// ⚠ **There is no bare form** (decision 11 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`): a missing sub-verb is a
/// USAGE error with the usage on stderr, and no network is needed to reach it.
///
/// It is a SEPARATE case from the one directly above and the pair is the point — both exit 2, and
/// only the message tells them apart. This one must name the ROSTER (what to type), that one names
/// the two ways to describe a run. A single test could not have caught the sub-verb refusal
/// regressing into the "nothing to run" one, because the rung is identical.
#[test]
fn a_backtest_with_no_subcommand_is_a_usage_error_naming_the_roster() {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .arg("backtest")
        .output()
        .expect("run vike-cli backtest");
    assert_eq!(out.status.code(), Some(2), "a missing sub-verb is a usage error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("vike-cli backtest"), "names the plane; stderr: {stderr}");
    assert!(stderr.contains("subcommand"), "says what was expected; stderr: {stderr}");
    assert!(stderr.contains("run"), "…and names the roster; stderr: {stderr}");
    assert!(stderr.contains("params"), "…every row of it; stderr: {stderr}");
}

/// ⚠ …and `--list-params` answers with the SUB-VERB that replaced it, on both spellings that used
/// to work: the bare `backtest --list-params` (the router's arm) and `backtest run --list-params`
/// (the run parser's). Answering either as "unknown argument" would tell an operator a flag that
/// shipped for months had never existed.
#[test]
fn the_retired_list_params_flag_names_the_subcommand_that_replaced_it() {
    for argv in [
        vec!["backtest", "--list-params", "--script", "s.rhai"],
        vec!["backtest", "run", "--list-params", "--script", "s.rhai"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
            .args(&argv)
            .output()
            .expect("run vike-cli backtest");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{argv:?}: {stderr}");
        assert!(stderr.contains("--list-params"), "{argv:?} names it: {stderr}");
        assert!(stderr.contains("backtest params"), "{argv:?} names the replacement: {stderr}");
        assert!(
            !stderr.contains("unknown argument"),
            "{argv:?} must not read as a flag that never existed: {stderr}"
        );
    }
}

/// `backtest params` is the re-homed discovery mode, and it is OFFLINE — it reads the script on
/// this machine, so no server, store or engine is consulted and none needs to exist.
#[test]
fn params_lists_a_scripts_knobs_with_no_server_anywhere() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("knobs.rhai");
    // `discover_params` runs the TOP LEVEL only — where `param(name, default)` is called — so no
    // `on_bar` hook is needed to list a script's knobs.
    std::fs::write(&script, "let fast = param(\"fast\", 10.0);\n").expect("write script");

    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["backtest", "params", "--script"])
        .arg(&script)
        .output()
        .expect("run vike-cli backtest params");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "params must exit 0 with no server; stderr: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("fast"), "names the declared knob: {stdout}");

    // …and a run-only flag is refused BY NAME rather than dropped or called unknown.
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["backtest", "params", "--script"])
        .arg(&script)
        .args(["--addr", "1.2.3.4:9"])
        .output()
        .expect("run vike-cli backtest params");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("--addr"), "names the flag: {stderr}");
    assert!(stderr.contains("params"), "…and the subcommand that refused it: {stderr}");
}

/// No subcommand prints the command list and exits non-zero (git-style: nothing to do).
#[test]
fn no_subcommand_lists_commands() {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli")).output().expect("run vike-cli");
    assert!(!out.status.success(), "no subcommand must exit non-zero");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("backtest"), "help must list the backtest command; stdout: {stdout}");
}

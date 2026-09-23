//! End-to-end tests for the `vike-cli backtest` command (headless two-layer plan, PR-1).
//!
//! Hermetic and loopback-only: spawn the REAL [`vike_backtest::compute_server::serve`] over an in-memory
//! `MemHistStore` on an ephemeral `127.0.0.1:0` port (the exact spawn pattern of
//! `crates/vike-datahub/tests/roundtrip.rs`), then run the SHIPPED bin via `CARGO_BIN_EXE_vike-cli`
//! as `vike-cli backtest …` against that address. No prod store, no external network.
//!
//! These assert the PLUMBING — a valid profile prints a well-formed JSON report carrying the
//! profile's `name`, and a malformed profile exits non-zero with a message on stderr. `MemHistStore`
//! is an inert stub whose `load_bars` returns empty, so the run closes zero trades; we assert the
//! request -> engine -> report -> response -> print path, NOT non-empty results.

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
// ⚠ The COMPUTE server, not `vike_datahub::serve`. Ruling 7 of
// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` moved the `Run*` verbs to
// `vike-backend backtest --addr`, so pointing this harness at the data daemon would now prove a
// wrong-plane refusal while claiming to prove the command works.
use vike_backtest::compute_server::serve;

/// The planted-engine plant and the `ETXTBSY` retry that survives spawning one. See that module's
/// doc for the race and for why matching the errno — and only the errno — is what keeps the retry
/// from hiding a genuinely missing engine, which this file has a case about
/// ([`local_without_the_engine_binary_says_so`]).
mod common;

/// A minimal, valid bar-mode profile carrying a distinctive `name`. Over an empty `MemHistStore` it
/// loads no bars (zero trades), which is enough to exercise the whole path; the `name` is what we
/// assert survives into the report JSON. Mirrors the TOML shape in the vike-datahub roundtrip test.
const PROFILE_NAME: &str = "vike_cli_backtest_smoke";
const MINIMAL_BAR_PROFILE: &str = r#"
name = "vike_cli_backtest_smoke"

[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

/// Bind an ephemeral loopback listener, spawn `serve` over a fresh in-memory store on a detached
/// thread, and return the assigned address for the CLI to connect to.
fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// Write `contents` to a uniquely-named temp file and return its path. Unique per (pid, nanos) so
/// parallel test threads never collide; the OS reclaims the temp dir, so no explicit cleanup.
fn write_temp_profile(contents: &str, tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut path = std::env::temp_dir();
    path.push(format!("vike_cli_bt_{tag}_{}_{nanos}.toml", std::process::id()));
    let mut f = std::fs::File::create(&path).expect("create temp profile");
    f.write_all(contents.as_bytes()).expect("write temp profile");
    path
}

/// A valid profile: `vike-cli backtest` exits 0 and prints a JSON report carrying the profile name.
#[test]
fn valid_profile_prints_report_json_with_name() {
    let addr = spawn_server();
    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "valid");

    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .arg("backtest")
        .arg("run")
        .arg("--profile")
        .arg(&profile)
        .arg("--addr")
        .arg(addr.to_string())
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

/// `--json` prints the report verbatim; it still parses as JSON with the same `name`.
#[test]
fn json_flag_prints_valid_report() {
    let addr = spawn_server();
    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "json");

    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .arg("backtest")
        .arg("run")
        .arg("--profile")
        .arg(&profile)
        .arg("--addr")
        .arg(addr.to_string())
        .arg("--json")
        .output()
        .expect("run vike-cli backtest run --json");

    assert!(out.status.success(), "vike-cli backtest run --json must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json stdout must be valid JSON");
    assert_eq!(value.get("name").and_then(|n| n.as_str()), Some(PROFILE_NAME));

    let _ = std::fs::remove_file(&profile);
}

/// A malformed profile (`from > to`, caught by the server's validation): exit non-zero, the error on
/// stderr, nothing on stdout.
#[test]
fn invalid_profile_exits_1_with_stderr() {
    let addr = spawn_server();
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
/// `exit_for_parse_error`).
#[test]
fn a_bare_backtest_run_is_a_usage_error_naming_both_routes() {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .arg("backtest")
        .arg("run")
        .output()
        .expect("run vike-cli backtest run");
    assert_eq!(out.status.code(), Some(2), "a bare backtest run is a usage error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage:"), "must print usage; stderr: {stderr}");
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
    assert!(stderr.contains("usage:"), "must print usage; stderr: {stderr}");
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

// ── --local: driving the standalone engine ──────────────────────────────────────────────────────

/// Run `vike-cli` with an environment that resolves no project settings, so `resolve_policy` (which
/// runs before every subcommand) cannot pick up this machine's real `policy.toml` — and so that
/// `<project>/bin` resolves inside the case's own scratch rather than on the developer's box.
/// A project directory this test OWNS, plus the `settings/` child to hand [`run_cli`].
///
/// ⚠ **THE INVARIANT THIS EXISTS TO MAKE STRUCTURAL, because getting it wrong is invisible on the
/// box that writes the test.** [`run_cli`] sets `VIKE_SETTINGS_DIR`, and
/// `crates/vike-cli/src/lib.rs`'s `resolve_policy` computes `<project>` as that directory's
/// **PARENT**. So handing it a BARE `TempDir` makes `<project>` the SHARED SYSTEM TEMP ROOT, and
/// `<project>/tmp` — where a `--local` run stages a rewritten profile
/// (`crates/vike-cli/src/cmd/engine.rs`'s `scratch_root`) — becomes `/tmp/tmp`: a path owned by
/// whichever user created it first, on a `/tmp` that is 1777 sticky.
///
/// That is not hypothetical. `local_without_the_engine_binary_says_so` passed on a the CI box
/// verification lane as one user and FAILED in CI as another on the SAME box, with
/// `cannot create a scratch directory under /tmp/tmp: Permission denied (os error 13)` — the lane
/// had created `/tmp/tmp` at 0775 earlier the same day. The direction is SYMMETRIC: whoever runs
/// first locks the other out, so the test that "passes locally" proves nothing about the other user.
///
/// Returning the `settings` CHILD forces `<project>` to be the TempDir itself, so the scratch root
/// is `<temp>/tmp` — owned by this test, on every box and every user. It also keeps
/// `scratch_root`'s SWEEP (`vike_model::scratch::sweep`, capped at `DEFAULT_MAX_SCRATCH_ENTRIES`)
/// pointed at a directory this test owns instead of one shared with every other process on the box.
///
/// ⚠ A test that does NOT reach `execute` (a usage-rung refusal) is unaffected either way, because
/// nothing calls `scratch_root` on that path — but it uses this helper too, so the file has ONE
/// pattern and a later assertion that does reach staging cannot quietly inherit the hazard.
fn project_with_settings() -> (tempfile::TempDir, PathBuf) {
    let project = tempfile::tempdir().expect("tempdir");
    let settings = project.path().join("settings");
    std::fs::create_dir_all(&settings).expect("create settings");
    (project, settings)
}

/// The runner every case that PLANTS a stand-in engine under `<project>/bin/` goes through, so all
/// three inherit the `ETXTBSY` retry without having to know it exists.
///
/// ⚠ It is NOT the only spawn in this file, and that is why the rule is a check rather than a
/// convention: more than a dozen cases below call `Command::new(env!("CARGO_BIN_EXE_vike-cli"))`
/// directly, every one of them legitimately — they want the AMBIENT environment, which several are
/// asserting about, where this runner deliberately redirects the settings directory. None of them
/// plants an engine, and `planted_binary_retry.rs`'s
/// `a_case_that_plants_an_engine_may_not_spawn_the_cli_itself` is what keeps a new one from
/// copying the majority idiom into a case that does.
///
/// ⚠ The three planted cases are exec'd by the CHILD, which is why a lost coin toss has never
/// looked like a spawn error here: measured 2026-09-14,
/// `the_project_bin_engine_is_the_one_that_runs` failed on "the child's stdout is INHERITED, not
/// captured:" with an EMPTY value, because the `vike-cli` it drives could not exec the file this
/// test had just written. `common`'s module doc carries the mechanism; the predicate is the errno
/// alone, and a spawn failure that is not that errno still panics on the first attempt exactly as
/// this function did before.
fn run_cli(settings_dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    common::output_retrying_etxtbsy(&format!("run vike-cli {args:?}"), || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
        c.args(args)
            .env("VIKE_SETTINGS_DIR", settings_dir)
            .env_remove("VIKE_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL");
        c
    })
}

/// `--local` with an engine that is not there is a CONNECT-class failure naming what is missing and
/// how to get it — not a panic, and not a usage error the user cannot act on.
///
/// ⚠ The engine is NAMED with `--engine` rather than left to the search. That is the only hermetic
/// way to test the miss: the search's third rung looks beside THIS executable, and a lane that has
/// also built `-p vike-backtest --features datafusion-store` into the same `target/` really does
/// have a `backtest` binary sitting there — so a test that relied on the search finding nothing
/// would pass on a developer box and fail in the full CI matrix, for a reason having nothing to do
/// with this code.
#[test]
fn local_without_the_engine_binary_says_so() {
    // ⚠ The `settings/` CHILD, never the bare TempDir — see `project_with_settings`. Since stage 2
    // this run STAGES before it looks for an engine (the built text can never equal the authored
    // file's, because the builder re-serializes through `toml::to_string`), so `<project>/tmp` is
    // on this test's path whether or not the engine is found. Passing the bare TempDir put that at
    // `/tmp/tmp` and made the verdict depend on which user had created it first.
    let (project, settings) = project_with_settings();
    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "local_missing");
    let absent = project.path().join("no-such-engine");

    let out = run_cli(
        &settings,
        &[
            "backtest",
            "run",
            "--local",
            "--engine",
            absent.to_str().expect("utf-8 temp path"),
            "--profile",
            profile.to_str().expect("utf-8 temp path"),
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "a missing engine is a connect-class failure: {err}");
    assert!(err.contains("backtest"), "the message must name the missing engine: {err}");
    assert!(err.contains("--engine"), "…and how to point at one: {err}");

    // ⚠ THE POSITIVE PROOF, and it is the point of this pair of lines. A green run cannot show
    // WHERE the profile was staged, and the user who owns `/tmp/tmp` gets a green either way — so
    // "it passed on my box" is not evidence here. `ScratchDir::create_in` does `create_dir_all` on
    // `<root>/<tag>-<pid>-<n>`, which leaves `<root>` behind after the staged directory is dropped.
    // So `<temp>/tmp` existing IS the assertion that the scratch root resolved inside this test's
    // own TempDir, and it holds for every user on every box.
    assert!(
        project.path().join("tmp").is_dir(),
        "the profile must have been staged under THIS test's project, not the shared system temp \
         root — see `project_with_settings`; stderr was: {err}"
    );
}

/// The two run MODES are exclusive, and each of the other's flags is REFUSED rather than ignored:
/// a `--store` that reached a remote run would name a directory on the wrong machine, and an
/// `--addr` typed beside `--local` says the operator believes they are talking to a server.
#[test]
fn the_local_and_remote_flag_sets_refuse_each_other() {
    // ⚠ Every row here is refused inside `parse_args`, which runs BEFORE `execute`, so this test
    // never reaches staging and its verdict could not flip on the `/tmp/tmp` ownership hazard. It
    // uses `project_with_settings` anyway so the file has ONE pattern — see that helper.
    let (_project, settings) = project_with_settings();
    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "local_modes");
    let p = profile.to_str().expect("utf-8 temp path");

    for (args, needle) in [
        (vec!["backtest", "run", "--local", "--profile", p, "--addr", "127.0.0.1:1"], "--addr"),
        (vec!["backtest", "run", "--profile", p, "--store", "/tmp/store"], "--store"),
        (vec!["backtest", "run", "--profile", p, "--engine", "/tmp/backtest"], "--engine"),
        // ⚠ The three SEARCH rows that used to live here — `--optimizer tpe`, `--rank-by multi`,
        // `--trials 8` — are GONE with stage 7: the wire carries a method now
        // (`Request::RunParamscanProfile`'s `search`), so none of them is a usage error any more.
        // Their replacement is `crates/vike-cli/tests/search_walkforward_cli.rs`'s
        // `every_search_knob_now_reaches_the_dial`, which asserts the OPPOSITE property against an
        // unreachable address. Deleting a row without replacing it would have left the widening
        // unasserted on this surface. What stays below is the SPELLING check, which is still a
        // local usage error on both routes and still costs no round trip.
        (vec!["backtest", "run", "--local", "--profile", p, "--rank-by", "bogus"], "--rank-by"),
        (vec!["backtest", "run", "--local", "--profile", p, "--optimizer", "bogus"], "--optimizer"),
    ] {
        let out = run_cli(&settings, &args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?} must be a usage error: {err}");
        assert!(err.contains(needle), "{args:?} must name the offending flag: {err}");
    }
}

/// ⚠ A walk-forward has no `--local` arm, and after the fold that is a NAMED refusal rather than
/// an unknown argument. The standalone engine has ONE profile path, branching on
/// `BacktestProfile::is_paramscan` and nothing else — neither `vike_backtest::harness::run_walkforward`
/// nor its optimizing sibling is reachable from any binary — so there is nothing local to spawn.
/// `crates/vike-cli/src/cmd/walkforward.rs`'s module doc carries the condition that would change
/// it.
#[test]
fn a_walkforward_profile_has_no_local_arm() {
    let (_project, settings) = project_with_settings();
    // ⚠ The profile must DECLARE a window. Before the fold this case used the plain
    // MINIMAL_BAR_PROFILE, because `--local` was refused by the `walkforward` VERB's parser; the
    // refusal is now a property of the PROFILE, so a profile with no `[walkforward]` table would
    // be an ordinary local backtest here and would go looking for an engine.
    let profile = write_temp_profile(
        &format!("{MINIMAL_BAR_PROFILE}\n[walkforward]\nn_splits = 2\n"),
        "wf_local",
    );

    let out = run_cli(
        &settings,
        &["backtest", "run", "--local", "--profile", profile.to_str().expect("utf-8 temp path")],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.contains("--local"), "the message must name the flag: {err}");
    assert!(err.contains("[walkforward]"), "…and why this profile cannot take it: {err}");
}

/// The engine found under `<project>/bin/` is the one that runs — the rung that answers on an
/// ordinary install, and the only one this crate can point at without an absolute path.
///
/// ⚠ A SCRIPT stands in for the engine, so this test asserts the plumbing (which binary, which
/// argv, whose exit code) without needing a DataFusion build in a `vike-cli` test lane. It is
/// unix-only for exactly that reason: a `#!` line is what makes a text file executable, and
/// Windows has no equivalent that `Command::new` will run.
#[cfg(unix)]
#[test]
fn the_project_bin_engine_is_the_one_that_runs() {
    let (project, settings) = project_with_settings();
    let bin = project.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");

    // The stand-in echoes its argv and exits 7 — a code neither ladder assigns a meaning to, so
    // seeing it proves this process ran THIS file and folded ITS status rather than inventing one.
    common::plant_engine(&bin, "backtest", "#!/bin/sh\necho \"argv: $*\"\nexit 7\n");

    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "local_found");
    let out = run_cli(
        &settings,
        &[
            "backtest",
            "run",
            "--local",
            "--profile",
            profile.to_str().expect("utf-8 temp path"),
            "--store",
            "/some/store",
            "--json",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(stdout.contains("argv:"), "the child's stdout is INHERITED, not captured: {stdout}");
    assert!(stdout.contains("--profile"), "{stdout}");
    assert!(stdout.contains("--store /some/store"), "--store is forwarded: {stdout}");
    assert!(stdout.contains("--json"), "--json is forwarded: {stdout}");
    assert_eq!(out.status.code(), Some(1), "an unclassified child code folds to 1: {stderr}");
    assert!(stderr.contains("exited 7"), "…and the real one is named: {stderr}");
}

/// `--preset` is applied CLIENT-SIDE in both modes, and the local arm hands the engine the
/// REWRITTEN profile through a staged file — the property that makes `--local` a rehearsal for a
/// remote run rather than a second, subtly different one.
#[cfg(unix)]
#[test]
fn a_preset_reaches_the_local_engine_as_a_rewritten_profile() {
    let (project, settings) = project_with_settings();
    let bin = project.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");

    // This stand-in prints the profile it was handed, so the test can read what the child saw.
    common::plant_engine(&bin, "backtest", "#!/bin/sh\ncat \"$2\"\n");

    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "local_preset");
    let preset = write_temp_profile("size = 42.0\n", "local_preset_knobs");

    let out = run_cli(
        &settings,
        &[
            "backtest",
            "run",
            "--local",
            "--profile",
            profile.to_str().expect("utf-8 temp path"),
            "--preset",
            preset.to_str().expect("utf-8 temp path"),
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");

    let seen: toml::Value = toml::from_str(&stdout).expect("the child was handed valid TOML");
    assert_eq!(
        seen["strategy"]["params"]["size"].as_float(),
        Some(42.0),
        "the preset's knob reached the engine: {stdout}"
    );
    // …and the operator's own profile is untouched on disk.
    assert_eq!(std::fs::read_to_string(&profile).expect("read back"), MINIMAL_BAR_PROFILE);

    // The staged copy is removed with its scratch directory when the process exits — nothing is
    // left under `<project>/tmp` for the next run to trip over.
    let tmp = project.path().join("tmp");
    let leftovers: Vec<_> = std::fs::read_dir(&tmp)
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "the scratch directory must not outlive the run: {leftovers:?}");
}

/// THE INVERSION, end to end: no file anywhere, a profile built entirely from flags, through the
/// real loopback compute server.
///
/// `MemHistStore` is an inert stub whose `load_bars` returns empty, so the run closes zero trades —
/// this asserts the flags → profile → wire → engine → report path, not a result.
#[test]
fn a_profile_built_entirely_from_flags_reaches_the_engine() {
    let addr = spawn_server();
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args([
            "backtest",
            "run",
            "--addr",
            &addr.to_string(),
            "--venue",
            "binance",
            "--symbol",
            "BTCUSDT",
            "--interval",
            "1d",
            "--from",
            "0",
            "--to",
            "100000",
            "--strategy",
            "buy_hold",
            "--cash",
            "1000",
            "--set",
            "strategy.params.size=1.0",
            "--set",
            "strategy.params.symbol=BTCUSDT",
            "--json",
        ])
        .output()
        .expect("run vike-cli backtest");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "a flags-only run must succeed; stderr: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("final_equity"), "a real report came back: {stdout}");
}

/// `--show-effective` prints the built profile and RUNS NOTHING: exit 0, no dial, no engine.
///
/// ⚠ `--addr 127.0.0.1:1` is a port nothing listens on. A run would exit 3; exit 0 with a profile
/// on stdout is what proves the flag stopped before the socket.
#[test]
fn show_effective_prints_a_usable_profile_and_never_dials() {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args([
            "backtest",
            "run",
            "--addr",
            "127.0.0.1:1",
            "--venue",
            "binance",
            "--symbol",
            "BTCUSDT",
            "--interval",
            "1d",
            "--from",
            "0",
            "--to",
            "100000",
            "--strategy",
            "buy_hold",
            "--cash",
            "1000",
            "--show-effective",
        ])
        .output()
        .expect("run vike-cli backtest");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "--show-effective is a success; stderr: {stderr}");
    assert!(!stderr.contains("cannot connect"), "it must not have dialled: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: toml::Value = toml::from_str(&stdout).unwrap_or_else(|e| panic!("{e}; {stdout}"));
    assert_eq!(v["data"]["venue"].as_str(), Some("binance"));
    assert_eq!(v["data"]["kind"].as_str(), Some("bar"), "the implied default is in the document");
}

/// `--write-profile` writes what the flags built, and refuses to clobber an existing file.
#[test]
fn write_profile_writes_a_runnable_file_and_refuses_to_clobber() {
    // ⚠ A self-deleting handle, held for the whole test. This file's `journal_scratch_gate` row is
    // pinned `file-only`, so a hand-minted temp DIRECTORY here would change the shape that pin
    // recorded without the gate saying so.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("written.toml");
    let p = path.to_str().expect("utf-8 temp path");

    let base = [
        "backtest",
        "run",
        "--venue",
        "binance",
        "--symbol",
        "BTCUSDT",
        "--interval",
        "1d",
        "--from",
        "0",
        "--to",
        "100000",
        "--strategy",
        "buy_hold",
        "--cash",
        "1000",
        "--show-effective",
        "--write-profile",
    ];
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(base)
        .arg(p)
        .output()
        .expect("run vike-cli backtest");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let written = std::fs::read_to_string(&path).expect("the file was written");
    let v: toml::Value = toml::from_str(&written).expect("written TOML");
    assert_eq!(v["engine"]["cash"].as_integer(), Some(1000));
    assert!(!written.starts_with('#'), "the WRITTEN file is the bare document, no origin header");

    // …and a second write onto the same path refuses rather than clobbering.
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(base)
        .arg(p)
        .output()
        .expect("run vike-cli backtest");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "clobbering is a usage error; stderr: {stderr}");
    assert!(stderr.contains(p), "the refusal names the path: {stderr}");
}

/// THE SCRATCH-FREE PATH, pinned. `--local --profile X` with no other flag hands the child the
/// OPERATOR'S OWN file and stages nothing — which is what lets `--local` work in a checkout that
/// has no project above it at all.
///
/// ⚠ The fixture is the NORMALIZED text, computed here by the same re-parse/re-serialize the
/// builder performs, because the predicate compares TEXT: `toml::to_string` over a re-parsed
/// `toml::Value` drops comments and reorders keys, so an authored profile that carries either one
/// always compares unequal and is always staged. That is correct rather than unfortunate — the
/// child must receive the same bytes the remote arm would — and this test pins the case where the
/// two genuinely are the same bytes. Nothing pinned this before stage 2.
#[cfg(unix)]
#[test]
fn local_with_an_unchanged_profile_hands_the_child_the_operators_own_file() {
    let (project, settings) = project_with_settings();
    let bin = project.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");

    common::plant_engine(&bin, "backtest", "#!/bin/sh\necho \"argv: $*\"\n");

    let normalized = {
        let v: toml::Value = toml::from_str(MINIMAL_BAR_PROFILE).expect("fixture parses");
        toml::to_string(&v).expect("fixture re-serializes")
    };
    let profile = write_temp_profile(&normalized, "local_scratchfree");
    let p = profile.to_str().expect("utf-8 temp path");

    let out = run_cli(&settings, &["backtest", "run", "--local", "--profile", p]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stdout.contains(p), "the child was handed the operator's own path: {stdout}");

    // …and nothing was staged: `<project>/tmp` was never written into.
    let tmp = project.path().join("tmp");
    let leftovers: Vec<_> = std::fs::read_dir(&tmp)
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "an unchanged profile stages nothing: {leftovers:?}");

    let _ = std::fs::remove_file(&profile);
}

/// A `--local` run whose profile came from FLAGS has no file to hand the child, so it must stage —
/// and when NO PROJECT ROOT RESOLVES it refuses, naming what forced the staging.
///
/// # ⚠ HOW THE ABSENCE IS GUARANTEED, because the obvious way does not guarantee it
///
/// The branch under test is `execute_local`'s, reached when
/// `crates/vike-cli/src/cmd/engine.rs`'s `scratch_root` answers `None` — which happens for exactly
/// one reason: `Resolved::project_root` is `None`.
///
/// The obvious spelling is "unset `VIKE_SETTINGS_DIR` and run somewhere empty", and it is **not
/// hermetic**. With the variable unset, `<project>` comes from a WALK UP from the working
/// directory looking for a `Cargo.toml` declaring `[workspace]` or a `settings/` DIRECTORY — and a
/// `TempDir` sits under the shared system temp root, whose ancestors this test does not own. One
/// `/tmp/settings` created by any user or any other test turns `project_root` into `Some("/tmp")`,
/// the staging succeeds, and this case silently stops testing its own refusal. That is the same
/// shape as the `/tmp/tmp` ownership failure `project_with_settings` exists for: ambient state on a
/// shared box deciding a verdict.
///
/// So the absence is established by ARITHMETIC instead of by the filesystem.
/// `crates/vike-cli/src/lib.rs`'s `resolve_policy` computes
/// `project_root = settings_dir.parent().filter(|p| !p.as_os_str().is_empty())`, and
/// `vike-secrets`' `project_settings_dir_from` returns a `VIKE_SETTINGS_DIR` override **verbatim**,
/// with no existence and no `is_dir` probe. A RELATIVE override therefore has `""` as its parent,
/// the filter drops it, and `project_root` is `None` — on every box, every user and every ancestor
/// layout, touching no shared path. That filter is not incidental: `resolve_policy`'s own comment
/// names this exact case ("a relative `settings` has `""` as its parent").
///
/// `current_dir` is still the test's own TempDir, so any relative read lands inside it.
///
/// ⚠ If a later change resolves the override to an absolute path, `project_root` becomes `Some`,
/// staging succeeds and this test goes RED on its message assertions rather than passing for the
/// wrong reason. Repair it by finding another deterministic route to `project_root == None` — do
/// not reach back for the ambient walk.
#[test]
fn local_with_a_flags_built_profile_refuses_without_a_project_and_says_why() {
    // A self-deleting handle, held for the whole test, purely so relative reads have somewhere
    // harmless to land. Nothing about the verdict depends on what is above it — see the doc.
    let cwd = tempfile::tempdir().expect("temp dir");
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args([
            "backtest",
            "run",
            "--local",
            "--venue",
            "binance",
            "--symbol",
            "BTCUSDT",
            "--from",
            "0",
            "--to",
            "100000",
            "--strategy",
            "buy_hold",
            "--cash",
            "1000",
        ])
        .current_dir(cwd.path())
        // ⚠ RELATIVE, and that is the whole mechanism — see the doc comment. Not `env_remove`,
        // which hands the verdict to whatever happens to sit above the system temp directory.
        .env("VIKE_SETTINGS_DIR", "settings")
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .expect("run vike-cli backtest");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "must refuse; stderr: {stderr}");
    assert!(stderr.contains("stage"), "names what it could not do: {stderr}");
    assert!(
        stderr.contains("--profile") && stderr.contains("--write-profile"),
        "…and both ways out: {stderr}"
    );
    // ⚠ …and it refused for the RIGHT reason. The message for a flags-built profile names the
    // flags; the one for a changed file names the file. Asserting it here is what stops this case
    // passing on a refusal that arrived from somewhere else entirely.
    assert!(
        stderr.contains("built from flags"),
        "the refusal must be the no-file-to-hand-the-child one: {stderr}"
    );

    // ⚠ THE DISCRIMINATOR, run inside this test's own sandbox rather than asserted in prose. The
    // SAME command line with an ABSOLUTE settings directory resolves a project, stages happily, and
    // gets all the way to the missing engine (rung 3) — so the refusal above is genuinely caused by
    // the relative override and not by something ambient on this box that would answer the same way
    // whatever the test did. Without this, a green here is compatible with the mechanism not
    // working at all.
    let (project, settings) = project_with_settings();
    let out = run_cli(
        &settings,
        &[
            "backtest",
            "run",
            "--local",
            "--engine",
            project.path().join("no-such-engine").to_str().expect("utf-8 temp path"),
            "--venue",
            "binance",
            "--symbol",
            "BTCUSDT",
            "--from",
            "0",
            "--to",
            "100000",
            "--strategy",
            "buy_hold",
            "--cash",
            "1000",
        ],
    );
    let staged_err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(3),
        "with a project it must STAGE and then fail on the engine, not refuse: {staged_err}"
    );
    assert!(
        !staged_err.contains("built from flags"),
        "…so the staging refusal must NOT appear: {staged_err}"
    );
    assert!(
        project.path().join("tmp").is_dir(),
        "…and it staged inside THIS test's project: {staged_err}"
    );
}

/// ⚠ THE §15.1 ONE-ROSTER GATE'S CLI LEG. Every method the protocol names must be ACCEPTED by this
/// binary's arg parser on BOTH routes — `--local` and remote — because `OPTIMIZERS` was a
/// three-element array refusing `genetic` at parse time, on both, and a spelling check that has
/// drifted from the roster is invisible until somebody types the missing name.
///
/// Acceptance is proven NEGATIVELY: a usage error is exit 2, so "not 2" means the parser let it
/// through. What happens afterwards (an unreachable daemon, a missing engine) is not this test's
/// business and differs per box — which is exactly why it asserts on the rung rather than on
/// success.
#[test]
fn every_roster_method_is_accepted_on_both_routes() {
    let (_project, settings) = project_with_settings();
    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "roster");
    let p = profile.to_str().expect("utf-8 temp path");

    for name in vike_datahub_client::SEARCH_METHODS {
        for route in [vec!["--addr", "127.0.0.1:1"], vec!["--local"]] {
            let mut args = vec!["backtest", "run", "--profile", p, "--optimizer", name];
            args.extend(route.iter().copied());
            // genetic refuses an absent seed by design — supply one so the ROSTER, not the seed
            // rule, is what this test measures.
            if name == "genetic" {
                args.extend(["--seed", "7"]);
            }
            let out = run_cli(&settings, &args);
            let err = String::from_utf8_lossy(&out.stderr);
            assert_ne!(
                out.status.code(),
                Some(2),
                "--optimizer {name} must not be a USAGE error on {route:?}: {err}"
            );
        }
    }

    // …and the check is real: a name outside the roster IS a usage error, on both routes.
    for route in [vec!["--addr", "127.0.0.1:1"], vec!["--local"]] {
        let mut args = vec!["backtest", "run", "--profile", p, "--optimizer", "bogus"];
        args.extend(route.iter().copied());
        let out = run_cli(&settings, &args);
        assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    }

    let _ = std::fs::remove_file(&profile);
}

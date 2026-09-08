//! End-to-end tests for the `vike-cli backtest` command (headless two-layer plan, PR-1).
//!
//! Hermetic and loopback-only: spawn the REAL [`vike_datahub::serve`] over an in-memory
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
use vike_datahub::serve;

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
        .arg("--profile")
        .arg(&profile)
        .arg("--addr")
        .arg(addr.to_string())
        .arg("--json")
        .output()
        .expect("run vike-cli backtest --json");

    assert!(out.status.success(), "vike-cli backtest --json must exit 0");
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

/// A missing `--profile` is a local arg error: exit non-zero, usage on stderr, no network needed.
#[test]
fn missing_profile_arg_exits_1() {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .arg("backtest")
        .output()
        .expect("run vike-cli backtest");
    assert!(!out.status.success(), "missing --profile must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage:"), "must print usage; stderr: {stderr}");
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
fn run_cli(settings_dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(args)
        .env("VIKE_SETTINGS_DIR", settings_dir)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
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
    let scratch = tempfile::tempdir().expect("tempdir");
    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "local_missing");
    let absent = scratch.path().join("no-such-engine");

    let out = run_cli(
        scratch.path(),
        &[
            "backtest",
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
}

/// The two run MODES are exclusive, and each of the other's flags is REFUSED rather than ignored:
/// a `--store` that reached a remote run would name a directory on the wrong machine, and an
/// `--addr` typed beside `--local` says the operator believes they are talking to a server.
#[test]
fn the_local_and_remote_flag_sets_refuse_each_other() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "local_modes");
    let p = profile.to_str().expect("utf-8 temp path");

    for (args, needle) in [
        (vec!["backtest", "--local", "--profile", p, "--addr", "127.0.0.1:1"], "--addr"),
        (vec!["backtest", "--profile", p, "--store", "/tmp/store"], "--store"),
        (vec!["backtest", "--profile", p, "--engine", "/tmp/backtest"], "--engine"),
        (vec!["sweep", "--local", "--profile", p, "--addr", "127.0.0.1:1"], "--addr"),
        (vec!["sweep", "--profile", p, "--store", "/tmp/store"], "--store"),
    ] {
        let out = run_cli(scratch.path(), &args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?} must be a usage error: {err}");
        assert!(err.contains(needle), "{args:?} must name the offending flag: {err}");
    }
}

/// `walkforward` has NO `--local`, deliberately — the standalone engine has no walk-forward mode to
/// drive. The flag is therefore an unknown argument on the usage rung, which is a smaller lie than
/// a flag that exists and always fails. `crates/vike-cli/src/cmd/walkforward.rs`'s module doc is
/// where that decision lives, and this is what would go red if somebody added the flag without it.
#[test]
fn walkforward_has_no_local_arm() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "wf_local");

    let out = run_cli(
        scratch.path(),
        &["walkforward", "--local", "--profile", profile.to_str().expect("utf-8 temp path")],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.contains("--local"), "the message must name the flag it did not know: {err}");
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
    use std::os::unix::fs::PermissionsExt;

    let project = tempfile::tempdir().expect("tempdir");
    let settings = project.path().join("settings");
    std::fs::create_dir_all(&settings).expect("create settings");
    let bin = project.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");

    // The stand-in echoes its argv and exits 7 — a code neither ladder assigns a meaning to, so
    // seeing it proves this process ran THIS file and folded ITS status rather than inventing one.
    let engine = bin.join("backtest");
    std::fs::write(&engine, "#!/bin/sh\necho \"argv: $*\"\nexit 7\n").expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "local_found");
    let out = run_cli(
        &settings,
        &[
            "backtest",
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
    use std::os::unix::fs::PermissionsExt;

    let project = tempfile::tempdir().expect("tempdir");
    let settings = project.path().join("settings");
    std::fs::create_dir_all(&settings).expect("create settings");
    let bin = project.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");

    // This stand-in prints the profile it was handed, so the test can read what the child saw.
    let engine = bin.join("backtest");
    std::fs::write(&engine, "#!/bin/sh\ncat \"$2\"\n").expect("plant engine");
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let profile = write_temp_profile(MINIMAL_BAR_PROFILE, "local_preset");
    let preset = write_temp_profile("size = 42.0\n", "local_preset_knobs");

    let out = run_cli(
        &settings,
        &[
            "backtest",
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

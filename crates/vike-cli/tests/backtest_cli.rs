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

//! End-to-end tests for the `vike-cli sweep` / `vike-cli walkforward` commands over the v7
//! PROFILE-shaped datahub verbs.
//!
//! Hermetic and loopback-only, the exact pattern of `backtest_cli.rs`: spawn the REAL
//! [`vike_datahub::serve`] over an in-memory `MemHistStore` on an ephemeral `127.0.0.1:0` port and
//! run the SHIPPED bin against that address. The server here is a LEAN (no-`serve-datafusion`)
//! build, which is itself part of the contract — these verbs run the DataFusion-free
//! `vike_backtest::harness`, unlike the Studio `RunSweep`/`RunWalkforward` a lean server refuses.
//!
//! `MemHistStore` is an inert stub whose `load_bars` returns empty, so these assert the PLUMBING —
//! profile TOML → server → harness → report JSON → rendered table — not results. The bit-parity of
//! the numbers is `vike-datahub`'s `run_sweep_walkforward_profile_roundtrip` gate.

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::serve;

/// A minimal, valid bar-mode profile. `{extra}` appends the `[sweep]` / `[walkforward]` section a
/// given test needs.
fn profile(extra: &str) -> String {
    format!(
        r#"
name = "vike_cli_profile_verb_smoke"

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
{extra}
"#
    )
}

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

/// Write `contents` to a uniquely-named temp file and return its path (unique per (pid, nanos) so
/// parallel test threads never collide; the OS reclaims the temp dir).
fn write_temp_profile(contents: &str, tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut path = std::env::temp_dir();
    path.push(format!("vike_cli_pv_{tag}_{}_{nanos}.toml", std::process::id()));
    let mut f = std::fs::File::create(&path).expect("create temp profile");
    f.write_all(contents.as_bytes()).expect("write temp profile");
    path
}

/// Run the shipped bin as `vike-cli <subcommand> --profile <file> --addr <addr> [extra…]`.
fn run_cli(subcommand: &str, profile_path: &Path, addr: SocketAddr, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .arg(subcommand)
        .arg("--profile")
        .arg(profile_path)
        .arg("--addr")
        .arg(addr.to_string())
        .args(extra)
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli {subcommand}: {e}"))
}

/// A profile with a `[sweep]` grid: the CLI ships the TOML, the server expands + ranks it, and the
/// rendered table names every grid point.
#[test]
fn sweep_ships_the_profile_and_prints_the_server_ranked_table() {
    let addr = spawn_server();
    let path = write_temp_profile(&profile("\n[sweep]\nsize = [1.0, 2.0]\n"), "sweep");

    let out = run_cli("sweep", &path, addr, &["--rank-by", "return"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "vike-cli sweep must exit 0; stderr: {stderr}");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("parameter sweep — 2 point(s)"), "stdout: {stdout}");
    // The header echoes the metric the SERVER applied, under the server's own name for it
    // (`--rank-by return` resolves to `RankMetric::TotalReturn`).
    assert!(
        stdout.contains("ranked server-side by total_return"),
        "names the SERVER's metric: {stdout}"
    );
    assert!(stdout.contains("size=1"), "each grid point's overrides are shown: {stdout}");

    let _ = std::fs::remove_file(&path);
}

/// `--json` prints the server's ranked `SweepReport` verbatim — valid JSON carrying the metric the
/// SERVER ranked by (no client-side re-ranking exists any more).
#[test]
fn sweep_json_is_the_server_report_verbatim() {
    let addr = spawn_server();
    let path = write_temp_profile(&profile("\n[sweep]\nsize = [1.0, 2.0]\n"), "sweepjson");

    let out = run_cli("sweep", &path, addr, &["--json"]);
    assert!(out.status.success(), "vike-cli sweep --json must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json stdout must be valid JSON");
    assert_eq!(v["rank_by"], "sharpe", "no --rank-by ⇒ the server's default metric");
    assert_eq!(v["rows"].as_array().map(Vec::len), Some(2));

    let _ = std::fs::remove_file(&path);
}

/// A profile with no `[sweep]` table is a clean SERVER-side error (the profile is parsed once,
/// server-side): exit non-zero, message on stderr, nothing on stdout.
#[test]
fn sweep_without_a_sweep_table_exits_1_with_stderr() {
    let addr = spawn_server();
    let path = write_temp_profile(&profile(""), "nosweep");

    let out = run_cli("sweep", &path, addr, &[]);
    assert!(!out.status.success(), "a profile with no [sweep] must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("[sweep]"), "names the missing section: {stderr}");
    assert!(out.stdout.is_empty(), "no table on stdout for a failed sweep");

    let _ = std::fs::remove_file(&path);
}

/// `--rank-by` is validated LOCALLY against the server's metric roster, so a typo never costs a
/// round-trip (and needs no server at all).
#[test]
fn an_unknown_rank_by_is_a_local_usage_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["sweep", "--profile", "run.toml", "--rank-by", "bogus"])
        .output()
        .expect("run vike-cli sweep");
    assert!(!out.status.success(), "an unknown --rank-by must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--rank-by"), "names the flag: {stderr}");
    assert!(stderr.contains("usage:"), "prints usage: {stderr}");
}

/// A `[walkforward]` profile reaches the server's walk-forward runner. Over the inert
/// `MemHistStore` there are no bars, so the run is a clean server-side DATA error rather than a
/// zero-window report — which is exactly the contract: the request/response path works and an empty
/// slice is never silently reported as a walk-forward.
#[test]
fn walkforward_ships_the_profile_and_surfaces_an_empty_slice_cleanly() {
    let addr = spawn_server();
    let path = write_temp_profile(&profile("\n[walkforward]\nn_splits = 4\n"), "wf");

    let out = run_cli("walkforward", &path, addr, &[]);
    assert!(!out.status.success(), "an empty bar slice must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no bars"), "names the data problem: {stderr}");

    let _ = std::fs::remove_file(&path);
}

/// A profile with no `[walkforward]` table is a clean server-side error naming the section.
#[test]
fn walkforward_without_its_table_exits_1_with_stderr() {
    let addr = spawn_server();
    let path = write_temp_profile(&profile(""), "nowf");

    let out = run_cli("walkforward", &path, addr, &[]);
    assert!(!out.status.success(), "a profile with no [walkforward] must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("[walkforward]"), "names the missing section: {stderr}");

    let _ = std::fs::remove_file(&path);
}

//! End-to-end tests for `vike-cli backtest run`'s parameter SEARCH and its WALK-FORWARD over the
//! v7 PROFILE-shaped datahub verbs.
//!
//! ⚠ The file name says `walkforward` and there is no such VERB any more — decision 3 of
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` folded it into
//! `backtest run`, where the PROFILE selects it. The name is kept deliberately:
//! `crates/vike-ops/tests/journal_scratch_gate.rs`'s `TREE_PIN` names this exact path, and renaming
//! it would redden that gate in BOTH directions at once — a stale row and a newly-unpinned site,
//! which is the #1754 failure `crates/vike-ops/tests/path_key_gate.rs` exists to generalise.
//!
//! ⚠ This file was `sweep_walkforward_cli.rs`. Ruling 13 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` deleted the `sweep` verb
//! — the word "optimize" lives in `--optimizer`, not in a second verb — so the search half now
//! drives `backtest`. What each case asserts is unchanged except where the ROUTING changed, and
//! the one that did is called out at its site.
//!
//! Hermetic and loopback-only, the exact pattern of `backtest_cli.rs`: spawn the REAL
//! [`vike_backtest::compute_server::serve`] over an in-memory `MemHistStore` on an ephemeral `127.0.0.1:0` port and
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
// ⚠ The COMPUTE server, not `vike_datahub::serve`. Ruling 7 of
// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` moved the `Run*` verbs to
// `vike-backend backtest --addr`, so pointing this harness at the data daemon would now prove a
// wrong-plane refusal while claiming to prove the command works.
use vike_backtest::compute_server::serve;

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

/// Run the shipped bin as `vike-cli backtest run --profile <file> --addr <addr> [extra…]`.
///
/// ⚠ The sub-verb is not a parameter any more. It was, while `backtest` and `walkforward` were two
/// top-level verbs; decision 3 folded the second into the first and decision 11 made the sub-verb
/// mandatory, so every case in this file drives the SAME two tokens and what varies is the
/// PROFILE — which is the routing property the file exists to pin.
fn run_cli(profile_path: &Path, addr: SocketAddr, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["backtest", "run"])
        .arg("--profile")
        .arg(profile_path)
        .arg("--addr")
        .arg(addr.to_string())
        .args(extra)
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli backtest run: {e}"))
}

/// A profile with a `[sweep]` grid: the CLI ships the TOML, the server expands + ranks it, and the
/// rendered table names every grid point.
#[test]
fn a_sweep_profile_routes_to_the_search_verb_and_prints_the_server_ranked_table() {
    let addr = spawn_server();
    let path = write_temp_profile(&profile("\n[sweep]\nsize = [1.0, 2.0]\n"), "sweep");

    let out = run_cli(&path, addr, &["--rank-by", "return"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "vike-cli backtest must exit 0; stderr: {stderr}");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("parameter search — 2 point(s)"), "stdout: {stdout}");
    // The header echoes the metric the SERVER applied, under the server's own name for it
    // (`--rank-by return` resolves to `RankMetric::TotalReturn`).
    assert!(
        stdout.contains("ranked server-side by total_return"),
        "names the SERVER's metric: {stdout}"
    );
    assert!(stdout.contains("size=1"), "each grid point's overrides are shown: {stdout}");

    let _ = std::fs::remove_file(&path);
}

/// `--json` prints the server's ranked `ParamscanReport` verbatim — valid JSON carrying the metric the
/// SERVER ranked by (no client-side re-ranking exists any more).
#[test]
fn the_search_json_is_the_server_report_verbatim() {
    let addr = spawn_server();
    let path = write_temp_profile(&profile("\n[sweep]\nsize = [1.0, 2.0]\n"), "sweepjson");

    let out = run_cli(&path, addr, &["--json"]);
    assert!(out.status.success(), "vike-cli backtest run --json must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json stdout must be valid JSON");
    assert_eq!(v["rank_by"], "sharpe", "no --rank-by ⇒ the server's default metric");
    assert_eq!(v["rows"].as_array().map(Vec::len), Some(2));

    let _ = std::fs::remove_file(&path);
}

/// ⚠ **THE CASE WHOSE MEANING CHANGED, and it changed for the better.**
///
/// It asserted that `vike-cli sweep` on a profile with no `[sweep]` table came back as a clean
/// server-side error naming the missing section. That refusal existed because the VERB promised a
/// grid the profile could not supply. There is no such verb now: the PROFILE selects, on both
/// routes and on both machines (`declares_a_paramscan_grid` here, `BacktestProfile::is_paramscan` there),
/// so this input is simply one backtest — which is what `--local` had always done with it, and the
/// divergence `crates/vike-cli/src/cmd/backtest.rs`'s routing closes.
///
/// ⚠ **What it asserts is the report SHAPE, not an error string, and the first writing of this
/// case got that wrong.** It claimed the run was "a clean server-side DATA error" over the inert
/// `MemHistStore` and asserted a non-zero exit naming "no bars". It is not: an empty slice is a
/// SUCCESSFUL backtest reporting zero trades — `crates/vike-cli/tests/backtest_cli.rs`'s
/// `valid_profile_prints_report_json_with_name` has asserted exit 0 on this exact store since long
/// before this branch, and that file's own doc says so ("loads no bars (zero trades), which is
/// enough to exercise the whole path"). Only `walkforward` errors on it, because it has no windows
/// to split.
///
/// So the two wire verbs are told apart POSITIVELY, by the document that comes back: `RunBacktest`
/// answers a `BacktestReport` carrying the profile's `name`, while `RunParamscanProfile` answers a
/// `ParamscanReport` of `rows`/`rank_by` (and, on a profile with no grid, an error naming `[paramscan]`).
/// Asserting the shape is stronger than asserting a message and cannot go stale against one.
#[test]
fn a_profile_with_no_grid_runs_one_backtest_rather_than_refusing() {
    let addr = spawn_server();
    let path = write_temp_profile(&profile(""), "nogrid");

    let out = run_cli(&path, addr, &["--json"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "a no-grid profile is one backtest, not a refusal: {stderr}");
    assert!(
        !stderr.contains("[paramscan]"),
        "…and NOT a missing grid: the profile decides what runs, so a profile without one is a \
         backtest rather than a refused search: {stderr}"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json stdout must be valid JSON");
    assert_eq!(
        v["name"].as_str(),
        Some("vike_cli_profile_verb_smoke"),
        "the document is a BacktestReport — the shape `RunBacktest` answers: {stdout}"
    );
    assert!(
        v.get("rows").is_none() && v.get("rank_by").is_none(),
        "…and NOT the ParamscanReport `RunParamscanProfile` would have answered, which is the routing this \
         case exists to pin: {stdout}"
    );

    let _ = std::fs::remove_file(&path);
}

/// `--rank-by` is validated LOCALLY against the metric roster, so a typo never costs a round trip
/// (and needs no server at all). `--optimizer` is checked the same way, on the same rung.
#[test]
fn an_unknown_search_selector_is_a_local_usage_error() {
    for (flag, value) in [("--rank-by", "bogus"), ("--optimizer", "bogus")] {
        let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
            .args(["backtest", "run", "--local", "--profile", "run.toml", flag, value])
            .output()
            .expect("run vike-cli backtest");
        assert!(!out.status.success(), "an unknown {flag} must exit non-zero");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(flag), "names the flag: {stderr}");
        assert!(stderr.contains("usage:"), "prints usage: {stderr}");
    }
}

/// ⚠ **The narrowing this verb shipped with is GONE, and this test is its inverse.** It used to
/// assert that six knob spellings were refused on the USAGE rung without a socket being opened,
/// because `Request::RunParamscanProfile` carried no method selector. Stage 7 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` widened the wire, so each of
/// them now REACHES the daemon — proven here by the one thing a refusal could never produce: a
/// CONNECT failure against an address nothing listens on.
///
/// ⚠ `--addr 127.0.0.1:1` is load-bearing. Without it the command dials the DEFAULT compute
/// address, and on a box where a daemon happens to be listening this test would run real searches.
#[test]
fn every_search_knob_now_reaches_the_dial() {
    let path = write_temp_profile(&profile("\n[paramscan]\nsize = [1.0, 2.0]\n"), "remotemethod");
    let p = path.to_str().expect("utf-8 temp path");
    for extra in [
        vec!["--optimizer", "tpe"],
        vec!["--optimizer", "euler"],
        vec!["--optimizer", "genetic", "--seed", "7"],
        vec!["--rank-by", "multi"],
        vec!["--trials", "8"],
        vec!["--seed", "1"],
        vec!["--euler-depth", "2"],
    ] {
        let mut args = vec!["backtest", "run", "--profile", p, "--addr", "127.0.0.1:1"];
        args.extend(extra.iter().copied());
        let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
            .args(&args)
            .output()
            .expect("run vike-cli backtest");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(3), "{args:?} must reach the dial: {stderr}");
        assert!(stderr.contains("cannot connect"), "{args:?} must have dialled: {stderr}");
        assert!(
            !stderr.contains("--local only"),
            "{args:?} must NOT be refused as local-only any more: {stderr}"
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// ⚠ …and the ONE thing still refused before the dial: a method that is not in the roster at all.
/// That is a SPELLING check against `vike_datahub_client::SEARCH_METHODS`, so it stays on the usage
/// rung and costs no round trip — the property the whole `one_of` family exists for.
#[test]
fn an_unknown_method_is_still_refused_before_the_dial() {
    let path = write_temp_profile(&profile("\n[paramscan]\nsize = [1.0, 2.0]\n"), "unknownmethod");
    let p = path.to_str().expect("utf-8 temp path");
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["backtest", "run", "--profile", p, "--addr", "127.0.0.1:1", "--optimizer", "bogus"])
        .output()
        .expect("run vike-cli backtest");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(!stderr.contains("cannot connect"), "a typo must not cost a round trip: {stderr}");
    let _ = std::fs::remove_file(&path);
}

/// ⚠ **THE ROUTING HOLE, closed.** A method named on a profile with NO grid table must not route to
/// `RunBacktest`, whose server arm ignores the method entirely — that is the same silent downgrade
/// one verb over. It routes to the search verb, where the server's own "profile has no [paramscan]
/// table" refusal answers, exactly as the local engine's pre-flight does. Proven here by the dial:
/// reaching a CONNECT failure means this side did not quietly pick the other verb.
#[test]
fn a_method_on_a_profile_with_no_grid_still_goes_to_the_search_verb() {
    let path = write_temp_profile(&profile(""), "nogrid");
    let p = path.to_str().expect("utf-8 temp path");
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["backtest", "run", "--profile", p, "--addr", "127.0.0.1:1", "--optimizer", "tpe"])
        .output()
        .expect("run vike-cli backtest");
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_file(&path);
}

/// A `[walkforward]` profile reaches the server's walk-forward runner THROUGH `backtest run` —
/// decision 3's fold. Over the inert `MemHistStore` there are no bars, so the run is a clean
/// server-side DATA error rather than a zero-window report, which is also the POSITIVE
/// discrimination this case needs: the same profile through the plain-backtest verb SUCCEEDS with
/// a `BacktestReport` (`a_profile_with_no_grid_runs_one_backtest_rather_than_refusing` pins that),
/// so "exit non-zero, naming no bars" can only have come from the walk-forward arm.
#[test]
fn a_walkforward_profile_routes_to_the_walk_forward_verb() {
    let addr = spawn_server();
    let path = write_temp_profile(&profile("\n[walkforward]\nn_splits = 4\n"), "wf");

    let out = run_cli(&path, addr, &[]);
    assert!(!out.status.success(), "an empty bar slice must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no bars"), "names the data problem: {stderr}");

    let _ = std::fs::remove_file(&path);
}

/// ⚠ `--rank-by` configures NOTHING on the walk-forward route and is refused by name rather than
/// dropped. `DatahubClient::run_walkforward_profile` takes the profile TOML and nothing else —
/// there is no ranking field on that call — while a window's own ranking is `[walkforward].rank_by`
/// in the profile. A flag that parsed, set a field and then changed nothing is the shape
/// `Policy::max_total_exposure` was DELETED for: positive confirmation of something false.
///
/// ⚠ The address is an unreachable one and the refusal must beat it. That is the half a
/// code-only assertion would miss: exit 2 rather than exit 3 is what says the flag was judged
/// BEFORE the dial.
#[test]
fn a_ranking_flag_on_a_walkforward_run_is_refused_naming_the_profile_key() {
    let path = write_temp_profile(&profile("\n[walkforward]\nn_splits = 2\n"), "wfrank");
    for extra in [vec!["--rank-by", "sharpe"], vec!["--optimizer", "grid"]] {
        let mut args =
            vec!["backtest", "run", "--profile", path.to_str().expect("utf-8 temp path")];
        args.extend(["--addr", "127.0.0.1:1"]);
        args.extend(extra.iter().copied());
        let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
            .args(&args)
            .output()
            .expect("run vike-cli backtest run");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(extra[0]), "{args:?} names the flag: {stderr}");
        assert!(stderr.contains("[walkforward]"), "{args:?} names where it belongs: {stderr}");
        assert!(!stderr.contains("cannot connect"), "{args:?} must not have dialled: {stderr}");
    }
    let _ = std::fs::remove_file(&path);
}

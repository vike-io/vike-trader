//! Parity gate for the v7 PROFILE-shaped `RunParamscanProfile` / `RunWalkforwardProfile` verbs: a run
//! executed LOCALLY (`vike_backtest::harness::run_paramscan` / `harness::run_walkforward` over the same
//! store) and the SAME run executed OVER THE WIRE must produce a BYTE-IDENTICAL report JSON — the
//! profile-shaped sibling of the Studio-verb gate that used to sit beside it in `vike-datahub`.
//!
//! It also pins the CAPABILITY these verbs exist for: because the whole profile TOML crosses the
//! wire and the SERVER parses it with the one `ProfileBacktest::from_toml_str`, sections the old
//! client-side `DataSliceWire`/`StrategyEngineConfigWire` mapping could not carry — here `[risk]` — reach the
//! engine and change the answer. Under the DTO-shaped verbs that section was silently dropped.
//!
//! ⚠ It MOVED here from `vike-datahub` under ruling 7 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`, with the verbs it gates:
//! `RunParamscanProfile`/`RunWalkforwardProfile` are served by `vike-backend backtesthub --addr` now, so a
//! parity gate over `vike_datahub::serve` would be comparing the local run against a refusal.
//!
//! Both verbs are served on EVERY build of this crate that compiles the server (the harness is
//! DataFusion-free — proven by
//! the layer gate's store fence, not by a lean build of this crate, which links the store). This file needs
//! the concrete store because it needs
//! a store that can actually HOLD bars: `HistoryStoreMemory`'s `append_bars`/`load_bars` are inert stubs,
//! so a meaningful run needs the concrete `HistoryStoreDataFusion` over a temp dir.

use std::sync::Arc;

use tempfile::TempDir;
use vike_backtest::harness::{self, ProfileBacktest, RankMetric};
use vike_backtesthub::test_support::spawn_server_over;
use vike_backtesthub_client::BacktesthubClient;
use vike_data::HistoryStore;
use vike_marketdata::Bar;
use vike_store::HistoryStoreDataFusion;

const CASH: f64 = 1000.0;

/// The profile every test ships: `engine_extra` adds `[engine]` keys, `extra` appends whole
/// sections (a `[sweep]` grid, a `[walkforward]` table, a `[risk]` section…). Bar mode over the
/// seeded fixture below.
///
/// `fee_rate` is left OFF by default deliberately: on this fixture (open == close per bar) a
/// zero-cost fill leaves equity at exactly `cash` on the entry bar, which is the contract
/// `walk_forward_strategy` debug-asserts for every window's first sample.
fn profile_toml(engine_extra: &str, extra: &str) -> String {
    format!(
        r#"
name = "profile_verb_parity"

[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1m"
from = "0"
to = "100000000"

[engine]
cash = {CASH}
{engine_extra}

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
{extra}
"#
    )
}

/// A `HistoryStoreDataFusion` over a temp dir seeded with 400 deterministic oscillating BTCUSDT bars (the
/// `run_sweep_walkforward_roundtrip` fixture). The `TempDir` is returned so the caller keeps it
/// alive for the store's lifetime.
fn seeded_bar_store() -> (TempDir, Arc<dyn HistoryStore + Send + Sync>) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let mut px = 100.0f64;
    let mut seed = 0x1234_5678u64;
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            px = (px + ((seed >> 32) as f64 / u32::MAX as f64 - 0.5) * 2.0).max(1.0);
            Bar {
                timestamp: 60_000 * (i as i64 + 1),
                open: px,
                high: px,
                low: px,
                close: px,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    (dir, store)
}

/// `RunBacktest`'s answer over a real socket, parsed. What an `--addr` caller is handed.
fn run_over_the_wire(store: &Arc<dyn HistoryStore + Send + Sync>, toml: &str) -> serde_json::Value {
    let addr = spawn_server_over(Arc::clone(store), vike_strategies_check::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let json = client.run_backtest(toml).expect("the run answers");
    serde_json::from_str(&json).expect("the report is JSON")
}

/// `[data]` keys added to [`profile_toml`]'s bar profile, whose window is far wider than
/// [`seeded_bar_store`]'s tape, so an armed rule always has something to say.
fn with_data_keys(keys: &str) -> String {
    let toml = profile_toml("", "");
    let widened = toml.replace("kind = \"bar\"", &format!("kind = \"bar\"\n{keys}"));
    assert_ne!(widened, toml, "the fixture's text moved");
    widened
}

fn warnings(report: &serde_json::Value) -> Vec<String> {
    report["data_warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("the report carries no data_warnings: {report}"))
        .iter()
        .map(|l| l.as_str().expect("a sentence").to_string())
        .collect()
}

/// `data.on_gap = "warn"` said so in the daemon's log and nowhere else, so the caller who armed it
/// got a clean report over a window the store does not cover.
#[test]
fn a_warning_coverage_gate_reaches_the_address_caller() {
    let (_dir, store) = seeded_bar_store();
    let keys = "require_coverage = true\non_gap = \"warn\"";
    let got = warnings(&run_over_the_wire(&store, &with_data_keys(keys)));
    assert!(got.iter().any(|l| l.contains("does not cover the window")), "{got:?}");
}

/// The strict refusal promises `data.universe = "covered"` runs "with the finding on the record".
#[test]
fn a_covered_universe_finding_reaches_the_address_caller() {
    let (_dir, store) = seeded_bar_store();
    let got = warnings(&run_over_the_wire(&store, &with_data_keys("universe = \"covered\"")));
    assert!(got.iter().any(|l| l.contains("BTCUSDT")), "{got:?}");
}

/// A multi-symbol tick run computed on a subset came back as a clean report over the symbols it
/// declared.
#[test]
fn a_tick_symbol_with_no_rows_reaches_the_address_caller() {
    let dir = tempfile::tempdir().unwrap();
    let hist = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let trades = [
        vike_data::test_support::grouped_ticks::trade("BTCUSDT", 1_000, 100),
        vike_data::test_support::grouped_ticks::trade("BTCUSDT", 2_000, 101),
    ];
    hist.append_trades("binance", "BTCUSDT", &trades, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(hist);

    let toml = profile_toml("", "")
        .replace("symbols = [\"BTCUSDT\"]", "symbols = [\"BTCUSDT\", \"ETHUSDT\"]")
        .replace("kind = \"bar\"", "kind = \"tick\"");
    let got = warnings(&run_over_the_wire(&store, &toml));
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("ETHUSDT"), "names the symbol that did not run: {got:?}");
}

/// The other half: a run nothing was found in has no such key, so its report is the bytes it was.
#[test]
fn a_clean_run_has_no_data_warnings_key() {
    let (_dir, store) = seeded_bar_store();
    let report = run_over_the_wire(&store, &profile_toml("", ""));
    assert!(report.get("data_warnings").is_none(), "{report}");
}

/// LOCAL == REMOTE for the sweep verb: the same profile ranked by the same metric produces the
/// identical `ParamscanReport` JSON whether run in-process or shipped as TOML to the server. The server
/// does the ranking, so a client never re-implements sharpe/return/max_dd.
#[test]
fn profile_sweep_is_byte_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategies_check::stand_in);

    let toml = profile_toml("", "\n[paramscan]\nsize = [1.0, 2.0, 3.0]\n");
    let profile = ProfileBacktest::from_toml_str(&toml)
        .expect("profile parses")
        .with_stand_in(vike_strategies_check::stand_in);
    let local = harness::run_paramscan(&profile, Arc::clone(&store), RankMetric::Sharpe)
        .expect("local sweep runs");
    let local_json = serde_json::to_string(&local).expect("local report serializes");

    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote_json =
        client.run_paramscan_profile(&toml, Some("sharpe"), None).expect("remote sweep runs");

    assert_eq!(local_json, remote_json, "local and remote sweep reports must be byte-identical");

    let v: serde_json::Value = serde_json::from_str(&remote_json).unwrap();
    assert_eq!(v["rows"].as_array().map(Vec::len), Some(3), "one row per grid point");
    assert_eq!(v["rank_by"], "sharpe");
    // Rows arrive ALREADY ranked, each carrying its own BacktestReport — what the CLI renders.
    assert!(v["rows"][0]["report"]["final_equity"].is_number(), "rows carry server stats: {v}");
}

/// LOCAL == REMOTE for the walk-forward verb, and the split count (read from the profile's
/// `[walkforward]` table server-side) produces exactly that many OOS windows.
#[test]
fn profile_walkforward_is_byte_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategies_check::stand_in);

    let toml = profile_toml("", "\n[walkforward]\nn_splits = 4\n");
    let profile = ProfileBacktest::from_toml_str(&toml)
        .expect("profile parses")
        .with_stand_in(vike_strategies_check::stand_in);
    let local = harness::run_walkforward(&profile, Arc::clone(&store)).expect("local wf runs");
    let local_json = serde_json::to_string(&local).expect("local report serializes");

    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote_json = client.run_walkforward_profile(&toml).expect("remote wf runs");

    assert_eq!(local_json, remote_json, "local and remote walk-forward reports must match");

    let v: serde_json::Value = serde_json::from_str(&remote_json).unwrap();
    assert_eq!(v["windows"].as_array().map(Vec::len), Some(4), "one window per split");
    assert!(v["out_of_sample_equity_curve"].as_array().is_some_and(|c| !c.is_empty()));
}

/// THE CAPABILITY THESE VERBS EXIST FOR: a `[risk]` section — which the DTO-shaped `RunSweep` wire
/// verb cannot carry at all — crosses with the profile and reaches the engine.
///
/// The signal is the one `harness_risk_wiring.rs` established: with `fee_rate > 0` a FILLED order
/// strictly moves equity off `cash` (the fee is debited immediately), while a DENIED order leaves it
/// at EXACTLY `cash` — no fill, no fee, no position to mark. So with a `max_notional_per_order` far
/// below the first order's notional every grid point must land exactly on `cash`, and the same
/// profile without `[risk]` must not. A silently-dropped section would make the two runs identical.
#[test]
fn a_risk_section_crosses_the_wire_and_changes_the_sweep() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(store, vike_strategies_check::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");

    let grid = "\n[paramscan]\nsize = [1.0, 2.0]\n";
    let unrestricted = client
        .run_paramscan_profile(&profile_toml("fee_rate = 0.001", grid), None, None)
        .expect("the unrestricted sweep runs");
    let restricted = client
        .run_paramscan_profile(
            &profile_toml(
                "fee_rate = 0.001",
                &format!("{grid}\n[risk]\nmax_notional_per_order = 1.0\n"),
            ),
            None,
            None,
        )
        .expect("the risk-gated sweep runs");

    let equities = |json: &str| -> Vec<f64> {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        v["rows"]
            .as_array()
            .expect("rows array")
            .iter()
            .map(|r| {
                r["report"]["final_equity"]
                    .as_f64()
                    .unwrap_or_else(|| panic!("every point must run: {r}"))
            })
            .collect()
    };

    let gated = equities(&restricted);
    assert_eq!(gated.len(), 2, "one row per grid point");
    for e in &gated {
        assert_eq!(*e, CASH, "every risk-denied point must leave equity at cash: {gated:?}");
    }
    let ungated = equities(&unrestricted);
    assert!(
        ungated.iter().all(|e| *e != CASH),
        "without [risk] every point must actually fill (fee debited), or this proves nothing: \
         {ungated:?}"
    );
}

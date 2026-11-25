//! Parity gate for the v7 PROFILE-shaped `RunParamscanProfile` / `RunWalkforwardProfile` verbs: a run
//! executed LOCALLY (`vike_backtest::search::paramscan::run_paramscan` / `walkforward::runner::run_walkforward` over the same
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
use vike_backtest::run::profile::ProfileBacktest;
use vike_backtest::search::paramscan::RankMetric;
use vike_backtesthub::test_support::spawn_server_over;
use vike_backtesthub_client::BacktesthubClient;
use vike_data::HistoryStore;
use vike_marketdata::Bar;
use vike_store::HistoryStoreDataFusion;

// The book's forward report (chapter 22), compiled into this test binary to read a run THIS daemon saved.
#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/forward_report/forward_report.rs"]
pub mod user_forward_report;

const CASH: f64 = 1000.0;

/// The profile every test ships: `engine_extra` adds `[engine]` keys, `extra` appends whole
/// sections (a `[sweep]` grid, a `[walkforward]` table, a `[risk]` section…). Bar mode over the
/// seeded fixture below.
///
/// No `commission` is named by default deliberately: on this fixture (open == close per bar) a
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
    let addr = spawn_server_over(Arc::clone(store), vike_strategy_lab::stand_in);
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
    // Beside the USDT line every BTCUSDT run on a store with no USDT-USD series carries (decision 0243).
    let got: Vec<&String> = got
        .iter()
        .filter(|l| l.as_str() != vike_backtest::run::data_plan::USDT_USD_NOT_FOUND)
        .collect();
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("ETHUSDT"), "names the symbol that did not run: {got:?}");
}

/// The other half: a run nothing was found in carries only the line every BTCUSDT run on a store with no
/// USDT-USD series carries, its USDT booked one to one (decision 0243, amended 2026-11-16).
#[test]
fn a_clean_run_carries_only_its_usdt_line() {
    let (_dir, store) = seeded_bar_store();
    let report = run_over_the_wire(&store, &profile_toml("", ""));
    let usdt = serde_json::json!([vike_backtest::run::data_plan::USDT_USD_NOT_FOUND]);
    assert_eq!(report.get("data_warnings"), Some(&usdt), "{report}");
}

/// LOCAL == REMOTE for the sweep verb: the same profile ranked by the same metric produces the
/// identical `ParamscanReport` JSON whether run in-process or shipped as TOML to the server. The server
/// does the ranking, so a client never re-implements sharpe/return/max_dd.
#[test]
fn profile_sweep_is_byte_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategy_lab::stand_in);

    let toml = profile_toml("", "\n[paramscan]\nsize = [1.0, 2.0, 3.0]\n");
    let profile = ProfileBacktest::from_toml_str(&toml)
        .expect("profile parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    let local = vike_backtest::search::paramscan::run_paramscan(
        &profile,
        Arc::clone(&store),
        RankMetric::Sharpe,
    )
    .expect("local sweep runs");
    let local_json = serde_json::to_string(&local).expect("local report serializes");

    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote_json =
        client.run_paramscan_profile(&toml, Some("sharpe"), None).expect("remote sweep runs");

    assert_eq!(local_json, remote_json, "local and remote sweep reports must be byte-identical");

    let v: serde_json::Value = serde_json::from_str(&remote_json).unwrap();
    assert_eq!(v["rows"].as_array().map(Vec::len), Some(3), "one row per grid point");
    assert_eq!(v["rank_by"], "sharpe");
    // Rows arrive ALREADY ranked, each carrying its own TradingResults — what the CLI renders.
    assert!(v["rows"][0]["report"]["final_equity"].is_number(), "rows carry server stats: {v}");
}

/// LOCAL == REMOTE for the walk-forward verb, and the split count (read from the profile's
/// `[walkforward]` table server-side) produces exactly that many OOS windows.
#[test]
fn profile_walkforward_is_byte_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategy_lab::stand_in);

    let toml = profile_toml("", "\n[walkforward]\nn_splits = 4\n");
    let profile = ProfileBacktest::from_toml_str(&toml)
        .expect("profile parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    let local = vike_backtest::walkforward::runner::run_walkforward(&profile, Arc::clone(&store))
        .expect("local wf runs");
    let local_json = serde_json::to_string(&local).expect("local report serializes");

    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote_json = client.run_walkforward_profile(&toml).expect("remote wf runs");

    assert_eq!(local_json, remote_json, "local and remote walk-forward reports must match");

    let v: serde_json::Value = serde_json::from_str(&remote_json).unwrap();
    assert_eq!(v["steps"].as_array().map(Vec::len), Some(4), "one step per split");
    assert!(v["equity_curve"].as_array().is_some_and(|c| !c.is_empty()));
}

/// LOCAL == REMOTE for the two programs a profile picks with an answer of its own: the
/// combinatorial walk (`[walkforward]` form (d), over `RunWalkforwardProfile`) and the permutation
/// test (`[permutation]`, over `RunBacktest`). Each answer also leaves one run on the daemon's disk,
/// a `backtest` run whose report is the answer and whose config is the profile it ran.
#[test]
fn the_combinatorial_walk_and_the_permutation_test_are_byte_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let runs = tempfile::tempdir().unwrap();
    let addr = vike_backtesthub::test_support::spawn_server_over_saving_to(
        Arc::clone(&store),
        vike_strategy_lab::stand_in,
        runs.path().to_path_buf(),
    );
    let local_profile = |toml: &str| {
        ProfileBacktest::from_toml_str(toml)
            .expect("profile parses")
            .with_stand_in(vike_strategy_lab::stand_in)
    };
    let mut client = BacktesthubClient::connect(addr).expect("connect");

    let walk = profile_toml("", "\n[walkforward]\ngroups = 4\ntest_groups = 2\n");
    let local = vike_backtest::walkforward::combinatorial::run_walkforward_combinatorial_profile(
        &local_profile(&walk),
        Arc::clone(&store),
        None,
    )
    .expect("local combinatorial walk runs");
    let remote = client.run_combinatorial_walkforward_profile(&walk).expect("remote walk runs");
    assert_eq!(serde_json::to_string(&local).unwrap(), remote);
    let v: serde_json::Value = serde_json::from_str(&remote).unwrap();
    assert_eq!(v["paths"].as_array().map(Vec::len), Some(3), "C(3, 1) paths");

    let test = profile_toml("", "\n[permutation]\npermutations = 5\nseed = 3\n");
    let local = vike_backtest::permutation::run_permutation_profile(
        &local_profile(&test),
        Arc::clone(&store),
        None,
    )
    .expect("local permutation test runs");
    let remote = client.run_permutation_profile(&test).expect("remote test runs");
    assert_eq!(serde_json::to_string(&local).unwrap(), remote);
    let v: serde_json::Value = serde_json::from_str(&remote).unwrap();
    assert_eq!(v["permuted_scores"].as_array().map(Vec::len), Some(5));

    // The two saved runs, one per answer, read back through the one scan every listing uses.
    let scan = vike_runs::scan_runs(runs.path());
    assert!(scan.diagnostics.is_empty(), "{:?}", scan.diagnostics);
    let mut protocols: Vec<String> = Vec::new();
    for run in &scan.runs {
        assert_eq!(run.manifest.kind, vike_runs::BACKTEST_RUN_KIND);
        assert_eq!(run.manifest.config.name.as_deref(), Some("profile_verb_parity"));
        let config = vike_runs::read_config_toml(&run.dir).unwrap();
        assert!(config == walk || config == test, "the config is the profile it ran");
        assert!(run.report.is_some(), "a saved run carries its report");
        protocols.push(run.manifest.detail["protocol"].as_str().unwrap().to_string());
    }
    protocols.sort();
    assert_eq!(protocols, ["combinatorial_walkforward", "permutation_test"]);
}

/// THE CAPABILITY THESE VERBS EXIST FOR: a `[risk]` section — which the DTO-shaped `RunSweep` wire
/// verb cannot carry at all — crosses with the profile and reaches the engine.
///
/// The signal is the one `harness_risk_wiring.rs` established: with a `commission` > 0 a FILLED order
/// strictly moves equity off `cash` (the fee is debited immediately), while a DENIED order leaves it
/// at EXACTLY `cash` — no fill, no fee, no position to mark. So with a `max_notional_per_order` far
/// below the first order's notional every grid point must land exactly on `cash`, and the same
/// profile without `[risk]` must not. A silently-dropped section would make the two runs identical.
#[test]
fn a_risk_section_crosses_the_wire_and_changes_the_sweep() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(store, vike_strategy_lab::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");

    // The commission rides in `[strategy.params]` (`extra` is appended there), ahead of the grid.
    let grid = "commission = 0.1\n\n[paramscan]\nsize = [1.0, 2.0]\n";
    let unrestricted = client
        .run_paramscan_profile(&profile_toml("", grid), None, None)
        .expect("the unrestricted sweep runs");
    let restricted = client
        .run_paramscan_profile(
            &profile_toml("", &format!("{grid}\n[risk]\nmax_notional_per_order = 1.0\n")),
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

/// A profile over [`seeded_bar_store`]'s tape that CLOSES a trade: momentum, its position closed by its three-minute
/// time barrier.
fn trading_profile() -> String {
    r#"
name = "forward_report_backtest"

[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1m"
from = "0"
to = "100000000"

[engine]
cash = 1000.0

[strategy]
file = "trend/momentum.rs"
[strategy.params]
qty = 1.0
time_limit_ms = 180000
venue = "binance"
"#
    .to_string()
}

/// Book chapter 22. A single backtest's trade ledger rides its answer to the saver and NO client reads it: a saving
/// daemon and a non-saving one answer the same bytes, with no `trade_ledger` key, and the saved `report.json` is that
/// answer. The run keeps `trades.json`, the engine's own trades. Then the forward report reads THAT run through the
/// study host's saved runs (`vike_study_runner::study_run::run_study_with`) against fills that entered each signal 30 s late at
/// 1 bp worse: every trade pairs, end to end.
#[test]
fn a_daemon_saved_backtest_keeps_its_trade_ledger_and_the_forward_report_reads_it() {
    let (_dir, store) = seeded_bar_store();
    let runs = tempfile::tempdir().unwrap();
    let saving = vike_backtesthub::test_support::spawn_server_over_saving_to(
        Arc::clone(&store),
        vike_strategy_lab::stand_in,
        runs.path().to_path_buf(),
    );
    let plain = spawn_server_over(Arc::clone(&store), vike_strategy_lab::stand_in);
    let toml = trading_profile();
    let ask = |addr| BacktesthubClient::connect(addr).expect("connect").run_backtest(&toml);
    let answer = ask(saving).expect("the saving daemon answers");
    assert_eq!(
        answer,
        ask(plain).expect("the plain daemon answers"),
        "saving on or off, one answer"
    );
    assert!(!answer.contains("trade_ledger"), "a client never reads the ledger: {answer}");

    let profile = ProfileBacktest::from_toml_str(&toml)
        .expect("profile parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    let local = vike_backtest::run::run_backtest_cancellable(&profile, Arc::clone(&store), None)
        .expect("the local run");
    assert!(
        !local.trades.is_empty(),
        "the profile must close a trade, or the ledger proves nothing"
    );
    let scan = vike_runs::scan_runs(runs.path());
    assert_eq!(scan.runs.len(), 1, "{:?}", scan.diagnostics);
    let run = &scan.runs[0];
    let ledger = vike_runs::read_trades(&run.dir).expect("the saved run keeps its ledger");
    assert_eq!((&ledger.trades, ledger.source_len), (&local.trades, local.trades.len()));
    let report = std::fs::read_to_string(run.dir.join(vike_runs::REPORT_FILE)).unwrap();
    let as_value = |json: &str| serde_json::from_str::<serde_json::Value>(json).unwrap();
    assert_eq!(as_value(&report), as_value(&answer), "report.json is the answer a client read");

    // The forward test: each round trip entered 30 s after the backtest's and left 20 s after it, 1 bp worse
    // both times (an exit before the next entry, whatever the bars did).
    let fill = |n: usize, ts: i64, side: i32, qty: f64, price: f64| vike_data::ExecFillRow {
        ts,
        trade_id: format!("t{n}"),
        order_client_id: format!("c{n}"),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty,
        px: price,
        commission: 0.0,
        mark_price: None,
        liquidity_side: "taker".into(),
        commission_asset: String::new(),
        venue_order_id: None,
        account: None,
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
        mount_id: None,
        received_ms: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
    };
    let mut fills = Vec::new();
    for t in &ledger.trades {
        let side = if t.is_long { 1 } else { -1 };
        let s = f64::from(side);
        let n = fills.len();
        fills.push(fill(
            n,
            t.entry_timestamp + 30_000,
            side,
            t.size,
            t.entry_price * (1.0 + s * 1e-4),
        ));
        fills.push(fill(
            n + 1,
            t.exit_timestamp + 20_000,
            -side,
            t.size,
            t.exit_price * (1.0 - s * 1e-4),
        ));
    }
    // The study reads the account's fills and the symbol's KEPT row (`forward_report` refuses a symbol the host
    // cannot value), which only the datahub's store keeps, so it runs over a double holding both.
    let study_store = vike_data::HistoryStoreMemory::new();
    study_store
        .append_exec_fills("binance", "BTCUSDT", &fills, Some("forward"))
        .expect("the fills");
    study_store.serve_kept_instruments(vec![vike_model::Instrument::new(
        "binance",
        "BTCUSDT",
        "BTC",
        "USDT",
        vike_model::Contract::CryptoSpot,
    )]);

    let recipe: toml::Value = toml::from_str(&format!(
        "backtest_run = \"{}\"\nvenue = \"binance\"\nsymbol = \"BTCUSDT\"\ncapital = 1000.0\n",
        run.manifest.run_id
    ))
    .unwrap();
    let study = vike_study_runner::study_run::run_study_with(
        &user_forward_report::run,
        vike_study_runner::study_run::StudyRunRequest {
            study: "forward_report",
            params: &recipe,
            store: Arc::new(study_store),
            learner: None,
            sim: None,
            window: vike_data::TimestampRange::of(0, 60_000 * 402),
            scratch: runs.path().join("scratch"),
            runs_root: runs.path(),
            produced_by: "test",
            git_sha: None,
            config: vike_runs::RunConfig { path: None, name: None },
            clock: &vike_model::ClockLive,
        },
    )
    .expect("the forward report runs");
    let metric = |name: &str| study.outcome.metric_value(name).unwrap_or(f64::NAN);
    let trades = ledger.trades.len() as f64;
    assert_eq!(
        ["backtest_trades", "forward_trades", "matched_trades", "backtest_only_trades"].map(metric),
        [trades, trades, trades, 0.0]
    );
    assert_eq!(metric("entry_delay_ms"), 30_000.0);
    for slippage in ["entry_slippage_bps", "exit_slippage_bps"] {
        assert!((metric(slippage) - 1.0).abs() < 1e-6, "{slippage}: {}", metric(slippage));
    }
}

/// The gap of 2026-11-17: a backtest the desktop STUDIO starts of a strategy FILE (its Run is a `Summarized`
/// `RunSlice`, its Saved pane's Compare all a plain `RunSlice`) is saved as a run, as `RunBacktest`'s is. A saving
/// daemon and a plain one answer the same bytes. Each saved run is the profile run the slice is: its `config.toml`,
/// run as a profile through `RunBacktest`, answers its `report.json`, and its `trades.json` is the run's own trades.
#[test]
fn a_studio_run_of_a_strategy_file_is_saved_and_answers_the_same_bytes() {
    use vike_backtesthub_client::test_support::btcusdt_1m_bars;
    use vike_backtesthub_client::wire_studio::StrategySpecWire;
    let (_dir, store) = seeded_bar_store();
    let runs = tempfile::tempdir().unwrap();
    let saving = vike_backtesthub::test_support::spawn_server_over_saving_to(
        Arc::clone(&store),
        vike_strategy_lab::stand_in,
        runs.path().to_path_buf(),
    );
    let plain = spawn_server_over(Arc::clone(&store), vike_strategy_lab::stand_in);
    let connect = |addr| BacktesthubClient::connect(addr).expect("connect");
    // `trading_profile`'s strategy, which closes a trade on this tape.
    let spec = || StrategySpecWire::File {
        file: "trend/momentum.rs".into(),
        params_toml: "qty = 1.0\ntime_limit_ms = 180000\nvenue = \"binance\"\n".into(),
        sdk: None,
    };
    let summary = |addr| {
        let mut summary =
            connect(addr).run_slice_summary(spec(), btcusdt_1m_bars(), None).expect("the Run");
        // The name the daemon holds its trades under counts this process's runs; it names no run's content.
        summary.run = String::new();
        // The saved run's id says where a saving daemon kept the run (decision 0078's amendment of 2026-11-18).
        let saved_run = std::mem::take(&mut summary.saved_run);
        (serde_json::to_string(&summary).unwrap(), saved_run)
    };
    let ((saving_summary, saved_run), (plain_summary, unsaved)) = (summary(saving), summary(plain));
    assert_eq!(saving_summary, plain_summary, "saving on or off, one summary");
    assert!(!saved_run.is_empty() && unsaved.is_empty(), "only the saving daemon names a run");
    let whole = |addr| connect(addr).run_slice(spec(), btcusdt_1m_bars(), None).expect("the run");
    let saved_answer = whole(saving);
    assert_eq!(
        serde_json::to_string(&saved_answer).unwrap(),
        serde_json::to_string(&whole(plain)).unwrap(),
        "saving on or off, one answer"
    );
    assert!(
        !saved_answer.trades.is_empty(),
        "the run must close a trade, or the ledger proves nothing"
    );

    let scan = vike_runs::scan_runs(runs.path());
    assert_eq!(scan.runs.len(), 2, "the Run and the whole run: {:?}", scan.diagnostics);
    let as_value = |json: &str| serde_json::from_str::<serde_json::Value>(json).unwrap();
    for run in &scan.runs {
        assert_eq!(run.manifest.kind, vike_runs::BACKTEST_RUN_KIND);
        assert_eq!(run.manifest.detail["protocol"], "single");
        assert_eq!(run.manifest.detail["strategy_file"], "trend/momentum.rs");
        let ledger = vike_runs::read_trades(&run.dir).expect("the saved run keeps its ledger");
        assert_eq!(ledger.trades, saved_answer.trades);
        let profile = vike_runs::read_config_toml(&run.dir).expect("its profile");
        let again = connect(plain).run_backtest(&profile).expect("config.toml runs as a profile");
        let report = std::fs::read_to_string(run.dir.join(vike_runs::REPORT_FILE)).unwrap();
        assert_eq!(
            as_value(&report),
            as_value(&again),
            "report.json is config.toml's RunBacktest answer"
        );
    }
    // The Run's saved run keeps its EVENT LOG in place of its whole curve (decision 0078's amendment of 2026-11-19),
    // and the saving daemon reads the curve the whole run answers back by the saved run's id, rebuilt from it.
    let saved = vike_runs::finished_run_dir(runs.path(), &saved_run).expect("the Run's saved run");
    assert!(vike_runs::read_equity_log(&saved).is_ok(), "a bar run keeps its log");
    assert!(vike_runs::read_equity_curve(&saved).is_err(), "and no curve beside it");
    let bits = |curve: &[f64]| curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    let (equity, timestamp) = connect(saving).read_whole_curve(&saved_run).expect("read by id");
    assert_eq!(bits(&equity), bits(&saved_answer.equity_curve));
    assert_eq!(timestamp, saved_answer.equity_timestamp);
}

/// **`vike-cli backtest run`'s default answer** (decision 0078's amendment of 2026-11-18): a summarized `RunBacktest`
/// is the summary of the very run the plain verb reports (its final equity, trade count and Sharpe to the bit); its
/// whole curve reads back page by page, every thinned point of the summary at its step; and a saving daemon saves it
/// as the plain verb saves a single run, its whole curve kept and the saved run named in the summary.
#[test]
fn a_summarized_run_backtest_is_the_plain_runs_summary_and_keeps_its_whole_curve() {
    let daemons = two_daemons();
    let toml = profile_toml("", "");
    let report: serde_json::Value =
        serde_json::from_str(&connect(daemons.plain).run_backtest(&toml).expect("the plain run"))
            .unwrap();
    let mut client = connect(daemons.saving);
    let summary = client.run_backtest_summary(&toml).expect("the summary");
    let number = |key: &str| report[key].as_f64().map(f64::to_bits);
    assert_eq!(Some(summary.final_equity.to_bits()), number("final_equity"));
    assert_eq!(summary.sharpe.map(f64::to_bits), number("sharpe"));
    assert_eq!(Some(summary.n_trades as u64), report["n_trades"].as_u64());
    assert!(summary.rows.iter().any(|(id, _)| id == "sharpe"), "every metric, rendered");

    let (equity, timestamp) = client.read_whole_curve(&summary.run).expect("the whole curve");
    assert_eq!(equity.len(), summary.curve_len);
    assert!(timestamp.is_empty() || timestamp.len() == equity.len(), "aligned or untracked");
    assert!(summary.curve_len > 1, "the run has a curve to page");
    for &(step, value) in &summary.curve {
        assert_eq!(equity[step].to_bits(), value.to_bits(), "step {step}");
    }

    assert!(!summary.saved_run.is_empty(), "the saving daemon names the run it saved");
    let saved = saved_runs(daemons.runs.path());
    assert_eq!(saved.len(), 1, "one single run");
    let (protocol, config, kept_report) = &saved[0];
    assert_eq!((protocol.as_str(), config.as_str()), ("single", toml.as_str()));
    assert_eq!(kept_report["final_equity"], report["final_equity"]);
    // A bar run keeps its event log, not its curve (decision 0078's amendment of 2026-11-19): a read by the saved
    // run's id rebuilds the curve from it, the held curve to the bit.
    let dir = vike_runs::finished_run_dir(daemons.runs.path(), &summary.saved_run).unwrap();
    let log = vike_runs::read_equity_log(&dir).expect("the event log on disk");
    assert!(vike_runs::read_equity_curve(&dir).is_err(), "no curve beside the log");
    // Its day stamps beside the log, none in the manifest every scan reads (the amendment's day-stamps line).
    let days = vike_runs::read_day_stamps(&dir).expect("the day stamps on disk");
    let manifest = std::fs::read_to_string(dir.join(vike_runs::MANIFEST_FILE)).unwrap();
    assert!(!manifest.contains("\"stamp\""), "no day stamp in the manifest");
    let bits = |curve: &[f64]| curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    let (rebuilt, rebuilt_timestamp) =
        client.read_whole_curve(&summary.saved_run).expect("rebuilt");
    assert_eq!((bits(&rebuilt), &rebuilt_timestamp), (bits(&equity), &timestamp));
    // The same run as a run saved before the log reads it (its curve file, which a reader takes first): the same
    // pages, to the bit.
    let file = vike_runs::RunEquityCurve { equity: equity.clone(), timestamp: timestamp.clone() };
    std::fs::write(dir.join(vike_runs::EQUITY_CURVE_FILE), file.to_bytes()).unwrap();
    let (from_file, from_file_timestamp) =
        client.read_whole_curve(&summary.saved_run).expect("read");
    assert_eq!((bits(&from_file), from_file_timestamp), (bits(&rebuilt), rebuilt_timestamp));
    let steps = equity.len();
    println!(
        "SIZE: {steps} steps, {} log bytes, {} curve bytes, manifest {} bytes, day stamps {} bytes",
        log.len(),
        24 + 16 * steps,
        manifest.len(),
        days.len()
    );
}

/// `dir`'s saved run rewritten to the layout a build from before `vike_runs::DAY_STAMPS_FILE` saved (decision 0078's
/// amendment of 2026-11-19): the recipe under `detail.equity_log`, each series' day stamps inline, and no file beside.
fn to_inline_day_stamps(dir: &std::path::Path) {
    let mut manifest = vike_runs::read_manifest(dir).unwrap();
    let detail = manifest.detail.as_object_mut().unwrap();
    let mut recipe = detail.remove("equity_log_recipe").expect("a run saved now keeps it apart");
    recipe.as_object_mut().unwrap().remove("day_stamps").expect("and names its file");
    let file = vike_runs::read_day_stamps(dir).unwrap();
    let days: Vec<Option<Vec<vike_data::BarDay>>> = rmp_serde::from_slice(&file).unwrap();
    let series = recipe["series"].as_array_mut().unwrap();
    assert_eq!(series.len(), days.len());
    for (series, days) in series.iter_mut().zip(days) {
        assert!(days.as_ref().is_some_and(|d| !d.is_empty()), "a saved log's bars are stamped");
        series["days"] = serde_json::to_value(days).unwrap();
    }
    detail.insert("equity_log".into(), recipe);
    let text = serde_json::to_string_pretty(&manifest).unwrap();
    std::fs::write(dir.join(vike_runs::MANIFEST_FILE), text).unwrap();
    std::fs::remove_file(dir.join(vike_runs::DAY_STAMPS_FILE)).unwrap();
}

/// **A run saved with its day stamps INLINE** (`detail.equity_log`, before `vike_runs::DAY_STAMPS_FILE`) rebuilds
/// its curve as it always did: to the bit of the one a run saved now rebuilds, and refused with the same words naming
/// the restamped day once a bar is rewritten under it. Listing and pruning count it as they count a run saved now.
#[test]
fn a_run_saved_with_inline_day_stamps_rebuilds_its_curve_as_before() {
    let (_dir, store) = seeded_bar_store();
    let runs = tempfile::tempdir().unwrap();
    let saving = vike_backtesthub::test_support::spawn_server_over_saving_to(
        Arc::clone(&store),
        vike_strategy_lab::stand_in,
        runs.path().to_path_buf(),
    );
    let mut client = connect(saving);
    let mut saved = || {
        let summary = client.run_backtest_summary(&profile_toml("", "")).expect("the summary");
        let dir = vike_runs::finished_run_dir(runs.path(), &summary.saved_run).unwrap();
        (summary.saved_run, dir)
    };
    let (now, _) = saved();
    let (inline, inline_dir) = saved();
    let (changed, changed_dir) = saved();
    let common = |scan: vike_runs::RunScan| {
        let rows = scan
            .runs
            .iter()
            .map(|r| (r.run_id.clone(), r.manifest.strategy_file().map(str::to_string)));
        (rows.collect::<Vec<_>>(), scan.diagnostics)
    };
    let listed = common(vike_runs::scan_runs_common(runs.path()));
    to_inline_day_stamps(&inline_dir);
    to_inline_day_stamps(&changed_dir);
    assert_eq!(
        common(vike_runs::scan_runs_common(runs.path())),
        listed,
        "one listing of both layouts"
    );
    assert_eq!(
        common(vike_runs::scan_runs(runs.path())),
        listed,
        "the whole read lists them alike"
    );

    let bits = |curve: &[f64]| curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    let (equity, timestamp) = client.read_whole_curve(&now).expect("a run saved now");
    let (old, old_timestamp) = client.read_whole_curve(&inline).expect("a run saved inline");
    assert_eq!((bits(&old), old_timestamp), (bits(&equity), timestamp), "to the bit");

    let rewritten = Bar::ohlcv(60_000 * 201, 1234.5, 1234.5, 1234.5, 1234.5, 0.0);
    store.append_bars("binance", "BTCUSDT", "1m", &[rewritten], None).unwrap();
    let why = client.read_whole_curve(&changed).unwrap_err();
    assert!(why.contains("not the ones the run marked") && why.contains("restamped"), "{why}");
}

/// **A summary's results ARE its saved report's** (decision 0078's amendment of 2026-11-19, the owner's yes): a
/// summarized `RunBacktest` is summarized over the run's full record with the report its saved `report.json` was
/// written from, so its results table carries that report's honesty block (its denials in the summary's `refused`),
/// zero-trade diagnosis and closed-trade statistics, and every number of it to the bit. A declared warm-up is the
/// counter that shows it: the whole answer's
/// wire record carries none, so a summary of the decoded record read it as zero. A run that trades (its closed trades
/// the block a summary of the decoded record left out), and one whose warm-up swallows its slice (a zero-trade
/// diagnosis that names the warm-up).
#[test]
fn a_summarys_results_are_its_saved_reports_honesty_zero_trade_and_numbers() {
    let daemons = two_daemons();
    for warmup in [None, Some(1000)] {
        let toml = match warmup {
            None => trading_profile(),
            Some(bars) => trading_profile().replace(
                "interval = \"1m\"\n",
                &format!("interval = \"1m\"\nwarmup = \"{bars}bars\"\n"),
            ),
        };
        let summary = connect(daemons.saving).run_backtest_summary(&toml).expect("the summary");
        let dir = vike_runs::finished_run_dir(daemons.runs.path(), &summary.saved_run)
            .expect("the saved run");
        let text = std::fs::read_to_string(dir.join(vike_runs::REPORT_FILE)).unwrap();
        let mut report: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut table = serde_json::to_value(summary.results.as_ref().expect("a results table"))
            .expect("the table encodes");
        // The denials cross ONCE, in `refused` and `refused_detail` (the amendment on the denials, same day): the saved
        // report's own groups, and the table's honesty block holds none of them.
        let empty = || serde_json::Value::Array(Vec::new());
        let held = report["honesty"].as_object_mut().expect("the report's honesty block");
        let denials = held.remove("denials").unwrap_or_else(empty);
        let details = held.remove("denial_details").unwrap_or_else(empty);
        assert_eq!(serde_json::to_value(&summary.refused).unwrap(), denials, "{warmup:?}");
        assert_eq!(serde_json::to_value(&summary.refused_detail).unwrap(), details, "{warmup:?}");
        let none = table["honesty"].as_object_mut().expect("the table's honesty block");
        assert_eq!(none.remove("denials"), Some(empty()), "{warmup:?}: none in the table");
        assert_eq!(table["honesty"], report["honesty"], "{warmup:?}: the honesty block");
        assert_eq!(
            table["zero_trade"], report["zero_trade"],
            "{warmup:?}: the zero-trade diagnosis"
        );
        match warmup {
            None => assert!(report["n_trades"].as_u64() > Some(0), "the profile trades"),
            Some(bars) => {
                let named = report["honesty"]["warmup"].as_u64().expect("a warm-up count");
                assert!(named >= bars, "the report names the warm-up: {named}");
                assert!(table["zero_trade"].is_object(), "and diagnoses the run by it");
            }
        }
        assert!(table["closed_trades"].is_object(), "{warmup:?}: the closed-trade statistics");
        assert_eq!(table["sharpe"], report["sharpe"]);
        // Everything else too: the table is the report without its name, its cost stamp and the closed-trade lists.
        for doc in [&mut report, &mut table] {
            let object = doc.as_object_mut().expect("an object");
            object.remove("name");
            object.remove("realism");
            let closed = object["closed_trades"].as_object_mut().expect("the closed-trade block");
            for list in ["trades", "open_trades", "buy_hold_curve"] {
                closed.remove(list);
            }
        }
        assert_eq!(table, report, "{warmup:?}: the summary's results are the saved report's");
        let bits = |v: &serde_json::Value| v.as_f64().map(f64::to_bits);
        assert_eq!(summary.sharpe.map(f64::to_bits), bits(&report["sharpe"]), "{warmup:?}");
    }
}

/// **A saved bar run's curve is never rebuilt from changed data** (decision 0078's amendment of 2026-11-19): a build whose
/// fold moves the curve, or a bar the store rewrote under the run, is refused in words over the wire, naming what
/// changed; a build that rebuilds the very curve the run answered serves it.
#[test]
fn a_saved_bar_runs_curve_is_refused_over_changed_bars_or_a_moved_fold() {
    let (_dir, store) = seeded_bar_store();
    let runs = tempfile::tempdir().unwrap();
    let saving = vike_backtesthub::test_support::spawn_server_over_saving_to(
        Arc::clone(&store),
        vike_strategy_lab::stand_in,
        runs.path().to_path_buf(),
    );
    let mut client = connect(saving);
    let mut saved = || {
        let summary = client.run_backtest_summary(&profile_toml("", "")).expect("the summary");
        let dir = vike_runs::finished_run_dir(runs.path(), &summary.saved_run).unwrap();
        (summary.saved_run, dir)
    };
    let (same, same_dir) = saved();
    let (moved, moved_dir) = saved();
    let (changed, _) = saved();
    let rewrite = |dir: &std::path::Path, edit: &dyn Fn(&mut serde_json::Value)| {
        let mut manifest = vike_runs::read_manifest(dir).unwrap();
        edit(&mut manifest.detail["equity_log_recipe"]);
        let text = serde_json::to_string_pretty(&manifest).unwrap();
        std::fs::write(dir.join(vike_runs::MANIFEST_FILE), text).unwrap();
    };

    // Another build that rebuilds the same curve serves it; one whose fold moved is refused, naming both builds.
    rewrite(&same_dir, &|recipe: &mut serde_json::Value| recipe["build"] = "0000000".into());
    assert!(client.read_whole_curve(&same).is_ok(), "the curve came back to the bit");
    rewrite(&moved_dir, &|recipe: &mut serde_json::Value| {
        recipe["build"] = "0000000".into();
        recipe["curve_fnv"] = "0000000000000000".into();
    });
    let why = client.read_whole_curve(&moved).unwrap_err();
    assert!(why.contains("saved by build 0000000") && why.contains("Run it again"), "{why}");

    // A bar rewritten under a saved run (the store answers the newest write): refused, naming the series and the day
    // the store restamped.
    let rewritten = Bar::ohlcv(60_000 * 201, 1234.5, 1234.5, 1234.5, 1234.5, 0.0);
    store.append_bars("binance", "BTCUSDT", "1m", &[rewritten], None).unwrap();
    let why = client.read_whole_curve(&changed).unwrap_err();
    assert!(why.contains("not the ones the run marked") && why.contains("restamped"), "{why}");
}

/// A saving and a plain test daemon over [`seeded_bar_store`]'s tape, the saving one under its own temp root.
struct TwoDaemons {
    _dir: TempDir,
    runs: TempDir,
    saving: std::net::SocketAddr,
    plain: std::net::SocketAddr,
}

fn two_daemons() -> TwoDaemons {
    let (dir, store) = seeded_bar_store();
    let runs = tempfile::tempdir().unwrap();
    let saving = vike_backtesthub::test_support::spawn_server_over_saving_to(
        Arc::clone(&store),
        vike_strategy_lab::stand_in,
        runs.path().to_path_buf(),
    );
    let plain = spawn_server_over(store, vike_strategy_lab::stand_in);
    TwoDaemons { _dir: dir, runs, saving, plain }
}

fn connect(addr: std::net::SocketAddr) -> BacktesthubClient {
    BacktesthubClient::connect(addr).expect("connect")
}

/// `trading_profile`'s strategy as the Studio names it.
fn momentum_spec() -> vike_backtesthub_client::wire_studio::StrategySpecWire {
    vike_backtesthub_client::wire_studio::StrategySpecWire::File {
        file: "trend/momentum.rs".into(),
        params_toml: "qty = 1.0\ntime_limit_ms = 180000\nvenue = \"binance\"\n".into(),
        sdk: None,
    }
}

/// Every saved run under `runs`: each one's protocol, `config.toml` and `report.json` as a value.
fn saved_runs(runs: &std::path::Path) -> Vec<(String, String, serde_json::Value)> {
    let scan = vike_runs::scan_runs(runs);
    assert!(scan.diagnostics.is_empty(), "{:?}", scan.diagnostics);
    scan.runs
        .iter()
        .map(|run| {
            assert_eq!(run.manifest.kind, vike_runs::BACKTEST_RUN_KIND);
            let report = std::fs::read_to_string(run.dir.join(vike_runs::REPORT_FILE)).unwrap();
            (
                run.manifest.detail["protocol"].as_str().unwrap().to_string(),
                vike_runs::read_config_toml(&run.dir).unwrap(),
                serde_json::from_str(&report).unwrap(),
            )
        })
        .collect()
}

/// A Studio WALK-FORWARD (whole and summarized) answers the same bytes saving on or off, and is saved exactly as a
/// profile walk-forward is: its `config.toml`, run through `RunWalkforwardProfile` (on the saving daemon, which saves
/// that walk too), answers its `report.json`. Three runs, one report.
#[test]
fn a_studio_walk_forward_is_saved_as_the_profile_walk_it_is_and_answers_the_same_bytes() {
    let daemons = two_daemons();
    let walk = vike_backtesthub_client::wire_studio::WalkforwardWire::fixed(4);
    let whole = |addr| {
        let answer = connect(addr).run_walkforward(momentum_spec(), btcusdt(), walk.clone(), None);
        serde_json::to_string(&answer.expect("the walk")).unwrap()
    };
    assert_eq!(whole(daemons.saving), whole(daemons.plain), "saving on or off, one answer");
    let summary = |addr| {
        let answer =
            connect(addr).run_walkforward_summary(momentum_spec(), btcusdt(), walk.clone(), None);
        serde_json::to_string(&answer.expect("the summarized walk")).unwrap()
    };
    assert_eq!(summary(daemons.saving), summary(daemons.plain), "saving on or off, one summary");

    let studio = saved_runs(daemons.runs.path());
    assert_eq!(studio.len(), 2, "the whole walk and the summarized one");
    let (protocol, config, report) = &studio[0];
    assert_eq!(protocol, "walkforward");
    let again = connect(daemons.saving).run_walkforward_profile(config).expect("config.toml walks");
    assert_eq!(&serde_json::from_str::<serde_json::Value>(&again).unwrap(), report);
    let all = saved_runs(daemons.runs.path());
    assert_eq!(all.len(), 3, "the profile walk is saved too");
    assert!(all.iter().all(|(p, c, r)| p == "walkforward" && c == config && r == report));
}

/// A Studio SCAN (whole and summarized) answers the same bytes saving on or off, and is ONE run per scan whose
/// `report.json` is its table (every point's overrides and report, best first) and whose `config.toml`, run through
/// `RunParamscanProfile` (saved too), answers that table.
#[test]
fn a_studio_scan_is_one_run_holding_its_table_and_answers_the_same_bytes() {
    let daemons = two_daemons();
    let grid = vike_backtesthub_client::wire_studio::ParamscanWire {
        axes: vec![(
            "time_limit_ms".to_string(),
            [120_000, 180_000, 240_000]
                .into_iter()
                .map(vike_model::params::ParamValue::Int)
                .collect(),
        )],
    };
    let whole = |addr| {
        let answer = connect(addr).run_paramscan(momentum_spec(), btcusdt(), grid.clone(), None);
        serde_json::to_string(&answer.expect("the scan")).unwrap()
    };
    assert_eq!(whole(daemons.saving), whole(daemons.plain), "saving on or off, one answer");
    let summary = |addr| {
        let mut summary = connect(addr)
            .run_paramscan_summary(momentum_spec(), btcusdt(), grid.clone(), None)
            .expect("the summarized scan");
        // The name the daemon holds a scan's points under counts this process's runs, not the scan.
        summary.run.clear();
        if let Some(best) = summary.best.as_mut() {
            best.run.clear();
        }
        serde_json::to_string(&summary).unwrap()
    };
    assert_eq!(summary(daemons.saving), summary(daemons.plain), "saving on or off, one summary");

    let studio = saved_runs(daemons.runs.path());
    assert_eq!(studio.len(), 2, "one run per scan, not one per point");
    let (protocol, config, report) = &studio[0];
    assert_eq!(protocol, "paramscan");
    assert_eq!(report["rows"].as_array().map(Vec::len), Some(3), "every point, in one table");
    assert!(report["rows"][0]["report"].is_object(), "the best set's report leads the table");
    let again = connect(daemons.saving).run_paramscan_profile(config, None, None).expect("scans");
    assert_eq!(&serde_json::from_str::<serde_json::Value>(&again).unwrap(), report);
    let all = saved_runs(daemons.runs.path());
    assert_eq!(all.len(), 3, "the profile scan is saved too");
    assert!(all.iter().all(|(p, c, r)| p == "paramscan" && c == config && r == report));
}

/// An EDITOR BUFFER's run answers the same bytes saving on or off, and keeps the buffer's text as the file its profile
/// names (`editor-buffer.rhai`), its sha256 under `detail`: the retention counts it by that name.
#[test]
fn an_editor_buffer_run_is_saved_with_its_text_and_answers_the_same_bytes() {
    let daemons = two_daemons();
    let script = vike_studio_core::test_support::cross_script::LONG_CROSS_SCRIPT;
    let spec = || vike_backtesthub_client::wire_studio::StrategySpecWire::Rhai(script.to_string());
    let whole = |addr| {
        let answer = connect(addr).run_slice(spec(), btcusdt(), None).expect("the buffer runs");
        serde_json::to_string(&answer).unwrap()
    };
    assert_eq!(whole(daemons.saving), whole(daemons.plain), "saving on or off, one answer");

    let scan = vike_runs::scan_runs(daemons.runs.path());
    assert_eq!(scan.runs.len(), 1, "{:?}", scan.diagnostics);
    let run = &scan.runs[0];
    let buffer = vike_backtesthub::studio_run::EDITOR_BUFFER_FILE;
    assert_eq!(run.manifest.detail["protocol"], "single");
    assert_eq!(run.manifest.strategy_file(), Some(buffer), "counted by the buffer's name");
    let sha = vike_backtest::run::StrategySource::of(script.to_string()).sha256;
    assert_eq!(run.manifest.detail["strategy_sha256"], sha.as_str());
    assert_eq!(std::fs::read_to_string(run.dir.join(buffer)).unwrap(), script);
    assert!(vike_runs::read_config_toml(&run.dir).unwrap().contains(buffer));
}

fn btcusdt() -> vike_backtesthub_client::wire_studio::DataSliceWire {
    vike_backtesthub_client::test_support::btcusdt_1m_bars()
}

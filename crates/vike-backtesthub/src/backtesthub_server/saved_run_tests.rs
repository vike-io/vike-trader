use super::*;
use vike_backtesthub_client::wire_studio::StrategySpecWire;

/// A clock frozen at `secs`.
fn at(secs: i64) -> impl Fn() -> i64 {
    move || secs * 1000
}

const PROFILE: &str = "name = \"perm\"\n[permutation]\npermutations = 3\n";

/// A permutation answer leaves one run: kind `backtest`, its protocol under `detail`, the profile as
/// `config.toml`, the answer as `report.json`, and the manifest that marks it finished.
#[test]
fn a_permutation_answer_is_saved_as_a_backtest_run_of_its_protocol() {
    let runs = tempfile::tempdir().unwrap();
    let request = Request::RunBacktest(PROFILE.to_string());
    let answer = r#"{"trained":true,"p_value":0.25,"permuted_scores":[0.1,0.2,0.3]}"#;
    let response = Response::PermutationReport(answer.to_string());
    let back = save(
        response,
        None,
        start(&request, &at(1_800_000_000)),
        runs.path(),
        &at(1_800_000_060),
        None,
    );
    assert!(matches!(back, Response::PermutationReport(ref json) if json == answer));

    let scan = vike_runs::scan_runs(runs.path());
    assert_eq!(scan.runs.len(), 1, "{:?}", scan.diagnostics);
    let run = &scan.runs[0];
    assert_eq!(run.manifest.kind, BACKTEST_RUN_KIND);
    assert_eq!(run.manifest.detail["protocol"], "permutation_training");
    assert_eq!(run.manifest.config.name.as_deref(), Some("perm"));
    assert_eq!(run.manifest.started_at, utc_rfc3339(1_800_000_000));
    assert_eq!(run.manifest.finished_at, utc_rfc3339(1_800_000_060));
    assert_eq!(vike_runs::read_config_toml(&run.dir).unwrap(), PROFILE);
    let report: Value =
        serde_json::from_str(&std::fs::read_to_string(run.dir.join("report.json")).unwrap())
            .unwrap();
    assert_eq!(report, serde_json::from_str::<Value>(answer).unwrap());
}

/// Every walk-forward answer is saved under its own protocol (the owner's word: every backtest is a run), and so is a
/// scan's table; a walk's data plan (a `Report` to a walk), an error and a request that carries no profile save nothing.
#[test]
fn every_walk_and_scan_answer_is_saved_and_a_plan_or_an_error_is_not() {
    let runs = tempfile::tempdir().unwrap();
    let walk = Request::RunWalkforwardProfile { profile_toml: "[walkforward]\n".to_string() };
    let scan_request = Request::RunParamscanProfile {
        profile_toml: "[paramscan]\nsize = [1.0, 2.0]\n".to_string(),
        rank_by: None,
        search: None,
    };
    let clock = at(1_800_000_000);
    for response in [
        Response::CombinatorialWalkforwardReport("{\"paths\":[]}".to_string()),
        Response::WalkforwardReport(
            "{\"steps\":[],\"overall\":{\"strategy_file\":\"trend/sma_cross.rs\"}}".into(),
        ),
        Response::Report("{\"sharpe\":1.0}".to_string()),
        Response::Error("refused".to_string()),
    ] {
        save(response, None, start(&walk, &clock), runs.path(), &clock, None);
    }
    let table = "{\"rows\":[],\"strategy_file\":\"trend/sma_cross.rs\"}";
    save(
        Response::ParamscanReport(table.to_string()),
        None,
        start(&scan_request, &clock),
        runs.path(),
        &clock,
        None,
    );
    save(
        Response::PermutationReport("{}".to_string()),
        None,
        start(&Request::Ping, &clock),
        runs.path(),
        &clock,
        None,
    );

    let scan = vike_runs::scan_runs(runs.path());
    let mut saved: Vec<(String, Option<String>)> = scan
        .runs
        .iter()
        .map(|run| {
            let detail = &run.manifest.detail;
            let file = detail.get("strategy_file").and_then(Value::as_str).map(str::to_string);
            (detail["protocol"].as_str().unwrap().to_string(), file)
        })
        .collect();
    saved.sort();
    let file = Some("trend/sma_cross.rs".to_string());
    assert_eq!(
        saved,
        [
            ("combinatorial_walkforward".to_string(), None),
            ("paramscan".to_string(), file.clone()),
            ("walkforward".to_string(), file),
        ],
        "{:?}",
        scan.diagnostics
    );
    assert!(
        scan.runs.iter().all(|run| run.manifest.config.name.is_none()),
        "no name, none recorded"
    );
}

/// A report over a two-sample curve, with the strategy file named, as `verbs::run_backtest` builds one.
fn a_report() -> TradingResults {
    let result = vike_analytics::TradingRecord {
        equity_curve: vec![1_000.0, 1_010.0],
        final_equity: 1_010.0,
        ..Default::default()
    };
    TradingResults::from_record(Some("sma".to_string()), &result, 365.0).with_strategy(
        "trend/sma_cross.rs",
        &"ab".repeat(32),
        None,
    )
}

fn forecasts() -> RunForecasts {
    let row = |timestamp| vike_model::ForecastRow {
        timestamp,
        symbol: "BTCUSDT".to_string(),
        forecast: vike_model::Forecast::new(0.01, 86_400_000, None).unwrap(),
    };
    RunForecasts::bounded(&[row(1), row(2)]).unwrap()
}

/// Book W35, the owner's word: a single backtest's answer is saved as a run, its forecasts taken out of the report
/// into `forecasts.json`, the report written as the plain report, and the strategy file and its sha256 named in
/// `detail`. The answer goes back unchanged, forecasts and all.
#[test]
fn a_single_backtest_is_saved_with_its_forecasts_in_their_own_document() {
    let runs = tempfile::tempdir().unwrap();
    let profile = "name = \"sma\"\n[strategy]\nfile = \"trend/sma_cross.rs\"\n";
    let request = Request::RunBacktest(profile.to_string());
    let answer = report_answer(&a_report(), Some(forecasts())).unwrap();
    assert!(answer.ends_with(r#""source_len":2}}"#), "the forecasts ride LAST: {answer}");
    let back = save(
        Response::Report(answer.clone()),
        None,
        start(&request, &at(1_800_000_000)),
        runs.path(),
        &at(1_800_000_009),
        None,
    );
    assert!(matches!(back, Response::Report(ref json) if *json == answer));

    let scan = vike_runs::scan_runs(runs.path());
    assert_eq!(scan.runs.len(), 1, "{:?}", scan.diagnostics);
    let run = &scan.runs[0];
    assert_eq!(run.manifest.kind, BACKTEST_RUN_KIND);
    assert_eq!(run.manifest.detail["protocol"], "single");
    assert_eq!(run.manifest.detail["strategy_file"], "trend/sma_cross.rs");
    assert_eq!(run.manifest.detail["strategy_sha256"], "ab".repeat(32));
    assert_eq!(vike_runs::read_config_toml(&run.dir).unwrap(), profile);
    assert_eq!(vike_runs::read_forecasts(&run.dir).unwrap().forecasts, forecasts().forecasts);
    let report: Value =
        serde_json::from_str(&std::fs::read_to_string(run.dir.join("report.json")).unwrap())
            .unwrap();
    let plain: Value = serde_json::from_str(&serde_json::to_string(&a_report()).unwrap()).unwrap();
    assert_eq!(report, plain, "report.json is the report, the forecasts are not in it");
}

/// A run that published none answers the bytes it always did and saves no `forecasts.json`; a data plan
/// (`[data] explain = true`) is a `Report` too and saves nothing.
#[test]
fn a_quiet_backtest_answers_its_old_bytes_and_a_data_plan_is_no_run() {
    let report = a_report();
    assert_eq!(report_answer(&report, None).unwrap(), serde_json::to_string(&report).unwrap());

    let runs = tempfile::tempdir().unwrap();
    let clock = at(1_800_000_000);
    let quiet = Request::RunBacktest("name = \"q\"\n".to_string());
    let json = report_answer(&report, None).unwrap();
    save(Response::Report(json.clone()), None, start(&quiet, &clock), runs.path(), &clock, None);
    let plan = Request::RunBacktest("[data]\nexplain = true\n".to_string());
    let plan_answer = Response::Report("{\"plan\":[]}".to_string());
    save(plan_answer, None, start(&plan, &clock), runs.path(), &clock, None);

    let scan = vike_runs::scan_runs(runs.path());
    assert_eq!(scan.runs.len(), 1, "the backtest, not the plan: {:?}", scan.diagnostics);
    assert!(!scan.runs[0].dir.join(vike_runs::FORECASTS_FILE).exists(), "no forecast, no file");
}

/// A two-trade ledger, the second closed short.
fn ledger() -> RunTrades {
    let trade = |entry: i64, is_long: bool, pnl: f64| vike_model::Trade {
        entry_price: 100.0,
        exit_price: 100.0 + pnl,
        size: 1.5,
        pnl,
        fees: 0.03,
        entry_timestamp: entry,
        exit_timestamp: entry + 60_000,
        symbol: "BTCUSDT".to_string(),
        mae: 0.0,
        mfe: 0.0,
        is_long,
    };
    RunTrades {
        schema: vike_runs::TRADES_SCHEMA,
        trades: vec![trade(0, true, 2.5), trade(120_000, false, -0.125)],
        source_len: 2,
    }
}

/// Book chapter 22: the ledger rides a single backtest's answer LAST and the one strip gives back EXACTLY the
/// answer it was added to (with forecasts and without), so a client reads today's bytes; the ledger text read back
/// is the ledger. An answer that carries none, and every other answer, pass the strip untouched.
#[test]
fn the_trade_ledger_strip_gives_back_the_answer_byte_for_byte() {
    for forecasts in [None, Some(forecasts())] {
        let answer = report_answer(&a_report(), forecasts).unwrap();
        let carried = with_trade_ledger(answer.clone(), &ledger()).unwrap();
        assert!(carried.ends_with("\"source_len\":2}}"), "the ledger rides LAST: {carried}");
        let (back, text) = take_trade_ledger(Response::Report(carried));
        assert!(matches!(back, Response::Report(ref json) if *json == answer), "{back:?}");
        let read: RunTrades = serde_json::from_str(&text.expect("the ledger")).unwrap();
        assert_eq!((read.trades, read.source_len), (ledger().trades, 2));
    }
    let plain = serde_json::to_string(&a_report()).unwrap();
    let (back, text) = take_trade_ledger(Response::Report(plain.clone()));
    assert!(matches!(back, Response::Report(ref json) if *json == plain) && text.is_none());
    let (back, text) = take_trade_ledger(Response::PermutationReport("{\"p\":1}".into()));
    assert!(
        matches!(back, Response::PermutationReport(ref j) if j == "{\"p\":1}") && text.is_none()
    );
    assert_eq!(
        with_trade_ledger("{}".to_string(), &ledger()).unwrap(),
        "{}",
        "nothing to carry it in"
    );
}

/// The saver writes the ledger it is handed as the single run's `trades.json`, and the validation answers keep none.
#[test]
fn a_single_backtest_saves_the_ledger_it_carried_as_trades_json() {
    let runs = tempfile::tempdir().unwrap();
    let clock = at(1_800_000_000);
    let request = Request::RunBacktest("name = \"sma\"\n".to_string());
    let answer = report_answer(&a_report(), None).unwrap();
    let text = serde_json::to_string(&ledger()).unwrap();
    let back = save(
        Response::Report(answer.clone()),
        Some(text.clone()),
        start(&request, &clock),
        runs.path(),
        &clock,
        None,
    );
    assert!(matches!(back, Response::Report(ref json) if *json == answer));
    let walk = Request::RunWalkforwardProfile { profile_toml: "[walkforward]\n".to_string() };
    let combinatorial = Response::CombinatorialWalkforwardReport("{\"paths\":[]}".to_string());
    save(combinatorial, Some(text), start(&walk, &clock), runs.path(), &clock, None);

    let scan = vike_runs::scan_runs(runs.path());
    assert_eq!(scan.runs.len(), 2, "{:?}", scan.diagnostics);
    for run in &scan.runs {
        let kept = vike_runs::read_trades(&run.dir);
        match run.manifest.detail["protocol"].as_str() {
            Some("single") => assert_eq!(kept.unwrap().trades, ledger().trades),
            _ => assert!(matches!(kept, Err(vike_runs::RunReadError::Missing { .. }))),
        }
    }
}

/// `config.saved_runs_per_strategy`: after three saved single backtests of ONE strategy file, a retention of 2 removes
/// the oldest, so the key this saver writes under `detail` is the one the rule counts by. The combinatorial walk-forward
/// names no strategy file and is never counted.
#[test]
fn the_retention_counts_the_runs_this_saver_writes_and_removes_the_oldest() {
    let runs = tempfile::tempdir().unwrap();
    let request = Request::RunBacktest("name = \"sma\"\n".to_string());
    let answer = report_answer(&a_report(), None).unwrap();
    for secs in [1_800_000_000, 1_800_000_100, 1_800_000_200] {
        let clock = at(secs);
        save(
            Response::Report(answer.clone()),
            None,
            start(&request, &clock),
            runs.path(),
            &clock,
            None,
        );
    }
    let walk = Request::RunWalkforwardProfile { profile_toml: "[walkforward]\n".to_string() };
    let clock = at(1_700_000_000);
    let combinatorial = Response::CombinatorialWalkforwardReport("{\"paths\":[]}".to_string());
    save(combinatorial, None, start(&walk, &clock), runs.path(), &clock, None);
    let oldest = vike_runs::scan_runs(runs.path())
        .runs
        .into_iter()
        .find(|run| run.manifest.finished_at == utc_rfc3339(1_800_000_000))
        .expect("the first single run was saved")
        .run_id;

    let retention = vike_runs::RunRetention {
        keep_per_strategy: 2,
        // No mark store: nothing is marked.
        marks_root: runs.path().join("absent-marks"),
        referencing_documents: Vec::new(),
    };
    prune(&retention, runs.path());

    let scan = vike_runs::scan_runs(runs.path());
    assert_eq!(scan.runs.len(), 3, "{:?}", scan.diagnostics);
    assert!(scan.runs.iter().all(|run| run.run_id != oldest), "{oldest} was not removed");
    assert_eq!(
        scan.runs.iter().filter(|run| run.manifest.detail["protocol"] == "single").count(),
        2
    );
}

/// REVIEW-LIVE-MONEY H1: below the optional writers' free-space floor (`vike_boot::disk_floor`, here over a FAKE
/// reader of a disk with 1 GiB of 100 free) a single backtest is answered with the same bytes and NOTHING is written
/// under the runs root; with room, the same answer is saved as before.
#[test]
fn below_the_free_space_floor_the_answer_returns_and_nothing_is_saved() {
    let runs = tempfile::tempdir().unwrap();
    let clock = at(1_800_000_000);
    let request = Request::RunBacktest("name = \"sma\"\n".to_string());
    let answer = report_answer(&a_report(), None).unwrap();
    let floor = vike_boot::disk_floor::DiskFloor::of_mib(10_240);
    let disk = |available: u64| {
        move |_: &Path| {
            Ok::<_, String>(vike_boot::disk_floor::FilesystemSpace {
                available_bytes: available,
                total_bytes: 100 << 30,
            })
        }
    };
    let full = floor.refusal_with(runs.path(), disk(1 << 30));
    assert!(full.is_some(), "the premise: 1 GiB free is below a 10 GiB floor");
    let back = save(
        Response::Report(answer.clone()),
        None,
        start(&request, &clock),
        runs.path(),
        &clock,
        full,
    );
    assert!(
        matches!(back, Response::Report(ref json) if *json == answer),
        "the answer is untouched"
    );
    assert_eq!(std::fs::read_dir(runs.path()).unwrap().count(), 0, "nothing under the runs root");

    let room = floor.refusal_with(runs.path(), disk(50 << 30));
    assert_eq!(room, None);
    save(Response::Report(answer), None, start(&request, &clock), runs.path(), &clock, room);
    assert_eq!(vike_runs::scan_runs(runs.path()).runs.len(), 1, "with room it is saved");
}

/// A Studio `RunSlice` of `spec` over the test slice.
fn studio_slice(spec: StrategySpecWire) -> Request {
    Request::RunSlice {
        spec,
        slice: Box::new(vike_backtesthub_client::test_support::btcusdt_1m_bars()),
        params: None,
    }
}

fn sma_file() -> StrategySpecWire {
    StrategySpecWire::File {
        file: "trend/sma_cross.rs".into(),
        params_toml: String::new(),
        sdk: None,
    }
}

/// What a Studio run of the strategy file leaves: its profile, and `RunBacktest`'s answer for it, ledger LAST.
fn studio_run() -> StudioRun {
    let answer = report_answer(&a_report(), Some(forecasts())).unwrap();
    StudioRun {
        profile_toml: "[strategy]\nfile = \"trend/sma_cross.rs\"\n".to_string(),
        answer: Response::Report(with_trade_ledger(answer, &ledger()).unwrap()),
        source: None,
        equity_log: None,
    }
}

fn studio_scan(spec: StrategySpecWire) -> Request {
    Request::RunParamscan {
        spec,
        slice: Box::new(vike_backtesthub_client::test_support::btcusdt_1m_bars()),
        paramscan: vike_backtesthub_client::wire_studio::ParamscanWire { axes: Vec::new() },
        params: None,
    }
}

/// Every Studio run may be saved, summarized (the Studio's Run, Sweep and Walk-Forward) or not (the Saved pane's
/// Compare all), of a strategy file or of an editor buffer; only a `RunSlice` answers a single run's `Report`. A
/// request that runs nothing is no run.
#[test]
fn every_studio_run_is_one_that_may_be_saved() {
    let clock = at(1_800_000_000);
    let buffer = StrategySpecWire::Rhai("fn on_bar() {}".into());
    let walk = Request::RunWalkforward {
        spec: sma_file(),
        slice: Box::new(vike_backtesthub_client::test_support::btcusdt_1m_bars()),
        walkforward: vike_backtesthub_client::wire_studio::WalkforwardWire::fixed(3),
        params: None,
    };
    for (request, single) in [
        (studio_slice(sma_file()), true),
        (studio_slice(buffer.clone()), true),
        (studio_scan(sma_file()), false),
        (studio_scan(buffer), false),
        (walk, false),
    ] {
        for request in [Request::Summarized(Box::new(request.clone())), request] {
            let start = start(&request, &clock).expect("a Studio run may be saved");
            assert_eq!(start.single, single, "{request:?}");
            assert!(start.profile_toml.is_empty(), "its profile comes with the run");
        }
    }
    for request in
        [Request::Ping, Request::ListStrategies, Request::Summarized(Box::new(Request::Ping))]
    {
        assert!(start(&request, &clock).is_none(), "{request:?}");
    }
}

/// A Studio run is saved as the profile run it is, through the one saver: kind `backtest`, `single`, the file and
/// its sha256 under `detail` (the key the retention counts by), its profile as `config.toml`, the plain report, its
/// forecasts and its trade ledger in their own documents. With saving off nothing is written.
#[test]
fn a_studio_run_is_saved_as_the_profile_run_it_left() {
    let runs = tempfile::tempdir().unwrap();
    let request = Request::Summarized(Box::new(studio_slice(sma_file())));
    save_studio(
        studio_run(),
        None,
        None,
        start(&request, &at(1_800_000_000)),
        runs.path(),
        &at(1_800_000_009),
        None,
    );

    let scan = vike_runs::scan_runs(runs.path());
    assert_eq!(scan.runs.len(), 1, "{:?}", scan.diagnostics);
    let run = &scan.runs[0];
    assert_eq!(run.manifest.kind, BACKTEST_RUN_KIND);
    assert_eq!(run.manifest.detail["protocol"], "single");
    assert_eq!(run.manifest.detail["strategy_file"], "trend/sma_cross.rs");
    assert_eq!(run.manifest.detail["strategy_sha256"], "ab".repeat(32));
    assert_eq!(run.manifest.started_at, utc_rfc3339(1_800_000_000));
    assert_eq!(vike_runs::read_config_toml(&run.dir).unwrap(), studio_run().profile_toml);
    assert_eq!(vike_runs::read_forecasts(&run.dir).unwrap().forecasts, forecasts().forecasts);
    assert_eq!(vike_runs::read_trades(&run.dir).unwrap().trades, ledger().trades);
    let report: Value =
        serde_json::from_str(&std::fs::read_to_string(run.dir.join("report.json")).unwrap())
            .unwrap();
    let plain: Value = serde_json::from_str(&serde_json::to_string(&a_report()).unwrap()).unwrap();
    assert_eq!(report, plain, "report.json is the report alone");

    save_studio(studio_run(), None, None, None, runs.path(), &at(1_800_000_100), None);
    assert_eq!(vike_runs::scan_runs(runs.path()).runs.len(), 1, "saving off writes nothing");
}

/// **A summarized single run keeps its whole curve in the run it is saved as** (decision 0078's amendment of
/// 2026-11-18), a summarized `RunBacktest` as a Studio Run does: `vike_runs::EQUITY_CURVE_FILE` reads back as the curve
/// handed to the saver, the saver answers the run's id and [`with_saved_run`] writes it into the summary. A scan keeps
/// no whole curve even when one is handed.
#[test]
fn a_summarized_single_run_is_saved_with_its_whole_curve_and_named_in_its_summary() {
    let runs = tempfile::tempdir().unwrap();
    let clock = at(1_800_000_000);
    let curve = RunEquityCurve {
        equity: vec![1000.0, 1000.5, 999.25],
        timestamp: vec![0, 60_000, 120_000],
    };
    let request = Request::Summarized(Box::new(Request::RunBacktest(
        "[strategy]\nfile = \"trend/sma_cross.rs\"\n".to_string(),
    )));
    let saved = save_studio(
        studio_run(),
        Some(&curve),
        None,
        start(&request, &clock),
        runs.path(),
        &clock,
        None,
    )
    .expect("saved");
    let dir = vike_runs::finished_run_dir(runs.path(), &saved).expect("a finished run");
    assert_eq!(vike_runs::read_equity_curve(&dir).unwrap(), curve);
    assert_eq!(vike_runs::read_trades(&dir).unwrap().trades, ledger().trades);

    let summary = vike_backtesthub_client::summary::RunSummaryWire::default();
    let named = with_saved_run(Response::RunSummary(Box::new(summary)), Some(saved.clone()));
    assert!(matches!(named, Response::RunSummary(s) if s.saved_run == saved));
    assert!(matches!(with_saved_run(Response::Pong, Some(saved)), Response::Pong));

    let scanned = save_studio(
        scan_run_for_curve(),
        Some(&curve),
        None,
        start(&studio_scan(sma_file()), &clock),
        runs.path(),
        &clock,
        None,
    )
    .expect("saved");
    let dir = vike_runs::finished_run_dir(runs.path(), &scanned).expect("a finished run");
    assert!(vike_runs::read_equity_curve(&dir).is_err(), "a scan keeps no whole curve");
}

/// A two-series recipe of a curve hashing `fnv`, each series stamped over `days` UTC days (the second series' last day
/// provisional, and a stamp past `i64`'s range, so the round trip is tested where JSON and msgpack differ).
fn stamped_recipe(fnv: String, days: i64) -> vike_backtest::run::equity_log::EquityLogRecipe {
    use vike_backtest::run::equity_log::{EquityLogRecipe, EquityLogSeries};
    let series = |symbol: &str, last_provisional: bool| EquityLogSeries {
        venue: "binance".into(),
        symbol: symbol.into(),
        source: None,
        interval: "1m".into(),
        from: Some(0),
        to: None,
        splits: Vec::new(),
        first_bar: 0,
        last_bar: (days - 1) * 86_400_000,
        closes_fnv: "00000000000000ff".into(),
        days: Some(
            (0..days)
                .map(|day| BarDay {
                    day: 20_000 + day,
                    stamp: u64::MAX - day as u64,
                    provisional: last_provisional && day == days - 1,
                })
                .collect(),
        ),
    };
    EquityLogRecipe {
        format: 1,
        build: Some("aaaaaaa".into()),
        steps: 3,
        curve_fnv: fnv,
        cash: 1000.0,
        currency: "USD".into(),
        decide: "sequential".into(),
        symbols: vec!["BTCUSDT".into(), "ETHUSDT".into()],
        values: vec![vike_model::ContractValue::linear(1.0); 2],
        series: vec![series("BTCUSDT", false), series("ETHUSDT", true)],
    }
}

/// **A summarized single bar run keeps its EVENT LOG in the curve's place** (decision 0078's amendment of
/// 2026-11-19), its recipe as the manifest's `detail.equity_log_recipe` and its day stamps as
/// `vike_runs::DAY_STAMPS_FILE` (the amendment's second line of that day: no stamp in the manifest), when that log
/// rebuilds the very curve the run is held with ([`curve_fnv`]); a log that names another curve is not trusted, and
/// the run keeps its curve.
#[test]
fn a_bar_run_saves_its_event_log_in_place_of_the_curve_it_rebuilds() {
    use vike_backtest::run::equity_log::{EquityLog, curve_fnv};
    let runs = tempfile::tempdir().unwrap();
    let clock = at(1_800_000_000);
    let curve = RunEquityCurve {
        equity: vec![1000.0, 1000.5, 999.25],
        timestamp: vec![0, 60_000, 120_000],
    };
    let log = |fnv: String| EquityLog {
        recipe: stamped_recipe(fnv, 184),
        events: b"vikeelg1\0\0\0\0\0\0\0\0".to_vec(),
    };
    let request = Request::Summarized(Box::new(Request::RunBacktest(
        "[strategy]\nfile = \"trend/sma_cross.rs\"\n".to_string(),
    )));
    let kept = log(curve_fnv(&curve.equity, &curve.timestamp));
    let run = StudioRun { equity_log: Some(kept.clone()), ..studio_run() };
    let saved =
        save_studio(run, Some(&curve), None, start(&request, &clock), runs.path(), &clock, None)
            .expect("saved");
    let dir = vike_runs::finished_run_dir(runs.path(), &saved).expect("a finished run");
    assert_eq!(vike_runs::read_equity_log(&dir).unwrap(), kept.events);
    assert!(vike_runs::read_equity_curve(&dir).is_err(), "the log, in the curve's place");
    let manifest = vike_runs::read_manifest(&dir).unwrap();
    assert_eq!(saved_recipe(&dir, &manifest.detail).unwrap(), kept.recipe, "every stamp, back");
    assert_eq!(manifest.detail["protocol"], "single");
    let text = std::fs::read_to_string(dir.join(vike_runs::MANIFEST_FILE)).unwrap();
    assert!(!text.contains("\"stamp\"") && manifest.detail.get(INLINE_RECIPE_KEY).is_none());
    assert_eq!(manifest.detail[EQUITY_LOG_RECIPE_KEY]["day_stamps"]["count"], 2 * 184);
    let sidecar = vike_runs::read_day_stamps(&dir).unwrap();
    println!("SIZE: manifest {} bytes, day stamps {} bytes", text.len(), sidecar.len());

    // Not this run's day stamps (another run's file, cut short, or a manifest whose count was edited): refused.
    std::fs::write(dir.join(vike_runs::DAY_STAMPS_FILE), &sidecar[..sidecar.len() - 1]).unwrap();
    let why = saved_recipe(&dir, &manifest.detail).unwrap_err();
    assert!(why.contains("day_stamps.bin hashes"), "{why}");
    std::fs::write(dir.join(vike_runs::DAY_STAMPS_FILE), &sidecar).unwrap();
    let mut edited = manifest.detail.clone();
    edited[EQUITY_LOG_RECIPE_KEY]["day_stamps"]["count"] = 1.into();
    let why = saved_recipe(&dir, &edited).unwrap_err();
    assert!(why.contains("holds 368 day stamps of 2 series"), "{why}");
    std::fs::remove_file(dir.join(vike_runs::DAY_STAMPS_FILE)).unwrap();
    let why = saved_recipe(&dir, &manifest.detail).unwrap_err();
    assert!(why.contains("no day_stamps.bin"), "{why}");

    let other = StudioRun { equity_log: Some(log("0".repeat(16))), ..studio_run() };
    let saved =
        save_studio(other, Some(&curve), None, start(&request, &clock), runs.path(), &clock, None)
            .expect("saved");
    let dir = vike_runs::finished_run_dir(runs.path(), &saved).expect("a finished run");
    assert_eq!(
        vike_runs::read_equity_curve(&dir).unwrap(),
        curve,
        "an untrusted log keeps the curve"
    );
    assert!(vike_runs::read_equity_log(&dir).is_err());
    assert!(vike_runs::read_day_stamps(&dir).is_err());
    let detail = vike_runs::read_manifest(&dir).unwrap().detail;
    assert!(detail.get(EQUITY_LOG_RECIPE_KEY).is_none() && detail.get(INLINE_RECIPE_KEY).is_none());
}

/// **A `.rs` file's run held as its EVENT LOG** (its child pinned the bars and sent no curve: decision 0078's
/// amendment of 2026-11-19 on what the hub holds) is saved with that log, its recipe and day stamps, and no curve
/// file; a run's own log with no curve held (a run that was not summarized) keeps neither file, as before.
#[test]
fn a_run_held_as_its_log_is_saved_with_it_and_a_log_beside_no_curve_is_not() {
    use vike_backtest::run::equity_log::EquityLog;
    let runs = tempfile::tempdir().unwrap();
    let clock = at(1_800_000_000);
    let held = EquityLog {
        recipe: stamped_recipe("0123456789abcdef".into(), 31),
        events: b"vikeelg1\0\0\0\0\0\0\0\0".to_vec(),
    };
    let summarized = start(&Request::Summarized(Box::new(studio_slice(sma_file()))), &clock);
    let saved = save_studio(studio_run(), None, Some(&held), summarized, runs.path(), &clock, None)
        .expect("saved");
    let dir = vike_runs::finished_run_dir(runs.path(), &saved).expect("a finished run");
    assert_eq!(vike_runs::read_equity_log(&dir).unwrap(), held.events);
    assert!(vike_runs::read_equity_curve(&dir).is_err(), "no curve came up, none is written");
    let manifest = vike_runs::read_manifest(&dir).unwrap();
    assert_eq!(saved_recipe(&dir, &manifest.detail).unwrap(), held.recipe, "every stamp, back");

    let unsummarized = StudioRun { equity_log: Some(held), ..studio_run() };
    let plain = start(&studio_slice(sma_file()), &clock);
    let saved =
        save_studio(unsummarized, None, None, plain, runs.path(), &clock, None).expect("saved");
    let dir = vike_runs::finished_run_dir(runs.path(), &saved).expect("a finished run");
    assert!(vike_runs::read_equity_log(&dir).is_err(), "a log beside no curve is not kept");
    assert!(vike_runs::read_equity_curve(&dir).is_err());
}

/// **A summarized `RunBacktest`'s `config.toml` is the profile this process read off the request**, not the copy its
/// run (a sandboxed child) sent up: a curve rebuild reads that file. A Studio run's is the one its run built.
#[test]
fn a_profile_runs_config_is_the_requests_profile_not_the_runs_copy() {
    let runs = tempfile::tempdir().unwrap();
    let clock = at(1_800_000_000);
    let sent = "[strategy]\nfile = \"trend/sma_cross.rs\"\n";
    let request = Request::Summarized(Box::new(Request::RunBacktest(sent.to_string())));
    let copy = StudioRun { profile_toml: format!("{sent}# the run's own copy\n"), ..studio_run() };
    let saved = save_studio(copy, None, None, start(&request, &clock), runs.path(), &clock, None)
        .expect("saved");
    let dir = vike_runs::finished_run_dir(runs.path(), &saved).expect("a finished run");
    assert_eq!(vike_runs::read_config_toml(&dir).unwrap(), sent);
}

/// **A run saved with its day stamps INLINE** (`detail.equity_log`, before `vike_runs::DAY_STAMPS_FILE`) reads its
/// recipe exactly as it always did, with no file beside it; and the same recipe saved now reads back equal, so the two
/// layouts hand a rebuild one recipe. Nothing rewrites the older run.
#[test]
fn a_run_saved_with_its_day_stamps_inline_reads_the_recipe_it_always_did() {
    let recipe = stamped_recipe("0123456789abcdef".into(), 31);
    let runs = tempfile::tempdir().unwrap();
    let dir = runs.path().join("1800000000-a-0");
    std::fs::create_dir_all(&dir).unwrap();
    let inline = json!({ "protocol": "single", INLINE_RECIPE_KEY: recipe });
    std::fs::write(dir.join(vike_runs::MANIFEST_FILE), inline.to_string()).unwrap();
    let before = std::fs::read(dir.join(vike_runs::MANIFEST_FILE)).unwrap();
    assert_eq!(saved_recipe(&dir, &inline).unwrap(), recipe);
    assert_eq!(std::fs::read(dir.join(vike_runs::MANIFEST_FILE)).unwrap(), before);
    assert!(!dir.join(vike_runs::DAY_STAMPS_FILE).exists(), "nothing is added to an older run");

    let (apart, bytes) = day_stamps_apart(&recipe).unwrap();
    std::fs::write(dir.join(vike_runs::DAY_STAMPS_FILE), &bytes).unwrap();
    let now = json!({ "protocol": "single", EQUITY_LOG_RECIPE_KEY: apart });
    assert_eq!(saved_recipe(&dir, &now).unwrap(), recipe, "both layouts, one recipe");
    let why = saved_recipe(&dir, &json!({ "protocol": "single" })).unwrap_err();
    assert_eq!(why, "its manifest records no recipe");
}

/// A scan's saved run, for [`a_summarized_single_run_is_saved_with_its_whole_curve_and_named_in_its_summary`].
fn scan_run_for_curve() -> StudioRun {
    StudioRun {
        profile_toml: "[strategy]\nfile = \"trend/sma_cross.rs\"\n".to_string(),
        answer: Response::ParamscanReport(
            "{\"rows\":[],\"strategy_file\":\"trend/sma_cross.rs\"}".to_string(),
        ),
        source: None,
        equity_log: None,
    }
}

/// REVIEW-LIVE-MONEY H1 for a Studio run: below the free-space floor nothing is written under the runs root, and the
/// skip is logged at `error`.
#[test]
fn below_the_free_space_floor_a_studio_run_is_not_saved_and_the_skip_is_logged() {
    let runs = tempfile::tempdir().unwrap();
    let clock = at(1_800_000_000);
    let request = Request::Summarized(Box::new(studio_slice(sma_file())));
    let floor = vike_boot::disk_floor::DiskFloor::of_mib(10_240);
    let full = floor.refusal_with(runs.path(), |_: &Path| {
        Ok::<_, String>(vike_boot::disk_floor::FilesystemSpace {
            available_bytes: 1 << 30,
            total_bytes: 100 << 30,
        })
    });
    assert!(full.is_some(), "the premise: 1 GiB free is below a 10 GiB floor");
    let ((), events) = vike_log::capture::captured(|| {
        save_studio(studio_run(), None, None, start(&request, &clock), runs.path(), &clock, full);
    });
    assert_eq!(std::fs::read_dir(runs.path()).unwrap().count(), 0, "nothing under the runs root");
    assert!(
        events.iter().any(|e| e.level == tracing::Level::ERROR && e.message.contains("NOT saved")),
        "the skip is logged at error: {events:?}"
    );
}

/// An editor buffer's run keeps the buffer's text as the file its profile names (`editor-buffer.rhai`), with its
/// sha256 under `detail`: the run runs again from its own directory. A strategy file's run keeps no copy.
#[test]
fn an_editor_buffer_run_keeps_its_text_as_the_file_its_profile_names() {
    let runs = tempfile::tempdir().unwrap();
    let clock = at(1_800_000_000);
    let text = "fn on_bar(ctx) {}\n";
    let sha = vike_backtest::run::StrategySource::of(text.to_string()).sha256;
    let result = vike_analytics::TradingRecord {
        equity_curve: vec![1_000.0, 1_010.0],
        final_equity: 1_010.0,
        ..Default::default()
    };
    let report = TradingResults::from_record(None, &result, 365.0).with_strategy(
        crate::studio_run::EDITOR_BUFFER_FILE,
        &sha,
        None,
    );
    let buffer = StudioRun {
        profile_toml: "[strategy]\nfile = \"editor-buffer.rhai\"\n".to_string(),
        answer: Response::Report(report_answer(&report, None).unwrap()),
        source: Some(text.to_string()),
        equity_log: None,
    };
    let request = studio_slice(StrategySpecWire::Rhai(text.to_string()));
    save_studio(buffer, None, None, start(&request, &clock), runs.path(), &clock, None);
    let single = start(&studio_slice(sma_file()), &clock);
    save_studio(studio_run(), None, None, single, runs.path(), &clock, None);

    let scan = vike_runs::scan_runs(runs.path());
    assert_eq!(scan.runs.len(), 2, "{:?}", scan.diagnostics);
    for run in &scan.runs {
        let kept = run.dir.join(crate::studio_run::EDITOR_BUFFER_FILE);
        if run.manifest.strategy_file() == Some(crate::studio_run::EDITOR_BUFFER_FILE) {
            assert_eq!(run.manifest.detail["strategy_sha256"], sha.as_str());
            assert_eq!(std::fs::read_to_string(&kept).unwrap(), text, "the buffer, as it ran");
        } else {
            assert!(!kept.exists(), "a strategy file's run keeps no copy");
        }
    }
}

/// A Studio scan is ONE run whose `report.json` is its table (every point's overrides and report, best first), and
/// the retention counts it under its strategy file with that file's other runs: three scans and a retention of 1
/// leave the newest and the one a person TAGGED, never more.
#[test]
fn a_scan_is_one_run_the_retention_counts_and_a_tagged_scan_is_kept() {
    let runs = tempfile::tempdir().unwrap();
    let table = serde_json::json!({
        "rows": [
            { "overrides": [["fast", 5]], "report": serde_json::to_value(a_report()).unwrap() },
            { "overrides": [["fast", 9]], "report": serde_json::to_value(a_report()).unwrap() },
        ],
        "strategy_file": "trend/sma_cross.rs",
        "strategy_sha256": "ab".repeat(32),
    });
    let scan_run = || StudioRun {
        profile_toml: "[strategy]\nfile = \"trend/sma_cross.rs\"\n[paramscan]\nfast = [5, 9]\n"
            .into(),
        answer: Response::ParamscanReport(table.to_string()),
        source: None,
        equity_log: None,
    };
    for secs in [1_800_000_000, 1_800_000_100, 1_800_000_200] {
        let clock = at(secs);
        let scan = start(&studio_scan(sma_file()), &clock);
        save_studio(scan_run(), None, None, scan, runs.path(), &clock, None);
    }
    let by_age = runs_by_age;
    let saved = by_age(runs.path());
    assert_eq!(saved.len(), 3, "one run per scan, not one per point");
    for run in &saved {
        assert_eq!(run.manifest.detail["protocol"], "paramscan");
        assert_eq!(run.manifest.strategy_file(), Some("trend/sma_cross.rs"));
        let report: Value =
            serde_json::from_str(&std::fs::read_to_string(run.dir.join("report.json")).unwrap())
                .unwrap();
        assert_eq!(report, table, "report.json is the scan's table");
    }
    vike_runs::add_tags(&saved[0].dir, &["keep".to_string()], None, 1_800_000_300).unwrap();

    let retention = vike_runs::RunRetention {
        keep_per_strategy: 1,
        marks_root: runs.path().join("absent-marks"),
        referencing_documents: Vec::new(),
    };
    prune(&retention, runs.path());
    let left: Vec<String> = by_age(runs.path()).into_iter().map(|run| run.run_id).collect();
    assert_eq!(
        left,
        [saved[0].run_id.clone(), saved[2].run_id.clone()],
        "the tagged and the newest"
    );
}

/// The saved runs under `runs`, oldest first.
fn runs_by_age(runs: &Path) -> Vec<vike_runs::ScannedRun> {
    let mut scan = vike_runs::scan_runs(runs).runs;
    scan.sort_by(|a, b| a.manifest.finished_at.cmp(&b.manifest.finished_at));
    scan
}

/// The combinatorial walk-forward and the permutation test are counted too (nothing saved grows without bound): each
/// names its strategy file (a combinatorial walk on its paths, a permutation test at its top), so they count with the
/// file's other runs under the SAME never-delete rules. Over a retention of 1, the tagged, the marked and the
/// referenced one (a study's run names it) stay with the newest, and only the unprotected oldest goes.
#[test]
fn a_combinatorial_walk_and_a_permutation_test_count_under_the_retentions_rules() {
    let runs = tempfile::tempdir().unwrap();
    let marks = tempfile::tempdir().unwrap();
    let (file, sha) = ("trend/sma_cross.rs", "ab".repeat(32));
    let combinatorial = json!({
        "groups": 4,
        "paths": [
            { "overall": { "strategy_file": file, "strategy_sha256": sha } },
            { "overall": { "strategy_file": file } },
        ],
    })
    .to_string();
    let permutation = |trained: bool| {
        json!({ "trained": trained, "p_value": 0.5, "strategy_file": file, "strategy_sha256": sha })
            .to_string()
    };
    let walk =
        Request::RunWalkforwardProfile { profile_toml: "[walkforward]\ngroups = 4\n".into() };
    let test = Request::RunBacktest("[permutation]\npermutations = 3\n".into());
    let answers = [
        (Response::CombinatorialWalkforwardReport(combinatorial.clone()), &walk),
        (Response::PermutationReport(permutation(false)), &test),
        (Response::CombinatorialWalkforwardReport(combinatorial.clone()), &walk),
        (Response::PermutationReport(permutation(true)), &test),
        (Response::CombinatorialWalkforwardReport(combinatorial), &walk),
    ];
    for (n, (answer, request)) in answers.into_iter().enumerate() {
        let clock = at(1_800_000_000 + 100 * n as i64);
        save(answer, None, start(request, &clock), runs.path(), &clock, None);
    }
    let saved = runs_by_age(runs.path());
    assert_eq!(saved.len(), 5);
    for run in &saved {
        assert_eq!(run.manifest.strategy_file(), Some(file), "{:?}", run.manifest.detail);
        assert_eq!(run.manifest.detail["strategy_sha256"], sha.as_str());
    }
    vike_runs::add_tags(&saved[1].dir, &["keep".to_string()], None, 1_800_000_500).unwrap();
    vike_runs::write_mark(marks.path(), "baseline/sma", &saved[2].run_id, None, 1_800_000_500)
        .unwrap();
    // A study's run that names the fourth (as a study names the runs it read).
    let study = create_run_dir(runs.path(), 1_800_000_350, None).unwrap();
    let manifest = RunManifest {
        schema: MANIFEST_SCHEMA,
        run_id: study.run_id.clone(),
        kind: "study".to_string(),
        produced_by: "test".to_string(),
        started_at: utc_rfc3339(1_800_000_350),
        finished_at: utc_rfc3339(1_800_000_350),
        git_sha: None,
        strategy_sdk: None,
        fingerprint: None,
        config: RunConfig { path: None, name: None },
        detail: json!({}),
        kept_rows: Vec::new(),
    };
    let reads = json!({ "runs": [saved[3].run_id] });
    write_run_with(&study.path, &manifest, &reads, &RunExtras::default()).unwrap();

    let retention = vike_runs::RunRetention {
        keep_per_strategy: 1,
        marks_root: marks.path().to_path_buf(),
        referencing_documents: Vec::new(),
    };
    prune(&retention, runs.path());
    let left: Vec<String> = runs_by_age(runs.path())
        .into_iter()
        .filter(|run| run.manifest.kind == BACKTEST_RUN_KIND)
        .map(|run| run.run_id)
        .collect();
    let kept: Vec<String> = saved[1..].iter().map(|run| run.run_id.clone()).collect();
    assert_eq!(
        left, kept,
        "the tagged, the marked, the referenced and the newest; the oldest went"
    );
}

// --- a summarized profile search's save, row by row (decision 0078's amendment of 2026-11-19) -------------------

/// The start `handle_request` takes for a summarized profile search.
fn search_start(clock: &dyn Clock) -> Option<SavedRunStart> {
    let search = Request::RunParamscanProfile {
        profile_toml: "name = \"scan\"\n[paramscan]\nfast = [1, 2]\n".to_string(),
        rank_by: None,
        search: None,
    };
    start(&Request::Summarized(Box::new(search)), clock)
}

/// One row of a search's whole report, as the search hands it over.
const SEARCH_ROW: &str = r#"{"overrides":[["fast",1]],"report":null,"error":"no bars"}"#;

/// The search's whole report with no rows.
const SEARCH_TAIL: &str = r#"{"rows":[],"rank_by":"sharpe","strategy_file":"trend/momentum.rs"}"#;

/// The summary the search answers its client.
fn search_summary() -> Response {
    Response::ParamscanProfileSummary(Box::new(
        vike_backtesthub_client::summary::ParamscanProfileSummaryWire {
            run: "a-1".to_string(),
            saved_run: String::new(),
            total: 2,
            page_size: 1000,
            rows: Vec::new(),
            rank_by: "sharpe".to_string(),
            summary: None,
            account_currency: None,
            strategy_file: "trend/momentum.rs".to_string(),
            strategy_sha256: String::new(),
            strategy_sdk: None,
            data_warnings: Vec::new(),
            strategy_failures: Default::default(),
            answered_from_saved_run: String::new(),
        },
    ))
}

/// The saved run `response` names, or `None` when it names none.
fn saved_run_of(response: &Response) -> Option<String> {
    match response {
        Response::ParamscanProfileSummary(summary) if !summary.saved_run.is_empty() => {
            Some(summary.saved_run.clone())
        }
        _ => None,
    }
}

/// Everything under `runs`.
fn entries(runs: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(runs)
        .map_or_else(|_| Vec::new(), |dir| dir.flatten().map(|e| e.path()).collect())
}

/// The rows arrive one at a time and the run is the plain verb's: its `report.json` the whole report's compact text,
/// protocol `paramscan`, counted under its file, named in the summary.
#[test]
fn a_summarized_search_is_saved_as_its_rows_arrive() {
    let runs = tempfile::tempdir().unwrap();
    let mut save = ParamscanSave::new(search_start(&at(1_800_000_000)), runs.path(), None);
    save.row(Some(SEARCH_ROW));
    save.row(Some(SEARCH_ROW));
    save.tail(SEARCH_TAIL.to_string());
    let answer = save.finish(search_summary(), &at(1_800_000_060));

    let id = saved_run_of(&answer).expect("the summary names its saved run");
    let dir = vike_runs::finished_run_dir(runs.path(), &id).expect("a finished run");
    let report = std::fs::read_to_string(dir.join(vike_runs::REPORT_FILE)).unwrap();
    let rest = SEARCH_TAIL.strip_prefix("{\"rows\":[]").unwrap();
    assert_eq!(report, format!("{{\"rows\":[{SEARCH_ROW},{SEARCH_ROW}]{rest}\n"));
    let manifest = vike_runs::read_manifest(&dir).unwrap();
    assert_eq!(manifest.detail["protocol"], "paramscan");
    assert_eq!(manifest.strategy_file(), Some("trend/momentum.rs"));
    assert_eq!(manifest.config.name.as_deref(), Some("scan"));
}

/// A point whose whole report could not cross the child's pipe: the run already begun is REMOVED, not left a
/// half-written document no prune would ever count, and the summary names no run.
#[test]
fn a_row_over_the_frame_cap_removes_the_run_it_began() {
    let runs = tempfile::tempdir().unwrap();
    let mut save = ParamscanSave::new(search_start(&at(1_800_000_000)), runs.path(), None);
    save.row(Some(SEARCH_ROW));
    assert_eq!(entries(runs.path()).len(), 1, "the run is begun at its first row");
    save.row(None);
    assert!(entries(runs.path()).is_empty(), "and removed when it cannot be finished");
    save.row(Some(SEARCH_ROW));
    save.tail(SEARCH_TAIL.to_string());
    let answer = save.finish(search_summary(), &at(1_800_000_060));
    assert_eq!(saved_run_of(&answer), None, "the answer goes back, naming no run");
    assert!(entries(runs.path()).is_empty(), "nothing begins again after the stop");
}

/// A search that ends without its summary (cancelled, or its child died mid-answer) answers its error, and the run
/// it began is removed; so is one whose save is dropped unfinished.
#[test]
fn a_search_that_ends_without_its_summary_removes_the_run_it_began() {
    let runs = tempfile::tempdir().unwrap();
    let mut save = ParamscanSave::new(search_start(&at(1_800_000_000)), runs.path(), None);
    save.row(Some(SEARCH_ROW));
    let answer = save.finish(Response::Error("cancelled".to_string()), &at(1_800_000_060));
    assert!(matches!(answer, Response::Error(ref why) if why == "cancelled"));
    assert!(entries(runs.path()).is_empty(), "{:?}", entries(runs.path()));

    let mut dropped = ParamscanSave::new(search_start(&at(1_800_000_000)), runs.path(), None);
    dropped.row(Some(SEARCH_ROW));
    drop(dropped);
    assert!(entries(runs.path()).is_empty(), "{:?}", entries(runs.path()));
}

/// The free-space floor is asked before the first byte and again as the report grows: refusing part-way removes
/// the run begun, and refusing at the start begins none.
#[test]
fn the_free_space_floor_is_asked_as_the_report_grows() {
    let runs = tempfile::tempdir().unwrap();
    let asked = std::cell::Cell::new(0);
    let room = |_: &Path| {
        asked.set(asked.get() + 1);
        (asked.get() > 2).then(|| "the disk is full".to_string())
    };
    let mut save =
        ParamscanSave::new(search_start(&at(1_800_000_000)), runs.path(), Some(Box::new(room)));
    // Asked at every row once a row's bytes are written, as a real save is every `ROOM_CHECK_BYTES`.
    save.room_every = 1;
    save.row(Some(SEARCH_ROW));
    save.row(Some(SEARCH_ROW));
    assert_eq!(
        (asked.get(), entries(runs.path()).len()),
        (2, 1),
        "two rows, two looks, one run begun"
    );
    save.row(Some(SEARCH_ROW));
    assert_eq!(asked.get(), 3);
    assert!(entries(runs.path()).is_empty(), "the refusal removed what was begun");
    save.tail(SEARCH_TAIL.to_string());
    assert_eq!(saved_run_of(&save.finish(search_summary(), &at(1_800_000_060))), None);

    let full = |_: &Path| Some("the disk is full".to_string());
    let mut save =
        ParamscanSave::new(search_start(&at(1_800_000_000)), runs.path(), Some(Box::new(full)));
    save.row(Some(SEARCH_ROW));
    save.tail(SEARCH_TAIL.to_string());
    assert_eq!(saved_run_of(&save.finish(search_summary(), &at(1_800_000_060))), None);
    assert!(entries(runs.path()).is_empty(), "a refusal at the start writes nothing");
    assert_eq!(ROOM_CHECK_BYTES, 64 << 20, "the real save looks every 64 MiB");
}

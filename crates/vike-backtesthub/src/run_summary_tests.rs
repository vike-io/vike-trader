use super::*;
use vike_backtesthub_client::summary::CURVE_PAGE_SIZE_MAX;
use vike_backtesthub_client::wire_studio::{
    ParamscanEntryWire, ParamscanWire, StrategySpecWire, StudioParamscanWire, TradingRecordWire,
};
use vike_model::params::ParamValue;

/// No strategy file is read by these runs: the profile parse [`periods_of_request`] makes needs a folder.
fn nowhere() -> &'static Path {
    Path::new(".")
}

fn slice() -> Box<DataSliceWire> {
    Box::new(DataSliceWire {
        venue: "binance".to_string(),
        symbols: vec!["BTCUSDT".to_string()],
        interval: "15m".to_string(),
        start: None,
        end: None,
        kind: ReplayKindWire::Bars,
    })
}

fn spec() -> StrategySpecWire {
    StrategySpecWire::Rhai("fn on_bar(strategy) {}".to_string())
}

fn trade(i: usize) -> Trade {
    Trade {
        entry_price: 100.0 + i as f64,
        exit_price: 101.0 + i as f64,
        size: 1.0,
        pnl: 1.0,
        fees: 0.1,
        entry_timestamp: i as i64,
        exit_timestamp: i as i64 + 1,
        symbol: "BTCUSDT".to_string(),
        mae: 0.0,
        mfe: 0.0,
        is_long: true,
    }
}

/// `steps` of a curve whose every value carries all its digits.
fn curve(steps: usize) -> (Vec<f64>, Vec<i64>) {
    let equity = (0..steps).map(|i| 10_000.0 + (i % 97) as f64 + (i as f64).sqrt() / 3.0).collect();
    (equity, (0..steps as i64).map(|i| 1_704_067_200_000 + i * 60_000).collect())
}

fn whole(trades: usize) -> TradingRecordWire {
    let (equity_curve, equity_timestamp) = curve(5_000);
    TradingRecordWire {
        final_equity: *equity_curve.last().unwrap(),
        equity_timestamp,
        equity_curve,
        n_trades: trades,
        trades: (0..trades).map(trade).collect(),
        ..TradingRecordWire::default()
    }
}

fn page(run: &str, point: Option<usize>, page: usize) -> TradesPageWire {
    match trades_page(run, point, page, 200) {
        Response::TradesPage(page) => page,
        other => panic!("a held run pages: {other:?}"),
    }
}

/// Every page of run `run`'s whole curve through [`curve_page`], each encoded and decoded as its own frame,
/// joined: the curve a client reads back.
fn read_back(run: &str, runs_root: &Path) -> (Vec<f64>, Vec<i64>) {
    let (mut equity, mut timestamp) = (Vec::new(), Vec::new());
    for page in 0.. {
        let frame =
            serde_json::to_vec(&curve_page(run, page, CURVE_PAGE_SIZE_MAX, runs_root, None))
                .expect("a page encodes");
        assert!(frame.len() < vike_hub_protocol::frame::MAX_FRAME_LEN as usize);
        match serde_json::from_slice(&frame).expect("a page decodes") {
            Response::CurvePage(part) if part.equity.is_empty() => break,
            Response::CurvePage(part) => {
                equity.extend(part.equity);
                timestamp.extend(part.timestamp);
            }
            other => panic!("a held curve pages: {other:?}"),
        }
    }
    (equity, timestamp)
}

fn bits(curve: &[f64]) -> Vec<u64> {
    curve.iter().map(|v| v.to_bits()).collect()
}

/// One test, because the held runs are process-wide: a single run is answered summarized and its
/// trades and whole curve page; a scan holds every point, answers the best and opens another; and a run
/// past [`HELD_RUNS`] newer ones is refused in words.
#[test]
fn a_summarized_run_is_held_paged_opened_and_dropped_in_turn() {
    let inner = Request::RunSlice { spec: spec(), slice: slice(), params: None };
    let Response::RunSummary(summary) = summarized(inner, nowhere(), |_| {
        (Response::RunResult(whole(450)), WholeRecords::default())
    }) else {
        panic!("a single run answers its summary");
    };
    assert!(!summary.run.is_empty());
    assert_eq!(summary.point, None);
    assert_eq!(summary.n_trades, 450);
    assert_eq!(summary.curve_len, 5_000);
    let pages: Vec<TradesPageWire> = (0..3).map(|p| page(&summary.run, None, p)).collect();
    assert_eq!(pages.iter().map(|p| p.trades.len()).collect::<Vec<_>>(), vec![200, 200, 50]);
    let joined: Vec<Trade> = pages.into_iter().flat_map(|p| p.trades).collect();
    assert_eq!(joined, (0..450).map(trade).collect::<Vec<_>>());
    let (equity, timestamp) = read_back(&summary.run, nowhere());
    assert_eq!(bits(&equity), bits(&whole(0).equity_curve), "the whole curve, held, to the bit");
    assert_eq!(timestamp, whole(0).equity_timestamp);

    let scan = StudioParamscanWire {
        entries: (0..3)
            .map(|i| ParamscanEntryWire {
                overrides: vec![("fast".to_string(), ParamValue::Int(i))],
                result: whole(10 + i as usize),
            })
            .collect(),
        dsr: Some(0.5),
        pbo: None,
        best_index: 0,
        cost_model: None,
    };
    let inner = Request::RunParamscan {
        spec: spec(),
        slice: slice(),
        paramscan: ParamscanWire::default(),
        params: None,
    };
    let Response::ParamscanSummary(scanned) = summarized(inner, nowhere(), |_| {
        (Response::ParamscanResult(scan), WholeRecords::default())
    }) else {
        panic!("a scan answers its summary");
    };
    assert_eq!(scanned.points.len(), 3);
    assert_eq!(scanned.points[2].n_trades, 12);
    let best = scanned.best.as_ref().expect("the best point");
    assert_eq!((best.point, best.n_trades), (Some(0), 10));
    match point_summary(&scanned.run, 2) {
        Response::RunSummary(opened) => assert_eq!((opened.point, opened.n_trades), (Some(2), 12)),
        other => panic!("another point opens: {other:?}"),
    }
    assert_eq!(page(&scanned.run, Some(1), 0).trades.len(), 11);
    assert!(
        matches!(point_summary(&scanned.run, 3), Response::Error(why) if why.contains("no point 3"))
    );
    assert!(
        matches!(curve_page(&scanned.run, 0, 10, nowhere(), None), Response::Error(why)
            if why.contains("a scan's points keep none")),
        "a scan keeps no point's whole curve"
    );

    // The single run is saved, as a serving daemon saves it (`with_saved_run` remembers the pair).
    let runs = tempfile::tempdir().unwrap();
    let saved = save_curve(runs.path(), &held_curve(&summary.run).expect("held"));
    remember_saved(&summary.run, &saved);

    for _ in 0..HELD_RUNS {
        let inner = Request::RunSlice { spec: spec(), slice: slice(), params: None };
        summarized(inner, nowhere(), |_| (Response::RunResult(whole(1)), WholeRecords::default()));
    }
    match trades_page(&summary.run, None, 0, 200) {
        Response::Error(why) => {
            assert!(why.contains("no longer holds") && why.contains("Run it again"), "{why}")
        }
        other => panic!("a dropped run is refused in words: {other:?}"),
    }
    // The held copy is gone; a reader naming the HELD run still reads the whole curve, from the run it was
    // saved as, to the bit. Where that run is not on disk it is refused in words.
    assert!(held_curve(&summary.run).is_none(), "the held copy was dropped");
    let (equity, timestamp) = read_back(&summary.run, runs.path());
    assert_eq!(bits(&equity), bits(&whole(0).equity_curve), "read from the saved run");
    assert_eq!(timestamp, whole(0).equity_timestamp);
    match curve_page(&summary.run, 0, 10, nowhere(), None) {
        Response::Error(why) => assert!(why.contains("saved_run") && why.contains("Run it again")),
        other => panic!("a dropped run's curve with no saved run is refused in words: {other:?}"),
    }
    // Here, after the eviction above, so no hold of this test drops the run it reads.
    a_run_past_the_frame_cap_is_held_and_read_back_whole_then_read_from_its_saved_run();
}

/// A writer that only counts, so a frame's length is measured without holding the frame.
struct Count(usize);

impl std::io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A failure is the run's own; a request none of the four has no summary.
#[test]
fn a_failure_passes_through_and_a_non_studio_request_is_refused() {
    let inner = Request::RunSlice { spec: spec(), slice: slice(), params: None };
    let failed = summarized(inner, nowhere(), |_| {
        (Response::Error("compile: nope".to_string()), WholeRecords::default())
    });
    assert!(matches!(failed, Response::Error(why) if why == "compile: nope"));
    let refused = summarized(Request::Ping, nowhere(), |_| unreachable!("nothing runs"));
    assert!(matches!(refused, Response::Error(why) if why.contains("Ping has no summary")));
    // A `RunBacktest` is summarized at its profile's annualization: a 1-minute profile's, and a profile
    // that does not parse runs to its own refusal, which passes through.
    let minute = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\n\
                  interval = \"1m\"\nfrom = \"0\"\nto = \"100000\"\n\n[engine]\ncash = 1000.0\n\n\
                  [strategy]\nfile = \"trend/buy_hold.rs\"\n[strategy.params]\nsize = 1.0\n";
    let periods = periods_of_request(&Request::RunBacktest(minute.to_string()), nowhere());
    assert_eq!(periods, Ok(periods_per_year_for(Some("1m"))));
    let broken = summarized(Request::RunBacktest("[[".to_string()), nowhere(), |_| {
        (
            Response::Error("profile parse/validate failed: nope".to_string()),
            WholeRecords::default(),
        )
    });
    assert!(matches!(broken, Response::Error(why) if why.contains("parse/validate failed")));
}

/// What a sandboxed child streams (its answer, its closed points, its trailing trades and curve) is held as
/// the same summary made in this process would be: the run named in the answer and in every point.
#[test]
fn a_childs_streamed_summary_is_held_as_an_in_process_one() {
    let Summary::Run(single, trades, curve, None) =
        summarize_answer(Response::RunResult(whole(7)), WholeRecords::default(), 252.0)
    else {
        panic!("a run summarizes as a run, its curve whole");
    };
    let held = hold_summary(summary_from_child(
        Response::RunSummary(single),
        Vec::new(),
        trades,
        curve,
        None,
    ));
    let Response::RunSummary(single) = held else { panic!("a run answers its summary") };
    assert_eq!(page(&single.run, None, 0).trades, (0..7).map(trade).collect::<Vec<_>>());
    assert_eq!(bits(&read_back(&single.run, nowhere()).0), bits(&whole(7).equity_curve));

    let scan = StudioParamscanWire {
        entries: (0..2)
            .map(|i| ParamscanEntryWire {
                overrides: vec![("fast".to_string(), ParamValue::Int(i))],
                result: whole(3 + i as usize),
            })
            .collect(),
        dsr: None,
        pbo: None,
        best_index: 0,
        cost_model: None,
    };
    let Summary::Scan(answer, points) =
        summarize_answer(Response::ParamscanResult(scan), WholeRecords::default(), 252.0)
    else {
        panic!("a scan summarizes as a scan");
    };
    let held = hold_summary(summary_from_child(
        Response::ParamscanSummary(answer),
        points,
        Vec::new(),
        RunEquityCurve::default(),
        None,
    ));
    let Response::ParamscanSummary(scan) = held else { panic!("a scan answers its summary") };
    assert!(!scan.run.is_empty());
    assert_eq!(scan.best.as_ref().map(|best| best.run.as_str()), Some(scan.run.as_str()));
    assert_eq!(page(&scan.run, Some(1), 0).trades.len(), 4);
    match point_summary(&scan.run, 1) {
        Response::RunSummary(point) => {
            assert_eq!((point.run.clone(), point.point), (scan.run, Some(1)))
        }
        other => panic!("the streamed point opens: {other:?}"),
    }
}

/// **A run past the old frame cap reads back whole** (decision 0078's amendment of 2026-11-18): five years of
/// 1-minute steps, whose whole answer `vike_hub_protocol::frame::MAX_FRAME_LEN` refuses, are held (as
/// [`summarize_answer`] leaves a single run's curve: its summary over a curve that size is the client crate's
/// `summary_tests.rs`'s) and read back through `Request::ReadCurve`'s pages, each under the cap, bit for bit; and once
/// the run is saved, the saved run answers the same pages by its id. Called by the held-runs test above, after its
/// eviction, because the held runs are process-wide.
fn a_run_past_the_frame_cap_is_held_and_read_back_whole_then_read_from_its_saved_run() {
    let steps = 1826 * 1440;
    let (equity, timestamp) = curve(steps);
    let long = TradingRecordWire {
        equity_curve: equity.clone(),
        equity_timestamp: timestamp.clone(),
        ..TradingRecordWire::default()
    };
    let mut whole_len = Count(0);
    serde_json::to_writer(&mut whole_len, &Response::RunResult(long)).expect("encodes");
    assert!(whole_len.0 > vike_hub_protocol::frame::MAX_FRAME_LEN as usize, "{}", whole_len.0);
    let held = RunEquityCurve { equity: equity.clone(), timestamp: timestamp.clone() };
    let single = RunSummaryWire { curve_len: steps, ..RunSummaryWire::default() };
    let Response::RunSummary(summary) =
        hold_summary(Summary::Run(Box::new(single), Vec::new(), held, None))
    else {
        panic!("a single run answers its summary");
    };
    let (back_equity, back_timestamp) = read_back(&summary.run, nowhere());
    assert_eq!(bits(&back_equity), bits(&equity));
    assert_eq!(back_timestamp, timestamp);

    // Saved: the run directory keeps the held curve, and a read by the saved run's id is the same curve.
    let runs = tempfile::tempdir().unwrap();
    let saved = save_curve(runs.path(), &held_curve(&summary.run).expect("held"));
    let (saved_equity, saved_timestamp) = read_back(&saved, runs.path());
    assert_eq!(bits(&saved_equity), bits(&equity));
    assert_eq!(saved_timestamp, timestamp);
    assert!(matches!(curve_page("../etc", 0, 1, runs.path(), None), Response::Error(why)
        if why.contains("not a run id")));
}

/// One finished run directory under `runs_root` keeping `curve` as its whole curve, as the saver writes a
/// summarized single run (`crate::backtesthub_server::saved_run`'s `save_studio`); answers its id.
fn save_curve(runs_root: &Path, curve: &RunEquityCurve) -> String {
    let dir = vike_runs::create_run_dir(runs_root, 1_800_000_000, None).unwrap();
    let manifest = vike_runs::RunManifest {
        schema: vike_runs::MANIFEST_SCHEMA,
        run_id: dir.run_id.clone(),
        kind: vike_runs::BACKTEST_RUN_KIND.to_string(),
        produced_by: "test".to_string(),
        started_at: vike_model::time::utc_rfc3339(1_800_000_000),
        finished_at: vike_model::time::utc_rfc3339(1_800_000_001),
        git_sha: None,
        strategy_sdk: None,
        fingerprint: None,
        config: vike_runs::RunConfig { path: None, name: None },
        detail: serde_json::json!({ "protocol": "single" }),
        kept_rows: Vec::new(),
    };
    let extras = vike_runs::RunExtras { equity_curve: Some(curve), ..Default::default() };
    vike_runs::write_run_with(&dir.path, &manifest, &serde_json::json!({}), &extras).unwrap();
    dir.run_id
}

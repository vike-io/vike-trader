//! **The SUMMARIZED Studio runs over a real socket** (decision 0078's amendment of 2026-11-16): a
//! `Request::Summarized` run answers exactly the summary of the whole answer the same run gives in
//! process, its trades page back whole and in order, a 24-point scan answers each point and opens
//! any of them, and a walk-forward answers its windows with the stitched curve thinned. The fields of 2026-11-20 take
//! effect over the socket too: the curves' times, the window times, a point's drawdown, a Rhai Run's parameters and
//! the funding switch.
//!
//! Hermetic, as `tests/studio_wire_parity.rs`: an ephemeral listener over a `HistoryStoreDataFusion`
//! seeded on a temp dir. Its own binary because the held runs are process-wide
//! (`vike_backtesthub`'s `run_summary`, newest 8 kept), and this file makes fewer than 8 of them.

use std::sync::Arc;

use tempfile::TempDir;
use vike_analytics::TradingRecord;
use vike_backtesthub::studio_run::{
    StrategyFiles, run_paramscan_local, run_paramscan_slice_with_params, run_slice,
    run_slice_local, run_walkforward_local,
};
use vike_backtesthub::test_support::spawn_server_over;
use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::summary::{
    CURVE_POINTS, TRADES_PAGE_SIZE, downsample, summarize_record,
};
use vike_backtesthub_client::test_support::btcusdt_1m_bars;
use vike_backtesthub_client::wire_studio::{
    DataSliceWire, ParamscanWire, StrategySpecWire, WalkforwardWire,
};
use vike_data::HistoryStore;
use vike_model::params::ParamValue;
use vike_store::HistoryStoreDataFusion;
use vike_studio_core::test_support::cross_script::LONG_CROSS_SCRIPT;
use vike_studio_core::test_support::sweep_script::SWEEP_SCRIPT;
use vike_studio_core::test_support::walk::random_walk_bars;
use vike_studio_core::wire_run::{to_data_slice, to_strategy_spec};

/// 3,000 one-minute bars of a deterministic walk: more than [`CURVE_POINTS`], so the curve is thinned.
fn seeded_bar_store() -> (TempDir, Arc<dyn HistoryStore + Send + Sync>) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars = random_walk_bars(3_000, 60_000, Some("BTCUSDT"));
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    (dir, Arc::new(store))
}

fn files() -> StrategyFiles {
    StrategyFiles::stand_in(vike_strategy_lab::stand_in)
}

/// The FULL record of the run `run_slice_local` answers: what the server summarizes (decision 0078's amendment of
/// 2026-11-19), the closed-trade block and every simulator counter its wire record does not carry included.
fn full_record(
    spec: &StrategySpecWire,
    slice: &DataSliceWire,
    store: &Arc<dyn HistoryStore + Send + Sync>,
) -> TradingRecord {
    let spec = to_strategy_spec(spec).expect("a spec");
    run_slice(&spec, &files(), &to_data_slice(slice), store, None).expect("the full record")
}

/// The summary over the wire IS the local whole answer summarized, and its trades, paged, are that
/// answer's trades.
#[test]
fn a_summarized_run_is_the_local_run_summarized_and_its_trades_page_back() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategy_lab::stand_in);
    let spec = StrategySpecWire::Rhai(LONG_CROSS_SCRIPT.to_string());
    let slice = btcusdt_1m_bars();

    let whole = run_slice_local(&spec, &files(), &slice, None, Arc::clone(&store)).expect("local");
    let periods = vike_analytics::results::periods_per_year_for(Some("1m"));
    let (local_equity, local_timestamp) =
        (whole.equity_curve.clone(), whole.equity_timestamp.clone());
    let record = full_record(&spec, &slice, &store);
    let (local, local_trades) = summarize_record(whole, record, None, periods);

    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote = client.run_slice_summary(spec, slice, None).expect("remote summary");
    assert!(!remote.run.is_empty());
    assert_eq!(remote.rows, local.rows, "every Performance row as the local run prints it");
    assert_eq!(remote.curve, local.curve);
    assert_eq!(remote.curve_len, 3_000);
    assert!(remote.curve.len() <= CURVE_POINTS + 4);
    // The curve's times (2026-11-20): each kept point's own, rising, from the slice's first bar to its last.
    let kept: Vec<i64> = remote.curve.iter().map(|&(step, _)| local_timestamp[step]).collect();
    assert_eq!(remote.curve_timestamp, kept);
    assert!(remote.curve_timestamp.windows(2).all(|pair| pair[0] < pair[1]));
    let bars = random_walk_bars(3_000, 60_000, Some("BTCUSDT"));
    let window = (bars[0].timestamp, bars[bars.len() - 1].timestamp);
    let ends = (remote.curve_timestamp[0], remote.curve_timestamp[remote.curve.len() - 1]);
    assert_eq!(ends, window, "the curve's times span the run's window");
    assert_eq!(remote.n_trades, local.n_trades);
    assert!(remote.n_trades > 0, "the cross trades over the walk");
    let mut paged = Vec::new();
    for page in 0.. {
        let cut = client.read_trades(&remote.run, None, page, TRADES_PAGE_SIZE).expect("a page");
        assert_eq!(cut.total, local_trades.len());
        if cut.trades.is_empty() {
            break;
        }
        paged.extend(cut.trades);
    }
    assert_eq!(paged, local_trades, "the pages, joined, are the run's trades in close order");
    let gone = client.read_trades("0-0", None, 0, TRADES_PAGE_SIZE).expect_err("not held");
    assert!(gone.contains("no longer holds"), "{gone}");
    // The WHOLE curve on request (decision 0078's amendment of 2026-11-18): page by page, it is the local run's
    // curve to the bit, timestamps and all.
    let (equity, timestamp) = client.read_whole_curve(&remote.run).expect("the whole curve");
    let bits = |curve: &[f64]| curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&equity), bits(&local_equity));
    assert_eq!(timestamp, local_timestamp);
    let pages = client.read_curve(&remote.run, 0, 1_000).expect("a page");
    assert_eq!((pages.total, pages.equity.len()), (3_000, 1_000), "a page is one page");
}

/// A summary over the wire carries its RESULTS TABLE for a comparison (decision 0078's amendment, the owner's
/// word of 2026-11-19): the local run's results, without its trades and curves, and small.
#[test]
fn a_summary_carries_the_results_table_without_trades_or_curves() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategy_lab::stand_in);
    let spec = StrategySpecWire::Rhai(LONG_CROSS_SCRIPT.to_string());
    let slice = btcusdt_1m_bars();
    let whole = run_slice_local(&spec, &files(), &slice, None, Arc::clone(&store)).expect("local");
    let periods = vike_analytics::results::periods_per_year_for(Some("1m"));
    let (local, _) = summarize_record(whole, full_record(&spec, &slice, &store), None, periods);

    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote = client.run_slice_summary(spec, slice, None).expect("remote summary");
    let results = remote.results.as_ref().expect("this server sends the results table");
    assert_eq!(remote.results, local.results, "the local run's table");
    assert!(results.n_trades > 0, "the cross trades over the walk");
    let closed = results.closed_trades.as_ref().expect("the closed-trade statistics");
    assert!(
        closed.trades.is_empty()
            && closed.open_trades.is_empty()
            && closed.buy_hold_curve.is_empty()
    );
    let bytes = serde_json::to_vec(results).expect("encodes").len();
    eprintln!("the results table of a {}-trade run: {bytes} bytes", results.n_trades);
    // 8 KiB since 2026-11-19 (decision 0078's amendment of that day): the table carries its run's closed-trade
    // statistics now, which the decoded record it was made from before had none of (5,344 bytes for this run).
    assert!(bytes < 8_192, "a row of the compare table costs a few kilobytes at most: {bytes}");
}

/// A 24-point scan answers every point and the best one's summary, and opens any other point.
#[test]
fn a_twenty_four_point_scan_answers_every_point_and_opens_any() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategy_lab::stand_in);
    let spec = StrategySpecWire::Rhai(SWEEP_SCRIPT.to_string());
    let slice = btcusdt_1m_bars();
    let grid = ParamscanWire {
        axes: vec![("fast".to_string(), (2..26).map(|f| ParamValue::Float(f as f64)).collect())],
    };

    let whole = run_paramscan_local(&spec, &files(), &slice, &grid, None, Arc::clone(&store))
        .expect("local scan");
    let periods = vike_analytics::results::periods_per_year_for(Some("1m"));
    // Each point's FULL record, as the server summarizes it (decision 0078's amendment of 2026-11-19).
    let full = run_paramscan_slice_with_params(
        &to_strategy_spec(&spec).expect("a spec"),
        &files(),
        &to_data_slice(&slice),
        &store,
        &grid.axes,
        None,
    )
    .expect("the full scan");
    let local_point = |at: usize| {
        let record = full.entries[at].result.clone();
        summarize_record(whole.entries[at].result.clone(), record, None, periods)
    };

    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote = client.run_paramscan_summary(spec, slice, grid, None).expect("remote scan");
    assert_eq!(remote.points.len(), 24);
    for (at, (point, entry)) in remote.points.iter().zip(&whole.entries).enumerate() {
        assert_eq!(point.overrides, entry.overrides, "ranked as the whole scan ranked");
        assert_eq!(point.n_trades, entry.result.n_trades);
        // A point's drawdown (2026-11-20) is its own run's.
        let own = full.entries[at].result.max_drawdown();
        assert_eq!(point.max_drawdown.map(f64::to_bits), Some(own.to_bits()), "point {at}");
    }
    let best = remote.best.as_ref().expect("the best point");
    assert_eq!(best.point, Some(0));
    let (local_best, _) = local_point(0);
    assert_eq!(best.rows, local_best.rows);

    let opened = client.read_point(&remote.run, 17).expect("point 17 opens");
    let (local_17, local_17_trades) = local_point(17);
    assert_eq!(opened.point, Some(17));
    assert_eq!(opened.rows, local_17.rows);
    let opened_drawdown = opened.results.as_ref().map(|results| results.max_drawdown);
    assert_eq!(remote.points[17].max_drawdown, opened_drawdown, "the point's own summary says so");
    let page =
        client.read_trades(&remote.run, Some(17), 0, local_17_trades.len().max(1)).expect("page");
    assert_eq!(page.trades, local_17_trades);
}

/// A walk-forward answers its steps and overall results as the whole walk does, its stitched curve
/// thinned. Compared as their JSON: a results' source and period do not cross the wire.
#[test]
fn a_summarized_walk_forward_is_the_local_walk_with_its_curve_thinned() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategy_lab::stand_in);
    let spec = StrategySpecWire::Rhai(LONG_CROSS_SCRIPT.to_string());
    let slice = btcusdt_1m_bars();
    let walk = WalkforwardWire::fixed(2);

    let whole = run_walkforward_local(&spec, &files(), &slice, &walk, None, Arc::clone(&store))
        .expect("local walk");
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote = client.run_walkforward_summary(spec, slice, walk, None).expect("remote walk");
    assert_eq!(
        serde_json::to_value(&remote.steps).unwrap(),
        serde_json::to_value(&whole.steps).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&remote.overall).unwrap(),
        serde_json::to_value(&whole.overall).unwrap()
    );
    assert_eq!(remote.steps.len(), 2);
    assert_eq!(remote.overall.sharpe.to_bits(), whole.overall.sharpe.to_bits());
    assert_eq!(remote.profitable_step_share.to_bits(), whole.profitable_step_share.to_bits());
    assert_eq!(remote.equity_curve_len, whole.equity_curve.len());
    assert_eq!(remote.equity_curve, downsample(&whole.equity_curve, CURVE_POINTS));

    // The window times (2026-11-20): each half's first and last bar, as its bar range says.
    let bars = random_walk_bars(3_000, 60_000, Some("BTCUSDT"));
    for step in &remote.steps {
        let (start, end) = step.test_range;
        let times = step.window_times.expect("every window holds bars");
        assert_eq!(
            (times.test_start, times.test_end),
            (bars[start].timestamp, bars[end - 1].timestamp)
        );
        assert_eq!(
            times.train_end,
            Some(bars[start - 1].timestamp),
            "the train half ends where the test begins"
        );
        assert_eq!(
            times.train_start,
            Some(bars[0].timestamp),
            "the anchored walk trains from the first bar"
        );
    }
    // The stitched curve's times: the whole walk's own at each kept point, rising, inside the tested bars.
    assert_eq!(whole.equity_timestamp.len(), whole.equity_curve.len());
    let kept: Vec<i64> =
        remote.equity_curve.iter().map(|&(step, _)| whole.equity_timestamp[step]).collect();
    assert_eq!(remote.equity_curve_timestamp, kept);
    assert!(kept.windows(2).all(|pair| pair[0] <= pair[1]), "{kept:?}");
    let first_tested = bars[remote.steps[0].test_range.0].timestamp;
    assert_eq!((kept[0], kept[kept.len() - 1]), (first_tested, bars[bars.len() - 1].timestamp));
}

/// A Rhai buffer's Run at its parameters (2026-11-20) IS the scan point at the same values: the script's `param()`
/// value and the run's commission and slippage, applied as a scan applies a point's, so the two curves and trade lists
/// are one; and the parameters take effect (the bare buffer runs another curve, uncharged).
#[test]
fn a_rhai_run_at_its_parameters_is_the_same_point_of_a_scan() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategy_lab::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let slice = btcusdt_1m_bars();
    let grid = ParamscanWire {
        axes: vec![
            ("fast".to_string(), vec![ParamValue::Float(7.0), ParamValue::Float(9.0)]),
            ("commission".to_string(), vec![ParamValue::Float(0.1)]),
            ("slippage".to_string(), vec![ParamValue::Float(0.05)]),
        ],
    };
    let bare = StrategySpecWire::Rhai(SWEEP_SCRIPT.to_string());
    let scan = client.run_paramscan(bare.clone(), slice.clone(), grid, None).expect("the scan");
    let point = scan
        .entries
        .iter()
        .find(|entry| entry.overrides.contains(&("fast".to_string(), ParamValue::Float(9.0))))
        .expect("the fast = 9 point");

    let spec = StrategySpecWire::RhaiWithParameters {
        source: SWEEP_SCRIPT.to_string(),
        parameters_toml: "fast = 9.0\ncommission = 0.1\nslippage = 0.05\n".to_string(),
    };
    let run = client.run_slice(spec, slice.clone(), None).expect("the run");
    let bits = |curve: &[f64]| curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&run.equity_curve), bits(&point.result.equity_curve), "the scan point's curve");
    assert_eq!(run.trades, point.result.trades, "and its trades");
    let stamp = run.cost_model.as_ref().expect("a single run is stamped");
    assert_eq!(stamp.source, "override", "the commission param priced it");
    assert!(stamp.fees_paid > 0.0, "{stamp:?}");

    let plain = client.run_slice(bare, slice, None).expect("the bare buffer");
    assert_ne!(bits(&plain.equity_curve), bits(&run.equity_curve), "the parameters took effect");
}

/// The Studio's funding switch over a real socket (2026-11-20): the same perpetual slice pays its stored funding with
/// `attach_funding = true` and none with the key absent (off since 2026-11-21) or `false`.
#[test]
fn a_studio_run_with_its_funding_switch_off_pays_no_funding_over_the_wire() {
    use vike_backtesthub_client::wire_studio::{ReplayKindWire, StrategyEngineConfigWire};
    use vike_marketdata::Bar;
    use vike_marketdata::test_support::flat_bar_zero_volume;
    const HOUR: i64 = 3_600_000;
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let ramp: Vec<Bar> = (0..48)
        .map(|i| {
            let (open, close) = (100.0 + i as f64, 101.0 + i as f64);
            Bar { open, high: close, low: open, close, ..flat_bar_zero_volume(i * HOUR, open) }
        })
        .collect();
    store.append_bars("bybit", "BTCUSDT.P", "1h", &ramp, None).unwrap();
    let funding: Vec<Bar> = (1..=5)
        .map(|k| Bar { funding: Some(1e-4 * k as f64), ..flat_bar_zero_volume(k * 8 * HOUR, 0.0) })
        .collect();
    store.append_bars("bybit", "BTCUSDT.P", "funding", &funding, None).unwrap();
    let addr = spawn_server_over(Arc::new(store), vike_strategy_lab::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let spec = StrategySpecWire::File {
        file: "trend/buy_hold.rs".to_string(),
        params_toml: "size = 1.0\nsymbol = \"BTCUSDT.P\"".to_string(),
        sdk: None,
    };
    let slice = DataSliceWire {
        venue: "bybit".to_string(),
        symbols: vec!["BTCUSDT.P".to_string()],
        interval: "1h".to_string(),
        start: None,
        end: None,
        kind: ReplayKindWire::Bars,
    };
    let mut run = |attach_funding: Option<bool>| {
        let params =
            StrategyEngineConfigWire { attach_funding, ..StrategyEngineConfigWire::default() };
        client.run_slice(spec.clone(), slice.clone(), Some(params)).expect("the run")
    };
    let charged = run(Some(true)).funding_paid;
    assert!(charged < 0.0, "a perpetual pays its funding with the switch on: {charged}");
    assert_eq!(run(None).funding_paid, 0.0, "the key absent pays none");
    assert_eq!(run(Some(false)).funding_paid, 0.0, "the switch off pays none");
}

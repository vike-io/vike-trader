//! **A profile's SUMMARIZED parameter search** (decision 0078's amendment of 2026-11-19, the owner's "do 1-7"), over a
//! real socket against the real backtesthub: each point answers its overrides and its metric table, the very numbers
//! the whole search reports for it; the run the daemon saves is the whole report, byte for byte the plain verb's; and a
//! search whose whole report is bigger than the frame cap, which the plain verb could never answer, answers.
//!
//! Its own binary: every summarized search is a held run (`run_summary`'s `HELD_RUNS`), and `tests/studio_summary.rs`
//! reads its own held runs back.

use std::sync::Arc;

use vike_analytics::TradingResultsTable;
use vike_backtest::run::profile::ProfileBacktest;
use vike_backtest::search::paramscan::RankMetric;
use vike_backtesthub::test_support::{spawn_server_over, spawn_server_over_saving_to};
use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::client::ParamscanProfileAnswer;
use vike_backtesthub_client::summary::{
    PARAMSCAN_ROWS_PAGE_SIZE, ParamscanProfileSummaryWire, ParamscanRowsPageWire,
};
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_hub_protocol::frame::{BodyCodec, MAX_FRAME_LEN};

/// `bars` one-minute BTCUSDT bars of the seeded walk, in a memory store.
fn store_of(bars: usize) -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    let walk =
        vike_studio_core::test_support::walk::random_walk_bars(bars, 60_000, Some("BTCUSDT"));
    store.append_bars("binance", "BTCUSDT", "1m", &walk, None).expect("seed the bars");
    Arc::new(store)
}

/// Momentum over the store, its every position closed by its time barrier, and `grid` as its `[paramscan]`.
fn search_profile(time_limit_ms: i64, grid: &str) -> String {
    format!(
        "name = \"summarized_search\"\n\n[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\n\
         interval = \"1m\"\nfrom = \"0\"\nto = \"2000000000\"\n\n[engine]\ncash = 1000.0\n\n[strategy]\n\
         file = \"trend/momentum.rs\"\n[strategy.params]\nqty = 1.0\ntime_limit_ms = {time_limit_ms}\n\
         venue = \"binance\"\n\n[paramscan]\n{grid}\n"
    )
}

/// The summarized answer of `profile`, by the client's one fallback toward a daemon that has the summary.
fn summarized(client: &mut BacktesthubClient, profile: &str) -> ParamscanProfileSummaryWire {
    match client
        .run_paramscan_profile_answer(profile, None, None, false)
        .expect("the search answers")
    {
        ParamscanProfileAnswer::Summary(summary) => *summary,
        ParamscanProfileAnswer::Whole(_) => panic!("this backtesthub advertises the summary"),
    }
}

/// The saved run `summary` names: its `report.json` text and its manifest.
fn saved(
    runs: &std::path::Path,
    summary: &ParamscanProfileSummaryWire,
) -> (String, vike_runs::RunManifest) {
    let dir =
        vike_runs::finished_run_dir(runs, &summary.saved_run).expect("the saved run is finished");
    let report = std::fs::read_to_string(dir.join(vike_runs::REPORT_FILE)).expect("its report");
    (report, vike_runs::read_manifest(&dir).expect("its manifest"))
}

/// Each point of a summarized search is the whole search's point: the same overrides, rank and every metric-catalog
/// number of its report (`TradingResultsTable::from_report` over the in-process search's own rows), and the run the
/// daemon saved is the plain verb's whole report, byte for byte, kind `backtest`, protocol `paramscan`, the profile
/// as its `config.toml`. The rows read back in pages are the first page's continuation.
#[test]
fn each_point_answers_the_whole_searchs_numbers_and_the_saved_run_is_the_whole_report() {
    let store = store_of(400);
    let runs = tempfile::tempdir().expect("a runs root");
    let saving = spawn_server_over_saving_to(
        Arc::clone(&store),
        vike_strategy_lab::stand_in,
        runs.path().to_path_buf(),
    );
    let plain = spawn_server_over(Arc::clone(&store), vike_strategy_lab::stand_in);
    let profile = search_profile(180_000, "time_limit_ms = [60000, 120000, 180000, 300000]");

    let summary = summarized(&mut BacktesthubClient::connect(saving).expect("connect"), &profile);
    let mut json_client = BacktesthubClient::connect(plain).expect("connect");
    assert_eq!(json_client.prefer_body(BodyCodec::Json), BodyCodec::Json);
    let whole = json_client.run_paramscan_profile(&profile, None, None).expect("the whole search");

    let local = vike_backtest::search::paramscan::run_paramscan(
        &ProfileBacktest::from_toml_str(&profile)
            .expect("the profile parses")
            .with_stand_in(vike_strategy_lab::stand_in),
        Arc::clone(&store),
        RankMetric::Sharpe,
    )
    .expect("the in-process search runs");
    assert_eq!(
        serde_json::to_string(&local).expect("encodes"),
        whole,
        "the same search three ways"
    );

    assert_eq!((summary.total, summary.rows.len()), (4, 4), "one row a point");
    assert_eq!(summary.rank_by, "sharpe");
    assert_eq!(summary.strategy_file, "trend/momentum.rs");
    for (at, (row, point)) in summary.rows.iter().zip(&local.rows).enumerate() {
        assert_eq!(
            row.overrides, point.overrides,
            "point {at} is ranked where the whole search ranks it"
        );
        let table = point.report.as_ref().map(TradingResultsTable::from_report);
        // Through JSON, where a non-finite number reads `null` on both sides.
        let as_json = |t: &Option<TradingResultsTable>| serde_json::to_value(t).expect("encodes");
        assert_eq!(as_json(&row.report), as_json(&table), "point {at}'s metric table");
        assert_eq!(row.error, point.error);
        let report = row.report.as_ref().expect("every point ran");
        assert!(report.metric("n_trades").is_some_and(|n| n > 0.0), "point {at} traded");
        assert!(report.metric("sharpe").is_some() && report.metric("max_drawdown").is_some());
    }

    let (text, manifest) = saved(runs.path(), &summary);
    assert_eq!(text, format!("{whole}\n"), "the saved report is the whole report, byte for byte");
    assert_eq!(manifest.kind, vike_runs::BACKTEST_RUN_KIND);
    assert_eq!(manifest.detail["protocol"], "paramscan");
    assert_eq!(
        manifest.strategy_file(),
        Some("trend/momentum.rs"),
        "the retention counts it by its file"
    );
    let dir = vike_runs::finished_run_dir(runs.path(), &summary.saved_run).expect("the run");
    assert_eq!(vike_runs::read_config_toml(&dir).expect("its config"), profile);

    // The rows a page at a time: the summary's own first page, then the rest.
    let mut client = BacktesthubClient::connect(saving).expect("connect");
    let pages: Vec<_> = (0..2)
        .map(|page| client.read_paramscan_rows(&summary.run, page, 3).expect("a page").rows)
        .collect();
    assert_eq!(pages.concat(), summary.rows, "two pages of three hold the four rows in order");
}

/// **The defect this exists for**: a search whose WHOLE report is over the 64 MiB frame cap answers its summary. The
/// plain verb's answer is that report (its JSON text is at least that long in a JSON frame and on a sandboxed child's
/// pipe, which carries it as one string), so it could not cross; the summarized search crosses one page of metric
/// tables at a time (here seven pages), and its saved run still holds every point whole.
#[test]
fn a_search_whose_whole_report_is_over_the_frame_cap_answers_its_summary() {
    let store = store_of(2_000);
    let runs = tempfile::tempdir().expect("a runs root");
    let saving = spawn_server_over_saving_to(
        Arc::clone(&store),
        vike_strategy_lab::stand_in,
        runs.path().to_path_buf(),
    );
    // Thousands of points, each its whole report in the plain verb's answer.
    let qty: Vec<String> = (0..100).map(|i| format!("{:.2}", 1.0 + f64::from(i) / 100.0)).collect();
    let limits: Vec<String> = (1..=70).map(|minutes| (minutes * 60_000).to_string()).collect();
    let grid = format!("qty = [{}]\ntime_limit_ms = [{}]", qty.join(", "), limits.join(", "));
    let profile = search_profile(60_000, &grid);
    let summary = summarized(&mut BacktesthubClient::connect(saving).expect("connect"), &profile);

    let points = 100 * 70;
    assert_eq!(
        (summary.total, summary.rows.len()),
        (points, points),
        "every point, over every page"
    );
    assert_eq!(summary.page_size, PARAMSCAN_ROWS_PAGE_SIZE, "read in pages of the default size");
    let page = ParamscanRowsPageWire::of(&summary.rows, 0, summary.page_size);
    let frame = serde_json::to_vec(&page).expect("encodes").len();
    assert!(
        frame < (MAX_FRAME_LEN / 8) as usize,
        "a page of metric tables is a small frame: {frame} bytes"
    );
    let (text, _) = saved(runs.path(), &summary);
    let whole = text.len();
    assert!(
        whole > MAX_FRAME_LEN as usize,
        "the planted search must be bigger than the old limit: its whole report is {whole} bytes, the cap \
         {MAX_FRAME_LEN}"
    );
    // One JSON document, written a row at a time (read through without building it).
    serde_json::from_str::<serde::de::IgnoredAny>(&text).expect("the saved report is one document");
    assert!(summary.rows.iter().all(|row| row.report.is_some()), "every point ran");
    println!(
        "whole report {whole} bytes (cap {MAX_FRAME_LEN}); {points} points; a page of rows {frame} bytes"
    );
}

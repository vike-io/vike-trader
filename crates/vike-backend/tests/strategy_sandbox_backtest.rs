//! **A sandboxed run writes the bytes the in-process run writes** (plan 2b, F4): one strategy FILE, the
//! same requests, answered once in THIS process (the artifact loaded by hand, the hub's own dispatch over
//! the strategy files a sandboxed child answers with) and once by a real hub whose route hands them to a
//! `vike-backend strategy-sandbox` child. The answers' frame bodies must be equal byte for byte: a single
//! run, an 8-point sweep, a walk-forward and a profile run.
//!
//! The in-process half names the loader, which `tests/strategy_file_run.rs` forbids itself so that it
//! proves the production route; that is why this is a separate file.
//!
//! [`measure_the_sandboxs_spawn_and_data_cost`] is the run-cost MEASUREMENT (ignored; run it alone):
//!
//! ```text
//! LANE=the build runner MSYS_NO_PATHCONV=1 just the CI box <branch> cargo test --release -p vike-backend --test strategy_sandbox_backtest -- --ignored --nocapture
//! ```
#![cfg(all(feature = "vike-strategy-sandbox", target_os = "linux"))]

#[path = "support/strategy_file.rs"]
mod strategy_file;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_backtesthub::test_support::{
    DayStamps, answer_in_a_child, answer_in_this_process, spawn_hub_over, stamping,
};
use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::wire_studio::{
    DataSliceWire, ParamscanWire, ReplayKindWire, StrategyEngineConfigWire, WalkforwardWire,
};
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_math::statistics::high_median_of_sorted;
use vike_model::Strategy;
use vike_model::params::ParamValue;
use vike_simulator::OrderBrokerSimulator;

use strategy_file::{
    FILE, SOURCE, ask, ask_raw, build_the_artifact, enter_a_project_holding,
    sandbox_children_once_reaped, seeded_store, slice, spec, use_the_multicall_as_the_sandbox,
};

/// The artifact loaded in THIS process, as the child loads it: the build the in-process half runs (the
/// fixture exports no `refuse`, so it refuses nothing).
fn loaded_here(artifact: &Path) -> vike_backtesthub::studio_run::StrategyFileBuild {
    let vtable = vike_rust_strategy::loader::load(artifact).expect("load in this process").vtable;
    Box::new(move |params_toml: &str, facts_json: &str| {
        Ok(Box::new(vike_rust_strategy::host::StrategyFileHost::<OrderBrokerSimulator>::new(
            vtable,
            params_toml,
            facts_json,
        )) as Box<dyn Strategy<OrderBrokerSimulator>>)
    })
}

/// The first byte two bodies differ at, for a failure a reader can act on.
fn first_difference(a: &[u8], b: &[u8]) -> String {
    let at = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    let window = |s: &[u8]| {
        String::from_utf8_lossy(&s[at.saturating_sub(80).min(s.len())..(at + 80).min(s.len())])
            .into_owned()
    };
    format!(
        "lengths {} vs {}; first difference at byte {at}:\n  {}\n  {}",
        a.len(),
        b.len(),
        window(a),
        window(b)
    )
}

/// A profile over the seeded walk naming [`FILE`], with `extra` appended (a `[walkforward]` table).
fn profile(extra: &str) -> String {
    format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"0\"\nto = \"100000000\"\n\n[engine]\ncash = 100000.0\n\n[strategy]\nfile = \
         \"{FILE}\"\n[strategy.params]\nevery = 5\n{extra}"
    )
}

#[test]
fn a_sandboxed_run_writes_the_bytes_the_in_process_run_writes() {
    // This test plays the hub, so it runs the boot's step 0 first, as a root does: no descriptor it
    // inherited (CI's runner hands it a pipe) reaches the strategy process, which would refuse it.
    let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
    let artifact = build_the_artifact();
    let (_project, strategies, restore) = enter_a_project_holding(&artifact);
    use_the_multicall_as_the_sandbox();
    let store = seeded_store(400);
    let address = spawn_hub_over(Arc::clone(&store), strategies.clone());

    let eight_points = ParamscanWire {
        axes: vec![(
            "every".to_string(),
            [3, 4, 5, 7, 9, 11, 13, 17].into_iter().map(ParamValue::Int).collect(),
        )],
    };
    let requests = [
        Request::RunSlice { spec: spec(5, 0), slice: slice(), params: None },
        Request::RunParamscan {
            spec: spec(5, 0),
            slice: slice(),
            paramscan: eight_points,
            params: None,
        },
        Request::RunWalkforward {
            spec: spec(5, 0),
            slice: slice(),
            walkforward: WalkforwardWire::fixed(4),
            params: None,
        },
        Request::RunBacktest(profile("")),
    ];
    for request in requests {
        let here = answer_in_this_process(
            request.clone(),
            &store,
            loaded_here(&artifact),
            SOURCE.to_string(),
        );
        let here = serde_json::to_vec(&here).expect("encode the in-process answer");
        let there = ask_raw(address, &request);
        assert!(
            there == here,
            "the sandboxed answer differs from the in-process one: {}",
            first_difference(&there, &here)
        );
        // Not two equal refusals: each answer is the run it asked for.
        match serde_json::from_slice::<Response>(&there).expect("decode") {
            Response::RunResult(r) => assert!(r.n_trades > 0, "the run traded"),
            Response::ParamscanResult(r) => assert_eq!(r.entries.len(), 8, "eight points"),
            Response::WalkforwardResult(_) | Response::Report(_) => {}
            other => panic!("not a run: {other:?}"),
        }
        println!("identical: {} bytes", there.len());
    }
    match ask(address, &Request::RunSlice { spec: spec(5, 0), slice: slice(), params: None }) {
        Response::RunResult(_) => {}
        other => panic!("{FILE} must run on a second request too: {other:?}"),
    }
    // A SUMMARIZED run of the file (decision 0078's amendment of 2026-11-16): run in the child, then
    // summarized by the hub, which holds its trades. Its curve and trades are the in-process run's.
    let run = Request::RunSlice { spec: spec(5, 0), slice: slice(), params: None };
    let Response::RunResult(whole) =
        answer_in_this_process(run.clone(), &store, loaded_here(&artifact), SOURCE.to_string())
    else {
        panic!("the in-process run answers its whole result");
    };
    let whole_curve: Vec<u64> = whole.equity_curve.iter().map(|v| v.to_bits()).collect();
    let (local, local_trades) = vike_backtesthub_client::summary::summarize(whole, 1.0);
    let Response::RunSummary(summary) = ask(address, &Request::Summarized(Box::new(run))) else {
        panic!("a summarized run of {FILE} answers its summary");
    };
    assert_eq!((summary.curve.clone(), summary.n_trades), (local.curve, local.n_trades));
    // Its WHOLE curve (decision 0078's amendment of 2026-11-18): streamed up by the child in parts, held by the hub,
    // read back in one page, the in-process run's curve to the bit.
    let read = Request::ReadCurve { run: summary.run.clone(), page: 0, page_size: 500_000 };
    match ask(address, &read) {
        Response::CurvePage(page) => {
            let paged: Vec<u64> = page.equity.iter().map(|v| v.to_bits()).collect();
            assert_eq!(paged, whole_curve, "the child's whole curve, held by the hub");
        }
        other => panic!("the hub holds the sandboxed run's whole curve: {other:?}"),
    }
    let page =
        Request::ReadTrades { run: summary.run.clone(), point: None, page: 0, page_size: 1000 };
    match ask(address, &page) {
        Response::TradesPage(page) => {
            assert_eq!(page.trades, local_trades[..local_trades.len().min(1000)]);
        }
        other => panic!("the hub holds the sandboxed run's trades: {other:?}"),
    }
    a_summarized_walk_forward_carries_its_times_out_of_the_child(address);
    the_funding_switch_reaches_the_child(strategies.clone());
    a_sixty_point_summarized_scan_answers_and_opens_any_point(address, &store, &artifact);
    a_summarized_profile_search_hands_up_its_rows_through_the_child(
        address,
        &store,
        &artifact,
        strategies.clone(),
    );
    a_pinned_run_is_held_and_saved_as_its_log(&store, strategies.clone(), &artifact);
    a_two_mount_portfolio_runs_in_one_child(strategies.clone(), &artifact);
    a_saving_hub_saves_every_run_its_child_answered(address, &store, strategies);
    assert!(sandbox_children_once_reaped().is_empty(), "no child outlives the request it served");
    std::env::set_current_dir(restore).expect("restore the working directory");
}

/// **A `[[mounts]]` PORTFOLIO of two `.rs` mounts runs in ONE sandboxed child** (decision 0300, decision 0281): the
/// file twice, each mount on its own symbol with its own params (`every` 5 and 7), answered by a real hub's child
/// with the bytes this process writes with each mount's file loaded here, and its report's mount table carrying
/// each mount's own trades on its own symbol.
fn a_two_mount_portfolio_runs_in_one_child(strategies: std::path::PathBuf, artifact: &Path) {
    let store = HistoryStoreMemory::new();
    for symbol in ["BTCUSDT", "ETHUSDT"] {
        let bars =
            vike_studio_core::test_support::walk::random_walk_bars(400, 60_000, Some(symbol));
        store.append_bars("binance", symbol, "1m", &bars, None).expect("seed bars");
    }
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let address = spawn_hub_over(Arc::clone(&store), strategies);
    let mount = |symbol: &str, every: i64| {
        format!(
            "\n[[mounts]]\nsymbols = [\"{symbol}\"]\n[mounts.strategy]\nfile = \"{FILE}\"\n\
             [mounts.strategy.params]\nevery = {every}\n"
        )
    };
    let profile = format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\", \"ETHUSDT\"]\nkind = \"bar\"\n\
         interval = \"1m\"\nfrom = \"0\"\nto = \"100000000\"\n\n[engine]\ncash = 100000.0\n{}{}",
        mount("BTCUSDT", 5),
        mount("ETHUSDT", 7)
    );
    let request = Request::RunBacktest(profile);
    let here = vike_backtesthub::test_support::answer_in_this_process_of_mounts(
        request.clone(),
        &store,
        vec![
            (loaded_here(artifact), SOURCE.to_string()),
            (loaded_here(artifact), SOURCE.to_string()),
        ],
    );
    let here = serde_json::to_vec(&here).expect("encode the in-process answer");
    let there = ask_raw(address, &request);
    assert!(
        there == here,
        "the sandboxed portfolio differs from the in-process one: {}",
        first_difference(&there, &here)
    );
    let Response::Report(report) = serde_json::from_slice::<Response>(&there).expect("decode")
    else {
        panic!("a portfolio answers its report: {}", String::from_utf8_lossy(&there));
    };
    let report: serde_json::Value = serde_json::from_str(&report).expect("a report");
    let rows = report["mounts"]["rows"].as_array().expect("the report's mount table");
    assert_eq!(rows.len(), 2, "{report}");
    for (row, symbol) in rows.iter().zip(["BTCUSDT", "ETHUSDT"]) {
        assert_eq!(row["symbols"], serde_json::json!([symbol]), "{row}");
        assert!(row["n_trades"].as_u64().is_some_and(|n| n > 0), "each mount trades: {row}");
    }
    assert_ne!(rows[0]["n_trades"], rows[1]["n_trades"], "each mount ran its own params");
    assert_eq!(report["strategy_file"], serde_json::json!(format!("{FILE} + {FILE}")));
}

/// **A summarized walk-forward of a `.rs` file carries its dates out of the sandboxed child** (2026-11-20): the
/// stitched curve's times live in memory only (`WalkforwardResults::equity_timestamp` is never serialized), so the
/// walk must be summarized IN the child, where they are, and its summary must reach the client with one time per
/// thinned point, inside the slice, in order, and each window's dates beside its bars.
fn a_summarized_walk_forward_carries_its_times_out_of_the_child(address: std::net::SocketAddr) {
    let walk = Request::RunWalkforward {
        spec: spec(5, 0),
        slice: slice(),
        walkforward: WalkforwardWire::fixed(4),
        params: None,
    };
    let Response::WalkforwardSummary(summary) = ask(address, &Request::Summarized(Box::new(walk)))
    else {
        panic!("a summarized walk-forward of {FILE} answers its summary");
    };
    let times = &summary.equity_curve_timestamp;
    assert!(!times.is_empty(), "the child's summary carries the stitched curve's times");
    assert_eq!(times.len(), summary.equity_curve.len(), "one time per thinned point");
    assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "in time order");
    for step in &summary.steps {
        let window = step.window_times.expect("each window carries its dates");
        assert!(window.test_start <= window.test_end, "{window:?}");
        assert!(times[0] <= window.test_end && window.test_start <= times[times.len() - 1]);
    }
}

/// **The funding switch reaches the sandboxed child** (2026-11-20): the file on a perpetual whose store holds funding
/// settlements pays none with `attach_funding` OFF or absent (off since 2026-11-21) and some with it ON, so the hub
/// forwards the switch into the child's slice profile rather than dropping it.
fn the_funding_switch_reaches_the_child(strategies: std::path::PathBuf) {
    const PERP: &str = "BTCUSDT.P";
    let hour = 3_600_000;
    let store = HistoryStoreMemory::new();
    let bars = vike_studio_core::test_support::walk::random_walk_bars(48, hour, Some(PERP));
    // A settlement every 8 hours, its mark the bar's close.
    let settlements: Vec<_> = bars
        .iter()
        .step_by(8)
        .skip(1)
        .map(|bar| {
            let mut row = bar.clone();
            row.funding = Some(1e-3);
            row
        })
        .collect();
    store.append_bars("bybit", PERP, "1h", &bars, None).expect("seed bars");
    store.append_bars("bybit", PERP, "funding", &settlements, None).expect("seed funding");
    let address = spawn_hub_over(Arc::new(store), strategies);
    let funding_paid = |attach_funding: Option<bool>| {
        let slice = DataSliceWire {
            venue: "bybit".to_string(),
            symbols: vec![PERP.to_string()],
            interval: "1h".to_string(),
            start: None,
            end: None,
            kind: ReplayKindWire::Bars,
        };
        let params = Some(StrategyEngineConfigWire { attach_funding, ..Default::default() });
        let run = Request::RunSlice { spec: spec(3, 0), slice: Box::new(slice), params };
        match ask(address, &run) {
            Response::RunResult(result) => result.funding_paid,
            other => panic!("the file runs on the perpetual: {other:?}"),
        }
    };
    assert_eq!(funding_paid(Some(false)), 0.0, "switched off in the child: no funding");
    assert_eq!(funding_paid(None), 0.0, "absent in the child: no funding");
    assert_ne!(funding_paid(Some(true)), 0.0, "switched on: the stored settlements are charged");
}

/// Book W35 through a REAL sandboxed child (the owner's word: every `.rs` backtest is a run): a SAVING hub answers each
/// Studio verb and a profile run of the file with the bytes the plain hub (`plain`) writes, and keeps one run per
/// request, kind `backtest`, its protocol and the file under `detail`. Each single run keeps the trade ledger its child
/// sent up (the summarized Run's included: the child summarized it, and its saved run came up in its own frame).
fn a_saving_hub_saves_every_run_its_child_answered(
    plain: std::net::SocketAddr,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    strategies: std::path::PathBuf,
) {
    let runs = tempfile::tempdir().expect("a runs root");
    let saving = vike_backtesthub::test_support::spawn_hub_over_saving_to(
        Arc::clone(store),
        strategies,
        runs.path().to_path_buf(),
    );
    let three = ParamscanWire {
        axes: vec![("every".to_string(), [3, 5, 9].into_iter().map(ParamValue::Int).collect())],
    };
    let requests = [
        Request::RunSlice { spec: spec(5, 0), slice: slice(), params: None },
        Request::RunParamscan { spec: spec(5, 0), slice: slice(), paramscan: three, params: None },
        Request::RunWalkforward {
            spec: spec(5, 0),
            slice: slice(),
            walkforward: WalkforwardWire::fixed(4),
            params: None,
        },
        Request::RunBacktest(profile("")),
    ];
    for request in &requests {
        let (saved, unsaved) = (ask_raw(saving, request), ask_raw(plain, request));
        assert!(
            saved == unsaved,
            "saving changed the answer: {}",
            first_difference(&saved, &unsaved)
        );
    }
    let run = Request::RunSlice { spec: spec(5, 0), slice: slice(), params: None };
    let Response::RunSummary(summary) = ask(saving, &Request::Summarized(Box::new(run))) else {
        panic!("the saving hub answers the Studio's Run with its summary");
    };
    // A memory store stamps no day, so nothing pins the bars: the child sent its curve, and the saved run keeps it.
    let kept = vike_runs::finished_run_dir(runs.path(), &summary.saved_run).expect("the Run's run");
    assert!(vike_runs::read_equity_curve(&kept).is_ok(), "an unpinned run keeps its curve file");
    assert!(vike_runs::read_equity_log(&kept).is_err(), "and no log");

    let scan = vike_runs::scan_runs(runs.path());
    assert!(scan.diagnostics.is_empty(), "{:?}", scan.diagnostics);
    let mut protocols = Vec::new();
    for run in &scan.runs {
        assert_eq!(run.manifest.kind, vike_runs::BACKTEST_RUN_KIND);
        assert_eq!(run.manifest.strategy_file(), Some(FILE), "counted under the file");
        let protocol = run.manifest.detail["protocol"].as_str().expect("a protocol").to_string();
        if protocol == "single" {
            let ledger = vike_runs::read_trades(&run.dir).expect("the child's ledger was saved");
            assert!(!ledger.trades.is_empty(), "the run traded, and its trades came up whole");
        }
        protocols.push(protocol);
    }
    protocols.sort();
    assert_eq!(protocols, ["paramscan", "single", "single", "single", "walkforward"]);
    assert!(summary.n_trades > 0, "the summarized Run traded");
}

/// **The hub holds a pinned run's event log, not its curve** (decision 0078's amendment of 2026-11-19 on what the hub
/// holds), through a REAL sandboxed child: over a store that stamps every day of its bars, settled, a summarized single
/// run's child sends its log in its curve's place, the saving hub keeps that log and no curve file in the run it saves,
/// and the whole curve read back by the held name (rebuilt from the held log) and by the saved run's id (from the
/// saved log) is the in-process run's, to the bit.
fn a_pinned_run_is_held_and_saved_as_its_log(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    strategies: std::path::PathBuf,
    artifact: &Path,
) {
    let pinning = stamping(Arc::clone(store), DayStamps::Settled);
    let runs = tempfile::tempdir().expect("a runs root");
    let hub = vike_backtesthub::test_support::spawn_hub_over_saving_to(
        pinning,
        strategies,
        runs.path().to_path_buf(),
    );
    let run = Request::RunSlice { spec: spec(5, 0), slice: slice(), params: None };
    let Response::RunResult(whole) =
        answer_in_this_process(run.clone(), store, loaded_here(artifact), SOURCE.to_string())
    else {
        panic!("the in-process run answers its whole result");
    };
    let Response::RunSummary(summary) = ask(hub, &Request::Summarized(Box::new(run))) else {
        panic!("a summarized run of {FILE} answers its summary");
    };
    let bits = |curve: &[f64]| curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    for name in [&summary.run, &summary.saved_run] {
        match ask(hub, &Request::ReadCurve { run: name.clone(), page: 0, page_size: 500_000 }) {
            Response::CurvePage(page) => {
                assert_eq!(bits(&page.equity), bits(&whole.equity_curve), "{name}: to the bit");
                assert_eq!(page.timestamp, whole.equity_timestamp, "{name}: every step's time");
            }
            other => panic!("{name}: the hub reads the pinned run's whole curve: {other:?}"),
        }
    }
    let dir = vike_runs::finished_run_dir(runs.path(), &summary.saved_run).expect("the run saved");
    assert!(vike_runs::read_equity_log(&dir).is_ok(), "the saved run keeps the log the child sent");
    assert!(vike_runs::read_equity_curve(&dir).is_err(), "and no curve file");
}

/// A SUMMARIZED 60-point scan of the file (the coordinator's ruling of 2026-11-16: a `.rs` scan has no
/// point-count ceiling below the in-process one): the child summarizes each point and streams it, the
/// hub answers every point, opens any of them, and pages its trades, which are the in-process scan's.
fn a_sixty_point_summarized_scan_answers_and_opens_any_point(
    address: std::net::SocketAddr,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    artifact: &Path,
) {
    let sixty =
        ParamscanWire { axes: vec![("every".to_string(), (3..63).map(ParamValue::Int).collect())] };
    let scan =
        Request::RunParamscan { spec: spec(5, 0), slice: slice(), paramscan: sixty, params: None };
    let Response::ParamscanResult(whole) =
        answer_in_this_process(scan.clone(), store, loaded_here(artifact), SOURCE.to_string())
    else {
        panic!("the in-process scan answers its whole result");
    };
    let Response::ParamscanSummary(summary) = ask(address, &Request::Summarized(Box::new(scan)))
    else {
        panic!("a summarized 60-point scan of {FILE} answers its summary");
    };
    assert_eq!(summary.points.len(), 60);
    for (point, entry) in summary.points.iter().zip(&whole.entries) {
        assert_eq!(point.overrides, entry.overrides, "ranked as the in-process scan ranked");
        assert_eq!(point.n_trades, entry.result.n_trades);
    }
    for at in [0, 37, 59] {
        let Response::RunSummary(opened) =
            ask(address, &Request::ReadPoint { run: summary.run.clone(), point: at })
        else {
            panic!("point {at} opens");
        };
        let (local, local_trades) =
            vike_backtesthub_client::summary::summarize(whole.entries[at].result.clone(), 1.0);
        assert_eq!((opened.point, opened.curve.clone()), (Some(at), local.curve));
        let page = Request::ReadTrades {
            run: summary.run.clone(),
            point: Some(at),
            page: 0,
            page_size: 1000,
        };
        match ask(address, &page) {
            Response::TradesPage(page) => {
                assert_eq!(page.total, local_trades.len());
                assert_eq!(page.trades, local_trades[..local_trades.len().min(1000)]);
            }
            other => panic!("the hub holds point {at}'s trades: {other:?}"),
        }
    }
}

/// A SUMMARIZED PROFILE search of the file through a real sandboxed child (decision 0078's amendment of 2026-11-19):
/// the child hands up each row in its own frame, the hub answers each point's overrides and metric table (the very
/// rows the in-process search summarizes, and the whole search's numbers), and a saving hub's run is the plain verb's
/// whole report, byte for byte, written row by row as the rows came up.
fn a_summarized_profile_search_hands_up_its_rows_through_the_child(
    plain: std::net::SocketAddr,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    artifact: &Path,
    strategies: std::path::PathBuf,
) {
    let runs = tempfile::tempdir().expect("a runs root");
    let saving = vike_backtesthub::test_support::spawn_hub_over_saving_to(
        Arc::clone(store),
        strategies,
        runs.path().to_path_buf(),
    );
    let search = Request::RunParamscanProfile {
        profile_toml: profile("\n[paramscan]\nevery = [3, 4, 5, 7, 9, 11]\n"),
        rank_by: None,
        search: None,
    };
    let Response::ParamscanReport(whole) = ask(plain, &search) else {
        panic!("the plain search of {FILE} answers its whole report");
    };
    let summarized = Request::Summarized(Box::new(search));
    let Response::ParamscanProfileSummary(here) = answer_in_this_process(
        summarized.clone(),
        store,
        loaded_here(artifact),
        SOURCE.to_string(),
    ) else {
        panic!("the in-process summarized search answers its summary");
    };
    let Response::ParamscanProfileSummary(there) = ask(saving, &summarized) else {
        panic!("a summarized search of {FILE} answers its summary");
    };
    let rows = |s: &vike_backtesthub_client::summary::ParamscanProfileSummaryWire| {
        serde_json::to_string(&s.rows).expect("encode the rows")
    };
    assert_eq!(rows(&there), rows(&here), "the sandboxed rows are the in-process rows");
    let whole_report: serde_json::Value = serde_json::from_str(&whole).expect("the whole report");
    let sdk = whole_report["strategy_sdk"].as_u64().and_then(|n| u32::try_from(n).ok());
    assert_eq!(there.strategy_sdk, sdk, "the summary states the SDK the whole report states");
    let whole_rows = whole_report["rows"].as_array().expect("its rows");
    assert_eq!((there.total, there.rows.len(), whole_rows.len()), (6, 6, 6));
    for (row, point) in there.rows.iter().zip(whole_rows) {
        let metric = |id: &str| row.report.as_ref().and_then(|table| table.metric(id));
        assert_eq!(serde_json::to_value(&row.overrides).expect("encode"), point["overrides"]);
        assert_eq!(metric("n_trades"), point["report"]["n_trades"].as_f64(), "{point}");
        assert_eq!(metric("sharpe"), point["report"]["sharpe"].as_f64(), "{point}");
        assert!(metric("n_trades").is_some_and(|n| n > 0.0), "each point traded");
    }
    let dir = vike_runs::finished_run_dir(runs.path(), &there.saved_run).expect("the saved run");
    let saved = std::fs::read_to_string(dir.join(vike_runs::REPORT_FILE)).expect("its report");
    assert_eq!(saved, format!("{whole}\n"), "the saved run is the plain verb's whole report");
    assert_eq!(
        vike_runs::read_manifest(&dir).expect("its manifest").detail["protocol"],
        "paramscan"
    );
}

/// The median of `n` timed runs of `f`.
fn median(n: usize, mut f: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    times.sort();
    high_median_of_sorted(&times).expect("at least one timed run")
}

/// **MEASURED, not estimated** (plan 2b section 5): what a sandboxed run costs over the in-process one.
///
/// 1. Spawn, jail and load: the 400-bar slice, ten runs each way, medians.
/// 2. The data: `crates/vike-backtest/profiles/wf_momentum.toml`'s window and walk (BTCUSDT 1m over its
///    24 months, six anchored windows) over 1,051,200 SYNTHETIC bars of that window in a memory store,
///    run by [`FILE`] (the registry's `momentum` is no file on this branch), in this process and in a
///    child. The difference is the spawn and the 1.05M bars crossing the pipe; the answers must be equal.
#[test]
#[ignore = "a measurement: run it alone with --ignored --nocapture, release build, on a lane"]
fn measure_the_sandboxs_spawn_and_data_cost() {
    use_the_multicall_as_the_sandbox();
    let artifact = build_the_artifact();
    let (_project, strategies, restore) = enter_a_project_holding(&artifact);

    let small = seeded_store(400);
    let run = Request::RunSlice { spec: spec(5, 0), slice: slice(), params: None };
    let here = median(10, || {
        let _ = answer_in_this_process(run.clone(), &small, loaded_here(&artifact), SOURCE.into());
    });
    let there = median(10, || {
        let _ = answer_in_a_child(&run, &small, strategies.clone()).expect("a .rs file run");
    });
    println!(
        "spawn+jail+load (400-bar slice, median of 10): in-process {here:?}, sandboxed {there:?}, \
         cost {:?}",
        there.saturating_sub(here)
    );

    let walkforward_momentum = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../vike-backtest/profiles/wf_momentum.toml"),
    )
    .expect("read wf_momentum.toml");
    let window = |key: &str| {
        walkforward_momentum
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key} = ")))
            .map(|v| v.trim_matches('"').to_string())
            .expect("the profile's window")
    };
    let (from, to) = (window("from"), window("to"));
    let n_splits = window("n_splits");
    let walk_toml = format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"{from}\"\nto = \"{to}\"\n\n[engine]\ncash = 100000.0\n\n\
         [strategy]\nfile = \"{FILE}\"\n[strategy.params]\nevery = 5\ncommission = 0.1\n\n[walkforward]\nn_splits = \
         {n_splits}\n"
    );
    let n = 1_051_200;
    let start: i64 = from.parse().expect("from is epoch ms");
    let mut bars =
        vike_studio_core::test_support::walk::random_walk_bars(n, 60_000, Some("BTCUSDT"));
    let offset = start - bars[0].timestamp;
    for bar in &mut bars {
        bar.timestamp += offset;
    }
    let big = HistoryStoreMemory::new();
    big.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("seed 1.05M bars");
    let big: Arc<dyn HistoryStore + Send + Sync> = Arc::new(big);
    let walk = Request::RunWalkforwardProfile { profile_toml: walk_toml };
    let (mut here, mut there) = (Vec::new(), Vec::new());
    // Three walks each way, medians: one walk is a second, and a lane's load moves within one.
    let here_took = median(3, || {
        let answer =
            answer_in_this_process(walk.clone(), &big, loaded_here(&artifact), SOURCE.into());
        here = serde_json::to_vec(&answer).expect("encode");
    });
    let there_took = median(3, || {
        let answer = answer_in_a_child(&walk, &big, strategies.clone()).expect("a .rs file run");
        there = serde_json::to_vec(&answer).expect("encode");
    });
    assert!(there == here, "the walk differs: {}", first_difference(&there, &here));
    assert!(
        matches!(serde_json::from_slice::<Response>(&here), Ok(Response::WalkforwardReport(_))),
        "the measurement must be a walk, not two equal refusals: {}",
        String::from_utf8_lossy(&here[..here.len().min(400)])
    );
    let cost = there_took.as_secs_f64() - here_took.as_secs_f64();
    println!(
        "wf_momentum window over {n} bars (median of 3): in-process {here_took:?}, sandboxed \
         {there_took:?}, cost {:+.1} ms ({:+.2}% of the in-process run)",
        cost * 1e3,
        100.0 * cost / here_took.as_secs_f64()
    );
    std::env::set_current_dir(restore).expect("restore the working directory");
}

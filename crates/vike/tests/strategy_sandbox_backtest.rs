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
//! LANE=vike-fresh3 MSYS_NO_PATHCONV=1 just the CI box <branch> cargo test --release -p vike-backend --test strategy_sandbox_backtest -- --ignored --nocapture
//! ```
#![cfg(all(feature = "vike-strategy-sandbox", target_os = "linux"))]

#[path = "support/strategy_file.rs"]
mod strategy_file;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_backtesthub::test_support::{answer_in_a_child, answer_in_this_process, spawn_hub_over};
use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::wire_studio::{ParamscanWire, WalkforwardWire};
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_model::Strategy;
use vike_model::params::ParamValue;
use vike_simulator::OrderBrokerSimulator;

use strategy_file::{
    FILE, SOURCE, ask, ask_raw, build_the_artifact, enter_a_project_holding, sandbox_children,
    seeded_store, slice, spec, use_the_multicall_as_the_sandbox,
};

/// The artifact loaded in THIS process, as the child loads it: the build the in-process half runs.
fn loaded_here(artifact: &Path) -> vike_backtesthub::studio_run::StrategyFileBuild {
    let vtable = vike_rust_strategy::loader::load(artifact).expect("load in this process").vtable;
    Box::new(move |params_toml: &str, facts_json: &str| {
        Box::new(vike_rust_strategy::host::StrategyFileHost::<OrderBrokerSimulator>::new(
            vtable,
            params_toml,
            facts_json,
        )) as Box<dyn Strategy<OrderBrokerSimulator>>
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
    let addr = spawn_hub_over(Arc::clone(&store), strategies);

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
        let there = ask_raw(addr, &request);
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
    match ask(addr, &Request::RunSlice { spec: spec(5, 0), slice: slice(), params: None }) {
        Response::RunResult(_) => {}
        other => panic!("{FILE} must run on a second request too: {other:?}"),
    }
    assert!(sandbox_children().is_empty(), "no child outlives the request it served");
    std::env::set_current_dir(restore).expect("restore the working directory");
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
    times[n / 2]
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

    let wf_momentum = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../vike-backtest/profiles/wf_momentum.toml"),
    )
    .expect("read wf_momentum.toml");
    let window = |key: &str| {
        wf_momentum
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key} = ")))
            .map(|v| v.trim_matches('"').to_string())
            .expect("the profile's window")
    };
    let (from, to) = (window("from"), window("to"));
    let n_splits = window("n_splits");
    let walk_toml = format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"{from}\"\nto = \"{to}\"\n\n[engine]\ncash = 100000.0\nfee_rate = 0.001\n\n\
         [strategy]\nfile = \"{FILE}\"\n[strategy.params]\nevery = 5\n\n[walkforward]\nn_splits = \
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

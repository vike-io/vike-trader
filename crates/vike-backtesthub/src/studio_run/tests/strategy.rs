//! Basic runs, native registry strategies, and the strategy file arm and its lookup directory.

use std::assert_matches;
use std::path::PathBuf;
use vike_data::TimestampRange;
use vike_model::params::{ParamValue, Params};

use super::support::{rhai, seeded_store, slice};
use crate::studio_run::strategy::strategy_files_dir_from;
use crate::studio_run::{
    build_strategy, build_strategy_with, run_paramscan_slice, run_slice, run_walkforward_slice,
    strategy_files_dir,
};
// The trade-counting tests run the long-only cross: the slice is binance `BTCUSDT`, a spot pair by its name.
use vike_studio_core::test_support::cross_script::{CROSS_SCRIPT, LONG_CROSS_SCRIPT};
use vike_studio_core::{DataSlice, RunError, StrategySpec, params_from_rows, spawn_outcome};

#[test]
fn run_slice_backtests_over_the_store() {
    let (_dir, store, _bars) = seeded_store();
    let out = run_slice(
        &rhai(LONG_CROSS_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice(),
        &store,
        None,
    );
    let res = out.expect("should backtest");
    assert!(res.n_trades > 0, "the cross should trade over 400 bars");
}

/// A refused order reaches the Studio (the owner's ruling of 2026-11-12, part C): over the spot
/// slice the reversing cross has its shorts refused by the spot law, and the `RunSlice` answer
/// carries each refusal in the gate's own words. The long-only cross refuses nothing, so its answer
/// carries none.
#[test]
fn a_run_slice_answer_carries_the_orders_the_run_refused() {
    let (_dir, store, _b) = seeded_store();
    let wire_slice = vike_studio_core::wire_run::to_wire_slice(&slice());
    let answer = |src: &str| {
        let spec = vike_studio_core::wire_run::to_wire_spec(&rhai(src)).expect("a rhai spec");
        crate::studio_run::run_slice_local(
            &spec,
            &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
            &wire_slice,
            None,
            store.clone(),
        )
        .expect("runs")
    };
    let reversing = answer(CROSS_SCRIPT).refused_orders;
    assert!(reversing.iter().map(|(_, n)| n).sum::<u64>() > 0, "the shorts are refused on spot");
    assert!(
        reversing.iter().all(|(d, _)| d.reason.starts_with("a spot account cannot sell more")),
        "{reversing:?}"
    );
    assert!(
        answer(LONG_CROSS_SCRIPT).refused_orders.is_empty(),
        "the long-only cross refuses none"
    );
}

#[test]
fn compile_error_maps_to_runerror_compile() {
    let (_dir, store, _b) = seeded_store();
    let out = run_slice(
        &rhai("fn on_bar( {"),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice(),
        &store,
        None,
    );
    assert_matches!(out, Err(RunError::Compile(_)));
}

#[test]
fn empty_slice_maps_to_runerror_data() {
    let (_dir, store, _b) = seeded_store();
    let missing = DataSlice::bars("binance", "NOPE", "1m", TimestampRange::all());
    let out = run_slice(
        &rhai(CROSS_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &missing,
        &store,
        None,
    );
    assert_matches!(out, Err(RunError::Data(_)));
}

/// A run handed to the ONE worker spawn (`spawn_outcome`) delivers the synchronous run's outcome.
#[test]
fn a_run_on_the_worker_spawn_delivers_the_same_outcome() {
    let (_dir, store, _b) = seeded_store();
    let (spec, data_slice) = (rhai(LONG_CROSS_SCRIPT), slice());
    let rx = spawn_outcome(move || {
        run_slice(
            &spec,
            &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
            &data_slice,
            &store,
            None,
        )
    });
    let out = rx.recv().expect("worker sends one outcome");
    assert!(out.unwrap().n_trades > 0);
}

// ---- native (registry) strategies -------------------------------------------------------

/// The native path runs a REGISTRY strategy — no Rhai anywhere — and its params table is
/// honored: `buy_hold` with `size = 2` buys 2 units once, so the run produces a position and
/// a non-flat curve.
#[test]
fn native_strategy_runs_from_the_registry_with_params() {
    let (_dir, store, _b) = seeded_store();
    let spec = StrategySpec::file(
        "trend/buy_hold.rs",
        params_from_rows(&[("size".into(), "2".into()), ("symbol".into(), "BTCUSDT".into())]),
    );
    let res = run_slice(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice(),
        &store,
        None,
    )
    .expect("buy_hold should run natively");
    assert!(!res.equity_curve.is_empty(), "a native run produces an equity curve");
}

#[test]
fn unknown_native_name_is_a_strategy_error_not_a_compile_error() {
    let (_dir, store, _b) = seeded_store();
    let spec = StrategySpec::file("other/no_such_strategy.rs", vike_studio_core::empty_params());
    let out = run_slice(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice(),
        &store,
        None,
    );
    assert_matches!(out, Err(RunError::Strategy(_)), "got {out:?}");
}

/// **A built artifact named by `(name, sha)` never builds in a hub** (plan 2b): no hub loads an
/// artifact, and the refusal names the strategy FILE to run instead, for an empty, a blank and a real sha
/// alike, through the single run's door and the sweep's. (This pinned the empty-sha refusal and the
/// loader's `Missing` words while this crate `dlopen`ed artifacts; a `.rs` FILE runs in a sandboxed child
/// now, and `crates/vike/tests/strategy_file_run.rs` pins the missing build through that route.)
#[test]
fn a_built_artifact_named_by_name_and_sha_is_refused_naming_the_file_to_run() {
    let files = crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in);
    for sha in [String::new(), "   ".to_string(), "a".repeat(64)] {
        let spec = StrategySpec::strategy_file("my_strat", sha, vike_studio_core::empty_params());
        // `Box<dyn Strategy<OrderBrokerSimulator>>` is not `Debug`: match the Ok side out explicitly.
        match build_strategy(&spec, &files) {
            Err(RunError::Strategy(msg)) => {
                assert!(msg.contains("my_strat"), "the refusal names the strategy: {msg}");
                assert!(msg.contains("[strategy] file"), "and the key to run it by: {msg}");
                assert!(msg.contains("no hub loads an artifact"), "{msg}");
            }
            Ok(_) => panic!("an artifact named by (name, sha) must never build in a hub"),
            Err(other) => panic!("wrong RunError variant: {other:?}"),
        }
        // The sweep door delegates to the same table, so no point can smuggle one past it.
        let point = build_strategy_with(&spec, &files, &[("fast".into(), ParamValue::Float(5.0))]);
        assert!(point.is_err(), "the sweep door inherits the same refusal");
    }
}

/// [`strategy_files_dir`] resolves the PROJECT's `user_data/strategy-builds`, which is where the builder
/// service's artifacts land and therefore the only place the host may look.
///
/// ⚠ **The leaf assertion alone could not fail for its stated reason**, and that is what this
/// test used to be: both branches of [`strategy_files_dir`] end in `user_data/strategy-builds`, so a WALK
/// that resolved nothing — the fallback, a bare relative path — passed it identically to a
/// walk that found the project. The distinguishing property is ABSOLUTENESS plus the PROJECT
/// ROOT: only the resolved branch answers an absolute path under the project this test runs
/// in. Both are asserted, and the sibling below drives the FALLBACK branch through the same
/// function so the two are shown to differ rather than assumed to.
#[test]
fn the_strategy_file_lookup_directory_is_the_resolved_projects_user_data_strategy_files() {
    let dir = strategy_files_dir();
    let rendered = dir.to_string_lossy().replace('\\', "/");
    assert!(rendered.ends_with("user_data/strategy-builds"), "{rendered}");
    // The resolved branch, distinguished from the fallback: an ABSOLUTE path whose parent
    // chain is the project this test runs in. A `cargo test -p` runs with the CWD at the
    // crate directory, so a walk that found nothing would answer the bare relative literal.
    assert!(
        dir.is_absolute(),
        "the walk must have RESOLVED a project, not fallen through to the relative default: \
             {rendered}"
    );
    let cwd = std::env::current_dir().expect("cwd");
    assert!(
        cwd.starts_with(dir.parent().and_then(std::path::Path::parent).expect("<project>")),
        "the resolved project must be an ANCESTOR of the working directory: {rendered} vs {}",
        cwd.display()
    );
}

/// ...and BOTH branches of [`strategy_files_dir_from`], driven through the production function.
///
/// ⚠ **The fallback assertion used to sit behind an `if let Some(..)`**, so on any box where
/// the resolver answered — which is every box — it asserted nothing at all, and the sibling
/// above claimed the fallback was "exercised separately below" when it was exercised by
/// nothing. It cannot be made unconditional through the filesystem: the walk climbs to `/`
/// and no test can promise that no ancestor of a temp directory carries a project marker.
/// `None` is the one input that reaches the fallback on every box, which is why
/// [`strategy_files_dir_from`] takes the working directory as a parameter.
#[test]
fn both_branches_of_the_strategy_file_directory_resolution_are_reachable_and_differ() {
    // THE FALLBACK, unconditionally: no working directory, so nothing to walk from.
    let fallback = strategy_files_dir_from(None);
    assert_eq!(fallback, PathBuf::from("user_data").join("strategy-builds"));
    assert!(!fallback.is_absolute(), "the fallback is the bare relative default: {fallback:?}");

    // THE RESOLVED BRANCH, over a planted project — an ABSOLUTE answer under that project.
    let tmp = tempfile::tempdir().expect("scratch");
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir_all(a.join("settings")).expect("plant marker a");
    std::fs::create_dir_all(b.join("settings")).expect("plant marker b");
    let ra = strategy_files_dir_from(Some(&a));
    let rb = strategy_files_dir_from(Some(&b));
    assert_eq!(ra, a.join("user_data").join("strategy-builds"));
    assert!(ra.is_absolute());

    // The two branches genuinely differ, and the resolver is a FUNCTION OF ITS START PATH:
    // two projects cannot answer one directory, which is what stops a run loading a strategy file
    // from one project while writing its run into another.
    assert_ne!(ra, fallback, "the two branches must not produce the same answer");
    assert_ne!(ra, rb, "two projects must not share one strategy file directory");
}

#[test]
fn native_walkforward_and_sweep_use_the_registry_too() {
    let (_dir, store, _b) = seeded_store();
    let spec = StrategySpec::file(
        "trend/buy_hold.rs",
        params_from_rows(&[("symbol".into(), "BTCUSDT".into())]),
    );
    let rep = run_walkforward_slice(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice(),
        &store,
        4,
    )
    .unwrap();
    assert_eq!(rep.windows.len(), 4);

    let grid = vec![(
        "size".to_string(),
        vec![ParamValue::Float(1.0), ParamValue::Float(2.0), ParamValue::Float(3.0)],
    )];
    let sw = run_paramscan_slice(
        &spec,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice(),
        &store,
        &grid,
    )
    .unwrap();
    assert_eq!(sw.entries.len(), 3, "a native sweep overrides strategy.params per point");
}

// The two tests below were `vike-studio-core`'s `spec.rs` tests until `strategy_config` moved here with the
// run pipeline (2026-11-10): they pin the sweep-point merge a spec's table goes through.

/// A sweep point's params: the spec's table with the point's overrides written over it, by the
/// harness's own merge (`vike_backtest::harness::ProfileStrategy::with_overrides`).
fn overridden(base: Params, overrides: &[(String, ParamValue)]) -> Params {
    crate::studio_run::strategy_config(
        &StrategySpec::file("trend/buy_hold.rs", base),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
    )
    .expect("a file spec's table")
    .with_overrides(overrides)
    .params
}

#[test]
fn a_sweep_point_layers_its_overrides_on_top_of_the_base() {
    let base = params_from_rows(&[
        ("size".to_string(), "1.0".to_string()),
        ("symbol".to_string(), "BTCUSDT".to_string()),
    ]);
    let v = overridden(base, &[("size".to_string(), ParamValue::Float(4.0))]);
    assert_eq!(v.get("size").and_then(ParamValue::as_float), Some(4.0));
    assert_eq!(
        v.get("symbol").and_then(ParamValue::as_str),
        Some("BTCUSDT"),
        "an un-swept key must survive the override"
    );
}

/// Decision 0083's recorded defect: a swept knob the axis carries as an INTEGER is written as an
/// integer, so an `as_integer` reader sees it, and a `Float` stays a `Float` even at an integral
/// value. The kind is decided where the axis is made (`vike_studio`'s `start_sweep`, whose
/// `a_sweep_axis_is_an_int_only_on_an_integer_knob` pins the rule); the merge writes it as
/// given.
#[test]
fn a_swept_integer_knob_stays_an_integer() {
    let base = params_from_rows(&[
        ("rungs".to_string(), "4".to_string()),
        ("size".to_string(), "1.5".to_string()),
    ]);
    let v = overridden(
        base,
        &[("rungs".to_string(), ParamValue::Int(6)), ("size".to_string(), ParamValue::Float(2.0))],
    );
    assert_eq!(v.get("rungs"), Some(&ParamValue::Int(6)), "an Int knob keeps its kind");
    assert_eq!(v.get("rungs").and_then(ParamValue::as_integer), Some(6));
    assert_eq!(
        v.get("size"),
        Some(&ParamValue::Float(2.0)),
        "a Float knob stays a Float, even at an integral value"
    );
}

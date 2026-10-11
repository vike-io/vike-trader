//! Basic runs, native registry strategies, and the plugin arm and its lookup directory.

use std::assert_matches;
use std::path::PathBuf;
use vike_data::TsRange;
use vike_sim::EngineParams;

use super::support::{rhai, seeded_store, slice};
use crate::run::strategy::plugins_dir_from;
use crate::run::{
    DataSlice, RunError, build_strategy, build_strategy_in, build_strategy_with, plugins_dir,
    run_paramscan_slice, run_slice, run_walkforward_slice, spawn_run,
};
use crate::spec::{StrategySpec, params_from_rows};
use crate::test_support::cross_script::CROSS_SCRIPT;

#[test]
fn run_slice_backtests_over_the_store() {
    let (_dir, store, _bars) = seeded_store();
    let out = run_slice(&rhai(CROSS_SCRIPT), &slice(), &store, EngineParams::default());
    let res = out.expect("should backtest");
    assert!(res.n_trades > 0, "the cross should trade over 400 bars");
}

#[test]
fn compile_error_maps_to_runerror_compile() {
    let (_dir, store, _b) = seeded_store();
    let out = run_slice(&rhai("fn on_bar( {"), &slice(), &store, EngineParams::default());
    assert_matches!(out, Err(RunError::Compile(_)));
}

#[test]
fn empty_slice_maps_to_runerror_data() {
    let (_dir, store, _b) = seeded_store();
    let missing = DataSlice::bars("binance", "NOPE", "1m", TsRange::all());
    let out = run_slice(&rhai(CROSS_SCRIPT), &missing, &store, EngineParams::default());
    assert_matches!(out, Err(RunError::Data(_)));
}

#[test]
fn spawn_run_delivers_the_same_outcome() {
    let (_dir, store, _b) = seeded_store();
    let rx = spawn_run(rhai(CROSS_SCRIPT), slice(), store.clone(), EngineParams::default());
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
    let spec = StrategySpec::native(
        "buy_hold",
        params_from_rows(&[("size".into(), "2".into()), ("symbol".into(), "BTCUSDT".into())]),
    );
    let res = run_slice(&spec, &slice(), &store, EngineParams::default())
        .expect("buy_hold should run natively");
    assert!(!res.equity_curve.is_empty(), "a native run produces an equity curve");
}

#[test]
fn every_registered_native_strategy_resolves() {
    for name in crate::spec::native_strategies() {
        assert!(
            build_strategy(&StrategySpec::native_default(*name)).is_ok(),
            "{name} should build"
        );
    }
}

#[test]
fn unknown_native_name_is_a_strategy_error_not_a_compile_error() {
    let (_dir, store, _b) = seeded_store();
    let spec = StrategySpec::native_default("no_such_strategy");
    let out = run_slice(&spec, &slice(), &store, EngineParams::default());
    assert_matches!(out, Err(RunError::Strategy(_)), "got {out:?}");
}

/// **Fix-round-1 CRITICAL, pinned.** An absent sha must never resolve as if it named a real
/// artifact — this is the permanent guard every caller (Run/Sweep/Walk-Forward/Compare-all)
/// relies on regardless of whether it remembered to ask `run_blocked_reason` first. Both an
/// empty string (the studio-side `unwrap_or_default()` shape) and a whitespace-only one are
/// refused, and the refusal names the EMPTINESS rather than falling through to whatever the
/// loader would have said about `<name>-.so` — the two must stay DISTINGUISHABLE so this
/// guard cannot be deleted by mistake as "redundant" now that the loader HAS landed. (This
/// said "the generic 'loader not wired' message … once the loader lands"; that message is
/// gone and its arm is the real `dlopen` now. The requirement is unchanged and is why the
/// sibling test below pins the OTHER refusal by its own words.)
#[test]
fn a_plugin_spec_with_no_sha_is_refused_and_never_reaches_the_loader_arm() {
    let empty = StrategySpec::plugin("my_strat", "", crate::spec::empty_params());
    // `Box<dyn Strategy<SimBroker>>` is not `Debug`, so the Ok side cannot ride `expect_err`/
    // `{:?}` on the whole `Result` — match it out explicitly instead.
    match build_strategy(&empty) {
        Err(RunError::Strategy(msg)) => {
            assert!(msg.contains("no sha"), "{msg}");
            assert!(msg.contains("names no artifact"), "{msg}");
        }
        Ok(_) => panic!("an empty sha must be refused"),
        Err(other) => panic!("wrong RunError variant: {other:?}"),
    }

    let whitespace = StrategySpec::plugin("my_strat", "   ", crate::spec::empty_params());
    assert!(build_strategy(&whitespace).is_err(), "whitespace-only is not a sha either");

    // The overrides door delegates to `build_strategy`, so the same refusal is reachable
    // through it too — a sweep point can never smuggle an empty sha past this guard.
    let via_overrides = build_strategy_with(&empty, &[("fast".to_string(), 5.0)]);
    assert!(via_overrides.is_err(), "the overrides path must inherit the same refusal");
}

/// The complement: a NON-empty sha reaches the LOADER arm, and with no artifact on disk the
/// loader's own `Missing` refusal is what comes back — naming the path and telling the author
/// to build it. That proves the two refusals are genuinely distinct rather than one silently
/// swallowing the other.
///
/// ⚠ This test used to assert the message said "not wired". It is the one assertion the join
/// was supposed to invalidate, and it now pins the OPPOSITE property: reaching the loader is
/// the success condition, and "no artifact at this path" is the loader speaking rather than
/// this function guessing. A missing artifact must NEVER be a silent fall back to some other
/// strategy — the design's own list of red tests says so in as many words.
#[test]
fn a_plugin_spec_with_a_real_looking_sha_reaches_the_loader_and_reports_a_missing_artifact() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let spec = StrategySpec::plugin("my_strat", "a".repeat(64), crate::spec::empty_params());
    match build_strategy_in(&spec, dir.path()) {
        Err(RunError::Strategy(msg)) => {
            assert!(msg.contains("my_strat"), "the refusal must name the artifact: {msg}");
            assert!(msg.to_lowercase().contains("build"), "{msg}");
            assert!(
                !msg.contains("no sha"),
                "a real sha must not trip the emptiness refusal: {msg}"
            );
        }
        Ok(_) => panic!("no artifact exists at that path, so nothing can load"),
        Err(other) => panic!("wrong RunError variant: {other:?}"),
    }
}

/// [`plugins_dir`] resolves the PROJECT's `user_data/plugins`, which is where the builder
/// service's artifacts land and therefore the only place the host may look.
///
/// ⚠ **The leaf assertion alone could not fail for its stated reason**, and that is what this
/// test used to be: both branches of [`plugins_dir`] end in `user_data/plugins`, so a WALK
/// that resolved nothing — the fallback, a bare relative path — passed it identically to a
/// walk that found the project. The distinguishing property is ABSOLUTENESS plus the PROJECT
/// ROOT: only the resolved branch answers an absolute path under the project this test runs
/// in. Both are asserted, and the sibling below drives the FALLBACK branch through the same
/// function so the two are shown to differ rather than assumed to.
#[test]
fn the_plugin_lookup_directory_is_the_resolved_projects_user_data_plugins() {
    let dir = plugins_dir();
    let rendered = dir.to_string_lossy().replace('\\', "/");
    assert!(rendered.ends_with("user_data/plugins"), "{rendered}");
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

/// ...and BOTH branches of [`plugins_dir_from`], driven through the production function.
///
/// ⚠ **The fallback assertion used to sit behind an `if let Some(..)`**, so on any box where
/// the resolver answered — which is every box — it asserted nothing at all, and the sibling
/// above claimed the fallback was "exercised separately below" when it was exercised by
/// nothing. It cannot be made unconditional through the filesystem: the walk climbs to `/`
/// and no test can promise that no ancestor of a temp directory carries a project marker.
/// `None` is the one input that reaches the fallback on every box, which is why
/// [`plugins_dir_from`] takes the working directory as a parameter.
#[test]
fn both_branches_of_the_plugin_directory_resolution_are_reachable_and_differ() {
    // THE FALLBACK, unconditionally: no working directory, so nothing to walk from.
    let fallback = plugins_dir_from(None);
    assert_eq!(fallback, PathBuf::from("user_data").join("plugins"));
    assert!(!fallback.is_absolute(), "the fallback is the bare relative default: {fallback:?}");

    // THE RESOLVED BRANCH, over a planted project — an ABSOLUTE answer under that project.
    let tmp = tempfile::tempdir().expect("scratch");
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir_all(a.join("settings")).expect("plant marker a");
    std::fs::create_dir_all(b.join("settings")).expect("plant marker b");
    let ra = plugins_dir_from(Some(&a));
    let rb = plugins_dir_from(Some(&b));
    assert_eq!(ra, a.join("user_data").join("plugins"));
    assert!(ra.is_absolute());

    // The two branches genuinely differ, and the resolver is a FUNCTION OF ITS START PATH:
    // two projects cannot answer one directory, which is what stops a run loading a plugin
    // from one project while writing its run into another.
    assert_ne!(ra, fallback, "the two branches must not produce the same answer");
    assert_ne!(ra, rb, "two projects must not share one plugin directory");
}

#[test]
fn native_walkforward_and_sweep_use_the_registry_too() {
    let (_dir, store, _b) = seeded_store();
    let spec =
        StrategySpec::native("buy_hold", params_from_rows(&[("symbol".into(), "BTCUSDT".into())]));
    let rep = run_walkforward_slice(&spec, &slice(), &store, 4).unwrap();
    assert_eq!(rep.windows.len(), 4);

    let grid = vec![("size".to_string(), vec![1.0, 2.0, 3.0])];
    let sw = run_paramscan_slice(&spec, &slice(), &store, &grid).unwrap();
    assert_eq!(sw.entries.len(), 3, "a native sweep overrides strategy.params per point");
}

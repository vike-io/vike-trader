//! The parameter sweep: ranking, determinism, DSR and PBO — each the profile search's own.

use vike_backtest::run::profile::ProfileBacktest;
use vike_backtest::search::optimize::{DEFAULT_CSCV_SPLITS, overfit_stats};
use vike_backtest::search::optimize::{SearchEvaluatorStore, optimize};
use vike_backtest::search::paramscan::{OverfitReturnBuckets, SearchGrid};
use vike_backtest::search::paramscan::{ParamscanExecution, RankMetric};
use vike_model::params::ParamValue;

use super::support::{rhai, seeded_store, slice};
use crate::studio_run::run_paramscan_slice;
use vike_studio_core::test_support::sweep_script::SWEEP_SCRIPT;

/// The `fast` axis over `values`.
fn fast(values: &[f64]) -> Vec<(String, Vec<ParamValue>)> {
    vec![("fast".to_string(), values.iter().map(|v| ParamValue::Float(*v)).collect())]
}

#[test]
fn sweep_slice_ranks_and_deflates() {
    let (_dir, store, _b) = seeded_store();
    let sw = run_paramscan_slice(
        &rhai(SWEEP_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice(),
        &store,
        &fast(&[3.0, 5.0, 8.0]),
    )
    .unwrap();
    assert_eq!(sw.entries.len(), 3, "one entry per grid point");
    assert!((0.0..=1.0).contains(&sw.dsr), "a deflated Sharpe is a probability: {}", sw.dsr);
    assert_eq!(sw.best_index, 0);
    // ranked best-first by annualized sharpe (non-increasing)
    let s: Vec<f64> = sw
        .entries
        .iter()
        .map(|e| vike_analytics::metrics::sharpe(&e.result.equity_curve, 252.0))
        .collect();
    for w in s.windows(2) {
        if w[0].is_nan() || w[1].is_nan() {
            continue;
        }
        assert!(w[0] >= w[1]);
    }
}

/// DETERMINISM GATE for the bounded-pool sweep: the SAME grid run twice must produce the same
/// entries in the same rank order with BIT-identical equity, no matter which point finished
/// first. The harness evaluator reassembles by index and its ranking sort is stable, so ordering
/// can never depend on scheduling.
#[test]
fn parallel_sweep_is_deterministic_across_repeated_runs() {
    let (_dir, store, _b) = seeded_store();
    // Enough points that the bounded pool really interleaves them.
    let grid = fast(&[2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
    let a = run_paramscan_slice(
        &rhai(SWEEP_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice(),
        &store,
        &grid,
    )
    .unwrap();
    let b = run_paramscan_slice(
        &rhai(SWEEP_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice(),
        &store,
        &grid,
    )
    .unwrap();
    assert_eq!(a.entries.len(), 8);
    assert_eq!(a.entries.len(), b.entries.len());
    for (x, y) in a.entries.iter().zip(&b.entries) {
        assert_eq!(x.overrides, y.overrides, "same point at the same rank");
        assert_eq!(
            x.result.final_equity.to_bits(),
            y.result.final_equity.to_bits(),
            "bit-identical equity across runs"
        );
    }
    assert_eq!(a.dsr.to_bits(), b.dsr.to_bits());
}

#[test]
fn sweep_populates_pbo_for_a_multi_point_grid() {
    let (_dir, store, _b) = seeded_store();
    let sw = run_paramscan_slice(
        &rhai(SWEEP_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice(),
        &store,
        &fast(&[3.0, 5.0, 8.0]),
    )
    .unwrap();
    assert!(
        (0.0..=1.0).contains(&sw.pbo) || sw.pbo.is_nan(),
        "pbo in [0,1] or NaN, got {}",
        sw.pbo
    );
}

#[test]
fn sweep_pbo_is_nan_for_a_single_point_grid() {
    let (_dir, store, _b) = seeded_store();
    let sw = run_paramscan_slice(
        &rhai(SWEEP_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice(),
        &store,
        &fast(&[5.0]),
    )
    .unwrap();
    assert!(sw.pbo.is_nan(), "single trial -> PBO not assessable, got {}", sw.pbo);
}

/// **A Studio sweep IS the profile search** (audit D2): the same grid written as a profile file's
/// `[sweep]` and searched the way `vike-cli backtest run --keep-trials returns` searches it ranks
/// the same points in the same order at the same equity, with the same deflated Sharpe and PBO.
#[test]
fn a_studio_sweep_equals_the_profile_search_of_the_same_grid() {
    let (_dir, store, _b) = seeded_store();
    let sw = run_paramscan_slice(
        &rhai(SWEEP_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategy_lab::stand_in),
        &slice(),
        &store,
        &fast(&[3.0, 4.0, 5.0, 6.0]),
    )
    .unwrap();

    let mut text = String::from(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"\"\nto = \"\"\n\n[engine]\ncash = 10000.0\n\n[strategy]\n\
         file = \"editor-buffer.rhai\"\n",
    );
    text.push_str("\n\n[paramscan]\nfast = [3.0, 4.0, 5.0, 6.0]\n");
    let mut profile = ProfileBacktest::from_toml_str(&text).expect("the profile file parses");
    // The file a door read: the Studio's buffer, as the profile run of the same script holds it.
    profile.strategy.as_mut().unwrap().source =
        Some(vike_backtest::run::StrategySource::of(SWEEP_SCRIPT.to_string()));
    let evaluation = SearchEvaluatorStore::classic(
        &profile,
        store.clone(),
        RankMetric::Sharpe,
        ParamscanExecution::Sequential,
    )
    .with_return_buckets(OverfitReturnBuckets::DEFAULT);
    let report =
        optimize(&SearchGrid, &profile, &evaluation).expect("the profile search runs").report;
    let overfit = overfit_stats(&report.rows, &evaluation.captured_returns(), DEFAULT_CSCV_SPLITS)
        .expect("400 bars give a matrix");

    assert_eq!(sw.entries.len(), report.rows.len());
    for (studio, cli) in sw.entries.iter().zip(&report.rows) {
        assert_eq!(studio.overrides, cli.overrides, "the same point at the same rank");
        let cli_equity = cli.report.as_ref().expect("every point ran").final_equity;
        assert_eq!(studio.result.final_equity.to_bits(), cli_equity.to_bits());
    }
    assert_eq!(Some(sw.dsr.to_bits()), overfit.deflated_sharpe.map(f64::to_bits), "dsr");
    assert_eq!(Some(sw.pbo.to_bits()), overfit.pbo.map(f64::to_bits), "pbo");
}

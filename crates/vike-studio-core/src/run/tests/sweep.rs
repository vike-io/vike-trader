//! The parameter sweep: ranking, determinism, NaN-last, DSR and PBO.

use vike_analytics::BacktestResult;
use vike_analytics::metrics::sharpe;
use vike_analytics::report::{DAILY_PERIODS_PER_YEAR, periods_per_year_for_interval};
use vike_backtest::harness::cmp_scores_desc;
use vike_model::params::ParamValue;

use super::support::{rhai, seeded_store, slice};
use crate::run::paramscan::{N_SPLITS, rank_entries_by_sharpe, transpose, trial_columns};
use crate::run::{ParamscanEntry, run_paramscan_slice};
use crate::test_support::sweep_script::SWEEP_SCRIPT;

#[test]
fn sweep_slice_ranks_and_deflates() {
    let (_dir, store, _b) = seeded_store();
    let grid = vec![(
        "fast".to_string(),
        vec![ParamValue::Float(3.0), ParamValue::Float(5.0), ParamValue::Float(8.0)],
    )];
    let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
    assert_eq!(sw.entries.len(), 3, "one entry per grid point");
    assert!((0.0..=1.0).contains(&sw.dsr));
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
/// first. `map_bounded` reassembles by index and the ranking sort is stable, so ordering can
/// never depend on scheduling.
#[test]
fn parallel_sweep_is_deterministic_across_repeated_runs() {
    let (_dir, store, _b) = seeded_store();
    // Enough points that the bounded pool really interleaves them.
    let grid = vec![(
        "fast".to_string(),
        vec![
            ParamValue::Float(2.0),
            ParamValue::Float(3.0),
            ParamValue::Float(4.0),
            ParamValue::Float(5.0),
            ParamValue::Float(6.0),
            ParamValue::Float(7.0),
            ParamValue::Float(8.0),
            ParamValue::Float(9.0),
        ],
    )];
    let a = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
    let b = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
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

fn entry(curve: Vec<f64>) -> ParamscanEntry {
    let last = *curve.last().unwrap();
    ParamscanEntry {
        overrides: vec![],
        result: BacktestResult { equity_curve: curve, final_equity: last, ..Default::default() },
    }
}

/// Regression (mirrors the harness sweep's `nan_sharpe_ranks_last_never_first`): a NaN-Sharpe
/// entry constructed FIRST must rank LAST — with the old
/// `partial_cmp(..).unwrap_or(Equal)` sort it compared Equal to every finite point and,
/// grid-placed first, stayed at rank #1 (= `best_index` 0, feeding the DSR).
#[test]
fn nan_sharpe_entry_ranks_last_never_best_index() {
    // A NaN in the equity curve (a blown-up strategy) poisons mean/var -> NaN sharpe.
    let degenerate = entry(vec![1000.0, f64::NAN, 1000.0, 1000.0]);
    let good = entry(vec![1000.0, 1010.0, 1005.0, 1030.0]);
    assert!(sharpe(&degenerate.result.equity_curve, 252.0).is_nan(), "precondition");
    assert!(sharpe(&good.result.equity_curve, 252.0).is_finite(), "precondition");

    // NaN entry deliberately placed first, so a broken comparator would leave it ranked #1.
    let mut entries = vec![degenerate, good];
    rank_entries_by_sharpe(&mut entries);
    assert!(
        sharpe(&entries[0].result.equity_curve, 252.0).is_finite(),
        "finite-Sharpe point is best_index 0"
    );
    assert!(
        sharpe(&entries[1].result.equity_curve, 252.0).is_nan(),
        "NaN-Sharpe point ranks last, not first"
    );
}

/// [`rank_entries_by_sharpe`]'s annualization factor is ORDERING-INERT, which is the whole
/// reason it stays [`DAILY_PERIODS_PER_YEAR`] while [`run_walkforward_slice_with_params`]
/// derives its own from the slice. `metrics::sharpe` multiplies by `sqrt(periods_per_year)` —
/// ONE uniform positive scale applied to every entry — so no factor can reorder the ranking,
/// and a NaN score stays NaN under all of them. Gated here so that doc comment is not a claim
/// nothing checks, and so a later reader cannot derive the value and believe something moved.
#[test]
fn ranking_order_is_invariant_to_the_annualization_factor() {
    // `entry` stamps `final_equity` from the curve's last point, and these four differ, so the
    // equity bits identify WHICH entry landed at each rank.
    let curves = [
        vec![1000.0, 1010.0, 1005.0, 1030.0],
        vec![1000.0, 990.0, 1002.0, 995.0],
        vec![1000.0, f64::NAN, 1000.0, 1000.0],
        vec![1000.0, 1001.0, 1002.0, 1004.0],
    ];
    let order_at = |ppy: f64| -> Vec<u64> {
        let mut entries: Vec<ParamscanEntry> = curves.iter().cloned().map(entry).collect();
        entries.sort_by(|a, b| {
            cmp_scores_desc(
                sharpe(&a.result.equity_curve, ppy),
                sharpe(&b.result.equity_curve, ppy),
            )
        });
        entries.iter().map(|e| e.result.final_equity.to_bits()).collect()
    };

    let daily = order_at(DAILY_PERIODS_PER_YEAR);
    for interval in ["1m", "5m", "1h", "1d", "7d"] {
        assert_eq!(
            order_at(periods_per_year_for_interval(interval)),
            daily,
            "the {interval} factor must not reorder a sweep"
        );
    }

    // ...and the SHIPPED ranker produces exactly that order, with the NaN point still last.
    let mut entries: Vec<ParamscanEntry> = curves.iter().cloned().map(entry).collect();
    rank_entries_by_sharpe(&mut entries);
    let ranked: Vec<u64> = entries.iter().map(|e| e.result.final_equity.to_bits()).collect();
    assert_eq!(ranked, daily, "the shipped ranker agrees with the scale-free order");
    let last = &entries.last().expect("four entries").result.equity_curve;
    assert!(sharpe(last, DAILY_PERIODS_PER_YEAR).is_nan(), "the NaN point still sorts last");
}

#[test]
fn trial_columns_are_per_trial_returns_and_transpose_is_consistent() {
    let entries =
        vec![entry(vec![100.0, 110.0, 121.0, 133.0]), entry(vec![100.0, 90.0, 99.0, 108.0])];
    let (cols, t) = trial_columns(&entries);
    assert_eq!(cols.len(), 2, "N columns");
    assert!(cols.iter().all(|c| c.len() == t), "each column length T");
    for (j, e) in entries.iter().enumerate() {
        let r = vike_analytics::metrics::returns(&e.result.equity_curve);
        assert_eq!(cols[j], r[..t].to_vec(), "column j is trial j's returns");
    }
    let m = transpose(&cols, t);
    assert_eq!(m.len(), t, "matrix has T rows");
    assert!(m.iter().all(|row| row.len() == 2), "each row width N");
    for ti in 0..t {
        for j in 0..2 {
            assert_eq!(m[ti][j], cols[j][ti], "matrix[t][j] == columns[j][t]");
        }
    }
}

#[test]
fn sweep_populates_pbo_for_a_multi_point_grid() {
    let (_dir, store, _b) = seeded_store();
    let grid = vec![(
        "fast".to_string(),
        vec![ParamValue::Float(3.0), ParamValue::Float(5.0), ParamValue::Float(8.0)],
    )];
    let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
    assert!(
        (0.0..=1.0).contains(&sw.pbo) || sw.pbo.is_nan(),
        "pbo in [0,1] or NaN, got {}",
        sw.pbo
    );
}

#[test]
fn sweep_pbo_is_nan_for_a_single_point_grid() {
    let (_dir, store, _b) = seeded_store();
    let grid = vec![("fast".to_string(), vec![ParamValue::Float(5.0)])];
    let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
    assert!(sw.pbo.is_nan(), "single trial -> PBO not assessable, got {}", sw.pbo);
}

#[test]
fn sweep_dsr_is_effective_n_deflated() {
    let (_dir, store, _b) = seeded_store();
    // closely-spaced fast values -> strongly correlated equity curves -> effective_n < N,
    // so the effective-N DSR differs from the plain deflated_sharpe_ratio on the same inputs.
    let grid = vec![(
        "fast".to_string(),
        vec![
            ParamValue::Float(3.0),
            ParamValue::Float(4.0),
            ParamValue::Float(5.0),
            ParamValue::Float(6.0),
        ],
    )];
    let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();

    let per_obs = |r: &BacktestResult| {
        vike_analytics::metrics::sharpe(&r.equity_curve, 252.0) / 252.0_f64.sqrt()
    };
    let trials: Vec<f64> = sw.entries.iter().map(|e| per_obs(&e.result)).collect();
    let best = &sw.entries[0].result;
    let n = vike_analytics::metrics::returns(&best.equity_curve).len();
    let skew = vike_analytics::metrics::returns_skewness(&best.equity_curve);
    let kurt = vike_analytics::metrics::returns_kurtosis(&best.equity_curve) + 3.0;
    let plain =
        vike_analytics::overfit::deflated_sharpe_ratio(per_obs(best), &trials, n, skew, kurt);

    assert!(
        (sw.dsr - plain).abs() > 1e-9,
        "effective-N DSR ({}) should differ from plain DSR ({})",
        sw.dsr,
        plain
    );
}

#[test]
fn pbo_discriminates_overfit_from_stable() {
    // Hand-built per-trial columns (length 48 >= N_SPLITS) fed straight through the transpose
    // into pbo_cscv — asserts the ORDERING (oracle-parity retired; we port the math), decoupled
    // from any strategy that might not reliably overfit the RNG seed.
    let t = 48usize;
    let const_col = |v: f64| vec![v; t];
    let half_col = |first: f64, second: f64| -> Vec<f64> {
        (0..t).map(|i| if i < t / 2 { first } else { second }).collect()
    };
    // Stable: one column dominates in every row -> IS-best is always OOS-best -> PBO ~ 0.
    let stable = vec![const_col(1.0), const_col(0.0), const_col(0.0), const_col(0.0)];
    // Overfit: the IS-best flips to OOS-worst across the timeline halves -> high PBO.
    let overfit = vec![half_col(1.0, 0.0), half_col(0.0, 1.0), const_col(0.5), const_col(0.5)];
    let stable_pbo = vike_analytics::overfit::pbo_cscv(&transpose(&stable, t), N_SPLITS);
    let overfit_pbo = vike_analytics::overfit::pbo_cscv(&transpose(&overfit, t), N_SPLITS);
    assert!(overfit_pbo > 0.0, "overfit matrix should register some overfit, got {overfit_pbo}");
    assert!(
        overfit_pbo > stable_pbo,
        "overfit PBO {overfit_pbo} should exceed stable PBO {stable_pbo}"
    );
}

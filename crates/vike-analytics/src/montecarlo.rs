//! Monte Carlo resampling of backtest trade P&L — port of `analysis/montecarlo.py`.
//!
//! Characterises outcome dispersion by building many synthetic equity paths from the
//! observed trade list. Seeded for reproducibility — same seed -> same output, run to run,
//! within this Rust implementation — and **there is no golden fixture for this module**:
//! porting CPython's exact `random.Random` (MT19937 + its specific `shuffle`/`_randbelow`)
//! was ruled out as not worth it for this one file, so the exporter never froze one and no
//! test here replays one.
//!
//! ⚠ That paragraph used to read "deliberately NOT bit-parity-gated against the Python
//! oracle", which described an exemption from a live comparison. There is no longer a live
//! comparison to be exempt from — the Python app is retired, no exporter survives in this
//! tree, and nothing anywhere compares against it
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`). What survives is
//! the PROVENANCE of the decision: when the two implementations were run side by side, the
//! same seed produced statistically equivalent, not bit-identical, resampled paths.

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{RngExt, SeedableRng};

// This module's own max_drawdown_curve was byte-identical to metrics::max_drawdown, and its
// own percentile was the same linear-interpolation formula as metrics::percentile — both
// deduped there (metrics::percentile keeps this module's empty-guard as the superset).
use crate::metrics::{max_drawdown, percentile};

fn build_curve(start_equity: f64, pnls: &[f64]) -> Vec<f64> {
    let mut curve = Vec::with_capacity(pnls.len() + 1);
    curve.push(start_equity);
    let mut eq = start_equity;
    for &p in pnls {
        eq += p;
        curve.push(eq);
    }
    curve
}

/// `mc_resample`'s `method` parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResampleMethod {
    /// Reorders the trade list (preserves the exact multiset of P&Ls per path).
    Shuffle,
    /// Samples with replacement.
    Bootstrap,
}

/// Result of [`mc_resample`].
#[derive(Debug, Clone, PartialEq)]
pub struct McResample {
    /// Final equity value of each of the `n_sims` paths.
    pub terminal: Vec<f64>,
    /// Max drawdown fraction of each path.
    pub max_drawdowns: Vec<f64>,
    /// A representative sample of full equity curves (up to 20 paths), for plotting.
    pub curves_sample: Vec<Vec<f64>>,
}

/// Run `n_sims` Monte Carlo paths over the trade P&L list.
///
/// `method: Shuffle` reorders the trade list; `Bootstrap` samples with replacement. Both
/// produce `n_sims` equity paths, seeded from `seed` for run-to-run reproducibility (see
/// the module-level note: no golden fixture exists for this module, because CPython's
/// `random.Random` was never ported).
pub fn mc_resample(
    trade_pnls: &[f64],
    start_equity: f64,
    n_sims: usize,
    seed: u64,
    method: ResampleMethod,
) -> McResample {
    let mut pnls = trade_pnls.to_vec();
    let mut rng = StdRng::seed_from_u64(seed);
    let n = pnls.len();

    let mut terminals = Vec::with_capacity(n_sims);
    let mut max_drawdowns = Vec::with_capacity(n_sims);
    let mut all_curves = Vec::with_capacity(n_sims);

    for _ in 0..n_sims {
        let resampled = match method {
            ResampleMethod::Shuffle => {
                pnls.shuffle(&mut rng);
                pnls.clone()
            }
            ResampleMethod::Bootstrap => {
                if n == 0 {
                    Vec::new()
                } else {
                    (0..n).map(|_| pnls[rng.random_range(0..n)]).collect()
                }
            }
        };
        let curve = build_curve(start_equity, &resampled);
        terminals.push(*curve.last().unwrap());
        max_drawdowns.push(max_drawdown(&curve));
        all_curves.push(curve);
    }

    // Thin down curves_sample to at most 20 paths spread evenly across sims.
    let step = (n_sims / 20).max(1);
    let curves_sample: Vec<Vec<f64>> = all_curves.into_iter().step_by(step).take(20).collect();

    McResample { terminal: terminals, max_drawdowns, curves_sample }
}

/// Per-step percentile bands across equal-length simulation curves.
///
/// Returns `(quantile, per_step_values)` pairs in the same order as `qs` (a `Vec` of pairs
/// rather than a dict, since `f64` isn't hashable in Rust).
pub fn confidence_bands(curves: &[Vec<f64>], qs: &[f64]) -> Vec<(f64, Vec<f64>)> {
    if curves.is_empty() {
        return qs.iter().map(|&q| (q, Vec::new())).collect();
    }
    let length = curves[0].len();
    let mut result: Vec<(f64, Vec<f64>)> =
        qs.iter().map(|&q| (q, Vec::with_capacity(length))).collect();
    for step in 0..length {
        let mut col: Vec<f64> = curves.iter().filter(|c| step < c.len()).map(|c| c[step]).collect();
        col.sort_by(f64::total_cmp);
        for (q, values) in result.iter_mut() {
            values.push(percentile(&col, *q));
        }
    }
    result
}

/// Fraction of simulation paths that end at or below `ruin_level`.
pub fn risk_of_ruin(terminal_equities: &[f64], ruin_level: f64) -> f64 {
    if terminal_equities.is_empty() {
        return 0.0;
    }
    let below = terminal_equities.iter().filter(|&&e| e <= ruin_level).count();
    below as f64 / terminal_equities.len() as f64
}

/// Result of [`mc_summary`].
#[derive(Debug, Clone, PartialEq)]
pub struct McSummary {
    pub terminal_p5: f64,
    pub terminal_p50: f64,
    pub terminal_p95: f64,
    pub max_dd_p50: f64,
    pub max_dd_p95: f64,
    pub prob_loss: f64,
    pub risk_of_ruin: f64,
}

/// Run Monte Carlo (shuffle method) and return key summary statistics.
///
/// `ruin_pct`: ruin is defined as terminal equity `<= ruin_pct * start_equity`.
pub fn mc_summary(
    trade_pnls: &[f64],
    start_equity: f64,
    n_sims: usize,
    seed: u64,
    ruin_pct: f64,
) -> McSummary {
    let result = mc_resample(trade_pnls, start_equity, n_sims, seed, ResampleMethod::Shuffle);
    let mut terminals = result.terminal;
    terminals.sort_by(f64::total_cmp);
    let mut mdd_sorted = result.max_drawdowns;
    mdd_sorted.sort_by(f64::total_cmp);

    let ruin_level = ruin_pct * start_equity;

    McSummary {
        terminal_p5: percentile(&terminals, 0.05),
        terminal_p50: percentile(&terminals, 0.50),
        terminal_p95: percentile(&terminals, 0.95),
        max_dd_p50: percentile(&mdd_sorted, 0.50),
        max_dd_p95: percentile(&mdd_sorted, 0.95),
        prob_loss: risk_of_ruin(&terminals, start_equity),
        risk_of_ruin: risk_of_ruin(&terminals, ruin_level),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- build_curve / max_drawdown (no RNG — exact, deterministic; max_drawdown itself is
    // metrics::max_drawdown — see the `use` above — re-tested here only through this module's
    // own call site in mc_resample) ---

    #[test]
    fn build_curve_cumulative() {
        let curve = build_curve(100.0, &[10.0, -5.0, 20.0]);
        assert_eq!(curve, vec![100.0, 110.0, 105.0, 125.0]);
    }

    #[test]
    fn max_drawdown_curve_known_value() {
        let curve = [100.0, 120.0, 90.0, 110.0];
        assert!((max_drawdown(&curve) - 0.25).abs() < 1e-9); // (120-90)/120
    }

    #[test]
    fn max_drawdown_curve_monotone_is_zero() {
        assert_eq!(max_drawdown(&[100.0, 110.0, 120.0]), 0.0);
    }

    #[test]
    fn max_drawdown_curve_empty_is_zero() {
        assert_eq!(max_drawdown(&[]), 0.0);
    }

    // --- mc_resample (RNG-dependent — structural/statistical invariants, not literal values) ---

    #[test]
    fn mc_resample_shapes() {
        let pnls = [10.0, -5.0, 20.0, -10.0, 15.0];
        let result = mc_resample(&pnls, 1000.0, 50, 0, ResampleMethod::Shuffle);
        assert_eq!(result.terminal.len(), 50);
        assert_eq!(result.max_drawdowns.len(), 50);
        assert!(result.curves_sample.len() <= 20);
    }

    #[test]
    fn mc_resample_shuffle_preserves_total_pnl_every_path() {
        // Shuffling only reorders — every path's terminal equity must equal
        // start_equity + sum(original pnls) exactly, regardless of RNG algorithm.
        let pnls = [10.0, -5.0, 20.0, -10.0, 15.0];
        let expected_terminal = 1000.0 + pnls.iter().sum::<f64>();
        let result = mc_resample(&pnls, 1000.0, 30, 42, ResampleMethod::Shuffle);
        for &t in &result.terminal {
            assert!((t - expected_terminal).abs() < 1e-9);
        }
    }

    #[test]
    fn mc_resample_same_seed_is_deterministic() {
        let pnls = [10.0, -5.0, 20.0, -10.0, 15.0];
        let a = mc_resample(&pnls, 1000.0, 20, 7, ResampleMethod::Bootstrap);
        let b = mc_resample(&pnls, 1000.0, 20, 7, ResampleMethod::Bootstrap);
        assert_eq!(a, b);
    }

    #[test]
    fn mc_resample_different_seeds_usually_differ() {
        let pnls = [10.0, -5.0, 20.0, -10.0, 15.0, -3.0, 8.0];
        let a = mc_resample(&pnls, 1000.0, 20, 1, ResampleMethod::Bootstrap);
        let b = mc_resample(&pnls, 1000.0, 20, 2, ResampleMethod::Bootstrap);
        assert_ne!(a.terminal, b.terminal);
    }

    #[test]
    fn mc_resample_bootstrap_draws_only_from_original_pnls() {
        let pnls = [10.0, -5.0, 20.0];
        let result = mc_resample(&pnls, 0.0, 10, 3, ResampleMethod::Bootstrap);
        for curve in &result.curves_sample {
            for w in curve.windows(2) {
                let step = w[1] - w[0];
                assert!(pnls.iter().any(|&p| (p - step).abs() < 1e-9));
            }
        }
    }

    #[test]
    fn mc_resample_empty_pnls() {
        let result = mc_resample(&[], 1000.0, 5, 0, ResampleMethod::Shuffle);
        assert_eq!(result.terminal, vec![1000.0; 5]);
        assert_eq!(result.max_drawdowns, vec![0.0; 5]);
    }

    // --- confidence_bands (no RNG — exact) ---

    #[test]
    fn confidence_bands_shapes_and_ordering() {
        let curves =
            vec![vec![100.0, 110.0, 90.0], vec![100.0, 105.0, 95.0], vec![100.0, 120.0, 130.0]];
        let bands = confidence_bands(&curves, &[0.05, 0.50, 0.95]);
        assert_eq!(bands.len(), 3);
        for (_, values) in &bands {
            assert_eq!(values.len(), 3);
        }
        let p5 = &bands[0].1;
        let p50 = &bands[1].1;
        let p95 = &bands[2].1;
        for step in 0..3 {
            assert!(p5[step] <= p50[step] + 1e-9);
            assert!(p50[step] <= p95[step] + 1e-9);
        }
    }

    #[test]
    fn confidence_bands_empty_curves() {
        let bands = confidence_bands(&[], &[0.05, 0.50, 0.95]);
        assert_eq!(bands, vec![(0.05, vec![]), (0.50, vec![]), (0.95, vec![])]);
    }

    // --- risk_of_ruin (no RNG — exact) ---

    #[test]
    fn risk_of_ruin_known_value() {
        let terminals = [900.0, 1100.0, 800.0, 1200.0];
        assert!((risk_of_ruin(&terminals, 1000.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn risk_of_ruin_empty_is_zero() {
        assert_eq!(risk_of_ruin(&[], 1000.0), 0.0);
    }

    // --- mc_summary (RNG-dependent — structural invariants) ---

    #[test]
    fn mc_summary_percentiles_ordered_and_probs_bounded() {
        let pnls = [10.0, -5.0, 20.0, -10.0, 15.0, -8.0, 12.0];
        let summary = mc_summary(&pnls, 1000.0, 200, 0, 0.5);
        assert!(summary.terminal_p5 <= summary.terminal_p50 + 1e-9);
        assert!(summary.terminal_p50 <= summary.terminal_p95 + 1e-9);
        assert!(summary.max_dd_p50 <= summary.max_dd_p95 + 1e-9);
        assert!((0.0..=1.0).contains(&summary.prob_loss));
        assert!((0.0..=1.0).contains(&summary.risk_of_ruin));
        assert!(summary.risk_of_ruin <= summary.prob_loss + 1e-9); // ruin_level < start_equity
    }
}

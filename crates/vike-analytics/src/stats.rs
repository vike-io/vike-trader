//! Survival statistics for signal research: an autocorrelation-robust t-statistic, the
//! Benjamini-Hochberg false-discovery correction, a moving block bootstrap for Sharpe
//! confidence intervals, and Hansen's Superior Predictive Ability test.
//!
//! All pure functions over `&[f64]`. They sit beside [`crate::metrics`] because they answer the
//! question that catalog cannot: not "what was the Sharpe" but "would this Sharpe survive its own
//! autocorrelation, and the fact that it was selected as the best of many".
//!
//! Seeded RNG streams here are reproducible RUN TO RUN within this implementation, but are
//! deliberately NOT required to match NumPy's PCG64 — reproducing that generator's exact stream
//! buys nothing a confidence interval consumes. Consumers use the interval, not its last digits.

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::metrics::percentile;
use crate::overfit::normal_cdf;

/// Bartlett-kernel Newey-West t-statistic for the mean of an autocorrelated series.
///
/// `V = γ₀ + 2 Σ_{k=1..L} (1 − k/(L+1)) γₖ`, then `t = mean / √(V/n)`, with `L = min(lag, n−1)`.
///
/// A per-bar PnL series is autocorrelated by construction — a position held for many bars earns
/// correlated returns — so the ordinary t-statistic overstates significance. This is the
/// correction, and `lag` should be at least the holding horizon.
///
/// Returns `0.0` rather than a NaN or an infinity for a degenerate input (fewer than two
/// observations, or a non-positive long-run variance): callers compare it against a threshold, and
/// zero fails every threshold, which is the safe direction.
pub fn newey_west_tstat(x: &[f64], lag: usize) -> f64 {
    let s: Vec<f64> = x.iter().copied().filter(|v| !v.is_nan()).collect();
    let n = s.len();
    if n < 2 {
        return 0.0;
    }
    let nf = n as f64;
    let mean = s.iter().sum::<f64>() / nf;
    let centered: Vec<f64> = s.iter().map(|v| v - mean).collect();
    let gamma0 = centered.iter().map(|v| v * v).sum::<f64>() / nf;

    let l = lag.min(n - 1);
    let mut cov_sum = 0.0;
    for k in 1..=l {
        let gamma_k =
            centered[k..].iter().zip(centered[..n - k].iter()).map(|(a, b)| a * b).sum::<f64>()
                / nf;
        let weight = 1.0 - k as f64 / (l as f64 + 1.0);
        cov_sum += 2.0 * weight * gamma_k;
    }
    let long_run_var = gamma0 + cov_sum;
    if long_run_var <= 0.0 {
        return 0.0;
    }
    mean / (long_run_var / nf).sqrt()
}

/// Two-sided normal p-value for a t-statistic: `2 * (1 − Φ(|t|))`.
///
/// The normal rather than Student's t: these series are thousands of bars long, where the two
/// agree to well past any threshold anyone gates on.
pub fn two_sided_p(tstat: f64) -> f64 {
    if tstat.is_nan() {
        return 1.0;
    }
    (2.0 * (1.0 - normal_cdf(tstat.abs()))).clamp(0.0, 1.0)
}

/// Benjamini-Hochberg step-up procedure. Returns a rejection mask **in the caller's order**.
///
/// Sweeping 300 signal variants and keeping whichever cleared `p < 0.05` would produce ~15 false
/// positives from noise alone. This controls the expected FALSE-DISCOVERY RATE — the share of
/// rejections that are wrong — at `q`.
///
/// Step-UP is the part worth understanding: it finds the LARGEST rank `k` whose sorted p-value is
/// at or below `q·k/n`, then rejects every hypothesis at rank `k` and below. So a middling p-value
/// can be rejected on the strength of the ones beneath it, which is exactly the intended behaviour
/// and not a bug to "fix".
pub fn bh_fdr(pvalues: &[f64], q: f64) -> Vec<bool> {
    let n = pvalues.len();
    if n == 0 {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..n).collect();
    order
        .sort_by(|a, b| pvalues[*a].partial_cmp(&pvalues[*b]).unwrap_or(std::cmp::Ordering::Equal));

    let mut largest_pass: Option<usize> = None;
    for (rank, idx) in order.iter().enumerate() {
        let threshold = q * (rank as f64 + 1.0) / n as f64;
        if pvalues[*idx] <= threshold {
            largest_pass = Some(rank);
        }
    }

    let mut out = vec![false; n];
    if let Some(k) = largest_pass {
        for idx in order.iter().take(k + 1) {
            out[*idx] = true;
        }
    }
    out
}

/// Moving block bootstrap (Kunsch) confidence interval for the annualized Sharpe: returns the
/// 5th and 95th percentiles of the resampled distribution.
///
/// Fixed-length contiguous blocks are Kunsch's construction. Politis-Romano's STATIONARY
/// bootstrap is a different procedure — geometric block lengths, wrapping at the series end —
/// and is not what this function does; the name above is corrected rather than reused.
///
/// Resampling individual BARS would destroy the autocorrelation that makes a signal's Sharpe
/// uncertain in the first place, and would report a falsely tight interval. Contiguous blocks of
/// `block_len` bars preserve the local dependence structure.
///
/// Returns `(0.0, 0.0)` when the series is shorter than one block plus a bar — there is nothing to
/// resample, and inventing an interval would be worse than declaring none.
pub fn block_bootstrap_sharpe(
    pnl: &[f64],
    block_len: usize,
    n_resamples: usize,
    periods_per_year: f64,
    seed: u64,
) -> (f64, f64) {
    let s: Vec<f64> = pnl.iter().copied().filter(|v| !v.is_nan()).collect();
    let n = s.len();
    if block_len == 0 || n < block_len + 1 || n_resamples == 0 {
        return (0.0, 0.0);
    }
    let mut rng = StdRng::seed_from_u64(seed);
    let n_blocks = n.div_ceil(block_len);
    let mut sharpes = Vec::with_capacity(n_resamples);

    for _ in 0..n_resamples {
        let mut sample: Vec<f64> = Vec::with_capacity(n_blocks * block_len);
        for _ in 0..n_blocks {
            let start = rng.random_range(0..=(n - block_len));
            sample.extend_from_slice(&s[start..start + block_len]);
        }
        sample.truncate(n);
        let m = sample.iter().sum::<f64>() / sample.len() as f64;
        let var = sample.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / sample.len() as f64;
        let sd = var.sqrt();
        sharpes.push(if sd == 0.0 { 0.0 } else { m / sd * periods_per_year.sqrt() });
    }

    sharpes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (percentile(&sharpes, 0.05), percentile(&sharpes, 0.95))
}

/// Hansen's Superior Predictive Ability p-value, via block bootstrap.
///
/// Answers the question a per-signal t-statistic cannot: given that the best of `k` candidates was
/// SELECTED as the best, is its performance still surprising? Each column is centred at its own
/// mean, making the null "every candidate has zero expected PnL", and the maximum Sharpe across
/// candidates is re-drawn under that null. The p-value is the share of draws whose maximum matches
/// or beats the observed one.
///
/// What this actually computes is White's Reality Check on the maximum Sharpe (the recentred
/// bootstrap max-statistic test), not Hansen's studentized SPA — that variant standardizes each
/// candidate by its own bootstrap variance before taking the max, which shrinks the influence of
/// noisy low-performing candidates and is not implemented here. The name is kept because it is
/// this module's public API, not because it promises the studentized variant.
///
/// `pnl_matrix` is column-major: one inner `Vec` per candidate signal. Rows are aligned bars, so
/// every column must be the same length; shorter columns are truncated to the shortest.
///
/// Returns `1.0` — reject nothing — for an empty panel or a series shorter than one block.
pub fn hansens_spa(
    pnl_matrix: &[Vec<f64>],
    n_resamples: usize,
    block_len: usize,
    seed: u64,
) -> f64 {
    let k = pnl_matrix.len();
    if k == 0 || block_len == 0 || n_resamples == 0 {
        return 1.0;
    }
    let n = pnl_matrix.iter().map(|c| c.len()).min().unwrap_or(0);
    if n < block_len + 1 {
        return 1.0;
    }

    let cols: Vec<&[f64]> = pnl_matrix.iter().map(|c| &c[..n]).collect();
    let means: Vec<f64> = cols.iter().map(|c| c.iter().sum::<f64>() / n as f64).collect();

    let sharpe_of = |c: &[f64], m: f64| -> f64 {
        let var = c.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / c.len() as f64;
        let sd = var.sqrt();
        if sd == 0.0 {
            0.0
        } else {
            m / sd
        }
    };
    let obs_max = cols
        .iter()
        .zip(means.iter())
        .map(|(c, m)| sharpe_of(c, *m))
        .fold(f64::NEG_INFINITY, f64::max);

    let mut rng = StdRng::seed_from_u64(seed);
    let n_blocks = n.div_ceil(block_len);
    let mut at_or_above = 0usize;

    for _ in 0..n_resamples {
        // ONE index sequence shared by every column: candidates must be resampled in lockstep
        // or the cross-sectional dependence between them is destroyed. Independent draws make
        // the maximum over columns stochastically larger, inflating the null distribution and
        // costing the test power.
        let mut idx: Vec<usize> = Vec::with_capacity(n_blocks * block_len);
        for _ in 0..n_blocks {
            let start = rng.random_range(0..=(n - block_len));
            idx.extend(start..start + block_len);
        }
        idx.truncate(n);

        let mut null_max = f64::NEG_INFINITY;
        for (c, m) in cols.iter().zip(means.iter()) {
            let sample: Vec<f64> = idx.iter().map(|i| c[*i] - m).collect();
            let sm = sample.iter().sum::<f64>() / n as f64;
            null_max = null_max.max(sharpe_of(&sample, sm));
        }
        if null_max >= obs_max {
            at_or_above += 1;
        }
    }
    at_or_above as f64 / n_resamples as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_series_shorter_than_two_has_no_tstat() {
        assert_eq!(newey_west_tstat(&[], 4), 0.0);
        assert_eq!(newey_west_tstat(&[1.0], 4), 0.0);
    }

    #[test]
    fn a_zero_lag_reduces_to_the_ordinary_t_of_the_mean() {
        // lag 0 means no autocovariance terms: V = gamma_0 (population variance).
        let x: [f64; 4] = [1.0, 2.0, 3.0, 4.0];
        let n = x.len() as f64;
        let mean = 2.5;
        let var = x.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
        let expected = mean / (var / n).sqrt();
        assert!((newey_west_tstat(&x, 0) - expected).abs() < 1e-12);
    }

    #[test]
    fn a_flat_series_has_no_variance_and_therefore_no_tstat() {
        assert_eq!(newey_west_tstat(&[3.0; 10], 4), 0.0);
    }

    #[test]
    fn nans_are_dropped_not_propagated() {
        let clean = newey_west_tstat(&[1.0, 2.0, 3.0, 4.0], 2);
        let dirty = newey_west_tstat(&[1.0, f64::NAN, 2.0, 3.0, 4.0], 2);
        assert!((clean - dirty).abs() < 1e-12);
    }

    #[test]
    fn the_bartlett_weight_and_the_factor_of_two_are_both_pinned() {
        // x = [1,2,3,4], lag 1: gamma0 = 1.25, gamma1 = 0.3125, weight = 1 - 1/2 = 0.5,
        // V = 1.25 + 2*0.5*0.3125 = 1.5625, t = 2.5 / sqrt(1.5625/4) = 4.0 exactly.
        let x: [f64; 4] = [1.0, 2.0, 3.0, 4.0];
        assert!((newey_west_tstat(&x, 1) - 4.0).abs() < 1e-12);
    }

    #[test]
    fn the_two_sided_p_of_a_zero_tstat_is_one() {
        assert!((two_sided_p(0.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_two_sigma_tstat_lands_near_the_familiar_five_percent() {
        let p = two_sided_p(1.959_963_984_540_054);
        assert!((p - 0.05).abs() < 1e-6, "got {p}");
    }

    #[test]
    fn the_p_value_is_symmetric_in_the_sign_of_the_tstat() {
        assert!((two_sided_p(2.5) - two_sided_p(-2.5)).abs() < 1e-15);
    }

    #[test]
    fn an_empty_input_rejects_nothing() {
        assert!(bh_fdr(&[], 0.10).is_empty());
    }

    #[test]
    fn a_single_tiny_pvalue_is_rejected() {
        assert_eq!(bh_fdr(&[0.001], 0.10), vec![true]);
    }

    #[test]
    fn all_large_pvalues_are_rejected_by_nothing() {
        assert_eq!(bh_fdr(&[0.9, 0.8, 0.7], 0.10), vec![false, false, false]);
    }

    #[test]
    fn rejection_is_a_step_up_so_a_large_p_rides_in_on_a_small_one() {
        // Thresholds at q=0.10, n=3: 0.0333 / 0.0667 / 0.10.
        // Rank 2 (0.08) FAILS its own threshold; rank 3 (0.09) passes, so step-up rejects
        // all three. A step-down that stops at the first failure would return [true,false,false].
        assert_eq!(bh_fdr(&[0.001, 0.08, 0.09], 0.10), vec![true, true, true]);
    }

    #[test]
    fn the_mask_is_returned_in_the_callers_order_not_sorted_order() {
        // 0.9 must stay false in position 0 even though 0.001 in position 1 is rejected.
        assert_eq!(bh_fdr(&[0.9, 0.001], 0.10), vec![false, true]);
    }

    #[test]
    fn a_series_shorter_than_one_block_yields_a_degenerate_interval() {
        assert_eq!(block_bootstrap_sharpe(&[0.1; 5], 24, 100, 8760.0, 0), (0.0, 0.0));
    }

    #[test]
    fn the_interval_is_ordered_and_brackets_something_plausible() {
        let pnl: Vec<f64> = (0..500).map(|i| ((i as f64) * 0.31).sin() * 0.01).collect();
        let (lo, hi) = block_bootstrap_sharpe(&pnl, 24, 200, 8760.0, 7);
        assert!(lo <= hi, "lo {lo} must not exceed hi {hi}");
        assert!(lo.is_finite() && hi.is_finite());
    }

    #[test]
    fn the_same_seed_reproduces_the_same_interval() {
        let pnl: Vec<f64> = (0..300).map(|i| ((i as f64) * 0.17).cos() * 0.02).collect();
        let a = block_bootstrap_sharpe(&pnl, 24, 100, 8760.0, 42);
        let b = block_bootstrap_sharpe(&pnl, 24, 100, 8760.0, 42);
        assert_eq!(a, b);
    }

    #[test]
    fn a_different_seed_generally_moves_the_interval() {
        let pnl: Vec<f64> = (0..300).map(|i| ((i as f64) * 0.17).cos() * 0.02).collect();
        let a = block_bootstrap_sharpe(&pnl, 24, 100, 8760.0, 1);
        let b = block_bootstrap_sharpe(&pnl, 24, 100, 8760.0, 2);
        assert_ne!(a, b);
    }

    #[test]
    fn blocks_are_contiguous_so_an_alternating_series_resamples_to_exactly_zero_mean() {
        // Every contiguous 2-block of +/-1bp sums to exactly 0, so every resample's mean —
        // and therefore its Sharpe — is 0. A per-bar bootstrap could not do that 200 times.
        let pnl: Vec<f64> = (0..100).map(|i| if i % 2 == 0 { 0.01 } else { -0.01 }).collect();
        assert_eq!(block_bootstrap_sharpe(&pnl, 2, 200, 8760.0, 5), (0.0, 0.0));
    }

    #[test]
    fn an_empty_panel_cannot_reject_anything() {
        assert_eq!(hansens_spa(&[], 100, 24, 0), 1.0);
    }

    #[test]
    fn a_panel_shorter_than_a_block_cannot_reject_anything() {
        assert_eq!(hansens_spa(&[vec![0.1; 5]], 100, 24, 0), 1.0);
    }

    #[test]
    fn the_pvalue_is_a_probability() {
        let a: Vec<f64> = (0..300).map(|i| ((i as f64) * 0.11).sin() * 0.01).collect();
        let b: Vec<f64> = (0..300).map(|i| ((i as f64) * 0.23).cos() * 0.01).collect();
        let p = hansens_spa(&[a, b], 100, 24, 3);
        assert!((0.0..=1.0).contains(&p), "got {p}");
    }

    #[test]
    fn the_same_seed_reproduces_the_same_pvalue() {
        let a: Vec<f64> = (0..300).map(|i| ((i as f64) * 0.11).sin() * 0.01).collect();
        let p1 = hansens_spa(std::slice::from_ref(&a), 100, 24, 9);
        let p2 = hansens_spa(std::slice::from_ref(&a), 100, 24, 9);
        assert_eq!(p1, p2);
    }

    #[test]
    fn one_index_sequence_is_shared_across_columns() {
        let a: Vec<f64> = (0..300).map(|i| ((i as f64) * 0.11).sin() * 0.01).collect();
        let two = vec![a.clone(), a.clone()];
        // Duplicating a column must not move the p-value: shared indices give both columns
        // the same null draw. Independent per-column draws would make null_max a max of two
        // iid values and the equality would break.
        assert_eq!(
            hansens_spa(&two, 100, 24, 9),
            hansens_spa(std::slice::from_ref(&a), 100, 24, 9)
        );
    }

    #[test]
    fn a_strongly_positive_candidate_is_rejected_near_zero() {
        // mean ~0.01, sd ~0.001 -> observed Sharpe well into double digits. If the null
        // centring (`c[*i] - m`) were dropped, null_max would approach obs_max and p would
        // sit near 0.5 instead of collapsing toward 0.
        let strong: Vec<f64> = (0..300).map(|i| 0.01 + ((i as f64) * 0.11).sin() * 0.001).collect();
        let p = hansens_spa(&[strong], 200, 24, 11);
        assert!(p < 0.05, "got {p}");
    }
}

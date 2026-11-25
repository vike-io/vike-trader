//! Survival statistics for signal research: a moving block bootstrap for Sharpe confidence intervals,
//! White's Reality Check and Hansen's studentized Superior Predictive Ability test.
//!
//! All pure functions over `&[f64]`. They sit beside [`crate::metrics`] because they answer the
//! question that catalog cannot: not "what was the Sharpe" but "would this Sharpe survive its own
//! autocorrelation, and the fact that it was selected as the best of many". The pure formulas they
//! stand on are `vike-math`'s (decision 0294): the block draw (`vike_math::resampling`), the means and
//! variances (`vike_math::statistics`), and the autocorrelation-robust t-statistic, the sample
//! autocorrelation function and the Benjamini-Hochberg correction a study reads beside them
//! (`vike_math::time_series`, `vike_math::hypothesis_testing`).
//!
//! Seeded RNG streams here are reproducible RUN TO RUN within this implementation, but are
//! deliberately NOT required to match NumPy's PCG64 — reproducing that generator's exact stream
//! buys nothing a confidence interval consumes. Consumers use the interval, not its last digits.

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use vike_math::resampling::moving_block_indices;
use vike_math::statistics::{mean, mean_variance, percentile, plain_mean};

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
    let mut sharpes = Vec::with_capacity(n_resamples);
    let mut index: Vec<usize> = Vec::with_capacity(n.div_ceil(block_len) * block_len);

    for _ in 0..n_resamples {
        moving_block_indices(n, block_len, |max| rng.random_range(0..=max), &mut index);
        let sample: Vec<f64> = index.iter().map(|&i| s[i]).collect();
        let (m, var) = mean_variance(sample.iter().copied(), 0.0);
        let sd = var.sqrt();
        sharpes.push(if sd == 0.0 { 0.0 } else { m / sd * periods_per_year.sqrt() });
    }

    sharpes.sort_by(f64::total_cmp);
    (percentile(&sharpes, 0.05), percentile(&sharpes, 0.95))
}

/// White's Reality Check p-value on the maximum Sharpe, via block bootstrap.
///
/// Answers the question a per-signal t-statistic cannot: given that the best of `k` candidates was
/// SELECTED as the best, is its performance still surprising? Each column is centred at its own
/// mean, making the null "every candidate has zero expected PnL", and the maximum Sharpe across
/// candidates is re-drawn under that null. The p-value is the share of draws whose maximum matches
/// or beats the observed one.
///
/// This is the recentred bootstrap max-statistic test of White (2000). It was named `hansens_spa`
/// until 2026-11-16, which it never was: Hansen's test studentizes each candidate and leaves a
/// clearly poor one out of the null, and is [`hansens_studentized_spa`] beside it. Every candidate
/// is recentred here, so a poor but noisy candidate still adds to the null maximum and raises the
/// p-value.
///
/// `pnl_matrix` is column-major: one inner `Vec` per candidate signal. Rows are aligned bars, so
/// every column must be the same length; shorter columns are truncated to the shortest.
///
/// Returns `1.0` — reject nothing — for an empty panel or a series shorter than one block.
pub fn whites_reality_check(
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
    // The plain mean twin: `stats::whites_reality_check`'s probe row pins this fold.
    let means: Vec<f64> = cols.iter().map(|c| plain_mean(c.iter().copied())).collect();

    let sharpe_of = |c: &[f64]| -> f64 {
        let (m, var) = mean_variance(c.iter().copied(), 0.0);
        let sd = var.sqrt();
        if sd == 0.0 { 0.0 } else { m / sd }
    };
    let obs_max = cols.iter().map(|c| sharpe_of(c)).fold(f64::NEG_INFINITY, f64::max);

    let mut rng = StdRng::seed_from_u64(seed);
    let mut at_or_above = 0usize;
    let mut index: Vec<usize> = Vec::with_capacity(n.div_ceil(block_len) * block_len);

    for _ in 0..n_resamples {
        // ONE index sequence shared by every column: candidates must be resampled in lockstep
        // or the cross-sectional dependence between them is destroyed. Independent draws make
        // the maximum over columns stochastically larger, inflating the null distribution and
        // costing the test power.
        moving_block_indices(n, block_len, |max| rng.random_range(0..=max), &mut index);

        let mut null_max = f64::NEG_INFINITY;
        for (c, m) in cols.iter().zip(means.iter()) {
            let sample: Vec<f64> = index.iter().map(|i| c[*i] - m).collect();
            null_max = null_max.max(sharpe_of(&sample));
        }
        if null_max >= obs_max {
            at_or_above += 1;
        }
    }
    at_or_above as f64 / n_resamples as f64
}

/// Hansen's Superior Predictive Ability p-value (Hansen 2005, the consistent `SPA_c`), via the same
/// moving block bootstrap as [`whites_reality_check`].
///
/// Same question as White's test, two changes, both from the paper:
///
/// 1. **Studentized.** Each candidate's mean `d̄_k` is divided by `ω̂_k`, its bootstrap standard
///    error from the same draws (`ω̂_k² = (1/B) Σ_b (√n (d̄*_{k,b} − d̄_k))²`), so every candidate
///    is on one scale: `T = max(0, max_k √n d̄_k / ω̂_k)`.
/// 2. **Recentred only where the candidate could be good.** A candidate whose `t_k = √n d̄_k / ω̂_k`
///    lies below `−√(2 ln ln n)` keeps its own negative mean under the null instead of being moved
///    to zero, so it cannot reach the bootstrap maximum:
///    `T*_b = max(0, max_k √n (d̄*_{k,b} − μ_k) / ω̂_k)`, `μ_k = d̄_k` when `t_k ≥ −√(2 ln ln n)`,
///    else `0`.
///
/// The p-value is the share of draws with `T*_b ≥ T`. A candidate's column is its performance
/// against the benchmark (a strategy's per-bar PnL against holding cash is the PnL itself), one
/// inner `Vec` per candidate, truncated to the shortest. ONE index sequence is shared by every
/// column, exactly as in [`whites_reality_check`], so the draws depend on `n`, `block_len` and
/// `seed` alone: a candidate far below the threshold changes nothing, where White's test grows.
/// A candidate whose `ω̂_k` is zero (a constant column) has no `t` and takes no part.
///
/// Hansen draws with the stationary bootstrap; the moving blocks here keep the two tests on one
/// resampling scheme, which is the comparison this module offers.
///
/// Returns `1.0` (reject nothing) for an empty panel, a series shorter than one block, or a panel
/// with no candidate that varies.
pub fn hansens_studentized_spa(
    pnl_matrix: &[Vec<f64>],
    n_resamples: usize,
    block_len: usize,
    seed: u64,
) -> f64 {
    if pnl_matrix.is_empty() || block_len == 0 || n_resamples == 0 {
        return 1.0;
    }
    let n = pnl_matrix.iter().map(Vec::len).min().unwrap_or(0);
    if n < block_len + 1 {
        return 1.0;
    }
    let nf = n as f64;
    let root_n = nf.sqrt();
    let cols: Vec<&[f64]> = pnl_matrix.iter().map(|c| &c[..n]).collect();
    let means: Vec<f64> = cols.iter().map(|c| mean(c.iter().copied())).collect();

    // Every draw's resampled means, kept: the standard errors and the null statistics are both read
    // off the SAME draws, as in the paper.
    let mut rng = StdRng::seed_from_u64(seed);
    let mut index: Vec<usize> = Vec::with_capacity(n.div_ceil(block_len) * block_len);
    let mut draws: Vec<Vec<f64>> = Vec::with_capacity(n_resamples);
    for _ in 0..n_resamples {
        moving_block_indices(n, block_len, |max| rng.random_range(0..=max), &mut index);
        draws.push(cols.iter().map(|c| mean(index.iter().map(|&i| c[i]))).collect());
    }

    let omegas: Vec<f64> = (0..cols.len())
        .map(|k| {
            let mut squares = 0.0;
            for draw in &draws {
                let deviation = root_n * (draw[k] - means[k]);
                squares += deviation * deviation;
            }
            (squares / n_resamples as f64).sqrt()
        })
        .collect();
    // ln ln n is negative below n = e^e^0 = e; the threshold is then zero, never a NaN.
    let threshold = -(2.0 * libm::log(libm::log(nf))).max(0.0).sqrt();

    let mut observed = 0.0_f64;
    let mut centres = vec![0.0; cols.len()];
    for ((mean, omega), centre) in means.iter().zip(&omegas).zip(centres.iter_mut()) {
        if *omega == 0.0 {
            continue;
        }
        let t = root_n * mean / omega;
        observed = observed.max(t);
        if t >= threshold {
            *centre = *mean;
        }
    }

    let mut at_or_above = 0usize;
    for draw in &draws {
        let mut null_max = 0.0_f64;
        for ((resampled, omega), centre) in draw.iter().zip(&omegas).zip(&centres) {
            if *omega == 0.0 {
                continue;
            }
            null_max = null_max.max(root_n * (resampled - centre) / omega);
        }
        if null_max >= observed {
            at_or_above += 1;
        }
    }
    at_or_above as f64 / n_resamples as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An infinite bar makes every resample that draws it a NaN Sharpe; the interval is still read
    /// off the resampled Sharpes in a total order (`percentile`'s contract), never off a slice the NaNs
    /// left unsorted. The want side redraws the same blocks from the same seed.
    #[test]
    fn a_nan_sharpe_leaves_the_interval_read_off_sorted_sharpes() {
        let mut pnl: Vec<f64> = (0..1000).map(|i| ((i * 37 % 101) as f64 - 50.0) * 1e-4).collect();
        pnl[0] = f64::INFINITY;
        let (block_len, n_resamples, seed) = (50, 400, 7);
        let got = block_bootstrap_sharpe(&pnl, block_len, n_resamples, 252.0, seed);
        let mut rng = StdRng::seed_from_u64(seed);
        let mut index = Vec::new();
        let mut sharpes: Vec<f64> = (0..n_resamples)
            .map(|_| {
                moving_block_indices(
                    pnl.len(),
                    block_len,
                    |max| rng.random_range(0..=max),
                    &mut index,
                );
                let sample: Vec<f64> = index.iter().map(|&i| pnl[i]).collect();
                let (m, var) = mean_variance(sample.iter().copied(), 0.0);
                let sd = var.sqrt();
                if sd == 0.0 { 0.0 } else { m / sd * 252.0_f64.sqrt() }
            })
            .collect();
        assert!(sharpes.iter().any(|s| s.is_nan()), "some resample draws the infinite bar");
        sharpes.sort_by(f64::total_cmp);
        let want = (percentile(&sharpes, 0.05), percentile(&sharpes, 0.95));
        assert_eq!((got.0.to_bits(), got.1.to_bits()), (want.0.to_bits(), want.1.to_bits()));
        assert!(got.0 <= got.1, "{got:?}");
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
        assert_eq!(whites_reality_check(&[], 100, 24, 0), 1.0);
    }

    #[test]
    fn a_panel_shorter_than_a_block_cannot_reject_anything() {
        assert_eq!(whites_reality_check(&[vec![0.1; 5]], 100, 24, 0), 1.0);
    }

    #[test]
    fn the_pvalue_is_a_probability() {
        let a: Vec<f64> = (0..300).map(|i| ((i as f64) * 0.11).sin() * 0.01).collect();
        let b: Vec<f64> = (0..300).map(|i| ((i as f64) * 0.23).cos() * 0.01).collect();
        let p = whites_reality_check(&[a, b], 100, 24, 3);
        assert!((0.0..=1.0).contains(&p), "got {p}");
    }

    #[test]
    fn the_same_seed_reproduces_the_same_pvalue() {
        let a: Vec<f64> = (0..300).map(|i| ((i as f64) * 0.11).sin() * 0.01).collect();
        let p1 = whites_reality_check(std::slice::from_ref(&a), 100, 24, 9);
        let p2 = whites_reality_check(std::slice::from_ref(&a), 100, 24, 9);
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
            whites_reality_check(&two, 100, 24, 9),
            whites_reality_check(std::slice::from_ref(&a), 100, 24, 9)
        );
    }

    #[test]
    fn a_strongly_positive_candidate_is_rejected_near_zero() {
        // mean ~0.01, sd ~0.001 -> observed Sharpe well into double digits. If the null
        // centring (`c[*i] - m`) were dropped, null_max would approach obs_max and p would
        // sit near 0.5 instead of collapsing toward 0.
        let strong: Vec<f64> = (0..300).map(|i| 0.01 + ((i as f64) * 0.11).sin() * 0.001).collect();
        let p = whites_reality_check(&[strong], 200, 24, 11);
        assert!(p < 0.05, "got {p}");
    }

    /// `n` uniform draws in `[-1, 1)` from one seed: independent noise, which the sine fixtures
    /// above are not.
    fn noise(n: usize, seed: u64) -> Vec<f64> {
        let mut rng = StdRng::seed_from_u64(seed);
        (0..n).map(|_| rng.random_range(-1.0..1.0)).collect()
    }

    /// `level + scale * u` over [`noise`].
    fn noisy(level: f64, scale: f64, seed: u64) -> Vec<f64> {
        noise(300, seed).iter().map(|u| level + scale * u).collect()
    }

    #[test]
    fn the_spa_of_an_empty_short_or_constant_panel_rejects_nothing() {
        assert_eq!(hansens_studentized_spa(&[], 100, 24, 0), 1.0);
        assert_eq!(hansens_studentized_spa(&[vec![0.1; 5]], 100, 24, 0), 1.0);
        // A constant column has no standard error and so no t: nothing takes part.
        assert_eq!(hansens_studentized_spa(&[vec![0.1; 300]], 100, 24, 0), 1.0);
    }

    #[test]
    fn the_spa_is_a_reproducible_probability() {
        let panel = [noise(300, 1), noise(300, 2)];
        let p = hansens_studentized_spa(&panel, 200, 24, 5);
        assert_eq!(p, hansens_studentized_spa(&panel, 200, 24, 5));
        assert!((0.0..=1.0).contains(&p), "got {p}");
    }

    #[test]
    fn the_spa_rejects_a_strongly_positive_candidate() {
        let p = hansens_studentized_spa(&[noisy(0.01, 0.001, 3)], 200, 24, 11);
        assert!(p < 0.05, "got {p}");
    }

    #[test]
    fn the_spa_of_a_losing_panel_is_exactly_one() {
        // T = max(0, t) = 0 when every t is negative, and every bootstrap statistic is at least 0.
        let panel = [noisy(-0.01, 0.001, 4), noisy(-0.002, 0.01, 5)];
        assert_eq!(hansens_studentized_spa(&panel, 200, 24, 11), 1.0);
    }

    #[test]
    fn poor_candidates_raise_whites_pvalue_and_leave_hansens_unchanged() {
        // One candidate with a modest edge, then twenty clearly poor ones: every bar of each is
        // negative, so every resampled mean is too, and each t sits far below -sqrt(2 ln ln n).
        let good = noisy(0.0005, 0.01, 7);
        let mut panel = vec![good.clone()];
        panel.extend((0..20).map(|j| noisy(-0.01, 0.001, 100 + j)));
        let alone = std::slice::from_ref(&good);
        // Hansen: a poor candidate keeps its own mean under the null and never reaches the max.
        assert_eq!(
            hansens_studentized_spa(&panel, 400, 24, 13),
            hansens_studentized_spa(alone, 400, 24, 13)
        );
        // White: every candidate is recentred to zero, so each one adds a draw to the null max.
        let (white_panel, white_alone) =
            (whites_reality_check(&panel, 400, 24, 13), whites_reality_check(alone, 400, 24, 13));
        assert!(white_panel > white_alone, "panel {white_panel} vs alone {white_alone}");
    }
}

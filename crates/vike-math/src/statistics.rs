//! Sample statistics: the mean (compensated, and the plain-fold twin a platform-probe row pins), the
//! variance, the downside variance and the covariance, the bias-corrected skewness and excess
//! kurtosis, the Pearson correlation and its one closing formula, the doubled average ranks, the
//! linear-interpolation percentile and the nearest-rank one, the median and the high median, the
//! z-score, the sum-to-one normalisation, the correlation distance and the equal-width bin.
//!
//! ⚠ Two summation rules (this crate's module doc, rule 3). The canonical functions fold with
//! [`crate::summation::compensated_sum`]; [`plain_mean`] is the plain left fold (`.sum::<f64>()`), kept
//! ONLY because platform-probe rows were recorded through it: `crates/vike-analytics/tests/libm_platform_probe.rs`'s
//! `overfit::pbo_cscv` and `stats::whites_reality_check` rows. ONE variance ([`mean_variance`]) and ONE
//! Pearson ([`pearson`]) since 2026-11-18: their plain twins folded into these (decision 0294's
//! amendment of that day). A new caller takes the canonical.
//!
//! Each function returns what its doc says for a degenerate input. A caller that wants a `0.0` for
//! "no answer" writes that mapping at its own site, beside its own guard.

use crate::powers::{cube, fourth_power, square};
use crate::summation::compensated_sum;

/// The mean and the variance of `xs` with a `ddof` degrees-of-freedom divisor (`1.0` the sample
/// variance, `0.0` the population one), both compensated sums. The variance is
/// [`centred_sum_of_squares`]` / (n - ddof)`. An empty input answers `(NaN, NaN)`; `n == ddof` divides
/// by zero. The callers' guards (`n < 2`, zero dispersion) stay at each call site.
///
/// ⚠ The square is [`square`] since 2026-11-19, the same bits as the `libm::pow(x - mean, 2.0)` the
/// platform-probe rows that hash it were recorded through
/// (`crates/vike-analytics/tests/libm_platform_probe.rs`'s `statistics::mean_variance variance (powf)`
/// row): the `libm` crate's `y == 2` branch returns `x * x`, held bit for bit by
/// `crates/vike-math/src/statistics_tests.rs`'s `square_is_libm_pow_of_two_bit_for_bit`.
pub fn mean_variance<I>(xs: I, ddof: f64) -> (f64, f64)
where
    I: ExactSizeIterator<Item = f64> + Clone,
{
    let n = xs.len() as f64;
    let mean = compensated_sum(xs.clone()) / n;
    let var = centred_sum_of_squares(xs, mean) / (n - ddof);
    (mean, var)
}

/// The downside variance of `xs` below `target` (the second lower partial moment):
/// `Σ min(x - target, 0)² / (n - ddof)`, every value in the count (one at or above `target` adds zero),
/// the sum compensated in index order. Its square root is the Sortino ratio's downside deviation. No
/// guard: an empty input or `n == ddof` divides by zero, and the callers' guards stay at each call site
/// (vike-analytics' `metrics::sortino`, target `0` and ddof `1`; the closed-trade ledger's monthly
/// Sortino, the monthly risk-free rate and its divisor).
///
/// ⚠ The square is a multiply, the same bits as the `libm::pow(.., 2.0)` vike-analytics squared with
/// until 2026-11-18: the `libm` crate's `y == 2` branch returns `x * x`, and a value past
/// `min(.., 0)` is never the NaN or the one that would leave earlier.
pub fn downside_variance(xs: &[f64], target: f64, ddof: f64) -> f64 {
    let below = |x: f64| {
        let short = (x - target).min(0.0);
        short * short
    };
    compensated_sum(xs.iter().map(|&x| below(x))) / (xs.len() as f64 - ddof)
}

/// The compensated mean of `xs`; `NaN` for an empty input.
pub fn mean<I>(xs: I) -> f64
where
    I: IntoIterator<Item = f64>,
    I::IntoIter: ExactSizeIterator,
{
    let xs = xs.into_iter();
    let n = xs.len() as f64;
    compensated_sum(xs) / n
}

/// `Σ (x - mean)²`, compensated: the inner half of [`mean_variance`], for a caller that holds its
/// mean already (a total sum of squares, an R² denominator).
pub fn centred_sum_of_squares<I>(xs: I, mean: f64) -> f64
where
    I: Iterator<Item = f64>,
{
    compensated_sum(xs.map(|x| square(x - mean)))
}

/// The covariance of `a` and `b` over the shorter length `n`, with a `ddof` divisor:
/// `Σ (a - mean_a)(b - mean_b) / (n - ddof)`, every sum compensated. `covariance(a, b, d)` and
/// `covariance(b, a, d)` are the same bits (the same products, summed in the same order). An empty
/// pair answers `NaN`; the callers' `n < 2` guards stay at each call site.
pub fn covariance(a: &[f64], b: &[f64], ddof: f64) -> f64 {
    let n = a.len().min(b.len());
    let nf = n as f64;
    let mean_a = compensated_sum(a[..n].iter().copied()) / nf;
    let mean_b = compensated_sum(b[..n].iter().copied()) / nf;
    compensated_sum((0..n).map(|i| (a[i] - mean_a) * (b[i] - mean_b))) / (nf - ddof)
}

/// The PLAIN-fold mean, `xs.sum::<f64>() / n` (the same fold as `slice.iter().sum::<f64>()`): the twin of [`mean`] that the platform-probe
/// rows named in this module's doc pin (`overfit::pbo_cscv`'s column means,
/// `stats::whites_reality_check`'s candidate means). `NaN` for an empty input. A new caller takes
/// [`mean`].
pub fn plain_mean<I>(xs: I) -> f64
where
    I: ExactSizeIterator<Item = f64>,
{
    let n = xs.len() as f64;
    xs.sum::<f64>() / n
}

/// The one Pearson closing formula, `covariance / √(variance_x · variance_y)`, from moments the caller
/// already holds (sums, or sums over a divisor shared by all three). No guard and no clamp: the
/// streaming indicator cores clamp to `[-1, 1]` and answer `NaN` on a zero product, [`pearson`] answers
/// `0.0` on no dispersion, each at its own site.
#[inline]
pub fn correlation_of_moments(covariance: f64, variance_x: f64, variance_y: f64) -> f64 {
    covariance / (variance_x * variance_y).sqrt()
}

/// The Pearson correlation of `a` and `b` over their common prefix, from compensated moments over one
/// `n - 1` divisor: [`correlation_of_moments`] of [`covariance`] and the two [`mean_variance`]
/// variances. `0.0` when the prefix is shorter than two or either side has no dispersion (a variance
/// `<= 0`). Not clamped: a result one ulp past `±1` stays. A `NaN` input answers `NaN`.
///
/// ⚠ Its `0.0` for "no answer" is this function's documented contract, carried verbatim from
/// `vike-analytics`' `benchmark::correlation` and the plain twin it absorbed on 2026-11-18 (the
/// benchmark's correlation and its R², the rank IC, the lead-lag edges and the effective trial count
/// were written against it; the `benchmark::r_squared` platform-probe row pins its operation order).
pub fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let (a, b) = (&a[..n], &b[..n]);
    let (_, variance_a) = mean_variance(a.iter().copied(), 1.0);
    let (_, variance_b) = mean_variance(b.iter().copied(), 1.0);
    if variance_a <= 0.0 || variance_b <= 0.0 {
        return 0.0;
    }
    correlation_of_moments(covariance(a, b, 1.0), variance_a, variance_b)
}

/// The bias-corrected sample skewness of `xs` (the adjusted Fisher–Pearson coefficient, matching
/// `pandas.Series.skew`): `n / ((n - 1)(n - 2)) · Σ ((x - mean) / sd)³`, the sample deviation from
/// [`mean_variance`] at ddof 1, every sum compensated. `0.0` for fewer than three values or no
/// dispersion.
///
/// ⚠ Its `0.0` for "no answer" is this function's documented contract, carried verbatim from
/// `vike-analytics`' `metrics::returns_skewness`, whose platform-probe row pins its operation order.
///
/// ⚠ The cube is [`cube`] since 2026-11-19 (a `libm::pow(.., 3.0)` before), the owner's ruling for
/// the report's speed: it moves the last bits, measured in decision 0294's amendment of that day.
pub fn sample_skewness(xs: &[f64]) -> f64 {
    let n = xs.len();
    if n < 3 {
        return 0.0;
    }
    let nf = n as f64;
    let (mean, var) = mean_variance(xs.iter().copied(), 1.0);
    let std = var.sqrt();
    if std == 0.0 {
        return 0.0;
    }
    let sum_cubed = compensated_sum(xs.iter().map(|&r| cube((r - mean) / std)));
    (nf / ((nf - 1.0) * (nf - 2.0))) * sum_cubed
}

/// The bias-corrected sample EXCESS kurtosis of `xs` (the adjusted Fisher–Pearson coefficient,
/// matching `pandas.Series.kurt`; `0.0` for a normal law), the sample deviation from [`mean_variance`]
/// at ddof 1, every sum compensated. `0.0` for fewer than four values or no dispersion.
///
/// ⚠ Its `0.0` for "no answer" is this function's documented contract, carried verbatim from
/// `vike-analytics`' `metrics::returns_kurtosis`, whose platform-probe row pins its operation order.
///
/// ⚠ The fourth power is [`fourth_power`] since 2026-11-19 (a `libm::pow(.., 4.0)` before), the
/// owner's ruling for the report's speed: it moves the last bits, measured in decision 0294's
/// amendment of that day. The `(n - 1)²` is [`square`], the same bits as the `pow` it replaced.
pub fn sample_excess_kurtosis(xs: &[f64]) -> f64 {
    let n = xs.len();
    if n < 4 {
        return 0.0;
    }
    let nf = n as f64;
    let (mean, var) = mean_variance(xs.iter().copied(), 1.0);
    let std = var.sqrt();
    if std == 0.0 {
        return 0.0;
    }
    let sum_quartic = compensated_sum(xs.iter().map(|&r| fourth_power((r - mean) / std)));
    let term1 = (nf * (nf + 1.0)) / ((nf - 1.0) * (nf - 2.0) * (nf - 3.0)) * sum_quartic;
    let term2 = 3.0 * square(nf - 1.0) / ((nf - 2.0) * (nf - 3.0));
    term1 - term2
}

/// Twice each value's 1-based AVERAGE rank (ties share the mean of their ranks, Spearman's
/// convention), so a tie's half rank stays an integer: a tie group over sorted positions `p..p + g`
/// (0-based) ranks `(2p + g + 1) / 2`, doubled `2p + g + 1`. Sorted by [`f64::total_cmp`].
pub fn doubled_average_ranks(values: &[f64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    let mut ranks = vec![0; values.len()];
    let mut p = 0;
    while p < order.len() {
        let mut g = 1;
        while p + g < order.len() && values[order[p + g]] == values[order[p]] {
            g += 1;
        }
        for &i in &order[p..p + g] {
            ranks[i] = 2 * p + g + 1;
        }
        p += g;
    }
    ranks
}

/// The `q`-quantile (`0 <= q <= 1`) of `sorted_vals` by linear interpolation between the two
/// neighbouring order statistics, matching `numpy.percentile`. `sorted_vals` must be sorted
/// ascending; `0.0` for an empty slice. (`n == 1` needs no special case: the interpolation formula
/// below already falls through to `sorted_vals[0]` bit-identically — `lo == 0`, `frac == 0.0`,
/// `lo + 1 < n` is false, so the `else` arm returns `sorted_vals[0]` directly either way.)
///
/// ⚠ **This is the workspace's ONE percentile, and it went `pub` because a second one had already
/// been written.** `crates/vike-backtesthub/src/bin/cheap_np_depth.rs`'s `pct` computed a
/// NEAREST-RANK answer — `v[round((n - 1) * q)]`, always an observed sample, never an
/// interpolant — and printed it under the same `p10`..`p90` names an operator sizes positions
/// from. The two agree only where `(n - 1) * q` lands on an integer or where the two samples
/// being interpolated between are equal: on `[0, 1, 2, 3]` at `q = 0.5` the nearest-rank reading
/// is `2.0` against this function's `1.5`. Reach for this one; do not write a third. The nearest-rank
/// reading has one home, [`nearest_rank_of_sorted`], for a display scale, never for a printed `pNN`.
///
/// ⚠ **Sorting is the CALLER's job and the comparator is part of the contract.** This body does
/// no comparison at all, so it cannot detect an unsorted slice — it will silently interpolate
/// between two arbitrary neighbours. Sort with [`f64::total_cmp`], never with
/// `partial_cmp().unwrap_or(Ordering::Equal)`: only the former is a total order, so only the former
/// actually leaves the slice sorted when a NaN is present.
///
/// ⚠ Its `0.0` for an empty slice is this function's documented contract, carried verbatim from
/// `vike-analytics`' `metrics` module (every artifact stamped with [`PERCENTILE_METHOD`] was written
/// against it).
pub fn percentile(sorted_vals: &[f64], q: f64) -> f64 {
    if sorted_vals.is_empty() {
        return 0.0;
    }
    let n = sorted_vals.len();
    let pos = q * (n - 1) as f64;
    let lo = pos as usize;
    let frac = pos - lo as f64;
    if lo + 1 < n {
        sorted_vals[lo] * (1.0 - frac) + sorted_vals[lo + 1] * frac
    } else {
        sorted_vals[lo]
    }
}

/// The NAME of the convention [`percentile`] computes — what an artifact writes down when it has to
/// say WHICH percentile produced its numbers. It lives beside the body rather than beside any
/// artifact, so the string and the algorithm it names cannot come to disagree.
///
/// ⚠ **Change this string in the same edit that changes [`percentile`]'s body, and in no other.**
/// It exists because the numbers moved once already and no artifact said so:
/// `crates/vike-backtesthub/src/bin/cheap_np_depth.rs`'s `pct` published a NEAREST-RANK reading under
/// the same `p10`..`p90` keys this function now answers, and the two disagree in BOTH directions on
/// one sample — so a file from either side of that change is indistinguishable from a file from the
/// other, and no scale factor converts one into the other.
///
/// **A name rather than a version number, because the discriminating fact is WHICH ALGORITHM, and
/// this tree holds more than two of them.** Besides the interpolation above there is the
/// `d.len() / 2` median `crates/vike-backtesthub/src/bin/cheap_np_askgate/main.rs`'s `anchor_json`
/// used to spell, and the floor-rank `pct` over `u64` in `crates/vike-core/tests/runtime_latency.rs`,
/// which is deliberately a convention of its own. `numpy_linear` names this one unambiguously: the
/// method `numpy.percentile` defaults to.
pub const PERCENTILE_METHOD: &str = "numpy_linear";

/// The median of a SORTED slice: its middle value, or the mean of the two middle ones,
/// `(lower + upper) / 2.0`. The one tail of the rolling medians (`rolling::rolling_median`,
/// `vike-indicators`' `MedianCore` and `strategy_kit::FlexiMedian`); each window keeps its own `NaN`
/// rule. Panics on an empty slice.
#[inline]
pub fn median_of_sorted(s: &[f64]) -> f64 {
    let mid = s.len() / 2;
    if s.len() % 2 == 1 { s[mid] } else { (s[mid - 1] + s[mid]) / 2.0 }
}

/// The median of `values`, sorted here by [`f64::total_cmp`] (a `NaN` sorts to an end and counts as a
/// value): [`median_of_sorted`] of them, the mean of the two middle ones on an even length. `None` for
/// an empty input.
pub fn median(mut values: Vec<f64>) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    (!values.is_empty()).then(|| median_of_sorted(&values))
}

/// The HIGH median of a SORTED slice: its element at `len / 2`, the UPPER of the two middle ones on an
/// even length, so the answer is always an observed value (a count stays a count, a timestamp gap a
/// gap). `None` for an empty slice. Sorting, and any filter before it, are the caller's.
#[inline]
pub fn high_median_of_sorted<T: Copy>(sorted: &[T]) -> Option<T> {
    sorted.get(sorted.len() / 2).copied()
}

/// The NEAREST-RANK `q`-quantile of a SORTED slice: its element at `round((len - 1) · q)`, always an
/// observed value, never an interpolant. A different statistic from [`percentile`] (which interpolates
/// and is the one a report's `p10`..`p90` print); this one scales a display to an observed value.
/// `None` for an empty slice, or for a `q` above `1` that rounds past the last element.
pub fn nearest_rank_of_sorted<T: Copy>(sorted: &[T], q: f64) -> Option<T> {
    let last = sorted.len().checked_sub(1)?;
    sorted.get((last as f64 * q).round() as usize).copied()
}

/// The z-score `(value - mean) / deviation`. No guard: a zero deviation answers `±∞` or `NaN`, and a
/// caller that maps a constant series to `0.0` writes that at its own site.
#[inline]
pub fn z_score(value: f64, mean: f64, deviation: f64) -> f64 {
    (value - mean) / deviation
}

/// `values` divided by their compensated total, so they sum to one up to rounding. `None` unless the
/// total is finite and positive (an empty input's total is `0`).
pub fn normalised_to_unit_sum(values: &[f64]) -> Option<Vec<f64>> {
    let total = compensated_sum(values.iter().copied());
    (total.is_finite() && total > 0.0).then(|| values.iter().map(|v| v / total).collect())
}

/// The correlation distance `√((1 - ρ) / 2)`: `0` for `ρ = 1`, `1` for `ρ = -1`; a `ρ` rounded past `1`
/// answers `0` rather than the square root of a negative number. A `NaN` correlation answers `NaN`.
#[inline]
pub fn correlation_distance(correlation: f64) -> f64 {
    let half = (1.0 - correlation) / 2.0;
    if half < 0.0 { 0.0 } else { half.sqrt() }
}

/// The bin of `value` among `bins` equal-width bins of `width` from `low`: `(value - low) / width`
/// truncated, clamped to the last bin. Anything below `low`, and a `NaN`, lands in bin 0 (the
/// saturating float-to-integer cast). A division, never a multiplication by a reciprocal: a bin is a
/// discrete answer, and a value on an edge must not move. `bins` must be at least `1`.
#[inline(always)]
pub fn equal_width_bin(value: f64, low: f64, width: f64, bins: usize) -> usize {
    (((value - low) / width) as usize).min(bins - 1)
}

#[cfg(test)]
#[path = "statistics_tests.rs"]
mod tests;

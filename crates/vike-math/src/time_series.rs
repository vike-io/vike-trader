//! Measures of one series in time: the autocovariance and the sample autocorrelation function, the
//! Bartlett long-run variance and the Newey–West t-statistic of a mean; and of two aligned series,
//! their cross-correlation at every lag up to a bound with its lead-lag ratio ([`lead_lag`]); and a
//! value's fall from the series' peak ([`drawdown_fraction`]).
//!
//! Pure functions over `&[f64]`, folds in index order, `libm` for every transcendental (decision 0032).

use crate::statistics::pearson;

/// The Bartlett-kernel long-run variance of a series already centred on its mean, `T` long:
///
/// `s²(l) = (1/T) Σ e_t² + (2/T) Σ_(s=1..l) (1 − s/(l+1)) Σ_(t>s) e_t e_(t−s)`.
///
/// The ONE copy (decision 0294; REVIEW-LAWS H1): the KPSS statistic's denominator
/// (`stationarity_tests::kpss_statistic`) and the Newey–West t-statistic's ([`newey_west_t_statistic`]).
/// The weights use the bandwidth `lags` as given, as Kwiatkowski, Phillips, Schmidt and Shin (1992,
/// eq. 10) and Newey and West (1987) write them; an autocovariance past `T − 1` is zero, so the sum
/// stops there and the weights do not change.
///
/// ⚠ The arithmetic is KPSS's, to the operation, and the indicator row's bits rest on it: `e·e` summed
/// plainly, each lag's products summed plainly in index order, then added to the running total as
/// `((2·w)·Σ)/T`. A rewrite that is algebraically equal is still a change. `NaN` for an empty series.
pub fn bartlett_long_run_variance(centred: &[f64], lags: usize) -> f64 {
    let mut squares = 0.0;
    for e in centred {
        squares += e * e;
    }
    bartlett_long_run_variance_of_squares(centred, lags, squares)
}

/// [`bartlett_long_run_variance`] from its first sum, `squares = Σ e_t²` (plain, in index order), which a
/// caller already folding the series in one pass hands over instead of a second pass: the KPSS
/// statistic folds it beside its partial sums, as the indicator row always did.
#[inline]
pub(crate) fn bartlett_long_run_variance_of_squares(
    centred: &[f64],
    lags: usize,
    squares: f64,
) -> f64 {
    let n = centred.len();
    let t = n as f64;
    let mut long_run = squares / t;
    for s in 1..=lags.min(n.saturating_sub(1)) {
        let mut covariance = 0.0;
        for i in s..n {
            covariance += centred[i] * centred[i - s];
        }
        long_run += 2.0 * (1.0 - s as f64 / (lags as f64 + 1.0)) * covariance / t;
    }
    long_run
}

/// Bartlett-kernel Newey-West t-statistic for the mean of an autocorrelated series:
/// `t = mean / √(V/n)` with `V` the [`bartlett_long_run_variance`] of the centred series at bandwidth
/// `lag`.
///
/// A per-bar PnL series is autocorrelated by construction — a position held for many bars earns
/// correlated returns — so the ordinary t-statistic overstates significance. This is the
/// correction, and `lag` should be at least the holding horizon. `NaN` values are dropped first.
///
/// Returns `0.0` rather than a NaN or an infinity for a degenerate input (fewer than two
/// observations, or a non-positive long-run variance): callers compare it against a threshold, and
/// zero fails every threshold, which is the safe direction. (This `0.0` is the function's documented
/// contract, carried verbatim from `vike-analytics`' `stats` module.)
pub fn newey_west_t_statistic(x: &[f64], lag: usize) -> f64 {
    let s: Vec<f64> = x.iter().copied().filter(|v| !v.is_nan()).collect();
    let n = s.len();
    if n < 2 {
        return 0.0;
    }
    let nf = n as f64;
    let mean = s.iter().sum::<f64>() / nf;
    let centered: Vec<f64> = s.iter().map(|v| v - mean).collect();
    let long_run_var = bartlett_long_run_variance(&centered, lag);
    if long_run_var <= 0.0 {
        return 0.0;
    }
    mean / (long_run_var / nf).sqrt()
}

/// The sample autocorrelation function of `x` at the lags `1..=max_lag`, the Box-Jenkins estimator:
/// each lag's autocovariance over the lag-0 one, both about the WHOLE sample's mean and both divided
/// by `n` (so every value is in `[-1, 1]` and the sequence is positive definite).
///
/// Read on returns it is near zero for a liquid market; read on ABSOLUTE returns it is positive and
/// decays slowly, which is volatility clustering (`vike-analytics`' `synthetic::stylized_facts` reads
/// both).
///
/// A lag at or beyond `x.len()`, and every lag of a series with no dispersion, is `NaN`: undefined,
/// not zero. A `NaN` inside `x` makes every lag `NaN`.
pub fn autocorrelation(x: &[f64], max_lag: usize) -> Vec<f64> {
    if x.is_empty() {
        return vec![f64::NAN; max_lag];
    }
    let mean = crate::summation::compensated_sum(x.iter().copied()) / x.len() as f64;
    let centered: Vec<f64> = x.iter().map(|v| v - mean).collect();
    let variance = autocovariance(&centered, 0);
    (1..=max_lag)
        .map(|lag| {
            if lag < x.len() && variance > 0.0 {
                autocovariance(&centered, lag) / variance
            } else {
                f64::NAN
            }
        })
        .collect()
}

/// `γ_lag` of a series already centred on its mean: the sum of its products `lag` apart, over its
/// length. The ONE autocovariance, [`autocorrelation`]'s;
/// `lag < centered.len()`.
pub fn autocovariance(centered: &[f64], lag: usize) -> f64 {
    let n = centered.len();
    centered[lag..].iter().zip(&centered[..n - lag]).map(|(a, b)| a * b).sum::<f64>() / n as f64
}

/// The natural log of a positive finite value, `libm::log(x)`; `NaN` unless `0 < x < ∞`. The one rule
/// every log-price reader shares (a price a logarithm may be taken of: finite and strictly positive).
#[inline(always)]
pub fn log_of_positive(x: f64) -> f64 {
    if x > 0.0 && x < f64::INFINITY { libm::log(x) } else { f64::NAN }
}

/// The log of a ratio, `libm::log(numerator / denominator)`; `NaN` unless both are above zero. A log
/// return is `log_ratio(close, previous_close)`. The argument ORDER is the caller's: `ln(a / b)` is not
/// bit for bit `-ln(b / a)` (the two divisions round differently).
#[inline(always)]
pub fn log_ratio(numerator: f64, denominator: f64) -> f64 {
    if numerator > 0.0 && denominator > 0.0 { libm::log(numerator / denominator) } else { f64::NAN }
}

/// How far `value` stands below `peak`, as a fraction of the peak: `(peak - value) / peak`, in that
/// operand order (the spelling of `vike-analytics`' `DrawdownWalk`, the core's drawdown latch and the
/// alert rule). `None` unless the peak is above zero: a zero, negative or `NaN` peak has no fraction to
/// measure against, and the caller that shows `0.0` there, or refuses, says so at its own site.
/// Not clamped: a value above the peak answers a NEGATIVE fraction, which a caller holding the running
/// maximum never passes.
///
/// ⚠ Two spellings that look the same are not: `1.0 - value / peak` is equal algebraically and not bit
/// for bit (two roundings in another order), and a guard of `peak != 0.0` measures a negative peak
/// this one refuses. A reader moving here from either moves its numbers.
#[inline]
pub fn drawdown_fraction(peak: f64, value: f64) -> Option<f64> {
    (peak > 0.0).then_some((peak - value) / peak)
}

/// The rescaled range R/S of one block of returns: the range of the running sum of deviations from
/// the block mean (the starting zero included) over the block's population standard deviation.
/// `None` for a block that never moves, whose deviation is zero. Plain left folds in index order
/// (the `hurst` indicator's bits rest on them).
pub fn rescaled_range(block: &[f64]) -> Option<f64> {
    if crate::rolling::is_constant_window(block) {
        return None;
    }
    let n = block.len() as f64;
    let mean = block.iter().sum::<f64>() / n;
    let (mut run, mut hi, mut lo, mut ss) = (0.0, 0.0f64, 0.0f64, 0.0);
    for r in block {
        let dev = r - mean;
        run += dev;
        hi = hi.max(run);
        lo = lo.min(run);
        ss += dev * dev;
    }
    let sd = (ss / n).sqrt();
    if sd > 0.0 { Some((hi - lo) / sd) } else { None }
}

/// The Shannon entropy of `values` binned into `counts.len()` equal-width bins over their own range,
/// divided by `log_bins` (the caller's `ln(bins)`, so the answer is in `[0, 1]`); `0` for a window of
/// identical values, `NaN` for fewer than two bins or a value that is not finite. `counts` is the
/// caller's scratch, overwritten. The highest value lands on `bins` exactly and belongs to the last
/// bin, as numpy's histogram does; a value's bin is `((v - low) * (bins / (high - low))) as usize`.
pub fn shannon_entropy_of_bins(values: &[f64], counts: &mut [usize], log_bins: f64) -> f64 {
    if counts.len() < 2 || values.iter().any(|r| !r.is_finite()) {
        return f64::NAN;
    }
    if crate::rolling::is_constant_window(values) {
        return 0.0;
    }
    let mut low = values[0];
    let mut high = values[0];
    for &r in values {
        low = low.min(r);
        high = high.max(r);
    }
    let last = counts.len() - 1;
    let scale = counts.len() as f64 / (high - low);
    counts.fill(0);
    for &r in values {
        counts[(((r - low) * scale) as usize).min(last)] += 1;
    }
    let n = values.len() as f64;
    let mut h = 0.0;
    for &c in counts.iter() {
        if c > 0 {
            let p = c as f64 / n;
            h -= p * libm::log(p);
        }
    }
    h / log_bins
}

/// Lempel and Ziv's `c(n)` of the symbol sequence `s`: the number of components of its exhaustive
/// history, by Kaspar and Schuster's scan (`i` the start of the copy being tried, `u` where the
/// current component starts, `v` the length matched, `v_max` the longest match from any start).
/// Symbols compare by `==`. Lempel and Ziv, "On the Complexity of Finite Sequences" (1976); Kaspar and
/// Schuster, Physical Review A 36(2) (1987). Requires a non-empty `s`.
pub fn lempel_ziv_complexity(s: &[f64]) -> usize {
    let n = s.len();
    let (mut i, mut c, mut u, mut v, mut v_max) = (0, 1, 1, 1, 1);
    while u + v <= n {
        if s[i + v - 1] == s[u + v - 1] {
            v += 1;
        } else {
            v_max = v_max.max(v);
            i += 1;
            if i == u {
                c += 1;
                u += v_max;
                v = 1;
                i = 0;
                v_max = 1;
            } else {
                v = 1;
            }
        }
    }
    if v != 1 { c + 1 } else { c }
}

/// The first `width` weights of the fractional difference `(1 - B)^d`, `w_0 = 1` and
/// `w_k = -w_(k-1) (d - k + 1) / k` (López de Prado, *Advances in Financial Machine Learning*, 5.4.2),
/// in `k` order (`w_0` first).
pub fn fractional_difference_weights(d: f64, width: usize) -> Vec<f64> {
    let mut weights = Vec::with_capacity(width);
    let mut w = 1.0;
    for k in 0..width {
        if k > 0 {
            w = -w * (d - (k - 1) as f64) / k as f64;
        }
        weights.push(w);
    }
    weights
}

/// The Haar wavelet detail energy of `returns` at one scale: over the whole blocks of `block` values
/// (oldest first), `Σ (sum of the older half - sum of the newer half)^2 / block`; `NaN` when a value
/// is `NaN`. Each half is a plain fold from `0.0`.
pub fn haar_detail_energy(returns: &[f64], block: usize) -> f64 {
    if returns.iter().any(|r| r.is_nan()) {
        return f64::NAN;
    }
    let half = block / 2;
    let mut energy = 0.0;
    for chunk in returns.chunks_exact(block) {
        let older = chunk[..half].iter().fold(0.0, |sum, &r| sum + r);
        let newer = chunk[half..].iter().fold(0.0, |sum, &r| sum + r);
        energy += crate::powers::square(older - newer);
    }
    energy / block as f64
}

/// The power of one frequency of `series` by Goertzel's recurrence (Goertzel 1958): with
/// `coefficient = 2 cos(2 pi / P)`, `s = y + coefficient s[1] - s[2]` over the series, then
/// `s[1]^2 + s[2]^2 - coefficient s[1] s[2]`, the squared modulus of the Fourier sum at `1/P`.
pub fn goertzel_power(series: &[f64], coefficient: f64) -> f64 {
    let (mut s1, mut s2) = (0.0, 0.0);
    for &y in series {
        let s = y + coefficient * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    s1 * s1 + s2 * s2 - coefficient * s1 * s2
}

/// The cross-correlation of two aligned series, from [`lead_lag`].
#[derive(Clone, Debug, PartialEq)]
pub struct LeadLag {
    /// `ρ(k) = corr(x_t, y_{t+k})` for `k = -max_lag ..= max_lag`, at index `k + max_lag`, each over
    /// the pairs both series hold: a positive `k` is `x` moving `k` steps before `y`.
    pub correlations: Vec<f64>,
    /// The lag of the largest `|ρ(k)|`; on a tie the smaller `|k|`, then the positive one.
    pub peak_lag: i64,
    /// `ρ(peak_lag)`.
    pub peak_correlation: f64,
    /// `Σ_{k=1..max_lag} ρ(k)² / Σ_{k=1..max_lag} ρ(-k)²` (Huth and Abergel, "High frequency lead/lag
    /// relationships — empirical facts", Journal of Empirical Finance 26, 2014, section 3.2): above
    /// one `x` leads `y`, below one `y` leads `x`. Infinite when only `x`'s side correlates, one when
    /// neither does.
    pub lead_lag_ratio: f64,
}

/// The cross-correlation of `x` and `y` (aligned: one entry per common observation, returns rather
/// than prices) at the lags `-max_lag ..= max_lag`, with its peak and lead-lag ratio.
///
/// `None` when the lengths differ, `max_lag` is zero, a lag would leave fewer than two pairs, or an
/// entry is not finite. A series that does not vary correlates `0` ([`crate::statistics::pearson`]'s
/// documented contract).
pub fn lead_lag(x: &[f64], y: &[f64], max_lag: usize) -> Option<LeadLag> {
    let n = x.len();
    if max_lag == 0 || y.len() != n || n < max_lag + 2 || x.iter().chain(y).any(|v| !v.is_finite())
    {
        return None;
    }
    let bound = i64::try_from(max_lag).ok()?;
    let correlations: Vec<f64> = (-bound..=bound)
        .map(|k| {
            let shift = k.unsigned_abs() as usize;
            if k >= 0 {
                pearson(&x[..n - shift], &y[shift..])
            } else {
                pearson(&x[shift..], &y[..n - shift])
            }
        })
        .collect();
    let at = |k: i64| correlations[(k + bound) as usize];
    let mut peak_lag = 0;
    let (mut leading, mut lagging) = (0.0, 0.0);
    for step in 1..=bound {
        for k in [step, -step] {
            if at(k).abs() > at(peak_lag).abs() {
                peak_lag = k;
            }
        }
        leading += at(step) * at(step);
        lagging += at(-step) * at(-step);
    }
    let lead_lag_ratio = if lagging > 0.0 {
        leading / lagging
    } else if leading > 0.0 {
        f64::INFINITY
    } else {
        1.0
    };
    let peak_correlation = at(peak_lag);
    Some(LeadLag { correlations, peak_lag, peak_correlation, lead_lag_ratio })
}

#[cfg(test)]
#[path = "time_series_tests.rs"]
mod tests;

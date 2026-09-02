//! Statistics indicators — faithful f64 port of vike-trader-app
//! `core/indicators/statistics.py`. Ports ONLY the single-series indicators
//! (`inputs=["close"]`): the linear-regression family (linearreg / slope / angle
//! / intercept / tsf), var, zscore, skew, kurtosis, mad, std_error,
//! std_error_bands, rank_correlation. The two-series ones (`beta`, `correl`,
//! `correl_log`, all with `inputs=["close","benchmark"]`) are handled elsewhere
//! and are NOT ported here.
//!
//! Each `batch_*` is a line-for-line port of the Python function (Python `None`
//! → `f64::NAN`; builtin `sum()` over a window → naive `.iter().sum()`, matching
//! the volatility/momentum ports; `x ** k` → `crates/vike-indicators/src/math.rs`'s `sq` / `cube`
//! / `quart`, NOT `x.powi(k)` — that spelling is a libcall in an MSVC `dev` build rather than the
//! "repeated multiply" this line asserted for months, which is the whole of `sq`'s doc).
//! `math.degrees(math.atan(b))` is ported as the exact CPython arithmetic
//! `atan(b) * (180.0 / PI)` (not Rust's `to_degrees()`, whose precomputed
//! constant can differ by a ULP) — over `libm::atan` rather than `f64::atan`,
//! because IEEE 754 does not require `atan` to be correctly rounded and the
//! two boxes disagree on its last bit (see the crate manifest).
//! Streaming is history-recompute via
//! [`hist_indicator!`] (correct-by-construction for these causal indicators).
#![allow(clippy::needless_range_loop)]

use super::macros::hist_indicator;
use super::pback;
use crate::math::{sq, Columns};
use vike_model::Bar;

// ============================ shared local helpers =========================

/// OLS fit over `x = 0..p-1` — port of `statistics.py:_ols`. Returns
/// `(slope, intercept)` via the closed form
/// `b = (p·Σxy − Σx·Σy)/(p·Σx² − (Σx)²)`, `a = (Σy − b·Σx)/p`. The integer
/// `Σx`/`Σx²` closed forms are computed exactly then cast (matching Python's
/// `int-expr / float`); the `Σy`/`Σxy` folds are naive left folds.
fn ols(window: &[f64]) -> (f64, f64) {
    let p = window.len();
    let sx = (p * (p - 1)) as f64 / 2.0; // Σ x  where x = 0,1,..,p-1
    let sx2 = (p * (p - 1) * (2 * p - 1)) as f64 / 6.0; // Σ x²
    let sy: f64 = window.iter().sum();
    let mut sxy = 0.0;
    for i in 0..p {
        sxy += i as f64 * window[i];
    }
    let denom = p as f64 * sx2 - sx * sx;
    if denom == 0.0 {
        return (0.0, sy / p as f64);
    }
    let b = (p as f64 * sxy - sx * sy) / denom;
    let a = (sy - b * sx) / p as f64;
    (b, a)
}

/// Rolling linearreg value `a + b*(period-1)` — port of `statistics.py:linearreg`.
/// Reused by `std_error_bands` (its `mid` line).
fn linearreg_series(v: &[f64], period: usize) -> Vec<f64> {
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    for i in (period - 1)..n {
        let (b, a) = ols(&v[i + 1 - period..=i]);
        out[i] = a + b * (period - 1) as f64;
    }
    out
}

/// Rolling standard error of the OLS estimate — port of `statistics.py:std_error`.
/// `se = sqrt(Σresid² / (period-2)) / sqrt(period)`. Reused by `std_error_bands`.
fn std_error_series(v: &[f64], period: usize) -> Vec<f64> {
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    for i in (period - 1)..n {
        let window = &v[i + 1 - period..=i];
        let (b, a) = ols(window);
        let mut residuals_sq = 0.0;
        for j in 0..period {
            residuals_sq += sq(window[j] - (a + b * j as f64));
        }
        if period > 2 {
            let mse = residuals_sq / (period - 2) as f64;
            out[i] = mse.sqrt() / (period as f64).sqrt();
        } else {
            out[i] = 0.0;
        }
    }
    out
}

// ============================ batch kernels ================================

fn batch_linearreg(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    vec![linearreg_series(&v, period)]
}

fn batch_linearreg_slope(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    for i in (period - 1)..n {
        let (b, _a) = ols(&v[i + 1 - period..=i]);
        out[i] = b;
    }
    vec![out]
}

fn batch_linearreg_angle(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    for i in (period - 1)..n {
        let (b, _a) = ols(&v[i + 1 - period..=i]);
        // math.degrees(math.atan(b)) — CPython arithmetic exactly.
        out[i] = libm::atan(b) * (180.0 / std::f64::consts::PI);
    }
    vec![out]
}

fn batch_linearreg_intercept(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    for i in (period - 1)..n {
        let (_b, a) = ols(&v[i + 1 - period..=i]);
        out[i] = a;
    }
    vec![out]
}

fn batch_tsf(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    for i in (period - 1)..n {
        let (b, a) = ols(&v[i + 1 - period..=i]);
        out[i] = a + b * period as f64;
    }
    vec![out]
}

/// Population variance of the `period` values ending at `i` — a TWO-PASS window fold
/// (`mean`, then `Σ(x - mean)²`), the shape `volatility.rs`'s `bollinger_vals` and `base.rs`'s
/// `batch_bollinger` already used.
///
/// ⚠ **This carried a `run_sum` / `run_sum2` pair and closed with
/// `run_sum2/period - mean*mean`, and BOTH halves of that were bugs.**
///
/// 1. The carried pair made the value a function of all history, not of the window: re-run over a
///    truncated tail and the identical window returns different bits, so `var`/`zscore` were wrong
///    past `hist_indicator!`'s drain on any long series (MEASURED 64/64 and 231/232 mismatching
///    retained lengths at `period` 20 and 200; 0 of each now).
/// 2. `E[x²] - E[x]²` is catastrophic cancellation on price data, where the mean dwarfs the
///    spread. That is not merely imprecise — it FLIPS `batch_zscore`'s `if sd != 0.0` guard, which
///    decides between a NUMBER and a NaN. Measured on a dead-flat window at exactly 100.0: the old
///    form returns `var = 2.9e-11` and `z = 5.0e-8` on a full run and `var = 0.0`, `z = NaN` on a
///    truncated one. The two-pass form returns exactly `0.0` and `NaN` either way there.
///
/// ⚠ **"there" is load-bearing, and this doc used to end with the claim that it was not.** It
/// said the two-pass form is zero on any flat window "because a window of identical values has
/// `x - mean == 0` exactly", which is FALSE: `mean` is `Σx / n`, and that only rounds back to the
/// repeated value for some values. `100.0` is one of them, which is why the sentence survived —
/// its own test used that fixture. `0.1` is not, and a flat window there returned a
/// rounding-sized variance and a spurious `z` of order 1. The exact-zero property is now a
/// DECISION rather than a hope: `window::rolling_var` returns `0.0` outright for a window whose
/// values are all equal. ⚠ Not because pandas does — pandas' rolling accumulator carries history
/// and is the very bug described above, measured; `crates/vike-indicators/src/window.rs`'s
/// `is_constant_window` carries that measurement and the argument for diverging from it.
///
/// The old `.max(0.0)` clamp went with it: it existed to repair the cancellation's small NEGATIVE
/// results, and a sum of squares over a positive count cannot be negative. Keeping it would also
/// keep a trap — `f64::max` returns the non-NaN operand, so a NaN input silently became `0.0`.
fn batch_var(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    vec![crate::window::rolling_var(&v, crate::window::WindowSpec::indicator(period))]
}

/// Rolling z-score. Same two-pass window variance as [`batch_var`] — see its doc for why the
/// `run_sum`/`run_sum2` pair and the `E[x²] - E[x]²` closing form both had to go, and note that
/// the guard — now `window.rs`'s `zscore`, `sd.is_nan() || sd == 0.0` (it moved there with the
/// Task 2 reroute) — is the specific thing the cancellation was flipping.
fn batch_zscore(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    vec![crate::window::zscore(&v, crate::window::WindowSpec::indicator(period), None)]
}

fn batch_skew(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    if period < 3 {
        return vec![out];
    }
    for i in (period - 1)..n {
        let window = &v[i + 1 - period..=i];
        let p = period;
        let mean = window.iter().sum::<f64>() / p as f64;
        let diffs: Vec<f64> = window.iter().map(|x| x - mean).collect();
        let m2 = diffs.iter().map(|d| d * d).sum::<f64>() / p as f64;
        if m2 == 0.0 {
            out[i] = 0.0;
            continue;
        }
        let sd = m2.sqrt();
        let m3 = diffs.iter().map(|d| crate::math::cube(*d)).sum::<f64>() / p as f64;
        out[i] = (m3 / crate::math::cube(sd)) * (((p * (p - 1)) as f64).sqrt() / (p - 2) as f64);
    }
    vec![out]
}

fn batch_kurtosis(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    if period < 4 {
        return vec![out];
    }
    for i in (period - 1)..n {
        let window = &v[i + 1 - period..=i];
        let p = period;
        let mean = window.iter().sum::<f64>() / p as f64;
        let diffs: Vec<f64> = window.iter().map(|x| x - mean).collect();
        let m2 = diffs.iter().map(|d| d * d).sum::<f64>() / p as f64;
        if m2 == 0.0 {
            out[i] = 0.0;
            continue;
        }
        let sd = m2.sqrt();
        let m4 = diffs.iter().map(|d| crate::math::quart(*d)).sum::<f64>() / p as f64;
        let pop_kurt = m4 / crate::math::quart(sd);
        let denom = ((p - 2) * (p - 3)) as f64;
        out[i] = if denom != 0.0 {
            ((p + 1) * (p - 1)) as f64 / denom * pop_kurt - 3.0 * ((p - 1) * (p - 1)) as f64 / denom
        } else {
            f64::NAN
        };
    }
    vec![out]
}

fn batch_mad(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    for i in (period - 1)..n {
        let window = &v[i + 1 - period..=i];
        let mean = window.iter().sum::<f64>() / period as f64;
        out[i] = window.iter().map(|x| (x - mean).abs()).sum::<f64>() / period as f64;
    }
    vec![out]
}

fn batch_std_error(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    vec![std_error_series(&v, period)]
}

fn batch_std_error_bands(bars: &[Bar], period: usize, mult: f64) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let lr = linearreg_series(&v, period);
    let se = std_error_series(&v, period);
    let n = v.len();
    let (mut upper, mut lower) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    let mid = lr.clone();
    for i in 0..n {
        if !lr[i].is_nan() && !se[i].is_nan() {
            upper[i] = lr[i] + mult * se[i];
            lower[i] = lr[i] - mult * se[i];
        }
    }
    vec![upper, mid, lower]
}

fn batch_rank_correlation(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    let p = period;
    let p2 = p * p;
    for i in (p - 1)..n {
        let window = &v[i + 1 - p..=i];
        // Rank prices (1-based ascending); stable sort ties by original index,
        // matching Python's stable `sorted(range(p), key=lambda j: window[j])`.
        let mut indexed: Vec<usize> = (0..p).collect();
        indexed.sort_by(|&a, &b| window[a].partial_cmp(&window[b]).unwrap());
        let mut price_rank = vec![0i64; p];
        for (rank, &j) in indexed.iter().enumerate() {
            price_rank[j] = (rank + 1) as i64;
        }
        // Time-index rank is [1, 2, ..., p]. sum_d2 is an exact integer sum.
        let mut sum_d2: i64 = 0;
        for j in 0..p {
            let d = price_rank[j] - (j as i64 + 1);
            sum_d2 += d * d;
        }
        let denom = (p * (p2 - 1)) as i64;
        out[i] = if denom != 0 { 100.0 * (1.0 - 6.0 * sum_d2 as f64 / denom as f64) } else { 0.0 };
    }
    vec![out]
}

// ======================= streaming structs (macro) =========================

hist_indicator! {
    /// `linearreg` — linear-regression value at the last point of each window.
    Linearreg, "linearreg", 1, [period = 14.0],
    |bars| batch_linearreg(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `linearreg_slope` — rolling OLS slope.
    LinearregSlope, "linearreg_slope", 1, [period = 14.0],
    |bars| batch_linearreg_slope(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `linearreg_angle` — rolling OLS slope in degrees.
    LinearregAngle, "linearreg_angle", 1, [period = 14.0],
    |bars| batch_linearreg_angle(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `linearreg_intercept` — rolling OLS intercept.
    LinearregIntercept, "linearreg_intercept", 1, [period = 14.0],
    |bars| batch_linearreg_intercept(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `tsf` — Time Series Forecast (regression projected one step ahead).
    Tsf, "tsf", 1, [period = 14.0],
    |bars| batch_tsf(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `var` — rolling population variance.
    Var, "var", 1, [period = 20.0],
    |bars| batch_var(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `zscore` — rolling z-score.
    Zscore, "zscore", 1, [period = 20.0],
    |bars| batch_zscore(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `skew` — rolling sample skewness (Fisher–Pearson, bias-corrected).
    Skew, "skew", 1, [period = 20.0],
    |bars| batch_skew(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `kurtosis` — rolling excess kurtosis (Fisher, sample-corrected).
    Kurtosis, "kurtosis", 1, [period = 20.0],
    |bars| batch_kurtosis(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `mad` — rolling mean absolute deviation.
    Mad, "mad", 1, [period = 20.0],
    |bars| batch_mad(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `std_error` — standard error of the OLS estimate over each window.
    StdError, "std_error", 1, [period = 20.0],
    |bars| batch_std_error(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `std_error_bands` — standard-error bands around the regression line.
    StdErrorBands, "std_error_bands", 3, [period = 20.0, mult = 2.0],
    |bars| batch_std_error_bands(bars, period.round() as usize, mult),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `rank_correlation` — Spearman rank-correlation index (RCI), scaled to [-100, 100].
    RankCorrelation, "rank_correlation", 1, [period = 14.0],
    |bars| batch_rank_correlation(bars, period.round() as usize),
    lookback = pback(period, 1)
}

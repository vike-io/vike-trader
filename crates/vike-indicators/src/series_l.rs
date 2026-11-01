//! Whole-series calls on plain `f64` slices for group `l` of the fourth wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`).
//!
//! `series::linearreg`, `series::linearreg_slope`, `series::linearreg_angle`,
//! `series::linearreg_intercept`, `series::tsf`, `series::var`, `series::zscore`, `series::skew`,
//! `series::kurtosis`, `series::mad`, `series::std_error`, `series::std_error_bands`,
//! `series::rank_correlation`, `series::r_squared`, `series::hurst` and `series::autocorr`: the same
//! arithmetic as the indicator of that name in [`crate::indicators`], over plain prices with no `Bar`
//! in between. Each returns one value per input, `NaN` while the indicator is warming up, and refuses
//! a parameter the indicator's registry row would not accept with a [`crate::ParamError`].

use crate::ParamError;
use crate::fast_g::period as cap;
use crate::fast_l::{
    ANGLE, AutoCore, BANDS, HurstCore, INTERCEPT, KURT, LINREG, MadCore, MomentCore, RSQ, RankCore,
    RegCore, SKEW, SLOPE, STDERR, TSF, VAR, ZSCORE,
};

/// One output per input of a one-line statistic of the close: the core is built from the period read
/// as a window length and fed every close.
macro_rules! series1 {
    (
        $(#[$m:meta])*
        $name:ident, $key:literal, |$n:ident| $make:expr
    ) => {
        $(#[$m])*
        #[inline]
        pub fn $name(close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
            crate::typed::checked($key, &[period])?;
            let $n = cap(period);
            let mut core = $make;
            let mut out = vec![f64::NAN; close.len()];
            for (o, &c) in out.iter_mut().zip(close) {
                *o = core.push(c);
            }
            Ok(out)
        }
    };
}

series1! {
    /// Linear regression of `close` over `period` values (registry range `2..=200`): the
    /// least-squares line through the last `period` closes, read at the newest bar. `NaN` for the first
    /// `period - 1` values and while a `NaN` is inside the window.
    linearreg, "linearreg", |n| RegCore::<LINREG>::new(n, 0.0)
}
series1! {
    /// Linear regression slope of `close` over `period` values (registry range `2..=200`): the slope of
    /// the least-squares line through the last `period` closes, in price per bar. `NaN` for the first
    /// `period - 1` values and while a `NaN` is inside the window.
    linearreg_slope, "linearreg_slope", |n| RegCore::<SLOPE>::new(n, 0.0)
}
series1! {
    /// Linear regression angle of `close` over `period` values (registry range `2..=200`): the slope of
    /// the least-squares line through the last `period` closes as `atan(slope)` in degrees. `NaN` for
    /// the first `period - 1` values and while a `NaN` is inside the window.
    linearreg_angle, "linearreg_angle", |n| RegCore::<ANGLE>::new(n, 0.0)
}
series1! {
    /// Linear regression intercept of `close` over `period` values (registry range `2..=200`): the
    /// least-squares line through the last `period` closes, read at the OLDEST bar of the window. `NaN`
    /// for the first `period - 1` values and while a `NaN` is inside the window.
    linearreg_intercept, "linearreg_intercept", |n| RegCore::<INTERCEPT>::new(n, 0.0)
}
series1! {
    /// Time Series Forecast of `close` over `period` values (registry range `2..=200`): the
    /// least-squares line through the last `period` closes projected one bar past the newest. `NaN` for
    /// the first `period - 1` values and while a `NaN` is inside the window.
    tsf, "tsf", |n| RegCore::<TSF>::new(n, 0.0)
}
series1! {
    /// Rolling population variance of `close` over `period` values (registry range `2..=200`). `NaN`
    /// for the first `period - 1` values and while a `NaN` is inside the window; exactly `0.0` for a
    /// window of identical values.
    var, "var", |n| MomentCore::<VAR>::new(n)
}
series1! {
    /// Rolling z-score of `close` over `period` values (registry range `2..=200`): the newest value
    /// minus the window mean, over the window's population standard deviation. `NaN` for the first
    /// `period - 1` values, while a `NaN` is inside the window, and for a window of identical values.
    zscore, "zscore", |n| MomentCore::<ZSCORE>::new(n)
}
series1! {
    /// Rolling sample skewness of `close` over `period` values (registry range `2..=200`;
    /// Fisher-Pearson, bias-corrected). `NaN` for the first `period - 1` values, while a `NaN` is inside
    /// the window, and always for a `period` under 3; exactly `0.0` for a window of identical values.
    skew, "skew", |n| MomentCore::<SKEW>::new(n)
}
series1! {
    /// Rolling excess kurtosis of `close` over `period` values (registry range `2..=200`; Fisher,
    /// sample-corrected). `NaN` for the first `period - 1` values, while a `NaN` is inside the window,
    /// and always for a `period` under 4; exactly `0.0` for a window of identical values.
    kurtosis, "kurtosis", |n| MomentCore::<KURT>::new(n)
}
series1! {
    /// Rolling mean absolute deviation of `close` over `period` values (registry range `2..=200`): the
    /// mean of `|close - mean|` over the window. `NaN` for the first `period - 1` values and while a
    /// `NaN` is inside the window.
    mad, "mad", |n| MadCore::new(n)
}
series1! {
    /// Standard error of the estimate of the least-squares line through the last `period` values of
    /// `close` (registry range `2..=200`): `sqrt(sum(residual^2) / (period - 2)) / sqrt(period)`. `NaN`
    /// for the first `period - 1` values and while a `NaN` is inside the window; `0.0` for a `period`
    /// of 2.
    std_error, "std_error", |n| RegCore::<STDERR>::new(n, 0.0)
}
series1! {
    /// Spearman rank-correlation index of `close` over `period` values (registry range `2..=200`),
    /// scaled to `[-100, 100]`: the correlation of the window's ranks with their place in it, equal
    /// values ranked oldest first. `NaN` for the first `period - 1` values and while a `NaN` is inside
    /// the window.
    rank_correlation, "rank_correlation", |n| RankCore::new(n)
}
series1! {
    /// R-squared of the rolling linear regression of `close` over `period` values (registry range
    /// `2..=200`): the squared correlation of the window with its index, from 0 to 1. `NaN` for the
    /// first `period - 1` values, while a `NaN` is inside the window, and for a window of identical
    /// values.
    r_squared, "r_squared", |n| RegCore::<RSQ>::new(n, 0.0)
}

/// Hurst exponent of `close` by rescaled-range analysis over `period` values (registry range
/// `17..=400`): near 0.5 for a random walk, above it for a trending series, below for a mean-reverting
/// one. `NaN` for the first `period - 1` values, while a non-positive close is inside the window, and
/// when fewer than two block sizes produced a value.
#[inline]
pub fn hurst(close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("hurst", &[period])?;
    let mut core = HurstCore::new(cap(period));
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Autocorrelation of `close` over `period` values (registry range `5..=200`) at `lag` bars (registry
/// range `1..=50`): the Pearson correlation between each close of the window and the close `lag` bars
/// later. Between -1 and 1; `NaN` for the first `period - 1` values, for a window of identical values,
/// while a `NaN` is inside the window, and always when `period` is under `lag + 2`.
#[inline]
pub fn autocorr(close: &[f64], period: f64, lag: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("autocorr", &[period, lag])?;
    let mut core = AutoCore::new(cap(period), cap(lag));
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Standard-error bands of `close`: `[upper, mid, lower]`, each as long as `close`. `mid` is the
/// least-squares line through the last `period` closes (registry range `2..=200`) read at the newest
/// bar, `upper` and `lower` `mult` (`0.1..=10`) standard errors of the estimate either side. `NaN` for
/// the first `period - 1` values and while a `NaN` is inside the window.
#[inline]
pub fn std_error_bands(close: &[f64], period: f64, mult: f64) -> Result<[Vec<f64>; 3], ParamError> {
    crate::typed::checked("std_error_bands", &[period, mult])?;
    let mut core = RegCore::<BANDS>::new(cap(period), mult);
    let len = close.len();
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, b, d] = &mut out;
    for (((x, y), z), &c) in a.iter_mut().zip(b.iter_mut()).zip(d.iter_mut()).zip(close) {
        let v = core.push3(c);
        *x = v[0];
        *y = v[1];
        *z = v[2];
    }
    Ok(out)
}

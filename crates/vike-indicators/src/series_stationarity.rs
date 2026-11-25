//! Whole-series calls on plain `f64` slices for the stationarity and memory measures
//! (`crate::indicators::Kpss`, `FractionalDifference`, `ShannonEntropy`, `LempelZiv`), re-exported
//! from [`crate::series`] (`series::<name>(...)`): `kpss`, `fractional_difference`,
//! `shannon_entropy` and `lempel_ziv`.
//!
//! Each feeds the typed stepper value by value, so the result equals the stream and `vectorize`
//! bit for bit.

use crate::IndicatorParamError;
use crate::indicators::{FractionalDifference, Kpss, LempelZiv, ShannonEntropy};

/// The KPSS level-stationarity statistic of the log of `close` over `period` closes
/// (`20..=400`) with `lags` (`0..=32`) Bartlett-weighted autocovariances; `NaN` for the first
/// `period - 1` values.
#[inline]
pub fn kpss(close: &[f64], period: f64, lags: f64) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = Kpss::new(period, lags)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

/// The fixed-width-window fractional difference of order `d` (`0..=2`) of the log of `close`, over
/// `window` weights (`2..=1000`); `NaN` for the first `window - 1` values.
#[inline]
pub fn fractional_difference(
    close: &[f64],
    d: f64,
    window: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = FractionalDifference::new(d, window)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

/// The Shannon entropy, from 0 to 1, of the last `period` (`10..=1000`) log returns of `close` in
/// `bins` (`2..=100`) equal-width bins; `NaN` for the first `period` values.
#[inline]
pub fn shannon_entropy(
    close: &[f64],
    period: f64,
    bins: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = ShannonEntropy::new(period, bins)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

/// The Lempel–Ziv complexity, `c(n) · log2(n) / n`, of the last `period` (`10..=1000`) up/down moves
/// of `close`; `NaN` for the first `period` values.
#[inline]
pub fn lempel_ziv(close: &[f64], period: f64) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = LempelZiv::new(period)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

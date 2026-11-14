//! Whole-series calls on plain `f64` slices for the filters (`crate::indicators::GaussianFilter`,
//! `EarlyOnsetTrend`, `GhFilter`, `Kalman`), re-exported from [`crate::series`]
//! (`series::<name>(...)`): `gaussian_filter`, `early_onset_trend`, `gh_filter` and `kalman`.
//!
//! Each feeds the typed stepper value by value, so the result equals the stream and `vectorize`
//! bit for bit.

use crate::IndicatorParamError;
use crate::indicators::{EarlyOnsetTrend, GaussianFilter, GhFilter, Kalman};

/// Ehlers' Gaussian filter of `close`: `poles` (1 to 4) exponential averages in cascade, 3 dB down
/// at `period` bars (registry range `2..=1000`); a value from the first, climbing from 0.
#[inline]
pub fn gaussian_filter(
    close: &[f64],
    period: f64,
    poles: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = GaussianFilter::new(period, poles)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

/// Ehlers' early-onset trend of `close`: the quotient transform, with constant `k`
/// (`-0.99..=0.99`), of a 100-bar high pass smoothed over `lp_period` bars (`2..=400`) and
/// normalised by its decaying peak; `NaN` for the first two values.
#[inline]
pub fn early_onset_trend(
    close: &[f64],
    lp_period: f64,
    k: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = EarlyOnsetTrend::new(lp_period, k)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

/// The g-h tracking filter of `close` with position gain `g` (`0.001..=1.49`) and velocity gain `h`
/// (`0.001..=1.0`): each value the prediction for the next; the first value is the first close.
#[inline]
pub fn gh_filter(close: &[f64], g: f64, h: f64) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = GhFilter::new(g, h)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

/// The scalar Kalman filter of `close` as a random walk with step variance `process_noise`
/// (`0..=1000`) seen through noise of variance `measurement_noise` (`0.0001..=1000`): the first
/// value is the first close.
#[inline]
pub fn kalman(
    close: &[f64],
    process_noise: f64,
    measurement_noise: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = Kalman::new(process_noise, measurement_noise)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

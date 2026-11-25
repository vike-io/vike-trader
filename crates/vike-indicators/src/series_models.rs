//! Whole-series calls on plain `f64` slices for the indicators read off a model
//! (`crate::indicators::GarchVolatility`, `HmmRegime`, `DominantCycle`, `HaarDetailEnergy`, `Adf`),
//! re-exported from [`crate::series`] (`series::<name>(...)`): `garch_volatility`, `hmm_regime`,
//! `dominant_cycle`, `haar_detail_energy` and `adf`.
//!
//! Each feeds the typed stepper value by value, so the result equals the stream and `vectorize`
//! bit for bit.

use crate::IndicatorParamError;
use crate::indicators::{Adf, DominantCycle, GarchVolatility, HaarDetailEnergy, HmmRegime};

/// GARCH(1,1) conditional volatility of `close`'s log returns: on each bar the forecast standard
/// deviation of the next return, `omega` (`0..=1`), `alpha` and `beta` (`0..=1` each) the model's
/// parameters; `NaN` for the first value.
#[inline]
pub fn garch_volatility(
    close: &[f64],
    omega: f64,
    alpha: f64,
    beta: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = GarchVolatility::new(omega, alpha, beta)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

/// The filtered probability of state 1 of a two-state Gaussian hidden Markov model of `close`'s log
/// returns: each state's mean (`-1..=1`), standard deviation (`0.000001..=1`) and probability of
/// staying (`0..=1`), state 0's then state 1's; `NaN` for the first value.
#[inline]
pub fn hmm_regime(
    close: &[f64],
    mean_0: f64,
    sigma_0: f64,
    stay_0: f64,
    mean_1: f64,
    sigma_1: f64,
    stay_1: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = HmmRegime::new(mean_0, sigma_0, stay_0, mean_1, sigma_1, stay_1)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

/// The dominant cycle of `close`: the period, in bars, of the largest Fourier power of the last
/// `length` closes (`4..=256`), linearly detrended, searched from `min_period` to `max_period`
/// (`2..=256` each); `NaN` for the first `length - 1` values.
#[inline]
pub fn dominant_cycle(
    close: &[f64],
    length: f64,
    min_period: f64,
    max_period: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = DominantCycle::new(length, min_period, max_period)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

/// The Haar wavelet detail energy of `close`'s log returns at `level` (`1..=8`) over the most whole
/// blocks of `2^level` returns that fit in `length` (`2..=1000`), at least one; `NaN` until that
/// window of returns is full.
#[inline]
pub fn haar_detail_energy(
    close: &[f64],
    length: f64,
    level: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = HaarDetailEnergy::new(length, level)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

/// The augmented Dickey-Fuller t-statistic of the last `period` log closes (`20..=400`) with `lags`
/// lagged differences (`0..=8`) and a constant; `NaN` for the first `period - 1` values.
#[inline]
pub fn adf(close: &[f64], period: f64, lags: f64) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = Adf::new(period, lags)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

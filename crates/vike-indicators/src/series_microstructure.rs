//! Whole-series calls on plain `f64` slices for the market-microstructure measures
//! (`crate::indicators::KyleLambda`, `AmihudIlliquidity`, `RollSpread`), re-exported from
//! [`crate::series`] (`series::<name>(...)`): `kyle_lambda`, `amihud_illiquidity` and
//! `roll_spread`.
//!
//! Each feeds the typed stepper value by value, so the result equals the stream and `vectorize`
//! bit for bit. A call with two slices answers as many values as the shorter has.

use crate::IndicatorParamError;
use crate::indicators::{AmihudIlliquidity, KyleLambda, RollSpread};

/// Kyle's lambda from parallel `close` and `volume` slices: over the last `period` bar-to-bar
/// changes (registry range `2..=1000`), the least-squares slope of the change of the close on the
/// volume signed by the close's direction; `NaN` for the first `period` values.
#[inline]
pub fn kyle_lambda(
    close: &[f64],
    volume: &[f64],
    period: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = KyleLambda::new(period)?;
    Ok(close.iter().zip(volume).map(|(&c, &v)| s.next_cv(c, v)).collect())
}

/// Amihud's illiquidity from parallel `close` and `volume` slices: the mean of
/// `|return| / (close * volume)` over the bars of the last `period` (registry range `1..=1000`)
/// that traded; `NaN` for the first `period` values.
#[inline]
pub fn amihud_illiquidity(
    close: &[f64],
    volume: &[f64],
    period: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = AmihudIlliquidity::new(period)?;
    Ok(close.iter().zip(volume).map(|(&c, &v)| s.next_cv(c, v)).collect())
}

/// Roll's spread of `close`: `2 sqrt(-cov)` of the sample serial covariance of the close-to-close
/// changes over the last `period` pairs (registry range `2..=1000`), `0` where it is not negative;
/// `NaN` for the first `period + 1` values.
#[inline]
pub fn roll_spread(close: &[f64], period: f64) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = RollSpread::new(period)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

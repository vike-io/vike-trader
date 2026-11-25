//! Whole-series calls on plain `f64` slices for the four oscillators of
//! `crate::indicators::streams_osc`, re-exported from [`crate::series`] (`series::<name>(...)`):
//! `wavetrend`, `qqe`, `stc` and `percent_rank`.
//!
//! Each folds the typed stepper over the slice, so the result equals the stream and `vectorize` bit
//! for bit.

use crate::IndicatorParamError;
use crate::indicators::{PercentRank, Qqe, Stc, Wavetrend};

/// WaveTrend of the typical price of `high`, `low` and `close` (registry ranges `channel` and
/// `average` `2..=500`): `[wavetrend, signal]`, see [`Wavetrend`]. The slices are read up to the
/// shortest.
#[inline]
pub fn wavetrend(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    channel: f64,
    average: f64,
) -> Result<[Vec<f64>; 2], IndicatorParamError> {
    let mut w = Wavetrend::new(channel, average)?;
    let (mut a, mut b) = (Vec::with_capacity(close.len()), Vec::with_capacity(close.len()));
    for ((&h, &l), &c) in high.iter().zip(low).zip(close) {
        let [x, y] = w.next_hlc(h, l, c);
        a.push(x);
        b.push(y);
    }
    Ok([a, b])
}

/// QQE of `close` (registry ranges `rsi_period` `2..=100`, `smoothing` `1..=100`, `factor`
/// `0.1..=20`): `[qqe, trail]`, see [`Qqe`].
#[inline]
pub fn qqe(
    close: &[f64],
    rsi_period: f64,
    smoothing: f64,
    factor: f64,
) -> Result<[Vec<f64>; 2], IndicatorParamError> {
    let mut q = Qqe::new(rsi_period, smoothing, factor)?;
    let (mut a, mut b) = (Vec::with_capacity(close.len()), Vec::with_capacity(close.len()));
    for &c in close {
        let [x, y] = q.next_value(c);
        a.push(x);
        b.push(y);
    }
    Ok([a, b])
}

/// Schaff trend cycle of `close` (registry ranges `fast` `2..=500`, `slow` `2..=1000`, `cycle`
/// `2..=500`), see [`Stc`].
#[inline]
pub fn stc(
    close: &[f64],
    fast: f64,
    slow: f64,
    cycle: f64,
) -> Result<Vec<f64>, IndicatorParamError> {
    let mut s = Stc::new(fast, slow, cycle)?;
    Ok(close.iter().map(|&c| s.next_value(c)).collect())
}

/// Percent rank of each value of `x` among the `length` values before it (registry range
/// `1..=5000`), see [`PercentRank`].
#[inline]
pub fn percent_rank(x: &[f64], length: f64) -> Result<Vec<f64>, IndicatorParamError> {
    let mut r = PercentRank::new(length)?;
    Ok(x.iter().map(|&v| r.next_value(v)).collect())
}

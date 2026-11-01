//! Whole-series calls on plain `f64` slices for group `d2` of the second wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`): `trima`, `hma`, `kama`, `stddev`
//! and `true_range`.
//!
//! Each runs the same core as the typed stepper (`crate::fast_d2`), so the result equals the
//! stream and `vectorize` bit for bit.

use crate::ParamError;
use crate::fast_d2::{HmaCore, KamaCore, StddevCore, TrimaCore};

/// Triangular moving average of `close` over `length` values (registry range `2..=400`): the mean
/// over `ceil(length / 2)` values of the mean over `length / 2 + 1` of them; `NaN` for the first
/// `length - 1` values.
#[inline]
pub fn trima(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("trima", &[length])?;
    let mut core = TrimaCore::new(length.round() as usize);
    // Filled first, then written by index: the page faults of a fresh buffer land in the fill, not
    // inside the recurrence.
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Hull moving average of `close` over `length` values (registry range `2..=400`):
/// `WMA(sqrt(length))` of `2 * WMA(length / 2) - WMA(length)`, the two roots floored and at least 2;
/// `NaN` for the first `length + floor(sqrt(length)) - 2` values.
#[inline]
pub fn hma(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("hma", &[length])?;
    let mut core = HmaCore::new(length.round() as usize);
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Kaufman's adaptive moving average of `close`: `period` is the efficiency-ratio window (registry
/// range `2..=200`), `fast` and `slow` the spans of the limiting averages (`1..=100` and `2..=400`);
/// `NaN` for the first `period` values. A `NaN` close poisons the rest of the series.
#[inline]
pub fn kama(close: &[f64], period: f64, fast: f64, slow: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("kama", &[period, fast, slow])?;
    let mut core = KamaCore::new(
        (period.round() as usize).max(1),
        fast.round() as usize,
        slow.round() as usize,
    );
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Rolling population standard deviation of `close` over `length` values (registry range
/// `2..=200`); `NaN` for the first `length - 1` values, and exactly `0.0` for a window of
/// bit-equal values.
#[inline]
pub fn stddev(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("stddev", &[length])?;
    let mut core = StddevCore::new(length.round() as usize);
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// True range of each bar, from parallel `high`, `low` and `close` slices: the largest of
/// `high - low`, `|high - previous close|` and `|low - previous close|` (computed as
/// `max(high, previous close) - min(low, previous close)`), and `high - low` for the first bar.
/// The result is as long as the SHORTEST of the three. It has no parameter, so it cannot fail.
#[inline]
pub fn true_range(high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    let len = high.len().min(low.len()).min(close.len());
    let mut out = vec![f64::NAN; len];
    if len == 0 {
        return out;
    }
    out[0] = crate::fast_d2::true_range(high[0], low[0], f64::NAN);
    // No state to carry: the previous close is the previous element of the input.
    for (((o, &h), &l), &pc) in
        out[1..].iter_mut().zip(&high[1..len]).zip(&low[1..len]).zip(&close[..len - 1])
    {
        *o = crate::fast_d2::true_range(h, l, pc);
    }
    out
}

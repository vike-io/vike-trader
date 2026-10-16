//! Whole-series calls on plain `f64` slices for group `j` of the third wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`): `bbands_pctb`, `bbands_width`,
//! `donchian_width`, `ulcer`, `chop` and the six realized-volatility rows (`rvol_ewma`,
//! `rvol_close_to_close`, `rvol_parkinson`, `rvol_garman_klass`, `rvol_rogers_satchell`,
//! `rvol_yang_zhang`).
//!
//! Each runs the same core as the typed stepper (`crate::fast_j`), so the result equals the stream
//! and `vectorize` bit for bit. A call over several parallel slices returns as many values as the
//! SHORTEST of them has.

use crate::ParamError;
use crate::fast_j::{
    ChopCore, CloseToCloseCore, DonchianWidthCore, EwmaVolCore, GarmanKlassCore, ParkinsonCore,
    PctbCore, RogersSatchellCore, UlcerCore, WidthCore, YangZhangCore,
};

/// Bollinger %B of `close`: `(close - lower band) / (upper band - lower band)`, the bands being the
/// mean of the last `period` closes (registry range `2..=200`) plus and minus `k` population
/// standard deviations (`0.5..=5.0`). `NaN` for the first `period - 1` values, and `NaN` where the
/// band has no width (a window of bit-equal closes).
#[inline]
pub fn bbands_pctb(close: &[f64], period: f64, k: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("bbands_pctb", &[period, k])?;
    let mut core = PctbCore::new(period.round() as usize, k);
    // Filled first, then written by index: the page faults of a fresh buffer land in the fill, not
    // inside the recurrence.
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Bollinger band width of `close`: `(upper band - lower band) / mid line`, the bands being the mean
/// of the last `period` closes (registry range `2..=200`) plus and minus `k` population standard
/// deviations (`0.5..=5.0`). `NaN` for the first `period - 1` values and where the mid line is zero;
/// exactly `0.0` for a window of bit-equal closes.
#[inline]
pub fn bbands_width(close: &[f64], period: f64, k: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("bbands_width", &[period, k])?;
    let mut core = WidthCore::new(period.round() as usize, k);
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Donchian channel width: the highest `high` minus the lowest `low` of the last `period` values
/// (registry range `2..=200`), from parallel slices. `NaN` for the first `period - 1` values; a
/// `NaN` high or low is skipped as if absent.
#[inline]
pub fn donchian_width(high: &[f64], low: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("donchian_width", &[period])?;
    let mut core = DonchianWidthCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len())];
    for ((o, &h), &l) in out.iter_mut().zip(high).zip(low) {
        *o = core.push(h, l);
    }
    Ok(out)
}

/// Ulcer Index of `close`: the root of the mean squared percentage drawdown of the last `period`
/// closes (registry range `2..=200`) from the highest of them. `NaN` for the first `period - 1`
/// values; exactly `0.0` for a window of bit-equal closes.
#[inline]
pub fn ulcer(close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("ulcer", &[period])?;
    let mut core = UlcerCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Choppiness Index over `period` bars (registry range `2..=200`), from parallel `high`, `low` and
/// `close` slices: `100 * log10(sum of the true ranges / (highest high - lowest low)) /
/// log10(period)`. `NaN` for the first `period - 1` values and where the range or the sum of true
/// ranges is not positive.
#[inline]
pub fn chop(high: &[f64], low: &[f64], close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("chop", &[period])?;
    let mut core = ChopCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len()).min(close.len())];
    for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
        *o = core.push(h, l, c);
    }
    Ok(out)
}

/// RiskMetrics EWMA volatility of `close`, per period (registry range of `lambda` `0.5..=0.999`, the
/// daily-data default is 0.94): the first log return seeds the variance at `r^2`, every later one
/// folds `lambda * v + (1 - lambda) * r^2`. `NaN` for the first value, and for a value whose close or
/// previous close is not a finite positive number (the variance is left as it was).
#[inline]
pub fn rvol_ewma(close: &[f64], lambda: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("rvol_ewma", &[lambda])?;
    let mut core = EwmaVolCore::new(lambda);
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Close-to-close realized volatility of `close`, per period: the sample standard deviation of the
/// last `window` log returns (registry range `2..=400`). `NaN` until `window` returns exist; exactly
/// `0.0` for a window of bit-equal returns.
#[inline]
pub fn rvol_close_to_close(close: &[f64], window: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("rvol_close_to_close", &[window])?;
    let mut core = CloseToCloseCore::new(window.round() as usize);
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Parkinson realized volatility per period, from parallel `high` and `low` slices:
/// `sqrt(mean(ln(H/L)^2) / (4 ln 2))` over the last `window` bars (registry range `2..=400`). `NaN`
/// until `window` bars with a finite positive high and low exist.
#[inline]
pub fn rvol_parkinson(high: &[f64], low: &[f64], window: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("rvol_parkinson", &[window])?;
    let mut core = ParkinsonCore::new(window.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len())];
    for ((o, &h), &l) in out.iter_mut().zip(high).zip(low) {
        *o = core.push(h, l);
    }
    Ok(out)
}

/// Garman-Klass realized volatility per period, from parallel `open`, `high`, `low` and `close`
/// slices: `sqrt(mean(0.5 ln(H/L)^2 - (2 ln 2 - 1) ln(C/O)^2))` over the last `window` bars (registry
/// range `2..=400`). `NaN` until `window` bars with four finite positive prices exist.
#[inline]
pub fn rvol_garman_klass(
    open: &[f64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    window: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("rvol_garman_klass", &[window])?;
    let mut core = GarmanKlassCore::new(window.round() as usize);
    let mut out = vec![f64::NAN; open.len().min(high.len()).min(low.len()).min(close.len())];
    for ((((o, &x), &h), &l), &c) in out.iter_mut().zip(open).zip(high).zip(low).zip(close) {
        *o = core.push(x, h, l, c);
    }
    Ok(out)
}

/// Rogers-Satchell realized volatility per period, from parallel `open`, `high`, `low` and `close`
/// slices: `sqrt(mean(ln(H/C) ln(H/O) + ln(L/C) ln(L/O)))` over the last `window` bars (registry range
/// `2..=400`). `NaN` until `window` bars with four finite positive prices exist.
#[inline]
pub fn rvol_rogers_satchell(
    open: &[f64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    window: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("rvol_rogers_satchell", &[window])?;
    let mut core = RogersSatchellCore::new(window.round() as usize);
    let mut out = vec![f64::NAN; open.len().min(high.len()).min(low.len()).min(close.len())];
    for ((((o, &x), &h), &l), &c) in out.iter_mut().zip(open).zip(high).zip(low).zip(close) {
        *o = core.push(x, h, l, c);
    }
    Ok(out)
}

/// Yang-Zhang realized volatility per period, from parallel `open`, `high`, `low` and `close`
/// slices: the sample variance of the overnight returns `ln(O / previous C)` plus `k` times that of
/// the open-to-close returns plus `1 - k` times the Rogers-Satchell mean, rooted, over the last
/// `window` bars (registry range `2..=400`). `NaN` until `window` bars with four finite positive
/// prices and a finite positive previous close exist.
#[inline]
pub fn rvol_yang_zhang(
    open: &[f64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    window: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("rvol_yang_zhang", &[window])?;
    let mut core = YangZhangCore::new(window.round() as usize);
    let mut out = vec![f64::NAN; open.len().min(high.len()).min(low.len()).min(close.len())];
    for ((((o, &x), &h), &l), &c) in out.iter_mut().zip(open).zip(high).zip(low).zip(close) {
        *o = core.push(x, h, l, c);
    }
    Ok(out)
}

//! Whole-series calls on plain `f64` slices for group `e` of the third wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`): `alma`, `envelopes`, `midpoint`,
//! `midprice`, `vwma`, `atr_trailing_stop`, `evwma`, `frama`, `hilo_activator` and `vidya`.
//!
//! Each runs the same core as the typed stepper (`crate::fast_e`), so the result equals the stream
//! and `vectorize` bit for bit. A result is as long as the SHORTEST of the slices it reads, `NaN`
//! while the indicator is still warming up, and a parameter the indicator's registry row would not
//! accept is a [`crate::ParamError`].

use crate::ParamError;
use crate::fast_e::{
    AlmaCore, AtrStopCore, EnvelopesCore, EvwmaCore, FramaCore, HiloCore, Mid1Core, MidCore,
    VidyaCore, VwmaCore,
};

/// Arnaud Legoux moving average of `close`: a Gaussian-weighted mean over `period` values (registry
/// range `2..=400`). `offset` (`0..=1`) is where in the window the weights peak, 0 the oldest value
/// and 1 the newest; `sigma` (`1..=20`) is how narrow the Gaussian is. `NaN` for the first
/// `period - 1` values.
#[inline]
pub fn alma(close: &[f64], period: f64, offset: f64, sigma: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("alma", &[period, offset, sigma])?;
    let mut core = AlmaCore::new(period.round() as usize, offset, sigma);
    // Filled first, then written by index: the page faults of a fresh buffer land in the fill, not
    // inside the recurrence.
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Envelopes of `close`: `[upper, mid, lower]`, each as long as `close`. `mid` is the simple
/// moving average over `period` values (registry range `2..=200`), `upper` and `lower` are `pct`
/// percent (`0.1..=20`) above and below it. `NaN` for the first `period - 1` values.
#[inline]
pub fn envelopes(close: &[f64], period: f64, pct: f64) -> Result<[Vec<f64>; 3], ParamError> {
    crate::typed::checked("envelopes", &[period, pct])?;
    let mut core = EnvelopesCore::new(period.round() as usize, pct);
    let len = close.len();
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, b, d] = &mut out;
    for (((x, y), z), &c) in a.iter_mut().zip(b.iter_mut()).zip(d.iter_mut()).zip(close) {
        let v = core.push(c);
        *x = v[0];
        *y = v[1];
        *z = v[2];
    }
    Ok(out)
}

/// Midpoint of `close`: `(highest close + lowest close) / 2` over `period` values (registry range
/// `2..=400`); `NaN` for the first `period - 1` values. A `NaN` close is skipped as if absent.
#[inline]
pub fn midpoint(close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("midpoint", &[period])?;
    let mut core = Mid1Core::new(period.round() as usize);
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Midprice: `(highest high + lowest low) / 2` over `period` bars (registry range `2..=400`), from
/// parallel `high` and `low` slices; the result is as long as the shorter. `NaN` for the first
/// `period - 1` values. A `NaN` high or low is skipped as if absent.
#[inline]
pub fn midprice(high: &[f64], low: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("midprice", &[period])?;
    let mut core = MidCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len())];
    for ((o, &h), &l) in out.iter_mut().zip(high).zip(low) {
        *o = core.push(h, l);
    }
    Ok(out)
}

/// Volume-weighted moving average of `close` over `period` bars (registry range `2..=400`), from
/// parallel `close` and `volume` slices; the result is as long as the shorter. `NaN` for the first
/// `period - 1` values and for a window that traded no volume.
#[inline]
pub fn vwma(close: &[f64], volume: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("vwma", &[period])?;
    let mut core = VwmaCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; close.len().min(volume.len())];
    for ((o, &c), &v) in out.iter_mut().zip(close).zip(volume) {
        *o = core.push(c, v);
    }
    Ok(out)
}

/// ATR trailing stop, from parallel `high`, `low` and `close` slices (the result is as long as the
/// shortest): a stop that trails the close at `mult` (`0.5..=10`) times Wilder's average true
/// range over `period` bars (`1..=200`) and only moves in the trend's direction until price
/// crosses it. `NaN` for the first `period - 1` values. Path dependent: the value carries every
/// bar since the start of the slices.
#[inline]
pub fn atr_trailing_stop(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    period: f64,
    mult: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("atr_trailing_stop", &[period, mult])?;
    let mut core = AtrStopCore::new(period.round() as usize, mult);
    let mut out = vec![f64::NAN; high.len().min(low.len()).min(close.len())];
    for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
        *o = core.push(h, l, c);
    }
    Ok(out)
}

/// Elastic volume-weighted moving average of `close` over `period` bars (registry range
/// `2..=400`), from parallel `close` and `volume` slices; the result is as long as the shorter.
/// `NaN` for the first `period - 1` values. Path dependent: a `NaN` close poisons the rest.
#[inline]
pub fn evwma(close: &[f64], volume: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("evwma", &[period])?;
    let mut core = EvwmaCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; close.len().min(volume.len())];
    for ((o, &c), &v) in out.iter_mut().zip(close).zip(volume) {
        *o = core.push(c, v);
    }
    Ok(out)
}

/// Ehlers' fractal adaptive moving average of `close`, from parallel `high`, `low` and `close`
/// slices (the result is as long as the shortest). `period` (registry range `2..=400`, step 2) is
/// rounded down to an even window; `NaN` for the first `period - 1` values. Path dependent: a `NaN`
/// close poisons the rest.
#[inline]
pub fn frama(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    period: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("frama", &[period])?;
    let mut core = FramaCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len()).min(close.len())];
    for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
        *o = core.push(h, l, c);
    }
    Ok(out)
}

/// Gann HiLo Activator, from parallel `high`, `low` and `close` slices (the result is as long as
/// the shortest): a stop line under price in an uptrend (the simple average of the lows over
/// `period` bars, registry range `1..=200`) and over it in a downtrend (the average of the highs).
/// `NaN` for the first `period - 1` values. Path dependent: the trend persists until it is broken.
#[inline]
pub fn hilo_activator(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    period: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("hilo_activator", &[period])?;
    let mut core = HiloCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len()).min(close.len())];
    for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
        *o = core.push(h, l, c);
    }
    Ok(out)
}

/// Chande's variable index dynamic average of `close`: an EMA over `period` values (registry range
/// `2..=200`) whose smoothing follows the absolute Chande momentum oscillator of the last
/// `cmo_period` changes (`2..=200`). `NaN` for the first `cmo_period` values. Path dependent: a
/// `NaN` close poisons the rest.
#[inline]
pub fn vidya(close: &[f64], period: f64, cmo_period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("vidya", &[period, cmo_period])?;
    let mut core = VidyaCore::new(period.round() as usize, cmo_period.round() as usize);
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

//! Whole-series calls on a slice of closes: `series::sma(&close, 20.0)` and `series::rsi(&close, 14.0)`.
//!
//! The same arithmetic as [`crate::indicators::Sma`] and [`crate::indicators::Rsi`], over plain `f64`
//! prices, with no `Bar` in between. Reading only the closes is the cheapest way to run a whole
//! history through an indicator: a `Bar` is ~120 bytes, a close is 8. Each function returns one
//! value per input, `NaN` while the indicator is still warming up, and refuses a `length` the
//! indicator's registry row would not accept with a [`crate::ParamError`] (the `_into` form refills
//! a buffer you own instead of allocating one).

use crate::ParamError;

pub use crate::series_a::*;
pub use crate::series_b::*;
pub use crate::series_c::*;
pub use crate::series_d1::*;
pub use crate::series_d2::*;
pub use crate::series_e::*;
pub use crate::series_f::*;
pub use crate::series_g::*;
pub use crate::series_h::*;
pub use crate::series_i::*;
pub use crate::series_j::*;
pub use crate::series_k::*;
pub use crate::series_l::*;
pub use crate::series_m::*;
pub use crate::series_n::*;
pub use crate::series_o::*;
pub use crate::series_q::*;
pub use crate::series_r::*;

/// Simple moving average of `close` over `length` values (registry range `1..=1000`); `NaN` for the
/// first `length - 1` values.
#[inline]
pub fn sma(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    let mut out = Vec::new();
    sma_into(close, length, &mut out)?;
    Ok(out)
}

/// [`sma`] into `out`, which is cleared first and keeps its allocation.
#[inline]
pub fn sma_into(close: &[f64], length: f64, out: &mut Vec<f64>) -> Result<(), ParamError> {
    crate::typed::checked("sma", &[length])?;
    crate::roll::sma_run(close.len(), length.round() as usize, |i| close[i], out);
    Ok(())
}

/// Wilder's relative strength index of `close` over `length` moves (registry range `1..=1000`);
/// `NaN` for the first `length` values.
#[inline]
pub fn rsi(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    let mut out = Vec::new();
    rsi_into(close, length, &mut out)?;
    Ok(out)
}

/// [`rsi`] into `out`, which is cleared first and keeps its allocation.
#[inline]
pub fn rsi_into(close: &[f64], length: f64, out: &mut Vec<f64>) -> Result<(), ParamError> {
    crate::typed::checked("rsi", &[length])?;
    crate::roll::rsi_run(close.len(), length.round() as usize, |i| close[i], out);
    Ok(())
}

/// Exponential moving average of `close` over `length` values (registry range `1..=1000`),
/// seeded with the mean of the first `length` values; `NaN` for the first `length - 1`.
#[inline]
pub fn ema(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("ema", &[length])?;
    let mut core = crate::roll::EmaCore::new(length.round() as usize);
    // Filled first, then written by index: the page faults of a fresh buffer land in the fill, not
    // inside the recurrence (measured: 180 us for 50,000 values by `push`, against 71 us into a warm buffer).
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// MACD of `close`: `[macd, signal, histogram]`, each as long as `close`, `NaN` while a line is
/// not defined yet. Periods are the registry's (`1..=1000` each).
#[inline]
pub fn macd(close: &[f64], fast: f64, slow: f64, signal: f64) -> Result<[Vec<f64>; 3], ParamError> {
    crate::typed::checked("macd", &[fast, slow, signal])?;
    let mut core = crate::roll::MacdCore::new(
        fast.round() as usize,
        slow.round() as usize,
        signal.round() as usize,
    );
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

/// Bollinger Bands of `close`: `[upper, mid, lower]`, each as long as `close`, a `length`-value
/// mean and `mult` population standard deviations either side (`length` `1..=1000`, `mult`
/// `0.1..=10`); `NaN` for the first `length - 1` values.
#[inline]
pub fn bollinger(close: &[f64], length: f64, mult: f64) -> Result<[Vec<f64>; 3], ParamError> {
    crate::typed::checked("bollinger", &[length, mult])?;
    let mut core = crate::roll::BollingerCore::new(length.round() as usize, mult);
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

/// Average true range over `length` bars (registry range `1..=1000`), from parallel `high`,
/// `low` and `close` slices; the result is as long as the SHORTEST of the three. `NaN` for the
/// first `length - 1` values.
#[inline]
pub fn atr(high: &[f64], low: &[f64], close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("atr", &[length])?;
    let mut core = crate::roll::AtrCore::new(length.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len()).min(close.len())];
    for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
        *o = core.push(h, l, c);
    }
    Ok(out)
}

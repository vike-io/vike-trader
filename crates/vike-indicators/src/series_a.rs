//! Whole-series calls on plain `f64` slices for group `a` of the second wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`).
//!
//! The same arithmetic as the typed steppers ([`crate::indicators::Wma`] and the rest of the
//! group): each call runs the stepper's own core, so a series equals a stream of the same values
//! bit for bit. Every function returns one value per input, `NaN` while the indicator is still
//! warming up, and refuses a parameter the indicator's registry row would not accept with a
//! [`crate::ParamError`]. Inputs given as parallel slices (`high`, `low`, `close`) give a result as
//! long as the SHORTEST of them.

use crate::ParamError;
use crate::fast_a::{
    AwesomeCore, CciCore, DonchianCore, KeltnerCore, StochasticCore, WilliamsCore, WmaCore,
};

/// Linearly weighted moving average of `close` over `length` values (registry range `1..=1000`),
/// weights `1..=length` with the newest heaviest; `NaN` for the first `length - 1` values.
#[inline]
pub fn wma(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("wma", &[length])?;
    let mut core = WmaCore::new(length.round() as usize);
    // Filled first, then written by index (see `series::ema`).
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Commodity channel index of the typical price `(high + low + close) / 3` over `length` values
/// (registry range `1..=1000`); `NaN` for the first `length - 1`, and 0 for a window with no spread.
#[inline]
pub fn cci(high: &[f64], low: &[f64], close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("cci", &[length])?;
    let mut core = CciCore::new(length.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len()).min(close.len())];
    for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
        *o = core.push(h, l, c);
    }
    Ok(out)
}

/// Williams %R over `length` bars (registry range `1..=1000`), from `-100` to `0`; `NaN` for the
/// first `length - 1` values, and 0 for a window with no range.
#[inline]
pub fn williams(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    length: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("williams", &[length])?;
    let mut core = WilliamsCore::new(length.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len()).min(close.len())];
    for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
        *o = core.push(h, l, c);
    }
    Ok(out)
}

/// Slow stochastic oscillator: `[%K, %D]`, each as long as the shortest input. The raw %K is
/// taken over `length` bars, `%K` is its `smooth_k`-value mean and `%D` the `smooth_d`-value mean of
/// `%K` (each parameter `1..=1000`); `NaN` while a line is not defined yet.
#[inline]
pub fn stochastic(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    length: f64,
    smooth_k: f64,
    smooth_d: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("stochastic", &[length, smooth_k, smooth_d])?;
    let mut core = StochasticCore::new(
        length.round() as usize,
        smooth_k.round() as usize,
        smooth_d.round() as usize,
    );
    let len = high.len().min(low.len()).min(close.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [k, d] = &mut out;
    for ((((ok, od), &h), &l), &c) in k.iter_mut().zip(d.iter_mut()).zip(high).zip(low).zip(close) {
        let v = core.push(h, l, c);
        *ok = v[0];
        *od = v[1];
    }
    Ok(out)
}

/// Awesome oscillator: the `fast_length`-value mean minus the `slow_length`-value mean of the
/// median price `(high + low) / 2` (each registry range `1..=1000`); `NaN` until the longer of the
/// two windows is full.
#[inline]
pub fn awesome(
    high: &[f64],
    low: &[f64],
    fast_length: f64,
    slow_length: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("awesome", &[fast_length, slow_length])?;
    let mut core = AwesomeCore::new(fast_length.round() as usize, slow_length.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len())];
    for ((o, &h), &l) in out.iter_mut().zip(high).zip(low) {
        *o = core.push(h, l);
    }
    Ok(out)
}

/// Donchian channel: `[upper, mid, lower]`, each as long as the shorter input: the highest high
/// and lowest low of the last `length` bars (registry range `1..=1000`) and their midpoint; `NaN`
/// for the first `length - 1` values. A `NaN` high or low is skipped as if absent.
#[inline]
pub fn donchian(high: &[f64], low: &[f64], length: f64) -> Result<[Vec<f64>; 3], ParamError> {
    crate::typed::checked("donchian", &[length])?;
    let mut core = DonchianCore::new(length.round() as usize);
    let len = high.len().min(low.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, b, d] = &mut out;
    for (((x, y), z), (&h, &l)) in
        a.iter_mut().zip(b.iter_mut()).zip(d.iter_mut()).zip(high.iter().zip(low))
    {
        let v = core.push(h, l);
        *x = v[0];
        *y = v[1];
        *z = v[2];
    }
    Ok(out)
}

/// Keltner channel: `[upper, mid, lower]`, each as long as the shortest input. The mid line is an
/// exponential average of `close` over `ema_length` values (seeded with their mean), the bands are
/// `mult` average true ranges over `atr_length` bars either side (`ema_length` and `atr_length`
/// `1..=1000`, `mult` `0.1..=10`). `NaN` for the first `ema_length - 1` values, and the bands for
/// the first `max(ema_length, atr_length) - 1`.
#[inline]
pub fn keltner(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    ema_length: f64,
    atr_length: f64,
    mult: f64,
) -> Result<[Vec<f64>; 3], ParamError> {
    crate::typed::checked("keltner", &[ema_length, atr_length, mult])?;
    let mut core = KeltnerCore::new(ema_length.round() as usize, atr_length.round() as usize, mult);
    let len = high.len().min(low.len()).min(close.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, b, d] = &mut out;
    for ((((x, y), z), (&h, &l)), &c) in
        a.iter_mut().zip(b.iter_mut()).zip(d.iter_mut()).zip(high.iter().zip(low)).zip(close)
    {
        let v = core.push(h, l, c);
        *x = v[0];
        *y = v[1];
        *z = v[2];
    }
    Ok(out)
}

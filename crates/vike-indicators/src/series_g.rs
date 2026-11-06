//! Whole-series calls on plain `f64` slices for group `g` of the third wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`).
//!
//! The same arithmetic as the typed steppers ([`crate::indicators::Dx`], [`crate::indicators::Supertrend`]
//! and the rest of the group): each call runs the stepper's own core, so a series equals a stream of
//! the same values bit for bit. Every function returns one value per input, `NaN` while the
//! indicator is still warming up, and refuses a parameter the indicator's registry row would not
//! accept with a [`crate::ParamError`]. Inputs given as parallel slices (`high`, `low`, `close`)
//! give a result as long as the SHORTEST of them.

use crate::ParamError;
use crate::fast_g::{
    AdxrCore, ChaikinCore, ChandelierCore, DX, DmCore, MINUS_DI, MINUS_DM, NatrCore, PLUS_DI,
    PLUS_DM, SmmaCore, StarcCore, SupertrendCore,
};

/// One line of the directional movement system from parallel `high`, `low` and `close` slices.
macro_rules! directional_hlc {
    ($(#[$m:meta])* $name:ident, $key:literal, $kind:ident) => {
        $(#[$m])*
        #[inline]
        pub fn $name(
            high: &[f64],
            low: &[f64],
            close: &[f64],
            period: f64,
        ) -> Result<Vec<f64>, ParamError> {
            crate::typed::checked($key, &[period])?;
            let mut core = DmCore::<$kind>::new(period.round() as usize);
            // Filled first, then written by index (see `series::ema`).
            let mut out = vec![f64::NAN; high.len().min(low.len()).min(close.len())];
            for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
                *o = core.push(h, l, c);
            }
            Ok(out)
        }
    };
}

/// One line of the directional movement system from parallel `high` and `low` slices.
macro_rules! directional_hl {
    ($(#[$m:meta])* $name:ident, $key:literal, $kind:ident) => {
        $(#[$m])*
        #[inline]
        pub fn $name(high: &[f64], low: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
            crate::typed::checked($key, &[period])?;
            let mut core = DmCore::<$kind>::new(period.round() as usize);
            let mut out = vec![f64::NAN; high.len().min(low.len())];
            for ((o, &h), &l) in out.iter_mut().zip(high).zip(low) {
                *o = core.push(h, l, f64::NAN);
            }
            Ok(out)
        }
    };
}

directional_hlc! {
    /// Directional movement index from parallel `high`, `low` and `close` slices (the result is as
    /// long as the shortest): `100 * |+DI - -DI| / (+DI + -DI)` of the Wilder-smoothed directional
    /// movement, `0` while the smoothed range is not positive. `period` is the registry's
    /// (`2..=100`); `NaN` for the first `period` values.
    dx, "dx", DX
}

directional_hlc! {
    /// Plus directional indicator (+DI) from parallel `high`, `low` and `close` slices (the result
    /// is as long as the shortest): the Wilder-smoothed upward movement as a percentage of the
    /// smoothed true range. `period` is the registry's (`2..=100`); `NaN` for the first `period`
    /// values. It is the `plus_di` line of [`crate::series::adx`].
    plus_di, "plus_di", PLUS_DI
}

directional_hlc! {
    /// Minus directional indicator (-DI) from parallel `high`, `low` and `close` slices (the result
    /// is as long as the shortest): the Wilder-smoothed downward movement as a percentage of the
    /// smoothed true range. `period` is the registry's (`2..=100`); `NaN` for the first `period`
    /// values. It is the `minus_di` line of [`crate::series::adx`].
    minus_di, "minus_di", MINUS_DI
}

directional_hl! {
    /// Plus directional movement (+DM) from parallel `high` and `low` slices (the result is as long
    /// as the shorter): the Wilder-smoothed sum of the rise in the high on bars where it beats the
    /// fall in the low, in price units. `period` is the registry's (`2..=100`); `NaN` for the
    /// first `period` values.
    plus_dm, "plus_dm", PLUS_DM
}

directional_hl! {
    /// Minus directional movement (-DM) from parallel `high` and `low` slices (the result is as long
    /// as the shorter): the Wilder-smoothed sum of the fall in the low on bars where it beats the
    /// rise in the high, in price units. `period` is the registry's (`2..=100`); `NaN` for the
    /// first `period` values.
    minus_dm, "minus_dm", MINUS_DM
}

/// Chaikin volatility from parallel `high` and `low` slices (the result is as long as the shorter):
/// the percentage change over `roc_period` bars (registry range `1..=100`) of the EMA over
/// `ema_period` bars (`2..=100`) of the high-low range. `NaN` until both the average and the earlier
/// value it is compared with exist, and where that earlier average is zero.
#[inline]
pub fn chaikin_volatility(
    high: &[f64],
    low: &[f64],
    ema_period: f64,
    roc_period: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("chaikin_volatility", &[ema_period, roc_period])?;
    let mut core = ChaikinCore::new(ema_period.round() as usize, roc_period.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len())];
    for ((o, &h), &l) in out.iter_mut().zip(high).zip(low) {
        *o = core.push(h, l);
    }
    Ok(out)
}

/// Chandelier exit from parallel `high`, `low` and `close` slices (the result is as long as the
/// shortest): `[long_stop, short_stop]`, the highest high minus (and the lowest low plus) `mult`
/// Wilder ATRs over the last `period` bars (registry ranges `2..=200` and `0.5..=10`). Both are `NaN`
/// for the first `period - 1` values.
#[inline]
pub fn chandelier(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    period: f64,
    mult: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("chandelier", &[period, mult])?;
    let mut core = ChandelierCore::new(period.round() as usize, mult);
    let len = high.len().min(low.len()).min(close.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, b] = &mut out;
    for ((((x, y), &h), &l), &c) in a.iter_mut().zip(b.iter_mut()).zip(high).zip(low).zip(close) {
        let v = core.push(h, l, c);
        *x = v[0];
        *y = v[1];
    }
    Ok(out)
}

/// STARC bands from parallel `high`, `low` and `close` slices (the result is as long as the
/// shortest): `[upper, mid, lower]`, the `sma_period`-bar mean of the close (registry range
/// `1..=200`) and `mult` (`0.5..=10`) Wilder ATRs over `atr_period` bars (`1..=200`) either side.
/// All three are `NaN` until both the mean and the ATR exist.
#[inline]
pub fn starc(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    sma_period: f64,
    atr_period: f64,
    mult: f64,
) -> Result<[Vec<f64>; 3], ParamError> {
    crate::typed::checked("starc", &[sma_period, atr_period, mult])?;
    let mut core = StarcCore::new(sma_period.round() as usize, atr_period.round() as usize, mult);
    let len = high.len().min(low.len()).min(close.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, b, d] = &mut out;
    for (((((x, y), z), &h), &l), &c) in
        a.iter_mut().zip(b.iter_mut()).zip(d.iter_mut()).zip(high).zip(low).zip(close)
    {
        let v = core.push(h, l, c);
        *x = v[0];
        *y = v[1];
        *z = v[2];
    }
    Ok(out)
}

/// Wilder's smoothed moving average of `close` over `period` values (registry range `2..=400`),
/// seeded with the mean of the first `period` values; `NaN` for the first `period - 1`.
#[inline]
pub fn smma(close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("smma", &[period])?;
    let mut core = SmmaCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Supertrend from parallel `high`, `low` and `close` slices (the result is as long as the
/// shortest): `[supertrend, direction]` for an ATR of `period` bars (registry range `1..=100`) and
/// bands `mult` ATRs from the midpoint (`0.5..=10`). `direction` is `1.0` for an up-trend and `-1.0`
/// for a down-trend; both are `NaN` for the first `period` values.
#[inline]
pub fn supertrend(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    period: f64,
    mult: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("supertrend", &[period, mult])?;
    let mut core = SupertrendCore::new(period.round() as usize, mult);
    let len = high.len().min(low.len()).min(close.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, b] = &mut out;
    for ((((x, y), &h), &l), &c) in a.iter_mut().zip(b.iter_mut()).zip(high).zip(low).zip(close) {
        let v = core.push(h, l, c);
        *x = v[0];
        *y = v[1];
    }
    Ok(out)
}

/// ADX rating from parallel `high`, `low` and `close` slices (the result is as long as the
/// shortest): the mean of the [`crate::series::adx`] line and its value `period` bars earlier (registry range
/// `2..=100`). `NaN` until both exist, at index `3 * period - 1`.
#[inline]
pub fn adxr(high: &[f64], low: &[f64], close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("adxr", &[period])?;
    let mut core = AdxrCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len()).min(close.len())];
    for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
        *o = core.push(h, l, c);
    }
    Ok(out)
}

/// Normalized ATR from parallel `high`, `low` and `close` slices (the result is as long as the
/// shortest): Wilder's average true range over `period` bars (registry range `2..=100`) as a
/// percentage of the close, `100 * atr / close`. `NaN` for the first `period` values and wherever
/// the close is exactly zero.
#[inline]
pub fn natr(high: &[f64], low: &[f64], close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("natr", &[period])?;
    let mut core = NatrCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len()).min(close.len())];
    for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
        *o = core.push(h, l, c);
    }
    Ok(out)
}

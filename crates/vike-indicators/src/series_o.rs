//! Whole-series calls on plain `f64` slices for group `o` of the fourth wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`).
//!
//! The price transforms and Heikin-Ashi take the bar's columns as parallel slices (the result is as
//! long as the shortest) and cannot fail: they have no parameter. `pivot_points` likewise. The
//! structure indicators with parameters refuse a value the registry row would not accept with a
//! [`crate::ParamError`].

use crate::ParamError;
use crate::fast_o::{self, PivotCore, PocCore};

/// Average price `(open + high + low + close) / 4` of each bar, from parallel slices (the result is
/// as long as the shortest). It has no parameter, so it cannot fail.
#[inline]
pub fn avgprice(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    // Stateless, so built by `collect` (each element written once, no prefill pass in front of it).
    open.iter()
        .zip(high)
        .zip(low)
        .zip(close)
        .map(|(((&o, &h), &l), &c)| fast_o::avgprice(o, h, l, c))
        .collect()
}

/// Median price `(high + low) / 2` of each bar, from parallel slices (the result is as long as the
/// shorter). It has no parameter, so it cannot fail.
#[inline]
pub fn medprice(high: &[f64], low: &[f64]) -> Vec<f64> {
    high.iter().zip(low).map(|(&h, &l)| fast_o::medprice(h, l)).collect()
}

/// Typical price `(high + low + close) / 3` of each bar, from parallel slices (the result is as long
/// as the shortest). It has no parameter, so it cannot fail.
#[inline]
pub fn typprice(high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    high.iter().zip(low).zip(close).map(|((&h, &l), &c)| fast_o::typprice(h, l, c)).collect()
}

/// Weighted close `(high + low + 2 * close) / 4` of each bar, from parallel slices (the result is as
/// long as the shortest). It has no parameter, so it cannot fail.
#[inline]
pub fn wclprice(high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    high.iter().zip(low).zip(close).map(|((&h, &l), &c)| fast_o::wclprice(h, l, c)).collect()
}

/// Heikin-Ashi candles from parallel `open`, `high`, `low` and `close` slices (the result is as long
/// as the shortest): `[ha_open, ha_high, ha_low, ha_close]`, each as long as the inputs and defined
/// from the first bar. The first candle opens at the mean of the bar's open and close, every later
/// one at the mean of the previous candle's open and close. It has no parameter, so it cannot fail.
#[inline]
pub fn heikin_ashi(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> [Vec<f64>; 4] {
    let len = open.len().min(high.len()).min(low.len()).min(close.len());
    let mut out =
        [vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]];
    fast_o::heikin_ashi_fill(open, high, low, close, &mut out);
    out
}

/// Classic floor pivots from the PRIOR bar, from parallel `high`, `low` and `close` slices (the
/// result is as long as the shortest): `[p, r1, r2, r3, s1, s2, s3]`, each as long as the inputs, all
/// `NaN` at the first bar. It has no parameter, so it cannot fail.
#[inline]
pub fn pivot_points(high: &[f64], low: &[f64], close: &[f64]) -> [Vec<f64>; 7] {
    let len = high.len().min(low.len()).min(close.len());
    let mut core = PivotCore::new();
    let mut out: [Vec<f64>; 7] = std::array::from_fn(|_| vec![f64::NAN; len]);
    for (i, ((&h, &l), &c)) in high.iter().zip(low).zip(close).enumerate() {
        let v = core.push(h, l, c);
        for (col, x) in out.iter_mut().zip(v) {
            col[i] = x;
        }
    }
    out
}

/// Rolling point of control of the volume profile from parallel `high`, `low`, `close` and `volume`
/// slices (the result is as long as the shortest): the centre price of the highest-volume bin when
/// the last `window` bars' volume is bucketed by close into `bins` bins across their price range (the
/// first bin wins a tie; the window's price when it has no range). `window` is `5..=500`, `bins`
/// `4..=200`; `NaN` for the first `window - 1` values.
#[inline]
pub fn volume_profile_poc(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    volume: &[f64],
    window: f64,
    bins: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("volume_profile_poc", &[window, bins])?;
    let mut core = PocCore::new(window.round() as usize, bins.round() as usize);
    let len = high.len().min(low.len()).min(close.len()).min(volume.len());
    let mut out = vec![f64::NAN; len];
    for ((((o, &h), &l), &c), &v) in out.iter_mut().zip(high).zip(low).zip(close).zip(volume) {
        *o = core.push(h, l, c, v);
    }
    Ok(out)
}

/// ZigZag pivots from parallel `high` and `low` slices (the result is as long as the shorter): the
/// price of each swing extreme at its own bar once price has reversed by `deviation` percent
/// (`0.1..=50`) from it, `NaN` between pivots. It reads FUTURE bars (a pivot is only known after the
/// reversal), so the value at the newest bars is final only once the reversal has happened; this is
/// the batch kernel of the `zigzag` indicator on plain slices.
#[inline]
pub fn zigzag(high: &[f64], low: &[f64], deviation: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("zigzag", &[deviation])?;
    Ok(fast_o::zigzag_slices(high, low, deviation))
}

/// Williams fractals from parallel `high` and `low` slices (the result is as long as the shorter):
/// `[fractal_up, fractal_down]`, a bar whose high is above (low below) the `n` (`1..=10`) bars on
/// each side, its own price at that bar and `NaN` elsewhere. It reads FUTURE bars (the `n` after),
/// so the last `n` values are always `NaN`; this is the batch kernel of the `williams_fractal`
/// indicator on plain slices.
#[inline]
pub fn williams_fractal(high: &[f64], low: &[f64], n: f64) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("williams_fractal", &[n])?;
    Ok(fast_o::williams_fractal_slices(high, low, n.round() as usize))
}

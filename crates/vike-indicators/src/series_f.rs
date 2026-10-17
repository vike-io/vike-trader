//! Whole-series calls on plain `f64` slices for group `f` of the third wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`).
//!
//! `series::rocp`, `series::rocr`, `series::rocr100`, `series::bop`, `series::dpo`, `series::ac`,
//! `series::aroonosc`, `series::elder_ray`, `series::ultosc` and `series::vortex`: the same
//! arithmetic as [`crate::indicators::Rocp`], [`crate::indicators::Rocr`],
//! [`crate::indicators::Rocr100`], [`crate::indicators::Bop`], [`crate::indicators::Dpo`],
//! [`crate::indicators::Ac`], [`crate::indicators::Aroonosc`], [`crate::indicators::ElderRay`],
//! [`crate::indicators::Ultosc`] and [`crate::indicators::Vortex`], over plain prices with no `Bar`
//! in between. Each returns one value per input (the SHORTEST of the input slices when there are
//! several), `NaN` while the indicator is warming up, and refuses a parameter the indicator's
//! registry row would not accept with a [`crate::ParamError`].

use crate::ParamError;
use crate::fast_d1::window;
use crate::fast_f::{
    AcCore, AroonOscCore, ElderRayCore, UltoscCore, VortexCore, bop_of, rocp_of, rocr_of,
    rocr100_of,
};

/// `out[i] = of(close[i], close[i - n])`, `NaN` for the first `n` values: written once, front to
/// back, with the input itself as the window (no ring).
#[inline(always)]
fn lagged(close: &[f64], n: usize, of: impl Fn(f64, f64) -> f64) -> Vec<f64> {
    let len = close.len();
    let mut out = Vec::with_capacity(len);
    out.resize(len.min(n), f64::NAN);
    if len > n {
        out.extend(close[n..].iter().zip(close).map(|(&now, &then)| of(now, then)));
    }
    out
}

/// Rate-of-change proportion of `close`: `(close[i] - close[i - length]) / close[i - length]`,
/// `length` in the registry range `1..=400`; `NaN` for the first `length` values and where the
/// earlier close is zero.
#[inline]
pub fn rocp(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("rocp", &[length])?;
    Ok(lagged(close, window(length), rocp_of))
}

/// Rate-of-change ratio of `close`: `close[i] / close[i - length]`, `length` in the registry range
/// `1..=400`; `NaN` for the first `length` values and where the earlier close is zero.
#[inline]
pub fn rocr(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("rocr", &[length])?;
    Ok(lagged(close, window(length), rocr_of))
}

/// Rate-of-change ratio of `close` times 100: `close[i] / close[i - length] * 100`, `length` in the
/// registry range `1..=400`; `NaN` for the first `length` values and where the earlier close is
/// zero.
#[inline]
pub fn rocr100(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("rocr100", &[length])?;
    Ok(lagged(close, window(length), rocr100_of))
}

/// Balance of power of parallel `open`, `high`, `low` and `close` slices:
/// `(close - open) / (high - low)`, `0` on a bar with no range. The result is as long as the
/// SHORTEST slice and has a value from the first bar.
#[inline]
pub fn bop(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    let len = open.len().min(high.len()).min(low.len()).min(close.len());
    // No state to carry, so a pure map: written once, front to back, and free to vectorize.
    let mut out = Vec::with_capacity(len);
    out.extend(
        open.iter().zip(high).zip(low).zip(close).map(|(((&op, &h), &l), &c)| bop_of(op, h, l, c)),
    );
    out
}

/// Detrended price oscillator of `close`: the close `length / 2 + 1` bars back minus the
/// `length`-bar mean of the close, `length` in the registry range `2..=400`; `NaN` until both are
/// defined.
#[inline]
pub fn dpo(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("dpo", &[length])?;
    let n = window(length);
    let shift = n / 2 + 1;
    let len = close.len();
    // The mean is `DpoCore`'s (`sma_run` runs the operations of `SmaCore::push`), then the close
    // `shift` bars back is taken off it in place.
    let mut out = Vec::new();
    crate::roll::sma_run(len, n, |i| close[i], &mut out);
    let (head, tail) = out.split_at_mut(shift.min(len));
    head.fill(f64::NAN);
    for (o, &then) in tail.iter_mut().zip(close) {
        *o = then - *o;
    }
    Ok(out)
}

/// Accelerator oscillator of parallel `high` and `low` slices: the awesome oscillator (`SMA5 -
/// SMA34` of the median price) minus its own 5-value mean. The result is as long as the SHORTER
/// slice; `NaN` for the first 37 values.
#[inline]
pub fn ac(high: &[f64], low: &[f64]) -> Vec<f64> {
    let mut core = AcCore::new();
    let len = high.len().min(low.len());
    let mut out = vec![f64::NAN; len];
    for ((o, &h), &l) in out.iter_mut().zip(high).zip(low) {
        *o = core.push(h, l);
    }
    out
}

/// Aroon oscillator of parallel `high` and `low` slices: aroon up minus aroon down, from -100 to
/// 100, as long as the SHORTER slice. How recently, within the last `length + 1` bars (`length` in
/// the registry range `2..=400`), the highest high and the lowest low were made; `NaN` for the first
/// `length` values.
#[inline]
pub fn aroonosc(high: &[f64], low: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("aroonosc", &[length])?;
    let mut core = AroonOscCore::new(window(length));
    let len = high.len().min(low.len());
    let mut out = vec![f64::NAN; len];
    for ((o, &h), &l) in out.iter_mut().zip(high).zip(low) {
        *o = core.push(h, l);
    }
    Ok(out)
}

/// Elder ray of parallel `high`, `low` and `close` slices: `[bull_power, bear_power]`, the high and
/// the low minus the `length`-bar exponential average of the close (`length` in the registry range
/// `2..=200`, seeded with the mean of the first `length` closes). Each is as long as the SHORTEST
/// slice; `NaN` for the first `length - 1` values.
#[inline]
pub fn elder_ray(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    length: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("elder_ray", &[length])?;
    let mut core = ElderRayCore::new(window(length));
    let len = high.len().min(low.len()).min(close.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [bull, bear] = &mut out;
    for ((((u, d), &h), &l), &c) in
        bull.iter_mut().zip(bear.iter_mut()).zip(high).zip(low).zip(close)
    {
        let v = core.push(h, l, c);
        *u = v[0];
        *d = v[1];
    }
    Ok(out)
}

/// Ultimate oscillator of parallel `high`, `low` and `close` slices, 0 to 100: buying pressure over
/// true range summed across `p1`, `p2` and `p3` bars (registry ranges `2..=100`, `2..=200` and
/// `2..=400`), weighted 4, 2 and 1, and `0` for a window with no range. The result is as long as the
/// SHORTEST slice; `NaN` for the first `max(p1, p2, p3)` values.
#[inline]
pub fn ultosc(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    p1: f64,
    p2: f64,
    p3: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("ultosc", &[p1, p2, p3])?;
    let mut core = UltoscCore::new(window(p1), window(p2), window(p3));
    let len = high.len().min(low.len()).min(close.len());
    let mut out = vec![f64::NAN; len];
    for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
        *o = core.push(h, l, c);
    }
    Ok(out)
}

/// Vortex indicator of parallel `high`, `low` and `close` slices: `[vi_plus, vi_minus]` over
/// `length` bars (registry range `2..=200`), each as long as the SHORTEST slice. `NaN` for the first
/// `length` values and where the true range sums to nothing.
#[inline]
pub fn vortex(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    length: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("vortex", &[length])?;
    let mut core = VortexCore::new(window(length));
    let len = high.len().min(low.len()).min(close.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [plus, minus] = &mut out;
    for ((((p, m), &h), &l), &c) in
        plus.iter_mut().zip(minus.iter_mut()).zip(high).zip(low).zip(close)
    {
        let v = core.push(h, l, c);
        *p = v[0];
        *m = v[1];
    }
    Ok(out)
}

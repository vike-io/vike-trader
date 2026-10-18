//! Whole-series calls on plain `f64` slices for group `c` of the second wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`).

use crate::ParamError;
use crate::fast_c::{AdCore, AdxCore, ObvCore, PsarCore, StochRsiCore};

/// Rate of change of `close` over `length` values, in percent (registry range `1..=1000`):
/// `100 * (c[i] - c[i - length]) / c[i - length]`. `NaN` for the first `length` values and wherever
/// the value `length` back is exactly zero.
#[inline]
pub fn roc(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("roc", &[length])?;
    Ok(crate::fast_c::roc_vec(length.round() as usize, close))
}

/// On-balance volume from parallel `close` and `volume` slices (the result is as long as the
/// shorter): a running sum of volume, added when the close rose and subtracted when it fell. The
/// first value is `0`. It has no parameter, so it cannot fail.
#[inline]
pub fn obv(close: &[f64], volume: &[f64]) -> Vec<f64> {
    let mut core = ObvCore::new();
    let mut out = vec![f64::NAN; close.len().min(volume.len())];
    for ((o, &c), &v) in out.iter_mut().zip(close).zip(volume) {
        *o = core.push(c, v);
    }
    out
}

/// Parabolic SAR from parallel `high` and `low` slices (the result is as long as the shorter),
/// with the acceleration `step` (registry range `0.001..=0.5`) and the cap `max_af` on the
/// acceleration factor (`0.05..=1.0`). The first value is the first low (an up-trend is assumed).
#[inline]
pub fn psar(high: &[f64], low: &[f64], step: f64, max_af: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("psar", &[step, max_af])?;
    let mut core = PsarCore::new(step, max_af);
    let mut out = vec![f64::NAN; high.len().min(low.len())];
    for ((o, &h), &l) in out.iter_mut().zip(high).zip(low) {
        *o = core.push(h, l);
    }
    Ok(out)
}

/// Chaikin accumulation/distribution line from parallel `high`, `low`, `close` and `volume`
/// slices (the result is as long as the shortest): a running sum of
/// `((close - low) - (high - close)) / (high - low) * volume`, a bar with no range adding `0`.
/// It has no parameter, so it cannot fail.
#[inline]
pub fn ad(high: &[f64], low: &[f64], close: &[f64], volume: &[f64]) -> Vec<f64> {
    let len = high.len().min(low.len()).min(close.len()).min(volume.len());
    let mut core = AdCore::new();
    let mut out = vec![f64::NAN; len];
    for ((((o, &h), &l), &c), &v) in out.iter_mut().zip(high).zip(low).zip(close).zip(volume) {
        *o = core.push(h, l, c, v);
    }
    out
}

/// Average directional index from parallel `high`, `low` and `close` slices (the result is as long
/// as the shortest): `[adx, plus_di, minus_di]`, each as long as the inputs. `period` is the
/// registry's (`2..=100`); the DI lines start at index `period`, `adx` at `2 * period - 1`, and
/// each is `NaN` before that.
#[inline]
pub fn adx(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    period: f64,
) -> Result<[Vec<f64>; 3], ParamError> {
    crate::typed::checked("adx", &[period])?;
    let mut core = AdxCore::new(period.round() as usize);
    let len = high.len().min(low.len()).min(close.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, p, m] = &mut out;
    for (((((x, y), z), &h), &l), &c) in
        a.iter_mut().zip(p.iter_mut()).zip(m.iter_mut()).zip(high).zip(low).zip(close)
    {
        let v = core.push(h, l, c);
        *x = v[0];
        *y = v[1];
        *z = v[2];
    }
    Ok(out)
}

/// Stochastic RSI of `close`: `[%K, %D]`, each as long as `close`. The Wilder RSI over `rsi_p`
/// moves (`2..=100`) is placed inside its own `k`-value range (`2..=100`) for `%K`, and `%D` is the
/// mean of the last `d` (`1..=50`) defined `%K`. `NaN` until each line exists
/// (`%K` at index `rsi_p + k - 1`).
#[inline]
pub fn stochrsi(close: &[f64], rsi_p: f64, k: f64, d: f64) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("stochrsi", &[rsi_p, k, d])?;
    let mut core =
        StochRsiCore::new(rsi_p.round() as usize, k.round() as usize, d.round() as usize);
    let len = close.len();
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, b] = &mut out;
    for ((x, y), &c) in a.iter_mut().zip(b.iter_mut()).zip(close) {
        let v = core.push(c);
        *x = v[0];
        *y = v[1];
    }
    Ok(out)
}

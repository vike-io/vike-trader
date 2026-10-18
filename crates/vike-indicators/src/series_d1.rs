//! Whole-series calls on plain `f64` slices for group `d1` of the second wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`).
//!
//! `series::mom`, `series::cmo`, `series::aroon`, `series::stochf`, `series::mfi` and
//! `series::cmf`: the same arithmetic as [`crate::indicators::Mom`], [`crate::indicators::Cmo`],
//! [`crate::indicators::Aroon`], [`crate::indicators::Stochf`], [`crate::indicators::Mfi`] and
//! [`crate::indicators::Cmf`], over plain prices with no `Bar` in between. Each returns one value per
//! input (the SHORTEST of the input slices when there are several), `NaN` while the indicator is
//! warming up, and refuses a parameter the indicator's registry row would not accept with a
//! [`crate::ParamError`].

use crate::ParamError;
use crate::fast_d1::{AroonCore, CmfCore, CmoCore, MfiCore, StochfCore, window};

/// Momentum of `close`: `close[i] - close[i - length]`, `length` in the registry range `1..=400`;
/// `NaN` for the first `length` values.
#[inline]
pub fn mom(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("mom", &[length])?;
    let n = window(length);
    let len = close.len();
    // Written once, front to back (`extend` of an exact-size iterator), not filled and then rewritten.
    let mut out = Vec::with_capacity(len);
    out.resize(len.min(n), f64::NAN);
    if len > n {
        // The one subtraction `MomCore::push` makes, the ring replaced by the input itself.
        out.extend(close[n..].iter().zip(close).map(|(now, then)| now - then));
    }
    Ok(out)
}

/// Chande momentum oscillator of `close` over `length` moves (registry range `2..=200`), from -100
/// to 100, `0` where none of the moves in the window moved; `NaN` for the first `length` values.
#[inline]
pub fn cmo(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("cmo", &[length])?;
    let mut core = CmoCore::new(window(length));
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Aroon of parallel `high` and `low` slices: `[aroon_up, aroon_down]`, each 0 to 100 and as long as
/// the SHORTER slice. How recently, within the last `length + 1` bars (`length` in the registry
/// range `2..=400`), the highest high and the lowest low were made; `NaN` for the first `length`
/// values.
#[inline]
pub fn aroon(high: &[f64], low: &[f64], length: f64) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("aroon", &[length])?;
    let mut core = AroonCore::new(window(length));
    let len = high.len().min(low.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [up, dn] = &mut out;
    for (((u, d), &h), &l) in up.iter_mut().zip(dn.iter_mut()).zip(high).zip(low) {
        let v = core.push(h, l);
        *u = v[0];
        *d = v[1];
    }
    Ok(out)
}

/// Fast stochastic of parallel `high`, `low` and `close` slices: `[%K, %D]`, each 0 to 100 and as
/// long as the SHORTEST slice. `%K` is the close's place in the `k`-bar range (`k` in the registry
/// range `2..=100`), `0` on a window with no range, and `%D` the mean of the last `d` values of `%K`
/// (`d` in `1..=50`); `NaN` until `k` bars (`%K`) and `k + d - 1` bars (`%D`) are in.
#[inline]
pub fn stochf(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    k: f64,
    d: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("stochf", &[k, d])?;
    let mut core = StochfCore::new(window(k), window(d));
    let len = high.len().min(low.len()).min(close.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [pk, pd] = &mut out;
    for ((((a, b), &h), &l), &c) in pk.iter_mut().zip(pd.iter_mut()).zip(high).zip(low).zip(close) {
        let v = core.push(h, l, c);
        *a = v[0];
        *b = v[1];
    }
    Ok(out)
}

/// Money flow index of parallel `high`, `low`, `close` and `volume` slices over `length` bars
/// (registry range `2..=100`), 0 to 100, `100` when nothing flowed down; the result is as long as
/// the SHORTEST slice. `NaN` for the first `length` values.
#[inline]
pub fn mfi(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    volume: &[f64],
    length: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("mfi", &[length])?;
    let mut core = MfiCore::new(window(length));
    let len = high.len().min(low.len()).min(close.len()).min(volume.len());
    let mut out = vec![f64::NAN; len];
    for ((((o, &h), &l), &c), &v) in out.iter_mut().zip(high).zip(low).zip(close).zip(volume) {
        *o = core.push(h, l, c, v);
    }
    Ok(out)
}

/// Chaikin money flow of parallel `high`, `low`, `close` and `volume` slices over `length` bars
/// (registry range `2..=200`), -1 to 1, `NaN` where the window has no volume; the result is as long
/// as the SHORTEST slice. `NaN` for the first `length - 1` values.
#[inline]
pub fn cmf(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    volume: &[f64],
    length: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("cmf", &[length])?;
    let mut core = CmfCore::new(window(length));
    let len = high.len().min(low.len()).min(close.len()).min(volume.len());
    let mut out = vec![f64::NAN; len];
    for ((((o, &h), &l), &c), &v) in out.iter_mut().zip(high).zip(low).zip(close).zip(volume) {
        *o = core.push(h, l, c, v);
    }
    Ok(out)
}

//! Whole-series calls on plain `f64` slices for group `i` of the third wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`): `zlema`, `coppock`, `tsi`,
//! `smi_ergodic`, `mass`, `kst`, `qstick`, `rwi`, `acceleration_bands` and `ttm_squeeze`.
//!
//! Each runs the same core as the typed stepper (`crate::fast_i`), so the result equals the stream
//! and `vectorize` bit for bit. Every function returns one value per input, `NaN` while the
//! indicator is still warming up, and refuses a parameter the indicator's registry row would not
//! accept with a [`crate::ParamError`]. Inputs given as parallel slices give a result as long as the
//! SHORTEST of them.

use crate::ParamError;
use crate::fast_i::{
    AccelCore, CoppockCore, KstCore, MassCore, QstickCore, RwiCore, SmiCore, TsiCore, TtmCore,
    ZlemaCore,
};

/// Zero-lag exponential moving average of `close` over `length` values (registry range
/// `2..=400`): an EMA of `close + (close - close[(length - 1) / 2 back])`, seeded with the mean of
/// the first `length` de-lagged values; `NaN` for the first `length - 1`. A `NaN` close poisons the
/// rest of the series.
#[inline]
pub fn zlema(close: &[f64], length: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("zlema", &[length])?;
    let mut core = ZlemaCore::new(length.round() as usize);
    // Filled first, then written by index: the page faults of a fresh buffer land in the fill, not
    // inside the recurrence.
    let mut out = vec![f64::NAN; close.len()];
    core.run(close, |&c| c, &mut out);
    Ok(out)
}

/// Coppock curve of `close`: a `wma_p`-value linearly weighted average of the sum of the
/// `roc_long`- and `roc_short`-bar rates of change in percent (registry ranges `wma_p` `2..=100`,
/// `roc_long` and `roc_short` `2..=200`). A bar whose rates are not both defined (a `NaN` or zero
/// reference close) is `NaN` and left out of the average; `NaN` until `wma_p` sums exist.
#[inline]
pub fn coppock(
    close: &[f64],
    wma_p: f64,
    roc_long: f64,
    roc_short: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("coppock", &[wma_p, roc_long, roc_short])?;
    let mut core = CoppockCore::new(
        wma_p.round() as usize,
        roc_long.round() as usize,
        roc_short.round() as usize,
    );
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// True strength index of `close`: 100 times the `short`-smoothed `long`-smoothed close-to-close
/// change over the same smoothing of its absolute value (registry ranges `long` `2..=400`, `short`
/// `2..=200`); `NaN` for the first `long + short - 2` values and wherever the smoothed absolute
/// change is exactly zero. A `NaN` close poisons the rest of the series.
#[inline]
pub fn tsi(close: &[f64], long: f64, short: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("tsi", &[long, short])?;
    let mut core = TsiCore::new(long.round() as usize, short.round() as usize);
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// SMI ergodic oscillator of `close`: `[smi, signal]`, each as long as `close`. `smi` is the true
/// strength ratio (see [`tsi`]; registry ranges `long` `2..=400`, `short` and `signal` `2..=200`),
/// `signal` an EMA of its defined values over `signal` of them. `NaN` while a line is not defined.
#[inline]
pub fn smi_ergodic(
    close: &[f64],
    long: f64,
    short: f64,
    signal: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("smi_ergodic", &[long, short, signal])?;
    let mut core =
        SmiCore::new(long.round() as usize, short.round() as usize, signal.round() as usize);
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

/// Mass index from parallel `high` and `low` slices (registry ranges `period` `2..=200`,
/// `ema_period` `2..=50`): the sum over `period` bars of `EMA(high - low) / EMA(EMA(high - low))`
/// (both over `ema_period`); the result is as long as the SHORTER slice. `NaN` until a full window
/// of defined ratios exists, and while an undefined one is inside it.
#[inline]
pub fn mass(
    high: &[f64],
    low: &[f64],
    period: f64,
    ema_period: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("mass", &[period, ema_period])?;
    let mut core = MassCore::new(period.round() as usize, ema_period.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len())];
    for ((o, &h), &l) in out.iter_mut().zip(high).zip(low) {
        *o = core.push(h, l);
    }
    Ok(out)
}

/// Know Sure Thing of `close`: `[kst, signal]`, each as long as `close`. The weighted sum
/// `1*r1 + 2*r2 + 3*r3 + 4*r4` of four rates of change in percent (`roc1..roc4` bars back, registry
/// range `1..=200`), each averaged over `sma1..sma4` defined values (`2..=200`); `signal` is the
/// simple average of `kst` over `signal` defined values. A rate that is undefined (a `NaN` or zero
/// reference close) is skipped by its average and makes `kst` `NaN` at that bar.
#[inline]
pub fn kst(
    close: &[f64],
    roc1: f64,
    sma1: f64,
    roc2: f64,
    sma2: f64,
    roc3: f64,
    sma3: f64,
    roc4: f64,
    sma4: f64,
    signal: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("kst", &[roc1, sma1, roc2, sma2, roc3, sma3, roc4, sma4, signal])?;
    let mut core = KstCore::new(
        [
            roc1.round() as usize,
            roc2.round() as usize,
            roc3.round() as usize,
            roc4.round() as usize,
        ],
        [
            sma1.round() as usize,
            sma2.round() as usize,
            sma3.round() as usize,
            sma4.round() as usize,
        ],
        signal.round() as usize,
    );
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

/// Qstick from parallel `open` and `close` slices: the simple average of `close - open` over
/// `period` bars (registry range `2..=200`); the result is as long as the SHORTER slice. `NaN` for
/// the first `period - 1` values and while a `NaN` is inside the window.
#[inline]
pub fn qstick(open: &[f64], close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("qstick", &[period])?;
    let mut core = QstickCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; open.len().min(close.len())];
    for ((o, &op), &c) in out.iter_mut().zip(open).zip(close) {
        *o = core.push(op, c);
    }
    Ok(out)
}

/// Random walk index from parallel `high`, `low` and `close` slices: `[rwi_high, rwi_low]`, each as
/// long as the SHORTEST of them. `rwi_high` is `(high - low period bars ago) / (ATR * sqrt(period))`
/// and `rwi_low` is `(high period bars ago - low) / (ATR * sqrt(period))`, with ATR the plain
/// average of the last `period` true ranges (registry range `2..=200`). `NaN` for the first
/// `period` values and where the average range is zero.
#[inline]
pub fn rwi(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    period: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("rwi", &[period])?;
    let mut core = RwiCore::new(period.round() as usize);
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

/// Headley's acceleration bands from parallel `high`, `low` and `close` slices:
/// `[upper, mid, lower]`, each as long as the SHORTEST of them. With `w = (high - low) / (high +
/// low)` (0 when the sum is 0): `upper` is the `period`-bar mean of `high * (1 + factor * w)`, `mid`
/// of the close, `lower` of `low * (1 - factor * w)` (registry ranges `period` `2..=400`, `factor`
/// `0.5..=10`); `NaN` for the first `period - 1` values.
#[inline]
pub fn acceleration_bands(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    period: f64,
    factor: f64,
) -> Result<[Vec<f64>; 3], ParamError> {
    crate::typed::checked("acceleration_bands", &[period, factor])?;
    let mut core = AccelCore::new(period.round() as usize, factor);
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

/// TTM Squeeze from parallel `high`, `low` and `close` slices: `[momentum, squeeze]`, each as long
/// as the SHORTEST of them (registry ranges `length` `2..=200`, `bb_mult` and `kc_mult`
/// `0.5..=5`). `squeeze` is `1.0` while the Bollinger bands (`bb_mult` population standard
/// deviations of the close either side of its `length`-bar mean) lie strictly inside the Keltner
/// channel (`kc_mult` mean true ranges either side of it), else `0.0`, from value `length - 1`;
/// `momentum` is the least-squares line's value at the newest point of
/// `close - ((highest high + lowest low) / 2 + mean) / 2` over the last `length` values of it,
/// from value `2 * length - 2`.
#[inline]
pub fn ttm_squeeze(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    length: f64,
    bb_mult: f64,
    kc_mult: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("ttm_squeeze", &[length, bb_mult, kc_mult])?;
    let mut core = TtmCore::new(length.round() as usize, bb_mult, kc_mult);
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

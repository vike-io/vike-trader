//! Whole-series calls on plain `f64` slices for group `k` of the fourth wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`): the volume family `adosc`,
//! `anchored_vwap`, `efi`, `eom`, `kvo`, `net_volume`, `nvi`, `pvi`, `pvt`, `volume_osc`,
//! `williams_ad` and `vwap`.
//!
//! Each runs the same core as the typed stepper (`crate::fast_k`), so the result equals the stream
//! and `vectorize` bit for bit. A call over several parallel slices returns as many values as the
//! SHORTEST of them has, `NaN` while the indicator is warming up, and refuses a parameter the
//! indicator's registry row would not accept with a [`crate::ParamError`]. The two calls that read
//! the bar's time (`anchored_vwap`, `vwap`) take it as `ts`, epoch milliseconds, the same `i64` a
//! `Bar` holds.

use crate::ParamError;
use crate::fast_k::{
    AdoscCore, AnchoredVwapCore, EfiCore, EomCore, KvoCore, NetVolumeCore, PvtCore,
    VolumeIndexCore, VolumeOscCore, VwapCore, WilliamsAdCore,
};

/// Chaikin A/D oscillator from parallel `high`, `low`, `close` and `volume` slices: the `fast`-value
/// (registry range `2..=50`) EMA of the accumulation/distribution line minus its `slow`-value
/// (`2..=200`) EMA. `NaN` until both have seeded (index `max(fast, slow) - 1`).
#[inline]
pub fn adosc(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    volume: &[f64],
    fast: f64,
    slow: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("adosc", &[fast, slow])?;
    let mut core = AdoscCore::new(fast.round() as usize, slow.round() as usize);
    let len = high.len().min(low.len()).min(close.len()).min(volume.len());
    let mut out = vec![f64::NAN; len];
    for ((((o, &h), &l), &c), &v) in out.iter_mut().zip(high).zip(low).zip(close).zip(volume) {
        *o = core.push(h, l, c, v);
    }
    Ok(out)
}

/// Anchored VWAP from parallel `ts` (epoch milliseconds), `high`, `low`, `close` and `volume` slices:
/// the volume-weighted average of the typical price `(high + low + close) / 3` accumulated from the
/// first bar whose `ts` is at or after `anchor_ts` (registry range `0..=4e12`; 0 anchors at the first
/// bar). `NaN` before the anchor and while no volume has accumulated.
#[inline]
pub fn anchored_vwap(
    ts: &[i64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    volume: &[f64],
    anchor_ts: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("anchored_vwap", &[anchor_ts])?;
    let mut core = AnchoredVwapCore::new(anchor_ts);
    let len = ts.len().min(high.len()).min(low.len()).min(close.len()).min(volume.len());
    let mut out = vec![f64::NAN; len];
    for (((((o, &t), &h), &l), &c), &v) in
        out.iter_mut().zip(ts).zip(high).zip(low).zip(close).zip(volume)
    {
        *o = core.push(t, h, l, c, v);
    }
    Ok(out)
}

/// Elder force index from parallel `close` and `volume` slices: the `period`-value (registry range
/// `2..=200`) EMA of `(close - previous close) * volume`. The first bar has no previous close, so the
/// line starts at index `period`.
#[inline]
pub fn efi(close: &[f64], volume: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("efi", &[period])?;
    let mut core = EfiCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; close.len().min(volume.len())];
    for ((o, &c), &v) in out.iter_mut().zip(close).zip(volume) {
        *o = core.push(c, v);
    }
    Ok(out)
}

/// Ease of movement from parallel `high`, `low` and `volume` slices: the mean over the last `period`
/// values (registry range `2..=200`) of `(midpoint move) / (volume / (high - low))`, a bar with no
/// range or no volume counting `0`. `NaN` for the first `period` values.
#[inline]
pub fn eom(high: &[f64], low: &[f64], volume: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("eom", &[period])?;
    let mut core = EomCore::new(period.round() as usize);
    let mut out = vec![f64::NAN; high.len().min(low.len()).min(volume.len())];
    for (((o, &h), &l), &v) in out.iter_mut().zip(high).zip(low).zip(volume) {
        *o = core.push(h, l, v);
    }
    Ok(out)
}

/// Klinger volume oscillator from parallel `high`, `low`, `close` and `volume` slices: `[kvo, signal]`,
/// each as long as the shortest. `kvo` is a `fast`-value EMA (registry range `2..=200`) of the signed
/// volume force minus a `slow`-value one (`2..=500`), from index `max(fast, slow)`; `signal` is a
/// `signal`-value EMA (`2..=100`) of `kvo`, trailing it by `signal - 1` values.
#[inline]
pub fn kvo(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    volume: &[f64],
    fast: f64,
    slow: f64,
    signal: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("kvo", &[fast, slow, signal])?;
    let mut core =
        KvoCore::new(fast.round() as usize, slow.round() as usize, signal.round() as usize);
    let len = high.len().min(low.len()).min(close.len()).min(volume.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, b] = &mut out;
    for (((((x, y), &h), &l), &c), &v) in
        a.iter_mut().zip(b.iter_mut()).zip(high).zip(low).zip(close).zip(volume)
    {
        let r = core.push(h, l, c, v);
        *x = r[0];
        *y = r[1];
    }
    Ok(out)
}

/// Signed volume from parallel `close` and `volume` slices: the bar's volume when the close rose, its
/// negation when it fell, `0` otherwise and on the first bar. It has no parameter, so it cannot fail.
#[inline]
pub fn net_volume(close: &[f64], volume: &[f64]) -> Vec<f64> {
    let mut core = NetVolumeCore::new();
    let mut out = vec![f64::NAN; close.len().min(volume.len())];
    for ((o, &c), &v) in out.iter_mut().zip(close).zip(volume) {
        *o = core.push(c, v);
    }
    out
}

/// Negative volume index from parallel `close` and `volume` slices: starts at 1000 and is multiplied by
/// `1 + (close - previous close) / previous close` on the bars whose volume fell from the previous
/// bar. It has no parameter, so it cannot fail.
#[inline]
pub fn nvi(close: &[f64], volume: &[f64]) -> Vec<f64> {
    let mut core = VolumeIndexCore::<false>::new();
    let mut out = vec![f64::NAN; close.len().min(volume.len())];
    for ((o, &c), &v) in out.iter_mut().zip(close).zip(volume) {
        *o = core.push(c, v);
    }
    out
}

/// Positive volume index from parallel `close` and `volume` slices: starts at 1000 and is multiplied
/// by `1 + (close - previous close) / previous close` on the bars whose volume rose from the previous
/// bar. It has no parameter, so it cannot fail.
#[inline]
pub fn pvi(close: &[f64], volume: &[f64]) -> Vec<f64> {
    let mut core = VolumeIndexCore::<true>::new();
    let mut out = vec![f64::NAN; close.len().min(volume.len())];
    for ((o, &c), &v) in out.iter_mut().zip(close).zip(volume) {
        *o = core.push(c, v);
    }
    out
}

/// Price volume trend from parallel `close` and `volume` slices: a running sum of
/// `volume * (close - previous close) / previous close`. The first value is `0`. It has no
/// parameter, so it cannot fail.
#[inline]
pub fn pvt(close: &[f64], volume: &[f64]) -> Vec<f64> {
    let mut core = PvtCore::new();
    let mut out = vec![f64::NAN; close.len().min(volume.len())];
    for ((o, &c), &v) in out.iter_mut().zip(close).zip(volume) {
        *o = core.push(c, v);
    }
    out
}

/// Volume oscillator of a `volume` slice: `(short - long) / long * 100`, `short` and `long` being
/// EMAs of the volume over `short` values (registry range `2..=50`) and `long` values (`2..=200`).
/// `NaN` until both have seeded (index `max(short, long) - 1`) and wherever the long average is `0`.
#[inline]
pub fn volume_osc(volume: &[f64], short: f64, long: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("volume_osc", &[short, long])?;
    let mut core = VolumeOscCore::new(short.round() as usize, long.round() as usize);
    let mut out = vec![f64::NAN; volume.len()];
    for (o, &v) in out.iter_mut().zip(volume) {
        *o = core.push(v);
    }
    Ok(out)
}

/// Williams' accumulation/distribution from parallel `high`, `low` and `close` slices: a running sum
/// of the close's distance above the true low on an up close and below the true high on a down close
/// (the previous close extends both), nothing on an unchanged close. The first value is `0`. It has
/// no parameter, so it cannot fail.
#[inline]
pub fn williams_ad(high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    let mut core = WilliamsAdCore::new();
    let mut out = vec![f64::NAN; high.len().min(low.len()).min(close.len())];
    for (((o, &h), &l), &c) in out.iter_mut().zip(high).zip(low).zip(close) {
        *o = core.push(h, l, c);
    }
    out
}

/// Volume-weighted average price restarted each UTC day, from parallel `ts` (epoch milliseconds),
/// `high`, `low`, `close` and `volume` slices: the running sum of typical price times volume over the
/// running sum of volume, both zeroed when `ts / 86_400_000` changes. `NaN` while the day's volume is
/// not positive. It has no parameter, so it cannot fail.
#[inline]
pub fn vwap(ts: &[i64], high: &[f64], low: &[f64], close: &[f64], volume: &[f64]) -> Vec<f64> {
    let mut core = VwapCore::new();
    let len = ts.len().min(high.len()).min(low.len()).min(close.len()).min(volume.len());
    let mut out = vec![f64::NAN; len];
    for (((((o, &t), &h), &l), &c), &v) in
        out.iter_mut().zip(ts).zip(high).zip(low).zip(close).zip(volume)
    {
        *o = core.push(t, h, l, c, v);
    }
    out
}

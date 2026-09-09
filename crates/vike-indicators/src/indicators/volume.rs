//! Volume indicators — faithful f64 port of vike-trader-app
//! `core/indicators/volume.py`. Skips names already in `base.rs` (obv/vwap).
//! Each `batch_*` is a line-for-line port of the Python function (Python `None`
//! → `f64::NAN`); streaming is history-recompute via [`hist_indicator!`]
//! (correct-by-construction for these causal indicators). Fold order is load-bearing for bit
//! parity.
//!
//! ⚠ **This header used to say "and the running-sum forms" are load-bearing.** `batch_cmf`'s was,
//! and it was WRONG: a carried sum is not a function of its window, so the drain returned
//! different bits for the identical bars. The genuinely cumulative series here (`ad`, `nvi`,
//! `pvi`, `pvt`, `net_volume`) accumulate BY DEFINITION and are exempt from the drain via
//! `is_path_dependent` instead.
#![allow(clippy::needless_range_loop)]

use super::macros::hist_indicator;
use super::{pback, pbars};
use crate::math::{Columns, ema, sma, smooth_defined};
use vike_model::Bar;

// ============================ shared local helpers =========================

/// Chaikin Line Value per bar — port of `volume.py:_clv_series`:
/// `((close-low)-(high-close))/(high-low)`, `0.0` when the bar has no range.
fn clv_series(h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 0..n {
        let rng = h[i] - l[i];
        if rng != 0.0 {
            out[i] = ((c[i] - l[i]) - (h[i] - c[i])) / rng;
        }
    }
    out
}

// ============================ batch kernels ================================

fn batch_ad(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let clv = clv_series(&x.h, &x.l, &x.c);
    let n = x.c.len();
    let mut out = vec![0.0; n];
    if n > 0 {
        out[0] = clv[0] * x.v[0];
        for i in 1..n {
            out[i] = out[i - 1] + clv[i] * x.v[i];
        }
    }
    vec![out]
}

fn batch_adosc(bars: &[Bar], fast: usize, slow: usize) -> Vec<Vec<f64>> {
    let ad_line = batch_ad(bars).swap_remove(0);
    let ema_fast = ema(&ad_line, fast);
    let ema_slow = ema(&ad_line, slow);
    let n = ad_line.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !ema_fast[i].is_nan() && !ema_slow[i].is_nan() {
            out[i] = ema_fast[i] - ema_slow[i];
        }
    }
    vec![out]
}

/// Chaikin Money Flow — TWO window folds (`Σ clv*volume` and `Σ volume`) per bar.
///
/// ⚠ **This carried a `run_clvv` / `run_v` pair**, so re-running the kernel over a truncated tail
/// returned different bits for the identical bars: MEASURED 64/64 and 232/232 mismatching retained
/// lengths at `period` 20 and 200, and 0 of each once the window is folded.
/// `crates/vike-indicators/src/math.rs`'s `sma`
/// carries the argument for the whole family. `clv_series` is per-bar arithmetic with no history
/// of its own, which is what makes `cmf`'s reach exactly `period` bars.
fn batch_cmf(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let clv = clv_series(&x.h, &x.l, &x.c);
    let n = x.c.len();
    let mut out = vec![f64::NAN; n];
    if period == 0 || n < period {
        return vec![out];
    }
    for i in (period - 1)..n {
        let (lo, hi) = (i + 1 - period, i);
        let run_clvv: f64 = (lo..=hi).map(|j| clv[j] * x.v[j]).sum();
        let run_v: f64 = x.v[lo..=hi].iter().sum();
        out[i] = if run_v != 0.0 { run_clvv / run_v } else { f64::NAN };
    }
    vec![out]
}

fn batch_efi(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    // Force values are only defined from index 1 onward (no prior close at i=0);
    // EMA the defined tail so the seed is not polluted by a synthetic raw[0].
    let mut raw = vec![f64::NAN; n];
    for i in 1..n {
        raw[i] = (x.c[i] - x.c[i - 1]) * x.v[i];
    }
    vec![smooth_defined(&raw, ema, period)]
}

fn batch_eom(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.h.len();
    let mut raw = vec![f64::NAN; n];
    for i in 1..n {
        let mid_move = ((x.h[i] + x.l[i]) / 2.0) - ((x.h[i - 1] + x.l[i - 1]) / 2.0);
        let hl = x.h[i] - x.l[i];
        if hl != 0.0 && x.v[i] != 0.0 {
            let box_ratio = x.v[i] / hl;
            raw[i] = mid_move / box_ratio;
        } else {
            raw[i] = 0.0;
        }
    }
    vec![smooth_defined(&raw, sma, period)]
}

fn batch_kvo(bars: &[Bar], fast: usize, slow: usize, signal: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();

    // Step 1: per-bar volume force (defined from index 1 onward).
    let mut vf_raw = vec![f64::NAN; n];
    for i in 1..n {
        let hlc3_cur = (x.h[i] + x.l[i] + x.c[i]) / 3.0;
        let hlc3_prev = (x.h[i - 1] + x.l[i - 1] + x.c[i - 1]) / 3.0;
        let t = if hlc3_cur > hlc3_prev { 1.0 } else { -1.0 };
        vf_raw[i] = x.v[i] * t;
    }

    // Step 2: EMA of vf over the defined tail (smooth_defined == the Python
    // "compact defined, ema, scatter back" form). The slow EMA being NaN below
    // its warm-up subsumes Python's `len(defined) >= slow` gate on both EMAs.
    let ema_fast = smooth_defined(&vf_raw, ema, fast);
    let ema_slow = smooth_defined(&vf_raw, ema, slow);

    // Step 3: kvo = fast EMA - slow EMA.
    let mut kvo_raw = vec![f64::NAN; n];
    for i in 0..n {
        if !ema_fast[i].is_nan() && !ema_slow[i].is_nan() {
            kvo_raw[i] = ema_fast[i] - ema_slow[i];
        }
    }

    // Step 4: signal = EMA(kvo, signal) over the defined kvo tail.
    let signal_full = smooth_defined(&kvo_raw, ema, signal);

    vec![kvo_raw, signal_full]
}

fn batch_mfi(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let mut out = vec![f64::NAN; n];
    let tp: Vec<f64> = (0..n).map(|i| (x.h[i] + x.l[i] + x.c[i]) / 3.0).collect();
    let rmf: Vec<f64> = (0..n).map(|i| tp[i] * x.v[i]).collect();
    for i in period..n {
        let (mut pos, mut neg) = (0.0, 0.0);
        for j in (i + 1 - period)..=i {
            if tp[j] > tp[j - 1] {
                pos += rmf[j];
            } else if tp[j] < tp[j - 1] {
                neg += rmf[j];
            }
        }
        out[i] = if neg == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + pos / neg) };
    }
    vec![out]
}

fn batch_net_volume(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        if x.c[i] > x.c[i - 1] {
            out[i] = x.v[i];
        } else if x.c[i] < x.c[i - 1] {
            out[i] = -x.v[i];
        }
    }
    vec![out]
}

fn batch_nvi(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let mut out = vec![1000.0; n];
    for i in 1..n {
        if x.v[i] < x.v[i - 1] {
            let rocp = if x.c[i - 1] != 0.0 { (x.c[i] - x.c[i - 1]) / x.c[i - 1] } else { 0.0 };
            out[i] = out[i - 1] * (1.0 + rocp);
        } else {
            out[i] = out[i - 1];
        }
    }
    vec![out]
}

fn batch_pvi(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let mut out = vec![1000.0; n];
    for i in 1..n {
        if x.v[i] > x.v[i - 1] {
            let rocp = if x.c[i - 1] != 0.0 { (x.c[i] - x.c[i - 1]) / x.c[i - 1] } else { 0.0 };
            out[i] = out[i - 1] * (1.0 + rocp);
        } else {
            out[i] = out[i - 1];
        }
    }
    vec![out]
}

fn batch_pvt(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let rocp = if x.c[i - 1] != 0.0 { (x.c[i] - x.c[i - 1]) / x.c[i - 1] } else { 0.0 };
        out[i] = out[i - 1] + x.v[i] * rocp;
    }
    vec![out]
}

fn batch_volume_osc(bars: &[Bar], short: usize, long: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).v;
    let ema_short = ema(&v, short);
    let ema_long = ema(&v, long);
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !ema_short[i].is_nan() && !ema_long[i].is_nan() && ema_long[i] != 0.0 {
            out[i] = (ema_short[i] - ema_long[i]) / ema_long[i] * 100.0;
        }
    }
    vec![out]
}

// ======================= streaming structs (macro) =========================

hist_indicator! {
    /// `ad` — Chaikin Accumulation/Distribution line.
    Ad, "ad", 1, [],
    |bars| batch_ad(bars),
    lookback = 0
}
hist_indicator! {
    /// `adosc` — Chaikin A/D oscillator `EMA(ad, fast) - EMA(ad, slow)`.
    Adosc, "adosc", 1, [fast = 3.0, slow = 10.0],
    |bars| batch_adosc(bars, fast.round() as usize, slow.round() as usize),
    lookback = pback(fast.max(slow), 1)
}
hist_indicator! {
    /// `cmf` — Chaikin Money Flow.
    Cmf, "cmf", 1, [period = 20.0],
    |bars| batch_cmf(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `efi` — Elder Force Index.
    Efi, "efi", 1, [period = 13.0],
    |bars| batch_efi(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `eom` — Ease of Movement.
    Eom, "eom", 1, [period = 14.0],
    |bars| batch_eom(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `kvo` — Klinger Volume Oscillator (kvo, signal).
    Kvo, "kvo", 2, [fast = 34.0, slow = 55.0, signal = 13.0],
    |bars| batch_kvo(bars, fast.round() as usize, slow.round() as usize, signal.round() as usize),
    // The signal line is a `signal`-bar EMA of the kvo line.
    lookback = pbars(fast.max(slow)), full = pbars(fast.max(slow)) + pback(signal, 1)
}
hist_indicator! {
    /// `mfi` — Money Flow Index (volume-weighted RSI).
    Mfi, "mfi", 1, [period = 14.0],
    |bars| batch_mfi(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `net_volume` — signed (non-cumulative) per-bar volume.
    NetVolume, "net_volume", 1, [],
    |bars| batch_net_volume(bars),
    lookback = 0
}
hist_indicator! {
    /// `nvi` — Negative Volume Index.
    Nvi, "nvi", 1, [],
    |bars| batch_nvi(bars),
    lookback = 0
}
hist_indicator! {
    /// `pvi` — Positive Volume Index.
    Pvi, "pvi", 1, [],
    |bars| batch_pvi(bars),
    lookback = 0
}
hist_indicator! {
    /// `pvt` — Price Volume Trend.
    Pvt, "pvt", 1, [],
    |bars| batch_pvt(bars),
    lookback = 0
}
hist_indicator! {
    /// `volume_osc` — Volume Oscillator (% EMA spread).
    VolumeOsc, "volume_osc", 1, [short = 5.0, long = 10.0],
    |bars| batch_volume_osc(bars, short.round() as usize, long.round() as usize),
    lookback = pback(short.max(long), 1)
}

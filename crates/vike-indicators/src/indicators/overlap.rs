//! Overlap / trend indicators — faithful f64 port of vike-trader-app
//! `core/indicators/overlap.py`. Skips names already in `base.rs`
//! (sma/ema/wma/psar). Each `batch_*` is a line-for-line port of the Python
//! function (Python `None` → `f64::NAN`); streaming is history-recompute via
//! [`hist_indicator!`] (correct-by-construction for these causal indicators).
//! `ichimoku` reads future bars (forward-shifted senkou + future chikou) so it
//! is `batch_only` in the registry — its `on_bar` fold is a causal best-effort.
#![allow(clippy::needless_range_loop)]

use super::macros::hist_indicator;
use super::{pback, pbars};
use crate::math::{Columns, atr_v, ema, rma, sma, smooth_defined, sq, wma};
use vike_model::Bar;

// ============================ batch kernels ================================

fn batch_dema(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let e1 = ema(&c, period);
    let e2 = smooth_defined(&e1, ema, period);
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !e1[i].is_nan() && !e2[i].is_nan() {
            out[i] = 2.0 * e1[i] - e2[i];
        }
    }
    vec![out]
}

fn batch_tema(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let e1 = ema(&c, period);
    let e2 = smooth_defined(&e1, ema, period);
    let e3 = smooth_defined(&e2, ema, period);
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !e1[i].is_nan() && !e2[i].is_nan() && !e3[i].is_nan() {
            out[i] = 3.0 * e1[i] - 3.0 * e2[i] + e3[i];
        }
    }
    vec![out]
}

fn batch_trima(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    // p1 = ceil(period/2), p2 = floor(period/2) + 1
    let p1 = period.div_ceil(2);
    let p2 = period / 2 + 1;
    let inner = sma(&c, p1);
    vec![smooth_defined(&inner, sma, p2)]
}

fn batch_smma(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    // Wilder/RMA smoothed MA seeded by SMA(period) — `math::rma` is bit-identical
    // to the Python `smma` recurrence.
    let c = Columns::from_bars(bars).c;
    vec![rma(&c, period)]
}

fn batch_zlema(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let lag = (period - 1) / 2;
    let mut delagged = vec![0.0; n];
    for i in 0..n {
        if i >= lag {
            delagged[i] = c[i] + (c[i] - c[i - lag]);
        } else {
            delagged[i] = c[i];
        }
    }
    vec![ema(&delagged, period)]
}

fn batch_hma(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let half = (period / 2).max(2);
    let sqrt_p = ((period as f64).sqrt() as usize).max(2);
    let w_half = wma(&c, half);
    let w_full = wma(&c, period);
    let mut combined = vec![0.0; n];
    for i in 0..n {
        combined[i] = if !w_half[i].is_nan() && !w_full[i].is_nan() {
            2.0 * w_half[i] - w_full[i]
        } else {
            f64::NAN
        };
    }
    let safe: Vec<f64> = combined.iter().map(|v| if v.is_nan() { 0.0 } else { *v }).collect();
    let raw = wma(&safe, sqrt_p);
    let mut out = vec![f64::NAN; n];
    if n >= sqrt_p {
        for i in (sqrt_p - 1)..n {
            let window = &combined[i + 1 - sqrt_p..=i];
            if window.iter().all(|v| !v.is_nan()) {
                out[i] = raw[i];
            }
        }
    }
    vec![out]
}

/// Volume-weighted MA — TWO window folds (`Σ close*volume` and `Σ volume`) per bar.
///
/// ⚠ **This carried a `run_pv` / `run_v` pair.** A carried sum is not a function of its window, so
/// re-running the kernel over a truncated tail returned different bits for the identical bars:
/// MEASURED 52/64 and 230/232 mismatching retained lengths at `period` 20 and 200, and 0 of each
/// once the window is folded. `crates/vike-indicators/src/math.rs`'s `sma` carries the argument
/// for the whole family.
fn batch_vwma(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let mut out = vec![f64::NAN; n];
    if period == 0 || n < period {
        return vec![out];
    }
    for i in (period - 1)..n {
        let (lo, hi) = (i + 1 - period, i);
        let run_pv: f64 = (lo..=hi).map(|j| x.c[j] * x.v[j]).sum();
        let run_v: f64 = x.v[lo..=hi].iter().sum();
        out[i] = if run_v != 0.0 { run_pv / run_v } else { f64::NAN };
    }
    vec![out]
}

fn batch_t3(bars: &[Bar], period: usize, v: f64) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    // one GD pass: (1+v)*EMA - v*EMA(EMA)
    let gd = |series: &[f64]| -> Vec<f64> {
        let e1 = smooth_defined(series, ema, period);
        let e2 = smooth_defined(&e1, ema, period);
        let mut result = vec![f64::NAN; series.len()];
        for i in 0..series.len() {
            if !e1[i].is_nan() && !e2[i].is_nan() {
                result[i] = (1.0 + v) * e1[i] - v * e2[i];
            }
        }
        result
    };
    let gd1 = gd(&c);
    let gd2 = gd(&gd1);
    let gd3 = gd(&gd2);
    vec![gd3]
}

fn batch_alma(bars: &[Bar], period: usize, offset: f64, sigma: f64) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    let m = offset * (period as f64 - 1.0);
    let s = period as f64 / sigma;
    let raw_weights: Vec<f64> =
        (0..period).map(|k| libm::exp(-sq(k as f64 - m) / (2.0 * s * s))).collect();
    let weight_sum: f64 = raw_weights.iter().sum();
    let weights: Vec<f64> = raw_weights.iter().map(|w| w / weight_sum).collect();
    if n >= period {
        for i in (period - 1)..n {
            let mut acc = 0.0;
            for k in 0..period {
                acc += weights[k] * c[i + 1 - period + k];
            }
            out[i] = acc;
        }
    }
    vec![out]
}

fn batch_midpoint(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    if n >= period {
        for i in (period - 1)..n {
            let w = &c[i + 1 - period..=i];
            let mx = w.iter().cloned().fold(f64::MIN, f64::max);
            let mn = w.iter().cloned().fold(f64::MAX, f64::min);
            out[i] = (mx + mn) / 2.0;
        }
    }
    vec![out]
}

fn batch_midprice(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.h.len();
    let mut out = vec![f64::NAN; n];
    if n >= period {
        for i in (period - 1)..n {
            let hh = x.h[i + 1 - period..=i].iter().cloned().fold(f64::MIN, f64::max);
            let ll = x.l[i + 1 - period..=i].iter().cloned().fold(f64::MAX, f64::min);
            out[i] = (hh + ll) / 2.0;
        }
    }
    vec![out]
}

fn batch_supertrend(bars: &[Bar], period: usize, mult: f64) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let mut st = vec![f64::NAN; n];
    let mut direction = vec![f64::NAN; n];
    let atr_vals = atr_v(&x.h, &x.l, &x.c, period);

    let start = match (0..n).find(|&i| !atr_vals[i].is_nan()) {
        Some(s) => s,
        None => return vec![st, direction],
    };

    let hl2_seed = (x.h[start] + x.l[start]) / 2.0;
    let mut final_upper = hl2_seed + mult * atr_vals[start];
    let mut final_lower = hl2_seed - mult * atr_vals[start];
    let mut curr_dir: f64 = if x.c[start] >= hl2_seed { 1.0 } else { -1.0 };
    st[start] = if curr_dir == 1.0 { final_lower } else { final_upper };
    direction[start] = curr_dir;

    for i in (start + 1)..n {
        if atr_vals[i].is_nan() {
            continue;
        }
        let hl2 = (x.h[i] + x.l[i]) / 2.0;
        let basic_upper = hl2 + mult * atr_vals[i];
        let basic_lower = hl2 - mult * atr_vals[i];

        // Carry rule for upper band: tighten only (closes/final_* always defined here).
        if x.c[i - 1] <= final_upper {
            final_upper = basic_upper.min(final_upper);
        } else {
            final_upper = basic_upper;
        }
        // Carry rule for lower band: ratchet only up.
        if x.c[i - 1] >= final_lower {
            final_lower = basic_lower.max(final_lower);
        } else {
            final_lower = basic_lower;
        }

        if curr_dir == 1.0 {
            if x.c[i] < final_lower {
                curr_dir = -1.0;
            }
        } else if x.c[i] > final_upper {
            curr_dir = 1.0;
        }

        st[i] = if curr_dir == 1.0 { final_lower } else { final_upper };
        direction[i] = curr_dir;
    }

    vec![st, direction]
}

fn batch_ichimoku(bars: &[Bar], tenkan: usize, kijun: usize, senkou: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();

    let donchian_mid = |period: usize| -> Vec<f64> {
        let mut out = vec![f64::NAN; n];
        if n >= period {
            for i in (period - 1)..n {
                let hh = x.h[i + 1 - period..=i].iter().cloned().fold(f64::MIN, f64::max);
                let ll = x.l[i + 1 - period..=i].iter().cloned().fold(f64::MAX, f64::min);
                out[i] = (hh + ll) / 2.0;
            }
        }
        out
    };

    let tenkan_line = donchian_mid(tenkan);
    let kijun_line = donchian_mid(kijun);
    let senkou_b_raw = donchian_mid(senkou);

    // senkou_a raw: average of tenkan and kijun where both defined.
    let mut senkou_a_raw = vec![f64::NAN; n];
    for i in 0..n {
        if !tenkan_line[i].is_nan() && !kijun_line[i].is_nan() {
            senkou_a_raw[i] = (tenkan_line[i] + kijun_line[i]) / 2.0;
        }
    }

    // Forward-shift senkou_a / senkou_b by kijun bars (value at i lands at i+kijun).
    let mut senkou_a = vec![f64::NAN; n];
    let mut senkou_b = vec![f64::NAN; n];
    for i in 0..n {
        if !senkou_a_raw[i].is_nan() {
            let target = i + kijun;
            if target < n {
                senkou_a[target] = senkou_a_raw[i];
            }
        }
        if !senkou_b_raw[i].is_nan() {
            let target = i + kijun;
            if target < n {
                senkou_b[target] = senkou_b_raw[i];
            }
        }
    }

    // chikou: current close plotted kijun bars back → chikou[i] = closes[i+kijun].
    let mut chikou = vec![f64::NAN; n];
    for i in 0..n {
        let target = i + kijun;
        if target < n {
            chikou[i] = x.c[target];
        }
    }

    vec![tenkan_line, kijun_line, senkou_a, senkou_b, chikou]
}

fn batch_mcginley(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    if n == 0 {
        return vec![out];
    }
    let mut prev = c[0];
    out[0] = prev;
    for i in 1..n {
        let cv = c[i];
        if prev == 0.0 {
            prev = cv;
        } else {
            let ratio = cv / prev;
            prev += (cv - prev) / (period as f64 * crate::math::quart(ratio));
        }
        out[i] = prev;
    }
    vec![out]
}

fn batch_gmma(bars: &[Bar]) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let periods = [3usize, 5, 8, 10, 12, 15, 30, 35, 40, 45, 50, 60];
    periods.iter().map(|&p| ema(&c, p)).collect()
}

fn batch_envelopes(bars: &[Bar], period: usize, pct: f64) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mid = sma(&c, period);
    let (mut upper, mut lower) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    let factor = pct / 100.0;
    for i in 0..n {
        if !mid[i].is_nan() {
            upper[i] = mid[i] * (1.0 + factor);
            lower[i] = mid[i] * (1.0 - factor);
        }
    }
    vec![upper, mid, lower]
}

fn batch_alligator(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.h.len();
    let median: Vec<f64> = (0..n).map(|i| (x.h[i] + x.l[i]) / 2.0).collect();

    let jaw_raw = rma(&median, 13);
    let teeth_raw = rma(&median, 8);
    let lips_raw = rma(&median, 5);

    let forward_shift = |raw: &[f64], shift: usize| -> Vec<f64> {
        let mut out = vec![f64::NAN; n];
        for i in 0..n {
            if !raw[i].is_nan() {
                let target = i + shift;
                if target < n {
                    out[target] = raw[i];
                }
            }
        }
        out
    };

    let jaw = forward_shift(&jaw_raw, 8);
    let teeth = forward_shift(&teeth_raw, 5);
    let lips = forward_shift(&lips_raw, 3);

    vec![jaw, teeth, lips]
}

// ======================= streaming structs (macro) =========================

hist_indicator! {
    /// `dema` — double EMA `2*EMA - EMA(EMA)`.
    Dema, "dema", 1, [period = 20.0],
    |bars| batch_dema(bars, period.round() as usize),
    lookback = (2 * pbars(period)).saturating_sub(2)
}
hist_indicator! {
    /// `tema` — triple EMA `3*EMA - 3*EMA(EMA) + EMA(EMA(EMA))`.
    Tema, "tema", 1, [period = 20.0],
    |bars| batch_tema(bars, period.round() as usize),
    lookback = (3 * pbars(period)).saturating_sub(3)
}
hist_indicator! {
    /// `trima` — triangular MA (SMA of SMA).
    Trima, "trima", 1, [period = 20.0],
    |bars| batch_trima(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `smma` — Wilder/RMA smoothed MA.
    Smma, "smma", 1, [period = 14.0],
    |bars| batch_smma(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `zlema` — zero-lag EMA.
    Zlema, "zlema", 1, [period = 20.0],
    |bars| batch_zlema(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `hma` — Hull MA.
    Hma, "hma", 1, [period = 20.0],
    |bars| batch_hma(bars, period.round() as usize),
    lookback = (pbars(period) + (pbars(period) as f64).sqrt().floor() as usize).saturating_sub(2)
}
hist_indicator! {
    /// `vwma` — volume-weighted MA.
    Vwma, "vwma", 1, [period = 20.0],
    |bars| batch_vwma(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `t3` — Tillson T3 (triple generalized DEMA).
    T3, "t3", 1, [period = 20.0, v = 0.7],
    |bars| batch_t3(bars, period.round() as usize, v),
    lookback = (6 * pbars(period)).saturating_sub(6)
}
hist_indicator! {
    /// `alma` — Arnaud Legoux MA (Gaussian-weighted window).
    Alma, "alma", 1, [period = 20.0, offset = 0.85, sigma = 6.0],
    |bars| batch_alma(bars, period.round() as usize, offset, sigma),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `midpoint` — `(max + min) / 2` of close over a rolling window.
    Midpoint, "midpoint", 1, [period = 14.0],
    |bars| batch_midpoint(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `midprice` — `(max(high) + min(low)) / 2` over a rolling window.
    Midprice, "midprice", 1, [period = 14.0],
    |bars| batch_midprice(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `supertrend` — ATR band trend follower (supertrend, direction).
    Supertrend, "supertrend", 2, [period = 10.0, mult = 3.0],
    |bars| batch_supertrend(bars, period.round() as usize, mult),
    lookback = pbars(period)
}
hist_indicator! {
    /// `ichimoku` — Ichimoku cloud (tenkan, kijun, senkou_a, senkou_b, chikou).
    Ichimoku, "ichimoku", 5, [tenkan = 9.0, kijun = 26.0, senkou = 52.0],
    |bars| batch_ichimoku(bars, tenkan.round() as usize, kijun.round() as usize, senkou.round() as usize)
}
hist_indicator! {
    /// `mcginley` — McGinley Dynamic.
    Mcginley, "mcginley", 1, [period = 14.0],
    |bars| batch_mcginley(bars, period.round() as usize),
    lookback = 0
}
hist_indicator! {
    /// `gmma` — Guppy Multiple MA (12 EMA lines).
    Gmma, "gmma", 12, [],
    |bars| batch_gmma(bars),
    // Shortest EMA is 3 (index 2); the longest of the twelve is 60 (index 59).
    lookback = 2, full = 59
}
hist_indicator! {
    /// `envelopes` — SMA ± pct% (upper, mid, lower).
    Envelopes, "envelopes", 3, [period = 20.0, pct = 2.5],
    |bars| batch_envelopes(bars, period.round() as usize, pct),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `alligator` — Williams Alligator (jaw, teeth, lips).
    Alligator, "alligator", 3, [],
    |bars| batch_alligator(bars),
    // lips(5)-1 + shift(3) = 7; jaw is (13)-1 + shift(8) = 20.
    lookback = 7, full = 20
}

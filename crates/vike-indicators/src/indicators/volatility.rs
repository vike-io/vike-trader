//! Volatility indicators — faithful f64 port of vike-trader-app
//! `core/indicators/volatility.py`. Skips names already in `base.rs`
//! (atr/bollinger/keltner/donchian). Streaming is history-recompute via
//! [`hist_indicator!`], and fold order is load-bearing for bit parity.
//!
//! ⚠ **This file's header used to read "running-sum std forms are reproduced EXACTLY (not a
//! two-pass window)".** They are not, any more: a running sum is not a function of its window, so
//! `stddev`/`hvol`/`relative_volatility` returned different bits once `hist_indicator!` truncated
//! history — and `run_sum2/p - mean*mean` is catastrophic cancellation on price data on top of
//! that. Both are now two-pass window folds; `stddev_series` carries the argument.
//!
//! # ⚠ A CONSTANT window is zero by DECISION here too, and this file learned it second
//!
//! Two-pass is necessary and NOT sufficient. `mean` is `Σx / n`, and `n` copies of `v` summed then
//! divided by `n` only rounds back to `v` for some values — `100.0` is one of them, `0.1` is not
//! (20 copies fold to `3fb999999999999b` against the value's `3fb999999999999a`). Every deviation
//! is then ~1.4e-17 rather than `0`, and the variance is rounding-sized rather than zero.
//! `crates/vike-indicators/src/window.rs`'s `rolling_var` was given
//! [`crate::window::is_constant_window`] for this; the three folds in THIS file do not route
//! through that module and so kept the defect. MEASURED on a 20-bar window flat at `0.1`:
//!
//! | indicator | before | after | at `100.0`, before and after |
//! |---|---|---|---|
//! | `stddev` | `1.3877787807814457e-17` | `0` | `0` |
//! | `bbands_width` | `5.551115123125782e-16` | `0` | `0` |
//! | `bbands_pctb` | `0.25` | `NaN` | `NaN` |
//! | `hvol` | `6.628369094757308e-15` (constant LOG-return window) | `0` | `0` |
//!
//! `bbands_pctb` is the one that matters most: it is `(price - lower) / (upper - lower)`, a tiny
//! numerator over a tiny denominator, so it returned an arbitrary FINITE number — `0.25`, which
//! looks like a perfectly ordinary mid-band reading — where there is no band and no information.
//! The `100.0` column is why none of this was caught: `tests/parity.rs`'s flat-window fixture used
//! the one value that cannot fail, and now uses `0.1` with
//! `the_constant_window_fixture_actually_bites` asserting the substitution cannot be undone.
//!
//! ⚠ The cure is an EXACT equality over the window's INPUTS, never an epsilon on the OUTPUT. A
//! threshold is a second law with a number in it and would swallow a genuinely small-but-real
//! variance — `a_variance_that_is_small_but_real_still_yields_a_nonzero_band` is the gate.
#![allow(clippy::needless_range_loop)]

use super::macros::hist_indicator;
use super::{pback, pbars};
use crate::math::{Columns, atr_v, ema, sma, smooth_defined, sq, true_range};
use crate::window::is_constant_window;
use vike_model::Bar;

// ============================ shared local helpers =========================

/// Rolling population stddev — a TWO-PASS window fold (`mean`, then `Σ(x - mean)²`), the same
/// shape as [`bollinger_vals`] below. Reused by relative_volatility.
///
/// ⚠ **This carried a `run_sum` / `run_sum2` pair and closed with `run_sum2/p - mean*mean`.**
/// `crates/vike-indicators/src/indicators/statistics.rs`'s `batch_var` carries the full argument
/// for why both halves had to go (truncation-dependence, then catastrophic cancellation that
/// FLIPS the zero-variance guards). MEASURED here: 64/64 and 231/232 mismatching retained lengths
/// at `period` 20 and 200 before, 0 of each after.
///
/// A window whose closes are all bit-equal returns exactly `0.0` without folding — see the module
/// doc's "A CONSTANT window is zero by DECISION here too" table for the measurement, and
/// [`crate::window::is_constant_window`] for why the predicate reads the INPUTS rather than
/// thresholding the output. Every other window is byte-identical to what this returned before.
fn stddev_series(v: &[f64], period: usize) -> Vec<f64> {
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    if period == 0 || n < period {
        return out;
    }
    for i in (period - 1)..n {
        let window = &v[i + 1 - period..=i];
        if is_constant_window(window) {
            out[i] = 0.0;
            continue;
        }
        let mean = window.iter().sum::<f64>() / period as f64;
        let var = window.iter().map(|x| sq(x - mean)).sum::<f64>() / period as f64;
        out[i] = var.sqrt();
    }
    out
}

/// Bollinger bands over a plain value series — port of `volatility.py:bollinger`
/// (two-pass window var, matches the base Bollinger math). Returns (upper, mid, lower).
///
/// ⚠ **Its variance half was always two-pass; the `mid` was the accumulator, and that alone is
/// what made `bbands_pctb`/`bbands_width` non-truncation-invariant.** `math::sma` folding its
/// window fixes both rows without an edit here — which is worth stating, because the ratchet's own
/// comment used to credit this function's two-pass variance as if the bug were elsewhere.
///
/// ⚠ **Two-pass was still not enough, and this is the function where it cost the most.** On a
/// constant window `m` is `Σx / period`, which does not round back to the repeated value for every
/// value; each `x - m` is then ~1.4e-17 rather than `0`, so `upper` and `lower` straddle `m` by a
/// rounding artifact instead of coinciding with it. `bbands_pctb` divides by exactly that gap and
/// returned `0.25` on a dead-flat 20-bar window at `0.1` — a plausible-looking mid-band reading
/// where the correct answer is NaN, because a zero-width band carries no information about where
/// price sits inside it. `sd` is now exactly `0.0` on such a window, which makes
/// `upper == lower == m` bit-exactly and lets `batch_bbands_pctb`'s `bw != 0.0` guard decline. See
/// the module doc for the full before/after table.
fn bollinger_vals(v: &[f64], period: usize, k: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mid = sma(v, period);
    let n = v.len();
    let (mut upper, mut lower) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    if n >= period {
        for i in (period - 1)..n {
            let window = &v[i + 1 - period..=i];
            let m = mid[i];
            // `k * 0.0` is `±0.0`, and `m ± 0.0` is `m` for every finite `m` and every sign of
            // `k` — so a constant window collapses both bands onto the mid line exactly, which is
            // what `bbands_width`'s `0.0` and `bbands_pctb`'s NaN both key on.
            let sd = if is_constant_window(window) {
                0.0
            } else {
                let var = window.iter().map(|x| sq(x - m)).sum::<f64>() / period as f64;
                var.sqrt()
            };
            upper[i] = m + k * sd;
            lower[i] = m - k * sd;
        }
    }
    (upper, mid, lower)
}

/// Donchian channel over high/low — port of `volatility.py:donchian`. (upper, mid, lower).
fn donchian_vals(h: &[f64], l: &[f64], period: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n = h.len();
    let (mut upper, mut mid, mut lower) = (vec![f64::NAN; n], vec![f64::NAN; n], vec![f64::NAN; n]);
    if n >= period {
        for i in (period - 1)..n {
            let hh = h[i + 1 - period..=i].iter().cloned().fold(f64::MIN, f64::max);
            let ll = l[i + 1 - period..=i].iter().cloned().fold(f64::MAX, f64::min);
            upper[i] = hh;
            lower[i] = ll;
            mid[i] = (hh + ll) / 2.0;
        }
    }
    (upper, mid, lower)
}

// ============================ batch kernels ================================

fn batch_true_range(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    vec![true_range(&x.h, &x.l, &x.c)]
}

fn batch_natr(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let atr_vals = atr_v(&x.h, &x.l, &x.c, period);
    let n = x.c.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !atr_vals[i].is_nan() && x.c[i] != 0.0 {
            out[i] = 100.0 * atr_vals[i] / x.c[i];
        }
    }
    vec![out]
}

fn batch_stddev(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    vec![stddev_series(&c, period)]
}

fn batch_hvol(bars: &[Bar], period: usize, ann: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let n = v.len();
    let mut log_rets = vec![f64::NAN; n];
    for i in 1..n {
        if v[i] > 0.0 && v[i - 1] > 0.0 {
            log_rets[i] = libm::log(v[i] / v[i - 1]);
        }
    }
    let mut out = vec![f64::NAN; n];
    if period == 0 {
        return vec![out];
    }
    // The DEFINED log returns, in order. Same two-pass window fold as `stddev_series` — see its
    // doc for why the `run_sum`/`run_sum2` pair had to go. Keeping the whole prefix rather than
    // trimming to `period` EXCHANGES the O(period) `buf.remove(0)` memmove for an O(period) float
    // fold — same complexity, worse constant (measured 1.4x at p=20, 7.8x at p=200), and `buf` grows
    // from O(period) to O(n): a `vectorize` over a 300k-bar chart now holds ~2.4 MB beside
    // `log_rets`. It is the price of truncation invariance here, not a saving.
    //
    // ⚠ This compaction is why `hvol` is deliberately NOT `WindowReach::Finite`
    // (`crates/vike-indicators/src/indicators/mod.rs`'s `window_reach` says so): `ln(v[i]/v[i-1])`
    // is undefined wherever a close is `<= 0.0`, so the `period` DEFINED values behind a bar can
    // span arbitrarily many bars. That is a reach the warm-up depth cannot express.
    //
    // ⚠ The constant-window predicate applies here over the LOG RETURNS, not over the closes, and
    // that is a genuinely different window from the other two folds in this file. A dead-flat price
    // window was never the failing case for `hvol`: `ln(v/v)` is `0.0` exactly, and `0.0` is one of
    // the values whose naive mean rounds back, so it already answered `0`. The case that failed is
    // a window of identical NON-ZERO log returns — a price series compounding at a fixed rate —
    // where the mean misses the repeated value exactly as it does for `0.1`. MEASURED on 200 bars
    // of `px *= 1.01`, whose last 20 log returns are bit-identical at `0.009950330853168092`:
    // `hvol` returned `6.628369094757308e-15` where the true annualised volatility of a perfectly
    // constant growth rate is `0`.
    let mut buf: Vec<f64> = Vec::new();
    for i in 0..n {
        if !log_rets[i].is_nan() {
            buf.push(log_rets[i]);
            if buf.len() >= period {
                let window = &buf[buf.len() - period..];
                let sd = if is_constant_window(window) {
                    0.0
                } else {
                    let mean = window.iter().sum::<f64>() / period as f64;
                    let var = window.iter().map(|x| sq(x - mean)).sum::<f64>() / period as f64;
                    var.sqrt()
                };
                out[i] = sd * (ann as f64).sqrt() * 100.0;
            }
        }
    }
    vec![out]
}

fn batch_bbands_pctb(bars: &[Bar], period: usize, k: f64) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let (upper, _mid, lower) = bollinger_vals(&v, period, k);
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !upper[i].is_nan() {
            let bw = upper[i] - lower[i];
            if bw != 0.0 {
                out[i] = (v[i] - lower[i]) / bw;
            }
        }
    }
    vec![out]
}

fn batch_bbands_width(bars: &[Bar], period: usize, k: f64) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let (upper, mid, lower) = bollinger_vals(&v, period, k);
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !mid[i].is_nan() && mid[i] != 0.0 {
            out[i] = (upper[i] - lower[i]) / mid[i];
        }
    }
    vec![out]
}

fn batch_donchian_width(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (upper, _mid, lower) = donchian_vals(&x.h, &x.l, period);
    let n = x.h.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !upper[i].is_nan() {
            out[i] = upper[i] - lower[i];
        }
    }
    vec![out]
}

fn batch_ulcer(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    if n >= period {
        for i in (period - 1)..n {
            let window = &v[i + 1 - period..=i];
            let peak = window.iter().cloned().fold(f64::MIN, f64::max);
            let sq_sum = if peak != 0.0 {
                window.iter().map(|c| sq((c - peak) / peak * 100.0)).sum::<f64>()
            } else {
                0.0
            };
            out[i] = (sq_sum / period as f64).sqrt();
        }
    }
    vec![out]
}

fn batch_chop(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let trs = true_range(&x.h, &x.l, &x.c);
    let n = x.c.len();
    let mut out = vec![f64::NAN; n];
    let log_p = libm::log10(period as f64);
    if n >= period {
        for i in (period - 1)..n {
            let sum_tr: f64 = trs[i + 1 - period..=i].iter().sum();
            let hh = x.h[i + 1 - period..=i].iter().cloned().fold(f64::MIN, f64::max);
            let ll = x.l[i + 1 - period..=i].iter().cloned().fold(f64::MAX, f64::min);
            let rng = hh - ll;
            if rng > 0.0 && sum_tr > 0.0 {
                out[i] = 100.0 * libm::log10(sum_tr / rng) / log_p;
            }
        }
    }
    vec![out]
}

fn batch_relative_volatility(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let v = Columns::from_bars(bars).c;
    let n = v.len();
    let sd_series = stddev_series(&v, period);
    let (mut u_raw, mut d_raw) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    for i in 1..n {
        let sd = sd_series[i];
        if sd.is_nan() {
            continue;
        }
        if v[i] > v[i - 1] {
            u_raw[i] = sd;
            d_raw[i] = 0.0;
        } else if v[i] < v[i - 1] {
            u_raw[i] = 0.0;
            d_raw[i] = sd;
        } else {
            u_raw[i] = 0.0;
            d_raw[i] = 0.0;
        }
    }
    let ema_u = smooth_defined(&u_raw, ema, period);
    let ema_d = smooth_defined(&d_raw, ema, period);
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !ema_u[i].is_nan() && !ema_d[i].is_nan() {
            let denom = ema_u[i] + ema_d[i];
            out[i] = if denom != 0.0 { 100.0 * ema_u[i] / denom } else { 50.0 };
        }
    }
    vec![out]
}

fn batch_high_low_52w(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.h.len();
    let (mut high_n, mut low_n) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    if n >= period {
        for i in (period - 1)..n {
            high_n[i] = x.h[i + 1 - period..=i].iter().cloned().fold(f64::MIN, f64::max);
            low_n[i] = x.l[i + 1 - period..=i].iter().cloned().fold(f64::MAX, f64::min);
        }
    }
    vec![high_n, low_n]
}

fn batch_mass(bars: &[Bar], period: usize, ema_period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.h.len();
    let hl: Vec<f64> = (0..n).map(|i| x.h[i] - x.l[i]).collect();
    let ema1 = ema(&hl, ema_period);
    let ema2 = smooth_defined(&ema1, ema, ema_period);
    let mut ratio = vec![f64::NAN; n];
    for i in 0..n {
        if !ema1[i].is_nan() && !ema2[i].is_nan() && ema2[i] != 0.0 {
            ratio[i] = ema1[i] / ema2[i];
        }
    }
    let mut out = vec![f64::NAN; n];
    if n >= period {
        for i in (period - 1)..n {
            let window = &ratio[i + 1 - period..=i];
            if window.iter().all(|v| !v.is_nan()) {
                out[i] = window.iter().sum();
            }
        }
    }
    vec![out]
}

// ======================= streaming structs (macro) =========================

hist_indicator! {
    /// `true_range` — gap-aware true range.
    TrueRange, "true_range", 1, [],
    |bars| batch_true_range(bars),
    lookback = 0
}
hist_indicator! {
    /// `natr` — normalized ATR (%).
    Natr, "natr", 1, [period = 14.0],
    |bars| batch_natr(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `stddev` — rolling population standard deviation.
    Stddev, "stddev", 1, [period = 20.0],
    |bars| batch_stddev(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `hvol` — annualized historical volatility (%).
    Hvol, "hvol", 1, [period = 20.0, ann = 365.0],
    |bars| batch_hvol(bars, period.round() as usize, ann.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `bbands_pctb` — Bollinger %B.
    BbandsPctb, "bbands_pctb", 1, [period = 20.0, k = 2.0],
    |bars| batch_bbands_pctb(bars, period.round() as usize, k),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `bbands_width` — Bollinger band width.
    BbandsWidth, "bbands_width", 1, [period = 20.0, k = 2.0],
    |bars| batch_bbands_width(bars, period.round() as usize, k),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `donchian_width` — Donchian channel width.
    DonchianWidth, "donchian_width", 1, [period = 20.0],
    |bars| batch_donchian_width(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `ulcer` — Ulcer Index.
    Ulcer, "ulcer", 1, [period = 14.0],
    |bars| batch_ulcer(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `chop` — Choppiness Index.
    Chop, "chop", 1, [period = 14.0],
    |bars| batch_chop(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `relative_volatility` — Relative Volatility Index.
    RelativeVolatility, "relative_volatility", 1, [period = 14.0],
    |bars| batch_relative_volatility(bars, period.round() as usize),
    lookback = (2 * pbars(period)).saturating_sub(2)
}
hist_indicator! {
    /// `high_low_52w` — rolling N-period high/low.
    HighLow52w, "high_low_52w", 2, [period = 252.0],
    |bars| batch_high_low_52w(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `mass` — Mass Index.
    Mass, "mass", 1, [period = 25.0, ema_period = 9.0],
    |bars| batch_mass(bars, period.round() as usize, ema_period.round() as usize),
    lookback = (pbars(period) + 2 * pbars(ema_period)).saturating_sub(3)
}

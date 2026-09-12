//! Momentum indicators — faithful f64 port of vike-trader-app
//! `core/indicators/momentum.py`. Skips the names already in `base.rs`
//! (rsi/macd/stochastic/cci/roc/ao→awesome/williams_r→williams). Each `batch_*`
//! is a line-for-line port of the Python function (Python `None` → `f64::NAN`);
//! streaming is history-recompute via [`hist_indicator!`] (correct-by-
//! construction for these causal indicators).
#![allow(clippy::needless_range_loop)]

use super::macros::hist_indicator;
use super::{pback, pbars};
use crate::math::{Columns, atr_v, ema, sma, smooth_defined, true_range, wma};
use vike_model::Bar;

// ============================ shared local helpers =========================

/// Wilder RSI over an arbitrary value series — port of `momentum.py:rsi`
/// (`None` warm-up while `len <= period`). Used by connors_rsi / stochrsi.
fn rsi_vals(values: &[f64], period: usize) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    if n <= period || period == 0 {
        return out;
    }
    let (mut gains, mut losses) = (0.0, 0.0);
    for i in 1..=period {
        let ch = values[i] - values[i - 1];
        gains += ch.max(0.0);
        losses += (-ch).max(0.0);
    }
    let (mut ag, mut al) = (gains / period as f64, losses / period as f64);
    out[period] = if al == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + ag / al) };
    for i in (period + 1)..n {
        let ch = values[i] - values[i - 1];
        ag = (ag * (period as f64 - 1.0) + ch.max(0.0)) / period as f64;
        al = (al * (period as f64 - 1.0) + (-ch).max(0.0)) / period as f64;
        out[i] = if al == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + ag / al) };
    }
    out
}

/// Percent ROC over an arbitrary series — port of `momentum.py:roc`
/// (`(v[i]/v[i-p] - 1) * 100`, `None` when `v[i-p] == 0`). Used by coppock.
fn roc_vals(values: &[f64], period: usize) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    for i in period..n {
        let prev = values[i - period];
        out[i] = if prev != 0.0 { (values[i] / prev - 1.0) * 100.0 } else { f64::NAN };
    }
    out
}

/// Awesome-oscillator series (SMA5−SMA34 of median) — the unregistered `ao`
/// helper `ac` builds on (registered `awesome` in base.rs is the same math).
fn ao_vals(bars: &[Bar]) -> Vec<f64> {
    let x = Columns::from_bars(bars);
    let n = x.h.len();
    let median: Vec<f64> = (0..n).map(|i| (x.h[i] + x.l[i]) / 2.0).collect();
    let s5 = sma(&median, 5);
    let s34 = sma(&median, 34);
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !s5[i].is_nan() && !s34[i].is_nan() {
            out[i] = s5[i] - s34[i];
        }
    }
    out
}

// ============================ batch kernels ================================

fn batch_mom(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    for i in period..n {
        out[i] = c[i] - c[i - period];
    }
    vec![out]
}

fn batch_rocp(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    for i in period..n {
        let prev = c[i - period];
        out[i] = if prev != 0.0 { (c[i] - prev) / prev } else { f64::NAN };
    }
    vec![out]
}

fn batch_rocr(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    for i in period..n {
        let prev = c[i - period];
        out[i] = if prev != 0.0 { c[i] / prev } else { f64::NAN };
    }
    vec![out]
}

fn batch_rocr100(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    for i in period..n {
        let prev = c[i - period];
        out[i] = if prev != 0.0 { c[i] / prev * 100.0 } else { f64::NAN };
    }
    vec![out]
}

fn batch_apo(bars: &[Bar], fast: usize, slow: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let (ef, es) = (ema(&c, fast), ema(&c, slow));
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !ef[i].is_nan() && !es[i].is_nan() {
            out[i] = ef[i] - es[i];
        }
    }
    vec![out]
}

fn batch_ppo(bars: &[Bar], fast: usize, slow: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let (ef, es) = (ema(&c, fast), ema(&c, slow));
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !ef[i].is_nan() && !es[i].is_nan() && es[i] != 0.0 {
            out[i] = (ef[i] - es[i]) / es[i] * 100.0;
        }
    }
    vec![out]
}

fn batch_cmo(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    for i in period..n {
        let (mut sum_up, mut sum_dn) = (0.0, 0.0);
        for j in (i - period + 1)..=i {
            let d = c[j] - c[j - 1];
            if d > 0.0 {
                sum_up += d;
            } else {
                sum_dn += -d;
            }
        }
        let denom = sum_up + sum_dn;
        out[i] = if denom != 0.0 { 100.0 * (sum_up - sum_dn) / denom } else { 0.0 };
    }
    vec![out]
}

fn batch_bop(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let mut out = vec![0.0; n];
    for i in 0..n {
        let rng = x.h[i] - x.l[i];
        out[i] = if rng != 0.0 { (x.c[i] - x.o[i]) / rng } else { 0.0 };
    }
    vec![out]
}

fn batch_dpo(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    let shift = period / 2 + 1;
    let ma = sma(&c, period);
    if n >= period {
        for i in (period - 1)..n {
            if i >= shift && !ma[i].is_nan() {
                out[i] = c[i - shift] - ma[i];
            }
        }
    }
    vec![out]
}

fn batch_trix(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let e1 = ema(&c, period);
    let e2 = smooth_defined(&e1, ema, period);
    let e3 = smooth_defined(&e2, ema, period);
    let mut out = vec![f64::NAN; n];
    let mut prev_idx: Option<usize> = None;
    for i in 0..n {
        if !e3[i].is_nan() {
            if let Some(p) = prev_idx
                && e3[p] != 0.0
            {
                out[i] = (e3[i] - e3[p]) / e3[p] * 100.0;
            }
            prev_idx = Some(i);
        }
    }
    vec![out]
}

fn batch_tsi(bars: &[Bar], long: usize, short: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut delta = vec![0.0; n];
    for i in 1..n {
        delta[i] = c[i] - c[i - 1];
    }
    let abs_delta: Vec<f64> = delta.iter().map(|d| d.abs()).collect();
    let ema1_d = ema(&delta, long);
    let ema1_a = ema(&abs_delta, long);
    let ema2_d = smooth_defined(&ema1_d, ema, short);
    let ema2_a = smooth_defined(&ema1_a, ema, short);
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !ema2_d[i].is_nan() && !ema2_a[i].is_nan() && ema2_a[i] != 0.0 {
            out[i] = 100.0 * ema2_d[i] / ema2_a[i];
        }
    }
    vec![out]
}

fn batch_smi_ergodic(bars: &[Bar], long: usize, short: usize, signal: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut delta = vec![0.0; n];
    for i in 1..n {
        delta[i] = c[i] - c[i - 1];
    }
    let abs_delta: Vec<f64> = delta.iter().map(|d| d.abs()).collect();
    let double_ema = |src: &[f64]| smooth_defined(&ema(src, long), ema, short);
    let ema2_d = double_ema(&delta);
    let ema2_a = double_ema(&abs_delta);
    let mut smi = vec![f64::NAN; n];
    for i in 0..n {
        if !ema2_d[i].is_nan() && !ema2_a[i].is_nan() && ema2_a[i] != 0.0 {
            smi[i] = 100.0 * ema2_d[i] / ema2_a[i];
        }
    }
    let sig = smooth_defined(&smi, ema, signal);
    vec![smi, sig]
}

fn batch_coppock(bars: &[Bar], wma_p: usize, roc_long: usize, roc_short: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let rl = roc_vals(&c, roc_long);
    let rs = roc_vals(&c, roc_short);
    let mut combined = vec![f64::NAN; n];
    for i in 0..n {
        if !rl[i].is_nan() && !rs[i].is_nan() {
            combined[i] = rl[i] + rs[i];
        }
    }
    vec![smooth_defined(&combined, wma, wma_p)]
}

#[allow(clippy::too_many_arguments)] // 9 periods, faithful to momentum.py:kst
fn batch_kst(
    bars: &[Bar],
    roc1: usize,
    sma1: usize,
    roc2: usize,
    sma2: usize,
    roc3: usize,
    sma3: usize,
    roc4: usize,
    sma4: usize,
    signal: usize,
) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let roc_sma = |period_r: usize, period_s: usize| -> Vec<f64> {
        let mut rocv = vec![f64::NAN; n];
        for i in period_r..n {
            let prev = c[i - period_r];
            rocv[i] = if prev != 0.0 { (c[i] - prev) / prev * 100.0 } else { f64::NAN };
        }
        smooth_defined(&rocv, sma, period_s)
    };
    let r1 = roc_sma(roc1, sma1);
    let r2 = roc_sma(roc2, sma2);
    let r3 = roc_sma(roc3, sma3);
    let r4 = roc_sma(roc4, sma4);
    let mut kst_line = vec![f64::NAN; n];
    for i in 0..n {
        if !r1[i].is_nan() && !r2[i].is_nan() && !r3[i].is_nan() && !r4[i].is_nan() {
            kst_line[i] = 1.0 * r1[i] + 2.0 * r2[i] + 3.0 * r3[i] + 4.0 * r4[i];
        }
    }
    let sig = smooth_defined(&kst_line, sma, signal);
    vec![kst_line, sig]
}

fn batch_aroon(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.h.len();
    let (mut up, mut down) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    for i in period..n {
        let wh = &x.h[i - period..=i];
        let wl = &x.l[i - period..=i];
        let max_h = wh.iter().cloned().fold(f64::MIN, f64::max);
        let min_l = wl.iter().cloned().fold(f64::MAX, f64::min);
        // bars since the most recent (from the right) extreme match
        let since_hh = (0..=period).find(|&k| wh[period - k] == max_h).unwrap();
        let since_ll = (0..=period).find(|&k| wl[period - k] == min_l).unwrap();
        up[i] = 100.0 * (period - since_hh) as f64 / period as f64;
        down[i] = 100.0 * (period - since_ll) as f64 / period as f64;
    }
    vec![up, down]
}

fn batch_aroonosc(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let a = batch_aroon(bars, period);
    let (up, down) = (&a[0], &a[1]);
    let n = up.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !up[i].is_nan() && !down[i].is_nan() {
            out[i] = up[i] - down[i];
        }
    }
    vec![out]
}

fn batch_adx(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let (mut plus_di, mut minus_di, mut adx_line) =
        (vec![f64::NAN; n], vec![f64::NAN; n], vec![f64::NAN; n]);
    if n <= period || period == 0 {
        return vec![adx_line, plus_di, minus_di];
    }
    let (mut tr, mut pdm, mut mdm) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    for i in 1..n {
        let up = x.h[i] - x.h[i - 1];
        let dn = x.l[i - 1] - x.l[i];
        pdm[i] = if up > dn && up > 0.0 { up } else { 0.0 };
        mdm[i] = if dn > up && dn > 0.0 { dn } else { 0.0 };
        tr[i] = (x.h[i] - x.l[i]).max((x.h[i] - x.c[i - 1]).abs()).max((x.l[i] - x.c[i - 1]).abs());
    }
    let pf = period as f64;
    let mut atr_s = tr[1..=period].iter().sum::<f64>();
    let mut pdm_s = pdm[1..=period].iter().sum::<f64>();
    let mut mdm_s = mdm[1..=period].iter().sum::<f64>();
    let mut dx_list: Vec<(usize, f64)> = Vec::new();
    for i in period..n {
        if i > period {
            atr_s += tr[i] - atr_s / pf;
            pdm_s += pdm[i] - pdm_s / pf;
            mdm_s += mdm[i] - mdm_s / pf;
        }
        let pdi = if atr_s > 0.0 { 100.0 * pdm_s / atr_s } else { 0.0 };
        let mdi = if atr_s > 0.0 { 100.0 * mdm_s / atr_s } else { 0.0 };
        plus_di[i] = pdi;
        minus_di[i] = mdi;
        let denom = pdi + mdi;
        dx_list.push((i, if denom > 0.0 { 100.0 * (pdi - mdi).abs() / denom } else { 0.0 }));
    }
    if dx_list.len() >= period {
        let mut prev = dx_list[..period].iter().map(|&(_, d)| d).sum::<f64>() / pf;
        adx_line[dx_list[period - 1].0] = prev;
        for k in period..dx_list.len() {
            let (i, dxv) = dx_list[k];
            prev = (prev * (pf - 1.0) + dxv) / pf;
            adx_line[i] = prev;
        }
    }
    vec![adx_line, plus_di, minus_di]
}

fn batch_adxr(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let adx_line = batch_adx(bars, period).swap_remove(0);
    let n = adx_line.len();
    let mut out = vec![f64::NAN; n];
    for i in period..n {
        if !adx_line[i].is_nan() && !adx_line[i - period].is_nan() {
            out[i] = (adx_line[i] + adx_line[i - period]) / 2.0;
        }
    }
    vec![out]
}

fn batch_elder_ray(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let ema_c = ema(&x.c, period);
    let (mut bull, mut bear) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    for i in 0..n {
        if !ema_c[i].is_nan() {
            bull[i] = x.h[i] - ema_c[i];
            bear[i] = x.l[i] - ema_c[i];
        }
    }
    vec![bull, bear]
}

fn batch_stochf(bars: &[Bar], k: usize, d: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let mut k_line = vec![f64::NAN; n];
    if n >= k {
        for i in (k - 1)..n {
            let hh = x.h[i + 1 - k..=i].iter().cloned().fold(f64::MIN, f64::max);
            let ll = x.l[i + 1 - k..=i].iter().cloned().fold(f64::MAX, f64::min);
            let rng = hh - ll;
            k_line[i] = if rng != 0.0 { 100.0 * (x.c[i] - ll) / rng } else { 0.0 };
        }
    }
    let mut d_line = smooth_defined(&k_line, sma, d);
    for i in 0..n {
        if i < (k - 1) + (d - 1) {
            d_line[i] = f64::NAN;
        }
    }
    vec![k_line, d_line]
}

fn batch_stochrsi(bars: &[Bar], rsi_p: usize, k: usize, d: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let rsi_series = rsi_vals(&c, rsi_p);
    let mut k_line = vec![f64::NAN; n];
    for i in 0..n {
        if i + 1 < k {
            continue;
        }
        let start = i + 1 - k;
        let window = &rsi_series[start..=i];
        if window.iter().any(|v| v.is_nan()) {
            continue;
        }
        let hh = window.iter().cloned().fold(f64::MIN, f64::max);
        let ll = window.iter().cloned().fold(f64::MAX, f64::min);
        let rng = hh - ll;
        k_line[i] = if rng != 0.0 { 100.0 * (rsi_series[i] - ll) / rng } else { 0.0 };
    }
    let d_line = smooth_defined(&k_line, sma, d);
    vec![k_line, d_line]
}

fn batch_ultosc(bars: &[Bar], p1: usize, p2: usize, p3: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let mut out = vec![f64::NAN; n];
    let (mut bp, mut tr) = (vec![0.0; n], vec![0.0; n]);
    for i in 1..n {
        let prev_c = x.c[i - 1];
        let true_low = x.l[i].min(prev_c);
        let true_high = x.h[i].max(prev_c);
        bp[i] = x.c[i] - true_low;
        tr[i] = true_high - true_low;
    }
    let largest = p1.max(p2).max(p3);
    let avg = |p: usize, idx: usize| -> f64 {
        let s_bp: f64 = bp[idx - p + 1..=idx].iter().sum();
        let s_tr: f64 = tr[idx - p + 1..=idx].iter().sum();
        if s_tr != 0.0 { s_bp / s_tr } else { 0.0 }
    };
    for i in largest..n {
        let a1 = avg(p1, i);
        let a2 = avg(p2, i);
        let a3 = avg(p3, i);
        out[i] = 100.0 * (4.0 * a1 + 2.0 * a2 + a3) / 7.0;
    }
    vec![out]
}

fn batch_vortex(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let trs = true_range(&x.h, &x.l, &x.c);
    let (mut vmp, mut vmm) = (vec![0.0; n], vec![0.0; n]);
    for i in 1..n {
        vmp[i] = (x.h[i] - x.l[i - 1]).abs();
        vmm[i] = (x.l[i] - x.h[i - 1]).abs();
    }
    let (mut vip, mut vim) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    for i in period..n {
        let sum_tr: f64 = trs[i - period + 1..=i].iter().sum();
        let sum_vp: f64 = vmp[i - period + 1..=i].iter().sum();
        let sum_vm: f64 = vmm[i - period + 1..=i].iter().sum();
        if sum_tr > 0.0 {
            vip[i] = sum_vp / sum_tr;
            vim[i] = sum_vm / sum_tr;
        }
    }
    vec![vip, vim]
}

fn batch_chande_kroll_stop(bars: &[Bar], p: usize, x_mult: usize, q: usize) -> Vec<Vec<f64>> {
    let cols = Columns::from_bars(bars);
    let n = cols.c.len();
    let atr_vals = atr_v(&cols.h, &cols.l, &cols.c, p);
    let (mut first_high, mut first_low) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    if n >= p {
        for i in (p - 1)..n {
            if atr_vals[i].is_nan() {
                continue;
            }
            let hh = cols.h[i + 1 - p..=i].iter().cloned().fold(f64::MIN, f64::max);
            let ll = cols.l[i + 1 - p..=i].iter().cloned().fold(f64::MAX, f64::min);
            first_high[i] = hh - x_mult as f64 * atr_vals[i];
            first_low[i] = ll + x_mult as f64 * atr_vals[i];
        }
    }
    let (mut long_stop, mut short_stop) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    if n >= q {
        for i in (q - 1)..n {
            let wh: Vec<f64> = (i + 1 - q..=i)
                .filter_map(|j| (!first_high[j].is_nan()).then_some(first_high[j]))
                .collect();
            let wl: Vec<f64> = (i + 1 - q..=i)
                .filter_map(|j| (!first_low[j].is_nan()).then_some(first_low[j]))
                .collect();
            if !wh.is_empty() {
                long_stop[i] = wh.iter().cloned().fold(f64::MIN, f64::max);
            }
            if !wl.is_empty() {
                short_stop[i] = wl.iter().cloned().fold(f64::MAX, f64::min);
            }
        }
    }
    vec![long_stop, short_stop]
}

fn batch_asi(bars: &[Bar], limit: f64) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let mut out = vec![f64::NAN; n];
    let mut cum = 0.0;
    for i in 1..n {
        let (c, cp) = (x.c[i], x.c[i - 1]);
        let (o, op) = (x.o[i], x.o[i - 1]);
        let (h, l) = (x.h[i], x.l[i]);
        let a = (h - cp).abs();
        let b = (l - cp).abs();
        let c2 = h - l;
        let r = a.max(b).max(c2);
        let k = a.max(b);
        let si = if r == 0.0 || limit == 0.0 {
            0.0
        } else {
            let t = r + 0.25 * (cp - op).abs();
            if t == 0.0 {
                0.0
            } else {
                let numerator = (c - cp) + 0.5 * (c - o) + 0.25 * (cp - op);
                50.0 * (numerator / t) * (k / limit)
            }
        };
        cum += si;
        out[i] = cum;
    }
    vec![out]
}

fn batch_fisher(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.h.len();
    let (mut fish, mut trig) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    if n < period || period == 0 {
        return vec![fish, trig];
    }
    let median: Vec<f64> = (0..n).map(|i| (x.h[i] + x.l[i]) / 2.0).collect();
    let (mut prev_value, mut prev_fish) = (0.0, 0.0);
    for i in (period - 1)..n {
        let hi = median[i + 1 - period..=i].iter().cloned().fold(f64::MIN, f64::max);
        let lo = median[i + 1 - period..=i].iter().cloned().fold(f64::MAX, f64::min);
        let rng = hi - lo;
        let norm = if rng == 0.0 { 0.0 } else { (median[i] - lo) / rng };
        let mut value = 0.66 * (2.0 * norm - 1.0) + 0.67 * prev_value;
        value = value.clamp(-0.999, 0.999);
        let f = 0.5 * libm::log((1.0 + value) / (1.0 - value)) + 0.5 * prev_fish;
        fish[i] = f;
        trig[i] = if i > period - 1 { prev_fish } else { f64::NAN };
        prev_value = value;
        prev_fish = f;
    }
    if period - 1 < n {
        trig[period - 1] = f64::NAN;
    }
    vec![fish, trig]
}

fn batch_connors_rsi(bars: &[Bar], rsi_p: usize, streak_p: usize, rank_p: usize) -> Vec<Vec<f64>> {
    let c = Columns::from_bars(bars).c;
    let n = c.len();
    let mut streak = vec![0.0; n];
    for i in 1..n {
        let diff = c[i] - c[i - 1];
        streak[i] = if diff > 0.0 {
            if streak[i - 1] > 0.0 { streak[i - 1] + 1.0 } else { 1.0 }
        } else if diff < 0.0 {
            if streak[i - 1] < 0.0 { streak[i - 1] - 1.0 } else { -1.0 }
        } else {
            0.0
        };
    }
    let rsi1 = rsi_vals(&c, rsi_p);
    let rsi2 = rsi_vals(&streak, streak_p);
    let mut roc1 = vec![f64::NAN; n];
    for i in 1..n {
        if c[i - 1] != 0.0 {
            roc1[i] = (c[i] / c[i - 1] - 1.0) * 100.0;
        }
    }
    let mut prank = vec![f64::NAN; n];
    for i in rank_p..n {
        let cur = roc1[i];
        if cur.is_nan() {
            continue;
        }
        // Count in place rather than materialising the window. The old form collected a
        // `Vec<f64>` of the finite values and then scanned it, so this loop allocated once per
        // BAR — and because `on_bar` re-runs the whole kernel, that was ~1,500 allocations per
        // bar at the default `rank_p` of 100. Both quantities below are integer COUNTS and the
        // arithmetic that consumes them is unchanged, so this cannot move a float bit; the
        // `on_bar == vectorize` parity gate re-proves that.
        let mut finite = 0usize;
        let mut count_below = 0usize;
        for j in (i + 1 - rank_p)..=i {
            let v = roc1[j];
            if v.is_nan() {
                continue;
            }
            finite += 1;
            if v < cur {
                count_below += 1;
            }
        }
        if finite == 0 {
            continue;
        }
        prank[i] = 100.0 * count_below as f64 / finite as f64;
    }
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !rsi1[i].is_nan() && !rsi2[i].is_nan() && !prank[i].is_nan() {
            out[i] = (rsi1[i] + rsi2[i] + prank[i]) / 3.0;
        }
    }
    vec![out]
}

/// Symmetric weighted MA [1,2,2,1]/6 over 4 bars — port of `momentum._swma`.
fn swma(values: &[f64]) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    for i in 3..n {
        out[i] = (values[i - 3] + 2.0 * values[i - 2] + 2.0 * values[i - 1] + values[i]) / 6.0;
    }
    out
}

fn batch_relative_vigor(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let n = x.c.len();
    let co: Vec<f64> = (0..n).map(|i| x.c[i] - x.o[i]).collect();
    let hl: Vec<f64> = (0..n).map(|i| x.h[i] - x.l[i]).collect();
    let num_sw = swma(&co);
    let den_sw = swma(&hl);
    let mut rvgi = vec![f64::NAN; n];
    if n > period - 1 + 3 {
        for i in (period - 1 + 3)..n {
            let (mut num_sum, mut den_sum, mut valid) = (0.0, 0.0, true);
            for kk in (i - period + 1)..=i {
                if num_sw[kk].is_nan() || den_sw[kk].is_nan() {
                    valid = false;
                    break;
                }
                num_sum += num_sw[kk];
                den_sum += den_sw[kk];
            }
            if valid && den_sum != 0.0 {
                rvgi[i] = num_sum / den_sum;
            }
        }
    }
    // signal = 4-bar SWMA of the defined rvgi tail, mapped back
    let defined: Vec<(usize, f64)> =
        rvgi.iter().enumerate().filter(|(_, v)| !v.is_nan()).map(|(i, v)| (i, *v)).collect();
    let mut sig = vec![f64::NAN; n];
    if defined.len() >= 4 {
        let raw = swma(&defined.iter().map(|(_, v)| *v).collect::<Vec<_>>());
        for ((i, _), sv) in defined.iter().zip(raw) {
            sig[*i] = sv;
        }
    }
    vec![rvgi, sig]
}

fn batch_ac(bars: &[Bar]) -> Vec<Vec<f64>> {
    let ao = ao_vals(bars);
    let n = ao.len();
    let sma5 = smooth_defined(&ao, sma, 5);
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !ao[i].is_nan() && !sma5[i].is_nan() {
            out[i] = ao[i] - sma5[i];
        }
    }
    vec![out]
}

// ======================= streaming structs (macro) =========================

hist_indicator! {
    /// `mom` — momentum `v[i] - v[i-period]`.
    Mom, "mom", 1, [period = 10.0],
    |bars| batch_mom(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `rocp` — rate-of-change proportion `(v[i]-v[i-p])/v[i-p]`.
    Rocp, "rocp", 1, [period = 10.0],
    |bars| batch_rocp(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `rocr` — rate-of-change ratio `v[i]/v[i-p]`.
    Rocr, "rocr", 1, [period = 10.0],
    |bars| batch_rocr(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `rocr100` — rate-of-change ratio ×100.
    Rocr100, "rocr100", 1, [period = 10.0],
    |bars| batch_rocr100(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `apo` — absolute price oscillator `EMA(fast) - EMA(slow)`.
    Apo, "apo", 1, [fast = 12.0, slow = 26.0],
    |bars| batch_apo(bars, fast.round() as usize, slow.round() as usize),
    lookback = pback(fast.max(slow), 1)
}
hist_indicator! {
    /// `ppo` — percentage price oscillator.
    Ppo, "ppo", 1, [fast = 12.0, slow = 26.0],
    |bars| batch_ppo(bars, fast.round() as usize, slow.round() as usize),
    lookback = pback(fast.max(slow), 1)
}
hist_indicator! {
    /// `cmo` — Chande momentum oscillator.
    Cmo, "cmo", 1, [period = 14.0],
    |bars| batch_cmo(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `bop` — balance of power `(C-O)/(H-L)`.
    Bop, "bop", 1, [],
    |bars| batch_bop(bars),
    lookback = 0
}
hist_indicator! {
    /// `dpo` — detrended price oscillator.
    Dpo, "dpo", 1, [period = 20.0],
    |bars| batch_dpo(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `trix` — 1-bar % ROC of triple-EMA.
    Trix, "trix", 1, [period = 18.0],
    |bars| batch_trix(bars, period.round() as usize),
    lookback = 3 * pback(period, 1) + 1
}
hist_indicator! {
    /// `tsi` — true strength index.
    Tsi, "tsi", 1, [long = 25.0, short = 13.0],
    |bars| batch_tsi(bars, long.round() as usize, short.round() as usize),
    lookback = (pbars(long) + pbars(short)).saturating_sub(2)
}
hist_indicator! {
    /// `smi_ergodic` — SMI ergodic (smi, signal).
    SmiErgodic, "smi_ergodic", 2, [long = 20.0, short = 5.0, signal = 5.0],
    |bars| batch_smi_ergodic(bars, long.round() as usize, short.round() as usize, signal.round() as usize),
    // The signal line is a `signal`-bar EMA of the smi line.
    lookback = (pbars(long) + pbars(short)).saturating_sub(2),
    full = (pbars(long) + pbars(short)).saturating_sub(2) + pback(signal, 1)
}
hist_indicator! {
    /// `coppock` — Coppock curve.
    Coppock, "coppock", 1, [wma_p = 10.0, roc_long = 14.0, roc_short = 11.0],
    |bars| batch_coppock(bars, wma_p.round() as usize, roc_long.round() as usize, roc_short.round() as usize),
    lookback = pbars(roc_long.max(roc_short)) + pback(wma_p, 1)
}
hist_indicator! {
    /// `kst` — Know Sure Thing (kst, signal).
    Kst, "kst", 2,
    [roc1 = 10.0, sma1 = 10.0, roc2 = 15.0, sma2 = 10.0, roc3 = 20.0, sma3 = 10.0, roc4 = 30.0, sma4 = 15.0, signal = 9.0],
    |bars| batch_kst(
        bars,
        roc1.round() as usize, sma1.round() as usize,
        roc2.round() as usize, sma2.round() as usize,
        roc3.round() as usize, sma3.round() as usize,
        roc4.round() as usize, sma4.round() as usize,
        signal.round() as usize
    ),
    // `batch_kst` emits only where ALL FOUR roc_sma components are defined, so the
    // warm-up is the MAX over components — NOT component 4, which merely happens to
    // dominate at the defaults (a caller can make any of the four dominate).
    lookback = (pbars(roc1) + pback(sma1, 1))
        .max(pbars(roc2) + pback(sma2, 1))
        .max(pbars(roc3) + pback(sma3, 1))
        .max(pbars(roc4) + pback(sma4, 1)),
    full = (pbars(roc1) + pback(sma1, 1))
        .max(pbars(roc2) + pback(sma2, 1))
        .max(pbars(roc3) + pback(sma3, 1))
        .max(pbars(roc4) + pback(sma4, 1))
        + pback(signal, 1)
}
hist_indicator! {
    /// `aroon` — Aroon up/down.
    Aroon, "aroon", 2, [period = 14.0],
    |bars| batch_aroon(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `aroonosc` — Aroon oscillator.
    Aroonosc, "aroonosc", 1, [period = 14.0],
    |bars| batch_aroonosc(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `adx` — average directional index (adx, +DI, −DI).
    Adx, "adx", 3, [period = 14.0],
    |bars| batch_adx(bars, period.round() as usize),
    // +DI/-DI land at `period`; the ADX line itself (output line 0) is a second
    // Wilder smoothing of DX and only lands at `2*period - 1`.
    lookback = pbars(period), full = (2 * pbars(period)).saturating_sub(1)
}
hist_indicator! {
    /// `adxr` — ADX rating.
    Adxr, "adxr", 1, [period = 14.0],
    |bars| batch_adxr(bars, period.round() as usize),
    lookback = (3 * pbars(period)).saturating_sub(1)
}
hist_indicator! {
    /// `elder_ray` — bull/bear power.
    ElderRay, "elder_ray", 2, [period = 13.0],
    |bars| batch_elder_ray(bars, period.round() as usize),
    lookback = pback(period, 1)
}
hist_indicator! {
    /// `stochf` — fast stochastic (%K, %D).
    Stochf, "stochf", 2, [k = 14.0, d = 3.0],
    |bars| batch_stochf(bars, k.round() as usize, d.round() as usize),
    lookback = pback(k, 1), full = pback(k, 1) + pback(d, 1)
}
hist_indicator! {
    /// `stochrsi` — stochastic RSI (%K, %D).
    Stochrsi, "stochrsi", 2, [rsi_p = 14.0, k = 14.0, d = 3.0],
    |bars| batch_stochrsi(bars, rsi_p.round() as usize, k.round() as usize, d.round() as usize),
    lookback = pbars(rsi_p) + pback(k, 1),
    full = pbars(rsi_p) + pback(k, 1) + pback(d, 1)
}
hist_indicator! {
    /// `ultosc` — ultimate oscillator.
    Ultosc, "ultosc", 1, [p1 = 7.0, p2 = 14.0, p3 = 28.0],
    |bars| batch_ultosc(bars, p1.round() as usize, p2.round() as usize, p3.round() as usize),
    lookback = pbars(p1.max(p2).max(p3))
}
hist_indicator! {
    /// `vortex` — Vortex indicator (VI+, VI−).
    Vortex, "vortex", 2, [period = 14.0],
    |bars| batch_vortex(bars, period.round() as usize),
    lookback = pbars(period)
}
hist_indicator! {
    /// `chande_kroll_stop` — long/short stops.
    ChandeKrollStop, "chande_kroll_stop", 2, [p = 10.0, x = 1.0, q = 9.0],
    |bars| batch_chande_kroll_stop(bars, p.round() as usize, x.round() as usize, q.round() as usize),
    lookback = pbars(p).max(pback(q, 1))
}
hist_indicator! {
    /// `asi` — accumulative swing index.
    Asi, "asi", 1, [limit = 1.0],
    |bars| batch_asi(bars, limit),
    lookback = 1
}
hist_indicator! {
    /// `fisher` — Fisher transform (fisher, trigger).
    Fisher, "fisher", 2, [period = 9.0],
    |bars| batch_fisher(bars, period.round() as usize),
    // The trigger line repeats the PREVIOUS fisher value -> one bar later.
    lookback = pback(period, 1), full = pbars(period)
}
hist_indicator! {
    /// `connors_rsi` — Connors RSI.
    ConnorsRsi, "connors_rsi", 1, [rsi_p = 3.0, streak_p = 2.0, rank_p = 100.0],
    |bars| batch_connors_rsi(bars, rsi_p.round() as usize, streak_p.round() as usize, rank_p.round() as usize),
    // `batch_connors_rsi` averages rsi1 (defined at rsi_p), rsi2 (at streak_p) and
    // prank (at rank_p) and emits only where all three are defined: the warm-up is
    // the MAX, not rank_p — which only dominates at the defaults.
    lookback = pbars(rsi_p).max(pbars(streak_p)).max(pbars(rank_p))
}
hist_indicator! {
    /// `relative_vigor` — relative vigor index (rvgi, signal).
    RelativeVigor, "relative_vigor", 2, [period = 10.0],
    |bars| batch_relative_vigor(bars, period.round() as usize),
    // The signal line is a 4-bar SWMA of rvgi -> three bars later.
    lookback = pbars(period) + 2, full = pbars(period) + 5
}
hist_indicator! {
    /// `ac` — accelerator oscillator.
    Ac, "ac", 1, [],
    |bars| batch_ac(bars),
    lookback = 37
}

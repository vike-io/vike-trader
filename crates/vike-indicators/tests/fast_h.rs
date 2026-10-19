//! `gmma`, `alligator`, `relative_vigor`, `high_low_52w`, `fisher`, `chande_kroll_stop`,
//! `connors_rsi`, `hvol`, `relative_volatility`, `median_ma`, `linreg_channel`, `mcginley` and `asi`:
//! the fast cores against a textbook reference, and the stream (`next(&Bar)`), the plain-number entry
//! (`next_value`, `next_hl`, `next_hlc`, `next_ohlc`), the batch (`vectorize`) and the slice call
//! (`series::*`) against each other, bit for bit.
//!
//! The reference does each step the long way: a window folded from scratch, the divisions, the SMA
//! seed. The cores carry running sums, multiply by a precomputed reciprocal and reorder a division
//! off the recurrence's chain, so the last bits can differ from the reference; the tolerance is 1e-9
//! relative on three scales of series (see `close`). Where the arithmetic is only a selection
//! (`high_low_52w`, `median_ma`, the Alligator's forward plot) the comparison is bit for bit.

use vike_indicators::indicators::{
    Alligator, Asi, ChandeKrollStop, ConnorsRsi, Fisher, Gmma, HighLow52w, Hvol, LinregChannel,
    Mcginley, MedianMa, RelativeVigor, RelativeVolatility,
};
use vike_indicators::{Indicator, series};
use vike_marketdata::Bar;

fn closes(level: f64, step: f64, n: usize) -> Vec<f64> {
    let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut p = level;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            p += ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * step;
            p
        })
        .collect()
}

/// A bar around each close: the open is the previous close, the high and low reach out a spread that
/// varies. A NaN close makes a NaN bar (open, high, low and close of the bar after it too).
fn bars_of(c: &[f64], scale: f64) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let o = if i == 0 { x } else { c[i - 1] };
            let up = (0.3 + (i % 7) as f64 * 0.1) * scale;
            let dn = (0.2 + (i % 5) as f64 * 0.15) * scale;
            Bar::ohlcv(
                i as i64 * 60_000,
                o,
                o.max(x) + up,
                o.min(x) - dn,
                x,
                100.0 + (i % 13) as f64,
            )
        })
        .collect()
}

struct Cols {
    o: Vec<f64>,
    h: Vec<f64>,
    l: Vec<f64>,
    c: Vec<f64>,
}

fn cols(b: &[Bar]) -> Cols {
    Cols {
        o: b.iter().map(|x| x.open).collect(),
        h: b.iter().map(|x| x.high).collect(),
        l: b.iter().map(|x| x.low).collect(),
        c: b.iter().map(|x| x.close).collect(),
    }
}

// ---- comparisons ------------------------------------------------------------------------------

/// `got` against `want` to a relative `rel` of the value, with a floor of a thousandth of the
/// series' own scale so a value that is zero in exact arithmetic is not held to a relative error.
fn close(name: &str, got: &[f64], want: &[f64], rel: f64) {
    assert_eq!(got.len(), want.len(), "{name}: length");
    let typ = want.iter().filter(|v| v.is_finite()).fold(0.0_f64, |a, v| a.max(v.abs()));
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if w.is_nan() {
            assert!(g.is_nan(), "{name}[{i}]: want NaN, got {g}");
        } else {
            assert!(g.is_finite(), "{name}[{i}]: want {w}, got {g}");
            let err = (g - w).abs() / w.abs().max(1.0e-3 * typ);
            assert!(err <= rel, "{name}[{i}]: got {g}, want {w}, relative error {err:e}");
        }
    }
}

fn same(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

/// The scales: (level, step, bar-spread scale).
const SCALES: [(f64, f64, f64); 3] =
    [(100.0, 2.0, 1.0), (1.0e6, 0.01, 0.01), (1.0e-3, 1.0e-6, 1.0e-6)];

/// Each scale, clean and with a NaN planted in the middle of the closes.
fn inputs() -> Vec<(String, Cols)> {
    let mut v = Vec::new();
    for (level, step, spread) in SCALES {
        for nan in [false, true] {
            let mut c = closes(level, step, 3000);
            if nan {
                c[1200] = f64::NAN;
            }
            v.push((format!("level {level} nan {nan}"), cols(&bars_of(&c, spread))));
        }
    }
    v
}

// ---- the textbook references ------------------------------------------------------------------

/// An EMA seeded with the mean of the first `n` values.
fn ema_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if c.len() < n {
        return out;
    }
    let alpha = 2.0 / (n as f64 + 1.0);
    let mut prev = c[..n].iter().sum::<f64>() / n as f64;
    out[n - 1] = prev;
    for i in n..c.len() {
        prev = alpha * c[i] + (1.0 - alpha) * prev;
        out[i] = prev;
    }
    out
}

fn gmma_ref(c: &[f64]) -> Vec<Vec<f64>> {
    [3usize, 5, 8, 10, 12, 15, 30, 35, 40, 45, 50, 60].iter().map(|&n| ema_ref(c, n)).collect()
}

/// Wilder's smoothing seeded with the mean of the first `n` values.
fn rma_ref(x: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    if x.len() < n {
        return out;
    }
    let mut prev = x[..n].iter().sum::<f64>() / n as f64;
    out[n - 1] = prev;
    for i in n..x.len() {
        prev = (prev * (n as f64 - 1.0) + x[i]) / n as f64;
        out[i] = prev;
    }
    out
}

fn alligator_ref(h: &[f64], l: &[f64]) -> Vec<Vec<f64>> {
    let med: Vec<f64> = h.iter().zip(l).map(|(a, b)| (a + b) / 2.0).collect();
    [(13usize, 8usize), (8, 5), (5, 3)]
        .iter()
        .map(|&(n, shift)| {
            let raw = rma_ref(&med, n);
            let mut out = vec![f64::NAN; med.len()];
            if med.len() > shift {
                out[shift..].copy_from_slice(&raw[..med.len() - shift]);
            }
            out
        })
        .collect()
}

fn swma(a: f64, b: f64, c: f64, d: f64) -> f64 {
    (a + 2.0 * b + 2.0 * c + d) / 6.0
}

fn vigor_ref(o: &[f64], h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<Vec<f64>> {
    let len = c.len();
    let (mut num, mut den) = (vec![f64::NAN; len], vec![f64::NAN; len]);
    for i in 3..len {
        let co = |k: usize| c[k] - o[k];
        let hl = |k: usize| h[k] - l[k];
        num[i] = swma(co(i - 3), co(i - 2), co(i - 1), co(i));
        den[i] = swma(hl(i - 3), hl(i - 2), hl(i - 1), hl(i));
    }
    let mut rvgi = vec![f64::NAN; len];
    #[expect(
        clippy::needless_range_loop,
        reason = "a textbook reference: i is the bar index, read in several arrays"
    )]
    for i in (n + 2)..len {
        let (mut ns, mut ds, mut ok) = (0.0, 0.0, true);
        for k in (i + 1 - n)..=i {
            if num[k].is_nan() || den[k].is_nan() {
                ok = false;
                break;
            }
            ns += num[k];
            ds += den[k];
        }
        if ok && ds != 0.0 {
            rvgi[i] = ns / ds;
        }
    }
    let mut signal = vec![f64::NAN; len];
    let defined: Vec<usize> = (0..len).filter(|&i| !rvgi[i].is_nan()).collect();
    for w in defined.windows(4) {
        signal[w[3]] = swma(rvgi[w[0]], rvgi[w[1]], rvgi[w[2]], rvgi[w[3]]);
    }
    vec![rvgi, signal]
}

fn hilo_ref(h: &[f64], l: &[f64], n: usize) -> Vec<Vec<f64>> {
    let mut out = vec![vec![f64::NAN; h.len()], vec![f64::NAN; h.len()]];
    for i in (n - 1)..h.len() {
        out[0][i] = h[i + 1 - n..=i].iter().cloned().fold(f64::MIN, f64::max);
        out[1][i] = l[i + 1 - n..=i].iter().cloned().fold(f64::MAX, f64::min);
    }
    out
}

fn fisher_ref(h: &[f64], l: &[f64], n: usize) -> Vec<Vec<f64>> {
    let med: Vec<f64> = h.iter().zip(l).map(|(a, b)| (a + b) / 2.0).collect();
    let mut out = vec![vec![f64::NAN; h.len()], vec![f64::NAN; h.len()]];
    let (mut prev_value, mut prev_fish, mut started) = (0.0_f64, 0.0_f64, false);
    for i in (n - 1)..med.len() {
        let w = &med[i + 1 - n..=i];
        let hi = w.iter().cloned().fold(f64::MIN, f64::max);
        let lo = w.iter().cloned().fold(f64::MAX, f64::min);
        let rng = hi - lo;
        let norm = if rng == 0.0 { 0.0 } else { (med[i] - lo) / rng };
        let value = (0.66 * (2.0 * norm - 1.0) + 0.67 * prev_value).clamp(-0.999, 0.999);
        let f = 0.5 * ((1.0 + value) / (1.0 - value)).ln() + 0.5 * prev_fish;
        out[0][i] = f;
        out[1][i] = if started { prev_fish } else { f64::NAN };
        prev_value = value;
        prev_fish = f;
        started = true;
    }
    out
}

fn cks_ref(h: &[f64], l: &[f64], c: &[f64], p: usize, x: f64, q: usize) -> Vec<Vec<f64>> {
    let len = c.len();
    let mut tr = vec![f64::NAN; len];
    for i in 1..len {
        tr[i] = (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs());
    }
    let mut atr = vec![f64::NAN; len];
    if len > p {
        let mut prev = tr[1..=p].iter().sum::<f64>() / p as f64;
        atr[p] = prev;
        for i in (p + 1)..len {
            prev = (prev * (p as f64 - 1.0) + tr[i]) / p as f64;
            atr[i] = prev;
        }
    }
    let (mut fh, mut fl) = (vec![f64::NAN; len], vec![f64::NAN; len]);
    for i in p..len {
        if atr[i].is_nan() {
            continue;
        }
        let hh = h[i + 1 - p..=i].iter().cloned().fold(f64::MIN, f64::max);
        let ll = l[i + 1 - p..=i].iter().cloned().fold(f64::MAX, f64::min);
        fh[i] = hh - x * atr[i];
        fl[i] = ll + x * atr[i];
    }
    let mut out = vec![vec![f64::NAN; len], vec![f64::NAN; len]];
    for i in (q - 1)..len {
        let hs: Vec<f64> = fh[i + 1 - q..=i].iter().cloned().filter(|v| !v.is_nan()).collect();
        let ls: Vec<f64> = fl[i + 1 - q..=i].iter().cloned().filter(|v| !v.is_nan()).collect();
        if !hs.is_empty() {
            out[0][i] = hs.iter().cloned().fold(f64::MIN, f64::max);
            out[1][i] = ls.iter().cloned().fold(f64::MAX, f64::min);
        }
    }
    out
}

/// Wilder's RSI of any series: the mean of the first `n` moves, then the recurrence.
fn rsi_ref(x: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    if x.len() <= n {
        return out;
    }
    let gain = |i: usize| (x[i] - x[i - 1]).max(0.0);
    let loss = |i: usize| (-(x[i] - x[i - 1])).max(0.0);
    let mut ag = (1..=n).map(gain).sum::<f64>() / n as f64;
    let mut al = (1..=n).map(loss).sum::<f64>() / n as f64;
    let rsi = |ag: f64, al: f64| if al == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + ag / al) };
    out[n] = rsi(ag, al);
    #[expect(
        clippy::needless_range_loop,
        reason = "a textbook reference: i is the bar index, read through `gain` and `loss`"
    )]
    for i in (n + 1)..x.len() {
        ag = (ag * (n as f64 - 1.0) + gain(i)) / n as f64;
        al = (al * (n as f64 - 1.0) + loss(i)) / n as f64;
        out[i] = rsi(ag, al);
    }
    out
}

fn connors_ref(c: &[f64], rsi_p: usize, streak_p: usize, rank_p: usize) -> Vec<f64> {
    let len = c.len();
    let mut streak = vec![0.0_f64; len];
    for i in 1..len {
        let d = c[i] - c[i - 1];
        streak[i] = if d > 0.0 {
            if streak[i - 1] > 0.0 { streak[i - 1] + 1.0 } else { 1.0 }
        } else if d < 0.0 {
            if streak[i - 1] < 0.0 { streak[i - 1] - 1.0 } else { -1.0 }
        } else {
            0.0
        };
    }
    let mut roc = vec![f64::NAN; len];
    for i in 1..len {
        if c[i - 1] != 0.0 {
            roc[i] = (c[i] / c[i - 1] - 1.0) * 100.0;
        }
    }
    let (r1, r2) = (rsi_ref(c, rsi_p), rsi_ref(&streak, streak_p));
    let mut out = vec![f64::NAN; len];
    for i in rank_p..len {
        if roc[i].is_nan() {
            continue;
        }
        let w: Vec<f64> = roc[i + 1 - rank_p..=i].iter().cloned().filter(|v| !v.is_nan()).collect();
        let below = w.iter().filter(|&&v| v < roc[i]).count();
        let prank = 100.0 * below as f64 / w.len() as f64;
        if !r1[i].is_nan() && !r2[i].is_nan() {
            out[i] = (r1[i] + r2[i] + prank) / 3.0;
        }
    }
    out
}

/// The population standard deviation of a window, the long way (two passes; a window of equal
/// numbers is exactly zero).
fn pop_sd(w: &[f64]) -> f64 {
    if !w[0].is_nan() && w.iter().all(|&v| v == w[0]) {
        return 0.0;
    }
    let n = w.len() as f64;
    let mean = w.iter().sum::<f64>() / n;
    (w.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n).sqrt()
}

fn hvol_ref(c: &[f64], n: usize, ann: f64) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    let mut rets: Vec<f64> = Vec::new();
    for i in 1..c.len() {
        if !(c[i] > 0.0 && c[i - 1] > 0.0) {
            continue;
        }
        let lr = (c[i] / c[i - 1]).ln();
        if lr.is_nan() {
            continue;
        }
        rets.push(lr);
        if rets.len() >= n {
            out[i] = pop_sd(&rets[rets.len() - n..]) * ann.sqrt() * 100.0;
        }
    }
    out
}

fn relvol_ref(c: &[f64], n: usize) -> Vec<f64> {
    let len = c.len();
    let mut out = vec![f64::NAN; len];
    // The streams of up and down deviations, only on bars whose window holds numbers.
    let (mut ups, mut downs): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    let alpha = 2.0 / (n as f64 + 1.0);
    let (mut eu, mut ed) = (f64::NAN, f64::NAN);
    for i in 1..len {
        if i + 1 < n {
            continue;
        }
        let sd = pop_sd(&c[i + 1 - n..=i]);
        if sd.is_nan() {
            continue;
        }
        let (u, d) = if c[i] > c[i - 1] {
            (sd, 0.0)
        } else if c[i] < c[i - 1] {
            (0.0, sd)
        } else {
            (0.0, 0.0)
        };
        ups.push(u);
        downs.push(d);
        let k = ups.len();
        if k == n {
            eu = ups.iter().sum::<f64>() / n as f64;
            ed = downs.iter().sum::<f64>() / n as f64;
        } else if k > n {
            eu = alpha * u + (1.0 - alpha) * eu;
            ed = alpha * d + (1.0 - alpha) * ed;
        }
        if k >= n {
            out[i] = if eu + ed != 0.0 { 100.0 * eu / (eu + ed) } else { 50.0 };
        }
    }
    out
}

fn median_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    for i in (n - 1)..c.len() {
        let mut w = c[i + 1 - n..=i].to_vec();
        if w.iter().any(|v| v.is_nan()) {
            continue;
        }
        w.sort_by(|a, b| a.partial_cmp(b).unwrap());
        out[i] = if n % 2 == 1 { w[n / 2] } else { (w[n / 2 - 1] + w[n / 2]) / 2.0 };
    }
    out
}

fn linreg_ref(c: &[f64], n: usize, mult: f64) -> Vec<Vec<f64>> {
    let mut out = vec![vec![f64::NAN; c.len()]; 3];
    let nf = n as f64;
    let xm = (nf - 1.0) / 2.0;
    for i in (n - 1)..c.len() {
        let w = &c[i + 1 - n..=i];
        let (mid, sd) = if !w[0].is_nan() && w.iter().all(|&v| v == w[0]) {
            (w[0], 0.0)
        } else {
            let ym = w.iter().sum::<f64>() / nf;
            let sxy: f64 = w.iter().enumerate().map(|(j, y)| (j as f64 - xm) * (y - ym)).sum();
            let sxx: f64 = (0..n).map(|j| (j as f64 - xm) * (j as f64 - xm)).sum();
            let slope = sxy / sxx;
            let icpt = ym - slope * xm;
            let r: Vec<f64> =
                w.iter().enumerate().map(|(j, y)| y - (icpt + slope * j as f64)).collect();
            let rm = r.iter().sum::<f64>() / nf;
            let sd = (r.iter().map(|v| (v - rm) * (v - rm)).sum::<f64>() / nf).sqrt();
            (icpt + slope * (nf - 1.0), sd)
        };
        out[0][i] = mid + mult * sd;
        out[1][i] = mid;
        out[2][i] = mid - mult * sd;
    }
    out
}

fn mcginley_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    let mut prev = f64::NAN;
    for (i, &x) in c.iter().enumerate() {
        prev = if i == 0 || prev == 0.0 {
            x
        } else {
            let r = x / prev;
            prev + (x - prev) / (n as f64 * r * r * r * r)
        };
        out[i] = prev;
    }
    out
}

fn asi_ref(o: &[f64], h: &[f64], l: &[f64], c: &[f64], limit: f64) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    let mut cum = 0.0;
    for i in 1..c.len() {
        let (cp, op) = (c[i - 1], o[i - 1]);
        let a = (h[i] - cp).abs();
        let b = (l[i] - cp).abs();
        let r = a.max(b).max(h[i] - l[i]);
        let k = a.max(b);
        let si = if r == 0.0 || limit == 0.0 {
            0.0
        } else {
            let t = r + 0.25 * (cp - op).abs();
            let numerator = (c[i] - cp) + 0.5 * (c[i] - o[i]) + 0.25 * (cp - op);
            50.0 * (numerator / t) * (k / limit)
        };
        cum += si;
        out[i] = cum;
    }
    out
}

// ---- tolerance against the textbook ------------------------------------------------------------

#[test]
fn gmma_matches_the_textbook() {
    for (what, x) in inputs() {
        let got = series::gmma(&x.c);
        let want = gmma_ref(&x.c);
        for k in 0..12 {
            close(&format!("gmma line {k} {what}"), &got[k], &want[k], 1e-12);
        }
    }
}

#[test]
fn alligator_matches_the_textbook() {
    for (what, x) in inputs() {
        let got = series::alligator(&x.h, &x.l);
        let want = alligator_ref(&x.h, &x.l);
        for k in 0..3 {
            close(&format!("alligator line {k} {what}"), &got[k], &want[k], 1e-12);
        }
    }
}

#[test]
fn relative_vigor_matches_the_textbook() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 10, 64, 200] {
            let got = series::relative_vigor(&x.o, &x.h, &x.l, &x.c, n as f64).unwrap();
            let want = vigor_ref(&x.o, &x.h, &x.l, &x.c, n);
            for k in 0..2 {
                close(&format!("relative_vigor({n}) line {k} {what}"), &got[k], &want[k], 1e-9);
            }
        }
    }
}

#[test]
fn high_low_52w_matches_the_window_scan_bit_for_bit() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 20, 252, 1000] {
            let got = series::high_low_52w(&x.h, &x.l, n as f64).unwrap();
            let want = hilo_ref(&x.h, &x.l, n);
            for k in 0..2 {
                assert!(same(&got[k], &want[k]), "high_low_52w({n}) line {k} {what}");
            }
        }
    }
}

#[test]
fn fisher_matches_the_textbook() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 9, 64, 100] {
            let got = series::fisher(&x.h, &x.l, n as f64).unwrap();
            let want = fisher_ref(&x.h, &x.l, n);
            for k in 0..2 {
                close(&format!("fisher({n}) line {k} {what}"), &got[k], &want[k], 1e-9);
            }
        }
    }
}

#[test]
fn chande_kroll_stop_matches_the_textbook() {
    for (what, x) in inputs() {
        for (p, mx, q) in
            [(2usize, 1usize, 2usize), (10, 1, 9), (20, 3, 5), (64, 2, 64), (200, 10, 200)]
        {
            let got =
                series::chande_kroll_stop(&x.h, &x.l, &x.c, p as f64, mx as f64, q as f64).unwrap();
            let want = cks_ref(&x.h, &x.l, &x.c, p, mx as f64, q);
            for k in 0..2 {
                close(
                    &format!("chande_kroll_stop({p},{mx},{q}) line {k} {what}"),
                    &got[k],
                    &want[k],
                    1e-9,
                );
            }
        }
    }
}

#[test]
fn connors_rsi_matches_the_textbook() {
    for (what, x) in inputs() {
        for (a, b, r) in [(2usize, 2usize, 10usize), (3, 2, 100), (14, 5, 50), (100, 100, 500)] {
            close(
                &format!("connors_rsi({a},{b},{r}) {what}"),
                &series::connors_rsi(&x.c, a as f64, b as f64, r as f64).unwrap(),
                &connors_ref(&x.c, a, b, r),
                1e-9,
            );
        }
    }
}

#[test]
fn hvol_matches_the_textbook() {
    for (what, x) in inputs() {
        for (n, ann) in [(2usize, 1usize), (5, 252), (20, 365), (64, 52), (200, 365)] {
            close(
                &format!("hvol({n},{ann}) {what}"),
                &series::hvol(&x.c, n as f64, ann as f64).unwrap(),
                &hvol_ref(&x.c, n, ann as f64),
                1e-9,
            );
        }
    }
}

#[test]
fn relative_volatility_matches_the_textbook() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 14, 64, 200] {
            close(
                &format!("relative_volatility({n}) {what}"),
                &series::relative_volatility(&x.c, n as f64).unwrap(),
                &relvol_ref(&x.c, n),
                1e-9,
            );
        }
    }
}

#[test]
fn median_ma_matches_the_sorted_window_bit_for_bit() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 20, 64, 201, 400] {
            let got = series::median_ma(&x.c, n as f64).unwrap();
            assert!(same(&got, &median_ref(&x.c, n)), "median_ma({n}) {what}");
        }
    }
}

#[test]
fn linreg_channel_matches_the_textbook() {
    for (what, x) in inputs() {
        for (n, mult) in [(2usize, 2.0), (3, 0.5), (20, 2.0), (100, 2.0), (400, 10.0)] {
            let got = series::linreg_channel(&x.c, n as f64, mult).unwrap();
            let want = linreg_ref(&x.c, n, mult);
            for k in 0..3 {
                close(
                    &format!("linreg_channel({n},{mult}) line {k} {what}"),
                    &got[k],
                    &want[k],
                    1e-9,
                );
            }
        }
    }
}

#[test]
fn mcginley_matches_the_textbook() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 14, 64, 200] {
            close(
                &format!("mcginley({n}) {what}"),
                &series::mcginley(&x.c, n as f64).unwrap(),
                &mcginley_ref(&x.c, n),
                1e-9,
            );
        }
    }
}

#[test]
fn asi_matches_the_textbook() {
    for (what, x) in inputs() {
        for limit in [0.1, 1.0, 2.5, 10.0] {
            close(
                &format!("asi({limit}) {what}"),
                &series::asi(&x.o, &x.h, &x.l, &x.c, limit).unwrap(),
                &asi_ref(&x.o, &x.h, &x.l, &x.c, limit),
                1e-9,
            );
        }
    }
}

// ---- the laws the old kernels kept ---------------------------------------------------------------

/// A window of bit-identical values has a deviation of EXACTLY zero, whatever came before it: a big
/// move leaves a carried sum with a residue, and the old two-pass fold never had one.
#[test]
fn a_flat_stretch_is_exactly_flat_after_a_big_move() {
    let mut c = closes(100.0, 2.0, 400);
    c.extend(std::iter::repeat_n(0.1, 400));
    let mut d = closes(100.0, 2.0, 400);
    d.extend(std::iter::repeat_n(5000.0, 400));
    for n in [2usize, 14, 50, 200] {
        // hvol: ln(v / v) = 0 for the whole window.
        let h = series::hvol(&c, n as f64, 365.0).unwrap();
        for (i, v) in h.iter().enumerate().take(800).skip(400 + n + 1) {
            assert_eq!(*v, 0.0, "hvol({n})[{i}]");
        }
    }
    for n in [2usize, 14, 50, 100, 400] {
        // linreg_channel: a window of equal closes is its own line, with no band.
        let [u, m, l] = series::linreg_channel(&d, n as f64, 2.0).unwrap();
        for i in 400 + n..800 {
            assert!(u[i] == 5000.0 && m[i] == 5000.0 && l[i] == 5000.0, "linreg({n})[{i}]");
        }
    }
}

/// A `NaN` inside the window makes the answer `NaN` while it is inside, and the answer is exactly the
/// textbook one the update after it left.
#[test]
fn a_nan_leaves_no_trace_once_it_is_out_of_the_window() {
    let mut c = closes(100.0, 2.0, 600);
    c[300] = f64::NAN;
    for n in [2usize, 14, 100] {
        let med = series::median_ma(&c, n as f64).unwrap();
        for (i, v) in med.iter().enumerate().take(300 + n).skip(300) {
            assert!(v.is_nan(), "median_ma({n})[{i}] holds the NaN");
        }
        assert!(same(&med, &median_ref(&c, n)));
        let lc = series::linreg_channel(&c, n as f64, 2.0).unwrap();
        for (i, v) in lc[1].iter().enumerate().skip(300).take(n) {
            assert!(v.is_nan(), "linreg_channel({n})[{i}] holds the NaN");
        }
        let want = linreg_ref(&c, n, 2.0);
        for k in 0..3 {
            close(&format!("linreg_channel({n}) line {k}"), &lc[k], &want[k], 1e-9);
        }
        assert!(lc[1][300 + n].is_finite(), "linreg_channel({n}) recovers");
        let hv = series::hvol(&c, n as f64, 365.0).unwrap();
        // A NaN close has no log return, and neither does the bar after it: the window just skips both.
        assert!(hv[310 + n].is_finite());
        close(&format!("hvol({n})"), &hv, &hvol_ref(&c, n, 365.0), 1e-9);
    }
}

/// A window that is a straight line has a residual spread of zero to the rounding of the numbers, not
/// the square root of that rounding.
#[test]
fn a_straight_line_has_no_band() {
    for (level, slope) in [(100.0, 0.5), (1.0e6, 3.0), (1.0e-3, 1.0e-5)] {
        let c: Vec<f64> = (0..300).map(|i| level + slope * i as f64).collect();
        for n in [3usize, 20, 100] {
            let [u, m, l] = series::linreg_channel(&c, n as f64, 2.0).unwrap();
            for i in (n - 1)..300 {
                let width = u[i] - l[i];
                assert!(
                    width.abs() <= 1e-9 * m[i].abs().max(1e-3 * slope * n as f64),
                    "n={n} i={i} width {width:e}"
                );
                let want = level + slope * i as f64;
                assert!((m[i] - want).abs() <= 1e-9 * want.abs().max(1.0e-3), "mid n={n} i={i}");
            }
        }
    }
}

/// The Alligator's lines are the smoothing plotted forward: the NaN of the first bars and the shift.
#[test]
fn the_alligator_plots_forward() {
    let c = closes(100.0, 2.0, 100);
    let b = bars_of(&c, 1.0);
    let x = cols(&b);
    let a = series::alligator(&x.h, &x.l);
    // jaw: seed at bar 12, plotted 8 later; teeth: 7 + 5; lips: 4 + 3.
    for (k, first) in [20usize, 12, 7].into_iter().enumerate() {
        assert!(a[k][first - 1].is_nan() && a[k][first].is_finite(), "line {k} starts at {first}");
    }
}

// ---- the ways in -------------------------------------------------------------------------------

fn lengths(n: usize) -> [usize; 8] {
    [0, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600]
}

fn planted(len: usize, spread: f64) -> Vec<Bar> {
    let mut c = closes(100.0, 2.0, len);
    if len > 40 {
        c[len / 3] = f64::NAN;
    }
    bars_of(&c, spread)
}

/// One-line indicators: stream, plain-number entry, batch and slice call, bit for bit.
fn ways1(label: &str, stream: Vec<f64>, value: Vec<f64>, batch: Vec<Vec<f64>>, slice: Vec<f64>) {
    assert!(same(&stream, &value), "{label}: next vs the plain-number entry");
    assert!(same(&stream, &batch[0]), "{label}: vectorize");
    assert!(same(&stream, &slice), "{label}: series");
}

/// Multi-line indicators.
fn ways<const N: usize>(
    label: &str,
    stream: Vec<[f64; N]>,
    value: Vec<[f64; N]>,
    batch: Vec<Vec<f64>>,
    slice: Vec<Vec<f64>>,
) {
    for k in 0..N {
        let s: Vec<f64> = stream.iter().map(|v| v[k]).collect();
        let v: Vec<f64> = value.iter().map(|v| v[k]).collect();
        assert!(same(&s, &v), "{label} line {k}: next vs the plain-number entry");
        assert!(same(&s, &batch[k]), "{label} line {k}: vectorize");
        assert!(same(&s, &slice[k]), "{label} line {k}: series");
    }
}

/// Stream == plain-number entry == vectorize == series, bit for bit, over periods, lengths and a NaN.
#[test]
fn every_way_in_gives_the_same_bits() {
    // gmma, alligator: no parameter
    for len in lengths(60) {
        let b = planted(len, 1.0);
        let x = cols(&b);
        let mut ind = Gmma::default();
        let stream: Vec<[f64; 12]> = b.iter().map(|y| ind.next(y)).collect();
        let mut ind = Gmma::default();
        let value: Vec<[f64; 12]> = x.c.iter().map(|&y| ind.next_value(y)).collect();
        ways("gmma", stream, value, Gmma::default().vectorize(&b), series::gmma(&x.c).to_vec());

        let mut ind = Alligator::default();
        let stream: Vec<[f64; 3]> = b.iter().map(|y| ind.next(y)).collect();
        let mut ind = Alligator::default();
        let value: Vec<[f64; 3]> = b.iter().map(|y| ind.next_hl(y.high, y.low)).collect();
        ways(
            "alligator",
            stream,
            value,
            Alligator::default().vectorize(&b),
            series::alligator(&x.h, &x.l).to_vec(),
        );
    }
    // relative_vigor
    for n in [2usize, 3, 10, 20, 64, 200] {
        for len in lengths(n) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let nf = n as f64;
            let mut ind = RelativeVigor::new(nf).unwrap();
            let stream: Vec<[f64; 2]> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = RelativeVigor::new(nf).unwrap();
            let value: Vec<[f64; 2]> =
                b.iter().map(|y| ind.next_ohlc(y.open, y.high, y.low, y.close)).collect();
            ways(
                &format!("relative_vigor n={n} len={len}"),
                stream,
                value,
                RelativeVigor::new(nf).unwrap().vectorize(&b),
                series::relative_vigor(&x.o, &x.h, &x.l, &x.c, nf).unwrap().to_vec(),
            );
        }
    }
    // high_low_52w
    for n in [2usize, 3, 20, 64, 252, 511, 512, 513, 1000] {
        for len in lengths(n) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let nf = n as f64;
            let mut ind = HighLow52w::new(nf).unwrap();
            let stream: Vec<[f64; 2]> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = HighLow52w::new(nf).unwrap();
            let value: Vec<[f64; 2]> = b.iter().map(|y| ind.next_hl(y.high, y.low)).collect();
            ways(
                &format!("high_low_52w n={n} len={len}"),
                stream,
                value,
                HighLow52w::new(nf).unwrap().vectorize(&b),
                series::high_low_52w(&x.h, &x.l, nf).unwrap().to_vec(),
            );
        }
    }
    // fisher
    for n in [2usize, 3, 9, 20, 64, 100] {
        for len in lengths(n) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let nf = n as f64;
            let mut ind = Fisher::new(nf).unwrap();
            let stream: Vec<[f64; 2]> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = Fisher::new(nf).unwrap();
            let value: Vec<[f64; 2]> = b.iter().map(|y| ind.next_hl(y.high, y.low)).collect();
            ways(
                &format!("fisher n={n} len={len}"),
                stream,
                value,
                Fisher::new(nf).unwrap().vectorize(&b),
                series::fisher(&x.h, &x.l, nf).unwrap().to_vec(),
            );
        }
    }
    // chande_kroll_stop
    for (p, mx, q) in
        [(2usize, 1usize, 2usize), (10, 1, 9), (20, 3, 5), (64, 2, 64), (200, 10, 200), (5, 1, 50)]
    {
        for len in lengths(p.max(q)) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let (pf, xf, qf) = (p as f64, mx as f64, q as f64);
            let mut ind = ChandeKrollStop::new(pf, xf, qf).unwrap();
            let stream: Vec<[f64; 2]> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = ChandeKrollStop::new(pf, xf, qf).unwrap();
            let value: Vec<[f64; 2]> =
                b.iter().map(|y| ind.next_hlc(y.high, y.low, y.close)).collect();
            ways(
                &format!("chande_kroll_stop ({p},{mx},{q}) len={len}"),
                stream,
                value,
                ChandeKrollStop::new(pf, xf, qf).unwrap().vectorize(&b),
                series::chande_kroll_stop(&x.h, &x.l, &x.c, pf, xf, qf).unwrap().to_vec(),
            );
        }
    }
    // connors_rsi
    for (a, s, r) in
        [(2usize, 2usize, 10usize), (3, 2, 100), (14, 5, 50), (100, 100, 500), (3, 2, 500)]
    {
        for len in lengths(a.max(s).max(r)) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let (af, sf, rf) = (a as f64, s as f64, r as f64);
            let mut ind = ConnorsRsi::new(af, sf, rf).unwrap();
            let stream: Vec<f64> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = ConnorsRsi::new(af, sf, rf).unwrap();
            let value: Vec<f64> = x.c.iter().map(|&y| ind.next_value(y)).collect();
            ways1(
                &format!("connors_rsi ({a},{s},{r}) len={len}"),
                stream,
                value,
                ConnorsRsi::new(af, sf, rf).unwrap().vectorize(&b),
                series::connors_rsi(&x.c, af, sf, rf).unwrap(),
            );
        }
    }
    // hvol
    for (n, ann) in [(2usize, 1usize), (5, 252), (20, 365), (64, 52), (200, 365)] {
        for len in lengths(n) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let (nf, af) = (n as f64, ann as f64);
            let mut ind = Hvol::new(nf, af).unwrap();
            let stream: Vec<f64> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = Hvol::new(nf, af).unwrap();
            let value: Vec<f64> = x.c.iter().map(|&y| ind.next_value(y)).collect();
            ways1(
                &format!("hvol ({n},{ann}) len={len}"),
                stream,
                value,
                Hvol::new(nf, af).unwrap().vectorize(&b),
                series::hvol(&x.c, nf, af).unwrap(),
            );
        }
    }
    // relative_volatility, median_ma, mcginley
    for n in [2usize, 3, 14, 20, 64, 200] {
        for len in lengths(n) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let nf = n as f64;
            let mut ind = RelativeVolatility::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = RelativeVolatility::new(nf).unwrap();
            let value: Vec<f64> = x.c.iter().map(|&y| ind.next_value(y)).collect();
            ways1(
                &format!("relative_volatility n={n} len={len}"),
                stream,
                value,
                RelativeVolatility::new(nf).unwrap().vectorize(&b),
                series::relative_volatility(&x.c, nf).unwrap(),
            );

            let mut ind = Mcginley::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = Mcginley::new(nf).unwrap();
            let value: Vec<f64> = x.c.iter().map(|&y| ind.next_value(y)).collect();
            ways1(
                &format!("mcginley n={n} len={len}"),
                stream,
                value,
                Mcginley::new(nf).unwrap().vectorize(&b),
                series::mcginley(&x.c, nf).unwrap(),
            );
        }
    }
    for n in [2usize, 3, 20, 64, 201, 400] {
        for len in lengths(n) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let nf = n as f64;
            let mut ind = MedianMa::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = MedianMa::new(nf).unwrap();
            let value: Vec<f64> = x.c.iter().map(|&y| ind.next_value(y)).collect();
            ways1(
                &format!("median_ma n={n} len={len}"),
                stream,
                value,
                MedianMa::new(nf).unwrap().vectorize(&b),
                series::median_ma(&x.c, nf).unwrap(),
            );
        }
    }
    // linreg_channel
    for (n, mult) in [(2usize, 2.0), (3, 0.5), (20, 2.0), (64, 1.5), (100, 2.0), (400, 10.0)] {
        for len in lengths(n) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let nf = n as f64;
            let mut ind = LinregChannel::new(nf, mult).unwrap();
            let stream: Vec<[f64; 3]> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = LinregChannel::new(nf, mult).unwrap();
            let value: Vec<[f64; 3]> = x.c.iter().map(|&y| ind.next_value(y)).collect();
            ways(
                &format!("linreg_channel ({n},{mult}) len={len}"),
                stream,
                value,
                LinregChannel::new(nf, mult).unwrap().vectorize(&b),
                series::linreg_channel(&x.c, nf, mult).unwrap().to_vec(),
            );
        }
    }
    // asi
    for limit in [0.1, 1.0, 2.5, 10.0] {
        for len in lengths(2) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let mut ind = Asi::new(limit).unwrap();
            let stream: Vec<f64> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = Asi::new(limit).unwrap();
            let value: Vec<f64> =
                b.iter().map(|y| ind.next_ohlc(y.open, y.high, y.low, y.close)).collect();
            ways1(
                &format!("asi {limit} len={len}"),
                stream,
                value,
                Asi::new(limit).unwrap().vectorize(&b),
                series::asi(&x.o, &x.h, &x.l, &x.c, limit).unwrap(),
            );
        }
    }
}

/// `reset` returns a stepper to its first state, and a clone carries on identically.
#[test]
fn reset_and_clone_restore_the_state() {
    let b = bars_of(&closes(100.0, 2.0, 700), 1.0);
    macro_rules! check {
        ($ind:expr) => {{
            let mut a = $ind;
            let first: Vec<Vec<f64>> = b.iter().map(|y| a.on_bar(y)).collect();
            let mut forked = a.clone();
            a.reset();
            let second: Vec<Vec<f64>> = b.iter().map(|y| a.on_bar(y)).collect();
            assert_eq!(first.len(), second.len());
            for (x, y) in first.iter().zip(&second) {
                assert!(same(x, y), "{}: a reset stepper repeats itself", a.name());
            }
            // The fork was taken after the whole series; feeding both one more bar agrees.
            let extra = Bar::ohlcv(1, 101.0, 102.0, 99.5, 100.5, 321.0);
            let mut cont = a.clone();
            let (p, q) = (forked.on_bar(&extra), cont.on_bar(&extra));
            assert!(same(&p, &q), "{}: a clone after the series carries on the same", a.name());
        }};
    }
    check!(Gmma::default());
    check!(Alligator::default());
    check!(RelativeVigor::new(10.0).unwrap());
    check!(HighLow52w::new(252.0).unwrap());
    check!(Fisher::new(9.0).unwrap());
    check!(ChandeKrollStop::new(10.0, 1.0, 9.0).unwrap());
    check!(ConnorsRsi::new(3.0, 2.0, 100.0).unwrap());
    check!(Hvol::new(20.0, 365.0).unwrap());
    check!(RelativeVolatility::new(14.0).unwrap());
    check!(MedianMa::new(20.0).unwrap());
    check!(LinregChannel::new(100.0, 2.0).unwrap());
    check!(Mcginley::new(14.0).unwrap());
    check!(Asi::new(1.0).unwrap());
}

#[test]
fn out_of_range_parameters_are_refused() {
    let e = &[1.0_f64][..];
    assert!(RelativeVigor::new(1.0).is_err() && series::relative_vigor(e, e, e, e, 201.0).is_err());
    assert!(HighLow52w::new(1.0).is_err() && series::high_low_52w(e, e, 1001.0).is_err());
    assert!(Fisher::new(101.0).is_err() && series::fisher(e, e, 1.0).is_err());
    assert!(
        ChandeKrollStop::new(10.0, 0.0, 9.0).is_err()
            && series::chande_kroll_stop(e, e, e, 10.0, 1.0, 201.0).is_err()
    );
    assert!(
        ConnorsRsi::new(3.0, 2.0, 9.0).is_err()
            && series::connors_rsi(e, 101.0, 2.0, 100.0).is_err()
    );
    assert!(Hvol::new(20.0, 366.0).is_err() && series::hvol(e, f64::NAN, 365.0).is_err());
    assert!(
        RelativeVolatility::new(1.0).is_err() && series::relative_volatility(e, 201.0).is_err()
    );
    assert!(MedianMa::new(401.0).is_err() && series::median_ma(e, 1.0).is_err());
    assert!(
        LinregChannel::new(100.0, 0.4).is_err() && series::linreg_channel(e, 1.0, 2.0).is_err()
    );
    assert!(Mcginley::new(1.0).is_err() && series::mcginley(e, 201.0).is_err());
    assert!(Asi::new(0.0).is_err() && series::asi(e, e, e, e, 11.0).is_err());
}

/// The worst relative error of each indicator against its reference over the periods and scales
/// above, printed (`--ignored --nocapture`); the tests above hold it under 1e-9.
#[test]
#[ignore = "a measurement, not a check"]
fn measure_the_worst_relative_error() {
    fn worst(got: &[f64], want: &[f64]) -> f64 {
        let typ = want.iter().filter(|v| v.is_finite()).fold(0.0_f64, |a, v| a.max(v.abs()));
        got.iter()
            .zip(want)
            .filter(|(g, w)| g.is_finite() && w.is_finite())
            .map(|(g, w)| (g - w).abs() / w.abs().max(1.0e-3 * typ))
            .fold(0.0, f64::max)
    }
    let mut w = [0.0_f64; 11];
    for (_, x) in inputs() {
        let g = series::gmma(&x.c);
        let r = gmma_ref(&x.c);
        for k in 0..12 {
            w[0] = w[0].max(worst(&g[k], &r[k]));
        }
        for n in [2usize, 10, 64, 200] {
            let g = series::relative_vigor(&x.o, &x.h, &x.l, &x.c, n as f64).unwrap();
            let r = vigor_ref(&x.o, &x.h, &x.l, &x.c, n);
            w[1] = w[1].max(worst(&g[0], &r[0])).max(worst(&g[1], &r[1]));
        }
        for n in [2usize, 9, 64, 100] {
            let g = series::fisher(&x.h, &x.l, n as f64).unwrap();
            let r = fisher_ref(&x.h, &x.l, n);
            w[2] = w[2].max(worst(&g[0], &r[0])).max(worst(&g[1], &r[1]));
        }
        for (p, mx, q) in [(2usize, 1usize, 2usize), (10, 1, 9), (64, 2, 64), (200, 10, 200)] {
            let g =
                series::chande_kroll_stop(&x.h, &x.l, &x.c, p as f64, mx as f64, q as f64).unwrap();
            let r = cks_ref(&x.h, &x.l, &x.c, p, mx as f64, q);
            w[3] = w[3].max(worst(&g[0], &r[0])).max(worst(&g[1], &r[1]));
        }
        for (a, b, c) in [(2usize, 2usize, 10usize), (3, 2, 100), (14, 5, 50), (100, 100, 500)] {
            let g = series::connors_rsi(&x.c, a as f64, b as f64, c as f64).unwrap();
            w[4] = w[4].max(worst(&g, &connors_ref(&x.c, a, b, c)));
        }
        for (n, ann) in [(2usize, 1usize), (5, 252), (20, 365), (200, 365)] {
            let g = series::hvol(&x.c, n as f64, ann as f64).unwrap();
            w[5] = w[5].max(worst(&g, &hvol_ref(&x.c, n, ann as f64)));
        }
        for n in [2usize, 14, 64, 200] {
            let g = series::relative_volatility(&x.c, n as f64).unwrap();
            w[6] = w[6].max(worst(&g, &relvol_ref(&x.c, n)));
        }
        for (n, mult) in [(2usize, 2.0), (20, 2.0), (100, 2.0), (400, 10.0)] {
            let g = series::linreg_channel(&x.c, n as f64, mult).unwrap();
            let r = linreg_ref(&x.c, n, mult);
            for k in 0..3 {
                w[7] = w[7].max(worst(&g[k], &r[k]));
            }
        }
        for n in [2usize, 14, 64, 200] {
            let g = series::mcginley(&x.c, n as f64).unwrap();
            w[8] = w[8].max(worst(&g, &mcginley_ref(&x.c, n)));
        }
        for limit in [0.1, 1.0, 10.0] {
            let g = series::asi(&x.o, &x.h, &x.l, &x.c, limit).unwrap();
            w[9] = w[9].max(worst(&g, &asi_ref(&x.o, &x.h, &x.l, &x.c, limit)));
        }
        let g = series::alligator(&x.h, &x.l);
        let r = alligator_ref(&x.h, &x.l);
        for k in 0..3 {
            w[10] = w[10].max(worst(&g[k], &r[k]));
        }
    }
    println!(
        "worst relative error: gmma {:.2e} relative_vigor {:.2e} fisher {:.2e} chande_kroll_stop {:.2e} \
         connors_rsi {:.2e} hvol {:.2e} relative_volatility {:.2e} linreg_channel {:.2e} \
         mcginley {:.2e} asi {:.2e} alligator {:.2e}  (high_low_52w and median_ma are bit-exact)",
        w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10]
    );
}

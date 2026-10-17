//! The eight pair indicators (`ratio`, `spread`, `spread_zscore`, `beta`, `correl`, `correl_log`,
//! `half_life`, `kalman_beta`): the fast cores against a textbook reference and against a copy of the
//! kernels they replaced, and the stream (`on_pair`), the plain-number entry (`on_pair_values`), the
//! batch (`vectorize_pair`) and the slice call (`series::*`) against each other, bit for bit.
//!
//! The textbook reference does each value the long way: every window summed from scratch in two
//! passes, on values shifted by the window's first (so a price near a million with moves of a
//! hundredth is exact), and a window of bit-equal values is constant by decision
//! (`window::is_constant_window`). The cores carry running sums, so the last bits differ; the
//! tolerance is 1e-9 of `max(|value|, 1)` (the outputs are z-scores, betas and correlations, which
//! are order one; a correlation near zero is an absolute question).
//!
//! The replaced kernels are kept below as `old_*`: the NaN POSITIONS must be those of the old code on
//! every series (a NaN travelling through the input included), the values equal within the old
//! kernels' own rounding noise.

use vike_indicators::pairs::{
    Beta, Correl, CorrelLog, HalfLife, KalmanBeta, Ratio, Spread, SpreadZscore,
};
use vike_indicators::{PairIndicator, series};
use vike_marketdata::Bar;

fn walk(seed: u64, step: f64, n: usize) -> Vec<f64> {
    let mut s = seed;
    let mut p = 0.0;
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

/// A primary and a benchmark that move together part of the time: positive, correlated, distinct.
fn pair(level: f64, step: f64, n: usize) -> (Vec<f64>, Vec<f64>) {
    let (w1, w2) = (walk(0x9e37_79b9_7f4a_7c15, step, n), walk(0x2545_f491_4f6c_dd1d, step, n));
    let a = w1.iter().map(|x| level + x).collect();
    let b = w1.iter().zip(&w2).map(|(x, y)| 0.5 * level + 0.4 * x + 0.6 * y).collect();
    (a, b)
}

/// A mean-reverting spread: `b` is a walk, `a = b + s` with `s_t = (1 - theta) s_{t-1} + noise`, so
/// the half-life is well defined and `lambda` is far from zero.
fn reverting(level: f64, step: f64, n: usize, theta: f64) -> (Vec<f64>, Vec<f64>) {
    let noise = walk(0x1234_5678_9abc_def1, step, n);
    let b: Vec<f64> = walk(0x0f0f_1e1e_2d2d_3c3c, step, n).iter().map(|x| level + x).collect();
    let mut s = 0.0;
    let mut prev = 0.0;
    let a = b
        .iter()
        .zip(&noise)
        .map(|(bb, w)| {
            s = (1.0 - theta) * s + (w - prev);
            prev = *w;
            bb + s
        })
        .collect();
    (a, b)
}

fn bars_of(c: &[f64]) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| Bar::ohlcv(i as i64 * 60_000, x, x + 0.5, x - 0.5, x, 1.0))
        .collect()
}

const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];
const WINDOWS: [usize; 6] = [2, 3, 5, 20, 64, 200];

fn same(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

/// Largest error against the reference, relative to `max(|want|, 1)`; NaN must be NaN in both.
fn close(name: &str, got: &[f64], want: &[f64], tol: f64) -> f64 {
    assert_eq!(got.len(), want.len(), "{name}: length");
    let mut worst = 0.0_f64;
    for (i, (&g, &w)) in got.iter().zip(want).enumerate() {
        if g.is_nan() || w.is_nan() {
            assert!(g.is_nan() && w.is_nan(), "{name}[{i}]: got {g}, want {w} (one is NaN)");
            continue;
        }
        let err = (g - w).abs() / w.abs().max(1.0);
        assert!(err <= tol, "{name}[{i}]: got {g}, want {w}, error {err:e}");
        worst = worst.max(err);
    }
    worst
}

fn nan_positions(v: &[f64]) -> Vec<bool> {
    v.iter().map(|x| x.is_nan()).collect()
}

// ------------------------------------------------------------------ textbook references

struct Stats {
    cov: f64,
    vu: f64,
    vv: f64,
    const_u: bool,
    const_v: bool,
}

fn is_const(w: &[f64]) -> bool {
    w.iter().all(|x| x.to_bits() == w[0].to_bits())
}

/// Two-pass population moments of a window pair, on values shifted by the first.
fn stats(u: &[f64], v: &[f64]) -> Stats {
    let p = u.len() as f64;
    let du: Vec<f64> = u.iter().map(|x| x - u[0]).collect();
    let dv: Vec<f64> = v.iter().map(|x| x - v[0]).collect();
    let (mu, mv) = (du.iter().sum::<f64>() / p, dv.iter().sum::<f64>() / p);
    let cov = du.iter().zip(&dv).map(|(x, y)| (x - mu) * (y - mv)).sum::<f64>() / p;
    let vu = du.iter().map(|x| (x - mu) * (x - mu)).sum::<f64>() / p;
    let vv = dv.iter().map(|y| (y - mv) * (y - mv)).sum::<f64>() / p;
    Stats { cov, vu, vv, const_u: is_const(u), const_v: is_const(v) }
}

fn ratio_ref(a: &[f64], b: &[f64]) -> Vec<f64> {
    a.iter().zip(b).map(|(x, y)| if *y != 0.0 { x / y } else { f64::NAN }).collect()
}

fn spread_ref(a: &[f64], b: &[f64], log: bool) -> Vec<f64> {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            if !log {
                x - y
            } else if *x > 0.0 && *y > 0.0 {
                libm::log(*x) - libm::log(*y)
            } else {
                f64::NAN
            }
        })
        .collect()
}

/// A non-finite value inside the window makes the output `NaN`, and only while it is inside.
fn zscore_ref(a: &[f64], b: &[f64], n: usize, beta: f64) -> Vec<f64> {
    let s: Vec<f64> = a.iter().zip(b).map(|(x, y)| x - beta * y).collect();
    (0..s.len())
        .map(|i| {
            if i + 1 < n {
                return f64::NAN;
            }
            let w = &s[i + 1 - n..=i];
            if w.iter().any(|x| !x.is_finite()) || is_const(w) {
                return f64::NAN;
            }
            let st = stats(w, w);
            ((s[i] - w[0]) - w.iter().map(|x| x - w[0]).sum::<f64>() / n as f64) / st.vu.sqrt()
        })
        .collect()
}

fn pearson_ref(st: &Stats) -> f64 {
    if st.const_u || st.const_v {
        return f64::NAN;
    }
    (st.cov / (st.vu * st.vv).sqrt()).clamp(-1.0, 1.0)
}

fn correl_ref(a: &[f64], b: &[f64], n: usize) -> Vec<f64> {
    (0..a.len())
        .map(|i| {
            if i + 1 < n {
                return f64::NAN;
            }
            let (wa, wb) = (&a[i + 1 - n..=i], &b[i + 1 - n..=i]);
            if wa.iter().chain(wb).any(|x| !x.is_finite()) {
                return f64::NAN;
            }
            pearson_ref(&stats(wa, wb))
        })
        .collect()
}

/// For each bar the last `n` consecutive defined return pairs ending there, if there are `n`.
fn windows_of(ra: &[f64], rb: &[f64], n: usize) -> Vec<Option<(Vec<f64>, Vec<f64>)>> {
    let mut run = 0usize;
    (0..ra.len())
        .map(|i| {
            if ra[i].is_nan() || rb[i].is_nan() {
                run = 0;
                return None;
            }
            run += 1;
            (run >= n).then(|| (ra[i + 1 - n..=i].to_vec(), rb[i + 1 - n..=i].to_vec()))
        })
        .collect()
}

fn simple_returns(c: &[f64]) -> Vec<f64> {
    (0..c.len())
        .map(|i| if i > 0 && c[i - 1] != 0.0 { (c[i] - c[i - 1]) / c[i - 1] } else { f64::NAN })
        .collect()
}

fn log_returns(c: &[f64]) -> Vec<f64> {
    (0..c.len())
        .map(|i| {
            if i > 0 && c[i] > 0.0 && c[i - 1] > 0.0 {
                libm::log(c[i] / c[i - 1])
            } else {
                f64::NAN
            }
        })
        .collect()
}

fn beta_ref(a: &[f64], b: &[f64], n: usize) -> Vec<f64> {
    windows_of(&simple_returns(a), &simple_returns(b), n)
        .into_iter()
        .map(|w| match w {
            Some((wa, wb)) => {
                let st = stats(&wa, &wb);
                if st.const_v { f64::NAN } else { st.cov / st.vv }
            }
            None => f64::NAN,
        })
        .collect()
}

fn correl_log_ref(a: &[f64], b: &[f64], n: usize) -> Vec<f64> {
    windows_of(&log_returns(a), &log_returns(b), n)
        .into_iter()
        .map(|w| match w {
            Some((wa, wb)) => pearson_ref(&stats(&wa, &wb)),
            None => f64::NAN,
        })
        .collect()
}

fn half_life_ref(a: &[f64], b: &[f64], n: usize, beta: f64) -> Vec<f64> {
    let s: Vec<f64> = a.iter().zip(b).map(|(x, y)| x - beta * y).collect();
    (0..s.len())
        .map(|i| {
            if i < n {
                return f64::NAN;
            }
            let lag: Vec<f64> = s[i - n..i].to_vec();
            let dif: Vec<f64> = (i - n + 1..=i).map(|t| s[t] - s[t - 1]).collect();
            if lag.iter().chain(&dif).any(|x| !x.is_finite()) || is_const(&lag) {
                return f64::NAN;
            }
            let st = stats(&lag, &dif);
            let lambda = st.cov / st.vu;
            if lambda < 0.0 { -std::f64::consts::LN_2 / lambda } else { f64::NAN }
        })
        .collect()
}

// ------------------------------------------------------------------ the kernels this replaced

fn old_zscore(ca: &[f64], cb: &[f64], period: usize, beta: f64) -> Vec<f64> {
    let n = ca.len();
    let s: Vec<f64> = (0..n).map(|i| ca[i] - beta * cb[i]).collect();
    let mut out = vec![f64::NAN; n];
    let (mut run_sum, mut run_sum2) = (0.0, 0.0);
    for i in 0..n {
        run_sum += s[i];
        run_sum2 += s[i] * s[i];
        if i >= period {
            run_sum -= s[i - period];
            run_sum2 -= s[i - period] * s[i - period];
        }
        if i + 1 >= period {
            let mean = run_sum / period as f64;
            let var = run_sum2 / period as f64 - mean * mean;
            let sd = var.max(0.0).sqrt();
            if sd != 0.0 {
                out[i] = (s[i] - mean) / sd;
            }
        }
    }
    out
}

fn old_beta(ca: &[f64], cb: &[f64], period: usize) -> Vec<f64> {
    let n = ca.len();
    let mut out = vec![f64::NAN; n];
    let (mut ret_a, mut ret_b) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    for i in 1..n {
        if ca[i - 1] != 0.0 {
            ret_a[i] = (ca[i] - ca[i - 1]) / ca[i - 1];
        }
        if cb[i - 1] != 0.0 {
            ret_b[i] = (cb[i] - cb[i - 1]) / cb[i - 1];
        }
    }
    let (mut buf_a, mut buf_b): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    for i in 0..n {
        if !ret_a[i].is_nan() && !ret_b[i].is_nan() {
            buf_a.push(ret_a[i]);
            buf_b.push(ret_b[i]);
        } else {
            buf_a.clear();
            buf_b.clear();
            continue;
        }
        if buf_a.len() > period {
            buf_a.remove(0);
            buf_b.remove(0);
        }
        if buf_a.len() == period {
            let mean_a = buf_a.iter().sum::<f64>() / period as f64;
            let mean_b = buf_b.iter().sum::<f64>() / period as f64;
            let cov = (0..period).map(|j| (buf_a[j] - mean_a) * (buf_b[j] - mean_b)).sum::<f64>()
                / period as f64;
            let vb = (0..period).map(|j| (buf_b[j] - mean_b) * (buf_b[j] - mean_b)).sum::<f64>()
                / period as f64;
            if vb != 0.0 {
                out[i] = cov / vb;
            }
        }
    }
    out
}

fn old_correl(ca: &[f64], cb: &[f64], period: usize) -> Vec<f64> {
    let n = ca.len();
    let mut out = vec![f64::NAN; n];
    let (mut run_a, mut run_b, mut run_a2, mut run_b2, mut run_ab) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for i in 0..n {
        run_a += ca[i];
        run_b += cb[i];
        run_a2 += ca[i] * ca[i];
        run_b2 += cb[i] * cb[i];
        run_ab += ca[i] * cb[i];
        if i >= period {
            let (oa, ob) = (ca[i - period], cb[i - period]);
            run_a -= oa;
            run_b -= ob;
            run_a2 -= oa * oa;
            run_b2 -= ob * ob;
            run_ab -= oa * ob;
        }
        if i + 1 >= period {
            let p = period as f64;
            let num = p * run_ab - run_a * run_b;
            let denom =
                ((p * run_a2 - run_a * run_a) * (p * run_b2 - run_b * run_b)).max(0.0).sqrt();
            if denom != 0.0 {
                out[i] = (num / denom).clamp(-1.0, 1.0);
            }
        }
    }
    out
}

fn old_correl_log(ca: &[f64], cb: &[f64], period: usize) -> Vec<f64> {
    let n = ca.len();
    let (mut lr_a, mut lr_b) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    for i in 1..n {
        if ca[i] > 0.0 && ca[i - 1] > 0.0 {
            lr_a[i] = libm::log(ca[i] / ca[i - 1]);
        }
        if cb[i] > 0.0 && cb[i - 1] > 0.0 {
            lr_b[i] = libm::log(cb[i] / cb[i - 1]);
        }
    }
    let mut out = vec![f64::NAN; n];
    let (mut buf_a, mut buf_b): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    for i in 1..n {
        if !lr_a[i].is_nan() && !lr_b[i].is_nan() {
            buf_a.push(lr_a[i]);
            buf_b.push(lr_b[i]);
        } else {
            buf_a.clear();
            buf_b.clear();
            continue;
        }
        if buf_a.len() > period {
            buf_a.remove(0);
            buf_b.remove(0);
        }
        if buf_a.len() == period {
            let p = period as f64;
            let sa: f64 = buf_a.iter().sum();
            let sb: f64 = buf_b.iter().sum();
            let sa2: f64 = buf_a.iter().map(|x| x * x).sum();
            let sb2: f64 = buf_b.iter().map(|x| x * x).sum();
            let sab: f64 = (0..period).map(|j| buf_a[j] * buf_b[j]).sum();
            let num = p * sab - sa * sb;
            let denom = ((p * sa2 - sa * sa) * (p * sb2 - sb * sb)).max(0.0).sqrt();
            if denom != 0.0 {
                out[i] = (num / denom).clamp(-1.0, 1.0);
            }
        }
    }
    out
}

fn old_half_life(ca: &[f64], cb: &[f64], period: usize, beta: f64) -> Vec<f64> {
    let n = ca.len();
    let mut out = vec![f64::NAN; n];
    if period == 0 {
        return out;
    }
    let s: Vec<f64> = (0..n).map(|i| ca[i] - beta * cb[i]).collect();
    let (mut lag, mut dif): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    for i in 1..n {
        lag.push(s[i - 1]);
        dif.push(s[i] - s[i - 1]);
        if lag.len() > period {
            lag.remove(0);
            dif.remove(0);
        }
        if lag.len() == period {
            let p = period as f64;
            let mean_l = lag.iter().sum::<f64>() / p;
            let mean_d = dif.iter().sum::<f64>() / p;
            let cov = (0..period).map(|j| (lag[j] - mean_l) * (dif[j] - mean_d)).sum::<f64>() / p;
            let var_l = (0..period).map(|j| (lag[j] - mean_l) * (lag[j] - mean_l)).sum::<f64>() / p;
            if var_l != 0.0 {
                let lambda = cov / var_l;
                if lambda < 0.0 {
                    out[i] = -std::f64::consts::LN_2 / lambda;
                }
            }
        }
    }
    out
}

fn old_kalman(ca: &[f64], cb: &[f64], delta: f64, ve: f64) -> Vec<f64> {
    let n = ca.len();
    let mut out = vec![f64::NAN; n];
    let denom = 1.0 - delta;
    let vw = if denom != 0.0 { delta / denom } else { 0.0 };
    let (mut x_beta, mut x_alpha) = (0.0f64, 0.0f64);
    let (mut p00, mut p01, mut p10, mut p11) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let (r00, r01, r10, r11) = (p00 + vw, p01, p10, p11 + vw);
        let f0 = cb[i];
        let yhat = f0 * x_beta + x_alpha;
        let rf0 = r00 * f0 + r01;
        let rf1 = r10 * f0 + r11;
        let q = f0 * rf0 + rf1 + ve;
        if !q.is_finite() || q == 0.0 {
            continue;
        }
        let e = ca[i] - yhat;
        let (k0, k1) = (rf0 / q, rf1 / q);
        x_beta += k0 * e;
        x_alpha += k1 * e;
        p00 = r00 - k0 * rf0;
        p01 = r01 - k0 * rf1;
        p10 = r10 - k1 * rf0;
        p11 = r11 - k1 * rf1;
        out[i] = x_beta;
    }
    out
}

// ------------------------------------------------------------------ against the textbook

#[test]
fn ratio_and_spread_are_the_same_expressions() {
    for (level, step) in SCALES {
        let (a, b) = pair(level, step, 3000);
        assert!(same(&series::ratio(&a, &b), &ratio_ref(&a, &b)), "ratio level {level}");
        assert!(same(&series::spread(&a, &b, 0.0).unwrap(), &spread_ref(&a, &b, false)));
        assert!(same(&series::spread(&a, &b, 1.0).unwrap(), &spread_ref(&a, &b, true)));
    }
    // A zero benchmark close is NaN, a non-positive close is NaN in the log spread.
    let (a, mut b) = pair(100.0, 2.0, 50);
    b[10] = 0.0;
    assert!(series::ratio(&a, &b)[10].is_nan());
    let mut a2 = a.clone();
    a2[20] = -1.0;
    assert!(series::spread(&a2, &b, 1.0).unwrap()[20].is_nan());
    assert!(series::spread(&a2, &b, 0.0).unwrap()[20].is_finite());
}

#[test]
fn spread_zscore_matches_a_window_folded_from_scratch() {
    let mut worst = 0.0_f64;
    for (level, step) in SCALES {
        let (a, b) = pair(level, step, 3000);
        for n in WINDOWS {
            for beta in [1.0, 0.5, -2.0] {
                let got = series::spread_zscore(&a, &b, n as f64, beta).unwrap();
                let want = zscore_ref(&a, &b, n, beta);
                worst = worst.max(close(
                    &format!("spread_zscore({n},{beta}) {level}"),
                    &got,
                    &want,
                    1e-9,
                ));
            }
        }
    }
    eprintln!("MAXERR spread_zscore {worst:e}");
}

#[test]
fn beta_matches_a_window_folded_from_scratch() {
    let mut worst = 0.0_f64;
    for (level, step) in SCALES {
        let (a, b) = pair(level, step, 3000);
        for n in WINDOWS {
            let got = series::beta(&a, &b, n as f64).unwrap();
            worst =
                worst.max(close(&format!("beta({n}) {level}"), &got, &beta_ref(&a, &b, n), 1e-9));
        }
    }
    eprintln!("MAXERR beta {worst:e}");
}

#[test]
fn correl_matches_a_window_folded_from_scratch() {
    let mut worst = 0.0_f64;
    for (level, step) in SCALES {
        let (a, b) = pair(level, step, 3000);
        for n in WINDOWS {
            let got = series::correl(&a, &b, n as f64).unwrap();
            worst = worst.max(close(
                &format!("correl({n}) {level}"),
                &got,
                &correl_ref(&a, &b, n),
                1e-9,
            ));
        }
    }
    eprintln!("MAXERR correl {worst:e}");
}

#[test]
fn correl_log_matches_a_window_folded_from_scratch() {
    let mut worst = 0.0_f64;
    for (level, step) in SCALES {
        let (a, b) = pair(level, step, 3000);
        for n in WINDOWS {
            let got = series::correl_log(&a, &b, n as f64).unwrap();
            let want = correl_log_ref(&a, &b, n);
            worst = worst.max(close(&format!("correl_log({n}) {level}"), &got, &want, 1e-9));
        }
    }
    eprintln!("MAXERR correl_log {worst:e}");
}

#[test]
fn half_life_matches_a_window_folded_from_scratch() {
    let mut worst = 0.0_f64;
    // Compared as the slope `lambda = -ln 2 / half_life`: a window with a slope near zero has a huge
    // half-life, and the half-life is then as ill-conditioned as the slope itself (a covariance that
    // is a millionth of the variance keeps 10 digits, not 15).
    let slope = |v: &[f64]| -> Vec<f64> { v.iter().map(|h| -std::f64::consts::LN_2 / h).collect() };
    for (level, step) in SCALES {
        for theta in [0.05, 0.3] {
            let (a, b) = reverting(level, step, 3000, theta);
            for n in WINDOWS {
                for beta in [1.0, 0.7] {
                    let got = series::half_life(&a, &b, n as f64, beta).unwrap();
                    let want = half_life_ref(&a, &b, n, beta);
                    let name = format!("half_life({n},{beta}) theta {theta} level {level}");
                    worst = worst.max(close(&name, &slope(&got), &slope(&want), 1e-9));
                }
            }
        }
    }
    eprintln!("MAXERR half_life (as slope) {worst:e}");
    // The reverting data really does give numbers (the comparison above is not all NaN).
    let (a, b) = reverting(100.0, 2.0, 3000, 0.3);
    let got = series::half_life(&a, &b, 64.0, 1.0).unwrap();
    assert!(got.iter().filter(|x| x.is_finite()).count() > 2000);
}

#[test]
fn kalman_beta_is_the_old_recurrence_bit_for_bit() {
    for (level, step) in SCALES {
        let (a, b) = pair(level, step, 3000);
        for (delta, ve) in [(0.0001, 0.001), (0.01, 0.5), (0.1, 1.0), (1e-6, 1e-6)] {
            let got = series::kalman_beta(&a, &b, delta, ve).unwrap();
            assert!(same(&got, &old_kalman(&a, &b, delta, ve)), "kalman({delta},{ve}) {level}");
        }
    }
    // A benchmark close that makes the innovation variance unusable holds the state, bar NaN.
    let (a, mut b) = pair(100.0, 2.0, 100);
    b[30] = f64::NAN;
    let got = series::kalman_beta(&a, &b, 0.0001, 0.001).unwrap();
    assert!(got[30].is_nan() && got[29].is_finite() && got[31].is_finite());
}

// ------------------------------------------------------------------ against the old kernels

/// A series with a NaN in each leg, a zero and a negative dropped in. (No flat stretch: on one the old
/// running sums of `spread_zscore` and `correl` answered a rounding residue, which is the defect the
/// flat-window test below pins the new answer to.)
fn rough(n: usize) -> (Vec<f64>, Vec<f64>) {
    let (mut a, mut b) = pair(100.0, 2.0, n);
    a[200] = f64::NAN;
    b[700] = f64::NAN;
    b[400] = 0.0;
    a[900] = -5.0;
    (a, b)
}

#[test]
fn nan_positions_are_those_of_the_old_kernels() {
    let (a, b) = rough(2000);
    let clean = pair(100.0, 2.0, 2000);
    for (name, (a, b)) in [("rough", (&a, &b)), ("clean", (&clean.0, &clean.1))] {
        for n in WINDOWS {
            let nf = n as f64;
            let cases: [(&str, Vec<f64>, Vec<f64>); 6] = [
                (
                    "spread_zscore",
                    series::spread_zscore(a, b, nf, 1.0).unwrap(),
                    old_zscore(a, b, n, 1.0),
                ),
                ("beta", series::beta(a, b, nf).unwrap(), old_beta(a, b, n)),
                ("correl", series::correl(a, b, nf).unwrap(), old_correl(a, b, n)),
                ("correl_log", series::correl_log(a, b, nf).unwrap(), old_correl_log(a, b, n)),
                (
                    "half_life",
                    series::half_life(a, b, nf, 1.0).unwrap(),
                    old_half_life(a, b, n, 1.0),
                ),
                (
                    "kalman_beta",
                    series::kalman_beta(a, b, 0.0001, 0.001).unwrap(),
                    old_kalman(a, b, 0.0001, 0.001),
                ),
            ];
            for (ind, got, old) in cases {
                // The old running sums of `spread_zscore` and `correl` were poisoned for good by the
                // first non-finite close (changed on purpose: they recover now), so against them
                // only the bars before it are compared.
                let first_bad =
                    a.iter().zip(b.iter()).position(|(x, y)| !x.is_finite() || !y.is_finite());
                let upto = match (ind, first_bad) {
                    ("spread_zscore" | "correl", Some(i)) => i,
                    _ => got.len(),
                };
                assert_eq!(
                    nan_positions(&got[..upto]),
                    nan_positions(&old[..upto]),
                    "{ind}({n}) on the {name} series: NaN positions"
                );
                // The old kernels are noisy themselves (running sums, means by division): a window of
                // a few values whose members are close has a variance the old sums cannot resolve
                // (`spread_zscore(2)` answered -1.0025 where the answer is exactly -1), so the values
                // are compared from 20 up, where a window has the spread to be resolved.
                if n >= 20 {
                    close(&format!("{ind}({n}) {name} vs old"), &got[..upto], &old[..upto], 1e-6);
                }
            }
        }
    }
}

#[test]
fn a_non_finite_close_leaves_every_windowed_pair_indicator_exact_after_it_left() {
    // Changed on purpose: `spread_zscore` and `correl` used to be poisoned for good by one
    // non-finite close (running sums that were never rebuilt). Now, like the other three, they are
    // `NaN` while it is inside the window and exact again the update after it left.
    for (level, step) in SCALES {
        let (mut a, mut b) = pair(level, step, 900);
        a[200] = f64::NAN;
        b[500] = f64::INFINITY;
        for n in [2usize, 5, 20, 64] {
            let nf = n as f64;
            let z = series::spread_zscore(&a, &b, nf, 1.0).unwrap();
            let c = series::correl(&a, &b, nf).unwrap();
            for (name, v) in [("spread_zscore", &z), ("correl", &c)] {
                assert!(v[100].is_finite(), "{name}({n}) before the NaN");
                assert!(v[200..200 + n].iter().all(|x| x.is_nan()), "{name}({n}) NaN inside");
                assert!(v[200 + n..500].iter().all(|x| x.is_finite()), "{name}({n}) recovers");
                assert!(v[500..500 + n].iter().all(|x| x.is_nan()), "{name}({n}) infinity inside");
                assert!(v[500 + n..].iter().all(|x| x.is_finite()), "{name}({n}) recovers again");
            }
            close(
                &format!("spread_zscore({n}) NaN {level}"),
                &z,
                &zscore_ref(&a, &b, n, 1.0),
                1e-9,
            );
            close(&format!("correl({n}) NaN {level}"), &c, &correl_ref(&a, &b, n), 1e-9);
            // The window kernels recover once the NaN has left the window.
            let h = series::half_life(&a, &b, nf, 1.0).unwrap();
            let r = series::beta(&a, &b, nf).unwrap();
            let l = series::correl_log(&a, &b, nf).unwrap();
            for (name, v) in [("half_life", &h), ("beta", &r), ("correl_log", &l)] {
                assert!(v[200..200 + n].iter().all(|x| x.is_nan()), "{name}({n}) NaN inside");
                assert!(v[300..400].iter().any(|x| x.is_finite()), "{name}({n}) recovers");
            }
        }
    }
}

#[test]
fn a_flat_window_has_exactly_zero_variance_whatever_came_before() {
    for flat in [100.0, 0.1, 123456.789] {
        let (mut a, mut b) = pair(1.0e6, 0.5, 900);
        for i in 700..900 {
            a[i] = flat;
            b[i] = flat * 0.5;
        }
        for n in [2usize, 20, 200] {
            let nf = n as f64;
            let z = series::spread_zscore(&a, &b, nf, 1.0).unwrap();
            let c = series::correl(&a, &b, nf).unwrap();
            let r = series::beta(&a, &b, nf).unwrap();
            let l = series::correl_log(&a, &b, nf).unwrap();
            let h = series::half_life(&a, &b, nf, 1.0).unwrap();
            for i in 700 + n + 1..900 {
                assert!(z[i].is_nan(), "spread_zscore({n}) at {i} flat at {flat}");
                assert!(c[i].is_nan(), "correl({n}) at {i} flat at {flat}");
                assert!(r[i].is_nan(), "beta({n}) at {i} flat at {flat}");
                assert!(l[i].is_nan(), "correl_log({n}) at {i} flat at {flat}");
                assert!(h[i].is_nan(), "half_life({n}) at {i} flat at {flat}");
            }
        }
    }
}

// ------------------------------------------------------------------ every way in, the same bits

macro_rules! by_values {
    ($ty:ty, $p:expr, $a:expr, $b:expr) => {{
        let mut s = <$ty>::with_params($p);
        $a.iter().zip($b).map(|(&x, &y)| s.on_pair_values(x, y)).collect::<Vec<f64>>()
    }};
}

fn values(name: &str, p: &[f64], a: &[f64], b: &[f64]) -> Vec<f64> {
    match name {
        "ratio" => by_values!(Ratio, p, a, b),
        "spread" => by_values!(Spread, p, a, b),
        "spread_zscore" => by_values!(SpreadZscore, p, a, b),
        "beta" => by_values!(Beta, p, a, b),
        "correl" => by_values!(Correl, p, a, b),
        "correl_log" => by_values!(CorrelLog, p, a, b),
        "half_life" => by_values!(HalfLife, p, a, b),
        "kalman_beta" => by_values!(KalmanBeta, p, a, b),
        _ => unreachable!(),
    }
}

fn boxed(name: &str, p: &[f64]) -> Box<dyn PairIndicator> {
    match name {
        "ratio" => Box::new(Ratio::with_params(p)),
        "spread" => Box::new(Spread::with_params(p)),
        "spread_zscore" => Box::new(SpreadZscore::with_params(p)),
        "beta" => Box::new(Beta::with_params(p)),
        "correl" => Box::new(Correl::with_params(p)),
        "correl_log" => Box::new(CorrelLog::with_params(p)),
        "half_life" => Box::new(HalfLife::with_params(p)),
        "kalman_beta" => Box::new(KalmanBeta::with_params(p)),
        _ => unreachable!(),
    }
}

fn by_series(name: &str, p: &[f64], a: &[f64], b: &[f64]) -> Option<Vec<f64>> {
    match name {
        "ratio" => Some(series::ratio(a, b)),
        "spread" => series::spread(a, b, p[0]).ok(),
        "spread_zscore" => series::spread_zscore(a, b, p[0], p[1]).ok(),
        "beta" => series::beta(a, b, p[0]).ok(),
        "correl" => series::correl(a, b, p[0]).ok(),
        "correl_log" => series::correl_log(a, b, p[0]).ok(),
        "half_life" => series::half_life(a, b, p[0], p[1]).ok(),
        "kalman_beta" => series::kalman_beta(a, b, p[0], p[1]).ok(),
        _ => unreachable!(),
    }
}

/// The inputs of the bit-equality tests: clean, a NaN in either, a zero and a negative, an infinity.
fn variants(n: usize) -> Vec<(&'static str, Vec<f64>, Vec<f64>)> {
    let (a, b) = pair(100.0, 2.0, n);
    let mut out = vec![("clean", a.clone(), b.clone())];
    let nan_a = |at: usize| {
        let mut x = a.clone();
        if at < n {
            x[at] = f64::NAN;
        }
        x
    };
    out.push(("nan primary", nan_a(40), b.clone()));
    let mut bn = b.clone();
    if 90 < n {
        bn[90] = f64::NAN;
    }
    out.push(("nan benchmark", a.clone(), bn));
    let (mut az, mut bz) = (a.clone(), b.clone());
    if 120 < n {
        bz[60.min(n - 1)] = 0.0;
        az[120] = -1.0;
    }
    out.push(("zero and negative", az, bz));
    let mut ai = a.clone();
    if 70 < n {
        ai[70] = f64::INFINITY;
    }
    out.push(("infinity", ai, b));
    out
}

fn check_ways(name: &'static str, p: &[f64], via_series: bool) {
    let n = p.first().map_or(20, |x| *x as usize);
    for len in [0, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600] {
        for (what, a, b) in variants(len.max(130)) {
            let (a, b) = (&a[..len], &b[..len]);
            let (ba, bb) = (bars_of(a), bars_of(b));
            let mut ind = boxed(name, p);
            let streamed: Vec<f64> =
                ba.iter().zip(&bb).map(|(x, y)| ind.on_pair(x, y)[0]).collect();
            let want = ind.vectorize_pair(&ba, &bb).remove(0);
            let tag = format!("{name}{p:?} len {len} {what}");
            assert!(same(&streamed, &want), "{tag}: on_pair vs vectorize_pair");
            assert!(same(&values(name, p, a, b), &want), "{tag}: on_pair_values vs vectorize_pair");
            if via_series {
                let s = by_series(name, p, a, b).expect("in range");
                assert!(same(&s, &want), "{tag}: series vs vectorize_pair");
            }
        }
    }
}

#[test]
fn every_way_in_gives_the_same_bits() {
    check_ways("ratio", &[], true);
    check_ways("spread", &[0.0], true);
    check_ways("spread", &[1.0], true);
    for n in [2.0, 3.0, 5.0, 20.0, 64.0, 100.0, 199.0, 200.0] {
        for beta in [1.0, -2.0] {
            check_ways("spread_zscore", &[n, beta], true);
            check_ways("half_life", &[n, beta], true);
        }
        check_ways("beta", &[n], true);
        check_ways("correl", &[n], true);
        check_ways("correl_log", &[n], true);
    }
    for (d, v) in [(0.0001, 0.001), (0.1, 1.0), (1e-6, 1e-6)] {
        check_ways("kalman_beta", &[d, v], true);
    }
}

#[test]
fn periods_past_the_registry_range_still_agree_between_stream_and_batch() {
    // `with_params` is not clamped; these are the periods that put the rebuild at every lap (511,
    // 512, 513) and past it (1000). `series::*` refuses them, so they go through the types only.
    for n in [1.0, 511.0, 512.0, 513.0, 1000.0] {
        for name in ["spread_zscore", "beta", "correl", "correl_log", "half_life"] {
            let p: Vec<f64> =
                if name == "spread_zscore" || name == "half_life" { vec![n, 1.0] } else { vec![n] };
            check_ways(name, &p, false);
        }
    }
}

#[test]
fn value_follows_the_last_output_and_reset_starts_again() {
    let (a, b) = pair(100.0, 2.0, 300);
    let (ba, bb) = (bars_of(&a), bars_of(&b));
    for name in [
        "ratio",
        "spread",
        "spread_zscore",
        "beta",
        "correl",
        "correl_log",
        "half_life",
        "kalman_beta",
    ] {
        let p: &[f64] = &[];
        let mut ind = boxed(name, p);
        assert!(ind.value()[0].is_nan(), "{name}: value() before any bar");
        let first: Vec<f64> = ba.iter().zip(&bb).map(|(x, y)| ind.on_pair(x, y)[0]).collect();
        assert!(same(&ind.value(), &first[first.len() - 1..]), "{name}: value()");
        ind.reset();
        assert!(ind.value()[0].is_nan(), "{name}: value() after reset");
        let second: Vec<f64> = ba.iter().zip(&bb).map(|(x, y)| ind.on_pair(x, y)[0]).collect();
        assert!(same(&first, &second), "{name}: reset");
    }
}

// ------------------------------------------------------------------ edges

#[test]
fn degenerate_raw_parameters_never_wrap_or_allocate_the_address_space() {
    let (a, b) = pair(100.0, 2.0, 200);
    for p in [0.0, f64::NAN, f64::INFINITY, -3.0, 1.0e18] {
        for name in ["spread_zscore", "beta", "correl", "correl_log", "half_life"] {
            let params: Vec<f64> =
                if name == "spread_zscore" || name == "half_life" { vec![p, 1.0] } else { vec![p] };
            let out = values(name, &params, &a, &b);
            assert_eq!(out.len(), 200);
            // A window of 0 never leaves warm-up (and so do a window longer than the series).
            assert!(out.iter().all(|x| x.is_nan()), "{name}({p}) answered");
        }
    }
}

#[test]
fn a_period_of_one_never_has_a_variance() {
    let (a, b) = pair(100.0, 2.0, 100);
    for name in ["spread_zscore", "beta", "correl", "correl_log"] {
        assert!(values(name, &[1.0, 1.0], &a, &b).iter().all(|x| x.is_nan()), "{name}(1)");
    }
}

#[test]
fn series_refuses_what_the_pair_registry_refuses_and_stops_at_the_shorter_slice() {
    let (a, b) = pair(100.0, 2.0, 50);
    for bad in [1.0, 201.0, f64::NAN] {
        assert!(series::beta(&a, &b, bad).is_err());
        assert!(series::correl(&a, &b, bad).is_err());
        assert!(series::correl_log(&a, &b, bad).is_err());
        assert!(series::spread_zscore(&a, &b, bad, 1.0).is_err());
        assert!(series::half_life(&a, &b, bad, 1.0).is_err());
    }
    assert!(series::spread(&a, &b, 2.0).is_err());
    assert!(series::spread_zscore(&a, &b, 20.0, 5.5).is_err());
    assert!(series::kalman_beta(&a, &b, 0.0, 0.001).is_err());
    assert!(series::kalman_beta(&a, &b, 0.0001, 2.0).is_err());
    let e = series::beta(&a, &b, 1.0).unwrap_err();
    assert_eq!((e.indicator, e.param), ("beta", "period"));
    // The shorter slice decides the length (the old batch kernel indexed past a shorter benchmark).
    assert_eq!(series::ratio(&a, &b[..30]).len(), 30);
    assert_eq!(series::correl(&a[..10], &b, 5.0).unwrap().len(), 10);
}

#[test]
fn the_registry_defaults_are_the_new_defaults() {
    let (a, b) = pair(100.0, 2.0, 300);
    let (ba, bb) = (bars_of(&a), bars_of(&b));
    for meta in vike_indicators::pair_registry() {
        let defaults: Vec<f64> = meta.params.iter().map(|p| p.default).collect();
        let via_registry = (meta.make)().vectorize_pair(&ba, &bb).remove(0);
        let via_series = by_series(meta.name, &defaults, &a, &b).expect("defaults are in range");
        assert!(same(&via_registry, &via_series), "{}", meta.name);
    }
}

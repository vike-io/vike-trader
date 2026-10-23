//! `linearreg`, `linearreg_slope`, `linearreg_angle`, `linearreg_intercept`, `tsf`, `var`, `zscore`,
//! `skew`, `kurtosis`, `mad`, `std_error`, `std_error_bands`, `rank_correlation`, `r_squared`, `hurst`
//! and `autocorr`: the fast cores against a textbook reference, and the stream (`next(&Bar)`), the
//! plain-number entry (`next_value`), the batch (`vectorize`) and the slice call (`series::*`) against
//! each other, bit for bit.
//!
//! The reference does each step the long way: a window folded from scratch, centred sums, the
//! divisions. The cores carry running sums of the window, so the last bits can differ from the
//! reference; the tolerance is 1e-9 relative on three scales of series (see `close`). Where the
//! arithmetic is only integer counting or a copy of the old kernel (`rank_correlation`, `hurst`) the
//! comparison is bit for bit, against a copy of the old kernel kept in this file.

use vike_indicators::indicators::{
    Autocorr, Hurst, Kurtosis, Linearreg, LinearregAngle, LinearregIntercept, LinearregSlope, Mad,
    RSquared, RankCorrelation, Skew, StdError, StdErrorBands, Tsf, Var, Zscore,
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

fn bars_of(c: &[f64]) -> Vec<Bar> {
    c.iter().enumerate().map(|(i, &x)| Bar::ohlcv(i as i64 * 60_000, x, x, x, x, 1.0)).collect()
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
            let den = w.abs().max(1.0e-3 * typ);
            let err = if den == 0.0 { (g - w).abs() } else { (g - w).abs() / den };
            assert!(err <= rel, "{name}[{i}]: got {g}, want {w}, relative error {err:e}");
        }
    }
}

fn same(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

/// The scales: (level, step).
const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];

/// Each scale, clean and with a NaN planted in the middle of the closes.
fn inputs() -> Vec<(String, Vec<f64>)> {
    let mut v = Vec::new();
    for (level, step) in SCALES {
        for nan in [false, true] {
            let mut c = closes(level, step, 3000);
            if nan {
                c[1200] = f64::NAN;
            }
            v.push((format!("level {level} nan {nan}"), c));
        }
    }
    v
}

// ---- the textbook references ------------------------------------------------------------------

/// Each window handed to `f` as is.
fn windows_raw(c: &[f64], n: usize, f: impl Fn(&[f64]) -> f64) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    for i in (n - 1)..c.len() {
        out[i] = f(&c[i + 1 - n..=i]);
    }
    out
}

/// Each window handed to `f` as its differences from its first value (exact for values of the same
/// order), together with that first value: the reference does not lose its own digits to a price near a
/// million. A window with a `NaN` in it stays all `NaN`.
fn windows_lv(c: &[f64], n: usize, f: impl Fn(&[f64], f64) -> f64) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    for i in (n - 1)..c.len() {
        let w = &c[i + 1 - n..=i];
        let d: Vec<f64> = w.iter().map(|v| v - w[0]).collect();
        out[i] = f(&d, w[0]);
    }
    out
}

/// [`windows_lv`] for a statistic that does not depend on the level.
fn windows(c: &[f64], n: usize, f: impl Fn(&[f64]) -> f64) -> Vec<f64> {
    windows_lv(c, n, |d, _| f(d))
}

fn mean(w: &[f64]) -> f64 {
    w.iter().sum::<f64>() / w.len() as f64
}

fn flat(w: &[f64]) -> bool {
    !w[0].is_nan() && w.iter().all(|&v| v == w[0])
}

/// The least-squares line of the window against `0..n`, centred: `(slope, intercept at index 0)`.
fn fit(w: &[f64]) -> (f64, f64) {
    let nf = w.len() as f64;
    let xm = (nf - 1.0) / 2.0;
    let ym = mean(w);
    let sxy: f64 = w.iter().enumerate().map(|(j, y)| (j as f64 - xm) * (y - ym)).sum();
    let sxx: f64 = (0..w.len()).map(|j| (j as f64 - xm) * (j as f64 - xm)).sum();
    let slope = sxy / sxx;
    (slope, ym - slope * xm)
}

/// `[linearreg, slope, angle, intercept, tsf]`.
fn lin_ref(c: &[f64], n: usize) -> [Vec<f64>; 5] {
    let nf = n as f64;
    [
        windows_lv(c, n, |d, w0| {
            let (s, a) = fit(d);
            (a + s * (nf - 1.0)) + w0
        }),
        windows(c, n, |d| fit(d).0),
        windows(c, n, |d| fit(d).0.atan() * (180.0 / std::f64::consts::PI)),
        windows_lv(c, n, |d, w0| fit(d).1 + w0),
        windows_lv(c, n, |d, w0| {
            let (s, a) = fit(d);
            (a + s * nf) + w0
        }),
    ]
}

fn std_error_w(w: &[f64]) -> f64 {
    let n = w.len();
    if n <= 2 {
        return 0.0;
    }
    let (s, a) = fit(w);
    let ss: f64 = w.iter().enumerate().map(|(j, y)| (y - (a + s * j as f64)).powi(2)).sum();
    (ss / (n - 2) as f64).sqrt() / (n as f64).sqrt()
}

fn std_error_ref(c: &[f64], n: usize) -> Vec<f64> {
    windows(c, n, std_error_w)
}

fn bands_ref(c: &[f64], n: usize, mult: f64) -> [Vec<f64>; 3] {
    let nf = n as f64;
    let lr = |d: &[f64], w0: f64| {
        let (s, a) = fit(d);
        (a + s * (nf - 1.0)) + w0
    };
    [
        windows_lv(c, n, |d, w0| lr(d, w0) + mult * std_error_w(d)),
        windows_lv(c, n, lr),
        windows_lv(c, n, |d, w0| lr(d, w0) - mult * std_error_w(d)),
    ]
}

fn r_squared_ref(c: &[f64], n: usize) -> Vec<f64> {
    windows(c, n, |w| {
        if flat(w) {
            return f64::NAN;
        }
        let nf = w.len() as f64;
        let xm = (nf - 1.0) / 2.0;
        let ym = mean(w);
        let sxy: f64 = w.iter().enumerate().map(|(j, y)| (j as f64 - xm) * (y - ym)).sum();
        let sxx: f64 = (0..w.len()).map(|j| (j as f64 - xm) * (j as f64 - xm)).sum();
        let syy: f64 = w.iter().map(|y| (y - ym) * (y - ym)).sum();
        let r2 = sxy * sxy / (sxx * syy);
        // Not `f64::min`: it would turn the `NaN` of a window with a `NaN` in it into 1.
        if r2 > 1.0 { 1.0 } else { r2 }
    })
}

fn var_w(w: &[f64]) -> f64 {
    if flat(w) {
        return 0.0;
    }
    let m = mean(w);
    w.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / w.len() as f64
}

fn zscore_ref(c: &[f64], n: usize) -> Vec<f64> {
    windows(c, n, |w| {
        let sd = var_w(w).sqrt();
        if sd.is_nan() || sd == 0.0 { f64::NAN } else { (w[w.len() - 1] - mean(w)) / sd }
    })
}

fn skew_ref(c: &[f64], n: usize) -> Vec<f64> {
    windows(c, n, |w| {
        if n < 3 {
            return f64::NAN;
        }
        let (nf, m) = (n as f64, mean(w));
        let m2 = w.iter().map(|v| (v - m).powi(2)).sum::<f64>() / nf;
        if m2 == 0.0 {
            return 0.0;
        }
        let m3 = w.iter().map(|v| (v - m).powi(3)).sum::<f64>() / nf;
        m3 / m2.powf(1.5) * (nf * (nf - 1.0)).sqrt() / (nf - 2.0)
    })
}

fn kurtosis_ref(c: &[f64], n: usize) -> Vec<f64> {
    windows(c, n, |w| {
        if n < 4 {
            return f64::NAN;
        }
        let (nf, m) = (n as f64, mean(w));
        let m2 = w.iter().map(|v| (v - m).powi(2)).sum::<f64>() / nf;
        if m2 == 0.0 {
            return 0.0;
        }
        let m4 = w.iter().map(|v| (v - m).powi(4)).sum::<f64>() / nf;
        let g2 = m4 / (m2 * m2);
        let d = (nf - 2.0) * (nf - 3.0);
        (nf + 1.0) * (nf - 1.0) / d * g2 - 3.0 * (nf - 1.0) * (nf - 1.0) / d
    })
}

fn mad_ref(c: &[f64], n: usize) -> Vec<f64> {
    windows(c, n, |w| {
        let m = mean(w);
        w.iter().map(|v| (v - m).abs()).sum::<f64>() / w.len() as f64
    })
}

/// The old rank correlation: ordinal ranks from a stable sort (the old kernel panicked on a NaN in the
/// window; here a NaN window is NaN), the integer sum of squared rank differences.
fn rank_ref(c: &[f64], n: usize) -> Vec<f64> {
    windows_raw(c, n, |w| {
        if w.iter().any(|v| v.is_nan()) {
            return f64::NAN;
        }
        let p = n;
        let mut idx: Vec<usize> = (0..p).collect();
        idx.sort_by(|&a, &b| w[a].partial_cmp(&w[b]).unwrap());
        let mut rank = vec![0i64; p];
        for (r, &j) in idx.iter().enumerate() {
            rank[j] = (r + 1) as i64;
        }
        let mut sum_d2 = 0i64;
        for (j, &r) in rank.iter().enumerate() {
            let d = r - (j as i64 + 1);
            sum_d2 += d * d;
        }
        let denom = (p * (p * p - 1)) as i64;
        if denom != 0 { 100.0 * (1.0 - 6.0 * sum_d2 as f64 / denom as f64) } else { 0.0 }
    })
}

fn autocorr_ref(c: &[f64], n: usize, k: usize) -> Vec<f64> {
    windows(c, n, |w| {
        if k == 0 || n < k + 2 {
            return f64::NAN;
        }
        let pairs = n - k;
        let (x, y) = (&w[..pairs], &w[k..]);
        if flat(x) || flat(y) {
            return f64::NAN;
        }
        let (mx, my) = (mean(x), mean(y));
        let sxy: f64 = x.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum();
        let sxx: f64 = x.iter().map(|a| (a - mx) * (a - mx)).sum();
        let syy: f64 = y.iter().map(|b| (b - my) * (b - my)).sum();
        if sxx > 0.0 && syy > 0.0 { (sxy / (sxx * syy).sqrt()).clamp(-1.0, 1.0) } else { f64::NAN }
    })
}

// ---- the old hurst, kept here: the new core is bit for bit this ------------------------------

fn is_const(w: &[f64]) -> bool {
    match w.split_first() {
        Some((first, rest)) => !first.is_nan() && rest.iter().all(|v| v == first),
        None => false,
    }
}

fn rescaled_range(block: &[f64]) -> Option<f64> {
    if is_const(block) {
        return None;
    }
    let n = block.len() as f64;
    let mean = block.iter().sum::<f64>() / n;
    let (mut run, mut hi, mut lo, mut ss) = (0.0, 0.0f64, 0.0f64, 0.0);
    for r in block {
        let dev = r - mean;
        run += dev;
        hi = hi.max(run);
        lo = lo.min(run);
        ss += dev * dev;
    }
    let sd = (ss / n).sqrt();
    if sd > 0.0 { Some((hi - lo) / sd) } else { None }
}

fn log_log_slope(points: &[(f64, f64)]) -> f64 {
    if points.len() < 2 {
        return f64::NAN;
    }
    let n = points.len() as f64;
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for &(x, y) in points {
        let dx = x - mx;
        sxy += dx * (y - my);
        sxx += dx * dx;
    }
    if sxx > 0.0 { sxy / sxx } else { f64::NAN }
}

fn hurst_window(window: &[f64]) -> f64 {
    let m = window.len();
    if window.iter().any(|r| !r.is_finite()) {
        return f64::NAN;
    }
    let mut points = [(0.0f64, 0.0f64); 64];
    let mut count_points = 0usize;
    let mut size = 8usize;
    while size <= m {
        let blocks = m / size;
        let first = m - blocks * size;
        let (mut total, mut count) = (0.0, 0usize);
        for b in 0..blocks {
            let lo = first + b * size;
            if let Some(rs) = rescaled_range(&window[lo..lo + size]) {
                total += rs;
                count += 1;
            }
        }
        if count > 0 {
            let mean_rs = total / count as f64;
            if mean_rs > 0.0 {
                points[count_points] = (libm::log(size as f64), libm::log(mean_rs));
                count_points += 1;
            }
        }
        size *= 2;
    }
    log_log_slope(&points[..count_points])
}

fn hurst_old(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if n < 17 {
        return out;
    }
    let m = n - 1;
    let rets: Vec<f64> = (1..c.len()).map(|i| libm::log(c[i] / c[i - 1])).collect();
    // `rets[k]` is the return of bar `k + 1`; the first window ends at bar `m`.
    for b in m..c.len() {
        out[b] = hurst_window(&rets[b - m..b]);
    }
    out
}

// ---- tolerance against the textbook ------------------------------------------------------------

const PERIODS: [usize; 7] = [2, 3, 5, 14, 20, 64, 200];

#[test]
fn the_regression_family_matches_the_textbook() {
    for (what, c) in inputs() {
        for n in PERIODS {
            let nf = n as f64;
            let want = lin_ref(&c, n);
            let got = [
                series::linearreg(&c, nf).unwrap(),
                series::linearreg_slope(&c, nf).unwrap(),
                series::linearreg_angle(&c, nf).unwrap(),
                series::linearreg_intercept(&c, nf).unwrap(),
                series::tsf(&c, nf).unwrap(),
            ];
            for (k, name) in
                ["linearreg", "linearreg_slope", "linearreg_angle", "linearreg_intercept", "tsf"]
                    .iter()
                    .enumerate()
            {
                close(&format!("{name}({n}) {what}"), &got[k], &want[k], 1e-9);
            }
            close(
                &format!("std_error({n}) {what}"),
                &series::std_error(&c, nf).unwrap(),
                &std_error_ref(&c, n),
                1e-9,
            );
            close(
                &format!("r_squared({n}) {what}"),
                &series::r_squared(&c, nf).unwrap(),
                &r_squared_ref(&c, n),
                1e-9,
            );
            for mult in [0.5, 2.0, 10.0] {
                let got = series::std_error_bands(&c, nf, mult).unwrap();
                let want = bands_ref(&c, n, mult);
                for k in 0..3 {
                    close(
                        &format!("std_error_bands({n},{mult}) line {k} {what}"),
                        &got[k],
                        &want[k],
                        1e-9,
                    );
                }
            }
        }
    }
}

#[test]
fn the_moments_match_the_textbook() {
    for (what, c) in inputs() {
        for n in PERIODS {
            let nf = n as f64;
            close(
                &format!("var({n}) {what}"),
                &series::var(&c, nf).unwrap(),
                &windows(&c, n, var_w),
                1e-9,
            );
            close(
                &format!("zscore({n}) {what}"),
                &series::zscore(&c, nf).unwrap(),
                &zscore_ref(&c, n),
                1e-9,
            );
            close(
                &format!("skew({n}) {what}"),
                &series::skew(&c, nf).unwrap(),
                &skew_ref(&c, n),
                1e-9,
            );
            close(
                &format!("kurtosis({n}) {what}"),
                &series::kurtosis(&c, nf).unwrap(),
                &kurtosis_ref(&c, n),
                1e-9,
            );
            close(
                &format!("mad({n}) {what}"),
                &series::mad(&c, nf).unwrap(),
                &mad_ref(&c, n),
                1e-9,
            );
        }
    }
}

/// The rank correlation is integer counting: the new core is exactly the old kernel's answer, ties
/// included (the closes are rounded to a grid so that equal values occur).
#[test]
fn rank_correlation_is_the_old_kernel_bit_for_bit() {
    for (what, c) in inputs() {
        let coarse: Vec<f64> =
            c.iter().map(|v| if v.is_nan() { *v } else { (v * 4.0).round() / 4.0 }).collect();
        for n in PERIODS {
            for (label, x) in [("fine", &c), ("ties", &coarse)] {
                let got = series::rank_correlation(x, n as f64).unwrap();
                assert!(same(&got, &rank_ref(x, n)), "rank_correlation({n}) {label} {what}");
            }
        }
    }
}

/// The Hurst exponent is the old kernel's own arithmetic run once per block instead of once per window:
/// the new core is exactly the old answer.
#[test]
fn hurst_is_the_old_kernel_bit_for_bit() {
    for (what, c) in inputs() {
        for n in [17usize, 18, 24, 33, 64, 100, 257, 400] {
            let got = series::hurst(&c, n as f64).unwrap();
            let want = hurst_old(&c, n);
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                assert!(
                    g.to_bits() == w.to_bits() || (g.is_nan() && w.is_nan()),
                    "hurst({n}) {what}[{i}]: got {g}, old {w}"
                );
            }
            // Not vacuous: a good part of the series is defined.
            assert!(
                got.iter().filter(|v| v.is_finite()).count() > 1000 || what.contains("nan true")
            );
        }
    }
}

#[test]
fn autocorr_matches_the_textbook() {
    for (what, c) in inputs() {
        for (n, k) in [(5usize, 1usize), (5, 3), (30, 1), (30, 5), (64, 2), (200, 50), (52, 50)] {
            close(
                &format!("autocorr({n},{k}) {what}"),
                &series::autocorr(&c, n as f64, k as f64).unwrap(),
                &autocorr_ref(&c, n, k),
                1e-9,
            );
        }
    }
}

// ---- the old laws ------------------------------------------------------------------------------

/// A flat stretch after a big move gives exactly what the old kernels gave: no residue of the carried
/// sums, no invented number.
#[test]
fn a_flat_stretch_is_exactly_what_the_old_kernels_gave() {
    let mut c = closes(100.0, 2.0, 400);
    c.extend(std::iter::repeat_n(5000.0, 400));
    for n in [4usize, 5, 14, 20, 64, 200] {
        let nf = n as f64;
        let all: [(&str, Vec<f64>, Option<f64>); 13] = [
            ("var", series::var(&c, nf).unwrap(), Some(0.0)),
            ("zscore", series::zscore(&c, nf).unwrap(), None),
            ("linearreg", series::linearreg(&c, nf).unwrap(), Some(5000.0)),
            ("linearreg_slope", series::linearreg_slope(&c, nf).unwrap(), Some(0.0)),
            ("linearreg_angle", series::linearreg_angle(&c, nf).unwrap(), Some(0.0)),
            ("linearreg_intercept", series::linearreg_intercept(&c, nf).unwrap(), Some(5000.0)),
            ("tsf", series::tsf(&c, nf).unwrap(), Some(5000.0)),
            ("std_error", series::std_error(&c, nf).unwrap(), Some(0.0)),
            ("mad", series::mad(&c, nf).unwrap(), Some(0.0)),
            ("skew", series::skew(&c, nf).unwrap(), Some(0.0)),
            ("kurtosis", series::kurtosis(&c, nf).unwrap(), Some(0.0)),
            ("rank_correlation", series::rank_correlation(&c, nf).unwrap(), Some(100.0)),
            ("r_squared", series::r_squared(&c, nf).unwrap(), None),
        ];
        for (name, v, want) in all {
            for (i, x) in v.iter().enumerate().take(800).skip(400 + n) {
                match want {
                    Some(w) => {
                        assert!(x.to_bits() == w.to_bits(), "{name}({n})[{i}] = {x:e}, want {w}")
                    }
                    None => assert!(x.is_nan(), "{name}({n})[{i}] = {x:e}, want NaN"),
                }
            }
        }
        let bands = series::std_error_bands(&c, nf, 2.0).unwrap();
        let rows = bands[0].iter().zip(&bands[1]).zip(&bands[2]).enumerate();
        for (i, ((u, m), l)) in rows.take(800).skip(400 + n) {
            assert!(*u == 5000.0 && *m == 5000.0 && *l == 5000.0, "std_error_bands({n})[{i}]");
        }
    }
    for n in [6usize, 30, 52] {
        let a = series::autocorr(&c, n as f64, 1.0).unwrap();
        for (i, x) in a.iter().enumerate().take(800).skip(400 + n) {
            assert!(x.is_nan(), "autocorr({n},1)[{i}] = {x:e}");
        }
    }
    for n in [17usize, 50, 100] {
        let h = series::hurst(&c, n as f64).unwrap();
        for (i, x) in h.iter().enumerate().take(800).skip(400 + n) {
            assert!(x.is_nan(), "hurst({n})[{i}] = {x:e}");
        }
    }
}

/// A flat stretch of a value whose mean does not round back (0.1) is still a variance of exactly zero
/// and a z-score that is NaN, whatever came before it.
#[test]
fn a_flat_stretch_of_a_tenth_has_no_variance() {
    let mut c = closes(100.0, 2.0, 400);
    c.extend(std::iter::repeat_n(0.1, 400));
    for n in [2usize, 14, 20, 100] {
        let v = series::var(&c, n as f64).unwrap();
        let z = series::zscore(&c, n as f64).unwrap();
        for i in 400 + n..800 {
            assert!(v[i] == 0.0, "var({n})[{i}] = {:e}", v[i]);
            assert!(z[i].is_nan(), "zscore({n})[{i}] = {:e}", z[i]);
        }
    }
}

/// A `NaN` inside the window makes the answer `NaN` while it is inside, and the answer is exactly the
/// textbook one the update after it left.
#[test]
fn a_nan_leaves_no_trace_once_it_is_out_of_the_window() {
    let mut c = closes(100.0, 2.0, 600);
    c[300] = f64::NAN;
    for n in [4usize, 14, 100] {
        let nf = n as f64;
        let lines: [(&str, Vec<f64>); 12] = [
            ("linearreg", series::linearreg(&c, nf).unwrap()),
            ("slope", series::linearreg_slope(&c, nf).unwrap()),
            ("angle", series::linearreg_angle(&c, nf).unwrap()),
            ("var", series::var(&c, nf).unwrap()),
            ("zscore", series::zscore(&c, nf).unwrap()),
            ("skew", series::skew(&c, nf).unwrap()),
            ("kurtosis", series::kurtosis(&c, nf).unwrap()),
            ("mad", series::mad(&c, nf).unwrap()),
            ("std_error", series::std_error(&c, nf).unwrap()),
            ("rank_correlation", series::rank_correlation(&c, nf).unwrap()),
            ("r_squared", series::r_squared(&c, nf).unwrap()),
            ("autocorr", series::autocorr(&c, nf.max(5.0), 1.0).unwrap()),
        ];
        for (name, v) in &lines {
            for (i, x) in v.iter().enumerate().skip(300).take(n) {
                assert!(x.is_nan(), "{name}({n})[{i}] holds the NaN");
            }
            assert!(v[300 + n.max(5)].is_finite(), "{name}({n}) recovers");
        }
    }
}

// ---- the ways in -------------------------------------------------------------------------------

fn lengths(n: usize) -> [usize; 8] {
    [0, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600]
}

/// Closes with a NaN a third of the way in and a flat stretch half way in.
fn planted(len: usize) -> Vec<f64> {
    let mut c = closes(100.0, 2.0, len);
    if len > 40 {
        c[len / 3] = f64::NAN;
        let (lo, hi) = (len / 2, (len / 2 + 30).min(len));
        let v = c[lo];
        for x in &mut c[lo..hi] {
            *x = v;
        }
    }
    c
}

fn ways1(label: &str, stream: Vec<f64>, value: Vec<f64>, batch: Vec<Vec<f64>>, slice: Vec<f64>) {
    assert!(same(&stream, &value), "{label}: next vs the plain-number entry");
    assert!(same(&stream, &batch[0]), "{label}: vectorize");
    assert!(same(&stream, &slice), "{label}: series");
}

macro_rules! ways_of {
    ($label:expr, $ctor:expr, $series:expr, $bars:expr, $c:expr) => {{
        let mut ind = $ctor;
        let stream: Vec<f64> = $bars.iter().map(|y| ind.next(y)).collect();
        let mut ind = $ctor;
        let value: Vec<f64> = $c.iter().map(|&y| ind.next_value(y)).collect();
        ways1($label, stream, value, $ctor.vectorize(&$bars), $series);
    }};
}

/// Stream == plain-number entry == vectorize == series, bit for bit, over periods, lengths, a NaN and a
/// flat stretch.
#[test]
fn every_way_in_gives_the_same_bits() {
    for n in [2usize, 3, 4, 5, 14, 20, 64, 199, 200] {
        let nf = n as f64;
        for len in lengths(n) {
            let c = planted(len);
            let b = bars_of(&c);
            let l = |s: &str| format!("{s}({n}) len={len}");
            ways_of!(
                &l("linearreg"),
                Linearreg::new(nf).unwrap(),
                series::linearreg(&c, nf).unwrap(),
                b,
                c
            );
            ways_of!(
                &l("linearreg_slope"),
                LinearregSlope::new(nf).unwrap(),
                series::linearreg_slope(&c, nf).unwrap(),
                b,
                c
            );
            ways_of!(
                &l("linearreg_angle"),
                LinearregAngle::new(nf).unwrap(),
                series::linearreg_angle(&c, nf).unwrap(),
                b,
                c
            );
            ways_of!(
                &l("linearreg_intercept"),
                LinearregIntercept::new(nf).unwrap(),
                series::linearreg_intercept(&c, nf).unwrap(),
                b,
                c
            );
            ways_of!(&l("tsf"), Tsf::new(nf).unwrap(), series::tsf(&c, nf).unwrap(), b, c);
            ways_of!(&l("var"), Var::new(nf).unwrap(), series::var(&c, nf).unwrap(), b, c);
            ways_of!(&l("zscore"), Zscore::new(nf).unwrap(), series::zscore(&c, nf).unwrap(), b, c);
            ways_of!(&l("skew"), Skew::new(nf).unwrap(), series::skew(&c, nf).unwrap(), b, c);
            ways_of!(
                &l("kurtosis"),
                Kurtosis::new(nf).unwrap(),
                series::kurtosis(&c, nf).unwrap(),
                b,
                c
            );
            ways_of!(&l("mad"), Mad::new(nf).unwrap(), series::mad(&c, nf).unwrap(), b, c);
            ways_of!(
                &l("std_error"),
                StdError::new(nf).unwrap(),
                series::std_error(&c, nf).unwrap(),
                b,
                c
            );
            ways_of!(
                &l("rank_correlation"),
                RankCorrelation::new(nf).unwrap(),
                series::rank_correlation(&c, nf).unwrap(),
                b,
                c
            );
            ways_of!(
                &l("r_squared"),
                RSquared::new(nf).unwrap(),
                series::r_squared(&c, nf).unwrap(),
                b,
                c
            );

            // std_error_bands: three lines
            for mult in [0.1, 2.0, 10.0] {
                let mut ind = StdErrorBands::new(nf, mult).unwrap();
                let stream: Vec<[f64; 3]> = b.iter().map(|y| ind.next(y)).collect();
                let mut ind = StdErrorBands::new(nf, mult).unwrap();
                let value: Vec<[f64; 3]> = c.iter().map(|&y| ind.next_value(y)).collect();
                let batch = StdErrorBands::new(nf, mult).unwrap().vectorize(&b);
                let slice = series::std_error_bands(&c, nf, mult).unwrap();
                for k in 0..3 {
                    let s: Vec<f64> = stream.iter().map(|v| v[k]).collect();
                    let v: Vec<f64> = value.iter().map(|v| v[k]).collect();
                    let label = format!("std_error_bands({n},{mult}) line {k} len={len}");
                    assert!(same(&s, &v), "{label}: next vs the plain-number entry");
                    assert!(same(&s, &batch[k]), "{label}: vectorize");
                    assert!(same(&s, &slice[k]), "{label}: series");
                }
            }
        }
    }
    for n in [17usize, 18, 33, 100, 257, 399, 400] {
        let nf = n as f64;
        for len in lengths(n) {
            let c = planted(len);
            let b = bars_of(&c);
            ways_of!(
                &format!("hurst({n}) len={len}"),
                Hurst::new(nf).unwrap(),
                series::hurst(&c, nf).unwrap(),
                b,
                c
            );
        }
    }
    for (n, k) in
        [(5usize, 1usize), (5, 3), (30, 1), (30, 5), (64, 2), (200, 50), (52, 50), (10, 9)]
    {
        let (nf, kf) = (n as f64, k as f64);
        for len in lengths(n) {
            let c = planted(len);
            let b = bars_of(&c);
            ways_of!(
                &format!("autocorr({n},{k}) len={len}"),
                Autocorr::new(nf, kf).unwrap(),
                series::autocorr(&c, nf, kf).unwrap(),
                b,
                c
            );
        }
    }
}

/// `reset` returns a stepper to its first state, and a clone carries on identically.
#[test]
fn reset_and_clone_restore_the_state() {
    let b = bars_of(&planted(900));
    macro_rules! check {
        ($ind:expr) => {{
            let mut a = $ind;
            let first: Vec<Vec<f64>> = b.iter().map(|y| a.on_bar(y)).collect();
            let mut forked = a.clone();
            a.reset();
            assert!(
                a.value().iter().all(|v| v.is_nan()),
                "{}: a reset stepper reads NaN",
                a.name()
            );
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
    check!(Linearreg::new(14.0).unwrap());
    check!(LinearregSlope::new(14.0).unwrap());
    check!(LinearregAngle::new(14.0).unwrap());
    check!(LinearregIntercept::new(14.0).unwrap());
    check!(Tsf::new(14.0).unwrap());
    check!(Var::new(20.0).unwrap());
    check!(Zscore::new(20.0).unwrap());
    check!(Skew::new(20.0).unwrap());
    check!(Kurtosis::new(20.0).unwrap());
    check!(Mad::new(20.0).unwrap());
    check!(StdError::new(20.0).unwrap());
    check!(StdErrorBands::new(20.0, 2.0).unwrap());
    check!(RankCorrelation::new(14.0).unwrap());
    check!(RSquared::new(14.0).unwrap());
    check!(Hurst::new(100.0).unwrap());
    check!(Autocorr::new(30.0, 1.0).unwrap());
}

#[test]
fn out_of_range_parameters_are_refused() {
    let e = &[1.0_f64][..];
    assert!(Linearreg::new(1.0).is_err() && series::linearreg(e, 201.0).is_err());
    assert!(LinearregSlope::new(201.0).is_err() && series::linearreg_slope(e, 1.0).is_err());
    assert!(LinearregAngle::new(f64::NAN).is_err() && series::linearreg_angle(e, 0.0).is_err());
    assert!(
        LinearregIntercept::new(1.0).is_err() && series::linearreg_intercept(e, 201.0).is_err()
    );
    assert!(Tsf::new(1.0).is_err() && series::tsf(e, 201.0).is_err());
    assert!(Var::new(1.0).is_err() && series::var(e, 201.0).is_err());
    assert!(Zscore::new(201.0).is_err() && series::zscore(e, 1.0).is_err());
    assert!(Skew::new(1.0).is_err() && series::skew(e, 201.0).is_err());
    assert!(Kurtosis::new(201.0).is_err() && series::kurtosis(e, 1.0).is_err());
    assert!(Mad::new(1.0).is_err() && series::mad(e, 201.0).is_err());
    assert!(StdError::new(1.0).is_err() && series::std_error(e, 201.0).is_err());
    assert!(
        StdErrorBands::new(20.0, 0.0).is_err() && series::std_error_bands(e, 20.0, 11.0).is_err()
    );
    assert!(RankCorrelation::new(1.0).is_err() && series::rank_correlation(e, 201.0).is_err());
    assert!(RSquared::new(201.0).is_err() && series::r_squared(e, 1.0).is_err());
    assert!(Hurst::new(16.0).is_err() && series::hurst(e, 401.0).is_err());
    assert!(Autocorr::new(4.0, 1.0).is_err() && series::autocorr(e, 30.0, 51.0).is_err());
}

/// The skewness needs 3 values and the kurtosis 4: a shorter window never answers (period 2 is in the
/// registry range), and neither does an autocorrelation whose window cannot hold two pairs.
#[test]
fn a_window_too_short_never_answers() {
    let c = closes(100.0, 2.0, 200);
    assert!(series::skew(&c, 2.0).unwrap().iter().all(|v| v.is_nan()));
    assert!(series::kurtosis(&c, 2.0).unwrap().iter().all(|v| v.is_nan()));
    assert!(series::kurtosis(&c, 3.0).unwrap().iter().all(|v| v.is_nan()));
    assert!(series::kurtosis(&c, 4.0).unwrap().iter().skip(3).all(|v| v.is_finite()));
    assert!(series::autocorr(&c, 5.0, 4.0).unwrap().iter().all(|v| v.is_nan()));
    assert!(series::autocorr(&c, 5.0, 3.0).unwrap().iter().skip(4).all(|v| v.is_finite()));
    // A standard error of a two-value window is zero, after the warm-up.
    let se = series::std_error(&c, 2.0).unwrap();
    assert!(se[0].is_nan() && se[1..].iter().all(|v| *v == 0.0));
}

/// A straight line has a standard error and a residual of zero to the rounding of the numbers, and an
/// r-squared of 1.
#[test]
fn a_straight_line_has_no_error() {
    for (level, slope) in [(100.0, 0.5), (1.0e6, 3.0), (1.0e-3, 1.0e-5)] {
        let c: Vec<f64> = (0..300).map(|i| level + slope * i as f64).collect();
        for n in [3usize, 20, 100] {
            let se = series::std_error(&c, n as f64).unwrap();
            let r2 = series::r_squared(&c, n as f64).unwrap();
            let sl = series::linearreg_slope(&c, n as f64).unwrap();
            for i in (n - 1)..300 {
                assert!(se[i] <= 1e-6 * slope * n as f64, "std_error n={n} i={i}: {:e}", se[i]);
                assert!((r2[i] - 1.0).abs() <= 1e-9, "r_squared n={n} i={i}: {}", r2[i]);
                assert!((sl[i] - slope).abs() <= 1e-9 * slope, "slope n={n} i={i}: {}", sl[i]);
            }
        }
    }
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
    let mut w = [0.0_f64; 14];
    for (_, c) in inputs() {
        for n in PERIODS {
            let nf = n as f64;
            let r = lin_ref(&c, n);
            let g = [
                series::linearreg(&c, nf).unwrap(),
                series::linearreg_slope(&c, nf).unwrap(),
                series::linearreg_angle(&c, nf).unwrap(),
                series::linearreg_intercept(&c, nf).unwrap(),
                series::tsf(&c, nf).unwrap(),
            ];
            for k in 0..5 {
                w[k] = w[k].max(worst(&g[k], &r[k]));
            }
            w[5] = w[5].max(worst(&series::var(&c, nf).unwrap(), &windows(&c, n, var_w)));
            w[6] = w[6].max(worst(&series::zscore(&c, nf).unwrap(), &zscore_ref(&c, n)));
            w[7] = w[7].max(worst(&series::skew(&c, nf).unwrap(), &skew_ref(&c, n)));
            w[8] = w[8].max(worst(&series::kurtosis(&c, nf).unwrap(), &kurtosis_ref(&c, n)));
            w[9] = w[9].max(worst(&series::mad(&c, nf).unwrap(), &mad_ref(&c, n)));
            w[10] = w[10].max(worst(&series::std_error(&c, nf).unwrap(), &std_error_ref(&c, n)));
            let g = series::std_error_bands(&c, nf, 2.0).unwrap();
            let r = bands_ref(&c, n, 2.0);
            for k in 0..3 {
                w[10] = w[10].max(worst(&g[k], &r[k]));
            }
            w[11] = w[11].max(worst(&series::r_squared(&c, nf).unwrap(), &r_squared_ref(&c, n)));
        }
        for (n, k) in [(5usize, 1usize), (30, 1), (30, 5), (64, 2), (200, 50)] {
            let g = series::autocorr(&c, n as f64, k as f64).unwrap();
            w[12] = w[12].max(worst(&g, &autocorr_ref(&c, n, k)));
        }
    }
    println!(
        "worst relative error: linearreg {:.2e} slope {:.2e} angle {:.2e} intercept {:.2e} tsf {:.2e} \
         var {:.2e} zscore {:.2e} skew {:.2e} kurtosis {:.2e} mad {:.2e} std_error(+bands) {:.2e} \
         r_squared {:.2e} autocorr {:.2e}  (rank_correlation and hurst are bit-exact)",
        w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10], w[11], w[12]
    );
}

/// A bad tick (a close a thousand times its neighbours) leaves its square in a running sum until the
/// next rebuild: the worst relative error of each statistic after the tick has left the window, printed
/// (`--ignored --nocapture`).
#[test]
#[ignore = "a measurement, not a check"]
fn measure_a_bad_tick() {
    fn worst_after(got: &[f64], want: &[f64], from: &[usize], n: usize) -> f64 {
        let typ = want.iter().filter(|v| v.is_finite()).fold(0.0_f64, |a, v| a.max(v.abs()));
        let mut w = 0.0_f64;
        for &s in from {
            for i in s + n + 1..s + 400 {
                if got[i].is_finite() && want[i].is_finite() {
                    w = w.max((got[i] - want[i]).abs() / want[i].abs().max(1.0e-3 * typ));
                }
            }
        }
        w
    }
    let spikes = [500usize, 1500, 2500];
    for factor in [10.0, 1.0e3, 1.0e5] {
        let mut c = closes(100.0, 2.0, 3000);
        for &s in &spikes {
            c[s] *= factor;
        }
        for n in [2usize, 5, 20, 64] {
            let nf = n as f64;
            let r = lin_ref(&c, n);
            let g = [
                series::linearreg_slope(&c, nf).unwrap(),
                series::var(&c, nf).unwrap(),
                series::zscore(&c, nf).unwrap(),
                series::skew(&c, nf).unwrap(),
                series::kurtosis(&c, nf).unwrap(),
                series::mad(&c, nf).unwrap(),
                series::std_error(&c, nf).unwrap(),
                series::r_squared(&c, nf).unwrap(),
            ];
            let want = [
                r[1].clone(),
                windows(&c, n, var_w),
                zscore_ref(&c, n),
                skew_ref(&c, n),
                kurtosis_ref(&c, n),
                mad_ref(&c, n),
                std_error_ref(&c, n),
                r_squared_ref(&c, n),
            ];
            let names =
                ["slope", "var", "zscore", "skew", "kurtosis", "mad", "std_error", "r_squared"];
            let line: Vec<String> = names
                .iter()
                .zip(g.iter().zip(&want))
                .map(|(name, (g, w))| format!("{name} {:.1e}", worst_after(g, w, &spikes, n)))
                .collect();
            let ac = if n >= 5 {
                let g = series::autocorr(&c, nf, 1.0).unwrap();
                format!("autocorr {:.1e}", worst_after(&g, &autocorr_ref(&c, n, 1), &spikes, n))
            } else {
                String::new()
            };
            println!("bad tick x{factor:e} n={n}: {}  {ac}", line.join("  "));
        }
    }
}

/// The windows around one bad tick, got against want (`--ignored --nocapture`).
#[test]
#[ignore = "a measurement, not a check"]
fn show_a_bad_tick() {
    let mut c = closes(100.0, 2.0, 3000);
    c[500] *= 10.0;
    for n in [5usize, 20] {
        let nf = n as f64;
        let k = series::kurtosis(&c, nf).unwrap();
        let kw = kurtosis_ref(&c, n);
        let z = series::zscore(&c, nf).unwrap();
        let zw = zscore_ref(&c, n);
        let v = series::var(&c, nf).unwrap();
        let vw = windows(&c, n, var_w);
        for i in 498..500 + n + 12 {
            println!(
                "n={n} i={i}: kurt {:.9e} vs {:.9e} | z {:.9e} vs {:.9e} | var {:.9e} vs {:.9e}",
                k[i], kw[i], z[i], zw[i], v[i], vw[i]
            );
        }
    }
}

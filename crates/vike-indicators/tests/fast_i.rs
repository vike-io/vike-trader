//! ZLEMA, COPPOCK, TSI, SMI_ERGODIC, MASS, KST, QSTICK, RWI, ACCELERATION_BANDS and TTM_SQUEEZE: the
//! fast cores against a textbook reference, and the stream (`next`), the plain-number entry
//! (`next_value` / `next_hl` / `next_hlc` / `next_oc`), the batch (`vectorize`) and the slice call
//! (`series::*`) against each other, bit for bit.
//!
//! The reference does each value the long way (every window folded from scratch, divisions, the SMA
//! seed of every average, the dropped-NaN rule of `math::smooth_defined` spelled out as compress and
//! expand). The cores carry running sums and multiply by `1/n`, so the last bits differ; the
//! tolerance is relative 1e-9 of the value, at three scales: level 100 with steps of 2, level 1e6
//! with steps of a hundredth, level 1e-3 with steps of a millionth.
//!
//! The one place the tolerance has an absolute part is the TTM Squeeze momentum: it is a regression
//! of `close - (midpoint + mean) / 2`, a difference of numbers the size of the price level, so each
//! value carries a few ulps OF THE LEVEL (at 1e6 that is 1e-10 on a value of 1e-2) whichever way it
//! is summed. The bound there is 1e-9 relative plus 8e-16 of the level.

use vike_indicators::indicators::{
    AccelerationBands, Coppock, Kst, Mass, Qstick, Rwi, SmiErgodic, Tsi, TtmSqueeze, Zlema,
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

/// A bar around each close, every extent a multiple of `s` (the series' step) so the bar is as wide
/// as the series moves at every scale; the open is not the close, and the spread varies.
fn bars_of(c: &[f64], s: f64) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let up = s * (0.3 + (i % 7) as f64 * 0.1);
            let dn = s * (0.2 + (i % 5) as f64 * 0.15);
            let open = x - s * 0.5 * (((i * 3) % 7) as f64 - 3.0) / 3.0;
            Bar::ohlcv(i as i64 * 60_000, open, x.max(open) + up, x.min(open) - dn, x, 1.0)
        })
        .collect()
}

fn opens(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| x.open).collect()
}
fn highs(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| x.high).collect()
}
fn lows(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| x.low).collect()
}
fn closes_of(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| x.close).collect()
}

// ------------------------------------------------------------------ textbook references

fn mean_of(w: &[f64]) -> f64 {
    w.iter().sum::<f64>() / w.len() as f64
}

/// `math::ema` over a raw series: `NaN` for `n - 1` values, the mean of the first `n`, then the
/// alpha recurrence.
fn ema_vec(x: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    if n == 0 || x.len() < n {
        return out;
    }
    let a = 2.0 / (n as f64 + 1.0);
    let mut prev = x[..n].iter().sum::<f64>() / n as f64;
    out[n - 1] = prev;
    for i in n..x.len() {
        prev = a * x[i] + (1.0 - a) * prev;
        out[i] = prev;
    }
    out
}

fn sma_vec(x: &[f64], n: usize) -> Vec<f64> {
    (0..x.len())
        .map(|i| if n == 0 || i + 1 < n { f64::NAN } else { mean_of(&x[i + 1 - n..=i]) })
        .collect()
}

fn wma_vec(x: &[f64], n: usize) -> Vec<f64> {
    (0..x.len())
        .map(|i| {
            if n == 0 || i + 1 < n {
                return f64::NAN;
            }
            let top: f64 =
                x[i + 1 - n..=i].iter().enumerate().map(|(k, v)| (k + 1) as f64 * v).sum();
            top / (n * (n + 1) / 2) as f64
        })
        .collect()
}

/// `math::smooth_defined`: the average runs over the defined values only; a position whose input
/// was `NaN` stays `NaN`.
fn smooth_defined(src: &[f64], f: impl Fn(&[f64]) -> Vec<f64>) -> Vec<f64> {
    let idx: Vec<usize> = (0..src.len()).filter(|&i| !src[i].is_nan()).collect();
    let dense: Vec<f64> = idx.iter().map(|&i| src[i]).collect();
    let r = f(&dense);
    let mut out = vec![f64::NAN; src.len()];
    for (k, &i) in idx.iter().enumerate() {
        out[i] = r[k];
    }
    out
}

fn zlema_ref(c: &[f64], n: usize) -> Vec<f64> {
    let lag = (n - 1) / 2;
    let x: Vec<f64> =
        (0..c.len()).map(|i| if i >= lag { c[i] + (c[i] - c[i - lag]) } else { c[i] }).collect();
    ema_vec(&x, n)
}

fn roc_ratio(c: &[f64], k: usize) -> Vec<f64> {
    (0..c.len())
        .map(|i| if i >= k && c[i - k] != 0.0 { (c[i] / c[i - k] - 1.0) * 100.0 } else { f64::NAN })
        .collect()
}

fn coppock_ref(c: &[f64], wma_p: usize, long: usize, short: usize) -> Vec<f64> {
    let (rl, rs) = (roc_ratio(c, long), roc_ratio(c, short));
    let comb: Vec<f64> = rl.iter().zip(&rs).map(|(a, b)| a + b).collect();
    smooth_defined(&comb, |d| wma_vec(d, wma_p))
}

fn delta_of(c: &[f64]) -> Vec<f64> {
    (0..c.len()).map(|i| if i == 0 { 0.0 } else { c[i] - c[i - 1] }).collect()
}

fn tsi_ratio(c: &[f64], long: usize, short: usize) -> Vec<f64> {
    let d = delta_of(c);
    let a: Vec<f64> = d.iter().map(|v| v.abs()).collect();
    let d2 = smooth_defined(&ema_vec(&d, long), |x| ema_vec(x, short));
    let a2 = smooth_defined(&ema_vec(&a, long), |x| ema_vec(x, short));
    d2.iter()
        .zip(&a2)
        .map(
            |(d, a)| if !d.is_nan() && !a.is_nan() && *a != 0.0 { 100.0 * d / a } else { f64::NAN },
        )
        .collect()
}

fn smi_ref(c: &[f64], long: usize, short: usize, signal: usize) -> [Vec<f64>; 2] {
    let smi = tsi_ratio(c, long, short);
    let sig = smooth_defined(&smi, |x| ema_vec(x, signal));
    [smi, sig]
}

fn mass_ref(h: &[f64], l: &[f64], np: usize, ne: usize) -> Vec<f64> {
    let hl: Vec<f64> = h.iter().zip(l).map(|(a, b)| a - b).collect();
    let e1 = ema_vec(&hl, ne);
    let e2 = smooth_defined(&e1, |x| ema_vec(x, ne));
    let ratio: Vec<f64> = e1
        .iter()
        .zip(&e2)
        .map(|(a, b)| if !a.is_nan() && !b.is_nan() && *b != 0.0 { a / b } else { f64::NAN })
        .collect();
    (0..ratio.len())
        .map(|i| {
            if np == 0 || i + 1 < np {
                return f64::NAN;
            }
            let w = &ratio[i + 1 - np..=i];
            if w.iter().any(|v| v.is_nan()) { f64::NAN } else { w.iter().sum() }
        })
        .collect()
}

fn roc_diff(c: &[f64], k: usize) -> Vec<f64> {
    (0..c.len())
        .map(|i| {
            if i >= k && c[i - k] != 0.0 { (c[i] - c[i - k]) / c[i - k] * 100.0 } else { f64::NAN }
        })
        .collect()
}

fn kst_ref(c: &[f64], p: [usize; 9]) -> [Vec<f64>; 2] {
    let r: Vec<Vec<f64>> = (0..4)
        .map(|k| smooth_defined(&roc_diff(c, p[2 * k]), |d| sma_vec(d, p[2 * k + 1])))
        .collect();
    let kst: Vec<f64> = (0..c.len())
        .map(|i| {
            if r.iter().all(|v| !v[i].is_nan()) {
                1.0 * r[0][i] + 2.0 * r[1][i] + 3.0 * r[2][i] + 4.0 * r[3][i]
            } else {
                f64::NAN
            }
        })
        .collect();
    let sig = smooth_defined(&kst, |d| sma_vec(d, p[8]));
    [kst, sig]
}

fn qstick_ref(o: &[f64], c: &[f64], n: usize) -> Vec<f64> {
    let body: Vec<f64> = o.iter().zip(c).map(|(o, c)| c - o).collect();
    sma_vec(&body, n)
}

fn true_range_ref(h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
    (0..c.len())
        .map(|i| {
            if i == 0 {
                h[0] - l[0]
            } else {
                (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs())
            }
        })
        .collect()
}

fn rwi_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> [Vec<f64>; 2] {
    let tr = true_range_ref(h, l, c);
    let mut out = [vec![f64::NAN; c.len()], vec![f64::NAN; c.len()]];
    for i in n..c.len() {
        let atr = tr[i + 1 - n..=i].iter().sum::<f64>() / n as f64;
        let denom = atr * (n as f64).sqrt();
        if denom > 0.0 {
            out[0][i] = (h[i] - l[i - n]) / denom;
            out[1][i] = (h[i - n] - l[i]) / denom;
        }
    }
    out
}

fn accel_ref(h: &[f64], l: &[f64], c: &[f64], n: usize, factor: f64) -> [Vec<f64>; 3] {
    let w: Vec<f64> =
        h.iter().zip(l).map(|(h, l)| if h + l == 0.0 { 0.0 } else { (h - l) / (h + l) }).collect();
    let up: Vec<f64> = (0..h.len()).map(|i| h[i] * (1.0 + factor * w[i])).collect();
    let lo: Vec<f64> = (0..h.len()).map(|i| l[i] * (1.0 - factor * w[i])).collect();
    [sma_vec(&up, n), sma_vec(c, n), sma_vec(&lo, n)]
}

fn ttm_ref(h: &[f64], l: &[f64], c: &[f64], n: usize, bbm: f64, kcm: f64) -> [Vec<f64>; 2] {
    let len = c.len();
    let tr = true_range_ref(h, l, c);
    let mut squeeze = vec![f64::NAN; len];
    let mut delta = vec![f64::NAN; len];
    for i in n - 1..len {
        let w = &c[i + 1 - n..=i];
        let mid = mean_of(w);
        let dev = (w.iter().map(|x| (x - mid) * (x - mid)).sum::<f64>() / n as f64).sqrt();
        let range = mean_of(&tr[i + 1 - n..=i]);
        let (bu, bl) = (mid + bbm * dev, mid - bbm * dev);
        let (ku, kl) = (mid + kcm * range, mid - kcm * range);
        squeeze[i] = if bl > kl && bu < ku { 1.0 } else { 0.0 };
        let hh = h[i + 1 - n..=i].iter().copied().fold(f64::MIN, f64::max);
        let ll = l[i + 1 - n..=i].iter().copied().fold(f64::MAX, f64::min);
        delta[i] = c[i] - ((hh + ll) / 2.0 + mid) / 2.0;
    }
    let mut momentum = vec![f64::NAN; len];
    for i in 2 * n - 2..len {
        let y = &delta[i + 1 - n..=i];
        if y.iter().any(|v| v.is_nan()) {
            continue;
        }
        let xm = (n as f64 - 1.0) / 2.0;
        let ym = mean_of(y);
        let (mut sxy, mut sxx) = (0.0, 0.0);
        for (k, v) in y.iter().enumerate() {
            sxy += (k as f64 - xm) * (v - ym);
            sxx += (k as f64 - xm) * (k as f64 - xm);
        }
        momentum[i] = ym + sxy / sxx * ((n as f64 - 1.0) - xm);
    }
    [momentum, squeeze]
}

// ------------------------------------------------------------------ comparison helpers

/// Relative error of every value (plus `abs` of absolute slack); NaN positions must agree.
fn max_rel_error(name: &str, got: &[f64], want: &[f64], rel: f64, abs: f64) -> f64 {
    assert_eq!(got.len(), want.len(), "{name}: length");
    let mut worst = 0.0_f64;
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if w.is_nan() {
            assert!(g.is_nan(), "{name}[{i}]: want NaN, got {g}");
        } else {
            assert!(!g.is_nan(), "{name}[{i}]: want {w}, got NaN");
            let err = (g - w).abs();
            assert!(
                err <= rel * w.abs() + abs,
                "{name}[{i}]: got {g}, want {w}, error {err:e} (relative {:e})",
                err / w.abs().max(1e-300)
            );
            worst = worst.max(err / w.abs().max(abs.max(1e-300)));
        }
    }
    worst
}

fn same(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];
const REL: f64 = 1e-9;

/// The same series with a NaN dropped in, to check a NaN leaves the textbook as it leaves the core.
fn with_nan(mut c: Vec<f64>, at: usize) -> Vec<f64> {
    c[at] = f64::NAN;
    c
}

// ------------------------------------------------------------------ against the textbook

#[test]
fn zlema_matches_the_lagged_ema() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        for n in [2usize, 3, 4, 5, 20, 64, 200, 400] {
            let got = series::zlema(&c, n as f64).unwrap();
            max_rel_error(&format!("zlema({n}) {level}"), &got, &zlema_ref(&c, n), REL, 0.0);
            // A NaN poisons it for good, in the reference as in the core.
            let cn = with_nan(c.clone(), 1500);
            let got = series::zlema(&cn, n as f64).unwrap();
            max_rel_error(&format!("zlema({n}) NaN {level}"), &got, &zlema_ref(&cn, n), REL, 0.0);
            assert!(got[2999].is_nan());
        }
    }
}

#[test]
fn coppock_matches_the_weighted_sum_of_two_rates() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        for (w, long, short) in [
            (10usize, 14usize, 11usize),
            (2, 2, 2),
            (5, 3, 2),
            (100, 200, 150),
            (7, 20, 2),
            (50, 2, 200),
        ] {
            let got = series::coppock(&c, w as f64, long as f64, short as f64).unwrap();
            max_rel_error(
                &format!("coppock({w},{long},{short}) {level}"),
                &got,
                &coppock_ref(&c, w, long, short),
                REL,
                0.0,
            );
            // A NaN close drops its own rate and the rates that look back at it: those bars are
            // NaN and the average carries on over the others.
            let cn = with_nan(c.clone(), 1500);
            let got = series::coppock(&cn, w as f64, long as f64, short as f64).unwrap();
            max_rel_error(
                &format!("coppock({w},{long},{short}) NaN {level}"),
                &got,
                &coppock_ref(&cn, w, long, short),
                REL,
                0.0,
            );
            assert!(got[1500].is_nan() && !got[2999].is_nan());
        }
    }
}

#[test]
fn coppock_skips_a_zero_reference_close() {
    let mut c = closes(100.0, 2.0, 600);
    c[300] = 0.0;
    let got = series::coppock(&c, 10.0, 14.0, 11.0).unwrap();
    max_rel_error("coppock zero close", &got, &coppock_ref(&c, 10, 14, 11), REL, 0.0);
    // The bars 14 and 11 after the zero have it as their reference: undefined, left out.
    assert!(got[314].is_nan() && got[311].is_nan() && !got[312].is_nan());
}

#[test]
fn tsi_matches_the_double_smoothed_ratio() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        for (long, short) in [(25usize, 13usize), (2, 2), (3, 2), (20, 5), (64, 32), (400, 200)] {
            let got = series::tsi(&c, long as f64, short as f64).unwrap();
            max_rel_error(
                &format!("tsi({long},{short}) {level}"),
                &got,
                &tsi_ratio(&c, long, short),
                REL,
                0.0,
            );
            let cn = with_nan(c.clone(), 1500);
            let got = series::tsi(&cn, long as f64, short as f64).unwrap();
            max_rel_error(
                &format!("tsi({long},{short}) NaN {level}"),
                &got,
                &tsi_ratio(&cn, long, short),
                REL,
                0.0,
            );
            assert!(got[2999].is_nan(), "a NaN close poisons tsi for good");
        }
    }
}

#[test]
fn tsi_of_a_flat_start_is_undefined_until_the_price_moves() {
    let mut c = vec![100.0; 80];
    c.extend(closes(100.0, 2.0, 400));
    let got = series::tsi(&c, 10.0, 4.0).unwrap();
    max_rel_error("tsi flat start", &got, &tsi_ratio(&c, 10, 4), REL, 0.0);
    assert!(got[..80].iter().all(|v| v.is_nan()) && !got[200].is_nan());
    let [smi, sig] = series::smi_ergodic(&c, 10.0, 4.0, 3.0).unwrap();
    let [wsmi, wsig] = smi_ref(&c, 10, 4, 3);
    max_rel_error("smi flat start", &smi, &wsmi, REL, 0.0);
    max_rel_error("signal flat start", &sig, &wsig, REL, 0.0);
}

#[test]
fn smi_ergodic_matches_the_ratio_and_its_signal() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        for (long, short, signal) in
            [(20usize, 5usize, 5usize), (2, 2, 2), (3, 2, 4), (64, 32, 16), (400, 200, 200)]
        {
            let [smi, sig] =
                series::smi_ergodic(&c, long as f64, short as f64, signal as f64).unwrap();
            let [wsmi, wsig] = smi_ref(&c, long, short, signal);
            max_rel_error(&format!("smi({long},{short},{signal}) {level}"), &smi, &wsmi, REL, 0.0);
            max_rel_error(
                &format!("signal({long},{short},{signal}) {level}"),
                &sig,
                &wsig,
                REL,
                0.0,
            );
            let cn = with_nan(c.clone(), 1500);
            let [smi, sig] =
                series::smi_ergodic(&cn, long as f64, short as f64, signal as f64).unwrap();
            let [wsmi, wsig] = smi_ref(&cn, long, short, signal);
            max_rel_error(&format!("smi NaN {level}"), &smi, &wsmi, REL, 0.0);
            max_rel_error(&format!("signal NaN {level}"), &sig, &wsig, REL, 0.0);
        }
    }
}

#[test]
fn mass_matches_the_window_sum_of_ratios() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        let b = bars_of(&c, step);
        let (h, l) = (highs(&b), lows(&b));
        for (np, ne) in [(25usize, 9usize), (2, 2), (5, 3), (20, 50), (200, 50), (3, 7)] {
            let got = series::mass(&h, &l, np as f64, ne as f64).unwrap();
            max_rel_error(
                &format!("mass({np},{ne}) {level}"),
                &got,
                &mass_ref(&h, &l, np, ne),
                REL,
                0.0,
            );
            let mut hn = h.clone();
            hn[1500] = f64::NAN;
            let got = series::mass(&hn, &l, np as f64, ne as f64).unwrap();
            max_rel_error(
                &format!("mass({np},{ne}) NaN {level}"),
                &got,
                &mass_ref(&hn, &l, np, ne),
                REL,
                0.0,
            );
            assert!(got[2999].is_nan(), "a NaN range poisons mass for good");
        }
    }
}

const KST_SETS: [[usize; 9]; 4] = [
    [10, 10, 15, 10, 20, 10, 30, 15, 9],
    [1, 2, 1, 2, 1, 2, 1, 2, 2],
    [3, 2, 5, 3, 7, 4, 11, 5, 6],
    [200, 200, 150, 100, 100, 50, 50, 20, 30],
];

fn kst_series(c: &[f64], p: [usize; 9]) -> [Vec<f64>; 2] {
    let f = |i: usize| p[i] as f64;
    series::kst(c, f(0), f(1), f(2), f(3), f(4), f(5), f(6), f(7), f(8)).unwrap()
}

#[test]
fn kst_matches_four_smoothed_rates_and_their_signal() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        for p in KST_SETS {
            let [kst, sig] = kst_series(&c, p);
            let [wk, ws] = kst_ref(&c, p);
            max_rel_error(&format!("kst{p:?} {level}"), &kst, &wk, REL, 0.0);
            max_rel_error(&format!("kst signal{p:?} {level}"), &sig, &ws, REL, 0.0);
            let cn = with_nan(c.clone(), 1500);
            let [kst, sig] = kst_series(&cn, p);
            let [wk, ws] = kst_ref(&cn, p);
            max_rel_error(&format!("kst NaN{p:?} {level}"), &kst, &wk, REL, 0.0);
            max_rel_error(&format!("kst signal NaN{p:?} {level}"), &sig, &ws, REL, 0.0);
            assert!(kst[1500].is_nan() && !kst[2999].is_nan());
        }
    }
}

#[test]
fn kst_skips_a_zero_reference_close() {
    let mut c = closes(100.0, 2.0, 700);
    c[300] = 0.0;
    let p = KST_SETS[0];
    let [kst, sig] = kst_series(&c, p);
    let [wk, ws] = kst_ref(&c, p);
    max_rel_error("kst zero close", &kst, &wk, REL, 0.0);
    max_rel_error("kst signal zero close", &sig, &ws, REL, 0.0);
    // The zero close is itself a defined bar (a rate of -100 against its reference); the bars 10, 15, 20
    // and 30 after it, whose component has it as the reference, are undefined.
    assert!(!kst[300].is_nan() && !kst[305].is_nan());
    for k in [310usize, 315, 320, 330] {
        assert!(kst[k].is_nan(), "kst[{k}]");
    }
}

#[test]
fn qstick_matches_the_mean_body() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        let b = bars_of(&c, step);
        let (o, cl) = (opens(&b), closes_of(&b));
        for n in [2usize, 3, 8, 20, 64, 200] {
            let got = series::qstick(&o, &cl, n as f64).unwrap();
            max_rel_error(&format!("qstick({n}) {level}"), &got, &qstick_ref(&o, &cl, n), REL, 0.0);
            let cn = with_nan(cl.clone(), 1500);
            let got = series::qstick(&o, &cn, n as f64).unwrap();
            max_rel_error(
                &format!("qstick({n}) NaN {level}"),
                &got,
                &qstick_ref(&o, &cn, n),
                REL,
                0.0,
            );
            assert!(got[1500..1500 + n].iter().all(|v| v.is_nan()) && !got[1500 + n].is_nan());
        }
    }
}

#[test]
fn rwi_matches_the_random_walk_ratio() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        let b = bars_of(&c, step);
        let (h, l, cl) = (highs(&b), lows(&b), closes_of(&b));
        for n in [2usize, 3, 14, 64, 200] {
            let got = series::rwi(&h, &l, &cl, n as f64).unwrap();
            let want = rwi_ref(&h, &l, &cl, n);
            max_rel_error(&format!("rwi_high({n}) {level}"), &got[0], &want[0], REL, 0.0);
            max_rel_error(&format!("rwi_low({n}) {level}"), &got[1], &want[1], REL, 0.0);
            let cn = with_nan(cl.clone(), 1500);
            let got = series::rwi(&h, &l, &cn, n as f64).unwrap();
            let want = rwi_ref(&h, &l, &cn, n);
            max_rel_error(&format!("rwi_high({n}) NaN {level}"), &got[0], &want[0], REL, 0.0);
            max_rel_error(&format!("rwi_low({n}) NaN {level}"), &got[1], &want[1], REL, 0.0);
        }
    }
}

#[test]
fn rwi_of_a_flat_market_has_no_value() {
    let h = vec![10.0; 100];
    let got = series::rwi(&h, &h, &h, 14.0).unwrap();
    assert!(got[0].iter().chain(&got[1]).all(|v| v.is_nan()));
}

#[test]
fn acceleration_bands_match_the_three_means() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        let b = bars_of(&c, step);
        let (h, l, cl) = (highs(&b), lows(&b), closes_of(&b));
        for (n, factor) in [(2usize, 0.5), (3, 1.0), (20, 4.0), (64, 10.0), (400, 2.5)] {
            let got = series::acceleration_bands(&h, &l, &cl, n as f64, factor).unwrap();
            let want = accel_ref(&h, &l, &cl, n, factor);
            for (k, name) in ["upper", "mid", "lower"].iter().enumerate() {
                max_rel_error(&format!("{name}({n}) {level}"), &got[k], &want[k], REL, 0.0);
            }
            let mut hn = h.clone();
            hn[1500] = f64::NAN;
            let got = series::acceleration_bands(&hn, &l, &cl, n as f64, factor).unwrap();
            let want = accel_ref(&hn, &l, &cl, n, factor);
            for (k, name) in ["upper", "mid", "lower"].iter().enumerate() {
                max_rel_error(&format!("{name}({n}) NaN {level}"), &got[k], &want[k], REL, 0.0);
            }
            // The mid line reads only the close, so a NaN high leaves it alone.
            assert!(got[1][1500..].iter().all(|v| !v.is_nan()));
        }
    }
}

#[test]
fn acceleration_bands_with_a_zero_sum_have_no_width() {
    let z = vec![0.0; 40];
    let got = series::acceleration_bands(&z, &z, &z, 5.0, 4.0).unwrap();
    for line in &got {
        assert_eq!(line[4], 0.0);
    }
}

const TTM_SETS: [(usize, f64, f64); 5] =
    [(20, 2.0, 1.5), (2, 2.0, 1.5), (3, 0.5, 5.0), (64, 5.0, 0.5), (200, 2.0, 1.5)];

#[test]
fn ttm_squeeze_matches_the_bands_and_the_regression() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        let b = bars_of(&c, step);
        let (h, l, cl) = (highs(&b), lows(&b), closes_of(&b));
        for (n, bbm, kcm) in TTM_SETS {
            let got = series::ttm_squeeze(&h, &l, &cl, n as f64, bbm, kcm).unwrap();
            let want = ttm_ref(&h, &l, &cl, n, bbm, kcm);
            max_rel_error(
                &format!("ttm momentum({n}) {level}"),
                &got[0],
                &want[0],
                REL,
                8e-16 * level,
            );
            assert!(same(&got[1], &want[1]), "ttm squeeze({n}) {level}: the flag differs");
            let cn = with_nan(cl.clone(), 1500);
            let got = series::ttm_squeeze(&h, &l, &cn, n as f64, bbm, kcm).unwrap();
            let want = ttm_ref(&h, &l, &cn, n, bbm, kcm);
            max_rel_error(
                &format!("ttm momentum({n}) NaN {level}"),
                &got[0],
                &want[0],
                REL,
                8e-16 * level,
            );
            assert!(same(&got[1], &want[1]), "ttm squeeze({n}) NaN {level}: the flag differs");
            let mut hn = h.clone();
            hn[1700] = f64::NAN;
            let got = series::ttm_squeeze(&hn, &l, &cl, n as f64, bbm, kcm).unwrap();
            let want = ttm_ref(&hn, &l, &cl, n, bbm, kcm);
            max_rel_error(
                &format!("ttm momentum({n}) NaN high {level}"),
                &got[0],
                &want[0],
                REL,
                8e-16 * level,
            );
            assert!(same(&got[1], &want[1]), "ttm squeeze({n}) NaN high {level}: the flag differs");
        }
    }
}

#[test]
fn ttm_squeeze_of_a_flat_close_is_on_and_its_momentum_is_the_midpoint_offset() {
    // Closes flat at 100 with a range of 2: the deviation is exactly 0, so the Bollinger bands
    // collapse to the mean and sit inside any Keltner channel with a positive range.
    let n = 10usize;
    let h = vec![101.0; 100];
    let l = vec![99.0; 100];
    let c = vec![100.0; 100];
    let [momentum, squeeze] = series::ttm_squeeze(&h, &l, &c, n as f64, 2.0, 1.5).unwrap();
    assert!(squeeze[..n - 1].iter().all(|v| v.is_nan()));
    assert!(squeeze[n - 1..].iter().all(|v| *v == 1.0));
    // close - ((101 + 99) / 2 + 100) / 2 = 0 on every bar, so the fitted line is 0.
    assert!(momentum[2 * n - 2..].iter().all(|v| v.abs() < 1e-12));
}

#[test]
fn the_first_value_is_where_the_registry_says() {
    let c = closes(100.0, 2.0, 900);
    let b = bars_of(&c, 2.0);
    let (o, h, l, cl) = (opens(&b), highs(&b), lows(&b), closes_of(&b));
    let first = |v: &[f64]| v.iter().position(|x| !x.is_nan());
    for n in [2usize, 5, 20, 64, 200] {
        let nf = n as f64;
        assert_eq!(
            first(&series::zlema(&c, nf).unwrap()),
            Some(Zlema::new(nf).unwrap().lookback())
        );
        assert_eq!(
            first(&series::qstick(&o, &cl, nf).unwrap()),
            Some(Qstick::new(nf).unwrap().lookback())
        );
        assert_eq!(
            first(&series::rwi(&h, &l, &cl, nf).unwrap()[0]),
            Some(Rwi::new(nf).unwrap().lookback())
        );
        assert_eq!(
            first(&series::acceleration_bands(&h, &l, &cl, nf, 4.0).unwrap()[0]),
            Some(AccelerationBands::new(nf, 4.0).unwrap().lookback())
        );
        let t = TtmSqueeze::new(nf, 2.0, 1.5).unwrap();
        let got = series::ttm_squeeze(&h, &l, &cl, nf, 2.0, 1.5).unwrap();
        assert_eq!(first(&got[1]), Some(t.lookback()));
        assert_eq!(first(&got[0]), Some(t.lookback_full()));
    }
    for (long, short) in [(25.0, 13.0), (4.0, 2.0), (40.0, 20.0)] {
        assert_eq!(
            first(&series::tsi(&c, long, short).unwrap()),
            Some(Tsi::new(long, short).unwrap().lookback())
        );
        let m = SmiErgodic::new(long, short, 5.0).unwrap();
        let [smi, sig] = series::smi_ergodic(&c, long, short, 5.0).unwrap();
        assert_eq!(first(&smi), Some(m.lookback()));
        assert_eq!(first(&sig), Some(m.lookback_full()));
    }
    for (np, ne) in [(25.0, 9.0), (2.0, 2.0), (10.0, 5.0)] {
        assert_eq!(
            first(&series::mass(&h, &l, np, ne).unwrap()),
            Some(Mass::new(np, ne).unwrap().lookback())
        );
    }
    assert_eq!(
        first(&series::coppock(&c, 10.0, 14.0, 11.0).unwrap()),
        Some(Coppock::new(10.0, 14.0, 11.0).unwrap().lookback())
    );
    let k = Kst::default();
    let [kst, sig] = series::kst(&c, 10.0, 10.0, 15.0, 10.0, 20.0, 10.0, 30.0, 15.0, 9.0).unwrap();
    assert_eq!(first(&kst), Some(k.lookback()));
    assert_eq!(first(&sig), Some(k.lookback_full()));
}

// ------------------------------------------------------------------ every way in, the same bits

fn lengths(n: usize) -> [usize; 8] {
    [0, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600]
}

/// Bars with a NaN close a third of the way in and a NaN high two thirds of the way in.
fn bars_for(len: usize) -> Vec<Bar> {
    let mut c = closes(100.0, 2.0, len);
    if len > 40 {
        c[len / 3] = f64::NAN;
    }
    let mut b = bars_of(&c, 2.0);
    if len > 40 {
        b[2 * len / 3].high = f64::NAN;
    }
    b
}

/// The four ways in for a one-line indicator that reads a [`Bar`] and a plain-number entry.
fn check_one<I: Indicator + Clone>(
    name: &str,
    fresh: impl Fn() -> I,
    b: &[Bar],
    plain: impl Fn(&mut I, &Bar) -> f64,
    nextbar: impl Fn(&mut I, &Bar) -> f64,
    sliced: &[f64],
) {
    let mut ind = fresh();
    let stream: Vec<f64> = b.iter().map(|x| nextbar(&mut ind, x)).collect();
    let mut ind = fresh();
    let value: Vec<f64> = b.iter().map(|x| plain(&mut ind, x)).collect();
    assert!(same(&stream, &value), "{name}: next vs the plain-number entry");
    assert!(same(&stream, &fresh().vectorize(b)[0]), "{name}: vectorize");
    assert!(same(&stream, sliced), "{name}: series");
    // `on_bar` is `next`.
    let mut ind = fresh();
    let via: Vec<f64> = b.iter().map(|x| ind.on_bar(x)[0]).collect();
    assert!(same(&stream, &via), "{name}: on_bar");
}

fn check_lines<I: Indicator + Clone, const N: usize>(
    name: &str,
    fresh: impl Fn() -> I,
    b: &[Bar],
    plain: impl Fn(&mut I, &Bar) -> [f64; N],
    nextbar: impl Fn(&mut I, &Bar) -> [f64; N],
    sliced: &[Vec<f64>],
) {
    let mut ind = fresh();
    let stream: Vec<[f64; N]> = b.iter().map(|x| nextbar(&mut ind, x)).collect();
    let mut ind = fresh();
    let value: Vec<[f64; N]> = b.iter().map(|x| plain(&mut ind, x)).collect();
    let batch = fresh().vectorize(b);
    let mut ind = fresh();
    let via: Vec<Vec<f64>> = b.iter().map(|x| ind.on_bar(x)).collect();
    assert_eq!(batch.len(), N, "{name}: lines");
    assert_eq!(sliced.len(), N, "{name}: series lines");
    for k in 0..N {
        let s: Vec<f64> = stream.iter().map(|v| v[k]).collect();
        let v: Vec<f64> = value.iter().map(|v| v[k]).collect();
        let o: Vec<f64> = via.iter().map(|v| v[k]).collect();
        assert!(same(&s, &v), "{name}[{k}]: next vs the plain-number entry");
        assert!(same(&s, &batch[k]), "{name}[{k}]: vectorize");
        assert!(same(&s, &sliced[k]), "{name}[{k}]: series");
        assert!(same(&s, &o), "{name}[{k}]: on_bar");
    }
}

#[test]
fn every_way_in_gives_the_same_bits_for_zlema_tsi_coppock_and_qstick() {
    for n in [2usize, 3, 7, 20, 64, 200, 400] {
        for len in lengths(n) {
            let b = bars_for(len);
            let (o, cl) = (opens(&b), closes_of(&b));
            let nf = n as f64;
            check_one(
                &format!("zlema n={n} len={len}"),
                || Zlema::new(nf).unwrap(),
                &b,
                |i, x| i.next_value(x.close),
                |i, x| i.next(x),
                &series::zlema(&cl, nf).unwrap(),
            );
            if n <= 200 {
                check_one(
                    &format!("qstick n={n} len={len}"),
                    || Qstick::new(nf).unwrap(),
                    &b,
                    |i, x| i.next_oc(x.open, x.close),
                    |i, x| i.next(x),
                    &series::qstick(&o, &cl, nf).unwrap(),
                );
            }
            let short = (n / 2).clamp(2, 200) as f64;
            check_one(
                &format!("tsi n={n} len={len}"),
                || Tsi::new(nf, short).unwrap(),
                &b,
                |i, x| i.next_value(x.close),
                |i, x| i.next(x),
                &series::tsi(&cl, nf, short).unwrap(),
            );
            let w = n.clamp(2, 100) as f64;
            let r = n.clamp(2, 200) as f64;
            check_one(
                &format!("coppock n={n} len={len}"),
                || Coppock::new(w, r, (n / 2).clamp(2, 200) as f64).unwrap(),
                &b,
                |i, x| i.next_value(x.close),
                |i, x| i.next(x),
                &series::coppock(&cl, w, r, (n / 2).clamp(2, 200) as f64).unwrap(),
            );
        }
    }
}

#[test]
fn every_way_in_gives_the_same_bits_for_mass_and_the_multi_line_indicators() {
    for n in [2usize, 3, 7, 20, 64, 200, 400] {
        for len in lengths(n) {
            let b = bars_for(len);
            let (h, l, cl) = (highs(&b), lows(&b), closes_of(&b));
            let nf = n as f64;
            let np = n.clamp(2, 200) as f64;
            let ne = (n / 3).clamp(2, 50) as f64;
            check_one(
                &format!("mass n={n} len={len}"),
                || Mass::new(np, ne).unwrap(),
                &b,
                |i, x| i.next_hl(x.high, x.low),
                |i, x| i.next(x),
                &series::mass(&h, &l, np, ne).unwrap(),
            );
            let short = (n / 2).clamp(2, 200) as f64;
            let sig = (n / 3).clamp(2, 200) as f64;
            check_lines(
                &format!("smi_ergodic n={n} len={len}"),
                || SmiErgodic::new(nf, short, sig).unwrap(),
                &b,
                |i, x| i.next_value(x.close),
                |i, x| i.next(x),
                &series::smi_ergodic(&cl, nf, short, sig).unwrap(),
            );
            let m = |k: f64| (nf * k).round().clamp(1.0, 200.0);
            let s = |k: f64| (nf * k).round().clamp(2.0, 200.0);
            let params = [m(0.5), s(0.5), m(1.0), s(0.75), m(1.5), s(1.0), m(2.0), s(1.5), s(0.25)];
            check_lines(
                &format!("kst n={n} len={len}"),
                || {
                    let p = params;
                    Kst::new(p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8]).unwrap()
                },
                &b,
                |i, x| i.next_value(x.close),
                |i, x| i.next(x),
                &series::kst(
                    &cl, params[0], params[1], params[2], params[3], params[4], params[5],
                    params[6], params[7], params[8],
                )
                .unwrap(),
            );
            if n <= 200 {
                let nf = n as f64;
                check_lines(
                    &format!("rwi n={n} len={len}"),
                    || Rwi::new(nf).unwrap(),
                    &b,
                    |i, x| i.next_hlc(x.high, x.low, x.close),
                    |i, x| i.next(x),
                    &series::rwi(&h, &l, &cl, nf).unwrap(),
                );
                check_lines(
                    &format!("ttm n={n} len={len}"),
                    || TtmSqueeze::new(nf, 2.0, 1.5).unwrap(),
                    &b,
                    |i, x| i.next_hlc(x.high, x.low, x.close),
                    |i, x| i.next(x),
                    &series::ttm_squeeze(&h, &l, &cl, nf, 2.0, 1.5).unwrap(),
                );
            }
            check_lines(
                &format!("acceleration_bands n={n} len={len}"),
                || AccelerationBands::new(nf, 4.0).unwrap(),
                &b,
                |i, x| i.next_hlc(x.high, x.low, x.close),
                |i, x| i.next(x),
                &series::acceleration_bands(&h, &l, &cl, nf, 4.0).unwrap(),
            );
        }
    }
}

#[test]
fn reset_returns_to_the_first_bar() {
    let b = bars_for(900);
    fn run<I: Indicator>(ind: &mut I, b: &[Bar]) -> Vec<Vec<f64>> {
        b.iter().map(|x| ind.on_bar(x)).collect()
    }
    fn again<I: Indicator>(name: &str, mut ind: I, b: &[Bar]) {
        let first = run(&mut ind, b);
        ind.reset();
        let second = run(&mut ind, b);
        for (x, y) in first.iter().zip(&second) {
            assert!(same(x, y), "{name}: reset");
        }
    }
    again("zlema", Zlema::default(), &b);
    again("coppock", Coppock::default(), &b);
    again("tsi", Tsi::default(), &b);
    again("smi_ergodic", SmiErgodic::default(), &b);
    again("mass", Mass::default(), &b);
    again("kst", Kst::default(), &b);
    again("qstick", Qstick::default(), &b);
    again("rwi", Rwi::default(), &b);
    again("acceleration_bands", AccelerationBands::default(), &b);
    again("ttm_squeeze", TtmSqueeze::default(), &b);
}

#[test]
fn a_parameter_outside_the_registry_range_is_refused() {
    let c = closes(100.0, 2.0, 50);
    assert!(series::zlema(&c, 1.0).is_err() && series::zlema(&c, f64::NAN).is_err());
    assert!(series::coppock(&c, 1.0, 14.0, 11.0).is_err());
    assert!(series::tsi(&c, 25.0, 201.0).is_err());
    assert!(series::smi_ergodic(&c, 20.0, 5.0, 1.0).is_err());
    assert!(series::mass(&c, &c, 25.0, 51.0).is_err());
    assert!(series::kst(&c, 0.0, 10.0, 15.0, 10.0, 20.0, 10.0, 30.0, 15.0, 9.0).is_err());
    assert!(series::qstick(&c, &c, 1.0).is_err());
    assert!(series::rwi(&c, &c, &c, 201.0).is_err());
    assert!(series::acceleration_bands(&c, &c, &c, 20.0, 0.1).is_err());
    assert!(series::ttm_squeeze(&c, &c, &c, 20.0, 2.0, 6.0).is_err());
}

/// The numbers the report quotes: the worst relative error against the textbook, per indicator,
/// over every scale, window and NaN position this file uses.
///
/// cargo test -p vike-indicators --release --test fast_i -- --ignored --nocapture
#[test]
#[ignore = "prints the measured worst errors"]
fn report_worst_errors() {
    let mut worst = [0.0_f64; 10];
    let names = [
        "zlema",
        "coppock",
        "tsi",
        "smi_ergodic",
        "mass",
        "kst",
        "qstick",
        "rwi",
        "acceleration_bands",
        "ttm_squeeze",
    ];
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        let b = bars_of(&c, step);
        let (o, h, l, cl) = (opens(&b), highs(&b), lows(&b), closes_of(&b));
        let mut upd = |k: usize, e: f64| worst[k] = worst[k].max(e);
        for n in [2usize, 3, 5, 20, 64, 200, 400] {
            upd(
                0,
                max_rel_error(
                    "z",
                    &series::zlema(&c, n as f64).unwrap(),
                    &zlema_ref(&c, n),
                    1.0,
                    0.0,
                ),
            );
        }
        for (w, long, short) in
            [(10usize, 14usize, 11usize), (2, 2, 2), (100, 200, 150), (7, 20, 2)]
        {
            let got = series::coppock(&c, w as f64, long as f64, short as f64).unwrap();
            upd(1, max_rel_error("c", &got, &coppock_ref(&c, w, long, short), 1.0, 0.0));
        }
        for (long, short) in [(25usize, 13usize), (2, 2), (64, 32), (400, 200)] {
            let got = series::tsi(&c, long as f64, short as f64).unwrap();
            upd(2, max_rel_error("t", &got, &tsi_ratio(&c, long, short), 1.0, 0.0));
            let [smi, sig] = series::smi_ergodic(&c, long as f64, short as f64, 5.0).unwrap();
            let [ws, wg] = smi_ref(&c, long, short, 5);
            upd(
                3,
                max_rel_error("s", &smi, &ws, 1.0, 0.0)
                    .max(max_rel_error("g", &sig, &wg, 1.0, 0.0)),
            );
        }
        for (np, ne) in [(25usize, 9usize), (2, 2), (200, 50)] {
            let got = series::mass(&h, &l, np as f64, ne as f64).unwrap();
            upd(4, max_rel_error("m", &got, &mass_ref(&h, &l, np, ne), 1.0, 0.0));
        }
        for p in KST_SETS {
            let [k, s] = kst_series(&c, p);
            let [wk, ws] = kst_ref(&c, p);
            upd(
                5,
                max_rel_error("k", &k, &wk, 1.0, 0.0).max(max_rel_error("ks", &s, &ws, 1.0, 0.0)),
            );
        }
        for n in [2usize, 8, 64, 200] {
            let got = series::qstick(&o, &cl, n as f64).unwrap();
            upd(6, max_rel_error("q", &got, &qstick_ref(&o, &cl, n), 1.0, 0.0));
        }
        for n in [2usize, 14, 64, 200] {
            let got = series::rwi(&h, &l, &cl, n as f64).unwrap();
            let want = rwi_ref(&h, &l, &cl, n);
            upd(
                7,
                max_rel_error("r", &got[0], &want[0], 1.0, 0.0)
                    .max(max_rel_error("r", &got[1], &want[1], 1.0, 0.0)),
            );
        }
        for (n, f) in [(2usize, 0.5), (20, 4.0), (400, 2.5)] {
            let got = series::acceleration_bands(&h, &l, &cl, n as f64, f).unwrap();
            let want = accel_ref(&h, &l, &cl, n, f);
            let e =
                (0..3).map(|k| max_rel_error("a", &got[k], &want[k], 1.0, 0.0)).fold(0.0, f64::max);
            upd(8, e);
        }
        for (n, bbm, kcm) in TTM_SETS {
            let got = series::ttm_squeeze(&h, &l, &cl, n as f64, bbm, kcm).unwrap();
            let want = ttm_ref(&h, &l, &cl, n, bbm, kcm);
            // Relative to the value, with the absolute slack of the level noted in the module doc.
            upd(9, max_rel_error("t", &got[0], &want[0], 1.0, 8e-16 * level));
        }
    }
    for (n, w) in names.iter().zip(worst) {
        println!("worst relative error: {n} {w:e}");
    }
}

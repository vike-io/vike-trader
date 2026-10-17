//! `mom`, `cmo`, `aroon`, `stochf`, `mfi` and `cmf`: the bounded-state steppers against a textbook
//! reference, and the stream, the batch (`vectorize`), the plain-number entry (`next_value`,
//! `next_hl`, `next_hlc`, `next_hlcv`) and the slice call (`series::*`) against each other, bit for
//! bit.
//!
//! The reference does each step the long way: a window folded from scratch, the old kernels'
//! expressions. The steppers carry exact suffix sums and monotonic deques and use a few cheaper
//! equivalent forms (`100 * up / (up + down)` for the money flow index), so the last bits can differ
//! from the reference; the tolerance is 1e-9 relative on three scales of series. `mom` and `aroon`
//! do no arithmetic that rounds differently from the reference and are compared bit for bit.

use vike_indicators::indicators::{Aroon, Cmf, Cmo, Mfi, Mom, Stochf};
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

/// A bar around each close with a spread that varies, a volume that varies, and a stretch of bars
/// with no volume.
fn bars_of(c: &[f64], scale: f64) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let up = (0.3 + (i % 7) as f64 * 0.1) * scale;
            let dn = (0.2 + (i % 5) as f64 * 0.15) * scale;
            let vol = if (150..190).contains(&i) { 0.0 } else { 100.0 + (i % 13) as f64 * 7.0 };
            Bar::ohlcv(i as i64 * 60_000, x, x + up, x - dn, x, vol)
        })
        .collect()
}

struct Cols {
    h: Vec<f64>,
    l: Vec<f64>,
    c: Vec<f64>,
    v: Vec<f64>,
}

fn cols(b: &[Bar]) -> Cols {
    Cols {
        h: b.iter().map(|x| x.high).collect(),
        l: b.iter().map(|x| x.low).collect(),
        c: b.iter().map(|x| x.close).collect(),
        v: b.iter().map(|x| x.volume).collect(),
    }
}

// ---- the textbook references ----------------------------------------------------------------

fn mom_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    for i in n..c.len() {
        out[i] = c[i] - c[i - n];
    }
    out
}

fn cmo_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    for (i, o) in out.iter_mut().enumerate().skip(n) {
        let (mut up, mut dn) = (0.0, 0.0);
        for j in (i - n + 1)..=i {
            let d = c[j] - c[j - 1];
            if d > 0.0 {
                up += d;
            } else {
                dn += -d;
            }
        }
        let denom = up + dn;
        *o = if denom != 0.0 { 100.0 * (up - dn) / denom } else { 0.0 };
    }
    out
}

/// The kernel's definition; a window with no number in it is `NaN` (the kernel panicked there).
fn aroon_ref(h: &[f64], l: &[f64], n: usize) -> [Vec<f64>; 2] {
    let mut out = [vec![f64::NAN; h.len()], vec![f64::NAN; h.len()]];
    for i in n..h.len() {
        let (wh, wl) = (&h[i - n..=i], &l[i - n..=i]);
        let max_h = wh.iter().cloned().fold(f64::MIN, f64::max);
        let min_l = wl.iter().cloned().fold(f64::MAX, f64::min);
        if let Some(k) = (0..=n).find(|&k| wh[n - k] == max_h) {
            out[0][i] = 100.0 * (n - k) as f64 / n as f64;
        }
        if let Some(k) = (0..=n).find(|&k| wl[n - k] == min_l) {
            out[1][i] = 100.0 * (n - k) as f64 / n as f64;
        }
    }
    out
}

fn stochf_ref(h: &[f64], l: &[f64], c: &[f64], k: usize, d: usize) -> [Vec<f64>; 2] {
    let len = c.len();
    let mut kl = vec![f64::NAN; len];
    for i in (k - 1)..len {
        let hh = h[i + 1 - k..=i].iter().cloned().fold(f64::MIN, f64::max);
        let ll = l[i + 1 - k..=i].iter().cloned().fold(f64::MAX, f64::min);
        let rng = hh - ll;
        kl[i] = if rng != 0.0 { 100.0 * (c[i] - ll) / rng } else { 0.0 };
    }
    let mut dl = vec![f64::NAN; len];
    for i in (k - 1 + d - 1)..len {
        dl[i] = kl[i + 1 - d..=i].iter().sum::<f64>() / d as f64;
    }
    [kl, dl]
}

fn mfi_ref(h: &[f64], l: &[f64], c: &[f64], v: &[f64], n: usize) -> Vec<f64> {
    let len = c.len();
    let tp: Vec<f64> = (0..len).map(|i| (h[i] + l[i] + c[i]) / 3.0).collect();
    let mut out = vec![f64::NAN; len];
    for (i, o) in out.iter_mut().enumerate().skip(n) {
        let (mut pos, mut neg) = (0.0, 0.0);
        for j in (i + 1 - n)..=i {
            if tp[j] > tp[j - 1] {
                pos += tp[j] * v[j];
            } else if tp[j] < tp[j - 1] {
                neg += tp[j] * v[j];
            }
        }
        *o = if neg == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + pos / neg) };
    }
    out
}

fn cmf_ref(h: &[f64], l: &[f64], c: &[f64], v: &[f64], n: usize) -> Vec<f64> {
    let len = c.len();
    let clv: Vec<f64> = (0..len)
        .map(|i| {
            let rng = h[i] - l[i];
            if rng != 0.0 { ((c[i] - l[i]) - (h[i] - c[i])) / rng } else { 0.0 }
        })
        .collect();
    let mut out = vec![f64::NAN; len];
    for (i, o) in out.iter_mut().enumerate().skip(n - 1) {
        let (lo, hi) = (i + 1 - n, i);
        let flow: f64 = (lo..=hi).map(|j| clv[j] * v[j]).sum();
        let vol: f64 = v[lo..=hi].iter().sum();
        *o = if vol != 0.0 { flow / vol } else { f64::NAN };
    }
    out
}

// ---- comparisons ------------------------------------------------------------------------------

fn assert_close(name: &str, got: &[f64], want: &[f64], rel: f64) {
    assert_eq!(got.len(), want.len(), "{name}: length");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if w.is_nan() {
            assert!(g.is_nan(), "{name}[{i}]: want NaN, got {g}");
        } else {
            let err = (g - w).abs() / w.abs().max(1.0);
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

/// Each scale, clean and with a NaN planted in the middle of every column.
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

#[test]
fn mom_matches_the_subtraction_bit_for_bit() {
    for (what, x) in inputs() {
        for n in [1usize, 2, 10, 64, 400] {
            let got = series::mom(&x.c, n as f64).unwrap();
            assert!(same(&got, &mom_ref(&x.c, n)), "mom({n}) {what}");
        }
    }
}

#[test]
fn cmo_matches_the_window_fold() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 14, 64, 200] {
            assert_close(
                &format!("cmo({n}) {what}"),
                &series::cmo(&x.c, n as f64).unwrap(),
                &cmo_ref(&x.c, n),
                1e-9,
            );
        }
    }
}

#[test]
fn aroon_matches_the_window_scan_bit_for_bit() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 14, 64, 400] {
            let got = series::aroon(&x.h, &x.l, n as f64).unwrap();
            let want = aroon_ref(&x.h, &x.l, n);
            for k in 0..2 {
                assert!(same(&got[k], &want[k]), "aroon({n}) line {k} {what}");
            }
        }
    }
}

#[test]
fn stochf_matches_the_window_fold() {
    for (what, x) in inputs() {
        for (k, d) in [(2usize, 1usize), (3, 2), (14, 3), (64, 50), (100, 7)] {
            let got = series::stochf(&x.h, &x.l, &x.c, k as f64, d as f64).unwrap();
            let want = stochf_ref(&x.h, &x.l, &x.c, k, d);
            for line in 0..2 {
                assert_close(
                    &format!("stochf({k},{d}) line {line} {what}"),
                    &got[line],
                    &want[line],
                    1e-9,
                );
            }
        }
    }
}

#[test]
fn mfi_matches_the_window_fold() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 14, 64, 100] {
            assert_close(
                &format!("mfi({n}) {what}"),
                &series::mfi(&x.h, &x.l, &x.c, &x.v, n as f64).unwrap(),
                &mfi_ref(&x.h, &x.l, &x.c, &x.v, n),
                1e-9,
            );
        }
    }
}

#[test]
fn cmf_matches_the_window_fold() {
    for (what, x) in inputs() {
        for n in [2usize, 3, 20, 64, 200] {
            assert_close(
                &format!("cmf({n}) {what}"),
                &series::cmf(&x.h, &x.l, &x.c, &x.v, n as f64).unwrap(),
                &cmf_ref(&x.h, &x.l, &x.c, &x.v, n),
                1e-9,
            );
        }
    }
}

/// The reason the window sums are rebuilt per lap and never carried: once a big move has left the
/// window, a stretch of equal prices is EXACTLY zero movement (`cmo` 0, `mfi` 100, `%K` 0), not a
/// rounding residue that the ratio then turns into a number.
#[test]
fn a_flat_stretch_is_exactly_flat_after_a_big_move() {
    let mut c = closes(100.0, 2.0, 400);
    c.extend(std::iter::repeat_n(5000.0, 400));
    let b: Vec<Bar> =
        c.iter().enumerate().map(|(i, &x)| Bar::ohlcv(i as i64, x, x, x, x, 250.0)).collect();
    let x = cols(&b);
    for n in [2usize, 14, 50, 100] {
        let cmo = series::cmo(&x.c, n as f64).unwrap();
        let mfi = series::mfi(&x.h, &x.l, &x.c, &x.v, n as f64).unwrap();
        for i in 400 + n..800 {
            assert_eq!(cmo[i], 0.0, "cmo({n})[{i}]");
            assert_eq!(mfi[i], 100.0, "mfi({n})[{i}]");
        }
    }
    let [k, _] = series::stochf(&x.h, &x.l, &x.c, 14.0, 3.0).unwrap();
    assert!(k[400 + 14..].iter().all(|&v| v == 0.0), "%K of a flat window is 0");
}

/// A stretch with no volume at all has no money flow: `NaN`, and exactly the textbook number once
/// volume returns.
#[test]
fn a_volumeless_window_is_nan_and_recovers_exactly() {
    let c = closes(100.0, 2.0, 400);
    let b = bars_of(&c, 1.0);
    let x = cols(&b);
    let got = series::cmf(&x.h, &x.l, &x.c, &x.v, 20.0).unwrap();
    // Volume is 0 on bars 150..190: the window ending at 169..=189 holds none.
    for (i, v) in got.iter().enumerate().take(190).skip(169) {
        assert!(v.is_nan(), "cmf[{i}] holds no volume, got {v}");
    }
    assert!(got[190].is_finite(), "cmf[190] has volume again");
    assert_close("cmf", &got, &cmf_ref(&x.h, &x.l, &x.c, &x.v, 20), 1e-9);
}

/// A window with no price in it (all `NaN`) answers `NaN` and does not panic; the next real window
/// is exact.
#[test]
fn a_window_of_nan_is_nan_and_recovers() {
    let c = closes(100.0, 2.0, 300);
    let mut x = cols(&bars_of(&c, 1.0));
    for i in 100..130 {
        x.h[i] = f64::NAN;
        x.l[i] = f64::NAN;
    }
    let a = series::aroon(&x.h, &x.l, 14.0).unwrap();
    for (i, (u, d)) in a[0].iter().zip(&a[1]).enumerate().take(130).skip(115) {
        assert!(u.is_nan() && d.is_nan(), "aroon[{i}] has no number in its window");
    }
    let want = aroon_ref(&x.h, &x.l, 14);
    assert!(same(&a[0], &want[0]) && same(&a[1], &want[1]));
    let [k, _] = series::stochf(&x.h, &x.l, &x.c, 14.0, 3.0).unwrap();
    assert!(k[115..130].iter().all(|v| v.is_nan()), "%K with no high or low in the window");
    assert!(k[150].is_finite());
}

/// Stream == next_value == vectorize == series, bit for bit, over periods, lengths and a NaN.
#[test]
fn every_way_in_gives_the_same_bits() {
    let lengths = |n: usize| [0usize, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600];
    let planted = |len: usize, spread: f64| {
        let mut c = closes(100.0, 2.0, len);
        if len > 40 {
            c[len / 3] = f64::NAN;
        }
        bars_of(&c, spread)
    };

    // mom
    for n in [1usize, 2, 7, 20, 64, 400] {
        for len in lengths(n) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let nf = n as f64;
            let mut ind = Mom::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = Mom::new(nf).unwrap();
            let value: Vec<f64> = x.c.iter().map(|&y| ind.next_value(y)).collect();
            assert!(same(&stream, &value), "mom n={n} len={len}: next vs next_value");
            assert!(same(&stream, &Mom::new(nf).unwrap().vectorize(&b)[0]), "mom vectorize");
            assert!(same(&stream, &series::mom(&x.c, nf).unwrap()), "mom series");
        }
    }
    // cmo
    for n in [2usize, 3, 14, 64, 200] {
        for len in lengths(n) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let nf = n as f64;
            let mut ind = Cmo::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = Cmo::new(nf).unwrap();
            let value: Vec<f64> = x.c.iter().map(|&y| ind.next_value(y)).collect();
            assert!(same(&stream, &value), "cmo n={n} len={len}: next vs next_value");
            assert!(same(&stream, &Cmo::new(nf).unwrap().vectorize(&b)[0]), "cmo vectorize");
            assert!(same(&stream, &series::cmo(&x.c, nf).unwrap()), "cmo series");
        }
    }
    // aroon
    for n in [2usize, 3, 14, 64, 400] {
        for len in lengths(n) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let nf = n as f64;
            let mut ind = Aroon::new(nf).unwrap();
            let stream: Vec<[f64; 2]> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = Aroon::new(nf).unwrap();
            let value: Vec<[f64; 2]> = b.iter().map(|y| ind.next_hl(y.high, y.low)).collect();
            let batch = Aroon::new(nf).unwrap().vectorize(&b);
            let sl = series::aroon(&x.h, &x.l, nf).unwrap();
            for k in 0..2 {
                let s: Vec<f64> = stream.iter().map(|v| v[k]).collect();
                let v: Vec<f64> = value.iter().map(|v| v[k]).collect();
                assert!(same(&s, &v), "aroon n={n} len={len} line {k}: next vs next_hl");
                assert!(same(&s, &batch[k]), "aroon n={n} len={len} line {k}: vectorize");
                assert!(same(&s, &sl[k]), "aroon n={n} len={len} line {k}: series");
            }
        }
    }
    // stochf
    for (k, d) in [(2usize, 1usize), (3, 2), (14, 3), (64, 50), (100, 7)] {
        for len in lengths(k + d) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let (kf, df) = (k as f64, d as f64);
            let mut ind = Stochf::new(kf, df).unwrap();
            let stream: Vec<[f64; 2]> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = Stochf::new(kf, df).unwrap();
            let value: Vec<[f64; 2]> =
                b.iter().map(|y| ind.next_hlc(y.high, y.low, y.close)).collect();
            let batch = Stochf::new(kf, df).unwrap().vectorize(&b);
            let sl = series::stochf(&x.h, &x.l, &x.c, kf, df).unwrap();
            for line in 0..2 {
                let s: Vec<f64> = stream.iter().map(|v| v[line]).collect();
                let v: Vec<f64> = value.iter().map(|v| v[line]).collect();
                assert!(same(&s, &v), "stochf ({k},{d}) len={len} line {line}: next vs next_hlc");
                assert!(
                    same(&s, &batch[line]),
                    "stochf ({k},{d}) len={len} line {line}: vectorize"
                );
                assert!(same(&s, &sl[line]), "stochf ({k},{d}) len={len} line {line}: series");
            }
        }
    }
    // mfi and cmf
    for n in [2usize, 3, 14, 64, 100] {
        for len in lengths(n) {
            let b = planted(len, 1.0);
            let x = cols(&b);
            let nf = n as f64;
            let mut ind = Mfi::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = Mfi::new(nf).unwrap();
            let value: Vec<f64> =
                b.iter().map(|y| ind.next_hlcv(y.high, y.low, y.close, y.volume)).collect();
            assert!(same(&stream, &value), "mfi n={n} len={len}: next vs next_hlcv");
            assert!(same(&stream, &Mfi::new(nf).unwrap().vectorize(&b)[0]), "mfi vectorize");
            assert!(same(&stream, &series::mfi(&x.h, &x.l, &x.c, &x.v, nf).unwrap()), "mfi series");

            let mut ind = Cmf::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|y| ind.next(y)).collect();
            let mut ind = Cmf::new(nf).unwrap();
            let value: Vec<f64> =
                b.iter().map(|y| ind.next_hlcv(y.high, y.low, y.close, y.volume)).collect();
            assert!(same(&stream, &value), "cmf n={n} len={len}: next vs next_hlcv");
            assert!(same(&stream, &Cmf::new(nf).unwrap().vectorize(&b)[0]), "cmf vectorize");
            assert!(same(&stream, &series::cmf(&x.h, &x.l, &x.c, &x.v, nf).unwrap()), "cmf series");
        }
    }
}

/// `reset` returns a stepper to its first state, and a clone carries on identically.
#[test]
fn reset_and_clone_restore_the_state() {
    let b = bars_of(&closes(100.0, 2.0, 500), 1.0);
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
    check!(Mom::new(10.0).unwrap());
    check!(Cmo::new(14.0).unwrap());
    check!(Aroon::new(14.0).unwrap());
    check!(Stochf::new(14.0, 3.0).unwrap());
    check!(Mfi::new(14.0).unwrap());
    check!(Cmf::new(20.0).unwrap());
}

#[test]
fn out_of_range_parameters_are_refused() {
    assert!(Mom::new(0.0).is_err() && series::mom(&[1.0], 401.0).is_err());
    assert!(Cmo::new(1.0).is_err() && series::cmo(&[1.0], f64::NAN).is_err());
    assert!(Aroon::new(1.0).is_err() && series::aroon(&[1.0], &[1.0], 401.0).is_err());
    assert!(
        Stochf::new(14.0, 0.0).is_err()
            && series::stochf(&[1.0], &[1.0], &[1.0], 1.0, 3.0).is_err()
    );
    assert!(Mfi::new(101.0).is_err() && series::mfi(&[1.0], &[1.0], &[1.0], &[1.0], 1.0).is_err());
    assert!(Cmf::new(1.0).is_err() && series::cmf(&[1.0], &[1.0], &[1.0], &[1.0], 201.0).is_err());
}

/// The worst relative error of each indicator against its reference over the periods and scales
/// above, printed (`--ignored --nocapture`); the tests above hold it under 1e-9.
#[test]
#[ignore = "a measurement, not a check"]
fn measure_the_worst_relative_error() {
    fn worst(got: &[f64], want: &[f64]) -> f64 {
        got.iter()
            .zip(want)
            .filter(|(g, w)| g.is_finite() && w.is_finite())
            .map(|(g, w)| (g - w).abs() / w.abs().max(1.0))
            .fold(0.0, f64::max)
    }
    let mut w = [0.0_f64; 6];
    for (_, x) in inputs() {
        for n in [2usize, 3, 14, 64, 200] {
            w[1] = w[1].max(worst(&series::cmo(&x.c, n as f64).unwrap(), &cmo_ref(&x.c, n)));
        }
        for n in [1usize, 2, 10, 64, 400] {
            w[0] = w[0].max(worst(&series::mom(&x.c, n as f64).unwrap(), &mom_ref(&x.c, n)));
        }
        for n in [2usize, 3, 14, 64, 400] {
            let (g, r) = (series::aroon(&x.h, &x.l, n as f64).unwrap(), aroon_ref(&x.h, &x.l, n));
            w[2] = w[2].max(worst(&g[0], &r[0])).max(worst(&g[1], &r[1]));
        }
        for (k, d) in [(2usize, 1usize), (3, 2), (14, 3), (64, 50), (100, 7)] {
            let g = series::stochf(&x.h, &x.l, &x.c, k as f64, d as f64).unwrap();
            let r = stochf_ref(&x.h, &x.l, &x.c, k, d);
            w[3] = w[3].max(worst(&g[0], &r[0])).max(worst(&g[1], &r[1]));
        }
        for n in [2usize, 3, 14, 64, 100] {
            let g = series::mfi(&x.h, &x.l, &x.c, &x.v, n as f64).unwrap();
            w[4] = w[4].max(worst(&g, &mfi_ref(&x.h, &x.l, &x.c, &x.v, n)));
        }
        let mut cmf = 0.0_f64;
        for n in [2usize, 3, 20, 64, 200] {
            let g = series::cmf(&x.h, &x.l, &x.c, &x.v, n as f64).unwrap();
            cmf = cmf.max(worst(&g, &cmf_ref(&x.h, &x.l, &x.c, &x.v, n)));
        }
        w[5] = w[5].max(cmf);
    }
    println!(
        "worst relative error: mom {:.2e} cmo {:.2e} aroon {:.2e} stochf {:.2e} mfi {:.2e} cmf {:.2e}",
        w[0], w[1], w[2], w[3], w[4], w[5]
    );
}

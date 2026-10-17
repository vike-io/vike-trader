//! `rocp`, `rocr`, `rocr100`, `bop`, `dpo`, `ac`, `aroonosc`, `elder_ray`, `ultosc` and `vortex`: the
//! bounded-state steppers against a textbook reference, and the stream, the batch (`vectorize`), the
//! plain-number entry (`next_value`, `next_ohlc`, `next_hl`, `next_hlc`) and the slice call
//! (`series::*`) against each other, bit for bit.
//!
//! The reference does each step the long way: a window folded from scratch, the old kernels'
//! expressions (including their `NaN` conventions). The steppers carry running means, exact suffix
//! sums and monotonic windows, so the last bits can differ from the reference; the tolerance is
//! relative 1e-9 on three scales of series, measured against the larger of the value and a floor (a
//! price-unit output has a floor of a ten-thousandth of the price level: a mean of a million-dollar
//! series is only good to its own last digit, and `dpo` and `ac` are differences of such means). The
//! rate-of-change family and `bop` do no arithmetic that rounds differently from the reference and are
//! compared bit for bit; `aroonosc` is one multiplication by `100 / n` away from the reference (1e-12).

use vike_indicators::indicators::{
    Ac, Aroonosc, Bop, Dpo, ElderRay, Rocp, Rocr, Rocr100, Ultosc, Vortex,
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

/// A bar around each close: the open is the previous close, the wicks vary with the bar.
fn bars_of(c: &[f64], scale: f64) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let o = if i == 0 { x } else { c[i - 1] };
            let up = (0.3 + (i % 7) as f64 * 0.1) * scale;
            let dn = (0.2 + (i % 5) as f64 * 0.15) * scale;
            Bar::ohlcv(i as i64 * 60_000, o, o.max(x) + up, o.min(x) - dn, x, 100.0)
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

// ---- the textbook references ----------------------------------------------------------------

fn rocp_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    for i in n..c.len() {
        let prev = c[i - n];
        out[i] = if prev != 0.0 { (c[i] - prev) / prev } else { f64::NAN };
    }
    out
}

fn rocr_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    for i in n..c.len() {
        let prev = c[i - n];
        out[i] = if prev != 0.0 { c[i] / prev } else { f64::NAN };
    }
    out
}

fn rocr100_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    for i in n..c.len() {
        let prev = c[i - n];
        out[i] = if prev != 0.0 { c[i] / prev * 100.0 } else { f64::NAN };
    }
    out
}

fn bop_ref(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
    (0..c.len())
        .map(|i| {
            let rng = h[i] - l[i];
            if rng != 0.0 { (c[i] - o[i]) / rng } else { 0.0 }
        })
        .collect()
}

/// A mean folded from scratch at every place, `NaN` before `n` values.
fn sma_ref(x: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    for i in (n - 1)..x.len() {
        out[i] = x[i + 1 - n..=i].iter().sum::<f64>() / n as f64;
    }
    out
}

fn dpo_ref(c: &[f64], n: usize) -> Vec<f64> {
    let shift = n / 2 + 1;
    let ma = sma_ref(c, n);
    let mut out = vec![f64::NAN; c.len()];
    for i in (n - 1)..c.len() {
        if i >= shift && !ma[i].is_nan() {
            out[i] = c[i - shift] - ma[i];
        }
    }
    out
}

/// The awesome oscillator, then its 5-value mean over the DEFINED values only (the kernel's
/// `smooth_defined`: a gap is skipped, not averaged).
fn ac_ref(h: &[f64], l: &[f64]) -> Vec<f64> {
    let median: Vec<f64> = h.iter().zip(l).map(|(a, b)| (a + b) / 2.0).collect();
    let (s5, s34) = (sma_ref(&median, 5), sma_ref(&median, 34));
    let ao: Vec<f64> = s5.iter().zip(&s34).map(|(a, b)| a - b).collect();
    let defined: Vec<(usize, f64)> =
        ao.iter().enumerate().filter(|(_, v)| !v.is_nan()).map(|(i, v)| (i, *v)).collect();
    let mut out = vec![f64::NAN; ao.len()];
    for k in 4..defined.len() {
        let mean = defined[k - 4..=k].iter().map(|(_, v)| v).sum::<f64>() / 5.0;
        out[defined[k].0] = defined[k].1 - mean;
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

fn aroonosc_ref(h: &[f64], l: &[f64], n: usize) -> Vec<f64> {
    let [up, down] = aroon_ref(h, l, n);
    up.iter().zip(&down).map(|(u, d)| u - d).collect()
}

fn elder_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> [Vec<f64>; 2] {
    let mut ema = vec![f64::NAN; c.len()];
    if c.len() >= n {
        let alpha = 2.0 / (n as f64 + 1.0);
        ema[n - 1] = c[..n].iter().sum::<f64>() / n as f64;
        for i in n..c.len() {
            ema[i] = alpha * c[i] + (1.0 - alpha) * ema[i - 1];
        }
    }
    let bull = h.iter().zip(&ema).map(|(x, e)| x - e).collect();
    let bear = l.iter().zip(&ema).map(|(x, e)| x - e).collect();
    [bull, bear]
}

fn ultosc_ref(h: &[f64], l: &[f64], c: &[f64], p1: usize, p2: usize, p3: usize) -> Vec<f64> {
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    let (mut bp, mut tr) = (vec![0.0; n], vec![0.0; n]);
    for i in 1..n {
        let true_low = l[i].min(c[i - 1]);
        let true_high = h[i].max(c[i - 1]);
        bp[i] = c[i] - true_low;
        tr[i] = true_high - true_low;
    }
    let avg = |p: usize, idx: usize| -> f64 {
        let s_bp: f64 = bp[idx - p + 1..=idx].iter().sum();
        let s_tr: f64 = tr[idx - p + 1..=idx].iter().sum();
        if s_tr != 0.0 { s_bp / s_tr } else { 0.0 }
    };
    for (i, o) in out.iter_mut().enumerate().skip(p1.max(p2).max(p3)) {
        *o = 100.0 * (4.0 * avg(p1, i) + 2.0 * avg(p2, i) + avg(p3, i)) / 7.0;
    }
    out
}

fn vortex_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> [Vec<f64>; 2] {
    let len = c.len();
    let mut tr = vec![0.0; len];
    if len > 0 {
        tr[0] = h[0] - l[0];
    }
    let (mut vmp, mut vmm) = (vec![0.0; len], vec![0.0; len]);
    for i in 1..len {
        tr[i] = (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs());
        vmp[i] = (h[i] - l[i - 1]).abs();
        vmm[i] = (l[i] - h[i - 1]).abs();
    }
    let (mut plus, mut minus) = (vec![f64::NAN; len], vec![f64::NAN; len]);
    for i in n..len {
        let st: f64 = tr[i + 1 - n..=i].iter().sum();
        let sp: f64 = vmp[i + 1 - n..=i].iter().sum();
        let sm: f64 = vmm[i + 1 - n..=i].iter().sum();
        if st > 0.0 {
            plus[i] = sp / st;
            minus[i] = sm / st;
        }
    }
    [plus, minus]
}

// ---- comparisons ------------------------------------------------------------------------------

/// `|got - want| <= rel * max(|want|, floor)` everywhere, `NaN` exactly where the reference has one.
fn assert_close(name: &str, got: &[f64], want: &[f64], rel: f64, floor: f64) {
    assert_eq!(got.len(), want.len(), "{name}: length");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if w.is_nan() {
            assert!(g.is_nan(), "{name}[{i}]: want NaN, got {g}");
        } else {
            let err = (g - w).abs() / w.abs().max(floor);
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
fn inputs() -> Vec<(String, f64, Cols)> {
    let mut v = Vec::new();
    for (level, step, spread) in SCALES {
        for nan in [false, true] {
            let mut c = closes(level, step, 3000);
            if nan {
                c[1200] = f64::NAN;
            }
            v.push((format!("level {level} nan {nan}"), level, cols(&bars_of(&c, spread))));
        }
    }
    v
}

#[test]
fn the_rate_of_change_family_and_bop_match_the_definition_bit_for_bit() {
    for (what, _, x) in inputs() {
        for n in [1usize, 2, 10, 64, 400] {
            let nf = n as f64;
            assert!(same(&series::rocp(&x.c, nf).unwrap(), &rocp_ref(&x.c, n)), "rocp({n}) {what}");
            assert!(same(&series::rocr(&x.c, nf).unwrap(), &rocr_ref(&x.c, n)), "rocr({n}) {what}");
            assert!(
                same(&series::rocr100(&x.c, nf).unwrap(), &rocr100_ref(&x.c, n)),
                "rocr100({n}) {what}"
            );
        }
        assert!(
            same(&series::bop(&x.o, &x.h, &x.l, &x.c), &bop_ref(&x.o, &x.h, &x.l, &x.c)),
            "bop {what}"
        );
    }
}

/// A zero close makes the ratio undefined: `NaN` for the rate of change `period` bars later, and
/// the next window is exact again.
#[test]
fn a_zero_close_makes_the_rate_of_change_nan_exactly_period_bars_later() {
    let mut c = closes(100.0, 2.0, 300);
    c[100] = 0.0;
    for n in [1usize, 5, 20] {
        for (name, got) in [
            ("rocp", series::rocp(&c, n as f64).unwrap()),
            ("rocr", series::rocr(&c, n as f64).unwrap()),
            ("rocr100", series::rocr100(&c, n as f64).unwrap()),
        ] {
            for (i, v) in got.iter().enumerate() {
                assert_eq!(v.is_nan(), i < n || i == 100 + n, "{name}({n})[{i}] = {v}");
            }
        }
    }
}

#[test]
fn dpo_matches_the_window_fold() {
    for (what, level, x) in inputs() {
        for n in [2usize, 3, 4, 20, 64, 400] {
            assert_close(
                &format!("dpo({n}) {what}"),
                &series::dpo(&x.c, n as f64).unwrap(),
                &dpo_ref(&x.c, n),
                1e-9,
                level * 1e-4,
            );
        }
    }
}

#[test]
fn ac_matches_the_awesome_oscillator_minus_its_mean() {
    for (what, level, x) in inputs() {
        assert_close(
            &format!("ac {what}"),
            &series::ac(&x.h, &x.l),
            &ac_ref(&x.h, &x.l),
            1e-9,
            level * 1e-4,
        );
    }
}

#[test]
fn aroonosc_matches_the_window_scan() {
    for (what, _, x) in inputs() {
        for n in [2usize, 3, 14, 64, 400] {
            let got = series::aroonosc(&x.h, &x.l, n as f64).unwrap();
            assert_close(
                &format!("aroonosc({n}) {what}"),
                &got,
                &aroonosc_ref(&x.h, &x.l, n),
                1e-12,
                1.0,
            );
        }
    }
}

#[test]
fn elder_ray_matches_the_textbook_average() {
    for (what, level, x) in inputs() {
        for n in [2usize, 3, 13, 64, 200] {
            let got = series::elder_ray(&x.h, &x.l, &x.c, n as f64).unwrap();
            let want = elder_ref(&x.h, &x.l, &x.c, n);
            for k in 0..2 {
                assert_close(
                    &format!("elder_ray({n}) line {k} {what}"),
                    &got[k],
                    &want[k],
                    1e-9,
                    level * 1e-4,
                );
            }
        }
    }
}

#[test]
fn ultosc_matches_the_window_fold() {
    for (what, _, x) in inputs() {
        for (p1, p2, p3) in
            [(2usize, 3usize, 4usize), (7, 14, 28), (5, 5, 5), (30, 20, 10), (100, 200, 400)]
        {
            assert_close(
                &format!("ultosc({p1},{p2},{p3}) {what}"),
                &series::ultosc(&x.h, &x.l, &x.c, p1 as f64, p2 as f64, p3 as f64).unwrap(),
                &ultosc_ref(&x.h, &x.l, &x.c, p1, p2, p3),
                1e-9,
                1.0,
            );
        }
    }
}

#[test]
fn vortex_matches_the_window_fold() {
    for (what, _, x) in inputs() {
        for n in [2usize, 3, 14, 64, 200] {
            let got = series::vortex(&x.h, &x.l, &x.c, n as f64).unwrap();
            let want = vortex_ref(&x.h, &x.l, &x.c, n);
            for k in 0..2 {
                assert_close(&format!("vortex({n}) line {k} {what}"), &got[k], &want[k], 1e-9, 1.0);
            }
        }
    }
}

/// The reason the window sums are rebuilt per lap and never carried: once a big move has left the
/// window, a stretch of bars with no range at all is EXACTLY no range (`ultosc` 0, `vortex` `NaN`,
/// `bop` 0), not a rounding residue that the ratio then turns into a number.
#[test]
fn a_flat_stretch_is_exactly_flat_after_a_big_move() {
    let mut c = closes(100.0, 2.0, 400);
    c.extend(std::iter::repeat_n(5000.0, 400));
    let b: Vec<Bar> =
        c.iter().enumerate().map(|(i, &x)| Bar::ohlcv(i as i64, x, x, x, x, 250.0)).collect();
    let x = cols(&b);
    let bop = series::bop(&x.o, &x.h, &x.l, &x.c);
    assert!(bop.iter().all(|&v| v == 0.0), "bop of bars with no range is 0");
    for (p1, p2, p3) in [(7usize, 14usize, 28usize), (2, 3, 4), (50, 100, 100)] {
        let u = series::ultosc(&x.h, &x.l, &x.c, p1 as f64, p2 as f64, p3 as f64).unwrap();
        let from = 400 + p1.max(p2).max(p3);
        for (i, v) in u.iter().enumerate().take(800).skip(from) {
            assert_eq!(*v, 0.0, "ultosc({p1},{p2},{p3})[{i}]");
        }
    }
    for n in [2usize, 14, 50] {
        let [plus, minus] = series::vortex(&x.h, &x.l, &x.c, n as f64).unwrap();
        for i in 400 + n..800 {
            assert!(plus[i].is_nan() && minus[i].is_nan(), "vortex({n})[{i}] has no range");
        }
    }
    // Aroon on a flat stretch: the newest bar wins every tie, so both are 100 and the oscillator 0.
    let osc = series::aroonosc(&x.h, &x.l, 14.0).unwrap();
    assert!(osc[400 + 15..].iter().all(|&v| v == 0.0), "aroonosc of a flat stretch is 0");
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
    let a = series::aroonosc(&x.h, &x.l, 14.0).unwrap();
    for (i, v) in a.iter().enumerate().take(130).skip(115) {
        assert!(v.is_nan(), "aroonosc[{i}] has no number in its window");
    }
    assert_close("aroonosc nan windows", &a, &aroonosc_ref(&x.h, &x.l, 14), 1e-12, 1.0);
    // Ultimate oscillator and vortex take the kernels' own conventions for a missing high or low.
    assert_close(
        "ultosc nan wicks",
        &series::ultosc(&x.h, &x.l, &x.c, 7.0, 14.0, 28.0).unwrap(),
        &ultosc_ref(&x.h, &x.l, &x.c, 7, 14, 28),
        1e-9,
        1.0,
    );
    let got = series::vortex(&x.h, &x.l, &x.c, 14.0).unwrap();
    let want = vortex_ref(&x.h, &x.l, &x.c, 14);
    assert_close("vortex nan wicks", &got[0], &want[0], 1e-9, 1.0);
    assert_close("vortex nan wicks", &got[1], &want[1], 1e-9, 1.0);
    // Accelerator: the gap is skipped, and it recovers on the textbook values.
    assert_close("ac nan wicks", &series::ac(&x.h, &x.l), &ac_ref(&x.h, &x.l), 1e-9, 0.01);
}

/// Stream == plain-number entry == `on_bar` == vectorize == series, bit for bit.
fn ways1<I: Indicator + Clone>(
    what: &str,
    ind: I,
    b: &[Bar],
    plain: impl Fn(&mut I, &Bar) -> f64,
    next: impl Fn(&mut I, &Bar) -> f64,
    slice: &[f64],
) {
    let (mut a, mut p, mut o) = (ind.clone(), ind.clone(), ind.clone());
    let stream: Vec<f64> = b.iter().map(|y| next(&mut a, y)).collect();
    let value: Vec<f64> = b.iter().map(|y| plain(&mut p, y)).collect();
    let on_bar: Vec<f64> = b.iter().map(|y| o.on_bar(y)[0]).collect();
    assert!(same(&stream, &value), "{what}: next vs the plain-number entry");
    assert!(same(&stream, &on_bar), "{what}: next vs on_bar");
    assert!(same(&stream, &ind.vectorize(b)[0]), "{what}: vectorize");
    assert!(same(&stream, slice), "{what}: series");
}

fn ways2<I: Indicator + Clone>(
    what: &str,
    ind: I,
    b: &[Bar],
    plain: impl Fn(&mut I, &Bar) -> [f64; 2],
    next: impl Fn(&mut I, &Bar) -> [f64; 2],
    slice: &[Vec<f64>; 2],
) {
    let (mut a, mut p, mut o) = (ind.clone(), ind.clone(), ind.clone());
    let stream: Vec<[f64; 2]> = b.iter().map(|y| next(&mut a, y)).collect();
    let value: Vec<[f64; 2]> = b.iter().map(|y| plain(&mut p, y)).collect();
    let on_bar: Vec<Vec<f64>> = b.iter().map(|y| o.on_bar(y)).collect();
    let batch = ind.vectorize(b);
    for k in 0..2 {
        let s: Vec<f64> = stream.iter().map(|v| v[k]).collect();
        let v: Vec<f64> = value.iter().map(|v| v[k]).collect();
        let ob: Vec<f64> = on_bar.iter().map(|v| v[k]).collect();
        assert!(same(&s, &v), "{what} line {k}: next vs the plain-number entry");
        assert!(same(&s, &ob), "{what} line {k}: next vs on_bar");
        assert!(same(&s, &batch[k]), "{what} line {k}: vectorize");
        assert!(same(&s, &slice[k]), "{what} line {k}: series");
    }
}

#[test]
fn every_way_in_gives_the_same_bits() {
    let lengths = |n: usize| [0usize, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600];
    let planted = |len: usize| {
        let mut c = closes(100.0, 2.0, len);
        if len > 40 {
            c[len / 3] = f64::NAN;
        }
        bars_of(&c, 1.0)
    };

    for n in [1usize, 2, 7, 20, 64, 400] {
        for len in lengths(n) {
            let b = planted(len);
            let x = cols(&b);
            let nf = n as f64;
            ways1(
                &format!("rocp n={n} len={len}"),
                Rocp::new(nf).unwrap(),
                &b,
                |i, y| i.next_value(y.close),
                |i, y| i.next(y),
                &series::rocp(&x.c, nf).unwrap(),
            );
            ways1(
                &format!("rocr n={n} len={len}"),
                Rocr::new(nf).unwrap(),
                &b,
                |i, y| i.next_value(y.close),
                |i, y| i.next(y),
                &series::rocr(&x.c, nf).unwrap(),
            );
            ways1(
                &format!("rocr100 n={n} len={len}"),
                Rocr100::new(nf).unwrap(),
                &b,
                |i, y| i.next_value(y.close),
                |i, y| i.next(y),
                &series::rocr100(&x.c, nf).unwrap(),
            );
        }
    }
    for len in lengths(37) {
        let b = planted(len);
        let x = cols(&b);
        ways1(
            &format!("bop len={len}"),
            Bop::default(),
            &b,
            |i, y| i.next_ohlc(y.open, y.high, y.low, y.close),
            |i, y| i.next(y),
            &series::bop(&x.o, &x.h, &x.l, &x.c),
        );
        ways1(
            &format!("ac len={len}"),
            Ac::default(),
            &b,
            |i, y| i.next_hl(y.high, y.low),
            |i, y| i.next(y),
            &series::ac(&x.h, &x.l),
        );
    }
    for n in [2usize, 3, 4, 20, 64, 400] {
        for len in lengths(n) {
            let b = planted(len);
            let x = cols(&b);
            let nf = n as f64;
            ways1(
                &format!("dpo n={n} len={len}"),
                Dpo::new(nf).unwrap(),
                &b,
                |i, y| i.next_value(y.close),
                |i, y| i.next(y),
                &series::dpo(&x.c, nf).unwrap(),
            );
        }
    }
    for n in [2usize, 3, 14, 64, 400] {
        for len in lengths(n) {
            let b = planted(len);
            let x = cols(&b);
            let nf = n as f64;
            ways1(
                &format!("aroonosc n={n} len={len}"),
                Aroonosc::new(nf).unwrap(),
                &b,
                |i, y| i.next_hl(y.high, y.low),
                |i, y| i.next(y),
                &series::aroonosc(&x.h, &x.l, nf).unwrap(),
            );
        }
    }
    for n in [2usize, 3, 13, 64, 200] {
        for len in lengths(n) {
            let b = planted(len);
            let x = cols(&b);
            let nf = n as f64;
            ways2(
                &format!("elder_ray n={n} len={len}"),
                ElderRay::new(nf).unwrap(),
                &b,
                |i, y| i.next_hlc(y.high, y.low, y.close),
                |i, y| i.next(y),
                &series::elder_ray(&x.h, &x.l, &x.c, nf).unwrap(),
            );
        }
    }
    for n in [2usize, 3, 14, 64, 200] {
        for len in lengths(n) {
            let b = planted(len);
            let x = cols(&b);
            let nf = n as f64;
            ways2(
                &format!("vortex n={n} len={len}"),
                Vortex::new(nf).unwrap(),
                &b,
                |i, y| i.next_hlc(y.high, y.low, y.close),
                |i, y| i.next(y),
                &series::vortex(&x.h, &x.l, &x.c, nf).unwrap(),
            );
        }
    }
    for (p1, p2, p3) in
        [(2usize, 3usize, 4usize), (7, 14, 28), (5, 5, 5), (30, 20, 10), (100, 200, 400)]
    {
        for len in lengths(p1.max(p2).max(p3)) {
            let b = planted(len);
            let x = cols(&b);
            let (a, c, d) = (p1 as f64, p2 as f64, p3 as f64);
            ways1(
                &format!("ultosc ({p1},{p2},{p3}) len={len}"),
                Ultosc::new(a, c, d).unwrap(),
                &b,
                |i, y| i.next_hlc(y.high, y.low, y.close),
                |i, y| i.next(y),
                &series::ultosc(&x.h, &x.l, &x.c, a, c, d).unwrap(),
            );
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
    check!(Rocp::new(10.0).unwrap());
    check!(Rocr::new(10.0).unwrap());
    check!(Rocr100::new(10.0).unwrap());
    check!(Bop::default());
    check!(Dpo::new(20.0).unwrap());
    check!(Ac::default());
    check!(Aroonosc::new(14.0).unwrap());
    check!(ElderRay::new(13.0).unwrap());
    check!(Ultosc::new(7.0, 14.0, 28.0).unwrap());
    check!(Vortex::new(14.0).unwrap());
}

/// The warm-up, the names and the defaults are the registry row's.
#[test]
fn warm_up_and_names_are_unchanged() {
    let b = bars_of(&closes(100.0, 2.0, 600), 1.0);
    let first_defined = |ind: &dyn Indicator| -> Vec<usize> {
        let cols = ind.vectorize(&b);
        cols.iter().map(|c| c.iter().position(|v| !v.is_nan()).unwrap()).collect()
    };
    assert_eq!(first_defined(&Rocp::default()), [10]);
    assert_eq!(first_defined(&Rocr::default()), [10]);
    assert_eq!(first_defined(&Rocr100::default()), [10]);
    assert_eq!(first_defined(&Bop::default()), [0]);
    assert_eq!(first_defined(&Dpo::default()), [19]);
    assert_eq!(first_defined(&Ac::default()), [37]);
    assert_eq!(first_defined(&Aroonosc::default()), [14]);
    assert_eq!(first_defined(&ElderRay::default()), [12, 12]);
    assert_eq!(first_defined(&Ultosc::default()), [28]);
    assert_eq!(first_defined(&Vortex::default()), [14, 14]);
    for (ind, lookback) in [
        (Box::new(Rocp::default()) as Box<dyn Indicator>, 10usize),
        (Box::new(Dpo::default()), 19),
        (Box::new(Ac::default()), 37),
        (Box::new(ElderRay::default()), 12),
        (Box::new(Ultosc::default()), 28),
        (Box::new(Vortex::default()), 14),
    ] {
        assert_eq!(ind.lookback(), lookback, "{}", ind.name());
        assert!(ind.lookback_exact());
    }
}

#[test]
fn out_of_range_parameters_are_refused() {
    assert!(Rocp::new(0.0).is_err() && series::rocp(&[1.0], 401.0).is_err());
    assert!(Rocr::new(f64::NAN).is_err() && series::rocr(&[1.0], 0.0).is_err());
    assert!(Rocr100::new(401.0).is_err() && series::rocr100(&[1.0], 0.5).is_err());
    assert!(Dpo::new(1.0).is_err() && series::dpo(&[1.0], 401.0).is_err());
    assert!(Aroonosc::new(1.0).is_err() && series::aroonosc(&[1.0], &[1.0], 401.0).is_err());
    assert!(
        ElderRay::new(1.0).is_err() && series::elder_ray(&[1.0], &[1.0], &[1.0], 201.0).is_err()
    );
    assert!(
        Ultosc::new(1.0, 14.0, 28.0).is_err()
            && Ultosc::new(7.0, 14.0, 401.0).is_err()
            && series::ultosc(&[1.0], &[1.0], &[1.0], 7.0, f64::NAN, 28.0).is_err()
    );
    assert!(Vortex::new(1.0).is_err() && series::vortex(&[1.0], &[1.0], &[1.0], 201.0).is_err());
}

/// The worst relative error of each indicator against its reference over the periods and scales
/// above, printed (`--ignored --nocapture`); the tests above hold it under 1e-9.
#[test]
#[ignore = "a measurement, not a check"]
fn measure_the_worst_relative_error() {
    fn worst(got: &[f64], want: &[f64], floor: f64) -> f64 {
        got.iter()
            .zip(want)
            .filter(|(g, w)| g.is_finite() && w.is_finite())
            .map(|(g, w)| (g - w).abs() / w.abs().max(floor))
            .fold(0.0, f64::max)
    }
    let mut w = [0.0_f64; 10];
    let mut w_abs = [0.0_f64; 10];
    for (_, level, x) in inputs() {
        let fl = level * 1e-4;
        for n in [2usize, 3, 4, 20, 64, 400] {
            let (g, r) = (series::dpo(&x.c, n as f64).unwrap(), dpo_ref(&x.c, n));
            w[4] = w[4].max(worst(&g, &r, fl));
            w_abs[4] = w_abs[4].max(worst(&g, &r, 1.0));
        }
        let (g, r) = (series::ac(&x.h, &x.l), ac_ref(&x.h, &x.l));
        w[5] = w[5].max(worst(&g, &r, fl));
        w_abs[5] = w_abs[5].max(worst(&g, &r, 1.0));
        for n in [2usize, 3, 13, 64, 200] {
            let g = series::elder_ray(&x.h, &x.l, &x.c, n as f64).unwrap();
            let r = elder_ref(&x.h, &x.l, &x.c, n);
            for k in 0..2 {
                w[7] = w[7].max(worst(&g[k], &r[k], fl));
                w_abs[7] = w_abs[7].max(worst(&g[k], &r[k], 1.0));
            }
        }
        for (p1, p2, p3) in
            [(2usize, 3usize, 4usize), (7, 14, 28), (5, 5, 5), (30, 20, 10), (100, 200, 400)]
        {
            let g = series::ultosc(&x.h, &x.l, &x.c, p1 as f64, p2 as f64, p3 as f64).unwrap();
            w[8] = w[8].max(worst(&g, &ultosc_ref(&x.h, &x.l, &x.c, p1, p2, p3), 1.0));
        }
        for n in [2usize, 3, 14, 64, 200] {
            let g = series::vortex(&x.h, &x.l, &x.c, n as f64).unwrap();
            let r = vortex_ref(&x.h, &x.l, &x.c, n);
            for k in 0..2 {
                w[9] = w[9].max(worst(&g[k], &r[k], 1.0));
            }
        }
    }
    println!(
        "worst relative error (floor = level/1e4 for dpo, ac, elder_ray; 1 for ultosc, vortex): \
         dpo {:.2e} ac {:.2e} elder_ray {:.2e} ultosc {:.2e} vortex {:.2e}",
        w[4], w[5], w[7], w[8], w[9]
    );
    println!(
        "with the floor of 1 (absolute error when the value is below 1): dpo {:.2e} ac {:.2e} \
         elder_ray {:.2e}",
        w_abs[4], w_abs[5], w_abs[7]
    );
}

//! TRIMA, HMA, KAMA, STDDEV and TRUE_RANGE: the fast cores against a textbook reference, and the
//! stream (`next`), the plain-number entry (`next_value` / `next_hlc`), the batch (`vectorize`) and
//! the slice call (`series::*`) against each other, bit for bit.
//!
//! The reference does each value the long way (every window folded from scratch, divisions, the
//! averages of averages one stage at a time). The cores carry running sums and multiply by `1/n`,
//! so the last bits differ; the tolerance is relative 1e-9 of the value (`stddev` at level 1e6 with
//! steps of a hundredth is the case that loses its digits in a careless running variance).

use vike_indicators::indicators::{Hma, Kama, Stddev, Trima, TrueRange};
use vike_indicators::{Indicator, series};
use vike_marketdata::Bar;

use crate::fast::{
    SCALES, WINDOWS, assert_close, closes, same, series_for, spread_bars, true_range_ref, with_nan,
};

// ------------------------------------------------------------------ textbook references

fn mean_of(w: &[f64]) -> f64 {
    w.iter().sum::<f64>() / w.len() as f64
}

/// The weighted mean of a window with weights `1..=len`, the newest heaviest.
fn wma_at(w: &[f64]) -> f64 {
    let n = w.len();
    let top: f64 = w.iter().enumerate().map(|(k, v)| (k + 1) as f64 * v).sum();
    top / (n * (n + 1) / 2) as f64
}

fn trima_ref(c: &[f64], n: usize) -> Vec<f64> {
    let (p1, p2) = (n.div_ceil(2), n / 2 + 1);
    let inner: Vec<f64> = (0..c.len())
        .map(|i| if i + 1 < p1 { f64::NAN } else { mean_of(&c[i + 1 - p1..=i]) })
        .collect();
    // The mean of the last `p2` inner values, defined once `p2` of them exist.
    (0..c.len())
        .map(|i| if i + 1 < p1 - 1 + p2 { f64::NAN } else { mean_of(&inner[i + 1 - p2..=i]) })
        .collect()
}

fn hma_ref(c: &[f64], n: usize) -> Vec<f64> {
    let half = (n / 2).max(2);
    let root = ((n as f64).sqrt() as usize).max(2);
    let combined: Vec<f64> = (0..c.len())
        .map(|i| {
            if i + 1 < n.max(half) {
                f64::NAN
            } else {
                2.0 * wma_at(&c[i + 1 - half..=i]) - wma_at(&c[i + 1 - n..=i])
            }
        })
        .collect();
    (0..c.len())
        .map(|i| {
            if i + 1 < root {
                return f64::NAN;
            }
            let w = &combined[i + 1 - root..=i];
            if w.iter().any(|v| v.is_nan()) { f64::NAN } else { wma_at(w) }
        })
        .collect()
}

fn kama_ref(c: &[f64], n: usize, fast: usize, slow: usize) -> Vec<f64> {
    let (fsc, ssc) = (2.0 / (fast as f64 + 1.0), 2.0 / (slow as f64 + 1.0));
    let mut out = vec![f64::NAN; c.len()];
    if c.len() <= n {
        return out;
    }
    let mut prev = c[n - 1];
    for i in n..c.len() {
        let w = &c[i - n..=i];
        let path: f64 = w.windows(2).map(|p| (p[1] - p[0]).abs()).sum();
        let net = (c[i] - c[i - n]).abs();
        let er = if path == 0.0 { 1.0 } else { net / path };
        let s = er * (fsc - ssc) + ssc;
        prev += s * s * (c[i] - prev);
        out[i] = prev;
    }
    out
}

/// The population standard deviation of a window in EXACT integer arithmetic: every finite `f64` is
/// a whole multiple of the ulp of its smallest member, so the window scales to `i128` without
/// rounding, `n * sum(v^2) - sum(v)^2` is exact, and the one rounding left is the final division.
/// A two-pass fold in `f64` is not a reference at this scale: its mean carries a rounding error of
/// half an ulp of the level, and on a window whose spread is a few thousand ulps (two closes near a
/// million) that error enters the deviation SQUARED, at nine digits.
fn exact_population_sd(w: &[f64]) -> f64 {
    if w.iter().any(|v| !v.is_finite()) {
        return f64::NAN;
    }
    let n = w.len() as i128;
    let e_min = w
        .iter()
        .filter(|v| **v != 0.0)
        .map(|v| ((v.to_bits() >> 52) & 0x7ff) as i32 - 1023)
        .min()
        .unwrap_or(0);
    let k = 52 - e_min;
    let scale = f64::from_bits(((1023 + k) as u64) << 52);
    let v: Vec<i128> = w.iter().map(|x| (x * scale) as i128).collect();
    let s1: i128 = v.iter().sum();
    let s2: i128 = v.iter().map(|x| x * x).sum();
    let num = n * s2 - s1 * s1;
    (num as f64).sqrt() / n as f64 / scale
}

fn stddev_ref(c: &[f64], n: usize) -> Vec<f64> {
    (0..c.len())
        .map(|i| if i + 1 < n { f64::NAN } else { exact_population_sd(&c[i + 1 - n..=i]) })
        .collect()
}

// ------------------------------------------------------------------ comparison helpers

// ------------------------------------------------------------------ against the textbook

#[test]
fn trima_matches_the_mean_of_a_mean() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        for n in WINDOWS {
            let want = trima_ref(&c, n);
            assert_close(
                &format!("trima({n}) level {level}"),
                &series::trima(&c, n as f64).unwrap(),
                &want,
                1e-9,
                1e-300,
            );
            let cn = with_nan(c.clone(), 1500);
            assert_close(
                &format!("trima({n}) NaN level {level}"),
                &series::trima(&cn, n as f64).unwrap(),
                &trima_ref(&cn, n),
                1e-9,
                1e-300,
            );
        }
    }
}

#[test]
fn hma_matches_the_weighted_averages() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        for n in WINDOWS {
            let want = hma_ref(&c, n);
            assert_close(
                &format!("hma({n}) level {level}"),
                &series::hma(&c, n as f64).unwrap(),
                &want,
                1e-9,
                1e-300,
            );
            let cn = with_nan(c.clone(), 1500);
            assert_close(
                &format!("hma({n}) NaN level {level}"),
                &series::hma(&cn, n as f64).unwrap(),
                &hma_ref(&cn, n),
                1e-9,
                1e-300,
            );
        }
    }
}

#[test]
fn kama_matches_kaufmans_recurrence() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        for (n, fast, slow) in
            [(10usize, 2usize, 30usize), (2, 1, 2), (20, 5, 50), (100, 3, 300), (200, 100, 400)]
        {
            let got = series::kama(&c, n as f64, fast as f64, slow as f64).unwrap();
            assert_close(
                &format!("kama({n},{fast},{slow}) level {level}"),
                &got,
                &kama_ref(&c, n, fast, slow),
                1e-9,
                1e-300,
            );
        }
        // A NaN poisons it for good, in the reference as in the core.
        let cn = with_nan(c.clone(), 1500);
        let got = series::kama(&cn, 10.0, 2.0, 30.0).unwrap();
        let want = kama_ref(&cn, 10, 2, 30);
        assert_close(&format!("kama NaN level {level}"), &got, &want, 1e-9, 1e-300);
        assert!(got[2999].is_nan(), "a NaN close poisons kama for good");
    }
}

#[test]
fn stddev_matches_a_window_folded_from_scratch() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for n in [2usize, 3, 5, 20, 64, 200] {
            let got = series::stddev(&c, n as f64).unwrap();
            assert_close(
                &format!("stddev({n}) level {level}"),
                &got,
                &stddev_ref(&c, n),
                1e-9,
                1e-300,
            );
            let cn = with_nan(c.clone(), 2000);
            let got = series::stddev(&cn, n as f64).unwrap();
            assert_close(
                &format!("stddev({n}) NaN level {level}"),
                &got,
                &stddev_ref(&cn, n),
                1e-9,
                1e-300,
            );
        }
    }
}

#[test]
fn stddev_of_a_flat_window_is_exactly_zero_whatever_came_before() {
    for flat in [100.0, 0.1, 123456.789] {
        let mut c = closes(1.0e6, 0.5, 700);
        c.extend(std::iter::repeat_n(flat, 100));
        for n in [2usize, 20, 200] {
            let got = series::stddev(&c, n as f64).unwrap();
            for (i, g) in got.iter().enumerate().skip(700 + n) {
                assert_eq!(*g, 0.0, "stddev({n}) at {i} over a window flat at {flat}");
            }
        }
    }
}

#[test]
fn true_range_is_the_gap_aware_range() {
    for (level, step) in SCALES {
        let c = closes(level, step, 3000);
        let b = spread_bars(&c);
        let (h, l): (Vec<f64>, Vec<f64>) = b.iter().map(|x| (x.high, x.low)).unzip();
        let got = series::true_range(&h, &l, &c);
        let want = true_range_ref(&h, &l, &c);
        assert!(
            same(&got, &want),
            "true_range level {level}: it is the same expression, so the same bits"
        );
    }
}

#[test]
fn the_first_value_is_where_the_registry_says() {
    let c = closes(100.0, 2.0, 600);
    let first = |v: &[f64]| v.iter().position(|x| !x.is_nan());
    for n in [4usize, 5, 20, 64, 400] {
        let nf = n as f64;
        assert_eq!(
            first(&series::trima(&c, nf).unwrap()),
            Some(Trima::new(nf).unwrap().lookback())
        );
        assert_eq!(first(&series::hma(&c, nf).unwrap()), Some(Hma::new(nf).unwrap().lookback()));
        assert_eq!(first(&series::stddev(&c, nf.min(200.0)).unwrap()), Some(n.min(200) - 1));
    }
    for n in [2usize, 10, 64, 200] {
        let nf = n as f64;
        assert_eq!(
            first(&series::kama(&c, nf, 2.0, 30.0).unwrap()),
            Some(Kama::new(nf, 2.0, 30.0).unwrap().lookback())
        );
    }
    assert_eq!(first(&series::true_range(&[1.0, 2.0], &[0.5, 1.5], &[0.8, 1.7])), Some(0));
    assert_eq!(TrueRange::default().lookback(), 0);
}

// ------------------------------------------------------------------ every way in, the same bits

fn lengths(n: usize) -> [usize; 8] {
    [0, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600]
}

#[test]
fn every_way_in_gives_the_same_bits_for_the_averages_and_the_deviation() {
    for n in [2usize, 3, 7, 20, 64, 200, 400] {
        for len in lengths(n) {
            let c = series_for(len);
            let b = spread_bars(&c);
            let nf = n as f64;

            let mut ind = Trima::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Trima::new(nf).unwrap();
            let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
            assert!(same(&stream, &value), "trima n={n} len={len}: next vs next_value");
            assert!(
                same(&stream, &Trima::new(nf).unwrap().vectorize(&b)[0]),
                "trima n={n} len={len}: vectorize"
            );
            assert!(
                same(&stream, &series::trima(&c, nf).unwrap()),
                "trima n={n} len={len}: series"
            );

            let mut ind = Hma::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Hma::new(nf).unwrap();
            let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
            assert!(same(&stream, &value), "hma n={n} len={len}: next vs next_value");
            assert!(
                same(&stream, &Hma::new(nf).unwrap().vectorize(&b)[0]),
                "hma n={n} len={len}: vectorize"
            );
            assert!(same(&stream, &series::hma(&c, nf).unwrap()), "hma n={n} len={len}: series");

            if n <= 200 {
                let mut ind = Stddev::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Stddev::new(nf).unwrap();
                let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
                assert!(same(&stream, &value), "stddev n={n} len={len}: next vs next_value");
                assert!(
                    same(&stream, &Stddev::new(nf).unwrap().vectorize(&b)[0]),
                    "stddev n={n} len={len}: vectorize"
                );
                assert!(
                    same(&stream, &series::stddev(&c, nf).unwrap()),
                    "stddev n={n} len={len}: series"
                );
            }
        }
    }
}

#[test]
fn every_way_in_gives_the_same_bits_for_kama_and_true_range() {
    for (n, fast, slow) in
        [(2usize, 2.0, 30.0), (7, 1.0, 2.0), (20, 5.0, 50.0), (64, 2.0, 30.0), (200, 100.0, 400.0)]
    {
        for len in lengths(n) {
            let c = series_for(len);
            let b = spread_bars(&c);
            let nf = n as f64;
            let mut ind = Kama::new(nf, fast, slow).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Kama::new(nf, fast, slow).unwrap();
            let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
            assert!(same(&stream, &value), "kama n={n} len={len}: next vs next_value");
            assert!(
                same(&stream, &Kama::new(nf, fast, slow).unwrap().vectorize(&b)[0]),
                "kama n={n} len={len}: vectorize"
            );
            assert!(
                same(&stream, &series::kama(&c, nf, fast, slow).unwrap()),
                "kama n={n} len={len}: series"
            );
        }
    }
    for len in [0usize, 1, 2, 20, 21, 2600] {
        let c = series_for(len);
        let b = spread_bars(&c);
        let (h, l): (Vec<f64>, Vec<f64>) = b.iter().map(|x| (x.high, x.low)).unzip();
        let mut ind = TrueRange::default();
        let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
        let mut ind = TrueRange::default();
        let value: Vec<f64> = b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
        assert!(same(&stream, &value), "true_range len={len}: next vs next_hlc");
        assert!(
            same(&stream, &TrueRange::default().vectorize(&b)[0]),
            "true_range len={len}: vectorize"
        );
        assert!(same(&stream, &series::true_range(&h, &l, &c)), "true_range len={len}: series");
    }
}

#[test]
fn reset_returns_to_the_first_bar() {
    let c = closes(100.0, 2.0, 700);
    let b = spread_bars(&c);
    fn run<I: Indicator>(ind: &mut I, b: &[Bar]) -> Vec<f64> {
        b.iter().map(|x| ind.on_bar(x)[0]).collect()
    }
    let mut t = Trima::new(20.0).unwrap();
    let first = run(&mut t, &b);
    t.reset();
    assert!(same(&first, &run(&mut t, &b)), "trima");
    let mut h = Hma::new(20.0).unwrap();
    let first = run(&mut h, &b);
    h.reset();
    assert!(same(&first, &run(&mut h, &b)), "hma");
    let mut k = Kama::default();
    let first = run(&mut k, &b);
    k.reset();
    assert!(same(&first, &run(&mut k, &b)), "kama");
    let mut s = Stddev::default();
    let first = run(&mut s, &b);
    s.reset();
    assert!(same(&first, &run(&mut s, &b)), "stddev");
    let mut r = TrueRange::default();
    let first = run(&mut r, &b);
    r.reset();
    assert!(same(&first, &run(&mut r, &b)), "true_range");
}

/// The numbers the report quotes: the worst relative error against the textbook, per indicator,
/// over every scale, window and NaN position this file uses.
///
/// cargo test -p vike-indicators --release --test fast -- --ignored --nocapture fast_d2::
#[test]
#[ignore = "prints the measured worst errors"]
fn report_worst_errors() {
    let (mut tw, mut hw, mut kw, mut sw) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for n in WINDOWS {
            tw = tw.max(assert_close(
                "trima",
                &series::trima(&c, n as f64).unwrap(),
                &trima_ref(&c, n),
                1.0,
                1e-300,
            ));
            hw = hw.max(assert_close(
                "hma",
                &series::hma(&c, n as f64).unwrap(),
                &hma_ref(&c, n),
                1.0,
                1e-300,
            ));
            if n <= 200 {
                sw = sw.max(assert_close(
                    "stddev",
                    &series::stddev(&c, n as f64).unwrap(),
                    &stddev_ref(&c, n),
                    1.0,
                    1e-300,
                ));
            }
        }
        for (n, fast, slow) in
            [(10usize, 2usize, 30usize), (2, 1, 2), (20, 5, 50), (100, 3, 300), (200, 100, 400)]
        {
            let got = series::kama(&c, n as f64, fast as f64, slow as f64).unwrap();
            kw = kw.max(assert_close("kama", &got, &kama_ref(&c, n, fast, slow), 1.0, 1e-300));
        }
    }
    println!("worst relative error: trima {tw:e}, hma {hw:e}, kama {kw:e}, stddev {sw:e}");
}

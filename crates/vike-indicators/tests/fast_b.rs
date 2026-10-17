//! `dema`, `tema`, `t3`, `trix`, `ppo` and `apo` (the EMA chains of group `b`): the fast cores against
//! a textbook reference, and the stream (`next`), the plain-number entry (`next_value`), the batch
//! (`vectorize`) and the slice call (`series::*`) against each other, bit for bit.
//!
//! The reference does every stage the long way (each EMA a whole-series pass seeded with the mean of
//! its first `n` DEFINED inputs, divisions, no carried state). The cores use the same recurrence in
//! the same order, so the bits are in fact equal today; the tolerance (relative 1e-9) is what is
//! promised, so a future change of summation order stays inside it.

use vike_indicators::indicators::{Apo, Dema, Ppo, T3, Tema, Trix};
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

/// An EMA over a whole series seeded with the mean of the first `n` values.
fn ema_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if n == 0 || c.len() < n {
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

/// An EMA over the DEFINED values of `src` only, written back to their positions.
fn ema_defined_ref(src: &[f64], n: usize) -> Vec<f64> {
    let idx: Vec<usize> = (0..src.len()).filter(|&i| !src[i].is_nan()).collect();
    let vals: Vec<f64> = idx.iter().map(|&i| src[i]).collect();
    let sm = ema_ref(&vals, n);
    let mut out = vec![f64::NAN; src.len()];
    for (k, &i) in idx.iter().enumerate() {
        out[i] = sm[k];
    }
    out
}

fn zip2(a: &[f64], b: &[f64], f: impl Fn(f64, f64) -> f64) -> Vec<f64> {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| if x.is_nan() || y.is_nan() { f64::NAN } else { f(x, y) })
        .collect()
}

fn dema_ref(c: &[f64], n: usize) -> Vec<f64> {
    let e1 = ema_ref(c, n);
    let e2 = ema_defined_ref(&e1, n);
    zip2(&e1, &e2, |a, b| 2.0 * a - b)
}

fn tema_ref(c: &[f64], n: usize) -> Vec<f64> {
    let e1 = ema_ref(c, n);
    let e2 = ema_defined_ref(&e1, n);
    let e3 = ema_defined_ref(&e2, n);
    (0..c.len())
        .map(|i| {
            if e1[i].is_nan() || e2[i].is_nan() || e3[i].is_nan() {
                f64::NAN
            } else {
                3.0 * e1[i] - 3.0 * e2[i] + e3[i]
            }
        })
        .collect()
}

fn trix_ref(c: &[f64], n: usize) -> Vec<f64> {
    let e1 = ema_ref(c, n);
    let e2 = ema_defined_ref(&e1, n);
    let e3 = ema_defined_ref(&e2, n);
    let mut out = vec![f64::NAN; c.len()];
    let mut prev: Option<f64> = None;
    for i in 0..c.len() {
        if e3[i].is_nan() {
            continue;
        }
        if let Some(p) = prev
            && p != 0.0
        {
            out[i] = (e3[i] - p) / p * 100.0;
        }
        prev = Some(e3[i]);
    }
    out
}

/// One generalized-DEMA pass over the defined values of `x`.
fn gd_ref(x: &[f64], n: usize, v: f64) -> Vec<f64> {
    let e1 = ema_defined_ref(x, n);
    let e2 = ema_defined_ref(&e1, n);
    zip2(&e1, &e2, |a, b| (1.0 + v) * a - v * b)
}

fn t3_ref(c: &[f64], n: usize, v: f64) -> Vec<f64> {
    gd_ref(&gd_ref(&gd_ref(c, n, v), n, v), n, v)
}

fn ppo_ref(c: &[f64], f: usize, s: usize) -> Vec<f64> {
    let (ef, es) = (ema_ref(c, f), ema_ref(c, s));
    (0..c.len())
        .map(|i| {
            if ef[i].is_nan() || es[i].is_nan() || es[i] == 0.0 {
                f64::NAN
            } else {
                (ef[i] - es[i]) / es[i] * 100.0
            }
        })
        .collect()
}

fn apo_ref(c: &[f64], f: usize, s: usize) -> Vec<f64> {
    zip2(&ema_ref(c, f), &ema_ref(c, s), |a, b| a - b)
}

fn assert_close(name: &str, got: &[f64], want: &[f64], rel: f64) {
    assert_close_floor(name, got, want, rel, 0.0);
}

/// [`assert_close`] with an absolute floor: a difference is also accepted when it is within `floor`.
/// Only for a value that is a DIFFERENCE of two averages of the price level (the absolute price
/// oscillator): its error is that of the averages, `eps * level / alpha`, however small the
/// difference is, so a relative bound on the difference alone cannot hold at a high level.
fn assert_close_floor(name: &str, got: &[f64], want: &[f64], rel: f64, floor: f64) {
    assert_eq!(got.len(), want.len(), "{name}: length");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if w.is_nan() {
            assert!(g.is_nan(), "{name}[{i}]: want NaN, got {g}");
        } else {
            let diff = (g - w).abs();
            let err = diff / w.abs().max(1.0);
            assert!(
                err <= rel || diff <= floor,
                "{name}[{i}]: got {g}, want {w}, relative error {err:e}, absolute {diff:e}"
            );
        }
    }
}

fn same(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];
const REL: f64 = 1e-9;

#[test]
fn dema_tema_t3_match_the_textbook_chains() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for n in [2usize, 3, 12, 20, 200, 400] {
            let nf = n as f64;
            assert_close(
                &format!("dema({n}) level {level}"),
                &series::dema(&c, nf).unwrap(),
                &dema_ref(&c, n),
                REL,
            );
            assert_close(
                &format!("tema({n}) level {level}"),
                &series::tema(&c, nf).unwrap(),
                &tema_ref(&c, n),
                REL,
            );
            for v in [0.0, 0.7, 1.0] {
                assert_close(
                    &format!("t3({n},{v}) level {level}"),
                    &series::t3(&c, nf, v).unwrap(),
                    &t3_ref(&c, n, v),
                    REL,
                );
            }
        }
    }
}

#[test]
fn trix_matches_the_textbook_rate_of_change_of_a_triple_ema() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for n in [2usize, 3, 18, 60, 200] {
            assert_close(
                &format!("trix({n}) level {level}"),
                &series::trix(&c, n as f64).unwrap(),
                &trix_ref(&c, n),
                REL,
            );
        }
    }
}

#[test]
fn ppo_and_apo_match_the_textbook_oscillators() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for (f, s) in [(12usize, 26usize), (2, 2), (2, 3), (9, 4), (50, 120), (200, 400), (3, 200)]
        {
            let (ff, sf) = (f as f64, s as f64);
            assert_close(
                &format!("ppo({f},{s}) level {level}"),
                &series::ppo(&c, ff, sf).unwrap(),
                &ppo_ref(&c, f, s),
                REL,
            );
            // The slowest average carries the largest error: `8 * eps * level / alpha`.
            let floor = 8.0 * f64::EPSILON * level * (f.max(s) as f64 + 1.0) / 2.0;
            assert_close_floor(
                &format!("apo({f},{s}) level {level}"),
                &series::apo(&c, ff, sf).unwrap(),
                &apo_ref(&c, f, s),
                REL,
                floor,
            );
        }
    }
}

/// A slow EMA that is exactly zero gives a `NaN` ppo (and a zero previous TRIX value gives a `NaN`
/// trix), the way the stepper always did; a flat series at zero is the case.
#[test]
fn a_zero_denominator_is_nan_not_infinity() {
    let c = vec![0.0; 200];
    for v in series::ppo(&c, 3.0, 5.0).unwrap() {
        assert!(v.is_nan());
    }
    for v in series::trix(&c, 3.0).unwrap() {
        assert!(v.is_nan());
    }
    // Zero, then a move: the first moved value has a zero previous TRIX value, the next does not.
    let mut c = vec![0.0; 40];
    c.extend((0..40).map(|i| 1.0 + i as f64));
    let t = series::trix(&c, 3.0).unwrap();
    assert_eq!(t.len(), c.len());
    assert!(t[10..=40].iter().all(|v| v.is_nan()), "zero previous value must give NaN");
    assert!(t[41..].iter().all(|v| v.is_finite()));
}

/// A `NaN` close poisons `dema`, `tema`, `trix`, `ppo` and `apo` for good (their first average is
/// fed the raw close) and is DROPPED by `t3` (its first average is fed the defined closes only).
#[test]
fn a_nan_close_poisons_all_but_t3_which_skips_it() {
    let mut c = closes(100.0, 2.0, 800);
    let k = 300;
    c[k] = f64::NAN;
    for out in [
        series::dema(&c, 10.0).unwrap(),
        series::tema(&c, 10.0).unwrap(),
        series::trix(&c, 10.0).unwrap(),
        series::ppo(&c, 5.0, 10.0).unwrap(),
        series::apo(&c, 5.0, 10.0).unwrap(),
    ] {
        assert!(out[..k].iter().skip(60).all(|v| v.is_finite()));
        assert!(out[k..].iter().all(|v| v.is_nan()), "poisoned for good");
    }
    let with = series::t3(&c, 5.0, 0.7).unwrap();
    let mut gone = c.clone();
    gone.remove(k);
    let without = series::t3(&gone, 5.0, 0.7).unwrap();
    assert!(with[k].is_nan());
    for i in 0..k {
        assert_eq!(with[i].to_bits(), without[i].to_bits(), "t3[{i}] before the NaN");
    }
    for i in k + 1..c.len() {
        assert_eq!(with[i].to_bits(), without[i - 1].to_bits(), "t3[{i}] after the dropped NaN");
    }
}

#[test]
fn every_way_in_gives_the_same_bits() {
    for n in [2usize, 3, 7, 20, 64, 200, 400] {
        for len in [0usize, 1, n - 1, n, n + 1, 3 * n, 3 * n + 1, 6 * n + 1, 2600] {
            let mut c = closes(100.0, 2.0, len);
            if len > 40 {
                c[len / 3] = f64::NAN;
            }
            let b = bars_of(&c);
            let nf = n as f64;

            macro_rules! ways {
                ($name:literal, $T:ident, $new:expr, $series:expr) => {{
                    let mut ind = $T::new($new).unwrap();
                    let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                    let mut ind = $T::new($new).unwrap();
                    let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
                    assert!(same(&stream, &value), "{} n={n} len={len}: next vs next_value", $name);
                    let batch = $T::new($new).unwrap().vectorize(&b);
                    assert!(same(&stream, &batch[0]), "{} n={n} len={len}: vectorize", $name);
                    assert!(same(&stream, &$series.unwrap()), "{} n={n} len={len}: series", $name);
                }};
            }
            ways!("dema", Dema, nf, series::dema(&c, nf));
            ways!("tema", Tema, nf, series::tema(&c, nf));
            if n <= 200 {
                ways!("trix", Trix, nf, series::trix(&c, nf));
            }
            // `T3::new` takes two numbers, so it is spelled out.
            let mut ind = T3::new(nf, 0.7).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = T3::new(nf, 0.7).unwrap();
            let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
            assert!(same(&stream, &value), "t3 n={n} len={len}: next vs next_value");
            assert!(same(&stream, &T3::new(nf, 0.7).unwrap().vectorize(&b)[0]), "t3 vectorize");
            assert!(same(&stream, &series::t3(&c, nf, 0.7).unwrap()), "t3 series");
        }
    }
    for (f, s) in [(12.0, 26.0), (2.0, 2.0), (2.0, 3.0), (9.0, 4.0), (50.0, 120.0), (200.0, 400.0)]
    {
        for len in [0usize, 1, 10, 100, 401, 2600] {
            let mut c = closes(100.0, 2.0, len);
            if len > 40 {
                c[len / 3] = f64::NAN;
            }
            let b = bars_of(&c);
            let mut ind = Ppo::new(f, s).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Ppo::new(f, s).unwrap();
            let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
            assert!(same(&stream, &value), "ppo({f},{s}) len={len}: next vs next_value");
            assert!(same(&stream, &Ppo::new(f, s).unwrap().vectorize(&b)[0]), "ppo vectorize");
            assert!(same(&stream, &series::ppo(&c, f, s).unwrap()), "ppo series");

            let mut ind = Apo::new(f, s).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Apo::new(f, s).unwrap();
            let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
            assert!(same(&stream, &value), "apo({f},{s}) len={len}: next vs next_value");
            assert!(same(&stream, &Apo::new(f, s).unwrap().vectorize(&b)[0]), "apo vectorize");
            assert!(same(&stream, &series::apo(&c, f, s).unwrap()), "apo series");
        }
    }
}

#[test]
fn a_reset_stream_replays_the_same_bits() {
    let c = closes(100.0, 2.0, 700);
    let b = bars_of(&c);
    let mut t = T3::new(9.0, 0.7).unwrap();
    let first: Vec<f64> = b.iter().map(|x| t.next(x)).collect();
    t.reset();
    let again: Vec<f64> = b.iter().map(|x| t.next(x)).collect();
    assert!(same(&first, &again), "t3");
    let mut t = Trix::new(9.0).unwrap();
    let first: Vec<f64> = b.iter().map(|x| t.next(x)).collect();
    t.reset();
    let again: Vec<f64> = b.iter().map(|x| t.next(x)).collect();
    assert!(same(&first, &again), "trix");
}

#[test]
fn the_slice_calls_refuse_what_the_registry_refuses() {
    assert!(series::dema(&[1.0; 10], 1.0).is_err());
    assert!(series::tema(&[1.0; 10], 401.0).is_err());
    assert!(series::trix(&[1.0; 10], f64::NAN).is_err());
    assert!(series::t3(&[1.0; 10], 5.0, 1.5).is_err());
    assert!(series::ppo(&[1.0; 10], 201.0, 5.0).is_err());
    assert!(series::apo(&[1.0; 10], 5.0, 1.0).is_err());
}

/// Prints the largest error against the textbook reference, per indicator, over the three scales and
/// the periods above (`--ignored --nocapture`); nothing is asserted here, the tests above hold the bound.
#[test]
#[ignore = "prints a measurement"]
fn report_the_largest_error_against_the_textbook() {
    fn worst(got: &[f64], want: &[f64]) -> f64 {
        got.iter()
            .zip(want)
            .filter(|(_, w)| !w.is_nan())
            .map(|(g, w)| (g - w).abs() / w.abs().max(1.0))
            .fold(0.0, f64::max)
    }
    let mut w = [0.0_f64; 7];
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for n in [2usize, 3, 12, 20, 200, 400] {
            let nf = n as f64;
            w[0] = w[0].max(worst(&series::dema(&c, nf).unwrap(), &dema_ref(&c, n)));
            w[1] = w[1].max(worst(&series::tema(&c, nf).unwrap(), &tema_ref(&c, n)));
            w[2] = w[2].max(worst(&series::t3(&c, nf, 0.7).unwrap(), &t3_ref(&c, n, 0.7)));
            if n <= 200 {
                w[3] = w[3].max(worst(&series::trix(&c, nf).unwrap(), &trix_ref(&c, n)));
            }
        }
        for (f, s) in [(12usize, 26usize), (2, 2), (2, 3), (9, 4), (50, 120), (200, 400), (3, 200)]
        {
            let (ff, sf) = (f as f64, s as f64);
            w[4] = w[4].max(worst(&series::ppo(&c, ff, sf).unwrap(), &ppo_ref(&c, f, s)));
            // The absolute price oscillator is a difference of two level-sized averages: report it
            // against the level as well.
            let a = worst(&series::apo(&c, ff, sf).unwrap(), &apo_ref(&c, f, s));
            w[5] = w[5].max(a);
            w[6] = w[6].max(a / level);
        }
    }
    println!(
        "max error vs textbook, relative to max(|want|, 1): dema {:.1e} tema {:.1e} t3 {:.1e} trix {:.1e} ppo {:.1e} apo {:.1e} (apo relative to the price level {:.1e})",
        w[0], w[1], w[2], w[3], w[4], w[5], w[6]
    );
}

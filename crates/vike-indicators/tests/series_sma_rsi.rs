//! `series::sma` / `series::rsi` and the `Sma` / `Rsi` steppers against an exact reference.
//!
//! The reference folds each window from scratch (SMA) and runs Wilder's recurrence with divisions
//! (RSI), the textbook form. The fast versions carry a running sum and multiply by `1/n`, so the
//! last bits differ; the tolerance is relative 1e-12, on a series with a high level and small steps
//! (where a careless running sum loses digits) and on one with NaN gaps.

use vike_indicators::Indicator;
use vike_indicators::indicators::{Rsi, Sma};
use vike_indicators::series;
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

fn sma_ref(c: &[f64], n: usize) -> Vec<f64> {
    (0..c.len())
        .map(|i| if i + 1 < n { f64::NAN } else { c[i + 1 - n..=i].iter().sum::<f64>() / n as f64 })
        .collect()
}

fn rsi_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if c.len() <= n {
        return out;
    }
    let (mut ag, mut al) = (0.0, 0.0);
    for i in 1..=n {
        let d = c[i] - c[i - 1];
        ag += d.max(0.0);
        al += (-d).max(0.0);
    }
    ag /= n as f64;
    al /= n as f64;
    let f = |ag: f64, al: f64| if al == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + ag / al) };
    out[n] = f(ag, al);
    for i in n + 1..c.len() {
        let d = c[i] - c[i - 1];
        ag = (ag * (n as f64 - 1.0) + d.max(0.0)) / n as f64;
        al = (al * (n as f64 - 1.0) + (-d).max(0.0)) / n as f64;
        out[i] = f(ag, al);
    }
    out
}

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

fn bars(c: &[f64]) -> Vec<Bar> {
    c.iter().enumerate().map(|(i, &x)| Bar::ohlcv(i as i64 * 60_000, x, x, x, x, 1.0)).collect()
}

#[test]
fn sma_matches_a_fresh_fold_at_every_scale_of_price() {
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)] {
        let c = closes(level, step, 6000);
        for n in [1usize, 2, 3, 20, 200, 1000] {
            let got = series::sma(&c, n as f64).unwrap();
            assert_close(&format!("sma({n}) level {level}"), &got, &sma_ref(&c, n), 1e-12);
        }
    }
}

#[test]
fn rsi_matches_wilders_textbook_recurrence() {
    for (level, step) in [(100.0, 2.0), (1.0e6, 0.01)] {
        let c = closes(level, step, 6000);
        for n in [1usize, 2, 14, 100] {
            let got = series::rsi(&c, n as f64).unwrap();
            assert_close(&format!("rsi({n}) level {level}"), &got, &rsi_ref(&c, n), 1e-9);
        }
    }
}

#[test]
fn a_nan_poisons_the_window_it_is_in_and_the_mean_recovers_after_it_leaves() {
    let mut c = closes(100.0, 2.0, 200);
    c[50] = f64::NAN;
    let got = series::sma(&c, 20.0).unwrap();
    for (i, g) in got.iter().enumerate().take(120).skip(19) {
        if (50..70).contains(&i) {
            assert!(g.is_nan(), "sma[{i}] should be NaN while the NaN is in the window");
        } else {
            assert!(g.is_finite(), "sma[{i}] should be finite once the NaN left");
        }
    }
}

#[test]
fn the_stepper_the_batch_and_the_slice_call_agree_bit_for_bit() {
    let c = closes(100.0, 2.0, 3000);
    let b = bars(&c);
    let mut sma = Sma::new(20.0).unwrap();
    let mut rsi = Rsi::new(14.0).unwrap();
    let (s_stream, r_stream): (Vec<f64>, Vec<f64>) =
        b.iter().map(|x| (sma.next(x), rsi.next(x))).unzip();
    let same = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits());
    assert!(same(&s_stream, &Sma::new(20.0).unwrap().vectorize(&b)[0]));
    assert!(same(&r_stream, &Rsi::new(14.0).unwrap().vectorize(&b)[0]));
    assert!(same(&s_stream, &series::sma(&c, 20.0).unwrap()));
    assert!(same(&r_stream, &series::rsi(&c, 14.0).unwrap()));
}

#[test]
fn a_length_outside_the_range_is_a_param_error() {
    assert!(series::sma(&[1.0], 0.0).is_err());
    assert!(series::rsi(&[1.0], f64::NAN).is_err());
}

#[test]
fn stream_and_slice_agree_bit_for_bit_for_every_window_and_every_tail() {
    let same = |a: &[f64], b: &[f64]| {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
    };
    // Windows below, at and above the rebuild interval; lengths that stop mid-lap, exactly on a lap,
    // shorter than the window, and a NaN and an infinity travelling through the window.
    for n in [1usize, 2, 3, 7, 20, 64, 511, 512, 513, 1000] {
        for len in [0usize, 1, n.saturating_sub(1), n, n + 1, 3 * n - 1, 3 * n, 3 * n + 1, 2600] {
            let mut c = closes(100.0, 2.0, len);
            if len > 40 {
                c[len / 3] = f64::NAN;
                c[len / 2] = f64::INFINITY;
            }
            let b = bars(&c);
            let mut sma = Sma::new(n as f64).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| sma.next(x)).collect();
            let slice = series::sma(&c, n as f64).unwrap();
            let batch = Sma::new(n as f64).unwrap().vectorize(&b).remove(0);
            assert!(same(&stream, &slice), "sma n={n} len={len}: stream vs slice");
            assert!(same(&stream, &batch), "sma n={n} len={len}: stream vs vectorize");

            let m = n.max(2) as f64; // the registry's RSI range starts at 2
            let mut rsi = Rsi::new(m).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| rsi.next(x)).collect();
            let slice = series::rsi(&c, m).unwrap();
            let batch = Rsi::new(m).unwrap().vectorize(&b).remove(0);
            assert!(same(&stream, &slice), "rsi n={n} len={len}: stream vs slice");
            assert!(same(&stream, &batch), "rsi n={n} len={len}: stream vs vectorize");
        }
    }
}

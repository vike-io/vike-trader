//! EMA, MACD, Bollinger and ATR: the fast cores against a textbook reference, and the stream, the
//! batch (`vectorize`), the slice call (`series::*`) and the plain-number entry (`next_value`,
//! `next_hlc`) against each other, bit for bit.
//!
//! The reference does each step the long way (a window folded from scratch, divisions, the SMA
//! seed). The cores carry running state and multiply by `1/n`, so the last bits differ; the
//! tolerance is relative 1e-9 on a high-level, small-step series (where a careless running
//! variance loses its digits) and on an ordinary one.

use vike_indicators::indicators::{Atr, Bollinger, Ema, Macd};
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

/// A bar around each close with a spread that varies, so the true range is not just `high - low`.
fn bars_of(c: &[f64]) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let up = 0.3 + (i % 7) as f64 * 0.1;
            let dn = 0.2 + (i % 5) as f64 * 0.15;
            Bar::ohlcv(i as i64 * 60_000, x, x + up, x - dn, x, 1.0)
        })
        .collect()
}

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

fn macd_ref(c: &[f64], f: usize, s: usize, g: usize) -> [Vec<f64>; 3] {
    let (ef, es) = (ema_ref(c, f), ema_ref(c, s));
    let line: Vec<f64> = ef.iter().zip(&es).map(|(a, b)| a - b).collect();
    let start = line.iter().position(|v| !v.is_nan());
    let mut sig = vec![f64::NAN; c.len()];
    if let Some(st) = start {
        let tail = ema_ref(&line[st..], g);
        sig[st..].copy_from_slice(&tail);
    }
    let hist: Vec<f64> = line.iter().zip(&sig).map(|(a, b)| a - b).collect();
    [line, sig, hist]
}

fn bollinger_ref(c: &[f64], n: usize, m: f64) -> [Vec<f64>; 3] {
    let mut out = [vec![f64::NAN; c.len()], vec![f64::NAN; c.len()], vec![f64::NAN; c.len()]];
    for i in 0..c.len() {
        if i + 1 < n {
            continue;
        }
        let w = &c[i + 1 - n..=i];
        let mean = w.iter().sum::<f64>() / n as f64;
        let var = w.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n as f64;
        let sd = var.sqrt();
        out[0][i] = mean + m * sd;
        out[1][i] = mean;
        out[2][i] = mean - m * sd;
    }
    out
}

fn atr_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if n == 0 || c.len() < n {
        return out;
    }
    let tr: Vec<f64> = (0..c.len())
        .map(|i| {
            if i == 0 {
                h[0] - l[0]
            } else {
                (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs())
            }
        })
        .collect();
    let mut prev = tr[..n].iter().sum::<f64>() / n as f64;
    out[n - 1] = prev;
    for i in n..c.len() {
        prev = (prev * (n as f64 - 1.0) + tr[i]) / n as f64;
        out[i] = prev;
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

fn same(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];

#[test]
fn ema_matches_the_textbook_recurrence() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for n in [1usize, 2, 12, 20, 200] {
            assert_close(
                &format!("ema({n}) level {level}"),
                &series::ema(&c, n as f64).unwrap(),
                &ema_ref(&c, n),
                1e-12,
            );
        }
    }
}

#[test]
fn macd_matches_the_textbook_lines() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for (f, s, g) in [(12usize, 26usize, 9usize), (3, 7, 2), (5, 5, 5)] {
            let got = series::macd(&c, f as f64, s as f64, g as f64).unwrap();
            let want = macd_ref(&c, f, s, g);
            for k in 0..3 {
                assert_close(
                    &format!("macd({f},{s},{g}) line {k} level {level}"),
                    &got[k],
                    &want[k],
                    1e-9,
                );
            }
        }
    }
}

#[test]
fn bollinger_matches_a_window_folded_from_scratch() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for (n, m) in [(2usize, 2.0), (20, 2.0), (20, 0.5), (200, 3.0)] {
            let got = series::bollinger(&c, n as f64, m).unwrap();
            let want = bollinger_ref(&c, n, m);
            for k in 0..3 {
                assert_close(
                    &format!("bollinger({n},{m}) band {k} level {level}"),
                    &got[k],
                    &want[k],
                    1e-9,
                );
            }
        }
    }
}

#[test]
fn atr_matches_wilders_average_of_the_true_range() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        let b = bars_of(&c);
        let (h, l): (Vec<f64>, Vec<f64>) = b.iter().map(|x| (x.high, x.low)).unzip();
        for n in [1usize, 2, 14, 100] {
            let got = series::atr(&h, &l, &c, n as f64).unwrap();
            assert_close(&format!("atr({n}) level {level}"), &got, &atr_ref(&h, &l, &c, n), 1e-9);
        }
    }
}

#[test]
fn every_way_in_gives_the_same_bits() {
    for n in [1usize, 2, 7, 20, 64, 511, 512, 513] {
        for len in [0usize, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600] {
            let mut c = closes(100.0, 2.0, len);
            if len > 40 {
                c[len / 3] = f64::NAN;
            }
            let b = bars_of(&c);
            let (h, l): (Vec<f64>, Vec<f64>) = b.iter().map(|x| (x.high, x.low)).unzip();
            let nf = n as f64;

            // EMA
            let mut ind = Ema::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Ema::new(nf).unwrap();
            let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
            assert!(same(&stream, &value), "ema n={n} len={len}: next vs next_value");
            assert!(same(&stream, &Ema::new(nf).unwrap().vectorize(&b)[0]), "ema vectorize");
            assert!(same(&stream, &series::ema(&c, nf).unwrap()), "ema series");

            // Bollinger (the registry range is 1..=1000 for the length)
            let mut ind = Bollinger::new(nf, 2.0).unwrap();
            let stream: Vec<[f64; 3]> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Bollinger::new(nf, 2.0).unwrap();
            let value: Vec<[f64; 3]> = c.iter().map(|&x| ind.next_value(x)).collect();
            let batch = Bollinger::new(nf, 2.0).unwrap().vectorize(&b);
            let sl = series::bollinger(&c, nf, 2.0).unwrap();
            for k in 0..3 {
                let s: Vec<f64> = stream.iter().map(|v| v[k]).collect();
                let v: Vec<f64> = value.iter().map(|v| v[k]).collect();
                assert!(same(&s, &v), "bollinger n={n} len={len} band {k}: next vs next_value");
                assert!(same(&s, &batch[k]), "bollinger n={n} len={len} band {k}: vectorize");
                assert!(same(&s, &sl[k]), "bollinger n={n} len={len} band {k}: series");
            }

            // ATR
            let mut ind = Atr::new(nf).unwrap();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Atr::new(nf).unwrap();
            let value: Vec<f64> = b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
            assert!(same(&stream, &value), "atr n={n} len={len}: next vs next_hlc");
            assert!(same(&stream, &Atr::new(nf).unwrap().vectorize(&b)[0]), "atr vectorize");
            assert!(same(&stream, &series::atr(&h, &l, &c, nf).unwrap()), "atr series");
        }
    }
    // MACD, with periods the registry accepts
    for (f, s, g) in [(12.0, 26.0, 9.0), (3.0, 7.0, 2.0), (1.0, 1.0, 1.0), (30.0, 5.0, 40.0)] {
        for len in [0usize, 1, 10, 100, 2600] {
            let mut c = closes(100.0, 2.0, len);
            if len > 40 {
                c[len / 3] = f64::NAN;
            }
            let b = bars_of(&c);
            let mut ind = Macd::new(f, s, g).unwrap();
            let stream: Vec<[f64; 3]> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Macd::new(f, s, g).unwrap();
            let value: Vec<[f64; 3]> = c.iter().map(|&x| ind.next_value(x)).collect();
            let batch = Macd::new(f, s, g).unwrap().vectorize(&b);
            let sl = series::macd(&c, f, s, g).unwrap();
            for k in 0..3 {
                let sk: Vec<f64> = stream.iter().map(|v| v[k]).collect();
                let vk: Vec<f64> = value.iter().map(|v| v[k]).collect();
                assert!(same(&sk, &vk), "macd({f},{s},{g}) len={len} line {k}: next vs next_value");
                assert!(same(&sk, &batch[k]), "macd({f},{s},{g}) len={len} line {k}: vectorize");
                assert!(same(&sk, &sl[k]), "macd({f},{s},{g}) len={len} line {k}: series");
            }
        }
    }
}

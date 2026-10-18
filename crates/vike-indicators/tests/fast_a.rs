//! WMA, CCI, Williams %R, the slow stochastic, the awesome oscillator, Donchian and Keltner: the fast
//! cores against a textbook reference, and the stream, the batch (`vectorize`), the slice call
//! (`series::*`) and the plain-number entry (`next_value`, `next_hl`, `next_hlc`) against each
//! other, bit for bit.
//!
//! The reference does each step the long way (a window folded from scratch, divisions). The cores
//! carry running state and multiply by `1/n`, so the last bits differ; the tolerance is relative
//! 1e-9 on three series: an ordinary one, a high-level small-step one (where a running sum of raw
//! prices loses every digit) and a tiny-level one.
//!
//! The CCI and awesome references are the textbook formulas on the window translated to its start (a
//! translation changes no deviation and no difference of means): a plain mean of values near a
//! million is only good to ~1e-10 in absolute terms, which is the size of the awesome oscillator and
//! of a CCI deviation on the small-step series, so a plain reference would measure ITS OWN rounding,
//! not the core's.

use vike_indicators::indicators::{Awesome, Cci, Donchian, Keltner, Stochastic, Williams, Wma};
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

/// A bar around each close with a spread that varies, so the true range is not just `high - low`
/// and the close is not always the extreme.
fn bars_of(c: &[f64]) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let up = 0.3 + (i % 7) as f64 * 0.1;
            let dn = 0.2 + (i % 5) as f64 * 0.15;
            let close = x + (i % 3) as f64 * 0.1 - 0.1;
            Bar::ohlcv(i as i64 * 60_000, x, x + up, x - dn, close, 1.0)
        })
        .collect()
}

/// The bars scaled to the level and step of a series, so the spread of a bar is as small (or as large)
/// as the moves between bars.
fn bars_at(level: f64, step: f64, n: usize) -> Vec<Bar> {
    let k = step / 2.0;
    closes(level, step, n)
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            let up = (0.3 + (i % 7) as f64 * 0.1) * k;
            let dn = (0.2 + (i % 5) as f64 * 0.15) * k;
            let close = x + ((i % 3) as f64 * 0.1 - 0.1) * k;
            Bar::ohlcv(i as i64 * 60_000, x, x + up, x - dn, close, 1.0)
        })
        .collect()
}

fn hlc(b: &[Bar]) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    (
        b.iter().map(|x| x.high).collect(),
        b.iter().map(|x| x.low).collect(),
        b.iter().map(|x| x.close).collect(),
    )
}

/// `got` against `want` to relative `rel`, with `floor` the smallest magnitude a value is measured
/// against (an oscillator that crosses zero has no meaningful relative error near it). Returns the
/// largest error seen.
fn assert_close(name: &str, got: &[f64], want: &[f64], rel: f64, floor: f64) -> f64 {
    assert_eq!(got.len(), want.len(), "{name}: length");
    let mut worst = 0.0_f64;
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if w.is_nan() {
            assert!(g.is_nan(), "{name}[{i}]: want NaN, got {g}");
        } else {
            let err = (g - w).abs() / w.abs().max(floor);
            assert!(err <= rel, "{name}[{i}]: got {g}, want {w}, relative error {err:e}");
            worst = worst.max(err);
        }
    }
    worst
}

fn same(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];

// ------------------------------------------------------------------------------------------------
// Textbook references
// ------------------------------------------------------------------------------------------------

fn wma_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    let denom = (n * (n + 1) / 2) as f64;
    for i in (n - 1)..c.len() {
        let w = &c[i + 1 - n..=i];
        let acc: f64 = w.iter().enumerate().map(|(k, v)| (k + 1) as f64 * v).sum();
        out[i] = acc / denom;
    }
    out
}

fn cci_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<f64> {
    let tp: Vec<f64> = (0..c.len()).map(|i| (h[i] + l[i] + c[i]) / 3.0).collect();
    let mut out = vec![f64::NAN; c.len()];
    for i in (n - 1)..c.len() {
        // The textbook formula on the window translated by its first value (translation changes no
        // deviation): the mean is then a small number, not one rounded to the price grid.
        let w: Vec<f64> = tp[i + 1 - n..=i].iter().map(|v| v - tp[i + 1 - n]).collect();
        let mean = w.iter().sum::<f64>() / n as f64;
        let md = w.iter().map(|v| (v - mean).abs()).sum::<f64>() / n as f64;
        out[i] = if md > 0.0 { (w[n - 1] - mean) / (0.015 * md) } else { 0.0 };
    }
    out
}

fn extremes(h: &[f64], l: &[f64], i: usize, n: usize) -> (f64, f64) {
    let hh = h[i + 1 - n..=i].iter().cloned().fold(f64::MIN, f64::max);
    let ll = l[i + 1 - n..=i].iter().cloned().fold(f64::MAX, f64::min);
    (hh, ll)
}

fn williams_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    for i in (n - 1)..c.len() {
        let (hh, ll) = extremes(h, l, i, n);
        let rng = hh - ll;
        out[i] = if rng > 0.0 { -100.0 * (hh - c[i]) / rng } else { 0.0 };
    }
    out
}

fn sma_nan(x: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    for i in (n - 1)..x.len() {
        let w = &x[i + 1 - n..=i];
        if w.iter().all(|v| !v.is_nan()) {
            out[i] = w.iter().sum::<f64>() / n as f64;
        }
    }
    out
}

fn stochastic_ref(
    h: &[f64],
    l: &[f64],
    c: &[f64],
    n: usize,
    sk: usize,
    sd: usize,
) -> [Vec<f64>; 2] {
    let mut raw = vec![f64::NAN; c.len()];
    for i in (n - 1)..c.len() {
        let (hh, ll) = extremes(h, l, i, n);
        let rng = hh - ll;
        raw[i] = if rng > 0.0 { 100.0 * (c[i] - ll) / rng } else { 0.0 };
    }
    let k = sma_nan(&raw, sk);
    let d = sma_nan(&k, sd);
    [k, d]
}

fn awesome_ref(h: &[f64], l: &[f64], fast: usize, slow: usize) -> Vec<f64> {
    let mp: Vec<f64> = h.iter().zip(l).map(|(a, b)| (a + b) / 2.0).collect();
    // Both means taken about ONE point (the start of the longer window): their difference is the
    // difference of two small numbers, not of two values rounded to the price grid.
    (0..mp.len())
        .map(|i| {
            let longest = fast.max(slow);
            if i + 1 < longest {
                return f64::NAN;
            }
            let c = mp[i + 1 - longest];
            let mean = |n: usize| mp[i + 1 - n..=i].iter().map(|v| v - c).sum::<f64>() / n as f64;
            mean(fast) - mean(slow)
        })
        .collect()
}

fn donchian_ref(h: &[f64], l: &[f64], n: usize) -> [Vec<f64>; 3] {
    let mut out = [vec![f64::NAN; h.len()], vec![f64::NAN; h.len()], vec![f64::NAN; h.len()]];
    let [up, mid, lo] = &mut out;
    for (i, ((u, m), d)) in
        up.iter_mut().zip(mid.iter_mut()).zip(lo.iter_mut()).enumerate().skip(n - 1)
    {
        let (hh, ll) = extremes(h, l, i, n);
        *u = hh;
        *m = (hh + ll) / 2.0;
        *d = ll;
    }
    out
}

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

fn atr_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if c.len() < n {
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

fn keltner_ref(h: &[f64], l: &[f64], c: &[f64], en: usize, an: usize, m: f64) -> [Vec<f64>; 3] {
    let (mid, atr) = (ema_ref(c, en), atr_ref(h, l, c, an));
    let up = mid.iter().zip(&atr).map(|(e, a)| e + m * a).collect();
    let lo = mid.iter().zip(&atr).map(|(e, a)| e - m * a).collect();
    [up, mid, lo]
}

// ------------------------------------------------------------------------------------------------
// Against the textbook
// ------------------------------------------------------------------------------------------------

#[test]
fn wma_matches_the_textbook_weighted_mean() {
    let mut worst = 0.0_f64;
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for n in [1usize, 2, 5, 20, 200, 700] {
            let got = series::wma(&c, n as f64).unwrap();
            worst = worst.max(assert_close(
                &format!("wma({n}) level {level}"),
                &got,
                &wma_ref(&c, n),
                1e-9,
                0.0,
            ));
        }
    }
    println!("wma: worst relative error {worst:e}");
}

#[test]
fn cci_matches_the_textbook_index() {
    let mut worst = 0.0_f64;
    for (level, step) in SCALES {
        let b = bars_at(level, step, 4000);
        let (h, l, c) = hlc(&b);
        for n in [1usize, 2, 5, 20, 200, 700] {
            worst = worst.max(assert_close(
                &format!("cci({n}) level {level}"),
                &series::cci(&h, &l, &c, n as f64).unwrap(),
                &cci_ref(&h, &l, &c, n),
                1e-9,
                1.0,
            ));
        }
    }
    println!("cci: worst relative error {worst:e}");
}

#[test]
fn williams_matches_the_textbook_percent_r() {
    let mut worst = 0.0_f64;
    for (level, step) in SCALES {
        let b = bars_at(level, step, 4000);
        let (h, l, c) = hlc(&b);
        for n in [1usize, 2, 14, 20, 200, 700] {
            worst = worst.max(assert_close(
                &format!("williams({n}) level {level}"),
                &series::williams(&h, &l, &c, n as f64).unwrap(),
                &williams_ref(&h, &l, &c, n),
                1e-12,
                1.0,
            ));
        }
    }
    println!("williams: worst relative error {worst:e}");
}

#[test]
fn stochastic_matches_the_textbook_slow_stochastic() {
    let mut worst = 0.0_f64;
    for (level, step) in SCALES {
        let b = bars_at(level, step, 4000);
        let (h, l, c) = hlc(&b);
        for (n, sk, sd) in
            [(14usize, 3usize, 3usize), (1, 1, 1), (5, 2, 7), (30, 20, 11), (200, 5, 5)]
        {
            let got = series::stochastic(&h, &l, &c, n as f64, sk as f64, sd as f64).unwrap();
            let want = stochastic_ref(&h, &l, &c, n, sk, sd);
            for k in 0..2 {
                worst = worst.max(assert_close(
                    &format!("stochastic({n},{sk},{sd}) line {k} level {level}"),
                    &got[k],
                    &want[k],
                    1e-9,
                    1.0,
                ));
            }
        }
    }
    println!("stochastic: worst relative error {worst:e}");
}

#[test]
fn awesome_matches_the_textbook_oscillator() {
    let mut worst = 0.0_f64;
    for (level, step) in SCALES {
        let b = bars_at(level, step, 4000);
        let (h, l, _) = hlc(&b);
        for (f, s) in [(5usize, 34usize), (1, 1), (2, 3), (34, 5), (20, 200), (300, 7)] {
            worst = worst.max(assert_close(
                &format!("awesome({f},{s}) level {level}"),
                &series::awesome(&h, &l, f as f64, s as f64).unwrap(),
                &awesome_ref(&h, &l, f, s),
                1e-9,
                step,
            ));
        }
    }
    println!("awesome: worst relative error {worst:e}");
}

#[test]
fn donchian_matches_the_textbook_channel() {
    for (level, step) in SCALES {
        let b = bars_at(level, step, 4000);
        let (h, l, _) = hlc(&b);
        for n in [1usize, 2, 14, 20, 200, 700] {
            let got = series::donchian(&h, &l, n as f64).unwrap();
            let want = donchian_ref(&h, &l, n);
            for k in 0..3 {
                assert_close(
                    &format!("donchian({n}) line {k} level {level}"),
                    &got[k],
                    &want[k],
                    1e-15,
                    0.0,
                );
            }
        }
    }
}

#[test]
fn keltner_matches_the_textbook_channel() {
    let mut worst = 0.0_f64;
    for (level, step) in SCALES {
        let b = bars_at(level, step, 4000);
        let (h, l, c) = hlc(&b);
        for (en, an, m) in
            [(20usize, 10usize, 2.0), (1, 1, 0.1), (3, 12, 1.5), (50, 7, 10.0), (200, 100, 2.0)]
        {
            let got = series::keltner(&h, &l, &c, en as f64, an as f64, m).unwrap();
            let want = keltner_ref(&h, &l, &c, en, an, m);
            for k in 0..3 {
                worst = worst.max(assert_close(
                    &format!("keltner({en},{an},{m}) line {k} level {level}"),
                    &got[k],
                    &want[k],
                    1e-9,
                    0.0,
                ));
            }
        }
    }
    println!("keltner: worst relative error {worst:e}");
}

// ------------------------------------------------------------------------------------------------
// The ways in agree bit for bit
// ------------------------------------------------------------------------------------------------

fn lines<const N: usize>(v: &[[f64; N]]) -> Vec<Vec<f64>> {
    (0..N).map(|k| v.iter().map(|x| x[k]).collect()).collect()
}

fn agree(what: &str, ways: [(&str, Vec<Vec<f64>>); 4]) {
    let (first_name, first) = &ways[0];
    for (name, other) in &ways[1..] {
        assert_eq!(first.len(), other.len(), "{what}: {first_name} vs {name}: line count");
        for (k, (a, b)) in first.iter().zip(other).enumerate() {
            assert!(same(a, b), "{what} line {k}: {first_name} vs {name}");
        }
    }
}

/// Bars of a series of length `len` with a NaN travelling through it in one of four ways.
fn variants(len: usize) -> Vec<Vec<Bar>> {
    let base = bars_of(&closes(100.0, 2.0, len));
    let mut all = vec![base.clone()];
    if len > 40 {
        let mut v = base.clone();
        v[len / 3] = Bar::ohlcv(0, f64::NAN, f64::NAN, f64::NAN, f64::NAN, 1.0);
        all.push(v);
        let mut v = base.clone();
        v[len / 2].close = f64::NAN;
        all.push(v);
        let mut v = base;
        v[2 * len / 3].high = f64::NAN;
        v[len / 4].low = f64::NAN;
        all.push(v);
    }
    all
}

const PERIODS: [usize; 9] = [1, 2, 7, 20, 64, 511, 512, 513, 1000];

fn lengths(n: usize) -> [usize; 8] {
    [0, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600]
}

#[test]
fn wma_every_way_in_gives_the_same_bits() {
    for n in PERIODS {
        for len in lengths(n) {
            for b in variants(len) {
                let (_, _, c) = hlc(&b);
                let nf = n as f64;
                let mut ind = Wma::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Wma::new(nf).unwrap();
                let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
                agree(
                    &format!("wma n={n} len={len}"),
                    [
                        ("next", vec![stream]),
                        ("next_value", vec![value]),
                        ("vectorize", Wma::new(nf).unwrap().vectorize(&b)),
                        ("series", vec![series::wma(&c, nf).unwrap()]),
                    ],
                );
            }
        }
    }
}

#[test]
fn cci_and_williams_every_way_in_gives_the_same_bits() {
    for n in PERIODS {
        for len in lengths(n) {
            for b in variants(len) {
                let (h, l, c) = hlc(&b);
                let nf = n as f64;

                let mut ind = Cci::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Cci::new(nf).unwrap();
                let value: Vec<f64> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("cci n={n} len={len}"),
                    [
                        ("next", vec![stream]),
                        ("next_hlc", vec![value]),
                        ("vectorize", Cci::new(nf).unwrap().vectorize(&b)),
                        ("series", vec![series::cci(&h, &l, &c, nf).unwrap()]),
                    ],
                );

                let mut ind = Williams::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Williams::new(nf).unwrap();
                let value: Vec<f64> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("williams n={n} len={len}"),
                    [
                        ("next", vec![stream]),
                        ("next_hlc", vec![value]),
                        ("vectorize", Williams::new(nf).unwrap().vectorize(&b)),
                        ("series", vec![series::williams(&h, &l, &c, nf).unwrap()]),
                    ],
                );
            }
        }
    }
}

#[test]
fn donchian_every_way_in_gives_the_same_bits() {
    for n in PERIODS {
        for len in lengths(n) {
            for b in variants(len) {
                let (h, l, _) = hlc(&b);
                let nf = n as f64;
                let mut ind = Donchian::new(nf).unwrap();
                let stream: Vec<[f64; 3]> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Donchian::new(nf).unwrap();
                let value: Vec<[f64; 3]> = b.iter().map(|x| ind.next_hl(x.high, x.low)).collect();
                agree(
                    &format!("donchian n={n} len={len}"),
                    [
                        ("next", lines(&stream)),
                        ("next_hl", lines(&value)),
                        ("vectorize", Donchian::new(nf).unwrap().vectorize(&b)),
                        ("series", series::donchian(&h, &l, nf).unwrap().to_vec()),
                    ],
                );
            }
        }
    }
}

#[test]
fn stochastic_every_way_in_gives_the_same_bits() {
    for (n, sk, sd) in [
        (1usize, 1usize, 1usize),
        (2, 3, 2),
        (14, 3, 3),
        (7, 64, 5),
        (20, 20, 511),
        (64, 512, 513),
        (511, 1, 1000),
        (512, 513, 2),
        (1000, 7, 7),
    ] {
        for len in lengths(n.max(sk).max(sd)) {
            for b in variants(len) {
                let (h, l, c) = hlc(&b);
                let (nf, kf, df) = (n as f64, sk as f64, sd as f64);
                let mut ind = Stochastic::new(nf, kf, df).unwrap();
                let stream: Vec<[f64; 2]> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Stochastic::new(nf, kf, df).unwrap();
                let value: Vec<[f64; 2]> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("stochastic ({n},{sk},{sd}) len={len}"),
                    [
                        ("next", lines(&stream)),
                        ("next_hlc", lines(&value)),
                        ("vectorize", Stochastic::new(nf, kf, df).unwrap().vectorize(&b)),
                        ("series", series::stochastic(&h, &l, &c, nf, kf, df).unwrap().to_vec()),
                    ],
                );
            }
        }
    }
}

#[test]
fn awesome_every_way_in_gives_the_same_bits() {
    for (f, s) in [
        (1usize, 1usize),
        (5, 34),
        (34, 5),
        (2, 3),
        (3, 2),
        (20, 64),
        (64, 20),
        (511, 512),
        (513, 7),
        (7, 1000),
        (1000, 1000),
    ] {
        for len in lengths(f.max(s)) {
            for b in variants(len) {
                let (h, l, _) = hlc(&b);
                let (ff, sf) = (f as f64, s as f64);
                let mut ind = Awesome::new(ff, sf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Awesome::new(ff, sf).unwrap();
                let value: Vec<f64> = b.iter().map(|x| ind.next_hl(x.high, x.low)).collect();
                agree(
                    &format!("awesome ({f},{s}) len={len}"),
                    [
                        ("next", vec![stream]),
                        ("next_hl", vec![value]),
                        ("vectorize", Awesome::new(ff, sf).unwrap().vectorize(&b)),
                        ("series", vec![series::awesome(&h, &l, ff, sf).unwrap()]),
                    ],
                );
            }
        }
    }
}

#[test]
fn keltner_every_way_in_gives_the_same_bits() {
    for (en, an, m) in [
        (1usize, 1usize, 0.1),
        (20, 10, 2.0),
        (10, 20, 2.0),
        (2, 3, 1.5),
        (64, 7, 3.0),
        (511, 512, 2.0),
        (513, 20, 0.5),
        (7, 1000, 10.0),
    ] {
        for len in lengths(en.max(an)) {
            for b in variants(len) {
                let (h, l, c) = hlc(&b);
                let (ef, af) = (en as f64, an as f64);
                let mut ind = Keltner::new(ef, af, m).unwrap();
                let stream: Vec<[f64; 3]> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Keltner::new(ef, af, m).unwrap();
                let value: Vec<[f64; 3]> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("keltner ({en},{an},{m}) len={len}"),
                    [
                        ("next", lines(&stream)),
                        ("next_hlc", lines(&value)),
                        ("vectorize", Keltner::new(ef, af, m).unwrap().vectorize(&b)),
                        ("series", series::keltner(&h, &l, &c, ef, af, m).unwrap().to_vec()),
                    ],
                );
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Behaviour the refactor must keep
// ------------------------------------------------------------------------------------------------

#[test]
fn a_nan_leaves_the_window_and_the_value_is_exact_again() {
    let mut c = closes(100.0, 2.0, 400);
    let b0 = bars_of(&c);
    c[100] = f64::NAN;
    let mut b = bars_of(&c);
    b[100] = Bar::ohlcv(0, f64::NAN, f64::NAN, f64::NAN, f64::NAN, 1.0);
    let (h, l, cl) = hlc(&b);
    let (h0, l0, c0) = hlc(&b0);
    // WMA, CCI, AO: NaN while the NaN is inside the window, then the same number a clean series gives.
    let n = 20;
    let wma = series::wma(&cl, n as f64).unwrap();
    let clean = wma_ref(&c0, n);
    assert!(
        wma[100..100 + n].iter().all(|v| v.is_nan()),
        "wma must be NaN while the NaN is in the window"
    );
    assert_close("wma after", &wma[100 + n..], &clean[100 + n..], 1e-9, 0.0);
    let cci = series::cci(&h, &l, &cl, n as f64).unwrap();
    assert!(
        cci[100..100 + n].iter().all(|v| v.is_nan()),
        "cci must be NaN while the NaN is in the window"
    );
    assert_close("cci after", &cci[100 + n..], &cci_ref(&h0, &l0, &c0, n)[100 + n..], 1e-9, 1.0);
    let ao = series::awesome(&h, &l, 5.0, 34.0).unwrap();
    assert!(
        ao[100..134].iter().all(|v| v.is_nan()),
        "awesome must be NaN while the NaN is in the slow window"
    );
    assert_close("awesome after", &ao[134..], &awesome_ref(&h0, &l0, 5, 34)[134..], 1e-9, 1.0);
}

#[test]
fn a_range_of_zero_gives_zero_not_nan() {
    let flat = vec![50.0; 100];
    for v in series::williams(&flat, &flat, &flat, 14.0).unwrap()[13..].iter() {
        assert_eq!(*v, 0.0);
    }
    for v in series::cci(&flat, &flat, &flat, 20.0).unwrap()[19..].iter() {
        assert_eq!(*v, 0.0);
    }
    let st = series::stochastic(&flat, &flat, &flat, 14.0, 3.0, 3.0).unwrap();
    for v in st[0][15..].iter().chain(&st[1][17..]) {
        assert_eq!(*v, 0.0);
    }
}

#[test]
fn a_parameter_outside_the_registry_range_is_refused() {
    let c = closes(100.0, 2.0, 100);
    assert!(series::wma(&c, 0.0).is_err());
    assert!(series::wma(&c, 1001.0).is_err());
    assert!(series::wma(&c, f64::NAN).is_err());
    assert!(series::cci(&c, &c, &c, 0.0).is_err());
    assert!(series::williams(&c, &c, &c, 1001.0).is_err());
    assert!(series::stochastic(&c, &c, &c, 14.0, 0.0, 3.0).is_err());
    assert!(series::awesome(&c, &c, 5.0, 1001.0).is_err());
    assert!(series::donchian(&c, &c, 0.0).is_err());
    assert!(series::keltner(&c, &c, &c, 20.0, 10.0, 0.0).is_err());
}

#[test]
fn reset_returns_a_stepper_to_its_first_bar() {
    let b = bars_of(&closes(100.0, 2.0, 300));
    macro_rules! check {
        ($ind:expr) => {{
            let mut ind = $ind;
            let first: Vec<Vec<f64>> = b.iter().map(|x| ind.on_bar(x)).collect();
            ind.reset();
            let second: Vec<Vec<f64>> = b.iter().map(|x| ind.on_bar(x)).collect();
            for (i, (x, y)) in first.iter().zip(&second).enumerate() {
                assert!(same(x, y), "{}: bar {i} differs after reset", ind.name());
            }
        }};
    }
    check!(Wma::new(20.0).unwrap());
    check!(Cci::new(20.0).unwrap());
    check!(Williams::new(14.0).unwrap());
    check!(Stochastic::new(14.0, 3.0, 3.0).unwrap());
    check!(Awesome::new(5.0, 34.0).unwrap());
    check!(Donchian::new(20.0).unwrap());
    check!(Keltner::new(20.0, 10.0, 2.0).unwrap());
}

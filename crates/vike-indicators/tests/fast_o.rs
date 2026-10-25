//! The price transforms (`avgprice`, `medprice`, `typprice`, `wclprice`, `heikin_ashi`) and the
//! structure indicators (`pivot_points`, `volume_profile_poc`, `zigzag`, `williams_fractal`): the fast
//! cores against references kept in this file, and the stream (`next`), the plain-number entry
//! (`next_ohlc`, `next_hl`, `next_hlc`, `next_hlcv`), the batch (`vectorize`) and the slice call
//! (`series::*`) against each other, bit for bit.
//!
//! The references are the ways these indicators were written before the fast cores: the textbook
//! formulas for the price transforms and the pivots (compared to a relative 1e-12 on three series:
//! level 100 with a step of 2, level 1e6 with a step of 0.01, level 1e-3 with a step of 1e-6), the
//! old candle function for Heikin-Ashi, and COPIES of the old kernels for the point of control, the
//! zigzag and the fractals, whose outputs are bin centres or pivot prices picked by discrete tests, so
//! those are compared bit for bit.
//!
//! `zigzag` and `williams_fractal` read future bars: their `on_bar` is a causal best effort and their
//! `vectorize` is authoritative, so the batch, the slice call and the old kernel are held together
//! and the stream is held to the old kernel's last value over the history so far.

use vike_indicators::indicators::{
    Avgprice, HeikinAshi, Medprice, PivotPoints, Typprice, VolumeProfilePoc, Wclprice,
    WilliamsFractal, Zigzag,
};
use vike_indicators::{Indicator, series};
use vike_marketdata::Bar;

// ---------------------------------------------------------------- data

fn xorshift(s: &mut u64) -> f64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    (*s >> 11) as f64 / (1u64 << 53) as f64
}

fn closes(level: f64, step: f64, n: usize) -> Vec<f64> {
    let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut p = level;
    (0..n)
        .map(|_| {
            p += (xorshift(&mut s) - 0.5) * step;
            p
        })
        .collect()
}

/// A bar around each close, its spread a few steps wide and varying, with a volume that varies.
fn bars_of(c: &[f64], scale: f64) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let up = scale * (0.3 + (i % 7) as f64 * 0.1);
            let dn = scale * (0.2 + (i % 5) as f64 * 0.15);
            let open = x + scale * ((i % 3) as f64 - 1.0) * 0.1;
            let vol = 100.0 + (i % 13) as f64 * 17.0;
            Bar::ohlcv(i as i64 * 60_000, open, x + up, x - dn, x, vol)
        })
        .collect()
}

/// A series built to hurt: a flat stretch (no range, so a one-price window), a zero-volume stretch,
/// a gap in each direction, one bar with no range that still moved, one that closes at zero.
fn awkward_bars() -> Vec<Bar> {
    let mut s: u64 = 0xD1B5_4A32_D192_ED03;
    let mut price = 100.0_f64;
    (0..2600usize)
        .map(|i| {
            let u = xorshift(&mut s);
            if i == 900 {
                price *= 2.5;
            }
            if i == 1250 {
                price *= 0.3;
            }
            let flat = (300..360).contains(&i);
            if !flat {
                price = (price + (i as f64 * 0.05).sin() * 0.6 + (u - 0.5) * 2.0).max(1.0);
            }
            let point = i == 1000;
            let close = if i == 1100 { 0.0 } else { price };
            let (high, low) = if flat || point {
                (price, price)
            } else {
                (price.max(close) + u * 1.5, price.min(close) - u * 0.7)
            };
            let volume = if (600..650).contains(&i) { 0.0 } else { 500.0 + u * 1000.0 };
            Bar::ohlcv(i as i64 * 60_000, price, high, low, close, volume)
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Field {
    Open,
    High,
    Low,
    Close,
    Volume,
}

/// `bars` with a `NaN` in one field of the bar at `at`.
fn with_nan(bars: &[Bar], at: usize, field: Field) -> Vec<Bar> {
    let mut out = bars.to_vec();
    match field {
        Field::Open => out[at].open = f64::NAN,
        Field::High => out[at].high = f64::NAN,
        Field::Low => out[at].low = f64::NAN,
        Field::Close => out[at].close = f64::NAN,
        Field::Volume => out[at].volume = f64::NAN,
    }
    out
}

/// Every data set the ways-in checks run on: three scales, the awkward series, and a `NaN`
/// travelling through each field.
fn datasets() -> Vec<(String, Vec<Bar>)> {
    let mut sets = Vec::new();
    for (level, step) in SCALES {
        sets.push((format!("level {level}"), bars_of(&closes(level, step, 2600), step * 0.5)));
    }
    let base = bars_of(&closes(100.0, 2.0, 2600), 1.0);
    for field in [Field::Open, Field::High, Field::Low, Field::Close, Field::Volume] {
        sets.push((format!("nan in {field:?}"), with_nan(&base, 1000, field)));
    }
    sets.push(("awkward".to_string(), awkward_bars()));
    // Prices on a coarse grid: equal extremes and equal closes everywhere (ties for the window's
    // extremes, ties between bins, repeated bars).
    let grid: Vec<Bar> = bars_of(&closes(100.0, 3.0, 2600), 1.0)
        .into_iter()
        .map(|b| {
            let r = |x: f64| (x * 0.5).round() / 0.5;
            Bar::ohlcv(
                b.ts,
                r(b.open),
                r(b.high),
                r(b.low),
                r(b.close),
                (b.volume / 50.0).round() * 50.0,
            )
        })
        .collect();
    sets.push(("grid".to_string(), grid));
    sets
}

const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];

/// The lengths a stepper is run over for a lookback `n`.
fn lengths(n: usize) -> Vec<usize> {
    let mut v = vec![0, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600];
    v.sort_unstable();
    v.dedup();
    v.into_iter().filter(|&l| l <= 2600).collect()
}

struct Cols {
    o: Vec<f64>,
    h: Vec<f64>,
    l: Vec<f64>,
    c: Vec<f64>,
    v: Vec<f64>,
}

fn cols(b: &[Bar]) -> Cols {
    Cols {
        o: b.iter().map(|x| x.open).collect(),
        h: b.iter().map(|x| x.high).collect(),
        l: b.iter().map(|x| x.low).collect(),
        c: b.iter().map(|x| x.close).collect(),
        v: b.iter().map(|x| x.volume).collect(),
    }
}

// ---------------------------------------------------------------- comparison helpers

fn assert_same(name: &str, got: &[f64], want: &[f64]) {
    assert_eq!(got.len(), want.len(), "{name}: length");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert!(
            g.to_bits() == w.to_bits() || (g.is_nan() && w.is_nan()),
            "{name}[{i}]: got {g:?} ({:016x}), want {w:?} ({:016x})",
            g.to_bits(),
            w.to_bits()
        );
    }
}

/// Relative error against `want`, with the denominator floored at `floor`.
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

fn col<const N: usize>(rows: &[[f64; N]], k: usize) -> Vec<f64> {
    rows.iter().map(|r| r[k]).collect()
}

fn lines_of<const N: usize>(rows: &[[f64; N]]) -> Vec<Vec<f64>> {
    (0..N).map(|k| col(rows, k)).collect()
}

fn assert_lines_same(name: &str, got: &[Vec<f64>], want: &[Vec<f64>]) {
    assert_eq!(got.len(), want.len(), "{name}: line count");
    for (k, (g, w)) in got.iter().zip(want).enumerate() {
        assert_same(&format!("{name} line {k}"), g, w);
    }
}

// ---------------------------------------------------------------- the references

fn avg_ref(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| (x.open + x.high + x.low + x.close) / 4.0).collect()
}
fn med_ref(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| (x.high + x.low) / 2.0).collect()
}
fn typ_ref(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| (x.high + x.low + x.close) / 3.0).collect()
}
fn wcl_ref(b: &[Bar]) -> Vec<f64> {
    b.iter().map(|x| (x.high + x.low + 2.0 * x.close) / 4.0).collect()
}

/// The old `heikin_ashi_candle`, folded.
fn ha_ref(b: &[Bar]) -> Vec<Vec<f64>> {
    let mut out = vec![Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut prev: Option<(f64, f64)> = None;
    for bar in b {
        let (o, h, l, c) = (bar.open, bar.high, bar.low, bar.close);
        let ha_close = (o + h + l + c) / 4.0;
        let ha_open = match prev {
            None => (o + c) / 2.0,
            Some((po, pc)) => (po + pc) / 2.0,
        };
        let ha_high = h.max(ha_open).max(ha_close);
        let ha_low = l.min(ha_open).min(ha_close);
        prev = Some((ha_open, ha_close));
        for (line, x) in out.iter_mut().zip([ha_open, ha_high, ha_low, ha_close]) {
            line.push(x);
        }
    }
    out
}

/// The old `batch_pivot_points`.
fn pivot_ref(b: &[Bar]) -> Vec<Vec<f64>> {
    let n = b.len();
    let mut out: Vec<Vec<f64>> = (0..7).map(|_| vec![f64::NAN; n]).collect();
    for i in 1..n {
        let (ph, pl, pc) = (b[i - 1].high, b[i - 1].low, b[i - 1].close);
        let p = (ph + pl + pc) / 3.0;
        let rng = ph - pl;
        out[0][i] = p;
        out[1][i] = 2.0 * p - pl;
        out[4][i] = 2.0 * p - ph;
        out[2][i] = p + rng;
        out[5][i] = p - rng;
        out[3][i] = ph + 2.0 * (p - pl);
        out[6][i] = pl - 2.0 * (ph - p);
    }
    out
}

/// The old `VolumeProfilePoc`: a ring of the last `window` bars histogrammed on every bar.
fn poc_ref(b: &[Bar], window: usize, bins: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; b.len()];
    for i in 0..b.len() {
        if i + 1 < window {
            continue;
        }
        let ring = &b[i + 1 - window..=i];
        let price_min = ring.iter().map(|x| x.low).fold(f64::MAX, f64::min);
        let price_max = ring.iter().map(|x| x.high).fold(f64::MIN, f64::max);
        if price_max == price_min {
            out[i] = price_min;
            continue;
        }
        let bin_width = (price_max - price_min) / bins as f64;
        let mut bucket = vec![0.0; bins];
        for x in ring {
            let mut idx = ((x.close - price_min) / bin_width) as usize;
            if idx >= bins {
                idx = bins - 1;
            }
            bucket[idx] += x.volume;
        }
        let max_vol = bucket.iter().copied().fold(f64::MIN, f64::max);
        let poc_idx = bucket.iter().position(|&bv| bv == max_vol).unwrap();
        out[i] = price_min + (poc_idx as f64 + 0.5) * bin_width;
    }
    out
}

/// The old `batch_zigzag`.
fn zigzag_ref(b: &[Bar], deviation: f64) -> Vec<f64> {
    let highs: Vec<f64> = b.iter().map(|x| x.high).collect();
    let lows: Vec<f64> = b.iter().map(|x| x.low).collect();
    let n = highs.len();
    let mut out = vec![f64::NAN; n];
    if n < 2 {
        return out;
    }
    let mut direction: i32 = 1;
    let mut extreme_price = highs[0];
    let mut extreme_idx = 0usize;
    for i in 1..n {
        if direction == 1 {
            if highs[i] >= extreme_price {
                extreme_price = highs[i];
                extreme_idx = i;
            } else if extreme_price > 0.0
                && (extreme_price - lows[i]) / extreme_price * 100.0 >= deviation
            {
                out[extreme_idx] = extreme_price;
                extreme_price = lows[i];
                extreme_idx = i;
                direction = -1;
            }
        } else if lows[i] <= extreme_price {
            extreme_price = lows[i];
            extreme_idx = i;
        } else if extreme_price > 0.0
            && (highs[i] - extreme_price) / extreme_price * 100.0 >= deviation
        {
            out[extreme_idx] = extreme_price;
            extreme_price = highs[i];
            extreme_idx = i;
            direction = 1;
        }
    }
    out
}

/// The old `batch_williams_fractal`.
fn fractal_ref(b: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let highs: Vec<f64> = b.iter().map(|x| x.high).collect();
    let lows: Vec<f64> = b.iter().map(|x| x.low).collect();
    let length = highs.len();
    let mut fu = vec![f64::NAN; length];
    let mut fd = vec![f64::NAN; length];
    if length > n {
        for i in n..(length - n) {
            let (h_centre, l_centre) = (highs[i], lows[i]);
            let (mut is_up, mut is_down) = (true, true);
            for j in (i - n)..=(i + n) {
                if j == i {
                    continue;
                }
                if highs[j] >= h_centre {
                    is_up = false;
                }
                if lows[j] <= l_centre {
                    is_down = false;
                }
                if !is_up && !is_down {
                    break;
                }
            }
            if is_up {
                fu[i] = h_centre;
            }
            if is_down {
                fd[i] = l_centre;
            }
        }
    }
    vec![fu, fd]
}

// ---------------------------------------------------------------- price transforms

#[test]
fn price_transforms_match_the_textbook_formulas() {
    for (level, step) in SCALES {
        let b = bars_of(&closes(level, step, 4000), step * 0.5);
        let x = cols(&b);
        let name = |s: &str| format!("{s} level {level}");
        assert_close(
            &name("avgprice"),
            &series::avgprice(&x.o, &x.h, &x.l, &x.c),
            &avg_ref(&b),
            1e-12,
            1e-300,
        );
        assert_close(&name("medprice"), &series::medprice(&x.h, &x.l), &med_ref(&b), 1e-12, 1e-300);
        assert_close(
            &name("typprice"),
            &series::typprice(&x.h, &x.l, &x.c),
            &typ_ref(&b),
            1e-12,
            1e-300,
        );
        assert_close(
            &name("wclprice"),
            &series::wclprice(&x.h, &x.l, &x.c),
            &wcl_ref(&b),
            1e-12,
            1e-300,
        );
    }
}

#[test]
fn price_transforms_keep_the_exact_bits_of_the_old_formulas() {
    for (name, b) in datasets() {
        let x = cols(&b);
        assert_same(
            &format!("avgprice {name}"),
            &series::avgprice(&x.o, &x.h, &x.l, &x.c),
            &avg_ref(&b),
        );
        assert_same(&format!("medprice {name}"), &series::medprice(&x.h, &x.l), &med_ref(&b));
        assert_same(&format!("typprice {name}"), &series::typprice(&x.h, &x.l, &x.c), &typ_ref(&b));
        assert_same(&format!("wclprice {name}"), &series::wclprice(&x.h, &x.l, &x.c), &wcl_ref(&b));
    }
}

#[test]
fn price_transforms_hand_values() {
    // O 10, H 14, L 8, C 12: (10+14+8+12)/4 = 11, (14+8)/2 = 11, (14+8+12)/3 = 34/3, (14+8+24)/4 = 11.5
    assert_eq!(series::avgprice(&[10.0], &[14.0], &[8.0], &[12.0]), vec![11.0]);
    assert_eq!(series::medprice(&[14.0], &[8.0]), vec![11.0]);
    assert_eq!(series::typprice(&[14.0], &[8.0], &[12.0]), vec![34.0 / 3.0]);
    assert_eq!(series::wclprice(&[14.0], &[8.0], &[12.0]), vec![11.5]);
}

/// Stream, plain-number entry, `vectorize` and slice call of the four one-line transforms.
#[test]
fn price_transform_ways_in_agree_bit_for_bit() {
    for (name, b) in datasets() {
        for len in lengths(1) {
            let b = &b[..len];
            let x = cols(b);
            let (mut a1, mut a2) = (Avgprice::default(), Avgprice::default());
            let (mut m1, mut m2) = (Medprice::default(), Medprice::default());
            let (mut t1, mut t2) = (Typprice::default(), Typprice::default());
            let (mut w1, mut w2) = (Wclprice::default(), Wclprice::default());
            let mut got: [Vec<f64>; 8] = Default::default();
            for (i, bar) in b.iter().enumerate() {
                got[0].push(a1.next(bar));
                got[1].push(a2.next_ohlc(x.o[i], x.h[i], x.l[i], x.c[i]));
                got[2].push(m1.next(bar));
                got[3].push(m2.next_hl(x.h[i], x.l[i]));
                got[4].push(t1.next(bar));
                got[5].push(t2.next_hlc(x.h[i], x.l[i], x.c[i]));
                got[6].push(w1.next(bar));
                got[7].push(w2.next_hlc(x.h[i], x.l[i], x.c[i]));
            }
            let tag = format!("{name} len {len}");
            let batch = [
                Avgprice::default().vectorize(b).remove(0),
                Medprice::default().vectorize(b).remove(0),
                Typprice::default().vectorize(b).remove(0),
                Wclprice::default().vectorize(b).remove(0),
            ];
            let slice = [
                series::avgprice(&x.o, &x.h, &x.l, &x.c),
                series::medprice(&x.h, &x.l),
                series::typprice(&x.h, &x.l, &x.c),
                series::wclprice(&x.h, &x.l, &x.c),
            ];
            for k in 0..4 {
                assert_same(
                    &format!("{tag}: next vs next_*, indicator {k}"),
                    &got[2 * k],
                    &got[2 * k + 1],
                );
                assert_same(
                    &format!("{tag}: next vs vectorize, indicator {k}"),
                    &got[2 * k],
                    &batch[k],
                );
                assert_same(
                    &format!("{tag}: next vs series, indicator {k}"),
                    &got[2 * k],
                    &slice[k],
                );
            }
            // `on_bar` and `value`.
            let mut a3 = Avgprice::default();
            for (i, bar) in b.iter().enumerate() {
                assert_eq!(a3.on_bar(bar)[0].to_bits(), got[0][i].to_bits());
                assert_eq!(a3.value()[0].to_bits(), got[0][i].to_bits());
            }
            a3.reset();
            assert!(a3.value()[0].is_nan());
        }
    }
}

// ---------------------------------------------------------------- heikin_ashi

#[test]
fn heikin_ashi_matches_the_old_candle_function() {
    for (name, b) in datasets() {
        let x = cols(&b);
        let want = ha_ref(&b);
        let got = series::heikin_ashi(&x.o, &x.h, &x.l, &x.c);
        assert_lines_same(&format!("heikin_ashi {name}"), &got, &want);
    }
}

#[test]
fn heikin_ashi_hand_values() {
    // Bar 0: O 10 H 14 L 8 C 12 -> close (10+14+8+12)/4 = 11, open (10+12)/2 = 11, high 14, low 8.
    // Bar 1: O 12 H 13 L 11 C 11 -> close 11.75, open (11+11)/2 = 11, high 13, low 11.
    let out = series::heikin_ashi(&[10.0, 12.0], &[14.0, 13.0], &[8.0, 11.0], &[12.0, 11.0]);
    assert_eq!(out[0], vec![11.0, 11.0]);
    assert_eq!(out[1], vec![14.0, 13.0]);
    assert_eq!(out[2], vec![8.0, 11.0]);
    assert_eq!(out[3], vec![11.0, 11.75]);
}

#[test]
fn heikin_ashi_ways_in_agree_bit_for_bit() {
    for (name, b) in datasets() {
        for len in lengths(1) {
            let b = &b[..len];
            let x = cols(b);
            let (mut s1, mut s2, mut s3) =
                (HeikinAshi::default(), HeikinAshi::default(), HeikinAshi::default());
            let (mut r1, mut r2, mut r3) = (Vec::new(), Vec::new(), Vec::new());
            for (i, bar) in b.iter().enumerate() {
                r1.push(s1.next(bar));
                r2.push(s2.next_ohlc(x.o[i], x.h[i], x.l[i], x.c[i]));
                r3.push(s3.on_bar(bar));
                assert_same("value is the last on_bar", &s3.value(), &r3[i]);
            }
            let tag = format!("{name} len {len}");
            let stream = lines_of(&r1);
            assert_lines_same(&format!("{tag}: next vs next_ohlc"), &stream, &lines_of(&r2));
            assert_lines_same(
                &format!("{tag}: next vs on_bar"),
                &stream,
                &(0..4).map(|k| r3.iter().map(|r| r[k]).collect()).collect::<Vec<Vec<f64>>>(),
            );
            assert_lines_same(
                &format!("{tag}: next vs vectorize"),
                &stream,
                &HeikinAshi::default().vectorize(b),
            );
            assert_lines_same(
                &format!("{tag}: next vs series"),
                &stream,
                &series::heikin_ashi(&x.o, &x.h, &x.l, &x.c),
            );
            // A reset stream starts again from the first candle.
            if len > 5 {
                s1.reset();
                assert_eq!(s1.next(&b[0]).map(f64::to_bits), r1[0].map(f64::to_bits));
            }
        }
    }
}

// ---------------------------------------------------------------- pivot_points

#[test]
fn pivot_points_match_the_textbook() {
    for (level, step) in SCALES {
        let b = bars_of(&closes(level, step, 4000), step * 0.5);
        let x = cols(&b);
        let got = series::pivot_points(&x.h, &x.l, &x.c);
        let want = pivot_ref(&b);
        for k in 0..7 {
            assert_close(
                &format!("pivot line {k} level {level}"),
                &got[k],
                &want[k],
                1e-12,
                1e-300,
            );
        }
    }
    for (name, b) in datasets() {
        let x = cols(&b);
        assert_lines_same(
            &format!("pivot {name}"),
            &series::pivot_points(&x.h, &x.l, &x.c),
            &pivot_ref(&b),
        );
    }
}

#[test]
fn pivot_points_hand_values() {
    // Bar 0: H 12 L 8 C 10 -> P 10, range 4: R1 12, R2 14, R3 16, S1 8, S2 6, S3 4 on bar 1.
    let out = series::pivot_points(&[12.0, 13.0], &[8.0, 9.0], &[10.0, 11.0]);
    for line in &out {
        assert!(line[0].is_nan());
    }
    let at1: Vec<f64> = out.iter().map(|l| l[1]).collect();
    assert_eq!(at1, vec![10.0, 12.0, 14.0, 16.0, 8.0, 6.0, 4.0]);
}

#[test]
fn pivot_points_ways_in_agree_bit_for_bit() {
    for (name, b) in datasets() {
        for len in lengths(1) {
            let b = &b[..len];
            let x = cols(b);
            let (mut s1, mut s2, mut s3) =
                (PivotPoints::default(), PivotPoints::default(), PivotPoints::default());
            let (mut r1, mut r2, mut r3) = (Vec::new(), Vec::new(), Vec::new());
            for (i, bar) in b.iter().enumerate() {
                r1.push(s1.next(bar));
                r2.push(s2.next_hlc(x.h[i], x.l[i], x.c[i]));
                r3.push(s3.on_bar(bar));
            }
            let tag = format!("{name} len {len}");
            let stream = lines_of(&r1);
            assert_lines_same(&format!("{tag}: next vs next_hlc"), &stream, &lines_of(&r2));
            let via_on_bar: Vec<Vec<f64>> =
                (0..7).map(|k| r3.iter().map(|r| r[k]).collect()).collect();
            assert_lines_same(&format!("{tag}: next vs on_bar"), &stream, &via_on_bar);
            assert_lines_same(
                &format!("{tag}: next vs vectorize"),
                &stream,
                &PivotPoints::default().vectorize(b),
            );
            assert_lines_same(
                &format!("{tag}: next vs series"),
                &stream,
                &series::pivot_points(&x.h, &x.l, &x.c),
            );
        }
    }
}

// ---------------------------------------------------------------- volume_profile_poc

/// `(window, bins)` pairs the registry accepts: `window` `5..=500`, `bins` `4..=200`.
const POC_PARAMS: [(usize, usize); 9] =
    [(5, 4), (5, 200), (6, 24), (20, 24), (50, 24), (64, 7), (100, 200), (499, 33), (500, 4)];

#[test]
fn volume_profile_poc_matches_the_old_kernel_bit_for_bit() {
    for (name, b) in datasets() {
        let x = cols(&b);
        for (w, bins) in POC_PARAMS {
            let want = poc_ref(&b, w, bins);
            let got =
                series::volume_profile_poc(&x.h, &x.l, &x.c, &x.v, w as f64, bins as f64).unwrap();
            assert_same(&format!("poc({w}, {bins}) {name}"), &got, &want);
        }
    }
}

#[test]
fn volume_profile_poc_hand_value() {
    // Closes 10.5 .. 13.5 with a half-point either side: the range is 10..14, four bins of width 1.
    // Volume 1, 5, 2, 1, 1 in bins 0, 1, 1, 2, 3: bin 1 holds 7, its centre is 11.5.
    let c = [10.5, 11.5, 11.7, 12.5, 13.5];
    let h: Vec<f64> = c.iter().map(|x| x + 0.5).collect();
    let l: Vec<f64> = c.iter().map(|x| x - 0.5).collect();
    let v = [1.0, 5.0, 2.0, 1.0, 1.0];
    let out = series::volume_profile_poc(&h, &l, &c, &v, 5.0, 4.0).unwrap();
    assert!(out[..4].iter().all(|x| x.is_nan()));
    assert_eq!(out[4], 11.5);
    // A window with no price range answers with that price.
    let flat = [7.0; 6];
    let out = series::volume_profile_poc(&flat, &flat, &flat, &[1.0; 6], 5.0, 4.0).unwrap();
    assert_eq!(out[4..], [7.0, 7.0]);
}

/// The old kernel unwrapped an empty "highest bin" and panicked when every bin held a `NaN`; the
/// answer is `NaN` now.
#[test]
fn volume_profile_poc_with_no_highest_bin_is_nan() {
    let c = [10.5, 11.5, 12.5, 13.5, 13.9];
    let h: Vec<f64> = c.iter().map(|x| x + 0.5).collect();
    let l: Vec<f64> = c.iter().map(|x| x - 0.5).collect();
    let v = [f64::NAN; 5];
    let out = series::volume_profile_poc(&h, &l, &c, &v, 5.0, 4.0).unwrap();
    assert!(out[4].is_nan());
    let mut s = VolumeProfilePoc::new(5.0, 4.0).unwrap();
    let mut last = 0.0;
    for i in 0..5 {
        last = s.next_hlcv(h[i], l[i], c[i], v[i]);
    }
    assert!(last.is_nan());
}

#[test]
fn volume_profile_poc_refuses_what_the_registry_refuses() {
    let z = [1.0; 10];
    assert!(series::volume_profile_poc(&z, &z, &z, &z, 4.0, 24.0).is_err());
    assert!(series::volume_profile_poc(&z, &z, &z, &z, 501.0, 24.0).is_err());
    assert!(series::volume_profile_poc(&z, &z, &z, &z, 50.0, 3.0).is_err());
    assert!(series::volume_profile_poc(&z, &z, &z, &z, 50.0, 201.0).is_err());
    assert!(series::volume_profile_poc(&z, &z, &z, &z, f64::NAN, 24.0).is_err());
    assert!(VolumeProfilePoc::new(4.0, 24.0).is_err());
}

#[test]
fn volume_profile_poc_ways_in_agree_bit_for_bit() {
    for (name, b) in datasets() {
        let x = cols(&b);
        for (w, bins) in POC_PARAMS {
            for len in lengths(w) {
                let b = &b[..len];
                let (h, l, c, v) = (&x.h[..len], &x.l[..len], &x.c[..len], &x.v[..len]);
                let mut s1 = VolumeProfilePoc::new(w as f64, bins as f64).unwrap();
                let mut s2 = s1.clone();
                let mut s3 = s1.clone();
                let (mut r1, mut r2, mut r3) = (Vec::new(), Vec::new(), Vec::new());
                for (i, bar) in b.iter().enumerate() {
                    r1.push(s1.next(bar));
                    r2.push(s2.next_hlcv(h[i], l[i], c[i], v[i]));
                    r3.push(s3.on_bar(bar)[0]);
                }
                let tag = format!("poc({w}, {bins}) {name} len {len}");
                assert_same(&format!("{tag}: next vs next_hlcv"), &r1, &r2);
                assert_same(&format!("{tag}: next vs on_bar"), &r1, &r3);
                assert_same(&format!("{tag}: next vs vectorize"), &r1, &s1.vectorize(b)[0]);
                let slice = series::volume_profile_poc(h, l, c, v, w as f64, bins as f64).unwrap();
                assert_same(&format!("{tag}: next vs series"), &r1, &slice);
                let first = r1.iter().position(|x| !x.is_nan());
                if let Some(f) = first {
                    assert!(f >= w - 1, "{tag}: an answer before the window is full");
                }
            }
        }
    }
}

#[test]
fn volume_profile_poc_reset_starts_over() {
    let b = bars_of(&closes(100.0, 2.0, 200), 1.0);
    let mut s = VolumeProfilePoc::new(20.0, 24.0).unwrap();
    let first: Vec<f64> = b.iter().map(|x| s.next(x)).collect();
    s.reset();
    let second: Vec<f64> = b.iter().map(|x| s.next(x)).collect();
    assert_same("poc after reset", &second, &first);
}

// ---------------------------------------------------------------- zigzag

const DEVIATIONS: [f64; 5] = [0.1, 0.5, 2.5, 5.0, 50.0];

#[test]
fn zigzag_matches_the_old_kernel_bit_for_bit() {
    for (name, b) in datasets() {
        let x = cols(&b);
        for dev in DEVIATIONS {
            let want = zigzag_ref(&b, dev);
            let batch = Zigzag::new(dev).unwrap().vectorize(&b).remove(0);
            let slice = series::zigzag(&x.h, &x.l, dev).unwrap();
            assert_same(&format!("zigzag({dev}) {name}: vectorize"), &batch, &want);
            assert_same(&format!("zigzag({dev}) {name}: series"), &slice, &want);
        }
    }
}

#[test]
fn zigzag_hand_value() {
    let h = [10.0, 11.0, 12.0, 11.0, 10.0, 9.0, 10.0, 11.0];
    let l = [9.0, 10.0, 11.0, 10.0, 9.0, 8.0, 9.0, 10.0];
    let out = series::zigzag(&h, &l, 10.0).unwrap();
    let want = [f64::NAN, f64::NAN, 12.0, f64::NAN, f64::NAN, 8.0, f64::NAN, f64::NAN];
    assert_same("zigzag", &out, &want);
    assert!(series::zigzag(&h, &l, 0.0).is_err());
    assert!(series::zigzag(&h, &l, 51.0).is_err());
    assert_eq!(series::zigzag(&h[..1], &l[..1], 5.0).unwrap().len(), 1);
    assert!(series::zigzag(&[], &[], 5.0).unwrap().is_empty());
    // The shorter slice decides the length.
    assert_eq!(series::zigzag(&h, &l[..4], 5.0).unwrap().len(), 4);
}

/// Streaming is untouched: the stream tip is the last value of the batch kernel over the history so
/// far (the history is short enough that the stream retains all of it).
#[test]
fn zigzag_stream_is_the_batch_tip_over_the_history() {
    let b = bars_of(&closes(100.0, 4.0, 300), 1.0);
    for dev in [0.5, 5.0] {
        let mut s = Zigzag::new(dev).unwrap();
        for i in 0..b.len() {
            let got = s.next(&b[i]);
            let want = *zigzag_ref(&b[..=i], dev).last().unwrap();
            assert!(got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan()), "bar {i}");
        }
    }
}

// ---------------------------------------------------------------- williams_fractal

#[test]
fn williams_fractal_matches_the_old_kernel_bit_for_bit() {
    for (name, b) in datasets() {
        let x = cols(&b);
        for n in [1usize, 2, 3, 5, 10] {
            let want = fractal_ref(&b, n);
            let batch = WilliamsFractal::new(n as f64).unwrap().vectorize(&b);
            let slice = series::williams_fractal(&x.h, &x.l, n as f64).unwrap();
            assert_lines_same(&format!("fractal({n}) {name}: vectorize"), &batch, &want);
            assert_lines_same(&format!("fractal({n}) {name}: series"), &slice, &want);
        }
    }
    // Short inputs, including one shorter than the window on each side.
    let b = bars_of(&closes(100.0, 4.0, 40), 1.0);
    for n in [1usize, 2, 10] {
        for len in [0usize, 1, n, 2 * n, 2 * n + 1, 2 * n + 2, 40] {
            let x = cols(&b[..len.min(40)]);
            let got = series::williams_fractal(&x.h, &x.l, n as f64).unwrap();
            assert_lines_same(
                &format!("fractal({n}) len {len}"),
                &got,
                &fractal_ref(&b[..len.min(40)], n),
            );
        }
    }
}

#[test]
fn williams_fractal_hand_value() {
    let h = [1.0, 2.0, 5.0, 2.0, 1.0, 2.0, 3.0];
    let l = [0.0, 1.0, 4.0, 1.0, 0.0, 1.0, 2.0];
    let [up, down] = series::williams_fractal(&h, &l, 2.0).unwrap();
    assert_same(
        "fractal_up",
        &up,
        &[f64::NAN, f64::NAN, 5.0, f64::NAN, f64::NAN, f64::NAN, f64::NAN],
    );
    assert_same(
        "fractal_down",
        &down,
        &[f64::NAN, f64::NAN, f64::NAN, f64::NAN, 0.0, f64::NAN, f64::NAN],
    );
    assert!(series::williams_fractal(&h, &l, 0.0).is_err());
    assert!(series::williams_fractal(&h, &l, 11.0).is_err());
}

/// Streaming is untouched: the stream tip is the last value of the batch kernel over the history so
/// far, which is `NaN` while the fractal still needs bars to its right.
#[test]
fn williams_fractal_stream_is_the_batch_tip_over_the_history() {
    let b = bars_of(&closes(100.0, 4.0, 200), 1.0);
    for n in [1usize, 2, 4] {
        let mut s = WilliamsFractal::new(n as f64).unwrap();
        for i in 0..b.len() {
            let got = s.next(&b[i]);
            let want = fractal_ref(&b[..=i], n);
            for k in 0..2 {
                let w = *want[k].last().unwrap();
                assert!(
                    got[k].to_bits() == w.to_bits() || (got[k].is_nan() && w.is_nan()),
                    "bar {i}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------- the registry rows

/// Each new entry point answers like the row of the registry that builds the type by name.
#[test]
fn the_registry_rows_answer_like_the_typed_steppers() {
    let b = bars_of(&closes(100.0, 2.0, 600), 1.0);
    for name in [
        "avgprice",
        "medprice",
        "typprice",
        "wclprice",
        "heikin_ashi",
        "pivot_points",
        "volume_profile_poc",
    ] {
        let row = vike_indicators::make(name).unwrap().vectorize(&b);
        let typed: Vec<Vec<f64>> = match name {
            "avgprice" => Avgprice::default().vectorize(&b),
            "medprice" => Medprice::default().vectorize(&b),
            "typprice" => Typprice::default().vectorize(&b),
            "wclprice" => Wclprice::default().vectorize(&b),
            "heikin_ashi" => HeikinAshi::default().vectorize(&b),
            "pivot_points" => PivotPoints::default().vectorize(&b),
            _ => VolumeProfilePoc::default().vectorize(&b),
        };
        assert_lines_same(name, &row, &typed);
    }
}

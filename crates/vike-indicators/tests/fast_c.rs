//! ROC, OBV, parabolic SAR, the A/D line, ADX and Stochastic RSI: the fast cores against a textbook
//! reference, and the stream (`next`), the plain-number entry (`next_value`, `next_cv`, `next_hl`,
//! `next_hlcv`, `next_hlc`), the batch (`vectorize`) and the slice call (`series::*`) against each
//! other, bit for bit.
//!
//! The references do each step the long way (divisions, the textbook `sar + af * (ep - sar)`, a
//! window folded from scratch, the simple-average seeds). The cores multiply by a precomputed
//! `1/n` or `(n-1)/n`, so the last bits differ; the tolerance is relative 1e-9 on three series
//! (level 100 with a step of 2, level 1e6 with a step of 0.01, level 1e-3 with a step of 1e-6) and on
//! one built to hurt (a flat stretch, zero volume, gaps, a bar closing at zero).

use vike_indicators::indicators::{Ad, Adx, Obv, Psar, Roc, Stochrsi};
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

/// A bar around each close, its spread a few steps wide and varying, with a volume that varies.
fn bars_of(c: &[f64], scale: f64) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let up = scale * (0.3 + (i % 7) as f64 * 0.1);
            let dn = scale * (0.2 + (i % 5) as f64 * 0.15);
            let vol = 100.0 + (i % 13) as f64 * 17.0;
            Bar::ohlcv(i as i64 * 60_000, x, x + up, x - dn, x, vol)
        })
        .collect()
}

/// A series built to hurt a smoother: a flat stretch (zero ranges, zero changes), a zero-volume
/// stretch, a gap in each direction, one bar with no range that still moved, one that closes at zero.
fn awkward_bars() -> Vec<Bar> {
    let mut s: u64 = 0xD1B5_4A32_D192_ED03;
    let mut price = 100.0_f64;
    (0..1500usize)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let u = (s >> 11) as f64 / (1u64 << 53) as f64;
            if i == 900 {
                price *= 2.5;
            }
            if i == 1250 {
                price *= 0.3;
            }
            let flat = (300..340).contains(&i);
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

/// Parallel `high`, `low`, `close` and `volume` columns.
type Columns = (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>);

fn hlcv(b: &[Bar]) -> Columns {
    (
        b.iter().map(|x| x.high).collect(),
        b.iter().map(|x| x.low).collect(),
        b.iter().map(|x| x.close).collect(),
        b.iter().map(|x| x.volume).collect(),
    )
}

// ---------------------------------------------------------------- textbook references

fn roc_ref(c: &[f64], n: usize) -> Vec<f64> {
    (0..c.len())
        .map(
            |i| {
                if i < n || c[i - n] == 0.0 {
                    f64::NAN
                } else {
                    100.0 * (c[i] - c[i - n]) / c[i - n]
                }
            },
        )
        .collect()
}

fn obv_ref(c: &[f64], v: &[f64]) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len().min(v.len())];
    let mut acc = 0.0;
    for i in 0..out.len() {
        if i > 0 {
            if c[i] > c[i - 1] {
                acc += v[i];
            } else if c[i] < c[i - 1] {
                acc -= v[i];
            }
        }
        out[i] = acc;
    }
    out
}

fn ad_ref(h: &[f64], l: &[f64], c: &[f64], v: &[f64]) -> Vec<f64> {
    let mut acc = 0.0;
    (0..h.len().min(l.len()).min(c.len()).min(v.len()))
        .map(|i| {
            let rng = h[i] - l[i];
            let clv = if rng != 0.0 { ((c[i] - l[i]) - (h[i] - c[i])) / rng } else { 0.0 };
            acc += clv * v[i];
            acc
        })
        .collect()
}

fn psar_ref(h: &[f64], l: &[f64], step: f64, max_af: f64) -> Vec<f64> {
    let n = h.len().min(l.len());
    let mut out = vec![f64::NAN; n];
    if n == 0 {
        return out;
    }
    let (mut up, mut sar, mut ep, mut af) = (true, l[0], h[0], step);
    out[0] = sar;
    for i in 1..n {
        let mut s = sar + af * (ep - sar);
        if up {
            let l2 = if i >= 2 { l[i - 2] } else { l[i - 1] };
            s = s.min(l[i - 1]).min(l2);
            if l[i] < s {
                up = false;
                s = ep.max(h[i - 1]).max(h[i]);
                ep = l[i];
                af = step;
            } else if h[i] > ep {
                ep = h[i];
                af = (af + step).min(max_af);
            }
        } else {
            let h2 = if i >= 2 { h[i - 2] } else { h[i - 1] };
            s = s.max(h[i - 1]).max(h2);
            if h[i] > s {
                up = true;
                s = ep.min(l[i - 1]).min(l[i]);
                ep = h[i];
                af = step;
            } else if l[i] < ep {
                ep = l[i];
                af = (af + step).min(max_af);
            }
        }
        sar = s;
        out[i] = s;
    }
    out
}

fn adx_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> [Vec<f64>; 3] {
    let len = h.len().min(l.len()).min(c.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]];
    let pf = n as f64;
    let (mut atr, mut pdm_s, mut mdm_s) = (0.0, 0.0, 0.0);
    let mut dxs: Vec<f64> = Vec::new();
    let mut adx = f64::NAN;
    for i in 1..len {
        let up = h[i] - h[i - 1];
        let dn = l[i - 1] - l[i];
        let pdm = if up > dn && up > 0.0 { up } else { 0.0 };
        let mdm = if dn > up && dn > 0.0 { dn } else { 0.0 };
        let tr = (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs());
        if i < n {
            atr += tr;
            pdm_s += pdm;
            mdm_s += mdm;
            continue;
        }
        if i == n {
            atr += tr;
            pdm_s += pdm;
            mdm_s += mdm;
        } else {
            atr += tr - atr / pf;
            pdm_s += pdm - pdm_s / pf;
            mdm_s += mdm - mdm_s / pf;
        }
        let pdi = if atr > 0.0 { 100.0 * pdm_s / atr } else { 0.0 };
        let mdi = if atr > 0.0 { 100.0 * mdm_s / atr } else { 0.0 };
        let denom = pdi + mdi;
        let dx = if denom > 0.0 { 100.0 * (pdi - mdi).abs() / denom } else { 0.0 };
        dxs.push(dx);
        if dxs.len() == n {
            adx = dxs.iter().sum::<f64>() / pf;
            out[0][i] = adx;
        } else if dxs.len() > n {
            adx = (adx * (pf - 1.0) + dx) / pf;
            out[0][i] = adx;
        }
        out[1][i] = pdi;
        out[2][i] = mdi;
    }
    out
}

/// `[%K, %D]`, and per line which positions are ILL-CONDITIONED: a window whose RSI values differ by less
/// than 1e-6 (a flat stretch: the RSI is constant in exact arithmetic and varies by rounding noise
/// in floating point), where `%K` is a ratio of two noise terms and no two correct implementations
/// agree. `%D` is ill where any of the `%K` it averages is.
fn stochrsi_ref(c: &[f64], rp: usize, k: usize, d: usize) -> ([Vec<f64>; 2], [Vec<bool>; 2]) {
    let len = c.len();
    let pf = rp as f64;
    let mut rsi = vec![f64::NAN; len];
    let (mut gains, mut losses, mut ag, mut al) = (0.0, 0.0, 0.0, 0.0);
    let val = |ag: f64, al: f64| if al == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + ag / al) };
    for i in 1..len {
        let ch = c[i] - c[i - 1];
        let (g, l) = (ch.max(0.0), (-ch).max(0.0));
        if i < rp {
            gains += g;
            losses += l;
        } else if i == rp {
            gains += g;
            losses += l;
            ag = gains / pf;
            al = losses / pf;
            rsi[i] = val(ag, al);
        } else {
            ag = (ag * (pf - 1.0) + g) / pf;
            al = (al * (pf - 1.0) + l) / pf;
            rsi[i] = val(ag, al);
        }
    }
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let mut ill = [vec![false; len], vec![false; len]];
    let mut defined: Vec<(f64, bool)> = Vec::new();
    for i in 0..len {
        if i + 1 < k {
            continue;
        }
        let w = &rsi[i + 1 - k..=i];
        if w.iter().any(|v| v.is_nan()) {
            continue;
        }
        let hh = w.iter().copied().fold(f64::MIN, f64::max);
        let ll = w.iter().copied().fold(f64::MAX, f64::min);
        let rng = hh - ll;
        let kv = if rng != 0.0 { 100.0 * (rsi[i] - ll) / rng } else { 0.0 };
        out[0][i] = kv;
        ill[0][i] = rng < 1e-6;
        defined.push((kv, rng < 1e-6));
        if defined.len() >= d {
            let last = &defined[defined.len() - d..];
            out[1][i] = last.iter().map(|x| x.0).sum::<f64>() / d as f64;
            ill[1][i] = last.iter().any(|x| x.1);
        }
    }
    (out, ill)
}

// ---------------------------------------------------------------- comparison helpers

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

fn same(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

fn col<const N: usize>(rows: &[[f64; N]], k: usize) -> Vec<f64> {
    rows.iter().map(|r| r[k]).collect()
}

const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];

// ---------------------------------------------------------------- against the textbook

#[test]
fn roc_matches_the_textbook_ratio() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for n in [1usize, 2, 10, 20, 200] {
            assert_close(
                &format!("roc({n}) level {level}"),
                &series::roc(&c, n as f64).unwrap(),
                &roc_ref(&c, n),
                1e-12,
                1e-300,
            );
        }
    }
    let (_, _, c, _) = hlcv(&awkward_bars());
    for n in [1usize, 10, 100] {
        assert_close(
            &format!("roc({n}) awkward"),
            &series::roc(&c, n as f64).unwrap(),
            &roc_ref(&c, n),
            1e-12,
            1e-300,
        );
    }
}

#[test]
fn obv_and_ad_match_the_running_sums() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        let b = bars_of(&c, step);
        let (h, l, c, v) = hlcv(&b);
        assert_close("obv", &series::obv(&c, &v), &obv_ref(&c, &v), 1e-9, 1.0);
        assert_close("ad", &series::ad(&h, &l, &c, &v), &ad_ref(&h, &l, &c, &v), 1e-9, 1.0);
    }
    let (h, l, c, v) = hlcv(&awkward_bars());
    assert_close("obv awkward", &series::obv(&c, &v), &obv_ref(&c, &v), 1e-9, 1.0);
    assert_close("ad awkward", &series::ad(&h, &l, &c, &v), &ad_ref(&h, &l, &c, &v), 1e-9, 1.0);
}

/// Wilder's rule: a stop may not sit inside the range of the bar it is checked on or of the bar
/// before it. On a reversal bar that means the old extreme is lifted (or dropped) to the bar's
/// range; before this was enforced a bar that spiked above the extreme and fell through the stop
/// left its dot inside its own range (14 of 5,000 bars on the bench series).
#[test]
fn psar_never_sits_inside_the_range_of_its_own_or_the_previous_bar() {
    for (level, step) in SCALES {
        let c = closes(level, step, 5000);
        let (h, l, _, _) = hlcv(&bars_of(&c, step));
        for (s, m) in [(0.02, 0.2), (0.01, 0.1), (0.04, 0.4)] {
            let sar = series::psar(&h, &l, s, m).unwrap();
            for i in 2..sar.len() {
                let below = sar[i] <= l[i] && sar[i] <= l[i - 1];
                let above = sar[i] >= h[i] && sar[i] >= h[i - 1];
                assert!(
                    below || above,
                    "psar({s},{m}) level {level} bar {i}: stop {} inside low {} high {} (previous low {} high {})",
                    sar[i],
                    l[i],
                    h[i],
                    l[i - 1],
                    h[i - 1]
                );
            }
        }
    }
}

#[test]
fn psar_matches_wilders_stop_and_reverse() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        let (h, l, _, _) = hlcv(&bars_of(&c, step));
        for (s, m) in [(0.02, 0.2), (0.001, 0.05), (0.5, 1.0), (0.1, 0.3)] {
            assert_close(
                &format!("psar({s},{m}) level {level}"),
                &series::psar(&h, &l, s, m).unwrap(),
                &psar_ref(&h, &l, s, m),
                1e-9,
                1e-300,
            );
        }
    }
    let (h, l, _, _) = hlcv(&awkward_bars());
    for (s, m) in [(0.02, 0.2), (0.1, 0.3)] {
        assert_close(
            &format!("psar({s},{m}) awkward"),
            &series::psar(&h, &l, s, m).unwrap(),
            &psar_ref(&h, &l, s, m),
            1e-9,
            1e-300,
        );
    }
}

#[test]
fn adx_matches_wilders_directional_system() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        let (h, l, c, _) = hlcv(&bars_of(&c, step));
        for n in [2usize, 3, 14, 50, 100] {
            let got = series::adx(&h, &l, &c, n as f64).unwrap();
            let want = adx_ref(&h, &l, &c, n);
            for k in 0..3 {
                assert_close(
                    &format!("adx({n}) line {k} level {level}"),
                    &got[k],
                    &want[k],
                    1e-9,
                    1.0,
                );
            }
        }
    }
    let (h, l, c, _) = hlcv(&awkward_bars());
    for n in [2usize, 14, 100] {
        let got = series::adx(&h, &l, &c, n as f64).unwrap();
        let want = adx_ref(&h, &l, &c, n);
        for k in 0..3 {
            assert_close(&format!("adx({n}) line {k} awkward"), &got[k], &want[k], 1e-9, 1.0);
        }
    }
}

#[test]
fn stochrsi_matches_the_rsi_placed_in_its_own_range() {
    for (level, step) in SCALES {
        let c = closes(level, step, 4000);
        for (rp, k, d) in
            [(14usize, 14usize, 3usize), (2, 2, 1), (5, 3, 2), (20, 64, 10), (100, 100, 50)]
        {
            let got = series::stochrsi(&c, rp as f64, k as f64, d as f64).unwrap();
            let (want, _) = stochrsi_ref(&c, rp, k, d);
            for j in 0..2 {
                assert_close(
                    &format!("stochrsi({rp},{k},{d}) line {j} level {level}"),
                    &got[j],
                    &want[j],
                    1e-9,
                    1.0,
                );
            }
        }
    }
    let (_, _, c, _) = hlcv(&awkward_bars());
    for (rp, k, d) in [(14usize, 14usize, 3usize), (3, 5, 2), (50, 30, 7)] {
        let got = series::stochrsi(&c, rp as f64, k as f64, d as f64).unwrap();
        let (want, ill) = stochrsi_ref(&c, rp, k, d);
        for j in 0..2 {
            let tag = format!("stochrsi({rp},{k},{d}) line {j} awkward");
            // Where the reference is well conditioned the two agree; where it is not, the core's
            // value is still a number inside the line's own range.
            for i in 0..c.len() {
                if ill[j][i] {
                    assert!(
                        (-1e-9..=100.0 + 1e-9).contains(&got[j][i]),
                        "{tag}[{i}]: ill-conditioned, got {}",
                        got[j][i]
                    );
                } else {
                    assert_close(
                        &format!("{tag}[{i}]"),
                        &got[j][i..=i],
                        &want[j][i..=i],
                        1e-9,
                        1.0,
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------- the ways in agree to the bit

/// A series of `len` closes with a NaN travelling through it, and the bars around it.
fn input(len: usize, with_nan: bool) -> (Vec<f64>, Vec<Bar>) {
    let mut c = closes(100.0, 2.0, len);
    if with_nan && len > 40 {
        c[len / 3] = f64::NAN;
    }
    let b = bars_of(&c, 2.0);
    (c, b)
}

#[test]
fn roc_every_way_in_gives_the_same_bits() {
    for n in [1usize, 2, 7, 20, 64, 511, 512, 513, 1000] {
        for len in [0usize, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600] {
            for with_nan in [false, true] {
                let (c, b) = input(len, with_nan);
                let nf = n as f64;
                let mut ind = Roc::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Roc::new(nf).unwrap();
                let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
                assert!(same(&stream, &value), "roc n={n} len={len}: next vs next_value");
                assert!(same(&stream, &Roc::new(nf).unwrap().vectorize(&b)[0]), "roc vectorize");
                assert!(same(&stream, &series::roc(&c, nf).unwrap()), "roc series n={n} len={len}");
            }
        }
    }
}

#[test]
fn obv_and_ad_every_way_in_gives_the_same_bits() {
    for len in [0usize, 1, 2, 3, 20, 513, 2600] {
        for with_nan in [false, true] {
            let (_, mut b) = input(len, with_nan);
            if with_nan && len > 100 {
                b[len / 2].volume = f64::NAN;
            }
            let (h, l, c2, v) = hlcv(&b);

            let mut ind = Obv::default();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Obv::default();
            let value: Vec<f64> = b.iter().map(|x| ind.next_cv(x.close, x.volume)).collect();
            assert!(same(&stream, &value), "obv len={len}: next vs next_cv");
            assert!(same(&stream, &Obv::default().vectorize(&b)[0]), "obv vectorize len={len}");
            assert!(same(&stream, &series::obv(&c2, &v)), "obv series len={len}");

            let mut ind = Ad::default();
            let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
            let mut ind = Ad::default();
            let value: Vec<f64> =
                b.iter().map(|x| ind.next_hlcv(x.high, x.low, x.close, x.volume)).collect();
            assert!(same(&stream, &value), "ad len={len}: next vs next_hlcv");
            assert!(same(&stream, &Ad::default().vectorize(&b)[0]), "ad vectorize len={len}");
            assert!(same(&stream, &series::ad(&h, &l, &c2, &v)), "ad series len={len}");
        }
    }
}

#[test]
fn psar_every_way_in_gives_the_same_bits() {
    for (s, m) in [(0.02, 0.2), (0.001, 0.05), (0.5, 1.0), (0.1, 0.3)] {
        for len in [0usize, 1, 2, 3, 10, 100, 2600] {
            for with_nan in [false, true] {
                let (_, b) = input(len, with_nan);
                let (h, l, _, _) = hlcv(&b);
                let mut ind = Psar::new(s, m).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Psar::new(s, m).unwrap();
                let value: Vec<f64> = b.iter().map(|x| ind.next_hl(x.high, x.low)).collect();
                assert!(same(&stream, &value), "psar({s},{m}) len={len}: next vs next_hl");
                assert!(
                    same(&stream, &Psar::new(s, m).unwrap().vectorize(&b)[0]),
                    "psar vectorize"
                );
                assert!(same(&stream, &series::psar(&h, &l, s, m).unwrap()), "psar series");
            }
        }
    }
}

#[test]
fn adx_every_way_in_gives_the_same_bits() {
    for n in [2usize, 3, 7, 14, 20, 64, 100] {
        for len in [0usize, 1, n, 2 * n - 1, 2 * n, 2 * n + 1, 3 * n + 1, 2600] {
            for with_nan in [false, true] {
                let (_, b) = input(len, with_nan);
                let (h, l, c, _) = hlcv(&b);
                let nf = n as f64;
                let mut ind = Adx::new(nf).unwrap();
                let stream: Vec<[f64; 3]> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Adx::new(nf).unwrap();
                let value: Vec<[f64; 3]> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                let batch = Adx::new(nf).unwrap().vectorize(&b);
                let sl = series::adx(&h, &l, &c, nf).unwrap();
                for k in 0..3 {
                    let s = col(&stream, k);
                    assert!(same(&s, &col(&value, k)), "adx n={n} len={len} line {k}: next_hlc");
                    assert!(same(&s, &batch[k]), "adx n={n} len={len} line {k}: vectorize");
                    assert!(same(&s, &sl[k]), "adx n={n} len={len} line {k}: series");
                }
            }
        }
    }
}

#[test]
fn stochrsi_every_way_in_gives_the_same_bits() {
    for (rp, k, d) in
        [(14usize, 14usize, 3usize), (2, 2, 1), (5, 3, 2), (20, 64, 10), (100, 100, 50)]
    {
        for len in [0usize, 1, rp, rp + k - 1, rp + k, rp + k + d, 3 * (rp + k) + 1, 2600] {
            for with_nan in [false, true] {
                let (c, b) = input(len, with_nan);
                let (rpf, kf, df) = (rp as f64, k as f64, d as f64);
                let mut ind = Stochrsi::new(rpf, kf, df).unwrap();
                let stream: Vec<[f64; 2]> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Stochrsi::new(rpf, kf, df).unwrap();
                let value: Vec<[f64; 2]> = c.iter().map(|&x| ind.next_value(x)).collect();
                let batch = Stochrsi::new(rpf, kf, df).unwrap().vectorize(&b);
                let sl = series::stochrsi(&c, rpf, kf, df).unwrap();
                for j in 0..2 {
                    let s = col(&stream, j);
                    let tag = format!("stochrsi({rp},{k},{d}) len={len} line {j}");
                    assert!(same(&s, &col(&value, j)), "{tag}: next_value");
                    assert!(same(&s, &batch[j]), "{tag}: vectorize");
                    assert!(same(&s, &sl[j]), "{tag}: series");
                }
            }
        }
    }
}

#[test]
fn a_reset_stepper_repeats_itself() {
    let (_, b) = input(700, true);
    macro_rules! repeats {
        ($ind:expr) => {{
            let mut ind = $ind;
            let first: Vec<Vec<f64>> = b.iter().map(|x| ind.on_bar(x)).collect();
            ind.reset();
            let second: Vec<Vec<f64>> = b.iter().map(|x| ind.on_bar(x)).collect();
            for (x, y) in first.iter().zip(&second) {
                assert!(same(x, y), "{}: a reset stepper differs", ind.name());
            }
        }};
    }
    repeats!(Roc::new(10.0).unwrap());
    repeats!(Obv::default());
    repeats!(Psar::new(0.02, 0.2).unwrap());
    repeats!(Ad::default());
    repeats!(Adx::new(14.0).unwrap());
    repeats!(Stochrsi::new(14.0, 14.0, 3.0).unwrap());
}

// ---------------------------------------------------------------- the measured error

/// The largest relative error (denominator floored at `floor`) of `got` against `want`, ignoring
/// positions where either is NaN.
fn max_rel(got: &[f64], want: &[f64], floor: f64) -> f64 {
    got.iter()
        .zip(want)
        .filter(|(g, w)| !g.is_nan() && !w.is_nan())
        .map(|(g, w)| (g - w).abs() / w.abs().max(floor))
        .fold(0.0, f64::max)
}

/// Prints the worst relative error of each fast core against its textbook reference over the three
/// scales and the awkward series. `cargo test -p vike-indicators --release --test fast_c -- --ignored --nocapture`
#[test]
#[ignore = "prints a measurement, asserts nothing"]
fn report_the_worst_relative_error() {
    let mut worst = [0.0_f64; 6];
    let mut seen = |k: usize, e: f64| worst[k] = worst[k].max(e);
    let mut series: Vec<Columns> = Vec::new();
    for (level, step) in SCALES {
        series.push(hlcv(&bars_of(&closes(level, step, 20_000), step)));
    }
    series.push(hlcv(&awkward_bars()));
    for (h, l, c, v) in &series {
        for n in [1usize, 2, 10, 20, 200] {
            seen(0, max_rel(&series::roc(c, n as f64).unwrap(), &roc_ref(c, n), 1e-300));
        }
        seen(1, max_rel(&series::obv(c, v), &obv_ref(c, v), 1.0));
        for (s, m) in [(0.02, 0.2), (0.001, 0.05), (0.5, 1.0), (0.1, 0.3)] {
            seen(2, max_rel(&series::psar(h, l, s, m).unwrap(), &psar_ref(h, l, s, m), 1e-300));
        }
        seen(3, max_rel(&series::ad(h, l, c, v), &ad_ref(h, l, c, v), 1.0));
        for n in [2usize, 3, 14, 50, 100] {
            let (got, want) = (series::adx(h, l, c, n as f64).unwrap(), adx_ref(h, l, c, n));
            for k in 0..3 {
                seen(4, max_rel(&got[k], &want[k], 1.0));
            }
        }
        for (rp, k, d) in [(14usize, 14usize, 3usize), (5, 3, 2), (20, 64, 10), (100, 100, 50)] {
            let got = series::stochrsi(c, rp as f64, k as f64, d as f64).unwrap();
            let (want, ill) = stochrsi_ref(c, rp, k, d);
            for j in 0..2 {
                // Only where the reference is well conditioned (see `stochrsi_ref`).
                let (g, w): (Vec<f64>, Vec<f64>) =
                    (0..c.len()).filter(|&i| !ill[j][i]).map(|i| (got[j][i], want[j][i])).unzip();
                seen(5, max_rel(&g, &w, 1.0));
            }
        }
    }
    for (name, e) in ["roc", "obv", "psar", "ad", "adx", "stochrsi"].iter().zip(worst) {
        println!("max relative error {name:9} {e:.3e}");
    }
}

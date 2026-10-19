//! `dx`, `plus_di`, `minus_di`, `plus_dm`, `minus_dm`, `chaikin_volatility`, `chandelier`, `starc`,
//! `smma`, `supertrend`, `adxr` and `natr`: the fast cores against a textbook reference, and the
//! stream (`next`), the plain-number entry (`next_hlc`, `next_hl`, `next_value`), the batch
//! (`vectorize`) and the slice call (`series::*`) against each other, bit for bit.
//!
//! The references do each step the long way (divisions, windows folded from scratch, the
//! simple-average seeds). The cores multiply by a precomputed `1/n` or `(n-1)/n`, so the last bits
//! differ; the tolerance is relative 1e-9 on three series (level 100 with a step of 2, level 1e6 with
//! a step of 0.01, level 1e-3 with a step of 1e-6) and on one built to hurt (a flat stretch, gaps, a
//! bar with no range that still moved, a bar closing at zero).

use vike_indicators::indicators::{
    Adx, Adxr, ChaikinVolatility, Chandelier, Dx, MinusDi, MinusDm, Natr, PlusDi, PlusDm, Smma,
    Starc, Supertrend,
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

/// A bar around each close, its spread a few steps wide and varying.
fn bars_of(c: &[f64], scale: f64) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let up = scale * (0.3 + (i % 7) as f64 * 0.1);
            let dn = scale * (0.2 + (i % 5) as f64 * 0.15);
            Bar::ohlcv(i as i64 * 60_000, x, x + up, x - dn, x, 100.0)
        })
        .collect()
}

/// A series built to hurt a smoother: a flat stretch (zero ranges, zero changes), a gap in each
/// direction, one bar with no range that still moved, one that closes at zero.
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
            Bar::ohlcv(i as i64 * 60_000, price, high, low, close, 500.0 + u * 1000.0)
        })
        .collect()
}

/// Parallel `high`, `low` and `close` columns.
type Columns = (Vec<f64>, Vec<f64>, Vec<f64>);

fn hlc(b: &[Bar]) -> Columns {
    (
        b.iter().map(|x| x.high).collect(),
        b.iter().map(|x| x.low).collect(),
        b.iter().map(|x| x.close).collect(),
    )
}

// ---------------------------------------------------------------- textbook references

/// The true range, the long way: the largest of the three distances (the first bar has no previous
/// close, so it is the high-low range).
fn tr_ref(h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
    (0..h.len())
        .map(|i| {
            let mut tr = h[i] - l[i];
            if i > 0 {
                tr = tr.max((h[i] - c[i - 1]).abs());
                tr = tr.max((l[i] - c[i - 1]).abs());
            }
            tr
        })
        .collect()
}

/// Wilder's smoothing of `x[start..]`: the mean of the first `n` values, then `(prev * (n-1) + x) / n`.
fn rma_ref(x: &[f64], start: usize, n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    if n == 0 || x.len() < start + n {
        return out;
    }
    let nf = n as f64;
    let mut prev = x[start..start + n].iter().sum::<f64>() / nf;
    out[start + n - 1] = prev;
    for i in start + n..x.len() {
        prev = (prev * (nf - 1.0) + x[i]) / nf;
        out[i] = prev;
    }
    out
}

/// An exponential moving average of `x`: the mean of the first `n` values, then `alpha x + (1-alpha) prev`.
fn ema_ref(x: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    if n == 0 || x.len() < n {
        return out;
    }
    let alpha = 2.0 / (n as f64 + 1.0);
    let mut prev = x[..n].iter().sum::<f64>() / n as f64;
    out[n - 1] = prev;
    for i in n..x.len() {
        prev = alpha * x[i] + (1.0 - alpha) * prev;
        out[i] = prev;
    }
    out
}

/// Wilder's ATR. With `skip` the first bar has no range and the ATR starts at bar `n` (natr,
/// supertrend); without it the first range is `high - low` and the ATR starts at bar `n - 1`.
fn atr_ref(h: &[f64], l: &[f64], c: &[f64], n: usize, skip: bool) -> Vec<f64> {
    rma_ref(&tr_ref(h, l, c), usize::from(skip), n)
}

fn smma_ref(c: &[f64], n: usize) -> Vec<f64> {
    rma_ref(c, 0, n)
}

fn natr_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<f64> {
    let atr = atr_ref(h, l, c, n, true);
    (0..c.len()).map(|i| if c[i] != 0.0 { 100.0 * atr[i] / c[i] } else { f64::NAN }).collect()
}

fn chandelier_ref(h: &[f64], l: &[f64], c: &[f64], n: usize, mult: f64) -> [Vec<f64>; 2] {
    let atr = atr_ref(h, l, c, n, false);
    let mut out = [vec![f64::NAN; h.len()], vec![f64::NAN; h.len()]];
    for i in 0..h.len() {
        if i + 1 < n || atr[i].is_nan() {
            continue;
        }
        let hh = h[i + 1 - n..=i].iter().copied().fold(f64::MIN, f64::max);
        let ll = l[i + 1 - n..=i].iter().copied().fold(f64::MAX, f64::min);
        out[0][i] = hh - mult * atr[i];
        out[1][i] = ll + mult * atr[i];
    }
    out
}

fn starc_ref(h: &[f64], l: &[f64], c: &[f64], sn: usize, an: usize, mult: f64) -> [Vec<f64>; 3] {
    let atr = atr_ref(h, l, c, an, false);
    let mut out = [vec![f64::NAN; h.len()], vec![f64::NAN; h.len()], vec![f64::NAN; h.len()]];
    for i in 0..h.len() {
        if i + 1 < sn || atr[i].is_nan() {
            continue;
        }
        let mid = c[i + 1 - sn..=i].iter().sum::<f64>() / sn as f64;
        out[0][i] = mid + mult * atr[i];
        out[1][i] = mid;
        out[2][i] = mid - mult * atr[i];
    }
    out
}

fn chaikin_ref(h: &[f64], l: &[f64], en: usize, rn: usize) -> Vec<f64> {
    let range: Vec<f64> = h.iter().zip(l).map(|(a, b)| a - b).collect();
    let ema = ema_ref(&range, en);
    (0..h.len())
        .map(|i| {
            if i < rn.max(1) {
                return f64::NAN;
            }
            let (now, then) = (ema[i], ema[i - rn]);
            if now.is_nan() || then.is_nan() || then == 0.0 {
                f64::NAN
            } else {
                (now - then) / then * 100.0
            }
        })
        .collect()
}

/// The three Wilder sums of the directional movement system, `(true range, +DM, -DM)` per bar: bars
/// `1..=n` are summed plainly, later bars decay by `x += new - x / n`. `NaN` before bar `n`.
fn dm_sums_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<(f64, f64, f64)> {
    let len = h.len();
    let mut out = vec![(f64::NAN, f64::NAN, f64::NAN); len];
    let pf = n as f64;
    let (mut atr, mut pdm_s, mut mdm_s) = (0.0, 0.0, 0.0);
    for i in 1..len {
        let up = h[i] - h[i - 1];
        let dn = l[i - 1] - l[i];
        let pdm = if up > dn && up > 0.0 { up } else { 0.0 };
        let mdm = if dn > up && dn > 0.0 { dn } else { 0.0 };
        let tr = (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs());
        if i <= n {
            atr += tr;
            pdm_s += pdm;
            mdm_s += mdm;
        } else {
            atr += tr - atr / pf;
            pdm_s += pdm - pdm_s / pf;
            mdm_s += mdm - mdm_s / pf;
        }
        if i >= n {
            out[i] = (atr, pdm_s, mdm_s);
        }
    }
    out
}

/// `(+DI, -DI, DX)` from the sums, the textbook way.
fn dm_lines(atr: f64, p: f64, m: f64) -> (f64, f64, f64) {
    let pdi = if atr > 0.0 { 100.0 * p / atr } else { 0.0 };
    let mdi = if atr > 0.0 { 100.0 * m / atr } else { 0.0 };
    let denom = pdi + mdi;
    let dx = if denom > 0.0 { 100.0 * (pdi - mdi).abs() / denom } else { 0.0 };
    (pdi, mdi, dx)
}

/// One column of the system: 0 = +DI, 1 = -DI, 2 = DX, 3 = +DM, 4 = -DM.
fn dm_ref(h: &[f64], l: &[f64], c: &[f64], n: usize, which: usize) -> Vec<f64> {
    dm_sums_ref(h, l, c, n)
        .into_iter()
        .map(|(a, p, m)| {
            if a.is_nan() {
                return f64::NAN;
            }
            let (pdi, mdi, dx) = dm_lines(a, p, m);
            [pdi, mdi, dx, p, m][which]
        })
        .collect()
}

/// ADX, the long way: the mean of the first `n` DX values, then Wilder's smoothing of DX.
fn adx_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; h.len()];
    let pf = n as f64;
    let mut dxs: Vec<f64> = Vec::new();
    let mut adx = f64::NAN;
    for (i, (a, p, m)) in dm_sums_ref(h, l, c, n).into_iter().enumerate() {
        if a.is_nan() {
            continue;
        }
        let (_, _, dx) = dm_lines(a, p, m);
        dxs.push(dx);
        if dxs.len() == n {
            adx = dxs.iter().sum::<f64>() / pf;
            out[i] = adx;
        } else if dxs.len() > n {
            adx = (adx * (pf - 1.0) + dx) / pf;
            out[i] = adx;
        }
    }
    out
}

fn adxr_ref(h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<f64> {
    let adx = adx_ref(h, l, c, n);
    (0..h.len())
        .map(|i| {
            if i >= n && !adx[i].is_nan() && !adx[i - n].is_nan() {
                (adx[i] + adx[i - n]) / 2.0
            } else {
                f64::NAN
            }
        })
        .collect()
}

/// Supertrend, the textbook way: the bands seeded at the first bar with an ATR, then the carry rules.
fn supertrend_ref(h: &[f64], l: &[f64], c: &[f64], n: usize, mult: f64) -> [Vec<f64>; 2] {
    let atr = atr_ref(h, l, c, n, true);
    let mut out = [vec![f64::NAN; h.len()], vec![f64::NAN; h.len()]];
    let mut started = false;
    let (mut fu, mut fl, mut dir) = (f64::NAN, f64::NAN, f64::NAN);
    for i in 0..h.len() {
        let a = atr[i];
        if a.is_nan() {
            continue;
        }
        let hl2 = (h[i] + l[i]) / 2.0;
        if !started {
            started = true;
            fu = hl2 + mult * a;
            fl = hl2 - mult * a;
            dir = if c[i] >= hl2 { 1.0 } else { -1.0 };
        } else {
            let (bu, bl, pc) = (hl2 + mult * a, hl2 - mult * a, c[i - 1]);
            fu = if pc <= fu { bu.min(fu) } else { bu };
            fl = if pc >= fl { bl.max(fl) } else { bl };
            if dir == 1.0 {
                if c[i] < fl {
                    dir = -1.0;
                }
            } else if c[i] > fu {
                dir = 1.0;
            }
        }
        out[0][i] = if dir == 1.0 { fl } else { fu };
        out[1][i] = dir;
    }
    out
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

fn cols<const N: usize>(rows: &[[f64; N]]) -> Vec<Vec<f64>> {
    (0..N).map(|k| col(rows, k)).collect()
}

const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];

/// The four series every reference test runs on: the three scales and the awkward one.
fn all_series() -> Vec<(String, Columns)> {
    let mut v: Vec<(String, Columns)> = SCALES
        .iter()
        .map(|&(level, step)| {
            (format!("level {level}"), hlc(&bars_of(&closes(level, step, 4000), step)))
        })
        .collect();
    v.push(("awkward".to_string(), hlc(&awkward_bars())));
    v
}

// ---------------------------------------------------------------- against the textbook

#[test]
fn the_directional_family_matches_wilders_sums() {
    for (tag, (h, l, c)) in all_series() {
        for n in [2usize, 3, 14, 50, 100] {
            let nf = n as f64;
            let got = [
                series::plus_di(&h, &l, &c, nf).unwrap(),
                series::minus_di(&h, &l, &c, nf).unwrap(),
                series::dx(&h, &l, &c, nf).unwrap(),
                series::plus_dm(&h, &l, nf).unwrap(),
                series::minus_dm(&h, &l, nf).unwrap(),
            ];
            for (k, name) in ["plus_di", "minus_di", "dx", "plus_dm", "minus_dm"].iter().enumerate()
            {
                // The DI lines and DX are percentages (floor 1); the DM sums are in price units.
                let floor = if k < 3 { 1.0 } else { 1e-300 };
                assert_close(
                    &format!("{name}({n}) {tag}"),
                    &got[k],
                    &dm_ref(&h, &l, &c, n, k),
                    1e-9,
                    floor,
                );
            }
        }
    }
}

#[test]
fn plus_di_and_minus_di_are_the_adx_lines_to_the_bit() {
    for (tag, (h, l, c)) in all_series() {
        for n in [2usize, 14, 100] {
            let adx = series::adx(&h, &l, &c, n as f64).unwrap();
            let pdi = series::plus_di(&h, &l, &c, n as f64).unwrap();
            let mdi = series::minus_di(&h, &l, &c, n as f64).unwrap();
            assert!(same(&pdi, &adx[1]), "plus_di({n}) {tag} differs from adx's plus_di");
            assert!(same(&mdi, &adx[2]), "minus_di({n}) {tag} differs from adx's minus_di");
        }
    }
}

#[test]
fn chaikin_volatility_matches_the_roc_of_the_ema_of_the_range() {
    for (tag, (h, l, _)) in all_series() {
        for (en, rn) in [(2usize, 1usize), (10, 10), (5, 3), (30, 20), (100, 100), (2, 100)] {
            assert_close(
                &format!("chaikin_volatility({en},{rn}) {tag}"),
                &series::chaikin_volatility(&h, &l, en as f64, rn as f64).unwrap(),
                &chaikin_ref(&h, &l, en, rn),
                1e-9,
                1.0,
            );
        }
    }
}

#[test]
fn chandelier_matches_the_extremes_less_atrs() {
    for (tag, (h, l, c)) in all_series() {
        for (n, mult) in [(2usize, 0.5), (22, 3.0), (10, 1.5), (60, 5.0), (200, 10.0)] {
            let got = series::chandelier(&h, &l, &c, n as f64, mult).unwrap();
            let want = chandelier_ref(&h, &l, &c, n, mult);
            for k in 0..2 {
                assert_close(
                    &format!("chandelier({n},{mult}) {tag} line {k}"),
                    &got[k],
                    &want[k],
                    1e-9,
                    1e-300,
                );
            }
        }
    }
}

#[test]
fn starc_matches_the_mean_and_atr_bands() {
    for (tag, (h, l, c)) in all_series() {
        for (sn, an, mult) in
            [(1usize, 1usize, 0.5), (6, 15, 2.0), (3, 2, 1.0), (50, 30, 5.0), (200, 200, 10.0)]
        {
            let got = series::starc(&h, &l, &c, sn as f64, an as f64, mult).unwrap();
            let want = starc_ref(&h, &l, &c, sn, an, mult);
            for k in 0..3 {
                assert_close(
                    &format!("starc({sn},{an},{mult}) {tag} line {k}"),
                    &got[k],
                    &want[k],
                    1e-9,
                    1e-300,
                );
            }
        }
    }
}

#[test]
fn smma_and_natr_match_wilders_smoothing() {
    for (tag, (h, l, c)) in all_series() {
        for n in [2usize, 3, 14, 100, 400] {
            assert_close(
                &format!("smma({n}) {tag}"),
                &series::smma(&c, n as f64).unwrap(),
                &smma_ref(&c, n),
                1e-9,
                1e-300,
            );
        }
        for n in [2usize, 3, 14, 50, 100] {
            assert_close(
                &format!("natr({n}) {tag}"),
                &series::natr(&h, &l, &c, n as f64).unwrap(),
                &natr_ref(&h, &l, &c, n),
                1e-9,
                1.0,
            );
        }
    }
}

#[test]
fn supertrend_matches_the_carried_bands() {
    for (tag, (h, l, c)) in all_series() {
        for (n, mult) in [(1usize, 0.5), (10, 3.0), (3, 1.5), (20, 3.0), (100, 10.0), (7, 10.0)] {
            let got = series::supertrend(&h, &l, &c, n as f64, mult).unwrap();
            let want = supertrend_ref(&h, &l, &c, n, mult);
            for k in 0..2 {
                assert_close(
                    &format!("supertrend({n},{mult}) {tag} line {k}"),
                    &got[k],
                    &want[k],
                    1e-9,
                    1e-300,
                );
            }
        }
    }
}

#[test]
fn adxr_is_the_mean_of_adx_and_adx_a_period_ago() {
    for (tag, (h, l, c)) in all_series() {
        for n in [2usize, 3, 14, 50, 100] {
            let got = series::adxr(&h, &l, &c, n as f64).unwrap();
            assert_close(&format!("adxr({n}) {tag}"), &got, &adxr_ref(&h, &l, &c, n), 1e-9, 1.0);
            // The same number built from the slice call of `adx`: the core is the one `adx` runs.
            let adx = series::adx(&h, &l, &c, n as f64).unwrap();
            let by_hand: Vec<f64> = (0..h.len())
                .map(|i| if i >= n { (adx[0][i] + adx[0][i - n]) * 0.5 } else { f64::NAN })
                .collect();
            assert!(same(&got, &by_hand), "adxr({n}) {tag} is not (adx + adx[-n]) / 2 to the bit");
        }
    }
}

// ---------------------------------------------------------------- the ways in agree to the bit

/// `len` bars with a NaN travelling through them: `mode` 0 none, 1 a whole bar, 2 only a close, 3 only
/// a high.
fn input(len: usize, mode: u8) -> Vec<Bar> {
    let mut c = closes(100.0, 2.0, len);
    if mode == 1 && len > 40 {
        c[len / 3] = f64::NAN;
    }
    let mut b = bars_of(&c, 2.0);
    if mode == 2 && len > 40 {
        b[len / 2].close = f64::NAN;
    }
    if mode == 3 && len > 40 {
        b[len / 2].high = f64::NAN;
    }
    b
}

const MODES: [u8; 4] = [0, 1, 2, 3];

fn lengths(n: usize) -> [usize; 8] {
    [0, 1, n.saturating_sub(1), n, n + 1, 3 * n, 3 * n + 1, 2600]
}

/// Every way in must hold the same bits.
fn agree(tag: &str, stream: &[Vec<f64>], value: &[Vec<f64>], batch: &[Vec<f64>], sl: &[Vec<f64>]) {
    for k in 0..stream.len() {
        assert!(same(&stream[k], &value[k]), "{tag} line {k}: next vs next_*");
        assert!(same(&stream[k], &batch[k]), "{tag} line {k}: vectorize");
        assert!(same(&stream[k], &sl[k]), "{tag} line {k}: series");
    }
}

#[test]
fn the_directional_family_every_way_in_gives_the_same_bits() {
    for n in [2usize, 3, 7, 14, 20, 64, 100] {
        for len in lengths(n) {
            for mode in MODES {
                let b = input(len, mode);
                let (h, l, c) = hlc(&b);
                let nf = n as f64;
                let tag = format!("n={n} len={len} mode={mode}");

                let mut ind = Dx::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Dx::new(nf).unwrap();
                let value: Vec<f64> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("dx {tag}"),
                    &[stream],
                    &[value],
                    &Dx::new(nf).unwrap().vectorize(&b),
                    &[series::dx(&h, &l, &c, nf).unwrap()],
                );

                let mut ind = PlusDi::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = PlusDi::new(nf).unwrap();
                let value: Vec<f64> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("plus_di {tag}"),
                    &[stream],
                    &[value],
                    &PlusDi::new(nf).unwrap().vectorize(&b),
                    &[series::plus_di(&h, &l, &c, nf).unwrap()],
                );

                let mut ind = MinusDi::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = MinusDi::new(nf).unwrap();
                let value: Vec<f64> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("minus_di {tag}"),
                    &[stream],
                    &[value],
                    &MinusDi::new(nf).unwrap().vectorize(&b),
                    &[series::minus_di(&h, &l, &c, nf).unwrap()],
                );

                let mut ind = PlusDm::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = PlusDm::new(nf).unwrap();
                let value: Vec<f64> = b.iter().map(|x| ind.next_hl(x.high, x.low)).collect();
                agree(
                    &format!("plus_dm {tag}"),
                    &[stream],
                    &[value],
                    &PlusDm::new(nf).unwrap().vectorize(&b),
                    &[series::plus_dm(&h, &l, nf).unwrap()],
                );

                let mut ind = MinusDm::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = MinusDm::new(nf).unwrap();
                let value: Vec<f64> = b.iter().map(|x| ind.next_hl(x.high, x.low)).collect();
                agree(
                    &format!("minus_dm {tag}"),
                    &[stream],
                    &[value],
                    &MinusDm::new(nf).unwrap().vectorize(&b),
                    &[series::minus_dm(&h, &l, nf).unwrap()],
                );
            }
        }
    }
}

#[test]
fn adxr_and_natr_every_way_in_gives_the_same_bits() {
    for n in [2usize, 3, 7, 14, 20, 64, 100] {
        for len in lengths(n).into_iter().chain([3 * n - 1, 3 * n + 2]) {
            for mode in MODES {
                let b = input(len, mode);
                let (h, l, c) = hlc(&b);
                let nf = n as f64;
                let tag = format!("n={n} len={len} mode={mode}");

                let mut ind = Adxr::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Adxr::new(nf).unwrap();
                let value: Vec<f64> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("adxr {tag}"),
                    &[stream],
                    &[value],
                    &Adxr::new(nf).unwrap().vectorize(&b),
                    &[series::adxr(&h, &l, &c, nf).unwrap()],
                );

                let mut ind = Natr::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Natr::new(nf).unwrap();
                let value: Vec<f64> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("natr {tag}"),
                    &[stream],
                    &[value],
                    &Natr::new(nf).unwrap().vectorize(&b),
                    &[series::natr(&h, &l, &c, nf).unwrap()],
                );
            }
        }
    }
}

#[test]
fn smma_every_way_in_gives_the_same_bits() {
    for n in [2usize, 3, 7, 14, 20, 64, 100, 400] {
        for len in lengths(n) {
            for mode in MODES {
                let b = input(len, mode);
                let (_, _, c) = hlc(&b);
                let nf = n as f64;
                let mut ind = Smma::new(nf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Smma::new(nf).unwrap();
                let value: Vec<f64> = c.iter().map(|&x| ind.next_value(x)).collect();
                agree(
                    &format!("smma n={n} len={len} mode={mode}"),
                    &[stream],
                    &[value],
                    &Smma::new(nf).unwrap().vectorize(&b),
                    &[series::smma(&c, nf).unwrap()],
                );
            }
        }
    }
}

#[test]
fn chaikin_volatility_every_way_in_gives_the_same_bits() {
    for (en, rn) in [(2usize, 1usize), (10, 10), (5, 3), (30, 20), (64, 64), (100, 100), (2, 100)] {
        for len in lengths(en + rn) {
            for mode in MODES {
                let b = input(len, mode);
                let (h, l, _) = hlc(&b);
                let (ef, rf) = (en as f64, rn as f64);
                let mut ind = ChaikinVolatility::new(ef, rf).unwrap();
                let stream: Vec<f64> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = ChaikinVolatility::new(ef, rf).unwrap();
                let value: Vec<f64> = b.iter().map(|x| ind.next_hl(x.high, x.low)).collect();
                agree(
                    &format!("chaikin_volatility({en},{rn}) len={len} mode={mode}"),
                    &[stream],
                    &[value],
                    &ChaikinVolatility::new(ef, rf).unwrap().vectorize(&b),
                    &[series::chaikin_volatility(&h, &l, ef, rf).unwrap()],
                );
            }
        }
    }
}

#[test]
fn chandelier_and_starc_every_way_in_give_the_same_bits() {
    for (n, mult) in [(2usize, 0.5), (3, 1.0), (7, 2.5), (22, 3.0), (64, 3.0), (200, 10.0)] {
        for len in lengths(n) {
            for mode in MODES {
                let b = input(len, mode);
                let (h, l, c) = hlc(&b);
                let nf = n as f64;
                let tag = format!("n={n} len={len} mode={mode}");

                let mut ind = Chandelier::new(nf, mult).unwrap();
                let stream: Vec<[f64; 2]> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Chandelier::new(nf, mult).unwrap();
                let value: Vec<[f64; 2]> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("chandelier {tag}"),
                    &cols(&stream),
                    &cols(&value),
                    &Chandelier::new(nf, mult).unwrap().vectorize(&b),
                    &series::chandelier(&h, &l, &c, nf, mult).unwrap(),
                );
            }
        }
    }
    for (sn, an, mult) in [
        (1usize, 1usize, 0.5),
        (6, 15, 2.0),
        (3, 2, 1.0),
        (20, 20, 3.0),
        (64, 20, 2.0),
        (200, 200, 10.0),
    ] {
        for len in lengths(sn.max(an)) {
            for mode in MODES {
                let b = input(len, mode);
                let (h, l, c) = hlc(&b);
                let (sf, af) = (sn as f64, an as f64);
                let tag = format!("({sn},{an}) len={len} mode={mode}");
                let mut ind = Starc::new(sf, af, mult).unwrap();
                let stream: Vec<[f64; 3]> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Starc::new(sf, af, mult).unwrap();
                let value: Vec<[f64; 3]> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("starc {tag}"),
                    &cols(&stream),
                    &cols(&value),
                    &Starc::new(sf, af, mult).unwrap().vectorize(&b),
                    &series::starc(&h, &l, &c, sf, af, mult).unwrap(),
                );
            }
        }
    }
}

#[test]
fn supertrend_every_way_in_gives_the_same_bits() {
    for (n, mult) in
        [(1usize, 0.5), (2, 1.0), (3, 1.5), (10, 3.0), (20, 3.0), (64, 3.0), (100, 10.0)]
    {
        for len in lengths(n) {
            for mode in MODES {
                let b = input(len, mode);
                let (h, l, c) = hlc(&b);
                let nf = n as f64;
                let mut ind = Supertrend::new(nf, mult).unwrap();
                let stream: Vec<[f64; 2]> = b.iter().map(|x| ind.next(x)).collect();
                let mut ind = Supertrend::new(nf, mult).unwrap();
                let value: Vec<[f64; 2]> =
                    b.iter().map(|x| ind.next_hlc(x.high, x.low, x.close)).collect();
                agree(
                    &format!("supertrend n={n} len={len} mode={mode}"),
                    &cols(&stream),
                    &cols(&value),
                    &Supertrend::new(nf, mult).unwrap().vectorize(&b),
                    &series::supertrend(&h, &l, &c, nf, mult).unwrap(),
                );
            }
        }
    }
}

#[test]
fn a_reset_stepper_repeats_itself() {
    let b = input(700, 1);
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
    repeats!(Dx::new(14.0).unwrap());
    repeats!(PlusDi::new(14.0).unwrap());
    repeats!(MinusDi::new(14.0).unwrap());
    repeats!(PlusDm::new(14.0).unwrap());
    repeats!(MinusDm::new(14.0).unwrap());
    repeats!(ChaikinVolatility::new(10.0, 10.0).unwrap());
    repeats!(Chandelier::new(22.0, 3.0).unwrap());
    repeats!(Starc::new(6.0, 15.0, 2.0).unwrap());
    repeats!(Smma::new(14.0).unwrap());
    repeats!(Supertrend::new(10.0, 3.0).unwrap());
    repeats!(Adxr::new(14.0).unwrap());
    repeats!(Natr::new(14.0).unwrap());
    repeats!(Adx::new(14.0).unwrap());
}

#[test]
fn a_nan_price_poisons_a_wilder_recurrence_for_good() {
    // One NaN bar early on: a Wilder recurrence (the ATR in `starc`, the average in `smma`) never forgets it
    // (`NaN` for the rest of the series), and the bars before it are untouched.
    let mut b = input(300, 0);
    b[60] = Bar::ohlcv(60 * 60_000, f64::NAN, f64::NAN, f64::NAN, f64::NAN, 1.0);
    let (h, l, c) = hlc(&b);
    let s = series::starc(&h, &l, &c, 6.0, 15.0, 2.0).unwrap();
    assert!(s[0][59].is_finite() && s[0][60].is_nan() && s[0][299].is_nan(), "starc upper");
    let sma = series::smma(&c, 14.0).unwrap();
    assert!(sma[59].is_finite() && sma[60].is_nan() && sma[299].is_nan(), "smma stays poisoned");
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
/// scales and the awkward series. `cargo test -p vike-indicators --release --test fast_g -- --ignored --nocapture`
#[test]
#[ignore = "prints a measurement, asserts nothing"]
fn report_the_worst_relative_error() {
    let names = [
        "dx",
        "plus_di",
        "minus_di",
        "plus_dm",
        "minus_dm",
        "chaikin_volatility",
        "chandelier",
        "starc",
        "smma",
        "supertrend",
        "adxr",
        "natr",
    ];
    let mut worst = [0.0_f64; 12];
    let mut seen = |k: usize, e: f64| worst[k] = worst[k].max(e);
    let mut sets: Vec<Columns> = SCALES
        .iter()
        .map(|&(level, step)| hlc(&bars_of(&closes(level, step, 20_000), step)))
        .collect();
    sets.push(hlc(&awkward_bars()));
    for (h, l, c) in &sets {
        for n in [2usize, 3, 14, 50, 100] {
            let nf = n as f64;
            seen(0, max_rel(&series::dx(h, l, c, nf).unwrap(), &dm_ref(h, l, c, n, 2), 1.0));
            seen(1, max_rel(&series::plus_di(h, l, c, nf).unwrap(), &dm_ref(h, l, c, n, 0), 1.0));
            seen(2, max_rel(&series::minus_di(h, l, c, nf).unwrap(), &dm_ref(h, l, c, n, 1), 1.0));
            seen(3, max_rel(&series::plus_dm(h, l, nf).unwrap(), &dm_ref(h, l, c, n, 3), 1e-300));
            seen(4, max_rel(&series::minus_dm(h, l, nf).unwrap(), &dm_ref(h, l, c, n, 4), 1e-300));
            seen(10, max_rel(&series::adxr(h, l, c, nf).unwrap(), &adxr_ref(h, l, c, n), 1.0));
            seen(11, max_rel(&series::natr(h, l, c, nf).unwrap(), &natr_ref(h, l, c, n), 1.0));
        }
        for (en, rn) in [(2usize, 1usize), (10, 10), (30, 20), (100, 100)] {
            let got = series::chaikin_volatility(h, l, en as f64, rn as f64).unwrap();
            seen(5, max_rel(&got, &chaikin_ref(h, l, en, rn), 1.0));
        }
        for (n, mult) in [(2usize, 0.5), (22, 3.0), (200, 10.0)] {
            let got = series::chandelier(h, l, c, n as f64, mult).unwrap();
            let want = chandelier_ref(h, l, c, n, mult);
            for k in 0..2 {
                seen(6, max_rel(&got[k], &want[k], 1e-300));
            }
        }
        for (sn, an, mult) in [(1usize, 1usize, 0.5), (6, 15, 2.0), (200, 200, 10.0)] {
            let got = series::starc(h, l, c, sn as f64, an as f64, mult).unwrap();
            let want = starc_ref(h, l, c, sn, an, mult);
            for k in 0..3 {
                seen(7, max_rel(&got[k], &want[k], 1e-300));
            }
        }
        for n in [2usize, 14, 100, 400] {
            seen(8, max_rel(&series::smma(c, n as f64).unwrap(), &smma_ref(c, n), 1e-300));
        }
        for (n, mult) in [(1usize, 0.5), (10, 3.0), (100, 10.0)] {
            let got = series::supertrend(h, l, c, n as f64, mult).unwrap();
            let want = supertrend_ref(h, l, c, n, mult);
            for k in 0..2 {
                seen(9, max_rel(&got[k], &want[k], 1e-300));
            }
        }
    }
    for (name, e) in names.iter().zip(worst) {
        println!("max relative error {name:20} {e:.3e}");
    }
}

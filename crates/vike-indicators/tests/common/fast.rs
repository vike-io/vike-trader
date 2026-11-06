//! Helpers the `fast_*` and `series_*` suites share: the random walk their series start from,
//! the scales and windows they run at, the bit and tolerance comparisons, the textbook references more
//! than one of them holds a core to, and the OLD candle helpers the old-kernel copies read
//! (`fast_m.rs`, `fast_n.rs` and `common/old_two_bar.rs`). Each suite carried its own copy of these;
//! the copies were identical (or differed by a literal, now a parameter), so there is one now.
//!
//! The `fast` binary (`tests/fast.rs`) declares it ONCE, `#[path = "common/fast.rs"] mod fast;`, and
//! each suite reads it as `crate::fast`; a helper some build of the group leaves unread keeps the
//! `dead_code` allowance. Helpers that differ in substance stay in their own file.
#![allow(dead_code)]

use std::ops::Range;

use vike_marketdata::Bar;

// ------------------------------------------------------------------ series

/// The seed of [`closes`].
pub const SEED: u64 = 0x9e37_79b9_7f4a_7c15;

/// Level and step of the three close series: an ordinary one, a high-level small-step one (where a
/// running sum of raw prices loses every digit) and a tiny-level one.
pub const SCALES: [(f64, f64); 3] = [(100.0, 2.0), (1.0e6, 0.01), (1.0e-3, 1.0e-6)];

/// [`SCALES`] with the spread of the bars built around each close.
pub const SPREAD_SCALES: [(f64, f64, f64); 3] =
    [(100.0, 2.0, 1.0), (1.0e6, 0.01, 0.01), (1.0e-3, 1.0e-6, 1.0e-6)];

/// The windows the rolling cores run at.
pub const WINDOWS: [usize; 8] = [2, 3, 4, 5, 20, 64, 200, 400];

/// The xorshift64 generator every `fast_*` series draws from, as uniforms in `[0, 1)`.
pub struct XorShift(pub u64);

impl XorShift {
    /// The next uniform in `[0, 1)`: the top 53 bits of the next state.
    pub fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A [`XorShift`] random walk from `level`: `n` steps, each uniform in `[-step/2, step/2)`.
pub fn walk(seed: u64, level: f64, step: f64, n: usize) -> Vec<f64> {
    let mut r = XorShift(seed);
    let mut p = level;
    (0..n)
        .map(|_| {
            p += (r.unit() - 0.5) * step;
            p
        })
        .collect()
}

/// The close series every `fast_*` binary starts from: [`walk`] at [`SEED`].
pub fn closes(level: f64, step: f64, n: usize) -> Vec<f64> {
    walk(SEED, level, step, n)
}

/// A bar around each close with a spread that varies, so the true range is not just `high - low`.
pub fn spread_bars(c: &[f64]) -> Vec<Bar> {
    c.iter()
        .enumerate()
        .map(|(i, &x)| {
            let up = 0.3 + (i % 7) as f64 * 0.1;
            let dn = 0.2 + (i % 5) as f64 * 0.15;
            Bar::ohlcv(i as i64 * 60_000, x, x + up, x - dn, x, 1.0)
        })
        .collect()
}

/// A series built to hurt a smoother: a `flat` stretch (zero ranges, zero changes), a `volumeless`
/// stretch (pass an empty range for none), a gap in each direction, one bar with no range that still
/// moved, one that closes at zero.
pub fn awkward_bars(n: usize, flat: Range<usize>, volumeless: Range<usize>) -> Vec<Bar> {
    let mut s: u64 = 0xD1B5_4A32_D192_ED03;
    let mut price = 100.0_f64;
    (0..n)
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
            let is_flat = flat.contains(&i);
            if !is_flat {
                price = (price + (i as f64 * 0.05).sin() * 0.6 + (u - 0.5) * 2.0).max(1.0);
            }
            let point = i == 1000;
            let close = if i == 1100 { 0.0 } else { price };
            let (high, low) = if is_flat || point {
                (price, price)
            } else {
                (price.max(close) + u * 1.5, price.min(close) - u * 0.7)
            };
            let volume = if volumeless.contains(&i) { 0.0 } else { 500.0 + u * 1000.0 };
            Bar::ohlcv(i as i64 * 60_000, price, high, low, close, volume)
        })
        .collect()
}

/// The columns of a bar series.
pub struct Cols {
    pub ts: Vec<i64>,
    pub o: Vec<f64>,
    pub h: Vec<f64>,
    pub l: Vec<f64>,
    pub c: Vec<f64>,
    pub v: Vec<f64>,
}

pub fn cols(b: &[Bar]) -> Cols {
    Cols {
        ts: b.iter().map(|x| x.ts).collect(),
        o: b.iter().map(|x| x.open).collect(),
        h: b.iter().map(|x| x.high).collect(),
        l: b.iter().map(|x| x.low).collect(),
        c: b.iter().map(|x| x.close).collect(),
        v: b.iter().map(|x| x.volume).collect(),
    }
}

/// Each scale, clean and with a NaN planted in the middle of the closes, as `bars_of` builds the bars.
pub fn inputs(bars_of: fn(&[f64], f64) -> Vec<Bar>) -> Vec<(String, Cols)> {
    let mut v = Vec::new();
    for (level, step, spread) in SPREAD_SCALES {
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

/// Column `k` of fixed-width rows.
pub fn col<const N: usize>(rows: &[[f64; N]], k: usize) -> Vec<f64> {
    rows.iter().map(|r| r[k]).collect()
}

// ------------------------------------------------------------------ comparison

/// Bit equality, with two NaNs counting as equal.
pub fn same(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

/// `got` against `want` to relative `rel`, NaN exactly where the reference has one; returns the
/// worst error seen. `floor` is the smallest magnitude a value is measured against: `1e-300` makes it
/// a plain relative error (a reference zero must be exact), `1.0` an absolute one below 1 (an
/// oscillator that crosses zero has no meaningful relative error near it).
pub fn assert_close(name: &str, got: &[f64], want: &[f64], rel: f64, floor: f64) -> f64 {
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

/// The worst relative error over the positions where both are finite, measured against
/// `max(|want|, 1e-3 * the largest |want|)`.
pub fn worst(got: &[f64], want: &[f64]) -> f64 {
    let typ = want.iter().filter(|v| v.is_finite()).fold(0.0_f64, |a, v| a.max(v.abs()));
    got.iter()
        .zip(want)
        .filter(|(g, w)| g.is_finite() && w.is_finite())
        .map(|(g, w)| (g - w).abs() / w.abs().max(1.0e-3 * typ))
        .fold(0.0, f64::max)
}

/// A one-line indicator's four ways in (stream, plain-number entry, batch and slice call) agree bit
/// for bit.
pub fn ways1(
    label: &str,
    stream: Vec<f64>,
    value: Vec<f64>,
    batch: Vec<Vec<f64>>,
    slice: Vec<f64>,
) {
    assert!(same(&stream, &value), "{label}: next vs the plain-number entry");
    assert!(same(&stream, &batch[0]), "{label}: vectorize");
    assert!(same(&stream, &slice), "{label}: series");
}

/// The largest relative error (denominator floored at `floor`) of `got` against `want`, ignoring
/// positions where either is NaN.
pub fn max_rel(got: &[f64], want: &[f64], floor: f64) -> f64 {
    got.iter()
        .zip(want)
        .filter(|(g, w)| !g.is_nan() && !w.is_nan())
        .map(|(g, w)| (g - w).abs() / w.abs().max(floor))
        .fold(0.0, f64::max)
}

/// The worst relative error over the positions where both are finite, the denominator floored at
/// `1.0` (an absolute error below 1): the measurement probes' yardstick.
pub fn worst_floor_one(got: &[f64], want: &[f64]) -> f64 {
    got.iter()
        .zip(want)
        .filter(|(g, w)| g.is_finite() && w.is_finite())
        .map(|(g, w)| (g - w).abs() / w.abs().max(1.0))
        .fold(0.0, f64::max)
}

/// The same series with a NaN dropped in, to check a NaN leaves a window as cleanly as the textbook.
pub fn with_nan(mut c: Vec<f64>, at: usize) -> Vec<f64> {
    c[at] = f64::NAN;
    c
}

/// An ordinary `len`-close series, with a NaN a third of the way in once it is longer than 40.
pub fn series_for(len: usize) -> Vec<f64> {
    let mut c = closes(100.0, 2.0, len);
    if len > 40 {
        c[len / 3] = f64::NAN;
    }
    c
}

/// Feed `$bars` through a fresh `$ind`, reset it and feed them again: the two runs agree bit for bit
/// ([`same`]), the failure naming the indicator.
macro_rules! assert_reset_repeats {
    ($bars:expr, $ind:expr) => {{
        let mut ind = $ind;
        let first: Vec<Vec<f64>> = $bars.iter().map(|x| ind.on_bar(x)).collect();
        ind.reset();
        let second: Vec<Vec<f64>> = $bars.iter().map(|x| ind.on_bar(x)).collect();
        for (x, y) in first.iter().zip(&second) {
            assert!(crate::fast::same(x, y), "{}: a reset stepper differs", ind.name());
        }
    }};
}
pub(crate) use assert_reset_repeats;

/// [`assert_reset_repeats`]'s length-checked twin that also forks: a clone taken after the whole
/// series and the reset stepper's own clone, fed one more bar (stamped `$extra_ts`), agree.
macro_rules! assert_reset_and_fork_agree {
    ($bars:expr, $extra_ts:expr, $ind:expr) => {{
        let mut a = $ind;
        let first: Vec<Vec<f64>> = $bars.iter().map(|y| a.on_bar(y)).collect();
        let mut forked = a.clone();
        a.reset();
        let second: Vec<Vec<f64>> = $bars.iter().map(|y| a.on_bar(y)).collect();
        assert_eq!(first.len(), second.len());
        for (x, y) in first.iter().zip(&second) {
            assert!(crate::fast::same(x, y), "{}: a reset stepper repeats itself", a.name());
        }
        // The fork was taken after the whole series; feeding both one more bar agrees.
        let extra = vike_marketdata::Bar::ohlcv($extra_ts, 101.0, 102.0, 99.5, 100.5, 321.0);
        let mut cont = a.clone();
        let (p, q) = (forked.on_bar(&extra), cont.on_bar(&extra));
        assert!(
            crate::fast::same(&p, &q),
            "{}: a clone after the series carries on the same",
            a.name()
        );
    }};
}
pub(crate) use assert_reset_and_fork_agree;

// ------------------------------------------------------------------ textbook references

/// An EMA over a whole series seeded with the mean of the first `n` values, then
/// `alpha x + (1 - alpha) prev`.
pub fn ema_ref(c: &[f64], n: usize) -> Vec<f64> {
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

/// The AROON kernel's definition; a window with no number in it is `NaN` (the kernel panicked there).
pub fn aroon_ref(h: &[f64], l: &[f64], n: usize) -> [Vec<f64>; 2] {
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

/// The gap-aware range: `high - low` on the first bar, then the widest of it and the two gaps to the
/// previous close.
pub fn true_range_ref(h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
    (0..c.len())
        .map(|i| {
            if i == 0 {
                h[0] - l[0]
            } else {
                (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs())
            }
        })
        .collect()
}

// ------------------------------------------------------------------ the old candle helpers

// `patterns.rs`'s candle helpers and `avg_body` and `math.rs`'s `sma` as they stood on `perf/wave4`,
// verbatim: the old kernels copied into `fast_m.rs`, `fast_n.rs` and `common/old_two_bar.rs` read
// them, and `fast_k.rs` holds its averages to the same `sma`.

/// The average-body context window.
pub const CTX: usize = 10;

pub fn body(o: f64, c: f64) -> f64 {
    (c - o).abs()
}
pub fn rng(h: f64, l: f64) -> f64 {
    h - l
}
pub fn upper(o: f64, h: f64, c: f64) -> f64 {
    h - o.max(c)
}
pub fn lower(o: f64, l: f64, c: f64) -> f64 {
    o.min(c) - l
}
pub fn is_white(o: f64, c: f64) -> bool {
    c > o
}
pub fn is_black(o: f64, c: f64) -> bool {
    c < o
}
pub fn is_marubozu(o: f64, h: f64, l: f64, c: f64) -> bool {
    let r = rng(h, l);
    if r <= 0.0 {
        return false;
    }
    upper(o, h, c) <= 0.05 * r && lower(o, l, c) <= 0.05 * r
}

/// `math::sma`: every window folded from scratch with `iter().sum()`, `NaN` before `n` values.
pub fn sma(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if c.len() < n || n == 0 {
        return out;
    }
    for i in (n - 1)..c.len() {
        out[i] = c[i + 1 - n..=i].iter().sum::<f64>() / n as f64;
    }
    out
}

/// `patterns.rs`'s `avg_body`: the [`sma`] of the bodies over [`CTX`] bars.
pub fn avg_body(opens: &[f64], closes: &[f64]) -> Vec<f64> {
    let bodies: Vec<f64> = (0..closes.len()).map(|i| (closes[i] - opens[i]).abs()).collect();
    sma(&bodies, CTX)
}

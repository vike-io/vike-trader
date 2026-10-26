//! The 18 one-bar candlestick patterns (group `m`, fourth wave): the new cores against a COPY of the
//! old batch kernels, bit for bit, and the ways in against each other, bit for bit.
//!
//! A pattern's output is a discrete signal (`+100.0`, `-100.0` or `0.0`), so there is no tolerance
//! anywhere: the old kernels (kept below verbatim, with the three helpers they read, because the
//! code they came from is gone from `src/`) and the new cores must give the same bits on every bar of
//! every series. The series are: three price scales (level 100 with a step of 2, level 1e6 with a
//! step of 0.01, level 1e-3 with a step of 1e-6) built from a mix of bar shapes so that every pattern
//! fires; a coarse-grid fuzz that lands on every `<=` / `>=` tie; a series with a `NaN` travelling
//! through each of the four fields; and flat and zero-priced bars. The ways in are `next(&Bar)`,
//! `next_ohlc`, `vectorize`, `series::<name>` and the registry's boxed `on_bar`.

#![allow(clippy::needless_range_loop, clippy::collapsible_if)]

use vike_indicators::indicators::{
    BeltHold, ClosingMarubozu, Doji, DragonflyDoji, GravestoneDoji, Hammer, HangingMan, HighWave,
    InvertedHammer, LongLine, LongleggedDoji, Marubozu, OpeningMarubozu, RickshawMan, ShootingStar,
    ShortLine, SpinningTop, Takuri,
};
use vike_indicators::{Indicator, make, series};
use vike_marketdata::Bar;

// ------------------------------------------------------------------------------------------------
// The old code, as it stood on `perf/wave4`
// ------------------------------------------------------------------------------------------------

const CTX: usize = 10;

fn body(o: f64, c: f64) -> f64 {
    (c - o).abs()
}
fn rng(h: f64, l: f64) -> f64 {
    h - l
}
fn upper(o: f64, h: f64, c: f64) -> f64 {
    h - o.max(c)
}
fn lower(o: f64, l: f64, c: f64) -> f64 {
    o.min(c) - l
}
fn is_white(o: f64, c: f64) -> bool {
    c > o
}
fn is_black(o: f64, c: f64) -> bool {
    c < o
}
fn is_doji(o: f64, h: f64, l: f64, c: f64, avg: f64) -> bool {
    !avg.is_nan() && avg > 0.0 && body(o, c) <= 0.1 * avg && rng(h, l) > 0.0
}

/// `math::sma`, verbatim: every window folded from scratch with `iter().sum()`.
fn sma(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if c.len() < n || n == 0 {
        return out;
    }
    for i in (n - 1)..c.len() {
        out[i] = c[i + 1 - n..=i].iter().sum::<f64>() / n as f64;
    }
    out
}

fn avg_body(opens: &[f64], closes: &[f64]) -> Vec<f64> {
    let bodies: Vec<f64> = (0..closes.len()).map(|i| (closes[i] - opens[i]).abs()).collect();
    sma(&bodies, CTX)
}

/// The old `Columns::needing` (every column here, the kernels read only what they asked for).
struct Need;
impl Need {
    const OPEN: Need = Need;
    const HIGH: Need = Need;
    const LOW: Need = Need;
    const CLOSE: Need = Need;
}
impl std::ops::BitOr for Need {
    type Output = Need;
    fn bitor(self, _: Need) -> Need {
        Need
    }
}
struct Columns {
    o: Vec<f64>,
    h: Vec<f64>,
    l: Vec<f64>,
    c: Vec<f64>,
}
impl Columns {
    fn needing(bars: &[Bar], _need: Need) -> Self {
        Columns {
            o: bars.iter().map(|b| b.open).collect(),
            h: bars.iter().map(|b| b.high).collect(),
            l: bars.iter().map(|b| b.low).collect(),
            c: bars.iter().map(|b| b.close).collect(),
        }
    }
}

fn batch_doji(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 0..n {
        if is_doji(o[i], h[i], l[i], c[i], avg[i]) {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_hammer(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 0..n {
        let b = body(o[i], c[i]);
        let r = rng(h[i], l[i]);
        if r <= 0.0 || b <= 0.0 {
            continue;
        }
        let lo = lower(o[i], l[i], c[i]);
        let up = upper(o[i], h[i], c[i]);
        if b <= 0.3 * r && lo >= 2.0 * b && up <= b {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_inverted_hammer(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 0..n {
        let b = body(o[i], c[i]);
        let r = rng(h[i], l[i]);
        if r <= 0.0 || b <= 0.0 {
            continue;
        }
        let up = upper(o[i], h[i], c[i]);
        let lo = lower(o[i], l[i], c[i]);
        if b <= 0.3 * r && up >= 2.0 * b && lo <= b {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_hanging_man(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 0..n {
        let b = body(o[i], c[i]);
        let r = rng(h[i], l[i]);
        if r <= 0.0 || b <= 0.0 {
            continue;
        }
        let lo = lower(o[i], l[i], c[i]);
        let up = upper(o[i], h[i], c[i]);
        if b <= 0.3 * r && lo >= 2.0 * b && up <= b {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_shooting_star(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 0..n {
        let b = body(o[i], c[i]);
        let r = rng(h[i], l[i]);
        if r <= 0.0 || b <= 0.0 {
            continue;
        }
        let up = upper(o[i], h[i], c[i]);
        let lo = lower(o[i], l[i], c[i]);
        if b <= 0.3 * r && up >= 2.0 * b && lo <= b {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_dragonfly_doji(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 0..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let b = body(o[i], c[i]);
        let r = rng(h[i], l[i]);
        if r <= 0.0 {
            continue;
        }
        let up = upper(o[i], h[i], c[i]);
        let lo = lower(o[i], l[i], c[i]);
        if b <= 0.1 * a && up <= 0.1 * r && lo >= 0.5 * r {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_gravestone_doji(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 0..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let b = body(o[i], c[i]);
        let r = rng(h[i], l[i]);
        if r <= 0.0 {
            continue;
        }
        let up = upper(o[i], h[i], c[i]);
        let lo = lower(o[i], l[i], c[i]);
        if b <= 0.1 * a && lo <= 0.1 * r && up >= 0.5 * r {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_longlegged_doji(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 0..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let b = body(o[i], c[i]);
        let r = rng(h[i], l[i]);
        if r <= 0.0 {
            continue;
        }
        let up = upper(o[i], h[i], c[i]);
        let lo = lower(o[i], l[i], c[i]);
        if b <= 0.1 * a && up >= 0.3 * r && lo >= 0.3 * r {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_rickshaw_man(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 0..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (oo, hh, ll, cc) = (o[i], h[i], l[i], c[i]);
        let b = body(oo, cc);
        let r = rng(hh, ll);
        if r <= 0.0 {
            continue;
        }
        let up = upper(oo, hh, cc);
        let lo = lower(oo, ll, cc);
        let body_mid = (oo.max(cc) + oo.min(cc)) / 2.0;
        let range_mid = (hh + ll) / 2.0;
        if b <= 0.1 * a
            && up >= 0.3 * r
            && lo >= 0.3 * r
            && (body_mid - range_mid).abs() <= 0.25 * r
        {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_takuri(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 0..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (oo, hh, ll, cc) = (o[i], h[i], l[i], c[i]);
        let b = body(oo, cc);
        let r = rng(hh, ll);
        if r <= 0.0 {
            continue;
        }
        let up = upper(oo, hh, cc);
        let lo = lower(oo, ll, cc);
        if b <= 0.1 * a && up <= 0.1 * r && lo >= 0.7 * r {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_marubozu(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 0..n {
        let (oo, hh, ll, cc) = (o[i], h[i], l[i], c[i]);
        let b = body(oo, cc);
        let r = rng(hh, ll);
        if r <= 0.0 || b <= 0.0 {
            continue;
        }
        let up = upper(oo, hh, cc);
        let lo = lower(oo, ll, cc);
        if up <= 0.05 * r && lo <= 0.05 * r {
            if is_white(oo, cc) {
                out[i] = 100.0;
            } else if is_black(oo, cc) {
                out[i] = -100.0;
            }
        }
    }
    vec![out]
}

fn batch_closing_marubozu(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 0..n {
        let (oo, hh, ll, cc) = (o[i], h[i], l[i], c[i]);
        let b = body(oo, cc);
        let r = rng(hh, ll);
        if r <= 0.0 || b <= 0.0 {
            continue;
        }
        let up = upper(oo, hh, cc);
        let lo = lower(oo, ll, cc);
        if is_white(oo, cc) && up <= 0.05 * r {
            out[i] = 100.0;
        } else if is_black(oo, cc) && lo <= 0.05 * r {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_spinning_top(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 0..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (oo, hh, ll, cc) = (o[i], h[i], l[i], c[i]);
        let b = body(oo, cc);
        let r = rng(hh, ll);
        if r <= 0.0 || b <= 0.0 {
            continue;
        }
        let up = upper(oo, hh, cc);
        let lo = lower(oo, ll, cc);
        if b <= 0.3 * r && up > b && lo > b {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_high_wave(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 0..n {
        let (oo, hh, ll, cc) = (o[i], h[i], l[i], c[i]);
        let b = body(oo, cc);
        let r = rng(hh, ll);
        if r <= 0.0 || b <= 0.0 {
            continue;
        }
        let up = upper(oo, hh, cc);
        let lo = lower(oo, ll, cc);
        if b <= 0.15 * r && up >= 3.0 * b && lo >= 3.0 * b {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_long_line(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::CLOSE);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 0..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (oo, cc) = (o[i], c[i]);
        let b = body(oo, cc);
        if b >= 1.3 * a {
            if is_white(oo, cc) {
                out[i] = 100.0;
            } else if is_black(oo, cc) {
                out[i] = -100.0;
            }
        }
    }
    vec![out]
}

fn batch_short_line(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 0..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let b = body(o[i], c[i]);
        let r = rng(h[i], l[i]);
        if b <= 0.5 * a && r <= a {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_belt_hold(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 0..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (oo, hh, ll, cc) = (o[i], h[i], l[i], c[i]);
        let b = body(oo, cc);
        let r = rng(hh, ll);
        if r <= 0.0 || b <= 0.0 {
            continue;
        }
        let lo = lower(oo, ll, cc);
        let up = upper(oo, hh, cc);
        if is_white(oo, cc) && lo <= 0.05 * r && b >= a {
            out[i] = 100.0;
        } else if is_black(oo, cc) && up <= 0.05 * r && b >= a {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_opening_marubozu(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 0..n {
        let (oo, hh, ll, cc) = (o[i], h[i], l[i], c[i]);
        let b = body(oo, cc);
        let r = rng(hh, ll);
        if r <= 0.0 || b <= 0.0 {
            continue;
        }
        let up = upper(oo, hh, cc);
        let lo = lower(oo, ll, cc);
        if is_white(oo, cc) && lo <= 0.05 * r {
            out[i] = 100.0;
        } else if is_black(oo, cc) && up <= 0.05 * r {
            out[i] = -100.0;
        }
    }
    vec![out]
}

// ------------------------------------------------------------------------------------------------
// The series
// ------------------------------------------------------------------------------------------------

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A mix of bar shapes around `level` with moves of about `step`, chosen so that each pattern fires.
fn shapes(level: f64, step: f64, n: usize, seed: u64) -> Vec<Bar> {
    let mut g = Lcg(seed);
    let mut px = level;
    (0..n)
        .map(|i| {
            let kind = (g.next() * 11.0) as usize;
            let gap = if g.next() < 0.1 { (g.next() - 0.5) * 3.0 * step } else { 0.0 };
            let o = px + gap;
            let r = g.next();
            let (c, up, dn) = match kind {
                0 => (o + (r - 0.5) * 4.0 * step, g.next() * 2.0 * step, g.next() * 2.0 * step),
                1 => (o + (r - 0.5) * 0.02 * step, g.next() * 3.0 * step, g.next() * 3.0 * step),
                2 => (o + (r - 0.5) * 0.02 * step, 0.0, g.next() * 4.0 * step),
                3 => (o + (r - 0.5) * 0.02 * step, g.next() * 4.0 * step, 0.0),
                4 => (
                    o + (r - 0.5) * 0.4 * step,
                    g.next() * 0.2 * step,
                    3.0 * step + g.next() * step,
                ),
                5 => (
                    o + (r - 0.5) * 0.4 * step,
                    3.0 * step + g.next() * step,
                    g.next() * 0.2 * step,
                ),
                6 => {
                    let b = 3.0 * step * (0.5 + r);
                    if i % 2 == 0 { (o + b, 0.0, 0.0) } else { (o - b, 0.0, 0.0) }
                }
                7 => (o + 3.0 * step * (0.5 + r), g.next() * 0.04 * step, 0.0),
                8 => (o - 3.0 * step * (0.5 + r), 0.0, g.next() * 0.04 * step),
                9 => (o + (r - 0.5) * 0.5 * step, 2.0 * step, 2.0 * step),
                _ => (o, 0.0, 0.0),
            };
            px = c;
            let h = o.max(c) + up;
            let l = o.min(c) - dn;
            Bar::ohlcv(i as i64 * 60_000, o, h, l, c, 100.0)
        })
        .collect()
}

/// Prices on a coarse grid, the high and the low NOT constrained to bracket the body: every
/// comparison of a threshold against a body, a shadow or a range lands on a tie somewhere.
fn grid_fuzz(n: usize, seed: u64) -> Vec<Bar> {
    let mut g = Lcg(seed);
    let mut v = || ((g.next() * 7.0) as i32 as f64) * 0.5;
    (0..n).map(|i| Bar::ohlcv(i as i64, v(), v(), v(), v(), 1.0)).collect()
}

/// `shapes` with a `NaN` (and now and then an infinity) in one field of some bars, so the average
/// body is poisoned for ten bars and recovers.
fn with_nan(n: usize) -> Vec<Bar> {
    let mut bars = shapes(100.0, 2.0, n, 0x1234_5678_9abc_def1);
    for i in (7..n).step_by(41) {
        match (i / 41) % 4 {
            0 => bars[i].open = f64::NAN,
            1 => bars[i].high = f64::NAN,
            2 => bars[i].low = f64::NAN,
            _ => bars[i].close = f64::NAN,
        }
    }
    for i in (19..n).step_by(97) {
        bars[i].high = f64::INFINITY;
    }
    bars
}

/// Flat bars, a bar at zero and a stretch with nothing but ties.
fn awkward(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let x = if (100..140).contains(&i) {
                0.0
            } else if (300..330).contains(&i) {
                5.0
            } else {
                (i % 5) as f64
            };
            Bar::ohlcv(i as i64, x, x, x, x, 0.0)
        })
        .collect()
}

fn datasets() -> Vec<(&'static str, Vec<Bar>)> {
    vec![
        ("level 100", shapes(100.0, 2.0, 2600, 0x9e37_79b9_7f4a_7c15)),
        ("level 1e6", shapes(1.0e6, 0.01, 2600, 0xD1B5_4A32_D192_ED03)),
        ("level 1e-3", shapes(1.0e-3, 1.0e-6, 2600, 0x2545_F491_4F6C_DD1D)),
        ("grid fuzz", grid_fuzz(30_000, 0xA076_1D64_78BD_642F)),
        ("nan travelling", with_nan(2600)),
        ("awkward", awkward(700)),
    ]
}

// ------------------------------------------------------------------------------------------------
// The checks
// ------------------------------------------------------------------------------------------------

fn same(a: &[f64], b: &[f64], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: length");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "{what}: bar {i}: {x} vs {y}");
    }
}

fn columns(bars: &[Bar]) -> [Vec<f64>; 4] {
    [
        bars.iter().map(|b| b.open).collect(),
        bars.iter().map(|b| b.high).collect(),
        bars.iter().map(|b| b.low).collect(),
        bars.iter().map(|b| b.close).collect(),
    ]
}

/// Returns how many bars fired (a nonzero signal), so a test can prove it is not vacuous.
macro_rules! check {
    ($fire:ident, $Type:ident, $key:literal, $series:path, $old:ident) => {
        fn $fire(bars: &[Bar], tag: &str) -> usize {
            let old = $old(bars).remove(0);
            let [o, h, l, c] = columns(bars);
            let what = |way: &str| format!("{} / {} / {way}", $key, tag);

            let mut s = $Type::default();
            let stream: Vec<f64> = bars.iter().map(|b| s.next(b)).collect();
            same(&old, &stream, &what("next(&Bar)"));

            let mut s = $Type::default();
            let plain: Vec<f64> =
                bars.iter().map(|b| s.next_ohlc(b.open, b.high, b.low, b.close)).collect();
            same(&old, &plain, &what("next_ohlc"));

            let vect = $Type::default().vectorize(bars).remove(0);
            same(&old, &vect, &what("vectorize"));

            same(&old, &$series(&o, &h, &l, &c), &what("series"));

            let mut boxed = make($key).unwrap();
            let via: Vec<f64> = bars.iter().map(|b| boxed.on_bar(b)[0]).collect();
            same(&old, &via, &what("registry on_bar"));

            old.iter().filter(|&&x| x != 0.0).count()
        }
    };
}

check!(fire_doji, Doji, "doji", series::doji, batch_doji);
check!(fire_hammer, Hammer, "hammer", series::hammer, batch_hammer);
check!(
    fire_inverted_hammer,
    InvertedHammer,
    "inverted_hammer",
    series::inverted_hammer,
    batch_inverted_hammer
);
check!(fire_hanging_man, HangingMan, "hanging_man", series::hanging_man, batch_hanging_man);
check!(
    fire_shooting_star,
    ShootingStar,
    "shooting_star",
    series::shooting_star,
    batch_shooting_star
);
check!(
    fire_dragonfly_doji,
    DragonflyDoji,
    "dragonfly_doji",
    series::dragonfly_doji,
    batch_dragonfly_doji
);
check!(
    fire_gravestone_doji,
    GravestoneDoji,
    "gravestone_doji",
    series::gravestone_doji,
    batch_gravestone_doji
);
check!(
    fire_longlegged_doji,
    LongleggedDoji,
    "longlegged_doji",
    series::longlegged_doji,
    batch_longlegged_doji
);
check!(fire_rickshaw_man, RickshawMan, "rickshaw_man", series::rickshaw_man, batch_rickshaw_man);
check!(fire_takuri, Takuri, "takuri", series::takuri, batch_takuri);
check!(fire_marubozu, Marubozu, "marubozu", series::marubozu, batch_marubozu);
check!(
    fire_closing_marubozu,
    ClosingMarubozu,
    "closing_marubozu",
    series::closing_marubozu,
    batch_closing_marubozu
);
check!(fire_spinning_top, SpinningTop, "spinning_top", series::spinning_top, batch_spinning_top);
check!(fire_high_wave, HighWave, "high_wave", series::high_wave, batch_high_wave);
check!(fire_long_line, LongLine, "long_line", series::long_line, batch_long_line);
check!(fire_short_line, ShortLine, "short_line", series::short_line, batch_short_line);
check!(fire_belt_hold, BeltHold, "belt_hold", series::belt_hold, batch_belt_hold);
check!(
    fire_opening_marubozu,
    OpeningMarubozu,
    "opening_marubozu",
    series::opening_marubozu,
    batch_opening_marubozu
);

/// Every pattern, on every series: the new cores are the old kernels, bit for bit; and each pattern
/// fires at least once on the shaped series (so the equality is not a row of zeros).
#[test]
fn every_one_bar_pattern_is_bit_identical_to_the_old_kernel() {
    type Fire = fn(&[Bar], &str) -> usize;
    let all: [(&str, Fire); 18] = [
        ("doji", fire_doji),
        ("hammer", fire_hammer),
        ("inverted_hammer", fire_inverted_hammer),
        ("hanging_man", fire_hanging_man),
        ("shooting_star", fire_shooting_star),
        ("dragonfly_doji", fire_dragonfly_doji),
        ("gravestone_doji", fire_gravestone_doji),
        ("longlegged_doji", fire_longlegged_doji),
        ("rickshaw_man", fire_rickshaw_man),
        ("takuri", fire_takuri),
        ("marubozu", fire_marubozu),
        ("closing_marubozu", fire_closing_marubozu),
        ("spinning_top", fire_spinning_top),
        ("high_wave", fire_high_wave),
        ("long_line", fire_long_line),
        ("short_line", fire_short_line),
        ("belt_hold", fire_belt_hold),
        ("opening_marubozu", fire_opening_marubozu),
    ];
    let sets = datasets();
    let mut silent = Vec::new();
    for (name, fire) in all {
        let mut fired = 0;
        for (tag, bars) in &sets {
            fired += fire(bars, tag);
        }
        println!("{name}: {fired} signals");
        if fired == 0 {
            silent.push(name);
        }
    }
    assert!(silent.is_empty(), "these patterns never fired on any series: {silent:?}");
}

/// Short series around the warm-up of the average body (10 bars) and its edges.
#[test]
fn short_prefixes_agree() {
    let bars = shapes(100.0, 2.0, 40, 0x51ED_2701_BEEF_0001);
    for len in [0usize, 1, 2, 3, 9, 10, 11, 12, 19, 20, 21, 40] {
        for (name, fire) in [
            ("doji", fire_doji as fn(&[Bar], &str) -> usize),
            ("long_line", fire_long_line),
            ("belt_hold", fire_belt_hold),
            ("short_line", fire_short_line),
            ("marubozu", fire_marubozu),
        ] {
            fire(&bars[..len], &format!("{name} prefix {len}"));
        }
    }
}

/// The slices may differ in length: the answer is as long as the shortest.
#[test]
fn series_is_as_long_as_the_shortest_slice() {
    let bars = shapes(100.0, 2.0, 50, 7);
    let [o, h, l, c] = columns(&bars);
    assert_eq!(series::doji(&o[..30], &h, &l, &c).len(), 30);
    assert_eq!(series::doji(&o, &h[..20], &l, &c).len(), 20);
    assert_eq!(series::doji(&o, &h, &l[..10], &c).len(), 10);
    assert_eq!(series::doji(&o, &h, &l, &c[..0]).len(), 0);
    let full = series::long_line(&o, &h, &l, &c);
    same(&full[..30], &series::long_line(&o[..30], &h, &l, &c), "a shorter open is a prefix");
}

/// `reset` returns a stepper to its state at construction, a clone continues independently, and
/// `value` is the last answer (`NaN` before the first bar).
#[test]
fn reset_clone_and_value() {
    let bars = shapes(100.0, 2.0, 200, 99);
    let mut a = LongLine::default();
    assert!(a.value()[0].is_nan());
    let first: Vec<f64> = bars.iter().map(|b| a.next(b)).collect();
    assert_eq!(a.value()[0].to_bits(), first[199].to_bits());
    a.reset();
    assert!(a.value()[0].is_nan());
    let again: Vec<f64> = bars.iter().map(|b| a.next(b)).collect();
    same(&first, &again, "after reset");

    let mut half = BeltHold::default();
    for b in &bars[..100] {
        half.next(b);
    }
    let mut twin = half.clone();
    let rest: Vec<f64> = bars[100..].iter().map(|b| half.next(b)).collect();
    let rest_twin: Vec<f64> = bars[100..].iter().map(|b| twin.next(b)).collect();
    same(&rest, &rest_twin, "a clone");
}

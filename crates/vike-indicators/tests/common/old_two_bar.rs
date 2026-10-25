//! The OLD two-bar candlestick kernels, copied verbatim from `src/indicators/patterns/two_bar.rs` as
//! it stood before the fourth wave (when the eighteen patterns streamed by re-running these over a
//! retained tail), with the few helpers they call (`patterns.rs`'s candle helpers and `avg_body`,
//! `math.rs`'s `Columns` and `sma`). `tests/fast_q.rs` and the benchmark hold the new steppers to
//! them bit for bit: a pattern's signal is discrete, so it gets no tolerance.
// Each including test binary uses a subset of it; the loops and nested ifs are the old file's, verbatim.
#![allow(dead_code, clippy::needless_range_loop, clippy::collapsible_if)]

use vike_marketdata::Bar;

pub struct Columns {
    pub o: Vec<f64>,
    pub h: Vec<f64>,
    pub l: Vec<f64>,
    pub c: Vec<f64>,
}

#[derive(Clone, Copy)]
pub struct Need(u8);

impl Need {
    pub const OPEN: Need = Need(1);
    pub const HIGH: Need = Need(2);
    pub const LOW: Need = Need(4);
    pub const CLOSE: Need = Need(8);
    fn has(self, other: Need) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for Need {
    type Output = Need;
    fn bitor(self, rhs: Need) -> Need {
        Need(self.0 | rhs.0)
    }
}

impl Columns {
    pub fn needing(bars: &[Bar], need: Need) -> Self {
        let n = bars.len();
        let col = |want: Need| if need.has(want) { Vec::with_capacity(n) } else { Vec::new() };
        let (mut o, mut h, mut l) = (col(Need::OPEN), col(Need::HIGH), col(Need::LOW));
        let mut c = col(Need::CLOSE);
        for b in bars {
            if need.has(Need::OPEN) {
                o.push(b.open);
            }
            if need.has(Need::HIGH) {
                h.push(b.high);
            }
            if need.has(Need::LOW) {
                l.push(b.low);
            }
            if need.has(Need::CLOSE) {
                c.push(b.close);
            }
        }
        Columns { o, h, l, c }
    }
}

const CTX: usize = 10;

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

fn avg_body(opens: &[f64], closes: &[f64]) -> Vec<f64> {
    let bodies: Vec<f64> = (0..closes.len()).map(|i| (closes[i] - opens[i]).abs()).collect();
    sma(&bodies, CTX)
}

fn is_marubozu(o: f64, h: f64, l: f64, c: f64) -> bool {
    let r = rng(h, l);
    if r <= 0.0 {
        return false;
    }
    upper(o, h, c) <= 0.05 * r && lower(o, l, c) <= 0.05 * r
}

// ---- the kernels, verbatim ----

pub fn batch_engulfing(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::CLOSE);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let (po, pc) = (o[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if is_black(po, pc) && is_white(oo, cc) && cc >= po && oo <= pc {
            out[i] = 100.0;
        } else if is_white(po, pc) && is_black(oo, cc) && oo >= pc && cc <= po {
            out[i] = -100.0;
        }
    }
    vec![out]
}

pub fn batch_doji_star(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 1..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (oo, hh, ll, cc) = (o[i], h[i], l[i], c[i]);
        let b = body(oo, cc);
        let r = rng(hh, ll);
        if r <= 0.0 || b > 0.1 * a {
            continue;
        }
        let (po, pc) = (o[i - 1], c[i - 1]);
        let prev_body = body(po, pc);
        if prev_body < a {
            continue;
        }
        if is_white(po, pc) && ll > pc {
            out[i] = -100.0;
        } else if is_black(po, pc) && hh < pc {
            out[i] = 100.0;
        }
    }
    vec![out]
}

pub fn batch_harami(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::CLOSE);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let (po, pc) = (o[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        let prev_hi = po.max(pc);
        let prev_lo = po.min(pc);
        let curr_hi = oo.max(cc);
        let curr_lo = oo.min(cc);
        if curr_hi >= prev_hi || curr_lo <= prev_lo {
            continue;
        }
        if is_black(po, pc) && is_white(oo, cc) {
            out[i] = 100.0;
        } else if is_white(po, pc) && is_black(oo, cc) {
            out[i] = -100.0;
        }
    }
    vec![out]
}

pub fn batch_harami_cross(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::CLOSE);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 1..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (po, pc) = (o[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if body(oo, cc) > 0.1 * a {
            continue;
        }
        let prev_hi = po.max(pc);
        let prev_lo = po.min(pc);
        let doji_mid = (oo + cc) / 2.0;
        if doji_mid >= prev_hi || doji_mid <= prev_lo {
            continue;
        }
        if is_black(po, pc) {
            out[i] = 100.0;
        } else if is_white(po, pc) {
            out[i] = -100.0;
        }
    }
    vec![out]
}

pub fn batch_piercing(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::LOW | Need::CLOSE);
    let (o, l, c) = (&x.o, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let (po, pl, pc) = (o[i - 1], l[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if !is_black(po, pc) {
            continue;
        }
        if !is_white(oo, cc) {
            continue;
        }
        let midpoint = (po + pc) / 2.0;
        if oo < pl && cc > midpoint && cc < po {
            out[i] = 100.0;
        }
    }
    vec![out]
}

pub fn batch_dark_cloud_cover(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::CLOSE);
    let (o, h, c) = (&x.o, &x.h, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let (po, ph, pc) = (o[i - 1], h[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if !is_white(po, pc) {
            continue;
        }
        if !is_black(oo, cc) {
            continue;
        }
        let midpoint = (po + pc) / 2.0;
        if oo > ph && cc < midpoint && cc > po {
            out[i] = -100.0;
        }
    }
    vec![out]
}

pub fn batch_counterattack(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::CLOSE);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 1..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (po, pc) = (o[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if (cc - pc).abs() > 0.03 * a + 1e-9 {
            continue;
        }
        if body(po, pc) < 0.3 * a || body(oo, cc) < 0.3 * a {
            continue;
        }
        if is_black(po, pc) && is_white(oo, cc) {
            out[i] = 100.0;
        } else if is_white(po, pc) && is_black(oo, cc) {
            out[i] = -100.0;
        }
    }
    vec![out]
}

pub fn batch_meeting_lines(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::CLOSE);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 1..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (po, pc) = (o[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if (cc - pc).abs() > 0.02 * a + 1e-9 {
            continue;
        }
        if body(po, pc) < 0.3 * a || body(oo, cc) < 0.3 * a {
            continue;
        }
        if is_black(po, pc) && is_white(oo, cc) {
            out[i] = 100.0;
        } else if is_white(po, pc) && is_black(oo, cc) {
            out[i] = -100.0;
        }
    }
    vec![out]
}

pub fn batch_separating_lines(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::CLOSE);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 1..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (po, pc) = (o[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if (oo - po).abs() > 0.01 * a + 1e-9 {
            continue;
        }
        if is_white(po, pc) && is_white(oo, cc) {
            out[i] = 100.0;
        } else if is_black(po, pc) && is_black(oo, cc) {
            out[i] = -100.0;
        }
    }
    vec![out]
}

pub fn batch_matching_low(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::CLOSE);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 1..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (po, pc) = (o[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if !is_black(po, pc) || !is_black(oo, cc) {
            continue;
        }
        if (cc - pc).abs() <= 0.01 * a + 1e-9 {
            out[i] = 100.0;
        }
    }
    vec![out]
}

pub fn batch_on_neck(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::LOW | Need::CLOSE);
    let (o, l, c) = (&x.o, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let (po, pl, pc) = (o[i - 1], l[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if !is_black(po, pc) || !is_white(oo, cc) {
            continue;
        }
        let prev_body = body(po, pc);
        if prev_body <= 0.0 {
            continue;
        }
        if oo < pl && (cc - pl).abs() <= 0.03 * prev_body {
            out[i] = -100.0;
        }
    }
    vec![out]
}

pub fn batch_in_neck(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::LOW | Need::CLOSE);
    let (o, l, c) = (&x.o, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let (po, pl, pc) = (o[i - 1], l[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if !is_black(po, pc) || !is_white(oo, cc) {
            continue;
        }
        let prev_body = body(po, pc);
        if prev_body <= 0.0 {
            continue;
        }
        if oo < pl && cc > pc && (cc - pc) <= 0.15 * prev_body {
            out[i] = -100.0;
        }
    }
    vec![out]
}

pub fn batch_thrusting(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::LOW | Need::CLOSE);
    let (o, l, c) = (&x.o, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let (po, pl, pc) = (o[i - 1], l[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if !is_black(po, pc) || !is_white(oo, cc) {
            continue;
        }
        let prev_body = body(po, pc);
        if prev_body <= 0.0 {
            continue;
        }
        let midpoint = (po + pc) / 2.0;
        if oo < pl && cc > pc && cc < midpoint {
            out[i] = -100.0;
        }
    }
    vec![out]
}

pub fn batch_kicking(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let (po, ph, pl, pc) = (o[i - 1], h[i - 1], l[i - 1], c[i - 1]);
        let (oo, hh, ll, cc) = (o[i], h[i], l[i], c[i]);
        if !is_marubozu(po, ph, pl, pc) || !is_marubozu(oo, hh, ll, cc) {
            continue;
        }
        if is_black(po, pc) && is_white(oo, cc) && oo > pc {
            out[i] = 100.0;
        } else if is_white(po, pc) && is_black(oo, cc) && oo < pc {
            out[i] = -100.0;
        }
    }
    vec![out]
}

pub fn batch_kicking_by_length(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let (po, ph, pl, pc) = (o[i - 1], h[i - 1], l[i - 1], c[i - 1]);
        let (oo, hh, ll, cc) = (o[i], h[i], l[i], c[i]);
        if !is_marubozu(po, ph, pl, pc) || !is_marubozu(oo, hh, ll, cc) {
            continue;
        }
        let is_gap_up = is_black(po, pc) && is_white(oo, cc) && oo > pc;
        let is_gap_down = is_white(po, pc) && is_black(oo, cc) && oo < pc;
        if !(is_gap_up || is_gap_down) {
            continue;
        }
        let prev_body = body(po, pc);
        let curr_body = body(oo, cc);
        if curr_body >= prev_body {
            out[i] = if is_white(oo, cc) { 100.0 } else { -100.0 };
        } else {
            out[i] = if is_white(po, pc) { 100.0 } else { -100.0 };
        }
    }
    vec![out]
}

pub fn batch_homing_pigeon(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::CLOSE);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let (po, pc) = (o[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if !is_black(po, pc) || !is_black(oo, cc) {
            continue;
        }
        let prev_hi = po.max(pc);
        let prev_lo = po.min(pc);
        let curr_hi = oo.max(cc);
        let curr_lo = oo.min(cc);
        if curr_hi < prev_hi && curr_lo > prev_lo {
            out[i] = 100.0;
        }
    }
    vec![out]
}

pub fn batch_gap_side_side_white(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::CLOSE);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 1..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (po, pc) = (o[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if !is_white(po, pc) || !is_white(oo, cc) {
            continue;
        }
        let prev_body = body(po, pc);
        let curr_body = body(oo, cc);
        if prev_body <= 0.0 || curr_body <= 0.0 {
            continue;
        }
        if oo > pc && (curr_body - prev_body).abs() <= 0.5 * prev_body.max(curr_body) {
            out[i] = 100.0;
        }
    }
    vec![out]
}

pub fn batch_tasuki_gap(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::CLOSE);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 1..n {
        let (po, pc) = (o[i - 1], c[i - 1]);
        let (oo, cc) = (o[i], c[i]);
        if is_white(po, pc) && is_black(oo, cc) {
            if po.min(pc) < oo && oo < po.max(pc) && cc > po {
                out[i] = 100.0;
            }
        } else if is_black(po, pc)
            && is_white(oo, cc)
            && po.min(pc) < oo
            && oo < po.max(pc)
            && cc < po
        {
            out[i] = -100.0;
        }
    }
    vec![out]
}

//! Candlestick patterns â faithful f64 port of vike-trader-app
//! `core/indicators/patterns.py` (63 patterns). Each pattern is a causal
//! per-bar signal returning a single line of `+100.0` (bullish / presence),
//! `-100.0` (bearish), or `0.0` (none) â Python ints as f64. A rolling average
//! body (`avg_body` = SMA(10) of `|close-open|`, NaN warm-up mirroring Python's
//! `None`) supplies the "long/short body" context many patterns use. Streaming
//! is history-recompute via [`hist_indicator!`] (correct-by-construction for
//! these causal patterns; naive-fold order is load-bearing).
#![allow(clippy::needless_range_loop)]
// Candlestick ports mirror the Python `if bullish { ... } elif bearish { ... }`
// shape with a nested directional check; collapsing the nested `if` obscures the
// 1:1 correspondence with patterns.py.
#![allow(clippy::collapsible_if)]

use super::macros::hist_indicator;
use crate::math::{sma, Columns};
use vike_model::Bar;

// ============================ shared helpers ===============================
// Direct ports of the private helpers at the top of patterns.py.

const CTX: usize = 10; // bars of context for the rolling average body

/// `_body` â absolute body size `|close - open|`.
fn body(o: f64, c: f64) -> f64 {
    (c - o).abs()
}

/// `_range` â full high-low range `high - low`.
fn rng(h: f64, l: f64) -> f64 {
    h - l
}

/// `_upper` â upper shadow `high - max(open, close)`.
fn upper(o: f64, h: f64, c: f64) -> f64 {
    h - o.max(c)
}

/// `_lower` â lower shadow `min(open, close) - low`.
fn lower(o: f64, l: f64, c: f64) -> f64 {
    o.min(c) - l
}

/// `_is_white` â bullish candle `close > open`.
fn is_white(o: f64, c: f64) -> bool {
    c > o
}

/// `_is_black` â bearish candle `close < open`.
fn is_black(o: f64, c: f64) -> bool {
    c < o
}

/// `_avg_body` â rolling SMA of `|close-open|` (the "average body" context),
/// aligned, NaN warm-up (Python `None`). Period is `_CTX` (=10) at every site.
fn avg_body(opens: &[f64], closes: &[f64]) -> Vec<f64> {
    let bodies: Vec<f64> = (0..closes.len()).map(|i| (closes[i] - opens[i]).abs()).collect();
    sma(&bodies, CTX)
}

/// `_is_doji` â body tiny relative to the average body (<=10%) and a real range.
/// (`avg is not None` â `!avg.is_nan()`.)
fn is_doji(o: f64, h: f64, l: f64, c: f64, avg: f64) -> bool {
    !avg.is_nan() && avg > 0.0 && body(o, c) <= 0.1 * avg && rng(h, l) > 0.0
}

/// `_is_marubozu` â both shadows <=5% of range (open/close-side near extremes).
fn is_marubozu(o: f64, h: f64, l: f64, c: f64) -> bool {
    let r = rng(h, l);
    if r <= 0.0 {
        return false;
    }
    upper(o, h, c) <= 0.05 * r && lower(o, l, c) <= 0.05 * r
}

// ============================ batch kernels ================================
// One `batch_<name>` per pattern; each is a line-for-line port. Column vectors
// (o/h/l/c) come from `Columns::from_bars`.

fn batch_doji(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_engulfing(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_hammer(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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
    let x = Columns::from_bars(bars);
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

fn batch_doji_star(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_harami(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_harami_cross(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_piercing(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_dark_cloud_cover(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_counterattack(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_meeting_lines(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_separating_lines(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_matching_low(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_on_neck(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_in_neck(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_thrusting(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_kicking(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_kicking_by_length(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_homing_pigeon(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_gap_side_side_white(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_tasuki_gap(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
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

fn batch_morning_star(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, h, c) = (&x.o, &x.h, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, h2, c2) = (o[i - 1], h[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if !is_black(o1, c1) || body(o1, c1) < a {
            continue;
        }
        if body(o2, c2) >= 0.3 * a || h2 >= c1 {
            continue;
        }
        if !is_white(o3, c3) || body(o3, c3) < a {
            continue;
        }
        let mid1 = (o1 + c1) / 2.0;
        if c3 > mid1 {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_evening_star(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, l, c) = (&x.o, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, l2, c2) = (o[i - 1], l[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if !is_white(o1, c1) || body(o1, c1) < a {
            continue;
        }
        if body(o2, c2) >= 0.3 * a || l2 <= c1 {
            continue;
        }
        if !is_black(o3, c3) || body(o3, c3) < a {
            continue;
        }
        let mid1 = (o1 + c1) / 2.0;
        if c3 < mid1 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_morning_doji_star(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, h, c) = (&x.o, &x.h, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, h2, c2) = (o[i - 1], h[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if !is_black(o1, c1) || body(o1, c1) < a {
            continue;
        }
        if body(o2, c2) > 0.1 * a || h2 >= c1 {
            continue;
        }
        if !is_white(o3, c3) || body(o3, c3) < a {
            continue;
        }
        let mid1 = (o1 + c1) / 2.0;
        if c3 > mid1 {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_evening_doji_star(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, l, c) = (&x.o, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, l2, c2) = (o[i - 1], l[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if !is_white(o1, c1) || body(o1, c1) < a {
            continue;
        }
        if body(o2, c2) > 0.1 * a || l2 <= c1 {
            continue;
        }
        if !is_black(o3, c3) || body(o3, c3) < a {
            continue;
        }
        let mid1 = (o1 + c1) / 2.0;
        if c3 < mid1 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_three_white_soldiers(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, h, c) = (&x.o, &x.h, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, h1, c1) = (o[i - 2], h[i - 2], c[i - 2]);
        let (o2, h2, c2) = (o[i - 1], h[i - 1], c[i - 1]);
        let (o3, h3, c3) = (o[i], h[i], c[i]);
        if !(is_white(o1, c1) && is_white(o2, c2) && is_white(o3, c3)) {
            continue;
        }
        if body(o1, c1) < 0.7 * a || body(o2, c2) < 0.7 * a || body(o3, c3) < 0.7 * a {
            continue;
        }
        if !(o1 < o2 && o2 < c1 && o2 < o3 && o3 < c2) {
            continue;
        }
        if upper(o1, h1, c1) > 0.3 * body(o1, c1)
            || upper(o2, h2, c2) > 0.3 * body(o2, c2)
            || upper(o3, h3, c3) > 0.3 * body(o3, c3)
        {
            continue;
        }
        if c1 < c2 && c2 < c3 {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_three_black_crows(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, l, c) = (&x.o, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, l1, c1) = (o[i - 2], l[i - 2], c[i - 2]);
        let (o2, l2, c2) = (o[i - 1], l[i - 1], c[i - 1]);
        let (o3, l3, c3) = (o[i], l[i], c[i]);
        if !(is_black(o1, c1) && is_black(o2, c2) && is_black(o3, c3)) {
            continue;
        }
        if body(o1, c1) < 0.7 * a || body(o2, c2) < 0.7 * a || body(o3, c3) < 0.7 * a {
            continue;
        }
        if !(c1 < o2 && o2 < o1 && c2 < o3 && o3 < o2) {
            continue;
        }
        if lower(o1, l1, c1) > 0.3 * body(o1, c1)
            || lower(o2, l2, c2) > 0.3 * body(o2, c2)
            || lower(o3, l3, c3) > 0.3 * body(o3, c3)
        {
            continue;
        }
        if c1 > c2 && c2 > c3 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_identical_three_crows(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, c2) = (o[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if !(is_black(o1, c1) && is_black(o2, c2) && is_black(o3, c3)) {
            continue;
        }
        if body(o1, c1) < 0.7 * a || body(o2, c2) < 0.7 * a || body(o3, c3) < 0.7 * a {
            continue;
        }
        let tol = 0.05 * a;
        if (o2 - c1).abs() > tol || (o3 - c2).abs() > tol {
            continue;
        }
        if c1 > c2 && c2 > c3 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_three_inside(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 2..n {
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, c2) = (o[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        let b1_hi = o1.max(c1);
        let b1_lo = o1.min(c1);
        let b2_hi = o2.max(c2);
        let b2_lo = o2.min(c2);
        if b2_hi >= b1_hi || b2_lo <= b1_lo {
            continue;
        }
        if is_black(o1, c1) && is_white(o2, c2) && is_white(o3, c3) && c3 > o1 {
            out[i] = 100.0;
        } else if is_white(o1, c1) && is_black(o2, c2) && is_black(o3, c3) && c3 < o1 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_three_outside(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 2..n {
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, c2) = (o[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if is_black(o1, c1) && is_white(o2, c2) && c2 >= o1 && o2 <= c1 {
            if is_white(o3, c3) && c3 > c2 {
                out[i] = 100.0;
            }
        } else if is_white(o1, c1) && is_black(o2, c2) && o2 >= c1 && c2 <= o1 {
            if is_black(o3, c3) && c3 < c2 {
                out[i] = -100.0;
            }
        }
    }
    vec![out]
}

fn batch_three_line_strike(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 3..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 3], c[i - 3]);
        let (o2, c2) = (o[i - 2], c[i - 2]);
        let (o3, c3) = (o[i - 1], c[i - 1]);
        let (o4, c4) = (o[i], c[i]);
        if is_white(o1, c1)
            && is_white(o2, c2)
            && is_white(o3, c3)
            && c1 < c2
            && c2 < c3
            && is_black(o4, c4)
            && o4 >= c3
            && c4 <= o1
        {
            out[i] = -100.0;
        } else if is_black(o1, c1)
            && is_black(o2, c2)
            && is_black(o3, c3)
            && c1 > c2
            && c2 > c3
            && is_white(o4, c4)
            && o4 <= c3
            && c4 >= o1
        {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_three_stars_in_south(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 2..n {
        let (o1, h1, l1, c1) = (o[i - 2], h[i - 2], l[i - 2], c[i - 2]);
        let (o2, h2, l2, c2) = (o[i - 1], h[i - 1], l[i - 1], c[i - 1]);
        let (o3, h3, l3, c3) = (o[i], h[i], l[i], c[i]);
        if !(is_black(o1, c1) && is_black(o2, c2) && is_black(o3, c3)) {
            continue;
        }
        let rng1 = rng(h1, l1);
        let rng2 = rng(h2, l2);
        let rng3 = rng(h3, l3);
        if rng1 <= 0.0 || rng2 <= 0.0 || rng3 <= 0.0 {
            continue;
        }
        if !(rng1 > rng2 && rng2 > rng3) {
            continue;
        }
        if l2 <= l1 {
            continue;
        }
        if h3 >= h2 || l3 <= l2 {
            continue;
        }
        out[i] = 100.0;
    }
    vec![out]
}

fn batch_abandoned_baby(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, h1, l1, c1) = (o[i - 2], h[i - 2], l[i - 2], c[i - 2]);
        let (o2, h2, l2, c2) = (o[i - 1], h[i - 1], l[i - 1], c[i - 1]);
        let (o3, h3, l3, c3) = (o[i], h[i], l[i], c[i]);
        if body(o2, c2) > 0.1 * a || rng(h2, l2) <= 0.0 {
            continue;
        }
        if is_black(o1, c1) && body(o1, c1) >= a && h2 < l1 && is_white(o3, c3) && l3 >= h2 {
            out[i] = 100.0;
        } else if is_white(o1, c1) && body(o1, c1) >= a && l2 > h1 && is_black(o3, c3) && h3 <= l2 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_advance_block(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, h, c) = (&x.o, &x.h, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, h1, c1) = (o[i - 2], h[i - 2], c[i - 2]);
        let (o2, h2, c2) = (o[i - 1], h[i - 1], c[i - 1]);
        let (o3, h3, c3) = (o[i], h[i], c[i]);
        if !(is_white(o1, c1) && is_white(o2, c2) && is_white(o3, c3)) {
            continue;
        }
        let b1 = body(o1, c1);
        let b2 = body(o2, c2);
        let b3 = body(o3, c3);
        if b1 <= 0.0 || b2 <= 0.0 || b3 <= 0.0 {
            continue;
        }
        let u1 = upper(o1, h1, c1);
        let u2 = upper(o2, h2, c2);
        let u3 = upper(o3, h3, c3);
        if !(c1 < c2 && c2 < c3) {
            continue;
        }
        let weakening_bodies = b2 < b1 && b3 < b2;
        let growing_shadows = u2 > u1 && u3 > u2;
        if weakening_bodies || growing_shadows {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_stalled_pattern(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, c2) = (o[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if !(is_white(o1, c1) && is_white(o2, c2) && is_white(o3, c3)) {
            continue;
        }
        if body(o1, c1) < a || body(o2, c2) < a {
            continue;
        }
        if body(o3, c3) >= 0.5 * a {
            continue;
        }
        if c1 < c2 && o3 >= c2 * 0.98 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_two_crows(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, c2) = (o[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if !is_white(o1, c1) || body(o1, c1) < a {
            continue;
        }
        if !is_black(o2, c2) || o2 <= c1 {
            continue;
        }
        if !is_black(o3, c3) {
            continue;
        }
        if o1 < c3 && c3 < c1 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_upside_gap_two_crows(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, c2) = (o[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if !is_white(o1, c1) || body(o1, c1) < a {
            continue;
        }
        if !is_black(o2, c2) || o2 <= c1 {
            continue;
        }
        if !is_black(o3, c3) {
            continue;
        }
        if o3 >= o2 && c3 <= c2 && c3 > c1 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_tristar(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, h1, l1, c1) = (o[i - 2], h[i - 2], l[i - 2], c[i - 2]);
        let (o2, h2, l2, c2) = (o[i - 1], h[i - 1], l[i - 1], c[i - 1]);
        let (o3, h3, l3, c3) = (o[i], h[i], l[i], c[i]);
        if body(o1, c1) > 0.1 * a
            || rng(h1, l1) <= 0.0
            || body(o2, c2) > 0.1 * a
            || rng(h2, l2) <= 0.0
            || body(o3, c3) > 0.1 * a
            || rng(h3, l3) <= 0.0
        {
            continue;
        }
        let mid1 = (o1.max(c1) + o1.min(c1)) / 2.0;
        let mid2 = (o2.max(c2) + o2.min(c2)) / 2.0;
        let mid3 = (o3.max(c3) + o3.min(c3)) / 2.0;
        if mid2 < mid1 && mid3 > mid2 {
            out[i] = 100.0;
        } else if mid2 > mid1 && mid3 < mid2 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_unique_three_river(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, l, c) = (&x.o, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, l1, c1) = (o[i - 2], l[i - 2], c[i - 2]);
        let (o2, l2, c2) = (o[i - 1], l[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if !is_black(o1, c1) || body(o1, c1) < a {
            continue;
        }
        if !is_black(o2, c2) {
            continue;
        }
        if !(o2.min(c2) > o1.min(c1) && o2.max(c2) < o1.max(c1)) {
            continue;
        }
        if l2 >= l1 {
            continue;
        }
        if !is_white(o3, c3) {
            continue;
        }
        if body(o3, c3) >= body(o2, c2) {
            continue;
        }
        out[i] = 100.0;
    }
    vec![out]
}

fn batch_stick_sandwich(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 2..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, c2) = (o[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if !(is_black(o1, c1) && is_white(o2, c2) && is_black(o3, c3)) {
            continue;
        }
        if (c3 - c1).abs() <= 0.03 * a + 1e-9 {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_ladder_bottom(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, h, c) = (&x.o, &x.h, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 4..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 4], c[i - 4]);
        let (o2, c2) = (o[i - 3], c[i - 3]);
        let (o3, c3) = (o[i - 2], c[i - 2]);
        let (o4, h4, c4) = (o[i - 1], h[i - 1], c[i - 1]);
        let (o5, c5) = (o[i], c[i]);
        if !(is_black(o1, c1) && is_black(o2, c2) && is_black(o3, c3) && is_black(o4, c4)) {
            continue;
        }
        if !(c1 > c2 && c2 > c3 && c3 > c4) {
            continue;
        }
        if upper(o4, h4, c4) <= 0.0 {
            continue;
        }
        if is_white(o5, c5) && c5 > o4 {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_concealing_baby_swallow(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 3..n {
        let (o1, h1, l1, c1) = (o[i - 3], h[i - 3], l[i - 3], c[i - 3]);
        let (o2, h2, l2, c2) = (o[i - 2], h[i - 2], l[i - 2], c[i - 2]);
        let (o3, h3, l3, c3) = (o[i - 1], h[i - 1], l[i - 1], c[i - 1]);
        let (o4, h4, l4, c4) = (o[i], h[i], l[i], c[i]);
        if !(is_black(o1, c1) && is_black(o2, c2) && is_black(o3, c3) && is_black(o4, c4)) {
            continue;
        }
        if !(is_marubozu(o1, h1, l1, c1) && is_marubozu(o2, h2, l2, c2)) {
            continue;
        }
        if upper(o3, h3, c3) <= 0.0 {
            continue;
        }
        if h4 >= h3 && l4 <= l3 {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_rise_fall_three_methods(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, h, l, c) = (&x.o, &x.h, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 4..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, h1, l1, c1) = (o[i - 4], h[i - 4], l[i - 4], c[i - 4]);
        let (o2, h2, l2, c2) = (o[i - 3], h[i - 3], l[i - 3], c[i - 3]);
        let (o3, h3, l3, c3) = (o[i - 2], h[i - 2], l[i - 2], c[i - 2]);
        let (o4, h4, l4, c4) = (o[i - 1], h[i - 1], l[i - 1], c[i - 1]);
        let (o5, c5) = (o[i], c[i]);
        let b1 = body(o1, c1);
        if b1 < a {
            continue;
        }
        if body(o2, c2) >= 0.5 * a || body(o3, c3) >= 0.5 * a || body(o4, c4) >= 0.5 * a {
            continue;
        }
        if is_white(o1, c1)
            && is_black(o2, c2)
            && is_black(o3, c3)
            && is_black(o4, c4)
            && is_white(o5, c5)
            && body(o5, c5) >= a
            && l2 > l1
            && h2 < h1
            && l3 > l1
            && h3 < h1
            && l4 > l1
            && h4 < h1
            && c5 > c1
        {
            out[i] = 100.0;
        } else if is_black(o1, c1)
            && is_white(o2, c2)
            && is_white(o3, c3)
            && is_white(o4, c4)
            && is_black(o5, c5)
            && body(o5, c5) >= a
            && h2 < h1
            && l2 > l1
            && h3 < h1
            && l3 > l1
            && h4 < h1
            && l4 > l1
            && c5 < c1
        {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_mat_hold(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, l, c) = (&x.o, &x.l, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 4..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 4], c[i - 4]);
        let (o2, c2) = (o[i - 3], c[i - 3]);
        let (o3, l3, c3) = (o[i - 2], l[i - 2], c[i - 2]);
        let (o4, l4, c4) = (o[i - 1], l[i - 1], c[i - 1]);
        let (o5, c5) = (o[i], c[i]);
        if !is_white(o1, c1) || body(o1, c1) < a {
            continue;
        }
        if !is_black(o2, c2) || body(o2, c2) >= 0.5 * a || o2 <= c1 {
            continue;
        }
        if body(o3, c3) >= 0.5 * a || body(o4, c4) >= 0.5 * a {
            continue;
        }
        if l3 <= o1 || l4 <= o1 {
            continue;
        }
        if is_white(o5, c5) && body(o5, c5) >= a && c5 > c1 {
            out[i] = 100.0;
        }
    }
    vec![out]
}

fn batch_hikkake(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (h, l, c) = (&x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 3..n {
        let (h1, l1) = (h[i - 3], l[i - 3]);
        let (h2, l2) = (h[i - 2], l[i - 2]);
        let (h3, l3) = (h[i - 1], l[i - 1]);
        let c4 = c[i];
        if h2 >= h1 || l2 <= l1 {
            continue;
        }
        if l3 < l2 && c4 > h2 {
            out[i] = 100.0;
        } else if h3 > h2 && c4 < l2 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_hikkake_mod(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (h, l, c) = (&x.h, &x.l, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 4..n {
        let (h1, l1) = (h[i - 4], l[i - 4]);
        let (h2, l2) = (h[i - 3], l[i - 3]);
        let (h3, l3) = (h[i - 2], l[i - 2]);
        let c4 = c[i - 1];
        let c5 = c[i];
        if h2 >= h1 || l2 <= l1 {
            continue;
        }
        if l3 < l2 && c4 > h2 && c5 > c4 {
            out[i] = 100.0;
        } else if h3 > h2 && c4 < l2 && c5 < c4 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_xside_gap_three_methods(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let mut out = vec![0.0; n];
    for i in 2..n {
        let (o1, c1) = (o[i - 2], c[i - 2]);
        let (o2, c2) = (o[i - 1], c[i - 1]);
        let (o3, c3) = (o[i], c[i]);
        if is_white(o1, c1) && is_white(o2, c2) && o2 > c1 && is_black(o3, c3) && c3 > c1 {
            out[i] = 100.0;
        } else if is_black(o1, c1) && is_black(o2, c2) && o2 < c1 && is_white(o3, c3) && c3 < c1 {
            out[i] = -100.0;
        }
    }
    vec![out]
}

fn batch_breakaway(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (o, c) = (&x.o, &x.c);
    let n = c.len();
    let avg = avg_body(o, c);
    let mut out = vec![0.0; n];
    for i in 4..n {
        let a = avg[i];
        if a.is_nan() || a <= 0.0 {
            continue;
        }
        let (o1, c1) = (o[i - 4], c[i - 4]);
        let (o2, c2) = (o[i - 3], c[i - 3]);
        let (o3, c3) = (o[i - 2], c[i - 2]);
        let (o4, c4) = (o[i - 1], c[i - 1]);
        let (o5, c5) = (o[i], c[i]);
        if is_black(o1, c1)
            && body(o1, c1) >= 0.7 * a
            && is_black(o2, c2)
            && is_black(o3, c3)
            && is_black(o4, c4)
            && c1 > c2
            && c2 > c3
            && c3 > c4
            && is_white(o5, c5)
            && c5 >= o2
        {
            out[i] = 100.0;
        } else if is_white(o1, c1)
            && body(o1, c1) >= 0.7 * a
            && is_white(o2, c2)
            && is_white(o3, c3)
            && is_white(o4, c4)
            && c1 < c2
            && c2 < c3
            && c3 < c4
            && is_black(o5, c5)
            && c5 <= o2
        {
            out[i] = -100.0;
        }
    }
    vec![out]
}

// ======================= streaming structs (macro) =========================

hist_indicator! {
    /// `doji` â tiny body relative to average body (presence marker).
    Doji, "doji", 1, [], |bars| batch_doji(bars)
}
hist_indicator! {
    /// `engulfing` â current body engulfs prior opposite-colour body.
    Engulfing, "engulfing", 1, [], |bars| batch_engulfing(bars)
}
hist_indicator! {
    /// `hammer` â small body, long lower shadow (bullish).
    Hammer, "hammer", 1, [], |bars| batch_hammer(bars)
}
hist_indicator! {
    /// `inverted_hammer` â small body, long upper shadow (bullish).
    InvertedHammer, "inverted_hammer", 1, [], |bars| batch_inverted_hammer(bars)
}
hist_indicator! {
    /// `hanging_man` â hammer geometry as a bearish signal.
    HangingMan, "hanging_man", 1, [], |bars| batch_hanging_man(bars)
}
hist_indicator! {
    /// `shooting_star` â inverted-hammer geometry as a bearish signal.
    ShootingStar, "shooting_star", 1, [], |bars| batch_shooting_star(bars)
}
hist_indicator! {
    /// `dragonfly_doji` â doji with long lower shadow (bullish).
    DragonflyDoji, "dragonfly_doji", 1, [], |bars| batch_dragonfly_doji(bars)
}
hist_indicator! {
    /// `gravestone_doji` â doji with long upper shadow (bearish).
    GravestoneDoji, "gravestone_doji", 1, [], |bars| batch_gravestone_doji(bars)
}
hist_indicator! {
    /// `longlegged_doji` â doji with long upper AND lower shadows.
    LongleggedDoji, "longlegged_doji", 1, [], |bars| batch_longlegged_doji(bars)
}
hist_indicator! {
    /// `rickshaw_man` â long-legged doji with body near the range middle.
    RickshawMan, "rickshaw_man", 1, [], |bars| batch_rickshaw_man(bars)
}
hist_indicator! {
    /// `takuri` â dragonfly doji with an exceptionally long lower shadow.
    Takuri, "takuri", 1, [], |bars| batch_takuri(bars)
}
hist_indicator! {
    /// `marubozu` â body â full range; white +100 / black -100.
    Marubozu, "marubozu", 1, [], |bars| batch_marubozu(bars)
}
hist_indicator! {
    /// `closing_marubozu` â no shadow on the close side.
    ClosingMarubozu, "closing_marubozu", 1, [], |bars| batch_closing_marubozu(bars)
}
hist_indicator! {
    /// `spinning_top` â small body, both shadows larger than body.
    SpinningTop, "spinning_top", 1, [], |bars| batch_spinning_top(bars)
}
hist_indicator! {
    /// `high_wave` â very small body, very long upper AND lower shadows.
    HighWave, "high_wave", 1, [], |bars| batch_high_wave(bars)
}
hist_indicator! {
    /// `long_line` â long candle (body â¥ 1.3Ã avg); white +100 / black -100.
    LongLine, "long_line", 1, [], |bars| batch_long_line(bars)
}
hist_indicator! {
    /// `short_line` â short, compact candle (presence).
    ShortLine, "short_line", 1, [], |bars| batch_short_line(bars)
}
hist_indicator! {
    /// `belt_hold` â long body opening at its extreme; white +100 / black -100.
    BeltHold, "belt_hold", 1, [], |bars| batch_belt_hold(bars)
}
hist_indicator! {
    /// `opening_marubozu` â no shadow on the open side.
    OpeningMarubozu, "opening_marubozu", 1, [], |bars| batch_opening_marubozu(bars)
}
hist_indicator! {
    /// `doji_star` â doji gapping away from a prior long body.
    DojiStar, "doji_star", 1, [], |bars| batch_doji_star(bars)
}
hist_indicator! {
    /// `harami` â current body inside prior opposite-colour body.
    Harami, "harami", 1, [], |bars| batch_harami(bars)
}
hist_indicator! {
    /// `harami_cross` â harami where the current bar is a doji.
    HaramiCross, "harami_cross", 1, [], |bars| batch_harami_cross(bars)
}
hist_indicator! {
    /// `piercing` â bullish penetration above prior midpoint.
    Piercing, "piercing", 1, [], |bars| batch_piercing(bars)
}
hist_indicator! {
    /// `dark_cloud_cover` â bearish penetration below prior midpoint.
    DarkCloudCover, "dark_cloud_cover", 1, [], |bars| batch_dark_cloud_cover(bars)
}
hist_indicator! {
    /// `counterattack` â opposite-colour bodies closing at âsame price.
    Counterattack, "counterattack", 1, [], |bars| batch_counterattack(bars)
}
hist_indicator! {
    /// `meeting_lines` â counterattack with a tighter close tolerance.
    MeetingLines, "meeting_lines", 1, [], |bars| batch_meeting_lines(bars)
}
hist_indicator! {
    /// `separating_lines` â same-colour continuation opening at prior open.
    SeparatingLines, "separating_lines", 1, [], |bars| batch_separating_lines(bars)
}
hist_indicator! {
    /// `matching_low` â two black candles with equal closes (bullish).
    MatchingLow, "matching_low", 1, [], |bars| batch_matching_low(bars)
}
hist_indicator! {
    /// `on_neck` â bearish continuation closing at âprior low.
    OnNeck, "on_neck", 1, [], |bars| batch_on_neck(bars)
}
hist_indicator! {
    /// `in_neck` â bearish continuation closing slightly into prior body.
    InNeck, "in_neck", 1, [], |bars| batch_in_neck(bars)
}
hist_indicator! {
    /// `thrusting` â bearish continuation closing below prior midpoint.
    Thrusting, "thrusting", 1, [], |bars| batch_thrusting(bars)
}
hist_indicator! {
    /// `kicking` â opposite-colour marubozu with a gap.
    Kicking, "kicking", 1, [], |bars| batch_kicking(bars)
}
hist_indicator! {
    /// `kicking_by_length` â kicking, signal by the longer marubozu's colour.
    KickingByLength, "kicking_by_length", 1, [], |bars| batch_kicking_by_length(bars)
}
hist_indicator! {
    /// `homing_pigeon` â two blacks, second harami-inside the first (bullish).
    HomingPigeon, "homing_pigeon", 1, [], |bars| batch_homing_pigeon(bars)
}
hist_indicator! {
    /// `gap_side_side_white` â two similar-size white candles gapping up.
    GapSideSideWhite, "gap_side_side_white", 1, [], |bars| batch_gap_side_side_white(bars)
}
hist_indicator! {
    /// `tasuki_gap` â gap then opposite-colour candle staying in the gap.
    TasukiGap, "tasuki_gap", 1, [], |bars| batch_tasuki_gap(bars)
}
hist_indicator! {
    /// `morning_star` â long black, star, long white (bullish).
    MorningStar, "morning_star", 1, [], |bars| batch_morning_star(bars)
}
hist_indicator! {
    /// `evening_star` â long white, star, long black (bearish).
    EveningStar, "evening_star", 1, [], |bars| batch_evening_star(bars)
}
hist_indicator! {
    /// `morning_doji_star` â morning star whose star is a doji.
    MorningDojiStar, "morning_doji_star", 1, [], |bars| batch_morning_doji_star(bars)
}
hist_indicator! {
    /// `evening_doji_star` â evening star whose star is a doji.
    EveningDojiStar, "evening_doji_star", 1, [], |bars| batch_evening_doji_star(bars)
}
hist_indicator! {
    /// `three_white_soldiers` â three progressing long white candles.
    ThreeWhiteSoldiers, "three_white_soldiers", 1, [], |bars| batch_three_white_soldiers(bars)
}
hist_indicator! {
    /// `three_black_crows` â three progressing long black candles.
    ThreeBlackCrows, "three_black_crows", 1, [], |bars| batch_three_black_crows(bars)
}
hist_indicator! {
    /// `identical_three_crows` â three crows each opening â at prior close.
    IdenticalThreeCrows, "identical_three_crows", 1, [], |bars| batch_identical_three_crows(bars)
}
hist_indicator! {
    /// `three_inside` â harami then confirming third bar.
    ThreeInside, "three_inside", 1, [], |bars| batch_three_inside(bars)
}
hist_indicator! {
    /// `three_outside` â engulfing then confirming third bar.
    ThreeOutside, "three_outside", 1, [], |bars| batch_three_outside(bars)
}
hist_indicator! {
    /// `three_line_strike` â three trend candles then an engulfing fourth.
    ThreeLineStrike, "three_line_strike", 1, [], |bars| batch_three_line_strike(bars)
}
hist_indicator! {
    /// `three_stars_in_south` â three black candles of diminishing range.
    ThreeStarsInSouth, "three_stars_in_south", 1, [], |bars| batch_three_stars_in_south(bars)
}
hist_indicator! {
    /// `abandoned_baby` â doji island reversal with gaps on both sides.
    AbandonedBaby, "abandoned_baby", 1, [], |bars| batch_abandoned_baby(bars)
}
hist_indicator! {
    /// `advance_block` â three whites weakening (bearish warning).
    AdvanceBlock, "advance_block", 1, [], |bars| batch_advance_block(bars)
}
hist_indicator! {
    /// `stalled_pattern` â two long whites then a small stalling white.
    StalledPattern, "stalled_pattern", 1, [], |bars| batch_stalled_pattern(bars)
}
hist_indicator! {
    /// `two_crows` â long white, gap-up black, black closing into bar1 body.
    TwoCrows, "two_crows", 1, [], |bars| batch_two_crows(bars)
}
hist_indicator! {
    /// `upside_gap_two_crows` â gap-up black then engulfing black in the gap.
    UpsideGapTwoCrows, "upside_gap_two_crows", 1, [], |bars| batch_upside_gap_two_crows(bars)
}
hist_indicator! {
    /// `tristar` â three dojis with a gapping middle (reversal by direction).
    Tristar, "tristar", 1, [], |bars| batch_tristar(bars)
}
hist_indicator! {
    /// `unique_three_river` â long black, lower-low black, small white (bullish).
    UniqueThreeRiver, "unique_three_river", 1, [], |bars| batch_unique_three_river(bars)
}
hist_indicator! {
    /// `stick_sandwich` â black/white/black with equal outer closes (bullish).
    StickSandwich, "stick_sandwich", 1, [], |bars| batch_stick_sandwich(bars)
}
hist_indicator! {
    /// `ladder_bottom` â four stepping blacks then a white reversal (bullish).
    LadderBottom, "ladder_bottom", 1, [], |bars| batch_ladder_bottom(bars)
}
hist_indicator! {
    /// `concealing_baby_swallow` â four blacks, marubozu pair, engulfing fourth.
    ConcealingBabySwallow, "concealing_baby_swallow", 1, [], |bars| batch_concealing_baby_swallow(bars)
}
hist_indicator! {
    /// `rise_fall_three_methods` â 5-bar continuation (rising/falling).
    RiseFallThreeMethods, "rise_fall_three_methods", 1, [], |bars| batch_rise_fall_three_methods(bars)
}
hist_indicator! {
    /// `mat_hold` â bullish gap variant of rising three methods.
    MatHold, "mat_hold", 1, [], |bars| batch_mat_hold(bars)
}
hist_indicator! {
    /// `hikkake` â inside bar then false breakout that reverses.
    Hikkake, "hikkake", 1, [], |bars| batch_hikkake(bars)
}
hist_indicator! {
    /// `hikkake_mod` â modified 5-bar hikkake with a confirming bar.
    HikkakeMod, "hikkake_mod", 1, [], |bars| batch_hikkake_mod(bars)
}
hist_indicator! {
    /// `xside_gap_three_methods` â gap then a partial-fill continuation candle.
    XsideGapThreeMethods, "xside_gap_three_methods", 1, [], |bars| batch_xside_gap_three_methods(bars)
}
hist_indicator! {
    /// `breakaway` â 5-bar gap run then a reversal closing into the gap.
    Breakaway, "breakaway", 1, [], |bars| batch_breakaway(bars)
}

#[cfg(test)]
mod flip_rate {
    use super::*;

    /// The OLD `math::sma`, as it stood before #1200 de-accumulated it: one running sum, slid one
    /// bar at a time. Kept here ONLY as the thing being measured against â nothing in the tree
    /// computes this way any more.
    fn sma_accumulated(c: &[f64], n: usize) -> Vec<f64> {
        let mut out = vec![f64::NAN; c.len()];
        if c.len() < n || n == 0 {
            return out;
        }
        let mut sum: f64 = c[..n].iter().sum();
        out[n - 1] = sum / n as f64;
        for i in n..c.len() {
            sum += c[i] - c[i - n];
            out[i] = sum / n as f64;
        }
        out
    }

    /// A deterministic OHLC random walk. No `rand` dependency and no seed plumbing: an LCG with a
    /// fixed constant is reproducible across platforms, which a float-hashing scheme would not be.
    ///
    /// â  The BODY DISTRIBUTION is what this measurement is sensitive to, so it is chosen rather
    /// than accepted: `|close - open|` spans three orders of magnitude here (a `u >= 0.82` arm
    /// makes ~18% of bars near-doji), because a flip can only happen to a bar whose body sits
    /// within the two kernels' disagreement of a threshold. A synthetic series of uniformly-fat
    /// candles would report a flip rate near zero and would be measuring its own fixture.
    fn walk(n: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
        let (mut o, mut h, mut l, mut c) = (vec![], vec![], vec![], vec![]);
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            // ⚠ FULL 52-bit mantissa, not a 31-bit slice. A coarse LCG slice makes every body a
            // dyadic rational of ~32 significant bits, and a ten-term sum of those is EXACT in
            // f64, so both kernels agree bit-for-bit and the measurement silently reports zero.
            ((state >> 11) as f64) / ((1u64 << 53) as f64)
        };
        let mut px = 64_000.0_f64;
        for _ in 0..n {
            let open = px;
            let u = next();
            // Three regimes, so the body distribution is wide: near-doji, ordinary, and fat.
            // Non-dyadic scales, for the same reason: a power-of-two scale preserves exactness.
            let scale = if u >= 0.82 {
                0.019
            } else if u >= 0.35 {
                3.7
            } else {
                21.3
            };
            let close = open + (next() - 0.5) * scale;
            let wick = next() * 6.0;
            o.push(open);
            c.push(close);
            h.push(open.max(close) + wick);
            l.push(open.min(close) - wick * next());
            px = close;
        }
        (o, h, l, c)
    }

    /// â  **The number #1200 did not produce.** It stated that de-accumulating `sma` "WILL flip some
    /// +/-100 signals on bars that sat exactly on a threshold" â true, and unactionable, because
    /// nobody could tell whether that meant one bar in a million or one in twenty.
    ///
    /// Every one of the ~40 `avg_body` comparison sites in this file has the SAME shape â `body`
    /// against `k * avg` for one of nine constants `k` â so the flip condition is exact and needs
    /// no pattern kernel to evaluate: bar `i` flips for multiplier `k` iff `body[i]` falls strictly
    /// between `k * avg_old[i]` and `k * avg_new[i]`. Counting that over every `k` measures every
    /// site at once.
    ///
    /// `#[ignore]`d: it is a MEASUREMENT, not a gate. The kernel it compares against no longer
    /// exists, so a permanent assertion here would be pinning a historical artifact rather than a
    /// property of the code. Run it with `--ignored --nocapture`.
    #[test]
    #[ignore = "measurement, not a gate: run with --ignored --nocapture"]
    fn measure_the_threshold_flip_rate_from_deaccumulating_sma() {
        // The nine multipliers actually used, with their site counts (`grep -o '[0-9.]*\s*\*\s*a'`).
        const KS: &[(f64, usize)] = &[
            (0.1, 13),
            (0.7, 11),
            (0.5, 8),
            (0.3, 6),
            (0.03, 2),
            (0.01, 2),
            (1.3, 1),
            (0.05, 1),
            (0.02, 1),
        ];

        for &n in &[10_000usize, 200_000] {
            let (o, h, l, c) = walk(n);
            let bodies: Vec<f64> = (0..n).map(|i| (c[i] - o[i]).abs()).collect();
            let new = sma(&bodies, CTX);
            let old = sma_accumulated(&bodies, CTX);

            let differing_avg = (0..n)
                .filter(|&i| !new[i].is_nan() && new[i].to_bits() != old[i].to_bits())
                .count();

            let mut total_flips = 0usize;
            let mut weighted = 0usize;
            println!("\n=== {n} bars ===");
            println!("avg_body values differing in bits: {differing_avg} / {n}");
            // ⚠ NON-VACUITY, asserted before any zero above is believed — and asserted on an
            // ADVERSARIAL input rather than on the realistic one, which is the whole subtlety here.
            //
            // The obvious guard, `assert!(differing_avg > 0)`, is WRONG: it presumes the realistic
            // fixture must produce a divergence, so a genuine finding of "these agree on real data"
            // is indistinguishable from a broken generator. The first two versions of this test hit
            // both sides of that. What must be proven is that the DETECTOR works — that
            // `sma_accumulated` really accumulates — and that is a property of the reconstruction,
            // provable on an input built for it.
            //
            // 1e16 enters the window, swamps the small terms, then leaves: the classic
            // catastrophic-cancellation shape a running sum cannot survive and a fresh sum does not
            // see. If these agreed, the reconstruction would not be an accumulator at all.
            let mut adv = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
            adv.push(1e16);
            adv.extend([0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]);
            let (an, ao) = (sma(&adv, CTX), sma_accumulated(&adv, CTX));
            let adv_diff = (0..adv.len())
                .filter(|&i| !an[i].is_nan() && an[i].to_bits() != ao[i].to_bits())
                .count();
            assert!(
                adv_diff > 0,
                "the DETECTOR is broken, so no count here means anything: `sma_accumulated` agrees \
                 with `sma` even on an input engineered for catastrophic cancellation, which an \
                 accumulating kernel cannot do. Fix the reconstruction before reading any number."
            );
            println!("detector check (adversarial input): {adv_diff} / {} bars differ", adv.len());
            for &(k, sites) in KS {
                let flips = (0..n)
                    .filter(|&i| {
                        !new[i].is_nan()
                            && new[i] > 0.0
                            && (bodies[i] <= k * old[i]) != (bodies[i] <= k * new[i])
                    })
                    .count();
                total_flips += flips;
                weighted += flips * sites;
                println!("  k={k:<5} ({sites:2} sites): {flips} flip(s)");
            }
            println!("distinct (bar, k) flips: {total_flips}; site-weighted: {weighted}");

            // `is_doji` is the single most-used gate (13 sites); evaluate it end-to-end rather than
            // via the generic condition, so the arithmetic above is cross-checked by real code.
            let doji_flips = (0..n)
                .filter(|&i| {
                    is_doji(o[i], h[i], l[i], c[i], old[i])
                        != is_doji(o[i], h[i], l[i], c[i], new[i])
                })
                .count();
            println!("is_doji() disagreements (real kernel): {doji_flips} / {n}");
        }
    }
}

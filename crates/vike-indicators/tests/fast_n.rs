//! The 27 candlestick patterns of `three_bar.rs` and `four_five_bar.rs` (group `n`, fourth wave):
//! the bounded-state steppers against a COPY OF THE OLD HISTORY-RECOMPUTE KERNELS kept in this file
//! (the old code is gone from `src/`), bit for bit, because the signal is discrete (`+100`, `-100`,
//! `0`) and does not get a tolerance.
//!
//! Every way in is compared with the old kernel on the same bars: the stream (`next(&Bar)`), the
//! plain-number entry (`next_ohlc`), `Indicator::on_bar` of the registry object, `vectorize` and the
//! slice call (`series::<name>`), over several generators (a coarse price grid so that equalities
//! and ties are common, trending runs, doji and marubozu mixes, two price scales, and `NaN` / infinity
//! / `-0.0` injected into any of the four prices) and over prefixes of every short length. A pattern
//! that never fires on the data would prove nothing, so each one must fire at least once.

#![allow(
    clippy::needless_range_loop,
    clippy::collapsible_if,
    clippy::type_complexity,
    unused_variables,
    dead_code
)]

use vike_indicators::indicators::{
    AbandonedBaby, AdvanceBlock, Breakaway, ConcealingBabySwallow, EveningDojiStar, EveningStar,
    Hikkake, HikkakeMod, IdenticalThreeCrows, LadderBottom, MatHold, MorningDojiStar, MorningStar,
    RiseFallThreeMethods, StalledPattern, StickSandwich, ThreeBlackCrows, ThreeInside,
    ThreeLineStrike, ThreeOutside, ThreeStarsInSouth, ThreeWhiteSoldiers, Tristar, TwoCrows,
    UniqueThreeRiver, UpsideGapTwoCrows, XsideGapThreeMethods,
};
use vike_indicators::{Indicator, make, series};
use vike_marketdata::Bar;

// ------------------------------------------------------------------ the old kernels, copied

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
fn is_marubozu(o: f64, h: f64, l: f64, c: f64) -> bool {
    let r = rng(h, l);
    if r <= 0.0 {
        return false;
    }
    upper(o, h, c) <= 0.05 * r && lower(o, l, c) <= 0.05 * r
}

/// `math::sma`: every window folded from scratch with `iter().sum()`.
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

/// `patterns.rs`'s `avg_body`.
fn avg_body(opens: &[f64], closes: &[f64]) -> Vec<f64> {
    let bodies: Vec<f64> = (0..closes.len()).map(|i| (closes[i] - opens[i]).abs()).collect();
    sma(&bodies, CTX)
}

fn old_morning_star(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_evening_star(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_morning_doji_star(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_evening_doji_star(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_three_white_soldiers(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_three_black_crows(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_identical_three_crows(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_three_inside(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_three_outside(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_three_stars_in_south(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_abandoned_baby(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_advance_block(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_stalled_pattern(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_two_crows(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_upside_gap_two_crows(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_tristar(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_unique_three_river(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_stick_sandwich(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_xside_gap_three_methods(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_three_line_strike(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_ladder_bottom(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_concealing_baby_swallow(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_rise_fall_three_methods(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_mat_hold(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_hikkake(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_hikkake_mod(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

fn old_breakaway(o: &[f64], h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
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
    out
}

// ------------------------------------------------------------------------------------ the data

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

type Ohlc = [f64; 4];

/// Prices on a grid of `step`, so that ties are common. `open_at_close` is the chance a bar opens at
/// the previous close, `bare` the chance a bar has no shadow on a side, `doji` the chance of a body
/// of zero.
fn grid(seed: u64, n: usize, step: f64, open_at_close: f64, bare: f64, doji: f64) -> Vec<Ohlc> {
    let mut r = Rng(seed);
    let mut prev = 10.0 * step;
    (0..n)
        .map(|_| {
            let o = if r.unit() < open_at_close { prev } else { (6 + r.below(9)) as f64 * step };
            let c = if r.unit() < doji { o } else { (6 + r.below(9)) as f64 * step };
            let up = if r.unit() < bare { 0.0 } else { r.below(4) as f64 * step };
            let dn = if r.unit() < bare { 0.0 } else { r.below(4) as f64 * step };
            prev = c;
            [o, o.max(c) + up, o.min(c) - dn, c]
        })
        .collect()
}

/// Runs of one colour with gaps and varying bodies and shadows.
fn runs(seed: u64, n: usize, level: f64) -> Vec<Ohlc> {
    let mut r = Rng(seed);
    let mut p = level;
    let mut dir = 1.0;
    (0..n)
        .map(|_| {
            if r.unit() < 0.25 {
                dir = -dir;
            }
            let o = p + (r.unit() - 0.5) * 0.4 * level * 0.01;
            let b = if r.unit() < 0.1 { 0.0 } else { r.unit() * 3.0 * level * 0.01 };
            let c = o + dir * b;
            let up = if r.unit() < 0.3 { 0.0 } else { r.unit() * level * 0.005 };
            let dn = if r.unit() < 0.3 { 0.0 } else { r.unit() * level * 0.005 };
            p = c;
            [o, o.max(c) + up, o.min(c) - dn, c]
        })
        .collect()
}

/// `NaN`, infinities and `-0.0` dropped into random prices.
fn dirty(seed: u64, n: usize) -> Vec<Ohlc> {
    let mut r = Rng(seed ^ 0xabcdef);
    let mut v = grid(seed, n, 1.0, 0.4, 0.5, 0.2);
    for bar in v.iter_mut() {
        if r.unit() < 0.04 {
            let which = r.below(4) as usize;
            bar[which] = match r.below(5) {
                0 => f64::NAN,
                1 => f64::INFINITY,
                2 => f64::NEG_INFINITY,
                3 => -0.0,
                _ => 0.0,
            };
        }
    }
    v
}

fn data() -> Vec<(&'static str, Vec<Ohlc>)> {
    vec![
        ("grid-1", grid(1, 60_000, 1.0, 0.3, 0.3, 0.1)),
        ("grid-2-bare", grid(2, 60_000, 1.0, 0.5, 0.7, 0.25)),
        ("grid-3-gappy", grid(3, 60_000, 1.0, 0.0, 0.5, 0.15)),
        ("grid-1e6", grid(4, 20_000, 1.0e6, 0.3, 0.3, 0.1)),
        ("grid-1e-3", grid(5, 20_000, 1.0e-3, 0.3, 0.3, 0.1)),
        ("runs-100", runs(6, 60_000, 100.0)),
        ("runs-1e6", runs(7, 20_000, 1.0e6)),
        ("dirty", dirty(8, 60_000)),
        ("dirty-2", dirty(9, 30_000)),
    ]
}

fn cols(v: &[Ohlc]) -> [Vec<f64>; 4] {
    [
        v.iter().map(|b| b[0]).collect(),
        v.iter().map(|b| b[1]).collect(),
        v.iter().map(|b| b[2]).collect(),
        v.iter().map(|b| b[3]).collect(),
    ]
}

fn bars(v: &[Ohlc]) -> Vec<Bar> {
    v.iter()
        .enumerate()
        .map(|(i, b)| Bar::ohlcv(i as i64 * 60_000, b[0], b[1], b[2], b[3], 1.0))
        .collect()
}

// ----------------------------------------------------------------------------------- the cases

type Old = fn(&[f64], &[f64], &[f64], &[f64]) -> Vec<f64>;
type Series = fn(&[f64], &[f64], &[f64], &[f64]) -> Vec<f64>;

struct Case {
    key: &'static str,
    old: Old,
    series: Series,
    /// `next(&Bar)` over the bars.
    stream: fn(&[Bar]) -> Vec<f64>,
    /// `next_ohlc` over the bars.
    plain: fn(&[Bar]) -> Vec<f64>,
    /// `Indicator::vectorize` on a default instance.
    batch: fn(&[Bar]) -> Vec<f64>,
}

macro_rules! case {
    ($Ty:ident, $key:literal, $old:ident, $series:ident) => {
        Case {
            key: $key,
            old: $old,
            series: series::$series,
            stream: |b| {
                let mut s = $Ty::default();
                b.iter().map(|x| s.next(x)).collect()
            },
            plain: |b| {
                let mut s = $Ty::default();
                b.iter().map(|x| s.next_ohlc(x.open, x.high, x.low, x.close)).collect()
            },
            batch: |b| $Ty::default().vectorize(b).remove(0),
        }
    };
}

fn cases() -> Vec<Case> {
    vec![
        case!(MorningStar, "morning_star", old_morning_star, morning_star),
        case!(EveningStar, "evening_star", old_evening_star, evening_star),
        case!(MorningDojiStar, "morning_doji_star", old_morning_doji_star, morning_doji_star),
        case!(EveningDojiStar, "evening_doji_star", old_evening_doji_star, evening_doji_star),
        case!(
            ThreeWhiteSoldiers,
            "three_white_soldiers",
            old_three_white_soldiers,
            three_white_soldiers
        ),
        case!(ThreeBlackCrows, "three_black_crows", old_three_black_crows, three_black_crows),
        case!(
            IdenticalThreeCrows,
            "identical_three_crows",
            old_identical_three_crows,
            identical_three_crows
        ),
        case!(ThreeInside, "three_inside", old_three_inside, three_inside),
        case!(ThreeOutside, "three_outside", old_three_outside, three_outside),
        case!(
            ThreeStarsInSouth,
            "three_stars_in_south",
            old_three_stars_in_south,
            three_stars_in_south
        ),
        case!(AbandonedBaby, "abandoned_baby", old_abandoned_baby, abandoned_baby),
        case!(AdvanceBlock, "advance_block", old_advance_block, advance_block),
        case!(StalledPattern, "stalled_pattern", old_stalled_pattern, stalled_pattern),
        case!(TwoCrows, "two_crows", old_two_crows, two_crows),
        case!(
            UpsideGapTwoCrows,
            "upside_gap_two_crows",
            old_upside_gap_two_crows,
            upside_gap_two_crows
        ),
        case!(Tristar, "tristar", old_tristar, tristar),
        case!(UniqueThreeRiver, "unique_three_river", old_unique_three_river, unique_three_river),
        case!(StickSandwich, "stick_sandwich", old_stick_sandwich, stick_sandwich),
        case!(
            XsideGapThreeMethods,
            "xside_gap_three_methods",
            old_xside_gap_three_methods,
            xside_gap_three_methods
        ),
        case!(ThreeLineStrike, "three_line_strike", old_three_line_strike, three_line_strike),
        case!(LadderBottom, "ladder_bottom", old_ladder_bottom, ladder_bottom),
        case!(
            ConcealingBabySwallow,
            "concealing_baby_swallow",
            old_concealing_baby_swallow,
            concealing_baby_swallow
        ),
        case!(
            RiseFallThreeMethods,
            "rise_fall_three_methods",
            old_rise_fall_three_methods,
            rise_fall_three_methods
        ),
        case!(MatHold, "mat_hold", old_mat_hold, mat_hold),
        case!(Hikkake, "hikkake", old_hikkake, hikkake),
        case!(HikkakeMod, "hikkake_mod", old_hikkake_mod, hikkake_mod),
        case!(Breakaway, "breakaway", old_breakaway, breakaway),
    ]
}

fn same(a: &[f64], b: &[f64]) -> Option<usize> {
    if a.len() != b.len() {
        return Some(usize::MAX);
    }
    a.iter().zip(b).position(|(x, y)| x.to_bits() != y.to_bits() && !(x.is_nan() && y.is_nan()))
}

fn assert_same(what: &str, a: &[f64], b: &[f64]) {
    if let Some(i) = same(a, b) {
        panic!(
            "{what}: lengths {} vs {}, first difference at {i}: {:?} vs {:?}",
            a.len(),
            b.len(),
            a.get(i),
            b.get(i)
        );
    }
}

// ------------------------------------------------------------------------------------- the tests

#[test]
fn every_way_in_equals_the_old_kernel_bit_for_bit() {
    let cs = cases();
    assert_eq!(cs.len(), 27);
    let mut fired = vec![[0usize; 2]; cs.len()];
    for (label, v) in data() {
        let [o, h, l, c] = cols(&v);
        let bs = bars(&v);
        for (k, case) in cs.iter().enumerate() {
            let old = (case.old)(&o, &h, &l, &c);
            let what = |way: &str| format!("{} / {label} / {way}", case.key);
            assert_same(&what("series"), &(case.series)(&o, &h, &l, &c), &old);
            assert_same(&what("next(&Bar)"), &(case.stream)(&bs), &old);
            assert_same(&what("next_ohlc"), &(case.plain)(&bs), &old);
            assert_same(&what("vectorize"), &(case.batch)(&bs), &old);
            let mut reg = make(case.key).unwrap();
            let via_registry: Vec<f64> = bs.iter().map(|b| reg.on_bar(b)[0]).collect();
            assert_same(&what("registry on_bar"), &via_registry, &old);
            let by_name = make(case.key).unwrap().vectorize(&bs).remove(0);
            assert_same(&what("registry vectorize"), &by_name, &old);
            if label.starts_with("dirty") {
                continue;
            }
            fired[k][0] += old.iter().filter(|x| **x > 0.0).count();
            fired[k][1] += old.iter().filter(|x| **x < 0.0).count();
        }
    }
    for (k, case) in cs.iter().enumerate() {
        println!("{:<26} +100: {:>6}  -100: {:>6}", case.key, fired[k][0], fired[k][1]);
        assert!(fired[k][0] + fired[k][1] > 0, "{} never fires on the test data", case.key);
    }
}

/// Short prefixes (every length around the window sizes) and slices of different lengths.
#[test]
fn short_series_and_mismatched_slices() {
    let cs = cases();
    let v = grid(11, 40, 1.0, 0.3, 0.3, 0.1);
    let [o, h, l, c] = cols(&v);
    let bs = bars(&v);
    for case in &cs {
        for n in 0..=40 {
            let old = (case.old)(&o[..n], &h[..n], &l[..n], &c[..n]);
            assert_same(
                &format!("{} series len {n}", case.key),
                &(case.series)(&o[..n], &h[..n], &l[..n], &c[..n]),
                &old,
            );
            assert_same(&format!("{} stream len {n}", case.key), &(case.stream)(&bs[..n]), &old);
            assert_same(&format!("{} batch len {n}", case.key), &(case.batch)(&bs[..n]), &old);
        }
        // The result is as long as the shortest input, and equals the call on the equal prefixes.
        let got = (case.series)(&o[..30], &h[..20], &l[..25], &c[..40]);
        assert_eq!(got.len(), 20, "{}", case.key);
        let old = (case.old)(&o[..20], &h[..20], &l[..20], &c[..20]);
        assert_same(&format!("{} min length vs old", case.key), &got, &old);
        assert!((case.series)(&[], &h, &l, &c).is_empty());
    }
}

/// `value()` is `NaN` before the first bar and the last answer after, `lookback` is the old `0` and
/// not exact, `reset` brings a stepper back to construction, and a clone carries on alike.
#[test]
fn state_contract_matches_the_old_type() {
    let v = grid(12, 300, 1.0, 0.3, 0.3, 0.1);
    let bs = bars(&v);
    let [o, h, l, c] = cols(&v);
    for case in cases() {
        let mut ind = make(case.key).unwrap();
        assert!(ind.value()[0].is_nan(), "{}", case.key);
        assert_eq!(ind.lookback(), 0, "{}", case.key);
        assert_eq!(ind.lookback_full(), 0, "{}", case.key);
        assert!(!ind.lookback_exact(), "{}", case.key);
        assert!(!ind.warmup_path_dependent(), "{}", case.key);
        assert_eq!(ind.name(), case.key);
        let old = (case.old)(&o, &h, &l, &c);
        let mut half = None;
        for (i, b) in bs.iter().enumerate() {
            let got = ind.on_bar(b)[0];
            assert_eq!(got.to_bits(), old[i].to_bits(), "{} at {i}", case.key);
            assert_eq!(ind.value()[0].to_bits(), got.to_bits());
            if i == 150 {
                half = Some(ind.clone_box());
            }
        }
        let mut cl = half.unwrap();
        for (i, b) in bs.iter().enumerate().skip(151) {
            assert_eq!(cl.on_bar(b)[0].to_bits(), old[i].to_bits(), "{} clone at {i}", case.key);
        }
        ind.reset();
        assert!(ind.value()[0].is_nan());
        for (i, b) in bs.iter().enumerate() {
            assert_eq!(
                ind.on_bar(b)[0].to_bits(),
                old[i].to_bits(),
                "{} after reset at {i}",
                case.key
            );
        }
    }
}

/// Near misses: take real firing windows (15 bars ending at a bar the old kernel signals on), nudge
/// one price of one of the last five bars (a step up or down, or a tie with another price of the
/// window), and require the new kernels to agree with the old ones on all of it. Every `return 0.0`
/// of every kernel is a test the data must be able to fail by a hair.
#[test]
fn near_misses_agree_with_the_old_kernels() {
    let cs = cases();
    let mut r = Rng(0x5eed);
    let sources = [
        grid(21, 40_000, 1.0, 0.3, 0.3, 0.1),
        grid(22, 40_000, 1.0, 0.5, 0.7, 0.25),
        grid(23, 40_000, 1.0, 0.0, 0.5, 0.15),
        runs(24, 40_000, 100.0),
    ];
    let mut mutated: Vec<Ohlc> = Vec::new();
    for src in &sources {
        let [o, h, l, c] = cols(src);
        for case in &cs {
            let old = (case.old)(&o, &h, &l, &c);
            let hits: Vec<usize> = (14..old.len()).filter(|&i| old[i] != 0.0).collect();
            for &i in hits.iter().take(300) {
                for _ in 0..6 {
                    let mut w: Vec<Ohlc> = src[i - 14..=i].to_vec();
                    let j = 10 + r.below(5) as usize;
                    let f = r.below(4) as usize;
                    match r.below(3) {
                        0 => w[j][f] += 1.0,
                        1 => w[j][f] -= 1.0,
                        _ => {
                            let (j2, f2) = (10 + r.below(5) as usize, r.below(4) as usize);
                            w[j][f] = w[j2][f2];
                        }
                    }
                    mutated.extend(w);
                }
            }
        }
    }
    println!("{} near-miss bars", mutated.len());
    let [o, h, l, c] = cols(&mutated);
    let bs = bars(&mutated);
    for case in &cs {
        let old = (case.old)(&o, &h, &l, &c);
        assert_same(
            &format!("{} series (near misses)", case.key),
            &(case.series)(&o, &h, &l, &c),
            &old,
        );
        assert_same(&format!("{} stream (near misses)", case.key), &(case.stream)(&bs), &old);
        assert_same(&format!("{} plain (near misses)", case.key), &(case.plain)(&bs), &old);
        let fires = old.iter().filter(|x| **x != 0.0).count();
        println!("{:<26} fires on {fires} near-miss bars", case.key);
        assert!(fires > 0, "{} never fires on the near misses", case.key);
    }
}

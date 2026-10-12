//! Three-bar candlestick patterns: each kernel reads the current bar and the two before it.

use super::{avg_body, body, is_black, is_white, lower, rng, upper};
use crate::indicators::macros::hist_indicator;
use crate::math::Columns;
use vike_marketdata::Bar;

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

hist_indicator! {
    /// `morning_star` — long black, star, long white (bullish).
    MorningStar, "morning_star", 1, [], |bars| batch_morning_star(bars)
}
hist_indicator! {
    /// `evening_star` — long white, star, long black (bearish).
    EveningStar, "evening_star", 1, [], |bars| batch_evening_star(bars)
}
hist_indicator! {
    /// `morning_doji_star` — morning star whose star is a doji.
    MorningDojiStar, "morning_doji_star", 1, [], |bars| batch_morning_doji_star(bars)
}
hist_indicator! {
    /// `evening_doji_star` — evening star whose star is a doji.
    EveningDojiStar, "evening_doji_star", 1, [], |bars| batch_evening_doji_star(bars)
}
hist_indicator! {
    /// `three_white_soldiers` — three progressing long white candles.
    ThreeWhiteSoldiers, "three_white_soldiers", 1, [], |bars| batch_three_white_soldiers(bars)
}
hist_indicator! {
    /// `three_black_crows` — three progressing long black candles.
    ThreeBlackCrows, "three_black_crows", 1, [], |bars| batch_three_black_crows(bars)
}
hist_indicator! {
    /// `identical_three_crows` — three crows each opening ≈ at prior close.
    IdenticalThreeCrows, "identical_three_crows", 1, [], |bars| batch_identical_three_crows(bars)
}
hist_indicator! {
    /// `three_inside` — harami then confirming third bar.
    ThreeInside, "three_inside", 1, [], |bars| batch_three_inside(bars)
}
hist_indicator! {
    /// `three_outside` — engulfing then confirming third bar.
    ThreeOutside, "three_outside", 1, [], |bars| batch_three_outside(bars)
}
hist_indicator! {
    /// `three_stars_in_south` — three black candles of diminishing range.
    ThreeStarsInSouth, "three_stars_in_south", 1, [], |bars| batch_three_stars_in_south(bars)
}
hist_indicator! {
    /// `abandoned_baby` — doji island reversal with gaps on both sides.
    AbandonedBaby, "abandoned_baby", 1, [], |bars| batch_abandoned_baby(bars)
}
hist_indicator! {
    /// `advance_block` — three whites weakening (bearish warning).
    AdvanceBlock, "advance_block", 1, [], |bars| batch_advance_block(bars)
}
hist_indicator! {
    /// `stalled_pattern` — two long whites then a small stalling white.
    StalledPattern, "stalled_pattern", 1, [], |bars| batch_stalled_pattern(bars)
}
hist_indicator! {
    /// `two_crows` — long white, gap-up black, black closing into bar1 body.
    TwoCrows, "two_crows", 1, [], |bars| batch_two_crows(bars)
}
hist_indicator! {
    /// `upside_gap_two_crows` — gap-up black then engulfing black in the gap.
    UpsideGapTwoCrows, "upside_gap_two_crows", 1, [], |bars| batch_upside_gap_two_crows(bars)
}
hist_indicator! {
    /// `tristar` — three dojis with a gapping middle (reversal by direction).
    Tristar, "tristar", 1, [], |bars| batch_tristar(bars)
}
hist_indicator! {
    /// `unique_three_river` — long black, lower-low black, small white (bullish).
    UniqueThreeRiver, "unique_three_river", 1, [], |bars| batch_unique_three_river(bars)
}
hist_indicator! {
    /// `stick_sandwich` — black/white/black with equal outer closes (bullish).
    StickSandwich, "stick_sandwich", 1, [], |bars| batch_stick_sandwich(bars)
}
hist_indicator! {
    /// `xside_gap_three_methods` — gap then a partial-fill continuation candle.
    XsideGapThreeMethods, "xside_gap_three_methods", 1, [], |bars| batch_xside_gap_three_methods(bars)
}

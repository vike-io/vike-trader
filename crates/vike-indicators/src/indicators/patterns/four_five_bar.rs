//! Four- and five-bar candlestick patterns: each kernel reads three or four bars back.

use super::{avg_body, body, is_black, is_marubozu, is_white, upper};
use crate::indicators::macros::hist_indicator;
use crate::math::Columns;
use vike_marketdata::Bar;

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

hist_indicator! {
    /// `three_line_strike` — three trend candles then an engulfing fourth.
    ThreeLineStrike, "three_line_strike", 1, [], |bars| batch_three_line_strike(bars)
}
hist_indicator! {
    /// `ladder_bottom` — four stepping blacks then a white reversal (bullish).
    LadderBottom, "ladder_bottom", 1, [], |bars| batch_ladder_bottom(bars)
}
hist_indicator! {
    /// `concealing_baby_swallow` — four blacks, marubozu pair, engulfing fourth.
    ConcealingBabySwallow, "concealing_baby_swallow", 1, [], |bars| batch_concealing_baby_swallow(bars)
}
hist_indicator! {
    /// `rise_fall_three_methods` — 5-bar continuation (rising/falling).
    RiseFallThreeMethods, "rise_fall_three_methods", 1, [], |bars| batch_rise_fall_three_methods(bars)
}
hist_indicator! {
    /// `mat_hold` — bullish gap variant of rising three methods.
    MatHold, "mat_hold", 1, [], |bars| batch_mat_hold(bars)
}
hist_indicator! {
    /// `hikkake` — inside bar then false breakout that reverses.
    Hikkake, "hikkake", 1, [], |bars| batch_hikkake(bars)
}
hist_indicator! {
    /// `hikkake_mod` — modified 5-bar hikkake with a confirming bar.
    HikkakeMod, "hikkake_mod", 1, [], |bars| batch_hikkake_mod(bars)
}
hist_indicator! {
    /// `breakaway` — 5-bar gap run then a reversal closing into the gap.
    Breakaway, "breakaway", 1, [], |bars| batch_breakaway(bars)
}

//! One-bar candlestick patterns: each kernel reads only the current bar.

use super::{avg_body, body, is_black, is_doji, is_white, lower, rng, upper};
use crate::indicators::macros::hist_indicator;
use crate::math::Columns;
use vike_marketdata::Bar;

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

// ======================= streaming structs (macro) =========================

hist_indicator! {
    /// `doji` — tiny body relative to average body (presence marker).
    Doji, "doji", 1, [], |bars| batch_doji(bars)
}
hist_indicator! {
    /// `hammer` — small body, long lower shadow (bullish).
    Hammer, "hammer", 1, [], |bars| batch_hammer(bars)
}
hist_indicator! {
    /// `inverted_hammer` — small body, long upper shadow (bullish).
    InvertedHammer, "inverted_hammer", 1, [], |bars| batch_inverted_hammer(bars)
}
hist_indicator! {
    /// `hanging_man` — hammer geometry as a bearish signal.
    HangingMan, "hanging_man", 1, [], |bars| batch_hanging_man(bars)
}
hist_indicator! {
    /// `shooting_star` — inverted-hammer geometry as a bearish signal.
    ShootingStar, "shooting_star", 1, [], |bars| batch_shooting_star(bars)
}
hist_indicator! {
    /// `dragonfly_doji` — doji with long lower shadow (bullish).
    DragonflyDoji, "dragonfly_doji", 1, [], |bars| batch_dragonfly_doji(bars)
}
hist_indicator! {
    /// `gravestone_doji` — doji with long upper shadow (bearish).
    GravestoneDoji, "gravestone_doji", 1, [], |bars| batch_gravestone_doji(bars)
}
hist_indicator! {
    /// `longlegged_doji` — doji with long upper AND lower shadows.
    LongleggedDoji, "longlegged_doji", 1, [], |bars| batch_longlegged_doji(bars)
}
hist_indicator! {
    /// `rickshaw_man` — long-legged doji with body near the range middle.
    RickshawMan, "rickshaw_man", 1, [], |bars| batch_rickshaw_man(bars)
}
hist_indicator! {
    /// `takuri` — dragonfly doji with an exceptionally long lower shadow.
    Takuri, "takuri", 1, [], |bars| batch_takuri(bars)
}
hist_indicator! {
    /// `marubozu` — body ≈ full range; white +100 / black -100.
    Marubozu, "marubozu", 1, [], |bars| batch_marubozu(bars)
}
hist_indicator! {
    /// `closing_marubozu` — no shadow on the close side.
    ClosingMarubozu, "closing_marubozu", 1, [], |bars| batch_closing_marubozu(bars)
}
hist_indicator! {
    /// `spinning_top` — small body, both shadows larger than body.
    SpinningTop, "spinning_top", 1, [], |bars| batch_spinning_top(bars)
}
hist_indicator! {
    /// `high_wave` — very small body, very long upper AND lower shadows.
    HighWave, "high_wave", 1, [], |bars| batch_high_wave(bars)
}
hist_indicator! {
    /// `long_line` — long candle (body ≥ 1.3× avg); white +100 / black -100.
    LongLine, "long_line", 1, [], |bars| batch_long_line(bars)
}
hist_indicator! {
    /// `short_line` — short, compact candle (presence).
    ShortLine, "short_line", 1, [], |bars| batch_short_line(bars)
}
hist_indicator! {
    /// `belt_hold` — long body opening at its extreme; white +100 / black -100.
    BeltHold, "belt_hold", 1, [], |bars| batch_belt_hold(bars)
}
hist_indicator! {
    /// `opening_marubozu` — no shadow on the open side.
    OpeningMarubozu, "opening_marubozu", 1, [], |bars| batch_opening_marubozu(bars)
}

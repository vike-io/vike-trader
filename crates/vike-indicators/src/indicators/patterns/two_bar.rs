//! Two-bar candlestick patterns: each kernel reads the current bar and the one before it.

use super::{avg_body, body, is_black, is_marubozu, is_white, rng};
use crate::indicators::macros::hist_indicator;
use crate::math::Columns;
use vike_marketdata::Bar;

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

hist_indicator! {
    /// `engulfing` — current body engulfs prior opposite-colour body.
    Engulfing, "engulfing", 1, [], |bars| batch_engulfing(bars)
}
hist_indicator! {
    /// `doji_star` — doji gapping away from a prior long body.
    DojiStar, "doji_star", 1, [], |bars| batch_doji_star(bars)
}
hist_indicator! {
    /// `harami` — current body inside prior opposite-colour body.
    Harami, "harami", 1, [], |bars| batch_harami(bars)
}
hist_indicator! {
    /// `harami_cross` — harami where the current bar is a doji.
    HaramiCross, "harami_cross", 1, [], |bars| batch_harami_cross(bars)
}
hist_indicator! {
    /// `piercing` — bullish penetration above prior midpoint.
    Piercing, "piercing", 1, [], |bars| batch_piercing(bars)
}
hist_indicator! {
    /// `dark_cloud_cover` — bearish penetration below prior midpoint.
    DarkCloudCover, "dark_cloud_cover", 1, [], |bars| batch_dark_cloud_cover(bars)
}
hist_indicator! {
    /// `counterattack` — opposite-colour bodies closing at ≈same price.
    Counterattack, "counterattack", 1, [], |bars| batch_counterattack(bars)
}
hist_indicator! {
    /// `meeting_lines` — counterattack with a tighter close tolerance.
    MeetingLines, "meeting_lines", 1, [], |bars| batch_meeting_lines(bars)
}
hist_indicator! {
    /// `separating_lines` — same-colour continuation opening at prior open.
    SeparatingLines, "separating_lines", 1, [], |bars| batch_separating_lines(bars)
}
hist_indicator! {
    /// `matching_low` — two black candles with equal closes (bullish).
    MatchingLow, "matching_low", 1, [], |bars| batch_matching_low(bars)
}
hist_indicator! {
    /// `on_neck` — bearish continuation closing at ≈prior low.
    OnNeck, "on_neck", 1, [], |bars| batch_on_neck(bars)
}
hist_indicator! {
    /// `in_neck` — bearish continuation closing slightly into prior body.
    InNeck, "in_neck", 1, [], |bars| batch_in_neck(bars)
}
hist_indicator! {
    /// `thrusting` — bearish continuation closing below prior midpoint.
    Thrusting, "thrusting", 1, [], |bars| batch_thrusting(bars)
}
hist_indicator! {
    /// `kicking` — opposite-colour marubozu with a gap.
    Kicking, "kicking", 1, [], |bars| batch_kicking(bars)
}
hist_indicator! {
    /// `kicking_by_length` — kicking, signal by the longer marubozu's colour.
    KickingByLength, "kicking_by_length", 1, [], |bars| batch_kicking_by_length(bars)
}
hist_indicator! {
    /// `homing_pigeon` — two blacks, second harami-inside the first (bullish).
    HomingPigeon, "homing_pigeon", 1, [], |bars| batch_homing_pigeon(bars)
}
hist_indicator! {
    /// `gap_side_side_white` — two similar-size white candles gapping up.
    GapSideSideWhite, "gap_side_side_white", 1, [], |bars| batch_gap_side_side_white(bars)
}
hist_indicator! {
    /// `tasuki_gap` — gap then opposite-colour candle staying in the gap.
    TasukiGap, "tasuki_gap", 1, [], |bars| batch_tasuki_gap(bars)
}

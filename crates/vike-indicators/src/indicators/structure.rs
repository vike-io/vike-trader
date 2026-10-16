//! Structure / pattern indicators — faithful f64 port of vike-trader-app
//! `core/indicators/structure.py`. Ports the four `@indicator`-decorated
//! functions (pivot_points, volume_profile_poc, zigzag, williams_fractal); the
//! unregistered `volume_profile`/`VolumeProfile` histogram helper is a
//! chart-render concern and is not ported here. Each `batch_*` is a line-for-line
//! port of the Python function (Python `None` → `f64::NAN`); streaming is
//! history-recompute via [`hist_indicator!`]. zigzag and williams_fractal read
//! future bars (a reversal-confirmed extreme / a centred 2n+1 window), so their
//! `on_bar` tip is a causal best-effort (NaN at the un-knowable tip) — they are
//! flagged `batch_only` in the registry and skipped by the `on_bar == vectorize`
//! parity gate. pivot_points (prior bar) and volume_profile_poc (trailing window)
//! are strictly causal.
#![allow(clippy::needless_range_loop)]

use super::macros::hist_indicator;
use crate::math::{Columns, Need};
use vike_marketdata::Bar;

// ============================ batch kernels ================================

fn batch_pivot_points(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::HIGH | Need::LOW | Need::CLOSE);
    let n = x.c.len();
    let mut p_out = vec![f64::NAN; n];
    let mut r1_out = vec![f64::NAN; n];
    let mut r2_out = vec![f64::NAN; n];
    let mut r3_out = vec![f64::NAN; n];
    let mut s1_out = vec![f64::NAN; n];
    let mut s2_out = vec![f64::NAN; n];
    let mut s3_out = vec![f64::NAN; n];
    for i in 1..n {
        let ph = x.h[i - 1];
        let pl = x.l[i - 1];
        let pc = x.c[i - 1];
        let p = (ph + pl + pc) / 3.0;
        let rng = ph - pl;
        p_out[i] = p;
        r1_out[i] = 2.0 * p - pl;
        s1_out[i] = 2.0 * p - ph;
        r2_out[i] = p + rng;
        s2_out[i] = p - rng;
        r3_out[i] = ph + 2.0 * (p - pl);
        s3_out[i] = pl - 2.0 * (ph - p);
    }
    vec![p_out, r1_out, r2_out, r3_out, s1_out, s2_out, s3_out]
}

#[expect(unused_assignments, unused_variables)] // last_pivot_* mirror the oracle's dead bookkeeping
fn batch_zigzag(bars: &[Bar], deviation: f64) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::HIGH | Need::LOW);
    let highs = &x.h;
    let lows = &x.l;
    let n = highs.len();
    let mut out = vec![f64::NAN; n];
    if n < 2 {
        return vec![out];
    }

    // direction: +1 = looking for a new high, −1 = looking for a new low
    let mut direction: i32 = 1;
    let mut last_pivot_price = lows[0];
    let mut last_pivot_idx = 0usize;
    let mut extreme_price = highs[0];
    let mut extreme_idx = 0usize;

    for i in 1..n {
        if direction == 1 {
            // In uptrend — track the running high
            if highs[i] >= extreme_price {
                extreme_price = highs[i];
                extreme_idx = i;
            } else if extreme_price > 0.0
                && (extreme_price - lows[i]) / extreme_price * 100.0 >= deviation
            {
                // Confirm the extreme as an up-pivot
                out[extreme_idx] = extreme_price;
                // Start tracking a new downswing from here
                last_pivot_price = extreme_price;
                last_pivot_idx = extreme_idx;
                extreme_price = lows[i];
                extreme_idx = i;
                direction = -1;
            }
        } else {
            // In downtrend — track the running low
            if lows[i] <= extreme_price {
                extreme_price = lows[i];
                extreme_idx = i;
            } else if extreme_price > 0.0
                && (highs[i] - extreme_price) / extreme_price * 100.0 >= deviation
            {
                // Confirm the extreme as a down-pivot
                out[extreme_idx] = extreme_price;
                // Start tracking a new upswing from here
                last_pivot_price = extreme_price;
                last_pivot_idx = extreme_idx;
                extreme_price = highs[i];
                extreme_idx = i;
                direction = 1;
            }
        }
    }

    vec![out]
}

fn batch_williams_fractal(bars: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::HIGH | Need::LOW);
    let highs = &x.h;
    let lows = &x.l;
    let length = highs.len();
    let mut fu = vec![f64::NAN; length];
    let mut fd = vec![f64::NAN; length];

    // Oracle `range(n, length - n)`: usize-safe (skip when `length <= n`, which
    // also covers the empty `range(n, length - n)` cases without underflow).
    if length > n {
        for i in n..(length - n) {
            let h_centre = highs[i];
            let l_centre = lows[i];
            let mut is_up = true;
            let mut is_down = true;
            for j in (i - n)..=(i + n) {
                if j == i {
                    continue;
                }
                if highs[j] >= h_centre {
                    is_up = false;
                }
                if lows[j] <= l_centre {
                    is_down = false;
                }
                if !is_up && !is_down {
                    break;
                }
            }
            if is_up {
                fu[i] = h_centre;
            }
            if is_down {
                fd[i] = l_centre;
            }
        }
    }

    vec![fu, fd]
}

// ======================= streaming structs (macro) =========================

hist_indicator! {
    /// `pivot_points` — classic floor pivots (P, R1-3, S1-3) from the prior bar.
    PivotPoints, "pivot_points", 7, [],
    |bars| batch_pivot_points(bars),
    lookback = 1
}
hist_indicator! {
    /// `zigzag` — swing-pivot detection via single-pass price reversal tracking.
    Zigzag, "zigzag", 1, [deviation = 5.0],
    |bars| batch_zigzag(bars, deviation)
}
hist_indicator! {
    /// `williams_fractal` — 2n+1 centred fractal patterns (fractal_up, fractal_down).
    WilliamsFractal, "williams_fractal", 2, [n = 2.0],
    |bars| batch_williams_fractal(bars, n.round() as usize)
}

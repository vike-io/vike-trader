//! Structure / pattern indicators — faithful f64 port of vike-trader-app
//! `core/indicators/structure.py`. Ports the four `@indicator`-decorated
//! functions (pivot_points, volume_profile_poc, zigzag, williams_fractal); the
//! unregistered `volume_profile`/`VolumeProfile` histogram helper is a
//! chart-render concern and is not ported here.
//!
//! `pivot_points` (the prior bar's OHLC) is strictly causal and streams from one bar of state on the
//! fast core in `crate::fast_o`; `volume_profile_poc` (a trailing window) is the hand-written
//! streamer in `streams_multi.rs`, on a core in the same file. `zigzag` and `williams_fractal` read
//! future bars (a reversal-confirmed extreme / a centred 2n+1 window), so their `on_bar` tip is a
//! causal best-effort (NaN at the un-knowable tip) — they are flagged `batch_only` in the registry,
//! skipped by the `on_bar == vectorize` parity gate, and stream by history-recompute via
//! [`hist_indicator!`] as they always did. Their batch kernels are the slice kernels of
//! `crate::fast_o`, which `crate::series::zigzag` and `crate::series::williams_fractal` also run.

use super::macros::{hist_indicator, typed_new};
use crate::Indicator;
use crate::fast_o::{self, PivotCore};
use crate::math::{Columns, Need};
use vike_marketdata::Bar;

// ============================ batch kernels ================================

fn batch_zigzag(bars: &[Bar], deviation: f64) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::HIGH | Need::LOW);
    vec![fast_o::zigzag_slices(&x.h, &x.l, deviation)]
}

fn batch_williams_fractal(bars: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::HIGH | Need::LOW);
    fast_o::williams_fractal_slices(&x.h, &x.l, n).into()
}

// ============================== pivot_points ===============================

/// `pivot_points` — classic floor pivots from the prior bar, seven lines `p`, `r1`, `r2`, `r3`, `s1`,
/// `s2`, `s3` (registry key `pivot_points`, no parameters). `NaN` on the first bar, which has no
/// prior bar. Built with `Default`.
#[derive(Clone)]
pub struct PivotPoints {
    core: PivotCore,
    last: [f64; 7],
}
impl PivotPoints {
    fn defaults() -> Self {
        Self { core: PivotCore::new(), last: [f64::NAN; 7] }
    }
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar and get `[p, r1, r2, r3, s1, s2, s3]`; this IS `Indicator::on_bar`, bit for
    /// bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 7] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`PivotPoints::next`] for a bar with those values.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 7] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for PivotPoints {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(PivotPoints, "pivot_points", []);
impl Indicator for PivotPoints {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        fast_o::pivot_bars(bars)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 7];
    }
    fn name(&self) -> &str {
        "pivot_points"
    }
    fn lookback(&self) -> usize {
        1
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// ======================= streaming structs (macro) =========================

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

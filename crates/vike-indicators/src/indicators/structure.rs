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
//!
//! `confirmed_pivot` is net-new Rust with no Python twin: the causal swing pivot, known `right` bars
//! after its extreme, streamed from bounded state on `crate::fast_o::SwingCore` (whose `push` the
//! stream, `vectorize` and `crate::series::confirmed_pivot` all run).

use super::macros::{hist_indicator, typed_new};
use super::pbars;
use crate::Indicator;
use crate::fast_o::{self, PivotCore, SwingCore};
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
/// prior bar. Built with `Default`. Option, off by default: the Fibonacci type
/// ([`PivotPoints::with_fibonacci`]).
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
    /// Option `fibonacci`, `false` by default: the Fibonacci type, the same `p` with the levels
    /// 0.382, 0.618 and 1 of the prior bar's range above and below it: `r1 = p + 0.382 * range`,
    /// `r2 = p + 0.618 * range`, `r3 = p + range`, `s1 = p - 0.382 * range`, `s2 = p - 0.618 * range`,
    /// `s3 = p - range`. Unset, the classic floor levels.
    #[inline]
    pub fn with_fibonacci(mut self, fibonacci: bool) -> Self {
        self.core.set_fibonacci(fibonacci);
        self
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
        fast_o::pivot_bars(&self.core, bars)
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

// ============================= confirmed_pivot ==============================

/// `confirmed_pivot` — swing highs and lows, each known `right` bars after its extreme (registry key
/// `confirmed_pivot`, parameters `left` and `right`, default 5 and 5; four lines `[high, low,
/// last_high, last_low]`).
///
/// A bar's high is a pivot high when none of the `left` bars before it has a higher high and all
/// `right` bars after it have a strictly lower one: an older bar may tie, a newer bar must lose, so
/// of two equal highs the later one is the pivot. Pivot lows mirror it. The pivot is reported on the
/// bar that confirms it, `right` bars after the extreme (that bar's index minus `right`): `high` /
/// `low` carry its price there and `NaN` on every other bar, while `last_high` / `last_low` hold the
/// newest confirmed pivot until the next one (`NaN` before the first), the one confirmed on this bar
/// included. Both sides can confirm on one bar. With `right` 0 (its range is `0..=200`) a pivot
/// confirms on its own bar: the bar's high is a pivot high when none of the `left` bars before it
/// is higher.
///
/// Nothing is confirmed until `left + 1 + right` bars are in. A `NaN` price as the window's oldest
/// bar, as the candidate or after it confirms nothing; one between the oldest bar and the candidate
/// counts as losing. Causal, unlike `williams_fractal` (which marks the extreme's own bar, reads the
/// future and refuses a tie on either side). The held lines remember a pivot from any distance back,
/// so the indicator is path-dependent ([`crate::Indicator::warmup_path_dependent`]).
#[derive(Clone)]
pub struct ConfirmedPivot {
    left: f64,
    right: f64,
    core: SwingCore,
    last: [f64; 4],
}
impl ConfirmedPivot {
    fn defaults() -> Self {
        Self::build(5.0, 5.0)
    }
    fn build(left: f64, right: f64) -> Self {
        Self { left, right, core: SwingCore::new(pbars(left), pbars(right)), last: [f64::NAN; 4] }
    }
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(5.0), p.get(1).copied().unwrap_or(5.0))
    }
    /// Feed the next bar and get `[high, low, last_high, last_low]`; this IS `Indicator::on_bar`, bit
    /// for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 4] {
        self.next_hl(bar.high, bar.low)
    }
    /// Feed the next bar's `high` and `low` as plain numbers, no [`Bar`] needed; the same answer as
    /// [`ConfirmedPivot::next`] for a bar with those prices.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> [f64; 4] {
        self.last = self.core.push(high, low);
        self.last
    }
}
impl Default for ConfirmedPivot {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(ConfirmedPivot, "confirmed_pivot", [left = 5.0, right = 5.0]);
impl Indicator for ConfirmedPivot {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 4>(self, bars, Self::next)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 4];
    }
    fn name(&self) -> &str {
        "confirmed_pivot"
    }
    // A lower bound: the window must be full, and where the first pivot lands is the data's.
    fn lookback(&self) -> usize {
        pbars(self.left) + pbars(self.right)
    }
    fn warmup_path_dependent(&self) -> bool {
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

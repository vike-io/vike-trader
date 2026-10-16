//! Four rolling-window statistics streamed from bounded state instead of a history recompute:
//! `hurst`, `linreg_channel`, `r_squared` and `median_ma`.
//!
//! As `hist_indicator!` types each bar re-ran the whole batch kernel over a retained tail of history
//! (measured per bar at 20k bars: `hurst` 9.3 ms, `linreg_channel` 4.1 ms, `median_ma` 384 us,
//! `r_squared` 39 us). `hurst` and `r_squared` are fixed-window folds with no recursion, so their
//! exact streaming state is a ring of the last values the window folds and ONE window evaluation per
//! bar: the per-window arithmetic is the batch kernel's, line for line and in the same order,
//! evaluated over the ring's contiguous slice (`moments`, `rescaled_range` and `log_log_slope` below
//! are copies of the private helpers in `oscillators.rs`, kept textually identical). A window is
//! recomputed from its values every bar, never updated with a carried sum, so those two match the
//! kernel's bits; `tests/streamers_stats_exact.rs` proves it.
//!
//! `median_ma` and `linreg_channel` run a fast core from `crate::fast_h` instead: a small struct of
//! plain numbers whose `push` is the ONE implementation of the arithmetic, shared by the stepper
//! (`next`, `next_value`, `on_bar`), by `vectorize` and by `crate::series`, so every way in agrees
//! bit for bit. The median keeps its window sorted and moves one element a bar; the regression keeps
//! running sums of the window, rebuilt from it, and decides a window that is (nearly) a straight line
//! from the window itself.
//!
//! Every type is hand-written in the `base/incremental.rs` sense (`trims_history() == false`).

use super::macros::typed_new;
use super::pback;
use crate::Indicator;
use crate::fast_h::{LinregCore, MedianCore};
use crate::window::is_constant_window;
use std::collections::VecDeque;
use vike_marketdata::Bar;

// ============================ copied kernel helpers ============================

/// The three sums a Pearson correlation of `x` against `y` is built from, `(Σdx·dy, Σdx², Σdy²)` with
/// the deviations taken from each series' own mean, or `None` when either series is constant (a
/// correlation with a series that never moves is undefined, not a rounding-sized number).
fn moments(x: &[f64], y: &[f64]) -> Option<(f64, f64, f64)> {
    if is_constant_window(x) || is_constant_window(y) {
        return None;
    }
    let n = x.len() as f64;
    // Net-new Rust, no Python twin: a naive left fold in the exact order the batch kernel folds, held bit for bit by `on_bar == vectorize` (tests/streamers_*_exact.rs, tests/all_indicators_exact.rs).
    let mx = x.iter().sum::<f64>() / n;
    // Net-new Rust, no Python twin: a naive left fold in the exact order the batch kernel folds, held bit for bit by `on_bar == vectorize` (tests/streamers_*_exact.rs, tests/all_indicators_exact.rs).
    let my = y.iter().sum::<f64>() / n;
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for k in 0..x.len() {
        let dx = x[k] - mx;
        let dy = y[k] - my;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    Some((sxy, sxx, syy))
}

/// The rescaled range R/S of one block of returns: the range of the running sum of deviations from
/// the block mean (the starting zero included) over the block's population standard deviation.
/// `None` for a block that never moves, whose deviation is zero.
fn rescaled_range(block: &[f64]) -> Option<f64> {
    if is_constant_window(block) {
        return None;
    }
    let n = block.len() as f64;
    // Net-new Rust, no Python twin: a naive left fold in the exact order the batch kernel folds, held bit for bit by `on_bar == vectorize` (tests/streamers_*_exact.rs, tests/all_indicators_exact.rs).
    let mean = block.iter().sum::<f64>() / n;
    let (mut run, mut hi, mut lo, mut ss) = (0.0, 0.0f64, 0.0f64, 0.0);
    for r in block {
        let dev = r - mean;
        run += dev;
        hi = hi.max(run);
        lo = lo.min(run);
        ss += dev * dev;
    }
    let sd = (ss / n).sqrt();
    if sd > 0.0 { Some((hi - lo) / sd) } else { None }
}

/// The least-squares slope of `y` on `x` over `(x, y)` points, `NaN` for fewer than two points or
/// points sharing one `x`. The `x` values are log block sizes, a fixed grid that never depends on
/// the data.
fn log_log_slope(points: &[(f64, f64)]) -> f64 {
    if points.len() < 2 {
        return f64::NAN;
    }
    let n = points.len() as f64;
    // Net-new Rust, no Python twin: a naive left fold in the exact order the batch kernel folds, held bit for bit by `on_bar == vectorize` (tests/streamers_*_exact.rs, tests/all_indicators_exact.rs).
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    // Net-new Rust, no Python twin: a naive left fold in the exact order the batch kernel folds, held bit for bit by `on_bar == vectorize` (tests/streamers_*_exact.rs, tests/all_indicators_exact.rs).
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for &(x, y) in points {
        let dx = x - mx;
        sxy += dx * (y - my);
        sxx += dx * dx;
    }
    if sxx > 0.0 { sxy / sxx } else { f64::NAN }
}

/// One Hurst estimate from the `m` log returns of a window: `NaN` when any return is not finite,
/// otherwise the slope of ln(mean R/S) on ln(block size) over block sizes 8, 16, 32, ... that fit,
/// each splitting the newest returns into whole blocks counted back from the latest return.
fn hurst_window(window: &[f64]) -> f64 {
    let m = window.len();
    if window.iter().any(|r| !r.is_finite()) {
        return f64::NAN;
    }
    // Block sizes double from 8, so at most 61 fit a `usize`; a fixed array keeps the per-bar path
    // off the heap.
    let mut points = [(0.0f64, 0.0f64); 64];
    let mut count_points = 0usize;
    let mut size = 8usize;
    while size <= m {
        let blocks = m / size;
        let first = m - blocks * size;
        let (mut total, mut count) = (0.0, 0usize);
        for b in 0..blocks {
            let lo = first + b * size;
            if let Some(rs) = rescaled_range(&window[lo..lo + size]) {
                total += rs;
                count += 1;
            }
        }
        if count > 0 {
            let mean_rs = total / count as f64;
            if mean_rs > 0.0 {
                points[count_points] = (libm::log(size as f64), libm::log(mean_rs));
                count_points += 1;
            }
        }
        size *= 2;
    }
    log_log_slope(&points[..count_points])
}

// ================================== hurst ==================================

/// Hurst exponent by rescaled-range analysis (registry key `hurst`, one line, parameter
/// `period`): near 0.5 for a random walk, above it for a trending series, below it for a
/// mean-reverting one. Over each window of `period` closes it takes the `period - 1` log
/// returns and, for every block size 8, 16, 32, ... that fits, splits the newest returns into
/// whole blocks of that size, working back from the latest return. Each block's R/S is the
/// range of the running sum of its deviations from its mean, the starting zero included, divided
/// by its population standard deviation; a block that never moves is skipped. The slope of the
/// log of the mean R/S against the log of the block size is the estimate. It needs at least 17
/// closes in the window. `NaN` for the first `period - 1` bars, when the window holds a
/// non-positive close, and when fewer than two block sizes produced a value.
#[derive(Clone)]
pub struct Hurst {
    period: f64,
    n: usize,
    prev_close: Option<f64>,
    rets: VecDeque<f64>,
    last: f64,
}
impl Hurst {
    fn defaults() -> Self {
        Self::build(100.0)
    }
    fn build(period: f64) -> Self {
        Self {
            period,
            n: period.round() as usize,
            prev_close: None,
            rets: VecDeque::new(),
            last: f64::NAN,
        }
    }
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(100.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
}
impl Default for Hurst {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Hurst, "hurst", [period = 100.0]);
impl Hurst {
    fn step(&mut self, bar: &Bar) -> f64 {
        let c = bar.close;
        let prev = self.prev_close.replace(c);
        // 17 closes are 16 returns: the kernel answers `NaN` everywhere below that.
        if self.n < 17 {
            self.last = f64::NAN;
            return self.last;
        }
        // The window folds the `period - 1` newest log returns; bar 0 has no return (the kernel's
        // first return is at index 1, and its first window ends at index `period - 1`).
        let m = self.n - 1;
        if let Some(pc) = prev {
            self.rets.push_back(libm::log(c / pc));
            if self.rets.len() > m {
                self.rets.pop_front();
            }
        }
        let out =
            if self.rets.len() == m { hurst_window(self.rets.make_contiguous()) } else { f64::NAN };
        self.last = out;
        self.last
    }
}
impl Indicator for Hurst {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn name(&self) -> &str {
        "hurst"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// ================================ r_squared ================================

/// R-squared of a rolling linear regression (registry key `r_squared`, one line, parameter
/// `period`): the squared Pearson correlation between the close and the bar index over the last
/// `period` closes, from 0 (no linear trend) to 1 (a perfect straight line). `NaN` for the first
/// `period - 1` bars and for a window of identical closes.
#[derive(Clone)]
pub struct RSquared {
    period: f64,
    n: usize,
    index: Vec<f64>,
    window: VecDeque<f64>,
    last: f64,
}
impl RSquared {
    fn defaults() -> Self {
        Self::build(14.0)
    }
    fn build(period: f64) -> Self {
        Self {
            period,
            n: period.round() as usize,
            index: Vec::new(),
            window: VecDeque::new(),
            last: f64::NAN,
        }
    }
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
}
impl Default for RSquared {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(RSquared, "r_squared", [period = 14.0]);
impl RSquared {
    fn step(&mut self, bar: &Bar) -> f64 {
        let n = self.n;
        let mut out = f64::NAN;
        // The kernel is all-`NaN` for a window shorter than two closes.
        if n >= 2 {
            self.window.push_back(bar.close);
            if self.window.len() > n {
                self.window.pop_front();
            }
            if self.window.len() == n {
                // The bar-index grid `0..period`: built once, when the first window completes.
                if self.index.len() != n {
                    self.index = (0..n).map(|k| k as f64).collect();
                }
                if let Some((sxy, sxx, syy)) = moments(&self.index, self.window.make_contiguous())
                    && sxx > 0.0
                    && syy > 0.0
                {
                    // r² = (Σdx·dy)² / (Σdx² · Σdy²), written as the product of the two regression
                    // slopes (y on x, x on y), which is the same number.
                    out = ((sxy / sxx) * (sxy / syy)).min(1.0);
                }
            }
        }
        self.last = out;
        self.last
    }
}
impl Indicator for RSquared {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn name(&self) -> &str {
        "r_squared"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// =============================== median_ma =================================

/// Rolling median of the close (`median_ma`): the middle value of the last `period` closes, or
/// the mean of the two middle ones when `period` is even. A moving average that ignores a lone
/// spike. One parameter, `period` (default 20); one output line, `NaN` for the first
/// `period - 1` bars and while a `NaN` close is inside the window.
#[derive(Clone)]
pub struct MedianMa {
    period: f64,
    core: MedianCore,
    last: f64,
}
impl MedianMa {
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: MedianCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(20.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as
    /// [`MedianMa::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for MedianMa {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(MedianMa, "median_ma", [period = 20.0]);
impl Indicator for MedianMa {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.next(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::next)
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "median_ma"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// ============================== linreg_channel =============================

/// Linear-regression channel (`linreg_channel`): the least-squares line through the last
/// `period` closes, with bands a multiple of the residual spread either side. Parameters
/// `period` (default 100) and `mult` (default 2.0); output lines `upper`, `mid` and `lower`,
/// `NaN` for the first `period - 1` bars.
///
/// Formula. Fit the closes of the window against `0..period - 1` by least squares; `mid` is
/// the fitted line's value at the last bar, `s` is the population standard deviation of the
/// window's residuals (each close minus the fitted line at its own position), and
/// `upper = mid + mult * s`, `lower = mid - mult * s`. A window of identical closes has
/// `s = 0`.
#[derive(Clone)]
pub struct LinregChannel {
    period: f64,
    core: LinregCore,
    last: [f64; 3],
}
impl LinregChannel {
    #[inline]
    fn defaults() -> Self {
        Self::build(100.0, 2.0)
    }
    #[inline]
    fn build(period: f64, mult: f64) -> Self {
        Self {
            period,
            core: LinregCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize, mult),
            last: [f64::NAN; 3],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let period = p.first().copied().unwrap_or(100.0);
        let mult = p.get(1).copied().unwrap_or(2.0);
        Self::build(period, mult)
    }
    /// Feed the next bar and get `[upper, mid, lower]`; this IS `Indicator::on_bar`, bit for bit,
    /// without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 3] {
        self.next_value(bar.close)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; `[upper, mid, lower]`, the same
    /// answer as [`LinregChannel::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> [f64; 3] {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for LinregChannel {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(LinregChannel, "linreg_channel", [period = 100.0, mult = 2.0]);
impl Indicator for LinregChannel {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 3>(self, bars, Self::next)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 3];
    }
    fn name(&self) -> &str {
        "linreg_channel"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

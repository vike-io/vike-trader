//! Four rolling-window statistics streamed from bounded state instead of a history recompute:
//! `hurst`, `linreg_channel`, `r_squared` and `median_ma`.
//!
//! As `hist_indicator!` types each bar re-ran the whole batch kernel over a retained tail of history
//! (measured per bar at 20k bars: `hurst` 9.3 ms, `linreg_channel` 4.1 ms, `median_ma` 384 us,
//! `r_squared` 39 us). All four now run a fast core instead: a small struct of plain numbers whose
//! `push` is the ONE implementation of the arithmetic, shared by the stepper (`next`, `next_value`,
//! `on_bar`), by `vectorize` and by `crate::series`, so every way in agrees bit for bit.
//!
//! * `hurst` runs `crate::fast_l::HurstCore`: the rescaled range of ONE new block per size a bar (the
//!   blocks counted back from the newest return repeat every `size` returns), kept in a ring, folded
//!   oldest block first as the old kernel folded them, so the answer is bit for bit the old one.
//! * `r_squared` runs `crate::fast_l::RegCore`, the regression core of the whole `linearreg` family:
//!   running sums of the window, with the old kernel's own centred folds as the answer wherever the
//!   sums cannot resolve it (a window of identical closes above all).
//! * `median_ma` and `linreg_channel` run the cores of `crate::fast_h`: the median keeps its window
//!   sorted and moves one element a bar; the regression keeps running sums of the window, rebuilt from
//!   it, and decides a window that is (nearly) a straight line from the window itself.
//!
//! Every type is hand-written in the `base/incremental.rs` sense (`trims_history() == false`).

use super::macros::typed_new;
use super::pback;
use crate::Indicator;
use crate::fast_h::{LinregCore, MedianCore};
use crate::fast_l::{HurstCore, RSQ, RegCore, stat1};
use vike_marketdata::Bar;

// ================================== hurst ==================================

stat1! {
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
    Hurst, "hurst", 100.0, HurstCore, |n| HurstCore::new(n)
}

// ================================ r_squared ================================

stat1! {
    /// R-squared of a rolling linear regression (registry key `r_squared`, one line, parameter
    /// `period`): the squared Pearson correlation between the close and the bar index over the last
    /// `period` closes, from 0 (no linear trend) to 1 (a perfect straight line). `NaN` for the first
    /// `period - 1` bars and for a window of identical closes.
    RSquared, "r_squared", 14.0, RegCore<RSQ>, |n| RegCore::<RSQ>::new(n, 0.0)
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

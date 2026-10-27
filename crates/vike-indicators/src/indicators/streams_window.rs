//! Four windowed indicators streamed from bounded state instead of a history recompute: `hvol`,
//! `relative_volatility`, `chande_kroll_stop` and `connors_rsi`.
//!
//! As `hist_indicator!` types each bar re-ran the whole batch kernel over a retained tail of
//! history (measured per bar at 20k bars, default parameters: `connors_rsi` 1,034 us,
//! `chande_kroll_stop` 178 us, `hvol` 74 us, `relative_volatility` 21 us, the last two growing to
//! milliseconds at period 200). Here each runs a fast core from `crate::fast_h`: a small struct of
//! plain numbers whose `push` is the ONE implementation of the arithmetic, shared by the stepper
//! (`next`, `next_value`, `next_hlc`, `on_bar`), by `vectorize` and by the whole-series calls in
//! `crate::series`, so every way in agrees bit for bit. The cores hold the rolling extremes as lap
//! maxima, the standard deviations as running sums rebuilt from the window (with the old kernels'
//! "a window of equal values has a deviation of exactly zero" decided from the window, not hoped for),
//! the recurrences (the Wilder average behind the true range, the EMAs behind the relative volatility
//! index, the RSI averages) as the kernel's own seed-then-recurrence, and the percent rank as a count
//! over a ring.
//!
//! Every type is hand-written in the `base/incremental.rs` sense (`trims_history() == false`).

use super::macros::typed_new;
use super::{pback, pbars};
use crate::Indicator;
use crate::fast_h::{ChandeKrollCore, ConnorsCore, HvolCore, RelVolCore};
use vike_marketdata::Bar;

// ================================== hvol ===================================

/// Annualized historical volatility in percent (registry key `hvol`, one line): the population
/// standard deviation of the last `period` log returns of the close, times the square root of
/// `ann` (periods per year) and 100. A bar whose close or previous close is not positive has no
/// log return and is skipped, not counted. Parameters: `period` (default 20) and `ann` (default
/// 365). `NaN` until `period` log returns exist.
#[derive(Clone)]
pub struct Hvol {
    period: f64,
    core: HvolCore,
    last: f64,
}
impl Hvol {
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0, 365.0)
    }
    #[inline]
    fn build(period: f64, ann: f64) -> Self {
        Self {
            period,
            core: HvolCore::new(
                period.round().min(crate::roll::RING_CAP as f64) as usize,
                ann.round() as usize,
            ),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let period = p.first().copied().unwrap_or(20.0);
        let ann = p.get(1).copied().unwrap_or(365.0);
        Self::build(period, ann)
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Hvol::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Hvol {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Hvol, "hvol", [period = 20.0, ann = 365.0]);
impl Indicator for Hvol {
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
        "hvol"
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// ============================ relative_volatility ==========================

/// Relative Volatility Index (registry key `relative_volatility`, one line, 0..=100): the rolling
/// population standard deviation of the close, split by the direction of the close (up bars feed
/// the up side, down bars the down side, flat bars neither), each side EMA-smoothed over
/// `period`, and the up side expressed as a percentage of the two. Parameter: `period` (default
/// 14). `NaN` while warming up; `50` when both smoothed sides are zero.
#[derive(Clone)]
pub struct RelativeVolatility {
    period: f64,
    core: RelVolCore,
    last: f64,
}
impl RelativeVolatility {
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: RelVolCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as
    /// [`RelativeVolatility::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for RelativeVolatility {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(RelativeVolatility, "relative_volatility", [period = 14.0]);
impl Indicator for RelativeVolatility {
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
        "relative_volatility"
    }
    fn lookback(&self) -> usize {
        (2 * pbars(self.period)).saturating_sub(2)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// ============================ chande_kroll_stop ============================

/// Chande Kroll Stop (registry key `chande_kroll_stop`, two lines `[long_stop, short_stop]`):
/// a Wilder ATR over `p` bars scaled by `x`, subtracted from the `p`-bar highest high and added to
/// the `p`-bar lowest low, then the highest of the first and the lowest of the second over the last
/// `q` bars. Parameters: `p` (default 10), `x` (default 1) and `q` (default 9). `NaN` while
/// warming up.
#[derive(Clone)]
pub struct ChandeKrollStop {
    p: f64,
    q: f64,
    core: ChandeKrollCore,
    last: [f64; 2],
}
impl ChandeKrollStop {
    #[inline]
    fn defaults() -> Self {
        Self::build(10.0, 1.0, 9.0)
    }
    #[inline]
    fn build(p: f64, x: f64, q: f64) -> Self {
        Self {
            p,
            q,
            core: ChandeKrollCore::new(
                p.round().min(crate::roll::RING_CAP as f64) as usize,
                (x.round() as usize) as f64,
                q.round().min(crate::roll::RING_CAP as f64) as usize,
            ),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let period = p.first().copied().unwrap_or(10.0);
        let x = p.get(1).copied().unwrap_or(1.0);
        let q = p.get(2).copied().unwrap_or(9.0);
        Self::build(period, x, q)
    }
    /// Feed the next bar and get `[long_stop, short_stop]`; this IS `Indicator::on_bar`, bit for
    /// bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
    /// Feed the next high, low and close as plain numbers, no [`Bar`] needed; the same answer as
    /// [`ChandeKrollStop::next`] for a bar with those.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for ChandeKrollStop {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(ChandeKrollStop, "chande_kroll_stop", [p = 10.0, x = 1.0, q = 9.0]);
impl Indicator for ChandeKrollStop {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 2>(self, bars, Self::next)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 2];
    }
    fn name(&self) -> &str {
        "chande_kroll_stop"
    }
    fn lookback(&self) -> usize {
        pbars(self.p).max(pback(self.q, 1))
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// ================================ connors_rsi ==============================

/// Connors RSI (registry key `connors_rsi`, one line, 0..=100): the mean of three parts, the RSI of
/// the close over `rsi_p` bars, the RSI over `streak_p` bars of the up/down streak length (how
/// many bars in a row the close rose or fell), and the percent rank of the bar's percentage
/// return among the last `rank_p` returns. Parameters: `rsi_p` (default 3), `streak_p` (default 2)
/// and `rank_p` (default 100). `NaN` until all three parts are defined.
#[derive(Clone)]
pub struct ConnorsRsi {
    rsi_p: f64,
    streak_p: f64,
    rank_p: f64,
    core: ConnorsCore,
    last: f64,
}
impl ConnorsRsi {
    #[inline]
    fn defaults() -> Self {
        Self::build(3.0, 2.0, 100.0)
    }
    #[inline]
    fn build(rsi_p: f64, streak_p: f64, rank_p: f64) -> Self {
        let cap = |p: f64| p.round().min(crate::roll::RING_CAP as f64) as usize;
        Self {
            rsi_p,
            streak_p,
            rank_p,
            core: ConnorsCore::new(cap(rsi_p), cap(streak_p), cap(rank_p)),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let rsi_p = p.first().copied().unwrap_or(3.0);
        let streak_p = p.get(1).copied().unwrap_or(2.0);
        let rank_p = p.get(2).copied().unwrap_or(100.0);
        Self::build(rsi_p, streak_p, rank_p)
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as
    /// [`ConnorsRsi::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for ConnorsRsi {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(ConnorsRsi, "connors_rsi", [rsi_p = 3.0, streak_p = 2.0, rank_p = 100.0]);
impl Indicator for ConnorsRsi {
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
        "connors_rsi"
    }
    fn lookback(&self) -> usize {
        pbars(self.rsi_p).max(pbars(self.streak_p)).max(pbars(self.rank_p))
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

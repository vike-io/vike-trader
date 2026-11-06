//! Volatility indicators — faithful f64 port of vike-trader-app
//! `core/indicators/volatility.py`. Skips names already in `base.rs`
//! (atr/bollinger/keltner/donchian). `true_range`, `stddev`, `bbands_pctb`, `bbands_width`,
//! `donchian_width`, `ulcer` and `chop` are hand-written steppers on a fast core
//! (`crate::fast_d2`, `crate::fast_j`); the rest stream by history-recompute via `hist_indicator!`,
//! where fold order is load-bearing for bit parity.
//!
//! ⚠ **This file's header used to read "running-sum std forms are reproduced EXACTLY (not a
//! two-pass window)".** They are not, any more: a running sum is not a function of its window, so
//! `stddev`/`hvol`/`relative_volatility` returned different bits once `hist_indicator!` truncated
//! history — and `run_sum2/p - mean*mean` is catastrophic cancellation on price data on top of
//! that. Both are now two-pass window folds; `stddev_series` carries the argument.
//!
//! # ⚠ A CONSTANT window is zero by DECISION here too, and this file learned it second
//!
//! Two-pass is necessary and NOT sufficient. `mean` is `Σx / n`, and `n` copies of `v` summed then
//! divided by `n` only rounds back to `v` for some values — `100.0` is one of them, `0.1` is not
//! (20 copies fold to `3fb999999999999b` against the value's `3fb999999999999a`). Every deviation
//! is then ~1.4e-17 rather than `0`, and the variance is rounding-sized rather than zero.
//! `crates/vike-indicators/src/window.rs`'s `rolling_var` was given
//! [`crate::window::is_constant_window`] for this; the three folds in THIS file do not route
//! through that module and so kept the defect. MEASURED on a 20-bar window flat at `0.1`:
//!
//! | indicator | before | after | at `100.0`, before and after |
//! |---|---|---|---|
//! | `stddev` | `1.3877787807814457e-17` | `0` | `0` |
//! | `bbands_width` | `5.551115123125782e-16` | `0` | `0` |
//! | `bbands_pctb` | `0.25` | `NaN` | `NaN` |
//! | `hvol` | `6.628369094757308e-15` (constant LOG-return window) | `0` | `0` |
//!
//! `bbands_pctb` is the one that matters most: it is `(price - lower) / (upper - lower)`, a tiny
//! numerator over a tiny denominator, so it returned an arbitrary FINITE number — `0.25`, which
//! looks like a perfectly ordinary mid-band reading — where there is no band and no information.
//! The `100.0` column is why none of this was caught: `tests/parity.rs`'s flat-window fixture used
//! the one value that cannot fail, and now uses `0.1` with
//! `the_constant_window_fixture_actually_bites` asserting the substitution cannot be undone.
//!
//! ⚠ The cure is an EXACT equality over the window's INPUTS, never an epsilon on the OUTPUT. A
//! threshold is a second law with a number in it and would swallow a genuinely small-but-real
//! variance — `a_variance_that_is_small_but_real_still_yields_a_nonzero_band` is the gate.
//!
//! `true_range` and `stddev` are no longer `hist_indicator!` types: they are hand-written steppers
//! on `crate::fast_d2` (`stddev_series` survives as the test oracle in this file's `tests`). So are
//! `bbands_pctb`, `bbands_width`, `donchian_width`, `ulcer` and `chop` (on `crate::fast_j`): the batch
//! kernels that held the two-pass window folds above are gone, and the flat window is still
//! `bbands_width` `0` and `bbands_pctb` `NaN` (`crate::fast_j::BandCore` re-centres on the repeated
//! value, so its deviation is exactly `0`).
#![allow(clippy::needless_range_loop)]

use super::{pback, pbars};
use crate::Indicator;
use crate::fast_d2::{StddevCore, TrueRangeCore};
use crate::fast_j::{ChopCore, DonchianWidthCore, PctbCore, UlcerCore, WidthCore};
use vike_marketdata::Bar;

// ============================== streaming structs ==============================

/// `true_range` — gap-aware true range of a bar: `max(high - low, |high - prev close|, |low - prev
/// close|)`, and `high - low` on the first bar (registry key `true_range`, one line, no parameters).
///
/// Stateless but for the previous close (`crate::fast_d2::TrueRangeCore`); the same bits as the
/// batch kernel it replaced.
#[derive(Clone)]
pub struct TrueRange {
    core: TrueRangeCore,
    last: f64,
}
impl TrueRange {
    /// The registry defaults. Private: `Default` is the public spelling, and a type with no
    /// parameters gets no `new`.
    #[inline]
    fn defaults() -> Self {
        Self { core: TrueRangeCore::new(), last: f64::NAN }
    }
    /// Parametric constructor for the registry; the row has no parameters.
    #[inline]
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next bar's high, low and close as plain numbers, no [`Bar`] needed; the same answer
    /// as [`TrueRange::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(high, low, close);
        self.last
    }
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Default for TrueRange {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for TrueRange {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        // The same expression `step` runs, without cloning the stepper and without a state to carry.
        let mut out = vec![f64::NAN; bars.len()];
        let mut prev_close = f64::NAN;
        for (o, bar) in out.iter_mut().zip(bars) {
            *o = crate::fast_d2::true_range(bar.high, bar.low, prev_close);
            prev_close = bar.close;
        }
        vec![out]
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "true_range"
    }
    fn lookback(&self) -> usize {
        0
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Rolling population standard deviation of the close (default period 20; registry key `stddev`,
/// one line). `NaN` until `period` closes have been seen; a window of bit-equal closes is exactly
/// `0.0`.
///
/// O(1) a bar (`crate::fast_d2::StddevCore`): running sums of the window shifted by its oldest
/// value at the last rebuild, so a price near a million with moves of a hundredth keeps its digits.
/// The last bits can differ from a fresh two-pass fold of the window, by the rounding noise every
/// running-sum deviation has. A `NaN` close makes the value `NaN` while it is inside the window,
/// and the value is exact again the bar after it left.
#[derive(Clone)]
pub struct Stddev {
    period: f64,
    core: StddevCore,
    last: f64,
}
impl Stddev {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self { period, core: StddevCore::new(pbars(period)), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=200`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("stddev", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Parametric constructor — `p[0]` = period; a missing entry falls back to the default.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(20.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as
    /// [`Stddev::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Default for Stddev {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Stddev {
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
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "stddev"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Bollinger %B of the close: `(close - lower band) / (upper band - lower band)` over `period` bars
/// and `k` population standard deviations (registry key `bbands_pctb`, one line). `NaN` for the first
/// `period - 1` bars, and `NaN` where the band has no width: a window of bit-equal closes.
///
/// O(1) a bar (`crate::fast_j::BandCore`): the mean and deviation of the window come from shifted
/// running sums, the arithmetic of [`crate::indicators::Bollinger`] with a re-centring guard, because
/// a ratio of the deviation needs its relative accuracy. The last bits differ from a fresh two-pass
/// fold of the window by the rounding noise every running sum has. A `NaN` close makes the value
/// `NaN` while it is inside the window.
#[derive(Clone)]
pub struct BbandsPctb {
    n: usize,
    core: PctbCore,
    last: f64,
}
impl BbandsPctb {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0, 2.0)
    }
    #[inline]
    fn build(period: f64, k: f64) -> Self {
        let n = pbars(period);
        Self { n, core: PctbCore::new(n, k), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=200`) and the band half-width `k` in standard
    /// deviations (`0.5..=5.0`). A value the registry would not accept (out of range, or `NaN`) is a
    /// [`crate::ParamError`] naming it; [`crate::make_with`] clamps instead. `Default` gives the
    /// registry defaults (20, 2.0).
    #[inline]
    pub fn new(period: f64, k: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period, k];
        crate::typed::checked("bbands_pctb", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Parametric constructor — `p[0]` = period, `p[1]` = k; a missing entry falls back to the default.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(20.0), p.get(1).copied().unwrap_or(2.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as
    /// [`BbandsPctb::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Default for BbandsPctb {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for BbandsPctb {
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
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "bbands_pctb"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Bollinger band width of the close: `(upper band - lower band) / mid line` over `period` bars and
/// `k` population standard deviations (registry key `bbands_width`, one line). `NaN` for the first
/// `period - 1` bars and where the mid line is zero; exactly `0.0` for a window of bit-equal closes.
///
/// Runs the same core as [`BbandsPctb`] (`crate::fast_j::BandCore`).
#[derive(Clone)]
pub struct BbandsWidth {
    n: usize,
    core: WidthCore,
    last: f64,
}
impl BbandsWidth {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0, 2.0)
    }
    #[inline]
    fn build(period: f64, k: f64) -> Self {
        let n = pbars(period);
        Self { n, core: WidthCore::new(n, k), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=200`) and the band half-width `k` in standard
    /// deviations (`0.5..=5.0`). A value the registry would not accept (out of range, or `NaN`) is a
    /// [`crate::ParamError`] naming it; [`crate::make_with`] clamps instead. `Default` gives the
    /// registry defaults (20, 2.0).
    #[inline]
    pub fn new(period: f64, k: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period, k];
        crate::typed::checked("bbands_width", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Parametric constructor — `p[0]` = period, `p[1]` = k; a missing entry falls back to the default.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(20.0), p.get(1).copied().unwrap_or(2.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as
    /// [`BbandsWidth::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Default for BbandsWidth {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for BbandsWidth {
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
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "bbands_width"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Donchian channel width: the highest high minus the lowest low of the last `period` bars (registry
/// key `donchian_width`, one line). `NaN` for the first `period - 1` bars; a `NaN` high or low is
/// skipped as if absent.
///
/// O(1) a bar: block-wise window extremes (`crate::fast_a::HiLo`, the core of
/// [`crate::indicators::Donchian`]); the same bits as the fold it replaced.
#[derive(Clone)]
pub struct DonchianWidth {
    n: usize,
    core: DonchianWidthCore,
    last: f64,
}
impl DonchianWidth {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        let n = pbars(period);
        Self { n, core: DonchianWidthCore::new(n), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=200`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("donchian_width", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Parametric constructor — `p[0]` = period; a missing entry falls back to the default.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(20.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next high and low as plain numbers, no [`Bar`] needed; the same answer as
    /// [`DonchianWidth::next`] for a bar with those.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> f64 {
        self.last = self.core.push(high, low);
        self.last
    }
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_hl(bar.high, bar.low)
    }
}
impl Default for DonchianWidth {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for DonchianWidth {
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
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "donchian_width"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Ulcer Index of the close: the root of the mean squared percentage drawdown of the last `period`
/// closes from the highest of them (default period 14; registry key `ulcer`, one line). `NaN` for the
/// first `period - 1` bars; exactly `0` for a window of bit-equal closes.
///
/// O(1) a bar (`crate::fast_j::UlcerCore`): the window's drawdown moments are carried and rebased
/// when the peak moves, and rebuilt every `ceil(512 / period)` laps. The last bits differ from a
/// fresh fold of the window by the rounding noise every running sum has. A `NaN` close makes the
/// value `NaN` while it is inside the window, and the value is exact again the bar after it left.
#[derive(Clone)]
pub struct Ulcer {
    n: usize,
    core: UlcerCore,
    last: f64,
}
impl Ulcer {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        let n = pbars(period);
        Self { n, core: UlcerCore::new(n), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=200`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("ulcer", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Parametric constructor — `p[0]` = period; a missing entry falls back to the default.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Ulcer::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Default for Ulcer {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Ulcer {
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
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "ulcer"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Choppiness Index: `100 * log10(sum of the true ranges / (highest high - lowest low)) /
/// log10(period)` over the last `period` bars (default 14; registry key `chop`, one line). `NaN` for
/// the first `period - 1` bars and where the range or the sum of true ranges is not positive.
///
/// O(1) a bar (`crate::fast_j::ChopCore`): a running sum of the true ranges and block-wise window
/// extremes, plus the one `libm::log10`. The last bits differ from a fresh fold of the window by the
/// rounding noise every running sum has.
#[derive(Clone)]
pub struct Chop {
    n: usize,
    core: ChopCore,
    last: f64,
}
impl Chop {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        let n = pbars(period);
        Self { n, core: ChopCore::new(n), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=200`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("chop", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Parametric constructor — `p[0]` = period; a missing entry falls back to the default.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next bar's high, low and close as plain numbers, no [`Bar`] needed; the same answer
    /// as [`Chop::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(high, low, close);
        self.last
    }
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Default for Chop {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Chop {
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
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "chop"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::sq;
    use crate::typed::tests::assert_matches_registry;
    use crate::window::is_constant_window;

    #[test]
    fn bbands_pctb() {
        assert_matches_registry("bbands_pctb", Box::new(BbandsPctb::new(20.0, 2.0).unwrap()));
    }

    #[test]
    fn bbands_width() {
        assert_matches_registry("bbands_width", Box::new(BbandsWidth::new(20.0, 2.0).unwrap()));
    }

    #[test]
    fn donchian_width() {
        assert_matches_registry("donchian_width", Box::new(DonchianWidth::new(20.0).unwrap()));
    }

    #[test]
    fn ulcer() {
        assert_matches_registry("ulcer", Box::new(Ulcer::new(14.0).unwrap()));
    }

    #[test]
    fn chop() {
        assert_matches_registry("chop", Box::new(Chop::new(14.0).unwrap()));
    }

    /// Rolling population stddev — a TWO-PASS window fold (`mean`, then `Σ(x - mean)²`), the same
    /// shape as [`bollinger_vals`]. Now the test oracle for `Stddev`, which streams
    /// `crate::fast_d2::StddevCore` instead; `streams_window.rs`'s `pop_sd` is the same fold.
    ///
    /// ⚠ **This carried a `run_sum` / `run_sum2` pair and closed with `run_sum2/p - mean*mean`.**
    /// `crates/vike-indicators/src/indicators/statistics.rs`'s `batch_var` carries the full argument
    /// for why both halves had to go (truncation-dependence, then catastrophic cancellation that
    /// FLIPS the zero-variance guards). MEASURED here: 64/64 and 231/232 mismatching retained lengths
    /// at `period` 20 and 200 before, 0 of each after.
    ///
    /// A window whose closes are all bit-equal returns exactly `0.0` without folding — see the module
    /// doc's "A CONSTANT window is zero by DECISION here too" table for the measurement, and
    /// [`crate::window::is_constant_window`] for why the predicate reads the INPUTS rather than
    /// thresholding the output. Every other window is byte-identical to what this returned before.
    fn stddev_series(v: &[f64], period: usize) -> Vec<f64> {
        let n = v.len();
        let mut out = vec![f64::NAN; n];
        if period == 0 || n < period {
            return out;
        }
        for i in (period - 1)..n {
            let window = &v[i + 1 - period..=i];
            if is_constant_window(window) {
                out[i] = 0.0;
                continue;
            }
            let mean = window.iter().sum::<f64>() / period as f64;
            let var = window.iter().map(|x| sq(x - mean)).sum::<f64>() / period as f64;
            out[i] = var.sqrt();
        }
        out
    }

    /// The population standard deviation of a window in EXACT integer arithmetic: every finite `f64` is
    /// a whole multiple of the ulp of its smallest member, so the window scales to `i128` without
    /// rounding, `n * sum(v^2) - sum(v)^2` is exact, and the one rounding left is the final division.
    /// A two-pass fold in `f64` is not a reference at this scale: its mean carries a rounding error of
    /// half an ulp of the level, and on a window whose spread is a few thousand ulps (two closes near a
    /// million) that error enters the deviation SQUARED, at nine digits.
    fn exact_population_sd(w: &[f64]) -> f64 {
        if w.iter().any(|v| !v.is_finite()) {
            return f64::NAN;
        }
        let n = w.len() as i128;
        let e_min = w
            .iter()
            .filter(|v| **v != 0.0)
            .map(|v| ((v.to_bits() >> 52) & 0x7ff) as i32 - 1023)
            .min()
            .unwrap_or(0);
        let k = 52 - e_min;
        let scale = f64::from_bits(((1023 + k) as u64) << 52);
        let v: Vec<i128> = w.iter().map(|x| (x * scale) as i128).collect();
        let s1: i128 = v.iter().sum();
        let s2: i128 = v.iter().map(|x| x * x).sum();
        let num = n * s2 - s1 * s1;
        (num as f64).sqrt() / n as f64 / scale
    }

    #[test]
    fn true_range() {
        assert_matches_registry("true_range", Box::new(TrueRange::default()));
    }

    #[test]
    fn stddev() {
        assert_matches_registry("stddev", Box::new(Stddev::new(20.0).unwrap()));
    }

    /// The streamer against the two-pass window fold it replaced: close to it everywhere (relative
    /// 1e-9) and exactly `0.0` wherever the oracle says a window is constant.
    #[test]
    fn stddev_follows_the_two_pass_fold_and_is_zero_on_a_flat_window() {
        let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut p = 1.0e6_f64;
        let mut close: Vec<f64> = (0..3000)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                p += ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.02;
                p
            })
            .collect();
        let flat = close[close.len() - 1] + 0.1;
        close.extend(std::iter::repeat_n(flat, 60));
        for n in [2usize, 20, 200] {
            let want = stddev_series(&close, n);
            let mut core = StddevCore::new(n);
            for (i, &c) in close.iter().enumerate() {
                let got = core.push(c);
                if want[i].is_nan() {
                    assert!(got.is_nan(), "n={n} i={i}");
                } else if want[i] == 0.0 {
                    assert_eq!(got, 0.0, "n={n} i={i}: a constant window is exactly zero");
                } else {
                    let exact = exact_population_sd(&close[i + 1 - n..=i]);
                    assert!((got - exact).abs() <= 1e-9 * exact, "n={n} i={i}: {got} vs {exact}");
                }
            }
        }
    }
}

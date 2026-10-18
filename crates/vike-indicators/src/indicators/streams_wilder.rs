//! `adx`, `adxr`, `natr`, `supertrend`, `smma` and `efi`, streamed from bounded state.
//!
//! These six used to be `hist_indicator!` types: every bar re-ran the whole batch kernel over a
//! retained tail of history (measured per bar at 20,000 bars: `adx` 51 us, `adxr` 84 us, `natr`
//! ~13 us, `supertrend` ~12 us, `smma` ~11 us, `efi` ~11 us), and the tail was a truncation of a
//! recurrence that never fully forgets. Every one of them is a Wilder or EMA recurrence, whose exact
//! state is a handful of numbers, so a bar here costs O(1) and nothing is truncated at all.
//!
//! `adx` (`crate::fast_c::AdxCore`) and `adxr`, `natr`, `supertrend` and `smma` (`crate::fast_g`) run
//! small cores whose `push` is the one implementation of the arithmetic: the stream, `vectorize` and
//! the whole-series call (`series::*`) all run it, so they agree bit for bit (held by `tests/parity`,
//! `tests/streamers_wilder_exact.rs` and `tests/fast_c.rs` / `tests/fast_g.rs`). The cores multiply
//! by a precomputed `1/n` where the textbook divides, so a value differs from the textbook form by
//! rounding noise only. `efi` runs `crate::fast_k::EfiCore`, a plain EMA (the seed divides by `n`).
//!
//! # Two details that are easy to get wrong
//!
// Net-new Rust, no Python twin: a naive left fold in the exact order the batch kernel folds, held bit for bit by `on_bar == vectorize` (tests/streamers_*_exact.rs, tests/all_indicators_exact.rs).
//! * **A seed is a left fold that starts at `-0.0`.** The kernels seed with `iter().sum::<f64>()`,
//!   and the standard library's float `Sum` starts at `-0.0`, not `+0.0`. The two differ when EVERY
//!   term is `-0.0` (a zero-volume stretch of falling closes makes `efi`'s force exactly that), so
//!   the seeds below begin at `-0.0`, not at `+0.0`.
//! * **A kernel that smooths "the defined tail" only sees the defined values.** `efi` compacts
//!   its `NaN` raw values away before the EMA, so a `NaN` force here is skipped, not pushed.

use super::macros::typed_new;
use super::{pback, pbars};
use crate::Indicator;
use crate::fast_g::{AdxrCore, NatrCore, SmmaCore, SupertrendCore, period, typed_new_inline};
use crate::fast_k::EfiCore;
use vike_marketdata::Bar;

/// `Default`, and the user-facing `next` for a one-line type.
macro_rules! scalar_surface {
    ($Name:ident) => {
        impl Default for $Name {
            #[inline]
            fn default() -> Self {
                Self::defaults()
            }
        }
        impl $Name {
            /// Feed the next bar and get this bar's value: `NaN` while warming up. This IS
            /// [`crate::Indicator::on_bar`], bit for bit, without the `Vec`.
            #[inline]
            pub fn next(&mut self, bar: &Bar) -> f64 {
                self.step(bar)
            }
        }
    };
}

/// The impl items every type here shares: `value` (`scalar` for an `f64` `last`, `lines` for an
/// `[f64; N]`), `name`, and the exact-warm-up flag (every replaced block used a `lookback =` clause,
/// which is arms 2 and 3 of `hist_indicator!`).
macro_rules! shared {
    (@rest $key:literal) => {
        fn name(&self) -> &str {
            $key
        }
        fn lookback_exact(&self) -> bool {
            true
        }
    };
    ($key:literal, scalar) => {
        fn value(&self) -> Vec<f64> {
            vec![self.last]
        }
        shared!(@rest $key);
    };
    ($key:literal, lines) => {
        fn value(&self) -> Vec<f64> {
            self.last.to_vec()
        }
        shared!(@rest $key);
    };
}

// ================================== adx ====================================

/// Average directional index (registry key `adx`, parameter `period`, three lines): the trend-strength
/// line `adx` plus the directional indicators `plus_di` and `minus_di`, in that order. The DI lines
/// appear at bar `period`; `adx`, a second Wilder smoothing of the directional movement index, appears
/// at bar `2 * period - 1`. Bars before a line's start hold `NaN`.
#[derive(Clone)]
pub struct Adx {
    period: f64,
    core: crate::fast_c::AdxCore,
    last: [f64; 3],
}
impl Adx {
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: crate::fast_c::AdxCore::new(
                period.round().min(crate::roll::RING_CAP as f64) as usize
            ),
            last: [f64::NAN; 3],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar and get `[adx, plus_di, minus_di]` (the registry row's `outputs`): `NaN`
    /// while warming up. This IS [`crate::Indicator::on_bar`], bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 3] {
        self.step(bar)
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Adx::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 3] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for Adx {
    #[inline]
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Adx, "adx", [period = 14.0]);
impl Adx {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 3] {
        self.last = self.core.push(bar.high, bar.low, bar.close);
        self.last
    }
}
impl Indicator for Adx {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::fast_c::adx_bars(&self.core, bars)
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    // +DI/-DI land at `period`; the ADX line itself (output line 0) is a second
    // Wilder smoothing of DX and only lands at `2*period - 1`.
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_full(&self) -> usize {
        (2 * pbars(self.period)).saturating_sub(1)
    }
    shared!("adx", lines);
}

// ================================== adxr ===================================

/// ADX rating (registry key `adxr`, parameter `period`, one line): the average of the `adx` line and
/// the `adx` value `period` bars earlier, `NaN` until both exist (bar `3 * period - 1`). This crate's
/// definition looks back `period` bars, not TA-Lib's `period - 1`.
#[derive(Clone)]
pub struct Adxr {
    period: f64,
    core: AdxrCore,
    last: f64,
}
impl Adxr {
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period_raw: f64) -> Self {
        Self { period: period_raw, core: AdxrCore::new(period(period_raw)), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Adxr::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
scalar_surface!(Adxr);
typed_new_inline!(Adxr, "adxr", [period = 14.0]);
impl Adxr {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for Adxr {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn lookback(&self) -> usize {
        (3 * pbars(self.period)).saturating_sub(1)
    }
    shared!("adxr", scalar);
}

// ================================== natr ===================================

/// Normalized ATR (registry key `natr`, parameter `period`, one line): Wilder's average true range as a
/// percentage of the close, `100 * atr / close`. `NaN` until bar `period`, and on a bar that closes at
/// exactly zero.
#[derive(Clone)]
pub struct Natr {
    period: f64,
    core: NatrCore,
    last: f64,
}
impl Natr {
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period_raw: f64) -> Self {
        Self { period: period_raw, core: NatrCore::new(period(period_raw)), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Natr::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
scalar_surface!(Natr);
typed_new_inline!(Natr, "natr", [period = 14.0]);
impl Natr {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for Natr {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    shared!("natr", scalar);
}

// =============================== supertrend ================================

/// Supertrend (registry key `supertrend`, parameters `period` and `mult`, two lines): an ATR-band trend
/// follower. `supertrend` is the active band (the lower one while the trend is up, the upper one while
/// it is down) and `direction` is `1.0` for up and `-1.0` for down. Both start at bar `period`.
#[derive(Clone)]
pub struct Supertrend {
    period: f64,
    mult: f64,
    core: SupertrendCore,
    last: [f64; 2],
}
impl Supertrend {
    #[inline]
    fn defaults() -> Self {
        Self::build(10.0, 3.0)
    }
    #[inline]
    fn build(period_raw: f64, mult: f64) -> Self {
        Self {
            period: period_raw,
            mult,
            core: SupertrendCore::new(period(period_raw), mult),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(10.0), p.get(1).copied().unwrap_or(3.0))
    }
    /// Feed the next bar and get `[supertrend, direction]` (the registry row's `outputs`): `NaN`
    /// while warming up. This IS [`crate::Indicator::on_bar`], bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.step(bar)
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Supertrend::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for Supertrend {
    #[inline]
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new_inline!(Supertrend, "supertrend", [period = 10.0, mult = 3.0]);
impl Supertrend {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for Supertrend {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 2>(self, bars, Self::step)
    }
    fn reset(&mut self) {
        *self = Self::build(self.period, self.mult);
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    shared!("supertrend", lines);
}

// ================================== smma ===================================

/// Wilder / RMA smoothed moving average of the close (registry key `smma`, parameter `period`, one
/// line): seeded with the simple average of the first `period` closes (bar `period - 1`), then
/// `(prev * (period - 1) + close) / period`.
#[derive(Clone)]
pub struct Smma {
    period: f64,
    core: SmmaCore,
    last: f64,
}
impl Smma {
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period_raw: f64) -> Self {
        Self { period: period_raw, core: SmmaCore::new(period(period_raw)), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Smma::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
scalar_surface!(Smma);
typed_new_inline!(Smma, "smma", [period = 14.0]);
impl Smma {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Indicator for Smma {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    shared!("smma", scalar);
}

// ================================== efi ====================================

/// Elder Force Index (registry key `efi`, parameter `period`, one line): the EMA of
/// `(close - previous close) * volume`. The first bar has no previous close, so the line starts at bar
/// `period`.
///
/// Runs `crate::fast_k::EfiCore`, the one `push` the stream, `vectorize` and `series::efi` share.
#[derive(Clone)]
pub struct Efi {
    period: f64,
    core: EfiCore,
    last: f64,
}
impl Efi {
    #[inline]
    fn defaults() -> Self {
        Self::build(13.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: EfiCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(13.0))
    }
    /// Feed the next bar and get this bar's value: `NaN` while warming up. This IS
    /// [`crate::Indicator::on_bar`], bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next bar's `close` and `volume` as plain numbers, no [`Bar`] needed; the same answer
    /// as [`Efi::next`] for a bar with those values.
    #[inline]
    pub fn next_cv(&mut self, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(close, volume);
        self.last
    }
}
impl Default for Efi {
    #[inline]
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Efi, "efi", [period = 13.0]);
impl Efi {
    // A `NaN` force (bar 0, or a `NaN` price or volume) is neither pushed nor emitted, as
    // `smooth_defined` smoothed only the non-`NaN` raw values and scattered them back.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_cv(bar.close, bar.volume)
    }
}
impl Indicator for Efi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    shared!("efi", scalar);
}

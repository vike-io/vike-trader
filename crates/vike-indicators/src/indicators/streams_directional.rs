//! `dx`, `plus_di`, `minus_di`, `plus_dm`, `minus_dm`, `chaikin_volatility`, `autocorr`,
//! `chandelier` and `starc`, streamed from bounded state.
//!
//! These nine used to be `hist_indicator!` types: every bar re-ran the whole batch kernel over a
//! retained tail of history (measured per bar at 20,000 bars: `chandelier` 53 us, `dx` 24 us, `autocorr`
//! 21 us, `starc` 20 us, `chaikin_volatility` 16 us, the four other directional rows 11-12 us). A
//! Wilder or EMA recurrence has an exact state of a handful of numbers and a fixed window is a ring of
//! its last `period` values, so a bar here costs O(1) for the recurrences and O(window) for a window
//! fold, and nothing is truncated at all.
//!
//! Eight of the nine (everything but `autocorr`) run the cores of `crate::fast_g`, whose `push` is the
//! one implementation of the arithmetic: the stream, `vectorize` and the whole-series call
//! (`series::*`) all run it, so they agree bit for bit (held by `tests/parity`,
//! `tests/streamers_directional_exact.rs` and `tests/fast_g.rs`). The cores multiply by a
//! precomputed `1/n` where the textbook divides, so a value differs from the textbook form by
//! rounding noise only. `autocorr` keeps its ring and its own moments below.
//!
//! # Details that are easy to get wrong
//!
//! * **A windowed fold is recomputed, never slid.** `autocorr`'s moments fold the retained ring from
//!   its oldest value to its newest, the order the batch kernel folds a slice in; `sum += x - old`
//!   would give other bits.
//! * **The five directional rows share ONE core** (`fast_g::DmCore`): the Wilder sums of true range,
//!   +DM and -DM, seeded at bar `period` with the plain sum of bars `1..=period`. Each row only
//!   picks its output, and carries only the sums that output needs.

use super::macros::typed_new;
use super::{pback, pbars};
use crate::Indicator;
use crate::fast_g::{
    ChaikinCore, ChandelierCore, DX, DmCore, MINUS_DI, MINUS_DM, PLUS_DI, PLUS_DM, StarcCore,
    period, typed_new_inline,
};
use crate::window::is_constant_window;
use std::collections::VecDeque;
use vike_marketdata::Bar;

// ============================ shared state =================================

/// Push `x` onto a ring holding the last `cap` values.
fn ring_push(ring: &mut VecDeque<f64>, cap: usize, x: f64) {
    ring.push_back(x);
    if ring.len() > cap {
        ring.pop_front();
    }
}

/// The three sums a Pearson correlation of `x` against `y` is built from, `(Σdx·dy, Σdx², Σdy²)` with
/// the deviations taken from each series' own mean, or `None` when either series is constant. This is
/// `oscillators.rs`'s `moments`, expression for expression.
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
    // `x` and `y` are the same length (the caller slices `pairs` of each), as in the kernel's index loop.
    for (xk, yk) in x.iter().zip(y) {
        let dx = xk - mx;
        let dy = yk - my;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    Some((sxy, sxx, syy))
}

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

/// The impl items every type here shares: `value` (`scalar` is an `f64`, `lines` an `[f64; N]`) and
/// `name`; the `exact` arms also declare the warm-up exact (every replaced block that used a
/// `lookback =` clause, i.e. all but `autocorr`).
macro_rules! shared {
    (@name $key:literal) => {
        fn name(&self) -> &str {
            $key
        }
    };
    ($key:literal, scalar) => {
        fn value(&self) -> Vec<f64> {
            vec![self.last]
        }
        shared!(@name $key);
    };
    ($key:literal, scalar, exact) => {
        shared!($key, scalar);
        fn lookback_exact(&self) -> bool {
            true
        }
    };
    ($key:literal, lines, exact) => {
        fn value(&self) -> Vec<f64> {
            self.last.to_vec()
        }
        shared!(@name $key);
        fn lookback_exact(&self) -> bool {
            true
        }
    };
}

// ============== dx, plus_di, minus_di, plus_dm, minus_dm (one core) ==============

/// One of the five directional rows: a `period` and the shared [`DmCore`] picking the row's line.
macro_rules! directional {
    ($(#[$m:meta])* $Name:ident, $key:literal, $kind:ident) => {
        $(#[$m])*
        #[derive(Clone)]
        pub struct $Name {
            period: f64,
            core: DmCore<$kind>,
            last: f64,
        }
        impl $Name {
            #[inline]
            fn defaults() -> Self {
                Self::build(14.0)
            }
            #[inline]
            fn build(period_raw: f64) -> Self {
                Self { period: period_raw, core: DmCore::new(period(period_raw)), last: f64::NAN }
            }
            #[inline]
            pub(crate) fn with_params(p: &[f64]) -> Self {
                Self::build(p.first().copied().unwrap_or(14.0))
            }
        }
        scalar_surface!($Name);
        typed_new_inline!($Name, $key, [period = 14.0]);
        impl $Name {
            #[inline]
            fn step(&mut self, bar: &Bar) -> f64 {
                self.last = self.core.push(bar.high, bar.low, bar.close);
                self.last
            }
        }
        impl Indicator for $Name {
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
            shared!($key, scalar, exact);
        }
    };
}

/// `next_hlc` for a directional row that reads the close (through the true range).
macro_rules! next_hlc {
    ($($Name:ident),*) => {$(
        impl $Name {
            /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed;
            /// the same answer as `next` for a bar with those prices.
            #[inline]
            pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
                self.last = self.core.push(high, low, close);
                self.last
            }
        }
    )*};
}

/// `next_hl` for a directional movement row, which reads no close.
macro_rules! next_hl {
    ($($Name:ident),*) => {$(
        impl $Name {
            /// Feed the next bar's `high` and `low` as plain numbers, no [`Bar`] needed; the same
            /// answer as `next` for a bar with those prices (the close is not read).
            #[inline]
            pub fn next_hl(&mut self, high: f64, low: f64) -> f64 {
                self.last = self.core.push(high, low, f64::NAN);
                self.last
            }
        }
    )*};
}

directional! {
    /// Directional Movement Index (registry key `dx`, one line, parameter `period`): how one-sided
    /// the directional movement is, `100 * |+DI - -DI| / (+DI + -DI)`, from 0 (balanced) to 100
    /// (all movement in one direction). It is the unsmoothed input of `adx`, Wilder-smoothed over
    /// `period` bars exactly as `adx`'s `plus_di` and `minus_di` lines are. `NaN` for the first
    /// `period` bars.
    Dx, "dx", DX
}
directional! {
    /// Plus Directional Indicator, +DI (registry key `plus_di`, one line, parameter `period`): the
    /// Wilder-smoothed upward directional movement as a percentage of the smoothed true range. It
    /// is the same line as `adx`'s `plus_di`. `NaN` for the first `period` bars.
    PlusDi, "plus_di", PLUS_DI
}
directional! {
    /// Minus Directional Indicator, -DI (registry key `minus_di`, one line, parameter `period`): the
    /// Wilder-smoothed downward directional movement as a percentage of the smoothed true range. It
    /// is the same line as `adx`'s `minus_di`. `NaN` for the first `period` bars.
    MinusDi, "minus_di", MINUS_DI
}
directional! {
    /// Plus Directional Movement, +DM (registry key `plus_dm`, one line, parameter `period`): the
    /// Wilder-smoothed sum of the bar-to-bar rise in the high on bars where that rise beats the
    /// fall in the low. This is the numerator of `plus_di`, in price units. `NaN` for the first
    /// `period` bars.
    PlusDm, "plus_dm", PLUS_DM
}
directional! {
    /// Minus Directional Movement, -DM (registry key `minus_dm`, one line, parameter `period`): the
    /// Wilder-smoothed sum of the bar-to-bar fall in the low on bars where that fall beats the rise
    /// in the high. This is the numerator of `minus_di`, in price units. `NaN` for the first
    /// `period` bars.
    MinusDm, "minus_dm", MINUS_DM
}
next_hlc!(Dx, PlusDi, MinusDi);
next_hl!(PlusDm, MinusDm);

// ============================ chaikin_volatility ===========================

/// Chaikin Volatility (registry key `chaikin_volatility`, one line, parameters `ema_period` and
/// `roc_period`): the percentage change over `roc_period` bars of the exponential moving
/// average (over `ema_period` bars) of the high-low range. Positive while ranges are widening.
/// `NaN` until both the average and the earlier value it is compared with exist, and where that
/// earlier average is zero.
#[derive(Clone)]
pub struct ChaikinVolatility {
    ema_period: f64,
    roc_period: f64,
    core: ChaikinCore,
    last: f64,
}
impl ChaikinVolatility {
    #[inline]
    fn defaults() -> Self {
        Self::build(10.0, 10.0)
    }
    #[inline]
    fn build(ema_period: f64, roc_period: f64) -> Self {
        Self {
            ema_period,
            roc_period,
            core: ChaikinCore::new(period(ema_period), period(roc_period)),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(10.0), p.get(1).copied().unwrap_or(10.0))
    }
    /// Feed the next bar's `high` and `low` as plain numbers, no [`Bar`] needed; the same answer
    /// as [`ChaikinVolatility::next`] for a bar with those prices.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> f64 {
        self.last = self.core.push(high, low);
        self.last
    }
}
scalar_surface!(ChaikinVolatility);
typed_new_inline!(ChaikinVolatility, "chaikin_volatility", [ema_period = 10.0, roc_period = 10.0]);
impl ChaikinVolatility {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_hl(bar.high, bar.low)
    }
}
impl Indicator for ChaikinVolatility {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn reset(&mut self) {
        *self = Self::build(self.ema_period, self.roc_period);
    }
    fn lookback(&self) -> usize {
        pback(self.ema_period, 1) + pbars(self.roc_period)
    }
    shared!("chaikin_volatility", scalar, exact);
}

// ================================== autocorr ===============================

/// Rolling autocorrelation of the close (registry key `autocorr`, one line, parameters `period`
/// and `lag`): the Pearson correlation, over the last `period` closes, between each close and
/// the close `lag` bars later (the `period - lag` overlapping pairs, each side with its own
/// mean). It lies between -1 and 1. `NaN` for the first `period - 1` bars, for a window of
/// identical closes, and always when `period` is smaller than `lag + 2`.
#[derive(Clone)]
pub struct Autocorr {
    period: f64,
    lag: f64,
    n: usize,
    k: usize,
    /// The last `period` closes, newest at the back (stays empty when the correlation is undefined).
    ring: VecDeque<f64>,
    last: f64,
}
impl Autocorr {
    fn defaults() -> Self {
        Self::build(30.0, 1.0)
    }
    fn build(period: f64, lag: f64) -> Self {
        Self {
            period,
            lag,
            n: period.round() as usize,
            k: lag.round() as usize,
            ring: VecDeque::new(),
            last: f64::NAN,
        }
    }
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(30.0), p.get(1).copied().unwrap_or(1.0))
    }
}
scalar_surface!(Autocorr);
typed_new!(Autocorr, "autocorr", [period = 30.0, lag = 1.0]);
impl Autocorr {
    fn step(&mut self, bar: &Bar) -> f64 {
        let mut out = f64::NAN;
        // The kernel needs at least two (value, lagged value) pairs, or the correlation is undefined.
        if self.k != 0 && self.n >= self.k.saturating_add(2) {
            ring_push(&mut self.ring, self.n, bar.close);
            if self.ring.len() == self.n {
                let pairs = self.n - self.k;
                let k = self.k;
                let w = self.ring.make_contiguous();
                if let Some((sxy, sxx, syy)) = moments(&w[..pairs], &w[k..])
                    && sxx > 0.0
                    && syy > 0.0
                {
                    out = (sxy / (sxx * syy).sqrt()).clamp(-1.0, 1.0);
                }
            }
        }
        self.last = out;
        self.last
    }
}
impl Indicator for Autocorr {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn reset(&mut self) {
        *self = Self::build(self.period, self.lag);
    }
    // No `lookback =` clause in the replaced block: the default (0, not exact) is kept.
    shared!("autocorr", scalar);
}

// ================================= chandelier ==============================

/// Chandelier Exit (`chandelier`): two volatility stops hung from the extremes of the last
/// `period` bars. Parameters `period` (default 22) and `mult` (default 3.0); output lines
/// `long_stop` (the stop for a long position) and `short_stop` (for a short), both `NaN` for
/// the first `period - 1` bars.
///
/// Formula. `long_stop = highest high of the last period bars - mult * ATR(period)` and
/// `short_stop = lowest low of the last period bars + mult * ATR(period)`, where the ATR is
/// Wilder's, seeded with the mean of the first `period` true ranges (the first true range is
/// `high - low`).
#[derive(Clone)]
pub struct Chandelier {
    period: f64,
    mult: f64,
    core: ChandelierCore,
    last: [f64; 2],
}
impl Chandelier {
    #[inline]
    fn defaults() -> Self {
        Self::build(22.0, 3.0)
    }
    #[inline]
    fn build(period_raw: f64, mult: f64) -> Self {
        Self {
            period: period_raw,
            mult,
            core: ChandelierCore::new(period(period_raw), mult),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(22.0), p.get(1).copied().unwrap_or(3.0))
    }
    /// Feed the next bar and get `[long_stop, short_stop]` (the registry row's `outputs`): `NaN`
    /// while warming up. This IS [`crate::Indicator::on_bar`], bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.step(bar)
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Chandelier::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for Chandelier {
    #[inline]
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new_inline!(Chandelier, "chandelier", [period = 22.0, mult = 3.0]);
impl Chandelier {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for Chandelier {
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
        pback(self.period, 1)
    }
    shared!("chandelier", lines, exact);
}

// =================================== starc =================================

/// STARC bands (`starc`, Stoller Average Range Channels): a short moving average with bands a
/// multiple of the average true range away. Parameters `sma_period` (default 6), `atr_period`
/// (default 15) and `mult` (default 2.0); output lines `upper`, `mid` and `lower`, all `NaN`
/// until both the average and the ATR exist (index `max(sma_period, atr_period) - 1`).
///
/// Formula. `mid = SMA(close, sma_period)`; `upper = mid + mult * ATR(atr_period)` and
/// `lower = mid - mult * ATR(atr_period)`, the ATR being Wilder's, seeded with the mean of the
/// first `atr_period` true ranges.
#[derive(Clone)]
pub struct Starc {
    sma_period: f64,
    atr_period: f64,
    mult: f64,
    core: StarcCore,
    last: [f64; 3],
}
impl Starc {
    #[inline]
    fn defaults() -> Self {
        Self::build(6.0, 15.0, 2.0)
    }
    #[inline]
    fn build(sma_period: f64, atr_period: f64, mult: f64) -> Self {
        Self {
            sma_period,
            atr_period,
            mult,
            core: StarcCore::new(period(sma_period), period(atr_period), mult),
            last: [f64::NAN; 3],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(
            p.first().copied().unwrap_or(6.0),
            p.get(1).copied().unwrap_or(15.0),
            p.get(2).copied().unwrap_or(2.0),
        )
    }
    /// Feed the next bar and get `[upper, mid, lower]` (the registry row's `outputs`): `NaN`
    /// while warming up. This IS [`crate::Indicator::on_bar`], bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 3] {
        self.step(bar)
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Starc::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 3] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for Starc {
    #[inline]
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new_inline!(Starc, "starc", [sma_period = 6.0, atr_period = 15.0, mult = 2.0]);
impl Starc {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 3] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for Starc {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 3>(self, bars, Self::step)
    }
    fn reset(&mut self) {
        *self = Self::build(self.sma_period, self.atr_period, self.mult);
    }
    fn lookback(&self) -> usize {
        pback(self.sma_period, 1).max(pback(self.atr_period, 1))
    }
    shared!("starc", lines, exact);
}

//! Bounded-state streamers for four oscillators: `wavetrend`, `qqe`, `stc` (the Schaff trend cycle)
//! and `percent_rank`. Each is written from its published definition, as a chain of this crate's
//! own averages and windows (`Ema`, `Sma`, `Rsi`, `Donchian`) stepped once a bar. `vectorize` is the
//! stream folded over a reset clone (`roll::batch_scalar` / `roll::batch_array`), so `on_bar`
//! equals `vectorize` bit for bit by construction; every type is hand-written
//! (`trims_history() == false`).
//!
//! The definitions and where they are published:
//!
//! - **WaveTrend**: "WaveTrend Oscillator Indicator: Formula and Signals", The Right Trader (2026),
//!   <https://therighttrader.com/learn/indicators/wavetrend-oscillator>. With `ap` the typical price
//!   `(high + low + close) / 3`: `esa = EMA(ap, channel)`, `d = EMA(|ap - esa|, channel)`,
//!   `ci = (ap - esa) / (0.015 * d)`, `wt1 = EMA(ci, average)`, `wt2 = SMA(wt1, 4)`. The page names
//!   no single inventor; its `0.015` is the constant of Donald R. Lambert's commodity channel index
//!   ("Commodity Channel Index: Tools for Trading Cyclic Trends", Commodities, October 1980).
//! - **QQE** (quantitative qualitative estimation): "QQE Indicator: Formula, Defaults, Signals, and
//!   Rules", The Right Trader (2026), <https://therighttrader.com/learn/indicators/qqe-indicator>.
//!   `fast = EMA(RSI(close, n), smoothing)`; the range is `EMA(EMA(|fast - fast[1]|, w), w) *
//!   factor` with Wilder's period `w = 2n - 1`; a long trail sits the range under `fast` and only
//!   rises while `fast` stays above it, a short trail sits the range over `fast` and only falls
//!   while `fast` stays under it; the QQE line is the long trail in an up state and the short trail
//!   in a down state. The RSI is J. Welles Wilder Jr.'s ("New Concepts in Technical Trading
//!   Systems", 1978).
//! - **Schaff trend cycle**: Brian Twomey, "The Schaff Trend Cycle", Technical Analysis of Stocks &
//!   Commodities V. 28:4 (April 2010), <https://store.traders.com/stcov28997at.html>, which prints
//!   Doug Schaff's formula (Doug Schaff, "Catching Currency Moves With The Schaff Trend Cycle
//!   Indicator", Chartpoint, July/August 2002): the MACD `EMA(fast) - EMA(slow)`; its place in its
//!   `cycle`-bar range in percent, kept while the range is not positive; that smoothed by half
//!   toward each new value from the first bar on; the same two steps once more over the smoothed
//!   line.
//! - **Percent rank**: `100 * F(x)`, `F` the empirical distribution function of the previous
//!   `length` values, `F(x) = #{x_i <= x} / length` (A. W. van der Vaart, "Asymptotic Statistics",
//!   Cambridge University Press, 1998, ch. 19;
//!   <https://en.wikipedia.org/wiki/Empirical_distribution_function>).

use std::collections::VecDeque;

use super::macros::typed_new;
use super::{Donchian, Ema, Rsi, Sma, pbars};
use crate::{Indicator, strategy_kit};
use vike_marketdata::Bar;

/// An EMA over `length` that skips a missing input: each stage of a chain starts at its input's
/// first number.
#[inline]
fn late_ema(length: f64) -> Ema {
    Ema::with_params(&[length]).with_skip_nan(true)
}

/// Did `a` and `b` change order since the last bar (`last` = that bar's `(a, b)`)? Strictly on one
/// side now, and on the other side or level then; a `NaN` on either bar is no cross.
#[inline]
fn crossed((a, b): (f64, f64), (a1, b1): (f64, f64)) -> bool {
    (a > b && a1 <= b1) || (a < b && a1 >= b1)
}

// ============================================================================ wavetrend

/// The WaveTrend oscillator (registry key `wavetrend`, two lines `[wavetrend, signal]`, parameters
/// `channel` and `average`, defaults 10 and 21): `EMA(ci, average)` of the channel index
/// `ci = (ap - esa) / (0.015 * d)`, `esa = EMA(ap, channel)`, `d = EMA(|ap - esa|, channel)`, and
/// its signal line, the SMA of the last 4 values. `ap` is the typical price; [`Wavetrend::next_value`]
/// takes any source. Each average starts at its input's first number, so the line is `NaN` for the
/// first `2 * (channel - 1) + average - 1` bars and the signal 3 bars longer. A `NaN` source is
/// skipped by every stage.
#[derive(Clone)]
pub struct Wavetrend {
    channel: f64,
    average: f64,
    esa: Ema,
    spread: Ema,
    tci: Ema,
    signal: Sma,
    last: [f64; 2],
}
impl Wavetrend {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(10.0, 21.0)
    }
    #[inline]
    fn build(channel: f64, average: f64) -> Self {
        Self {
            channel,
            average,
            esa: late_ema(channel),
            spread: late_ema(channel),
            tci: late_ema(average),
            signal: Sma::with_params(&[4.0]),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(10.0), p.get(1).copied().unwrap_or(21.0))
    }
    /// Feed the next bar and get `[wavetrend, signal]` of its typical price; this IS
    /// `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Wavetrend::next`] for a bar with those values.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.next_value(vike_math::price_transforms::typical_price(high, low, close))
    }
    /// Feed the next value of any source (the registry row's is the typical price) and get
    /// `[wavetrend, signal]`.
    #[inline]
    pub fn next_value(&mut self, ap: f64) -> [f64; 2] {
        let esa = self.esa.next_value(ap);
        let d = self.spread.next_value((ap - esa).abs());
        let wt1 = self.tci.next_value((ap - esa) / (0.015 * d));
        let wt2 = if wt1.is_nan() { f64::NAN } else { self.signal.next_value(wt1) };
        self.last = [wt1, wt2];
        self.last
    }
    /// The bar at which the `wavetrend` line first exists.
    fn warm(&self) -> usize {
        2 * pbars(self.channel).saturating_sub(1) + pbars(self.average).saturating_sub(1)
    }
}
impl Default for Wavetrend {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Wavetrend, "wavetrend", [channel = 10.0, average = 21.0]);
impl Indicator for Wavetrend {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array(self, bars, Self::next)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        *self = Self::build(self.channel, self.average);
    }
    fn name(&self) -> &str {
        "wavetrend"
    }
    fn lookback(&self) -> usize {
        self.warm()
    }
    fn lookback_full(&self) -> usize {
        self.warm() + 3
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// ============================================================================ qqe

/// QQE, quantitative qualitative estimation (registry key `qqe`, two lines `[qqe, trail]`,
/// parameters `rsi_period`, `smoothing` and `factor`, defaults 14, 5 and 4.236). `qqe` is
/// `EMA(RSI(close, rsi_period), smoothing)`; `trail` is the long trail in an up state and the short
/// trail in a down state. With `range = EMA(EMA(|qqe - qqe[1]|, w), w) * factor`, `w = 2 *
/// rsi_period - 1`: the long trail is `max(long[1], qqe - range)` while `qqe` was and is above
/// `long[1]`, else `qqe - range`; the short trail is `min(short[1], qqe + range)` while `qqe` was and
/// is under `short[1]`, else `qqe + range`. The state starts up, turns up when `qqe` crosses
/// `short[1]` and down when it crosses `long[1]` (a cross is a change of side since the last bar).
/// Each average starts at its input's first number: `qqe` is `NaN` for the first
/// `rsi_period + smoothing - 1` bars, `trail` for `2 * (w - 1) + 1` more.
#[derive(Clone)]
pub struct Qqe {
    p: [f64; 3],
    rs_form: bool,
    rsi: Rsi,
    fast: Ema,
    move_ema: Ema,
    range_ema: Ema,
    /// `qqe` on the last bar.
    fast1: f64,
    /// The trails as the last bar left them, and as the bar before it did.
    long: [f64; 2],
    short: [f64; 2],
    up: bool,
    last: [f64; 2],
}
impl Qqe {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build([14.0, 5.0, 4.236], false)
    }
    #[inline]
    fn build(p: [f64; 3], rs_form: bool) -> Self {
        let wilder = 2.0 * pbars(p[0]) as f64 - 1.0;
        Self {
            p,
            rs_form,
            rsi: Rsi::with_params(&[p[0]]).with_rs_form(rs_form),
            fast: late_ema(p[1]),
            move_ema: late_ema(wilder),
            range_ema: late_ema(wilder),
            fast1: f64::NAN,
            long: [f64::NAN; 2],
            short: [f64::NAN; 2],
            up: true,
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let mut all = [14.0, 5.0, 4.236];
        for (slot, given) in all.iter_mut().zip(p) {
            *slot = *given;
        }
        Self::build(all, false)
    }
    /// Option `rs_form`, `false` by default: the RSI is [`Rsi::with_rs_form`]'s, the platforms'
    /// arithmetic (a `NaN` close skipped); the rest is unchanged.
    #[inline]
    pub fn with_rs_form(self, rs_form: bool) -> Self {
        Self::build(self.p, rs_form)
    }
    /// Feed the next bar and get `[qqe, trail]`; this IS `Indicator::on_bar`, bit for bit, without
    /// the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_value(bar.close)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Qqe::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> [f64; 2] {
        let fast = self.fast.next_value(self.rsi.next_value(close));
        let range = self.range_ema.next_value(self.move_ema.next_value((self.fast1 - fast).abs()))
            * self.p[2];
        let [long1, long2] = self.long;
        let [short1, short2] = self.short;
        let (to_long, to_short) = (fast - range, fast + range);
        let long = if self.fast1 > long1 && fast > long1 {
            strategy_kit::max(long1, to_long)
        } else {
            to_long
        };
        let short = if self.fast1 < short1 && fast < short1 {
            strategy_kit::min(short1, to_short)
        } else {
            to_short
        };
        if crossed((fast, short1), (self.fast1, short2)) {
            self.up = true;
        } else if crossed((fast, long1), (self.fast1, long2)) {
            self.up = false;
        }
        self.fast1 = fast;
        self.long = [long, long1];
        self.short = [short, short1];
        self.last = [fast, if self.up { long } else { short }];
        self.last
    }
    /// The bar at which `qqe` first exists.
    fn warm(&self) -> usize {
        pbars(self.p[0]) + pbars(self.p[1]).saturating_sub(1)
    }
}
impl Default for Qqe {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Qqe, "qqe", [rsi_period = 14.0, smoothing = 5.0, factor = 4.236]);
impl Indicator for Qqe {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array(self, bars, Self::next)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        *self = Self::build(self.p, self.rs_form);
    }
    fn name(&self) -> &str {
        "qqe"
    }
    fn lookback(&self) -> usize {
        self.warm()
    }
    fn lookback_full(&self) -> usize {
        self.warm() + 2 * (2 * pbars(self.p[0])).saturating_sub(2) + 1
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// ============================================================================ stc

/// The Schaff trend cycle (registry key `stc`, one line, parameters `fast`, `slow` and `cycle`,
/// defaults 23, 50 and 10), bounded to `0..=100`. The MACD `EMA(close, fast) - EMA(close, slow)`;
/// `%K`, its place in the range of its last `cycle` values in percent, kept while that range is not
/// positive (0 before it ever is, which covers the averages' warm-up and the first `cycle - 1`
/// MACD values); `%D`, `%K` smoothed by half toward each new value (`d + 0.5 * (k - d)`) from the
/// first bar on; then the same two steps over `%D`, and that last smoothing is the line. Never
/// `NaN` on numbers: it reads 0 until the MACD's window has a range.
#[derive(Clone)]
pub struct Stc {
    p: [f64; 3],
    fast: Ema,
    slow: Ema,
    macd_window: Donchian,
    d_window: Donchian,
    k: f64,
    d: f64,
    kd: f64,
    last: f64,
}
/// How far each smoothing moves toward its new value.
const STC_FACTOR: f64 = 0.5;
impl Stc {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build([23.0, 50.0, 10.0])
    }
    #[inline]
    fn build(p: [f64; 3]) -> Self {
        Self {
            p,
            fast: Ema::with_params(&[p[0]]),
            slow: Ema::with_params(&[p[1]]),
            macd_window: Donchian::with_params(&[p[2]]),
            d_window: Donchian::with_params(&[p[2]]),
            k: 0.0,
            d: f64::NAN,
            kd: 0.0,
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let mut all = [23.0, 50.0, 10.0];
        for (slot, given) in all.iter_mut().zip(p) {
            *slot = *given;
        }
        Self::build(all)
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Stc::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        let macd = self.fast.next_value(close) - self.slow.next_value(close);
        let [hi, _, lo] = self.macd_window.next_hl(macd, macd);
        if hi - lo > 0.0 {
            self.k = (macd - lo) / (hi - lo) * 100.0;
        }
        self.d = if self.d.is_nan() { self.k } else { self.d + STC_FACTOR * (self.k - self.d) };
        let [hi, _, lo] = self.d_window.next_hl(self.d, self.d);
        if hi - lo > 0.0 {
            self.kd = (self.d - lo) / (hi - lo) * 100.0;
        }
        self.last = if self.last.is_nan() {
            self.kd
        } else {
            self.last + STC_FACTOR * (self.kd - self.last)
        };
        self.last
    }
}
impl Default for Stc {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Stc, "stc", [fast = 23.0, slow = 50.0, cycle = 10.0]);
impl Indicator for Stc {
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
        *self = Self::build(self.p);
    }
    fn name(&self) -> &str {
        "stc"
    }
}

// ============================================================================ percent_rank

/// Percent rank (registry key `percent_rank`, one line, parameter `length`, default 100): the
/// percent of the previous `length` values that are at or below this one, `100 * #{x_i <= x} /
/// length`, this value left out. `NaN` until `length` values came before it and for a `NaN` value;
/// a `NaN` in the window is not at or below anything.
#[derive(Clone)]
pub struct PercentRank {
    n: usize,
    seen: VecDeque<f64>,
    last: f64,
}
impl PercentRank {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::with_params(&[100.0])
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n = pbars(p.first().copied().unwrap_or(100.0));
        Self { n, seen: VecDeque::with_capacity(n + 1), last: f64::NAN }
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
    /// Feed the next value as a plain number, no [`Bar`] needed; the same answer as
    /// [`PercentRank::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, x: f64) -> f64 {
        let full = self.seen.len() == self.n;
        self.last = if full && !x.is_nan() {
            100.0 * self.seen.iter().filter(|&&v| v <= x).count() as f64 / self.n as f64
        } else {
            f64::NAN
        };
        if full {
            self.seen.pop_front();
        }
        self.seen.push_back(x);
        self.last
    }
}
impl Default for PercentRank {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(PercentRank, "percent_rank", [length = 100.0]);
impl Indicator for PercentRank {
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
        *self = Self::with_params(&[self.n as f64]);
    }
    fn name(&self) -> &str {
        "percent_rank"
    }
    fn lookback(&self) -> usize {
        self.n
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

//! Four fixed-window indicators streamed from bounded state instead of a history recompute:
//! `ttm_squeeze`, `acceleration_bands`, `rwi` and `qstick`.
//!
//! As `hist_indicator!` types each bar re-ran the whole batch kernel over a retained tail of history
//! (measured per bar at 20k bars, default parameters: `ttm_squeeze` 255 us and growing with the
//! series, `acceleration_bands` 30 us, `rwi` 12 us). None of them has a recursion: the value at a bar
//! is a function of the last `period` bars, so the streaming state is a few rings with running sums,
//! and a bar costs O(1) and grows nothing.
//!
//! All four run the cores in `crates/vike-indicators/src/fast_i.rs` (one `push` shared with
//! `series::*`, so a stream, `vectorize` and a slice call agree bit for bit): the windows are carried
//! as running sums rebuilt from their rings every few hundred bars, so a value differs from the
//! naive left fold of its window in the last bits only. The proof that `on_bar` folded over a series
//! equals `vectorize` is `tests/streamers_bands_exact.rs`.
//!
//! Every type is hand-written in the `base/incremental.rs` sense (`trims_history() == false`).

use super::macros::typed_new;
use super::{pback, pbars};
use crate::Indicator;
use crate::fast_i::{AccelCore, QstickCore, RwiCore, TtmCore};
use vike_marketdata::Bar;

// ================================ ttm_squeeze ==============================

/// TTM Squeeze (`ttm_squeeze`): a volatility-compression flag plus a momentum histogram.
/// Parameters `length` (default 20), `bb_mult` (Bollinger width, default 2.0) and `kc_mult`
/// (Keltner width, default 1.5); output lines `momentum` (histogram) and `squeeze` (dots,
/// `1.0` while the squeeze is on, else `0.0`). `squeeze` is `NaN` for the first `length - 1`
/// bars and `momentum` for the first `2 * length - 2`.
///
/// Formula. With `mid = SMA(close, length)`: the Bollinger bands are `mid +/- bb_mult * s`,
/// `s` the population standard deviation of the close over `length`; the Keltner channel is
/// `mid +/- kc_mult * SMA(true range, length)`. The squeeze is ON (`1.0`) when the Bollinger
/// bands lie strictly inside the Keltner channel. The momentum is the linear-regression value
/// (the fitted line's value at the last bar) over `length` bars of
/// `close - ((highest high + lowest low) / 2 + mid) / 2`, the highest high and lowest low also
/// taken over `length` bars.
#[derive(Clone)]
pub struct TtmSqueeze {
    length: f64,
    bb_mult: f64,
    kc_mult: f64,
    core: TtmCore,
    last: [f64; 2],
}
impl TtmSqueeze {
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0, 2.0, 1.5)
    }
    #[inline]
    fn build(length: f64, bb_mult: f64, kc_mult: f64) -> Self {
        Self {
            length,
            bb_mult,
            kc_mult,
            core: TtmCore::new(
                length.round().min(crate::roll::RING_CAP as f64) as usize,
                bb_mult,
                kc_mult,
            ),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let length = p.first().copied().unwrap_or(20.0);
        let bb_mult = p.get(1).copied().unwrap_or(2.0);
        let kc_mult = p.get(2).copied().unwrap_or(1.5);
        Self::build(length, bb_mult, kc_mult)
    }
    /// Feed the next bar and get `[momentum, squeeze]`; this IS `Indicator::on_bar`, bit for bit,
    /// without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.step(bar)
    }
    /// Feed the next bar's high, low and close as plain numbers, no [`Bar`] needed; the same
    /// answer as [`TtmSqueeze::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for TtmSqueeze {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(TtmSqueeze, "ttm_squeeze", [length = 20.0, bb_mult = 2.0, kc_mult = 1.5]);
impl TtmSqueeze {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for TtmSqueeze {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.length, self.bb_mult, self.kc_mult).core;
        let mut out = [vec![f64::NAN; bars.len()], vec![f64::NAN; bars.len()]];
        let [a, b] = &mut out;
        for ((x, y), bar) in a.iter_mut().zip(b.iter_mut()).zip(bars) {
            let v = core.push(bar.high, bar.low, bar.close);
            *x = v[0];
            *y = v[1];
        }
        out.into()
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        *self = Self::build(self.length, self.bb_mult, self.kc_mult);
    }
    fn name(&self) -> &str {
        "ttm_squeeze"
    }
    fn lookback(&self) -> usize {
        pback(self.length, 1)
    }
    fn lookback_full(&self) -> usize {
        (2 * pbars(self.length)).saturating_sub(2)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// ============================ acceleration_bands ===========================

/// Headley's acceleration bands (`acceleration_bands`): an envelope that widens with each
/// bar's own range. Parameters `period` (default 20) and `factor` (the width multiplier,
/// default 4.0); output lines `upper`, `mid` and `lower`, `NaN` for the first `period - 1`
/// bars.
///
/// Formula. With `w = (high - low) / (high + low)` per bar (0 when `high + low` is 0):
/// `upper = SMA(high * (1 + factor * w), period)`, `lower = SMA(low * (1 - factor * w), period)`
/// and `mid = SMA(close, period)`.
#[derive(Clone)]
pub struct AccelerationBands {
    period: f64,
    factor: f64,
    core: AccelCore,
    last: [f64; 3],
}
impl AccelerationBands {
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0, 4.0)
    }
    #[inline]
    fn build(period: f64, factor: f64) -> Self {
        Self {
            period,
            factor,
            core: AccelCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize, factor),
            last: [f64::NAN; 3],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let period = p.first().copied().unwrap_or(20.0);
        let factor = p.get(1).copied().unwrap_or(4.0);
        Self::build(period, factor)
    }
    /// Feed the next bar and get `[upper, mid, lower]`; this IS `Indicator::on_bar`, bit for bit,
    /// without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 3] {
        self.step(bar)
    }
    /// Feed the next bar's high, low and close as plain numbers, no [`Bar`] needed; the same
    /// answer as [`AccelerationBands::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 3] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for AccelerationBands {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(AccelerationBands, "acceleration_bands", [period = 20.0, factor = 4.0]);
impl AccelerationBands {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 3] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for AccelerationBands {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.period, self.factor).core;
        let mut out =
            [vec![f64::NAN; bars.len()], vec![f64::NAN; bars.len()], vec![f64::NAN; bars.len()]];
        let [a, b, d] = &mut out;
        for (((x, y), z), bar) in a.iter_mut().zip(b.iter_mut()).zip(d.iter_mut()).zip(bars) {
            let v = core.push(bar.high, bar.low, bar.close);
            *x = v[0];
            *y = v[1];
            *z = v[2];
        }
        out.into()
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        *self = Self::build(self.period, self.factor);
    }
    fn name(&self) -> &str {
        "acceleration_bands"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// =================================== rwi ===================================

/// Random Walk Index (registry key `rwi`, two lines `rwi_high` and `rwi_low`, parameter
/// `period`): how far price has moved over `period` bars compared with what a random walk of
/// that length would cover. `rwi_high` is `(high - low period bars ago) / (ATR * sqrt(period))`
/// and `rwi_low` is `(high period bars ago - low) / (ATR * sqrt(period))`, with ATR the plain
/// average of the last `period` true ranges. A reading above 1 suggests a trend rather than
/// noise. `NaN` for the first `period` bars, and where the average range is zero.
#[derive(Clone)]
pub struct Rwi {
    period: f64,
    core: RwiCore,
    last: [f64; 2],
}
impl Rwi {
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: RwiCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar and get `[rwi_high, rwi_low]`; this IS `Indicator::on_bar`, bit for bit,
    /// without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.step(bar)
    }
    /// Feed the next bar's high, low and close as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Rwi::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for Rwi {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Rwi, "rwi", [period = 14.0]);
impl Rwi {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for Rwi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.period).core;
        let mut out = [vec![f64::NAN; bars.len()], vec![f64::NAN; bars.len()]];
        let [a, b] = &mut out;
        for ((x, y), bar) in a.iter_mut().zip(b.iter_mut()).zip(bars) {
            let v = core.push(bar.high, bar.low, bar.close);
            *x = v[0];
            *y = v[1];
        }
        out.into()
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn name(&self) -> &str {
        "rwi"
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// ================================== qstick =================================

/// Qstick (registry key `qstick`, one line, parameter `period`): the simple average of
/// `close - open` over `period` bars, positive when candles mostly close above their open.
/// `NaN` for the first `period - 1` bars.
#[derive(Clone)]
pub struct Qstick {
    period: f64,
    core: QstickCore,
    last: f64,
}
impl Qstick {
    #[inline]
    fn defaults() -> Self {
        Self::build(8.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: QstickCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(8.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next bar's open and close as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Qstick::next`] for a bar with that open and close.
    #[inline]
    pub fn next_oc(&mut self, open: f64, close: f64) -> f64 {
        self.last = self.core.push(open, close);
        self.last
    }
}
impl Default for Qstick {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Qstick, "qstick", [period = 8.0]);
impl Qstick {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_oc(bar.open, bar.close)
    }
}
impl Indicator for Qstick {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.period).core;
        let mut out = vec![f64::NAN; bars.len()];
        for (o, b) in out.iter_mut().zip(bars) {
            *o = core.push(b.open, b.close);
        }
        vec![out]
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn name(&self) -> &str {
        "qstick"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

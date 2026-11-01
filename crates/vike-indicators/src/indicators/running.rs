//! The eight path-dependent indicators, streamed from bounded state instead of a history recompute.
//!
//! `ad`, `adosc`, `asi`, `mcginley`, `net_volume`, `nvi`, `pvi` and `pvt` each integrate every bar
//! since the mount (or read the previous bar), so `hist_indicator!`'s history trim is unsound for
//! them (`classify.rs`'s `is_path_dependent`). As macro-generated types they therefore kept the WHOLE
//! history and re-ran the batch kernel over it on every bar: O(n) per bar, O(n^2) for a run, and a
//! buffer that never stopped growing. Here each carries the few numbers its recurrence needs, so a
//! bar costs O(1) and nothing grows.
//!
//! The arithmetic is a small core's `push` (`crate::fast_c::AdCore`, `crate::fast_h::AsiCore` and
//! `McginleyCore`, `crate::fast_k` for the rest), the one body the stream, `vectorize` and the
//! whole-series call (`crate::series`) all run, so `on_bar` folded over a series equals `vectorize`
//! bit for bit. That is gated: `tests/parity.rs` folds every registry row (and past the length
//! where the old trim used to corrupt these), and `typed_new!`'s generated test holds each type's
//! `new(..)` to its registry row.
//!
//! Every type is hand-written in the `base/incremental.rs` sense (`trims_history() == false`), which
//! is also the right shape for a caller that seeds a streamer from a chart's history: the fold must
//! start at bar zero for a cumulative value, and a hand-written impl is the kind callers fold from
//! bar zero.

use super::macros::typed_new;
use crate::Indicator;
use crate::fast_h::{AsiCore, McginleyCore};
use crate::fast_k::{AdoscCore, NetVolumeCore, PvtCore, VolumeIndexCore};
use vike_marketdata::Bar;

/// The impl items every type here shares. `lookback` stays in each impl: its body reads `self`, and
/// a `self` written outside this macro cannot be seen from inside it.
macro_rules! shared {
    ($key:literal) => {
        fn value(&self) -> Vec<f64> {
            vec![self.last]
        }
        fn name(&self) -> &str {
            $key
        }
        fn lookback_exact(&self) -> bool {
            true
        }
        fn warmup_path_dependent(&self) -> bool {
            true
        }
    };
}

/// Chaikin Accumulation/Distribution line (registry key `ad`, one line, no parameters): a running
/// sum of money-flow volume, `clv * volume` per bar.
#[derive(Clone)]
pub struct Ad {
    core: crate::fast_c::AdCore,
    last: f64,
}
impl Ad {
    #[inline]
    fn defaults() -> Self {
        Self { core: crate::fast_c::AdCore::new(), last: f64::NAN }
    }
    /// Feed the next bar's `high`, `low`, `close` and `volume` as plain numbers, no [`Bar`] needed; the
    /// same answer as [`Ad::next`] for a bar with those values.
    #[inline]
    pub fn next_hlcv(&mut self, high: f64, low: f64, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(high, low, close, volume);
        self.last
    }
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
}
impl Default for Ad {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Ad {
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
}
typed_new!(Ad, "ad", []);
impl Ad {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.last = self.core.push(bar.high, bar.low, bar.close, bar.volume);
        self.last
    }
}
impl Indicator for Ad {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::fast_c::ad_bars(bars)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn lookback(&self) -> usize {
        0
    }
    shared!("ad");
}

/// Chaikin A/D oscillator (registry key `adosc`, one line): the fast EMA of the `ad` line minus
/// its slow EMA, `NaN` until both have seeded.
#[derive(Clone)]
pub struct Adosc {
    fast_n: usize,
    slow_n: usize,
    core: AdoscCore,
    last: f64,
}
impl Adosc {
    #[inline]
    fn defaults() -> Self {
        Self::build(3, 10)
    }
    #[inline]
    fn build(fast_n: usize, slow_n: usize) -> Self {
        Self { fast_n, slow_n, core: AdoscCore::new(fast_n, slow_n), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let fast =
            p.first().copied().unwrap_or(3.0).round().min(crate::roll::RING_CAP as f64) as usize;
        let slow =
            p.get(1).copied().unwrap_or(10.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self::build(fast, slow)
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next bar's `high`, `low`, `close` and `volume` as plain numbers, no [`Bar`] needed; the
    /// same answer as [`Adosc::next`] for a bar with those values.
    #[inline]
    pub fn next_hlcv(&mut self, high: f64, low: f64, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(high, low, close, volume);
        self.last
    }
}
impl Default for Adosc {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Adosc, "adosc", [fast = 3.0, slow = 10.0]);
impl Adosc {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_hlcv(bar.high, bar.low, bar.close, bar.volume)
    }
}
impl Indicator for Adosc {
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
        self.fast_n.max(self.slow_n).saturating_sub(1)
    }
    shared!("adosc");
}

/// Accumulative swing index (registry key `asi`, one line, parameter `limit`): a running sum of
/// Wilder's swing index, which reads the previous bar's open and close. `NaN` on the first bar.
#[derive(Clone)]
pub struct Asi {
    core: AsiCore,
    last: f64,
}
impl Asi {
    #[inline]
    fn defaults() -> Self {
        Self::build(1.0)
    }
    #[inline]
    fn build(limit: f64) -> Self {
        Self { core: AsiCore::new(limit), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(1.0))
    }
    /// Feed the next bar's `open`, `high`, `low` and `close` as plain numbers, no [`Bar`] needed;
    /// the same answer as [`Asi::next`] for a bar with those.
    #[inline]
    pub fn next_ohlc(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(open, high, low, close);
        self.last
    }
}
impl Default for Asi {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Asi {
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_ohlc(bar.open, bar.high, bar.low, bar.close)
    }
}
typed_new!(Asi, "asi", [limit = 1.0]);
impl Indicator for Asi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.next(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::next)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn lookback(&self) -> usize {
        1
    }
    shared!("asi");
}

/// McGinley Dynamic (registry key `mcginley`, one line, parameter `period`): a moving average that
/// speeds up and slows down with how far price has moved from it. Starts at the first close.
#[derive(Clone)]
pub struct Mcginley {
    core: McginleyCore,
    last: f64,
}
impl Mcginley {
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            core: McginleyCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as
    /// [`Mcginley::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Mcginley {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Mcginley {
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
typed_new!(Mcginley, "mcginley", [period = 14.0]);
impl Indicator for Mcginley {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.next(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::next)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn lookback(&self) -> usize {
        0
    }
    shared!("mcginley");
}

/// Signed per-bar volume (registry key `net_volume`, one line, no parameters): the bar's volume
/// when the close rose, its negation when the close fell, zero otherwise. Zero on the first bar.
#[derive(Clone)]
pub struct NetVolume {
    core: NetVolumeCore,
    last: f64,
}
impl NetVolume {
    #[inline]
    fn defaults() -> Self {
        Self { core: NetVolumeCore::new(), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_cv(bar.close, bar.volume)
    }
    /// Feed the next bar's `close` and `volume` as plain numbers, no [`Bar`] needed; the same answer
    /// as [`NetVolume::next`] for a bar with those values.
    #[inline]
    pub fn next_cv(&mut self, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(close, volume);
        self.last
    }
}
impl Default for NetVolume {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(NetVolume, "net_volume", []);
impl Indicator for NetVolume {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.next(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::next)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn lookback(&self) -> usize {
        0
    }
    shared!("net_volume");
}

/// Negative volume index (registry key `nvi`, one line, no parameters): starts at 1000 and moves
/// with the close only on bars whose volume fell from the previous bar.
#[derive(Clone)]
pub struct Nvi {
    core: VolumeIndexCore<false>,
    last: f64,
}
impl Nvi {
    #[inline]
    fn defaults() -> Self {
        Self { core: VolumeIndexCore::new(), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_cv(bar.close, bar.volume)
    }
    /// Feed the next bar's `close` and `volume` as plain numbers, no [`Bar`] needed; the same answer
    /// as [`Nvi::next`] for a bar with those values.
    #[inline]
    pub fn next_cv(&mut self, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(close, volume);
        self.last
    }
}
impl Default for Nvi {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Nvi, "nvi", []);
impl Indicator for Nvi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.next(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::next)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn lookback(&self) -> usize {
        0
    }
    shared!("nvi");
}

/// Positive volume index (registry key `pvi`, one line, no parameters): starts at 1000 and moves
/// with the close only on bars whose volume rose from the previous bar.
#[derive(Clone)]
pub struct Pvi {
    core: VolumeIndexCore<true>,
    last: f64,
}
impl Pvi {
    #[inline]
    fn defaults() -> Self {
        Self { core: VolumeIndexCore::new(), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_cv(bar.close, bar.volume)
    }
    /// Feed the next bar's `close` and `volume` as plain numbers, no [`Bar`] needed; the same answer
    /// as [`Pvi::next`] for a bar with those values.
    #[inline]
    pub fn next_cv(&mut self, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(close, volume);
        self.last
    }
}
impl Default for Pvi {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Pvi, "pvi", []);
impl Indicator for Pvi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.next(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::next)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn lookback(&self) -> usize {
        0
    }
    shared!("pvi");
}

/// Price volume trend (registry key `pvt`, one line, no parameters): a running sum of each bar's
/// volume times the close's percentage change. Zero on the first bar.
#[derive(Clone)]
pub struct Pvt {
    core: PvtCore,
    last: f64,
}
impl Pvt {
    #[inline]
    fn defaults() -> Self {
        Self { core: PvtCore::new(), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_cv(bar.close, bar.volume)
    }
    /// Feed the next bar's `close` and `volume` as plain numbers, no [`Bar`] needed; the same answer
    /// as [`Pvt::next`] for a bar with those values.
    #[inline]
    pub fn next_cv(&mut self, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(close, volume);
        self.last
    }
}
impl Default for Pvt {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Pvt, "pvt", []);
impl Indicator for Pvt {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.next(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::next)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn lookback(&self) -> usize {
        0
    }
    shared!("pvt");
}

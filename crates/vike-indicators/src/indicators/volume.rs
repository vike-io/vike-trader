//! Volume indicators — faithful f64 port of vike-trader-app
//! `core/indicators/volume.py`. Skips names already in `base.rs` (obv/vwap).
//!
//! Every type here is a hand-written bounded-state stepper over a core in `crate::fast_d1` (`cmf`,
//! `mfi`) or `crate::fast_k` (`eom`, `volume_osc`): no batch kernel, their `vectorize` IS the stepper
//! folded over the bars, and a bar costs a few operations whatever the history. The core's `push` is
//! the one body the stream, `vectorize` and the whole-series call (`crate::series`) all run, so they
//! agree bit for bit.
//!
//! ⚠ **This header used to say "and the running-sum forms" are load-bearing.** The old `batch_cmf`'s
//! was, and it was WRONG: a carried sum is not a function of its window, so the history drain
//! returned different bits for the identical bars. The genuinely cumulative series (`ad`, `nvi`,
//! `pvi`, `pvt`, `net_volume`, in `running.rs`) accumulate BY DEFINITION.

use super::{pback, pbars};
use crate::Indicator;
use crate::fast_d1::{CmfCore, MfiCore, window};
use crate::fast_k::{EomCore, VolumeOscCore};
use vike_marketdata::Bar;

// ======================= streaming structs =================================

/// Chaikin money flow over `period` bars (default 20; registry key `cmf`, one line, -1 to 1): the
/// volume-weighted mean of the close location value, `sum(clv * volume) / sum(volume)` over the last
/// `period` bars, `NaN` where the window has no volume. `NaN` for the first `period - 1` values.
///
/// Hand-written bounded state: the window's two totals come from a ring that is turned into exact
/// suffix sums once a lap (`crate::fast_d1::PairSums`), so a zero-volume stretch is exactly zero
/// volume rather than a rounding residue that a division then turns into a number.
#[derive(Clone)]
pub struct Cmf {
    period: f64,
    core: CmfCore,
    last: f64,
}
impl Cmf {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self { period, core: CmfCore::new(window(period)), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=200`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("cmf", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(20.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_hlcv(bar.high, bar.low, bar.close, bar.volume)
    }
    /// Feed the next high, low, close and volume as plain numbers, no [`Bar`] needed; the same answer
    /// as [`Cmf::next`] for a bar with those.
    #[inline]
    pub fn next_hlcv(&mut self, high: f64, low: f64, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(high, low, close, volume);
        self.last
    }
}
impl Default for Cmf {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Cmf {
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
        "cmf"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Ease of movement over `period` bars (default 14; registry key `eom`, one line): the mean of the
/// last `period` values of `(midpoint move) / (volume / (high - low))`, a bar with no range or no
/// volume counting `0`. `NaN` for the first `period` values; a bar whose value is `NaN` (a `NaN`
/// price) is skipped by the mean and emits `NaN`.
///
/// Hand-written bounded state: a ring-sum mean (`crate::fast_k::EomCore`), where the kernel
/// re-folded every window, so a value differs from the textbook fold by rounding noise only.
#[derive(Clone)]
pub struct Eom {
    period: f64,
    core: EomCore,
    last: f64,
}
impl Eom {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: EomCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: f64::NAN,
        }
    }
    /// Build with `period` bars (registry range `2..=200`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("eom", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_hlv(bar.high, bar.low, bar.volume)
    }
    /// Feed the next high, low and volume as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Eom::next`] for a bar with those.
    #[inline]
    pub fn next_hlv(&mut self, high: f64, low: f64, volume: f64) -> f64 {
        self.last = self.core.push(high, low, volume);
        self.last
    }
}
impl Default for Eom {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Eom {
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
        "eom"
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Money flow index over `period` bars (default 14; registry key `mfi`, one line, 0 to 100): the
/// volume-weighted RSI. Raw money flow (typical price `(high + low + close) / 3` times volume) is
/// split by whether the typical price rose or fell, and the index is `100 * up / (up + down)` over
/// the last `period` bars, `100` when nothing flowed down. `NaN` for the first `period` values.
///
/// Hand-written bounded state: the window's two totals come from a ring that is turned into exact
/// suffix sums once a lap (`crate::fast_d1::PairSums`).
#[derive(Clone)]
pub struct Mfi {
    period: f64,
    core: MfiCore,
    last: f64,
}
impl Mfi {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self { period, core: MfiCore::new(window(period)), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=100`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("mfi", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_hlcv(bar.high, bar.low, bar.close, bar.volume)
    }
    /// Feed the next high, low, close and volume as plain numbers, no [`Bar`] needed; the same answer
    /// as [`Mfi::next`] for a bar with those.
    #[inline]
    pub fn next_hlcv(&mut self, high: f64, low: f64, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(high, low, close, volume);
        self.last
    }
}
impl Default for Mfi {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Mfi {
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
        "mfi"
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Volume oscillator (default `short` 5 and `long` 10 bars; registry key `volume_osc`, one line): the
/// percentage gap of a `short`-bar average of the volume above a `long`-bar one,
/// `(short - long) / long * 100`. `NaN` until both have seeded (index `max(short, long) - 1`) and
/// wherever the long average is `0`. A `NaN` volume poisons both averages for good.
///
/// Hand-written bounded state: two exponential averages seeded with the mean of their first values
/// (`crate::fast_k::VolumeOscCore`), the kernel's recurrence term for term.
#[derive(Clone)]
pub struct VolumeOsc {
    short: f64,
    long: f64,
    core: VolumeOscCore,
    last: f64,
}
impl VolumeOsc {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(5.0, 10.0)
    }
    #[inline]
    fn build(short: f64, long: f64) -> Self {
        let cap = crate::roll::RING_CAP as f64;
        Self {
            short,
            long,
            core: VolumeOscCore::new(
                short.round().min(cap) as usize,
                long.round().min(cap) as usize,
            ),
            last: f64::NAN,
        }
    }
    /// Build with `short` and `long` bars (registry ranges `2..=50` and `2..=200`). A value the
    /// registry would not accept (out of range, or `NaN`) is a [`crate::ParamError`] naming it;
    /// [`crate::make_with`] clamps instead. `Default` gives the registry defaults (5, 10).
    #[inline]
    pub fn new(short: f64, long: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[short, long];
        crate::typed::checked("volume_osc", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(5.0), p.get(1).copied().unwrap_or(10.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.volume)
    }
    /// Feed the next VOLUME as a plain number, no [`Bar`] needed; the same answer as
    /// [`VolumeOsc::next`] for a bar with that volume.
    #[inline]
    pub fn next_value(&mut self, volume: f64) -> f64 {
        self.last = self.core.push(volume);
        self.last
    }
}
impl Default for VolumeOsc {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for VolumeOsc {
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
        "volume_osc"
    }
    fn lookback(&self) -> usize {
        pback(self.short.max(self.long), 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;
    use crate::typed::tests::assert_matches_registry;

    #[test]
    fn cmf_is_its_registry_row() {
        assert_matches_registry("cmf", Box::new(Cmf::new(20.0).unwrap()));
    }
    #[test]
    fn mfi_is_its_registry_row() {
        assert_matches_registry("mfi", Box::new(Mfi::new(14.0).unwrap()));
    }
    #[test]
    fn eom_is_its_registry_row() {
        assert_matches_registry("eom", Box::new(Eom::new(14.0).unwrap()));
    }
    #[test]
    fn volume_osc_is_its_registry_row() {
        assert_matches_registry("volume_osc", Box::new(VolumeOsc::new(5.0, 10.0).unwrap()));
    }
}

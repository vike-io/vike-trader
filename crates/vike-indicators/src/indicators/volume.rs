//! Volume indicators — faithful f64 port of vike-trader-app
//! `core/indicators/volume.py`. Skips names already in `base.rs` (obv/vwap).
//! Each `batch_*` is a line-for-line port of the Python function (Python `None`
//! → `f64::NAN`); streaming is history-recompute via [`hist_indicator!`]
//! (correct-by-construction for these causal indicators). Fold order is load-bearing for bit
//! parity. `cmf` and `mfi` are the exception: hand-written bounded-state steppers over the cores in
//! `crate::fast_d1` (no batch kernel; their `vectorize` IS the stepper folded over the bars).
//!
//! ⚠ **This header used to say "and the running-sum forms" are load-bearing.** `batch_cmf`'s was,
//! and it was WRONG: a carried sum is not a function of its window, so the drain returned
//! different bits for the identical bars. The genuinely cumulative series here (`ad`, `nvi`,
//! `pvi`, `pvt`, `net_volume`) accumulate BY DEFINITION and are exempt from the drain via
//! `is_path_dependent` instead.
#![allow(clippy::needless_range_loop)]

use super::macros::hist_indicator;
use super::{pback, pbars};
use crate::Indicator;
use crate::fast_d1::{CmfCore, MfiCore, window};
use crate::math::{Columns, Need, ema, sma, smooth_defined};
use vike_marketdata::Bar;

// ============================ shared local helpers =========================

/// Chaikin Line Value of ONE bar — port of `volume.py:_clv_series`:
/// `((close-low)-(high-close))/(high-low)`, `0.0` when the bar has no range. The single expression
/// both the streaming `ad`/`adosc` in `running.rs` and `crate::fast_d1::CmfCore` (which cannot reach this
/// private module, so it spells the same four operations) evaluate, so they cannot drift.
pub(super) fn clv(h: f64, l: f64, c: f64) -> f64 {
    let rng = h - l;
    if rng != 0.0 { ((c - l) - (h - c)) / rng } else { 0.0 }
}

// ============================ batch kernels ================================

fn batch_eom(bars: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::HIGH | Need::LOW | Need::VOLUME);
    let n = x.h.len();
    let mut raw = vec![f64::NAN; n];
    for i in 1..n {
        let mid_move = ((x.h[i] + x.l[i]) / 2.0) - ((x.h[i - 1] + x.l[i - 1]) / 2.0);
        let hl = x.h[i] - x.l[i];
        if hl != 0.0 && x.v[i] != 0.0 {
            let box_ratio = x.v[i] / hl;
            raw[i] = mid_move / box_ratio;
        } else {
            raw[i] = 0.0;
        }
    }
    vec![smooth_defined(&raw, sma, period)]
}

fn batch_volume_osc(bars: &[Bar], short: usize, long: usize) -> Vec<Vec<f64>> {
    let v = Columns::needing(bars, Need::VOLUME).v;
    let ema_short = ema(&v, short);
    let ema_long = ema(&v, long);
    let n = v.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if !ema_short[i].is_nan() && !ema_long[i].is_nan() && ema_long[i] != 0.0 {
            out[i] = (ema_short[i] - ema_long[i]) / ema_long[i] * 100.0;
        }
    }
    vec![out]
}

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
hist_indicator! {
    /// `eom` — Ease of Movement.
    Eom, "eom", 1, [period = 14.0],
    |bars| batch_eom(bars, period.round().min(crate::roll::RING_CAP as f64) as usize),
    lookback = pbars(period)
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
hist_indicator! {
    /// `volume_osc` — Volume Oscillator (% EMA spread).
    VolumeOsc, "volume_osc", 1, [short = 5.0, long = 10.0],
    |bars| batch_volume_osc(bars, short.round().min(crate::roll::RING_CAP as f64) as usize, long.round().min(crate::roll::RING_CAP as f64) as usize),
    lookback = pback(short.max(long), 1)
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
}

//! Overlap / trend indicators — faithful f64 port of vike-trader-app
//! `core/indicators/overlap.py`. Skips names already in `base.rs`
//! (sma/ema/wma/psar). `ichimoku` reads future bars (forward-shifted senkou + future chikou) so it
//! is `batch_only` in the registry — its `on_bar` fold is a causal best-effort, and its `batch_*`
//! kernel is the one history-recompute left in this file.
//!
//! Every other type here is a hand-written stepper on a core of `crate::fast_d2` (`trima`, `hma`)
//! or `crate::fast_e` (`vwma`, `alma`, `midpoint`, `midprice`, `envelopes`): the stream, `vectorize`
//! and `series::*` run one `push` each, so a bar costs a few nanoseconds (`alma` a dot product of
//! its window) and the three ways in agree bit for bit.
#![allow(clippy::needless_range_loop)]

use super::{pback, pbars};
use crate::Indicator;
use crate::fast_d2::{HmaCore, TrimaCore};
use crate::fast_e::{AlmaCore, EnvelopesCore, Mid1Core, MidCore, VwmaCore, scalar_indicator};
use crate::math::{Columns, Need};
use vike_marketdata::Bar;

// ============================ batch kernels ================================

pub(super) fn batch_ichimoku(
    bars: &[Bar],
    tenkan: usize,
    kijun: usize,
    senkou: usize,
) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::HIGH | Need::LOW | Need::CLOSE);
    let n = x.c.len();

    let donchian_mid = |period: usize| -> Vec<f64> {
        let mut out = vec![f64::NAN; n];
        if n >= period {
            for i in (period - 1)..n {
                let hh = x.h[i + 1 - period..=i].iter().cloned().fold(f64::MIN, f64::max);
                let ll = x.l[i + 1 - period..=i].iter().cloned().fold(f64::MAX, f64::min);
                out[i] = (hh + ll) / 2.0;
            }
        }
        out
    };

    let tenkan_line = donchian_mid(tenkan);
    let kijun_line = donchian_mid(kijun);
    let senkou_b_raw = donchian_mid(senkou);

    // senkou_a raw: average of tenkan and kijun where both defined.
    let mut senkou_a_raw = vec![f64::NAN; n];
    for i in 0..n {
        if !tenkan_line[i].is_nan() && !kijun_line[i].is_nan() {
            senkou_a_raw[i] = (tenkan_line[i] + kijun_line[i]) / 2.0;
        }
    }

    // Forward-shift senkou_a / senkou_b by kijun bars (value at i lands at i+kijun).
    let mut senkou_a = vec![f64::NAN; n];
    let mut senkou_b = vec![f64::NAN; n];
    for i in 0..n {
        if !senkou_a_raw[i].is_nan() {
            let target = i + kijun;
            if target < n {
                senkou_a[target] = senkou_a_raw[i];
            }
        }
        if !senkou_b_raw[i].is_nan() {
            let target = i + kijun;
            if target < n {
                senkou_b[target] = senkou_b_raw[i];
            }
        }
    }

    // chikou: current close plotted kijun bars back → chikou[i] = closes[i+kijun].
    let mut chikou = vec![f64::NAN; n];
    for i in 0..n {
        let target = i + kijun;
        if target < n {
            chikou[i] = x.c[target];
        }
    }

    vec![tenkan_line, kijun_line, senkou_a, senkou_b, chikou]
}

// ======================= streaming structs (macro) =========================

/// Triangular moving average of the close (default period 20; registry key `trima`, one line): the
/// mean over `ceil(period / 2)` closes of the mean over `period / 2 + 1` of them. `NaN` until
/// `period` closes have been seen.
///
/// Two running means in a row (`crate::fast_d2::TrimaCore`), so a bar costs a few nanoseconds
/// whatever the period. The last bits can differ from a fresh fold of each window, by the rounding
/// noise every running-sum average has. A `NaN` close makes the value `NaN` while it is inside the
/// two windows, and the value is exact again the bar after it left.
#[derive(Clone)]
pub struct Trima {
    period: f64,
    core: TrimaCore,
    last: f64,
}
impl Trima {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self { period, core: TrimaCore::new(pbars(period)), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=400`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("trima", raw)?;
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
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Trima::next`]
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
impl Default for Trima {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Trima {
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
        "trima"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Hull moving average of the close (default period 20; registry key `hma`, one line):
/// `WMA(sqrt(period))` of `2 * WMA(period / 2) - WMA(period)`, with `period / 2` and `sqrt(period)`
/// floored and at least 2. `NaN` until `period + sqrt(period) - 2` closes have been seen.
///
/// Three linearly weighted averages kept as running sums (`crate::fast_d2::HmaCore`), the first two
/// over one shared ring of closes, so a bar costs a few nanoseconds whatever the period. The last
/// bits can differ from a fresh fold of each window, by the rounding noise every running-sum
/// average has. A `NaN` close makes the value `NaN` while it is inside the windows, and the value
/// is exact again the bar after it left.
#[derive(Clone)]
pub struct Hma {
    period: f64,
    core: HmaCore,
    last: f64,
}
impl Hma {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self { period, core: HmaCore::new(pbars(period)), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=400`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("hma", raw)?;
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
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Hma::next`]
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
impl Default for Hma {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Hma {
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
        "hma"
    }
    fn lookback(&self) -> usize {
        (pbars(self.period) + (pbars(self.period) as f64).sqrt().floor() as usize).saturating_sub(2)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

scalar_indicator! {
    /// Volume-weighted moving average (`vwma`): the mean of the close over the last `period` bars,
    /// each weighted by its volume, `sum(close * volume) / sum(volume)`. One parameter, `period`
    /// (default 20, registry range `2..=400`); one output line, `NaN` for the first `period - 1`
    /// bars and `NaN` for a window that traded no volume.
    ///
    /// Two running sums on one ring (`crate::fast_e::VwmaCore`), so a bar costs a few nanoseconds
    /// whatever the period. The last bits can differ from a fresh fold of each window, by the
    /// rounding noise every running-sum average has; the sums are rebuilt from the window every
    /// `ceil(512 / period)` laps and whenever the window's volume has fallen to a hundred thousandth
    /// of the largest it has been since, so a volume spike that left the window leaves no residue.
    /// A `NaN` close or volume makes the value `NaN` while it is inside the window, and the value is
    /// exact again the bar after it left.
    Vwma, "vwma", VwmaCore, input = cv,
    [period = 20.0],
    build = VwmaCore::new(pbars(period)),
    lookback = pback(period, 1),
    path_dependent = false
}

scalar_indicator! {
    /// Arnaud Legoux moving average (`alma`): a Gaussian-weighted mean of the close over the last
    /// `period` bars. Parameters `period` (default 20, registry range `2..=400`), `offset` (where
    /// in the window the weights peak, 0 = oldest, 1 = newest; default 0.85) and `sigma` (how
    /// narrow the Gaussian is; default 6.0); one output line, `NaN` for the first `period - 1` bars.
    ///
    /// The weights are `exp(-(k - m)^2 / 2 s^2)` for `k` in `0..period`, with `m = offset *
    /// (period - 1)` and `s = period / sigma`, divided by their sum; `k = period - 1` is the newest
    /// bar. A bar costs one dot product of the window with the weights (`crate::fast_e::AlmaCore`),
    /// summed in eight independent lanes, so the last bits differ from a left-to-right fold. The
    /// value is recomputed from the window each bar: a `NaN` close is `NaN` while it is inside the
    /// window and the value is exact the bar after it left.
    Alma, "alma", AlmaCore, input = value,
    [period = 20.0, offset = 0.85, sigma = 6.0],
    build = AlmaCore::new(pbars(period), offset, sigma),
    lookback = pback(period, 1),
    path_dependent = false
}

scalar_indicator! {
    /// Midpoint (`midpoint`): `(highest close + lowest close) / 2` over the last `period` bars. One
    /// parameter, `period` (default 14, registry range `2..=400`); one output line, `NaN` for the
    /// first `period - 1` bars.
    ///
    /// O(1) a bar (`crate::fast_e::Mid1Core`, block-wise window extremes). A `NaN` close is skipped
    /// as if absent; a window with no number in it is `NaN`.
    Midpoint, "midpoint", Mid1Core, input = value,
    [period = 14.0],
    build = Mid1Core::new(pbars(period)),
    lookback = pback(period, 1),
    path_dependent = false
}

scalar_indicator! {
    /// Midprice (`midprice`): `(highest high + lowest low) / 2` over the last `period` bars. One
    /// parameter, `period` (default 14, registry range `2..=400`); one output line, `NaN` for the
    /// first `period - 1` bars.
    ///
    /// O(1) a bar (`crate::fast_e::MidCore`, block-wise window extremes). A `NaN` high or low is
    /// skipped as if absent; a window with no number in it is `NaN`.
    Midprice, "midprice", MidCore, input = hl,
    [period = 14.0],
    build = MidCore::new(pbars(period)),
    lookback = pback(period, 1),
    path_dependent = false
}

/// Envelopes of the close (default period 20, percent 2.5; registry key `envelopes`, three lines
/// `[upper, mid, lower]`): a simple moving average of `period` closes and the bands `pct` percent
/// above and below it. `NaN` for the first `period - 1` bars.
///
/// A running-sum average (`crate::fast_e::EnvelopesCore`, on `crate::roll::SmaCore`), so a bar
/// costs a few nanoseconds whatever the period; the last bits can differ from a fresh fold of each
/// window. A `NaN` close makes the lines `NaN` while it is inside the window, and they are exact
/// again the bar after it left.
#[derive(Clone)]
pub struct Envelopes {
    period: f64,
    core: EnvelopesCore,
    last: [f64; 3],
}
impl Envelopes {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0, 2.5)
    }
    #[inline]
    fn build(period: f64, pct: f64) -> Self {
        Self { period, core: EnvelopesCore::new(pbars(period), pct), last: [f64::NAN; 3] }
    }
    /// Build with `period` bars (registry range `2..=200`) and the band width `pct` in percent
    /// (`0.1..=20`). A value the registry would not accept (out of range, or `NaN`) is a
    /// [`crate::ParamError`] naming it; [`crate::make_with`] clamps instead. `Default` gives the
    /// registry defaults (20, 2.5).
    #[inline]
    pub fn new(period: f64, pct: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period, pct];
        crate::typed::checked("envelopes", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Parametric constructor — `p` in registry order; a missing entry falls back to the default.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(20.0), p.get(1).copied().unwrap_or(2.5))
    }
    /// Feed the next bar and get `[upper, mid, lower]`; this IS `Indicator::on_bar`, bit for bit,
    /// without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 3] {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; `[upper, mid, lower]`, the same
    /// answer as [`Envelopes::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> [f64; 3] {
        self.last = self.core.push(close);
        self.last
    }
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 3] {
        self.next_value(bar.close)
    }
}
impl Default for Envelopes {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Envelopes {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 3>(self, bars, Self::step)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 3];
    }
    fn name(&self) -> &str {
        "envelopes"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typed::tests::assert_matches_registry;

    #[test]
    fn trima() {
        assert_matches_registry("trima", Box::new(Trima::new(20.0).unwrap()));
    }

    #[test]
    fn hma() {
        assert_matches_registry("hma", Box::new(Hma::new(20.0).unwrap()));
    }

    #[test]
    fn envelopes() {
        assert_matches_registry("envelopes", Box::new(Envelopes::new(20.0, 2.5).unwrap()));
    }
}

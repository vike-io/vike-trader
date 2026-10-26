//! Indicator additions after the first release; kernels and types live here, their registry rows in
//! `registry/` beside the same name.
//!
//! What stays in this file is the BATCH side: the `batch_*` kernels and their private helpers
//! (`directional_lines`, `moments`, `rescaled_range`, ...). They are the specification and the
//! `vectorize` of every type in this family. The TYPES are hand-written bounded-state streamers
//! (`streams_directional.rs`, `streams_stats.rs`, `streams_bands.rs`) and the three cumulative or
//! recurrent series below (`williams_ad`, `heikin_ashi`, `anchored_vwap`), which carry their state
//! by hand, one step per bar, and report `warmup_path_dependent`: the value at a bar depends on
//! where the series started. Every one honours the crate contract, `on_bar` folded over a series
//! equals `vectorize`, bit for bit (`tests/streamers_*_exact.rs`, `tests/all_indicators_exact.rs`).
//!
//! Every squared-deviation fold over a window of data here is guarded by
//! `window::is_constant_window` (a window of one repeated value has no variance and no
//! correlation, it is not a rounding-sized number): the constant-window law the variance-fold gate
//! in the crate's parity tests enforces.
#![allow(clippy::needless_range_loop)]

use super::macros::typed_new;
use crate::Indicator;
use crate::fast_k::{AnchoredVwapCore, WilliamsAdCore};
use vike_marketdata::Bar;

// ===================== hand-written bounded-state streamers ====================
//
// Each carries the few numbers its recurrence needs, so a bar costs O(1). The arithmetic is the batch
// kernel's, through the same step function, so `on_bar` folded equals `vectorize` bit for bit.

/// Williams' accumulation/distribution line (registry key `williams_ad`, one line, no parameters):
/// a running sum of how far each close sits from the true low on an up close or the true high on a
/// down close (true high and low include the previous close), unchanged on a flat close. It ignores
/// volume. Zero on the first bar; the value depends on where the series started.
#[derive(Clone)]
pub struct WilliamsAd {
    core: WilliamsAdCore,
    last: f64,
}
impl WilliamsAd {
    #[inline]
    fn defaults() -> Self {
        Self { core: WilliamsAdCore::new(), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`WilliamsAd::next`] for a bar with those values.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for WilliamsAd {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(WilliamsAd, "williams_ad", []);
impl Indicator for WilliamsAd {
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
        "williams_ad"
    }
    fn lookback(&self) -> usize {
        0
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    fn warmup_path_dependent(&self) -> bool {
        true
    }
}

/// Heikin-Ashi candles (registry key `heikin_ashi`, four lines `ha_open`, `ha_high`, `ha_low` and
/// `ha_close`, no parameters): smoothed candles that make a trend easier to see. The close is the
/// average of the bar's open, high, low and close; the open is the average of the previous candle's
/// open and close (the first candle opens at the average of the bar's open and close); the high and
/// low are the bar's extremes widened to include the candle's open and close. Defined from the first
/// bar; the opens depend on where the series started.
#[derive(Clone)]
pub struct HeikinAshi {
    core: crate::fast_o::HeikinAshiCore,
    last: [f64; 4],
}
impl HeikinAshi {
    fn defaults() -> Self {
        Self { core: crate::fast_o::HeikinAshiCore::new(), last: [f64::NAN; 4] }
    }
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar and get `[ha_open, ha_high, ha_low, ha_close]`; this IS
    /// `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 4] {
        self.next_ohlc(bar.open, bar.high, bar.low, bar.close)
    }
    /// Feed the next bar's `open`, `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the
    /// same answer as [`HeikinAshi::next`] for a bar with those values.
    #[inline]
    pub fn next_ohlc(&mut self, open: f64, high: f64, low: f64, close: f64) -> [f64; 4] {
        self.last = self.core.push(open, high, low, close);
        self.last
    }
}
impl Default for HeikinAshi {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(HeikinAshi, "heikin_ashi", []);
impl Indicator for HeikinAshi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::fast_o::heikin_ashi_bars(bars)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 4];
    }
    fn name(&self) -> &str {
        "heikin_ashi"
    }
    fn lookback(&self) -> usize {
        0
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    fn warmup_path_dependent(&self) -> bool {
        true
    }
}

/// Anchored VWAP (registry key `anchored_vwap`, one line, parameter `anchor_ts`): the volume-weighted
/// average price accumulated from the first bar whose timestamp is at or after `anchor_ts` (epoch
/// milliseconds; the default 0 anchors at the first bar). The typical price `(high + low + close) / 3`
/// is weighted by volume, as in `vwap`, but nothing restarts at a day boundary. `NaN` before the
/// anchor and while no volume has accumulated. The value depends on the anchor and the bar it
/// starts from.
#[derive(Clone)]
pub struct AnchoredVwap {
    core: AnchoredVwapCore,
    last: f64,
}
impl AnchoredVwap {
    #[inline]
    fn defaults() -> Self {
        Self::build(0.0)
    }
    #[inline]
    fn build(anchor_ts: f64) -> Self {
        Self { core: AnchoredVwapCore::new(anchor_ts), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(0.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_ts_hlcv(bar.ts, bar.high, bar.low, bar.close, bar.volume)
    }
    /// Feed the next bar's timestamp (epoch milliseconds), `high`, `low`, `close` and `volume` as plain
    /// numbers, no [`Bar`] needed; the same answer as [`AnchoredVwap::next`] for a bar with those
    /// values.
    #[inline]
    pub fn next_ts_hlcv(&mut self, ts: i64, high: f64, low: f64, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(ts, high, low, close, volume);
        self.last
    }
}
impl Default for AnchoredVwap {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(AnchoredVwap, "anchored_vwap", [anchor_ts = 0.0]);
impl Indicator for AnchoredVwap {
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
        "anchored_vwap"
    }
    // The first bar at or after the anchor is data-dependent, so no warm-up index can be derived
    // from the parameters: `lookback` keeps the conservative 0 and is not exact.
    fn warmup_path_dependent(&self) -> bool {
        true
    }
}

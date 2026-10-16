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
use vike_marketdata::Bar;

// ============================ batch kernels ================================

/// One bar of Williams' accumulation/distribution: the close's distance above the true low on an up
/// close, below the true high on a down close, nothing on an unchanged close.
fn williams_ad_step(high: f64, low: f64, close: f64, prev_close: f64) -> f64 {
    if close > prev_close {
        close - low.min(prev_close)
    } else if close < prev_close {
        close - high.max(prev_close)
    } else {
        0.0
    }
}

/// One Heikin-Ashi candle `[open, high, low, close]`, given the previous candle's `(open, close)`.
fn heikin_ashi_candle(prev: Option<(f64, f64)>, bar: &Bar) -> [f64; 4] {
    let (o, h, l, c) = (bar.open, bar.high, bar.low, bar.close);
    let ha_close = (o + h + l + c) / 4.0;
    let ha_open = match prev {
        None => (o + c) / 2.0,
        Some((prev_open, prev_close)) => (prev_open + prev_close) / 2.0,
    };
    let ha_high = h.max(ha_open).max(ha_close);
    let ha_low = l.min(ha_open).min(ha_close);
    [ha_open, ha_high, ha_low, ha_close]
}

/// Typical price times volume, and volume, of one bar: the two terms an anchored VWAP accumulates.
fn anchored_vwap_terms(bar: &Bar) -> (f64, f64) {
    let tp = (bar.high + bar.low + bar.close) / 3.0;
    (tp * bar.volume, bar.volume)
}

// ======================= streaming structs (macro) =========================

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
    prev_close: Option<f64>,
    acc: f64,
    last: f64,
}
impl WilliamsAd {
    fn defaults() -> Self {
        Self { prev_close: None, acc: 0.0, last: f64::NAN }
    }
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
}
impl Default for WilliamsAd {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(WilliamsAd, "williams_ad", []);
impl WilliamsAd {
    fn step(&mut self, bar: &Bar) -> f64 {
        if let Some(pc) = self.prev_close {
            self.acc += williams_ad_step(bar.high, bar.low, bar.close, pc);
        }
        self.prev_close = Some(bar.close);
        self.last = self.acc;
        self.last
    }
}
impl Indicator for WilliamsAd {
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
        *self = Self::defaults();
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
    prev: Option<(f64, f64)>,
    last: [f64; 4],
}
impl HeikinAshi {
    fn defaults() -> Self {
        Self { prev: None, last: [f64::NAN; 4] }
    }
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar and get `[ha_open, ha_high, ha_low, ha_close]`; this IS
    /// `Indicator::on_bar`, bit for bit, without the Vec.
    pub fn next(&mut self, bar: &Bar) -> [f64; 4] {
        self.step(bar)
    }
}
impl Default for HeikinAshi {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(HeikinAshi, "heikin_ashi", []);
impl HeikinAshi {
    fn step(&mut self, bar: &Bar) -> [f64; 4] {
        let candle = heikin_ashi_candle(self.prev, bar);
        self.prev = Some((candle[0], candle[3]));
        self.last = candle;
        self.last
    }
}
impl Indicator for HeikinAshi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 4>(self, bars, Self::step)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        *self = Self::defaults();
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
    anchor_ts: f64,
    started: bool,
    pv: f64,
    vol: f64,
    last: f64,
}
impl AnchoredVwap {
    fn defaults() -> Self {
        Self::build(0.0)
    }
    fn build(anchor_ts: f64) -> Self {
        Self { anchor_ts, started: false, pv: 0.0, vol: 0.0, last: f64::NAN }
    }
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(0.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
}
impl Default for AnchoredVwap {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(AnchoredVwap, "anchored_vwap", [anchor_ts = 0.0]);
impl AnchoredVwap {
    fn step(&mut self, bar: &Bar) -> f64 {
        if !self.started && (bar.ts as f64) >= self.anchor_ts {
            self.started = true;
        }
        let mut out = f64::NAN;
        if self.started {
            let (term, v) = anchored_vwap_terms(bar);
            self.pv += term;
            self.vol += v;
            out = if self.vol > 0.0 { self.pv / self.vol } else { f64::NAN };
        }
        self.last = out;
        self.last
    }
}
impl Indicator for AnchoredVwap {
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
        *self = Self::build(self.anchor_ts);
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

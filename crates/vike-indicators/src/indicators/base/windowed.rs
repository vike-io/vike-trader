//! The rolling-window incremental family: the nine indicators under that banner and their kernels.

use crate::Indicator;
use vike_marketdata::Bar;

// ============================ batch kernels ================================
// One per indicator; these are the c_*(&Columns) functions from the oracle,
// re-headed to build Columns from &[Bar]. Do NOT "improve" the formulas.

// ===================== rolling-window incremental family ===================
// on_bar keeps only a fixed O(window) buffer. Eight of the nine (wma, bollinger, donchian,
// keltner, stochastic, cci, williams, awesome) now run a core from `crate::roll` or
// `crate::fast_a`: running sums or block extremes with a rebuild, O(1) a bar (cci scans its window
// for the mean absolute deviation). A stream, `vectorize` and `series::*` run the same `push`, so
// they agree bit for bit; the last bits differ from the naive window folds they replaced.

/// Linearly weighted moving average of the close over the last `length` bars (default 20; registry
/// key `wma`, one line). Weights run 1..=`length`, oldest to newest.
//
// `wma` — O(1) a bar: a running weighted sum over the last `n` closes, rebuilt every
// `ceil(512 / n)` laps (`crate::fast_a::WmaCore`).
#[derive(Clone)]
pub struct Wma {
    n: usize,
    core: crate::fast_a::WmaCore,
    last: f64,
}
impl Wma {
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[length];
        crate::typed::checked("wma", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Wma::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
    /// The registry defaults. Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self { n: 20, core: crate::fast_a::WmaCore::new(20), last: f64::NAN }
    }
    /// Parametric constructor — `p[0]` = period.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n =
            p.first().copied().unwrap_or(20.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self { n, core: crate::fast_a::WmaCore::new(n), last: f64::NAN }
    }
}
impl Default for Wma {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Wma {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.last = self.core.push(bar.close);
        self.last
    }
}
impl Indicator for Wma {
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
        "wma"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Bollinger Bands of the close: a `length`-bar mean and `mult` standard deviations either side
/// (registry key `bollinger`, three lines `[upper, mid, lower]`). Parameters: `length` (default
/// 20) and `mult` (default 2.0). The deviation is the population standard deviation of the closes
/// in the window.
//
// `bollinger` — O(1) a bar: the mean and population variance come from shifted running sums of the
// last `n` closes, rebuilt every `ceil(512 / n)` laps (`crate::roll::BollingerCore`). Outputs `[up, mid, lo]`.
#[derive(Clone)]
pub struct Bollinger {
    n: usize,
    core: crate::roll::BollingerCore,
    last: [f64; 3],
}
impl Bollinger {
    /// Build with `length` bars (registry range `1..=1000`) and the band width `mult` in standard
    /// deviations (`0.1..=10.0`). A value the registry would not accept (out of range, or `NaN`) is
    /// a [`crate::ParamError`] naming it; [`crate::make_with`] clamps instead. `Default` gives the
    /// registry defaults (20, 2.0).
    #[inline]
    pub fn new(length: f64, mult: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[length, mult];
        crate::typed::checked("bollinger", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar and get `[upper, mid, lower]`; this IS `Indicator::on_bar`, bit for bit,
    /// without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 3] {
        self.step(bar)
    }
    /// The registry defaults. Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self { n: 20, core: crate::roll::BollingerCore::new(20, 2.0), last: [f64::NAN; 3] }
    }
    /// Parametric constructor — `p[0]` = period (integer, rounded), `p[1]` =
    /// band multiplier (`m`, displayed as `"mult"` in the registry spec — the
    /// field name is unchanged). No sub-state depends on either.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n =
            p.first().copied().unwrap_or(20.0).round().min(crate::roll::RING_CAP as f64) as usize;
        let m = p.get(1).copied().unwrap_or(2.0);
        Self { n, core: crate::roll::BollingerCore::new(n, m), last: [f64::NAN; 3] }
    }
}
impl Default for Bollinger {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Bollinger {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 3] {
        self.last = self.core.push(bar.close);
        self.last
    }
}
impl Indicator for Bollinger {
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
        "bollinger"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Donchian channel: the highest high and lowest low of the last `length` bars and their midpoint
/// (registry key `donchian`, three lines `[upper, mid, lower]`). Parameter: `length` (default 20).
//
// `donchian` — O(1) a bar: block-wise window extremes (`crate::fast_a::DonchianCore`). A NaN high
// or low is skipped as if absent. Outputs `[up, mid, lo]`.
#[derive(Clone)]
pub struct Donchian {
    n: usize,
    core: crate::fast_a::DonchianCore,
    last: [f64; 3],
}
impl Donchian {
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[length];
        crate::typed::checked("donchian", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar and get `[upper, mid, lower]`; this IS `Indicator::on_bar`, bit for bit,
    /// without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 3] {
        self.step(bar)
    }
    /// Feed the next high and low as plain numbers, no [`Bar`] needed; `[upper, mid, lower]`, the
    /// same answer as [`Donchian::next`] for a bar with those.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> [f64; 3] {
        self.last = self.core.push(high, low);
        self.last
    }
    /// The registry defaults. Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self { n: 20, core: crate::fast_a::DonchianCore::new(20), last: [f64::NAN; 3] }
    }
    /// Parametric constructor — `p[0]` = period.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n =
            p.first().copied().unwrap_or(20.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self { n, core: crate::fast_a::DonchianCore::new(n), last: [f64::NAN; 3] }
    }
}
impl Default for Donchian {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Donchian {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 3] {
        self.last = self.core.push(bar.high, bar.low);
        self.last
    }
}
impl Indicator for Donchian {
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
        "donchian"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Keltner channel: an EMA of the close with bands `mult` average true ranges either side
/// (registry key `keltner`, three lines `[upper, mid, lower]`). Parameters: `ema_length` (default
/// 20), `atr_length` (default 10) and `mult` (default 2.0).
//
// `keltner` — TRUE O(1): EMA mid + Wilder-smoothed true-range band, both seeded with a plain mean
// (`crate::fast_a::KeltnerCore`, a branch-free steady state once both are seeded). Outputs
// `[up, mid, lo]`.
#[derive(Clone)]
pub struct Keltner {
    ema_n: usize,
    atr_n: usize,
    core: crate::fast_a::KeltnerCore,
    last: [f64; 3],
}
impl Keltner {
    /// Build with the EMA `ema_length` and the ATR `atr_length` in bars (each registry range
    /// `1..=1000`) and the band width `mult` in ATRs (`0.1..=10.0`). A value the registry would not
    /// accept (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`]
    /// clamps instead. `Default` gives the registry defaults (20, 10, 2.0).
    #[inline]
    pub fn new(ema_length: f64, atr_length: f64, mult: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[ema_length, atr_length, mult];
        crate::typed::checked("keltner", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar and get `[upper, mid, lower]`; this IS `Indicator::on_bar`, bit for bit,
    /// without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 3] {
        self.step(bar)
    }
    /// Feed the next high, low and close as plain numbers, no [`Bar`] needed; `[upper, mid, lower]`,
    /// the same answer as [`Keltner::next`] for a bar with those.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 3] {
        self.last = self.core.push(high, low, close);
        self.last
    }
    /// The registry defaults. Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self {
            ema_n: 20,
            atr_n: 10,
            core: crate::fast_a::KeltnerCore::new(20, 10, 2.0),
            last: [f64::NAN; 3],
        }
    }
    /// Parametric constructor — `p[0]` = EMA period, `p[1]` = ATR period,
    /// `p[2]` = band multiplier.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let ema_n =
            p.first().copied().unwrap_or(20.0).round().min(crate::roll::RING_CAP as f64) as usize;
        let atr_n =
            p.get(1).copied().unwrap_or(10.0).round().min(crate::roll::RING_CAP as f64) as usize;
        let mult = p.get(2).copied().unwrap_or(2.0);
        Self {
            ema_n,
            atr_n,
            core: crate::fast_a::KeltnerCore::new(ema_n, atr_n, mult),
            last: [f64::NAN; 3],
        }
    }
}
impl Default for Keltner {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Keltner {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 3] {
        self.last = self.core.push(bar.high, bar.low, bar.close);
        self.last
    }
}
impl Indicator for Keltner {
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
        "keltner"
    }
    fn lookback(&self) -> usize {
        self.ema_n.saturating_sub(1)
    }
    fn lookback_full(&self) -> usize {
        // The mid line is the EMA; the bands additionally need the ATR, which lands
        // at `atr_n - 1` — either can dominate.
        self.ema_n.max(self.atr_n).saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Volume-weighted average price, restarted each UTC day (registry key `vwap`, one line, no
/// parameters). It is the running sum of typical price times volume divided by the running sum of
/// volume, reset at each new UTC day. Path-dependent: a mid-day start differs from the full
/// day ([`crate::Indicator::warmup_path_dependent`]).
//
// `vwap` — TRUE O(1): running Σ(typical·vol) / Σ(vol), reset on each new UTC-day
// session, identical to `batch_vwap`.
#[derive(Clone)]
pub struct Vwap {
    pv: f64,
    vol: f64,
    last_day: i64,
    last: f64,
}
impl Vwap {
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// The registry defaults (it has no parameters). Private: `Default` is the public spelling.
    fn defaults() -> Self {
        Self { pv: 0.0, vol: 0.0, last_day: i64::MIN, last: f64::NAN }
    }
}
impl Default for Vwap {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Vwap {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    fn step(&mut self, bar: &Bar) -> f64 {
        let day = bar.ts / 86_400_000; // UTC day (ts is epoch-ms); mirrors Columns
        if day != self.last_day {
            self.pv = 0.0;
            self.vol = 0.0;
            self.last_day = day;
        }
        let tp = (bar.high + bar.low + bar.close) / 3.0;
        self.pv += tp * bar.volume;
        self.vol += bar.volume;
        let out = if self.vol > 0.0 { self.pv / self.vol } else { f64::NAN };
        self.last = out;
        self.last
    }
}
impl Indicator for Vwap {
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
        self.pv = 0.0;
        self.vol = 0.0;
        self.last_day = i64::MIN;
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "vwap"
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

/// Stochastic oscillator (registry key `stochastic`, two lines `[%K, %D]`). Parameters: the
/// lookback `length` (default 14) of the raw %K, and the %K and %D smoothing lengths (defaults 3
/// and 3). `%D` warms up after `%K`.
//
// `stochastic` — O(1) a bar: raw %K from block-wise 14-bar extrema, then two running-sum
// smoothings (%K, %D), each NaN while any NaN is in its window (`crate::fast_a::StochasticCore`).
// Outputs `[k, d]`.
#[derive(Clone)]
pub struct Stochastic {
    n: usize,
    sk: usize,
    sd: usize,
    core: crate::fast_a::StochasticCore,
    last: [f64; 2],
}
impl Stochastic {
    /// Build with the lookback `length` and the `%K` and `%D` smoothing lengths, all in bars (each
    /// registry range `1..=1000`). A value the registry would not accept (out of range, or `NaN`)
    /// is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps instead. `Default` gives
    /// the registry defaults (14, 3, 3).
    #[inline]
    pub fn new(length: f64, smooth_k: f64, smooth_d: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[length, smooth_k, smooth_d];
        crate::typed::checked("stochastic", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar and get `[%K, %D]`; this IS `Indicator::on_bar`, bit for bit, without the
    /// Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.step(bar)
    }
    /// Feed the next high, low and close as plain numbers, no [`Bar`] needed; `[%K, %D]`, the same
    /// answer as [`Stochastic::next`] for a bar with those.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.last = self.core.push(high, low, close);
        self.last
    }
    /// The registry defaults. Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self {
            n: 14,
            sk: 3,
            sd: 3,
            core: crate::fast_a::StochasticCore::new(14, 3, 3),
            last: [f64::NAN; 2],
        }
    }
    /// Parametric constructor — `p[0]` = %K period, `p[1]` = %K smoothing,
    /// `p[2]` = %D smoothing.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n =
            p.first().copied().unwrap_or(14.0).round().min(crate::roll::RING_CAP as f64) as usize;
        let sk =
            p.get(1).copied().unwrap_or(3.0).round().min(crate::roll::RING_CAP as f64) as usize;
        let sd =
            p.get(2).copied().unwrap_or(3.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self { n, sk, sd, core: crate::fast_a::StochasticCore::new(n, sk, sd), last: [f64::NAN; 2] }
    }
}
impl Default for Stochastic {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Stochastic {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.last = self.core.push(bar.high, bar.low, bar.close);
        self.last
    }
}
impl Indicator for Stochastic {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 2>(self, bars, Self::step)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 2];
    }
    fn name(&self) -> &str {
        "stochastic"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1) + self.sk.saturating_sub(1)
    }
    fn lookback_full(&self) -> usize {
        // %D is an `sd`-bar SMA of %K, so it trails %K by `sd - 1`.
        self.n.saturating_sub(1) + self.sk.saturating_sub(1) + self.sd.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Commodity channel index over the last `length` typical prices (default 20; registry key `cci`,
/// one line).
//
// `cci` — the mean is a shifted running sum (O(1)); the mean absolute deviation is one scan of the
// `n`-value window a bar (`crate::fast_a::CciCore`).
#[derive(Clone)]
pub struct Cci {
    n: usize,
    core: crate::fast_a::CciCore,
    last: f64,
}
impl Cci {
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[length];
        crate::typed::checked("cci", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next high, low and close as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Cci::next`] for a bar with those.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(high, low, close);
        self.last
    }
    /// The registry defaults. Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self { n: 20, core: crate::fast_a::CciCore::new(20), last: f64::NAN }
    }
    /// Parametric constructor — `p[0]` = period.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n =
            p.first().copied().unwrap_or(20.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self { n, core: crate::fast_a::CciCore::new(n), last: f64::NAN }
    }
}
impl Default for Cci {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Cci {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.last = self.core.push(bar.high, bar.low, bar.close);
        self.last
    }
}
impl Indicator for Cci {
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
        "cci"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Williams %R over the last `length` bars (default 14; registry key `williams`, one line,
/// -100..=0).
//
// `williams` — O(1) a bar: Williams %R from block-wise window extrema (`crate::fast_a::WilliamsCore`).
#[derive(Clone)]
pub struct Williams {
    n: usize,
    core: crate::fast_a::WilliamsCore,
    last: f64,
}
impl Williams {
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[length];
        crate::typed::checked("williams", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next high, low and close as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Williams::next`] for a bar with those.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(high, low, close);
        self.last
    }
    /// The registry defaults. Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self { n: 14, core: crate::fast_a::WilliamsCore::new(14), last: f64::NAN }
    }
    /// Parametric constructor — `p[0]` = period.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n =
            p.first().copied().unwrap_or(14.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self { n, core: crate::fast_a::WilliamsCore::new(n), last: f64::NAN }
    }
}
impl Default for Williams {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Williams {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.last = self.core.push(bar.high, bar.low, bar.close);
        self.last
    }
}
impl Indicator for Williams {
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
        "williams"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Awesome oscillator: a fast minus a slow average of the median price (registry key `awesome`, one
/// line). Parameters: the `fast_length` and `slow_length` of the two simple averages of the median
/// price (defaults 5 and 34). Each bar costs O(1), whatever the lengths.
//
// `awesome` — SMA(fast) − SMA(slow) of the median price from ONE ring and one shared shift, so the
// difference keeps its digits at any price level (`crate::fast_a::AwesomeCore`).
#[derive(Clone)]
pub struct Awesome {
    fast_n: usize,
    slow_n: usize,
    core: crate::fast_a::AwesomeCore,
    last: f64,
}
impl Awesome {
    /// Build with the `fast_length` and `slow_length` of the two median-price averages, in bars
    /// (each registry range `1..=1000`). A value the registry would not accept (out of range, or
    /// `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps instead. `Default`
    /// gives the registry defaults (5, 34).
    #[inline]
    pub fn new(fast_length: f64, slow_length: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[fast_length, slow_length];
        crate::typed::checked("awesome", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next high and low as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Awesome::next`] for a bar with those.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> f64 {
        self.last = self.core.push(high, low);
        self.last
    }
    /// The registry defaults. Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self { fast_n: 5, slow_n: 34, core: crate::fast_a::AwesomeCore::new(5, 34), last: f64::NAN }
    }
    /// Parametric constructor — `p[0]` = fast period, `p[1]` = slow period.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let fast_n =
            p.first().copied().unwrap_or(5.0).round().min(crate::roll::RING_CAP as f64) as usize;
        let slow_n =
            p.get(1).copied().unwrap_or(34.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self {
            fast_n,
            slow_n,
            core: crate::fast_a::AwesomeCore::new(fast_n, slow_n),
            last: f64::NAN,
        }
    }
}
impl Default for Awesome {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Awesome {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.last = self.core.push(bar.high, bar.low);
        self.last
    }
}
impl Indicator for Awesome {
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
        "awesome"
    }
    fn lookback(&self) -> usize {
        self.fast_n.max(self.slow_n).saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

impl Bollinger {
    /// Feed the next close as a plain number, no [`Bar`] needed; `[upper, mid, lower]`, the same
    /// answer as [`Bollinger::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> [f64; 3] {
        self.last = self.core.push(close);
        self.last
    }
}

#[cfg(test)]
mod typed_tests {
    use super::*;
    use crate::typed::tests::assert_matches_registry;

    #[test]
    fn typed_wma_matches_registry() {
        assert_matches_registry("wma", Box::new(Wma::new(20.0).unwrap()));
    }

    #[test]
    fn typed_bollinger_matches_registry() {
        assert_matches_registry("bollinger", Box::new(Bollinger::new(20.0, 2.0).unwrap()));
    }

    #[test]
    fn typed_donchian_matches_registry() {
        assert_matches_registry("donchian", Box::new(Donchian::new(20.0).unwrap()));
    }

    #[test]
    fn typed_keltner_matches_registry() {
        assert_matches_registry("keltner", Box::new(Keltner::new(20.0, 10.0, 2.0).unwrap()));
    }

    #[test]
    fn typed_vwap_matches_registry() {
        assert_matches_registry("vwap", Box::new(Vwap::default()));
    }

    #[test]
    fn typed_stochastic_matches_registry() {
        assert_matches_registry("stochastic", Box::new(Stochastic::new(14.0, 3.0, 3.0).unwrap()));
    }

    #[test]
    fn typed_cci_matches_registry() {
        assert_matches_registry("cci", Box::new(Cci::new(20.0).unwrap()));
    }

    #[test]
    fn typed_williams_matches_registry() {
        assert_matches_registry("williams", Box::new(Williams::new(14.0).unwrap()));
    }

    #[test]
    fn typed_awesome_matches_registry() {
        assert_matches_registry("awesome", Box::new(Awesome::new(5.0, 34.0).unwrap()));
    }
}

//! The true-incremental family: the eight indicators under that banner and their batch kernels.

use crate::Indicator;
use vike_marketdata::Bar;

// ========================= true-incremental family =========================

/// Simple moving average of the close over the last `length` bars (default 20; registry key `sma`,
/// one line). `NaN` until `length` bars have been seen.
///
/// A running sum updated with the one close that enters and the one that leaves, rebuilt from the
/// window every 512 bars so rounding error cannot accumulate (`crate::roll::SmaCore`): O(1) a
/// bar whatever `length`. The last bits can differ from a fresh left fold of the window, by the
/// rounding noise every running-sum SMA has.
#[derive(Clone)]
pub struct Sma {
    n: usize,
    window: crate::roll::SmaCore,
    last: f64,
}
impl Sma {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self { n: 20, window: crate::roll::SmaCore::new(20), last: f64::NAN }
    }
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[length];
        crate::typed::checked("sma", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Sma::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.window.push(close);
        self.last
    }
    /// Parametric constructor for [`crate::registry::IndicatorMeta::make_with`] —
    /// `p[0]` = period (assumed pre-coerced; a missing entry falls back to the
    /// default). No sub-state depends on `n` at construction, so the rest of the
    /// fields reuse `defaults()`'s empty state via struct-update.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n =
            p.first().copied().unwrap_or(20.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self { n, window: crate::roll::SmaCore::new(n), last: f64::NAN }
    }
}
impl Default for Sma {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Sma {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        // `n == 0` is all-NaN, like `math::sma`.
        self.last = self.window.push(bar.close);
        self.last
    }
}
impl Indicator for Sma {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut out = Vec::new();
        crate::roll::sma_run(bars.len(), self.n, |i| bars[i].close, &mut out);
        vec![out]
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        self.window.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "sma"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Exponential moving average of the close over `length` bars (default 20; registry key `ema`, one
/// line), with smoothing factor `2 / (length + 1)`.
//
// The `EmaState` recurrence, identical to `math::ema`.
#[derive(Clone)]
pub struct Ema {
    n: usize,
    core: crate::roll::EmaCore,
    last: f64,
}
impl Ema {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        let n = 20;
        Self { n, core: crate::roll::EmaCore::new(n), last: f64::NAN }
    }
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[length];
        crate::typed::checked("ema", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Parametric constructor — `p[0]` = period, threaded into BOTH the `n`
    /// field and the [`EmaState`] recurrence (the sub-state must be built from
    /// the same coerced period, else `make_with` at non-default params would
    /// silently seed the alpha recurrence with the wrong `n`).
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n =
            p.first().copied().unwrap_or(20.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self { n, core: crate::roll::EmaCore::new(n), last: f64::NAN }
    }
}
impl Default for Ema {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Ema {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.last = self.core.push(bar.close);
        self.last
    }
}
impl Indicator for Ema {
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
        "ema"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Relative strength index of the close over `length` bars (default 14; registry key `rsi`, one
/// line, 0..=100), using Wilder's smoothing of average gains and losses.
//
// Wilder averages via the same seed-then-recurrence fold as `math`'s batch RSI.
#[derive(Clone)]
pub struct Rsi {
    n: usize,
    core: crate::roll::RsiCore,
    last: f64,
}
impl Rsi {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    fn defaults() -> Self {
        Self { n: 14, core: crate::roll::RsiCore::new(14), last: f64::NAN }
    }
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    pub fn new(length: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[length];
        crate::typed::checked("rsi", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Parametric constructor — `p[0]` = period; no sub-state depends on it
    /// (the Wilder averages are plain fields, seeded lazily in `on_bar`).
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n =
            p.first().copied().unwrap_or(14.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self { n, core: crate::roll::RsiCore::new(n), last: f64::NAN }
    }
}
impl Default for Rsi {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Rsi {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.last = self.core.push(bar.close);
        self.last
    }
}
impl Indicator for Rsi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut out = Vec::new();
        crate::roll::rsi_run(bars.len(), self.n, |i| bars[i].close, &mut out);
        vec![out]
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "rsi"
    }
    fn lookback(&self) -> usize {
        self.n
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Average true range over `length` bars (default 14; registry key `atr`, one line): the true
/// range smoothed with Wilder's moving average.
//
// The true range fed through a Wilder (RMA) recurrence, identical to `math::atr`.
#[derive(Clone)]
pub struct Atr {
    n: usize,
    core: crate::roll::AtrCore,
    last: f64,
}
impl Atr {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        let n = 14;
        Self { n, core: crate::roll::AtrCore::new(n), last: f64::NAN }
    }
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[length];
        crate::typed::checked("atr", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Parametric constructor — `p[0]` = period.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n =
            p.first().copied().unwrap_or(14.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self { n, core: crate::roll::AtrCore::new(n), last: f64::NAN }
    }
}
impl Default for Atr {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Atr {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.last = self.core.push(bar.high, bar.low, bar.close);
        self.last
    }
}
impl Indicator for Atr {
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
        "atr"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Rate of change of the close over `length` bars, in percent (default 10; registry key `roc`, one
/// line): `100*(c[i]-c[i-n])/c[i-n]`.
//
// (Computed from an `n`-deep close ring: `crate::fast_c::RocCore`.)
#[derive(Clone)]
pub struct Roc {
    n: usize,
    core: crate::fast_c::RocCore,
    last: f64,
}
impl Roc {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self { n: 10, core: crate::fast_c::RocCore::new(10), last: f64::NAN }
    }
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (10).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[length];
        crate::typed::checked("roc", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Roc::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
    /// Parametric constructor — `p[0]` = period, threaded into the close ring.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let n =
            p.first().copied().unwrap_or(10.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self { n, core: crate::fast_c::RocCore::new(n), last: f64::NAN }
    }
}
impl Default for Roc {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Roc {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.last = self.core.push(bar.close);
        self.last
    }
}
impl Indicator for Roc {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::fast_c::roc_bars(self.n, bars)
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "roc"
    }
    fn lookback(&self) -> usize {
        self.n
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// On-balance volume (registry key `obv`, one line, no parameters): a running sum of volume,
/// added on an up-close and subtracted on a down-close. Cumulative, so it depends on where the
/// series started ([`crate::Indicator::warmup_path_dependent`]).
//
// Identical to `math`'s batch OBV.
#[derive(Clone)]
pub struct Obv {
    core: crate::fast_c::ObvCore,
    last: f64,
}
impl Obv {
    /// The registry defaults (it has no parameters). Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self { core: crate::fast_c::ObvCore::new(), last: f64::NAN }
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next bar's `close` and `volume` as plain numbers, no [`Bar`] needed; the same answer
    /// as [`Obv::next`] for a bar with those values.
    #[inline]
    pub fn next_cv(&mut self, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(close, volume);
        self.last
    }
}
impl Default for Obv {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Obv {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.last = self.core.push(bar.close, bar.volume);
        self.last
    }
}
impl Indicator for Obv {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::fast_c::obv_bars(bars)
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "obv"
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

/// Moving average convergence/divergence of the close (registry key `macd`, three lines
/// `[macd, signal, hist]`). Parameters: fast, slow and signal periods (defaults 12, 26, 9). `macd`
/// is the fast EMA minus the slow EMA, `signal` is an EMA of `macd`, and `hist` is `macd - signal`.
/// The `signal` and `hist` lines warm up later than `macd`.
//
// Three EMA recurrences (fast/slow diff, then signal EMA of the diff), reproducing `math`'s batch
// MACD fold exactly.
#[derive(Clone)]
pub struct Macd {
    fast_n: usize,
    slow_n: usize,
    sig_n: usize,
    core: crate::roll::MacdCore,
    last: [f64; 3],
}
impl Macd {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        let fast_n = 12;
        let slow_n = 26;
        let sig_n = 9;
        Self {
            fast_n,
            slow_n,
            sig_n,
            core: crate::roll::MacdCore::new(fast_n, slow_n, sig_n),
            last: [f64::NAN; 3],
        }
    }
    /// Build with these periods, in bars (each registry range `1..=1000`). A value the registry
    /// would not accept (out of range, or `NaN`) is a [`crate::ParamError`] naming it;
    /// [`crate::make_with`] clamps instead. `Default` gives the registry defaults (12, 26, 9).
    #[inline]
    pub fn new(
        fast_length: f64,
        slow_length: f64,
        signal_length: f64,
    ) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[fast_length, slow_length, signal_length];
        crate::typed::checked("macd", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar and get `[macd, signal, hist]`; this IS `Indicator::on_bar`, bit for bit,
    /// without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 3] {
        self.step(bar)
    }
    /// Parametric constructor — `p[0]` = fast period, `p[1]` = slow period,
    /// `p[2]` = signal period, each threaded into its own [`EmaState`].
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let fast_n =
            p.first().copied().unwrap_or(12.0).round().min(crate::roll::RING_CAP as f64) as usize;
        let slow_n =
            p.get(1).copied().unwrap_or(26.0).round().min(crate::roll::RING_CAP as f64) as usize;
        let sig_n =
            p.get(2).copied().unwrap_or(9.0).round().min(crate::roll::RING_CAP as f64) as usize;
        Self {
            fast_n,
            slow_n,
            sig_n,
            core: crate::roll::MacdCore::new(fast_n, slow_n, sig_n),
            last: [f64::NAN; 3],
        }
    }
}
impl Default for Macd {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Macd {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 3] {
        self.last = self.core.push(bar.close);
        self.last
    }
}
impl Indicator for Macd {
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
        "macd"
    }
    fn lookback(&self) -> usize {
        self.fast_n.max(self.slow_n).saturating_sub(1)
    }
    fn lookback_full(&self) -> usize {
        // The signal EMA (and hist) trail the macd line by `signal_n - 1`.
        self.fast_n.max(self.slow_n).saturating_sub(1) + self.sig_n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Parabolic SAR, Wilder's stop-and-reverse (registry key `psar`, one line of dots). Parameters:
/// acceleration `step` (default 0.02) and the cap `max_af` on the acceleration factor (default
/// 0.20). The value is path-dependent: it assumes an initial trend from the first bars
/// ([`crate::Indicator::warmup_path_dependent`]).
//
// The forward recurrence streamed directly (`crate::fast_c::PsarCore`; a buffer-recompute
// can't reproduce it: the first bar's value is its low). `vectorize` folds the same core.
#[derive(Clone)]
pub struct Psar {
    core: crate::fast_c::PsarCore,
    last: f64,
}
impl Psar {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self { core: crate::fast_c::PsarCore::new(0.02, 0.20), last: f64::NAN }
    }
    /// Build with the acceleration `step` (registry range `0.001..=0.5`) and the cap `max_af` on
    /// the acceleration factor (`0.05..=1.0`). A value the registry would not accept (out of range,
    /// or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps instead.
    /// `Default` gives the registry defaults (0.02, 0.20).
    #[inline]
    pub fn new(step: f64, max_af: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[step, max_af];
        crate::typed::checked("psar", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next bar's `high` and `low` as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Psar::next`] for a bar with those prices.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> f64 {
        self.last = self.core.push(high, low);
        self.last
    }
    /// Parametric constructor — `p[0]` = acceleration step, `p[1]` = max
    /// acceleration factor. Both are f64 multipliers (not periods): read
    /// directly, no rounding.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let step = p.first().copied().unwrap_or(0.02);
        let max_af = p.get(1).copied().unwrap_or(0.20);
        Self { core: crate::fast_c::PsarCore::new(step, max_af), last: f64::NAN }
    }
}
impl Default for Psar {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Psar {
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.last = self.core.push(bar.high, bar.low);
        self.last
    }
}
impl Indicator for Psar {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::fast_c::psar_bars(&self.core, bars)
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        // State only: the core keeps its parameters.
        self.core.clear();
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "psar"
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

// ---- plain-number entry points: the same recurrence as `next`, without a `Bar` ----------------

impl Ema {
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Ema::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}

impl Rsi {
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Rsi::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}

impl Macd {
    /// Feed the next close as a plain number, no [`Bar`] needed; `[macd, signal, histogram]`, the
    /// same answer as [`Macd::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> [f64; 3] {
        self.last = self.core.push(close);
        self.last
    }
}

impl Atr {
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Atr::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(high, low, close);
        self.last
    }
}

#[cfg(test)]
mod typed_tests {
    use super::*;
    use crate::typed::tests::assert_matches_registry;

    #[test]
    fn typed_sma_matches_registry() {
        assert_matches_registry("sma", Box::new(Sma::new(20.0).unwrap()));
    }

    #[test]
    fn typed_ema_matches_registry() {
        assert_matches_registry("ema", Box::new(Ema::new(20.0).unwrap()));
    }

    #[test]
    fn typed_rsi_matches_registry() {
        assert_matches_registry("rsi", Box::new(Rsi::new(14.0).unwrap()));
    }

    #[test]
    fn typed_atr_matches_registry() {
        assert_matches_registry("atr", Box::new(Atr::new(14.0).unwrap()));
    }

    #[test]
    fn typed_roc_matches_registry() {
        assert_matches_registry("roc", Box::new(Roc::new(10.0).unwrap()));
    }

    #[test]
    fn typed_obv_matches_registry() {
        assert_matches_registry("obv", Box::new(Obv::default()));
    }

    #[test]
    fn typed_macd_matches_registry() {
        assert_matches_registry("macd", Box::new(Macd::new(12.0, 26.0, 9.0).unwrap()));
    }

    #[test]
    fn typed_psar_matches_registry() {
        assert_matches_registry("psar", Box::new(Psar::new(0.02, 0.20).unwrap()));
    }
}

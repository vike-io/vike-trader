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
/// rounding noise every running-sum SMA has; [`Sma::with_fresh_sum`] removes it.
#[derive(Clone)]
pub struct Sma {
    n: usize,
    window: crate::roll::SmaCore,
    fresh_sum: bool,
    last: f64,
}
impl Sma {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::with_params(&[])
    }
    /// Option `fresh_sum`, `false` by default (and unset, every value is the one the registry row
    /// gives): each value is a fresh left fold of the window, oldest close first, divided by
    /// `length`, so its last bits are the window's own and two averages of the same closes compare
    /// equal. Unset, the running sum gives the same number up to rounding noise in the last bits.
    /// Costs `length` adds a bar instead of one.
    #[inline]
    pub fn with_fresh_sum(mut self, fresh_sum: bool) -> Self {
        self.fresh_sum = fresh_sum;
        self
    }
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::IndicatorParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::IndicatorParamError> {
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
        // `n == 0` is all-NaN, like `math::sma`.
        let mean = self.window.push(close);
        self.last = if self.fresh_sum { self.window.fresh_mean() } else { mean };
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
        Self { n, window: crate::roll::SmaCore::new(n), fresh_sum: false, last: f64::NAN }
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
        self.next_value(bar.close)
    }
}
impl Indicator for Sma {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        if self.fresh_sum {
            return crate::roll::batch_scalar(self, bars, Self::step);
        }
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
/// line), with smoothing factor `2 / (length + 1)`. Options, all off by default:
/// [`Ema::with_skip_nan`], [`Ema::with_seed_first`] and [`Ema::with_zero_seed`].
//
// The `EmaState` recurrence, identical to `math::ema`.
#[derive(Clone)]
pub struct Ema {
    n: usize,
    core: crate::roll::EmaCore,
    skip_nan: bool,
    seed_first: bool,
    zero_seed: bool,
    last: f64,
}
impl Ema {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::with_params(&[])
    }
    /// Option `skip_nan`, `false` by default: a `NaN` input is dropped, so the average neither seeds
    /// nor moves on it and that bar's value is `NaN`; the next number carries on from where it was.
    /// Unset (the registry row's behaviour), a `NaN` enters the seed or the recurrence and the line
    /// stays `NaN` for good.
    #[inline]
    pub fn with_skip_nan(mut self, skip_nan: bool) -> Self {
        self.skip_nan = skip_nan;
        self
    }
    /// Option `seed_first`, `false` by default: the first value seeds the average and is its first
    /// output, so there is no warm-up; the recurrence `alpha * x + (1 - alpha) * previous` runs from the
    /// second value on. Unset (the registry row's behaviour), the first `length - 1` values are
    /// `NaN` and the seed is the mean of the first `length`.
    #[inline]
    pub fn with_seed_first(mut self, seed_first: bool) -> Self {
        self.seed_first = seed_first;
        self
    }
    /// Option `zero_seed`, `false` by default: the average starts from 0, so the first value is
    /// `alpha * x` and the recurrence `alpha * x + (1 - alpha) * previous` runs from the first value on,
    /// with no warm-up (the platforms' smoothing of a variable initialised to 0). It gives way to
    /// [`Ema::with_seed_first`]. Unset (the registry row's behaviour), the first `length - 1`
    /// values are `NaN` and the seed is the mean of the first `length`.
    #[inline]
    pub fn with_zero_seed(mut self, zero_seed: bool) -> Self {
        self.zero_seed = zero_seed;
        self
    }
    /// One value through the options, then the recurrence.
    #[inline(always)]
    fn push(&mut self, x: f64) -> f64 {
        self.last = if !(self.skip_nan | self.seed_first | self.zero_seed) {
            self.core.push(x)
        } else if self.skip_nan && x.is_nan() {
            f64::NAN
        } else if self.seed_first {
            self.core.push_from_first(x)
        } else if self.zero_seed {
            self.core.push_from_zero(x)
        } else {
            self.core.push(x)
        };
        self.last
    }
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::IndicatorParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::IndicatorParamError> {
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
        Self {
            n,
            core: crate::roll::EmaCore::new(n),
            skip_nan: false,
            seed_first: false,
            zero_seed: false,
            last: f64::NAN,
        }
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
        self.push(bar.close)
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
        if self.seed_first | self.zero_seed { 0 } else { self.n.saturating_sub(1) }
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Relative strength index of the close over `length` bars (default 14; registry key `rsi`, one
/// line, 0..=100), using Wilder's smoothing of average gains and losses. Options, off by default:
/// the RS form's arithmetic ([`Rsi::with_rs_form`]), saturated means ([`Rsi::with_saturation`],
/// [`Rsi::with_zero_saturation`]) and means from 0 ([`Rsi::with_zero_seed`]).
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
    /// (out of range, or `NaN`) is a [`crate::IndicatorParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    pub fn new(length: f64) -> Result<Self, crate::IndicatorParamError> {
        let raw: &[f64] = &[length];
        crate::typed::checked("rsi", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Option `rs_form`, `false` by default: the platforms' arithmetic, `100 - 100 / (1 + up / down)`
    /// (`NaN` where both means are 0), each mean seeded with the sum of the first `length` moves
    /// divided by `length` and stepped as `alpha * move + (1 - alpha) * mean`, `alpha = 1 / length`.
    /// Equal on paper to the unset `100 * up / (up + down)` of means stepped as
    /// `mean * (length - 1) / length + move / length`, a few ulps apart in f64, which flips a
    /// comparison of two nearly equal values. A `NaN` close is skipped: it reads `NaN`, moves nothing,
    /// and the next move is measured from the last number (unset, it counts as a move of 0).
    #[inline]
    pub fn with_rs_form(mut self, rs_form: bool) -> Self {
        self.core.set_rs_form(rs_form);
        self
    }
    /// Option `saturation`, `false` by default: a mean move of `1e-10` or less reads as none, the
    /// losses' first. Mean losses at or under it read 100 (so a close held flat until both means
    /// decay reads 100, not `NaN` or 99.9999...), else mean gains at or under it read 0. Unset, only
    /// mean losses of exactly 0 read 100 (in the RS form, `NaN` when the gains are 0 too). Either form.
    #[inline]
    pub fn with_saturation(mut self, saturation: bool) -> Self {
        self.core.set_saturation(saturation);
        self
    }
    /// Option `zero_saturation`, `false` by default: [`Rsi::with_saturation`]'s rule at 0, mean
    /// losses of 0 read 100, else mean gains of 0 read 0. Apart from unset only where both means are
    /// 0: 100 there, where unset the RS form reads `NaN` (and the default form 100 already). Either
    /// form; under [`Rsi::with_saturation`] it reads nothing.
    #[inline]
    pub fn with_zero_saturation(mut self, zero_saturation: bool) -> Self {
        self.core.set_zero_saturation(zero_saturation);
        self
    }
    /// Option `zero_seed`, `false` by default: both means start from 0, as if the first close had
    /// moved nothing, so there is no seed: every move from the first one on is a step of the form's
    /// own (`mean * (length - 1) / length + move / length`, or the RS form's alpha step), and the
    /// RSI is there from the second close (bar 1). Unset, the means are seeded with the mean of the
    /// first `length` moves (bar `length`). Either form.
    #[inline]
    pub fn with_zero_seed(mut self, zero_seed: bool) -> Self {
        self.core.zero_seed = zero_seed;
        self
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
        if !self.core.is_plain() {
            return crate::roll::batch_scalar(self, bars, Self::step);
        }
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
        if self.core.zero_seed { self.n.min(1) } else { self.n }
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Average true range over `length` bars (default 14; registry key `atr`, one line): the true
/// range smoothed with Wilder's moving average. Option, off by default: seeded with the first true
/// range ([`Atr::with_seed_first`]).
//
// The true range fed through a Wilder (RMA) recurrence, identical to `math::atr`.
#[derive(Clone)]
pub struct Atr {
    n: usize,
    core: crate::roll::AtrCore,
    last: f64,
    /// `seed_first` only: the law of [`Atr::with_seed_first`], and the last close it read.
    seed_first: bool,
    pc: f64,
}
impl Atr {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::with_params(&[14.0])
    }
    /// Option `seed_first`, `false` by default: the average starts at the first true range and
    /// moves `atr + (tr - atr) / length` from there. The first bar has no previous close, so no
    /// true range and no value (`NaN`); the second bar's range seeds it, and a bar whose step is
    /// `NaN` takes its own range. Unset (the registry row's behaviour), the first `length` ranges
    /// (the first bar's `high - low`) are averaged and Wilder's step runs from there as
    /// `atr * (length - 1) / length + tr / length`.
    #[inline]
    pub fn with_seed_first(mut self, seed_first: bool) -> Self {
        self.seed_first = seed_first;
        self
    }
    /// Build with `length` bars (registry range `1..=1000`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::IndicatorParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::IndicatorParamError> {
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
        Self {
            n,
            core: crate::roll::AtrCore::new(n),
            last: f64::NAN,
            seed_first: false,
            pc: f64::NAN,
        }
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
        self.next_hlc(bar.high, bar.low, bar.close)
    }
    /// One bar of [`Atr::with_seed_first`]'s law.
    #[inline]
    fn seeded(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let pc = std::mem::replace(&mut self.pc, close);
        let tr = if pc.is_nan() {
            f64::NAN
        } else {
            (high - low).max((high - pc).abs()).max((low - pc).abs())
        };
        let next = self.last + (tr - self.last) / self.n as f64;
        if next.is_nan() { tr } else { next }
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
        self.pc = f64::NAN;
    }
    fn name(&self) -> &str {
        "atr"
    }
    fn lookback(&self) -> usize {
        if self.seed_first { 1 } else { self.n.saturating_sub(1) }
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
    /// (out of range, or `NaN`) is a [`crate::IndicatorParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (10).
    #[inline]
    pub fn new(length: f64) -> Result<Self, crate::IndicatorParamError> {
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
    /// would not accept (out of range, or `NaN`) is a [`crate::IndicatorParamError`] naming it;
    /// [`crate::make_with`] clamps instead. `Default` gives the registry defaults (12, 26, 9).
    #[inline]
    pub fn new(
        fast_length: f64,
        slow_length: f64,
        signal_length: f64,
    ) -> Result<Self, crate::IndicatorParamError> {
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
/// ([`crate::Indicator::warmup_path_dependent`]). Options, all off by default: a separate first
/// factor ([`Psar::with_start`]), the reversal tested before the clamp
/// ([`Psar::with_test_first`]), the starting direction from the first two closes
/// ([`Psar::with_direction_from_closes`]), the textbook steps ([`Psar::with_textbook_steps`]), and
/// the law that reverses on the close ([`Psar::with_reversal_on_close`]), optionally stepping by an
/// ATR ([`Psar::with_atr_step`]), and the law that takes the bar's extreme before the stop moves
/// ([`Psar::with_extreme_first`]). [`Psar::uptrend`] and [`Psar::stop_ahead`] read the state a
/// bar leaves.
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
    /// the acceleration factor (`0.001..=1.0`). A value the registry would not accept (out of range,
    /// or `NaN`) is a [`crate::IndicatorParamError`] naming it; [`crate::make_with`] clamps instead.
    /// `Default` gives the registry defaults (0.02, 0.20).
    #[inline]
    pub fn new(step: f64, max_af: f64) -> Result<Self, crate::IndicatorParamError> {
        let raw: &[f64] = &[step, max_af];
        crate::typed::checked("psar", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Option `start`, `step` by default: the acceleration factor of a trend's first extreme, apart
    /// from the `step` each later extreme adds (the `k`-th carries `min(start + step * (k - 1),
    /// max_af)`). Refused outside `step`'s registry range `0.001..=0.5`, or `NaN`, with a
    /// [`crate::IndicatorParamError`] naming `start`. Unset, every value is the registry row's.
    #[inline]
    pub fn with_start(mut self, start: f64) -> Result<Self, crate::IndicatorParamError> {
        crate::typed::checked_like("psar", 0, "start", start)?;
        self.core.set_start(start);
        Ok(self)
    }
    /// Option `test_first`, `false` by default: a bar reverses the trend when it crosses the stop as
    /// the stop moved this bar, BEFORE the stop is clamped to the last two bars' lows (highs); a
    /// reversal's stop is then the old extreme, raised (lowered) to this bar's and the last two bars'
    /// highs (lows). Unset (the registry row's behaviour), the clamp comes first, so a bar that
    /// touches only the unclamped stop does not reverse it.
    #[inline]
    pub fn with_test_first(mut self, test_first: bool) -> Self {
        let (_, from_closes, textbook) = self.core.options();
        self.core.set_options(test_first, from_closes, textbook);
        self
    }
    /// Option `direction_from_closes`, `false` by default: the first bar has no value (`NaN`) and the
    /// trend starts on the second, up when its close is above the first's, the stop at the first
    /// bar's low (high) moving toward the second bar's high (low). It reads the close, so feed it
    /// through [`Psar::next`] or [`Psar::next_hlc`] ([`Psar::next_hl`] has no close, which starts the
    /// trend down). Unset (the registry row's behaviour), the first bar starts an up-trend at its low.
    #[inline]
    pub fn with_direction_from_closes(mut self, direction_from_closes: bool) -> Self {
        let (test_first, _, textbook) = self.core.options();
        self.core.set_options(test_first, direction_from_closes, textbook);
        self
    }
    /// Option `textbook_steps`, `false` by default: the recurrence in its textbook steps, the stop
    /// moving as `sar + af * (ep - sar)` and the factor carried, `start` at a trend's opening and
    /// `min(af + step, max_af)` at each new extreme. Unset (the registry row's behaviour), the stop
    /// is `sar * (1 - af) + af * ep` and the `k`-th extreme's factor `min(start + step * (k - 1),
    /// max_af)`: the same numbers up to rounding in the last bits.
    #[inline]
    pub fn with_textbook_steps(mut self, textbook_steps: bool) -> Self {
        let (test_first, from_closes, _) = self.core.options();
        self.core.set_options(test_first, from_closes, textbook_steps);
        self
    }
    /// Option `reversal_on_close`, `false` by default: the stop-and-reverse judged on the CLOSE, each
    /// value being the level the NEXT bar's close is judged against. The first bar has no value
    /// (`NaN`) and fixes the direction, up when it closed above its open; the second starts the
    /// trend there, its value the first bar's low (high) and its extreme its own high (low). From
    /// the third bar, a close at or under (at or over) the last value reverses the trend: the value
    /// is then the old extreme, the extreme this bar's high (low) and the factor `start`. Otherwise
    /// the extreme and the factor take this bar first (`min(af + step, max_af)` at a new extreme)
    /// and the value moves `af` of the way to the extreme, `sar + af * (ep - sar)`, with no clamp to
    /// the last bars. [`Psar::with_start`] applies; the three options above do not. It reads the
    /// open and the close, so feed it through [`Psar::next`] (a [`Bar`]). Unset (the registry row's
    /// behaviour), the reversal is judged on the high and the low.
    #[inline]
    pub fn with_reversal_on_close(mut self, reversal_on_close: bool) -> Self {
        self.core.set_on_close(reversal_on_close);
        self
    }
    /// Option `extreme_first`, `false` by default: each bar takes its own extreme and factor FIRST
    /// (`max(high, ep)` up, `min(low, ep)` down; the factor `start` on the bar after a reversal,
    /// else raised by `step` to at most `max_af` whenever the extreme moved), then moves the stop
    /// `sar + af * (ep - sar)` toward that extreme and clamps it to the last two bars' lows (highs);
    /// a low under (high over) the clamped stop reverses the trend, the stop then the extreme just
    /// taken and the new extreme the lower low (higher high) of this bar and the last. The first
    /// bar starts an up-trend at its low. [`Psar::with_start`] applies; the three options above do
    /// not, and [`Psar::stop_ahead`] has no answer (`NaN`). Unset (the registry row's behaviour),
    /// the stop moves on the last bar's extreme and factor.
    #[inline]
    pub fn with_extreme_first(mut self, extreme_first: bool) -> Self {
        self.core.set_extreme_first(extreme_first);
        self
    }
    /// Option `atr_step`, unset by default and read under [`Psar::with_reversal_on_close`] only: the
    /// value moves `af` times Wilder's ATR over `atr_step` bars toward the price instead of `af` of
    /// the way to the extreme (`sar + af * atr` in an up-trend, `sar - af * atr` in a down-trend),
    /// the bar's true range standing in until the ATR has a value. Refused outside `atr`'s
    /// registry range `1..=1000`, or `NaN`, with a [`crate::IndicatorParamError`] naming `atr_step`.
    #[inline]
    pub fn with_atr_step(mut self, atr_step: f64) -> Result<Self, crate::IndicatorParamError> {
        let like = crate::registry::get("atr")
            .map(|m| crate::registry::ParamSpec { name: "atr_step", ..m.params[0] });
        crate::typed::within("psar", like.as_slice(), &[atr_step])?;
        self.core.set_atr(atr_step.round() as usize);
        Ok(self)
    }
    /// The trend the last bar left: `Some(true)` up (the stop under the price), `Some(false)` down,
    /// `None` before one has started (no bar yet, or only the first under
    /// [`Psar::with_direction_from_closes`] or [`Psar::with_reversal_on_close`]).
    #[inline]
    pub fn uptrend(&self) -> Option<bool> {
        self.core.trend()
    }
    /// The stop the next bar meets as the last bar left it: moved toward the extreme, before the next
    /// bar's reversal test and clamp (under [`Psar::with_reversal_on_close`], the value itself, which
    /// the next close is judged against). `NaN` before a trend has started.
    #[inline]
    pub fn stop_ahead(&self) -> f64 {
        self.core.stop_ahead()
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next bar's `high` and `low` as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Psar::next`] for a bar with those prices (but for [`Psar::with_direction_from_closes`],
    /// which reads the close: [`Psar::next_hlc`], and [`Psar::with_reversal_on_close`], which reads
    /// the open as well: [`Psar::next`]).
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> f64 {
        self.last = self.core.push(high, low);
        self.last
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Psar::next`] for a bar with those prices. The close is read by
    /// [`Psar::with_direction_from_closes`] and [`Psar::with_reversal_on_close`] only (the second
    /// reads the open too, which this lacks: a `NaN` open starts its trend down).
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push_hlc(high, low, close);
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
        self.last = self.core.push_ohlc(bar.open, bar.high, bar.low, bar.close);
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
        usize::from(self.core.starts_on_second())
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
        self.push(close)
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
        self.last = if self.seed_first {
            self.seeded(high, low, close)
        } else {
            self.core.push(high, low, close)
        };
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

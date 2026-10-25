//! Bounded-state streamers for ten indicators that are chains of EMAs, SMAs and WMAs: `trix`, `dema`,
//! `ppo`, `apo`, `zlema`, `coppock`, `tsi`, `smi_ergodic`, `kvo` and `mass`. `trix`, `dema`, `ppo` and `apo`
//! run the cores in `crates/vike-indicators/src/fast_b.rs`, and `zlema`, `coppock`, `tsi`,
//! `smi_ergodic` and `mass` the cores in `crates/vike-indicators/src/fast_i.rs` (one `push` shared
//! with `series::*`, so a stream, `vectorize` and a slice call agree bit for bit).
//!
//! As `hist_indicator!` types they re-ran the whole batch kernel over a retained tail of history on
//! every bar (measured per bar at 20k bars: `kvo` 127 us, `trix` 123 us, `mass` 103 us, `tsi` 64 us).
//! Each is a fixed chain of EMA / WMA / window-sum stages, so the exact streaming state is a few
//! averages and small rings: a bar costs O(1) and nothing grows. There is no truncated tail at all,
//! so the value is exact rather than exact-once-the-tail-has-forgotten.
//!
//! `kvo` runs `crate::fast_k::KvoCore`, which keeps the batch kernel's building blocks: a stage fed
//! ONLY the non-`NaN` values of the stage before it (`math::smooth_defined(&src, math::ema, n)`), a
//! position whose input was `NaN` staying `NaN` without touching the stage's state, and the
//! simple-mean seed at the `n`-th input before the alpha recurrence.
//!
//! Every type is hand-written in the `base/incremental.rs` sense (`trims_history() == false`), and
//! `tests/streamers_compound_exact.rs` folds each over a long series and compares every bar and line
//! with `vectorize` by bits.

use super::macros::typed_new;
use super::{pback, pbars};
use crate::Indicator;
use crate::fast_b::{ApoCore, DemaCore, PpoCore, TrixCore};
use crate::fast_i::{CoppockCore, MassCore, SmiCore, TsiCore, ZlemaCore};
use crate::fast_k::KvoCore;
use vike_marketdata::Bar;

/// The impl items every type here shares. `lookback` (and `lookback_full`, where a type has one) stays
/// in each impl: its body reads `self`, and a `self` written outside this macro cannot be seen from
/// inside it. `warmup_path_dependent`, `window_reach` and `trims_history` keep the trait defaults.
macro_rules! shared {
    (@rest $key:literal) => {
        fn name(&self) -> &str {
            $key
        }
        fn lookback_exact(&self) -> bool {
            true
        }
    };
    ($key:literal, scalar) => {
        fn value(&self) -> Vec<f64> {
            vec![self.last]
        }
        shared!(@rest $key);
    };
    ($key:literal, lines) => {
        fn value(&self) -> Vec<f64> {
            self.last.to_vec()
        }
        shared!(@rest $key);
    };
}

// =================================== trix ===================================

/// TRIX (registry key `trix`, one line, parameter `period`): the one-bar percentage rate of change of
/// a triple-smoothed EMA of the close. `NaN` until the third EMA has seeded and has a previous value.
#[derive(Clone)]
pub struct Trix {
    period: f64,
    core: TrixCore,
    last: f64,
}
impl Trix {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(18.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: TrixCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(18.0))
    }
    /// Feed the next bar and get this bar's output; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Trix::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Trix {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Trix, "trix", [period = 18.0]);
impl Trix {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Indicator for Trix {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.period).core;
        let mut out = vec![f64::NAN; bars.len()];
        core.run(bars, |b| b.close, &mut out);
        vec![out]
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn lookback(&self) -> usize {
        3 * pback(self.period, 1) + 1
    }
    shared!("trix", scalar);
}

// =================================== dema ===================================

/// Double exponential moving average of the close (registry key `dema`, one line, parameter
/// `period`): `2*EMA - EMA(EMA)`. `NaN` until both EMAs have seeded.
#[derive(Clone)]
pub struct Dema {
    period: f64,
    core: DemaCore,
    last: f64,
}
impl Dema {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: DemaCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(20.0))
    }
    /// Feed the next bar and get this bar's output; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Dema::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Dema {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Dema, "dema", [period = 20.0]);
impl Dema {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Indicator for Dema {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.period).core;
        let mut out = vec![f64::NAN; bars.len()];
        core.run(bars, |b| b.close, &mut out);
        vec![out]
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn lookback(&self) -> usize {
        (2 * pbars(self.period)).saturating_sub(2)
    }
    shared!("dema", scalar);
}

// =================================== ppo ====================================

/// Percentage price oscillator (registry key `ppo`, one line, parameters `fast` and `slow`):
/// `(EMA(fast) - EMA(slow)) / EMA(slow) * 100` of the close. `NaN` until both EMAs have seeded, and
/// wherever the slow EMA is exactly zero.
#[derive(Clone)]
pub struct Ppo {
    fast: f64,
    slow: f64,
    core: PpoCore,
    last: f64,
}
impl Ppo {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(12.0, 26.0)
    }
    #[inline]
    fn build(fast: f64, slow: f64) -> Self {
        let core = PpoCore::new(
            fast.round().min(crate::roll::RING_CAP as f64) as usize,
            slow.round().min(crate::roll::RING_CAP as f64) as usize,
        );
        Self { fast, slow, core, last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let fast = p.first().copied().unwrap_or(12.0);
        let slow = p.get(1).copied().unwrap_or(26.0);
        Self::build(fast, slow)
    }
    /// Feed the next bar and get this bar's output; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Ppo::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Ppo {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Ppo, "ppo", [fast = 12.0, slow = 26.0]);
impl Ppo {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Indicator for Ppo {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.fast, self.slow).core;
        let mut out = vec![f64::NAN; bars.len()];
        core.run(bars, |b| b.close, &mut out);
        vec![out]
    }
    fn reset(&mut self) {
        *self = Self::build(self.fast, self.slow);
    }
    fn lookback(&self) -> usize {
        pback(self.fast.max(self.slow), 1)
    }
    shared!("ppo", scalar);
}

// =================================== apo ====================================

/// Absolute price oscillator (registry key `apo`, one line, parameters `fast` and `slow`):
/// `EMA(fast) - EMA(slow)` of the close. `NaN` until both EMAs have seeded.
#[derive(Clone)]
pub struct Apo {
    fast: f64,
    slow: f64,
    core: ApoCore,
    last: f64,
}
impl Apo {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(12.0, 26.0)
    }
    #[inline]
    fn build(fast: f64, slow: f64) -> Self {
        let core = ApoCore::new(
            fast.round().min(crate::roll::RING_CAP as f64) as usize,
            slow.round().min(crate::roll::RING_CAP as f64) as usize,
        );
        Self { fast, slow, core, last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let fast = p.first().copied().unwrap_or(12.0);
        let slow = p.get(1).copied().unwrap_or(26.0);
        Self::build(fast, slow)
    }
    /// Feed the next bar and get this bar's output; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Apo::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Apo {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Apo, "apo", [fast = 12.0, slow = 26.0]);
impl Apo {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Indicator for Apo {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.fast, self.slow).core;
        let mut out = vec![f64::NAN; bars.len()];
        core.run(bars, |b| b.close, &mut out);
        vec![out]
    }
    fn reset(&mut self) {
        *self = Self::build(self.fast, self.slow);
    }
    fn lookback(&self) -> usize {
        pback(self.fast.max(self.slow), 1)
    }
    shared!("apo", scalar);
}

// =================================== zlema ==================================

/// Zero-lag exponential moving average of the close (registry key `zlema`, one line, parameter
/// `period`): an EMA of the close pushed forward by its own change over `(period - 1) / 2` bars.
/// `NaN` until the EMA has seeded.
#[derive(Clone)]
pub struct Zlema {
    period: f64,
    core: ZlemaCore,
    last: f64,
}
impl Zlema {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: ZlemaCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(20.0))
    }
    /// Feed the next bar and get this bar's output; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Zlema::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Zlema {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Zlema, "zlema", [period = 20.0]);
impl Zlema {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Indicator for Zlema {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.period).core;
        let mut out = vec![f64::NAN; bars.len()];
        core.run(bars, |b| b.close, &mut out);
        vec![out]
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    shared!("zlema", scalar);
}

// ================================== coppock =================================

/// Coppock curve (registry key `coppock`, one line, parameters `wma_p`, `roc_long`, `roc_short`): a
/// weighted moving average over the sum of two rates of change of the close. `NaN` until both rates
/// exist and the weighted average has `wma_p` of their sums.
#[derive(Clone)]
pub struct Coppock {
    wma_p: f64,
    roc_long: f64,
    roc_short: f64,
    core: CoppockCore,
    last: f64,
}
impl Coppock {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(10.0, 14.0, 11.0)
    }
    #[inline]
    fn build(wma_p: f64, roc_long: f64, roc_short: f64) -> Self {
        let cap = crate::roll::RING_CAP as f64;
        Self {
            wma_p,
            roc_long,
            roc_short,
            core: CoppockCore::new(
                wma_p.round().min(cap) as usize,
                roc_long.round().min(cap) as usize,
                roc_short.round().min(cap) as usize,
            ),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let wma_p = p.first().copied().unwrap_or(10.0);
        let roc_long = p.get(1).copied().unwrap_or(14.0);
        let roc_short = p.get(2).copied().unwrap_or(11.0);
        Self::build(wma_p, roc_long, roc_short)
    }
    /// Feed the next bar and get this bar's output; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as
    /// [`Coppock::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Coppock {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Coppock, "coppock", [wma_p = 10.0, roc_long = 14.0, roc_short = 11.0]);
impl Coppock {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Indicator for Coppock {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.wma_p, self.roc_long, self.roc_short).core;
        let mut out = vec![f64::NAN; bars.len()];
        for (o, b) in out.iter_mut().zip(bars) {
            *o = core.push(b.close);
        }
        vec![out]
    }
    fn reset(&mut self) {
        *self = Self::build(self.wma_p, self.roc_long, self.roc_short);
    }
    fn lookback(&self) -> usize {
        pbars(self.roc_long.max(self.roc_short)) + pback(self.wma_p, 1)
    }
    shared!("coppock", scalar);
}

// =================================== tsi ====================================

/// True strength index (registry key `tsi`, one line, parameters `long` and `short`): 100 times the
/// double-smoothed close-to-close change over the double-smoothed absolute change. `NaN` until both
/// smoothings have seeded, and wherever the smoothed absolute change is exactly zero.
#[derive(Clone)]
pub struct Tsi {
    long: f64,
    short: f64,
    core: TsiCore,
    last: f64,
}
impl Tsi {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(25.0, 13.0)
    }
    #[inline]
    fn build(long: f64, short: f64) -> Self {
        let cap = crate::roll::RING_CAP as f64;
        Self {
            long,
            short,
            core: TsiCore::new(long.round().min(cap) as usize, short.round().min(cap) as usize),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let long = p.first().copied().unwrap_or(25.0);
        let short = p.get(1).copied().unwrap_or(13.0);
        Self::build(long, short)
    }
    /// Feed the next bar and get this bar's output; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Tsi::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Tsi {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Tsi, "tsi", [long = 25.0, short = 13.0]);
impl Tsi {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Indicator for Tsi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.long, self.short).core;
        let mut out = vec![f64::NAN; bars.len()];
        for (o, b) in out.iter_mut().zip(bars) {
            *o = core.push(b.close);
        }
        vec![out]
    }
    fn reset(&mut self) {
        *self = Self::build(self.long, self.short);
    }
    fn lookback(&self) -> usize {
        (pbars(self.long) + pbars(self.short)).saturating_sub(2)
    }
    shared!("tsi", scalar);
}

// ================================ smi_ergodic ===============================

/// SMI ergodic oscillator (registry key `smi_ergodic`, two lines `[smi, signal]`, parameters `long`,
/// `short`, `signal`): the true-strength ratio of the close, and an EMA of it over `signal` bars.
/// `smi` is `NaN` until both smoothings have seeded (and wherever the smoothed absolute change is
/// zero); `signal` trails it by `signal - 1` defined values.
#[derive(Clone)]
pub struct SmiErgodic {
    long: f64,
    short: f64,
    signal: f64,
    core: SmiCore,
    last: [f64; 2],
}
impl SmiErgodic {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0, 5.0, 5.0)
    }
    #[inline]
    fn build(long: f64, short: f64, signal: f64) -> Self {
        let cap = crate::roll::RING_CAP as f64;
        Self {
            long,
            short,
            signal,
            core: SmiCore::new(
                long.round().min(cap) as usize,
                short.round().min(cap) as usize,
                signal.round().min(cap) as usize,
            ),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let long = p.first().copied().unwrap_or(20.0);
        let short = p.get(1).copied().unwrap_or(5.0);
        let signal = p.get(2).copied().unwrap_or(5.0);
        Self::build(long, short, signal)
    }
    /// Feed the next bar and get `[smi, signal]`; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as
    /// [`SmiErgodic::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> [f64; 2] {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for SmiErgodic {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(SmiErgodic, "smi_ergodic", [long = 20.0, short = 5.0, signal = 5.0]);
impl SmiErgodic {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_value(bar.close)
    }
}
impl Indicator for SmiErgodic {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.long, self.short, self.signal).core;
        let mut out = [vec![f64::NAN; bars.len()], vec![f64::NAN; bars.len()]];
        let [a, b] = &mut out;
        for ((x, y), bar) in a.iter_mut().zip(b.iter_mut()).zip(bars) {
            let v = core.push(bar.close);
            *x = v[0];
            *y = v[1];
        }
        out.into()
    }
    fn reset(&mut self) {
        *self = Self::build(self.long, self.short, self.signal);
    }
    // The signal line is a `signal`-bar EMA of the smi line.
    fn lookback(&self) -> usize {
        (pbars(self.long) + pbars(self.short)).saturating_sub(2)
    }
    fn lookback_full(&self) -> usize {
        (pbars(self.long) + pbars(self.short)).saturating_sub(2) + pback(self.signal, 1)
    }
    shared!("smi_ergodic", lines);
}

// =================================== kvo ====================================

/// Klinger volume oscillator (registry key `kvo`, two lines `[kvo, signal]`, parameters `fast`,
/// `slow`, `signal`): the difference of a fast and a slow EMA of the signed volume force, and an EMA
/// of that difference over `signal` bars. `kvo` is `NaN` until both EMAs have seeded (the first bar
/// has no force); `signal` trails it by `signal - 1` bars.
///
/// Runs `crate::fast_k::KvoCore`, the one `push` the stream, `vectorize` and `series::kvo` share.
#[derive(Clone)]
pub struct Kvo {
    fast: f64,
    slow: f64,
    signal: f64,
    core: KvoCore,
    last: [f64; 2],
}
impl Kvo {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(34.0, 55.0, 13.0)
    }
    #[inline]
    fn build(fast: f64, slow: f64, signal: f64) -> Self {
        let cap = crate::roll::RING_CAP as f64;
        Self {
            fast,
            slow,
            signal,
            core: KvoCore::new(
                fast.round().min(cap) as usize,
                slow.round().min(cap) as usize,
                signal.round().min(cap) as usize,
            ),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let fast = p.first().copied().unwrap_or(34.0);
        let slow = p.get(1).copied().unwrap_or(55.0);
        let signal = p.get(2).copied().unwrap_or(13.0);
        Self::build(fast, slow, signal)
    }
    /// Feed the next bar and get `[kvo, signal]`; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.step(bar)
    }
    /// Feed the next bar's `high`, `low`, `close` and `volume` as plain numbers, no [`Bar`] needed; the
    /// same answer as [`Kvo::next`] for a bar with those values.
    #[inline]
    pub fn next_hlcv(&mut self, high: f64, low: f64, close: f64, volume: f64) -> [f64; 2] {
        self.last = self.core.push(high, low, close, volume);
        self.last
    }
}
impl Default for Kvo {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Kvo, "kvo", [fast = 34.0, slow = 55.0, signal = 13.0]);
impl Kvo {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hlcv(bar.high, bar.low, bar.close, bar.volume)
    }
}
impl Indicator for Kvo {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 2>(self, bars, Self::step)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 2];
    }
    // The signal line is a `signal`-bar EMA of the kvo line.
    fn lookback(&self) -> usize {
        pbars(self.fast.max(self.slow))
    }
    fn lookback_full(&self) -> usize {
        pbars(self.fast.max(self.slow)) + pback(self.signal, 1)
    }
    shared!("kvo", lines);
}

// =================================== mass ===================================

/// Mass index (registry key `mass`, one line, parameters `period` and `ema_period`): the sum over
/// `period` bars of the ratio of an EMA of the high-low range to the EMA of that EMA. `NaN` until a
/// full window of defined ratios exists.
#[derive(Clone)]
pub struct Mass {
    period: f64,
    ema_period: f64,
    core: MassCore,
    last: f64,
}
impl Mass {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(25.0, 9.0)
    }
    #[inline]
    fn build(period: f64, ema_period: f64) -> Self {
        let cap = crate::roll::RING_CAP as f64;
        Self {
            period,
            ema_period,
            core: MassCore::new(
                period.round().min(cap) as usize,
                ema_period.round().min(cap) as usize,
            ),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let period = p.first().copied().unwrap_or(25.0);
        let ema_period = p.get(1).copied().unwrap_or(9.0);
        Self::build(period, ema_period)
    }
    /// Feed the next bar and get this bar's output; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next bar's high and low as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Mass::next`] for a bar with that high and low.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> f64 {
        self.last = self.core.push(high, low);
        self.last
    }
}
impl Default for Mass {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Mass, "mass", [period = 25.0, ema_period = 9.0]);
impl Mass {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_hl(bar.high, bar.low)
    }
}
impl Indicator for Mass {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.period, self.ema_period).core;
        let mut out = vec![f64::NAN; bars.len()];
        for (o, b) in out.iter_mut().zip(bars) {
            *o = core.push(b.high, b.low);
        }
        vec![out]
    }
    fn reset(&mut self) {
        *self = Self::build(self.period, self.ema_period);
    }
    fn lookback(&self) -> usize {
        (pbars(self.period) + 2 * pbars(self.ema_period)).saturating_sub(3)
    }
    shared!("mass", scalar);
}

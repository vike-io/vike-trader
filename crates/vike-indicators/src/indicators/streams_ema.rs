//! Bounded-state streamers for the three EMA/SMA chains `tema`, `t3` and `kst`.
//!
//! As `hist_indicator!` types they re-ran the whole batch kernel over a retained tail of history on
//! every bar. Each is a fixed chain of EMA / SMA stages over the previous stage's DEFINED values
//! (`math::smooth_defined`), so the exact streaming state is one average per stage: a bar costs O(1)
//! and nothing grows, and there is no truncated tail at all, so the value is exact rather than
//! exact-once-the-tail-has-forgotten. The mapping from `smooth_defined(src, ma, n)` to a stage:
//!
//! - it drops the `NaN` entries of `src`, so a stage is fed ONLY the non-`NaN` values of the stage
//!   before it;
//! - `ma` then returns `NaN` for its own warm-up; a position whose `src` was `NaN` stays `NaN`
//!   without touching the stage's state;
//! - `ma(_, 0)` is all `NaN`, so a zero period is a stage that never emits.
//!
//! `tema` takes its FIRST EMA straight over the closes (`math::ema`, no filtering: a `NaN` close
//! poisons it), `t3`'s first EMA goes through `smooth_defined` too (a `NaN` close is dropped); both
//! are mirrored as written. `tema` and `t3` run the cores in `crates/vike-indicators/src/fast_b.rs`
//! (one `push` shared with `series::tema` and `series::t3`, a branch-free steady state once every
//! stage is seeded); `kst` runs the core in `crates/vike-indicators/src/fast_i.rs`.
//!
//! Every type is hand-written in the `base/incremental.rs` sense (`trims_history() == false`), and
//! `tests/streamers_ema_exact.rs` folds each over a long series and compares every bar and line
//! with `vectorize` by bits.

use super::macros::typed_new;
use super::{pback, pbars};
use crate::Indicator;
use crate::fast_b::{T3Core, TemaCore};
use crate::fast_i::KstCore;
use vike_marketdata::Bar;

/// Triple exponential moving average of the close (registry key `tema`, one line, parameter
/// `period`): `3*EMA - 3*EMA(EMA) + EMA(EMA(EMA))`. `NaN` until all three EMAs have seeded.
#[derive(Clone)]
pub struct Tema {
    period: f64,
    core: TemaCore,
    last: f64,
}
impl Tema {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: TemaCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
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
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Tema::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Tema {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Tema, "tema", [period = 20.0]);
impl Tema {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Indicator for Tema {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.period).core;
        let mut out = vec![f64::NAN; bars.len()];
        core.run(bars, |b| b.close, &mut out);
        vec![out]
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn name(&self) -> &str {
        "tema"
    }
    fn lookback(&self) -> usize {
        (3 * pbars(self.period)).saturating_sub(3)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Tillson T3 of the close (registry key `t3`, one line, parameters `period` and volume factor `v`):
/// three chained generalized-DEMA passes, each `(1 + v) * EMA(x) - v * EMA(EMA(x))` over the defined
/// values of its input. `NaN` until the last pass has seeded.
#[derive(Clone)]
pub struct T3 {
    period: f64,
    v: f64,
    core: T3Core,
    last: f64,
}
impl T3 {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0, 0.7)
    }
    #[inline]
    fn build(period: f64, v: f64) -> Self {
        Self {
            period,
            v,
            core: T3Core::new(period.round().min(crate::roll::RING_CAP as f64) as usize, v),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let period = p.first().copied().unwrap_or(20.0);
        let v = p.get(1).copied().unwrap_or(0.7);
        Self::build(period, v)
    }
    /// Feed the next bar and get this bar's output; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`T3::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for T3 {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(T3, "t3", [period = 20.0, v = 0.7]);
impl T3 {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Indicator for T3 {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.period, self.v).core;
        let mut out = vec![f64::NAN; bars.len()];
        core.run(bars, |b| b.close, &mut out);
        vec![out]
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        *self = Self::build(self.period, self.v);
    }
    fn name(&self) -> &str {
        "t3"
    }
    fn lookback(&self) -> usize {
        (6 * pbars(self.period)).saturating_sub(6)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// The registry defaults of `kst`, in the row's parameter order: `roc1 sma1 roc2 sma2 roc3 sma3 roc4
/// sma4 signal`.
const KST_DEFAULTS: [f64; 9] = [10.0, 10.0, 15.0, 10.0, 20.0, 10.0, 30.0, 15.0, 9.0];

/// Know Sure Thing (registry key `kst`, two lines `[kst, signal]`, parameters `roc1 sma1 roc2 sma2
/// roc3 sma3 roc4 sma4 signal`): the weighted sum `1*r1 + 2*r2 + 3*r3 + 4*r4` of four smoothed
/// rates of change, and the simple moving average of that sum over `signal` bars. `kst` is `NaN`
/// until all four components exist; `signal` trails it by `signal - 1` bars.
#[derive(Clone)]
pub struct Kst {
    /// The nine parameters as given (registry order); `lookback` reads these, like the batch row.
    p: [f64; 9],
    core: KstCore,
    last: [f64; 2],
}
impl Kst {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(KST_DEFAULTS)
    }
    #[inline]
    fn build(p: [f64; 9]) -> Self {
        let n = |v: f64| v.round().min(crate::roll::RING_CAP as f64) as usize;
        Self {
            p,
            core: KstCore::new(
                [n(p[0]), n(p[2]), n(p[4]), n(p[6])],
                [n(p[1]), n(p[3]), n(p[5]), n(p[7])],
                n(p[8]),
            ),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let mut all = KST_DEFAULTS;
        for (slot, given) in all.iter_mut().zip(p) {
            *slot = *given;
        }
        Self::build(all)
    }
    /// Feed the next bar and get `[kst, signal]`; `NaN` while warming up. This IS
    /// `Indicator::on_bar`, bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Kst::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> [f64; 2] {
        self.last = self.core.push(close);
        self.last
    }
    /// The bar at which the `kst` line first exists: the slowest of the four components.
    fn warm(&self) -> usize {
        let [roc1, sma1, roc2, sma2, roc3, sma3, roc4, sma4, _signal] = self.p;
        (pbars(roc1) + pback(sma1, 1))
            .max(pbars(roc2) + pback(sma2, 1))
            .max(pbars(roc3) + pback(sma3, 1))
            .max(pbars(roc4) + pback(sma4, 1))
    }
}
impl Default for Kst {
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(
    Kst,
    "kst",
    [
        roc1 = 10.0,
        sma1 = 10.0,
        roc2 = 15.0,
        sma2 = 10.0,
        roc3 = 20.0,
        sma3 = 10.0,
        roc4 = 30.0,
        sma4 = 15.0,
        signal = 9.0
    ]
);
impl Kst {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_value(bar.close)
    }
}
impl Indicator for Kst {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut core = Self::build(self.p).core;
        let mut out = [vec![f64::NAN; bars.len()], vec![f64::NAN; bars.len()]];
        let [a, b] = &mut out;
        for ((x, y), bar) in a.iter_mut().zip(b.iter_mut()).zip(bars) {
            let v = core.push(bar.close);
            *x = v[0];
            *y = v[1];
        }
        out.into()
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        *self = Self::build(self.p);
    }
    fn name(&self) -> &str {
        "kst"
    }
    // `batch_kst` emits only where ALL FOUR roc_sma components are defined, so the warm-up is the MAX
    // over components, not component 4, which merely dominates at the defaults.
    fn lookback(&self) -> usize {
        self.warm()
    }
    fn lookback_full(&self) -> usize {
        self.warm() + pback(self.p[8], 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

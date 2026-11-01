//! Momentum indicators — faithful f64 port of vike-trader-app
//! `core/indicators/momentum.py`. Skips the names already in `base.rs`
//! (rsi/macd/stochastic/cci/roc/ao→awesome/williams_r→williams). Every type here is a hand-written
//! bounded-state stepper (`trims_history() == false`) over a core in `crate::fast_d1` (`mom`, `cmo`,
//! `aroon`, `stochf`) or `crate::fast_f` (`rocp`, `rocr`, `rocr100`, `bop`, `dpo`, `aroonosc`,
//! `elder_ray`, `ultosc`, `vortex`, `ac`). There is no batch kernel: a type's `vectorize` IS its
//! core folded over the bars (the rate-of-change family reads the input itself as the window), and
//! `crate::series` runs the same core over plain slices, so a stream, a batch and a slice call agree
//! bit for bit.

use super::{pback, pbars};
use crate::Indicator;
use crate::fast_d1::{AroonCore, CmoCore, MomCore, StochfCore, window};
use crate::fast_f::{
    AcCore, AroonOscCore, DpoCore, ElderRayCore, RocpCore, Rocr100Core, RocrCore, UltoscCore,
    VortexCore, bop_of, rocp_of, rocr_of, rocr100_of,
};
use vike_marketdata::Bar;

/// One rate-of-change type: `period` bars (default 10, registry range `1..=400`), one line, over a
/// core of `crate::fast_f` and the expression `$of` that core applies.
macro_rules! roc_stepper {
    ($(#[$m:meta])* $Name:ident, $key:literal, $Core:ident, $of:ident) => {
        $(#[$m])*
        ///
        /// Hand-written bounded state (a ring of the last `period` closes, `trims_history() == false`).
        #[derive(Clone)]
        pub struct $Name {
            period: f64,
            n: usize,
            core: $Core,
            last: f64,
        }
        impl $Name {
            /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
            #[inline]
            fn defaults() -> Self {
                Self::build(10.0)
            }
            #[inline]
            fn build(period: f64) -> Self {
                let n = window(period);
                Self { period, n, core: $Core::new(n), last: f64::NAN }
            }
            /// Build with `period` bars (registry range `1..=400`). A value the registry would not
            /// accept (out of range, or `NaN`) is a [`crate::ParamError`] naming it;
            /// [`crate::make_with`] clamps instead. `Default` gives the registry default (10).
            #[inline]
            pub fn new(period: f64) -> Result<Self, crate::ParamError> {
                let raw: &[f64] = &[period];
                crate::typed::checked($key, raw)?;
                Ok(Self::with_params(raw))
            }
            #[inline]
            pub(crate) fn with_params(p: &[f64]) -> Self {
                Self::build(p.first().copied().unwrap_or(10.0))
            }
            /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
            #[inline]
            pub fn next(&mut self, bar: &Bar) -> f64 {
                self.next_value(bar.close)
            }
            /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as `next`
            /// for a bar with that close.
            #[inline]
            pub fn next_value(&mut self, close: f64) -> f64 {
                self.last = self.core.push(close);
                self.last
            }
        }
        impl Default for $Name {
            fn default() -> Self {
                Self::defaults()
            }
        }
        impl Indicator for $Name {
            fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
                vec![self.next_value(bar.close)]
            }
            fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
                // The one expression `push` makes, with the ring replaced by the input itself.
                let n = self.n;
                let len = bars.len();
                let mut out = Vec::with_capacity(len);
                out.resize(len.min(n), f64::NAN);
                if len > n {
                    out.extend(bars[n..].iter().zip(bars).map(|(now, then)| $of(now.close, then.close)));
                }
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
                $key
            }
            fn lookback(&self) -> usize {
                pbars(self.period)
            }
            fn lookback_exact(&self) -> bool {
                true
            }
        }
    };
}

/// Momentum of the close over `period` bars (default 10; registry key `mom`, one line):
/// `close[i] - close[i - period]`. `NaN` for the first `period` values.
///
/// Hand-written bounded state (a ring of the last `period` closes, `trims_history() == false`).
#[derive(Clone)]
pub struct Mom {
    period: f64,
    n: usize,
    core: MomCore,
    last: f64,
}
impl Mom {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(10.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        let n = window(period);
        Self { period, n, core: MomCore::new(n), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `1..=400`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (10).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("mom", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(10.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Mom::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Mom {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Mom {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.next_value(bar.close)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        // The one subtraction `MomCore::push` makes, with the ring replaced by the input itself.
        let n = self.n;
        let len = bars.len();
        let mut out = Vec::with_capacity(len);
        out.resize(len.min(n), f64::NAN);
        if len > n {
            out.extend(bars[n..].iter().zip(bars).map(|(now, then)| now.close - then.close));
        }
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
        "mom"
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
roc_stepper! {
    /// Rate-of-change proportion of the close over `period` bars (default 10; registry key `rocp`,
    /// one line): `(close[i] - close[i - period]) / close[i - period]`. `NaN` for the first `period`
    /// values and wherever the earlier close is zero.
    Rocp, "rocp", RocpCore, rocp_of
}
roc_stepper! {
    /// Rate-of-change ratio of the close over `period` bars (default 10; registry key `rocr`, one
    /// line): `close[i] / close[i - period]`. `NaN` for the first `period` values and wherever the
    /// earlier close is zero.
    Rocr, "rocr", RocrCore, rocr_of
}
roc_stepper! {
    /// Rate-of-change ratio of the close over `period` bars times 100 (default 10; registry key
    /// `rocr100`, one line): `close[i] / close[i - period] * 100`. `NaN` for the first `period`
    /// values and wherever the earlier close is zero.
    Rocr100, "rocr100", Rocr100Core, rocr100_of
}
/// Chande momentum oscillator of the close over `period` moves (default 14; registry key `cmo`, one
/// line, -100 to 100): `100 * (up - down) / (up + down)` over the last `period` bar-to-bar moves, `0`
/// when none of them moved. `NaN` for the first `period` values.
///
/// Hand-written bounded state: the window's up and down totals come from a ring that is turned into
/// exact suffix sums once a lap (`crate::fast_d1::PairSums`), so a flat stretch is exactly `0`.
#[derive(Clone)]
pub struct Cmo {
    period: f64,
    core: CmoCore,
    last: f64,
}
impl Cmo {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self { period, core: CmoCore::new(window(period)), last: f64::NAN }
    }
    /// Build with `period` moves (registry range `2..=200`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("cmo", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Cmo::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Cmo {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Cmo {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.next_value(bar.close)]
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
        "cmo"
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Balance of power (registry key `bop`, one line, no parameters): `(close - open) / (high - low)`,
/// `0` on a bar with no range. A value from the first bar.
///
/// Hand-written and stateless (`trims_history() == false`).
#[derive(Clone)]
pub struct Bop {
    last: f64,
}
impl Bop {
    /// The registry defaults (there are none). Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self { last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_ohlc(bar.open, bar.high, bar.low, bar.close)
    }
    /// Feed the next open, high, low and close as plain numbers, no [`Bar`] needed; the same answer
    /// as [`Bop::next`] for a bar with those.
    #[inline]
    pub fn next_ohlc(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
        self.last = bop_of(open, high, low, close);
        self.last
    }
}
impl Default for Bop {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Bop {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.next(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut out = vec![f64::NAN; bars.len()];
        for (o, b) in out.iter_mut().zip(bars) {
            *o = bop_of(b.open, b.high, b.low, b.close);
        }
        vec![out]
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "bop"
    }
    fn lookback(&self) -> usize {
        0
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Detrended price oscillator of the close over `period` bars (default 20; registry key `dpo`, one
/// line): the close `period / 2 + 1` bars back minus the `period`-bar mean of the close. `NaN` until
/// both are defined.
///
/// Hand-written bounded state (a running mean and a ring of the last `period / 2 + 1` closes,
/// `trims_history() == false`).
#[derive(Clone)]
pub struct Dpo {
    period: f64,
    core: DpoCore,
    last: f64,
}
impl Dpo {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self { period, core: DpoCore::new(window(period)), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=400`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (20).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("dpo", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(20.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Dpo::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
}
impl Default for Dpo {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Dpo {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.next_value(bar.close)]
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
        "dpo"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Aroon up and down over `period` bars (default 14; registry key `aroon`, two lines
/// `[aroon_up, aroon_down]`, 0 to 100): how recently, within the last `period + 1` bars, the highest
/// high and the lowest low were made, `100 * (period - bars_ago) / period`; a tie goes to the newest
/// bar. `NaN` for the first `period` values.
///
/// Hand-written bounded state: two monotonic deques, O(1) a bar whatever `period`. A `NaN` high or
/// low is ignored, like the kernel's `f64::max`.
#[derive(Clone)]
pub struct Aroon {
    period: f64,
    core: AroonCore,
    last: [f64; 2],
}
impl Aroon {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self { period, core: AroonCore::new(window(period)), last: [f64::NAN; 2] }
    }
    /// Build with `period` bars (registry range `2..=400`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("aroon", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar and get `[aroon_up, aroon_down]`; this IS `Indicator::on_bar`, bit for
    /// bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hl(bar.high, bar.low)
    }
    /// Feed the next high and low as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Aroon::next`] for a bar with those.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> [f64; 2] {
        self.last = self.core.push(high, low);
        self.last
    }
}
impl Default for Aroon {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Aroon {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array(self, bars, Self::next)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 2];
    }
    fn name(&self) -> &str {
        "aroon"
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Aroon oscillator over `period` bars (default 14; registry key `aroonosc`, one line, -100 to 100):
/// aroon up minus aroon down, see [`Aroon`]. `NaN` for the first `period` values.
///
/// Hand-written bounded state, the same two monotonic windows as [`Aroon`], O(1) a bar whatever
/// `period` (`trims_history() == false`).
#[derive(Clone)]
pub struct Aroonosc {
    period: f64,
    core: AroonOscCore,
    last: f64,
}
impl Aroonosc {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self { period, core: AroonOscCore::new(window(period)), last: f64::NAN }
    }
    /// Build with `period` bars (registry range `2..=400`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("aroonosc", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_hl(bar.high, bar.low)
    }
    /// Feed the next high and low as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Aroonosc::next`] for a bar with those.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> f64 {
        self.last = self.core.push(high, low);
        self.last
    }
}
impl Default for Aroonosc {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Aroonosc {
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
        "aroonosc"
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Elder ray over `period` bars (default 13; registry key `elder_ray`, two lines
/// `[bull_power, bear_power]`): the high and the low minus the exponential average of the close, seeded
/// with the mean of its first `period` closes. `NaN` for the first `period - 1` values; a `NaN` close
/// poisons the average from then on.
///
/// Hand-written bounded state (`trims_history() == false`).
#[derive(Clone)]
pub struct ElderRay {
    period: f64,
    core: ElderRayCore,
    last: [f64; 2],
}
impl ElderRay {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(13.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self { period, core: ElderRayCore::new(window(period)), last: [f64::NAN; 2] }
    }
    /// Build with `period` bars (registry range `2..=200`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (13).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("elder_ray", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(13.0))
    }
    /// Feed the next bar and get `[bull_power, bear_power]`; this IS `Indicator::on_bar`, bit for
    /// bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
    /// Feed the next high, low and close as plain numbers, no [`Bar`] needed; the same answer as
    /// [`ElderRay::next`] for a bar with those.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for ElderRay {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for ElderRay {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array(self, bars, Self::next)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 2];
    }
    fn name(&self) -> &str {
        "elder_ray"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Fast stochastic over `k` bars with a `d`-value mean (defaults 14 and 3; registry key `stochf`, two
/// lines `[%K, %D]`, 0 to 100): `%K = 100 * (close - lowest low) / (highest high - lowest low)` over
/// the last `k` bars, `0` on a window with no range, and `%D` the simple mean of the last `d`
/// values of `%K`. `NaN` until `k` bars (`%K`) and `k + d - 1` bars (`%D`) are in.
///
/// Hand-written bounded state: two monotonic deques for the extremes and a ring for `%D`.
#[derive(Clone)]
pub struct Stochf {
    k: f64,
    d: f64,
    core: StochfCore,
    last: [f64; 2],
}
impl Stochf {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0, 3.0)
    }
    #[inline]
    fn build(k: f64, d: f64) -> Self {
        Self { k, d, core: StochfCore::new(window(k), window(d)), last: [f64::NAN; 2] }
    }
    /// Build with a `k`-bar window (registry range `2..=100`) and a `d`-value mean (`1..=50`). A
    /// value the registry would not accept (out of range, or `NaN`) is a [`crate::ParamError`]
    /// naming it; [`crate::make_with`] clamps instead. `Default` gives the registry defaults.
    #[inline]
    pub fn new(k: f64, d: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[k, d];
        crate::typed::checked("stochf", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let k = p.first().copied().unwrap_or(14.0);
        let d = p.get(1).copied().unwrap_or(3.0);
        Self::build(k, d)
    }
    /// Feed the next bar and get `[%K, %D]`; this IS `Indicator::on_bar`, bit for bit, without the
    /// Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
    /// Feed the next high, low and close as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Stochf::next`] for a bar with those.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for Stochf {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Stochf {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array(self, bars, Self::next)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 2];
    }
    fn name(&self) -> &str {
        "stochf"
    }
    fn lookback(&self) -> usize {
        pback(self.k, 1)
    }
    fn lookback_full(&self) -> usize {
        pback(self.k, 1) + pback(self.d, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Ultimate oscillator over three windows of `p1`, `p2` and `p3` bars (defaults 7, 14 and 28;
/// registry key `ultosc`, one line, 0 to 100): `100 * (4 * a1 + 2 * a2 + a3) / 7`, `a` being the
/// buying pressure over the true range summed across the window (`0` for a window with no range).
/// `NaN` for the first `max(p1, p2, p3)` values.
///
/// Hand-written bounded state: three sets of exact window sums, each rebuilt once a lap from its own
/// ring (`trims_history() == false`).
#[derive(Clone)]
pub struct Ultosc {
    p1: f64,
    p2: f64,
    p3: f64,
    core: UltoscCore,
    last: f64,
}
impl Ultosc {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(7.0, 14.0, 28.0)
    }
    #[inline]
    fn build(p1: f64, p2: f64, p3: f64) -> Self {
        Self {
            p1,
            p2,
            p3,
            core: UltoscCore::new(window(p1), window(p2), window(p3)),
            last: f64::NAN,
        }
    }
    /// Build with windows of `p1` (registry range `2..=100`), `p2` (`2..=200`) and `p3` (`2..=400`)
    /// bars. A value the registry would not accept (out of range, or `NaN`) is a
    /// [`crate::ParamError`] naming it; [`crate::make_with`] clamps instead. `Default` gives the
    /// registry defaults (7, 14, 28).
    #[inline]
    pub fn new(p1: f64, p2: f64, p3: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[p1, p2, p3];
        crate::typed::checked("ultosc", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        let p1 = p.first().copied().unwrap_or(7.0);
        let p2 = p.get(1).copied().unwrap_or(14.0);
        let p3 = p.get(2).copied().unwrap_or(28.0);
        Self::build(p1, p2, p3)
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
    /// Feed the next high, low and close as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Ultosc::next`] for a bar with those.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for Ultosc {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Ultosc {
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
        "ultosc"
    }
    fn lookback(&self) -> usize {
        pbars(self.p1.max(self.p2).max(self.p3))
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Vortex indicator over `period` bars (default 14; registry key `vortex`, two lines
/// `[vi_plus, vi_minus]`): `sum(|high - previous low|)` and `sum(|low - previous high|)` over the
/// window, each divided by the window's summed true range. `NaN` for the first `period` values and
/// where the true range sums to nothing.
///
/// Hand-written bounded state: exact window sums rebuilt once a lap from a ring
/// (`trims_history() == false`).
#[derive(Clone)]
pub struct Vortex {
    period: f64,
    core: VortexCore,
    last: [f64; 2],
}
impl Vortex {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self { period, core: VortexCore::new(window(period)), last: [f64::NAN; 2] }
    }
    /// Build with `period` bars (registry range `2..=200`). A value the registry would not accept
    /// (out of range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps
    /// instead. `Default` gives the registry default (14).
    #[inline]
    pub fn new(period: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period];
        crate::typed::checked("vortex", raw)?;
        Ok(Self::with_params(raw))
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar and get `[vi_plus, vi_minus]`; this IS `Indicator::on_bar`, bit for bit,
    /// without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
    /// Feed the next high, low and close as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Vortex::next`] for a bar with those.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
impl Default for Vortex {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Vortex {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array(self, bars, Self::next)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 2];
    }
    fn name(&self) -> &str {
        "vortex"
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
/// Accelerator oscillator (registry key `ac`, one line, no parameters): the awesome oscillator
/// (`SMA5 - SMA34` of the median price) minus its own 5-value mean. `NaN` for the first 37 values.
///
/// Hand-written bounded state (`trims_history() == false`).
#[derive(Clone)]
pub struct Ac {
    core: AcCore,
    last: f64,
}
impl Ac {
    /// The registry defaults (there are none). Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self { core: AcCore::new(), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.next_hl(bar.high, bar.low)
    }
    /// Feed the next high and low as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Ac::next`] for a bar with those.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> f64 {
        self.last = self.core.push(high, low);
        self.last
    }
}
impl Default for Ac {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Ac {
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
        "ac"
    }
    fn lookback(&self) -> usize {
        37
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
    fn mom_is_its_registry_row() {
        assert_matches_registry("mom", Box::new(Mom::new(10.0).unwrap()));
    }
    #[test]
    fn cmo_is_its_registry_row() {
        assert_matches_registry("cmo", Box::new(Cmo::new(14.0).unwrap()));
    }
    #[test]
    fn aroon_is_its_registry_row() {
        assert_matches_registry("aroon", Box::new(Aroon::new(14.0).unwrap()));
    }
    #[test]
    fn stochf_is_its_registry_row() {
        assert_matches_registry("stochf", Box::new(Stochf::new(14.0, 3.0).unwrap()));
    }
    #[test]
    fn rocp_is_its_registry_row() {
        assert_matches_registry("rocp", Box::new(Rocp::new(10.0).unwrap()));
    }
    #[test]
    fn rocr_is_its_registry_row() {
        assert_matches_registry("rocr", Box::new(Rocr::new(10.0).unwrap()));
    }
    #[test]
    fn rocr100_is_its_registry_row() {
        assert_matches_registry("rocr100", Box::new(Rocr100::new(10.0).unwrap()));
    }
    #[test]
    fn bop_is_its_registry_row() {
        assert_matches_registry("bop", Box::new(Bop::default()));
    }
    #[test]
    fn dpo_is_its_registry_row() {
        assert_matches_registry("dpo", Box::new(Dpo::new(20.0).unwrap()));
    }
    #[test]
    fn aroonosc_is_its_registry_row() {
        assert_matches_registry("aroonosc", Box::new(Aroonosc::new(14.0).unwrap()));
    }
    #[test]
    fn elder_ray_is_its_registry_row() {
        assert_matches_registry("elder_ray", Box::new(ElderRay::new(13.0).unwrap()));
    }
    #[test]
    fn ultosc_is_its_registry_row() {
        assert_matches_registry("ultosc", Box::new(Ultosc::new(7.0, 14.0, 28.0).unwrap()));
    }
    #[test]
    fn vortex_is_its_registry_row() {
        assert_matches_registry("vortex", Box::new(Vortex::new(14.0).unwrap()));
    }
    #[test]
    fn ac_is_its_registry_row() {
        assert_matches_registry("ac", Box::new(Ac::default()));
    }

    /// A raw parameter slice the registry would clamp (`with_params` is also reachable unclamped)
    /// must not wrap, overflow (a debug build panics) or ask for a ring of `usize::MAX` values.
    #[test]
    fn with_params_degenerate_never_wraps() {
        macro_rules! probe {
            ($($ty:ty),* $(,)?) => {$(
                for raw in [
                    &[0.0f64][..],
                    &[0.0, 0.0, 0.0][..],
                    &[f64::NAN, f64::NAN, f64::NAN][..],
                    &[-1e9, -1e9, -1e9][..],
                    &[f64::INFINITY, f64::NEG_INFINITY, 0.0][..],
                    &[1e18, 1e18, 1e18][..],
                ] {
                    let mut ind = <$ty>::with_params(raw);
                    let (lb, full) = (ind.lookback(), ind.lookback_full());
                    assert!(lb < 1_000_000, "{} lookback {lb} wrapped at {raw:?}", ind.name());
                    assert!(full >= lb && full < 1_000_000, "{} lookback_full at {raw:?}", ind.name());
                    // And it still steps.
                    let bar = Bar::ohlcv(0, 1.0, 2.0, 0.5, 1.5, 1.0);
                    for _ in 0..3 {
                        let _ = ind.on_bar(&bar);
                    }
                }
            )*};
        }
        probe!(Rocp, Rocr, Rocr100, Bop, Dpo, Aroonosc, ElderRay, Ultosc, Vortex, Ac);
    }
}

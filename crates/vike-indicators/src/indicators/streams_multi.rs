//! Bounded-state streamers for eight multi-line indicators: `gmma`, `alligator`, `relative_vigor`,
//! `high_low_52w`, `stochrsi`, `fisher`, `ichimoku` and `volume_profile_poc`.
//!
//! As `hist_indicator!` types they re-ran the whole batch kernel over a retained tail of history on
//! every bar (MEASURED per bar at 20k bars: `gmma` 141 us, `alligator` 54 us, `relative_vigor`
//! 29 us, `high_low_52w` 28 us, `stochrsi` 27 us, `fisher` 26 us, `ichimoku` 24 us,
//! `volume_profile_poc` 15 us). Here each carries only what its kernel reads at the newest bar.
//!
//! Six of them (`gmma`, `alligator`, `relative_vigor`, `high_low_52w`, `fisher` and `stochrsi`) run a
//! fast core (`crate::fast_h`, `crate::fast_c`): a small struct of plain numbers whose `push` is the
//! ONE implementation of the arithmetic, shared by the stepper (`next`, `next_value`, `next_hl`,
//! `next_ohlc`, `on_bar`), by `vectorize` and by the whole-series calls in `crate::series`, so every
//! way in agrees bit for bit. The other two (`ichimoku`, `volume_profile_poc`) keep the older shape: a
//! ring (`VecDeque`) of exactly the values a rolling window folds, recomputed oldest value first,
//! never updated by `sum += x - old`.
//!
//! `ichimoku`'s `chikou` line plots the close `kijun` bars BACK, i.e. it reads future bars, so the
//! registry row is `batch_only` and `on_bar` reports the causal best effort the macro-generated type
//! did: `NaN` at the tip (the batch has no bar `kijun` ahead of the newest one). `vectorize` is the
//! unchanged kernel and is authoritative for `chikou`; the type is untouched by the speed work. A
//! `shift`-bar forward plot (`ichimoku`'s cloud) is a delay line: the value the kernel writes at
//! index `i + shift` is the one produced `shift` bars earlier, so the newest bar reads the OLDEST
//! entry of a ring of `shift + 1` raw values.

use std::collections::VecDeque;

use super::macros::{lane_ty, typed_new};
use super::overlap::batch_ichimoku;
use super::{pback, pbars};
use crate::Indicator;
use crate::fast_h::{AlligatorCore, FisherCore, GmmaCore, HiLoCore, VigorCore};
use vike_marketdata::Bar;

/// `Default` and the user-facing `next` every type here shares (`$arity` is the output-line count).
macro_rules! surface {
    ($Name:ident, $arity:tt) => {
        impl Default for $Name {
            fn default() -> Self {
                Self::defaults()
            }
        }
        impl $Name {
            /// Feed the next bar and get this bar's output: an `f64` for a one-line indicator, an
            /// `[f64; N]` for N lines (their names are the registry row's `outputs`). `NaN` while
            /// warming up. This IS `Indicator::on_bar`, bit for bit, without the `Vec`.
            #[inline]
            pub fn next(&mut self, bar: &Bar) -> lane_ty!($arity) {
                self.step(bar)
            }
        }
    };
}

/// The `value` and `name` impl items every type here shares.
macro_rules! shared {
    (@name $key:literal) => {
        fn name(&self) -> &str {
            $key
        }
    };
    ($key:literal, scalar) => {
        fn value(&self) -> Vec<f64> {
            vec![self.last]
        }
        shared!(@name $key);
    };
    ($key:literal, lines) => {
        fn value(&self) -> Vec<f64> {
            self.last.to_vec()
        }
        shared!(@name $key);
    };
}

/// Append `x`, keeping only the newest `cap` entries.
fn push_capped(ring: &mut VecDeque<f64>, x: f64, cap: usize) {
    ring.push_back(x);
    if ring.len() > cap {
        ring.pop_front();
    }
}

/// `values.fold(f64::MIN, f64::max)`: the exact fold every kernel here uses for a rolling highest.
fn highest<'a>(values: impl Iterator<Item = &'a f64>) -> f64 {
    values.copied().fold(f64::MIN, f64::max)
}

/// `values.fold(f64::MAX, f64::min)`: the exact fold every kernel here uses for a rolling lowest.
fn lowest<'a>(values: impl Iterator<Item = &'a f64>) -> f64 {
    values.copied().fold(f64::MAX, f64::min)
}

/// A line plotted `shift` bars forward: `forward_shift` in the kernels writes `raw[i]` at
/// `i + shift`, so the value AT bar `i` is `raw[i - shift]` (`NaN` before that bar exists, and `NaN`
/// where the raw value was `NaN`).
#[derive(Clone)]
struct Delay {
    shift: usize,
    raw: VecDeque<f64>,
}
impl Delay {
    fn new(shift: usize) -> Self {
        Self { shift, raw: VecDeque::new() }
    }
    fn push(&mut self, raw: f64) -> f64 {
        push_capped(&mut self.raw, raw, self.shift + 1);
        if self.raw.len() == self.shift + 1 { self.raw[0] } else { f64::NAN }
    }
}

// ============================== gmma ======================================

/// Guppy Multiple Moving Average (registry key `gmma`, twelve lines, no parameters): the EMAs of
/// the close over 3, 5, 8, 10, 12, 15, 30, 35, 40, 45, 50 and 60 bars. Each line is `NaN` until its
/// own EMA has seeded.
#[derive(Clone)]
pub struct Gmma {
    core: GmmaCore,
    last: [f64; 12],
}
impl Gmma {
    /// The registry defaults. Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self { core: GmmaCore::new(), last: [f64::NAN; 12] }
    }
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Gmma::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> [f64; 12] {
        self.last = self.core.push(close);
        self.last
    }
}
surface!(Gmma, 12);
typed_new!(Gmma, "gmma", []);
impl Gmma {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 12] {
        self.next_value(bar.close)
    }
}
impl Indicator for Gmma {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 12>(self, bars, Self::step)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 12];
    }
    // Shortest EMA is 3 (index 2); the longest of the twelve is 60 (index 59).
    fn lookback(&self) -> usize {
        2
    }
    fn lookback_full(&self) -> usize {
        59
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    shared!("gmma", lines);
}

// ============================ alligator ===================================

/// Williams Alligator (registry key `alligator`, three lines `jaw`, `teeth`, `lips`, no
/// parameters): Wilder-smoothed median prices over 13, 8 and 5 bars, plotted 8, 5 and 3 bars
/// forward. `NaN` until each line's smoothing has seeded and its shift has elapsed.
#[derive(Clone)]
pub struct Alligator {
    core: AlligatorCore,
    last: [f64; 3],
}
impl Alligator {
    /// The registry defaults. Private: `Default` is the public spelling.
    #[inline]
    fn defaults() -> Self {
        Self { core: AlligatorCore::new(), last: [f64::NAN; 3] }
    }
    pub(crate) fn with_params(_p: &[f64]) -> Self {
        Self::defaults()
    }
    /// Feed the next high and low as plain numbers, no [`Bar`] needed; `[jaw, teeth, lips]`, the
    /// same answer as [`Alligator::next`] for a bar with those.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> [f64; 3] {
        self.last = self.core.push(high, low);
        self.last
    }
}
surface!(Alligator, 3);
typed_new!(Alligator, "alligator", []);
impl Alligator {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 3] {
        self.next_hl(bar.high, bar.low)
    }
}
impl Indicator for Alligator {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 3>(self, bars, Self::step)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 3];
    }
    // lips(5)-1 + shift(3) = 7; jaw is (13)-1 + shift(8) = 20.
    fn lookback(&self) -> usize {
        7
    }
    fn lookback_full(&self) -> usize {
        20
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    shared!("alligator", lines);
}

// =========================== relative_vigor ===============================

/// Relative Vigor Index (registry key `relative_vigor`, lines `rvgi` and `signal`, parameter
/// `period`): the sum over `period` bars of the 4-bar-smoothed close-minus-open divided by the sum
/// of the smoothed high-minus-low, and a 4-bar smoothing of that line. `NaN` while warming up and
/// wherever the range sum is zero.
#[derive(Clone)]
pub struct RelativeVigor {
    period: f64,
    core: VigorCore,
    last: [f64; 2],
}
impl RelativeVigor {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(10.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: VigorCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(10.0))
    }
    /// Feed the next bar's `open`, `high`, `low` and `close` as plain numbers, no [`Bar`] needed;
    /// `[rvgi, signal]`, the same answer as [`RelativeVigor::next`] for a bar with those.
    #[inline]
    pub fn next_ohlc(&mut self, open: f64, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.last = self.core.push(open, high, low, close);
        self.last
    }
}
surface!(RelativeVigor, 2);
typed_new!(RelativeVigor, "relative_vigor", [period = 10.0]);
impl RelativeVigor {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_ohlc(bar.open, bar.high, bar.low, bar.close)
    }
}
impl Indicator for RelativeVigor {
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
    // The signal line is a 4-bar SWMA of rvgi -> three bars later.
    fn lookback(&self) -> usize {
        pbars(self.period) + 2
    }
    fn lookback_full(&self) -> usize {
        pbars(self.period) + 5
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    shared!("relative_vigor", lines);
}

// ============================ high_low_52w ================================

/// Rolling N-bar high and low (registry key `high_low_52w`, lines `high_n` and `low_n`, parameter
/// `period`, 252 by default for a year of daily bars): the highest high and lowest low of the last
/// `period` bars. `NaN` until `period` bars have been seen.
#[derive(Clone)]
pub struct HighLow52w {
    period: f64,
    core: HiLoCore,
    last: [f64; 2],
}
impl HighLow52w {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(252.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: HiLoCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(252.0))
    }
    /// Feed the next high and low as plain numbers, no [`Bar`] needed; `[high_n, low_n]`, the same
    /// answer as [`HighLow52w::next`] for a bar with those.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> [f64; 2] {
        self.last = self.core.push(high, low);
        self.last
    }
}
surface!(HighLow52w, 2);
typed_new!(HighLow52w, "high_low_52w", [period = 252.0]);
impl HighLow52w {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hl(bar.high, bar.low)
    }
}
impl Indicator for HighLow52w {
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
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    shared!("high_low_52w", lines);
}

// ============================== stochrsi ==================================

/// Stochastic RSI (registry key `stochrsi`, lines `%K` and `%D`, parameters `rsi_p`, `k`, `d`): the
/// Wilder RSI of the close placed inside its own `k`-bar range, and a `d`-bar simple average of
/// that. `NaN` while warming up and wherever the RSI window holds a `NaN`.
#[derive(Clone)]
pub struct Stochrsi {
    rsi_p: f64,
    k: f64,
    d: f64,
    core: crate::fast_c::StochRsiCore,
    last: [f64; 2],
}
impl Stochrsi {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    fn defaults() -> Self {
        Self::build(14.0, 14.0, 3.0)
    }
    #[inline]
    fn build(rsi_p: f64, k: f64, d: f64) -> Self {
        Self {
            rsi_p,
            k,
            d,
            core: crate::fast_c::StochRsiCore::new(
                rsi_p.round().min(crate::roll::RING_CAP as f64) as usize,
                k.round().min(crate::roll::RING_CAP as f64) as usize,
                d.round().min(crate::roll::RING_CAP as f64) as usize,
            ),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(
            p.first().copied().unwrap_or(14.0),
            p.get(1).copied().unwrap_or(14.0),
            p.get(2).copied().unwrap_or(3.0),
        )
    }
}
impl Default for Stochrsi {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Stochrsi {
    /// Feed the next bar and get `[%K, %D]`. `NaN` while warming up. This IS `Indicator::on_bar`, bit
    /// for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.step(bar)
    }
}
typed_new!(Stochrsi, "stochrsi", [rsi_p = 14.0, k = 14.0, d = 3.0]);
impl Stochrsi {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.last = self.core.push(bar.close);
        self.last
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; `[%K, %D]`, the same answer as
    /// [`Stochrsi::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> [f64; 2] {
        self.last = self.core.push(close);
        self.last
    }
}
impl Indicator for Stochrsi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::fast_c::stochrsi_bars(&self.core, bars)
    }
    fn reset(&mut self) {
        *self = Self::build(self.rsi_p, self.k, self.d);
    }
    fn lookback(&self) -> usize {
        pbars(self.rsi_p) + pback(self.k, 1)
    }
    fn lookback_full(&self) -> usize {
        pbars(self.rsi_p) + pback(self.k, 1) + pback(self.d, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    shared!("stochrsi", lines);
}

// =============================== fisher ===================================

/// Fisher Transform (registry key `fisher`, lines `fisher` and `trigger`, parameter `period`): the
/// median price placed inside its `period`-bar range, smoothed and run through an inverse hyperbolic
/// tangent. The trigger is the previous bar's Fisher value. `NaN` until `period` bars have been seen
/// (the trigger one bar later).
#[derive(Clone)]
pub struct Fisher {
    period: f64,
    core: FisherCore,
    last: [f64; 2],
}
impl Fisher {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(9.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: FisherCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(9.0))
    }
    /// Feed the next high and low as plain numbers, no [`Bar`] needed; `[fisher, trigger]`, the
    /// same answer as [`Fisher::next`] for a bar with those.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> [f64; 2] {
        self.last = self.core.push(high, low);
        self.last
    }
}
surface!(Fisher, 2);
typed_new!(Fisher, "fisher", [period = 9.0]);
impl Fisher {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hl(bar.high, bar.low)
    }
}
impl Indicator for Fisher {
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
    // The trigger line repeats the PREVIOUS fisher value -> one bar later.
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_full(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    shared!("fisher", lines);
}

// ============================== ichimoku ==================================

/// `(highest high + lowest low) / 2` over the newest `p` entries of the rings: the kernel's
/// `donchian_mid` at its last index. `NaN` while fewer than `p` bars exist.
fn donchian_mid(highs: &mut VecDeque<f64>, lows: &mut VecDeque<f64>, p: usize) -> f64 {
    if p == 0 || highs.len() < p {
        return f64::NAN;
    }
    let h = highs.make_contiguous();
    let l = lows.make_contiguous();
    let hh = highest(h[h.len() - p..].iter());
    let ll = lowest(l[l.len() - p..].iter());
    (hh + ll) / 2.0
}

/// Ichimoku Cloud (registry key `ichimoku`, lines `tenkan`, `kijun`, `senkou_a`, `senkou_b`,
/// `chikou`, parameters `tenkan`, `kijun`, `senkou`): the mid of the highest high and lowest low over
/// each window, and the two cloud spans plotted `kijun` bars forward. `chikou` plots the close
/// `kijun` bars BACK, which needs bars that do not exist yet, so `on_bar` leaves it `NaN`; the batch
/// (`vectorize`, the registry's `compute`) is authoritative for it.
#[derive(Clone)]
pub struct Ichimoku {
    tenkan: f64,
    kijun: f64,
    senkou: f64,
    t: usize,
    k: usize,
    s: usize,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    span_a: Delay,
    span_b: Delay,
    last: [f64; 5],
}
impl Ichimoku {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    fn defaults() -> Self {
        Self::build(9.0, 26.0, 52.0)
    }
    fn build(tenkan: f64, kijun: f64, senkou: f64) -> Self {
        let k = kijun.round().min(crate::roll::RING_CAP as f64) as usize;
        Self {
            tenkan,
            kijun,
            senkou,
            t: tenkan.round().min(crate::roll::RING_CAP as f64) as usize,
            k,
            s: senkou.round().min(crate::roll::RING_CAP as f64) as usize,
            highs: VecDeque::new(),
            lows: VecDeque::new(),
            span_a: Delay::new(k),
            span_b: Delay::new(k),
            last: [f64::NAN; 5],
        }
    }
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(
            p.first().copied().unwrap_or(9.0),
            p.get(1).copied().unwrap_or(26.0),
            p.get(2).copied().unwrap_or(52.0),
        )
    }
}
surface!(Ichimoku, 5);
typed_new!(Ichimoku, "ichimoku", [tenkan = 9.0, kijun = 26.0, senkou = 52.0]);
impl Ichimoku {
    fn step(&mut self, bar: &Bar) -> [f64; 5] {
        let cap = self.t.max(self.k).max(self.s);
        push_capped(&mut self.highs, bar.high, cap);
        push_capped(&mut self.lows, bar.low, cap);
        let tenkan = donchian_mid(&mut self.highs, &mut self.lows, self.t);
        let kijun = donchian_mid(&mut self.highs, &mut self.lows, self.k);
        let senkou_b_raw = donchian_mid(&mut self.highs, &mut self.lows, self.s);
        let senkou_a_raw =
            if !tenkan.is_nan() && !kijun.is_nan() { (tenkan + kijun) / 2.0 } else { f64::NAN };
        let senkou_a = self.span_a.push(senkou_a_raw);
        let senkou_b = self.span_b.push(senkou_b_raw);
        // chikou[i] = close[i + kijun]: only a zero shift reaches a bar that exists at the tip.
        let chikou = if self.k == 0 { bar.close } else { f64::NAN };
        let out = [tenkan, kijun, senkou_a, senkou_b, chikou];
        self.last = out;
        out
    }
}
impl Indicator for Ichimoku {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        // `chikou` reads FUTURE bars, so the batch is authoritative and the step is a causal best effort:
        // this is the one stepper whose `vectorize` is NOT the step folded over the bars.
        batch_ichimoku(bars, self.t, self.k, self.s)
    }
    fn reset(&mut self) {
        *self = Self::build(self.tenkan, self.kijun, self.senkou);
    }
    shared!("ichimoku", lines);
}

// ========================== volume_profile_poc ============================

/// One bar of the volume profile's window: high, low, close, volume.
type ProfileBar = (f64, f64, f64, f64);

/// Rolling Point of Control (registry key `volume_profile_poc`, one line, parameters `window` and
/// `bins`): the centre price of the highest-volume bin when the last `window` bars' volume is
/// bucketed by close across their price range (the first bin wins a tie). The bar's own price when
/// the window has no range. `NaN` until `window` bars have been seen.
#[derive(Clone)]
pub struct VolumeProfilePoc {
    window_p: f64,
    bins_p: f64,
    window: usize,
    bins: usize,
    ring: VecDeque<ProfileBar>,
    /// Scratch for the histogram, rebuilt (all zero) on every bar.
    bucket: Vec<f64>,
    last: f64,
}
impl VolumeProfilePoc {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    fn defaults() -> Self {
        Self::build(50.0, 24.0)
    }
    fn build(window_p: f64, bins_p: f64) -> Self {
        Self {
            window_p,
            bins_p,
            window: window_p.round().min(crate::roll::RING_CAP as f64) as usize,
            bins: bins_p.round().min(crate::roll::RING_CAP as f64) as usize,
            ring: VecDeque::new(),
            bucket: Vec::new(),
            last: f64::NAN,
        }
    }
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(50.0), p.get(1).copied().unwrap_or(24.0))
    }
    /// `batch_volume_profile_poc` at the newest bar, over the full ring.
    fn poc(&mut self) -> f64 {
        let price_min = self.ring.iter().map(|b| b.1).fold(f64::MAX, f64::min);
        let price_max = self.ring.iter().map(|b| b.0).fold(f64::MIN, f64::max);
        if price_max == price_min {
            // Degenerate range: all prices the same; the POC is that price.
            return price_min;
        }
        let bins = self.bins;
        let bin_width = (price_max - price_min) / bins as f64;
        self.bucket.clear();
        self.bucket.resize(bins, 0.0);
        for &(_, _, c, v) in &self.ring {
            let mut idx = ((c - price_min) / bin_width) as usize;
            if idx >= bins {
                idx = bins - 1;
            }
            self.bucket[idx] += v;
        }
        let max_vol = self.bucket.iter().copied().fold(f64::MIN, f64::max);
        let poc_idx = self.bucket.iter().position(|&bv| bv == max_vol).unwrap();
        price_min + (poc_idx as f64 + 0.5) * bin_width
    }
}
surface!(VolumeProfilePoc, 1);
typed_new!(VolumeProfilePoc, "volume_profile_poc", [window = 50.0, bins = 24.0]);
impl VolumeProfilePoc {
    fn step(&mut self, bar: &Bar) -> f64 {
        let w = self.window;
        let mut out = f64::NAN;
        if w > 0 {
            self.ring.push_back((bar.high, bar.low, bar.close, bar.volume));
            if self.ring.len() > w {
                self.ring.pop_front();
            }
            if self.ring.len() == w {
                out = self.poc();
            }
        }
        self.last = out;
        self.last
    }
}
impl Indicator for VolumeProfilePoc {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn reset(&mut self) {
        *self = Self::build(self.window_p, self.bins_p);
    }
    fn lookback(&self) -> usize {
        pback(self.window_p, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    shared!("volume_profile_poc", scalar);
}

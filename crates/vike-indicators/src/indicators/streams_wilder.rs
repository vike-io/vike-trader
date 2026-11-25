//! `adx`, `adxr`, `natr`, `supertrend`, `smma` and `efi`, streamed from bounded state.
//!
//! These six used to be `hist_indicator!` types: every bar re-ran the whole batch kernel over a
//! retained tail of history (measured per bar at 20,000 bars: `adx` 51 us, `adxr` 84 us, `natr`
//! ~13 us, `supertrend` ~12 us, `smma` ~11 us, `efi` ~11 us), and the tail was a truncation of a
//! recurrence that never fully forgets. Every one of them is a Wilder or EMA recurrence, whose exact
//! state is a handful of numbers, so a bar here costs O(1) and nothing is truncated at all.
//!
//! `adx` (`crate::fast_c::AdxCore`) and `adxr`, `natr`, `supertrend` and `smma` (`crate::fast_g`) run
//! small cores whose `push` is the one implementation of the arithmetic: the stream, `vectorize` and
//! the whole-series call (`series::*`) all run it, so they agree bit for bit (held by `tests/parity`,
//! `tests/streamers/streamers_wilder_exact.rs` and `tests/fast/fast_c.rs` / `tests/fast/fast_g.rs`). The cores multiply
//! by a precomputed `1/n` where the textbook divides, so a value differs from the textbook form by
//! rounding noise only. `efi` runs `crate::fast_k::EfiCore`, a plain EMA (the seed divides by `n`).
//!
//! # Two details that are easy to get wrong
//!
// Net-new Rust, no Python twin: a naive left fold in the exact order the batch kernel folds, held bit for bit by `on_bar == vectorize` (tests/streamers/streamers_*_exact.rs, tests/all_indicators_exact.rs).
//! * **A seed is a left fold that starts at `-0.0`.** The kernels seed with `iter().sum::<f64>()`,
//!   and the standard library's float `Sum` starts at `-0.0`, not `+0.0`. The two differ when EVERY
//!   term is `-0.0` (a zero-volume stretch of falling closes makes `efi`'s force exactly that), so
//!   the seeds below begin at `-0.0`, not at `+0.0`.
//! * **A kernel that smooths "the defined tail" only sees the defined values.** `efi` compacts
//!   its `NaN` raw values away before the EMA, so a `NaN` force here is skipped, not pushed.

use super::macros::typed_new;
use super::{pback, pbars};
use crate::Indicator;
use crate::fast_g::{
    AdxrCore, NatrCore, PreviousBandsCore, SmmaCore, SupertrendCore, period, typed_new_inline,
};
use crate::fast_k::EfiCore;
use vike_marketdata::Bar;

/// `Default`, and the user-facing `next` for a one-line type.
macro_rules! scalar_surface {
    ($Name:ident) => {
        impl Default for $Name {
            #[inline]
            fn default() -> Self {
                Self::defaults()
            }
        }
        impl $Name {
            /// Feed the next bar and get this bar's value: `NaN` while warming up. This IS
            /// [`crate::Indicator::on_bar`], bit for bit, without the `Vec`.
            #[inline]
            pub fn next(&mut self, bar: &Bar) -> f64 {
                self.step(bar)
            }
        }
    };
}

/// The impl items every type here shares: `value` (`scalar` for an `f64` `last`, `lines` for an
/// `[f64; N]`), `name`, and the exact-warm-up flag (every replaced block used a `lookback =` clause,
/// which is arms 2 and 3 of `hist_indicator!`).
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

// ================================== adx ====================================

/// Average directional index (registry key `adx`, parameter `period`, three lines): the trend-strength
/// line `adx` plus the directional indicators `plus_di` and `minus_di`, in that order. The DI lines
/// appear at bar `period`; `adx`, a second Wilder smoothing of the directional movement index, appears
/// at bar `2 * period - 1`. Bars before a line's start hold `NaN`. Options, off by default: a separate
/// ADX smoothing period ([`Adx::with_adx_period`]), the first bar's moves in the DM seeds
/// ([`Adx::with_first_bar_moves`]), the form whose sums start at 0 ([`Adx::with_zero_seeded_sums`]),
/// DI lines held where the range has nothing ([`Adx::with_held_lines`]) or missing there and a DX
/// missing where they sum to 0 ([`Adx::with_missing_ratios`]), the first bar's range in every seed
/// ([`Adx::with_first_bar_range`]) and an ADX averaged from the first bar
/// ([`Adx::with_dx_from_first_bar`]).
#[derive(Clone)]
pub struct Adx {
    period: f64,
    /// The ADX smoothing period: `period` unless [`Adx::with_adx_period`] set it apart.
    adx_period: f64,
    core: crate::fast_c::AdxCore,
    /// `zero_seeded_sums` only: its core, which then answers instead of `core`.
    zero: Option<crate::fast_c::ZeroSeededAdxCore>,
    last: [f64; 3],
}
impl Adx {
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self::build2(period, period)
    }
    #[inline]
    fn build2(period: f64, adx_period: f64) -> Self {
        let bars = |p: f64| p.round().min(crate::roll::RING_CAP as f64) as usize;
        Self {
            period,
            adx_period,
            core: crate::fast_c::AdxCore::with_adx_period(bars(period), bars(adx_period)),
            zero: None,
            last: [f64::NAN; 3],
        }
    }
    /// A fresh `Adx` of these periods with this one's options.
    #[inline]
    fn rebuilt(&self, adx_period: f64) -> Self {
        let (first_bar_range, dx_from_first_bar) = self.core.start();
        Self::build2(self.period, adx_period)
            .with_first_bar_moves(self.core.first_moves)
            .with_zero_seeded_sums(self.zero.is_some())
            .with_held_lines(self.core.hold)
            .with_missing_ratios(self.core.missing)
            .with_first_bar_range(first_bar_range)
            .with_dx_from_first_bar(dx_from_first_bar)
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Option `adx_period`, `period` by default: the ADX line smooths the directional movement index
    /// over `adx_period` values (their mean, then Wilder's smoothing) while `plus_di` and `minus_di`
    /// keep `period`; `adx` then appears at bar `period + adx_period - 1`. Refused outside `period`'s
    /// registry range `2..=100`, or `NaN`, with a [`crate::IndicatorParamError`] naming `adx_period`. Unset,
    /// every value is the registry row's.
    #[inline]
    pub fn with_adx_period(self, adx_period: f64) -> Result<Self, crate::IndicatorParamError> {
        crate::typed::checked_like("adx", 0, "adx_period", adx_period)?;
        Ok(self.rebuilt(adx_period))
    }
    /// Option `first_bar_moves`, `false` by default: the first bar's directional moves count, as 0
    /// (there is no bar before it to beat), while it still has no true range. So the `+DM` and `-DM`
    /// sums are seeded with bars `0..period` and decay once at bar `period`, where the range's sum
    /// is seeded with bars `1..=period` (Wilder's means of the moves and of the range, one starting a
    /// bar before the other). The lines start at the same bars as unset. Under
    /// [`Adx::with_zero_seeded_sums`] or [`Adx::with_first_bar_range`] it reads nothing.
    #[inline]
    pub fn with_first_bar_moves(mut self, first_bar_moves: bool) -> Self {
        self.core.first_moves = first_bar_moves;
        self
    }
    /// Option `held_lines`, `false` by default: a bar whose range sum is not positive (`0` or `NaN`)
    /// holds the DI lines the last bar left, where unset they read `0`. Before the first there are
    /// none: the lines are `NaN`, and so is that bar's DX, which the ADX does not count (it waits a
    /// bar longer for its `adx_period` values). Under [`Adx::with_zero_seeded_sums`] it reads nothing.
    #[inline]
    pub fn with_held_lines(mut self, held_lines: bool) -> Self {
        self.core.hold = held_lines;
        // `dx_from_first_bar` gives way to it, so the counters are derived again.
        self.core.clear();
        self
    }
    /// Option `missing_ratios`, `false` by default: a ratio with nothing to divide by is missing
    /// (`0 / 0`), where unset it reads 0: the DI lines of a bar whose range sum is not positive, and
    /// the DX of a bar whose DI lines sum to 0. The ADX does not count a missing DX (it waits a bar
    /// longer for its `adx_period` values). Under [`Adx::with_zero_seeded_sums`] or
    /// [`Adx::with_held_lines`] it reads nothing.
    #[inline]
    pub fn with_missing_ratios(mut self, missing_ratios: bool) -> Self {
        self.core.missing = missing_ratios;
        // `dx_from_first_bar` gives way to it, so the counters are derived again.
        self.core.clear();
        self
    }
    /// Option `first_bar_range`, `false` by default: the first bar counts as any other, its moves 0
    /// (there is no bar before it to beat) and its true range `high - low`, so the three sums are
    /// seeded with bars `0..period` and every line starts a bar earlier (the DI lines at bar
    /// `period - 1`). Under [`Adx::with_zero_seeded_sums`] it reads nothing.
    #[inline]
    pub fn with_first_bar_range(mut self, first_bar_range: bool) -> Self {
        let (_, dx_from_first_bar) = self.core.start();
        self.core.set_start(first_bar_range, dx_from_first_bar);
        self
    }
    /// Option `dx_from_first_bar`, `false` by default: each bar before the DI lines has a DX of 0,
    /// so the ADX averages from the first bar: the mean of the first `adx_period` DX values at bar
    /// `adx_period - 1` (0 while they all come before the DI lines), then Wilder's smoothing. The DI
    /// lines keep their start. Under [`Adx::with_zero_seeded_sums`], [`Adx::with_held_lines`] or
    /// [`Adx::with_missing_ratios`] it reads nothing.
    #[inline]
    pub fn with_dx_from_first_bar(mut self, dx_from_first_bar: bool) -> Self {
        let (first_bar_range, _) = self.core.start();
        self.core.set_start(first_bar_range, dx_from_first_bar);
        self
    }
    /// Option `zero_seeded_sums`, `false` by default: the form whose sums start at 0. The bar before
    /// the first reads as 0 (its high, low and close), so the first bar has moves and a true range;
    /// the range, `+DM` and `-DM` sums run `s - s / period + x` from 0 from that bar; the DI lines are
    /// `sum / range_sum * 100` from the first bar, the DX `|+DI - -DI| / (+DI + -DI) * 100`, and the
    /// ADX the PLAIN mean of the last `adx_period` DX values (a `NaN` DX skipped), from bar
    /// `adx_period - 1`. Unset (the registry row's behaviour), the sums are seeded with bars
    /// `1..=period` and the ADX is Wilder's smoothing of the DX.
    #[inline]
    pub fn with_zero_seeded_sums(mut self, zero_seeded_sums: bool) -> Self {
        self.zero = zero_seeded_sums.then(|| {
            crate::fast_c::ZeroSeededAdxCore::new(period(self.period), period(self.adx_period))
        });
        self
    }
    /// Feed the next bar and get `[adx, plus_di, minus_di]` (the registry row's `outputs`): `NaN`
    /// while warming up. This IS [`crate::Indicator::on_bar`], bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 3] {
        self.step(bar)
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Adx::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 3] {
        self.last = match &mut self.zero {
            Some(zero) => zero.push(high, low, close),
            None => self.core.push(high, low, close),
        };
        self.last
    }
}
impl Default for Adx {
    #[inline]
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Adx, "adx", [period = 14.0]);
impl Adx {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 3] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for Adx {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        if self.zero.is_some() {
            return crate::roll::batch_array::<Self, 3>(self, bars, Self::step);
        }
        crate::fast_c::adx_bars(&self.core, bars)
    }
    fn reset(&mut self) {
        *self = self.rebuilt(self.adx_period);
    }
    // +DI/-DI land at `period`; the ADX line itself (output line 0) is a second
    // Wilder smoothing of DX and only lands at `period + adx_period - 1` (`2*period - 1` unset).
    // Under `zero_seeded_sums` the DI lines start on the first bar and the ADX at `adx_period - 1`;
    // `first_bar_range` starts both a bar earlier; `dx_from_first_bar` the ADX at `adx_period - 1`.
    fn lookback(&self) -> usize {
        self.starts().0.min(self.starts().1)
    }
    fn lookback_full(&self) -> usize {
        self.starts().0.max(self.starts().1)
    }
    shared!("adx", lines);
}
impl Adx {
    /// The first bars of the DI lines and of the ADX.
    fn starts(&self) -> (usize, usize) {
        let m = pbars(self.adx_period).max(1);
        if self.zero.is_some() {
            return (0, m - 1);
        }
        let di = pbars(self.period).saturating_sub(usize::from(self.core.start().0));
        (di, if self.core.lead() { m - 1 } else { di + m - 1 })
    }
}

// ================================== adxr ===================================

/// ADX rating (registry key `adxr`, parameter `period`, one line): the average of the `adx` line and
/// the `adx` value `period` bars earlier, `NaN` until both exist (bar `3 * period - 1`). This crate's
/// definition looks back `period` bars, not TA-Lib's `period - 1`.
#[derive(Clone)]
pub struct Adxr {
    period: f64,
    core: AdxrCore,
    last: f64,
}
impl Adxr {
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period_raw: f64) -> Self {
        Self { period: period_raw, core: AdxrCore::new(period(period_raw)), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Adxr::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
scalar_surface!(Adxr);
typed_new_inline!(Adxr, "adxr", [period = 14.0]);
impl Adxr {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for Adxr {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn lookback(&self) -> usize {
        (3 * pbars(self.period)).saturating_sub(1)
    }
    shared!("adxr", scalar);
}

// ================================== natr ===================================

/// Normalized ATR (registry key `natr`, parameter `period`, one line): Wilder's average true range as a
/// percentage of the close, `100 * atr / close`. `NaN` until bar `period`, and on a bar that closes at
/// exactly zero.
#[derive(Clone)]
pub struct Natr {
    period: f64,
    core: NatrCore,
    last: f64,
}
impl Natr {
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period_raw: f64) -> Self {
        Self { period: period_raw, core: NatrCore::new(period(period_raw)), last: f64::NAN }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Natr::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = self.core.push(high, low, close);
        self.last
    }
}
scalar_surface!(Natr);
typed_new_inline!(Natr, "natr", [period = 14.0]);
impl Natr {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for Natr {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn reset(&mut self) {
        *self = Self::build(self.period);
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    shared!("natr", scalar);
}

// =============================== supertrend ================================

/// Supertrend (registry key `supertrend`, parameters `period` and `mult`, two lines): an ATR-band trend
/// follower. `supertrend` is the active band (the lower one while the trend is up, the upper one while
/// it is down) and `direction` is `1.0` for up and `-1.0` for down. Both start at bar `period`.
/// Options, off by default: the trend judged against the previous bar's bands
/// ([`Supertrend::with_previous_bands`], and under it [`Supertrend::with_gated_turns`] and
/// [`Supertrend::with_carry_on_close`]) and the start
/// that reads down ([`Supertrend::with_down_start`], and under it [`Supertrend::with_held_direction`]),
/// and the bands carried on the trend ([`Supertrend::with_carry_on_trend`]).
/// [`Supertrend::next_centred`] runs the same law on a price, a centre line and an ATR of the caller's,
/// and [`Supertrend::next_centred_with_mult`] with a multiplier of the caller's on each bar.
#[derive(Clone)]
pub struct Supertrend {
    period: f64,
    mult: f64,
    core: SupertrendCore<true>,
    /// `with_previous_bands` only: its core, which then answers instead of `core`.
    previous: Option<PreviousBandsCore>,
    /// `with_down_start` only: its core, which then answers instead of `core`.
    down: Option<SupertrendCore<false>>,
    /// `with_held_direction`, `with_gated_turns`, `with_carry_on_close` and `with_carry_on_trend`,
    /// kept here so they hold whichever call comes first.
    held: bool,
    gated: bool,
    on_close: bool,
    on_trend: bool,
    last: [f64; 2],
}
impl Supertrend {
    #[inline]
    fn defaults() -> Self {
        Self::build(10.0, 3.0)
    }
    #[inline]
    fn build(period_raw: f64, mult: f64) -> Self {
        Self {
            period: period_raw,
            mult,
            core: SupertrendCore::new(period(period_raw), mult),
            previous: None,
            down: None,
            held: false,
            gated: false,
            on_close: false,
            on_trend: false,
            last: [f64::NAN; 2],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(10.0), p.get(1).copied().unwrap_or(3.0))
    }
    /// Option `previous_bands`, `false` by default: the trend judged against the PREVIOUS bar's
    /// bands. The ATR is the `atr` row's (the first bar's range `high - low`), so the line starts at
    /// bar `period - 1`; the direction is up (`1.0`) from the first bar, before any band. Each band is
    /// carried from the last bar's while the last close was STRICTLY beyond it (the lower band
    /// `max(band, last)` after a close over the last lower band, the upper `min(band, last)` after one
    /// under the last upper band; a band the last bar lacked carries nothing), and the trend turns up when the
    /// close is over the last bar's upper band, else down when it is under the last lower band, else
    /// holds. Unset (the registry row's behaviour), the first bar with an ATR seeds the bands and the
    /// direction, and the close is judged against this bar's carried bands.
    #[inline]
    pub fn with_previous_bands(mut self, previous_bands: bool) -> Self {
        self.previous = previous_bands.then(|| {
            let mut core = PreviousBandsCore::new(period(self.period), self.mult);
            core.gated = self.gated;
            core.on_close = self.on_close;
            core.on_trend = self.on_trend;
            core
        });
        self
    }
    /// Option `gated_turns`, `false` by default, read under [`Supertrend::with_previous_bands`]
    /// only: each turn is judged against the band the trend would cross, and a band the last bar
    /// lacked reads as this bar's. An up trend turns down on a close under the last lower band, a down
    /// trend up on one over the last upper band (so a close beyond both, after the bands crossed,
    /// turns the trend the way it leaves); on the first bar with an ATR the close is judged against
    /// that bar's own bands. The bands carry as unset. Unset, the close over the last upper band turns
    /// the trend up whatever it was, else under the last lower band down, and a band the last bar
    /// lacked turns nothing.
    #[inline]
    pub fn with_gated_turns(mut self, gated_turns: bool) -> Self {
        self.gated = gated_turns;
        if let Some(previous) = &mut self.previous {
            previous.gated = gated_turns;
        }
        self
    }
    /// Option `carry_on_close`, `false` by default, read under [`Supertrend::with_previous_bands`]
    /// only: each band is carried while THIS bar's close (the price, under
    /// [`Supertrend::next_centred`]) is strictly beyond the last band, not while the last bar's was.
    /// With [`Supertrend::with_gated_turns`] and a moving average as both the price and the centre,
    /// this is the profit maximizer's stop line, and with a band of a percent of that average, the
    /// trend tracker's. Unset, the last bar's close decides the carry.
    #[inline]
    pub fn with_carry_on_close(mut self, carry_on_close: bool) -> Self {
        self.on_close = carry_on_close;
        if let Some(previous) = &mut self.previous {
            previous.on_close = carry_on_close;
        }
        self
    }
    /// Option `down_start`, `false` by default: the start that reads down. The ATR is the `atr`
    /// row's (the first bar's range `high - low`), so the line starts at bar `period - 1`; the
    /// direction is down (`-1.0`) from the first bar, before any band, and on the first bar with an
    /// ATR whatever its close. That bar carries its bands from 0 (the bands the bar before lacked)
    /// by the rules every later bar follows, a missing last close breaking neither: so the upper band
    /// is 0 there unless it is under 0 or the last close was over 0, the lower band the mirror (on
    /// prices both are the bar's own). From the next bar on, as unset, except that the trend reads
    /// as down after a bar whose two bands met (its line was the upper band; under
    /// [`Supertrend::with_held_direction`] it holds instead). Unset (the registry
    /// row's behaviour), the line starts at bar `period` with the first range against a last
    /// close, and that bar's direction is up when the close is at or over its midpoint. Under
    /// [`Supertrend::with_previous_bands`] it reads nothing.
    #[inline]
    pub fn with_down_start(mut self, down_start: bool) -> Self {
        self.down = down_start.then(|| {
            let mut core = SupertrendCore::new(period(self.period), self.mult);
            core.held = self.held;
            core.on_trend = self.on_trend;
            core
        });
        self
    }
    /// Option `held_direction`, `false` by default, read under [`Supertrend::with_down_start`]
    /// only: the trend is always the one the last bar left, so after a bar whose two bands met it
    /// holds as that bar's direction said. Unset, such a bar's line counts as its upper band and the
    /// trend after it reads as down. Bands meet only when the ATR is 0 or a carried band reaches the
    /// other, so on prices that move the two agree.
    #[inline]
    pub fn with_held_direction(mut self, held_direction: bool) -> Self {
        self.held = held_direction;
        if let Some(down) = &mut self.down {
            down.held = held_direction;
        }
        self
    }
    /// Option `carry_on_trend`, `false` by default: each band is carried only while the trend is on
    /// its side, the lower band (`max(band, last)`) while it is up and the upper (`min(band, last)`)
    /// while it is down, whatever the close; the other band is this bar's. So the line holds its
    /// ratchet through the trend and starts afresh from this bar's band on a turn: the one-band
    /// Supertrend. Judged against this bar's bands (the registry row's law), the carry follows the
    /// last bar's trend and the trend starts up: on the first bar with an ATR only a close under its
    /// lower band turns it down (the direction is `NaN` before, as unset; under
    /// [`Supertrend::with_down_start`] that option's start stays). Under
    /// [`Supertrend::with_previous_bands`] the carry follows this bar's trend, after its turn, a band
    /// the last bar lacked reads as this bar's, and [`Supertrend::with_gated_turns`] and
    /// [`Supertrend::with_carry_on_close`] read nothing. Unset, a band is carried while the (last)
    /// close stays on its side of it.
    #[inline]
    pub fn with_carry_on_trend(mut self, carry_on_trend: bool) -> Self {
        self.on_trend = carry_on_trend;
        self.core.on_trend = carry_on_trend;
        if let Some(previous) = &mut self.previous {
            previous.on_trend = carry_on_trend;
        }
        if let Some(down) = &mut self.down {
            down.on_trend = carry_on_trend;
        }
        self
    }
    /// Feed the next bar and get `[supertrend, direction]` (the registry row's `outputs`): `NaN`
    /// while warming up. This IS [`crate::Indicator::on_bar`], bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 2] {
        self.step(bar)
    }
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Supertrend::next`] for a bar with those prices.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        self.last = match (&mut self.previous, &mut self.down) {
            (Some(previous), _) => previous.push(high, low, close),
            (None, Some(down)) => down.push(high, low, close),
            (None, None) => self.core.push(high, low, close),
        };
        self.last
    }
    /// The same law as [`Supertrend::next_hlc`] with the caller's numbers in place of the bar's:
    /// `price` is judged against the bands and carries them, as the close does; the bands sit
    /// `mult * atr` either side of `centre`, in place of the bar's midpoint; `atr` stands for the
    /// type's own ATR, which this call leaves where it is (so `period` reads nothing). For a band
    /// about a line that is not the midpoint (a pivot line, an average), an ATR of another average,
    /// or a price other than the close. Feed one type through this or through `next`, not both.
    #[inline]
    pub fn next_centred(&mut self, price: f64, centre: f64, atr: f64) -> [f64; 2] {
        self.last = match (&mut self.previous, &mut self.down) {
            (Some(previous), _) => previous.push_centred(price, centre, atr),
            (None, Some(down)) => down.push_centred(price, centre, atr),
            (None, None) => self.core.push_centred(price, centre, atr),
        };
        self.last
    }
    /// [`Supertrend::next_centred`] with this bar's own multiplier: the bands sit `mult * atr` either
    /// side of `centre`, in place of the type's `mult`, for a band whose width moves bar to bar (a
    /// multiplier that follows a regime or a channel's width). With the type's `mult` on every bar it
    /// IS `next_centred`, bit for bit. Feed one type through this or through `next_centred`, not
    /// both; `reset` brings back the type's `mult`.
    #[inline]
    pub fn next_centred_with_mult(
        &mut self,
        price: f64,
        centre: f64,
        atr: f64,
        mult: f64,
    ) -> [f64; 2] {
        match (&mut self.previous, &mut self.down) {
            (Some(previous), _) => previous.mult = mult,
            (None, Some(down)) => down.mult = mult,
            (None, None) => self.core.mult = mult,
        }
        self.next_centred(price, centre, atr)
    }
}
impl Default for Supertrend {
    #[inline]
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new_inline!(Supertrend, "supertrend", [period = 10.0, mult = 3.0]);
impl Supertrend {
    #[inline]
    fn step(&mut self, bar: &Bar) -> [f64; 2] {
        self.next_hlc(bar.high, bar.low, bar.close)
    }
}
impl Indicator for Supertrend {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.step(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 2>(self, bars, Self::step)
    }
    fn reset(&mut self) {
        let (previous, down) = (self.previous.is_some(), self.down.is_some());
        let fresh = Self::build(self.period, self.mult)
            .with_gated_turns(self.gated)
            .with_carry_on_close(self.on_close)
            .with_held_direction(self.held)
            .with_carry_on_trend(self.on_trend);
        *self = fresh.with_previous_bands(previous).with_down_start(down);
    }
    fn lookback(&self) -> usize {
        // Under `previous_bands` or `down_start` the ATR (and so the line) starts a bar earlier.
        let early = self.previous.is_some() || self.down.is_some();
        pbars(self.period).saturating_sub(usize::from(early))
    }
    shared!("supertrend", lines);
}

// ================================== smma ===================================

/// Wilder / RMA smoothed moving average of the close (registry key `smma`, parameter `period`, one
/// line): seeded with the simple average of the first `period` closes (bar `period - 1`), then
/// `(previous * (period - 1) + close) / period`. Options, all off by default: [`Smma::with_skip_nan`],
/// [`Smma::with_seed_first`], [`Smma::with_alpha_steps`] and [`Smma::with_zero_seed`].
/// [`Smma::next_value_with_period`] takes the period with each value.
#[derive(Clone)]
pub struct Smma {
    period: f64,
    core: SmmaCore,
    skip_nan: bool,
    seed_first: bool,
    last: f64,
}
impl Smma {
    #[inline]
    fn defaults() -> Self {
        Self::build(14.0)
    }
    #[inline]
    fn build(period_raw: f64) -> Self {
        Self {
            period: period_raw,
            core: SmmaCore::new(period(period_raw)),
            skip_nan: false,
            seed_first: false,
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(14.0))
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
    /// output, so there is no warm-up; `(previous * (period - 1) + x) / period` runs from the second value
    /// on. Unset (the registry row's behaviour), the first `period - 1` values are `NaN` and the seed
    /// is the mean of the first `period`.
    #[inline]
    pub fn with_seed_first(mut self, seed_first: bool) -> Self {
        self.seed_first = seed_first;
        self
    }
    /// Option `alpha_steps`, `false` by default: the other floating-point form of the same average,
    /// the seed's sum divided by `period` and each step `alpha * x + (1 - alpha) * previous` with
    /// `alpha = 1 / period`. Equal on paper to the unset form (the sum times `1 / period`, each step
    /// `previous * (period - 1) / period + x / period`), a few ulps apart in f64, which can flip a
    /// comparison of two nearly equal values.
    #[inline]
    pub fn with_alpha_steps(mut self, alpha_steps: bool) -> Self {
        self.core.alpha = alpha_steps;
        self
    }
    /// Option `zero_seed`, `false` by default: the average starts from 0, as if a 0 came before the
    /// first value, so there is no warm-up and no seed: every value is a step from the last (the
    /// form's own, `previous * (period - 1) / period + x / period`, or `alpha * x + (1 - alpha) * previous`
    /// under [`Smma::with_alpha_steps`]). A `NaN` value reads `NaN` and the next starts from 0 again
    /// (with [`Smma::with_skip_nan`] it moves nothing instead). Under it [`Smma::with_seed_first`]
    /// reads nothing.
    #[inline]
    pub fn with_zero_seed(mut self, zero_seed: bool) -> Self {
        self.core.zero = zero_seed;
        self
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Smma::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = if !(self.skip_nan | self.seed_first) {
            self.core.push(close)
        } else if self.skip_nan && close.is_nan() {
            f64::NAN
        } else if self.seed_first {
            self.core.push_from_first(close)
        } else {
            self.core.push(close)
        };
        self.last
    }
    /// [`Smma::next_value`] with this value's own `period` in place of the type's, for an average
    /// whose length moves bar to bar: rounded, and held within 1 and the type's `period`, which is
    /// also how many values are kept for the seed. The seed is the plain mean of the LAST `period`
    /// values, on the first value that has that many; each step after it weighs by this value's
    /// `period`, in the type's form ([`Smma::with_alpha_steps`] or not). The other options read as
    /// in `next_value`. With the type's `period` on every value it IS `next_value`, bit for bit.
    /// Feed one type through this or through `next_value`, not both.
    #[inline]
    pub fn next_value_with_period(&mut self, value: f64, period_raw: f64) -> f64 {
        let n = period(period_raw).clamp(1, period(self.period).max(1));
        self.last = if self.skip_nan && value.is_nan() {
            f64::NAN
        } else {
            self.core.push_period(value, n, self.seed_first)
        };
        self.last
    }
}
scalar_surface!(Smma);
typed_new_inline!(Smma, "smma", [period = 14.0]);
impl Smma {
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Indicator for Smma {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn reset(&mut self) {
        let (alpha, zero) = (self.core.alpha, self.core.zero);
        *self = Self {
            skip_nan: self.skip_nan,
            seed_first: self.seed_first,
            ..Self::build(self.period)
        };
        (self.core.alpha, self.core.zero) = (alpha, zero);
    }
    fn lookback(&self) -> usize {
        if self.seed_first || self.core.zero { 0 } else { pback(self.period, 1) }
    }
    shared!("smma", scalar);
}

// ================================== efi ====================================

/// Elder Force Index (registry key `efi`, parameter `period`, one line): the EMA of
/// `(close - previous close) * volume`. The first bar has no previous close, so the line starts at bar
/// `period`.
///
/// Runs `crate::fast_k::EfiCore`, the one `push` the stream, `vectorize` and `series::efi` share.
#[derive(Clone)]
pub struct Efi {
    period: f64,
    core: EfiCore,
    last: f64,
}
impl Efi {
    #[inline]
    fn defaults() -> Self {
        Self::build(13.0)
    }
    #[inline]
    fn build(period: f64) -> Self {
        Self {
            period,
            core: EfiCore::new(period.round().min(crate::roll::RING_CAP as f64) as usize),
            last: f64::NAN,
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(13.0))
    }
    /// Feed the next bar and get this bar's value: `NaN` while warming up. This IS
    /// [`crate::Indicator::on_bar`], bit for bit, without the `Vec`.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next bar's `close` and `volume` as plain numbers, no [`Bar`] needed; the same answer
    /// as [`Efi::next`] for a bar with those values.
    #[inline]
    pub fn next_cv(&mut self, close: f64, volume: f64) -> f64 {
        self.last = self.core.push(close, volume);
        self.last
    }
}
impl Default for Efi {
    #[inline]
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(Efi, "efi", [period = 13.0]);
impl Efi {
    // A `NaN` force (bar 0, or a `NaN` price or volume) is neither pushed nor emitted, as
    // `smooth_defined` smoothed only the non-`NaN` raw values and scattered them back.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_cv(bar.close, bar.volume)
    }
}
impl Indicator for Efi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        vec![self.step(bar)]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_scalar(self, bars, Self::step)
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = f64::NAN;
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    shared!("efi", scalar);
}

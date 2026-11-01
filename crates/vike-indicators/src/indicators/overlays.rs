//! Overlays added after the first release: adaptive and elastic moving averages and trailing stops.
//! Kernels and types live here, their registry rows in `registry/overlays.rs`.
//!
//! The infinite-memory recurrences (`kama`, `vidya`, `frama`, `evwma`, `hilo_activator`,
//! `atr_trailing_stop`) carry a value or a trend forever: the number at bar `i` depends on where the
//! series started, and every one reports `warmup_path_dependent`. Each is a hand-written stepper on
//! a small core (`crate::fast_d2::KamaCore` for `kama`, `crate::fast_e` for the rest) whose `push`
//! is the one implementation of the arithmetic: `next`, `next_*`, `vectorize` (the stream folded
//! over the bars) and the `series::*` calls all run it, so they agree bit for bit, and a bar costs
//! a few nanoseconds for a fixed period.
#![allow(clippy::needless_range_loop)]

use super::{pback, pbars};
use crate::Indicator;
use crate::fast_d2::KamaCore;
use crate::fast_e::{AtrStopCore, EvwmaCore, FramaCore, HiloCore, VidyaCore, scalar_indicator};
use vike_marketdata::Bar;

// ---- kama ------------------------------------------------------------------

/// Kaufman's adaptive moving average (`kama`): an average that speeds up in a trend and slows
/// down in noise. Parameters `period` (efficiency-ratio window, default 10), `fast` (default 2)
/// and `slow` (default 30); one output line, `NaN` for the first `period` bars.
///
/// Formula (TA-Lib's `KAMA`, with `fast` and `slow` free instead of fixed at 2 and 30). Over the
/// last `period + 1` closes, the efficiency ratio is `ER = |net move| / (sum of the absolute
/// bar-to-bar moves)`, and 1 when the net move equals the whole path (a straight run) or when
/// the window is flat. The smoothing constant is
/// `sc = (ER * (2/(fast+1) - 2/(slow+1)) + 2/(slow+1))^2` and
/// `KAMA = prev + sc * (close - prev)`. The recurrence is seeded with the close at index
/// `period - 1`, so the first value is at index `period`.
///
/// O(1) a bar (`crate::fast_d2::KamaCore`): the path is a running sum, the move that enters added
/// and the move that leaves subtracted, rebuilt from the window every `ceil(512 / (period + 1))`
/// laps, and a count of the non-zero moves keeps a flat window exactly flat. The recurrence is
/// written `prev * (1 - sc) + close * sc`. Differences from TA-Lib in the last bits only, and from
/// a fresh fold of the window by the rounding noise every running-sum path has.
///
/// ⚠ Path dependent: the value carries every bar since the series started (the smoothing
/// constant can be as small as `(2/(slow+1))^2`), so a caller must fold from the first bar. A
/// `NaN` close poisons it for good.
#[derive(Clone)]
pub struct Kama {
    period: f64,
    core: KamaCore,
    last: f64,
}
impl Kama {
    /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
    #[inline]
    fn defaults() -> Self {
        Self::build(10.0, 2.0, 30.0)
    }
    #[inline]
    fn build(period: f64, fast: f64, slow: f64) -> Self {
        Self {
            period,
            core: KamaCore::new(pbars(period).max(1), pbars(fast), pbars(slow)),
            last: f64::NAN,
        }
    }
    /// Build with the efficiency-ratio window `period` (registry range `2..=200`) and the `fast`
    /// (`1..=100`) and `slow` (`2..=400`) spans. A value the registry would not accept (out of
    /// range, or `NaN`) is a [`crate::ParamError`] naming it; [`crate::make_with`] clamps instead.
    /// `Default` gives the registry defaults (10, 2, 30).
    #[inline]
    pub fn new(period: f64, fast: f64, slow: f64) -> Result<Self, crate::ParamError> {
        let raw: &[f64] = &[period, fast, slow];
        crate::typed::checked("kama", raw)?;
        Ok(Self::with_params(raw))
    }
    /// Parametric constructor — `p` in registry order; a missing entry falls back to the default.
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(
            p.first().copied().unwrap_or(10.0),
            p.get(1).copied().unwrap_or(2.0),
            p.get(2).copied().unwrap_or(30.0),
        )
    }
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> f64 {
        self.step(bar)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; the same answer as [`Kama::next`]
    /// for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> f64 {
        self.last = self.core.push(close);
        self.last
    }
    /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it, so there is one body.
    #[inline]
    fn step(&mut self, bar: &Bar) -> f64 {
        self.next_value(bar.close)
    }
}
impl Default for Kama {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Indicator for Kama {
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
        "kama"
    }
    fn lookback(&self) -> usize {
        pbars(self.period)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    fn warmup_path_dependent(&self) -> bool {
        true
    }
}

// ---- vidya -----------------------------------------------------------------

scalar_indicator! {
    /// Chande's variable index dynamic average (`vidya`): an EMA whose smoothing speeds up when the
    /// market is directional. Parameters `period` (the EMA length, default 14) and `cmo_period` (the
    /// window of the volatility index, default 9); one output line, `NaN` for the first
    /// `cmo_period` bars.
    ///
    /// Formula. The volatility index is the absolute Chande momentum oscillator over the last
    /// `cmo_period` close-to-close changes, as a fraction: `vi = |Σup - Σdown| / (Σup + Σdown)`
    /// (0 when nothing moved), where `Σup` and `Σdown` are the sums of the rises and of the
    /// magnitudes of the falls. With `alpha = 2 / (period + 1)`:
    /// `VIDYA = alpha*vi*close + (1 - alpha*vi) * prev`. The recurrence is seeded with the close
    /// of the bar before the first output, so the first value is at index `cmo_period`.
    ///
    /// This is the CMO form of VIDYA (the one most platforms draw); Chande's original
    /// standard-deviation-ratio index is a different indicator.
    ///
    /// O(1) a bar (`crate::fast_e::VidyaCore`): the two sums are running sums over a ring of the
    /// changes, rebuilt from the window every `ceil(512 / cmo_period)` laps and whenever their
    /// total has fallen to a hundred thousandth of the largest it has been since, so the last bits
    /// can differ from a fresh fold of the window. A window with no movement is exactly 0.
    ///
    /// ⚠ Path dependent: the value carries every bar since the series started, and a `NaN` close
    /// poisons it for good.
    Vidya, "vidya", VidyaCore, input = value,
    [period = 14.0, cmo_period = 9.0],
    build = VidyaCore::new(pbars(period), pbars(cmo_period)),
    lookback = pbars(cmo_period),
    path_dependent = true
}

// ---- frama -----------------------------------------------------------------

scalar_indicator! {
    /// Ehlers' fractal adaptive moving average (`frama`): an average whose smoothing follows the
    /// fractal dimension of recent prices, fast in a trend and slow in a range. One parameter,
    /// `period` (default 16, rounded down to an even number); one output line, `NaN` for the first
    /// `period - 1` bars.
    ///
    /// Formula. Over the last `N` bars (`N` = `period` rounded down to even, at least 2), with
    /// `h = N/2`: `N1 = (highest high - lowest low of the older half) / h`, `N2` the same for the
    /// newer half, `N3 = (highest high - lowest low of all N bars) / N`, and
    /// `D = (ln(N1 + N2) - ln(N3)) / ln 2`. Then `alpha = exp(-4.6 * (D - 1))` clamped to
    /// `[0.01, 1]`, and `FRAMA = alpha*close + (1 - alpha) * prev`, on the CLOSE (Ehlers' original
    /// source is the median price). A window with no range at all (`N3 = 0`), or whose halves have
    /// none (`N1 + N2 = 0`), takes `alpha = 1`. Seeded with the close at index `N - 1`, which is
    /// therefore the first value.
    ///
    /// O(1) a bar (`crate::fast_e::FramaCore`) apart from one logarithm and, unless the smoothing
    /// is clamped, one exponential: the half-window extremes come from one block-wise window and
    /// a delayed copy of it, and the two logarithms are one (`ln(N1 + N2) - ln(N3)` is
    /// `ln((N1 + N2) / N3)`), so the last bits can differ from the textbook form.
    ///
    /// ⚠ Path dependent: the smoothing can be as slow as `alpha = 0.01`, so the value carries every
    /// bar since the series started.
    Frama, "frama", FramaCore, input = hlc,
    [period = 16.0],
    build = FramaCore::new(pbars(period)),
    lookback = (2 * (pbars(period) / 2).max(1)) - 1,
    path_dependent = true
}

// ---- evwma -----------------------------------------------------------------

scalar_indicator! {
    /// Elastic volume-weighted moving average (`evwma`, Christian Fries): a moving average whose
    /// step toward each new close is that bar's share of the volume traded over the last `period`
    /// bars, so a heavy bar moves it a lot and a light one barely at all. One parameter, `period`
    /// (default 20); one output line, `NaN` for the first `period - 1` bars.
    ///
    /// Formula. With `V` the sum of the volumes of the last `period` bars and `v` the current
    /// bar's volume: `EVWMA = ((V - v) * prev + v * close) / V`. Seeded with the close at index
    /// `period - 1`, which is therefore the first value. When the window traded no volume (`V = 0`)
    /// the previous value is carried.
    ///
    /// O(1) a bar (`crate::fast_e::EvwmaCore`): `V` is a running sum over a ring of volumes, rebuilt
    /// like `vwma`'s, and the step is written `prev * (1 - k) + close * k` with `k = v / V`, so the
    /// last bits can differ from the formula above.
    ///
    /// ⚠ Path dependent: the value carries every bar since the series started, and a `NaN` close
    /// poisons it for good.
    Evwma, "evwma", EvwmaCore, input = cv,
    [period = 20.0],
    build = EvwmaCore::new(pbars(period)),
    lookback = pback(period, 1),
    path_dependent = true
}

// ---- hilo activator --------------------------------------------------------

scalar_indicator! {
    /// Gann HiLo Activator (`hilo_activator`): a trend-following stop line that sits under price in
    /// an uptrend and over it in a downtrend. One parameter, `period` (the moving-average length,
    /// default 3); one output line, `NaN` for the first `period - 1` bars.
    ///
    /// Formula. `hi` and `lo` are the simple moving averages of the highs and of the lows. The
    /// trend turns up when the close is above the PREVIOUS bar's `hi`, turns down when it is below
    /// the previous bar's `lo`, and otherwise stays as it was. The line is the current `lo` in an
    /// uptrend and the current `hi` in a downtrend. The trend is seeded on the first bar that has
    /// both averages: up when the close is at or above `(hi + lo) / 2`, down otherwise (the first
    /// value is therefore at index `period - 1`).
    ///
    /// The two averages are running-sum averages (`crate::fast_e::HiloCore`, on
    /// `crate::roll::SmaCore`), so a close that sits exactly ON a previous average can fall on
    /// either side of it by the last bit of the rounding.
    ///
    /// ⚠ Path dependent: the trend persists until it is broken, however long that takes, so the
    /// value depends on every bar since the series started.
    HiloActivator, "hilo_activator", HiloCore, input = hlc,
    [period = 3.0],
    build = HiloCore::new(pbars(period)),
    lookback = pback(period, 1),
    path_dependent = true
}

// ---- atr trailing stop -----------------------------------------------------

scalar_indicator! {
    /// ATR trailing stop (`atr_trailing_stop`): a stop that trails the close at `mult` times the
    /// average true range and only ever moves in the trend's direction until price crosses it.
    /// Parameters `period` (Wilder ATR length, default 14) and `mult` (default 3.0); one output
    /// line, `NaN` for the first `period - 1` bars.
    ///
    /// Formula (the widely published "ATR trailing stop" rule). With `loss = mult * ATR` and
    /// `prev` the previous stop: if the close and the previous close are both above `prev` the stop
    /// is `max(prev, close - loss)`; if both are below it the stop is `min(prev, close + loss)`;
    /// otherwise the stop flips to `close - loss` when the close is above `prev` and to
    /// `close + loss` when it is not. The first value, at index `period - 1`, is `close - loss`.
    /// The ATR is Wilder's, seeded with the mean of the first `period` true ranges.
    ///
    /// O(1) a bar (`crate::fast_e::AtrStopCore`), the ATR as two multiplies; the last bits can
    /// differ from a division by `period`.
    ///
    /// ⚠ Path dependent: the stop ratchets for as long as the trend lasts, so the value depends on
    /// every bar since the series started.
    AtrTrailingStop, "atr_trailing_stop", AtrStopCore, input = hlc,
    [period = 14.0, mult = 3.0],
    build = AtrStopCore::new(pbars(period), mult),
    lookback = pback(period, 1),
    path_dependent = true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kama() {
        crate::typed::tests::assert_matches_registry(
            "kama",
            Box::new(Kama::new(10.0, 2.0, 30.0).unwrap()),
        );
    }
}

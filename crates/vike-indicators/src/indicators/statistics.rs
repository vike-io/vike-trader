//! Statistics indicators — the single-series ports of vike-trader-app `core/indicators/statistics.py`:
//! the linear-regression family (`linearreg` / `linearreg_slope` / `linearreg_angle` /
//! `linearreg_intercept` / `tsf`), `var`, `zscore`, `skew`, `kurtosis`, `mad`, `std_error`,
//! `std_error_bands` and `rank_correlation`. The two-series ones (`beta`, `correl`, `correl_log`) are
//! handled elsewhere and are NOT here.
//!
//! These used to be `hist_indicator!` types: every bar re-ran the whole batch kernel over a retained tail
//! of history. Each now runs a core of `crate::fast_l`, a small struct of plain numbers whose `push` is
//! the ONE implementation of the arithmetic, shared by the stepper (`next`, `next_value`, `on_bar`), by
//! `vectorize` and by `crate::series`, so every way in agrees bit for bit. The cores keep running sums of
//! the window (or, for `rank_correlation`, the window sorted) and update them with the value that enters
//! and the one that leaves; wherever the sums cannot resolve the answer (a flat or nearly flat window, a
//! `NaN` in it, a bad tick) the core rebuilds them and, if they still cannot, falls back to the old
//! kernel's formulas over the window, so a flat window and a window with a `NaN` in it give what the old
//! code gave. Everywhere else a value differs from the old one by rounding noise only
//! (`tests/fast_l.rs` holds it to a relative 1e-9 of a textbook reference on three scales of series).
//!
//! Details that carry over from the batch kernels:
//!
//! * `math.degrees(math.atan(b))` is `atan(b) * (180.0 / PI)` over `libm::atan`, not `to_degrees()`
//!   (whose precomputed constant can differ by a ULP) and not `f64::atan` (IEEE 754 does not require
//!   `atan` to be correctly rounded and the two boxes disagree on its last bit).
//! * `x ** k` is `crate::math`'s `sq` / `cube` / `quart`, never `powi`.
//! * `var` of a window of identical values is exactly `0.0` and its `zscore` is `NaN` — a decision
//!   (`crate::window::is_constant_window`), not a hope that the fold lands there.
//!
//! Every type is hand-written in the `base/incremental.rs` sense (`trims_history() == false`).
//!
//! # The batch kernels this file used to hold
//!
//! Each type had a `batch_<name>` kernel here (`batch_linearreg`, `batch_linearreg_slope`,
//! `batch_linearreg_angle`, `batch_linearreg_intercept`, `batch_tsf`, `batch_var`, `batch_zscore`,
//! `batch_skew`, `batch_kurtosis`, `batch_mad`, `batch_std_error`, `batch_std_error_bands` and
//! `batch_rank_correlation`, with the helpers `ols`, `linearreg_series` and `std_error_series`) that
//! the macro re-ran over a retained tail of history. They are gone: the arithmetic they held is the
//! COLD path of the cores in `crates/vike-indicators/src/fast_l.rs` (`ols_ordered`, `var_ordered`,
//! `zscore_ordered`, `skew_ordered`, `kurtosis_ordered`, `mad_ordered`, `std_error_ordered`) and the
//! rank sum is `RankCore`'s. What `batch_linearreg_angle` and `batch_skew` / `batch_kurtosis` were cited
//! for lives on: the angle is `libm::atan(slope) * (180.0 / PI)`, and the powers are `cube` / `quart`.
//! What `batch_var` documented, which several other files cite, is kept here whole:
//!
//! ⚠ **`batch_var` carried a `run_sum` / `run_sum2` pair and closed with `run_sum2/period -
//! mean*mean`, and BOTH halves of that were bugs.**
//!
//! 1. The carried pair made the value a function of all history, not of the window: re-run over a
//!    truncated tail and the identical window returns different bits, so `var`/`zscore` were wrong
//!    past `hist_indicator!`'s drain on any long series (MEASURED 64/64 and 231/232 mismatching
//!    retained lengths at `period` 20 and 200; 0 of each after the fix).
//! 2. `E[x²] - E[x]²` is catastrophic cancellation on price data, where the mean dwarfs the
//!    spread. That is not merely imprecise — it FLIPS `batch_zscore`'s `if sd != 0.0` guard, which
//!    decides between a NUMBER and a NaN. Measured on a dead-flat window at exactly 100.0: the old
//!    form returns `var = 2.9e-11` and `z = 5.0e-8` on a full run and `var = 0.0`, `z = NaN` on a
//!    truncated one. The two-pass form returns exactly `0.0` and `NaN` either way there.
//!
//! ⚠ **"there" is load-bearing.** It was once said that the two-pass form is zero on any flat window
//! "because a window of identical values has `x - mean == 0` exactly", which is FALSE: `mean` is
//! `Σx / n`, and that only rounds back to the repeated value for some values. `100.0` is one of them,
//! which is why the sentence survived — its own test used that fixture. `0.1` is not, and a flat window
//! there returned a rounding-sized variance and a spurious `z` of order 1. The exact-zero property is a
//! DECISION rather than a hope: `crate::window::rolling_var` returns `0.0` outright for a window whose
//! values are all equal, and the cores here do the same (`fast_l.rs`'s `constant`). ⚠ Not because pandas
//! does — pandas' rolling accumulator carries history and is the very bug described above, measured;
//! `crates/vike-indicators/src/window.rs`'s `is_constant_window` carries that measurement and the
//! argument for diverging from it.
//!
//! The `.max(0.0)` clamp went with it: it existed to repair the cancellation's small NEGATIVE results,
//! and a sum of squares over a positive count cannot be negative. Keeping it would also keep a trap —
//! `f64::max` returns the non-NaN operand, so a NaN input silently became `0.0`. (The same trap sits in
//! `f64::min`: `fast_l.rs`'s r-squared compares with `> 1.0` instead of `.min(1.0)` for that reason.)

use super::macros::typed_new;
use super::pback;
use crate::Indicator;
use crate::fast_l::{
    ANGLE, BANDS, INTERCEPT, KURT, LINREG, MadCore, MomentCore, RankCore, RegCore, SKEW, SLOPE,
    STDERR, TSF, VAR, ZSCORE, stat1,
};
use vike_marketdata::Bar;

stat1! {
    /// Linear regression (`linearreg`): the least-squares line through the last `period` closes
    /// (against `0..period - 1`), read at the newest bar. One parameter, `period` (default 14); one
    /// output line, `NaN` for the first `period - 1` bars and while a `NaN` close is inside the window.
    Linearreg, "linearreg", 14.0, RegCore<LINREG>, |n| RegCore::<LINREG>::new(n, 0.0)
}
stat1! {
    /// Linear regression slope (`linearreg_slope`): the slope of the least-squares line through the last
    /// `period` closes, in price per bar. One parameter, `period` (default 14); one output line, `NaN`
    /// for the first `period - 1` bars and while a `NaN` close is inside the window.
    LinearregSlope, "linearreg_slope", 14.0, RegCore<SLOPE>, |n| RegCore::<SLOPE>::new(n, 0.0)
}
stat1! {
    /// Linear regression angle (`linearreg_angle`): the slope of the least-squares line through the
    /// last `period` closes as an angle, `atan(slope)` in degrees. One parameter, `period` (default
    /// 14); one output line, `NaN` for the first `period - 1` bars and while a `NaN` close is inside
    /// the window.
    LinearregAngle, "linearreg_angle", 14.0, RegCore<ANGLE>, |n| RegCore::<ANGLE>::new(n, 0.0)
}
stat1! {
    /// Linear regression intercept (`linearreg_intercept`): the value at the OLDEST bar of the window of
    /// the least-squares line through the last `period` closes. One parameter, `period` (default 14);
    /// one output line, `NaN` for the first `period - 1` bars and while a `NaN` close is inside the
    /// window.
    LinearregIntercept, "linearreg_intercept", 14.0, RegCore<INTERCEPT>,
    |n| RegCore::<INTERCEPT>::new(n, 0.0)
}
stat1! {
    /// Time Series Forecast (`tsf`): the least-squares line through the last `period` closes projected
    /// one bar past the newest. One parameter, `period` (default 14); one output line, `NaN` for the
    /// first `period - 1` bars and while a `NaN` close is inside the window.
    Tsf, "tsf", 14.0, RegCore<TSF>, |n| RegCore::<TSF>::new(n, 0.0)
}
stat1! {
    /// Rolling population variance (`var`) of the last `period` closes. One parameter, `period`
    /// (default 20); one output line, `NaN` for the first `period - 1` bars and while a `NaN` close is
    /// inside the window; exactly `0.0` for a window of identical closes.
    Var, "var", 20.0, MomentCore<VAR>, |n| MomentCore::<VAR>::new(n)
}
stat1! {
    /// Rolling z-score (`zscore`) of the newest close against the last `period` closes:
    /// `(close - mean) / standard deviation`, the deviation the population one. One parameter,
    /// `period` (default 20); one output line, `NaN` for the first `period - 1` bars, while a `NaN`
    /// close is inside the window, and for a window of identical closes (an undefined z-score, never an
    /// infinity).
    Zscore, "zscore", 20.0, MomentCore<ZSCORE>, |n| MomentCore::<ZSCORE>::new(n)
}
stat1! {
    /// Rolling sample skewness (`skew`, Fisher-Pearson, bias-corrected) of the last `period` closes:
    /// `m3 / m2^1.5 * sqrt(p (p - 1)) / (p - 2)`. One parameter, `period` (default 20); one output line,
    /// `NaN` for the first `period - 1` bars, while a `NaN` close is inside the window, and always for
    /// a `period` under 3; exactly `0.0` for a window of identical closes.
    Skew, "skew", 20.0, MomentCore<SKEW>, |n| MomentCore::<SKEW>::new(n)
}
stat1! {
    /// Rolling excess kurtosis (`kurtosis`, Fisher, sample-corrected) of the last `period` closes. One
    /// parameter, `period` (default 20); one output line, `NaN` for the first `period - 1` bars, while a
    /// `NaN` close is inside the window, and always for a `period` under 4; exactly `0.0` for a window
    /// of identical closes.
    Kurtosis, "kurtosis", 20.0, MomentCore<KURT>, |n| MomentCore::<KURT>::new(n)
}
stat1! {
    /// Rolling mean absolute deviation (`mad`): the mean of `|close - mean|` over the last `period`
    /// closes. One parameter, `period` (default 20); one output line, `NaN` for the first `period - 1`
    /// bars and while a `NaN` close is inside the window.
    Mad, "mad", 20.0, MadCore, |n| MadCore::new(n)
}
stat1! {
    /// Standard error of the estimate (`std_error`) of the least-squares line through the last `period`
    /// closes: `sqrt(sum(residual^2) / (period - 2)) / sqrt(period)`. One parameter, `period` (default
    /// 20); one output line, `NaN` for the first `period - 1` bars and while a `NaN` close is inside the
    /// window (for a `period` of 2 the error is `0.0`).
    StdError, "std_error", 20.0, RegCore<STDERR>, |n| RegCore::<STDERR>::new(n, 0.0)
}
stat1! {
    /// Spearman rank-correlation index (`rank_correlation`, RCI) of the last `period` closes against
    /// their place in the window, scaled to `[-100, 100]`: `100 * (1 - 6 * sum(d^2) / (p (p^2 - 1)))`
    /// with `d` the difference of the two ranks, equal closes ranked oldest first. One parameter,
    /// `period` (default 14); one output line, `NaN` for the first `period - 1` bars and while a `NaN`
    /// close is inside the window.
    RankCorrelation, "rank_correlation", 14.0, RankCore, |n| RankCore::new(n)
}

// ============================== std_error_bands ================================

/// Standard-error bands (`std_error_bands`) around the regression line: the least-squares line through
/// the last `period` closes read at the newest bar (`mid`), and `upper` / `lower` a multiple of its
/// standard error (the `std_error` indicator) above and below. Parameters `period` (default 20) and
/// `mult` (default 2.0); output lines `upper`, `mid` and `lower`, `NaN` for the first `period - 1` bars
/// and while a `NaN` close is inside the window.
#[derive(Clone)]
pub struct StdErrorBands {
    period: f64,
    core: RegCore<BANDS>,
    last: [f64; 3],
}
impl StdErrorBands {
    #[inline]
    fn defaults() -> Self {
        Self::build(20.0, 2.0)
    }
    #[inline]
    fn build(period: f64, mult: f64) -> Self {
        Self {
            period,
            core: RegCore::<BANDS>::new(crate::fast_g::period(period), mult),
            last: [f64::NAN; 3],
        }
    }
    #[inline]
    pub(crate) fn with_params(p: &[f64]) -> Self {
        Self::build(p.first().copied().unwrap_or(20.0), p.get(1).copied().unwrap_or(2.0))
    }
    /// Feed the next bar and get `[upper, mid, lower]`; this IS `Indicator::on_bar`, bit for bit,
    /// without the Vec.
    #[inline]
    pub fn next(&mut self, bar: &Bar) -> [f64; 3] {
        self.next_value(bar.close)
    }
    /// Feed the next close as a plain number, no [`Bar`] needed; `[upper, mid, lower]`, the same answer
    /// as [`StdErrorBands::next`] for a bar with that close.
    #[inline]
    pub fn next_value(&mut self, close: f64) -> [f64; 3] {
        self.last = self.core.push3(close);
        self.last
    }
}
impl Default for StdErrorBands {
    #[inline]
    fn default() -> Self {
        Self::defaults()
    }
}
typed_new!(StdErrorBands, "std_error_bands", [period = 20.0, mult = 2.0]);
impl Indicator for StdErrorBands {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        self.next(bar).to_vec()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        crate::roll::batch_array::<Self, 3>(self, bars, Self::next)
    }
    fn value(&self) -> Vec<f64> {
        self.last.to_vec()
    }
    fn reset(&mut self) {
        self.core.clear();
        self.last = [f64::NAN; 3];
    }
    fn name(&self) -> &str {
        "std_error_bands"
    }
    fn lookback(&self) -> usize {
        pback(self.period, 1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

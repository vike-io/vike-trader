//! Whole-series calls on plain `f64` slices for group `b` of the second wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`).

use crate::ParamError;

/// Runs the core over `$close` into a fresh `NaN` buffer filled first and written by index (the page
/// faults of a new buffer land in the fill, not inside the recurrence).
macro_rules! run_closes {
    ($core:expr, $close:expr) => {{
        let mut core = $core;
        let mut out = vec![f64::NAN; $close.len()];
        core.run($close, |&c| c, &mut out);
        Ok(out)
    }};
}

/// Double exponential moving average of `close`, `2 * EMA - EMA(EMA)` with both EMAs over `period`
/// values (registry range `2..=400`); `NaN` for the first `2 * period - 2` values.
#[inline]
pub fn dema(close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("dema", &[period])?;
    run_closes!(crate::fast_b::DemaCore::new(period.round() as usize), close)
}

/// Triple exponential moving average of `close`, `3 * EMA - 3 * EMA(EMA) + EMA(EMA(EMA))` with every
/// EMA over `period` values (registry range `2..=400`); `NaN` for the first `3 * period - 3` values.
#[inline]
pub fn tema(close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("tema", &[period])?;
    run_closes!(crate::fast_b::TemaCore::new(period.round() as usize), close)
}

/// TRIX of `close`: the one-bar percentage change of a triple-smoothed EMA over `period` values
/// (registry range `2..=200`); `NaN` for the first `3 * period - 2` values, and wherever the
/// previous smoothed value is exactly zero.
#[inline]
pub fn trix(close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("trix", &[period])?;
    run_closes!(crate::fast_b::TrixCore::new(period.round() as usize), close)
}

/// Tillson T3 of `close`: three chained generalized-DEMA passes `(1 + v) * EMA - v * EMA(EMA)` over
/// `period` values (registry range `2..=400`) with volume factor `v` (`0..=1`); `NaN` for the first
/// `6 * period - 6` values. A `NaN` in `close` gives `NaN` at its own position only: it is skipped,
/// not folded into the averages.
#[inline]
pub fn t3(close: &[f64], period: f64, v: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("t3", &[period, v])?;
    run_closes!(crate::fast_b::T3Core::new(period.round() as usize, v), close)
}

/// Percentage price oscillator of `close`: `(EMA(fast) - EMA(slow)) / EMA(slow) * 100` (`fast`
/// `2..=200`, `slow` `2..=400`); `NaN` for the first `max(fast, slow) - 1` values and wherever the
/// slow EMA is exactly zero.
#[inline]
pub fn ppo(close: &[f64], fast: f64, slow: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("ppo", &[fast, slow])?;
    run_closes!(crate::fast_b::PpoCore::new(fast.round() as usize, slow.round() as usize), close)
}

/// Absolute price oscillator of `close`: `EMA(fast) - EMA(slow)` (`fast` `2..=200`, `slow`
/// `2..=400`); `NaN` for the first `max(fast, slow) - 1` values.
#[inline]
pub fn apo(close: &[f64], fast: f64, slow: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("apo", &[fast, slow])?;
    run_closes!(crate::fast_b::ApoCore::new(fast.round() as usize, slow.round() as usize), close)
}

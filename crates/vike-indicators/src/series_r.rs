//! Whole-series calls on plain `f64` slices for group `r` of the fourth wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`): the eight pair indicators `ratio`,
//! `spread`, `spread_zscore`, `beta`, `correl`, `correl_log`, `half_life` and `kalman_beta`.
//!
//! Each takes the closes of the primary instrument and of the benchmark, aligned bar for bar, and
//! runs the same core as the typed pair type (`crate::fast_r`), so the result equals the stream
//! (`on_pair`, `on_pair_values`) and `vectorize_pair` bit for bit. The result is as long as the
//! shorter of the two slices. A parameter outside the range of the pair registry row
//! ([`crate::pair_registry`]) is a [`crate::ParamError`], as for the single-series calls.

use crate::ParamError;
use crate::fast_r::{
    BetaCore, CorrelCore, HalfLifeCore, KalmanCore, RatioCore, SpreadCore, ZscoreCore,
    run_correl_log, run_slices,
};

/// Check `raw` against the pair registry row named `indicator`, in the row's parameter order
/// (`crate::typed::checked` reads the single-series registry only).
fn checked_pair(indicator: &'static str, raw: &[f64]) -> Result<(), ParamError> {
    let specs = crate::pairs::pair_get(indicator).map_or(&[][..], |meta| meta.params);
    for (spec, &got) in specs.iter().zip(raw) {
        // `!(a && b)` rather than `got < min || got > max`: a NaN fails both comparisons.
        if !(got >= spec.min && got <= spec.max) {
            return Err(ParamError {
                indicator,
                param: spec.name,
                min: spec.min,
                max: spec.max,
                got,
            });
        }
    }
    Ok(())
}

/// Price ratio `primary / benchmark`; `NaN` where the benchmark is `0`.
#[inline]
pub fn ratio(primary: &[f64], benchmark: &[f64]) -> Vec<f64> {
    let mut core = RatioCore::new();
    run_slices!(core, primary, benchmark)
}

/// Spread of the two closes. `log` (registry range `0..=1`, rounded): `0` is the arithmetic spread
/// `primary - benchmark`, any other value the log spread `ln(primary) - ln(benchmark)` (`NaN` where
/// either close is not positive).
#[inline]
pub fn spread(primary: &[f64], benchmark: &[f64], log: f64) -> Result<Vec<f64>, ParamError> {
    checked_pair("spread", &[log])?;
    let mut core = SpreadCore::new(log);
    Ok(run_slices!(core, primary, benchmark))
}

/// Rolling z-score of the hedge-ratio spread `primary - beta * benchmark` over `period` values
/// (registry range `2..=200`; `beta` in `-5..=5`, `1.0` the plain difference); `NaN` for the first
/// `period - 1` values and wherever the window has zero standard deviation. A non-finite close
/// makes the values `NaN` while it is inside the window, and they are exact again after it left.
#[inline]
pub fn spread_zscore(
    primary: &[f64],
    benchmark: &[f64],
    period: f64,
    beta: f64,
) -> Result<Vec<f64>, ParamError> {
    checked_pair("spread_zscore", &[period, beta])?;
    let mut core = ZscoreCore::new(period.round() as usize, beta);
    Ok(run_slices!(core, primary, benchmark))
}

/// Rolling beta of simple returns, `cov(r_primary, r_benchmark) / var(r_benchmark)` over `period`
/// returns (registry range `2..=200`); `NaN` while the window is not full, where a return is
/// undefined (a zero or `NaN` previous close) and where the benchmark's returns are constant.
#[inline]
pub fn beta(primary: &[f64], benchmark: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    checked_pair("beta", &[period])?;
    let mut core = BetaCore::new(period.round() as usize);
    Ok(run_slices!(core, primary, benchmark))
}

/// Rolling Pearson correlation of the two closes (price levels) over `period` values (registry range
/// `2..=200`), in `[-1, 1]`; `NaN` for the first `period - 1` values and where a window has no
/// variance. A non-finite close makes the values `NaN`
/// while it is inside the window, and they are exact again after it left.
#[inline]
pub fn correl(primary: &[f64], benchmark: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    checked_pair("correl", &[period])?;
    let mut core = CorrelCore::new(period.round() as usize);
    Ok(run_slices!(core, primary, benchmark))
}

/// Rolling Pearson correlation of the log returns over `period` returns (registry range `2..=200`),
/// in `[-1, 1]`; `NaN` while the window is not full and wherever a close is not positive.
#[inline]
pub fn correl_log(primary: &[f64], benchmark: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    checked_pair("correl_log", &[period])?;
    Ok(run_correl_log(primary, benchmark, period.round() as usize))
}

/// Mean-reversion half-life, in bars, of the hedge-ratio spread `primary - beta * benchmark` over a
/// rolling window of `period` values (registry range `2..=200`; `beta` in `-5..=5`); `NaN` whenever
/// the window is not mean-reverting or is degenerate.
#[inline]
pub fn half_life(
    primary: &[f64],
    benchmark: &[f64],
    period: f64,
    beta: f64,
) -> Result<Vec<f64>, ParamError> {
    checked_pair("half_life", &[period, beta])?;
    let mut core = HalfLifeCore::new(period.round() as usize, beta);
    Ok(run_slices!(core, primary, benchmark))
}

/// Time-varying hedge ratio of a two-state Kalman filter. `delta` (registry range `1e-6..=0.1`) sets
/// how fast the ratio may drift, `ve` (`1e-6..=1`) is the assumed observation noise. Defined from
/// the first value.
#[inline]
pub fn kalman_beta(
    primary: &[f64],
    benchmark: &[f64],
    delta: f64,
    ve: f64,
) -> Result<Vec<f64>, ParamError> {
    checked_pair("kalman_beta", &[delta, ve])?;
    let mut core = KalmanCore::new(delta, ve);
    Ok(run_slices!(core, primary, benchmark))
}

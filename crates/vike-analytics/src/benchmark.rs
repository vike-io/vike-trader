//! Benchmark-comparison analytics — port of `analysis/benchmark.py`.
//!
//! ⚠ "Port of" is PROVENANCE, not a standing obligation: the Python app is retired, no
//! exporter survives in this tree, and no test in this crate compares these functions against
//! it (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`). Where a doc
//! below says a panic "mirrors" Python's `ValueError`, it is recording where the behaviour
//! came from.
//!
//! Every `pow` here (`r_squared`, `treynor_ratio`, and `crate::metrics::cagr`, which `alpha` calls) is
//! `libm::pow` — the `libm` CRATE, never `f64::powf`
//! (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`). The
//! crate doc's "Cross-platform determinism" section carries what that costs, including that
//! these numbers no longer track CPython's platform libm, which is what the Python app
//! computed with — so even the SPIRIT of a cross-implementation comparison is gone here, not
//! just the mechanism.

use vike_math::statistics::{mean, pearson};

use crate::metrics;

/// Simple per-step returns: `(eq[i] / eq[i-1]) - 1`, padding zero denominators with 0.0
/// (length-preserving — unlike `metrics::returns`, benchmark comparisons pair two curves
/// element-wise and must not shorten).
fn returns(equity_curve: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(equity_curve.len().saturating_sub(1));
    for i in 1..equity_curve.len() {
        let previous = equity_curve[i - 1];
        if previous != 0.0 {
            out.push(equity_curve[i] / previous - 1.0);
        } else {
            out.push(0.0);
        }
    }
    out
}

/// Sample variance (`n-1`, `vike_math::statistics::mean_variance`); this crate's `0.0` under two values.
fn variance(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let (_, var) = vike_math::statistics::mean_variance(xs.iter().copied(), 1.0);
    var
}

/// Sample covariance (`n-1`, `vike_math::statistics::covariance`) over the shorter length; this crate's
/// `0.0` under two pairs.
fn covariance(a: &[f64], b: &[f64]) -> f64 {
    if a.len().min(b.len()) < 2 {
        return 0.0;
    }
    vike_math::statistics::covariance(a, b, 1.0)
}

fn check_lengths(a: &[f64], b: &[f64]) {
    assert_eq!(
        a.len(),
        b.len(),
        "equity curves must be the same length (got {} vs {})",
        a.len(),
        b.len()
    );
}

/// `cov(rs, rb) / var(rb)`. Returns 0.0 when benchmark has zero variance.
///
/// # Panics
///
/// Panics if `strategy_eq` and `bench_eq` differ in length — inherited from the ported
/// `analysis/benchmark.py`, which raised `ValueError` here (see the module header: this is
/// provenance, not a live comparison).
pub fn beta(strategy_eq: &[f64], bench_eq: &[f64]) -> f64 {
    check_lengths(strategy_eq, bench_eq);
    let rs = returns(strategy_eq);
    let rb = returns(bench_eq);
    let vb = variance(&rb);
    if vb == 0.0 {
        return 0.0;
    }
    covariance(&rs, &rb) / vb
}

/// Annualised Jensen's alpha: `CAGR_s - [rf + beta * (CAGR_b - rf)]`.
///
/// # Panics
///
/// Panics if `strategy_eq` and `bench_eq` differ in length.
pub fn alpha(strategy_eq: &[f64], bench_eq: &[f64], periods_per_year: f64, rf: f64) -> f64 {
    check_lengths(strategy_eq, bench_eq);
    let b = beta(strategy_eq, bench_eq);
    let cagr_s = metrics::cagr(strategy_eq, periods_per_year);
    let cagr_b = metrics::cagr(bench_eq, periods_per_year);
    cagr_s - (rf + b * (cagr_b - rf))
}

/// Pearson correlation of per-step returns. 0.0 when either series has zero variance.
///
/// # Panics
///
/// Panics if `strategy_eq` and `bench_eq` differ in length.
pub fn correlation(strategy_eq: &[f64], bench_eq: &[f64]) -> f64 {
    check_lengths(strategy_eq, bench_eq);
    pearson(&returns(strategy_eq), &returns(bench_eq))
}

/// Coefficient of determination: `correlation ** 2`.
///
/// # Panics
///
/// Panics if `strategy_eq` and `bench_eq` differ in length.
pub fn r_squared(strategy_eq: &[f64], bench_eq: &[f64]) -> f64 {
    libm::pow(correlation(strategy_eq, bench_eq), 2.0)
}

/// Annualised std-dev of the return differential (`rs - rb`).
///
/// # Panics
///
/// Panics if `strategy_eq` and `bench_eq` differ in length.
pub fn tracking_error(strategy_eq: &[f64], bench_eq: &[f64], periods_per_year: f64) -> f64 {
    check_lengths(strategy_eq, bench_eq);
    let rs = returns(strategy_eq);
    let rb = returns(bench_eq);
    if rs.len() < 2 {
        return 0.0;
    }
    let diffs: Vec<f64> = (0..rs.len()).map(|i| rs[i] - rb[i]).collect();
    (variance(&diffs) * periods_per_year).sqrt()
}

/// Annualised `mean(rs - rb) / std(rs - rb)`. 0.0 when tracking error is zero.
///
/// # Panics
///
/// Panics if `strategy_eq` and `bench_eq` differ in length.
pub fn information_ratio(strategy_eq: &[f64], bench_eq: &[f64], periods_per_year: f64) -> f64 {
    check_lengths(strategy_eq, bench_eq);
    let rs = returns(strategy_eq);
    let rb = returns(bench_eq);
    if rs.len() < 2 {
        return 0.0;
    }
    let diffs: Vec<f64> = (0..rs.len()).map(|i| rs[i] - rb[i]).collect();
    let mean_diff = mean(diffs.iter().copied());
    let te = variance(&diffs).sqrt();
    if te == 0.0 {
        return 0.0;
    }
    (mean_diff / te) * periods_per_year.sqrt()
}

/// Upside capture ratio: `mean(rs | rb > 0) / mean(rb | rb > 0)`.
///
/// Returns 0.0 when the benchmark has no positive-return bars.
///
/// # Panics
///
/// Panics if `strategy_eq` and `bench_eq` differ in length.
pub fn up_capture(strategy_eq: &[f64], bench_eq: &[f64]) -> f64 {
    check_lengths(strategy_eq, bench_eq);
    let rs = returns(strategy_eq);
    let rb = returns(bench_eq);
    let up_rs: Vec<f64> = (0..rb.len()).filter(|&i| rb[i] > 0.0).map(|i| rs[i]).collect();
    let up_rb: Vec<f64> = rb.iter().copied().filter(|&r| r > 0.0).collect();
    if up_rb.is_empty() {
        return 0.0;
    }
    let mean_rb = mean(up_rb.iter().copied());
    if mean_rb == 0.0 {
        return 0.0;
    }
    let mean_rs = mean(up_rs.iter().copied());
    mean_rs / mean_rb
}

/// Downside capture ratio: `mean(rs | rb < 0) / mean(rb | rb < 0)`.
///
/// Returns 0.0 when the benchmark has no negative-return bars.
///
/// # Panics
///
/// Panics if `strategy_eq` and `bench_eq` differ in length.
pub fn down_capture(strategy_eq: &[f64], bench_eq: &[f64]) -> f64 {
    check_lengths(strategy_eq, bench_eq);
    let rs = returns(strategy_eq);
    let rb = returns(bench_eq);
    let down_rs: Vec<f64> = (0..rb.len()).filter(|&i| rb[i] < 0.0).map(|i| rs[i]).collect();
    let down_rb: Vec<f64> = rb.iter().copied().filter(|&r| r < 0.0).collect();
    if down_rb.is_empty() {
        return 0.0;
    }
    let mean_rb = mean(down_rb.iter().copied());
    if mean_rb == 0.0 {
        return 0.0;
    }
    let mean_rs = mean(down_rs.iter().copied());
    mean_rs / mean_rb
}

/// Bundle of all benchmark metrics, mirroring Python's `benchmark_stats()` dict.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkStats {
    pub beta: f64,
    pub alpha: f64,
    pub correlation: f64,
    pub r_squared: f64,
    pub tracking_error: f64,
    pub information_ratio: f64,
    pub up_capture: f64,
    pub down_capture: f64,
}

/// Computes every benchmark statistic in one pass.
///
/// # Panics
///
/// Panics if `strategy_eq` and `bench_eq` differ in length.
pub fn benchmark_stats(
    strategy_eq: &[f64],
    bench_eq: &[f64],
    periods_per_year: f64,
    rf: f64,
) -> BenchmarkStats {
    check_lengths(strategy_eq, bench_eq);
    BenchmarkStats {
        beta: beta(strategy_eq, bench_eq),
        alpha: alpha(strategy_eq, bench_eq, periods_per_year, rf),
        correlation: correlation(strategy_eq, bench_eq),
        r_squared: r_squared(strategy_eq, bench_eq),
        tracking_error: tracking_error(strategy_eq, bench_eq, periods_per_year),
        information_ratio: information_ratio(strategy_eq, bench_eq, periods_per_year),
        up_capture: up_capture(strategy_eq, bench_eq),
        down_capture: down_capture(strategy_eq, bench_eq),
    }
}

/// Treynor ratio: excess return per unit of systematic risk (beta).
///
/// `Treynor = (annualized_return - rf_annual) / beta`, where `annualized_return` is the
/// geometric (CAGR-style) annualization of the strategy's own per-step returns and
/// `rf_annual = (1 + rf) ** periods_per_year - 1`. 0.0 when there are fewer than 2
/// aligned returns, the growth factor is non-positive, or beta is 0 (flat/degenerate
/// benchmark).
///
/// # Panics
///
/// Panics if `strategy_eq` and `bench_eq` differ in length — inherited from the ported
/// `analysis/benchmark.py`, which raised `ValueError` here (see the module header: this is
/// provenance, not a live comparison).
pub fn treynor_ratio(strategy_eq: &[f64], bench_eq: &[f64], periods_per_year: f64, rf: f64) -> f64 {
    check_lengths(strategy_eq, bench_eq);
    let rs = returns(strategy_eq);
    let n = rs.len();
    if n < 2 {
        return 0.0;
    }
    let b = beta(strategy_eq, bench_eq);
    if b == 0.0 {
        return 0.0;
    }
    let growth = rs.iter().fold(1.0, |accumulator, &r| accumulator * (1.0 + r));
    if growth <= 0.0 {
        return 0.0;
    }
    let exponent = periods_per_year / n as f64;
    if exponent > 1000.0 {
        return 0.0;
    }
    let annualized_return = libm::pow(growth, exponent) - 1.0;
    if !annualized_return.is_finite() {
        return 0.0;
    }
    let rf_annual = libm::pow(1.0 + rf, periods_per_year) - 1.0;
    if !rf_annual.is_finite() {
        return 0.0;
    }
    (annualized_return - rf_annual) / b
}

#[path = "benchmark_tests.rs"]
#[cfg(test)]
mod benchmark_tests;

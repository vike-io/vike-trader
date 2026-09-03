//! Benchmark-comparison analytics — port of `analysis/benchmark.py`.
//!
//! ⚠ "Port of" is PROVENANCE, not a standing obligation: the Python app is retired, no
//! exporter survives in this tree, and no test in this crate compares these functions against
//! it (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`). Where a doc
//! below says a panic "mirrors" Python's `ValueError`, it is recording where the behaviour
//! came from.
//!
//! Every `pow` here (`cagr`, `r_squared`, `treynor_ratio`) is `libm::pow` — the `libm` CRATE,
//! never `f64::powf`
//! (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`). The
//! crate doc's "Cross-platform determinism" section carries what that costs, including that
//! these numbers no longer track CPython's platform libm, which is what the Python app
//! computed with — so even the SPIRIT of a cross-implementation comparison is gone here, not
//! just the mechanism.

/// Simple per-step returns: `(eq[i] / eq[i-1]) - 1`, padding zero denominators with 0.0
/// (length-preserving — unlike `metrics::returns`, benchmark comparisons pair two curves
/// element-wise and must not shorten).
fn returns(equity_curve: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(equity_curve.len().saturating_sub(1));
    for i in 1..equity_curve.len() {
        let prev = equity_curve[i - 1];
        if prev != 0.0 {
            out.push(equity_curve[i] / prev - 1.0);
        } else {
            out.push(0.0);
        }
    }
    out
}

fn variance(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let (_, var) = crate::metrics::mean_variance(xs.iter().copied(), 1.0);
    var
}

fn covariance(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let ma = vike_model::py_sum(a[..n].iter().copied()) / n as f64;
    let mb = vike_model::py_sum(b[..n].iter().copied()) / n as f64;
    vike_model::py_sum((0..n).map(|i| (a[i] - ma) * (b[i] - mb))) / (n - 1) as f64
}

/// Annualised growth rate; 0.0 for a flat/short/non-positive curve.
fn cagr(equity_curve: &[f64], periods_per_year: f64) -> f64 {
    let n = equity_curve.len() as i64 - 1;
    if n < 1 || equity_curve[0] <= 0.0 {
        return 0.0;
    }
    let growth = equity_curve[equity_curve.len() - 1] / equity_curve[0];
    if growth <= 0.0 {
        return 0.0;
    }
    let exponent = periods_per_year / n as f64;
    if exponent > 1000.0 {
        return 0.0;
    }
    let result = libm::pow(growth, exponent) - 1.0;
    if result.is_finite() {
        result
    } else {
        0.0
    }
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
/// Panics if `strat_eq` and `bench_eq` differ in length — inherited from the ported
/// `analysis/benchmark.py`, which raised `ValueError` here (see the module header: this is
/// provenance, not a live comparison).
pub fn beta(strat_eq: &[f64], bench_eq: &[f64]) -> f64 {
    check_lengths(strat_eq, bench_eq);
    let rs = returns(strat_eq);
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
/// Panics if `strat_eq` and `bench_eq` differ in length.
pub fn alpha(strat_eq: &[f64], bench_eq: &[f64], periods_per_year: f64, rf: f64) -> f64 {
    check_lengths(strat_eq, bench_eq);
    let b = beta(strat_eq, bench_eq);
    let cagr_s = cagr(strat_eq, periods_per_year);
    let cagr_b = cagr(bench_eq, periods_per_year);
    cagr_s - (rf + b * (cagr_b - rf))
}

/// Pearson correlation of per-step returns. 0.0 when either series has zero variance.
///
/// # Panics
///
/// Panics if `strat_eq` and `bench_eq` differ in length.
pub fn correlation(strat_eq: &[f64], bench_eq: &[f64]) -> f64 {
    check_lengths(strat_eq, bench_eq);
    let rs = returns(strat_eq);
    let rb = returns(bench_eq);
    if rs.len() < 2 {
        return 0.0;
    }
    let vs = variance(&rs);
    let vb = variance(&rb);
    if vs <= 0.0 || vb <= 0.0 {
        return 0.0;
    }
    covariance(&rs, &rb) / (vs * vb).sqrt()
}

/// Coefficient of determination: `correlation ** 2`.
///
/// # Panics
///
/// Panics if `strat_eq` and `bench_eq` differ in length.
pub fn r_squared(strat_eq: &[f64], bench_eq: &[f64]) -> f64 {
    libm::pow(correlation(strat_eq, bench_eq), 2.0)
}

/// Annualised std-dev of the return differential (`rs - rb`).
///
/// # Panics
///
/// Panics if `strat_eq` and `bench_eq` differ in length.
pub fn tracking_error(strat_eq: &[f64], bench_eq: &[f64], periods_per_year: f64) -> f64 {
    check_lengths(strat_eq, bench_eq);
    let rs = returns(strat_eq);
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
/// Panics if `strat_eq` and `bench_eq` differ in length.
pub fn information_ratio(strat_eq: &[f64], bench_eq: &[f64], periods_per_year: f64) -> f64 {
    check_lengths(strat_eq, bench_eq);
    let rs = returns(strat_eq);
    let rb = returns(bench_eq);
    if rs.len() < 2 {
        return 0.0;
    }
    let diffs: Vec<f64> = (0..rs.len()).map(|i| rs[i] - rb[i]).collect();
    let mean_diff = vike_model::py_sum(diffs.iter().copied()) / diffs.len() as f64;
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
/// Panics if `strat_eq` and `bench_eq` differ in length.
pub fn up_capture(strat_eq: &[f64], bench_eq: &[f64]) -> f64 {
    check_lengths(strat_eq, bench_eq);
    let rs = returns(strat_eq);
    let rb = returns(bench_eq);
    let up_rs: Vec<f64> = (0..rb.len()).filter(|&i| rb[i] > 0.0).map(|i| rs[i]).collect();
    let up_rb: Vec<f64> = rb.iter().copied().filter(|&r| r > 0.0).collect();
    if up_rb.is_empty() {
        return 0.0;
    }
    let mean_rb = vike_model::py_sum(up_rb.iter().copied()) / up_rb.len() as f64;
    if mean_rb == 0.0 {
        return 0.0;
    }
    let mean_rs = vike_model::py_sum(up_rs.iter().copied()) / up_rs.len() as f64;
    mean_rs / mean_rb
}

/// Downside capture ratio: `mean(rs | rb < 0) / mean(rb | rb < 0)`.
///
/// Returns 0.0 when the benchmark has no negative-return bars.
///
/// # Panics
///
/// Panics if `strat_eq` and `bench_eq` differ in length.
pub fn down_capture(strat_eq: &[f64], bench_eq: &[f64]) -> f64 {
    check_lengths(strat_eq, bench_eq);
    let rs = returns(strat_eq);
    let rb = returns(bench_eq);
    let dn_rs: Vec<f64> = (0..rb.len()).filter(|&i| rb[i] < 0.0).map(|i| rs[i]).collect();
    let dn_rb: Vec<f64> = rb.iter().copied().filter(|&r| r < 0.0).collect();
    if dn_rb.is_empty() {
        return 0.0;
    }
    let mean_rb = vike_model::py_sum(dn_rb.iter().copied()) / dn_rb.len() as f64;
    if mean_rb == 0.0 {
        return 0.0;
    }
    let mean_rs = vike_model::py_sum(dn_rs.iter().copied()) / dn_rs.len() as f64;
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
/// Panics if `strat_eq` and `bench_eq` differ in length.
pub fn benchmark_stats(
    strat_eq: &[f64],
    bench_eq: &[f64],
    periods_per_year: f64,
    rf: f64,
) -> BenchmarkStats {
    check_lengths(strat_eq, bench_eq);
    BenchmarkStats {
        beta: beta(strat_eq, bench_eq),
        alpha: alpha(strat_eq, bench_eq, periods_per_year, rf),
        correlation: correlation(strat_eq, bench_eq),
        r_squared: r_squared(strat_eq, bench_eq),
        tracking_error: tracking_error(strat_eq, bench_eq, periods_per_year),
        information_ratio: information_ratio(strat_eq, bench_eq, periods_per_year),
        up_capture: up_capture(strat_eq, bench_eq),
        down_capture: down_capture(strat_eq, bench_eq),
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
/// Panics if `strat_eq` and `bench_eq` differ in length — inherited from the ported
/// `analysis/benchmark.py`, which raised `ValueError` here (see the module header: this is
/// provenance, not a live comparison).
pub fn treynor_ratio(strat_eq: &[f64], bench_eq: &[f64], periods_per_year: f64, rf: f64) -> f64 {
    check_lengths(strat_eq, bench_eq);
    let rs = returns(strat_eq);
    let n = rs.len();
    if n < 2 {
        return 0.0;
    }
    let b = beta(strat_eq, bench_eq);
    if b == 0.0 {
        return 0.0;
    }
    let growth = rs.iter().fold(1.0, |acc, &r| acc * (1.0 + r));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an equity curve whose per-step returns equal `rets` exactly.
    fn eq(rets: &[f64]) -> Vec<f64> {
        let mut curve = vec![1.0];
        for &r in rets {
            curve.push(curve.last().unwrap() * (1.0 + r));
        }
        curve
    }

    // Reference values verified against NautilusTrader's TreynorRatio statistic
    // (nautilus_trader develop crates/analysis, 2026-07-08) — same math, house
    // 0.0-sentinel convention instead of NaN for degenerate inputs.

    #[test]
    fn treynor_ratio_known_value() {
        // strategy = 2 * benchmark -> beta = 2 (rf = 0).
        let bench_eq = eq(&[0.01, -0.02, 0.015, -0.005, 0.025]);
        let strat_eq = eq(&[0.02, -0.04, 0.030, -0.010, 0.050]);
        let growth = 1.02_f64 * 0.96 * 1.03 * 0.99 * 1.05;
        let expected = (growth - 1.0) / 2.0;
        assert!((treynor_ratio(&strat_eq, &bench_eq, 5.0, 0.0) - expected).abs() < 1e-9);
    }

    #[test]
    fn treynor_ratio_period_ne_n_and_nonzero_rf() {
        let bench_eq = eq(&[0.01, 0.0, 0.015, 0.01]);
        let strat_eq = eq(&[0.02, -0.01, 0.03, 0.005]);
        let result = treynor_ratio(&strat_eq, &bench_eq, 252.0, 0.0001);
        assert!((result - 5.920_539_327_065_061).abs() < 1e-6);
    }

    #[test]
    fn treynor_ratio_flat_benchmark_is_zero() {
        let bench_eq = eq(&[0.01, 0.01, 0.01, 0.01, 0.01]);
        let strat_eq = eq(&[0.02, -0.04, 0.030, -0.010, 0.050]);
        assert_eq!(treynor_ratio(&strat_eq, &bench_eq, 5.0, 0.0), 0.0);
    }

    #[test]
    fn treynor_ratio_empty_is_zero() {
        assert_eq!(treynor_ratio(&[], &[], 252.0, 0.0), 0.0);
    }

    #[test]
    #[should_panic(expected = "equity curves must be the same length")]
    fn treynor_ratio_length_mismatch_panics() {
        treynor_ratio(&[1.0, 2.0], &[1.0, 2.0, 3.0], 252.0, 0.0);
    }

    #[test]
    fn beta_vs_itself_is_one() {
        let eq: Vec<f64> = (0..30).map(|i| 100.0 + i as f64 * 2.5).collect();
        assert!((beta(&eq, &eq) - 1.0).abs() < 1e-9);
    }

    #[test]
    #[should_panic(expected = "equity curves must be the same length")]
    fn beta_length_mismatch_panics() {
        beta(&[1.0, 2.0], &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn alpha_identical_curves_approx_zero() {
        let curve: Vec<f64> = (0..252).map(|i| 100.0 * 1.01_f64.powi(i)).collect();
        assert!(alpha(&curve, &curve, 252.0, 0.0).abs() < 1e-10);
    }

    #[test]
    fn correlation_vs_itself_is_one() {
        let curve: Vec<f64> = (0..30).map(|i| 100.0 + i as f64 * 1.3).collect();
        assert!((correlation(&curve, &curve) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn correlation_zero_for_flat() {
        let flat = vec![100.0; 20];
        let strat: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        assert_eq!(correlation(&strat, &flat), 0.0);
    }

    #[test]
    fn r_squared_is_correlation_squared() {
        let curve: Vec<f64> =
            (0..40).map(|i| 100.0 + i as f64 * 0.7 + if i % 2 == 0 { 2.0 } else { -2.0 }).collect();
        let bench: Vec<f64> =
            (0..40).map(|i| 100.0 + i as f64 * 0.5 + if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        let corr = correlation(&curve, &bench);
        assert!((r_squared(&curve, &bench) - corr.powf(2.0)).abs() < 1e-12);
    }

    #[test]
    fn tracking_error_identical_curves_is_zero() {
        let curve: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        assert!(tracking_error(&curve, &curve, 252.0).abs() < 1e-9);
    }

    #[test]
    fn tracking_error_positive() {
        let bench: Vec<f64> = (0..30).map(|i| 100.0 + i as f64).collect();
        let strat: Vec<f64> =
            (0..30).map(|i| 100.0 + i as f64 * 1.5 + if i % 2 == 0 { 3.0 } else { -3.0 }).collect();
        assert!(tracking_error(&strat, &bench, 252.0) > 0.0);
    }

    #[test]
    fn ir_identical_curves_is_zero() {
        let curve: Vec<f64> = (0..30).map(|i| 100.0 + i as f64 * 0.5).collect();
        assert!(information_ratio(&curve, &curve, 252.0).abs() < 1e-9);
    }

    #[test]
    fn ir_outperforming_is_nonnegative() {
        let bench: Vec<f64> = (0..30).map(|i| 100.0 + i as f64).collect();
        let strat: Vec<f64> = (0..30).map(|i| 100.0 + i as f64 * 2.0).collect();
        assert!(information_ratio(&strat, &bench, 252.0) >= 0.0);
    }

    #[test]
    fn up_capture_two_x_levered() {
        // Strategy returns 2x bench returns -> up capture ~= 2.
        let mut bench = vec![100.0];
        for i in 0..39 {
            let f = if i % 2 == 0 { 1.02 } else { 0.99 };
            bench.push(bench.last().unwrap() * f);
        }
        let mut strat = vec![bench[0] * 2.0];
        for i in 1..bench.len() {
            let rb = bench[i] / bench[i - 1] - 1.0;
            strat.push(strat.last().unwrap() * (1.0 + 2.0 * rb));
        }
        assert!((up_capture(&strat, &bench) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn up_capture_no_up_bars_returns_zero() {
        let bench: Vec<f64> = (0..20).map(|i| 100.0 - i as f64).collect();
        let strat: Vec<f64> = (0..20).map(|i| 100.0 - i as f64 * 0.5).collect();
        assert_eq!(up_capture(&strat, &bench), 0.0);
    }

    #[test]
    fn down_capture_no_down_bars_returns_zero() {
        let bench: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        let strat: Vec<f64> = (0..20).map(|i| 100.0 + i as f64 * 0.5).collect();
        assert_eq!(down_capture(&strat, &bench), 0.0);
    }

    #[test]
    fn benchmark_stats_vs_itself() {
        let curve: Vec<f64> =
            (0..40).map(|i| 100.0 + i as f64 * 1.5 + if i % 2 == 0 { 2.0 } else { -2.0 }).collect();
        let stats = benchmark_stats(&curve, &curve, 252.0, 0.0);
        assert!((stats.beta - 1.0).abs() < 1e-9);
        assert!((stats.correlation - 1.0).abs() < 1e-9);
        assert!((stats.r_squared - 1.0).abs() < 1e-9);
        assert!(stats.tracking_error.abs() < 1e-12);
        assert!(stats.alpha.abs() < 1e-10);
    }

    #[test]
    #[should_panic(expected = "equity curves must be the same length")]
    fn benchmark_stats_length_mismatch_panics() {
        benchmark_stats(&[1.0, 2.0], &[1.0, 2.0, 3.0], 252.0, 0.0);
    }
}

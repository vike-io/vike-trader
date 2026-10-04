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

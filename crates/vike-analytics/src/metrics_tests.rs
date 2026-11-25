use super::*;

/// Build an equity curve whose per-bar returns equal `rets` exactly.
fn eq_from_returns(rets: &[f64]) -> Vec<f64> {
    crate::test_support::equity_from_returns(1.0, rets)
}

fn t(pnl: f64) -> Trade {
    Trade {
        entry_price: 1.0,
        exit_price: 1.0,
        size: 1.0,
        pnl,
        fees: 0.0,
        entry_timestamp: 0,
        exit_timestamp: 0,
        symbol: String::new(),
        mae: 0.0,
        mfe: 0.0,
        is_long: false,
    }
}

// Reference values verified against an independent implementation of these statistics
// (2026-07-08) — same math, house 0.0-sentinel convention instead of NaN for
// degenerate inputs.

#[test]
fn risk_return_ratio_known_value() {
    let eq = eq_from_returns(&[0.1, -0.05, 0.2, -0.1, 0.15]);
    assert!((risk_return_ratio(&eq) - 0.463_600_445_571_753_45).abs() < 1e-9);
}

#[test]
fn risk_return_ratio_zero_std_is_zero() {
    assert_eq!(risk_return_ratio(&[100.0; 10]), 0.0);
}

#[test]
fn risk_return_ratio_empty_is_zero() {
    assert_eq!(risk_return_ratio(&[]), 0.0);
}

#[test]
fn returns_volatility_known_value() {
    let eq = eq_from_returns(&[0.01, -0.02, 0.03, -0.01, 0.02, 0.04, -0.03, 0.05, -0.04, 0.02]);
    assert!((returns_volatility(&eq, 252.0) - 0.485_262_815_389_763_96).abs() < 1e-9);
}

#[test]
fn returns_volatility_empty_is_zero() {
    assert_eq!(returns_volatility(&[], 252.0), 0.0);
}

#[test]
fn returns_skewness_known_value() {
    let eq = eq_from_returns(&[0.01, -0.02, 0.03, -0.01, 0.02, 0.04, -0.03, 0.05, -0.04, 0.02]);
    assert!((returns_skewness(&eq) - (-0.228_720_234_225_963_13)).abs() < 1e-9);
}

#[test]
fn returns_skewness_insufficient_data_is_zero() {
    assert_eq!(returns_skewness(&[100.0, 101.0, 99.0]), 0.0);
}

#[test]
fn returns_skewness_zero_dispersion_is_zero() {
    assert_eq!(returns_skewness(&[100.0; 5]), 0.0);
}

#[test]
fn returns_kurtosis_known_value() {
    let eq = eq_from_returns(&[0.01, -0.02, 0.03, -0.01, 0.02, 0.04, -0.03, 0.05, -0.04, 0.02]);
    assert!((returns_kurtosis(&eq) - (-1.262_244_325_199_502_8)).abs() < 1e-9);
}

#[test]
fn returns_kurtosis_insufficient_data_is_zero() {
    assert_eq!(returns_kurtosis(&[100.0, 101.0, 99.0, 102.0]), 0.0);
}

#[test]
fn tail_ratio_known_value() {
    let eq = eq_from_returns(&[0.01, -0.02, 0.03, -0.01, 0.02, 0.04, -0.03, 0.05, -0.04, 0.02]);
    assert!((tail_ratio(&eq) - 1.281_690_140_845_070_4).abs() < 1e-9);
}

#[test]
fn tail_ratio_symmetric_is_near_one() {
    let eq = eq_from_returns(&[-0.03, -0.02, -0.01, 0.0, 0.01, 0.02, 0.03]);
    assert!((tail_ratio(&eq) - 1.0).abs() < 1e-9);
}

#[test]
fn tail_ratio_insufficient_data_is_zero() {
    assert_eq!(tail_ratio(&[100.0, 101.0]), 0.0);
}

/// The equity curve of one ten-return series the VaR and ES tests read at several levels.
fn tail_curve() -> Vec<f64> {
    eq_from_returns(&[0.02, -0.05, 0.01, -0.08, 0.03, -0.02, 0.04, -0.10, 0.015, -0.03])
}

#[test]
fn value_at_risk_known_value() {
    assert!((value_at_risk(&tail_curve(), 0.95) - (-0.091)).abs() < 1e-9);
}

#[test]
fn value_at_risk_empty_is_zero() {
    assert_eq!(value_at_risk(&[], 0.95), 0.0);
}

#[test]
fn expected_shortfall_known_value() {
    assert!((expected_shortfall(&tail_curve(), 0.95) - (-0.10)).abs() < 1e-9);
}

#[test]
fn expected_shortfall_multi_element_tail() {
    assert!((expected_shortfall(&tail_curve(), 0.60) - (-0.065)).abs() < 1e-9);
}

#[test]
fn expected_shortfall_at_most_value_at_risk() {
    let eq = tail_curve();
    assert!(expected_shortfall(&eq, 0.90) <= value_at_risk(&eq, 0.90));
}

#[test]
fn expected_shortfall_empty_is_zero() {
    assert_eq!(expected_shortfall(&[], 0.95), 0.0);
}

#[test]
fn sqn_known_value() {
    let pnls = [10.0, -5.0, 20.0, -10.0, 15.0];
    let trades: Vec<Trade> = pnls.iter().map(|&p| t(p)).collect();
    let mean = pnls.iter().sum::<f64>() / pnls.len() as f64;
    let var = pnls.iter().map(|p| (p - mean).powf(2.0)).sum::<f64>() / (pnls.len() - 1) as f64;
    let expected = (pnls.len() as f64).sqrt() * mean / var.sqrt();
    assert!((sqn(&trades) - expected).abs() < 1e-9);
}

#[test]
fn sqn_no_trades_is_zero() {
    assert_eq!(sqn(&[]), 0.0);
}

#[test]
fn sqn_one_trade_is_zero() {
    assert_eq!(sqn(&[t(10.0)]), 0.0);
}

#[test]
fn sqn_zero_std_is_zero() {
    assert_eq!(sqn(&[t(10.0), t(10.0), t(10.0)]), 0.0);
}

#[test]
fn ulcer_performance_index_positive_for_curve_with_drawdown() {
    let eq = [10_000.0, 12_000.0, 9_000.0, 13_000.0];
    assert!(ulcer_performance_index(&eq, 252.0) > 0.0);
}

#[test]
fn ulcer_performance_index_zero_ulcer_and_positive_cagr_is_inf() {
    let eq: Vec<f64> = (0..252).map(|i| 10_000.0 + i as f64 * 50.0).collect();
    assert_eq!(ulcer_performance_index(&eq, 252.0), f64::INFINITY);
}

#[test]
fn ulcer_performance_index_flat_is_zero() {
    assert_eq!(ulcer_performance_index(&[100.0; 10], 252.0), 0.0);
}

fn tl(is_long: bool) -> Trade {
    Trade { is_long, ..t(0.0) }
}

#[test]
fn long_ratio_all_long() {
    assert_eq!(long_ratio(&[tl(true), tl(true), tl(true)]), 1.0);
}

#[test]
fn long_ratio_all_short() {
    assert_eq!(long_ratio(&[tl(false), tl(false)]), 0.0);
}

#[test]
fn long_ratio_mixed() {
    let trades = [tl(true), tl(false), tl(true), tl(false)];
    assert_eq!(long_ratio(&trades), 0.5);
}

#[test]
fn long_ratio_no_trades_is_zero() {
    assert_eq!(long_ratio(&[]), 0.0);
}

/// A trade's return is its price move in its own direction: a long that rose and a short that fell both earned.
#[test]
fn trade_return_follows_the_trades_direction() {
    let trade = |is_long: bool| Trade { entry_price: 100.0, exit_price: 110.0, is_long, ..t(0.0) };
    assert!((trade_return(&trade(true)) - 0.1).abs() < 1e-12);
    assert!((trade_return(&trade(false)) + 0.1).abs() < 1e-12);
}

/// The walk's drop at each value is the underwater line, and its worst is still `max_drawdown`'s.
#[test]
fn the_drawdown_walk_answers_each_values_drop_below_the_peak() {
    let curve = [100.0, 120.0, 90.0, 130.0, 117.0];
    let mut walk = DrawdownWalk::default();
    let drops: Vec<f64> = curve.iter().map(|&v| walk.push(v)).collect();
    assert_eq!(drops, vec![0.0, 0.0, 0.25, 0.0, 0.1]);
    assert_eq!(walk.worst(), max_drawdown(&curve));
}

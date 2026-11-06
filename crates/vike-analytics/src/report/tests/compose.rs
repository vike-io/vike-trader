//! Composing a report: `metrics::` parity, the `Display` table, zero-trade, periods-per-year.

use super::*;

#[test]
fn matches_direct_metrics_calls() {
    let r = sample_result();
    let report = BacktestReport::from_result(Some("demo".to_string()), &r, 252.0);

    assert_eq!(report.name.as_deref(), Some("demo"));
    assert_eq!(report.final_equity, r.final_equity);
    assert_eq!(report.n_trades, r.n_trades);
    assert_eq!(report.per_symbol_pnl, r.per_symbol_pnl);
    assert_eq!(report.total_return, metrics::total_return(&r.equity_curve));
    assert_eq!(report.max_drawdown, metrics::max_drawdown(&r.equity_curve));
    assert_eq!(report.win_rate, metrics::win_rate(&r.trades));
    assert_eq!(report.sharpe, metrics::sharpe(&r.equity_curve, 252.0));
    assert_eq!(report.profit_factor, metrics::profit_factor(&r.trades));

    // Sanity on the composed values themselves, not just that they match — a report that
    // merely "matches metrics" by both being wrong the same way would still pass the
    // assertions above.
    assert_eq!(report.win_rate, 0.5); // 1 win / 2 trades
    assert!((report.total_return - 0.005).abs() < 1e-12); // 1005/1000 - 1
    assert!((report.max_drawdown - (1010.0 - 990.0) / 1010.0).abs() < 1e-12);
}

#[test]
fn display_is_a_compact_human_table() {
    let r = sample_result();
    let report = BacktestReport::from_result(None, &r, 252.0);
    let s = report.to_string();
    assert!(s.contains("(unnamed)"));
    assert!(s.contains("final_equity:"));
    assert!(s.contains("win_rate:"));
    assert!(s.contains("sharpe:"));
    assert!(s.contains("max_drawdown:"));
    assert!(s.contains("BTCUSDT"));
}

/// OFF / byte-identical path: a run WITH trades is never diagnosed, so its report grows no
/// `zero_trade` field in JSON and its `Display` is the unchanged metrics table.
#[test]
fn zero_trade_is_absent_and_output_unchanged_for_a_run_with_trades() {
    let r = sample_result(); // n_trades = 2, moving equity curve
    let report = BacktestReport::from_result(Some("demo".to_string()), &r, 252.0);
    assert!(report.zero_trade.is_none(), "a run with trades is never diagnosed");

    // JSON: no `zero_trade` key (skip_serializing_if) -> byte-identical to before the field.
    let json = serde_json::to_string(&report).unwrap();
    assert!(
        !json.contains("zero_trade"),
        "a normal-run JSON must not grow a zero_trade field: {json}"
    );

    // Display: the full metrics table (the diagnosis branch is skipped).
    let s = report.to_string();
    assert!(s.contains("win_rate:"));
    assert!(s.contains("sharpe:"));
    assert!(s.contains("max_drawdown:"));
    assert!(!s.contains("probable cause"));
}

/// A contrived zero-trade / flat-equity run surfaces the correct ranked cause: its `Display`
/// prints the diagnosis INSTEAD OF the all-zero metrics rows, and its JSON carries `zero_trade`.
#[test]
fn zero_trade_diagnosis_replaces_the_table_for_a_flat_zero_trade_run() {
    let r = BacktestResult {
        n_trades: 0,
        equity_curve: vec![1000.0, 1000.0, 1000.0],
        final_equity: 1000.0,
        dropped: vec![
            ("BTCUSDT".to_string(), "insufficient-margin".to_string(), 1.0, 0.0),
            ("BTCUSDT".to_string(), "insufficient-margin".to_string(), 2.0, 0.0),
        ],
        ..Default::default()
    };
    let report = BacktestReport::from_result(None, &r, 252.0);

    let zt = report.zero_trade.as_ref().expect("a flat zero-trade run must be diagnosed");
    assert_eq!(zt.causes[0].code, "orders-denied");

    // Display: diagnosis present, the metric rows (win_rate/sharpe) replaced by it.
    let s = report.to_string();
    assert!(s.contains("probable cause"));
    assert!(s.contains("rejected before filling"));
    assert!(!s.contains("win_rate:"), "the bare metrics table must be replaced: {s}");
    assert!(!s.contains("sharpe:"));

    // JSON now carries the diagnosis.
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("zero_trade"), "a zero-trade run's JSON carries the diagnosis: {json}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["zero_trade"]["causes"][0]["code"], "orders-denied");
}

/// The daily anchor is preserved BIT-for-bit — the whole reason the intraday fix could ship
/// without moving a single existing daily report.
#[test]
fn the_daily_anchor_is_preserved_exactly() {
    assert_eq!(periods_per_year_for_interval("1d"), DAILY_PERIODS_PER_YEAR);
}

/// The two values the walk-forward divergence was measured at: `1h` is the interval the
/// Studio door reported `sqrt(24)` low, `1m` the one the CI roundtrip fixture uses and the
/// original intraday bug understated by `sqrt(1440)`.
#[test]
fn intraday_intervals_scale_off_the_daily_anchor() {
    assert_eq!(periods_per_year_for_interval("1h"), 6_048.0);
    assert_eq!(periods_per_year_for_interval("1m"), 362_880.0);
    // ...and the scale is exactly "how many of that interval fit in a day".
    assert_eq!(periods_per_year_for_interval("4h"), DAILY_PERIODS_PER_YEAR * 6.0);
}

/// An interval the parser cannot read falls back rather than fabricating a scale. This is a
/// reporting knob, not a validation site.
#[test]
fn an_unparseable_interval_falls_back_instead_of_fabricating_a_scale() {
    assert_eq!(periods_per_year_for_interval(""), DEFAULT_PERIODS_PER_YEAR);
    assert_eq!(periods_per_year_for_interval("not-an-interval"), DEFAULT_PERIODS_PER_YEAR);
}

// --- the long-form catalog, the honesty counters and the realism stamp ---------------------

/// The same property `matches_direct_metrics_calls` asserts for the compact eight, for the
/// thirty that joined them: every field is one `metrics::` call and nothing here is new math,
/// so a divergence is a wiring bug.
#[test]
fn extended_matches_direct_metrics_calls() {
    let r = sample_result();
    let e = ExtendedMetrics::from_result(&r, 252.0);
    let eq = &r.equity_curve;
    let tr = &r.trades;

    assert_eq!(e.net_profit, metrics::net_profit(tr));
    assert_eq!(e.gross_profit, metrics::gross_profit(tr));
    assert_eq!(e.gross_loss, metrics::gross_loss(tr));
    assert_eq!(e.total_fees, metrics::total_fees(tr));
    assert_eq!(e.avg_win, metrics::avg_win(tr));
    assert_eq!(e.avg_loss, metrics::avg_loss(tr));
    assert_eq!(e.largest_win, metrics::largest_win(tr));
    assert_eq!(e.largest_loss, metrics::largest_loss(tr));
    assert_eq!(e.payoff_ratio, metrics::payoff_ratio(tr));
    assert_eq!(e.expected_payoff, metrics::expected_payoff(tr));
    assert_eq!(e.consecutive_wins, metrics::consecutive_wins(tr));
    assert_eq!(e.consecutive_losses, metrics::consecutive_losses(tr));
    assert_eq!(e.sqn, metrics::sqn(tr));
    assert_eq!(e.long_ratio, metrics::long_ratio(tr));
    assert_eq!(e.sortino, metrics::sortino(eq, 252.0));
    assert_eq!(e.calmar, metrics::calmar(eq, 252.0));
    assert_eq!(e.cagr, metrics::cagr(eq, 252.0));
    assert_eq!(e.mar_ratio, metrics::mar_ratio(eq, 252.0));
    assert_eq!(e.recovery_factor, metrics::recovery_factor(eq));
    assert_eq!(e.ulcer_index, metrics::ulcer_index(eq));
    assert_eq!(e.ulcer_performance_index, metrics::ulcer_performance_index(eq, 252.0));
    assert_eq!(e.k_ratio, metrics::k_ratio(eq));
    assert_eq!(e.risk_return_ratio, metrics::risk_return_ratio(eq));
    assert_eq!(e.returns_volatility, metrics::returns_volatility(eq, 252.0));
    assert_eq!(e.returns_skewness, metrics::returns_skewness(eq));
    assert_eq!(e.returns_kurtosis, metrics::returns_kurtosis(eq));
    assert_eq!(e.tail_ratio, metrics::tail_ratio(eq));
    assert_eq!(e.omega, metrics::omega(eq, OMEGA_THRESHOLD));
    assert_eq!(e.value_at_risk_95, metrics::value_at_risk(eq, TAIL_CONFIDENCE));
    assert_eq!(e.expected_shortfall_95, metrics::expected_shortfall(eq, TAIL_CONFIDENCE));
}

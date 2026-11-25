//! `TradingResultsComparison`: the rows, the difference, the two tables' rules and the text rendering.

use super::*;
use crate::metric_catalog::MetricUnit;
use crate::record::TradingRecord;

fn results(name: &str, equity: Vec<f64>, pnls: &[f64]) -> TradingResults {
    let trades: Vec<_> =
        pnls.iter().map(|pnl| crate::test_support::trade(*pnl, 0.0, true, 1)).collect();
    let record = TradingRecord {
        n_trades: trades.len(),
        final_equity: equity.last().copied().unwrap_or(0.0),
        trades,
        equity_curve: equity,
        ..TradingRecord::default()
    };
    TradingResults::from_record(Some(name.to_string()), &record, 252.0)
}

fn backtest() -> TradingResults {
    results("backtest", vec![1000.0, 1010.0, 990.0, 1005.0], &[10.0, -5.0])
}

fn forward() -> TradingResults {
    results("forward", vec![1000.0, 1120.0, 1100.0], &[120.0, -20.0])
}

/// Its equity reached zero, so it is INVALID.
fn ruined() -> TradingResults {
    results("ruined", vec![1000.0, 0.0], &[-1000.0])
}

fn row(rows: &[TradingResultsComparisonRow], id: &str) -> Option<TradingResultsComparisonRow> {
    rows.iter().find(|r| r.spec.id == id).copied()
}

#[test]
fn the_rows_are_the_catalog_in_order_with_each_sides_own_value() {
    let (a, b) = (backtest(), forward());
    let comparison = TradingResultsComparison::of(a.clone(), b.clone());
    let rows = comparison.rows();
    let ids: Vec<&str> = rows.iter().map(|r| r.spec.id).collect();
    // Neither side armed closed-trade accounting, so those rows are left out, as each side's table does.
    let expected: Vec<&str> =
        METRICS.iter().filter(|m| m.home != MetricHome::ClosedTrades).map(|m| m.id).collect();
    assert_eq!(ids, expected);
    for r in &rows {
        assert_eq!(r.a, a.metric_value(r.spec.id), "{}: a's own value", r.spec.id);
        assert_eq!(r.b, b.metric_value(r.spec.id), "{}: b's own value", r.spec.id);
    }
}

#[test]
fn the_difference_is_b_minus_a() {
    let rows = TradingResultsComparison::of(backtest(), forward()).rows();
    assert_eq!(row(&rows, "final_equity").unwrap().difference, Some(95.0));
    assert_eq!(row(&rows, "n_trades").unwrap().difference, Some(0.0));
    for r in &rows {
        if let (Some(a), Some(b)) = (r.a, r.b)
            && (b - a).is_finite()
        {
            assert_eq!(r.difference, Some(b - a), "{}", r.spec.id);
        }
    }
}

#[test]
fn a_side_with_no_value_has_no_difference() {
    let older = TradingResults { extended: None, ..backtest() };
    let comparison = TradingResultsComparison::of(older, forward());
    let sortino = row(&comparison.rows(), "sortino").unwrap();
    assert_eq!(sortino.a, None, "a report with no extended block did not record it");
    assert!(sortino.b.is_some());
    assert_eq!(sortino.difference, None);
    let rendered = comparison.rendered_rows();
    let (_, cells) = rendered.iter().find(|(id, _)| *id == "sortino").unwrap();
    assert_eq!(cells[0], NOT_RECORDED, "words, never 0.0000");
    assert_eq!(cells[2], "", "no difference to print");
}

#[test]
fn the_inf_sentinel_has_no_difference() {
    let a = TradingResults { profit_factor: f64::INFINITY, ..backtest() };
    let b = TradingResults { profit_factor: f64::INFINITY, ..forward() };
    let both = TradingResultsComparison::of(a.clone(), b).rows();
    assert_eq!(row(&both, "profit_factor").unwrap().difference, None, "inf - inf is no number");
    let one = TradingResultsComparison::of(a, forward()).rows();
    assert_eq!(
        row(&one, "profit_factor").unwrap().difference,
        None,
        "nor is a finite value less inf"
    );
}

#[test]
fn an_invalid_side_keeps_only_its_money_and_count_values() {
    let ruined = ruined();
    assert!(ruined.invalid.is_some(), "the fixture reached zero");
    let comparison = TradingResultsComparison::of(ruined, forward());
    let rows = comparison.rows();
    let sharpe = row(&rows, "sharpe").unwrap();
    assert_eq!((sharpe.a, sharpe.difference), (None, None), "a ratio over a ruined curve");
    assert!(sharpe.b.is_some(), "the valid side keeps the row and its value");
    assert_eq!(row(&rows, "final_equity").unwrap().a, Some(0.0));
    let rendered = comparison.rendered_rows();
    let (_, cells) = rendered.iter().find(|(id, _)| *id == "sharpe").unwrap();
    assert_eq!(cells[0], INVALID_LABEL, "said as INVALID, never as not recorded");
}

#[test]
fn two_invalid_sides_show_only_money_and_count_rows() {
    let rows = TradingResultsComparison::of(ruined(), ruined()).rows();
    assert!(!rows.is_empty());
    for r in &rows {
        assert!(matches!(r.spec.unit, MetricUnit::Money | MetricUnit::Count), "{}", r.spec.id);
    }
}

#[test]
fn the_text_names_both_sides_then_one_row_per_metric() {
    let text = TradingResultsComparison::of(ruined(), forward()).to_string();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "a: backtest ruined");
    assert_eq!(lines[1], "b: backtest forward");
    assert!(lines[2].starts_with("a INVALID: equity reached 0"), "{}", lines[2]);
    assert_eq!(
        lines[3].split_whitespace().collect::<Vec<_>>(),
        ["metric", "a", "b", "b", "-", "a"]
    );
    let final_equity = lines.iter().find(|l| l.starts_with("final_equity ")).unwrap();
    assert_eq!(
        final_equity.split_whitespace().collect::<Vec<_>>(),
        ["final_equity", "0.00", "1100.00", "1100.00"]
    );
    let total_return = lines.iter().find(|l| l.starts_with("total_return ")).unwrap();
    let percent = MetricUnit::Percent.render(forward().total_return);
    assert_eq!(
        total_return.split_whitespace().collect::<Vec<_>>(),
        ["total_return", INVALID_LABEL, percent.as_str()]
    );
    assert!(lines.iter().all(|l| l.trim_end() == *l), "no trailing blanks:\n{text}");
    // The value columns line up: every row's `a` cell starts where the header's does.
    let column = lines[3].find(" a ").unwrap() + 1;
    for line in &lines[4..] {
        assert_ne!(line.as_bytes()[column], b' ', "{line}");
        assert_eq!(line.as_bytes()[column - 1], b' ', "{line}");
    }
}

/// A side is named by its source and period, the label its own table prints; a side read back with a
/// bare parse knows no source and is named by its profile name.
#[test]
fn a_side_is_named_by_its_source_label_or_by_its_name() {
    let record = TradingRecord {
        equity_curve: vec![1000.0, 1010.0],
        final_equity: 1010.0,
        equity_timestamp: vec![1_767_225_600_000, 1_767_312_000_000],
        ..TradingRecord::default()
    };
    let dated = TradingResults::from_record(Some("dated".to_string()), &record, 252.0)
        .with_strategy("strategies/sma.rhai", "00", None);
    let stored: TradingResults =
        serde_json::from_str(&serde_json::to_string(&forward()).unwrap()).unwrap();
    let text = TradingResultsComparison::of(dated, stored).to_string();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "a: backtest strategies/sma.rhai 2026-01-01 00:00 – 2026-01-02 00:00 UTC");
    assert_eq!(lines[1], "b: forward");
}

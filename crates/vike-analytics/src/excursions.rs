//! Trade-level excursion analytics — port of `analysis/excursions.py`.
//!
//! Pure functions over the trade list and OHLC bars. MAE/MFE (Maximum Adverse/Favorable
//! Excursion) are derived by scanning bars between each trade's entry and exit timestamps.

use vike_model::{Bar, Trade};

/// Infer +1 (long) / -1 (short) from an unsigned-size `Trade` — THE single source of truth.
///
/// `Trade.size` is recorded as `abs` (side is lost at the engine level), so `size >= 0` is NOT
/// a side test. A profit on a price rise is a long; a profit on a price fall is a short. The
/// degenerate `pnl == 0` / `exit == entry` case defaults to long.
pub fn trade_direction(trade: &Trade) -> i32 {
    if (trade.pnl >= 0.0) == (trade.exit_price >= trade.entry_price) {
        1
    } else {
        -1
    }
}

/// Returns `(mae, mfe)` as positive fractions of entry price over the trade's bar window.
///
/// MAE = worst adverse excursion, MFE = best favorable excursion, between `entry_ts` and
/// `exit_ts` (inclusive). `direction` overrides the inferred long/short side.
pub fn mae_mfe(trade: &Trade, bars: &[Bar], direction: Option<i32>) -> (f64, f64) {
    let entry = trade.entry_price;
    if entry == 0.0 {
        return (0.0, 0.0);
    }
    let d = direction.unwrap_or_else(|| trade_direction(trade));
    let window: Vec<&Bar> =
        bars.iter().filter(|b| trade.entry_ts <= b.ts && b.ts <= trade.exit_ts).collect();
    if window.is_empty() {
        return (0.0, 0.0);
    }
    if d > 0 {
        // long: adverse = low below entry, favorable = high above entry
        let mae = window.iter().map(|b| (entry - b.low) / entry).fold(0.0, f64::max);
        let mfe = window.iter().map(|b| (b.high - entry) / entry).fold(0.0, f64::max);
        (mae, mfe)
    } else {
        // short: adverse = high above entry, favorable = low below entry
        let mae = window.iter().map(|b| (b.high - entry) / entry).fold(0.0, f64::max);
        let mfe = window.iter().map(|b| (entry - b.low) / entry).fold(0.0, f64::max);
        (mae, mfe)
    }
}

/// Mean MFE / mean MAE across `trades` — entry-quality (>1 favors profitable excursions).
///
/// 0.0 with no trades; `inf` when mean MAE is 0 and mean MFE is positive.
pub fn edge_ratio(trades: &[Trade], bars: &[Bar]) -> f64 {
    if trades.is_empty() {
        return 0.0;
    }
    let pairs: Vec<(f64, f64)> = trades.iter().map(|t| mae_mfe(t, bars, None)).collect();
    let n = pairs.len() as f64;
    let mean_mae = vike_model::py_sum(pairs.iter().map(|&(m, _)| m)) / n;
    let mean_mfe = vike_model::py_sum(pairs.iter().map(|&(_, f)| f)) / n;
    if mean_mae == 0.0 {
        return if mean_mfe > 0.0 { f64::INFINITY } else { 0.0 };
    }
    mean_mfe / mean_mae
}

/// One row of [`expanding_trade_metrics`]'s running series.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpandingMetric {
    pub n: usize,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub avg_pnl: f64,
}

/// Running metrics after each trade: `n`, `win_rate`, `profit_factor`, `avg_pnl`.
///
/// `profit_factor` is `inf` while there are no losing trades and gross profit is positive.
pub fn expanding_trade_metrics(trades: &[Trade]) -> Vec<ExpandingMetric> {
    let mut out = Vec::with_capacity(trades.len());
    let mut wins = 0usize;
    let mut gross_profit = 0.0;
    let mut gross_loss = 0.0;
    let mut total_pnl = 0.0;
    for (i, t) in trades.iter().enumerate() {
        let n = i + 1;
        if t.pnl > 0.0 {
            wins += 1;
            gross_profit += t.pnl;
        } else if t.pnl < 0.0 {
            gross_loss += -t.pnl;
        }
        total_pnl += t.pnl;
        let pf = if gross_loss == 0.0 {
            if gross_profit > 0.0 {
                f64::INFINITY
            } else {
                0.0
            }
        } else {
            gross_profit / gross_loss
        };
        out.push(ExpandingMetric {
            n,
            win_rate: wins as f64 / n as f64,
            profit_factor: pf,
            avg_pnl: total_pnl / n as f64,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(entry: f64, exit: f64, pnl: f64, entry_ts: i64, exit_ts: i64) -> Trade {
        Trade {
            entry_price: entry,
            exit_price: exit,
            size: 1.0,
            pnl,
            fees: 0.0,
            entry_ts,
            exit_ts,
            symbol: String::new(),
            mae: 0.0,
            mfe: 0.0,
            is_long: false,
        }
    }

    fn bar(ts: i64, high: f64, low: f64) -> Bar {
        Bar {
            ts,
            open: (high + low) / 2.0,
            high,
            low,
            close: (high + low) / 2.0,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    // 4 bars at ts 0,100,200,300; bar index 2 carries the extreme low/high.
    fn window_bars(low_at_2: f64, high_at_2: f64) -> Vec<Bar> {
        vec![
            bar(0, 101.0, 99.0),
            bar(100, 102.0, 98.0),
            bar(200, high_at_2, low_at_2),
            bar(300, 101.0, 99.0),
        ]
    }

    #[test]
    fn mae_mfe_long() {
        // long: entry 100, exit 110, profit on a rise -> direction +1
        let t = trade(100.0, 110.0, 10.0, 0, 300);
        let bars = window_bars(95.0, 115.0);
        let (mae, mfe) = mae_mfe(&t, &bars, None);
        assert!((mae - 0.05).abs() < 1e-9); // (100-95)/100
        assert!((mfe - 0.15).abs() < 1e-9); // (115-100)/100
    }

    #[test]
    fn mae_mfe_short() {
        // short: entry 100, exit 90, profit on a fall -> direction -1
        let t = trade(100.0, 90.0, 10.0, 0, 300);
        let bars = window_bars(85.0, 105.0);
        let (mae, mfe) = mae_mfe(&t, &bars, None);
        assert!((mae - 0.05).abs() < 1e-9); // adverse = high: (105-100)/100
        assert!((mfe - 0.15).abs() < 1e-9); // favorable = low: (100-85)/100
    }

    #[test]
    fn mae_mfe_no_window_bars_is_zero() {
        let t = trade(100.0, 110.0, 10.0, 1000, 2000);
        let bars = window_bars(95.0, 115.0); // all ts < 1000
        assert_eq!(mae_mfe(&t, &bars, None), (0.0, 0.0));
    }

    #[test]
    fn edge_ratio_mean_mfe_over_mean_mae() {
        // two identical long trades, each MAE 0.05 / MFE 0.15 -> edge 3.0
        let bars = window_bars(95.0, 115.0);
        let t = trade(100.0, 110.0, 10.0, 0, 300);
        assert!((edge_ratio(&[t.clone(), t], &bars) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn edge_ratio_empty_is_zero() {
        assert_eq!(edge_ratio(&[], &[]), 0.0);
    }

    #[test]
    fn edge_ratio_inf_when_no_adverse() {
        // a trade whose window never goes below entry -> mean MAE 0 -> inf
        let bars = vec![bar(0, 120.0, 100.0), bar(100, 130.0, 105.0)];
        let t = trade(100.0, 120.0, 20.0, 0, 100);
        assert_eq!(edge_ratio(&[t], &bars), f64::INFINITY);
    }

    #[test]
    fn expanding_trade_metrics_running_series() {
        let trades = vec![
            trade(100.0, 110.0, 10.0, 0, 0),
            trade(100.0, 95.0, -5.0, 0, 0),
            trade(100.0, 120.0, 20.0, 0, 0),
        ];
        let out = expanding_trade_metrics(&trades);
        assert_eq!(out.iter().map(|r| r.n).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!((out[0].win_rate - 1.0).abs() < 1e-9);
        assert_eq!(out[0].profit_factor, f64::INFINITY);
        assert!((out[0].avg_pnl - 10.0).abs() < 1e-9);
        assert!((out[1].win_rate - 0.5).abs() < 1e-9);
        assert!((out[1].profit_factor - 10.0 / 5.0).abs() < 1e-9);
        assert!((out[1].avg_pnl - 2.5).abs() < 1e-9);
        assert!((out[2].win_rate - 2.0 / 3.0).abs() < 1e-9);
        assert!((out[2].profit_factor - 30.0 / 5.0).abs() < 1e-9);
        assert!((out[2].avg_pnl - 25.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn expanding_trade_metrics_empty() {
        assert_eq!(expanding_trade_metrics(&[]), vec![]);
    }
}

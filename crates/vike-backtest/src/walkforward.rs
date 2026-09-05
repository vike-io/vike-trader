//! Walk-forward evaluation of a strategy: run it on each out-of-sample window and stitch
//! the OOS windows into one equity curve — the honest way to backtest, the strategy twin
//! of vike-trader-app `ml/walkforward.py` (that one wraps an ML model; this one wraps any
//! `Strategy` via a caller-supplied `run_window` closure).

use crate::metrics::sharpe;
use crate::result::BacktestResult;
use crate::validation::{WalkMode, walk_forward_splits};
use vike_model::Bar;

/// One out-of-sample window's outcome.
///
/// `Serialize` — and deliberately NOT `Deserialize` — so the compute-to-data
/// `RunWalkforwardProfile` verb can return this report as JSON TEXT, exactly as
/// `BacktestReport`/`SweepReport` already cross the datahub wire. The asymmetry keeps the wire
/// schema decoupled from this crate's internal serde surface (see the datahub proto's module doc).
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct WfWindow {
    pub test_range: (usize, usize),
    pub oos_return: f64,
}

/// Stitched walk-forward result. `Serialize`-only, for the reason given on [`WfWindow`].
#[derive(Clone, Debug, serde::Serialize)]
pub struct WalkForwardReport {
    pub windows: Vec<WfWindow>,
    pub oos_equity_curve: Vec<f64>,
    pub oos_return: f64,
    pub oos_sharpe: f64,
    /// Fraction of windows profitable out-of-sample (the `overfit_verdict` input).
    pub wf_consistency: f64,
}

/// Run `run_window` over each walk-forward OOS window and stitch the results. `cash` is the
/// per-window starting equity (each window runs fresh at `cash`); the OOS curves are rebased
/// onto one running equity, mirroring Python `walk_forward_ml`'s stitch. Contract: each
/// window's backtest result must start its equity curve at exactly `cash`.
pub fn walk_forward_strategy(
    bars: &[Bar],
    n_splits: usize,
    mode: WalkMode,
    cash: f64,
    periods_per_year: f64,
    mut run_window: impl FnMut(&[Bar]) -> BacktestResult,
) -> WalkForwardReport {
    let splits = walk_forward_splits(bars.len(), n_splits, mode);
    let mut windows: Vec<WfWindow> = Vec::with_capacity(splits.len());
    let mut stitched: Vec<f64> = Vec::new();
    let mut equity = cash;

    for sp in &splits {
        let oos = run_window(&bars[sp.test_start..sp.test_end]);
        debug_assert!(
            oos.equity_curve.first().is_none_or(|&e0| (e0 - cash).abs() <= cash.abs() * 1e-9),
            "walk_forward_strategy: each window's backtest must start at `cash` ({cash}); got {:?} — the stitch rebasing assumes this",
            oos.equity_curve.first()
        );
        let start = equity;
        for &v in &oos.equity_curve {
            stitched.push(start * (v / cash));
        }
        equity = start * (oos.final_equity / cash);
        windows.push(WfWindow {
            test_range: (sp.test_start, sp.test_end),
            oos_return: (oos.final_equity / cash) - 1.0,
        });
    }

    let n = windows.len().max(1) as f64;
    let wf_consistency = windows.iter().filter(|w| w.oos_return > 0.0).count() as f64 / n;
    WalkForwardReport {
        oos_return: (equity / cash) - 1.0,
        oos_sharpe: sharpe(&stitched, periods_per_year),
        wf_consistency,
        windows,
        oos_equity_curve: stitched,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ref_strategies::BracketPerSymbol;
    use crate::{EngineParams, StrategyEngine};
    use vike_model::Bar;

    fn bar(ts: i64, close: f64) -> Bar {
        Bar {
            ts,
            open: close,
            high: close * 1.01,
            low: close * 0.99,
            close,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None, // the engine dispatch attaches "SYMBOL.VENUE"
        }
    }

    #[test]
    fn walk_forward_produces_one_report_per_window() {
        // deterministic gently-trending closes so the strategy actually trades
        let bars: Vec<Bar> = (0..240)
            .map(|i| bar(i as i64 * 60_000, 100.0 + i as f64 * 0.1 + (i as f64 * 0.7).sin() * 2.0))
            .collect();
        let cash = 10_000.0;
        let rep = walk_forward_strategy(&bars, 4, WalkMode::Anchored, cash, 252.0, |window| {
            StrategyEngine::new(
                vec![("SYM".to_string(), window.to_vec())],
                BracketPerSymbol,
                EngineParams::default(),
            )
            .run()
        });
        assert_eq!(rep.windows.len(), 4);
        assert!(!rep.oos_equity_curve.is_empty());
        assert!((0.0..=1.0).contains(&rep.wf_consistency));
        assert!(rep.oos_return.is_finite());
    }
}

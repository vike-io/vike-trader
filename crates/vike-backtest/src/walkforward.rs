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
/// ⚠ NOT `Copy` any more. It became a window that can carry the parameters its own search chose,
/// and an owned `Vec` cannot be `Copy`. Every consumer already took `&WfWindow`, so the loss costs
/// nothing today — but a future caller that expected an implicit copy gets a move instead.
#[derive(Clone, Debug, serde::Serialize)]
pub struct WfWindow {
    pub test_range: (usize, usize),
    pub oos_return: f64,
    /// The parameter overrides this window SELECTED, when the caller searched inside it — `None`
    /// on the fixed-parameter walk, which is every caller that does not optimize.
    ///
    /// This field is the whole diagnostic value of a walk-forward OPTIMIZATION and the reason the
    /// report grew rather than forking: a stitched OOS number tells you the procedure's result,
    /// while the per-window winners tell you whether the procedure found anything STABLE or
    /// wandered. Our own cohort study is the cautionary case — per-fold in-sample↔out-of-sample
    /// R² came out ≈ 0.01, which is only visible if each fold's choice was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen_params: Option<Vec<(String, toml::Value)>>,
}

/// What a window's closure hands back: the run, plus what it CHOSE if it chose anything.
///
/// A struct rather than a tuple because the second field is easy to mis-order and easy to fill in
/// wrong; [`WindowOutcome::fixed`] is the spelling for a caller that does not search, and reads as
/// the claim it makes.
#[derive(Debug)]
pub struct WindowOutcome {
    pub result: BacktestResult,
    pub chosen_params: Option<Vec<(String, toml::Value)>>,
}

impl WindowOutcome {
    /// A window run at FIXED parameters — the stability-check walk, and every pre-optimization
    /// caller. Records no choice because none was made.
    pub fn fixed(result: BacktestResult) -> Self {
        Self { result, chosen_params: None }
    }
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
    mut run_window: impl FnMut(&[Bar], &[Bar]) -> WindowOutcome,
) -> WalkForwardReport {
    let splits = walk_forward_splits(bars.len(), n_splits, mode);
    let mut windows: Vec<WfWindow> = Vec::with_capacity(splits.len());
    let mut stitched: Vec<f64> = Vec::new();
    let mut equity = cash;

    for sp in &splits {
        // BOTH halves are handed over. The train slice is what a caller SEARCHES on; a caller that
        // does not search ignores it, which is every pre-optimization caller and costs nothing (a
        // slice is a borrow, not a copy).
        //
        // ⚠ This is also what makes `WalkMode::Rolling` mean anything. Until the train half was
        // passed, this loop read only `test_start`/`test_end`, and the two modes differ in
        // `train_start` ALONE — so `Rolling` and `Anchored` returned byte-identical reports and the
        // mode parameter was decorative.
        let outcome =
            run_window(&bars[sp.train_start..sp.train_end], &bars[sp.test_start..sp.test_end]);
        let WindowOutcome { result: oos, chosen_params } = outcome;
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
            chosen_params,
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
    use crate::report::{DAILY_PERIODS_PER_YEAR, periods_per_year_for_interval};
    use crate::{EngineParams, StrategyEngine};
    use vike_model::Bar;

    /// The bar step every fixture below is built at, spelled as the interval STRING the
    /// annualization keys on — and the reason it is a named constant rather than a literal `60_000`
    /// in one place and a literal `252.0` in another.
    ///
    /// `periods_per_year` is the count of RETURN OBSERVATIONS a year produces, one per bar, because
    /// [`crate::metrics::sharpe`] scales by its square root. `252.0` is the count for DAILY bars;
    /// over the one-minute bars this module builds it is wrong by a factor of 1,440, which
    /// understates the reported Sharpe by `sqrt(1440) ≈ 37.9x`. This test passed exactly that
    /// literal until the fix — the daily scale on minute bars, inside the runner's only in-crate
    /// exercise — so the constant and the derivation below exist to stop the next reader copying
    /// the number back out.
    const INTERVAL: &str = "1m";

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
        // The bar STEP is derived from the same interval string the annualization is, so the two
        // cannot disagree about what these bars are: a fixture stepped at 60s while claiming an
        // hourly scale would be a lie no assertion here could catch.
        let step_ms = vike_model::time::interval_ms(INTERVAL).expect("the fixture interval parses");
        // deterministic gently-trending closes so the strategy actually trades
        let bars: Vec<Bar> = (0..240)
            .map(|i| {
                let close = 100.0 + i as f64 * 0.1 + (i as f64 * 0.7).sin() * 2.0;
                bar(i as i64 * step_ms, close)
            })
            .collect();
        let cash = 10_000.0;
        // Derived, never a literal — see [`INTERVAL`]. Pinned here as a NUMBER as well, so the
        // scale this exercise runs at is readable without following the derivation.
        let periods_per_year = periods_per_year_for_interval(INTERVAL);
        assert_eq!(
            periods_per_year,
            DAILY_PERIODS_PER_YEAR * 1_440.0,
            "1,440 one-minute bars fit in a day, so a year of them is 252 * 1440 observations"
        );
        let rep = walk_forward_strategy(
            &bars,
            4,
            WalkMode::Anchored,
            cash,
            periods_per_year,
            |_train, window| {
                WindowOutcome::fixed(
                    StrategyEngine::new(
                        vec![("SYM".to_string(), window.to_vec())],
                        BracketPerSymbol,
                        EngineParams::default(),
                    )
                    .run(),
                )
            },
        );
        assert_eq!(rep.windows.len(), 4);
        assert!(!rep.oos_equity_curve.is_empty());
        assert!((0.0..=1.0).contains(&rep.wf_consistency));
        assert!(
            rep.windows.iter().all(|w| w.chosen_params.is_none()),
            "a FIXED-parameter walk records no choice — `chosen_params` is the optimizing \
             driver's field, and a `Some` here would mean the two protocols had been conflated"
        );
        assert!(rep.oos_return.is_finite());
        // The reported Sharpe is the STITCHED curve annualized at the factor the caller passed —
        // not at some literal inside the runner. Bit-exact because it is the same call over the
        // same inputs: `oos_equity_curve` IS the `stitched` vector `oos_sharpe` was computed from.
        // ⚠ `metrics::sharpe` answers 0.0 at EVERY factor for a curve with no return dispersion,
        // so this assertion is only load-bearing while the fixture above actually trades; the
        // cross-plane gate that turns the same idea into a real regression test states that
        // precondition out loud (`crates/vike-studio-core/tests/walkforward_annualization.rs`).
        assert_eq!(
            rep.oos_sharpe.to_bits(),
            sharpe(&rep.oos_equity_curve, periods_per_year).to_bits(),
            "oos_sharpe must be the stitched OOS curve annualized at the caller's factor"
        );
    }
}

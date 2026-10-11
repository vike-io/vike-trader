//! Walk-forward evaluation of a strategy: run it on each out-of-sample window and stitch
//! the OOS windows into one equity curve — the honest way to backtest, the strategy twin
//! of vike-trader-app `ml/walkforward.py` (that one wraps an ML model; this one wraps any
//! `Strategy` via a caller-supplied `run_window` closure).
//!
//! This file is the FOLD and the root of the walk-forward family: [`runner`] is the
//! profile-driven runner (`run_walkforward` and its optimizing drivers over a `BacktestProfile`),
//! and [`windows`] resolves every `[walkforward]` window form into the one list of splits the fold
//! consumes.

use crate::harness::{run, sweep};
use vike_analytics::BacktestResult;
use vike_analytics::metrics::sharpe;
use vike_analytics::validation::{Split, WalkMode, walk_forward_splits};
use vike_model::Bar;

#[cfg(doc)]
use crate::harness::{
    WalkforwardCfg, profile, run_backtest, run_paramscan, run_paramscan_with, strategy_by_name,
};

pub mod runner;
pub mod windows;

/// One out-of-sample window's outcome.
///
/// `Serialize` — and deliberately NOT `Deserialize` — so the compute-to-data
/// `RunWalkforwardProfile` verb can return this report as JSON TEXT, exactly as
/// `BacktestReport`/`ParamscanReport` already cross the datahub wire. The asymmetry keeps the wire
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

/// Run `run_window` over each window of an ALREADY-RESOLVED list and stitch the results —
/// the entry point every `[walkforward]` window form reaches
/// (`crate::walkforward::windows::resolve_windows` is what produces the list).
///
/// `cash` is the per-window starting equity (each window runs fresh at `cash`); the OOS
/// curves are rebased onto one running equity, mirroring Python `walk_forward_ml`'s stitch.
/// Contract, unchanged: each window's backtest result must start its equity curve at exactly
/// `cash`.
///
/// # ⚠ The window list is a CONTRACT, and this function validates none of it
///
/// Two properties, and the second is the one that cost something. **Ascending:** the windows are
/// consumed IN ORDER and the stitch is sequential, so a list that is not ascending in `test_start`
/// produces an equity curve whose segments are out of time order. **NON-OVERLAPPING:** consecutive
/// windows must satisfy `w[i + 1].test_start >= w[i].test_end`, because the loop below COMPOUNDS
/// every window onto one running `equity` — so an overlapping list counts the same price history
/// more than once and reports an out-of-sample return, and an `oos_sharpe`, roughly the overlap
/// factor too high. Measured: over 1,200 hourly bars a `train = "400bars"` / `test = "100bars"` /
/// `step = "50bars"` list is 51 windows spanning a union of 800 bars, stitched into a 1,500-bar
/// curve — about +16% reported where the true out-of-sample is about +8%.
///
/// **`vike_backtest::walkforward::windows::resolve_windows` is the producer that ENFORCES both**,
/// and it is the only one in this tree: it refuses `step < test` by name before a list is built.
/// This function does not REFUSE either property — it returns a report, not a `Result` — and it
/// will not silently repair one, because a quiet re-sort or de-overlap would hide a splitter bug
/// rather than surface it.
///
/// ⚠ What it does instead is `debug_assert!` both, at the top of the body: free in release, firing
/// in every test run, naming the consequence rather than the predicate. Belt and braces with this
/// paragraph, deliberately — prose is what a reader consults, and the assert is what catches the
/// author who did not.
///
/// ⚠ So a caller BUILDING a `Vec<Split>` of its own — a CLI flag surface, a future `research`
/// driver, a test — owns both properties itself. Route through `resolve_windows` if you can; if you
/// cannot, the overlap check is one comparison, and its absence is not visible in any number this
/// report carries.
pub fn walk_forward_over_windows(
    bars: &[Bar],
    windows_in: &[Split],
    cash: f64,
    periods_per_year: f64,
    mut run_window: impl FnMut(&[Bar], &[Bar]) -> WindowOutcome,
) -> WalkForwardReport {
    // ⚠ BELT AND BRACES over the contract above, and it is deliberately both. The prose is what a
    // reader consults; these are what catch the author who did not, at the moment they introduce
    // it — which is the whole failure mode, because a violation of the second property is INVISIBLE
    // in every number the report carries.
    //
    // `debug_assert!` rather than a refusal, for three reasons that are worth not re-deriving: this
    // function returns a report and not a `Result`, so refusing would mean widening the signature
    // for every caller; `walkforward::windows::resolve_windows` already refuses the condition by
    // name on every in-tree path, so a second refusal would be the second answer to one question;
    // and a `debug_assert!` costs nothing in release while firing in every test run, which is
    // exactly where a future producer's own tests live.
    //
    // Ascending is checked FIRST on purpose: a reversed list violates both, and reporting
    // "overlapping" for it would send the author after the wrong bug.
    for pair in windows_in.windows(2) {
        debug_assert!(
            pair[1].test_start > pair[0].test_start,
            "walk_forward_over_windows: the window list must be strictly ascending in `test_start` \
             — the stitch below is sequential, so an out-of-order list yields an equity curve whose \
             segments are out of time order. Got {:?} then {:?}",
            pair[0],
            pair[1]
        );
        debug_assert!(
            pair[1].test_start >= pair[0].test_end,
            "walk_forward_over_windows: the window list must be NON-OVERLAPPING \
             (test_start[i + 1] >= test_end[i]) — the loop below COMPOUNDS every window onto ONE \
             running equity, so overlapping windows count the same bars more than once and inflate \
             the reported out-of-sample return and Sharpe by roughly the overlap factor, with \
             nothing in the report showing it. Got {:?} then {:?}. Produce the list with \
             `vike_backtest::walkforward::windows::resolve_windows`, which refuses this by name.",
            pair[0],
            pair[1]
        );
    }

    let mut windows: Vec<WfWindow> = Vec::with_capacity(windows_in.len());
    let mut stitched: Vec<f64> = Vec::new();
    let mut equity = cash;

    for sp in windows_in {
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
            "walk_forward_over_windows: each window's backtest must start at `cash` ({cash}); got {:?} — the stitch rebasing assumes this",
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

/// Run `run_window` over `n_splits` evenly-chopped walk-forward windows — the SPLIT-COUNT
/// entry point, unchanged for every caller that had one.
///
/// A wrapper over [`walk_forward_over_windows`] since the duration window forms landed. The
/// signature is deliberately frozen: `vike_studio_core::run_walkforward_slice_with_params`
/// calls it, and
/// `docs/decisions/0047-window-search-is-a-profile-capability-not-a-studio-one.md` rules
/// that the Studio door does not get the new forms. `walk_forward_splits` stays gap-free
/// here — `docs/decisions/0046-the-bar-mode-walk-forward-has-no-purge.md`.
pub fn walk_forward_strategy(
    bars: &[Bar],
    n_splits: usize,
    mode: WalkMode,
    cash: f64,
    periods_per_year: f64,
    run_window: impl FnMut(&[Bar], &[Bar]) -> WindowOutcome,
) -> WalkForwardReport {
    let splits = walk_forward_splits(bars.len(), n_splits, mode);
    walk_forward_over_windows(bars, &splits, cash, periods_per_year, run_window)
}

#[path = "walkforward_tests.rs"]
#[cfg(test)]
mod walkforward_tests;

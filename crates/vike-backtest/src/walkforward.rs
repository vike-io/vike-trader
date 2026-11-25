//! Walk-forward evaluation of a strategy: run it on each out-of-sample window and stitch
//! the OOS windows into one equity curve — the honest way to backtest, the strategy twin
//! of vike-trader-app `ml/walkforward.py` (that one wraps an ML model; this one wraps any
//! `Strategy` via a caller-supplied `run_window` closure).
//!
//! This file is the FOLD and the root of the walk-forward family: [`runner`] is the
//! profile-driven runner (`run_walkforward` and its optimizing drivers over a `ProfileBacktest`),
//! [`windows`] resolves every `[walkforward]` window form into the one list of splits the fold
//! consumes, [`combinatorial`] is the opt-in combinatorial purged walk over the same fold, and `ticks` is
//! the walks' tick lane (one prepared tape, every window a borrowed range of it).

use crate::run;
use crate::search::paramscan;
use vike_analytics::results::TradingResults;
use vike_analytics::validation::{WalkforwardMode, WalkforwardSplit, walkforward_splits};
use vike_analytics::walkforward::{
    WalkforwardResults, WalkforwardResultsStep, WalkforwardWindowTimes,
};
use vike_analytics::{TradingRecord, TradingRecordSampling, TradingRecordSource};
use vike_marketdata::Bar;
use vike_model::Trade;
use vike_model::params::ParamValue;

#[cfg(doc)]
use crate::run::{
    profile::{self, ProfileWalkforward},
    run_backtest,
};
#[cfg(doc)]
use crate::search::paramscan::{run_paramscan, run_paramscan_with};

pub mod combinatorial;
pub mod runner;
// The tick lane of the walks: one prepared tape, every window a borrowed range of it.
pub(crate) mod ticks;
pub mod windows;

// ⚠ The results types live in `vike_analytics::walkforward` (`WalkforwardResults`, `WalkforwardResultsStep`):
// the desktop's Studio renders them and links no harness (docs/decisions/0078's amendment of
// 2026-11-10). This fold produces them; every call site names them there, and nothing here re-exports them.

/// What a window's closure hands back: the run, plus what it CHOSE if it chose anything.
///
/// A struct rather than a tuple because the second field is easy to mis-order and easy to fill in
/// wrong; [`WalkforwardWindowOutcome::fixed`] is the spelling for a caller that does not search, and reads as
/// the claim it makes.
#[derive(Debug)]
pub struct WalkforwardWindowOutcome {
    pub result: TradingRecord,
    pub chosen_params: Option<Vec<(String, ParamValue)>>,
}

impl WalkforwardWindowOutcome {
    /// A window run at FIXED parameters — the stability-check walk, and every pre-optimization
    /// caller. Records no choice because none was made.
    pub fn fixed(result: TradingRecord) -> Self {
        Self { result, chosen_params: None }
    }
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
/// Each window becomes a [`WalkforwardResultsStep`] whose results are the window's own record's
/// ([`TradingResults::from_record`], its source [`TradingRecordSource::WalkforwardStep`]), and the
/// walk's `overall` results are those of the STITCHED record: every window's trades rebased by the
/// factor its curve was (`size`, `pnl` and `fees` times the running equity over `cash`), the stitched
/// curve and its timestamps, funding summed as run, `final_equity` the running equity.
///
/// ⚠ **Parity with the four numbers this fold returned before protocol 9 (decision 0021).** A
/// step's `total_return` is its curve's `last / first - 1`, where the old window return was
/// `final_equity / cash - 1`, and `overall.total_return` is the stitched curve's, where the old one
/// was `equity / cash - 1`. They are the same bits exactly when each window's curve starts at `cash`
/// and ends at its `final_equity`, which is this contract and the engine's (`run_bars.rs` re-points
/// the last sample at a settlement); `overall.sharpe` is the same `metrics::sharpe` call over the same
/// stitched curve. `walkforward_parity.rs` holds the bits on every walk-forward fixture.
///
/// # ⚠ The window list is a CONTRACT, and this function validates none of it
///
/// Two properties, and the second is the one that cost something. **Ascending:** the windows are
/// consumed IN ORDER and the stitch is sequential, so a list that is not ascending in `test_start`
/// produces an equity curve whose segments are out of time order. **NON-OVERLAPPING:** consecutive
/// windows must satisfy `w[i + 1].test_start >= w[i].test_end`, because the loop below COMPOUNDS
/// every window onto one running `equity` — so an overlapping list counts the same price history
/// more than once and reports an out-of-sample return, and an `overall.sharpe`, roughly the overlap
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
/// ⚠ So a caller BUILDING a `Vec<WalkforwardSplit>` of its own — a CLI flag surface, a future `research`
/// driver, a test — owns both properties itself. Route through `resolve_windows` if you can; if you
/// cannot, the overlap check is one comparison, and its absence is not visible in any number this
/// report carries.
pub fn walk_forward_over_windows(
    bars: &[Bar],
    windows_in: &[WalkforwardSplit],
    cash: f64,
    periods_per_year: f64,
    mut run_window: impl FnMut(&[Bar], &[Bar]) -> WalkforwardWindowOutcome,
) -> WalkforwardResults {
    // BOTH halves are handed over. The train slice is what a caller SEARCHES on; a caller that
    // does not search ignores it, which is every pre-optimization caller and costs nothing (a
    // slice is a borrow, not a copy).
    //
    // ⚠ This is also what makes `WalkforwardMode::Rolling` mean anything. Until the train half was
    // passed, this loop read only `test_start`/`test_end`, and the two modes differ in
    // `train_start` ALONE — so `Rolling` and `Anchored` returned byte-identical reports and the
    // mode parameter was decorative.
    walk_forward_over_splits(Some(bars), windows_in, cash, periods_per_year, |sp| {
        run_window(&bars[sp.train_start..sp.train_end], &bars[sp.test_start..sp.test_end])
    })
}

/// [`walk_forward_over_windows`] over a window list with no bar series beside it: `run_window` is handed each
/// [`WalkforwardSplit`] itself, its ranges indexing whatever axis the list was cut on (a tick walk's seed-bar
/// grid, `walkforward::ticks`; the profile bar walks' held series, each half run on a view of it,
/// `vike_simulator::BarSeriesShared::slice`, so no window copies a bar). The same stitch, under the same
/// contract. `bars` is that axis when it is a bar series, which each step's [`WalkforwardWindowTimes`] are read
/// off; `None` (a tick walk's grid) leaves them out.
pub(crate) fn walk_forward_over_splits(
    bars: Option<&[Bar]>,
    windows_in: &[WalkforwardSplit],
    cash: f64,
    periods_per_year: f64,
    mut run_window: impl FnMut(&WalkforwardSplit) -> WalkforwardWindowOutcome,
) -> WalkforwardResults {
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

    let mut steps: Vec<WalkforwardResultsStep> = Vec::with_capacity(windows_in.len());
    // The record `overall` is composed from. It is composed as the whole walk's ONE out-of-sample span,
    // a walk-forward's, so it carries no simulator counters it could only have summed; the results
    // drop that source below (they are no one step's).
    let span = match (windows_in.first(), windows_in.last()) {
        (Some(first), Some(last)) => (first.test_start, last.test_end),
        _ => (0, 0),
    };
    let mut stitched = TradingRecord {
        source: TradingRecordSource::WalkforwardStep { window: span, params: None },
        ..TradingRecord::default()
    };
    let mut equity = cash;
    // Each window's curve kept by TIME (`TradingRecord::sampling`, a tick walk's), in window order, with the
    // stitched equity it starts at.
    let mut by_time: Vec<(f64, TradingRecordSampling)> = Vec::new();

    for sp in windows_in {
        let WalkforwardWindowOutcome { result: mut out_of_sample, chosen_params } = run_window(sp);
        by_time.extend(out_of_sample.sampling.map(|s| (equity, s)));
        debug_assert!(
            out_of_sample
                .equity_curve
                .first()
                .is_none_or(|&e0| (e0 - cash).abs() <= cash.abs() * 1e-9),
            "walk_forward_over_windows: each window's backtest must start at `cash` ({cash}); got {:?} — the stitch rebasing assumes this",
            out_of_sample.equity_curve.first()
        );
        let start = equity;
        for &v in &out_of_sample.equity_curve {
            stitched.equity_curve.push(start * (v / cash));
        }
        equity = start * (out_of_sample.final_equity / cash);
        // Summed, NOT rebased like the curve: each window's funding as run fresh at `cash`, the number
        // the report carried before protocol 9 (kept bit for bit). A search's training runs are in no
        // number here.
        stitched.funding_paid += out_of_sample.funding_paid;
        // Every window's strategy failures together; the step keeps its own.
        stitched.strategy_failures.merge(&out_of_sample.strategy_failures);
        // Each window's trades on the STITCHED scale, by the factor its curve was rebased with, so
        // every money row of `overall` (net profit, fees, the largest win, ...) agrees with the
        // stitched curve; the step keeps its own, at `cash`.
        let factor = start / cash;
        stitched.trades.extend(out_of_sample.trades.iter().map(|trade| Trade {
            size: trade.size * factor,
            pnl: trade.pnl * factor,
            fees: trade.fees * factor,
            ..trade.clone()
        }));
        stitched.n_trades += out_of_sample.n_trades;
        stitched.equity_timestamp.extend_from_slice(&out_of_sample.equity_timestamp);
        let test_range = (sp.test_start, sp.test_end);
        out_of_sample.source = TradingRecordSource::WalkforwardStep {
            window: test_range,
            params: chosen_params.clone(),
        };
        steps.push(WalkforwardResultsStep {
            test_range,
            window_times: bars.and_then(|bars| window_times(bars, sp)),
            chosen_params,
            results: TradingResults::from_record(None, &out_of_sample, periods_per_year),
        });
    }
    stitched.final_equity = equity;
    // A window that kept no timestamps would shift every later one off its sample: then none is kept.
    if stitched.equity_timestamp.len() != stitched.equity_curve.len() {
        stitched.equity_timestamp.clear();
    }
    stitched.sampling = stitched_sampling(&by_time, windows_in.len(), cash);

    let n = steps.len().max(1) as f64;
    let profitable = steps.iter().filter(|s| s.results.total_return > 0.0).count() as f64 / n;
    let mut overall = TradingResults::from_record(None, &stitched, periods_per_year);
    // The stitch of every step's record, no one record's: `WalkforwardResults::overall_label` is its
    // label, where the span source composed above would read as one step's.
    overall.source = None;
    WalkforwardResults {
        overall,
        equity_curve: stitched.equity_curve,
        equity_timestamp: stitched.equity_timestamp,
        steps,
        profitable_step_share: profitable,
        account_currency: None,
    }
}

/// The stitched record's [`TradingRecord::sampling`]: `None` unless every one of the `windows` windows kept
/// its curve by time at one step (a tick walk's, decision 0078's amendment of 2026-11-20), so `overall`
/// annualizes its returns at that step as each step does, and a bar walk's stitch is what it was. The path's
/// numbers fold the windows' (each `(start, sampling)`, `start` the stitched equity it opens at, so its events
/// stand at `start * (v / cash)`): the first zero, the events, flat only if every window was, the peak and
/// trough scaled, and the path's EXACT drawdown, which the owner keeps exact (2026-11-20). Windows kept at
/// different steps, or some by time and some not, cannot come from one profile's walk (every window runs its
/// one `engine` table); a caller of the fold that hands such a list gets a stitch of points, never a step one
/// window did not keep.
///
/// The drawdown: at an event of window `j`, the stitched path's running peak is the larger of every earlier
/// window's scaled peak and window `j`'s own running peak scaled, so the deepest drop there is the larger of
/// window `j`'s own (a drawdown does not move under a scale) and the drop from the earlier peak to window `j`'s
/// scaled trough. Exact while equity stays above zero; an account that reached zero is refused as a result
/// anyway (`TradingRecord::equity_reached_zero`).
fn stitched_sampling(
    by_time: &[(f64, TradingRecordSampling)],
    windows: usize,
    cash: f64,
) -> Option<TradingRecordSampling> {
    let &(_, first) = by_time.first()?;
    if by_time.len() != windows || by_time.iter().any(|(_, s)| s.interval_ms != first.interval_ms) {
        return None;
    }
    let mut stitched = TradingRecordSampling {
        max_drawdown: 0.0,
        max_equity: f64::NEG_INFINITY,
        min_equity: f64::INFINITY,
        reached_zero_at: None,
        events: 0,
        flat: true,
        ..first
    };
    for &(start, s) in by_time {
        let (peak, trough) = (start * (s.max_equity / cash), start * (s.min_equity / cash));
        let earlier = stitched.max_equity;
        let across = vike_math::time_series::drawdown_fraction(earlier, trough).unwrap_or(0.0);
        stitched.max_drawdown = stitched.max_drawdown.max(s.max_drawdown).max(across);
        stitched.max_equity = earlier.max(peak);
        stitched.min_equity = stitched.min_equity.min(trough);
        stitched.reached_zero_at = stitched.reached_zero_at.or(s.reached_zero_at);
        stitched.events += s.events;
        stitched.flat &= s.flat;
    }
    Some(stitched)
}

/// The open times of the first and last bar of `split`'s two halves ([`WalkforwardWindowTimes`]): `None` when its test
/// half holds no bar, and no train pair when its train half holds none (a combinatorial path's window).
fn window_times(bars: &[Bar], split: &WalkforwardSplit) -> Option<WalkforwardWindowTimes> {
    let ends = |start: usize, end: usize| {
        let half = bars.get(start..end)?;
        Some((half.first()?.timestamp, half.last()?.timestamp))
    };
    let (test_start, test_end) = ends(split.test_start, split.test_end)?;
    let train = ends(split.train_start, split.train_end);
    Some(WalkforwardWindowTimes {
        train_start: train.map(|(start, _)| start),
        train_end: train.map(|(_, end)| end),
        test_start,
        test_end,
    })
}

/// Run `run_window` over `n_splits` evenly-chopped walk-forward windows — the SPLIT-COUNT
/// entry point, unchanged for every caller that had one.
///
/// A wrapper over [`walk_forward_over_windows`] since the duration window forms landed; the
/// Studio's walk no longer calls it (it is the profile walk, `runner::run_walkforward_optimized`),
/// so its callers are the tests that drive the fold directly. `walkforward_splits` stays gap-free
/// here — `docs/decisions/0046-the-bar-mode-walk-forward-has-no-purge.md`.
pub fn walk_forward_strategy(
    bars: &[Bar],
    n_splits: usize,
    mode: WalkforwardMode,
    cash: f64,
    periods_per_year: f64,
    run_window: impl FnMut(&[Bar], &[Bar]) -> WalkforwardWindowOutcome,
) -> WalkforwardResults {
    let splits = walkforward_splits(bars.len(), n_splits, mode);
    walk_forward_over_windows(bars, &splits, cash, periods_per_year, run_window)
}

#[path = "walkforward_tests.rs"]
#[cfg(test)]
pub(crate) mod walkforward_tests;

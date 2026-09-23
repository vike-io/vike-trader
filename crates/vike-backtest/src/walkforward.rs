//! Walk-forward evaluation of a strategy: run it on each out-of-sample window and stitch
//! the OOS windows into one equity curve — the honest way to backtest, the strategy twin
//! of vike-trader-app `ml/walkforward.py` (that one wraps an ML model; this one wraps any
//! `Strategy` via a caller-supplied `run_window` closure).

use crate::metrics::sharpe;
use crate::result::BacktestResult;
use crate::validation::{Split, WalkMode, walk_forward_splits};
use vike_model::Bar;

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
/// (`crate::harness::windows::resolve_windows` is what produces the list).
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
/// **`vike_backtest::harness::windows::resolve_windows` is the producer that ENFORCES both**, and
/// it is the only one in this tree: it refuses `step < test` by name before a list is built. This
/// function does not REFUSE either property — it returns a report, not a `Result` — and it will not
/// silently repair one, because a quiet re-sort or de-overlap would hide a splitter bug rather than
/// surface it.
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
    // `debug_assert!` rather than a refusal, for three reasons that are worth not re-deriving:
    // this function returns a report and not a `Result`, so refusing would mean widening the
    // signature for every caller; `harness::windows::resolve_windows` already refuses the condition
    // by name on every in-tree path, so a second refusal would be the second answer to one
    // question; and a `debug_assert!` costs nothing in release while firing in every test run,
    // which is exactly where a future producer's own tests live.
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
             `vike_backtest::harness::windows::resolve_windows`, which refuses this by name.",
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

    /// `n` deterministic gently-trending bars stepped at [`INTERVAL`].
    ///
    /// The bar STEP is derived from the same interval string the annualization is, so the two
    /// cannot disagree about what these bars are: a fixture stepped at 60s while claiming an
    /// hourly scale would be a lie no assertion here could catch.
    fn trending_bars(n: usize) -> Vec<Bar> {
        let step_ms = vike_model::time::interval_ms(INTERVAL).expect("the fixture interval parses");
        (0..n)
            .map(|i| {
                let close = 100.0 + i as f64 * 0.1 + (i as f64 * 0.7).sin() * 2.0;
                bar(i as i64 * step_ms, close)
            })
            .collect()
    }

    /// One window run at the fixture's fixed parameters — the closure body both runner tests
    /// hand to their walk, lifted so the two cannot drift into measuring different programs.
    fn window_outcome(window: &[Bar]) -> WindowOutcome {
        WindowOutcome::fixed(
            StrategyEngine::new(
                vec![("SYM".to_string(), window.to_vec())],
                BracketPerSymbol,
                EngineParams::default(),
            )
            .run(),
        )
    }

    #[test]
    fn walk_forward_produces_one_report_per_window() {
        let bars = trending_bars(240);
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
            |_train, window| window_outcome(window),
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

    /// The split-count entry point and the window-list entry point are ONE walk: the first is a
    /// wrapper over the second. Pinned because the window forms landing in `[walkforward]` route
    /// through the second, and a divergence would make a `n_splits = 4` profile and a
    /// four-window list report different numbers over identical bars.
    ///
    /// ⚠ The `cash` handed to both runners is `10_000.0` because that is what
    /// [`EngineParams::default`] starts a window at, and the stitch `debug_assert`s the two are
    /// equal — a smaller number here fires that assertion rather than measuring anything.
    ///
    /// ⚠ **Read what this does and does not prove.** After the split it is close to a tautology:
    /// [`walk_forward_strategy`] IS `walk_forward_splits` plus [`walk_forward_over_windows`], and
    /// this test recomputes the same splits and calls the same runner. What it can still catch is
    /// the wrapper being rewired — a different `mode`, a dropped argument, a re-sorted list — and
    /// that is the whole of its power. It is NOT evidence that either path is numerically right;
    /// `crates/vike-backtest/tests/walkforward_windows.rs`'s literal pin is where the numbers are
    /// held against measured values.
    #[test]
    fn the_split_count_wrapper_and_the_window_runner_agree() {
        let bars = trending_bars(200);
        let mode = WalkMode::Anchored;
        let ppy = periods_per_year_for_interval(INTERVAL);
        let via_count =
            walk_forward_strategy(&bars, 4, mode, 10_000.0, ppy, |_t, w| window_outcome(w));
        let splits = crate::validation::walk_forward_splits(bars.len(), 4, mode);
        let via_windows =
            walk_forward_over_windows(&bars, &splits, 10_000.0, ppy, |_t, w| window_outcome(w));
        assert_eq!(via_count.windows.len(), via_windows.windows.len());
        for (a, b) in via_count.windows.iter().zip(&via_windows.windows) {
            assert_eq!(a.test_range, b.test_range);
            assert_eq!(a.oos_return.to_bits(), b.oos_return.to_bits());
        }
        assert_eq!(via_count.oos_sharpe.to_bits(), via_windows.oos_sharpe.to_bits());
        assert_eq!(via_count.wf_consistency, via_windows.wf_consistency);
    }

    /// The NON-OVERLAP half of [`walk_forward_over_windows`]'s contract is ASSERTED, not merely
    /// documented — and this is the test that would have caught the stage-9 blocker if the
    /// producer-side refusal had never been written.
    ///
    /// An overlapping list counts the same bars into one compounded equity curve. Measured at the
    /// time: 1,200 hourly bars with `train = "400bars"` / `test = "100bars"` / `step = "50bars"`
    /// is 51 windows over a union of 800 bars, stitched into a 1,500-bar curve — about +16%
    /// reported where the true out-of-sample is about +8%.
    ///
    /// ⚠ `#[cfg(debug_assertions)]` because the assertion it exercises is a `debug_assert!`: a
    /// `--release` test run compiles the assert out, and this test would then fail for the wrong
    /// reason.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "NON-OVERLAPPING")]
    fn an_overlapping_window_list_trips_the_stitch_contract() {
        let bars = trending_bars(200);
        // Ascending in `test_start`, so the FIRST assert passes and the overlap one is what fires.
        let overlapping = vec![
            Split { train_start: 0, train_end: 50, test_start: 50, test_end: 100 },
            Split { train_start: 0, train_end: 75, test_start: 75, test_end: 125 },
        ];
        walk_forward_over_windows(
            &bars,
            &overlapping,
            10_000.0,
            periods_per_year_for_interval(INTERVAL),
            |_t, w| window_outcome(w),
        );
    }

    /// …and the ASCENDING half likewise. A descending list violates both properties, so the order
    /// of the two asserts decides the message — ascending is checked first, deliberately, because
    /// it is the more basic claim and reporting "overlapping" for a reversed list would send the
    /// author after the wrong bug.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "strictly ascending")]
    fn a_non_ascending_window_list_trips_the_stitch_contract() {
        let bars = trending_bars(200);
        let descending = vec![
            Split { train_start: 0, train_end: 100, test_start: 100, test_end: 150 },
            Split { train_start: 0, train_end: 0, test_start: 0, test_end: 50 },
        ];
        walk_forward_over_windows(
            &bars,
            &descending,
            10_000.0,
            periods_per_year_for_interval(INTERVAL),
            |_t, w| window_outcome(w),
        );
    }
}

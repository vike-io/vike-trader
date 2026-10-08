use super::*;
use vike_analytics::report::{DAILY_PERIODS_PER_YEAR, periods_per_year_for_interval};
use vike_model::Bar;
use vike_sim::{BracketPerSymbol, EngineParams, StrategyEngine};

/// The bar step every fixture below is built at, spelled as the interval STRING the
/// annualization keys on — and the reason it is a named constant rather than a literal `60_000`
/// in one place and a literal `252.0` in another.
///
/// `periods_per_year` is the count of RETURN OBSERVATIONS a year produces, one per bar, because
/// [`vike_analytics::metrics::sharpe`] scales by its square root. `252.0` is the count for DAILY bars;
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
    let via_count = walk_forward_strategy(&bars, 4, mode, 10_000.0, ppy, |_t, w| window_outcome(w));
    let splits = vike_analytics::validation::walk_forward_splits(bars.len(), 4, mode);
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

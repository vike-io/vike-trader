use super::*;
use vike_analytics::metrics::sharpe;
use vike_analytics::results::{DAILY_PERIODS_PER_YEAR, periods_per_year_for_interval};
use vike_marketdata::Bar;
use vike_simulator::test_support::fixtures::BracketPerSymbol;
use vike_simulator::{StrategyEngine, StrategyEngineConfig};

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
        timestamp: ts,
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
    let step_ms =
        vike_marketdata::interval::interval_ms(INTERVAL).expect("the fixture interval parses");
    (0..n)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.1 + (i as f64 * 0.7).sin() * 2.0;
            bar(i as i64 * step_ms, close)
        })
        .collect()
}

/// One window run at the fixture's fixed parameters — the closure body both runner tests
/// hand to their walk, lifted so the two cannot drift into measuring different programs.
fn window_outcome(window: &[Bar]) -> WalkforwardWindowOutcome {
    WalkforwardWindowOutcome::fixed(
        StrategyEngine::new(
            vec![("SYM".to_string(), window.to_vec())],
            BracketPerSymbol,
            StrategyEngineConfig::default(),
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
        WalkforwardMode::Anchored,
        cash,
        periods_per_year,
        |_train, window| window_outcome(window),
    );
    assert_eq!(rep.steps.len(), 4);
    assert!(!rep.equity_curve.is_empty());
    assert!((0.0..=1.0).contains(&rep.profitable_step_share));
    assert!(
        rep.steps.iter().all(|w| w.chosen_params.is_none()),
        "a FIXED-parameter walk records no choice — `chosen_params` is the optimizing \
             driver's field, and a `Some` here would mean the two protocols had been conflated"
    );
    assert!(rep.overall.total_return.is_finite());
    // The reported Sharpe is the STITCHED curve annualized at the factor the caller passed —
    // not at some literal inside the runner. Bit-exact because it is the same call over the
    // same inputs: `equity_curve` IS the stitched record's curve `overall.sharpe` was computed from.
    // ⚠ `metrics::sharpe` answers 0.0 at EVERY factor for a curve with no return dispersion,
    // so this assertion is only load-bearing while the fixture above actually trades; the
    // cross-plane gate that turns the same idea into a real regression test states that
    // precondition out loud (`crates/vike-backtesthub/tests/walkforward_annualization.rs`).
    assert_eq!(
        rep.overall.sharpe.to_bits(),
        sharpe(&rep.equity_curve, periods_per_year).to_bits(),
        "overall.sharpe must be the stitched OOS curve annualized at the caller's factor"
    );
}

/// Each step's window times are the open times of its halves' first and last bars, inclusive; the
/// stitched curve carries one timestamp a sample, in time order, inside the tested bars; a window with
/// no train half (a combinatorial path's) has its test pair alone.
#[test]
fn each_step_carries_its_windows_bar_times_and_the_stitched_curve_its_timestamps() {
    let bars = trending_bars(240);
    let periods_per_year = periods_per_year_for_interval(INTERVAL);
    let splits = walkforward_splits(bars.len(), 4, WalkforwardMode::Rolling);
    let rep = walk_forward_over_windows(&bars, &splits, 10_000.0, periods_per_year, |_, window| {
        window_outcome(window)
    });
    for (step, split) in rep.steps.iter().zip(&splits) {
        let at = |i: usize| bars[i].timestamp;
        let expected = WalkforwardWindowTimes {
            train_start: Some(at(split.train_start)),
            train_end: Some(at(split.train_end - 1)),
            test_start: at(split.test_start),
            test_end: at(split.test_end - 1),
        };
        assert_eq!(step.window_times, Some(expected), "{split:?}");
    }
    let stamps = &rep.equity_timestamp;
    assert_eq!(stamps.len(), rep.equity_curve.len());
    assert!(stamps.windows(2).all(|pair| pair[0] <= pair[1]), "{stamps:?}");
    let tested = bars[splits[0].test_start].timestamp..=bars[bars.len() - 1].timestamp;
    assert!(stamps.iter().all(|t| tested.contains(t)), "{stamps:?}");

    let no_train =
        [WalkforwardSplit { train_start: 60, train_end: 60, test_start: 60, test_end: 120 }];
    let rep =
        walk_forward_over_windows(&bars, &no_train, 10_000.0, periods_per_year, |_, window| {
            window_outcome(window)
        });
    let times = rep.steps[0].window_times.expect("the test half holds bars");
    assert_eq!((times.train_start, times.train_end), (None, None));
    assert_eq!((times.test_start, times.test_end), (bars[60].timestamp, bars[119].timestamp));
}

/// **The fold before backtesthub protocol 9, kept as the parity REFERENCE** (decision 0021: the
/// bits never move, and no tolerance is widened to make them agree). It returned, per window,
/// `final_equity / cash - 1`, and for the walk the running equity's `equity / cash - 1`, the Sharpe
/// of the stitched curve and the share of windows whose return was positive. Each step's
/// `final_equity` is its record's, copied by `TradingResults::from_record`, so the reference is
/// computed from the new results' own inputs.
///
/// Called on every walk-forward fixture this crate's tests produce (the fold's here, the profile
/// runner's in `walkforward/runner_tests.rs`, the combinatorial paths in
/// `walkforward/combinatorial_tests.rs`).
pub(crate) fn assert_bit_equal_to_the_fold_before_protocol_nine(
    rep: &WalkforwardResults,
    cash: f64,
    periods_per_year: f64,
) {
    let mut equity = cash;
    let mut profitable = 0usize;
    for (i, step) in rep.steps.iter().enumerate() {
        let window_return = (step.results.final_equity / cash) - 1.0;
        assert_eq!(
            step.results.total_return.to_bits(),
            window_return.to_bits(),
            "step {i} {:?}: total_return {} vs the old window return {window_return}",
            step.test_range,
            step.results.total_return
        );
        profitable += usize::from(window_return > 0.0);
        equity *= step.results.final_equity / cash;
    }
    assert_eq!(rep.overall.final_equity.to_bits(), equity.to_bits(), "the running equity");
    let walk_return = (equity / cash) - 1.0;
    assert_eq!(
        rep.overall.total_return.to_bits(),
        walk_return.to_bits(),
        "overall.total_return {} vs the old out-of-sample return {walk_return}",
        rep.overall.total_return
    );
    assert_eq!(
        rep.overall.sharpe.to_bits(),
        sharpe(&rep.equity_curve, periods_per_year).to_bits(),
        "overall.sharpe is the old Sharpe of the stitched curve"
    );
    let share = profitable as f64 / rep.steps.len().max(1) as f64;
    assert_eq!(rep.profitable_step_share.to_bits(), share.to_bits(), "the old consistency");
}

/// The parity reference over the fold's own fixtures: both walk modes, two split counts, and a
/// flat market whose every window returns exactly zero (the `> 0` edge of the share).
#[test]
fn the_folds_fixtures_are_bit_equal_to_the_fold_before_protocol_nine() {
    let ppy = periods_per_year_for_interval(INTERVAL);
    for (n, splits) in [(240, 4), (200, 4), (300, 6)] {
        for mode in [WalkforwardMode::Anchored, WalkforwardMode::Rolling] {
            let rep =
                walk_forward_strategy(&trending_bars(n), splits, mode, 10_000.0, ppy, |_t, w| {
                    window_outcome(w)
                });
            assert_eq!(rep.steps.len(), splits);
            assert_bit_equal_to_the_fold_before_protocol_nine(&rep, 10_000.0, ppy);
        }
    }
    let flat: Vec<Bar> = (0..120).map(|i| bar(i * 60_000, 100.0)).collect();
    let rep = walk_forward_strategy(&flat, 3, WalkforwardMode::Anchored, 10_000.0, ppy, |_t, w| {
        window_outcome(w)
    });
    assert_bit_equal_to_the_fold_before_protocol_nine(&rep, 10_000.0, ppy);
}

/// One window's record: from 1,000 to 1,100 with one closed trade of +100 gross and 2 of fees.
fn a_hundred_up() -> TradingRecord {
    let trade = vike_model::Trade {
        entry_price: 100.0,
        exit_price: 110.0,
        size: 10.0,
        pnl: 100.0,
        fees: 2.0,
        entry_timestamp: 0,
        exit_timestamp: 60_000,
        symbol: "SYM".to_string(),
        mae: 0.0,
        mfe: 0.0,
        is_long: true,
    };
    TradingRecord {
        equity_curve: vec![1000.0, 1100.0],
        final_equity: 1100.0,
        trades: vec![trade],
        n_trades: 1,
        ..TradingRecord::default()
    }
}

/// A tick walk's windows keep their curves by time (`TradingRecord::sampling`), and so does the stitch:
/// `overall` reads its returns at their one step and its drawdown is the worse of each window's whole path
/// and the stitched closes'. A window kept another way (a bar window's `None`, another step) leaves the
/// stitch a curve of points, as a bar walk's always is.
#[test]
fn a_stitch_of_windows_kept_by_time_is_kept_by_time() {
    let bars: Vec<Bar> = (0..4).map(|i| bar(i * 60_000, 100.0)).collect();
    let splits = [
        WalkforwardSplit { train_start: 0, train_end: 0, test_start: 0, test_end: 2 },
        WalkforwardSplit { train_start: 0, train_end: 2, test_start: 2, test_end: 4 },
    ];
    let kept = |interval_ms: i64, max_drawdown: f64, events: usize| TradingRecordSampling {
        interval_ms,
        max_drawdown,
        max_equity: 1100.0,
        min_equity: 1000.0,
        reached_zero_at: None,
        events,
        flat: false,
    };
    let walk = |second: Option<TradingRecordSampling>| {
        let mut sampled = [Some(kept(60_000, 0.25, 7)), second].into_iter();
        walk_forward_over_windows(&bars, &splits, 1000.0, 252.0, |_t, _w| {
            let sampling = sampled.next().flatten();
            WalkforwardWindowOutcome::fixed(TradingRecord { sampling, ..a_hundred_up() })
        })
    };
    let rep = walk(Some(kept(60_000, 0.125, 5)));
    assert_eq!(rep.overall.returns_interval_ms, Some(60_000));
    assert_eq!(rep.overall.max_drawdown, 0.25, "the deepest window's whole path");
    assert!(rep.steps.iter().all(|s| s.results.returns_interval_ms == Some(60_000)));
    for other in [None, Some(kept(3_600_000, 0.125, 5))] {
        assert_eq!(walk(other).overall.returns_interval_ms, None, "{other:?}");
    }
}

/// The stitch's drawdown is the stitched PATH's, exactly: window 1 peaks at 1,500 between its closes (1,000 then
/// 1,250) and window 2, opening at the stitched 1,250 (x1.25), troughs at its own 500 between its closes (1,000
/// both), 625 stitched. No close sees either, and each window alone fell 1/6 and 1/2, so the walk's drawdown is
/// the drop from the first window's peak to the second's trough, (1,500 - 625) / 1,500: what one run walking
/// that stitched path (1,000, 1,500, 1,250 | 1,250, 625, 1,250) reads.
#[test]
fn a_stitch_reads_the_drop_from_one_windows_peak_to_a_later_windows_trough() {
    let bars: Vec<Bar> = (0..4).map(|i| bar(i * 60_000, 100.0)).collect();
    let splits = [
        WalkforwardSplit { train_start: 0, train_end: 0, test_start: 0, test_end: 2 },
        WalkforwardSplit { train_start: 0, train_end: 2, test_start: 2, test_end: 4 },
    ];
    // (closes, the path's peak, trough and own drawdown)
    let windows = [
        ([1000.0, 1250.0], 1500.0, 1000.0, 250.0 / 1500.0),
        ([1000.0, 1000.0], 1000.0, 500.0, 0.5),
    ];
    let mut each = windows.iter();
    let rep = walk_forward_over_windows(&bars, &splits, 1000.0, 252.0, |_t, _w| {
        let &(closes, max_equity, min_equity, max_drawdown) = each.next().unwrap();
        let sampling = TradingRecordSampling {
            interval_ms: 60_000,
            max_drawdown,
            max_equity,
            min_equity,
            reached_zero_at: None,
            events: 3,
            flat: false,
        };
        let record = TradingRecord {
            equity_curve: closes.to_vec(),
            final_equity: closes[1],
            sampling: Some(sampling),
            ..TradingRecord::default()
        };
        WalkforwardWindowOutcome::fixed(record)
    });
    assert_eq!(rep.equity_curve, vec![1000.0, 1250.0, 1250.0, 1250.0], "no close sees the drop");
    let path = [1000.0, 1500.0, 1250.0, 1250.0, 625.0, 1250.0];
    let walked = vike_analytics::metrics::max_drawdown(&path);
    assert_eq!(walked.to_bits(), ((1500.0 - 625.0) / 1500.0_f64).to_bits(), "the hand count");
    assert_eq!(
        rep.overall.max_drawdown.to_bits(),
        walked.to_bits(),
        "the stitched path's drawdown"
    );
}

/// ⚠ **Every money row of `overall` is on the stitched scale.** Cash 1,000, two windows of one +100
/// trade each: the stitched curve compounds to 1,210 (21%), so the second window's trade is booked
/// at 1.1x — `net_profit` 210, not 200 — and so are its fees, its size and the largest win. Each
/// step keeps its own trade at `cash`.
#[test]
fn overall_books_every_steps_trades_at_its_compounding_factor() {
    let bars: Vec<Bar> = (0..4).map(|i| bar(i * 60_000, 100.0)).collect();
    let splits = [
        WalkforwardSplit { train_start: 0, train_end: 0, test_start: 0, test_end: 2 },
        WalkforwardSplit { train_start: 0, train_end: 2, test_start: 2, test_end: 4 },
    ];
    let rep = walk_forward_over_windows(&bars, &splits, 1000.0, 252.0, |_t, _w| {
        WalkforwardWindowOutcome::fixed(a_hundred_up())
    });
    let factor: f64 = 1100.0 / 1000.0;
    assert_eq!(rep.overall.final_equity.to_bits(), 1210.0_f64.to_bits());
    assert_eq!(rep.equity_curve, vec![1000.0, 1100.0, 1100.0, 1210.0]);
    let extended = rep.overall.extended.as_ref().expect("overall composes the block");
    let second = 100.0 * factor;
    assert_eq!(extended.net_profit.to_bits(), (100.0 + second).to_bits(), "210, not 200");
    assert_eq!(extended.gross_profit.to_bits(), (100.0 + second).to_bits());
    assert_eq!(extended.largest_win.to_bits(), second.to_bits());
    assert_eq!(extended.total_fees.to_bits(), (2.0 + 2.0 * factor).to_bits());
    assert_eq!(extended.expected_payoff.to_bits(), ((100.0 + second) / 2.0).to_bits());
    for step in &rep.steps {
        let own = step.results.extended.as_ref().expect("a step's own block");
        assert_eq!(own.net_profit, 100.0, "a step stays on its own scale");
        assert_eq!(own.total_fees, 2.0);
    }
}

/// Each step's results are its own record's, the walk's `overall` the stitched record's: the trades
/// of every step, the funding summed, and the stitched curve with its timestamps.
#[test]
fn each_step_reports_its_own_record_and_overall_the_stitched_one() {
    let bars = trending_bars(240);
    let ppy = periods_per_year_for_interval(INTERVAL);
    let mut records: Vec<TradingRecord> = Vec::new();
    let rep = walk_forward_strategy(&bars, 4, WalkforwardMode::Anchored, 10_000.0, ppy, |_t, w| {
        let outcome = window_outcome(w);
        records.push(outcome.result.clone());
        outcome
    });
    assert_eq!(records.len(), rep.steps.len());
    for (step, record) in rep.steps.iter().zip(&records) {
        assert_eq!(step.results.final_equity.to_bits(), record.final_equity.to_bits());
        assert_eq!(step.results.n_trades, record.n_trades);
        assert_eq!(
            step.results.source,
            Some(TradingRecordSource::WalkforwardStep { window: step.test_range, params: None })
        );
    }
    let trades: usize = records.iter().map(|r| r.n_trades).sum();
    assert_eq!(rep.overall.n_trades, trades, "overall counts every step's trades");
    assert_eq!(rep.equity_curve.len(), records.iter().map(|r| r.equity_curve.len()).sum::<usize>());
    let period = rep.overall.period.expect("the stitched record keeps its timestamps");
    assert_eq!(period.from_ms, records[0].equity_timestamp[0]);
    assert_eq!(period.to_ms, *records[3].equity_timestamp.last().unwrap());
    // The labels: a step reads as its window, the whole walk as the OVERALL result, never as a step.
    let (a, b) = rep.steps[0].test_range;
    assert_eq!(rep.steps[0].results.source_label(), Some(format!("walk-forward step [{a}, {b})")));
    assert_eq!(rep.overall.source, None, "the stitch is no one record's");
    assert_eq!(rep.overall.source_label(), None);
    let end = rep.steps[3].test_range.1;
    assert_eq!(rep.overall_label(), Some(format!("walk-forward overall [{a}, {end})")));
}

/// The split-count entry point and the window-list entry point are ONE walk: the first is a
/// wrapper over the second. Pinned because the window forms landing in `[walkforward]` route
/// through the second, and a divergence would make a `n_splits = 4` profile and a
/// four-window list report different numbers over identical bars.
///
/// ⚠ The `cash` handed to both runners is `10_000.0` because that is what
/// [`StrategyEngineConfig::default`] starts a window at, and the stitch `debug_assert`s the two are
/// equal — a smaller number here fires that assertion rather than measuring anything.
///
/// ⚠ **Read what this does and does not prove.** After the split it is close to a tautology:
/// [`walk_forward_strategy`] IS `walkforward_splits` plus [`walk_forward_over_windows`], and
/// this test recomputes the same splits and calls the same runner. What it can still catch is
/// the wrapper being rewired — a different `mode`, a dropped argument, a re-sorted list — and
/// that is the whole of its power. It is NOT evidence that either path is numerically right;
/// `crates/vike-backtest/tests/walkforward_windows.rs`'s literal pin is where the numbers are
/// held against measured values.
#[test]
fn the_split_count_wrapper_and_the_window_runner_agree() {
    let bars = trending_bars(200);
    let mode = WalkforwardMode::Anchored;
    let ppy = periods_per_year_for_interval(INTERVAL);
    let via_count = walk_forward_strategy(&bars, 4, mode, 10_000.0, ppy, |_t, w| window_outcome(w));
    let splits = vike_analytics::validation::walkforward_splits(bars.len(), 4, mode);
    let via_windows =
        walk_forward_over_windows(&bars, &splits, 10_000.0, ppy, |_t, w| window_outcome(w));
    assert_eq!(via_count.steps.len(), via_windows.steps.len());
    for (a, b) in via_count.steps.iter().zip(&via_windows.steps) {
        assert_eq!(a.test_range, b.test_range);
        assert_eq!(a.results.total_return.to_bits(), b.results.total_return.to_bits());
    }
    assert_eq!(via_count.overall.sharpe.to_bits(), via_windows.overall.sharpe.to_bits());
    assert_eq!(via_count.profitable_step_share, via_windows.profitable_step_share);
}

/// [`walk_forward_over_windows`] of `windows` over 200 trending bars: the stitch the two contract
/// tests below feed a broken window list.
#[cfg(debug_assertions)]
fn stitch(windows: &[WalkforwardSplit]) {
    walk_forward_over_windows(
        &trending_bars(200),
        windows,
        10_000.0,
        periods_per_year_for_interval(INTERVAL),
        |_t, w| window_outcome(w),
    );
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
    // Ascending in `test_start`, so the FIRST assert passes and the overlap one is what fires.
    let overlapping = vec![
        WalkforwardSplit { train_start: 0, train_end: 50, test_start: 50, test_end: 100 },
        WalkforwardSplit { train_start: 0, train_end: 75, test_start: 75, test_end: 125 },
    ];
    stitch(&overlapping);
}

/// …and the ASCENDING half likewise. A descending list violates both properties, so the order
/// of the two asserts decides the message — ascending is checked first, deliberately, because
/// it is the more basic claim and reporting "overlapping" for a reversed list would send the
/// author after the wrong bug.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "strictly ascending")]
fn a_non_ascending_window_list_trips_the_stitch_contract() {
    let descending = vec![
        WalkforwardSplit { train_start: 0, train_end: 100, test_start: 100, test_end: 150 },
        WalkforwardSplit { train_start: 0, train_end: 0, test_start: 0, test_end: 50 },
    ];
    stitch(&descending);
}

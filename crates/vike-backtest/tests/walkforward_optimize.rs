//! The walk-forward OPTIMIZING driver from OUTSIDE the crate: both of its doors, over a store that
//! is not DataFusion, reached by the paths a downstream caller actually has.
//!
//! `harness::walkforward`'s own unit tests already gate the knob parsing, the pre-flight refusals,
//! the control-equals-the-fixed-walk equality through the profile-driven door, and the mode pair
//! (invariant for the fixed walk, decisive once a window searches). This file deliberately does not
//! restate those. What it adds is the part an in-crate `super::` test cannot reach:
//!
//! * **The public surface.** Every call below goes through `vike_backtest::harness::walkforward::…`
//!   as a dependent crate sees it, so a symbol that stopped being `pub` — or a module that stopped
//!   being re-exported — fails here while every unit test still passes.
//! * **A store that is not `DataFusionHist`.** Every store-backed unit test in that module opens a
//!   temp-dir Parquet store behind `datafusion-store`; these run over `vike_data::MemHistStore`,
//!   whose bar seam is REAL storage (it honours the requested range and returns rows ts-ascending)
//!   rather than the inert stub its neighbours' comments still describe it as. The drivers claim to
//!   compile and run against the `HistStore` TRAIT with no Arrow tree in sight; this file is what
//!   spends that claim.
//! * **The `_with` door's own refusals.** `BacktestProfile::validate` refuses `search = "sweep"`
//!   beside no `[sweep]` table at LOAD, which covers every profile that came through the parser —
//!   and leaves the driver's runtime twin, the one that catches a CALLER passing
//!   `WindowSearch::Sweep` over a profile whose table says otherwise, gated by nothing.
//! * **A window that could evaluate nothing**, from both sides: candidates that fail to compile,
//!   and candidates that run but that the objective cannot rank.
//!
//! The two protocols, so nothing below blurs them: `run_walkforward` asks whether FIXED parameters
//! were stable out of sample; the optimizing driver asks whether the PROCEDURE of fit-then-trade
//! survives out of sample, each window scoring the `[sweep]` grid on its own train half and
//! carrying only the winner onto its validation half. `WindowSearch::None` is the CONTROL and a
//! first-class mode rather than an omitted flag — our own cohort study measured "no search at all"
//! tying every searched criterion inside one seed's noise band at roughly a ninth of the cost, so
//! an optimizer whose control is not run beside it produces confident numbers with no resolving
//! power.
//!
//! ⚠ Rides `hist-replay` alone, but `scripts/ci_feature_suite.sh`'s `backtest-hist-replay` arm runs
//! that build through CLIPPY ONLY — so what EXECUTES this file is that same arm's second lane,
//! `cargo test -p vike-backtest --features datafusion-store`, which implies `hist-replay`. Both
//! lanes see it; only one runs it.
#![cfg(feature = "hist-replay")]

use std::sync::Arc;

use vike_backtest::harness::run_walkforward;
use vike_backtest::harness::walkforward::{
    WindowSearch, run_walkforward_optimized, run_walkforward_optimized_with,
};
use vike_backtest::harness::{BacktestProfile, BacktestReport, HarnessError, expand_sweep};
use vike_backtest::validation::WalkMode;
use vike_backtest::walkforward::walk_forward_strategy;
use vike_backtest::walkforward::{WalkForwardReport, WfWindow, WindowOutcome};
use vike_backtest::{BacktestResult, Objective};
use vike_data::{HistStore, MemHistStore};
use vike_model::Bar;

const VENUE: &str = "test";
const SYMBOL: &str = "TESTUSDT";
const INTERVAL: &str = "1d";
/// Per-window starting equity: the number the synthetic mode test rebases against, and the same
/// one the fixture profile's `[engine] cash` carries, so both halves of this file are on one scale.
const CASH: f64 = 1000.0;
/// Bars in the fixture slice. With `n_splits = 4` the splitter's chunk is `250 / (4 + 1) = 50`, so
/// the four out-of-sample windows are exactly `[50,100) [100,150) [150,200) [200,250)` — numbers
/// this file pins rather than recomputes, because a test that derives its expectation from the code
/// under test agrees with that code however wrong both are.
const BARS: usize = 250;
const N_SPLITS: usize = 4;

/// The profile every store-backed test here starts from: ONE bar series, `buy_hold` at `size = 1.0`
/// and a four-window walk. `extra` appends further TOML TABLES (a `[sweep]` grid, typically), which
/// is why every section this constant declares is closed before the string ends.
const BASE: &str = r#"
name = "walkforward-optimize"

[data]
venue = "test"
symbols = ["TESTUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "1000000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "TESTUSDT"

[walkforward]
n_splits = 4
"#;

/// The grid the searching tests hand each window: three `size` points over one base profile.
///
/// `buy_hold` holds `size` units from the window's first bar to its last, the fixture's closes rise
/// strictly, and the profile sets neither a fee nor slippage — so a window's return is LINEAR in
/// `size` and the largest point wins every train half. That is what makes the winner a NUMBER this
/// file can pin (`4.0`) instead of merely "one of the grid's points": a search that ranked its
/// candidates backwards would still pass the membership check and fails this one.
const GRID: &str = r#"
[sweep]
size = [1.0, 2.0, 4.0]
"#;

fn profile(extra: &str) -> BacktestProfile {
    BacktestProfile::from_toml_str(&format!("{BASE}{extra}")).expect("fixture profile parses")
}

/// A flat OHLC bar — open == high == low == close, as the `harness::walkforward` unit fixture
/// builds them. Flat bars keep the entry bar's equity exactly at `cash`, which is the contract
/// `walk_forward_strategy`'s stitch `debug_assert`s on every window it folds.
fn flat_bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A store holding [`BARS`] strictly-rising bars for the fixture's one series — and the whole
/// reason this file is not a second copy of the in-crate tests: `MemHistStore` is DataFusion-free,
/// so these runs exercise the drivers against the `HistStore` trait rather than against the one
/// concrete backend. Cheap enough to build one PER RUN, which is what the tests below do, so no two
/// runs share a handle.
fn store() -> Arc<dyn HistStore + Send + Sync> {
    let store = MemHistStore::new();
    let bars: Vec<Bar> =
        (0..BARS).map(|i| flat_bar(i as i64 * 1_000, 100.0 + i as f64 * 0.1)).collect();
    store.append_bars(VENUE, SYMBOL, INTERVAL, &bars, None).expect("the double stores bars");
    Arc::new(store)
}

/// [`run_walkforward_optimized_with`] — the OBJECTIVE door, the one a `Box<dyn Fn>` can only be
/// reached through — over a fresh fixture [`store`]. The store is the fixture here and never the
/// variable under test, so binding it away leaves each call site naming only what it varies.
fn optimized_with(
    p: &BacktestProfile,
    search: WindowSearch,
    mode: WalkMode,
    objective: &Objective,
) -> Result<WalkForwardReport, HarnessError> {
    run_walkforward_optimized_with(p, store(), search, mode, objective)
}

/// Rank by the run's fractional return — the objective the grid tests search under. A plain
/// pass-through of one report field, so a window's ranking is decided by the backtest and nothing
/// else.
fn total_return_objective() -> Objective {
    Box::new(|r: &BacktestReport| r.total_return)
}

/// The `(test_start, test_end)` index pair of each window, in order.
fn window_ranges(rep: &WalkForwardReport) -> Vec<(usize, usize)> {
    rep.windows.iter().map(|w| w.test_range).collect()
}

/// Every float a caller reads off a report, as RAW BITS: the three summary scalars, the whole
/// stitched curve, then each window's own out-of-sample return. Bits rather than `==` because two
/// runs of one computation must agree to the last bit — a tolerance here would hide exactly the
/// class of divergence the comparison exists to catch.
fn report_bits(rep: &WalkForwardReport) -> Vec<u64> {
    let mut bits =
        vec![rep.oos_return.to_bits(), rep.oos_sharpe.to_bits(), rep.wf_consistency.to_bits()];
    bits.extend(rep.oos_equity_curve.iter().map(|v| v.to_bits()));
    bits.extend(rep.windows.iter().map(|w| w.oos_return.to_bits()));
    bits
}

/// A window result that starts at [`CASH`] and ends at `final_equity` — the two-point synthetic
/// curve the mode test folds through the stitch, and the smallest thing that satisfies
/// `walk_forward_strategy`'s "each window's backtest must start at `cash`" contract.
fn synthetic_window(final_equity: f64) -> BacktestResult {
    BacktestResult { equity_curve: vec![CASH, final_equity], final_equity, ..Default::default() }
}

/// The mode test's walk: three splits over `bars`, whose window closure reads exactly ONE thing —
/// the close of the first bar of its TRAINING half — and reports it twice, as the equity it
/// synthesizes and as the choice it records. Each fixture bar's close is its own index, so a
/// window's recorded number literally names the first bar it trained on.
fn walk_reading_the_train_half(bars: &[Bar], mode: WalkMode) -> WalkForwardReport {
    walk_forward_strategy(bars, 3, mode, CASH, 252.0, |train, _test| {
        let first = train.first().expect("every split trains on at least one bar").close;
        let choice = vec![("train_first_close".to_string(), toml::Value::Float(first))];
        let result = synthetic_window(CASH * (1.0 + first / 20.0));
        WindowOutcome { result, chosen_params: Some(choice) }
    })
}

/// The one float a window of the mode test recorded, read back off the report.
fn recorded_float(w: &WfWindow) -> f64 {
    let chosen = w.chosen_params.as_ref().expect("the window recorded its choice");
    chosen[0].1.as_float().expect("recorded as a TOML float")
}

/// What each window was HANDED, as raw bits — [`recorded_float`] over the whole report.
fn trained_on(rep: &WalkForwardReport) -> Vec<u64> {
    rep.windows.iter().map(|w| recorded_float(w).to_bits()).collect()
}

/// The CONTROL is the seam's own proof: [`WindowSearch::None`] through the optimizing driver must
/// produce, bit for bit, the report the fixed-parameter `run_walkforward` produces from the same
/// profile and the same bars. If the two disagree, the bars-in refactor that let a window search
/// its train half changed what a window MEANS, and every number the optimizing driver reports is
/// measured against a moved baseline.
///
/// The in-crate twin (`the_control_reproduces_the_fixed_parameter_walk_exactly`) makes this claim
/// through the ARGUMENT-FREE door over a Parquet store; this one makes it through the OBJECTIVE
/// door over a DataFusion-free one, and adds the geometry that twin does not assert — window count,
/// the four `(test_start, test_end)` pairs, one equity sample per replayed bar.
///
/// ⚠ **This test compares two code paths, and a test shaped that way passes when both sides regress
/// together** — it is the weaker of the two shapes this file uses, and it is still the right one
/// here, because the property being pinned IS an equality between two paths and no third oracle for
/// it exists. What narrows the hole is the block of hand-computed numbers above the comparison: the
/// window count, the four pairs the splitter's `250 / (4 + 1)` chunking dictates, one sample per
/// bar, every window profitable, and a stitched return strictly above zero. A joint regression to
/// zeros, to no windows, or to a differently-sliced walk fails those before the equality is ever
/// reached; what survives is a joint regression that keeps all of it and still moves the numbers,
/// which is a narrow residual rather than an open door.
///
/// The profile deliberately CARRIES the `[sweep]` grid: the control must ignore a grid it was not
/// asked to search, and a profile with no grid at all could not tell the two apart.
#[test]
fn the_control_reproduces_the_fixed_parameter_walk_bit_for_bit() {
    let p = profile(GRID);
    let obj = total_return_objective();
    let fixed = run_walkforward(&p, store()).unwrap();

    // The hand-computed half — what the fixed walk must BE before it is worth comparing anything
    // to it.
    assert_eq!(fixed.windows.len(), N_SPLITS, "one out-of-sample window per split");
    assert_eq!(
        window_ranges(&fixed),
        vec![(50, 100), (100, 150), (150, 200), (200, 250)],
        "the anchored splitter's chunk over 250 bars is 50, and the last window runs to the end"
    );
    assert_eq!(
        fixed.oos_equity_curve.len(),
        200,
        "the bar lane records one equity sample per bar, and the four windows span 200 bars"
    );
    assert_eq!(
        fixed.wf_consistency.to_bits(),
        1.0f64.to_bits(),
        "every window of a long-only hold on a strictly rising series is profitable"
    );
    assert!(fixed.oos_return > 0.0, "the walk actually traded: {}", fixed.oos_return);

    let control = optimized_with(&p, WindowSearch::None, WalkMode::Anchored, &obj).unwrap();

    assert_eq!(control.oos_return.to_bits(), fixed.oos_return.to_bits());
    assert_eq!(control.oos_sharpe.to_bits(), fixed.oos_sharpe.to_bits());
    assert_eq!(control.wf_consistency.to_bits(), fixed.wf_consistency.to_bits());
    assert_eq!(window_ranges(&control), window_ranges(&fixed));
    assert_eq!(report_bits(&control), report_bits(&fixed), "every stitched number must agree");
    assert!(
        control.windows.iter().all(|w| w.chosen_params.is_none()),
        "the control chose nothing — a `Some` here would mean the two protocols had been conflated"
    );
    assert!(
        fixed.windows.iter().all(|w| w.chosen_params.is_none()),
        "and neither does the fixed walk, which has no search to record"
    );
}

/// A searched window must record WHAT it chose. That field is the whole diagnostic value of a
/// walk-forward optimization: the stitched number tells you the procedure's result, the per-window
/// winners tell you whether the procedure found anything stable or wandered — the distinction our
/// own cohort study needed, and could only see because each fold's choice was kept.
///
/// Three claims, ascending in strength: the field is populated under `Grid` (and, in the control
/// test above, is not under `None`); the recorded overrides are a POINT OF THE GRID, compared
/// against `expand_sweep`'s own expansion of the same profile rather than against a hand-written
/// list — the in-crate twin reads the `size` back but never asks whether it was ever ON the grid;
/// and the winner is the specific point the fixture's arithmetic demands — see [`GRID`] for why
/// `4.0` is a derivable number here and not a value copied out of a passing run.
#[test]
fn every_searched_window_records_a_point_of_the_grid_it_searched() {
    let p = profile(GRID);
    let obj = total_return_objective();
    let points = expand_sweep(&p).expect("the grid expands");
    assert_eq!(points.len(), 3, "one point per `size` value");

    let searched = optimized_with(&p, WindowSearch::Sweep, WalkMode::Anchored, &obj).unwrap();
    assert_eq!(searched.windows.len(), N_SPLITS);

    for w in &searched.windows {
        let chosen = w.chosen_params.as_ref().expect("a searched window records what it chose");
        assert_eq!(chosen.len(), 1, "the grid has one axis, so a winner carries one override");
        assert_eq!(chosen[0].0, "size");
        assert!(
            points.iter().any(|pt| pt.overrides == *chosen),
            "window {:?} recorded {chosen:?}, which is not a point of the grid it searched",
            w.test_range
        );
        assert_eq!(
            chosen[0].1.as_float(),
            Some(4.0),
            "window {:?} should have taken the largest size: return is linear in it here",
            w.test_range
        );
    }
}

/// A search that cannot change the answer is not a search. The same profile, the same bars and the
/// same objective, run twice — once as the control, once over the grid — must not report the same
/// number, because the control trades the base `size = 1.0` while every window's search picks
/// `4.0`.
///
/// The direction is pinned too, and it is not decoration: `buy_hold`'s return over a window is
/// linear in `size` on this fixture, so a bigger position on a rising series compounds to a bigger
/// stitched return. An optimizing driver that reported the SMALLER number would be selecting
/// against its own objective — the failure a bare "they differ" assertion would pass.
#[test]
fn a_searched_walk_and_its_control_do_not_report_the_same_number() {
    let p = profile(GRID);
    let obj = total_return_objective();

    let control = optimized_with(&p, WindowSearch::None, WalkMode::Anchored, &obj).unwrap();
    let searched = optimized_with(&p, WindowSearch::Sweep, WalkMode::Anchored, &obj).unwrap();

    assert_ne!(
        searched.oos_return.to_bits(),
        control.oos_return.to_bits(),
        "the search picked a different point and the report did not move"
    );
    assert!(
        searched.oos_return > control.oos_return,
        "searched {} should beat the control {} on this fixture",
        searched.oos_return,
        control.oos_return
    );
    assert_eq!(
        window_ranges(&searched),
        window_ranges(&control),
        "only the parameters differ — both walks trade the same out-of-sample bars"
    );
}

/// The RUNNER's own half of the mode question: `walk_forward_strategy` must hand `Rolling` and
/// `Anchored` windows DIFFERENT training bars. The driver-level twin
/// (`the_two_walk_modes_choose_different_winners_when_a_window_searches`) proves the consequence —
/// two modes crowning different winners over one series — and this proves the cause, one layer
/// down, with no store and no strategy in the way.
///
/// ⚠ **The fixture has to make them differ, and a real strategy would not.** A window closure that
/// ignores its train half ties the two modes however the bars are shaped — the difference is not a
/// property of the data, it is a property of whether the training half is READ, which is exactly
/// why the modes returned byte-identical reports until the runner passed it. So this test drives
/// the runner through [`walk_reading_the_train_half`], whose window reads one bar and records which
/// one: the assertion is about plumbing rather than about arithmetic.
///
/// Every number is exact in binary (`1.0`, `1.25`, `1.5`, and their products with `1000.0`), which
/// is what lets the stitched return be pinned as a literal instead of as a tolerance.
#[test]
fn rolling_and_anchored_windows_are_handed_different_training_bars() {
    // 20 bars, 3 splits => chunk 5. Anchored trains from bar 0 every time; rolling trains on the
    // one chunk before its window: [0,5) [5,10) [10,15).
    let bars: Vec<Bar> = (0..20).map(|i| flat_bar(i as i64 * 1_000, i as f64)).collect();
    let anchored = walk_reading_the_train_half(&bars, WalkMode::Anchored);
    let rolling = walk_reading_the_train_half(&bars, WalkMode::Rolling);

    // What each mode actually handed the closure — the difference, at its source.
    assert_eq!(
        trained_on(&anchored),
        vec![0.0f64.to_bits(); 3],
        "an anchored walk expands from bar 0, so every window's first training bar is bar 0"
    );
    assert_eq!(
        trained_on(&rolling),
        vec![0.0f64.to_bits(), 5.0f64.to_bits(), 10.0f64.to_bits()],
        "a rolling walk trains on the chunk immediately before each window"
    );

    // ...and the reports that follow from it.
    assert_eq!(
        window_ranges(&anchored),
        vec![(5, 10), (10, 15), (15, 20)],
        "the modes differ in train_start alone: the validation windows are identical"
    );
    assert_eq!(window_ranges(&rolling), window_ranges(&anchored));
    assert_eq!(
        anchored.oos_return.to_bits(),
        0.0f64.to_bits(),
        "training from bar 0 every time yields factor 1.0 in every window"
    );
    assert_eq!(
        rolling.oos_return.to_bits(),
        0.875f64.to_bits(),
        "1.0 * 1.25 * 1.5 = 1.875 of the starting equity"
    );
    assert_ne!(
        report_bits(&rolling),
        report_bits(&anchored),
        "the two modes must no longer be two spellings of one answer"
    );
    assert_eq!(
        rolling.wf_consistency.to_bits(),
        (2.0f64 / 3.0).to_bits(),
        "two of the three rolling windows finish above their starting equity"
    );
}

/// [`WindowSearch::Sweep`] over a profile with no `[sweep]` table is REFUSED at the DRIVER, not
/// degenerated into the control: `expand_sweep` answers a table-less profile with ONE candidate, so
/// a driver that simply ran it would perform no search while its report still claimed an
/// optimization — a run that never happened, wearing the clothes of one that did.
///
/// This is the runtime twin of a load-time rule, and the twin is the whole point of the test.
/// `BacktestProfile::validate` refuses `search = "sweep"` beside no grid at LOAD, which is strictly
/// earlier and strictly better — and covers exactly the profiles that SPELL the pairing. It cannot
/// see a caller that passes `WindowSearch::Sweep` to the objective door over a profile whose table
/// says nothing at all, which is the only way this file can reach the check and is a path the
/// driver's own doc says survives for that reason.
///
/// Both halves are asserted, because "it errored" is not "it errored for the right reason": the
/// same profile under `WindowSearch::None` runs to a full report, so what was refused is the
/// SEARCH, not the profile. The empty-table spelling is covered too — `is_sweep()` reads a present
/// but empty `[sweep]` as "not a sweep", and an operator who typed the header and no axes holds
/// exactly the same false belief as one who typed neither.
#[test]
fn a_grid_search_over_a_profile_with_no_sweep_table_is_refused_by_the_driver_too() {
    let obj = total_return_objective();
    let p = profile("");

    let err = optimized_with(&p, WindowSearch::Sweep, WalkMode::Anchored, &obj).unwrap_err();
    assert!(
        matches!(err, HarnessError::Validation(ref m) if m.contains("[sweep]")),
        "the refusal must name the missing table: {err}"
    );

    // The same profile IS a legal control run, so what was refused is the search, not the profile.
    let control = optimized_with(&p, WindowSearch::None, WalkMode::Anchored, &obj).unwrap();
    assert_eq!(control.windows.len(), N_SPLITS);

    // A present-but-EMPTY table is the same refusal: `is_sweep()` reads it as "not a sweep".
    let p = profile("[sweep]\n");
    let err = optimized_with(&p, WindowSearch::Sweep, WalkMode::Anchored, &obj).unwrap_err();
    assert!(matches!(err, HarnessError::Validation(_)), "{err}");
}

/// A window in which EVERY candidate failed is a hard error, not a skipped window and not a report
/// of zeros — the same failure shape as a zero-split walk returning an all-zeros report with `Ok`.
///
/// The candidates fail for real rather than by injection: the profile names the `rhai` arm with a
/// script that compiles, and the grid overrides `src` with two that do not, so each point dies
/// inside the per-point backtest at strategy resolution — a `HarnessError::Validation` recorded as
/// that row's error, scored `NaN`, sorted last by `cmp_scores_desc`, and therefore never a winner.
/// The BASE script still resolves, which is what lets the run reach a window at all: pre-flight
/// passes and the failure is discovered where the driver's doc says it is, on the training half.
///
/// Driven through the ARGUMENT-FREE door (`search = "sweep"` in the profile's own `[walkforward]`
/// table), because that is the door an operator's TOML reaches and this failure is an operator's
/// failure — a grid whose points do not run.
#[test]
fn a_window_whose_every_candidate_fails_to_compile_is_an_error_not_a_report_of_zeros() {
    let toml_src = r#"
[data]
venue = "test"
symbols = ["TESTUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "1000000"

[engine]
cash = 1000.0

[strategy]
name = "rhai"
[strategy.params]
src = "fn on_bar() {}"

[sweep]
src = ["fn on_bar( { syntax error", "fn on_bar( { another syntax error"]

[walkforward]
n_splits = 2
search = "sweep"
"#;
    let p = BacktestProfile::from_toml_str(toml_src).expect("the profile itself is well formed");

    let err = run_walkforward_optimized(&p, store()).unwrap_err();
    match err {
        HarnessError::Data(msg) => {
            assert!(msg.contains("walk-forward optimization"), "{msg}");
            assert!(msg.contains("candidate"), "the error must say what failed: {msg}");
        }
        other => panic!("expected a Data error naming the failed candidates, got {other}"),
    }
}

/// The same refusal reached from the other side: candidates that RAN but that the objective cannot
/// rank. The driver's rule is "no finite score wins ⇒ the window decided nothing", and it draws no
/// distinction between a point that failed and a point that scored `NaN` — both are unrankable, and
/// crowning either would let a degenerate candidate steer the walk.
///
/// Worth its own test because it is the cheap way in, and because it can only be reached through
/// the objective door: an arbitrary `Box<dyn Fn>` is the one thing a `[walkforward]` table can
/// never name, so a profile-driven run cannot produce this shape at all. The control over the
/// identical profile and objective still returns a report — no search runs, so the objective is
/// never consulted — which is what pins the error to the SEARCH rather than to the walk.
#[test]
fn an_objective_that_can_rank_nothing_fails_the_walk_rather_than_reporting_zeros() {
    let p = profile(GRID);
    let unrankable: Objective = Box::new(|_: &BacktestReport| f64::NAN);

    let run = optimized_with(&p, WindowSearch::Sweep, WalkMode::Anchored, &unrankable);
    assert!(matches!(run, Err(HarnessError::Data(_))), "{run:?}");

    let control = optimized_with(&p, WindowSearch::None, WalkMode::Anchored, &unrankable).unwrap();
    assert_eq!(control.windows.len(), N_SPLITS);
    assert!(control.oos_return > 0.0, "the control still trades: {}", control.oos_return);
}

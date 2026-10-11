//! Compare-all over saved strategies, and the walk-forward runner's annualization and floor.

use std::assert_matches;
use vike_analytics::metrics::sharpe;
use vike_analytics::report::{DAILY_PERIODS_PER_YEAR, periods_per_year_for_interval};
use vike_analytics::validation::WalkMode;
use vike_backtest::walkforward::walk_forward_strategy;

use super::support::{rhai, seeded_store, slice};
use crate::run::{RunError, compare_all_slice, run_walkforward_slice, spawn_compare_all};
use crate::spec::{StrategySpec, params_from_rows};
use crate::test_support::cross_script::CROSS_SCRIPT;

// ---- compare / walk-forward / sweep ------------------------------------------------------

const NOOP_SCRIPT: &str = "fn on_bar() {}";

#[test]
fn compare_all_slice_runs_every_strategy_and_carries_names() {
    let (_dir, store, _b) = seeded_store();
    let strategies =
        vec![("noop".to_string(), rhai(NOOP_SCRIPT)), ("cross".to_string(), rhai(CROSS_SCRIPT))];
    let results = compare_all_slice(&strategies, &slice(), &store);
    assert_eq!(results.len(), 2);
    let noop = results.iter().find(|(n, _)| n == "noop").unwrap();
    assert_eq!(noop.1.as_ref().unwrap().n_trades, 0);
    let cross = results.iter().find(|(n, _)| n == "cross").unwrap();
    assert!(cross.1.as_ref().unwrap().n_trades > 0);
}

/// A Rhai row and a NATIVE row compare side by side in one pass — the compare pane's widening.
#[test]
fn compare_all_slice_mixes_rhai_and_native_rows() {
    let (_dir, store, _b) = seeded_store();
    let strategies = vec![
        ("cross".to_string(), rhai(CROSS_SCRIPT)),
        (
            "hold".to_string(),
            StrategySpec::native(
                "buy_hold",
                params_from_rows(&[("symbol".into(), "BTCUSDT".into())]),
            ),
        ),
        ("nope".to_string(), StrategySpec::native_default("not_a_strategy")),
    ];
    let results = compare_all_slice(&strategies, &slice(), &store);
    assert_eq!(results.len(), 3);
    assert!(results.iter().find(|(n, _)| n == "cross").unwrap().1.is_ok());
    assert!(results.iter().find(|(n, _)| n == "hold").unwrap().1.is_ok());
    let bad = results.iter().find(|(n, _)| n == "nope").unwrap();
    assert!(bad.1.is_err(), "an unknown native name surfaces as a failed row, not a panic");
}

#[test]
fn compare_all_slice_stringifies_a_per_strategy_failure_without_dropping_the_row() {
    let (_dir, store, _b) = seeded_store();
    let strategies =
        vec![("broken".to_string(), rhai("fn on_bar( {")), ("ok".to_string(), rhai(NOOP_SCRIPT))];
    let results = compare_all_slice(&strategies, &slice(), &store);
    assert_eq!(results.len(), 2, "a failing strategy stays in the output, not dropped");
    let broken = results.iter().find(|(n, _)| n == "broken").unwrap();
    assert!(broken.1.is_err());
    let ok = results.iter().find(|(n, _)| n == "ok").unwrap();
    assert!(ok.1.is_ok());
}

#[test]
fn spawn_compare_all_delivers_the_same_outcome_as_the_sync_call() {
    let (_dir, store, _b) = seeded_store();
    let strategies = vec![("cross".to_string(), rhai(CROSS_SCRIPT))];
    let rx = spawn_compare_all(strategies.clone(), slice(), store.clone());
    let got = rx.recv().expect("worker sends one outcome");
    let want = compare_all_slice(&strategies, &slice(), &store);
    assert_eq!(got.len(), want.len());
    assert_eq!(got[0].0, want[0].0);
    assert_eq!(got[0].1.as_ref().unwrap().n_trades, want[0].1.as_ref().unwrap().n_trades);
}

#[test]
fn walkforward_slice_stitches_oos_windows() {
    let (_dir, store, _b) = seeded_store();
    let rep = run_walkforward_slice(&rhai(CROSS_SCRIPT), &slice(), &store, 4).unwrap();
    assert_eq!(rep.windows.len(), 4);
    assert!(!rep.oos_equity_curve.is_empty());
    assert!((0.0..=1.0).contains(&rep.wf_consistency));
}

#[test]
fn walkforward_compile_error_maps_to_compile() {
    let (_dir, store, _b) = seeded_store();
    assert_matches!(
        run_walkforward_slice(&rhai("fn on_bar( {"), &slice(), &store, 4),
        Err(RunError::Compile(_))
    );
}

/// **The annualization bug.** `oos_sharpe` is scaled by `sqrt(periods_per_year)`, and that
/// factor must be the observation count the SLICE's own bar step produces — 252 · 1440 for the
/// 1m fixture — not the daily 252 this plane hardcoded while the harness plane derived its
/// own. The two doors disagreed by `sqrt(1440)` on exactly this interval, which is the one the
/// CI roundtrip fixture uses, and no test compared them.
#[test]
fn walkforward_annualizes_off_the_slice_interval_not_a_bare_252() {
    let (_dir, store, _b) = seeded_store();
    assert_eq!(slice().interval, "1m", "the fixture interval is what makes 1440 the ratio");
    let rep = run_walkforward_slice(&rhai(CROSS_SCRIPT), &slice(), &store, 4).unwrap();

    // `oos_sharpe` IS `sharpe(oos_equity_curve, factor)` — both come out of the same stitch —
    // so recomputing the curve at each candidate factor says which one the run actually used,
    // bit for bit, with no tolerance to argue about.
    let at_slice = sharpe(&rep.oos_equity_curve, periods_per_year_for_interval("1m"));
    let at_daily = sharpe(&rep.oos_equity_curve, DAILY_PERIODS_PER_YEAR);
    assert_eq!(
        rep.oos_sharpe.to_bits(),
        at_slice.to_bits(),
        "a 1m slice must annualize at 252 * 1440, i.e. one observation per BAR"
    );
    assert!(
        at_daily != 0.0,
        "precondition: a flat OOS curve would make every comparison below vacuous"
    );
    assert_ne!(rep.oos_sharpe.to_bits(), at_daily.to_bits(), "...and NOT at the daily anchor");

    // The SIZE of the correction, pinned because it is what a reader needs in order to reason
    // about an `oos_sharpe` printed by this door BEFORE the fix.
    let ratio = rep.oos_sharpe / at_daily;
    assert!((ratio - 1440.0_f64.sqrt()).abs() < 1e-9, "expected ~37.9x, got {ratio}");
}

/// **The `n_splits` floor.** Zero windows is not an empty result, it is a run that never
/// happened wearing the shape of a strategy that made nothing — so it is refused rather than
/// reported. The second half drives `walk_forward_strategy` at zero directly, because a guard
/// is only worth having if the thing it prevents is real.
#[test]
fn zero_splits_is_refused_rather_than_reported_as_an_all_zeros_run() {
    let (_dir, store, bars) = seeded_store();
    let err = run_walkforward_slice(&rhai(CROSS_SCRIPT), &slice(), &store, 0)
        .expect_err("n_splits = 0 must not produce a report");
    let RunError::Data(msg) = &err else { panic!("expected a Data refusal, got {err:?}") };
    assert!(msg.contains("n_splits"), "the refusal must name the parameter, got {msg:?}");

    // What the guard prevents. Both scalars are arbitrary here: with no splits the window
    // closure is never called, nothing is stitched, and `sharpe` of an empty curve is 0 at any
    // annualization.
    // ⚠ `|_, _|`, not `|_|`. This test arrived on one branch while the runner's closure grew a
    // second parameter (the TRAIN half) on another, and the mismatch was invisible to both:
    // each side compiled alone, and only the merged tree names it.
    let unguarded = walk_forward_strategy(&bars, 0, WalkMode::Anchored, 1_000.0, 1.0, |_, _| {
        unreachable!("zero splits means the window closure is never called")
    });
    assert!(unguarded.windows.is_empty(), "no windows");
    assert!(unguarded.oos_equity_curve.is_empty(), "nothing stitched");
    assert_eq!(unguarded.oos_return, 0.0, "reads as a strategy that made nothing");
    assert_eq!(unguarded.oos_sharpe, 0.0, "...at a Sharpe of zero");
    assert_eq!(unguarded.wf_consistency, 0.0, "...over `windows.len().max(1)` windows");
}

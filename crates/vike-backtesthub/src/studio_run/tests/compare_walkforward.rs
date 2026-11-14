//! The walk-forward runner's annualization and floor.

use std::assert_matches;
use vike_analytics::metrics::sharpe;
use vike_analytics::report::{DAILY_PERIODS_PER_YEAR, periods_per_year_for_interval};
use vike_analytics::validation::WalkforwardMode;
use vike_backtest::walkforward::walk_forward_strategy;

use super::support::{rhai, seeded_store, slice};
use crate::studio_run::run_walkforward_slice;
use vike_studio_core::test_support::cross_script::CROSS_SCRIPT;
use vike_studio_core::{RunError, StrategySpec, params_from_rows};

// ---- walk-forward ------------------------------------------------------------------------
//
// Compare-all's four tests stood here: "Compare all" no longer runs in-process (it is one hub
// `RunSlice` per saved row, `crates/vike-studio/src/backend/remote.rs`'s `spawn_compare_remote`),
// so the in-process `compare_all_slice` they exercised is deleted with them.

#[test]
fn walkforward_slice_stitches_out_of_sample_windows() {
    let (_dir, store, _b) = seeded_store();
    let rep = run_walkforward_slice(
        &rhai(CROSS_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice(),
        &store,
        4,
    )
    .unwrap();
    assert_eq!(rep.windows.len(), 4);
    assert!(!rep.out_of_sample_equity_curve.is_empty());
    assert!((0.0..=1.0).contains(&rep.walkforward_consistency));
}

#[test]
fn walkforward_compile_error_maps_to_compile() {
    let (_dir, store, _b) = seeded_store();
    assert_matches!(
        run_walkforward_slice(
            &rhai("fn on_bar( {"),
            &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
            &slice(),
            &store,
            4
        ),
        Err(RunError::Compile(_))
    );
}

/// **The annualization bug.** `out_of_sample_sharpe` is scaled by `sqrt(periods_per_year)`, and that
/// factor must be the observation count the SLICE's own bar step produces — 252 · 1440 for the
/// 1m fixture — not the daily 252 this plane hardcoded while the harness plane derived its
/// own. The two doors disagreed by `sqrt(1440)` on exactly this interval, which is the one the
/// CI roundtrip fixture uses, and no test compared them.
#[test]
fn walkforward_annualizes_off_the_slice_interval_not_a_bare_252() {
    let (_dir, store, _b) = seeded_store();
    assert_eq!(slice().interval, "1m", "the fixture interval is what makes 1440 the ratio");
    let rep = run_walkforward_slice(
        &rhai(CROSS_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice(),
        &store,
        4,
    )
    .unwrap();

    // `out_of_sample_sharpe` IS `sharpe(out_of_sample_equity_curve, factor)` — both come out of the same stitch —
    // so recomputing the curve at each candidate factor says which one the run actually used,
    // bit for bit, with no tolerance to argue about.
    let at_slice = sharpe(&rep.out_of_sample_equity_curve, periods_per_year_for_interval("1m"));
    let at_daily = sharpe(&rep.out_of_sample_equity_curve, DAILY_PERIODS_PER_YEAR);
    assert_eq!(
        rep.out_of_sample_sharpe.to_bits(),
        at_slice.to_bits(),
        "a 1m slice must annualize at 252 * 1440, i.e. one observation per BAR"
    );
    assert!(
        at_daily != 0.0,
        "precondition: a flat OOS curve would make every comparison below vacuous"
    );
    assert_ne!(
        rep.out_of_sample_sharpe.to_bits(),
        at_daily.to_bits(),
        "...and NOT at the daily anchor"
    );

    // The SIZE of the correction, pinned because it is what a reader needs in order to reason
    // about an `out_of_sample_sharpe` printed by this door BEFORE the fix.
    let ratio = rep.out_of_sample_sharpe / at_daily;
    assert!((ratio - 1440.0_f64.sqrt()).abs() < 1e-9, "expected ~37.9x, got {ratio}");
}

/// **The `n_splits` floor.** Zero windows is not an empty result, it is a run that never
/// happened wearing the shape of a strategy that made nothing — so it is refused rather than
/// reported. The second half drives `walk_forward_strategy` at zero directly, because a guard
/// is only worth having if the thing it prevents is real.
#[test]
fn zero_splits_is_refused_rather_than_reported_as_an_all_zeros_run() {
    let (_dir, store, bars) = seeded_store();
    let err = run_walkforward_slice(
        &rhai(CROSS_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice(),
        &store,
        0,
    )
    .expect_err("n_splits = 0 must not produce a report");
    let RunError::Data(msg) = &err else { panic!("expected a Data refusal, got {err:?}") };
    assert!(msg.contains("n_splits"), "the refusal must name the parameter, got {msg:?}");

    // What the guard prevents. Both scalars are arbitrary here: with no splits the window
    // closure is never called, nothing is stitched, and `sharpe` of an empty curve is 0 at any
    // annualization.
    // ⚠ `|_, _|`, not `|_|`. This test arrived on one branch while the runner's closure grew a
    // second parameter (the TRAIN half) on another, and the mismatch was invisible to both:
    // each side compiled alone, and only the merged tree names it.
    let unguarded =
        walk_forward_strategy(&bars, 0, WalkforwardMode::Anchored, 1_000.0, 1.0, |_, _| {
            unreachable!("zero splits means the window closure is never called")
        });
    assert!(unguarded.windows.is_empty(), "no windows");
    assert!(unguarded.out_of_sample_equity_curve.is_empty(), "nothing stitched");
    assert_eq!(unguarded.out_of_sample_return, 0.0, "reads as a strategy that made nothing");
    assert_eq!(unguarded.out_of_sample_sharpe, 0.0, "...at a Sharpe of zero");
    assert_eq!(unguarded.walkforward_consistency, 0.0, "...over `windows.len().max(1)` windows");
}

/// Decision 0083's recorded defect, on the WALK-FORWARD door: a searching walk records each
/// window's choice in the kind the strategy was handed, which is the kind its axis carried. An
/// `Int` axis over a knob the spec's params hold as an INTEGER is chosen as an `Int`; the `Float`
/// axis the Studio makes for Rhai, which has no params table, is recorded as a `Float`.
#[test]
fn a_searching_walk_records_an_integer_knob_as_an_integer() {
    use crate::studio_run::run_walkforward_slice_searching;
    use vike_backtesthub_client::wire_studio::{ParamscanWire, WalkforwardWindowSearchWire};
    use vike_model::params::ParamValue;

    let sweep = |axes: Vec<(String, Vec<ParamValue>)>| WalkforwardWindowSearchWire {
        method: "paramscan".to_string(),
        grid: ParamscanWire { axes },
        rank_by: None,
    };

    let (_dir, store, _b) = seeded_store();
    let grid = vec![("size".to_string(), vec![ParamValue::Int(1), ParamValue::Int(2)])];
    let native = StrategySpec::file(
        "trend/buy_hold.rs",
        params_from_rows(&[("size".into(), "1".into()), ("symbol".into(), "BTCUSDT".into())]),
    );
    let rep = run_walkforward_slice_searching(
        &native,
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice(),
        &store,
        2,
        Some(&sweep(grid)),
        None,
    )
    .expect("a searching walk over a native spec runs");
    assert_eq!(rep.windows.len(), 2);
    for w in &rep.windows {
        let chosen = w.chosen_params.as_ref().expect("a searching walk records its choice");
        let size = &chosen.iter().find(|(k, _)| k == "size").expect("the swept axis").1;
        assert!(matches!(size, ParamValue::Int(1 | 2)), "an Int knob stays Int: {size:?}");
    }

    let rep = run_walkforward_slice_searching(
        &rhai(vike_studio_core::test_support::sweep_script::SWEEP_SCRIPT),
        &crate::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice(),
        &store,
        2,
        Some(&sweep(vec![("fast".to_string(), vec![ParamValue::Float(3.0)])])),
        None,
    )
    .expect("a searching walk over a script runs");
    for w in &rep.windows {
        let chosen = w.chosen_params.as_ref().expect("a searching walk records its choice");
        assert_eq!(chosen, &vec![("fast".to_string(), ParamValue::Float(3.0))]);
    }
}

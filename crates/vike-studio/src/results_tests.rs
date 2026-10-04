use super::*;
use vike_analytics::report::{
    DAILY_PERIODS_PER_YEAR, DEFAULT_PERIODS_PER_YEAR, periods_per_year_for_interval,
};
use vike_analytics::{BacktestResult, metrics};

/// The factor for the fixture curve below, which is sampled at 60s (`equity_ts`) — i.e. 1m
/// bars. Derived, never written as a number, so these tests read the same way the pane does.
fn ppy_1m() -> f64 {
    periods_per_year_for_interval("1m")
}

fn sample() -> BacktestResult {
    BacktestResult {
        equity_curve: vec![10_000.0, 10_100.0, 10_050.0, 10_200.0],
        equity_ts: vec![0, 60_000, 120_000, 180_000],
        final_equity: 10_200.0,
        n_trades: 2,
        ..Default::default()
    }
}

#[test]
fn perf_rows_has_the_headline_metrics() {
    let r = sample();
    let rows = perf_rows(&r, ppy_1m());
    let labels: Vec<&str> = rows.iter().map(|(k, _)| *k).collect();
    for want in ["Total return", "Sharpe", "Max drawdown", "Trades"] {
        assert!(labels.contains(&want), "missing {want}");
    }
    // Total-return cell reflects the real metric
    let tr = rows.iter().find(|(k, _)| *k == "Total return").unwrap();
    assert!(tr.1.contains(&format!("{:.2}", metrics::total_return(&r.equity_curve) * 100.0)));
}

#[test]
fn returns_hist_is_per_bar_returns() {
    let r = sample();
    assert_eq!(returns_hist(&r), metrics::returns(&r.equity_curve));
}

/// Render one headless frame and hand it to the shared geometry invariant.
///
/// ⚠ The `textures_delta.clear()` runs BEFORE the assertion, and that order is load-bearing —
/// the same trap `crates/vike-data-manager/src/view_tests.rs`'s `run_frame` documents: asserting
/// first leaves the deltas unapplied, so dropping the frame during the assertion's unwind
/// panics a SECOND time in the destructor and the process ABORTS, printing a backtrace instead
/// of the coordinate that was wrong.
fn frame(f: impl FnMut(&mut egui::Ui)) {
    let ctx = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, f);
    out.textures_delta.clear();
    vike_ui_theme::frame_sanity::assert_frame_sane(&out);
}

/// THE KILL PROOF for `distribution_tab`'s empty-returns guard.
///
/// A `BacktestResult` whose equity curve is shorter than two points has NO returns — a return
/// is a difference between two samples, so `vike_analytics::metrics::returns` yields an empty
/// vector. Both curves below are that case: a run that stopped on its first bar, and a run that
/// produced no curve at all. Neither is exotic, and neither is reachable from the seeded
/// 400-bar fixtures that every other Studio render test uses — which is precisely why this
/// defect could sit in a shipped pane while `assert_frame_sane` ran green over six shell poses.
///
/// Without the guard, `distribution_tab`'s `(f64::MAX, f64::MIN)` fold leaves `lo` at
/// `f64::MAX`, `w` clamped to `1e-9`, and all 21 bar centres at ~`1.7e308` — `inf` as f32.
///
/// ⚠ MEASURED, not argued: the guard was removed and this test run on egui_plot 0.37. It fails,
/// but NOT where you would expect — `egui_plot`'s axis-bounds check panics inside `Plot::show`
/// with `Bad final plot bounds: PlotBounds { min: [inf, -0.5], max: [inf, 0.5] }`, so the frame
/// never reaches [`frame`]'s `assert_frame_sane` at all. That makes the defect a CRASH in the
/// results pane rather than bad geometry, and it makes this test's coverage broader than the
/// invariant it was written against: `frame` is still the right harness (it catches the
/// non-panicking geometry defects a future edit could introduce here), but the panic is what
/// fires today. Both sibling tests passed unchanged in the same run, so the failure is this
/// input, not the harness.
#[test]
fn a_degenerate_equity_curve_distributes_no_returns_and_paints_finite_geometry() {
    for equity_curve in [vec![10_000.0], Vec::new()] {
        let r = BacktestResult { equity_curve, ..Default::default() };
        assert!(
            returns_hist(&r).is_empty(),
            "premise: this curve must yield NO returns, or the test proves nothing",
        );
        frame(|ui| distribution_tab(ui, &r));
    }
}

/// The other half of the pair, and the reason the guard is an early RETURN rather than a
/// clamp: a populated curve must still reach the real binning path. Without this, a guard
/// widened by accident to swallow every input would keep the test above green while the
/// Distribution pane rendered permanently empty.
#[test]
fn a_populated_equity_curve_still_reaches_the_binned_histogram() {
    let r = sample();
    assert!(!returns_hist(&r).is_empty(), "premise: the sample curve has returns to bin");
    frame(|ui| distribution_tab(ui, &r));
}

#[test]
fn validation_rows_include_psr() {
    // a synthetic upward-drifting equity curve -> positive Sharpe, computable PSR
    let r = BacktestResult {
        equity_curve: (0..260).map(|i| 10_000.0 * (1.0 + 0.0004 * i as f64)).collect(),
        final_equity: 10_000.0 * (1.0 + 0.0004 * 259.0),
        n_trades: 3,
        ..Default::default()
    };
    let rows = validation_rows(&r, ppy_1m());
    assert!(rows.iter().any(|(k, _)| *k == "Prob. Sharpe > 0 (PSR)"), "PSR row present");
    assert!(rows.iter().any(|(k, _)| *k == "Sharpe"));
}

/// THE REGRESSION this file's parameter exists to prevent: the four annualized Performance
/// rows must move with the supplied factor, not with a constant baked in here.
///
/// Held on the raw sign column rather than the formatted strings, so a formatting change
/// cannot make it pass by accident: the 1m Sharpe is `sqrt(1440) ≈ 37.9x` the daily one, which
/// is precisely the error a Studio user was shown on the interval the CI roundtrip fixture
/// uses. The `sqrt` relation is asserted for Sharpe alone — CAGR/Calmar compound rather than
/// scale (and on this 4-point fixture the 1m factor trips `metrics::cagr`'s overflow guard to
/// `0.0`, the effect the module doc names), so pinning their exact ratio here would re-derive
/// maths that belongs to `vike_analytics::metrics`. That they MOVED is what this pane owns.
#[test]
fn the_annualized_rows_scale_with_the_supplied_factor() {
    let r = sample();
    let raw = |cells: &[(&'static str, String, Option<f64>)], label: &str| -> f64 {
        cells
            .iter()
            .find(|(k, _, _)| *k == label)
            .and_then(|(_, _, sign)| *sign)
            .unwrap_or_else(|| panic!("{label} must be a sign-carrying row"))
    };
    let daily = perf_cells(&r, DAILY_PERIODS_PER_YEAR);
    let minute = perf_cells(&r, ppy_1m());

    let (s_d, s_m) = (raw(&daily, "Sharpe"), raw(&minute, "Sharpe"));
    assert!(
        (s_m / s_d - (ppy_1m() / DAILY_PERIODS_PER_YEAR).sqrt()).abs() < 1e-9,
        "Sharpe must scale as sqrt(periods_per_year): daily {s_d}, 1m {s_m}",
    );
    for label in ["Sortino", "CAGR", "Calmar"] {
        assert_ne!(
            raw(&daily, label),
            raw(&minute, label),
            "{label} is annualized and must not be interval-blind",
        );
    }
    // ...and the rows that are NOT annualized must be untouched by the factor, or the change
    // would be scaling things it has no business scaling.
    for label in ["Total return", "Final equity", "Win rate", "Profit factor", "Trades"] {
        let d = daily.iter().find(|(k, _, _)| *k == label).expect("row present");
        let m = minute.iter().find(|(k, _, _)| *k == label).expect("row present");
        assert_eq!(d.1, m.1, "{label} is not an annualized metric");
    }
}

/// The daily ANCHOR, held from the display side: on a `1d` slice the pane must print exactly
/// what it printed when it held `const PPY: f64 = 252.0`, bit for bit. Without this the fix
/// could quietly move every daily report it was supposed to leave alone.
#[test]
fn a_daily_slice_still_prints_the_pre_fix_numbers() {
    let r = sample();
    let before = perf_cells(&r, 252.0); // the literal this module used to hold
    let after = perf_cells(&r, periods_per_year_for_interval("1d"));
    assert_eq!(before.len(), after.len(), "the row set must not change");
    // Compared through `to_bits` rather than `==`: a NaN cell (a flat curve's Calmar, say)
    // would make a float equality vacuously fail, and "bit for bit" is the actual claim.
    for ((kb, vb, sb), (ka, va, sa)) in before.iter().zip(&after) {
        assert_eq!(kb, ka);
        assert_eq!(vb, va, "{kb} printed differently");
        assert_eq!(sb.map(f64::to_bits), sa.map(f64::to_bits), "{kb} moved by a bit");
    }
}

/// `StudioState::display_periods_per_year` tells the reader that with no slice picked the
/// metrics "keep the anchor they have always had". That sentence is only true while
/// `DEFAULT_PERIODS_PER_YEAR` and `DAILY_PERIODS_PER_YEAR` are the same number — vike-analytics
/// keeps them equal today and says so, but says it as a TODAY. This is the assertion that
/// stops the Studio's promise outliving the constant it rests on; if it ever goes red, the
/// fallback did not change, the doc sentence did.
#[test]
fn the_fallback_factor_is_the_daily_anchor() {
    assert_eq!(DEFAULT_PERIODS_PER_YEAR.to_bits(), DAILY_PERIODS_PER_YEAR.to_bits());
}

/// The Validation tab's split, pinned: the Sharpe row is ANNUALIZED and moves with the factor;
/// `overfit::sharpe_moments`' outputs are per-OBSERVATION and must not. Scaling `sr_per_obs`
/// into the PSR would be an easy, invisible "unification" — this is the test that catches it.
#[test]
fn the_annualized_row_moves_with_the_factor_while_the_psr_inputs_do_not() {
    // an upward-drifting curve: positive Sharpe, computable PSR, no NaNs to confuse equality
    let r = BacktestResult {
        equity_curve: (0..260).map(|i| 10_000.0 * (1.0 + 0.0004 * i as f64)).collect(),
        final_equity: 10_000.0 * (1.0 + 0.0004 * 259.0),
        n_trades: 3,
        ..Default::default()
    };
    let daily = validation_rows(&r, DAILY_PERIODS_PER_YEAR);
    let minute = validation_rows(&r, ppy_1m());

    let row = |rows: &[(&'static str, String)], label: &str| -> String {
        rows.iter().find(|(k, _)| *k == label).map(|(_, v)| v.clone()).expect("row present")
    };
    assert_ne!(
        row(&daily, "Sharpe"),
        row(&minute, "Sharpe"),
        "the displayed Sharpe is annualized and must follow the interval",
    );
    for label in ["Observations", "Skew", "Kurtosis", "Prob. Sharpe > 0 (PSR)"] {
        assert_eq!(
            row(&daily, label),
            row(&minute, label),
            "{label} is per-observation and must never see the annualization factor",
        );
    }
}

#[test]
fn verdict_rows_reflect_dsr_and_walkforward() {
    // a StudioParamscan with a strong DSR + a walk-forward with full consistency -> Low risk
    let sweep = vike_studio_core::StudioParamscan {
        entries: vec![vike_studio_core::ParamscanEntry {
            overrides: vec![("fast".into(), 5.0)],
            result: BacktestResult {
                equity_curve: vec![100.0, 110.0, 121.0],
                final_equity: 121.0,
                n_trades: 2,
                ..Default::default()
            },
        }],
        dsr: 0.95,
        pbo: 0.1,
        best_index: 0,
    };
    let rows = validation_verdict_rows(Some(&sweep), None);
    assert!(rows.iter().any(|(k, _)| *k == "Deflated Sharpe"));
    assert!(rows.iter().any(|(k, _)| *k == "Overfit risk"));
}

fn sweep_with(dsr: f64, pbo: f64) -> vike_studio_core::StudioParamscan {
    vike_studio_core::StudioParamscan {
        entries: vec![vike_studio_core::ParamscanEntry {
            overrides: vec![("fast".into(), 5.0)],
            result: BacktestResult {
                equity_curve: vec![100.0, 110.0, 121.0],
                final_equity: 121.0,
                n_trades: 2,
                ..Default::default()
            },
        }],
        dsr,
        pbo,
        best_index: 0,
    }
}

#[test]
fn verdict_rows_include_a_pbo_percentage() {
    let sweep = sweep_with(0.95, 0.10);
    let rows = validation_verdict_rows(Some(&sweep), None);
    let pbo = rows.iter().find(|(k, _)| *k == "PBO").expect("PBO row present");
    assert_eq!(pbo.1, "10%");
}

#[test]
fn high_pbo_and_low_dsr_reads_high_risk() {
    let sweep = sweep_with(0.20, 0.80); // pbo>0.5 (+2) and dsr<0.5 (+2) -> 4 points -> High
    let rows = validation_verdict_rows(Some(&sweep), None);
    let risk = rows.iter().find(|(k, _)| *k == "Overfit risk").expect("risk row");
    assert_eq!(risk.1, "High");
}

#[test]
fn nan_pbo_reads_em_dash_and_not_assessed() {
    let sweep = sweep_with(0.95, f64::NAN);
    let rows = validation_verdict_rows(Some(&sweep), None);
    let pbo = rows.iter().find(|(k, _)| *k == "PBO").expect("PBO row present");
    assert_eq!(pbo.1, "—");
    assert!(
        rows.iter().any(|(_, v)| v.contains("not assessed")),
        "the verdict should carry a 'not assessed' note for NaN PBO"
    );
}

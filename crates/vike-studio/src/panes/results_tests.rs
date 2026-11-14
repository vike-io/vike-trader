use super::*;
use vike_analytics::report::{
    DAILY_PERIODS_PER_YEAR, DEFAULT_PERIODS_PER_YEAR, periods_per_year_for_interval,
};
use vike_analytics::{BacktestResult, metrics};

/// The factor for the fixture curve below, which is sampled at 60s (`equity_timestamp`) — i.e. 1m
/// bars. Derived, never written as a number, so these tests read the same way the pane does.
fn periods_per_year_1m() -> f64 {
    periods_per_year_for_interval("1m")
}

fn sample() -> BacktestResult {
    BacktestResult {
        equity_curve: vec![10_000.0, 10_100.0, 10_050.0, 10_200.0],
        equity_timestamp: vec![0, 60_000, 120_000, 180_000],
        final_equity: 10_200.0,
        n_trades: 2,
        ..Default::default()
    }
}

/// The Performance tab IS the catalog: every `METRICS` row in catalog order (`funding_paid`
/// included), each cell its unit's rendering — the text the CLI prints for the same run. A run that
/// did not arm closed-trade accounting carries no `MetricHome::ClosedTrades` row.
#[test]
fn perf_rows_are_the_whole_catalog_in_its_units() {
    use vike_analytics::metric_catalog::{METRICS, MetricHome, MetricUnit};
    let r = sample();
    let rows = perf_rows(&r, periods_per_year_1m());
    let ids: Vec<&str> = rows.iter().map(|(k, _)| *k).collect();
    let catalog: Vec<&str> =
        METRICS.iter().filter(|m| m.home != MetricHome::ClosedTrades).map(|m| m.id).collect();
    assert_eq!(ids, catalog);
    let cell = |id: &str| rows.iter().find(|(k, _)| *k == id).map(|(_, v)| v.clone()).unwrap();
    assert_eq!(
        cell("total_return"),
        MetricUnit::Percent.render(metrics::total_return(&r.equity_curve))
    );
    assert_eq!(cell("n_trades"), "2");
    assert_eq!(cell("funding_paid"), "0.00", "a perp run's funding cost is a row of its own");
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

const SPOT: &str =
    "a spot account cannot sell more than it holds: it has 1, this sells 2; nothing was sent";

fn refused(reasons: &[&str]) -> BacktestResult {
    let drop = |reason: &&str| vike_model::OrderDrop {
        symbol: "BTCUSDT".to_string(),
        reason: reason.to_string(),
        size: 2.0,
        weight: 1.0,
    };
    BacktestResult { dropped: reasons.iter().map(drop).collect(), ..sample() }
}

/// A refused order is SEEN (the owner's ruling of 2026-11-12, part C): the line counts the run's
/// refused orders and names each reason in the gate's words, most frequent first; a run that
/// refused none shows no line, so its pane is the pane it was.
#[test]
fn the_refused_line_counts_the_orders_and_names_each_reason() {
    assert_eq!(refused_line(&sample()), None);
    assert_eq!(refused_line(&refused(&[SPOT])), Some(format!("1 order refused: {SPOT}")));
    assert_eq!(
        refused_line(&refused(&[SPOT, SPOT, SPOT])),
        Some(format!("3 orders refused: {SPOT}"))
    );
    assert_eq!(
        refused_line(&refused(&["insufficient-margin", SPOT, SPOT])),
        Some(format!("3 orders refused:\n  2 × {SPOT}\n  1 × insufficient-margin"))
    );
    let seven = refused(&["a", "b", "c", "d", "e", "f", "g"]);
    let line = refused_line(&seven).expect("seven refusals");
    assert!(line.starts_with("7 orders refused:\n  1 × a\n"), "{line}");
    assert!(line.ends_with("\n  1 × e\n  … and 2 more reasons"), "{line}");
}

/// The line paints above the tabs on every tab without breaking the frame's geometry.
#[test]
fn the_results_pane_with_refused_orders_paints_finite_geometry() {
    let r = refused(&["insufficient-margin", SPOT, SPOT]);
    for mut tab in [ResultsTab::Equity, ResultsTab::Performance] {
        frame(|ui| results_ui(ui, &mut tab, &r, None, None, periods_per_year_1m()));
    }
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
    let rows = validation_rows(&r, periods_per_year_1m());
    assert!(rows.iter().any(|(k, _)| *k == PSR_LABEL), "PSR row present");
    assert!(rows.iter().any(|(k, _)| *k == "sharpe"));
    // D6: the kurtosis shown is the catalog's EXCESS kurtosis, the very row the Performance tab
    // and the CLI print; the old non-excess "Kurtosis" (3 higher, same word) is gone.
    let perf = perf_rows(&r, periods_per_year_1m());
    let kurt = rows.iter().find(|(k, _)| *k == "returns_kurtosis").expect("the catalog row");
    assert!(perf.contains(kurt), "{kurt:?} must be the Performance tab's own cell");
    assert!(!rows.iter().any(|(k, _)| *k == "Kurtosis"));
}

/// THE REGRESSION this file's parameter exists to prevent: the annualized Performance rows must
/// move with the supplied factor, not with a constant baked in here — and the rows that are NOT
/// annualized must be untouched by it, or the pane would be scaling things it has no business
/// scaling. (On this 4-point fixture the 1m factor trips `metrics::cagr`'s overflow guard to `0.0`,
/// the effect the module doc names; that it MOVED is what this pane owns.)
#[test]
fn the_annualized_rows_scale_with_the_supplied_factor() {
    let r = sample();
    let daily = perf_rows(&r, DAILY_PERIODS_PER_YEAR);
    let minute = perf_rows(&r, periods_per_year_1m());
    let cell = |rows: &[(&'static str, String)], id: &str| -> String {
        rows.iter().find(|(k, _)| *k == id).map(|(_, v)| v.clone()).expect("row present")
    };
    for id in ["sharpe", "sortino", "cagr", "calmar"] {
        assert_ne!(cell(&daily, id), cell(&minute, id), "{id} is annualized");
    }
    for id in ["total_return", "final_equity", "win_rate", "profit_factor", "n_trades"] {
        assert_eq!(cell(&daily, id), cell(&minute, id), "{id} is not an annualized metric");
    }
}

/// The daily ANCHOR, held from the display side: on a `1d` slice the pane prints exactly what it
/// prints at the literal `252.0` it once held, cell for cell.
#[test]
fn a_daily_slice_still_prints_the_pre_fix_numbers() {
    let r = sample();
    assert_eq!(perf_rows(&r, 252.0), perf_rows(&r, periods_per_year_for_interval("1d")));
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

/// The Validation tab's split, pinned: the `sharpe` row is ANNUALIZED and moves with the factor;
/// the return-shape moments and the PSR are per-OBSERVATION and must not. Scaling the Sharpe into
/// the PSR would be an easy, invisible "unification" — this is the test that catches it.
#[test]
fn the_annualized_row_moves_with_the_factor_while_the_psr_does_not() {
    // an upward-drifting curve: positive Sharpe, computable PSR, no NaNs to confuse equality
    let r = BacktestResult {
        equity_curve: (0..260).map(|i| 10_000.0 * (1.0 + 0.0004 * i as f64)).collect(),
        final_equity: 10_000.0 * (1.0 + 0.0004 * 259.0),
        n_trades: 3,
        ..Default::default()
    };
    let daily = validation_rows(&r, DAILY_PERIODS_PER_YEAR);
    let minute = validation_rows(&r, periods_per_year_1m());

    let row = |rows: &[(&'static str, String)], label: &str| -> String {
        rows.iter().find(|(k, _)| *k == label).map(|(_, v)| v.clone()).expect("row present")
    };
    assert_ne!(
        row(&daily, "sharpe"),
        row(&minute, "sharpe"),
        "the displayed Sharpe is annualized and must follow the interval",
    );
    for label in ["returns_skewness", "returns_kurtosis", PSR_LABEL] {
        assert_eq!(
            row(&daily, label),
            row(&minute, label),
            "{label} is per-observation and must never see the annualization factor",
        );
    }
}

#[test]
fn verdict_rows_reflect_dsr_and_walkforward() {
    // a StudioParamscan with a strong DSR -> Low risk
    let sweep = sweep_with(0.95, 0.1);
    let rows = validation_verdict_rows(Some(&sweep), None);
    assert!(rows.iter().any(|(k, _)| *k == "Deflated Sharpe"));
    assert!(rows.iter().any(|(k, _)| *k == "Overfit risk"));
}

fn walk_with(wf_consistency: f64) -> WalkforwardReport {
    WalkforwardReport {
        windows: Vec::new(),
        out_of_sample_equity_curve: vec![100.0, 99.0],
        out_of_sample_return: -0.01,
        out_of_sample_sharpe: -1.0,
        walkforward_consistency: wf_consistency,
        funding_paid: 0.0,
        account_currency: None,
        data_warnings: Vec::new(),
        strategy_file: String::new(),
        strategy_sha256: String::new(),
    }
}

/// D7: the verdict is the SWEEP's own (the trial ledger's rule, no walk-forward term). A
/// walk-forward that may be another strategy over another slice is shown as its own row and never
/// moves the sweep's risk level; with no sweep there is no verdict to show.
#[test]
fn a_walk_forward_is_never_folded_into_the_sweep_verdict() {
    let sweep = sweep_with(0.95, 0.10);
    let alone = validation_verdict_rows(Some(&sweep), None);
    let beside = validation_verdict_rows(Some(&sweep), Some(&walk_with(0.0)));
    let risk = |rows: &[(&'static str, String)]| {
        rows.iter().find(|(k, _)| *k == "Overfit risk").map(|(_, v)| v.clone()).unwrap()
    };
    assert_eq!(risk(&alone), "Low");
    assert_eq!(risk(&beside), "Low", "an unrelated walk-forward moved the sweep's verdict");
    assert_eq!(risk(&alone), format!("{:?}", sweep.verdict().level));
    assert!(beside.contains(&("Walk-forward consistency", "0.0000%".to_string())));

    let no_sweep = validation_verdict_rows(None, Some(&walk_with(1.0)));
    assert!(risk(&no_sweep).contains("run a param scan"), "{no_sweep:?}");
}

fn sweep_with(dsr: f64, pbo: f64) -> vike_studio_core::StudioParamscan {
    vike_studio_core::StudioParamscan {
        entries: vec![vike_studio_core::ParamscanEntry {
            overrides: vec![("fast".into(), ParamValue::Float(5.0))],
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
    assert_eq!(pbo.1, "10.0000%", "the catalog's own percent spelling");
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

/// GOLDEN: a sweep row's label prints a `Float` as an `f64` prints and an `Int` as itself, so a
/// whole float and an integer knob both read `5` (the label every row carried while an axis was an
/// `f64`), and a fractional value keeps its fraction.
#[test]
fn a_sweep_row_label_prints_each_override_as_the_f64_label_did() {
    let entry = vike_studio_core::ParamscanEntry {
        overrides: vec![
            ("fast".into(), ParamValue::Float(5.0)),
            ("rungs".into(), ParamValue::Int(6)),
            ("size".into(), ParamValue::Float(2.5)),
            ("tiny".into(), ParamValue::Float(1e-7)),
            ("neg".into(), ParamValue::Int(-3)),
        ],
        result: BacktestResult::default(),
    };
    assert_eq!(overrides_label(&entry), "fast=5, rungs=6, size=2.5, tiny=0.0000001, neg=-3");
}

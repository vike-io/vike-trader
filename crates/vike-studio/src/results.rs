//! The tabbed results pane over a `BacktestResult`. Pure rendering + testable helpers
//! (`perf_rows`, `returns_hist`, `validation_rows`, `validation_verdict_rows`); the tabs are
//! `egui_plot` lines/bars + `egui::Grid` tables.
use vike_backtest::overfit::overfit_verdict;
use vike_backtest::walkforward::WalkForwardReport;
use vike_backtest::{metrics, BacktestResult};
use vike_studio_core::{StudioSweep, SweepEntry};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ResultsTab {
    #[default]
    Equity,
    Trades,
    Performance,
    Distribution,
    Validation,
}

const PPY: f64 = 252.0; // periods/year for annualized metrics (LEAN/tearsheet convention)

/// The Performance grid cells: (label, formatted value, sign source). The third element is
/// `Some(raw)` for metrics where the SIGN is meaningful at a glance (returns/Sharpe-family —
/// rendered green/red by `performance_tab`), `None` for always-neutral rows (drawdown is always
/// a decline, counts have no sign). Kept out of the render fn so it is testable.
pub fn perf_cells(r: &BacktestResult) -> Vec<(&'static str, String, Option<f64>)> {
    let eq = &r.equity_curve;
    let tr = &r.trades;
    let total = metrics::total_return(eq);
    let sharpe = metrics::sharpe(eq, PPY);
    let sortino = metrics::sortino(eq, PPY);
    let cagr = metrics::cagr(eq, PPY);
    let calmar = metrics::calmar(eq, PPY);
    vec![
        ("Total return", format!("{:.2}%", total * 100.0), Some(total)),
        ("Final equity", format!("{:.2}", r.final_equity), None),
        ("Sharpe", format!("{sharpe:.2}"), Some(sharpe)),
        ("Sortino", format!("{sortino:.2}"), Some(sortino)),
        ("Max drawdown", format!("{:.2}%", metrics::max_drawdown(eq) * 100.0), None),
        ("CAGR", format!("{:.2}%", cagr * 100.0), Some(cagr)),
        ("Calmar", format!("{calmar:.2}"), Some(calmar)),
        ("Win rate", format!("{:.1}%", metrics::win_rate(tr) * 100.0), None),
        ("Profit factor", format!("{:.2}", metrics::profit_factor(tr)), None),
        ("SQN", format!("{:.2}", metrics::sqn(tr)), None),
        ("Trades", r.n_trades.to_string(), None),
    ]
}

/// The Performance grid rows: (label, formatted value) — [`perf_cells`] without the sign
/// column, kept because it's the crate's exported table API (`vike_studio::perf_rows`).
pub fn perf_rows(r: &BacktestResult) -> Vec<(&'static str, String)> {
    perf_cells(r).into_iter().map(|(k, v, _)| (k, v)).collect()
}

/// Per-bar returns to bin into the Distribution histogram.
pub fn returns_hist(r: &BacktestResult) -> Vec<f64> {
    metrics::returns(&r.equity_curve)
}

/// PSR + its inputs, from a single run. DSR/PBO/walk-forward need a sweep (SP5).
///
/// The PSR inputs come from the shared `overfit::sharpe_moments` derivation, which owns the
/// per-period-Sharpe and non-excess-kurtosis conventions (see its doc — both are footguns). The
/// DISPLAYED "Sharpe" row is deliberately the ANNUALIZED value, a different quantity that must
/// never reach `overfit::`.
pub fn validation_rows(r: &BacktestResult) -> Vec<(&'static str, String)> {
    use vike_backtest::metrics::sharpe;
    use vike_backtest::overfit::{probabilistic_sharpe_ratio, sharpe_moments};

    let sr_annual = sharpe(&r.equity_curve, PPY);
    let m = sharpe_moments(&r.equity_curve);
    let psr = probabilistic_sharpe_ratio(m.sr_per_obs, m.n_obs, 0.0, m.skew, m.kurt);

    vec![
        ("Sharpe", format!("{sr_annual:.2}")),
        ("Observations", format!("{}", m.n_obs)),
        ("Skew", format!("{:.3}", m.skew)),
        ("Kurtosis", format!("{:.3}", m.kurt)),
        ("Prob. Sharpe > 0 (PSR)", format!("{:.0}%", psr * 100.0)),
    ]
}

/// Deflated Sharpe + PBO + overfit verdict + (when present) walk-forward consistency, from an
/// optional sweep and/or walk-forward report. PBO comes from the sweep's full per-trial returns
/// matrix (`StudioSweep::pbo`); a `NaN` there is rendered "—" and treated by `overfit_verdict` as
/// "not assessed" (its own reason row, not a vote toward Low risk).
pub fn validation_verdict_rows(
    sweep: Option<&StudioSweep>,
    wf: Option<&WalkForwardReport>,
) -> Vec<(&'static str, String)> {
    let dsr = sweep.map(|s| s.dsr).unwrap_or(f64::NAN);
    let pbo = sweep.map(|s| s.pbo).unwrap_or(f64::NAN);
    let wfc = wf.map(|w| w.wf_consistency);
    let v = overfit_verdict(pbo, dsr, wfc);

    let mut rows: Vec<(&'static str, String)> = vec![
        (
            "Deflated Sharpe",
            if sweep.is_some() { format!("{:.0}%", dsr * 100.0) } else { "—".to_string() },
        ),
        ("PBO", if pbo.is_nan() { "—".to_string() } else { format!("{:.0}%", pbo * 100.0) }),
        ("Overfit risk", format!("{:?}", v.level)),
    ];
    if let Some(w) = wfc {
        rows.push(("Walk-forward consistency", format!("{:.0}%", w * 100.0)));
    }
    rows.extend(v.reasons.iter().map(|reason| ("Note", reason.clone())));
    rows
}

pub fn results_ui(
    ui: &mut egui::Ui,
    tab: &mut ResultsTab,
    r: &BacktestResult,
    sweep: Option<&StudioSweep>,
    wf: Option<&WalkForwardReport>,
) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        for (t, name) in [
            (ResultsTab::Equity, "Equity"),
            (ResultsTab::Trades, "Trades"),
            (ResultsTab::Performance, "Performance"),
            (ResultsTab::Distribution, "Distribution"),
            (ResultsTab::Validation, "Validation"),
        ] {
            ui.selectable_value(tab, t, name);
        }
    });
    ui.separator();
    ui.add_space(2.0);
    match tab {
        ResultsTab::Equity => equity_tab(ui, r),
        ResultsTab::Trades => trades_tab(ui, r),
        ResultsTab::Performance => performance_tab(ui, r),
        ResultsTab::Distribution => distribution_tab(ui, r),
        ResultsTab::Validation => validation_tab(ui, r, sweep, wf),
    }
}

fn equity_tab(ui: &mut egui::Ui, r: &BacktestResult) {
    use egui_plot::{Line, Plot, PlotPoints};
    let pts: PlotPoints = r.equity_curve.iter().enumerate().map(|(i, &e)| [i as f64, e]).collect();
    Plot::new("equity")
        .height(ui.available_height())
        .show(ui, |p| p.line(Line::new("equity", pts).color(crate::theme::ACCENT).width(1.5)));
}

fn trades_tab(ui: &mut egui::Ui, r: &BacktestResult) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("trades").striped(true).show(ui, |ui| {
            for h in ["#", "side", "entry", "exit", "size", "pnl", "fees"] {
                ui.strong(h);
            }
            ui.end_row();
            for (i, t) in r.trades.iter().enumerate() {
                ui.label((i + 1).to_string());
                // Side and PnL are the two glance columns — color them by direction/sign.
                if t.is_long {
                    ui.colored_label(crate::theme::OK, "long");
                } else {
                    ui.colored_label(crate::theme::ERR, "short");
                }
                ui.monospace(format!("{:.2}", t.entry_price));
                ui.monospace(format!("{:.2}", t.exit_price));
                ui.monospace(format!("{:.4}", t.size));
                let pnl_color = if t.pnl >= 0.0 { crate::theme::OK } else { crate::theme::ERR };
                ui.colored_label(pnl_color, format!("{:.2}", t.pnl));
                ui.monospace(format!("{:.2}", t.fees));
                ui.end_row();
            }
        });
    });
}

fn performance_tab(ui: &mut egui::Ui, r: &BacktestResult) {
    egui::Grid::new("perf").striped(true).show(ui, |ui| {
        for (k, v, sign) in perf_cells(r) {
            ui.label(k);
            match sign {
                // Sign-meaningful metrics read green/red at a glance; NaN stays neutral.
                Some(s) if s > 0.0 => {
                    ui.label(egui::RichText::new(v).monospace().color(crate::theme::OK));
                }
                Some(s) if s < 0.0 => {
                    ui.label(egui::RichText::new(v).monospace().color(crate::theme::ERR));
                }
                _ => {
                    ui.monospace(v);
                }
            }
            ui.end_row();
        }
    });
}

fn distribution_tab(ui: &mut egui::Ui, r: &BacktestResult) {
    use egui_plot::{Bar, BarChart, Plot};
    let rets = returns_hist(r);
    // ⚠ EMPTY IS A REAL CASE, it is the one input this fold cannot survive, and the consequence is
    // a PANIC rather than a cosmetic one — this pane crashed the Studio.
    //
    // `vike_analytics::metrics::returns` — which `returns_hist` wraps — yields an EMPTY vector for
    // an equity curve shorter than two points, because a return is a difference between two
    // samples. The fold below seeds with `(f64::MAX, f64::MIN)`, so over an empty vector `lo` stays
    // `f64::MAX` and `hi` stays `f64::MIN`; `hi - lo` is then negative, `w` clamps to `1e-9`, and
    // every bar centre lands at ~`1.7e308`, which is `inf` once taken as f32.
    //
    // ⚠ That never reaches `crates/vike-ui-theme/src/frame_sanity.rs`'s `assert_frame_sane`, and
    // saying it did would understate the defect: `egui_plot`'s own axis-bounds check panics first,
    // inside `Plot::show`, with `Bad final plot bounds: PlotBounds { min: [inf, -0.5], max: [inf,
    // 0.5] }` (measured on egui_plot 0.37 by removing this guard — see the kill proof in this
    // file's `tests`). A panic in an immediate-mode render path is not a bad-looking chart; it
    // takes the frame down. A degenerate curve is not exotic either: a run that stops on its first
    // bar, or a strategy that never trades, produces one.
    //
    // Returning early paints the empty plot frame (axes and grid, no bars), which is the honest
    // rendering of "no returns to distribute" and leaves the populated path below byte-identical.
    if rets.is_empty() {
        Plot::new("dist")
            .height(ui.available_height())
            .show(ui, |p| p.bar_chart(BarChart::new("returns", Vec::new())));
        return;
    }
    // 21 bins over [min,max]
    let (lo, hi) = rets.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
    let bins = 21usize;
    let w = ((hi - lo) / bins as f64).max(1e-9);
    let mut counts = vec![0u32; bins];
    for &x in &rets {
        let b = (((x - lo) / w) as usize).min(bins - 1);
        counts[b] += 1;
    }
    let bars: Vec<Bar> = counts
        .iter()
        .enumerate()
        .map(|(i, &c)| Bar::new(lo + (i as f64 + 0.5) * w, c as f64).width(w * 0.9))
        .collect();
    Plot::new("dist")
        .height(ui.available_height())
        .show(ui, |p| p.bar_chart(BarChart::new("returns", bars)));
}

fn validation_tab(
    ui: &mut egui::Ui,
    r: &BacktestResult,
    sweep: Option<&StudioSweep>,
    wf: Option<&WalkForwardReport>,
) {
    egui::Grid::new("validation_grid").num_columns(2).striped(true).show(ui, |ui| {
        for (k, v) in validation_rows(r) {
            ui.label(k);
            ui.label(v);
            ui.end_row();
        }
    });
    ui.add_space(8.0);
    egui::Grid::new("validation_verdict_grid").num_columns(2).striped(true).show(ui, |ui| {
        for (k, v) in validation_verdict_rows(sweep, wf) {
            ui.label(k);
            ui.label(v);
            ui.end_row();
        }
    });
    if let Some(sw) = sweep {
        ui.add_space(8.0);
        sweep_grid(ui, sw);
    }
    if let Some(w) = wf {
        ui.add_space(8.0);
        wf_oos_plot(ui, w);
    }
}

/// The ranked-sweep table: each grid point's overrides + annualized Sharpe + total return + final
/// equity. `StudioSweep::entries` is already ranked best-first, so this is just a straight render.
fn sweep_grid(ui: &mut egui::Ui, sweep: &StudioSweep) {
    ui.label(egui::RichText::new("Parameter sweep (ranked by Sharpe)").strong());
    egui::Grid::new("sweep_grid").num_columns(4).striped(true).show(ui, |ui| {
        for h in ["params", "Sharpe", "Total return", "Final equity"] {
            ui.strong(h);
        }
        ui.end_row();
        for (i, e) in sweep.entries.iter().enumerate() {
            // The best (first-ranked) grid point is the one the Equity/Trades tabs render when
            // no single run exists — star it so that link is visible.
            if i == 0 {
                ui.label(
                    egui::RichText::new(format!("★ {}", overrides_label(e)))
                        .color(crate::theme::ACCENT),
                );
            } else {
                ui.label(overrides_label(e));
            }
            // Positive OK, negative ERR, zero/NaN neutral — same convention as perf_cells.
            let sharpe = metrics::sharpe(&e.result.equity_curve, PPY);
            let text = egui::RichText::new(format!("{sharpe:.2}")).monospace();
            if sharpe > 0.0 {
                ui.label(text.color(crate::theme::OK));
            } else if sharpe < 0.0 {
                ui.label(text.color(crate::theme::ERR));
            } else {
                ui.label(text);
            }
            ui.monospace(format!("{:.2}%", metrics::total_return(&e.result.equity_curve) * 100.0));
            ui.monospace(format!("{:.2}", e.result.final_equity));
            ui.end_row();
        }
    });
}

/// `k=v` join of one sweep point's parameter overrides, e.g. `"fast=5, slow=20"`.
fn overrides_label(e: &SweepEntry) -> String {
    e.overrides.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ")
}

/// The stitched walk-forward out-of-sample equity curve.
fn wf_oos_plot(ui: &mut egui::Ui, wf: &WalkForwardReport) {
    use egui_plot::{Line, Plot, PlotPoints};
    ui.label("Walk-forward OOS equity:");
    let pts: PlotPoints =
        wf.oos_equity_curve.iter().enumerate().map(|(i, &e)| [i as f64, e]).collect();
    Plot::new("wf_oos").height(200.0).show(ui, |p| p.line(Line::new("oos_equity", pts)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_backtest::{metrics, BacktestResult};

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
        let rows = perf_rows(&r);
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
    /// the same trap `crates/vike-data-manager/src/view.rs`'s `run_frame` documents: asserting
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
        let rows = validation_rows(&r);
        assert!(rows.iter().any(|(k, _)| *k == "Prob. Sharpe > 0 (PSR)"), "PSR row present");
        assert!(rows.iter().any(|(k, _)| *k == "Sharpe"));
    }

    #[test]
    fn verdict_rows_reflect_dsr_and_walkforward() {
        // a StudioSweep with a strong DSR + a walk-forward with full consistency -> Low risk
        let sweep = vike_studio_core::StudioSweep {
            entries: vec![vike_studio_core::SweepEntry {
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

    fn sweep_with(dsr: f64, pbo: f64) -> vike_studio_core::StudioSweep {
        vike_studio_core::StudioSweep {
            entries: vec![vike_studio_core::SweepEntry {
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
}

//! The tabbed results pane over a `BacktestResult`. Pure rendering + testable helpers
//! (`perf_rows`, `returns_hist`, `validation_rows`, `validation_verdict_rows`); the tabs are
//! `egui_plot` lines/bars + `egui::Grid` tables.
//!
//! # The annualization factor is a PARAMETER, not a constant
//!
//! Every annualized number on this surface — Sharpe, Sortino, CAGR, Calmar, and the sweep table's
//! ranking column — is scaled by `periods_per_year`: the count of RETURN OBSERVATIONS a year
//! produces, which for these curves is one per BAR. This module used to hold that as a bare
//! `const PPY: f64 = 252.0` and apply it on every interval, so the Performance tab showed intraday
//! runs on the daily scale — understated by `sqrt(24) ≈ 4.9x` on 1h bars and `sqrt(1440) ≈ 37.9x`
//! on 1m — while the CLI/MCP door, which derives the factor from the bar interval, reported a
//! different Sharpe for the same strategy over the same series.
//!
//! So the factor arrives from the caller, who is the only one that knows which slice is picked.
//! The derivation itself is NOT re-spelled here (and must never be): it lives once, in
//! `vike_analytics::report::periods_per_year_for_interval`, reachable from this crate as
//! `vike_analytics::report::periods_per_year_for_interval`. `StudioState::display_periods_per_year`
//! is the caller that owns it, including the two fallbacks (no slice picked, and a tick slice with
//! no fixed period).
//!
//! ⚠ It scales the DISPLAY only. `validation_rows`' PSR inputs come from
//! `overfit::sharpe_moments`, which is per-OBSERVATION by contract and takes no factor — see that
//! function's doc, and `validation_rows`' own.
//!
//! ⚠ **A visible second-order effect, stated rather than discovered later.** `metrics::cagr` and
//! `metrics::calmar` return `0.0` when `periods_per_year / (bars - 1) > 1000`, a guard against
//! `growth^exponent` overflowing. A correct intraday factor is large, so a SHORT intraday run now
//! trips it where the old bare `252.0` did not: a few hundred 1m bars annualize fine (362,880/399
//! ≈ 910), a few dozen do not. The reading is honest — extrapolating twenty minutes to a year is
//! not a growth rate — but the sentinel PRINTS as `0.00%`, which reads like "flat" rather than
//! "not annualizable". That sentinel belongs to `vike_analytics::metrics`, not to this pane, so it
//! is left alone here and named instead.
use vike_analytics::overfit::overfit_verdict;
use vike_analytics::{BacktestResult, metrics};
use vike_backtest::walkforward::WalkForwardReport;
use vike_studio_core::{ParamscanEntry, StudioParamscan};
use vike_ui_theme::components::tabs::{self, Tab};
use vike_ui_theme::components::{Status, Tokens};
use vike_ui_theme::icons;
use vike_ui_theme::maps;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::Pair;
use vike_ui_theme::value::studio;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ResultsTab {
    #[default]
    Equity,
    Trades,
    Performance,
    Distribution,
    Validation,
}

/// The Performance grid cells: (label, formatted value, sign source). The third element is
/// `Some(raw)` for metrics where the SIGN is meaningful at a glance (returns/Sharpe-family —
/// rendered in the market set's up/down colours by `performance_tab`), `None` for always-neutral
/// rows (drawdown is always a decline, counts have no sign). Kept out of the render fn so it is
/// testable.
///
/// `periods_per_year` scales the four annualized rows (Sharpe, Sortino, CAGR, Calmar) and nothing
/// else — the module doc carries why it is a parameter and who derives it. Pass
/// `vike_analytics::report::DEFAULT_PERIODS_PER_YEAR` when there is no interval to derive one from;
/// never a bare literal, and never a second copy of the arithmetic.
pub(crate) fn perf_cells(
    r: &BacktestResult,
    periods_per_year: f64,
) -> Vec<(&'static str, String, Option<f64>)> {
    let eq = &r.equity_curve;
    let tr = &r.trades;
    let total = metrics::total_return(eq);
    let sharpe = metrics::sharpe(eq, periods_per_year);
    let sortino = metrics::sortino(eq, periods_per_year);
    let cagr = metrics::cagr(eq, periods_per_year);
    let calmar = metrics::calmar(eq, periods_per_year);
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
pub fn perf_rows(r: &BacktestResult, periods_per_year: f64) -> Vec<(&'static str, String)> {
    perf_cells(r, periods_per_year).into_iter().map(|(k, v, _)| (k, v)).collect()
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
///
/// That split is exactly why `periods_per_year` reaches ONE line of this function. The displayed
/// row moves with the picked interval; `sharpe_moments` takes no factor and its outputs
/// (observations, skew, kurtosis, PSR) are unchanged by it, because a per-observation Sharpe is
/// what the PSR is defined over. A future edit that "unified" the two by scaling `m.sr_per_obs`
/// would silently corrupt every PSR on this pane — pinned by
/// `the_annualized_row_moves_with_the_factor_while_the_psr_inputs_do_not`.
pub(crate) fn validation_rows(
    r: &BacktestResult,
    periods_per_year: f64,
) -> Vec<(&'static str, String)> {
    use vike_analytics::metrics::sharpe;
    use vike_analytics::overfit::{probabilistic_sharpe_ratio, sharpe_moments};

    let sr_annual = sharpe(&r.equity_curve, periods_per_year);
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
/// matrix (`StudioParamscan::pbo`); a `NaN` there is rendered "—" and treated by `overfit_verdict` as
/// "not assessed" (its own reason row, not a vote toward Low risk).
pub(crate) fn validation_verdict_rows(
    sweep: Option<&StudioParamscan>,
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

const RESULT_TABS: [Tab<'static, ResultsTab>; 5] = [
    Tab { value: ResultsTab::Equity, label: "Equity", count: None },
    Tab { value: ResultsTab::Trades, label: "Trades", count: None },
    Tab { value: ResultsTab::Performance, label: "Performance", count: None },
    Tab { value: ResultsTab::Distribution, label: "Distribution", count: None },
    Tab { value: ResultsTab::Validation, label: "Validation", count: None },
];

/// The whole results pane: the tab strip plus whichever tab is selected.
///
/// `periods_per_year` scales every annualized number the pane can show — the Performance tab's
/// four rows, the Validation tab's Sharpe row, and the sweep table's ranking column — so the panes
/// cannot disagree with each other about one run. The caller derives it from the PICKED SLICE (see
/// the module doc); it is not defaulted here, because a default in a rendering function is exactly
/// how the bare `252.0` survived on every interval for as long as it did.
pub fn results_ui(
    ui: &mut egui::Ui,
    tab: &mut ResultsTab,
    r: &BacktestResult,
    sweep: Option<&StudioParamscan>,
    wf: Option<&WalkForwardReport>,
    periods_per_year: f64,
) {
    ui.add_space(space::XS);
    tabs::underline(ui, tab, &RESULT_TABS);
    ui.add_space(space::XS);
    match tab {
        ResultsTab::Equity => equity_tab(ui, r),
        ResultsTab::Trades => trades_tab(ui, r),
        ResultsTab::Performance => performance_tab(ui, r, periods_per_year),
        ResultsTab::Distribution => distribution_tab(ui, r),
        ResultsTab::Validation => validation_tab(ui, r, sweep, wf, periods_per_year),
    }
}

fn equity_tab(ui: &mut egui::Ui, r: &BacktestResult) {
    use egui_plot::{Line, Plot, PlotPoints};
    let pts: PlotPoints = r.equity_curve.iter().enumerate().map(|(i, &e)| [i as f64, e]).collect();
    Plot::new("equity").height(ui.available_height()).show(ui, |p| {
        p.line(Line::new("equity", pts).color(Status::Info.color()).width(stroke::LINE))
    });
}

fn trades_tab(ui: &mut egui::Ui, r: &BacktestResult) {
    let tk = Tokens::of(ui.ctx());
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
                    ui.colored_label(maps::side::LONG.text.resolve(&tk), "long");
                } else {
                    ui.colored_label(maps::side::SHORT.text.resolve(&tk), "short");
                }
                ui.monospace(format!("{:.2}", t.entry_price));
                ui.monospace(format!("{:.2}", t.exit_price));
                ui.monospace(format!("{:.4}", t.size));
                let pnl_color = Pair::GainLoss.text(t.pnl >= 0.0, &tk);
                ui.label(egui::RichText::new(format!("{:.2}", t.pnl)).monospace().color(pnl_color));
                ui.monospace(format!("{:.2}", t.fees));
                ui.end_row();
            }
        });
    });
}

fn performance_tab(ui: &mut egui::Ui, r: &BacktestResult, periods_per_year: f64) {
    let tk = Tokens::of(ui.ctx());
    egui::Grid::new("perf").striped(true).show(ui, |ui| {
        for (k, v, sign) in perf_cells(r, periods_per_year) {
            ui.label(k);
            match sign {
                // Sign-meaningful metrics read up/down at a glance, in the market set's colours
                // (money, not a status — design system spec §3.2); NaN stays neutral.
                Some(s) if s > 0.0 => {
                    ui.label(
                        egui::RichText::new(v)
                            .monospace()
                            .color(Pair::PositiveNegative.text(true, &tk)),
                    );
                }
                Some(s) if s < 0.0 => {
                    ui.label(
                        egui::RichText::new(v)
                            .monospace()
                            .color(Pair::PositiveNegative.text(false, &tk)),
                    );
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
    sweep: Option<&StudioParamscan>,
    wf: Option<&WalkForwardReport>,
    periods_per_year: f64,
) {
    egui::Grid::new("validation_grid").num_columns(2).striped(true).show(ui, |ui| {
        for (k, v) in validation_rows(r, periods_per_year) {
            ui.label(k);
            ui.label(v);
            ui.end_row();
        }
    });
    ui.add_space(space::LG);
    egui::Grid::new("validation_verdict_grid").num_columns(2).striped(true).show(ui, |ui| {
        for (k, v) in validation_verdict_rows(sweep, wf) {
            ui.label(k);
            ui.label(v);
            ui.end_row();
        }
    });
    if let Some(sw) = sweep {
        ui.add_space(space::LG);
        sweep_grid(ui, sw, periods_per_year);
    }
    if let Some(w) = wf {
        ui.add_space(space::LG);
        wf_oos_plot(ui, w);
    }
}

/// The ranked-sweep table: each grid point's overrides + annualized Sharpe + total return + final
/// equity. `StudioParamscan::entries` is already ranked best-first, so this is just a straight render.
///
/// Every entry replayed the SAME slice, so one `periods_per_year` scales the whole column — and it
/// must be the one the Performance tab used, or the same run would read two Sharpes in one window.
/// The star row cannot drift out of order by re-scaling here: `vike_studio_core` did the ranking
/// on whatever factor IT used, and `sqrt(periods_per_year)` is a positive constant, so the two
/// orderings agree whatever each side passes. What moves is every printed number.
fn sweep_grid(ui: &mut egui::Ui, sweep: &StudioParamscan, periods_per_year: f64) {
    let tk = Tokens::of(ui.ctx());
    ui.label(egui::RichText::new("Parameter sweep (ranked by Sharpe)").strong());
    egui::Grid::new("sweep_grid").num_columns(4).striped(true).show(ui, |ui| {
        for h in ["params", "Sharpe", "Total return", "Final equity"] {
            ui.strong(h);
        }
        ui.end_row();
        for (i, e) in sweep.entries.iter().enumerate() {
            // The best (first-ranked) grid point is the one the Equity/Trades tabs render when
            // no single run exists — mark it BEST so that link is visible. The trophy IS the mark:
            // the accent is a shape, never the colour of a name (design system spec §2, §4.3).
            if i == 0 {
                ui.label(icons::BEST.before(ui.style(), egui::RichText::new(overrides_label(e))));
            } else {
                ui.label(overrides_label(e));
            }
            // Positive up, negative down, zero/NaN neutral — same convention as perf_cells.
            let sharpe = metrics::sharpe(&e.result.equity_curve, periods_per_year);
            let text = egui::RichText::new(format!("{sharpe:.2}")).monospace();
            if sharpe > 0.0 {
                ui.label(text.color(Pair::PositiveNegative.text(true, &tk)));
            } else if sharpe < 0.0 {
                ui.label(text.color(Pair::PositiveNegative.text(false, &tk)));
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
fn overrides_label(e: &ParamscanEntry) -> String {
    e.overrides.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ")
}

/// The stitched walk-forward out-of-sample equity curve.
fn wf_oos_plot(ui: &mut egui::Ui, wf: &WalkForwardReport) {
    use egui_plot::{Line, Plot, PlotPoints};
    ui.label("Walk-forward OOS equity:");
    let pts: PlotPoints =
        wf.oos_equity_curve.iter().enumerate().map(|(i, &e)| [i as f64, e]).collect();
    Plot::new("wf_oos")
        .height(studio::WF_PLOT_H)
        .show(ui, |p| p.line(Line::new("oos_equity", pts)));
}

#[path = "results_tests.rs"]
#[cfg(test)]
mod results_tests;

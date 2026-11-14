//! The tabbed results pane over a `BacktestResult`. Pure rendering + testable helpers
//! (`perf_rows`, `returns_hist`, `validation_rows`, `validation_verdict_rows`); the tabs are
//! `egui_plot` lines/bars + `egui::Grid` tables.
//!
//! # It SHOWS the metric catalog and computes none of its own
//!
//! The Performance tab is `BacktestReport::rendered_rows` — every
//! `vike_analytics::metric_catalog::METRICS` row, in catalog order, each cell its unit's
//! `MetricUnit::render` — the rows and the text the CLI, the HTML tearsheet and the desktop print.
//! It used to pick eleven rows of its own (no `funding_paid`, so a perp run's funding cost was
//! invisible here) at precisions of its own (`12.34%` beside the CLI's `12.3400%`). The Validation
//! tab reads the catalog's `returns_skewness`/`returns_kurtosis` (EXCESS kurtosis, the CLI's; its
//! own "Kurtosis" row printed the non-excess value, 3 higher under the same word), takes its PSR
//! from `vike_analytics::overfit::psr_of_curve`, and renders the sweep's own verdict
//! (`StudioParamscan::verdict`) rather than assembling one from the last sweep and the last
//! walk-forward, which could be two different runs.
//!
//! # The annualization factor is a PARAMETER, not a constant
//!
//! Every annualized number on this surface — Sharpe, Sortino, CAGR, Calmar, and the sweep table's
//! ranking column — is scaled by `periods_per_year`: the count of RETURN OBSERVATIONS a year
//! produces, which for these curves is one per BAR. This module used to hold that as a bare
//! `const PERIODS_PER_YEAR: f64 = 252.0` and apply it on every interval, so the Performance tab showed intraday
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
//! ⚠ It scales the DISPLAY only. `validation_rows`' PSR is `overfit::psr_of_curve`, per-OBSERVATION
//! by contract and taking no factor — see that function's doc, and `validation_rows`' own.
//!
//! ⚠ **A visible second-order effect, stated rather than discovered later.** `metrics::cagr` and
//! `metrics::calmar` return `0.0` when `periods_per_year / (bars - 1) > 1000`, a guard against
//! `growth^exponent` overflowing. A correct intraday factor is large, so a SHORT intraday run now
//! trips it where the old bare `252.0` did not: a few hundred 1m bars annualize fine (362,880/399
//! ≈ 910), a few dozen do not. The reading is honest — extrapolating twenty minutes to a year is
//! not a growth rate — but the sentinel PRINTS as `0.0000%`, which reads like "flat" rather than
//! "not annualizable". That sentinel belongs to `vike_analytics::metrics`, not to this pane, so it
//! is left alone here and named instead.
use vike_analytics::metric_catalog::{MetricUnit, spec_for};
use vike_analytics::overfit::psr_of_curve;
use vike_analytics::report::BacktestReport;
use vike_analytics::result::WalkforwardReport;
use vike_analytics::{BacktestResult, metrics};
use vike_model::params::ParamValue;
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

/// The Performance grid: `(catalog id, rendered value)` for EVERY metric-catalog row, in catalog
/// order — `BacktestReport::rendered_rows` over this run, the rows and the text every other door
/// prints. An unrecorded metric reads `not recorded`, never a number.
///
/// `periods_per_year` is the report's annualization factor — the module doc carries why it is a
/// parameter and who derives it. Pass `vike_analytics::report::DEFAULT_PERIODS_PER_YEAR` when there
/// is no interval to derive one from; never a bare literal, and never a second copy of the
/// arithmetic.
pub fn perf_rows(r: &BacktestResult, periods_per_year: f64) -> Vec<(&'static str, String)> {
    BacktestReport::from_result(None, r, periods_per_year).rendered_rows()
}

/// The catalog rows the Validation tab shows for a single run, in this order: the annualized
/// Sharpe and the two return-shape moments PSR is sensitive to.
const VALIDATION_IDS: [&str; 3] = ["sharpe", "returns_skewness", "returns_kurtosis"];

/// The Validation tab's label for the PSR row — not a catalog metric, so it carries its own words.
pub(crate) const PSR_LABEL: &str = "Prob. Sharpe > 0 (PSR)";

/// A fraction as a percentage, the catalog's own `Percent` spelling — for the numbers on this pane
/// that are fractions but not catalog metrics (PSR, DSR, PBO, walk-forward consistency).
fn percent(v: f64) -> String {
    MetricUnit::Percent.render(v)
}

/// Per-bar returns to bin into the Distribution histogram.
pub fn returns_hist(r: &BacktestResult) -> Vec<f64> {
    metrics::returns(&r.equity_curve)
}

/// The single-run Validation rows: the catalog's [`VALIDATION_IDS`] exactly as the Performance tab
/// renders them, then the PSR. DSR/PBO/walk-forward need a sweep ([`validation_verdict_rows`]).
///
/// The PSR is `overfit::psr_of_curve`, which owns the per-observation-Sharpe and non-excess-kurtosis
/// conventions (`overfit::sharpe_moments`' doc — both are footguns). The DISPLAYED `sharpe` row is
/// deliberately the ANNUALIZED value, a different quantity that must never reach `overfit::`, and
/// `returns_kurtosis` is the catalog's EXCESS kurtosis — the number the CLI prints under that id.
///
/// That split is exactly why `periods_per_year` moves the `sharpe` row and nothing else here. A
/// per-observation Sharpe is what the PSR is defined over, so a future edit that "unified" the two by
/// scaling it would silently corrupt every PSR on this pane — pinned by
/// `the_annualized_row_moves_with_the_factor_while_the_psr_does_not`.
pub(crate) fn validation_rows(
    r: &BacktestResult,
    periods_per_year: f64,
) -> Vec<(&'static str, String)> {
    let rows = perf_rows(r, periods_per_year);
    let mut out: Vec<(&'static str, String)> = VALIDATION_IDS
        .iter()
        .filter_map(|id| rows.iter().find(|(k, _)| k == id).cloned())
        .collect();
    out.push((PSR_LABEL, percent(psr_of_curve(&r.equity_curve))));
    out
}

/// The overfit verdict on the last SWEEP (its deflated Sharpe, its PBO, and `StudioParamscan::verdict`
/// — the producer's rule, the trial ledger's, with no walk-forward term), then the last walk-forward's
/// consistency as a row of its own. The two are never folded into one verdict: they can be two
/// different strategies over two different slices. A `NaN` DSR or PBO reads "—" (not assessed — the
/// verdict's own reason row says so, never a vote toward Low risk).
pub(crate) fn validation_verdict_rows(
    sweep: Option<&StudioParamscan>,
    walkforward: Option<&WalkforwardReport>,
) -> Vec<(&'static str, String)> {
    let assessed = |x: f64| if x.is_nan() { "—".to_string() } else { percent(x) };
    let mut rows: Vec<(&'static str, String)> = Vec::new();
    match sweep {
        Some(s) => {
            let v = s.verdict();
            rows.push(("Deflated Sharpe", assessed(s.dsr)));
            rows.push(("PBO", assessed(s.pbo)));
            rows.push(("Overfit risk", format!("{:?}", v.level)));
            rows.extend(v.reasons.into_iter().map(|reason| ("Note", reason)));
        }
        None => rows.push(("Overfit risk", "— (run a param scan to assess it)".to_string())),
    }
    if let Some(w) = walkforward {
        rows.push(("Walk-forward consistency", percent(w.walkforward_consistency)));
    }
    rows
}

/// How many distinct refusal reasons [`refused_line`] names before it counts the rest.
const REFUSED_REASONS_SHOWN: usize = 5;

/// The orders the run REFUSED, the line the pane shows above its tabs, or `None` when it refused
/// none: the count, then each distinct reason in the gate's own words (`BacktestResult::dropped`,
/// aggregated as the report's denials are), most frequent first. A run that holds its first buy and
/// closes nothing because the spot law refused its shorts says so here, beside the flat curve.
pub(crate) fn refused_line(r: &BacktestResult) -> Option<String> {
    let mut reasons = vike_analytics::zero_trade::aggregate_denials(&r.dropped);
    let total: u64 = reasons.iter().map(|(_, n)| n).sum();
    let orders = if total == 1 { "order" } else { "orders" };
    match reasons.as_slice() {
        [] => None,
        [(reason, _)] => Some(format!("{total} {orders} refused: {reason}")),
        _ => {
            reasons.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            let mut lines = vec![format!("{total} {orders} refused:")];
            let shown = reasons.iter().take(REFUSED_REASONS_SHOWN);
            lines.extend(shown.map(|(reason, n)| format!("  {n} × {reason}")));
            let more = reasons.len().saturating_sub(REFUSED_REASONS_SHOWN);
            if more > 0 {
                let word = if more == 1 { "reason" } else { "reasons" };
                lines.push(format!("  … and {more} more {word}"));
            }
            Some(lines.join("\n"))
        }
    }
}

const RESULT_TABS: [Tab<'static, ResultsTab>; 5] = [
    Tab { value: ResultsTab::Equity, label: "Equity", count: None },
    Tab { value: ResultsTab::Trades, label: "Trades", count: None },
    Tab { value: ResultsTab::Performance, label: "Performance", count: None },
    Tab { value: ResultsTab::Distribution, label: "Distribution", count: None },
    Tab { value: ResultsTab::Validation, label: "Validation", count: None },
];

/// The whole results pane: the refused orders when there are any ([`refused_line`]), the tab
/// strip, and whichever tab is selected.
///
/// `periods_per_year` scales every annualized number the pane can show — the Performance tab's
/// annualized catalog rows, the Validation tab's `sharpe` row, and the sweep table's ranking
/// column — so the panes
/// cannot disagree with each other about one run. The caller derives it from the PICKED SLICE (see
/// the module doc); it is not defaulted here, because a default in a rendering function is exactly
/// how the bare `252.0` survived on every interval for as long as it did.
pub fn results_ui(
    ui: &mut egui::Ui,
    tab: &mut ResultsTab,
    r: &BacktestResult,
    sweep: Option<&StudioParamscan>,
    walkforward: Option<&WalkforwardReport>,
    periods_per_year: f64,
) {
    ui.add_space(space::XS);
    if let Some(line) = refused_line(r) {
        ui.label(egui::RichText::new(line).color(Status::Warning.color()));
    }
    tabs::underline(ui, tab, &RESULT_TABS);
    ui.add_space(space::XS);
    match tab {
        ResultsTab::Equity => equity_tab(ui, r),
        ResultsTab::Trades => trades_tab(ui, r),
        ResultsTab::Performance => performance_tab(ui, r, periods_per_year),
        ResultsTab::Distribution => distribution_tab(ui, r),
        ResultsTab::Validation => validation_tab(ui, r, sweep, walkforward, periods_per_year),
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

/// Every catalog row, scrolled (the catalog is longer than the pane is tall). A value whose rendered
/// text is negative reads in the market set's DOWN colour and every other one stays neutral — the
/// desktop tearsheet's rule (`crates/vike-app-core/src/ui/tool_views/tearsheet.rs`'s `tearsheet_rows`),
/// keyed on what the catalog printed rather than on a row list of this pane's: a positive
/// `max_drawdown` or `ulcer_index` is not good news, while a negative number in this catalog never is.
fn performance_tab(ui: &mut egui::Ui, r: &BacktestResult, periods_per_year: f64) {
    let tk = Tokens::of(ui.ctx());
    egui::ScrollArea::vertical().id_salt("perf-scroll").show(ui, |ui| {
        egui::Grid::new("perf").striped(true).show(ui, |ui| {
            for (id, text) in perf_rows(r, periods_per_year) {
                ui.label(id);
                let value = egui::RichText::new(text.as_str()).monospace();
                if text.starts_with('-') {
                    ui.label(value.color(Pair::PositiveNegative.text(false, &tk)));
                } else {
                    ui.label(value);
                }
                ui.end_row();
            }
        });
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
    walkforward: Option<&WalkforwardReport>,
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
        for (k, v) in validation_verdict_rows(sweep, walkforward) {
            ui.label(k);
            ui.label(v);
            ui.end_row();
        }
    });
    if let Some(sw) = sweep {
        ui.add_space(space::LG);
        sweep_grid(ui, sw, periods_per_year);
    }
    if let Some(w) = walkforward {
        ui.add_space(space::LG);
        walkforward_out_of_sample_plot(ui, w);
    }
}

/// One catalog metric's cell: `v` as that row's own `MetricUnit` renders it, so a sweep row prints
/// a number exactly as the Performance tab and the CLI print the same metric.
fn cell(id: &str, v: f64) -> String {
    spec_for(id).map(|s| s.unit.render(v)).unwrap_or_default()
}

/// The ranked-sweep table: each grid point's overrides + annualized Sharpe + total return + final
/// equity, under their catalog ids and in their catalog units. `StudioParamscan::entries` is
/// already ranked best-first, so this is just a straight render. (Three compact numbers per row,
/// not a whole report: a full `BacktestReport` per grid point per frame would compute the long-form
/// block for every point on every repaint.)
///
/// Every entry replayed the SAME slice, so one `periods_per_year` scales the whole column — and it
/// must be the one the Performance tab used, or the same run would read two Sharpes in one window.
/// The star row cannot drift out of order by re-scaling here: `vike_studio_core` did the ranking
/// on whatever factor IT used, and `sqrt(periods_per_year)` is a positive constant, so the two
/// orderings agree whatever each side passes. What moves is every printed number.
fn sweep_grid(ui: &mut egui::Ui, sweep: &StudioParamscan, periods_per_year: f64) {
    let tk = Tokens::of(ui.ctx());
    ui.label(egui::RichText::new("Param scan (ranked by Sharpe)").strong());
    egui::Grid::new("sweep_grid").num_columns(4).striped(true).show(ui, |ui| {
        for h in ["params", "sharpe", "total_return", "final_equity"] {
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
            // Positive up, negative down, zero/NaN neutral: the column this table is RANKED by.
            let sharpe = metrics::sharpe(&e.result.equity_curve, periods_per_year);
            let text = egui::RichText::new(cell("sharpe", sharpe)).monospace();
            if sharpe > 0.0 {
                ui.label(text.color(Pair::PositiveNegative.text(true, &tk)));
            } else if sharpe < 0.0 {
                ui.label(text.color(Pair::PositiveNegative.text(false, &tk)));
            } else {
                ui.label(text);
            }
            ui.monospace(cell("total_return", metrics::total_return(&e.result.equity_curve)));
            ui.monospace(cell("final_equity", e.result.final_equity));
            ui.end_row();
        }
    });
}

/// `k=v` join of one sweep point's parameter overrides, e.g. `"fast=5, slow=20"`.
///
/// A `Float` is printed as an `f64` prints (`5`, `2.5`), the label every sweep row carried while an
/// axis was an `f64`; an `Int` prints as itself (`5`), so an integer knob's row reads the same.
/// Any other kind prints its TOML text.
fn overrides_label(e: &ParamscanEntry) -> String {
    e.overrides
        .iter()
        .map(|(k, v)| match v {
            ParamValue::Float(x) => format!("{k}={x}"),
            other => format!("{k}={other}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The stitched walk-forward out-of-sample equity curve.
fn walkforward_out_of_sample_plot(ui: &mut egui::Ui, walkforward: &WalkforwardReport) {
    use egui_plot::{Line, Plot, PlotPoints};
    ui.label("Walk-forward OOS equity:");
    let pts: PlotPoints = walkforward
        .out_of_sample_equity_curve
        .iter()
        .enumerate()
        .map(|(i, &e)| [i as f64, e])
        .collect();
    Plot::new("wf_oos")
        .height(studio::WF_PLOT_H)
        .show(ui, |p| p.line(Line::new("oos_equity", pts)));
}

#[path = "results_tests.rs"]
#[cfg(test)]
mod results_tests;

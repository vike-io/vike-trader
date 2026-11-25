//! The Walk-forward tab: the last walk-forward's overall line (the stitched out-of-sample results the server
//! computed over the whole stitched curve, `WalkforwardSummaryWire::overall`), one row a window (its
//! `WalkforwardResultsStep`: the bars it tested, the parameters it chose, its own results) and the stitched
//! out-of-sample equity curve. Every number is a catalog metric in its catalog unit, worded and rounded as the
//! rest of the pane ([`super::metric_words`], [`super::shown`]).
//!
//! **Dates** (2026-11-20): each window's train and test halves from its step's `window_times` (the first and last
//! bar of each), and the stitched curve's axis from the summary's `equity_curve_timestamp`. An older server sends
//! neither: a window then shows the bars it tested, the tab says why, and the axis counts samples.

use vike_analytics::INVALID_LABEL;
use vike_analytics::results::TradingResults;
use vike_analytics::walkforward::WalkforwardResultsStep;
use vike_backtesthub_client::summary::WalkforwardSummaryWire;
use vike_model::params::ParamValue;
use vike_ui_theme::components::{ThemeStatus, section};
use vike_ui_theme::metrics::space;
use vike_ui_theme::time_zone::{ThemeDisplayTimeZone, to_naive};
use vike_ui_theme::value::studio;

use super::{cell, equity, metric_words, params_label, percent, shown, to_pane_edge};

/// What the tab says before any walk-forward answered.
pub(crate) const NO_WALKFORWARD: &str =
    "No walk-forward yet: run one from the Param scan & Validate panel.";

/// What the tab says when no window carries its dates (an older server).
const NO_DATES: &str = "This server's walk-forward answer carries each window's tested bars, not its dates: \
     dates need a server update.";

/// A window half's dates, first bar to last, in UTC.
fn half_dates(from: i64, to: i64) -> String {
    let at = |ms: i64| {
        to_naive(ms, ThemeDisplayTimeZone::Utc)
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    };
    format!("{} – {}", at(from), at(to))
}

/// A catalog metric `id` of `results` as the pane shows it, or INVALID for a return or a ratio of results whose
/// equity reached zero.
fn metric(results: &TradingResults, id: &str, v: f64) -> String {
    match &results.invalid {
        Some(_) if id != "n_trades" => INVALID_LABEL.to_string(),
        _ => shown(id, &cell(id, v)),
    }
}

/// The overall line's cells, `(words, value)`: the stitched results' total return, Sharpe, maximum drawdown and
/// trades, then the share of windows profitable out of sample and how many windows there were.
pub(crate) fn walkforward_overall_cells(w: &WalkforwardSummaryWire) -> Vec<(String, String)> {
    let overall = &w.overall;
    let mut cells: Vec<(String, String)> = [
        ("total_return", overall.total_return),
        ("sharpe", overall.sharpe),
        ("max_drawdown", overall.max_drawdown),
        ("n_trades", overall.n_trades as f64),
    ]
    .into_iter()
    .map(|(id, v)| (metric_words(id), metric(overall, id, v)))
    .collect();
    cells.push(("Profitable windows".to_string(), percent(w.profitable_step_share)));
    cells.push(("Windows".to_string(), w.steps.len().to_string()));
    cells
}

/// The steps table's column headers, in [`walkforward_step_row`]'s order.
const STEP_COLUMNS: [&str; 8] = [
    "Window",
    "Trained",
    "Tested",
    "Chosen parameters",
    "Out-of-sample return",
    "Sharpe",
    "Maximum drawdown",
    "Trades closed",
];

/// Window `index` (from 0) as its row: its number, the dates it trained and tested on (`window_times`; without
/// them, from an older server, the bars it tested, `[start, end)`), the parameters it chose, or on a fixed walk
/// (which chooses none) the values `set` its request carried, and its own out-of-sample results.
pub(crate) fn walkforward_step_row(
    index: usize,
    step: &WalkforwardResultsStep,
    set: &[(String, ParamValue)],
) -> [String; 8] {
    let r = &step.results;
    let (start, end) = step.test_range;
    let chosen = match (&step.chosen_params, set) {
        (Some(params), _) => params_label(params),
        (None, []) => "the strategy's own (a fixed walk)".to_string(),
        (None, set) => format!("{} (as set: a fixed walk)", params_label(set)),
    };
    let times = step.window_times;
    let trained = match times.and_then(|t| Some((t.train_start?, t.train_end?))) {
        Some((from, to)) => half_dates(from, to),
        None => "—".to_string(),
    };
    let tested = match times {
        Some(t) => half_dates(t.test_start, t.test_end),
        None => format!("bars {start}–{end}"),
    };
    [
        (index + 1).to_string(),
        trained,
        tested,
        chosen,
        metric(r, "total_return", r.total_return),
        metric(r, "sharpe", r.sharpe),
        metric(r, "max_drawdown", r.max_drawdown),
        metric(r, "n_trades", r.n_trades as f64),
    ]
}

/// The Walk-forward tab: [`walkforward_view`] over the last walk-forward, or [`NO_WALKFORWARD`].
pub(super) fn walkforward_tab(
    ui: &mut egui::Ui,
    walkforward: Option<(&WalkforwardSummaryWire, &[(String, ParamValue)])>,
) {
    match walkforward {
        Some((w, set)) => walkforward_view(ui, w, set),
        None => {
            ui.label(egui::RichText::new(NO_WALKFORWARD).weak());
        }
    }
}

/// A walk-forward's view: the overall line, the windows' table (sideways in a scroll of its own, so a narrow
/// pane does not cut its last columns), and the stitched out-of-sample curve. `set` is the values its request
/// carried ([`walkforward_step_row`]). Also what the central panel shows when a walk-forward is the only result
/// there is.
pub(crate) fn walkforward_view(
    ui: &mut egui::Ui,
    w: &WalkforwardSummaryWire,
    set: &[(String, ParamValue)],
) {
    ui.label(egui::RichText::new("Walk-forward, out of sample (the windows stitched)").strong());
    if let Some(why) = &w.overall.invalid {
        let words = format!("{INVALID_LABEL}: {why}");
        ui.label(egui::RichText::new(words).color(ThemeStatus::Warning.color()));
    }
    section::context_bar(ui, |ui| {
        for (words, value) in walkforward_overall_cells(w) {
            section::meta_cell(ui, &words, &value, true);
            ui.add_space(space::LG);
        }
    });
    ui.add_space(space::SM);
    ui.scope(|ui| {
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        let scroll = egui::ScrollArea::horizontal().max_width(to_pane_edge(ui));
        scroll.id_salt("walkforward_steps_scroll").show(ui, |ui| {
            // A column gap of its own: at the item spacing the Trained and Tested dates ran together.
            let gap = egui::vec2(space::XL2, ui.spacing().item_spacing.y);
            egui::Grid::new("walkforward_steps").striped(true).spacing(gap).show(ui, |ui| {
                for h in STEP_COLUMNS {
                    ui.strong(h);
                }
                ui.end_row();
                for (i, step) in w.steps.iter().enumerate() {
                    for text in walkforward_step_row(i, step, set) {
                        ui.monospace(text);
                    }
                    ui.end_row();
                }
            });
        });
    });
    if w.steps.iter().all(|step| step.window_times.is_none()) {
        ui.label(egui::RichText::new(NO_DATES).weak());
    }
    ui.add_space(space::SM);
    walkforward_out_of_sample_plot(ui, w);
}

/// The stitched walk-forward out-of-sample equity curve, as the server thinned it, on a date axis when the summary
/// carries the samples' times (`equity_curve_timestamp`), else on the samples' count.
fn walkforward_out_of_sample_plot(ui: &mut egui::Ui, walkforward: &WalkforwardSummaryWire) {
    use egui_plot::{GridMark, Line, Plot, PlotPoints};
    ui.label("Out-of-sample equity:");
    let curve = &walkforward.equity_curve;
    let times = &walkforward.equity_curve_timestamp;
    let pts: PlotPoints = curve.iter().map(|&(i, e)| [i as f64, e]).collect();
    let height = ui.available_height().max(studio::WALKFORWARD_PLOT_HEIGHT);
    let span = times.first().zip(times.last()).map(|(&first, &last)| (first, last));
    Plot::new("wf_oos")
        .height(height)
        .x_axis_formatter(|mark: GridMark, _| match span {
            Some(span) => equity::curve_time(curve, times, mark.value)
                .map(|ms| equity::axis_date(span, ms))
                .unwrap_or_default(),
            None => format!("{:.0}", mark.value),
        })
        .show(ui, |p| p.line(Line::new("oos_equity", pts)));
}

//! The Equity tab: the run's thinned equity curve over a dashed line at its starting equity, its drawdown on
//! a pane of its own under it (the two share their x axis and their cursor), dates on that axis, and a hover
//! that reads the date, the equity and the drawdown at the pointer. `egui_plot`, as every plot of the Studio.
//!
//! # What it derives, and how exact each is
//!
//! - **The drawdown** at each point of the thinned curve, through `vike_analytics::metrics::DrawdownWalk` (the
//!   walk `max_drawdown` folds). The thinning keeps every bucket's lowest and highest point and the span of the
//!   maximum drawdown, so the deepest point drawn is the summary's maximum drawdown. ⚠ Not for a tick run kept by
//!   time (`TradingResults::returns_interval_ms`): its report's drawdown is every event's and its curve one close
//!   a step, so this pane can draw shallower than the strip says.
//! - **The dates** are each thinned point's own time, which the summary carries (`RunSummaryWire::curve_timestamp`,
//!   2026-11-20): the hover reads the nearest point's, and the axis interpolates between the two points around a mark
//!   ([`curve_time`]). An older server sends none, and only then are the run's first and last instants read back
//!   from its label ([`label_span`]) and spread evenly over the steps ([`time_at`]): off by a gap's share (a weekend,
//!   a session's close), so that hover says "about".

use egui_plot::{GridMark, HLine, HoverPosition, Line, LineStyle, Plot, PlotPoint, PlotPoints};
use vike_analytics::metric_catalog::MetricUnit;
use vike_analytics::metrics::DrawdownWalk;
use vike_backtesthub_client::summary::RunSummaryWire;
use vike_model::time::parse_utc_timestamp_ms;
use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::time_zone::{ThemeDisplayTimeZone, to_naive};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::chart;

use super::percent;

/// The first and last instants (epoch ms) of the run a results label names (`backtest <file> <from> – <to>
/// UTC`, each to the minute: `vike_analytics::record::TradingRecordSource::label`), or `None` for a label
/// that carries no span (a walk-forward step's, one whose instant is out of the calendar's range).
pub(crate) fn label_span(label: &str) -> Option<(i64, i64)> {
    let (head, to) = label.strip_suffix(" UTC")?.rsplit_once(" – ")?;
    let from = head.get(head.len().checked_sub("2021-01-01 00:00".len())?..)?;
    let ms = |text: &str| parse_utc_timestamp_ms(&format!("{}:00Z", text.replacen(' ', "T", 1)));
    Some((ms(from)?, ms(to)?))
}

/// Step `x` of a curve of `len` steps spread evenly over `span` ([`label_span`]), in epoch ms; `None` without
/// a span, for a curve of one step, and for a step outside the curve (the plot's margin).
pub(crate) fn time_at(span: Option<(i64, i64)>, len: usize, x: f64) -> Option<i64> {
    let (from, to) = span?;
    let last = len.checked_sub(1).filter(|&last| last > 0)? as f64;
    if !(0.0..=last).contains(&x) {
        return None;
    }
    Some(from + ((to - from) as f64 * x / last).round() as i64)
}

/// The time at step `x` of a thinned `curve` whose points carry `times` (one each, in order): a point's own time
/// at its step, interpolated between the two points around `x`; `None` outside the curve (the plot's margin) and
/// when `times` is not the curve's (an older server's summary carries none).
pub(crate) fn curve_time(curve: &[(usize, f64)], times: &[i64], x: f64) -> Option<i64> {
    if times.len() != curve.len() {
        return None;
    }
    let after = curve.partition_point(|&(step, _)| (step as f64) < x);
    let (&(next, _), &at_next) = (curve.get(after)?, times.get(after)?);
    let Some(before) = after.checked_sub(1) else {
        // At or before the first point: its own time at it, nothing before it.
        return (next as f64 <= x).then_some(at_next);
    };
    let (step, at) = (curve[before].0 as f64, times[before]);
    let share = (x - step) / (next as f64 - step);
    Some(at + ((at_next - at) as f64 * share).round() as i64)
}

/// The time at step `x` of `summary`'s curve, and whether it is the run's own ([`curve_time`]) or, from an older
/// server, the label's span spread over the steps ([`time_at`]).
fn time_of(summary: &RunSummaryWire, x: f64) -> Option<(i64, bool)> {
    if !summary.curve_timestamp.is_empty() {
        return curve_time(&summary.curve, &summary.curve_timestamp, x).map(|ms| (ms, true));
    }
    let span = summary.source_label.as_deref().and_then(label_span);
    time_at(span, summary.curve_len, x).map(|ms| (ms, false))
}

/// The first and last instants `summary`'s axis spans: its own times', else its label's ([`label_span`]).
fn span_of(summary: &RunSummaryWire) -> Option<(i64, i64)> {
    match (summary.curve_timestamp.first(), summary.curve_timestamp.last()) {
        (Some(&first), Some(&last)) => Some((first, last)),
        _ => summary.source_label.as_deref().and_then(label_span),
    }
}

/// The date an axis mark reads: the year and month over a span past two years, the day past two days, the
/// day and minute under that.
pub(crate) fn axis_date(span: (i64, i64), ms: i64) -> String {
    let days = (span.1 - span.0) / vike_model::MS_PER_DAY;
    let format = match days {
        d if d > 730 => "%Y-%m",
        d if d > 2 => "%Y-%m-%d",
        _ => "%m-%d %H:%M",
    };
    to_naive(ms, ThemeDisplayTimeZone::Utc)
        .map(|t| t.format(format).to_string())
        .unwrap_or_default()
}

/// What the hover reads at x: the curve point nearest the pointer, its time ([`time_of`]: its own, else "about"
/// from the label's span) or its step without either, its equity and its drawdown below the peak so far (`drops`,
/// one per curve point).
pub(crate) fn hover_text(summary: &RunSummaryWire, drops: &[f64], x: f64) -> Option<String> {
    let curve = &summary.curve;
    let after = curve.partition_point(|&(step, _)| (step as f64) < x);
    // The nearer of the two points around the pointer.
    let nearest = match (after.checked_sub(1), curve.get(after)) {
        (Some(before), Some(&(next, _))) if next as f64 - x < x - curve[before].0 as f64 => after,
        (Some(before), _) => before,
        (None, _) => after,
    };
    let (&(step, equity), &drop) = (curve.get(nearest)?, drops.get(nearest)?);
    let when = match time_of(summary, step as f64) {
        Some((ms, exact)) => {
            let at = to_naive(ms, ThemeDisplayTimeZone::Utc)?.format("%Y-%m-%d %H:%M");
            if exact { format!("{at} UTC") } else { format!("about {at} UTC") }
        }
        None => format!("step {step}"),
    };
    Some(format!("{when}\nequity {}\ndrawdown {}", MetricUnit::Money.render(equity), percent(drop)))
}

/// The pointer's x on a plot, wherever it hovers.
fn pointer_x(position: &HoverPosition<'_>) -> f64 {
    match position {
        HoverPosition::NearDataPoint { position, .. } | HoverPosition::Elsewhere { position } => {
            position.x
        }
    }
}

/// Each curve point's drawdown below the peak so far, as a share of it, at most the whole of it: an equity
/// below zero (a run gone INVALID) is a 100% drawdown, never more.
pub(crate) fn drawdowns(summary: &RunSummaryWire) -> Vec<f64> {
    let mut walk = DrawdownWalk::default();
    summary.curve.iter().map(|&(_, equity)| walk.push(equity).min(1.0)).collect()
}

/// The Equity tab: the equity plot, and under it the drawdown plot, a quarter of the room, carrying the dates.
pub(super) fn equity_tab(ui: &mut egui::Ui, summary: &RunSummaryWire) {
    let tk = ThemeTokens::of(ui.ctx());
    let drops = drawdowns(summary);
    let span = span_of(summary);
    let link = ui.make_persistent_id("equity-and-drawdown");
    let drawdown_height = ui.available_height() / 4.0;
    let equity_height =
        (ui.available_height() - drawdown_height - ui.spacing().item_spacing.y).max(0.0);
    let start = summary.curve.first().map(|&(_, equity)| equity);
    let equity: PlotPoints = summary.curve.iter().map(|&(i, e)| [i as f64, e]).collect();
    let colour = ThemeStatus::Warning.color();
    let shown = Plot::new("equity")
        .height(equity_height)
        .link_axis(link, [true, false])
        .link_cursor(link, [true, false])
        .show_axes([false, true])
        .y_axis_min_width(chart::Y_AXIS_GUTTER_WIDTH)
        .label_formatter(|position: &HoverPosition<'_>| {
            hover_text(summary, &drops, pointer_x(position))
        })
        .show(ui, |p| {
            // The starting equity in a colour and dash of its own, named at the left: in the grid's
            // grey it read as one more grid line, often the very one at the round start.
            if let Some(start) = start {
                let at_start = HLine::new("starting equity", start)
                    .style(LineStyle::dashed_dense())
                    .color(colour)
                    .width(stroke::LINE);
                p.hline(at_start);
            }
            p.line(
                Line::new("equity", equity).color(ThemeStatus::Info.color()).width(stroke::LINE),
            );
        });
    // Its name, painted after the plot so the curve (which begins at it) runs under it, not through
    // it (the owner's pass of 2026-11-21), on the plot's own background.
    if let Some(start) = start {
        let first = summary.curve.first().map_or(0.0, |&(step, _)| step as f64);
        let at = shown.transform.position_from_point(&PlotPoint::new(first, start));
        let painter = ui.painter_at(*shown.transform.frame());
        let words = format!("start {}", MetricUnit::Money.render(start));
        let galley = painter.layout_no_wrap(words, tk.font(ThemeTextRole::Caption), colour);
        let rect = egui::Align2::LEFT_BOTTOM.anchor_size(at, galley.size()).expand(space::XS);
        painter.rect_filled(rect, RADIUS, ui.visuals().extreme_bg_color);
        painter.galley(rect.min + egui::Vec2::splat(space::XS), galley, colour);
    }
    let underwater: PlotPoints =
        summary.curve.iter().zip(&drops).map(|(&(i, _), &drop)| [i as f64, -drop]).collect();
    let down = ThemeSidePair::PositiveNegative.text(false, &tk);
    Plot::new("equity-drawdown")
        .height(drawdown_height)
        .link_axis(link, [true, false])
        .link_cursor(link, [true, false])
        // The same gutter on both panes, so a step stands at the same x in each.
        .y_axis_min_width(chart::Y_AXIS_GUTTER_WIDTH)
        .y_axis_formatter(|mark: GridMark, _| percent(mark.value))
        .x_axis_formatter(|mark: GridMark, _| match span {
            Some(span) => {
                time_of(summary, mark.value).map(|(ms, _)| axis_date(span, ms)).unwrap_or_default()
            }
            None => format!("{:.0}", mark.value),
        })
        .include_y(0.0)
        .label_formatter(|position: &HoverPosition<'_>| {
            hover_text(summary, &drops, pointer_x(position))
        })
        .show(ui, |p| {
            p.line(Line::new("drawdown", underwater).color(down).width(stroke::LINE).fill(0.0))
        });
}

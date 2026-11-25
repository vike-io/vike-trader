//! X-axis grid-mark + tick-label helpers split out of `chart.rs` (chart
//! refactor PR-1): the forming-bar hour/day mark probes and the pure
//! label-pick / mark-merge builders. Bodies are verbatim.

use crate::chart::state::{ChartState, bar::ChartBar, marks::ChartTimeGrain};
use tick_guard::ChartAxisTickGuard;
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;

pub(crate) mod overlay;
pub(crate) mod tick_guard;

/// Hourly-grid mark for the forming bar (index `closed_len`), when it opens
/// exactly on a `state.time_zone()` wall-clock hour — the closed-prefix marks are cached in
/// `ChartState::hour_marks`, so only this O(1) probe runs per frame.
pub(crate) fn forming_hour_mark(state: &ChartState) -> Option<f64> {
    use chrono::Timelike;
    let f = state.bars.get(state.closed_len)?;
    let dt = vike_ui_theme::time_zone::to_naive(f.ot, state.time_zone())?;
    (dt.minute() == 0).then_some(f.t)
}

/// Day-divider mark for the forming bar (index `closed_len`), when its time_zone-local calendar DATE
/// differs from the last CLOSED bar's — the closed-prefix marks are cached in
/// `ChartState::day_marks`, so only this O(1) probe runs per frame. Mirrors `forming_hour_mark`.
pub(crate) fn forming_day_mark(state: &ChartState) -> Option<f64> {
    let f = state.bars.get(state.closed_len)?;
    let last_closed = state.closed_len.checked_sub(1).and_then(|i| state.bars.get(i))?;
    let f_date = vike_ui_theme::time_zone::to_naive(f.ot, state.time_zone())?.date();
    let last_date = vike_ui_theme::time_zone::to_naive(last_closed.ot, state.time_zone())?.date();
    (f_date != last_date).then_some(f.t)
}

/// Pure label pick (task A5): day-mark ticks show "%d %b"; otherwise by grain: SubMinute
/// "%H:%M:%S", Minute "%H:%M", HourPlus "%d %b %H:%M".
pub(crate) fn x_tick_label(
    ot: i64,
    time_zone: ThemeDisplayTimeZone,
    grain: ChartTimeGrain,
    is_day_mark: bool,
) -> String {
    let Some(dt) = vike_ui_theme::time_zone::to_naive(ot, time_zone) else { return String::new() };
    let fmt = if is_day_mark {
        "%d %b"
    } else {
        match grain {
            ChartTimeGrain::SubMinute => "%H:%M:%S",
            ChartTimeGrain::Minute => "%H:%M",
            ChartTimeGrain::HourPlus => "%d %b %H:%M",
        }
    };
    dt.format(fmt).to_string()
}

/// Pure mark merge (task A5): day marks (step = day_step) + hour marks (step = hour_step),
/// hour marks SUPPRESSED when grain == HourPlus; a position in both lists renders once, as a
/// day mark.
pub(crate) fn x_grid_marks(
    hour: &[f64],
    day: &[f64],
    forming_h: Option<f64>,
    forming_d: Option<f64>,
    grain: ChartTimeGrain,
    hour_step: f64,
    day_step: f64,
) -> Vec<egui_plot::GridMark> {
    let mut out: Vec<egui_plot::GridMark> = Vec::with_capacity(hour.len() + day.len() + 2);
    let day_all: Vec<f64> = day.iter().copied().chain(forming_d).collect();
    for &v in &day_all {
        out.push(egui_plot::GridMark { value: v, step_size: day_step });
    }
    if grain != ChartTimeGrain::HourPlus {
        for v in hour.iter().copied().chain(forming_h) {
            if !day_all.contains(&v) {
                out.push(egui_plot::GridMark { value: v, step_size: hour_step });
            }
        }
    }
    out
}

/// Everything the shared x-axis label + grid closures close over. All fields are
/// `Copy` (two shared slices + small scalars), so a call site passes it by value and
/// the fn moves the fields into the two `move` closures — byte-identical to the inline
/// `.x_axis_formatter(...).x_grid_spacer(...)` pair repeated at the price plot and each
/// sub-pane (chart refactor PR-3).
#[derive(Clone, Copy)]
pub(crate) struct ChartAxisXContext<'a> {
    pub bars: &'a [ChartBar],
    pub(crate) hour_marks: &'a [f64],
    pub(crate) day_marks: &'a [f64],
    pub(crate) forming_mark: Option<f64>,
    pub(crate) forming_day: Option<f64>,
    pub time_zone: ThemeDisplayTimeZone,
    pub(crate) grain: ChartTimeGrain,
    pub(crate) hour_step: f64,
    pub(crate) day_step: f64,
    /// The time axis' tick-label guard: [`attach_shared_x_axis`] feeds it the pixel scale and asks it
    /// which labels to leave undrawn (one cut by the window's left edge).
    pub guard: &'a ChartAxisTickGuard,
}

/// Attach the shared bar-indexed time-axis label formatter + grid spacer to a plot.
pub(crate) fn attach_shared_x_axis<'a>(
    plot: egui_plot::Plot<'a>,
    cx: ChartAxisXContext<'a>,
) -> egui_plot::Plot<'a> {
    let ChartAxisXContext {
        bars,
        hour_marks,
        day_marks,
        forming_mark,
        forming_day,
        time_zone,
        grain,
        hour_step,
        day_step,
        guard,
    } = cx;
    plot.x_axis_formatter(move |m, range| {
        let i = m.value.round();
        if i < 0.0 {
            return String::new();
        }
        let Some(b) = bars.get(i as usize) else { return String::new() };
        let is_day =
            day_marks.binary_search_by(|v| v.total_cmp(&i)).is_ok() || forming_day == Some(i);
        let text = x_tick_label(b.ot, time_zone, grain, is_day);
        if guard.hides(m.value, range, &text) { String::new() } else { text }
    })
    .x_grid_spacer(move |input| {
        guard.note_spacing(&input);
        x_grid_marks(hour_marks, day_marks, forming_mark, forming_day, grain, hour_step, day_step)
    })
}

/// Attach ONLY the shared time-axis grid spacer (no labels) — for MIDDLE panes,
/// so their vertical grid lines land at the same x's as the price + bottom panes
/// (TradingView-aligned grid). The x-axis LABELS stay on the bottom pane only
/// (via [`attach_shared_x_axis`]); without this, a middle pane fell back to
/// egui_plot's default grid spacer and its vertical lines were misaligned.
pub(crate) fn attach_shared_x_grid<'a>(
    plot: egui_plot::Plot<'a>,
    cx: ChartAxisXContext<'a>,
) -> egui_plot::Plot<'a> {
    let ChartAxisXContext {
        hour_marks,
        day_marks,
        forming_mark,
        forming_day,
        grain,
        hour_step,
        day_step,
        ..
    } = cx;
    plot.x_grid_spacer(move |_| {
        x_grid_marks(hour_marks, day_marks, forming_mark, forming_day, grain, hour_step, day_step)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Task A5: grid rendering — day dividers + tier-aware x labels + derived steps ---

    #[test]
    fn grid_marks_merge_and_suppress() {
        use crate::chart::state::marks::ChartTimeGrain;
        let hour = vec![60.0, 120.0, 180.0];
        let day = vec![120.0];
        let m = x_grid_marks(&hour, &day, None, None, ChartTimeGrain::Minute, 60.0, 1440.0);
        // 120 appears ONCE, as a day mark (step 1440); 60 & 180 as hour marks.
        assert_eq!(m.len(), 3);
        let at120: Vec<_> = m.iter().filter(|g| g.value == 120.0).collect();
        assert_eq!(at120.len(), 1);
        assert_eq!(at120[0].step_size, 1440.0);
        assert!(m.iter().any(|g| g.value == 60.0 && g.step_size == 60.0));
        // HourPlus: hour marks suppressed entirely.
        let m2 = x_grid_marks(&hour, &day, None, None, ChartTimeGrain::HourPlus, 1.0, 24.0);
        assert_eq!(m2.len(), 1);
        assert_eq!(m2[0].value, 120.0);
        // forming marks included.
        let m3 = x_grid_marks(
            &hour,
            &day,
            Some(200.0),
            Some(201.0),
            ChartTimeGrain::Minute,
            60.0,
            1440.0,
        );
        assert!(m3.iter().any(|g| g.value == 200.0 && g.step_size == 60.0));
        assert!(m3.iter().any(|g| g.value == 201.0 && g.step_size == 1440.0));
    }

    #[test]
    fn tick_labels_by_grain_and_day() {
        use crate::chart::state::marks::ChartTimeGrain;
        use vike_ui_theme::time_zone::ThemeDisplayTimeZone;
        let timestamp = chrono::DateTime::parse_from_rfc3339("2026-07-11T14:03:07Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            x_tick_label(timestamp, ThemeDisplayTimeZone::Utc, ChartTimeGrain::SubMinute, false),
            "14:03:07"
        );
        assert_eq!(
            x_tick_label(timestamp, ThemeDisplayTimeZone::Utc, ChartTimeGrain::Minute, false),
            "14:03"
        );
        assert_eq!(
            x_tick_label(timestamp, ThemeDisplayTimeZone::Utc, ChartTimeGrain::HourPlus, false),
            "11 Jul 14:03"
        );
        assert_eq!(
            x_tick_label(timestamp, ThemeDisplayTimeZone::Utc, ChartTimeGrain::Minute, true),
            "11 Jul"
        );
    }
}

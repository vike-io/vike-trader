//! X-axis grid-mark + tick-label helpers split out of `chart.rs` (chart
//! refactor PR-1): the forming-bar hour/day mark probes and the pure
//! label-pick / mark-merge builders. Bodies are verbatim.

use crate::model::{Bar, ChartState, TimeGrain};
use crate::tz::DisplayTz;

/// Hourly-grid mark for the forming bar (index `closed_len`), when it opens
/// exactly on a `state.tz()` wall-clock hour — the closed-prefix marks are cached in
/// `ChartState::hour_marks`, so only this O(1) probe runs per frame.
pub(crate) fn forming_hour_mark(state: &ChartState) -> Option<f64> {
    use chrono::Timelike;
    let f = state.bars.get(state.closed_len)?;
    let dt = crate::tz::to_naive(f.ot, state.tz())?;
    (dt.minute() == 0).then_some(f.t)
}

/// Day-divider mark for the forming bar (index `closed_len`), when its tz-local calendar DATE
/// differs from the last CLOSED bar's — the closed-prefix marks are cached in
/// `ChartState::day_marks`, so only this O(1) probe runs per frame. Mirrors `forming_hour_mark`.
pub(crate) fn forming_day_mark(state: &ChartState) -> Option<f64> {
    let f = state.bars.get(state.closed_len)?;
    let last_closed = state.closed_len.checked_sub(1).and_then(|i| state.bars.get(i))?;
    let f_date = crate::tz::to_naive(f.ot, state.tz())?.date();
    let last_date = crate::tz::to_naive(last_closed.ot, state.tz())?.date();
    (f_date != last_date).then_some(f.t)
}

/// Pure label pick (task A5): day-mark ticks show "%d %b"; otherwise by grain: Sub60
/// "%H:%M:%S", Minute "%H:%M", HourPlus "%d %b %H:%M".
pub(crate) fn x_tick_label(ot: i64, tz: DisplayTz, grain: TimeGrain, is_day_mark: bool) -> String {
    let Some(dt) = crate::tz::to_naive(ot, tz) else { return String::new() };
    let fmt = if is_day_mark {
        "%d %b"
    } else {
        match grain {
            TimeGrain::Sub60 => "%H:%M:%S",
            TimeGrain::Minute => "%H:%M",
            TimeGrain::HourPlus => "%d %b %H:%M",
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
    grain: TimeGrain,
    hour_step: f64,
    day_step: f64,
) -> Vec<egui_plot::GridMark> {
    let mut out: Vec<egui_plot::GridMark> = Vec::with_capacity(hour.len() + day.len() + 2);
    let day_all: Vec<f64> = day.iter().copied().chain(forming_d).collect();
    for &v in &day_all {
        out.push(egui_plot::GridMark { value: v, step_size: day_step });
    }
    if grain != TimeGrain::HourPlus {
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
pub(crate) struct XAxisCtx<'a> {
    pub bars: &'a [Bar],
    pub hour_marks: &'a [f64],
    pub day_marks: &'a [f64],
    pub forming_mark: Option<f64>,
    pub forming_day: Option<f64>,
    pub tz: DisplayTz,
    pub grain: TimeGrain,
    pub hour_step: f64,
    pub day_step: f64,
}

/// Attach the shared bar-indexed time-axis label formatter + grid spacer to a plot.
pub(crate) fn attach_shared_x_axis<'a>(
    plot: egui_plot::Plot<'a>,
    cx: XAxisCtx<'a>,
) -> egui_plot::Plot<'a> {
    let XAxisCtx {
        bars,
        hour_marks,
        day_marks,
        forming_mark,
        forming_day,
        tz,
        grain,
        hour_step,
        day_step,
    } = cx;
    plot.x_axis_formatter(move |m, _| {
        let i = m.value.round();
        if i < 0.0 {
            return String::new();
        }
        let Some(b) = bars.get(i as usize) else { return String::new() };
        let is_day =
            day_marks.binary_search_by(|v| v.total_cmp(&i)).is_ok() || forming_day == Some(i);
        x_tick_label(b.ot, tz, grain, is_day)
    })
    .x_grid_spacer(move |_| {
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
    cx: XAxisCtx<'a>,
) -> egui_plot::Plot<'a> {
    let XAxisCtx {
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
        use crate::model::TimeGrain;
        let hour = vec![60.0, 120.0, 180.0];
        let day = vec![120.0];
        let m = x_grid_marks(&hour, &day, None, None, TimeGrain::Minute, 60.0, 1440.0);
        // 120 appears ONCE, as a day mark (step 1440); 60 & 180 as hour marks.
        assert_eq!(m.len(), 3);
        let at120: Vec<_> = m.iter().filter(|g| g.value == 120.0).collect();
        assert_eq!(at120.len(), 1);
        assert_eq!(at120[0].step_size, 1440.0);
        assert!(m.iter().any(|g| g.value == 60.0 && g.step_size == 60.0));
        // HourPlus: hour marks suppressed entirely.
        let m2 = x_grid_marks(&hour, &day, None, None, TimeGrain::HourPlus, 1.0, 24.0);
        assert_eq!(m2.len(), 1);
        assert_eq!(m2[0].value, 120.0);
        // forming marks included.
        let m3 =
            x_grid_marks(&hour, &day, Some(200.0), Some(201.0), TimeGrain::Minute, 60.0, 1440.0);
        assert!(m3.iter().any(|g| g.value == 200.0 && g.step_size == 60.0));
        assert!(m3.iter().any(|g| g.value == 201.0 && g.step_size == 1440.0));
    }

    #[test]
    fn tick_labels_by_grain_and_day() {
        use crate::model::TimeGrain;
        use crate::tz::DisplayTz;
        let ts = chrono::DateTime::parse_from_rfc3339("2026-07-11T14:03:07Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(x_tick_label(ts, DisplayTz::Utc, TimeGrain::Sub60, false), "14:03:07");
        assert_eq!(x_tick_label(ts, DisplayTz::Utc, TimeGrain::Minute, false), "14:03");
        assert_eq!(x_tick_label(ts, DisplayTz::Utc, TimeGrain::HourPlus, false), "11 Jul 14:03");
        assert_eq!(x_tick_label(ts, DisplayTz::Utc, TimeGrain::Minute, true), "11 Jul");
    }
}

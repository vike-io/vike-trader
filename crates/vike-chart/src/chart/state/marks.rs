//! The time-axis facts a [`super::ChartState`] caches from its closed bars: the hour and day
//! boundary indices, the median bar interval and its [`ChartTimeGrain`], and the median mark gap.

use super::bar::ChartBar;
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;

/// Bar indices whose open-time falls on a `time_zone` wall-clock hour boundary
/// (minute == 0) — the chart's hourly gridline marks. For any fixed-offset
/// timezone (including :30/:45 offsets) exactly one mark occurs per 60
/// one-minute bars.
pub(crate) fn hour_mark_indices(bars: &[ChartBar], time_zone: ThemeDisplayTimeZone) -> Vec<f64> {
    use chrono::Timelike;
    bars.iter()
        .enumerate()
        .filter_map(|(i, b)| {
            vike_ui_theme::time_zone::to_naive(b.ot, time_zone)
                .is_some_and(|dt| dt.minute() == 0)
                .then_some(i as f64)
        })
        .collect()
}

/// Bar indices (== x values) where the time_zone-local calendar DATE changes vs the previous
/// bar — the day/session divider positions. Index 0 is never a divider.
pub(crate) fn day_mark_indices(bars: &[ChartBar], time_zone: ThemeDisplayTimeZone) -> Vec<f64> {
    let mut out = Vec::new();
    let mut previous_date = None;
    for (i, b) in bars.iter().enumerate() {
        let date = vike_ui_theme::time_zone::to_naive(b.ot, time_zone).map(|dt| dt.date());
        if i > 0 && date.is_some() && date != previous_date {
            out.push(i as f64);
        }
        previous_date = date;
    }
    out
}

/// Median seconds-per-bar over the last ≤512 CLOSED-bar deltas. None when <2 bars.
/// Bounded sample keeps the (bar-close-only) recompute O(512 log 512).
pub(crate) fn median_bar_secs(bars: &[ChartBar]) -> Option<f64> {
    if bars.len() < 2 {
        return None;
    }
    let tail = &bars[bars.len().saturating_sub(513)..];
    let mut deltas: Vec<i64> =
        tail.windows(2).map(|w| w[1].ot - w[0].ot).filter(|d| *d > 0).collect();
    deltas.sort_unstable();
    vike_math::statistics::high_median_of_sorted(&deltas).map(|delta| delta as f64 / 1000.0)
}

/// Coarse bar-duration tier derived from [`median_bar_secs`] — which axis-label/gridline-density
/// regime (task A5) a chart's inferred bar interval falls into: sub-minute tick/second bars,
/// minute-ish bars, or hour-plus/daily bars.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ChartTimeGrain {
    SubMinute,
    Minute,
    HourPlus,
}

/// Bucket a [`median_bar_secs`] reading into a [`ChartTimeGrain`]. `None` (too little history to
/// infer — fewer than 2 closed bars) defaults to `Minute`, the implicit pre-A4 assumption.
pub(crate) fn grain(median_secs: Option<f64>) -> ChartTimeGrain {
    match median_secs {
        Some(s) if s < 60.0 => ChartTimeGrain::SubMinute,
        Some(s) if s >= 3600.0 => ChartTimeGrain::HourPlus,
        _ => ChartTimeGrain::Minute,
    }
}

/// Median index-gap between consecutive marks — the egui_plot `GridMark` step hint. `fallback`
/// covers <2 marks (nothing to derive a gap from: a brand-new chart, or a visible window with no
/// hour/day boundary in it yet).
pub(crate) fn mark_step(marks: &[f64], fallback: f64) -> f64 {
    if marks.len() < 2 {
        return fallback;
    }
    let mut gaps: Vec<f64> = marks.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_unstable_by(|a, b| a.total_cmp(b));
    vike_math::statistics::high_median_of_sorted(&gaps).unwrap_or(fallback)
}

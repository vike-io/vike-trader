//! Oscillator and study sub-pane painting, and the open-time join of a second series.

use super::overlays::{dash_style, segment_line};
use crate::chart::colors::ChartColors;
use crate::chart::state::bar::ChartBar;
use crate::chart::study::ChartStudy;
use egui::Stroke;
use egui_plot::{HLine, PlotPoints, Polygon};
use vike_indicators::OutputStyle;
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::value::chart;

pub(crate) fn render_oscillator(
    p: &mut egui_plot::PlotUi,
    a: &ChartStudy,
    x0: f64,
    x1: f64,
    c: &ChartColors,
) {
    render_oscillator_parts(
        p,
        ChartStudyOscillatorPaint {
            show_bands: a.show_bands,
            show_overbought_oversold_fill: a.show_overbought_oversold_fill,
            bands: &a.bands,
            overbought_fill: a.overbought_fill,
            oversold_fill: a.oversold_fill,
            outputs: &a.outputs,
        },
        x0,
        x1,
        c,
    );
}

/// The oscillator-pane paint surface, borrowed from whatever owns it: an indicator
/// [`ChartStudy`]. It carries the bar-indexed `ChartStudyLine` series + band/fill state that
/// [`render_oscillator_parts`] below paints. ⚠ A tick-driven microstructure `ActiveStudy` was the
/// second owner until it was deleted unreachable (the shell never built one); the struct stays the
/// seam between an owner and the painter.
#[derive(Clone, Copy)]
pub(crate) struct ChartStudyOscillatorPaint<'a> {
    pub show_bands: bool,
    pub show_overbought_oversold_fill: bool,
    pub bands: &'a [crate::chart::study::ChartStudyBand],
    pub overbought_fill: egui::Color32,
    pub oversold_fill: egui::Color32,
    pub outputs: &'a [crate::chart::study::ChartStudyLine],
}

/// Draw one oscillator sub-pane's bands, overbought/oversold fills and output lines.
/// This is `render_oscillator`'s former body verbatim, reading through [`ChartStudyOscillatorPaint`]
/// instead of `&ChartStudy` — byte-identical render for the indicator path.
pub(crate) fn render_oscillator_parts(
    p: &mut egui_plot::PlotUi,
    a: ChartStudyOscillatorPaint<'_>,
    x0: f64,
    x1: f64,
    c: &ChartColors,
) {
    if a.show_bands {
        // Overbought/oversold translucent fills (TradingView RSI "Style" parity):
        // green above the highest band level, red below the lowest. Drawn FIRST so
        // the band hlines + oscillator series paint on top. Off by default
        // (`show_overbought_oversold_fill`), so the pane is pixel-identical until enabled. The
        // pane's y-bounds were set by `pane/sub.rs`; we fill within them (clipped to
        // the plot) across the full visible x-range.
        if a.show_overbought_oversold_fill && !a.bands.is_empty() {
            let bounds = p.plot_bounds();
            let (xmin, xmax) = (bounds.min()[0], bounds.max()[0]);
            let (ymin, ymax) = (bounds.min()[1], bounds.max()[1]);
            let top = a.bands.iter().map(|b| b.value).fold(f64::NEG_INFINITY, f64::max);
            let bottom = a.bands.iter().map(|b| b.value).fold(f64::INFINITY, f64::min);
            let quad = |lo: f64, hi: f64| vec![[xmin, lo], [xmax, lo], [xmax, hi], [xmin, hi]];
            if top < ymax {
                p.polygon(
                    Polygon::new("", PlotPoints::from(quad(top, ymax)))
                        .fill_color(a.overbought_fill)
                        .stroke(Stroke::NONE)
                        .allow_hover(false),
                );
            }
            if bottom > ymin {
                p.polygon(
                    Polygon::new("", PlotPoints::from(quad(ymin, bottom)))
                        .fill_color(a.oversold_fill)
                        .stroke(Stroke::NONE)
                        .allow_hover(false),
                );
            }
        }
        // Per-level band hlines (TradingView RSI "Style"): each level draws in its
        // OWN editable colour + show toggle; a level nobody coloured (`None`) draws in
        // the theme's level line, resolved here so it follows a theme change.
        for band in a.bands {
            if !band.show {
                continue;
            }
            p.hline(
                HLine::new("", band.value)
                    .color(band.color.unwrap_or(c.level_line))
                    .width(stroke::HAIRLINE)
                    .style(egui_plot::LineStyle::dashed_loose())
                    .allow_hover(false),
            );
        }
    }
    for line in a.outputs {
        if !line.visible {
            continue; // per-plot show/hide (T8 Style tab)
        }
        match line.style {
            OutputStyle::Histogram => {
                let vals = &line.series;
                let bars: Vec<egui_plot::Bar> = vals
                    .iter()
                    .enumerate()
                    .filter(|(i, v)| {
                        !v.is_nan() && (*i as f64) >= x0 - 2.0 && (*i as f64) <= x1 + 2.0
                    })
                    .map(|(i, &v)| {
                        let rising = i > 0 && !vals[i - 1].is_nan() && v > vals[i - 1];
                        let col = if rising { c.up_graphic } else { c.down_graphic };
                        egui_plot::Bar::new(i as f64, v).fill(col).width(chart::HISTOGRAM_BAR_WIDTH)
                    })
                    .collect();
                if !bars.is_empty() {
                    p.bar_chart(egui_plot::BarChart::new("", bars));
                }
            }
            // Full series, unsliced (oscillator panes are NOT windowed by T4 — own value
            // domains, x0/x1 filtering happens inside segment_line itself): base = 0.
            _ => segment_line(
                p,
                &line.series,
                line.color,
                line.width,
                dash_style(line.line_style),
                x0,
                x1,
                0,
            ),
        }
    }
}

/// For each PRIMARY bar index in `[lo, hi)`, find the overlay bar whose open-time
/// is nearest-at-or-before that primary bar's `ot`, returning that overlay bar's
/// index (or None where the overlay has no bar at/before that primary time — a
/// leading gap). Both slices are open-time-ascending. This maps a 2nd symbol onto
/// the primary's positional x-index. Reuses sync::nearest_index_by_timestamp's ascending-scan
/// technique, not its nearest-rounding — this is strict floor/at-or-before.
pub(crate) fn reindex_by_ot(
    primary: &[ChartBar],
    overlay: &[ChartBar],
    lo: usize,
    hi: usize,
) -> Vec<Option<usize>> {
    let lo = lo.min(primary.len());
    let hi = hi.min(primary.len());
    if lo >= hi {
        return Vec::new();
    }
    if overlay.is_empty() {
        return vec![None; hi - lo];
    }
    let mut out = Vec::with_capacity(hi - lo);
    // Forward two-pointer walk: both `primary[lo..hi]` and `overlay` are ot-ascending, so `j`
    // only ever advances — O(hi - lo + overlay.len()) total, no per-bar binary search. This is
    // a strict floor/asof lookup (unlike sync::nearest_index_by_timestamp, which can round UP to a
    // closer LATER bar): an overlay candle from the future must never get mapped onto a past
    // primary bar, so `j` only advances while the NEXT overlay bar is still <= the primary ot.
    let mut j = 0usize;
    for bar in &primary[lo..hi] {
        while j + 1 < overlay.len() && overlay[j + 1].ot <= bar.ot {
            j += 1;
        }
        out.push(if overlay[j].ot <= bar.ot { Some(j) } else { None });
    }
    out
}

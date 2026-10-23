//! Oscillator and study sub-pane painting, and the open-time join of a second series.

use super::overlays::{dash_style, seg_line};
use crate::colors::ChartColors;
use crate::indicators::{Active, OutputStyle};
use crate::model::Bar;
use egui::Stroke;
use egui_plot::{HLine, PlotPoints, Polygon};
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::value::chart;

pub(crate) fn render_oscillator(
    p: &mut egui_plot::PlotUi,
    a: &Active,
    x0: f64,
    x1: f64,
    c: &ChartColors,
) {
    render_oscillator_parts(
        p,
        OscPaint {
            show_bands: a.show_bands,
            show_ob_os_fill: a.show_ob_os_fill,
            bands: &a.bands,
            ob_fill: a.ob_fill,
            os_fill: a.os_fill,
            outputs: &a.outputs,
        },
        x0,
        x1,
        c,
    );
}

/// The oscillator-pane paint surface, borrowed from whatever owns it: an indicator
/// [`Active`] or a tick-driven [`crate::studies::ActiveStudy`]. Both carry the same
/// bar-indexed `OutputLine` series + band/fill state, so both render through the ONE
/// [`render_oscillator_parts`] body below — a study pane is pixel-identical in
/// machinery to an indicator pane.
#[derive(Clone, Copy)]
pub(crate) struct OscPaint<'a> {
    pub show_bands: bool,
    pub show_ob_os_fill: bool,
    pub bands: &'a [crate::indicators::BandLevel],
    pub ob_fill: egui::Color32,
    pub os_fill: egui::Color32,
    pub outputs: &'a [crate::indicators::OutputLine],
}

/// Draw one oscillator sub-pane's bands, overbought/oversold fills and output lines.
/// This is `render_oscillator`'s former body verbatim, reading through [`OscPaint`]
/// instead of `&Active` — byte-identical render for the indicator path.
pub(crate) fn render_oscillator_parts(
    p: &mut egui_plot::PlotUi,
    a: OscPaint<'_>,
    x0: f64,
    x1: f64,
    c: &ChartColors,
) {
    if a.show_bands {
        // Overbought/oversold translucent fills (TradingView RSI "Style" parity):
        // green above the highest band level, red below the lowest. Drawn FIRST so
        // the band hlines + oscillator series paint on top. Off by default
        // (`show_ob_os_fill`), so the pane is pixel-identical until enabled. The
        // pane's y-bounds were set by `subpanes.rs`; we fill within them (clipped to
        // the plot) across the full visible x-range.
        if a.show_ob_os_fill && !a.bands.is_empty() {
            let bounds = p.plot_bounds();
            let (xmin, xmax) = (bounds.min()[0], bounds.max()[0]);
            let (ymin, ymax) = (bounds.min()[1], bounds.max()[1]);
            let top = a.bands.iter().map(|b| b.value).fold(f64::NEG_INFINITY, f64::max);
            let bottom = a.bands.iter().map(|b| b.value).fold(f64::INFINITY, f64::min);
            let quad = |lo: f64, hi: f64| vec![[xmin, lo], [xmax, lo], [xmax, hi], [xmin, hi]];
            if top < ymax {
                p.polygon(
                    Polygon::new("", PlotPoints::from(quad(top, ymax)))
                        .fill_color(a.ob_fill)
                        .stroke(Stroke::NONE)
                        .allow_hover(false),
                );
            }
            if bottom > ymin {
                p.polygon(
                    Polygon::new("", PlotPoints::from(quad(ymin, bottom)))
                        .fill_color(a.os_fill)
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
                        let col = if rising { c.up_s } else { c.down_s };
                        egui_plot::Bar::new(i as f64, v).fill(col).width(chart::HISTOGRAM_BAR_W)
                    })
                    .collect();
                if !bars.is_empty() {
                    p.bar_chart(egui_plot::BarChart::new("", bars));
                }
            }
            // Full series, unsliced (oscillator panes are NOT windowed by T4 — own value
            // domains, x0/x1 filtering happens inside seg_line itself): base = 0.
            _ => seg_line(
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

/// Draw one tick-driven microstructure study ([`crate::studies::ActiveStudy`]) into an
/// oscillator sub-pane, through the SAME [`render_oscillator_parts`] body indicator
/// oscillators use — the study's series is already bar-indexed by
/// `ActiveStudy::sync` (see `studies.rs`'s tick->bar mapping contract), so nothing here
/// is study-specific. A gated study (`ActiveStudy::is_empty` — a book study with no L2
/// feed) has an EMPTY series, so this paints only the reference bands: no fabricated
/// values ever reach the pane.
pub fn render_study(
    p: &mut egui_plot::PlotUi,
    s: &crate::studies::ActiveStudy,
    x0: f64,
    x1: f64,
    c: &ChartColors,
) {
    render_oscillator_parts(
        p,
        OscPaint {
            show_bands: s.show_bands,
            show_ob_os_fill: s.show_ob_os_fill,
            bands: &s.bands,
            ob_fill: s.ob_fill,
            os_fill: s.os_fill,
            outputs: &s.outputs,
        },
        x0,
        x1,
        c,
    );
}

/// For each PRIMARY bar index in `[lo, hi)`, find the overlay bar whose open-time
/// is nearest-at-or-before that primary bar's `ot`, returning that overlay bar's
/// index (or None where the overlay has no bar at/before that primary time — a
/// leading gap). Both slices are open-time-ascending. This maps a 2nd symbol onto
/// the primary's positional x-index. Reuses sync::nearest_index_by_ts's ascending-scan
/// technique, not its nearest-rounding — this is strict floor/at-or-before.
pub fn reindex_by_ot(primary: &[Bar], overlay: &[Bar], lo: usize, hi: usize) -> Vec<Option<usize>> {
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
    // a strict floor/asof lookup (unlike sync::nearest_index_by_ts, which can round UP to a
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

//! Close-series painters: line/area, step line, baseline, HLC area and columns.

use crate::colors::ChartColors;
use crate::interact::vis;
use crate::model::Bar;
use egui::Stroke;
use egui_plot::{HLine, Line, PlotPoints, Points, Polygon};
use vike_ui_theme::color::with_alpha;
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::value::chart;

pub(crate) fn draw_line(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    area: bool,
    markers: bool,
    y_floor: f64, // RAW series min low, precomputed by the caller (cached — no per-frame fold)
    map: &dyn Fn(f64) -> f64,
    c: &ChartColors,
) {
    let pts: Vec<[f64; 2]> = vis(bars, x0, x1).map(|b| [b.t, map(b.c)]).collect();
    if pts.len() < 2 {
        return;
    }
    let line = c.line;
    if area {
        let y_floor = map(y_floor);
        // TradingView-style GRADIENT fill: instead of one flat trapezoid per
        // segment, stack K bands from the line down to the floor with the alpha
        // fading to transparent — dense at the line, gone at the bottom. Each
        // band stays a convex quad (per-segment), so no non-convex fan artifact.
        const K: usize = 6;
        for w in pts.windows(2) {
            let (a0, a1) = (w[0], w[1]);
            let lerp = |ly: f64, f: f64| ly + (y_floor - ly) * f;
            for k in 0..K {
                let (f0, f1) = (k as f64 / K as f64, (k + 1) as f64 / K as f64);
                let quad = vec![
                    [a0[0], lerp(a0[1], f0)],
                    [a1[0], lerp(a1[1], f0)],
                    [a1[0], lerp(a1[1], f1)],
                    [a0[0], lerp(a0[1], f1)],
                ];
                let alpha = (f64::from(chart::AREA_TOP_ALPHA) * (1.0 - f0)).round() as u8;
                p.polygon(
                    Polygon::new("", PlotPoints::from(quad))
                        .fill_color(with_alpha(c.line, alpha))
                        .stroke(Stroke::NONE)
                        .allow_hover(false),
                );
            }
        }
    }
    p.line(
        Line::new("", PlotPoints::from(pts.clone()))
            .color(line)
            .width(stroke::LINE)
            .allow_hover(false),
    );
    if markers {
        p.points(
            Points::new("", PlotPoints::from(pts))
                .radius(chart::LINE_MARKER_RADIUS)
                .color(line)
                .filled(true),
        );
    }
}

pub(crate) fn draw_step(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    map: &dyn Fn(f64) -> f64,
    c: &ChartColors,
) {
    let mut pts = Vec::new();
    for b in vis(bars, x0, x1) {
        let y = map(b.c);
        pts.push([b.t - 0.5, y]);
        pts.push([b.t + 0.5, y]);
    }
    if pts.len() >= 2 {
        p.line(
            Line::new("", PlotPoints::from(pts))
                .color(c.line)
                .width(stroke::LINE)
                .allow_hover(false),
        );
    }
}

pub(crate) fn draw_baseline(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    map: &dyn Fn(f64) -> f64,
    c: &ChartColors,
) {
    // vike baseline: green fill+line above the anchor, red below.
    let base_raw = bars.first().map(|b| b.c).unwrap_or(0.0);
    let base = map(base_raw);
    // (t, RAW close, MAPPED close) — the above/below classification vs the
    // baseline anchor stays RAW (semantic, scale-mode-invariant, same
    // reasoning as the candle bull/bear color); only the plotted points map.
    let pts: Vec<(f64, f64, f64)> = vis(bars, x0, x1).map(|b| (b.t, b.c, map(b.c))).collect();
    let up_fill = with_alpha(c.up_s, chart::BASELINE_WASH_ALPHA);
    let dn_fill = with_alpha(c.down_s, chart::BASELINE_WASH_ALPHA);
    for w in pts.windows(2) {
        let above = (w[0].1 + w[1].1) / 2.0 >= base_raw;
        let (col, fill) = if above { (c.up_s, up_fill) } else { (c.down_s, dn_fill) };
        let p0 = [w[0].0, w[0].2];
        let p1 = [w[1].0, w[1].2];
        let quad = vec![p0, p1, [p1[0], base], [p0[0], base]];
        p.polygon(
            Polygon::new("", PlotPoints::from(quad))
                .fill_color(fill)
                .stroke(Stroke::NONE)
                .allow_hover(false),
        );
        p.line(
            Line::new("", PlotPoints::from(vec![p0, p1]))
                .color(col)
                .width(stroke::LINE)
                .allow_hover(false),
        );
    }
    p.hline(
        HLine::new("", base)
            .color(c.zero_line)
            .width(stroke::HAIRLINE)
            .style(egui_plot::LineStyle::dashed_loose())
            .allow_hover(false),
    );
}

pub(crate) fn draw_hlc_area(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    map: &dyn Fn(f64) -> f64,
    c: &ChartColors,
) {
    let v: Vec<&Bar> = vis(bars, x0, x1).collect();
    if v.len() < 2 {
        return;
    }
    let fill = with_alpha(c.line, chart::HLC_FILL_ALPHA);
    // per-segment band quads (high→low) — convex, so no fan artifact. h and l
    // are mapped INDEPENDENTLY (same principle as PnF's box edges): they are
    // two distinct raw values, not a single height to be scaled.
    for w in v.windows(2) {
        let quad = vec![
            [w[0].t, map(w[0].h)],
            [w[1].t, map(w[1].h)],
            [w[1].t, map(w[1].l)],
            [w[0].t, map(w[0].l)],
        ];
        p.polygon(
            Polygon::new("", PlotPoints::from(quad))
                .fill_color(fill)
                .stroke(Stroke::NONE)
                .allow_hover(false),
        );
    }
    let cpts: Vec<[f64; 2]> = v.iter().map(|b| [b.t, map(b.c)]).collect();
    p.line(
        Line::new("", PlotPoints::from(cpts))
            .color(c.line)
            .width(stroke::HAIRLINE)
            .allow_hover(false),
    );
}

pub(crate) fn draw_columns(
    p: &mut egui_plot::PlotUi,
    bars: &[Bar],
    x0: f64,
    x1: f64,
    ymin: f64, // RAW floor, precomputed by the caller
    map: &dyn Fn(f64) -> f64,
    c: &ChartColors,
) {
    let ymin = map(ymin);
    for b in vis(bars, x0, x1) {
        let col = if b.c >= b.o { c.up } else { c.down }; // RAW classification
        let fill = with_alpha(col, chart::COLUMN_FILL_ALPHA);
        let y = map(b.c);
        let body = vec![[b.t - 0.32, ymin], [b.t + 0.32, ymin], [b.t + 0.32, y], [b.t - 0.32, y]];
        p.polygon(
            Polygon::new("", PlotPoints::from(body))
                .fill_color(fill)
                .stroke(Stroke::NONE)
                .allow_hover(false),
        );
    }
}

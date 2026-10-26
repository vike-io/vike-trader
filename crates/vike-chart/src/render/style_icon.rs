//! The chart-style menu icon painter.

use crate::chart::style::ChartStyle;
use egui::{Color32, Rect, Stroke};
use vike_ui_theme::color::faded;
use vike_ui_theme::components::Tokens;
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::side::Pair;
use vike_ui_theme::value::chart;

/// Draw a vike-style mini icon for a chart style into `r` (menu rows + title-bar brand).
pub fn draw_style_icon(p: &egui::Painter, r: Rect, style: ChartStyle) {
    use ChartStyle::*;
    // The menu icon previews what the chart will draw: the installed market colours, and the
    // theme's secondary text for the styles that have no direction (spec §3.5).
    let t = Tokens::of(p.ctx());
    let (up, dn) = Pair::RiseFall.colours(&t);
    let ln = t.theme.text2;
    let pt = |nx: f32, ny: f32| egui::pos2(r.left() + nx * r.width(), r.top() + ny * r.height());
    let vline = |x: f32, y0: f32, y1: f32, col: Color32, w: f32| {
        p.vline(pt(x, 0.0).x, pt(0.0, y0).y..=pt(0.0, y1).y, Stroke::new(w, col));
    };
    let hline = |x0: f32, x1: f32, y: f32, col: Color32, w: f32| {
        p.hline(pt(x0, 0.0).x..=pt(x1, 0.0).x, pt(0.0, y).y, Stroke::new(w, col));
    };
    let body = |xc: f32, y0: f32, y1: f32, col: Color32, hollow: bool| {
        let rr = Rect::from_min_max(pt(xc - 0.12, y0), pt(xc + 0.12, y1));
        if hollow {
            p.rect_stroke(rr, 0.0, Stroke::new(stroke::HAIRLINE, col), egui::StrokeKind::Middle);
        } else {
            p.rect_filled(rr, 0.0, col);
        }
    };
    let line = |pts: Vec<egui::Pos2>, col: Color32, w: f32| {
        p.add(egui::Shape::line(pts, Stroke::new(w, col)));
    };
    match style {
        Candles | HeikinAshi | VolumeCandles => {
            vline(0.33, 0.06, 0.94, up, stroke::HAIRLINE);
            body(0.33, 0.26, 0.64, up, false);
            vline(0.67, 0.2, 0.84, dn, stroke::HAIRLINE);
            body(0.67, 0.36, 0.74, dn, false);
        }
        Hollow => {
            vline(0.33, 0.06, 0.94, up, stroke::HAIRLINE);
            body(0.33, 0.26, 0.64, up, true);
            vline(0.67, 0.2, 0.84, dn, stroke::HAIRLINE);
            body(0.67, 0.36, 0.74, dn, true);
        }
        Bars => {
            vline(0.33, 0.1, 0.9, up, stroke::HAIRLINE);
            hline(0.16, 0.33, 0.34, up, stroke::HAIRLINE);
            hline(0.33, 0.5, 0.62, up, stroke::HAIRLINE);
            vline(0.7, 0.18, 0.82, dn, stroke::HAIRLINE);
            hline(0.53, 0.7, 0.42, dn, stroke::HAIRLINE);
            hline(0.7, 0.87, 0.7, dn, stroke::HAIRLINE);
        }
        HlcBars => {
            vline(0.33, 0.1, 0.9, up, stroke::HAIRLINE);
            hline(0.33, 0.52, 0.62, up, stroke::HAIRLINE);
            vline(0.7, 0.18, 0.82, dn, stroke::HAIRLINE);
            hline(0.7, 0.89, 0.7, dn, stroke::HAIRLINE);
        }
        HighLow => {
            vline(0.34, 0.1, 0.9, ln, stroke::LINE);
            vline(0.66, 0.22, 0.8, ln, stroke::LINE);
        }
        Line | LineMarkers => {
            let path = vec![pt(0.08, 0.72), pt(0.34, 0.36), pt(0.56, 0.6), pt(0.92, 0.22)];
            line(path.clone(), ln, stroke::LINE);
            if matches!(style, LineMarkers) {
                for q in &path {
                    p.circle_filled(*q, chart::ICON_DOT_RADIUS, ln);
                }
            }
        }
        StepLine => {
            line(
                vec![
                    pt(0.08, 0.66),
                    pt(0.35, 0.66),
                    pt(0.35, 0.4),
                    pt(0.62, 0.4),
                    pt(0.62, 0.64),
                    pt(0.92, 0.64),
                ],
                ln,
                stroke::LINE,
            );
        }
        Area | HlcArea => {
            let poly = vec![
                pt(0.08, 0.92),
                pt(0.08, 0.62),
                pt(0.4, 0.34),
                pt(0.64, 0.56),
                pt(0.92, 0.3),
                pt(0.92, 0.92),
            ];
            p.add(egui::Shape::convex_polygon(
                poly,
                faded(up, chart::AREA_ICON_FILL),
                Stroke::new(stroke::LINE, up),
            ));
        }
        Baseline => {
            hline(0.08, 0.92, 0.52, t.theme.analysis_line, stroke::HAIRLINE);
            line(
                vec![pt(0.08, 0.66), pt(0.4, 0.34), pt(0.62, 0.6), pt(0.92, 0.42)],
                up,
                stroke::LINE,
            );
        }
        Columns => {
            for (x, col) in [(0.22, up), (0.42, dn), (0.62, up), (0.82, dn)] {
                let h = if col == up { 0.32 } else { 0.5 };
                p.rect_filled(Rect::from_min_max(pt(x - 0.07, h), pt(x + 0.07, 0.9)), 0.0, col);
            }
        }
        Renko | Range | LineBreak => {
            p.rect_filled(Rect::from_min_max(pt(0.12, 0.5), pt(0.42, 0.72)), 0.0, up);
            p.rect_filled(Rect::from_min_max(pt(0.4, 0.28), pt(0.7, 0.5)), 0.0, up);
            p.rect_filled(Rect::from_min_max(pt(0.56, 0.56), pt(0.86, 0.78)), 0.0, dn);
        }
        Kagi => {
            line(
                vec![
                    pt(0.14, 0.72),
                    pt(0.14, 0.32),
                    pt(0.46, 0.32),
                    pt(0.46, 0.62),
                    pt(0.78, 0.62),
                    pt(0.78, 0.26),
                ],
                ln,
                stroke::LINE,
            );
        }
        PointFigure => {
            line(vec![pt(0.14, 0.3), pt(0.42, 0.62)], up, stroke::LINE);
            line(vec![pt(0.42, 0.3), pt(0.14, 0.62)], up, stroke::LINE);
            p.circle_stroke(pt(0.68, 0.46), 0.16 * r.height(), Stroke::new(stroke::LINE, dn));
        }
        Footprint => {
            // 2-column x 3-row cell grid (sell left / buy right per row) — a miniature
            // footprint ladder, visually distinct from Columns' single-column bars.
            for row in 0..3 {
                let y0 = 0.08 + row as f32 * 0.3;
                let y1 = y0 + 0.22;
                p.rect_filled(Rect::from_min_max(pt(0.08, y0), pt(0.47, y1)), 0.0, dn);
                p.rect_filled(Rect::from_min_max(pt(0.53, y0), pt(0.92, y1)), 0.0, up);
            }
        }
    }
}

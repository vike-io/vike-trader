//! The canvas background: a solid fill, or the TradingView gradient painted behind every pane.

use crate::options::ChartOptions;
use egui::Color32;
use vike_ui_theme::value::chart;

/// Paint a vertical gradient (top→bottom) filling `rect` — the TV "Background:
/// Gradient" canvas. One two-triangle mesh with the top vertices coloured `top`
/// and the bottom `bottom`; it is painted BEFORE the panes draw (so it sits behind
/// them), and the panes then run with a TRANSPARENT `extreme_bg_color` so this
/// gradient shows through. Because it spans the whole pane stack in a single fill,
/// the pane separators reveal its bands — TradingView's continuous-across-panes
/// background. The solid path (`ChartOptions::bg_gradient` off) never calls this.
fn paint_vertical_gradient(
    painter: &egui::Painter,
    rect: egui::Rect,
    top: Color32,
    bottom: Color32,
) {
    use egui::epaint::{Mesh, Vertex, WHITE_UV};
    let mut mesh = Mesh::default();
    for (pos, color) in [
        (rect.left_top(), top),
        (rect.right_top(), top),
        (rect.right_bottom(), bottom),
        (rect.left_bottom(), bottom),
    ] {
        mesh.vertices.push(Vertex { pos, uv: WHITE_UV, color });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// Draw phase — the canvas background: a solid fill, or the TradingView gradient painted once
/// behind the whole pane stack with the plots' own canvas made transparent.
pub(super) fn paint_canvas_background(
    ui: &mut egui::Ui,
    opts: &ChartOptions,
    c: &crate::colors::ChartColors,
    avail: f32,
) {
    // Canvas background (TV "Background"): egui_plot paints its canvas with
    // `extreme_bg_color`, so overriding it here recolors the price pane AND every
    // sub-pane (they share this `ui`). The resolved `c.bg` is the value
    // `vike_ui_theme::appearance::install` already set on Graphite — byte-identical when
    // untouched.
    //
    // Gradient mode (TV "Background: Gradient"): paint a single vertical gradient
    // across the WHOLE pane stack FIRST (so it sits behind every pane), then run the
    // plots with a TRANSPARENT canvas so the gradient shows through. Because it is
    // one continuous fill, the pane separators reveal its bands — the "different
    // pane colors" you see in TradingView. Solid mode is unchanged (byte-identical).
    if opts.bg_gradient {
        // Full chart rect: from the content top down over the pane stack + the
        // bottom time-axis strip, clamped to what's actually available (the same
        // over-report guard `avail` uses above).
        let h = (avail + chart::AXIS_LABEL_H).min(ui.available_height());
        let bg_rect =
            egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), h));
        paint_vertical_gradient(ui.painter(), bg_rect, c.bg_top, c.bg);
        ui.visuals_mut().extreme_bg_color = Color32::TRANSPARENT;
    } else {
        ui.visuals_mut().extreme_bg_color = c.bg;
    }
}

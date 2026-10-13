//! The window-header background (design system spec §2, "Header gradient": the chart's
//! background gradient in window headers — a setting, off by default).
//!
//! Off, it paints nothing: the window's own fill shows through, which is today's look. On, it
//! paints the theme's gradient under the header's contents — the theme's `grad_top` at the top edge
//! down to its `bg` at the bottom, the two stops the chart canvas uses.
//!
//! ⚠ The TOP corners are rounded to the window frame's INNER radius (`RADIUS - 1`: a header sits
//! inside the frame's one-point stroke). A square fill there would paint over the window's rounded
//! corners, which is why `crates/vike-desktop/src/chart_window.rs`'s `title_bar` paints no fill of
//! its own. The mesh is egui's own rounded-rect tessellation, anti-aliased rim included, recoloured
//! by height, so there is no arc arithmetic here: decision 0032 keeps `sin`/`cos` out of production
//! code.

use egui::epaint::{Mesh, RectShape, TessellationOptions, Tessellator};
use egui::{Color32, CornerRadius, Rect};

use crate::appearance::Appearance;
use crate::metrics::RADIUS;
use crate::theme::Theme;

/// Paint the header background for `a` into `rect`, the header's own rect (a title bar).
pub fn paint_header_background(painter: &egui::Painter, rect: Rect, a: &Appearance) {
    if !a.header_gradient {
        return;
    }
    let t = Theme::of(a.theme);
    let mesh = gradient_mesh(rect, t.grad_top, t.bg, painter.ctx().pixels_per_point());
    painter.add(egui::Shape::mesh(mesh));
}

/// `rect` with rounded top corners, filled from `top` at its top edge to `bottom` at its bottom.
pub(crate) fn gradient_mesh(
    rect: Rect,
    top: Color32,
    bottom: Color32,
    pixels_per_point: f32,
) -> Mesh {
    let r = RADIUS.saturating_sub(1);
    let shape =
        RectShape::filled(rect, CornerRadius { nw: r, ne: r, sw: 0, se: 0 }, Color32::WHITE);
    let mut mesh = Mesh::default();
    Tessellator::new(pixels_per_point, TessellationOptions::default(), [1, 1], Vec::new())
        .tessellate_rect(&shape, &mut mesh);
    for v in &mut mesh.vertices {
        // The anti-aliased rim is the tessellator's transparent outer ring: it stays transparent.
        if v.color != Color32::TRANSPARENT {
            let t = ((v.pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
            v.color = top.lerp_to_gamma(bottom, t);
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Theme, ThemeId};

    const RECT: Rect =
        Rect { min: egui::Pos2 { x: 10.0, y: 20.0 }, max: egui::Pos2 { x: 310.0, y: 50.0 } };

    #[test]
    fn off_paints_nothing_and_on_paints_one_mesh() {
        let meshes = |a: Appearance| {
            let ctx = egui::Context::default();
            let out = ctx.run_ui(egui::RawInput::default(), |ui| {
                paint_header_background(ui.painter(), RECT, &a);
            });
            let n = out.shapes.iter().filter(|c| matches!(c.shape, egui::Shape::Mesh(_))).count();
            out.drop_without_applying_deltas();
            n
        };
        assert_eq!(meshes(Appearance::default()), 0, "off is today's look: the window fill shows");
        assert_eq!(meshes(Appearance { header_gradient: true, ..Appearance::default() }), 1);
    }

    #[test]
    fn the_gradient_runs_from_the_themes_top_stop_to_its_background() {
        let near = |a: Color32, b: Color32| {
            a.to_array().iter().zip(b.to_array()).all(|(x, y)| x.abs_diff(y) <= 2)
        };
        for id in ThemeId::ALL {
            let t = Theme::of(id);
            let m = gradient_mesh(RECT, t.grad_top, t.bg, 1.0);
            let opaque: Vec<_> =
                m.vertices.iter().filter(|v| v.color != Color32::TRANSPARENT).collect();
            assert!(!opaque.is_empty(), "{id:?}: nothing painted");
            let top = opaque.iter().min_by(|a, b| a.pos.y.total_cmp(&b.pos.y)).unwrap();
            let bottom = opaque.iter().max_by(|a, b| a.pos.y.total_cmp(&b.pos.y)).unwrap();
            assert!(near(top.color, t.grad_top), "{id:?} top is {:?}", top.color);
            assert!(near(bottom.color, t.bg), "{id:?} bottom is {:?}", bottom.color);
        }
    }

    /// egui's own tessellation of `rect` with `corners` — the yardstick the corner test holds the
    /// header against, so no inset arithmetic is assumed here.
    fn tessellated(rect: Rect, corners: CornerRadius) -> Mesh {
        let mut mesh = Mesh::default();
        Tessellator::new(1.0, TessellationOptions::default(), [1, 1], Vec::new())
            .tessellate_rect(&RectShape::filled(rect, corners, Color32::WHITE), &mut mesh);
        mesh
    }

    /// How close any opaque vertex comes to `corner`, in L1.
    fn reach(mesh: &Mesh, corner: egui::Pos2) -> f32 {
        mesh.vertices
            .iter()
            .filter(|v| v.color != Color32::TRANSPARENT)
            .map(|v| (v.pos.x - corner.x).abs() + (v.pos.y - corner.y).abs())
            .fold(f32::INFINITY, f32::min)
    }

    /// The TOP corners are rounded — a square fill there would hide the window's rounded corners —
    /// and the bottom corners are square, like the body under the header.
    #[test]
    fn the_top_corners_are_rounded_and_the_bottom_ones_square() {
        let header = gradient_mesh(RECT, Color32::WHITE, Color32::WHITE, 1.0);
        let square = tessellated(RECT, CornerRadius::ZERO);
        for c in [RECT.left_top(), RECT.right_top()] {
            assert!(reach(&header, c) >= reach(&square, c) + 1.0, "{c:?} is not rounded");
        }
        for c in [RECT.left_bottom(), RECT.right_bottom()] {
            assert!((reach(&header, c) - reach(&square, c)).abs() < 0.01, "{c:?} is not square");
        }
    }
}

//! What one egui pass PAINTED, read back from its flattened shapes: the texts with their size and
//! resolved colour, the rectangle fills and the line-segment strokes. ONE copy for this crate's
//! component tests and every GUI crate above it (vike-app-core's chrome tests, vike-desktop's chart
//! window, vike-panels' trade tokens each carried their own until 2026-10-31). Compiled for this
//! crate's unit tests and under `test-support`, for the reason `frame_sanity` is.
//!
//! The pass itself, too: [`raw`] (the 800 × 600 screen, or [`raw_sized`]), [`ctx_with`] (a context
//! whose appearance is live) and [`flatten`] (a pass's shapes, `Shape::Vec`s opened), which the kit's
//! tests, vike-panels' cockpit, vike-app-core's window screens and chrome tests and vike-desktop's
//! chart window each spelled until 2026-11-01.

use egui::epaint::ClippedShape;
use egui::{Color32, Shape};

/// An 800 × 600 point screen.
pub fn raw() -> egui::RawInput {
    raw_sized(800.0, 600.0)
}

/// A `w` × `h` point screen, for a surface that needs more room than [`raw`]'s (vike-app-core's
/// caption, vike-desktop's chart title bar).
pub fn raw_sized(w: f32, h: f32) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))),
        ..Default::default()
    }
}

/// A context with `a` installed and one empty pass run, so egui's first-pass work (the fonts
/// built, the style applied) is behind the pass under test (`set_fonts` takes effect on the next
/// pass).
pub fn ctx_with(a: &crate::appearance::Appearance) -> egui::Context {
    let ctx = egui::Context::default();
    crate::appearance::install(&ctx, a);
    ctx.run_ui(raw(), |_| {}).drop_without_applying_deltas();
    ctx
}

/// Every shape of one pass, in paint order, `Shape::Vec`s flattened: what [`texts`], [`fills`] and
/// [`lines`] read (vike-data-manager's and vike-studio's appearance tests carried the walk too).
pub fn flatten(clipped: impl IntoIterator<Item = ClippedShape>) -> Vec<Shape> {
    fn walk(shape: Shape, out: &mut Vec<Shape>) {
        match shape {
            Shape::Vec(v) => v.into_iter().for_each(|s| walk(s, out)),
            s => out.push(s),
        }
    }
    let mut flat = Vec::new();
    clipped.into_iter().for_each(|c| walk(c.shape, &mut flat));
    flat
}

/// `(text, size, colour)` of every text painted, a placeholder colour resolved as the painter
/// resolves it.
pub fn texts(shapes: &[Shape]) -> Vec<(String, f32, Color32)> {
    shapes
        .iter()
        .filter_map(|s| match s {
            Shape::Text(t) => {
                let f = &t.galley.job.sections.first()?.format;
                let c = if f.color == Color32::PLACEHOLDER { t.fallback_color } else { f.color };
                Some((
                    t.galley.text().to_string(),
                    f.font_id.size,
                    t.override_text_color.unwrap_or(c),
                ))
            }
            _ => None,
        })
        .collect()
}

/// The fill colour of every rectangle painted.
pub fn fills(shapes: &[Shape]) -> Vec<Color32> {
    shapes
        .iter()
        .filter_map(|s| match s {
            Shape::Rect(r) => Some(r.fill),
            _ => None,
        })
        .collect()
}

/// The stroke colour of every line segment painted.
pub fn lines(shapes: &[Shape]) -> Vec<Color32> {
    shapes
        .iter()
        .filter_map(|s| match s {
            Shape::LineSegment { stroke, .. } => Some(stroke.color),
            _ => None,
        })
        .collect()
}

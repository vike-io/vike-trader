//! Test helpers for the kit: a context with an appearance installed and its fonts live, the shapes
//! one pass paints, and an accessibility harness.

use egui::accesskit::Role;
use egui::{Color32, Shape};
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;

use crate::appearance::{Appearance, install};

/// A context with `a` installed and one empty pass run, so egui's first-pass work (the fonts
/// built, the style applied) is behind the pass under test.
pub(crate) fn ctx_with(a: &Appearance) -> egui::Context {
    let ctx = egui::Context::default();
    install(&ctx, a);
    ctx.run_ui(raw(), |_| {}).drop_without_applying_deltas();
    ctx
}

/// An 800 × 600 point screen.
pub(crate) fn raw() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
        ..Default::default()
    }
}

/// Every shape one pass of `add` paints, `Shape::Vec`s flattened.
pub(crate) fn paint(ctx: &egui::Context, add: impl FnMut(&mut egui::Ui)) -> Vec<Shape> {
    paint_with(ctx, raw(), add)
}

/// [`paint`] with the pointer at `at`. egui styles a button from the state its PREVIOUS pass
/// ended in, so a hovered look shows on the second such pass.
pub(crate) fn paint_at(
    ctx: &egui::Context,
    at: egui::Pos2,
    add: impl FnMut(&mut egui::Ui),
) -> Vec<Shape> {
    paint_with(ctx, egui::RawInput { events: vec![egui::Event::PointerMoved(at)], ..raw() }, add)
}

/// [`paint_at`] with the primary button HELD DOWN at `at`: one pass that moves there, one that
/// presses, then the pass returned, in which the button is still down. egui decides what a pointer
/// is over from the PREVIOUS pass's layout, hence the three.
pub(crate) fn paint_pressed_at(
    ctx: &egui::Context,
    at: egui::Pos2,
    mut add: impl FnMut(&mut egui::Ui),
) -> Vec<Shape> {
    paint_at(ctx, at, &mut add);
    let press = egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    };
    paint_with(ctx, egui::RawInput { events: vec![press], ..raw() }, &mut add);
    paint(ctx, add)
}

/// [`paint`] once Tab has moved keyboard focus onto the first focusable widget `add` draws: one
/// pass to lay it out, one that presses Tab, then the pass returned.
pub(crate) fn paint_focused(ctx: &egui::Context, mut add: impl FnMut(&mut egui::Ui)) -> Vec<Shape> {
    paint(ctx, &mut add);
    let tab = egui::Event::Key {
        key: egui::Key::Tab,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    paint_with(ctx, egui::RawInput { events: vec![tab], ..raw() }, &mut add);
    paint(ctx, add)
}

fn paint_with(
    ctx: &egui::Context,
    input: egui::RawInput,
    mut add: impl FnMut(&mut egui::Ui),
) -> Vec<Shape> {
    let mut out = ctx.run_ui(input, |ui| add(ui));
    let clipped = std::mem::take(&mut out.shapes);
    out.drop_without_applying_deltas();
    let mut flat = Vec::new();
    for c in clipped {
        flatten(c.shape, &mut flat);
    }
    flat
}

fn flatten(shape: Shape, out: &mut Vec<Shape>) {
    match shape {
        Shape::Vec(v) => v.into_iter().for_each(|s| flatten(s, out)),
        s => out.push(s),
    }
}

/// The fill of every rectangle painted.
pub(crate) fn fills(shapes: &[Shape]) -> Vec<Color32> {
    shapes
        .iter()
        .filter_map(|s| match s {
            Shape::Rect(r) => Some(r.fill),
            _ => None,
        })
        .collect()
}

/// The stroke colour of every rectangle and line segment painted.
pub(crate) fn strokes(shapes: &[Shape]) -> Vec<Color32> {
    shapes
        .iter()
        .filter_map(|s| match s {
            Shape::Rect(r) => Some(r.stroke.color),
            Shape::LineSegment { stroke, .. } => Some(stroke.color),
            _ => None,
        })
        .collect()
}

/// The fill of every circle painted.
pub(crate) fn circles(shapes: &[Shape]) -> Vec<Color32> {
    shapes
        .iter()
        .filter_map(|s| match s {
            Shape::Circle(c) => Some(c.fill),
            _ => None,
        })
        .collect()
}

/// The stroke colour of every circle painted: a ring's colour (a filled disc has none of its own).
pub(crate) fn circle_strokes(shapes: &[Shape]) -> Vec<Color32> {
    shapes
        .iter()
        .filter_map(|s| match s {
            Shape::Circle(c) => Some(c.stroke.color),
            _ => None,
        })
        .collect()
}

/// `(text, colour)` of every text painted — a section's placeholder colour resolved to the
/// shape's fallback, as the painter resolves it.
pub(crate) fn texts(shapes: &[Shape]) -> Vec<(String, Color32)> {
    shapes
        .iter()
        .filter_map(|s| match s {
            Shape::Text(t) => {
                let c = t
                    .galley
                    .job
                    .sections
                    .first()
                    .map(|s| s.format.color)
                    .filter(|c| *c != Color32::PLACEHOLDER)
                    .unwrap_or(t.fallback_color);
                Some((t.galley.text().to_string(), t.override_text_color.unwrap_or(c)))
            }
            _ => None,
        })
        .collect()
}

/// A harness that installs `a` on its first frame and draws `add` from the second on (the fonts
/// land on the next pass). `Harness::run` steps until nothing repaints, so a test reads an
/// installed frame.
pub(crate) fn harness<'a>(a: Appearance, mut add: impl FnMut(&mut egui::Ui) + 'a) -> Harness<'a> {
    let id = egui::Id::new("vike_ui_theme::components::testing::installed");
    let mut h = Harness::builder().with_size(egui::vec2(640.0, 480.0)).build_ui(move |ui| {
        if !ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
            install(ui.ctx(), &a);
            ui.ctx().data_mut(|d| d.insert_temp(id, true));
            ui.ctx().request_repaint();
            return;
        }
        add(ui);
    });
    h.run();
    h
}

/// The text of every node with `role`. egui files a `Label`'s text under `value` and a
/// `Button`'s under `label`.
pub(crate) fn named(h: &Harness<'_>, role: Role) -> Vec<String> {
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == role)
        .map(|n| {
            let a = n.accesskit_node();
            a.label().or_else(|| a.value()).unwrap_or_default()
        })
        .collect()
}

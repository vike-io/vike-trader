//! Section structure (spec §4.2): the collapsible section with its controls on the header row, the
//! breadcrumb, the strip rule, the meta cell and the context bar.

use std::fmt::Debug;
use std::hash::Hash;

use egui::{
    Align2, CornerRadius, FontId, Response, RichText, Sense, Stroke, Ui, WidgetInfo, WidgetType,
};

use super::Tokens;
use crate::icons;
use crate::metrics::{RADIUS, stroke};
use crate::type_scale::TextRole;

/// A section whose header folds its body. The header row carries the title (Title role) behind a
/// disclosure caret, and the section's own `controls` right-aligned on the SAME row — a control that
/// belongs to a section sits on its header, never under it (spec §4.2). Open by default; the state
/// is kept per `id`. Returns whether the body was drawn.
pub fn section(
    ui: &mut Ui,
    id: impl Hash + Debug,
    title: &str,
    controls: impl FnOnce(&mut Ui),
    body: impl FnOnce(&mut Ui),
) -> bool {
    let t = Tokens::of(ui.ctx());
    let id = ui.make_persistent_id(id);
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(true);
    ui.horizontal(|ui| {
        ui.set_min_height(t.metrics.control_h);
        if header(ui, &t, title, open).clicked() {
            open = !open;
            ui.data_mut(|d| d.insert_temp(id, open));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), controls);
    });
    if open {
        body(ui);
    }
    open
}

/// The header's clickable part — the disclosure caret, then the title — one control height tall.
/// Painted here rather than as an `egui::Button`, so it carries exactly ONE widget info: the title
/// as its name and whether it is open, reported the way egui reports a collapsing header.
fn header(ui: &mut Ui, t: &Tokens, title: &str, open: bool) -> Response {
    let caret_px = t.text.px(TextRole::Body);
    let words =
        ui.painter().layout_no_wrap(title.to_string(), t.font(TextRole::Title), t.theme.text);
    let size = egui::vec2(caret_px + t.metrics.gap + words.size().x, t.metrics.control_h);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let caret = if open { icons::DISCLOSE_OPEN } else { icons::DISCLOSE_CLOSED };
    let caret_at = egui::pos2(rect.left() + caret_px / 2.0, rect.center().y);
    let caret_font = FontId::new(caret_px, icons::family());
    caret.paint(p, caret_at, Align2::CENTER_CENTER, caret_font, t.theme.text3);
    let words_at =
        egui::pos2(rect.left() + caret_px + t.metrics.gap, rect.center().y - words.size().y / 2.0);
    p.galley(words_at, words, t.theme.text);
    super::focus_ring(ui, t, &resp);
    resp.widget_info(|| WidgetInfo::selected(WidgetType::CollapsingHeader, true, open, title));
    resp
}

/// A breadcrumb (spec §4.2, from the Data Manager's `crumb`): `ancestor › … › leaf`, a
/// right-aligned summary, then a strip rule. Ancestors are the caption grey, the leaf the text
/// colour, the separators the border colour.
pub fn breadcrumb(ui: &mut Ui, trail: &[&str], summary: &str) {
    let t = Tokens::of(ui.ctx());
    let font = t.font(TextRole::Body);
    ui.horizontal(|ui| {
        let last = trail.len().saturating_sub(1);
        for (i, part) in trail.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("\u{203A}").font(font.clone()).color(t.theme.border));
            }
            let ink = if i == last { t.theme.text } else { t.theme.text3 };
            ui.label(RichText::new(*part).font(font.clone()).color(ink));
        }
        if !summary.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(summary).font(font.clone()).color(t.theme.text3));
            });
        }
    });
    strip_rule(ui);
}

/// A hairline across the available width, the density's gap above and below — what closes an
/// action strip (spec §4.2).
pub fn strip_rule(ui: &mut Ui) {
    let t = Tokens::of(ui.ctx());
    ui.add_space(t.metrics.gap);
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, Stroke::new(stroke::HAIRLINE, t.theme.border));
    ui.add_space(t.metrics.gap);
}

/// A meta cell (spec §4.2, from the Connections detail pane): a caption over its value. `mono` for a
/// value read character by character — a key name, an address, a number (spec §3.3).
pub fn meta_cell(ui: &mut Ui, label: &str, value: &str, mono: bool) -> Response {
    let t = Tokens::of(ui.ctx());
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.label(RichText::new(label).font(t.font(TextRole::Caption)).color(t.theme.text3));
        let f = if mono { t.mono(TextRole::Body) } else { t.font(TextRole::Body) };
        ui.label(RichText::new(value).font(f).color(t.theme.text));
    })
    .response
}

/// A context bar: the full-width strip that says what a screen is looking at — the Data Manager's
/// store bar, the Connections strip (spec §4.2). The surface fill, a border, the density's padding.
pub fn context_bar<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    let t = Tokens::of(ui.ctx());
    egui::Frame::new()
        .fill(t.theme.surface)
        .stroke(Stroke::new(stroke::HAIRLINE, t.theme.border))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(egui::Margin::symmetric(t.metrics.pad as i8, t.metrics.gap as i8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(add).inner
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::testing::{ctx_with, fills, harness, paint, strokes, texts};
    use egui_kittest::kittest::NodeT;
    use std::cell::Cell;
    use std::rc::Rc;

    /// The header folds its body, and the section's controls sit on the header's own row — above
    /// the body, never under it.
    #[test]
    fn a_section_folds_and_its_controls_sit_on_its_header_row() {
        // (open, the control's centre, the body's top)
        let seen = Rc::new(Cell::new((false, 0.0f32, f32::MAX)));
        let s = seen.clone();
        let mut h = harness(Appearance::default(), move |ui| {
            let (mut ctrl_y, mut body_top) = (0.0, f32::MAX);
            let open = section(
                ui,
                "positions",
                "Positions",
                |ui| ctrl_y = ui.button("Add").rect.center().y,
                |ui| body_top = ui.cursor().top(),
            );
            s.set((open, ctrl_y, body_top));
        });
        let (open, ctrl_y, body_top) = seen.get();
        assert!(open, "a section starts open");
        assert!(
            ctrl_y < body_top,
            "the controls ({ctrl_y}) sit on the header row, above the body ({body_top})"
        );
        h.root()
            .children_recursive()
            .find(|n| n.accesskit_node().label().as_deref() == Some("Positions"))
            .expect("the header is a button named by its title")
            .click();
        h.run();
        assert!(!seen.get().0, "clicking the header folds it");
    }

    #[test]
    fn the_breadcrumb_dims_its_ancestors_and_ends_on_a_rule() {
        let ctx = ctx_with(&Appearance::default());
        let t = Tokens::of(&ctx);
        let shapes =
            paint(&ctx, |ui| breadcrumb(ui, &["Data Manager", "All series"], "1,204 series"));
        let tx = texts(&shapes);
        assert!(tx.contains(&("Data Manager".to_string(), t.theme.text3)), "{tx:?}");
        assert!(tx.contains(&("All series".to_string(), t.theme.text)), "{tx:?}");
        assert!(strokes(&shapes).contains(&t.theme.border));
    }

    #[test]
    fn the_context_bar_is_the_surface_with_a_border_and_meta_is_caption_over_value() {
        let ctx = ctx_with(&Appearance::default());
        let t = Tokens::of(&ctx);
        let shapes = paint(&ctx, |ui| {
            context_bar(ui, |ui| meta_cell(ui, "Key", "BINANCE_LIVE_API_KEY", true));
        });
        assert!(fills(&shapes).contains(&t.theme.surface));
        assert!(strokes(&shapes).contains(&t.theme.border));
        let tx = texts(&shapes);
        assert!(tx.contains(&("Key".to_string(), t.theme.text3)), "{tx:?}");
        assert!(tx.contains(&("BINANCE_LIVE_API_KEY".to_string(), t.theme.text)), "{tx:?}");
    }
}

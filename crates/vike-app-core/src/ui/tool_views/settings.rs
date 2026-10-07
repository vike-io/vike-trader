//! The **Settings** window's body (design system spec §5). Its one section today is
//! **Appearance**: the five appearance settings as choices, the two colour settings as painted
//! previews. A click hands the choice to `crate::ui::appearance_settings::AppearanceSession`, which
//! applies it to every window once this frame has drawn and saves it as one row of this computer's
//! settings database. A save the database refused — most often because another writer held it —
//! is offered again with a "Save again" button.
//!
//! It is the first screen painted only from the CURRENT theme's tokens, so it follows every change
//! it makes. It keeps two rules (spec §2, §4.2). The accent is a SHAPE — the ring round the
//! selected preview, the line under the selected choice — never a coloured word. And a selected item
//! is a LABEL, not a button, which is how the accessibility tree reads the selection;
//! `crates/vike-app-core/tests/settings_window.rs` depends on it.

use egui::{
    Align2, CornerRadius, FontId, Rect, RichText, Sense, Stroke, StrokeKind, WidgetInfo,
    WidgetType, pos2, vec2,
};
use vike_ui_theme::market::MarketId;
use vike_ui_theme::metrics::{Density, RADIUS, space, stroke};
use vike_ui_theme::preview::{PREVIEW_SIZE, paint_market_preview, paint_theme_preview};
use vike_ui_theme::theme::{Theme, ThemeId};
use vike_ui_theme::type_scale::{TextRole, TextSize};
use vike_ui_theme::value::settings;

use crate::ui::appearance_settings::{
    AppearanceSession, MIGRATION_NOTE, SAVED_NOTE, SaveLine, pair_note, where_saved,
};

/// How far the selection ring reaches past a preview: `expand(space::XS)` plus its `stroke::EDGE`
/// outside stroke.
/// A tile reserves it on every side, so a body that clips to its own rect — a maximized or
/// user-resized window's scroll area — never cuts the ring.
const RING_REACH: f32 = space::XS + stroke::EDGE;

/// The Settings window's body. `session` is the app's one appearance session.
pub fn settings_tool_content(ui: &mut egui::Ui, session: &mut AppearanceSession) {
    let a = session.appearance();
    let t = Theme::of(a.theme);
    let mut next = a;

    ui.label(RichText::new("Appearance").size(a.text_size.px(TextRole::Title)).strong());
    let whose = where_saved(session.settings_dir(), session.database_present());
    ui.label(RichText::new(whose).color(t.text2));
    ui.label(RichText::new(MIGRATION_NOTE).color(t.text3));

    heading(ui, "Theme", a.text_size);
    ui.horizontal_wrapped(|ui| {
        for id in ThemeId::ALL {
            let paint =
                |p: &egui::Painter, r: Rect| paint_theme_preview(p, r, id, a.header_gradient);
            if tile(ui, t, a.text_size, id.label(), id == a.theme, paint) {
                next.theme = id;
            }
        }
    });
    heading(ui, "Market colours", a.text_size);
    ui.horizontal_wrapped(|ui| {
        for m in MarketId::ALL {
            let paint = |p: &egui::Painter, r: Rect| paint_market_preview(p, r, m, a.theme);
            if tile(ui, t, a.text_size, m.label(), m == a.market, paint) {
                next.market = m;
            }
        }
    });
    if let Some(note) = pair_note(&a) {
        ui.label(RichText::new(note).color(t.text2));
    }
    ui.add_space(space::MD);
    ui.checkbox(&mut next.header_gradient, "Gradient in window headers");

    heading(ui, "Density", a.text_size);
    ui.horizontal(|ui| {
        for d in Density::ALL {
            if segment(ui, t, d.label(), d == a.density) {
                next.density = d;
            }
        }
    });
    heading(ui, "Text size", a.text_size);
    ui.horizontal(|ui| {
        for s in TextSize::ALL {
            if segment(ui, t, s.label(), s == a.text_size) {
                next.text_size = s;
            }
        }
    });

    ui.add_space(space::LG);
    match session.last() {
        SaveLine::Idle => {}
        SaveLine::Saved => {
            ui.label(RichText::new(SAVED_NOTE).color(t.text2));
        }
        SaveLine::NotSaved(why) => {
            ui.label(RichText::new(why.as_str()).color(ui.visuals().warn_fg_color));
        }
    }
    // A refused save (most often another writer holding the store) is offered again: requesting
    // the look in force saves whatever of it the database does not hold yet.
    let save_again = session.can_save_again() && ui.button("Save again").clicked();
    if next != a {
        session.request(next);
    } else if save_again {
        session.request(a);
    }
}

/// A group heading, in the Strong role.
fn heading(ui: &mut egui::Ui, text: &str, size: TextSize) {
    ui.add_space(space::XL);
    ui.label(RichText::new(text).size(size.px(TextRole::Strong)).strong());
    ui.add_space(space::SM);
}

/// One preview with its name under it. The selected one is a LABEL with the accent ring; every
/// other one is a button, ringed in the border colour on hover. `true` when it was clicked.
fn tile(
    ui: &mut egui::Ui,
    t: &Theme,
    size: TextSize,
    name: &str,
    selected: bool,
    paint: impl FnOnce(&egui::Painter, Rect),
) -> bool {
    let sense = if selected { Sense::hover() } else { Sense::click() };
    let reach = vec2(RING_REACH, RING_REACH);
    let (rect, resp) =
        ui.allocate_exact_size(PREVIEW_SIZE + reach * 2.0 + vec2(0.0, settings::NAME_H), sense);
    let preview = Rect::from_min_size(rect.min + reach, PREVIEW_SIZE);
    paint(ui.painter(), preview);
    let ring = if selected {
        Some(Stroke::new(stroke::EDGE, t.accent))
    } else if resp.hovered() {
        Some(Stroke::new(stroke::HAIRLINE, t.border))
    } else {
        None
    };
    if let Some(stroke) = ring {
        ui.painter().rect_stroke(
            preview.expand(space::XS),
            CornerRadius::same(RADIUS),
            stroke,
            StrokeKind::Outside,
        );
    }
    ui.painter().text(
        pos2(preview.center().x, preview.bottom() + settings::NAME_H / 2.0 + space::HAIR),
        Align2::CENTER_CENTER,
        name,
        FontId::proportional(size.px(TextRole::Caption)),
        if selected { t.text } else { t.text2 },
    );
    let typ = if selected { WidgetType::Label } else { WidgetType::Button };
    resp.widget_info(|| WidgetInfo::labeled(typ, true, name));
    resp.clicked()
}

/// One choice of a Density or Text size row. The selected one is a LABEL with the accent line
/// under it; the others are buttons. `true` when a button was clicked.
fn segment(ui: &mut egui::Ui, t: &Theme, text: &str, selected: bool) -> bool {
    if selected {
        let r = ui.label(RichText::new(text).strong()).rect;
        ui.painter().hline(
            r.x_range(),
            r.bottom() + space::HAIR,
            Stroke::new(stroke::EDGE, t.accent),
        );
        false
    } else {
        ui.button(text).clicked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_ui_theme::appearance::{Appearance, install};

    /// Final review (re-graded Important): the selected tile's accent ring stays inside the
    /// body it was drawn in. A maximized or user-resized window lays its body out in a scroll
    /// area that clips to exactly that rect, so a ring reaching past it loses its edge — on
    /// Graphite and Classic, the DEFAULT choices, which are first in their rows.
    #[test]
    fn the_selection_ring_stays_inside_the_body() {
        let ctx = egui::Context::default();
        install(&ctx, &Appearance::default());
        let mut session = AppearanceSession::new(Appearance::default(), None);
        let body = Rect::from_min_size(pos2(40.0, 40.0), vec2(700.0, 700.0));
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
                settings_tool_content(ui, &mut session);
            });
        });
        let accent = Theme::of(ThemeId::Graphite).accent;
        let rings: Vec<Rect> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Rect(r) if r.stroke.color == accent => Some(r.visual_bounding_rect()),
                _ => None,
            })
            .collect();
        out.drop_without_applying_deltas();
        assert_eq!(rings.len(), 2, "one ring per preview row: the selected theme and market set");
        for r in rings {
            assert!(body.contains_rect(r), "the ring {r:?} leaves the body {body:?}");
        }
    }
}

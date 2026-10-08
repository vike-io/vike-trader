//! The window header (spec §4.1): the window's icon and title on the left; minimize, maximize or
//! restore, and close on the right; the density's header height. Its background is
//! `crate::header::paint_header_background` — the chart's gradient when `header_gradient` is on,
//! and nothing when it is off, so the window's own fill shows (spec §2: a setting, off by default).
//! It paints no fill of its own: a square one would cover the window's rounded top corners.

use egui::{Align2, Rect, Response, Sense, Ui};

use super::Tokens;
use super::button::IconButton;
use crate::appearance;
use crate::header::paint_header_background;
use crate::icons::{self, Icon};
use crate::type_scale::TextRole;

/// What the header's controls decided this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeaderActions {
    pub close: bool,
    pub minimize: bool,
    pub toggle_max: bool,
}

/// The header's actions, and the bar's own response — dragging it moves the window.
pub struct HeaderResponse {
    pub actions: HeaderActions,
    pub bar: Response,
}

/// A window header over the available width. The bar senses click-and-drag FIRST and the controls
/// are placed on top of it, so they win the click (egui resolves a hit to the last widget there).
/// A double click on the bar toggles maximize.
pub fn header(ui: &mut Ui, icon: Icon, title: &str, maximized: bool) -> HeaderResponse {
    let t = Tokens::of(ui.ctx());
    let h = t.metrics.header_h;
    let (bar, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click_and_drag());
    let p = ui.painter();
    paint_header_background(p, bar, &appearance::current(ui.ctx()));
    let icon_px = t.text.px(TextRole::Title);
    let x = bar.left() + t.metrics.pad;
    let icon_font = egui::FontId::new(icon_px, icons::family());
    icon.paint(p, egui::pos2(x, bar.center().y), Align2::LEFT_CENTER, icon_font, t.theme.text2);
    let title_at = egui::pos2(x + icon_px + t.metrics.gap, bar.center().y);
    p.text(title_at, Align2::LEFT_CENTER, title, t.font(TextRole::Strong), t.theme.text_ui);
    let mut actions =
        HeaderActions { toggle_max: resp.double_clicked(), ..HeaderActions::default() };
    let (max_icon, max_tip) =
        if maximized { (icons::RESTORE, "Restore") } else { (icons::MAXIMIZE, "Maximize") };
    let controls = [(icons::CLOSE, "Close"), (max_icon, max_tip), (icons::MINIMIZE, "Minimize")];
    for (i, (icon, tip)) in controls.into_iter().enumerate() {
        let at = egui::pos2(bar.right() - h * (i as f32 + 1.0), bar.top());
        if ui.put(Rect::from_min_size(at, egui::vec2(h, h)), IconButton::new(icon, tip)).clicked() {
            match i {
                0 => actions.close = true,
                1 => actions.toggle_max = true,
                _ => actions.minimize = true,
            }
        }
    }
    HeaderResponse { actions, bar: resp }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::testing::{ctx_with, harness, named, paint};
    use crate::metrics::Density;
    use egui::Shape;
    use egui::accesskit::Role;

    #[test]
    fn the_header_is_the_densitys_header_height() {
        for d in Density::ALL {
            let ctx = ctx_with(&Appearance { density: d, ..Appearance::default() });
            let mut h = 0.0;
            paint(&ctx, |ui| h = header(ui, icons::DATA, "Data", false).bar.rect.height());
            assert_eq!(h, d.metrics().header_h, "{d:?}");
        }
    }

    /// The header's background is step 4's: a gradient mesh exactly when the setting is on, and
    /// nothing at all when it is off, so the window's own fill shows (spec §2: a setting, off by
    /// default). The mesh's colours and rounded corners are `crate::header`'s own tests; this one
    /// proves the header delegates to it and paints no square fill over the window's corners.
    #[test]
    fn the_gradient_follows_the_setting() {
        for on in [false, true] {
            let ctx = ctx_with(&Appearance { header_gradient: on, ..Appearance::default() });
            let shapes = paint(&ctx, |ui| {
                header(ui, icons::DATA, "Data", false);
            });
            let meshes = shapes.iter().filter(|s| matches!(s, Shape::Mesh(_))).count();
            assert_eq!(meshes, usize::from(on), "{on}");
            let bg = Tokens::of(&ctx).theme.bg;
            let square = shapes.iter().any(|s| matches!(s, Shape::Rect(r) if r.fill == bg));
            assert!(!square, "{on}: the header painted a background fill of its own");
        }
    }

    #[test]
    fn the_controls_are_named_and_restore_replaces_maximize() {
        let h = harness(Appearance::default(), |ui| {
            header(ui, icons::DATA, "Data", true);
        });
        let b = named(&h, Role::Button);
        for name in ["Close", "Restore", "Minimize"] {
            assert!(b.contains(&name.to_string()), "{name}: {b:?}");
        }
        assert!(!b.contains(&"Maximize".to_string()), "{b:?}");
    }
}

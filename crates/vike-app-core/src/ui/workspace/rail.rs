//! The left rail: a narrow strip of **vertical tabs** (rotated text), one per
//! minimized window — matching vike's `MinimizedRail`. Clicking restores.

use super::state::WinState;
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::workspace;

/// Draw the rail. Shows ONLY minimized (and open) windows, like vike's
/// `MinimizedRail`; clicking a tab un-minimizes its window.
pub fn left_rail(ui: &mut egui::Ui, wins: &mut [WinState]) {
    ui.add_space(space::MD);
    ui.vertical_centered(|ui| {
        for w in wins.iter_mut() {
            if !(w.minimized && w.open) {
                continue; // vike's MinimizedRail shows ONLY minimized windows
            }
            if vertical_tab(ui, &w.title).clicked() {
                w.minimized = false;
            }
            ui.add_space(space::SM);
        }
    });
}

/// One vertical tab: a clickable rounded rect with text rotated 90° (reads
/// bottom-to-top). `active` = the window is currently visible.
fn vertical_tab(ui: &mut egui::Ui, text: &str) -> egui::Response {
    // The installed appearance: the theme's secondary grey, the Strong role (the chrome's text is
    // the menu's role — owner decision 1 of the design system's step-7 plan), its hover colour.
    let look = vike_ui_theme::appearance::current(ui.ctx());
    let t = vike_ui_theme::theme::Theme::of(look.theme);
    let col = t.text2;
    let font =
        egui::FontId::proportional(look.text_size.px(vike_ui_theme::type_scale::TextRole::Strong));
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, col);
    let sz = galley.size();
    // Size each tab to its text (rotated, so the label length is the tab HEIGHT) — like vike,
    // where "BTCUSDT · 30m" gets a taller tab than "Options" instead of a fixed box.
    let tab_w = ui.available_width().clamp(workspace::RAIL_TAB_MIN_W, workspace::RAIL_TAB_MAX_W);
    let tab_h = sz.x + workspace::RAIL_TAB_PAD;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(tab_w, tab_h), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(vike_ui_theme::metrics::RADIUS),
            t.hover,
        );
    }
    let pos = rect.center() + egui::vec2(-sz.y / 2.0, sz.x / 2.0);
    ui.painter().add(
        egui::epaint::TextShape::new(pos, galley, col).with_angle(-std::f32::consts::FRAC_PI_2),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::workspace::state::WinKind;
    use egui::{Rect, Shape, pos2, vec2};
    use vike_ui_theme::appearance::{Appearance, install};
    use vike_ui_theme::theme::{Theme, ThemeId};
    use vike_ui_theme::type_scale::{TextRole, TextSize};

    fn run(ctx: &egui::Context, input: egui::RawInput, wins: &mut [WinState]) -> Vec<Shape> {
        let mut out = ctx.run_ui(input, |ui| left_rail(ui, wins));
        let shapes = std::mem::take(&mut out.shapes).into_iter().map(|c| c.shape).collect();
        out.drop_without_applying_deltas();
        shapes
    }

    /// A rail tab's name is the theme's secondary grey at the Strong role, and a hovered tab takes
    /// the theme's hover fill — on every theme, at both text sizes.
    #[test]
    fn a_rail_tab_follows_the_theme_and_the_strong_role() {
        for theme in ThemeId::ALL {
            for size in TextSize::ALL {
                let ctx = egui::Context::default();
                install(&ctx, &Appearance { theme, text_size: size, ..Appearance::default() });
                let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0));
                let mut w = WinState::new("rail", "BTCUSDT", "1m", WinKind::Chart, rect);
                w.minimized = true;
                let mut wins = vec![w];
                let t = Theme::of(theme);
                let shapes = run(&ctx, egui::RawInput::default(), &mut wins);
                let label = shapes
                    .iter()
                    .find_map(|s| match s {
                        Shape::Text(x) => Some(x.clone()),
                        _ => None,
                    })
                    .expect("the tab's name");
                let f = &label.galley.job.sections[0].format;
                assert_eq!(f.color, t.text2, "{theme:?}");
                assert_eq!(f.font_id.size, size.px(TextRole::Strong), "{theme:?} {size:?}");
                let at = label.visual_bounding_rect().center();
                let hover = egui::RawInput {
                    events: vec![egui::Event::PointerMoved(at)],
                    ..Default::default()
                };
                let shapes = run(&ctx, hover, &mut wins);
                let filled =
                    shapes.iter().any(|s| matches!(s, Shape::Rect(r) if r.fill == t.hover));
                assert!(filled, "{theme:?}: the hovered tab");
            }
        }
    }
}

//! The left rail: a narrow strip of **vertical tabs** (rotated text), one per
//! minimized window — matching vike's `MinimizedRail`. Clicking restores.

use super::state::WinState;
use vike_ui_theme::palette as pal;

/// Draw the rail. Shows ONLY minimized (and open) windows, like vike's
/// `MinimizedRail`; clicking a tab un-minimizes its window.
pub fn left_rail(ui: &mut egui::Ui, wins: &mut [WinState]) {
    ui.add_space(6.0);
    ui.vertical_centered(|ui| {
        for w in wins.iter_mut() {
            if !(w.minimized && w.open) {
                continue; // vike's MinimizedRail shows ONLY minimized windows
            }
            if vertical_tab(ui, &w.title).clicked() {
                w.minimized = false;
            }
            ui.add_space(4.0);
        }
    });
}

/// One vertical tab: a clickable rounded rect with text rotated 90° (reads
/// bottom-to-top). `active` = the window is currently visible.
fn vertical_tab(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let col = pal::TEXT2;
    let galley =
        ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(13.0), col);
    let sz = galley.size();
    // Size each tab to its text (rotated, so the label length is the tab HEIGHT) — like vike,
    // where "BTCUSDT · 30m" gets a taller tab than "Options" instead of a fixed box.
    let tab_w = ui.available_width().clamp(20.0, 28.0);
    let tab_h = sz.x + 18.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(tab_w, tab_h), egui::Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, pal::HOVER);
    }
    let pos = rect.center() + egui::vec2(-sz.y / 2.0, sz.x / 2.0);
    ui.painter().add(
        egui::epaint::TextShape::new(pos, galley, col).with_angle(-std::f32::consts::FRAC_PI_2),
    );
    resp
}

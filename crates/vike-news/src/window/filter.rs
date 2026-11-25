//! The News toolbar's multi-select filter pill (Market, Category, Provider).

use vike_ui_theme::components::{ThemeTokens, role_px};
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::news;

/// A multi-select filter pill (News toolbar): button labelled `Name (n) ▾` opening a checklist
/// popover that stays open while toggling (CloseOnClickOutside). Mutates the selected set.
pub(super) fn news_filter_pill(
    ui: &mut egui::Ui,
    name: &str,
    options: &[String],
    selection: &mut std::collections::HashSet<String>,
) {
    use egui::RichText;
    let t = ThemeTokens::of(ui.ctx());
    let n = selection.len();
    let label = if n > 0 { format!("{name} ({n})") } else { name.to_string() };
    let resp = ui.add(
        egui::Button::new(
            RichText::new(label)
                .color(t.theme.text_secondary)
                .size(role_px(ui.ctx(), ThemeTextRole::Title)),
        )
        .fill(t.theme.surface)
        .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
        .min_size(egui::vec2(0.0, news::TOOLBAR_BUTTON_HEIGHT)),
    );
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(
        |ui| {
            ui.set_min_width(news::FILTER_MENU_MIN_WIDTH);
            for o in options {
                let mut on = selection.contains(o);
                if ui.checkbox(&mut on, o).changed() {
                    if on {
                        selection.insert(o.clone());
                    } else {
                        selection.remove(o);
                    }
                }
            }
        },
    );
}

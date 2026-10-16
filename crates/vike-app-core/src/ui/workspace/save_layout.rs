//! File → Save layout as…: the dialog that names a layout (design system step 7 moved it down out
//! of `vike-desktop`'s `draw_chrome` onto the component kit). The shell keeps the file write.
//!
//! Save is the view's one action, so it is the kit's PRIMARY button (spec §4.3). While the name
//! sanitizes to nothing, Save is disabled and says why (§4.2), and the reason is the field's error
//! line in the status red. The old dialog disabled it silently, under a fixed orange line.

use vike_ui_theme::components::Tokens;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::input::{self, Field};
use vike_ui_theme::value::workspace;

use super::persist::sanitize_layout_name;

/// Why Save is disabled: `sanitize_layout_name` keeps letters, digits, `-`, `_` and spaces.
pub const UNUSABLE_NAME: &str =
    "Use at least one letter or digit: every other character becomes _ in the file name";

/// What the operator decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutNameChoice {
    /// Save under this name, as typed; the shell sanitizes it again on write.
    Save(String),
    Cancel,
}

/// The dialog, centred. `focus` asks for the field's focus once, and the shell sets it when the
/// menu opens the dialog. `Some` once the operator has decided.
pub fn save_layout_dialog(
    ctx: &egui::Context,
    name: &mut String,
    focus: &mut bool,
) -> Option<LayoutNameChoice> {
    let mut choice = None;
    egui::Window::new("Save layout as")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.set_width(workspace::SAVE_LAYOUT_W);
            ui.label("Layout name:");
            let usable = !sanitize_layout_name(name).is_empty();
            let error = (!usable && !name.trim().is_empty()).then_some(UNUSABLE_NAME);
            ui.spacing_mut().text_edit_width = ui.available_width();
            let resp = input::text(
                ui,
                name,
                Field { hint: "e.g. Scalping", unit: None, error, ..Default::default() },
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if std::mem::take(focus) {
                resp.request_focus();
            }
            ui.add_space(Tokens::of(ui.ctx()).metrics.gap);
            ui.horizontal(|ui| {
                let save = if usable {
                    ActionButton::primary("Save")
                } else {
                    ActionButton::primary("Save").disabled_because(UNUSABLE_NAME)
                };
                if ui.add(save).clicked() || (enter && usable) {
                    choice = Some(LayoutNameChoice::Save(name.clone()));
                }
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if ui.add(ActionButton::secondary("Cancel")).clicked() || escape {
                    choice = Some(LayoutNameChoice::Cancel);
                }
            });
        });
    choice
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use std::sync::{Arc, Mutex};
    use vike_ui_theme::components::Status;

    fn harness(name: &str, sink: Arc<Mutex<Option<LayoutNameChoice>>>) -> Harness<'static> {
        let mut name = name.to_string();
        let mut focus = false;
        Harness::builder().with_size(egui::vec2(600.0, 400.0)).build_ui(move |ui| {
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            if let Some(c) = save_layout_dialog(ui.ctx(), &mut name, &mut focus) {
                *sink.lock().unwrap() = Some(c);
            }
        })
    }

    /// While the name is unusable, Save is disabled and SAYS WHY: on hover, through the kit's
    /// `disabled_because`, and under the field in the status red. The old dialog disabled it
    /// silently, under a fixed orange line.
    #[test]
    fn an_unusable_name_disables_save_and_says_why() {
        let mut h = harness("!!!", Arc::new(Mutex::new(None)));
        h.run();
        assert!(h.get_by_label("Save").accesskit_node().is_disabled());
        let red = h.output().shapes.iter().any(|c| match &c.shape {
            egui::Shape::Text(t) => {
                t.galley.text() == UNUSABLE_NAME
                    && t.galley.job.sections[0].format.color == Status::Error.color()
            }
            _ => false,
        });
        assert!(red, "the reason under the field, in the status red");
    }

    #[test]
    fn a_usable_name_is_saved() {
        let got = Arc::new(Mutex::new(None));
        let mut h = harness("Scalping", Arc::clone(&got));
        h.run();
        h.get_by_label("Save").click();
        h.run();
        assert_eq!(*got.lock().unwrap(), Some(LayoutNameChoice::Save("Scalping".to_string())));
    }
}

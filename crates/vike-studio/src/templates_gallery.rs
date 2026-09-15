//! Templates gallery: a browsable view of the starter Rhai strategies in
//! `vike_studio_core::templates::templates()`, each with a code preview + a "Load" button that
//! drops it into the editor. Richer than the one-line dropdown in the Sweep panel (which loads
//! blind) — you see the script before loading it. `template_preview` is pure/unit-tested; the egui
//! render (`gallery_ui`) follows SP2 headless discipline (not unit-tested).

use vike_studio_core::templates::templates;

/// First `max_lines` lines of `code`, with a trailing `…` marker when the script is longer — the
/// card preview shown per template in the gallery. Pure.
pub fn template_preview(code: &str, max_lines: usize) -> String {
    let trimmed = code.trim();
    let mut out: String = trimmed.lines().take(max_lines).collect::<Vec<_>>().join("\n");
    if trimmed.lines().count() > max_lines {
        out.push_str("\n…");
    }
    out
}

/// Render the gallery: one card per template (name + code preview + Load). Load overwrites
/// `editor_source` with the full template code. Thin over `templates()` + `template_preview`.
/// Returns `true` the frame a template's Load button is clicked, so the caller can re-baseline
/// its "unsaved changes" tracking (mirrors `IndicatorsPane::ui`'s bool-return convention).
pub fn gallery_ui(ui: &mut egui::Ui, editor_source: &mut String) -> bool {
    let mut loaded = false;
    egui::ScrollArea::vertical().id_salt("template-gallery").max_height(260.0).show(ui, |ui| {
        for (name, code) in templates() {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.strong(*name);
                    // Load hugs the right edge so every card's action lines up.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Load").clicked() {
                            *editor_source = code.to_string();
                            loaded = true;
                        }
                    });
                });
                ui.label(
                    egui::RichText::new(template_preview(code, 8)).monospace().weak().size(11.0),
                );
            });
            ui.add_space(2.0);
        }
    });
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_truncates_long_scripts_with_a_marker() {
        let code = "l1\nl2\nl3\nl4\nl5";
        let p = template_preview(code, 3);
        assert_eq!(p, "l1\nl2\nl3\n…");
    }

    #[test]
    fn preview_keeps_short_scripts_verbatim_without_a_marker() {
        let code = "  fn on_bar() { buy(1.0); }  ";
        let p = template_preview(code, 8);
        assert_eq!(p, "fn on_bar() { buy(1.0); }");
        assert!(!p.contains('…'));
    }

    #[test]
    fn every_registered_template_previews_non_empty() {
        // Guards against an empty/whitespace template slipping into the gallery.
        for (name, code) in templates() {
            assert!(!template_preview(code, 8).is_empty(), "template {name} previewed empty");
        }
    }
}

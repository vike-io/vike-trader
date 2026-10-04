//! The top File / View / Window / Help menu bar. Pure command emission: drawing
//! returns a `MenuResult` of the actions chosen this frame and the App applies
//! them — the menu never mutates window state itself.

use super::state::WinState;
use vike_ui_theme::icons;

#[derive(Default)]
pub struct MenuResult {
    pub arrange: Option<super::arrange::Arrange>,
    pub new_window: bool,
    pub quit: bool,
    pub toggle_rail: bool,
    pub save_workspace: bool,
    pub open_workspace: bool,
    /// File -> Export chart image…: capture the app framebuffer to a PNG this frame.
    /// The App drives the actual egui screenshot readback (see main.rs) — the menu
    /// only emits the intent, matching the command-emission contract above.
    pub export_chart: bool,
    /// File -> Save layout as…: open the layout-name entry dialog. The App owns the
    /// dialog + the actual `persist::save_layout` write (command-emission only here).
    pub save_layout_as: bool,
    /// File -> Load layout ▸ <name>: the named layout to load this frame (`None` = none
    /// picked). The App performs `persist::load_layout` + the full window rebuild.
    pub load_layout: Option<String>,
    /// File -> Delete layout ▸ <name>: the named layout to delete this frame. The App
    /// performs the `persist::delete_layout` file removal.
    pub delete_layout: Option<String>,
    /// Zone picked this frame from View -> Timezone (task A6); `None` = no change this frame.
    pub new_tz: Option<vike_chart::DisplayTz>,
    /// File -> Settings…: open the Settings window (the App spawns it through the launcher path).
    pub open_settings: bool,
}

/// Draw the File / View / Window / Help menu bar. Returns the actions chosen this frame.
/// `display_tz` is the app's CURRENT display timezone -- read-only here, just to mark the
/// active View -> Timezone radio; the menu never mutates App state itself (see module docs).
/// `layouts` is the App's current list of saved named-layout names (from
/// `persist::list_layouts`), read-only here to populate the Load/Delete submenus.
pub fn menu_bar(
    ui: &mut egui::Ui,
    _wins: &[WinState],
    display_tz: vike_chart::DisplayTz,
    layouts: &[String],
) -> MenuResult {
    use super::arrange::Arrange;
    let mut r = MenuResult::default();
    // The whole menu — the bar's words and every dropdown row — is ONE size, the Strong role (owner
    // decision 1 of the design system's step-7 plan). egui 0.36 draws a button's, a submenu
    // button's and a radio button's words in the BODY style: its widget style falls back to
    // `TextStyle::Body` whatever `TextStyle::Button` holds. So the bar and its dropdowns name the
    // Button style — the Strong role in `vike_ui_theme::appearance`'s type table — as the style
    // every word without one of its own takes; it follows a live text-size change like any role.
    let strong = |s: &mut egui::Style| {
        egui::containers::menu::menu_style(s);
        s.override_text_style = Some(egui::TextStyle::Button);
    };
    egui::MenuBar::new()
        .style(strong)
        .config(egui::containers::menu::MenuConfig::new().style(strong))
        .ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New chart window").clicked() {
                    r.new_window = true;
                }
                ui.separator();
                if ui.button("Open Workspace").clicked() {
                    r.open_workspace = true;
                }
                if ui.button("Save Workspace").clicked() {
                    r.save_workspace = true;
                }
                ui.separator();
                // Named layouts (TradingView-style): save-as / load / delete. Pure command
                // emission — the App owns the name-entry dialog and all file IO (see main.rs).
                if ui.button("Save layout as…").clicked() {
                    r.save_layout_as = true;
                    ui.close();
                }
                ui.add_enabled_ui(!layouts.is_empty(), |ui| {
                    submenu("Load layout").ui(ui, |ui| {
                        for name in layouts {
                            if ui.button(name).clicked() {
                                r.load_layout = Some(name.clone());
                                ui.close();
                            }
                        }
                    });
                    submenu("Delete layout").ui(ui, |ui| {
                        for name in layouts {
                            if ui.button(name).clicked() {
                                r.delete_layout = Some(name.clone());
                                ui.close();
                            }
                        }
                    });
                });
                ui.separator();
                let _ = ui.button("AI: generate a layout…");
                if ui.button("Export chart image…").clicked() {
                    r.export_chart = true;
                }
                ui.separator();
                if ui.button("Settings…").clicked() {
                    r.open_settings = true;
                    ui.close();
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    r.quit = true;
                }
            });
            ui.menu_button("View", |ui| {
                submenu("Timezone").ui(ui, |ui| {
                    for tz in vike_chart::DisplayTz::CURATED {
                        if ui.radio(display_tz == tz, tz.label()).clicked() {
                            r.new_tz = Some(tz);
                            ui.close();
                        }
                    }
                });
            });
            ui.menu_button("Window", |ui| {
                if ui.button("Cascade").clicked() {
                    r.arrange = Some(Arrange::Cascade);
                }
                if ui.button("Tile Horizontally").clicked() {
                    r.arrange = Some(Arrange::TileH);
                }
                if ui.button("Tile Vertically").clicked() {
                    r.arrange = Some(Arrange::TileV);
                }
                if ui.button("Grid").clicked() {
                    r.arrange = Some(Arrange::Grid);
                }
                ui.separator();
                if ui.button("Minimize All").clicked() {
                    r.arrange = Some(Arrange::MinimizeAll);
                }
                if ui.button("Restore All").clicked() {
                    r.arrange = Some(Arrange::RestoreAll);
                }
                ui.separator();
                if ui.button("Toggle left panel").clicked() {
                    r.toggle_rail = true;
                }
            });
            ui.menu_button("Help", |ui| {
                let _ = ui.button(format!("About {}", vike_ui_theme::brand::APP_NAME));
                let _ = ui.button("Keyboard shortcuts");
            });
        });
    r
}

/// A submenu's button, its arrow the registry's `icons::DISCLOSE_CLOSED`: every menu that opens a
/// submenu draws it with this. `ui.menu_button` inside a menu draws egui's own arrow instead — a
/// second glyph for the same meaning — and the layout entries once spelled an arrow into their
/// labels on top of it, which showed two.
pub fn submenu<'a>(atoms: impl egui::IntoAtoms<'a>) -> egui::containers::menu::SubMenuButton<'a> {
    egui::containers::menu::SubMenuButton::from_button(
        egui::Button::new(atoms).right_text(icons::DISCLOSE_CLOSED),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use std::sync::{Arc, Mutex};

    /// File → Settings… asks for the Settings window.
    #[test]
    fn file_settings_asks_for_the_settings_window() {
        let asked = Arc::new(Mutex::new(false));
        let sink = Arc::clone(&asked);
        let mut h = Harness::builder().with_size(egui::vec2(480.0, 360.0)).build_ui(move |ui| {
            // The File menu draws submenu arrows, and their family is bound only by the app's type.
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            if menu_bar(ui, &[], vike_chart::DisplayTz::Local, &[]).open_settings {
                *sink.lock().unwrap() = true;
            }
        });
        h.run();
        h.get_by_label("File").click();
        h.run();
        h.get_by_label("Settings…").click();
        h.run();
        assert!(*asked.lock().unwrap());
    }

    /// View → Timezone's rows are the menu's size too, the Strong role (owner decision 1 of the
    /// step-7 plan), at both text sizes: they are radio buttons in a NESTED submenu, so this is the
    /// check that the menu's style reaches a submenu's popup and not only the bar and its first
    /// dropdowns.
    #[test]
    fn the_timezone_rows_are_the_menus_size() {
        use vike_ui_theme::appearance::{Appearance, install};
        use vike_ui_theme::type_scale::{TextRole, TextSize};
        for size in TextSize::ALL {
            let mut h =
                Harness::builder().with_size(egui::vec2(480.0, 720.0)).build_ui(move |ui| {
                    // The first frame installs the appearance at this size and draws nothing: the
                    // bundled faces land on the next frame (`vike_ui_theme::harness`'s module doc).
                    let id = egui::Id::new("menu-test-installed");
                    if !ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
                        install(ui.ctx(), &Appearance { text_size: size, ..Appearance::default() });
                        ui.ctx().data_mut(|d| d.insert_temp(id, true));
                        ui.ctx().request_repaint();
                        return;
                    }
                    menu_bar(ui, &[], vike_chart::DisplayTz::Local, &[]);
                });
            h.run();
            h.get_by_label("View").click();
            h.run();
            h.get_by_label_contains("Timezone").click();
            h.run();
            let labels: Vec<&str> =
                vike_chart::DisplayTz::CURATED.iter().map(|t| t.label()).collect();
            let rows: Vec<f32> = h
                .output()
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) if labels.contains(&t.galley.text()) => {
                        Some(t.galley.job.sections[0].format.font_id.size)
                    }
                    _ => None,
                })
                .collect();
            let want = size.px(TextRole::Strong);
            assert_eq!(rows.len(), labels.len(), "{size:?}: every timezone row is drawn");
            assert!(rows.iter().all(|px| *px == want), "{size:?}: {rows:?}, want {want}");
        }
    }

    /// The WHOLE main menu is one size, the Strong role (owner decision 1 of the step-7 plan): the
    /// bar's own words and every row of a dropdown, at both text sizes. egui 0.36 draws a button's
    /// words in the BODY style — its widget style falls back to `TextStyle::Body` whatever
    /// `TextStyle::Button` holds — so a menu left to egui's defaults is the Body role throughout.
    #[test]
    fn the_whole_main_menu_is_the_strong_role() {
        use vike_ui_theme::appearance::{Appearance, install};
        use vike_ui_theme::type_scale::{TextRole, TextSize};
        for size in TextSize::ALL {
            let mut h =
                Harness::builder().with_size(egui::vec2(480.0, 720.0)).build_ui(move |ui| {
                    // The first frame installs the appearance at this size and draws nothing: the
                    // bundled faces land on the next frame (`vike_ui_theme::harness`'s module doc).
                    let id = egui::Id::new("menu-test-installed");
                    if !ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
                        install(ui.ctx(), &Appearance { text_size: size, ..Appearance::default() });
                        ui.ctx().data_mut(|d| d.insert_temp(id, true));
                        ui.ctx().request_repaint();
                        return;
                    }
                    menu_bar(ui, &[], vike_chart::DisplayTz::Local, &[]);
                });
            h.run();
            h.get_by_label("File").click();
            h.run();
            let texts: Vec<(String, f32)> = h
                .output()
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) if !t.galley.text().is_empty() => Some((
                        t.galley.text().to_string(),
                        t.galley.job.sections[0].format.font_id.size,
                    )),
                    _ => None,
                })
                .collect();
            for word in ["File", "View", "Window", "Help", "New chart window", "Settings…", "Exit"]
            {
                assert!(texts.iter().any(|(t, _)| t == word), "{size:?}: {word:?} is drawn");
            }
            let want = size.px(TextRole::Strong);
            let off: Vec<&(String, f32)> = texts.iter().filter(|(_, px)| *px != want).collect();
            assert!(off.is_empty(), "{size:?}: every menu text is {want}; these are not: {off:?}");
        }
    }
}

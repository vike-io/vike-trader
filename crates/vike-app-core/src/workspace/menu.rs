//! The top File / View / Window / Help menu bar. Pure command emission: drawing
//! returns a `MenuResult` of the actions chosen this frame and the App applies
//! them — the menu never mutates window state itself.

use super::state::WinState;

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
    egui::MenuBar::new().ui(ui, |ui| {
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
                ui.menu_button("Load layout ▸", |ui| {
                    for name in layouts {
                        if ui.button(name).clicked() {
                            r.load_layout = Some(name.clone());
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Delete layout ▸", |ui| {
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
            if ui.button("Exit").clicked() {
                r.quit = true;
            }
        });
        ui.menu_button("View", |ui| {
            ui.menu_button("Timezone", |ui| {
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
            let _ = ui.button("About Vike Trader");
            let _ = ui.button("Keyboard shortcuts");
        });
    });
    r
}

//! Chrome phases after the caption: menu actions, chart export, layout dialog, rail, status bar.

use super::*;

/// Chrome phase 2 — every action the menu bar emitted this frame: arrange, rail toggle,
/// new window, workspace save/load, named-layout save-as / load / delete, quit, and the chart
/// export (which [`drive_chart_export`] carries across frames).
pub(super) fn apply_menu_actions(app: &mut App, ctx: &egui::Context, menu: MenuBarActions) {
    if let Some(a) = menu.arrange {
        window::apply_arrange(&mut app.windows, app.desktop, a);
        // remember tiling modes so an app-maximize can re-fill (cascade opts out) — the rule
        // is the shared `initial_arrange::tiling_memory` now: this arm and the first-frame
        // planner carried two hand copies of the same `matches!`, one drift away from
        // disagreeing about which modes re-fill.
        if let Some(mode) = initial_arrange::tiling_memory(a) {
            app.last_arrange = Some(mode);
        }
    }
    if menu.toggle_rail {
        app.show_rail = !app.show_rail;
    }
    if menu.new_window {
        // On the user's default series, or empty: `spawn::default_series`, applied by
        // `apply_spawn` (no compiled symbol list cycles here any more).
        app.apply_spawn(ctx, spawn::WindowSpawnRequest::NewWindowChart);
    }
    if menu.save_workspace {
        match window::workspace::save(&app.windows, &app.indicator_favourites) {
            Ok(p) => tracing::info!("workspace saved → {}", p.display()),
            Err(e) => tracing::warn!("workspace save failed: {e}"),
        }
    }
    if menu.open_workspace {
        match window::workspace::load() {
            Ok(Some(ws)) => {
                app.restore_workspace(ctx, &ws);
                tracing::info!("workspace loaded ({} windows)", app.windows.len());
            }
            Ok(None) => tracing::warn!("{}", window::workspace::no_workspace_file()),
            // A refused (older build's) file: its one-line hint, already logged by the load.
            Err(why) => app.status.text = why,
        }
    }
    // Named layouts (TradingView-style). Save-as opens a modal name field (rendered below);
    // Load/Delete act directly on the name picked from the menu submenu this frame.
    if menu.save_layout_as {
        app.layout_name_input.clear();
        app.layout_name_prompt = true;
        app.layout_prompt_focus = true;
    }
    if let Some(name) = menu.load_layout {
        match window::workspace::load_layout(&name) {
            Ok(ws) => {
                app.restore_workspace(ctx, &ws);
                app.status.text = format!("layout '{name}' loaded");
                tracing::info!("layout '{}' loaded ({} windows)", name, app.windows.len());
            }
            Err(why) => app.status.text = why,
        }
    }
    if let Some(name) = menu.delete_layout {
        match window::workspace::delete_layout(&name) {
            Ok(()) => {
                app.status.text = format!("layout '{name}' deleted");
                tracing::info!("layout '{name}' deleted");
            }
            Err(e) => {
                app.status.text = format!("layout '{name}' delete failed: {e}");
                tracing::warn!("layout '{name}' delete failed: {e}");
            }
        }
    }
    if menu.quit {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    drive_chart_export(app, ctx, menu.export_chart);
}

/// Chrome phase 2b — File → Export chart image…: request the framebuffer screenshot when the menu
/// asked for one, and PNG-encode it on the frame the readback arrives. Called every frame,
/// because the readback lands a frame or two after the request.
fn drive_chart_export(app: &mut App, ctx: &egui::Context, export_chart: bool) {
    // File -> Export chart image…: reuse the VIKE_SHOT framebuffer-readback path — grab
    // egui's OWN wgpu framebuffer (the only capture that survives the flip-model swapchain;
    // an OS/GDI grab returns white). The menu action requests a screenshot this frame; the
    // readback arrives a frame or two later (below), where we PNG-encode it. Guarded so
    // the interactive export never fights the VIKE_SHOT self-shot (that block owns the readback
    // and exits under its own env var, so the two are mutually exclusive in practice anyway).
    if export_chart {
        app.export_pending = true;
        // A readback an EARLIER request parked late is not this export's picture.
        drop(app.take_shot());
        app.request_shot(ctx);
    }
    if app.export_pending
        && let Some(image) = app.take_shot()
    {
        app.export_pending = false;
        app.export_sequence += 1;
        match export_chart_png(&image, app.export_sequence) {
            Ok(p) => {
                app.status.text = format!("chart exported → {}", p.display());
                tracing::info!("chart image exported → {}", p.display());
            }
            Err(e) => {
                app.status.text = format!("chart export failed: {e}");
                tracing::warn!("chart image export failed: {e}");
            }
        }
    }
}

/// Chrome phase 3 — the "Save layout as…" naming modal, while `app.layout_name_prompt` is up.
pub(super) fn draw_save_layout_dialog(app: &mut App, ctx: &egui::Context) {
    // File -> Save layout as…: small centered modal to name the layout. Confirm (Save button
    // or Enter) sanitizes the name → `layouts/<name>.json`; Save is disabled while the name
    // sanitizes to nothing (empty / all-punctuation). The menu emitted the intent; the file
    // IO lives here (command-emission contract). Overwriting an existing name is allowed
    // (same as re-saving the single workspace) — the submenu shows what already exists.
    if app.layout_name_prompt {
        use window::workspace::save_layout::{WindowLayoutNameChoice, save_layout_dialog};
        let choice =
            save_layout_dialog(ctx, &mut app.layout_name_input, &mut app.layout_prompt_focus);
        if let Some(WindowLayoutNameChoice::Save(name)) = &choice {
            match window::workspace::save_layout(name, &app.windows, &app.indicator_favourites) {
                Ok(p) => {
                    app.status.text = format!("layout saved → {}", p.display());
                    tracing::info!("layout saved → {}", p.display());
                }
                Err(e) => {
                    app.status.text = format!("layout save failed: {e}");
                    tracing::warn!("layout save failed: {e}");
                }
            }
        }
        if choice.is_some() {
            app.layout_name_prompt = false;
            app.layout_name_input.clear();
        }
    }
}

/// Chrome phase 4 — the left rail of minimized-window tabs, shown only while one is minimized.
pub(super) fn draw_min_rail(app: &mut App, ui: &mut egui::Ui) {
    // --- left rail of vertical tabs ---
    let any_min = app.windows.iter().any(|w| w.minimized && w.open);
    if app.show_rail && any_min {
        egui::Panel::left("min_rail")
            .resizable(false)
            .default_size(vike_ui_theme::value::desktop::MIN_RAIL_WIDTH)
            .min_size(vike_ui_theme::value::desktop::MIN_RAIL_MIN_WIDTH)
            .max_size(vike_ui_theme::value::desktop::MIN_RAIL_MAX_WIDTH)
            .show(ui, |ui| {
                window::left_rail(ui, &mut app.windows);
            });
    }
}

/// Chrome phase 5 — the bottom status strip: this frame's reads of `App`, handed to
/// `vike_app_core::ui::status_bar`, plus the click that dismisses a latched control error. It must
/// run BEFORE the central panel so `app.desktop` excludes the strip.
pub(super) fn draw_status_bar(app: &mut App, ui: &mut egui::Ui) {
    // --- bottom status bar (vike-python-style: connection + workspace info) ---
    // A fixed-height strip pinned to the app's bottom edge, the egui analog of the PySide
    // status bar. It also fills what used to be dead space below the maximized chart window
    // (the window's own bottom frame inset never reached the desktop edge), so "empty space
    // on maximize" now reads as an intentional status strip. Added BEFORE the CentralPanel so
    // `desktop` (the window arena) excludes it.
    //
    // ⚠ That claim is TRUE and was VERIFIED, and it is still not the whole answer to "why is my
    // tool window under the status bar": `egui-0.37.0/src/containers/panel.rs`'s `show_inside_dyn`
    // sets `cursor.max[axis] = visible_outer_rect.min[axis]` on the parent for a
    // `PanelSide::Bottom`, so the `CentralPanel` below really does start above this strip — but
    // nothing about the ARENA stops a WINDOW from being laid out larger than it. The ceiling that
    // bounds it lives with the windows: `vike_app_core::ui::window`'s `show_window` pins every
    // window at a size capped at the arena.
    //
    // The strip itself is `vike_app_core::ui::status_bar` since design system step 7, where CI runs
    // its tests; what stays here is the frame's reads of `App` it is drawn from.
    let n_charts =
        app.windows.iter().filter(|w| w.open && w.kind == window::WindowKind::Chart).count();
    // How many venues the node runs and which accounts it lost — what the
    // ACTIVE backend's bridge last read off a frame (empty from a pre-B3 node, before the first
    // frame, or on every local path). Since B1 the bridge lives inside
    // `App::active_backend`'s `BackendConnection`, so the read goes through it — a switch
    // replaces the conn, so a stale daemon name cannot outlive its connection.
    let words = app.active_backend.as_ref().map(|b| b.bridge.node_words()).unwrap_or_default();
    // What this desktop may do there, in the role the node said, and the window title in the same
    // words: built in CI-covered vike-app-core (`status_bar::backend_access`).
    let access = vike_app_core::ui::status_bar::backend_access(
        ui.ctx(),
        app.active_backend.as_ref().map(|b| &b.record),
        app.tradehub_control(),
    );
    let dismissed = vike_app_core::ui::status_bar::status_bar(
        ui,
        &vike_app_core::ui::status_bar::StatusBar {
            status: &app.status,
            display_time_zone: app.appearance.display_time_zone(),
            now_ms: chrono::Utc::now().timestamp_millis(),
            n_charts,
            n_venues: words.venues,
            n_lost: words.lost.len(),
            access,
        },
    );
    if dismissed && let Some(ctrl) = app.tradehub_control() {
        ctrl.clear_last_error();
    }
}

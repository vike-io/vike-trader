//! Chrome phases after the caption: menu actions, chart export, layout dialog, rail, status bar.

use super::*;

/// Chrome phase 2 — every action the menu bar emitted this frame: arrange, rail toggle, timezone,
/// new window, workspace save/load, named-layout save-as / load / delete, quit, and the chart
/// export (which [`drive_chart_export`] carries across frames).
pub(super) fn apply_menu_actions(app: &mut App, ctx: &egui::Context, menu: workspace::MenuResult) {
    if let Some(a) = menu.arrange {
        workspace::apply_arrange(&mut app.wins, app.desktop, a);
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
    if let Some(tz) = menu.new_tz {
        app.display_tz = tz;
        // Immediate, same-frame effect: `sync_from_core`'s self-healing `set_tz` (see there)
        // only runs on the NEXT changed core snapshot, which could be a while for a quiet
        // symbol — loop every live chart right now so the axis/marks repaint this frame.
        for cs in app.charts.values_mut() {
            cs.set_tz(tz);
        }
    }
    if menu.new_window {
        // The `SYMS` round-robin stays in this binary on purpose: `SYMS` is ALSO
        // `crates/vike-desktop/src/chart_window.rs`'s symbol-picker quick-pick list, so it is
        // vike-desktop's constant, not the planner's. Only the cascade slot and the `WinState`
        // construction moved; the symbol arrives in the request. ⚠ this reads `next_win_n`
        // BEFORE `apply_spawn` advances it, which is the pre-increment value the inline
        // version indexed with — do not move the read below the call.
        let sym = SYMS[(app.next_win_n as usize) % SYMS.len()];
        let req = window_spawn::SpawnRequest::NewWindowChart { symbol: sym.to_string() };
        app.apply_spawn(ctx, req);
    }
    if menu.save_workspace {
        match workspace::persist::save(
            &app.wins,
            app.display_tz,
            app.gpu_render,
            &app.indicator_favs,
        ) {
            Ok(p) => tracing::info!("workspace saved → {}", p.display()),
            Err(e) => tracing::warn!("workspace save failed: {e}"),
        }
    }
    if menu.open_workspace {
        if let Some(ws) = workspace::persist::load() {
            app.restore_workspace(ctx, &ws);
            tracing::info!("workspace loaded ({} windows)", app.wins.len());
        } else {
            match workspace::persist::path() {
                Some(p) => tracing::warn!("no workspace file at {}", p.display()),
                None => tracing::warn!(
                    "no workspace file: neither $VIKE_WORKSPACE nor a project settings \
                     directory resolves from here"
                ),
            }
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
        if let Some(ws) = workspace::persist::load_layout(&name) {
            app.restore_workspace(ctx, &ws);
            app.status = format!("layout '{name}' loaded");
            tracing::info!("layout '{}' loaded ({} windows)", name, app.wins.len());
        } else {
            app.status = format!("layout '{name}' could not be loaded");
            tracing::warn!("layout '{name}' could not be loaded");
        }
    }
    if let Some(name) = menu.delete_layout {
        match workspace::persist::delete_layout(&name) {
            Ok(()) => {
                app.status = format!("layout '{name}' deleted");
                tracing::info!("layout '{name}' deleted");
            }
            Err(e) => {
                app.status = format!("layout '{name}' delete failed: {e}");
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
        && let Some(img) = app.take_shot()
    {
        app.export_pending = false;
        app.export_seq += 1;
        match export_chart_png(&img, app.export_seq) {
            Ok(p) => {
                app.status = format!("chart exported → {}", p.display());
                tracing::info!("chart image exported → {}", p.display());
            }
            Err(e) => {
                app.status = format!("chart export failed: {e}");
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
        use workspace::save_layout::{LayoutNameChoice, save_layout_dialog};
        let choice =
            save_layout_dialog(ctx, &mut app.layout_name_input, &mut app.layout_prompt_focus);
        if let Some(LayoutNameChoice::Save(name)) = &choice {
            match workspace::persist::save_layout(
                name,
                &app.wins,
                app.display_tz,
                app.gpu_render,
                &app.indicator_favs,
            ) {
                Ok(p) => {
                    app.status = format!("layout saved → {}", p.display());
                    tracing::info!("layout saved → {}", p.display());
                }
                Err(e) => {
                    app.status = format!("layout save failed: {e}");
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
    let any_min = app.wins.iter().any(|w| w.minimized && w.open);
    if app.show_rail && any_min {
        egui::Panel::left("min_rail")
            .resizable(false)
            .default_size(vike_ui_theme::value::desktop::MIN_RAIL_W)
            .min_size(vike_ui_theme::value::desktop::MIN_RAIL_MIN_W)
            .max_size(vike_ui_theme::value::desktop::MIN_RAIL_MAX_W)
            .show(ui, |ui| {
                workspace::left_rail(ui, &mut app.wins);
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
    // nothing about the ARENA stops a WINDOW from being laid out larger than it, and an unlatched
    // tool window is painted at its content whatever its declared size says. The ceiling that
    // bounds it lives with the windows, in `vike_app_core::ui::workspace`'s `WinState::arena_bounded`.
    //
    // The strip itself is `vike_app_core::ui::status_bar` since design system step 7, where CI runs
    // its tests; what stays here is the frame's reads of `App` it is drawn from.
    let n_charts =
        app.wins.iter().filter(|w| w.open && w.kind == workspace::WinKind::Chart).count();
    // Remote Scope::Write channel summary (observe mode only; `None` on every local
    // path so the strip renders byte-identically there). Reads the handle's two async
    // surfaces and lowers them to a string in CI-covered vike-app-core.
    //
    // `last_error` is deliberately the LATCHED view here — a status strip must not lose
    // a refusal between repaints, which is exactly why the per-command outcome moved to
    // `await_outcome`/`CommandTicket` instead of this accessor being made consuming.
    // The latch is cleared only when the operator clicks the segment to dismiss it.
    let ctrl_error = app.remote_ctrl().and_then(|c| c.last_error());
    // I3: WHICH daemon the armed channel points at — the identity the ACTIVE
    // backend's bridge last reported (`None` from a pre-B3 node, before the first
    // frame, or on every local path). Since B1 the bridge lives inside
    // `App::active_backend`'s `BackendConn`, so the read goes through it — a switch
    // replaces the conn, so a stale daemon name cannot outlive its connection.
    let daemon_identity = app.active_backend.as_ref().and_then(|b| b.bridge.identity());
    // ONE read of the handle's `is_connected`, used by BOTH the rendered line and the dot
    // beside it. The dot used to recover this bool by searching that line for
    // `"disconnected"` — a round trip through prose, out of a string the very next call
    // built FROM this bool, and one a latched `last_error` mentioning the word could
    // falsify. The two now cannot disagree because there is only one value.
    let control_connected = app.remote_ctrl().is_some_and(|c| c.is_connected());
    let control_line = vike_app_core::backend::tradehub_control::control_status_line(
        app.remote_ctrl().is_some(),
        control_connected,
        ctrl_error.as_deref(),
        daemon_identity.as_ref(),
    );
    // …and the three values become ONE `Option`: a segment that EXISTS carries its own
    // connectedness and its own latched-error flag, so "connected, with nothing to paint it
    // on" stops being representable. The bool is stored beside the line it was built from,
    // which is what keeps the two from ever being re-derived out of each other.
    let control =
        control_line.as_deref().map(|line| vike_app_core::ui::status_bar::ControlSegment {
            line,
            connected: control_connected,
            has_error: ctrl_error.is_some(),
        });
    let dismissed = vike_app_core::ui::status_bar::status_bar(
        ui,
        &vike_app_core::ui::status_bar::StatusBar {
            status: &app.status,
            display_tz: app.display_tz,
            now_ms: chrono::Utc::now().timestamp_millis(),
            n_charts,
            n_venues: app.feeds.len(),
            control,
        },
    );
    if dismissed && let Some(ctrl) = app.remote_ctrl() {
        ctrl.clear_last_error();
    }
}

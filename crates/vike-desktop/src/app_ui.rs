//! `App::ui`'s per-frame body — everything EXCEPT the two spots that must stay physically in
//! `main.rs`.
//!
//! ⚠ **Why this file is a split rather than a move.** `ui` is one ~2100-line frame body, and two
//! small blocks inside it read the process environment directly:
//!
//! 1. the headless self-screenshot capture (`VIKE_SHOT` / `VIKE_APPMAX` / `VIKE_SHOT_FRAME`), and
//! 2. the first-frame window arrangement's ten-knob `initial_arrange::ArrangeEnv` construction
//!    (`VIKE_ARRANGE` / `VIKE_TOOL` / `VIKE_TOOLS` / `VIKE_DOM_MODE` / `VIKE_DOM_GROUP` /
//!    `VIKE_DOM_VENUE` / `VIKE_CAL_PAGE` / `VIKE_MAX` / `VIKE_MIN` / `VIKE_POLY_COCKPIT_TOKEN`).
//!
//! `crates/vike-ops/tests/settings_registry.rs` classifies every `env::var` read by FILE PATH:
//! only `main.rs` / `src/bin/*` / `examples/*` earn `Layer::Binary`. A raw read moved into THIS
//! file would silently become a `Layer::Library` row, which the shrink-only `LIBRARY_PIN` ratchet
//! in that same gate refuses. So those two blocks stay inline in `main.rs`'s `ui` — the same rule
//! `App::new`'s `startup::StartupEnv` construction already follows — and everything between and
//! around them lives here, called from the thin `ui` that stays behind.
//!
//! The three functions are the three surviving pieces of the original body, in the original order:
//! [`frame_begin`] (channel drains + the frame's read-only snapshots), then island 1, then
//! [`draw_chrome`] (caption / menus / rail / status bar / desktop rect), then island 2, then
//! [`draw_windows`] (the window arena and every post-loop drain). They are free functions taking
//! `app: &mut App` rather than `App` methods so the split is visible at the call site, and the
//! values that used to be plain locals crossing the islands are threaded explicitly — see
//! [`FrameSnapshot`].

use super::*;

/// The frame-local values [`frame_begin`] computes and [`draw_windows`] consumes — the locals that
/// used to sit in one function body and now have to cross two island boundaries.
///
/// Each is a CHEAP snapshot of an `App` field taken while `app` is still fully borrowable, because
/// the window loop in [`draw_windows`] holds `app.wins` mutably for its whole duration and so
/// cannot reach back through `app` for these. That is exactly why they were snapshotted up front
/// in the first place; bundling them keeps the reason (and the snapshot POINT — before island 1,
/// unchanged) intact instead of re-reading the fields later, which would be a different program.
///
/// Bundled rather than passed as seven more parameters for the reason `StartupEnv`/`ArrangeEnv`
/// are bundled: a purpose-named struct beats a positional list nobody can read.
pub(crate) struct FrameSnapshot {
    /// Cross-venue instrument universe for every window title-bar's symbol picker.
    symbols_catalog: Arc<vike_catalog::Catalog>,
    /// Data Manager "Stored" view: the inventory tree, its gap map and its partial-day map.
    stored_tree: Arc<Vec<inventory::VenueNode>>,
    stored_partials: Arc<vike_data_manager::PartialDayMap>,
    stored_gaps: Arc<vike_data_manager::GapMap>,
    /// What the last background load learned about the remote store's coverage verb (spec §6-Q2).
    stored_coverage: vike_app_core::stored_mode::RemoteCoverage,
    /// `true` while a background `refresh_stored` load is in flight.
    stored_loading: bool,
    /// The last bulk-backfill run's human-readable status line.
    stored_backfill_status: String,
}

/// Segment 1 — everything before island 1 (the `VIKE_SHOT` capture block).
///
/// The per-frame INTAKE: the shutdown-signal latch, the chart-sync double-buffer swap, the core
/// snapshot fold, every background channel drain (flag/logo textures, the symbol catalog, the
/// Stored inventory load, the bulk-backfill status, the Gamma token resolutions), the read-only
/// snapshots the window loop will need ([`FrameSnapshot`]), and the Alt+L scale hotkey.
///
/// Returns the frame's `egui::Context` clone BY VALUE alongside the snapshot: the original body
/// held it as an owned local (`ui.ctx().clone()`) and passes `&ctx` to a dozen callees, so handing
/// it back owned keeps every one of those call sites spelled exactly as it was.
pub(crate) fn frame_begin(app: &mut App, ui: &mut egui::Ui) -> (egui::Context, FrameSnapshot) {
    // Unified shutdown signal (see `on_exit`): the instant the window-close is requested — one
    // or more frames before eframe calls `on_exit` — raise the flag so `ensure_feed_on` stops
    // opening NEW live feeds (blocking socket reads the bounded teardown would then wait on).
    // A single Relaxed load per GUI frame; nowhere near the vike-core hot fold. This app never
    // cancels a close, so `close_requested()` unambiguously means "closing".
    if !app.shutdown.load(std::sync::atomic::Ordering::Relaxed)
        && ui.ctx().input(|i| i.viewport().close_requested())
    {
        app.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // Chart sync groups (task B8): double-buffered per-frame swap, once per frame,
    // before any window is drawn — this frame's grouped windows read `sync_prev`
    // (what got harvested last frame) and write fresh entries into `sync_next` (see
    // `sync_feed`/`sync_harvest`, called from the window loop below).
    app.sync_prev = std::mem::take(&mut app.sync_next);
    app.sync_from_core();
    let ctx = ui.ctx().clone();
    // load any newly-fetched flag images as textures
    while let Ok((iso, ci)) = app.flag_rx.try_recv() {
        let tex = ctx.load_texture(format!("flag_{iso}"), ci, egui::TextureOptions::LINEAR);
        app.flags.insert(iso, tex);
    }
    while let Ok((src, ci)) = app.logo_rx.try_recv() {
        let tex = ctx.load_texture(format!("logo_{src}"), ci, egui::TextureOptions::LINEAR);
        app.news_logos.insert(src, tex);
    }
    // adopt the background-fetched symbol catalog once it lands (single send)
    if let Ok(list) = app.catalog_rx.try_recv() {
        app.symbols_catalog = Arc::new(list);
    }
    // Data Manager "Stored" view: adopt the latest background inventory load (tree + gap
    // map), if one landed this frame (see `refresh_stored`). `try_recv` drains to the newest
    // send — irrelevant here since only one load is ever in flight at a time.
    if let Ok((tree, gaps, partials, coverage)) = app.stored_rx.try_recv() {
        app.stored_tree = Arc::new(tree);
        app.stored_gaps = Arc::new(gaps);
        app.stored_partials = Arc::new(partials);
        // What this load negotiated about the coverage verb — the render-time input to the
        // Partial column's three states (spec §6-Q2).
        app.stored_coverage = coverage;
        app.stored_loading = false;
    }
    // dm-bulk-backfill: adopt the latest bulk-Backfill completion status, if one landed this
    // frame (see `App::maybe_spawn_stored_backfill`), and immediately kick off a reload so the
    // grid's coverage bars/gap cut-outs reflect the newly-ingested data — same
    // reload-on-completion shape as the per-row delete path (`stored_refresh_requested` below).
    if let Ok(msg) = app.stored_backfill_rx.try_recv() {
        app.stored_backfill_running = false;
        app.stored_backfill_status = msg;
        if !app.stored_loading {
            app.refresh_stored(&ctx);
        }
    }
    // Cockpit Gamma resolutions: assign each resolved YES token-id to its (still-placeholder)
    // window's `symbol` (so the window loop's `ensure_poly_book` subscribes it next frame) and
    // cache the resolved short market NAME by token in `poly_names` for the cockpit body's header.
    while let Ok((wid, token, name)) = app.poly_resolve_rx.try_recv() {
        // Cache the name by token unconditionally — the window may already be closed, in which
        // case the entry sits unused (bounded by the number of resolves), but a still-open
        // window reads it via `ToolCtx::poly_names`.
        app.poly_names.insert(token.clone(), name.clone());
        if let Some(w) =
            app.wins.iter_mut().find(|w| w.id == wid && w.symbol == POLY_PLACEHOLDER_TOKEN)
        {
            w.symbol = token;
            w.title = format!("Polymarket · {name}");
        }
    }
    // cheap Arc clone for the per-window title-bar symbol picker (borrows `app` immutably,
    // so it can't live inside the `&mut app.wins` window loop below — snapshot it here)
    let symbols_catalog = app.symbols_catalog.clone();
    // same cheap-Arc-clone reasoning for the Stored inventory tree (read-only in the window
    // loop; mutated only via `refresh_stored`, called after the loop below).
    let stored_tree = app.stored_tree.clone();
    let stored_partials = app.stored_partials.clone();
    let stored_gaps = app.stored_gaps.clone();
    let stored_coverage = app.stored_coverage;
    let stored_loading = app.stored_loading;
    let stored_backfill_status = app.stored_backfill_status.clone();

    // Alt+L: toggle Log/Linear scale on the hovered-else-focused chart
    // window (chart-UX bundle T3). Consumed only outside text-input focus
    // (a TextEdit — e.g. the symbol picker — must keep the literal 'l').
    // Target resolution: `ctx.layer_id_at(pointer)` is egui's own topmost-
    // layer-under-the-cursor query (z-order correct even for overlapping
    // windows — no vike-app-side hit-testing needed); when nothing is
    // hovered, `ctx.top_layer_id()` is the frontmost Order::Middle layer
    // overall. Windows/Areas move themselves to top on click automatically
    // (`egui::Context::move_to_top`'s own doc: "Areas and Windows also do
    // this automatically when being clicked on or interacted with" — the
    // same mechanism `workspace::show_window`'s explicit `move_to_top` call
    // uses after an arrange snap), so `top_layer_id()` IS the existing
    // "last-interacted/front window" signal already tracked by egui itself
    // — vike-app has no separate focus-tracking field of its own to prefer.
    if !ctx.egui_wants_keyboard_input()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::L))
    {
        let target_id = ctx
            .pointer_hover_pos()
            .and_then(|p| ctx.layer_id_at(p))
            .or_else(|| ctx.top_layer_id())
            .map(|l| l.id);
        if let Some(id) = target_id
            && let Some(w) = app.wins.iter_mut().find(|w| {
                w.kind == workspace::WinKind::Chart && w.open && !w.minimized && w.id == id
            })
        {
            w.scale = if w.scale == ScaleMode::Log { ScaleMode::Linear } else { ScaleMode::Log };
        }
    }

    (
        ctx,
        FrameSnapshot {
            symbols_catalog,
            stored_tree,
            stored_partials,
            stored_gaps,
            stored_coverage,
            stored_loading,
            stored_backfill_status,
        },
    )
}

/// Segment 2 — everything between island 1 (the `VIKE_SHOT` capture) and island 2 (the
/// first-frame `ArrangeEnv` arrangement).
///
/// The app CHROME: the frameless caption (brand, menu bar, GPU toggle, command palette, launcher
/// icons, window controls), every drained menu action (arrange / rail / timezone / new window /
/// workspace save+load / named layouts / quit / chart export), the "Save layout as" modal, the
/// minimized-window left rail, the bottom status bar, and finally the `CentralPanel` whose rect
/// becomes `app.desktop`.
///
/// ⚠ That last statement is load-bearing for the caller: island 2's guard is
/// `!app.did_initial_arrange && app.desktop.width() > 600.0`, so this function must run BEFORE it
/// — `app.desktop` is written here and read there, exactly as in the original single body.
///
/// `ctx` is taken BY VALUE for the reason given on [`frame_begin`]: the body passes `&ctx` to
/// `apply_spawn`/`restore_workspace`/`egui::Window::show`, and an owned binding keeps those call
/// sites unchanged. It is an `Arc`-backed handle, so the caller's `clone()` is a refcount bump.
pub(crate) fn draw_chrome(app: &mut App, ui: &mut egui::Ui, ctx: egui::Context) {
    // --- frameless main caption: V · File/Window/Help · palette · launchers · ─ □ ✕ ---
    let mut menu = workspace::MenuResult::default();
    let mut open_kind: Option<workspace::WinKind> = None;
    let mut palette_submit = false;
    let licons = app.launcher_icons.clone(); // cheap Arc clones, used in the caption closure
    // Named layouts for the File → Load/Delete submenus. Recomputed each frame (a read_dir of a
    // small dir is negligible, and only the entries drawn while the menu is open matter) so an
    // externally added/removed layout file is always reflected without a manual refresh.
    let layouts = workspace::persist::list_layouts();
    egui::Panel::top("caption").frame(egui::Frame::NONE).show_separator_line(false).show(
        ui,
        |ui| {
            use egui::{Align, Button, Layout, RichText, Sense, UiBuilder};
            const CAP_H: f32 = 32.0; // match Python titlebar.py TITLEBAR_H = 32
            let (cap, cap_resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), CAP_H),
                Sense::click_and_drag(),
            );
            ui.painter().rect_filled(cap, 0.0, theme::BG);
            if cap_resp.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if cap_resp.double_clicked() {
                let m = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!m));
            }
            // LEFT: V brand + menus
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(cap.shrink2(egui::vec2(8.0, 3.0)))
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    let (br, _) = ui.allocate_exact_size(egui::vec2(24.0, 22.0), Sense::hover());
                    ui.painter().rect_filled(br, 5.0, ACCENT);
                    ui.painter().text(
                        br.center(),
                        egui::Align2::CENTER_CENTER,
                        "V",
                        egui::FontId::proportional(15.0),
                        theme::BG,
                    );
                    ui.add_space(10.0);
                    menu = workspace::menu_bar(ui, &[], app.display_tz, &layouts);
                    // GPU candle layer (GPU Phase 2, Task 3): global render toggle, next to
                    // the other global display controls in this same caption cluster.
                    // Disabled/greyed when `!gpu_ok` (no wgpu backend, or the pipeline build
                    // failed at startup) — a user can never flip this on when there is no
                    // GPU render path to flip it to.
                    ui.add_space(10.0);
                    ui.add_enabled(app.gpu_ok, egui::Checkbox::new(&mut app.gpu_render, "GPU"))
                        .on_hover_text(if app.gpu_ok {
                            "GPU rendering (candles)"
                        } else {
                            "GPU unavailable"
                        });
                },
            );
            // RIGHT: window controls + launcher icons
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(cap.shrink2(egui::vec2(0.0, 2.0)))
                    .layout(Layout::right_to_left(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.visuals_mut().button_frame = false;
                    let ctl = |ui: &mut egui::Ui, g: &str, tip: &str| -> bool {
                        ui.add_sized([34.0, CAP_H - 4.0], Button::new(RichText::new(g).size(14.0)))
                            .on_hover_text(tip)
                            .clicked()
                    };
                    if ctl(ui, "✕", "Close") {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    let m = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                    if ctl(ui, if m { "❐" } else { "□" }, "Maximize") {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!m));
                    }
                    if ctl(ui, "─", "Minimize") {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    ui.add_space(10.0);
                    ui.spacing_mut().item_spacing.x = 2.0; // vike topbar QSS spacing:2px
                    // vike's 9 launcher icons (chart…options), drawn from the bundled PNGs
                    // rendered out of Python's icons.py — exact line-art + TOOL_COLORS.
                    for (name, _bytes, kind) in LAUNCHERS.iter().rev() {
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(26.0, 26.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                5.0,
                                egui::Color32::from_rgb(30, 36, 44),
                            );
                        }
                        if let Some(tex) = licons.get(*name) {
                            ui.painter().image(
                                tex.id(),
                                rect.shrink(4.0), // 26px box → 18px icon (vike topbar icon size)
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                        }
                        let r = resp.on_hover_text(*name);
                        if r.clicked()
                            && let Some(k) = kind
                        {
                            open_kind = Some(*k);
                        }
                    }
                },
            );
            // CENTER: command palette — centered only in the gap between the left
            // (brand + menus) and right (launchers + window controls) clusters.
            let gap_left = cap.left() + 190.0;
            let gap_right = cap.right() - 280.0;
            let pal_w = 360.0_f32.min((gap_right - gap_left - 12.0).max(120.0));
            let pal = egui::Rect::from_center_size(
                egui::pos2((gap_left + gap_right) / 2.0, cap.center().y),
                egui::vec2(pal_w, 24.0),
            );
            ui.scope_builder(
                UiBuilder::new().max_rect(pal).layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    // vike topbar.py: box bg=RAISE, idle border=BORDER grey, FOCUS border=green
                    // hsl(148,50,42)≈(54,161,104); radius 6, 12px. (egui's default focus ring is blue.)
                    {
                        let green = egui::Color32::from_rgb(54, 161, 104);
                        let border = theme::BORDER;
                        let v = ui.visuals_mut();
                        v.extreme_bg_color = theme::SURFACE; // RAISE field bg
                        v.selection.stroke = egui::Stroke::new(1.0, green); // TextEdit focus border
                        v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border);
                        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, border);
                        v.widgets.active.bg_stroke = egui::Stroke::new(1.0, green);
                        v.widgets.inactive.corner_radius = egui::CornerRadius::same(4);
                        v.widgets.hovered.corner_radius = egui::CornerRadius::same(4);
                        v.widgets.active.corner_radius = egui::CornerRadius::same(4);
                    }
                    let resp = ui.add_sized(
                        [pal_w, 24.0],
                        egui::TextEdit::singleline(&mut app.palette)
                            .hint_text("Type symbol or command…  ( / )")
                            .font(egui::FontId::proportional(12.0)),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        palette_submit = true;
                    }
                },
            );
        },
    );
    if palette_submit {
        // The trim / upper-case / `USDT` quote-currency ladder AND the empty-entry refusal
        // (which must spawn nothing and burn no window id — the counter is the `egui::Id`
        // seed) moved into the planner with the rest of the spawn; the buffer is still cleared
        // on EVERY submit, blank or not, exactly as before.
        let raw = app.palette.clone();
        app.palette.clear();
        app.apply_spawn(&ctx, window_spawn::SpawnRequest::Palette { raw });
    }
    if let Some(k) = open_kind {
        // The four-way launcher branch (chart / DOM / cockpit / generic tool) is one
        // exhaustive `match` in the planner now, so a NEW `WinKind` fails to compile there
        // instead of silently inheriting the generic tool geometry the old `else` gave it.
        //
        // The cockpit seed is the ONE environment read this arm makes, and it is still made
        // ONLY for the cockpit kind — so no other launcher gained an `env::var` when the
        // branch moved, and `VIKE_POLY_COCKPIT_TOKEN` keeps its single `vike-app` /
        // `Layer::Binary` row in `crates/vike-ops/src/settings.rs`'s `SETTINGS`.
        let poly_seed_token = if k == workspace::WinKind::Polymarket {
            poly_cockpit_seed_token()
        } else {
            String::new()
        };
        let req = window_spawn::SpawnRequest::Kind { kind: k, poly_seed_token };
        app.apply_spawn(&ctx, req);
    }
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
        app.apply_spawn(&ctx, req);
    }
    if menu.save_workspace {
        match workspace::persist::save(
            &app.wins,
            app.display_tz,
            app.of_backfill_hours,
            app.gpu_render,
            &app.indicator_favs,
        ) {
            Ok(p) => tracing::info!("workspace saved → {}", p.display()),
            Err(e) => tracing::warn!("workspace save failed: {e}"),
        }
    }
    if menu.open_workspace {
        if let Some(ws) = workspace::persist::load() {
            app.restore_workspace(&ctx, &ws);
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
            app.restore_workspace(&ctx, &ws);
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
    // File -> Export chart image…: reuse the VIKE_SHOT framebuffer-readback path — grab
    // egui's OWN wgpu framebuffer (the only capture that survives the flip-model swapchain;
    // an OS/GDI grab returns white). The menu action requests a screenshot this frame; the
    // readback event arrives a frame or two later (below), where we PNG-encode it. Guarded so
    // the interactive export never fights the VIKE_SHOT self-shot (that block owns the event
    // and exits under its own env var, so the two are mutually exclusive in practice anyway).
    if menu.export_chart {
        app.export_pending = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
    }
    if app.export_pending {
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
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

    // File -> Save layout as…: small centered modal to name the layout. Confirm (Save button
    // or Enter) sanitizes the name → `layouts/<name>.json`; Save is disabled while the name
    // sanitizes to nothing (empty / all-punctuation). The menu emitted the intent; the file
    // IO lives here (command-emission contract). Overwriting an existing name is allowed
    // (same as re-saving the single workspace) — the submenu shows what already exists.
    if app.layout_name_prompt {
        let mut do_save = false;
        let mut cancel = false;
        egui::Window::new("Save layout as")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(&ctx, |ui| {
                ui.set_width(280.0);
                ui.label("Layout name:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut app.layout_name_input)
                        .hint_text("e.g. Scalping")
                        .desired_width(f32::INFINITY),
                );
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if app.layout_prompt_focus {
                    resp.request_focus();
                    app.layout_prompt_focus = false;
                }
                let valid =
                    !workspace::persist::sanitize_layout_name(&app.layout_name_input).is_empty();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(valid, egui::Button::new("Save")).clicked()
                        || (enter && valid)
                    {
                        do_save = true;
                    }
                    if ui.button("Cancel").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        cancel = true;
                    }
                });
                if !valid && !app.layout_name_input.trim().is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(200, 120, 60),
                        "name has no usable characters",
                    );
                }
            });
        if do_save {
            let name = app.layout_name_input.clone();
            match workspace::persist::save_layout(
                &name,
                &app.wins,
                app.display_tz,
                app.of_backfill_hours,
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
            app.layout_name_prompt = false;
            app.layout_name_input.clear();
        } else if cancel {
            app.layout_name_prompt = false;
            app.layout_name_input.clear();
        }
    }

    // --- left rail of vertical tabs ---
    let any_min = app.wins.iter().any(|w| w.minimized && w.open);
    if app.show_rail && any_min {
        egui::Panel::left("min_rail")
            .resizable(false)
            .default_size(30.0)
            .min_size(24.0)
            .max_size(40.0)
            .show(ui, |ui| {
                workspace::left_rail(ui, &mut app.wins);
            });
    }

    // --- bottom status bar (vike-python-style: connection + workspace info) ---
    // A fixed-height strip pinned to the app's bottom edge, the egui analog of the PySide
    // status bar. It also fills what used to be dead space below the maximized chart window
    // (the window's own bottom frame inset never reached the desktop edge), so "empty space
    // on maximize" now reads as an intentional status strip. Added BEFORE the CentralPanel so
    // `desktop` (the window arena) excludes it.
    egui::Panel::bottom("statusbar")
        .resizable(false)
        .default_size(STATUS_BAR_H)
        .min_size(STATUS_BAR_H)
        .max_size(STATUS_BAR_H)
        .show_separator_line(false)
        .frame(egui::Frame::NONE.fill(theme::BG))
        .show(ui, |ui| {
            let n_charts =
                app.wins.iter().filter(|w| w.open && w.kind == workspace::WinKind::Chart).count();
            // Remote Scope::Control channel summary (observe mode only; `None` on every local
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
            let control_line = vike_app_core::tradehub_control::control_status_line(
                app.remote_ctrl().is_some(),
                control_connected,
                ctrl_error.as_deref(),
                daemon_identity.as_ref(),
            );
            // …and the three values become ONE `Option`: a segment that EXISTS carries its own
            // connectedness and its own latched-error flag, so "connected, with nothing to paint it
            // on" stops being representable. The bool is stored beside the line it was built from,
            // which is what keeps the two from ever being re-derived out of each other.
            let control = control_line.as_deref().map(|line| ControlSegment {
                line,
                connected: control_connected,
                has_error: ctrl_error.is_some(),
            });
            let dismissed =
                status_bar(ui, &app.status, app.display_tz, n_charts, app.feeds.len(), control);
            if dismissed && let Some(ctrl) = app.remote_ctrl() {
                ctrl.clear_last_error();
            }
        });

    // --- central desktop: the window arena ---
    // Frame::NONE (no 8px inset) so the tiled layout's outer margin == the 2px inter-tile
    // gap (the central inset would otherwise make the outer padding larger than the gaps).
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
        app.desktop = ui.max_rect();
    });
}

/// Segment 3 — everything after island 2 (the first-frame `ArrangeEnv` arrangement).
///
/// The window ARENA and its tail: the maximize-edge re-tile, the per-window loop (chart windows'
/// title bar + `chart::draw`, tool windows' `tool_content`, and every action harvested back onto
/// `WinState`), then the whole collect-then-apply tail the loop's `&mut app.wins` borrow forces —
/// clones, feed/depth/book ensures, orderflow registration, feed teardown + reap, the order
/// dispatch lane (plus the `VIKE_DOM_TESTORDER` QA injection), DataSet mutations, the Stored
/// view's delete/open/refresh/backfill, and the backend picker / editor / settings drains.
///
/// `snap` is destructured into the same local names the original body used, so every reference
/// below reads exactly as it did when they were plain `let`s a few hundred lines further up.
pub(crate) fn draw_windows(app: &mut App, ctx: egui::Context, snap: FrameSnapshot) {
    let FrameSnapshot {
        symbols_catalog,
        stored_tree,
        stored_partials,
        stored_gaps,
        stored_coverage,
        stored_loading,
        stored_backfill_status,
    } = snap;
    // Re-tile the workspace ONLY when the app window itself maximizes/restores (edge-detected),
    // so a tiled layout re-fills the new bounds instead of leaving dead space. This is NOT the
    // rejected "auto-arrange on every resize" — manual drags/resizes never trigger it; only the
    // maximize toggle does, and only when the last arrange was a tiling mode (cascade opts out).
    let maxed = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    if maxed != app.was_maximized {
        app.was_maximized = maxed;
        if app.last_arrange.is_some() {
            app.retile_frames = 5; // re-fill across the next few frames as bounds settle
        }
    }
    if app.retile_frames > 0 {
        if let Some(a) = app.last_arrange {
            workspace::apply_arrange(&mut app.wins, app.desktop, a);
        }
        app.retile_frames -= 1;
        ctx.request_repaint(); // keep ticking while the OS resize settles
    }

    // --- floating chart + tool windows ---
    let desktop = app.desktop;
    let core_snap = app.snap_cell.load_full(); // the paper trading state for the Trade window
    let td = app.tools.lock().unwrap().clone();
    // Options Refresh pill → immediate chain re-poll. Cloned out here (Sender is cheap-Clone) so
    // the per-window loop can borrow it while `app` is mutably split apart inside `show_window`.
    let opt_refresh = app.opt_refresh.clone();
    let flags = app.flags.clone(); // TextureHandle clones are Arc-cheap
    let logos = app.news_logos.clone();
    let dsets = app.datasets.clone(); // small; cloned so app.datasets is free to mutate after
    // (key "SYM@iv", bar count, first bar open-ms, last bar open-ms) — Data-manager columns
    let mut feeds: Vec<(String, usize, i64, i64)> = app
        .charts
        .iter()
        .map(|(k, c)| {
            (
                k.clone(),
                c.bars.len(),
                c.bars.first().map(|b| b.ot).unwrap_or(0),
                c.bars.last().map(|b| b.ot).unwrap_or(0),
            )
        })
        .collect();
    feeds.sort();
    // Cross-venue: (venue, symbol, interval, asset_class) — the primary/compare feeds to
    // ensure after the loop. Compare overlays and foreign-source study symbols are always
    // spot (`None` — see their push sites); the primary carries the window's own
    // `venue`/`asset_class` so a Bybit/OKX chart (or an OKX derivative) subscribes the right
    // feed (feed-routing slice 1).
    let mut to_ensure: Vec<(String, String, String, Option<vike_catalog::AssetClass>)> = Vec::new();
    // SP2 orderflow (Task 7): (venue, symbol, chart key, requested tick size) for every OPEN
    // chart window whose `WinState::orderflow_on()` is true this frame — collected here (pure
    // reads of `w`) because `app.wins.iter_mut()` below holds `app.wins` mutably, so the
    // actual `&mut App` work (trade subscribe + `of_aggs` registration) can only happen
    // AFTER the loop (mirrors `to_ensure`/`dom_depth_reqs`'s collect-then-apply shape). The
    // window's `venue` is carried so orderflow subscribes the RIGHT venue's tape (venue-aware
    // — any `subscribe_trades` venue, not just Binance). Pushed unconditionally every frame a
    // toggle is on — cheap, and idempotent downstream (`ensure_trade_feed_on`'s `spawned`
    // check, `of_aggs`'s `entry`), same as `dom_depth_reqs`.
    let mut of_wanted: Vec<(String, String, String, Option<f64>)> = Vec::new();
    let mut to_clone: Vec<usize> = Vec::new();
    // vike maximize = fill the workspace by HIDING every other live window.
    let maxed_idx = app.wins.iter().position(|w| w.open && !w.minimized && w.maximized);
    let mut user_moved_any = false; // any title-bar drag this frame → drop the tiling memory
    let mut to_stop: Vec<String> = Vec::new(); // Data-manager "Delete" → drop these feeds after
    let mut ds_save: Option<datasets::DataSet> = None; // DataSet Save → upsert after the loop
    let mut trade_orders: Vec<tools::TradeSubmit> = Vec::new(); // Trade ticket → core commands
    let mut trade_cancels: Vec<String> = Vec::new();
    let mut trade_margins: Vec<(String, String, f64)> = Vec::new(); // leverage pill → SetMargin
    let mut dom_actions: Vec<(String, String, dom::DomAction)> = Vec::new(); // (venue, inst, action) → core
    // Polymarket cockpit order intents resolved in the window loop, folded onto the command lane
    // after it (the cockpit twin of `dom_actions`; venue is always "polymarket").
    let mut cockpit_cmds: Vec<CockpitCmd> = Vec::new();
    let mut opt_orders: Vec<tools::OptOrderTicket> = Vec::new(); // Options confirm-ticket → deribit exec
    let mut opt_cancels: Vec<String> = Vec::new(); // Options chain marker → deribit cancel (coid)
    let mut dom_depth_reqs: Vec<(dom::DomVenue, String)> = Vec::new(); // DOM → ensure a venue depth stream
    let mut poly_book_reqs: Vec<String> = Vec::new(); // cockpit → ensure a polymarket token book+trade stream
    let mut ds_del: Option<String> = None; // DataSet Delete
    let mut ds_open: Option<String> = None; // "Test symbol" → open a chart after the loop
    // Data Manager "Stored" view (Task 3) OUT actions, applied after the window loop below.
    let mut stored_refresh_requested = false; // Refresh click / first-shown / post-delete reload
    let mut stored_deletes: Vec<(String, String, String, Option<String>)> = Vec::new(); // (venue, symbol, kind, interval)
    let mut stored_opens: Vec<(String, String, Option<String>)> = Vec::new(); // (venue, symbol, interval)
    // dm-bulk-backfill: grid v2's bulk Backfill/Update selection, drained below and spawned
    // after the loop via `App::maybe_spawn_stored_backfill` (same deferred-mutation shape).
    let mut stored_backfills: Vec<vike_data_manager::SeriesKey> = Vec::new();
    // Connections backend picker (split-plane B1) OUT slot + its availability gate, applied
    // after the loop via `App::apply_backend_action` (the same deferred-mutation shape as
    // every action above — the switch routine needs `&mut App` the loop can't give it).
    let mut backend_action: Option<vike_app_core::backend_conn::BackendAction> = None;
    let backend_switching = vike_app_core::backend_conn::switching_available(app.core.is_some());
    // Backends editor (split-plane I2) OUT slot: the drained Add/Edit/Delete registry
    // update, applied after the loop through `backend_editor::apply_registry_update` (which
    // pins delete-active's disconnect-BEFORE-save order).
    let mut backend_registry_update: Option<vike_app_core::backend_editor::RegistryUpdate> = None;
    // The ONE datahub resolution (REQ-2), taken once per frame BEFORE the windows loop (the
    // loop borrows `app` piecemeal): explicit `config.datahub_addr` first, else the active
    // backend's Welcome advertisement — see `App::resolved_datahub_addr`.
    let resolved_datahub_addr = app.resolved_datahub_addr();
    // ...and the market-data session is pointed at it HERE rather than at `App::new`, because at
    // `App::new` the answer is not known: the second rung of that resolution is the ACTIVE
    // backend's `Welcome` advertisement, which lands only after the observe bridge handshakes.
    // An equal value is a compare and a return, so the per-frame cost is one uncontended `Mutex`.
    app.md_session.set_addr(resolved_datahub_addr.as_deref());
    // The KEY follows the same rule for the same reason — both are properties of the ACTIVE backend
    // record, which a switch replaces. See `App::datahub_observe_key_name`.
    app.md_session.set_key_name(&app.datahub_observe_key_name());
    // Backend-settings section (split-plane REQ-7, read half): the ACTIVE backend's slot
    // state this frame — `Idle` when the slot belongs to another (switched-away) backend, so
    // stale rows never render and the section auto-refetches. The `refresh` OUT slot is
    // drained after the loop via `App::spawn_backend_settings_fetch` (the same
    // deferred-mutation shape as `backend_action` above).
    let mut backend_settings_refresh = false;
    let backend_settings_view: vike_app_core::tool_views::BackendSettingsState = {
        match app.active_backend.as_ref().map(|b| b.record.addr.as_str()) {
            Some(addr) => {
                let slot = app.backend_settings.lock().unwrap();
                if slot.0 == addr { slot.1.clone() } else { Default::default() }
            }
            None => Default::default(),
        }
    };
    // REQ-7 WRITE half: the edit flow is `app` state (typed into across frames) TAKEN into a
    // frame-local — the window loop below borrows `app.wins` mutably, so `&mut App` fields
    // can't cross into it — and written back after the loop. `backend_settings_write` is the
    // Save out-slot, drained after the loop into `spawn_backend_settings_write`.
    let mut backend_settings_edit = std::mem::take(&mut app.backend_settings_edit);
    let mut backend_settings_write: Option<vike_app_core::tool_views::SettingsWriteRequest> = None;
    // Fold a finished write first (the worker parked the already-folded flow state, keyed by
    // addr): still-active backend ⇒ show it, and a SAVED write refetches the table so the new
    // file value renders beside the restart note; switched-away ⇒ dropped, never painted
    // under the new backend (the fetch slot's discipline).
    if let Some((addr, folded)) = app.backend_settings_write_result.lock().unwrap().take() {
        if app.active_backend.as_ref().is_some_and(|b| b.record.addr == addr) {
            if vike_app_core::tool_views::should_refetch_after_write(&folded) {
                backend_settings_refresh = true;
            }
            backend_settings_edit = folded;
        } else {
            backend_settings_edit = Default::default();
        }
    }
    // Book is "stale" if no update landed within this window (or none has arrived yet).
    const DOM_STALE_MS: i64 = 2000;
    let now_ms = chrono::Local::now().timestamp_millis();
    let display_tz = app.display_tz; // Copy; read once (task A6 clocks + tool_content)
    // SP3 follow-up (Task 2): Copy; read once, threaded into every chart window's OF popup
    // this frame (`title_bar`'s `of_backfill_hours` param) — same up-front-copy shape as
    // `display_tz` right above, a plain local the per-window `title_bar` call below can read
    // without going back through `app`.
    let of_backfill_hours = app.of_backfill_hours;
    // GPU candle layer (GPU Phase 2, Task 3): Copy; read once, same up-front-copy shape as
    // `display_tz`/`of_backfill_hours` above — `app.wins.iter_mut()` mutably borrows `app`
    // for the whole window loop below, so a closure built inside it can't reach back through
    // `app.gpu_render`/`app.gpu_ok` directly. `gpu_build` captures neither `app` nor
    // anything else (its two params supply everything it needs), so it's built once here,
    // outside the loop, and every window below borrows the SAME closure by reference.
    let gpu_render = app.gpu_render;
    let gpu_ok = app.gpu_ok;
    let gpu_build =
        |instances: Vec<vike_chart::render::CandleInstance>, rect: egui::Rect| -> egui::Shape {
            eframe::egui_wgpu::Callback::new_paint_callback(
                rect,
                chart_gpu::CandleCallback { instances, rect },
            )
            .into()
        };
    {
        let charts = &app.charts;
        let books = std::sync::Arc::clone(&app.books); // read live L2 books inside the loop
        let live_venues = &app.live_venues; // which venues route to a LIVE exec client (else paper)
        let of_aggs = &app.of_aggs; // SP2 orderflow (Task 7): read-only inside the loop
        // Indicator favourites (part b): a MUTABLE handle to the global favourites set,
        // bound here (a distinct field from `app.wins` below, so the split borrow is
        // sound) so the per-window ƒx picker can star/unstar into it while `w` is `&mut`.
        let indicator_favs = &mut app.indicator_favs;
        for (wi, w) in app.wins.iter_mut().enumerate() {
            if maxed_idx.is_some_and(|mi| mi != wi) {
                continue; // another window is maximized — hide this one
            }
            if !w.open || w.minimized {
                continue;
            }
            let wid = w.id;
            let wid64 = wid.value(); // chart sync seam (task B8): u64 identity for the group registry
            let is_max = w.maximized;
            let mut act = TitleActions::default();

            if w.kind == workspace::WinKind::Chart {
                let key = w.key();
                let chart = charts.get(&key);
                let bars_slice: &[model::Bar] = chart.map(|c| c.bars.as_slice()).unwrap_or(&[]);
                let last = chart.and_then(|c| c.bars.last().copied());
                let prev = w.hover;
                let style = w.style;
                let sync_group = w.sync_group;
                // Chart sync seam (task B8): this window's ghost crosshair/injected range for
                // THIS frame, resolved from last frame's harvest (`app.sync_prev`); `None` for
                // an ungrouped window (or a group nobody has broadcast into yet).
                let sync_in = sync_feed(&app.sync_prev, sync_group, wid64);
                let symbol = w.symbol.clone();
                let venue = w.venue.clone(); // cross-venue: title-bar prefix + selection reset
                let interval = w.interval.clone();
                // C2a Task 4: gather this window's Compare overlays for the price pane. Clone
                // the symbol list (like `symbol`/`interval` above) so each `SeriesInput.symbol`
                // borrows THIS local rather than `w` — `w` is passed `&mut` into `show_window`
                // below, and the built `overlays` Vec must outlive that closure (it's read at
                // the `overlays: &overlays` feed site inside it). Each overlay's `ChartState` is
                // looked up in `app.charts` exactly like the primary `chart` above (same
                // immutable `charts` borrow, disjoint from the `&mut w`); a compare symbol not
                // yet synced is silently skipped (`filter_map`) until the ensure below populates
                // it. Empty `compare` ⇒ empty Vec ⇒ `overlays: &[]` — byte-identical to the
                // pre-Task-4 single-series render.
                //
                // ENSURE-SYNC: push each compare symbol onto `to_ensure` (idempotent, applied
                // after the loop by the existing `ensure_feed_on` fan-out) so its `ChartState`
                // gets created in `app.charts` AND live-synced by `sync_from_core` every frame
                // — mirrors the `of_wanted`/`to_ensure` collect-now/apply-after shape. Without
                // this the lookup above stays `None` and nothing overlays.
                let compare_syms: Vec<String> = w.compare.clone();
                for sym in &compare_syms {
                    // Compare overlays are Binance-namespaced (`{sym}@{interval}` lookup below)
                    // and always spot.
                    to_ensure.push((
                        DEFAULT_VENUE.to_string(),
                        sym.clone(),
                        interval.clone(),
                        None,
                    ));
                }
                let overlays: Vec<chart::SeriesInput> = compare_syms
                    .iter()
                    .enumerate()
                    .filter_map(|(i, sym)| {
                        // C2a Task 4 review Minor: never overlay the window's OWN
                        // symbol. `add_compare` blocks ADDING self, but if the user
                        // later switches the primary TO an already-compared symbol it
                        // would otherwise overlay itself — filter it here (case-
                        // insensitive, matching `add_compare`). Filtered INSIDE the
                        // enumerate so the surviving overlays keep their add-order
                        // color index (aligned with the title-bar chips).
                        if sym.eq_ignore_ascii_case(&symbol) {
                            return None;
                        }
                        let ckey = format!("{sym}@{interval}");
                        charts.get(&ckey).map(|st| chart::SeriesInput {
                            symbol: sym.as_str(),
                            state: st,
                            color: COMPARE_COLORS[i % COMPARE_COLORS.len()],
                        })
                    })
                    .collect();
                // SP2 orderflow (Task 7): this window's current toggle state, read before the
                // `show_window` closure below (same up-front extraction as `style`/`sync_group`
                // above — `w` is reborrowed by `show_window`, so the closure can't reach it
                // directly). `of` is this chart's aggregator, if orderflow has ever been
                // requested for it (`None` ⇒ nothing to render yet); `fps` is its per-bar
                // footprint snapshot for THIS frame. Queue the (idempotent) subscribe+register
                // request for after the loop when the toggle/style combo wants orderflow —
                // `app.wins` is mutably borrowed for the whole loop, so `&mut App` work
                // (`ensure_trade_feed_on`, `of_aggs.entry`) can't happen here (mirrors
                // `to_ensure`/`dom_depth_reqs`'s collect-then-apply shape).
                let cvd_on = w.cvd_on;
                let profile_on = w.profile_on;
                let of_tick_size_w = w.of_tick_size;
                let of = of_aggs.get(&key);
                let fps = of.map(|(_, _, a)| a.footprints());
                if w.orderflow_on() {
                    of_wanted.push((venue.clone(), symbol.clone(), key.clone(), of_tick_size_w));
                }
                // chart single-max default: reconcile Volume/CVD into the authored
                // sub-pane order (they are reorderable peers of study panes now),
                // then snapshot the unified order HERE — before `w.options` is taken
                // below, since `present_sub_panes()` reads `show_volume` from it. The
                // result is an owned Vec, so it survives the `show_window` borrow of `w`.
                w.sync_sub_panes();
                let sub_panes = w.present_sub_panes();
                let nav_in = w.nav.take();
                let mut inds = std::mem::take(&mut w.indicators);
                // Two-tier fold: the persistent instance advances over CLOSED bars
                // only (no-op / one on_bar / refold on structural change); the live
                // forming bar is previewed on a throwaway clone each frame, so a
                // tick never triggers a full-history refold.
                let n_closed = chart.map(|c| c.closed_len.min(c.bars.len())).unwrap_or(0);
                let (closed, forming) = bars_slice.split_at(n_closed);
                for a in &mut inds {
                    // Foreign-source study (TradingView "symbol" input): fold over
                    // ANOTHER symbol's bars re-indexed onto THIS chart's timeline
                    // (same interval), so the series stays index-aligned to the
                    // primary x-axis. `None` ⇒ the primary bars, byte-identical to
                    // before this feature. The `.clone()` releases the immutable
                    // `source_symbol` borrow before the `&mut a.update` below.
                    match a.source_symbol.clone() {
                        None => a.update(closed, forming.first()),
                        Some(src) => {
                            // Ensure-subscribe the foreign feed (idempotent, applied
                            // after the loop like the Compare gather) so its ChartState
                            // exists in `charts` and is live-synced every frame.
                            // Foreign-source study symbols carry no asset class — always spot.
                            to_ensure.push((
                                src.venue.clone(),
                                src.symbol.clone(),
                                interval.clone(),
                                None,
                            ));
                            let fkey = workspace::series_key(&src.venue, &src.symbol, &interval);
                            if let Some(fcs) = charts.get(&fkey) {
                                // Re-index the foreign bars onto the primary timeline,
                                // then split at the PRIMARY's closed count so the
                                // forming preview lines up (foreign closed bars are
                                // immutable, so the two-tier fast path still holds).
                                let aligned = indicators::align_source_bars(bars_slice, &fcs.bars);
                                let split = n_closed.min(aligned.len());
                                let (fc, ff) = aligned.split_at(split);
                                a.update(fc, ff.first());
                            } else {
                                a.update(&[], None); // not synced yet → show nothing
                            }
                        }
                    }
                }
                let mut cur = None;
                let mut nav_out = None;
                let mut remove_uid = None;
                let mut scale_out = None;
                let mut invert_out = None;
                let mut options_out = None;
                let mut indicator_out = None;
                // Chart sync seam (task B8): this frame's `ChartActions` sync outputs, harvested
                // into the group registry AFTER `show_window` returns (see `sync_harvest` below).
                let mut hover_ts_out = None;
                let mut visible_ts_out = None;
                let mut interacted_out = false;
                // SP2 orderflow (Task 7): the CVD sub-pane's own ✕ this frame
                // (`chart::ChartActions::cvd_toggle`) — harvested after `show_window` below.
                let mut cvd_toggle_out = false;
                // Volume-as-indicator: the volume pane ✕ was clicked this frame.
                let mut volume_remove_out = false;
                // C1 Task 4: the study relocation picked from a pane header's
                // ••• "Move to" menu this frame — applied to `w` after
                // `show_window` returns (see the harvest block below).
                let mut move_study_out: Option<(u64, chart::MoveTarget)> = None;
                // Feature #1: the pane ↑/↓ reorder picked this frame — applied
                // to `w` after `show_window` returns (harvest block below).
                let mut reorder_pane_out: Option<(chart::PaneKey, bool)> = None;
                let mut follow = std::mem::take(&mut w.follow);
                let options = std::mem::take(&mut w.options);
                let mut settings = std::mem::take(&mut w.settings);
                let mut indicator_dialog = std::mem::take(&mut w.indicator_dialog);
                let requested_scale = w.scale;
                let requested_invert = w.invert;
                // T9: same `mem::take`-then-write-back dance as follow/options/
                // settings/indicator_dialog above — `w` itself is passed into
                // `show_window` below, so the closure can't hold `&mut w.panes`
                // directly (it would conflict with that borrow of the whole `w`).
                let mut panes = std::mem::take(&mut w.panes);
                // C1 Task 4: thread the authored per-study assignment map into
                // `chart::draw`. Same `mem::take`/write-back dance — `w` is
                // borrowed by `show_window`, so the closure can't hold
                // `&w.study_pane`. The unified sub-pane order (`sub_panes`) was
                // snapshotted above; `study_pane` itself is taken out and written
                // back after `show_window` — restored before `move_study` is
                // applied below.
                let study_pane = std::mem::take(&mut w.study_pane);
                // C2b Task 6: the authored compare-series pane read-model, threaded
                // into `chart::draw` with the SAME `mem::take`/write-back dance as
                // `study_pane` above (`w` is borrowed by `show_window`, so the closure
                // can't hold `&w.series_pane`). `present_series_panes()` is an OWNED
                // snapshot computed HERE while `series_pane` is still populated;
                // `series_pane` itself is taken out and written back UNCONDITIONALLY
                // after `show_window` (all paths). Empty until Task 9's menu moves a
                // compare symbol into its own pane ⇒ `series_panes` is `&[]` and the
                // render is byte-identical.
                let series_panes = w.present_series_panes();
                let series_pane = std::mem::take(&mut w.series_pane);
                // C2b Task 7: per-symbol secondary-axis assignment, taken out with
                // the same `mem::take`/write-back dance (the closure can't borrow
                // `w`). EMPTY until Task 9's "Pin to scale" menu populates it ⇒
                // `chart::draw` routes every overlay to the shared % axis ⇒
                // byte-identical.
                let series_scale = std::mem::take(&mut w.series_scale);
                if workspace::show_window(&ctx, w, desktop, |ui| {
                    let (acts, drag) = chart_window::title_bar(
                        ui,
                        wid,
                        &symbol,
                        &venue,
                        &interval,
                        style,
                        sync_group,
                        is_max,
                        cvd_on,
                        profile_on,
                        of_tick_size_w,
                        of_backfill_hours,
                        &compare_syms, // C2a Task 4: current overlays → the Compare popup chips
                        &series_pane,  // C2b Task 9: own-pane assignment → the per-chip menu
                        &series_scale, // C2b Task 9: secondary-axis pins → the per-chip menu
                        &symbols_catalog, // symbol-search: live instrument universe → picker
                    );
                    act = acts;
                    // OHLC is now an in-plot overlay (chart::draw) so the window can shrink.
                    let _ = (prev, last);
                    if let Some(c) = chart {
                        let acts = chart::draw(
                            ui,
                            chart::ChartInputs {
                                state: c,
                                style,
                                nav: nav_in,
                                indicators: &inds,
                                // Tick-driven microstructure studies (vike-chart
                                // `studies.rs`). Empty until the app grows its own
                                // study menu + tick fan-in — an empty slice renders
                                // byte-identically to the pre-studies chart.
                                studies: &[],
                                follow: &mut follow,
                                options: &options,
                                settings: &mut settings,
                                indicator_dialog: &mut indicator_dialog,
                                scale: requested_scale, // chart-UX bundle T3: persisted per-window
                                invert: requested_invert, // TradingView "Invert scale": persisted per-window
                                panes: &mut panes, // chart-UX bundle T9: persisted per-window
                                sync: sync_in, // chart sync seam (task B7 shape, task B8 wiring)
                                // SP2 orderflow (Task 7): live-wired. `footprint` is `None` until
                                // this chart's aggregator exists AND has ingested at least one
                                // trade batch (a one-two-frame startup lag after the toggle
                                // flips) — `chart::draw` treats `None` as "nothing to render yet"
                                // exactly like the pre-Task-7 always-off wiring did.
                                // SP3 Task B #1: `footprints()` now hands out `Arc<Vec<..>>`
                                // (cached — see `orderflow::OrderflowAgg::cache`'s doc), so
                                // `.as_deref()` alone only reaches `&Vec<FootprintBar>` (one
                                // level, `Arc`→`Vec`); `.as_slice()` takes the second step to
                                // the `&[FootprintBar]` `ChartInputs::footprint` wants.
                                footprint: fps.as_deref().map(|v| v.as_slice()),
                                // SP3 TB-fix (post-Task-B review finding, MEDIUM): the
                                // monotonic generation backing `footprint`'s content, so the
                                // chart-side CVD cache can key on it instead of the footprint
                                // slice's own (ABA-able) address — see vike-chart's
                                // `model.rs::CvdCacheKey` doc. `0` when there's no aggregator
                                // yet, mirroring `footprint: None`'s own default-off shape.
                                footprint_gen: of.map(|(_, _, a)| a.generation()).unwrap_or(0),
                                cvd_on,
                                profile_on,
                                // The AGG's actual bucketing width (whichever of user-pinned or
                                // its own default it landed on) — footprint/profile rendering
                                // must match the granularity the data was bucketed at, so this
                                // is never the chart-side "derive" 0.0 once an agg exists (see
                                // `orderflow::OrderflowAgg::new`'s `<= 0.0 → 1.0` default).
                                of_tick_size: of_tick_size_w
                                    .or_else(|| of.map(|(_, _, a)| a.tick_size()))
                                    .unwrap_or(0.0),
                                // Chart single-max default: the authored UNIFIED
                                // sub-pane read-model (Volume/CVD/Study peers, in
                                // user order). `sub_panes` is `present_sub_panes()`'s
                                // owned snapshot; `study_pane_of` is the taken-out
                                // per-study assignment map. `chart::draw` gates each
                                // entry to visible panes internally.
                                sub_panes: &sub_panes,
                                study_pane_of: &study_pane,
                                // C2a Task 4: the Compare overlays gathered above (each a
                                // %-normalized line in the price pane). Empty ⇒ same `&[]`
                                // no-op the pre-Task-4 wiring fed.
                                overlays: &overlays,
                                // C2b Task 6: the authored own-pane compare-series model.
                                // `series_panes` is `present_series_panes()`'s owned
                                // snapshot; `series_pane_of` is the taken-out symbol→pane
                                // map. `chart::draw` reverse-looks-up each pane's symbol,
                                // finds its `SeriesInput` in `overlays`, and renders it as
                                // an absolute-price line in its own sub-pane; own-paned
                                // symbols are skipped by the price-overlay loop. Empty ⇒
                                // byte-identical.
                                series_panes: &series_panes,
                                series_pane_of: &series_pane,
                                // C2b Task 7: per-symbol secondary-axis ("Pin to
                                // scale ▸ Right") assignment. Empty ⇒ every overlay
                                // stays a %-line ⇒ byte-identical. Task 9's menu
                                // writes the pins back onto `w.series_scale`.
                                series_scale: &series_scale,
                                // GPU candle layer (GPU Phase 2, Task 3): wired only when the
                                // user has the toggle on AND the pipeline is actually
                                // available (`gpu_ok`) — off by default, so `None` here keeps
                                // the candle render byte-identical to the pre-Phase-2 egui/LOD
                                // path until both are true.
                                gpu_candles: (gpu_render && gpu_ok)
                                    .then_some(&gpu_build as &dyn Fn(_, _) -> _),
                            },
                        );
                        cur = acts.hovered;
                        nav_out = acts.nav_out;
                        remove_uid = acts.remove_uid;
                        scale_out = acts.scale_change;
                        invert_out = acts.invert_change;
                        options_out = acts.options_change;
                        indicator_out = acts.indicator_edit;
                        hover_ts_out = acts.hover_ts;
                        visible_ts_out = acts.visible_ts;
                        interacted_out = acts.interacted;
                        cvd_toggle_out = acts.cvd_toggle;
                        volume_remove_out = acts.volume_remove;
                        move_study_out = acts.move_study;
                        reorder_pane_out = acts.reorder_pane;
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("loading…");
                        });
                    }
                    drag
                }) {
                    user_moved_any = true;
                }
                w.follow = follow;
                w.options = options;
                w.settings = settings;
                w.indicator_dialog = indicator_dialog;
                w.panes = panes; // chart-UX bundle T9
                w.study_pane = study_pane; // C1 Task 4: restore before `move_study` apply below
                w.series_pane = series_pane; // C2b Task 6: restore (unconditional, all paths)
                w.series_scale = series_scale; // C2b Task 7: restore (unconditional, all paths)
                w.indicators = inds;
                w.hover = cur;
                w.nav = nav_out;
                // SP2 orderflow (Task 7): the CVD pane's own ✕ was clicked this frame — write
                // the persisted toggle back to off (mirrors `remove_uid`'s "the caller owns
                // the source of truth" shape; see `ChartActions::cvd_toggle`'s doc).
                if cvd_toggle_out {
                    w.cvd_on = false;
                }
                // Volume-as-indicator: the volume pane ✕ turns off show_volume
                // (re-add via the ƒx picker's "Volume" entry). `w.options` was
                // restored above, so this direct write persists like cvd.
                if volume_remove_out {
                    w.options.show_volume = false;
                }
                // C1 Task 4: apply the ••• "Move to" relocation onto the authored
                // pane model (`w.study_pane`/`w.pane_order` are both intact here —
                // `study_pane` was written back above, `pane_order` was never
                // taken). `move_study` re-points the study's pane and drops any
                // now-empty pane; like the sibling toggles this is a direct field
                // write (no dirty flag exists — `persist::save` reads live state
                // whenever the user explicitly saves).
                if let Some((uid, target)) = move_study_out {
                    w.move_study(uid, target);
                }
                // Feature #1: apply the pane ↑/↓ reorder onto `w.pane_order`
                // (intact here — never taken). Direct write, same shape as
                // `move_study` above. Volume/CVD/Study are peers now.
                if let Some((pane, up)) = reorder_pane_out {
                    w.reorder_pane(pane, up);
                }
                if let Some(sm) = scale_out {
                    w.scale = sm;
                }
                if let Some(iv) = invert_out {
                    w.invert = iv; // TradingView "Invert scale" toggle from the price-axis menu
                }
                if let Some(o) = options_out {
                    w.options = o; // T6: OK in the settings dialog committed a new palette/flags
                }
                // Chart sync seam (task B8): harvest this frame's sync outputs into the group
                // registry (no-op when `sync_group` is `None` — see `sync_harvest`'s doc).
                sync_harvest(
                    &mut app.sync_next,
                    &mut app.range_leader,
                    sync_group,
                    wid64,
                    interacted_out,
                    visible_ts_out,
                    hover_ts_out,
                );
                // T8: apply a live indicator edit onto the real `Active` (the
                // chart's `indicators` slice was immutable). set_params refolds
                // only when the params actually moved (a debounced, O(history)
                // refold, keyed off pointer-release); colours+widths always
                // re-apply (cheap). Restores from the snapshot on Cancel.
                if let Some((
                    uid,
                    chart::IndicatorEdit {
                        params,
                        source,
                        lines,
                        show_bands,
                        bands,
                        show_ob_os_fill,
                        ob_fill,
                        os_fill,
                        visible,
                    },
                )) = indicator_out
                    && let Some(a) = w.indicators.iter_mut().find(|a| a.uid == uid)
                {
                    // Source selector (chart source selector): a changed Source
                    // needs a refold, exactly like a param change. Set it FIRST
                    // so whichever refold runs below folds off the new series.
                    let source_changed = a.source != source;
                    a.source = source;
                    if a.params != params {
                        a.set_params(params, closed); // rebuilds + refolds (new source applied)
                    } else if source_changed {
                        a.recompute_full(closed); // params unchanged → just refold off the new source
                    }
                    for (line, (col, wid, vis, ls)) in a.outputs.iter_mut().zip(&lines) {
                        line.color = *col;
                        line.width = *wid;
                        line.visible = *vis;
                        line.line_style = *ls;
                    }
                    a.show_bands = show_bands;
                    // Per-level band colour/show (RSI "Style" parity): `bands`
                    // is index-aligned to `a.bands` (both seeded from
                    // `spec.bands`); update value untouched (static level).
                    for (band, (_v, col, show)) in a.bands.iter_mut().zip(&bands) {
                        band.color = *col;
                        band.show = *show;
                    }
                    a.show_ob_os_fill = show_ob_os_fill;
                    a.ob_fill = ob_fill;
                    a.os_fill = os_fill;
                    a.visible = visible;
                }
                if let Some(uid) = remove_uid {
                    w.remove_indicator(uid);
                }
                if act.open_picker {
                    w.picker_open = !w.picker_open;
                }
                if let Some(name) =
                    tool_views::fx_picker_popup(&ctx, w, indicator_favs, &symbols_catalog)
                {
                    // Part (a): route the add to the picker-chosen target pane
                    // (reusing `move_study`); `PaneTarget::Auto` == today's behavior.
                    let target = w.picker_target;
                    w.add_indicator_to(name, bars_slice, target);
                }
                if let Some(s) = act.set_style {
                    w.style = s;
                }
                // C2a Task 4: Compare popup — add/remove a %-overlay series. `add_compare`
                // dedups, ignores the window's own symbol, and (on the FIRST overlay)
                // auto-switches the scale to Percent so the overlay is visible; the per-frame
                // gather above ensure-subscribes each compare symbol's feed. Applied AFTER the
                // `scale_out` write above so a first-overlay auto-Percent isn't clobbered by a
                // same-frame manual scale toggle (they never realistically collide). The next
                // frame's gather picks up the mutated `w.compare`.
                if let Some(s) = act.add_compare {
                    w.add_compare(&s);
                }
                if let Some(s) = act.remove_compare {
                    w.remove_compare(&s);
                }
                // C2b Task 9: apply the per-chip Move-to / Pin-to-scale menu onto the
                // (already-restored above) `w.series_pane`/`w.series_scale`. Each menu
                // item sources `sym` from `w.compare` (the chip loop only iterates
                // compare), so `move_series_to_new_pane` is always called with a live
                // compare symbol — closing Task 5 review Minor-2 by construction.
                if let Some(s) = act.series_to_own_pane {
                    w.move_series_to_new_pane(&s);
                }
                if let Some(s) = act.series_to_overlay {
                    w.overlay_series(&s);
                }
                if let Some((s, assign)) = act.set_series_scale {
                    // Percent = the default ⇒ drop the pin (keep the map clean, so a
                    // later remove/re-add defaults correctly); Right = an explicit pin.
                    if assign == chart::ScaleAssign::Percent {
                        w.series_scale.shift_remove(&s);
                    } else {
                        w.series_scale.insert(s, assign);
                    }
                }
                // SP2 orderflow (Task 7): title-bar Orderflow popup checkboxes/field.
                if let Some(v) = act.cvd_on {
                    w.cvd_on = v;
                }
                if let Some(v) = act.profile_on {
                    w.profile_on = v;
                }
                if let Some(v) = act.of_tick_size {
                    w.of_tick_size = v;
                }
                // SP3 follow-up (Task 2): Backfill-hours field writes the GLOBAL
                // `App::of_backfill_hours` (not `w.*`, unlike the three siblings above — see
                // `TitleActions::new_backfill_hours`'s doc). `app.wins` is only borrowed via
                // `w` here (a disjoint field from `app.of_backfill_hours`), so writing it
                // directly alongside `w`'s own field writes is fine — same disjoint-field
                // pattern `sync_harvest`'s `&mut app.sync_next`/`&mut app.range_leader`
                // args already rely on inside this same loop. No separate "dirty"/autosave
                // flag exists anywhere in this file to also set: every one of these toggles
                // just writes the live field, and `workspace::persist::save` (only called
                // from the explicit `menu.save_workspace` action) reads it whenever that
                // actually happens — this field is picked up the exact same way.
                if let Some(v) = act.new_backfill_hours {
                    app.of_backfill_hours = v;
                }
                if let Some(s) = act.new_symbol {
                    // C2 tidy (FIX 1): drop any Compare chip that now matches the new
                    // primary symbol. `add_compare` already blocks ADDING the window's own
                    // symbol, but switching the PRIMARY onto an already-compared symbol
                    // used to leave a dangling chip — the price-pane render already
                    // self-filters an overlay matching `symbol` (see the
                    // `eq_ignore_ascii_case` guard above), so the chip was stale (harmless
                    // to the chart, but confusing/removable-for-nothing in the popup).
                    w.remove_compare(&s);
                    w.symbol = s;
                    // Cross-venue: a search hit (or quick-pick) carries its venue, set here
                    // ATOMICALLY with the symbol so `w.key()`/`ensure_feed_on` route to the
                    // right feed. `new_venue` is always `Some` when `new_symbol` is (both set
                    // at every selection site); default to Binance if a caller ever omits it.
                    w.venue = act.new_venue.take().unwrap_or_else(|| DEFAULT_VENUE.to_string());
                    // Feed-routing slice 1: set atomically with symbol/venue so `w.asset_class`
                    // never lags the symbol it describes (`take()` leaves `None` for the next
                    // frame, mirroring `new_venue`'s take-and-default idiom).
                    w.asset_class = act.new_asset_class.take();
                    w.retitle();
                    // Auto-scale the new symbol: re-engage follow-live + y-autofit and
                    // force a full-range refit of both axes (else a same-bar-count swap
                    // inherits the old symbol's zoom/pan — the "doesn't auto-scale" bug).
                    w.follow.on_series_change();
                    to_ensure.push((
                        w.venue.clone(),
                        w.symbol.clone(),
                        w.interval.clone(),
                        w.asset_class,
                    ));
                }
                if let Some(iv) = act.new_interval {
                    w.interval = iv;
                    w.retitle();
                    w.follow.on_series_change(); // same refit on a timeframe swap
                    to_ensure.push((
                        w.venue.clone(),
                        w.symbol.clone(),
                        w.interval.clone(),
                        w.asset_class,
                    ));
                }
                if let Some(g) = act.new_group {
                    w.sync_group = g; // task B8: chip click — None -> 1 -> 2 -> 3 -> 4 -> None
                }
                if act.clone {
                    to_clone.push(wi);
                }
            } else {
                let kind = w.kind;
                let symbol = w.symbol.clone(); // DOM canonical symbol; "" for other tools
                let mut tv = app.tool_views.remove(&wid).unwrap_or_default();
                // Studio (Task 7): the singleton session, split off `app` the same way `tv`
                // is (see `App::studio`'s doc — it's one instance, not keyed per-window), so
                // it's usable inside the `show_window` closure below and restored after.
                let mut studio = app.studio.take();
                let mut studio_error = app.studio_error.take();
                // Backends editor (split-plane I2): split off `app` the same way `studio`
                // is — one App-level instance (the same form whichever Connections window
                // renders it), usable inside the closure below and restored after.
                let mut backend_editor = std::mem::take(&mut app.backend_editor);
                // DOM venue routing: resolve the selected venue's id + inst up front — used for the
                // book lookup, the per-venue order/position view (dom_tool_content), and order routing.
                let dom_venue = tv.dom.venue;
                // Route the tool's live book by kind: a DOM window reads its selected crypto venue
                // (`tv.dom.venue`); a Polymarket cockpit reads the "polymarket" venue keyed by the
                // window's token-id `symbol`. Both pull from the SAME `BookStore`. (These carry the
                // DOM values for every other kind, unused there.)
                let (tool_venue_str, tool_inst): (&str, String) =
                    if kind == workspace::WinKind::Polymarket {
                        ("polymarket", symbol.clone())
                    } else {
                        (venue_str(dom_venue), venue_inst(dom_venue, &symbol))
                    };
                // Book for the tool venue (keyed by venue_str + inst); a DOM window also requests
                // its depth stream (idempotent). `None` (no book yet) counts as stale so it never
                // reads as live. NOTE: no polymarket live feed is wired into vike-app yet, so a
                // cockpit's book stays `None`/stale until one populates the store under "polymarket".
                let (dom_book, dom_stale) = if kind == workspace::WinKind::Dom {
                    dom_depth_reqs.push((dom_venue, symbol.clone()));
                    match books.get(tool_venue_str, &tool_inst) {
                        Some((book, ts)) => (Some(book), now_ms - ts > DOM_STALE_MS),
                        None => (None, true),
                    }
                } else if kind == workspace::WinKind::Polymarket {
                    // Ensure this token's live book+trade stream (drained into `ensure_poly_book`
                    // after the loop, like `dom_depth_reqs`); a placeholder token is not
                    // subscribed (still resolving via Gamma).
                    if tool_inst != POLY_PLACEHOLDER_TOKEN {
                        poly_book_reqs.push(tool_inst.clone());
                    }
                    match books.get(tool_venue_str, &tool_inst) {
                        // Cockpit books update far less often than a crypto DOM, so the cockpit
                        // branch uses the wider `POLY_STALE_MS` (not `DOM_STALE_MS`) — a quiet but
                        // valid up/down snapshot must not read STALE.
                        Some((book, ts)) => (Some(book), now_ms - ts > POLY_STALE_MS),
                        None => (None, true),
                    }
                } else {
                    (None, false)
                };
                // LIVE if this venue has a credential-gated exec client (else PAPER) — drives the
                // DOM's ● LIVE/PAPER badge and the window title tag.
                let dom_live = live_venues.contains(tool_venue_str);
                if workspace::show_window(&ctx, w, desktop, |ui| {
                    let (acts, drag) = tool_title_bar(ui, kind, is_max);
                    act = acts;
                    // vike tool bodies are inset ~8px from the window edges (the title bar stays
                    // flush); the chart-title flush fix zeroed the window margin, so re-add it here.
                    egui::Frame::new()
                        .inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 6 })
                        .show(ui, |ui| {
                            tool_content(
                                ui,
                                kind,
                                &tool_inst,
                                tool_venue_str,
                                dom_book.as_ref(),
                                dom_stale,
                                dom_live,
                                &td,
                                &feeds,
                                &flags,
                                &logos,
                                &dsets,
                                &core_snap,
                                &mut tv,
                                display_tz,
                                &mut studio,
                                &mut studio_error,
                                &stored_tree,
                                &stored_gaps,
                                &stored_partials,
                                stored_coverage,
                                stored_loading,
                                &stored_backfill_status,
                                &opt_refresh,
                                &app.feed_statuses,
                                &app.poly_names,
                                &tool_views::BackendPicker {
                                    backends: &app.backends,
                                    active: app.active_backend.as_ref().map(|b| &b.record),
                                    available: backend_switching,
                                },
                                &mut backend_action,
                                &mut backend_editor,
                                &mut backend_registry_update,
                                &backend_settings_view,
                                &mut backend_settings_refresh,
                                &mut backend_settings_edit,
                                &mut backend_settings_write,
                                resolved_datahub_addr.as_deref(),
                            );
                        });
                    drag
                }) {
                    user_moved_any = true;
                }
                // DOM: drain this frame's click-trade intents (tagged with venue + inst so the drain
                // routes each order to that venue's engine) and keep the title reflecting the live
                // venue + Pro/Elite mode + trading mode (LIVE when credential-gated, else PAPER).
                if kind == workspace::WinKind::Dom {
                    for a in tv.dom_actions.drain(..) {
                        dom_actions.push((tool_venue_str.to_string(), tool_inst.clone(), a));
                    }
                    let mode_tag = if dom_live { "LIVE" } else { "PAPER" };
                    w.title = format!(
                        "DOM · {} · {} · {mode_tag}",
                        tv.dom.venue.label(),
                        tv.dom.mode.label()
                    );
                }
                // Polymarket cockpit: translate this frame's ladder + ticket intents into
                // `CockpitCmd`s here (where the ticket stake + live book are still in scope) for
                // the post-loop command fold. The stake sizes both the ladder limits (shares =
                // stake/price) and the ticket market buys. Buying "Down" is modeled as SELL YES
                // (−1) on the SAME token — the paired NO-token id is not wired yet (see report).
                if kind == workspace::WinKind::Polymarket {
                    let stake = tv.cockpit_ticket.size;
                    let up_px = dom_book.as_ref().and_then(|b| b.best_ask()).map(|(p, _)| p);
                    let dn_px = dom_book.as_ref().and_then(|b| b.best_bid()).map(|(p, _)| 1.0 - p);
                    for a in tv.cockpit_ladder_actions.drain(..) {
                        match a {
                            cockpit::ProbLadderAction::PlaceLimit { side, price } => {
                                let qty = if price > 0.0 { (stake / price).max(0.0) } else { 0.0 };
                                cockpit_cmds.push(CockpitCmd::Submit {
                                    token: tool_inst.clone(),
                                    side,
                                    price: Some(price),
                                    qty,
                                });
                            }
                            cockpit::ProbLadderAction::CancelOrder(coid) => {
                                cockpit_cmds.push(CockpitCmd::Cancel(coid));
                            }
                        }
                    }
                    for a in tv.cockpit_ticket_actions.drain(..) {
                        match a {
                            cockpit::TicketAction::BuyUp => {
                                let px = up_px.unwrap_or(0.0);
                                let qty = if px > 0.0 { (stake / px).max(0.0) } else { 0.0 };
                                cockpit_cmds.push(CockpitCmd::Submit {
                                    token: tool_inst.clone(),
                                    side: 1,
                                    price: None,
                                    qty,
                                });
                            }
                            cockpit::TicketAction::BuyDown => {
                                let px = dn_px.unwrap_or(0.0);
                                let qty = if px > 0.0 { (stake / px).max(0.0) } else { 0.0 };
                                cockpit_cmds.push(CockpitCmd::Submit {
                                    token: tool_inst.clone(),
                                    side: -1,
                                    price: None,
                                    qty,
                                });
                            }
                            // Size/arm are pure UI state already applied by the widget to
                            // `tv.cockpit_ticket` — nothing to route to the core.
                            cockpit::TicketAction::SetSize(_)
                            | cockpit::TicketAction::ToggleArm => {}
                        }
                    }
                }
                // Options: drain a confirmed order ticket (only set AFTER the user clicks Confirm
                // in the ticket modal — never on the raw chain click) for submit below.
                if let Some(t) = tv.opt_submit.take() {
                    opt_orders.push(t);
                }
                // Options: drain a chain working-order-marker cancel (deribit coid) for routing below.
                if let Some(coid) = tv.opt_cancel.take() {
                    opt_cancels.push(coid);
                }
                if let Some(u) = tv.news_open_url.take() {
                    ctx.open_url(egui::OpenUrl::new_tab(u));
                }
                if let Some(k) = tv.data_delete.take() {
                    let ts =
                        vike_chart::to_naive(chrono::Utc::now().timestamp_millis(), display_tz)
                            .map(|dt| dt.format("%H:%M:%S").to_string())
                            .unwrap_or_default();
                    tv.data_log.push(format!("{ts}  Stopped {k}"));
                    to_stop.push(k);
                }
                if let Some(sub) = tv.trade_submit.take() {
                    trade_orders.push(sub);
                }
                if let Some(coid) = tv.trade_cancel.take() {
                    trade_cancels.push(coid);
                }
                if let Some(m) = tv.trade_set_margin.take() {
                    trade_margins.push(m);
                }
                if tv.ds_save {
                    tv.ds_save = false;
                    let v = &tv;
                    ds_save = Some(datasets::DataSet {
                        name: v.ds_name.trim().to_string(),
                        symbols: datasets::parse_symbols(&v.ds_symbols_text),
                        provider: v.ds_provider.clone(),
                        interval: v.ds_interval.clone(),
                        benchmark: v.ds_benchmark.trim().to_string(),
                        user: true, // corrected in the apply (preserve an existing flag)
                    });
                }
                if let Some(n) = tv.ds_delete.take() {
                    ds_del = Some(n);
                }
                if let Some(s) = tv.ds_test.take() {
                    ds_open = Some(s);
                }
                // Data Manager "Stored" view (Task 3): collect this window's OUT actions —
                // applied after the loop below, once `app` is fully borrowable again (same
                // deferred-mutation pattern as `ds_save`/`ds_del`/`ds_open` above).
                if tv.stored_refresh {
                    tv.stored_refresh = false;
                    stored_refresh_requested = true;
                }
                // Polymarket proxy box: seed ONCE from the value the credential store actually
                // holds, then leave it alone — re-seeding every frame would overwrite whatever
                // the operator is mid-way through typing. A stored `none`/`direct` shows as an
                // empty box (`proxy_display`), so "no proxy" looks like no proxy.
                if !tv.stored_proxy_loaded {
                    tv.stored_proxy_loaded = true;
                    tv.stored_proxy.buf = vike_app_core::tool_views::seed_polymarket_proxy_box(
                        &workspace_credentials(),
                    );
                }
                if let Some(v) = tv.stored_proxy_save.take() {
                    // A free function borrowing no `app`, so unlike the `stored_*` actions
                    // around it this needs no deferral to after the window loop.
                    vike_app_core::tool_views::save_polymarket_proxy(
                        &v,
                        credential_home().write_ctx(vike_model::now_ms()),
                    );
                }
                if let Some(key) = tv.stored_delete.take() {
                    stored_deletes.push(key);
                }
                // Grid v2 multi-select bulk delete: same drain target as the single-row
                // delete above — one `delete_series` call per selected key.
                for k in tv.stored_bulk_delete.drain(..) {
                    stored_deletes.push((k.venue, k.symbol, k.kind, k.interval));
                }
                // dm-bulk-backfill: same drain-into-a-Vec shape as bulk delete above, spawned
                // after the loop (see `stored_backfills`' declaration).
                stored_backfills.append(&mut tv.stored_backfill);
                if let Some(open) = tv.stored_open.take() {
                    stored_opens.push(open);
                }
                app.tool_views.insert(wid, tv);
                app.studio = studio;
                app.studio_error = studio_error;
                app.backend_editor = backend_editor;
            }

            // common to chart + tool windows
            if act.close {
                w.open = false;
            }
            if act.minimize {
                w.minimized = true;
            }
            if act.toggle_max {
                if w.maximized {
                    workspace::unmaximize(w);
                } else {
                    workspace::maximize(w, desktop);
                }
            }
        }
    }
    if user_moved_any {
        // user hand-placed a window — forget the tiling mode so an app-maximize
        // leaves the manual layout alone (respects the no-auto-arrange rule).
        app.last_arrange = None;
        app.retile_frames = 0;
    }

    // process clones + on-demand feeds (`app` fully borrowable again)
    for wi in to_clone {
        // The source read stays a scoped borrow: `apply_spawn` takes `&mut App`, and pushing
        // only APPENDS, so `wi` stays valid for the remaining clones exactly as before.
        //
        // ⚠ `src.venue` is read here and then DROPPED by the planner — deliberately, and this
        // is not a widening of the read: a clone has ALWAYS opened on `DEFAULT_VENUE` and
        // still does, byte for byte. Carrying it puts the whole of the (pinned) defect inside
        // `crates/vike-app-core/src/window_spawn.rs`'s `SpawnRequest`, so fixing it later is
        // one line in a file the merge gate compiles rather than another edit to this one.
        let (venue, symbol, interval) = {
            let src = &app.wins[wi];
            (src.venue.clone(), src.symbol.clone(), src.interval.clone())
        };
        let req = window_spawn::SpawnRequest::CloneWindow { venue, symbol, interval };
        app.apply_spawn(&ctx, req);
    }
    for (venue, s, iv, ac) in to_ensure {
        app.ensure_feed_on(&ctx, &venue, &s, &iv, ac);
    }
    // Fold every finished backfill worker's exit report BEFORE the `of_wanted` block below
    // calls `maybe_spawn_backfill`, so a re-walk whose cooldown elapsed can be claimed on this
    // same frame rather than the next one. Non-blocking; empty on all but a handful of frames
    // in a session (one report per walk, ever). The whole decision — retry, give up, or forget
    // — is `BackfillRetries::note_report`, in the CI-gated `vike-app-core`.
    for report in app.bf_done_rx.try_iter() {
        app.bf_retries.note_report(&report);
    }
    // SP2 orderflow (Task 7): (idempotently) subscribe the trade feed + register this
    // chart's aggregator for every window that wants orderflow this frame (collected
    // above as `of_wanted` — see its doc for why this can't happen inside the window loop).
    for (venue, symbol, key, tick_size) in of_wanted {
        // Venue-aware (Task: venue-aware orderflow): subscribe the WINDOW's own venue tape, so
        // any venue with a `subscribe_trades` feed (OKX/Bybit/…) drives CVD/footprint, not just
        // Binance.
        app.ensure_trade_feed_on(&venue, &symbol);
        // SP2 review finding A: when the user hasn't pinned `of_tick_size`, derive an adaptive
        // default from the chart's current price (~2 bps, nice-stepped) instead of the old fixed
        // `$1` fallback (`OrderflowAgg::new`'s `<= 0.0 → 1.0`), which was too fine on BTC and
        // useless on cheap coins. `px = 0` (no bars yet) → `nice_orderflow_tick(0.0) = 1.0`, an
        // acceptable fallback since the agg is created lazily once a chart exists (bars usual).
        let px = app.charts.get(&key).and_then(|c| c.bars.last()).map(|b| b.c).unwrap_or(0.0);
        let tick = tick_size.unwrap_or_else(|| orderflow::nice_orderflow_tick(px));
        app.of_aggs
            .entry(key.clone())
            .or_insert_with(|| (venue.clone(), symbol.clone(), orderflow::OrderflowAgg::new(tick)));
        // SP3 Task 3: background aggTrades backfill — Binance-only BY DESIGN. Only Binance
        // exposes the aggTrades REST paging `maybe_spawn_backfill` relies on, so a non-Binance
        // orderflow chart is live-only (no historical CVD), mirroring how the trade feeds
        // themselves are live-only. Idempotent via `bf_spawned` (see `maybe_spawn_backfill`'s
        // doc; `of_backfill_hours <= 0.0` is the master off-switch).
        if venue == DEFAULT_VENUE {
            app.maybe_spawn_backfill(&symbol, &key);
        }
    }
    // DOM windows: start (idempotently) the depth stream for each one's selected venue.
    for (venue, canonical) in dom_depth_reqs {
        app.ensure_depth(venue, &canonical);
    }
    // Polymarket cockpit windows: start (idempotently) each open window's token book+trade
    // stream, so its book flows into the shared BookStore under venue "polymarket".
    for token in poly_book_reqs {
        app.ensure_poly_book(&token);
    }
    // Data-manager Delete: drop the render series, stop syncing it from the core snapshot, AND
    // stop the live feed thread itself via per-key unsubscribe. That teardown is NOT spelled here
    // any more — `vike_app_core::feed_lifecycle::stop_series` is the one implementation, shared
    // with `reap_orphaned_feeds`'s per-orphan teardown below. The hand copy that used to sit here
    // had drifted on exactly the line that matters: it routed every unsubscribe to
    // `feeds["binance"]` under a comment claiming both key kinds were binance-only, which stopped
    // being true when `series_key` began namespacing non-default venues — so deleting an
    // `okx:…`/`bybit:…` row freed `spawned` while silently discarding the subscription id, leaking
    // the socket for the life of the process. See that function's doc for the post-mortem; its
    // tests run in the merge gate, which nothing in this file does.
    let (mut f, mut s) = app.feed_and_series_slots();
    for k in to_stop {
        feed_lifecycle::stop_series(&mut f, &mut s, &k);
        // Feed-leak fix: also drop this key's tick/vol (`aggs`) or orderflow (`of_aggs`)
        // aggregator so its shared trade tape becomes reapable. The Data-manager delete does
        // NOT remove the window from `app.wins` (it stays with `open=false`), so its key would
        // otherwise still count as live and `reap_orphaned_feeds`'s (2a) would keep the
        // aggregator — hence the explicit removal here. `reap_orphaned_feeds` runs right below
        // and stops the underlying `subscribe_trades` feed iff no OTHER aggregator on the same
        // (venue, symbol) remains (a shared feed is never stopped out from under a live chart).
        // Reached through the `SeriesSlots` bundle rather than through `app`, which the two
        // bundles hold mutably borrowed for this loop; they are the same two fields.
        s.aggs.remove(&k);
        s.of_aggs.remove(&k);
    }
    // C2 tidy (FIX 3): sweep any feed no window references anymore (an orphaned Compare
    // symbol, or the last window on that symbol's primary changing away from it) — see
    // `reap_orphaned_feeds`'s doc. Placed right after the manual Data-manager teardown
    // above since it's the same kind of cleanup, just automatic every frame.
    app.reap_orphaned_feeds();
    // Trade window: forward paper order tickets to the core command lane. Rejection
    // is surfaced in the CoreSnapshot (rejected_commands); orders paper-fill on the
    // next closed 1m bar via PaperExecutionClient.
    // Route order/session commands to the LOCAL core when one exists, else to a remote
    // Scope::Control channel (a `--observe` observer with control enabled). `None`/`None` — a
    // read-only observer, or one still connecting — drops the whole block, a read-only no-op
    // exactly as before this path existed. Every local-core path is byte-identical
    // (`Dispatch::Local` forwards straight to `core.try_command`).
    // Field-path spelling of `app.remote_ctrl()` (disjoint borrows): the whole-`app` borrow
    // a method call takes would pin `app.next_win_n`'s assignment below for `dispatch`'s
    // lifetime.
    let remote_ctrl = app.active_backend.as_ref().and_then(|b| b.ctrl.as_ref());
    let dispatch = match (&app.core, remote_ctrl) {
        (Some(core), _) => Some(Dispatch::Local(core)),
        (None, Some(ctrl)) => Some(Dispatch::Remote(ctrl)),
        (None, None) => None,
    };
    if let Some(dispatch) = dispatch {
        // Local preview caps for the order-entry safety layer, from the POLICY ceiling
        // (`max_notional_per_order` in `<vike home>/policy.toml`, resolved ONCE in `main` —
        // see `POLICY_ORDER_LIMITS`), else permissive. A LOCAL guard in front of the venue
        // RiskGate — an order that fails it is dropped with a warn instead of round-tripping
        // to a venue that would reject it.
        //
        // ⚠ This was a PER-FRAME `env::var("VIKE_MAX_ORDER_NOTIONAL")`. Phase 5 of the
        // settings-unification design removed that variable outright: a ceiling any exported
        // variable can raise is not a ceiling — and re-reading it every frame meant the limit
        // could also move under a running process. It is resolved once at startup now, and a
        // process that still finds the old variable set REFUSES TO BOOT (see `main`).
        let order_limits = *POLICY_ORDER_LIMITS.get_or_init(order_entry::OrderLimits::default);
        // The DECISION — which drained intent becomes which `vike_exec::Command`, and which the
        // local preview refuses — is `vike_app_core::order_dispatch::plan_dispatch`, a PURE
        // function; this file keeps only the I/O (drain in, `Dispatch::send` out, warn the
        // refusals).
        //
        // WHY IT MOVED. The block that used to sit here validated only THREE of its five submit
        // paths (the DOM ladder `Place`, the Polymarket cockpit `Submit`, the deribit options
        // confirm ticket). The **Trade window** — the manual order-entry panel a human types
        // into — called neither `validate` nor `validate_with_multiplier`, and neither did its
        // TP+SL bracket sub-path nor the DOM Close/Reverse exit, so the one MANUAL path had no
        // local notional cap at all. Fixing that here would have been exactly as unverified as
        // the bug: nothing compiles this file (`justfile`'s `ci_crates` omits vike-app, so
        // `just windows-check` skips it too, and `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI`
        // lists it) — the same reason `order_entry`'s notional-MULTIPLIER bug shipped. So the
        // decision now lives in a crate CI compiles and tests, behind a structural chokepoint
        // plus an exhaustive `SubmitSource` roster; see that module's doc for the gate.
        //
        // The Trade path also routes to `core_snap.venue`/`core_snap.symbol` where this file
        // hardcoded `("binance", "BTCUSDT")`. It WAS byte-identical on the old local FAT build
        // (binance/BTCUSDT was the primary `vike_run::build_node` mounted), and CORRECT for a
        // control-enabled `--observe` client, whose remote daemon's primary is
        // polymarket/hyperliquid — where the Trade card rendered `snap.symbol` while the BUY button
        // submitted binance/BTCUSDT. ⚠ Only the second case exists now: there is no local mount, so
        // the snapshot's own primary is the only answer there has ever been a right one.
        let plan = vike_app_core::order_dispatch::plan_dispatch(
            vike_app_core::order_dispatch::DispatchInputs {
                trade_orders,
                trade_cancels,
                trade_margins,
                dom_actions,
                cockpit_cmds,
                opt_orders,
                opt_cancels,
            },
            &order_limits,
            &core_snap,
            app.next_win_n,
            app.shot_n,
        );
        app.next_win_n = plan.next_win_n;
        for reject in &plan.rejects {
            tracing::warn!(
                submit_source = reject.source.label(),
                venue = %reject.venue,
                symbol = %reject.symbol,
                "order rejected by local preview: {}",
                reject.reason
            );
        }
        for cmd in plan.commands {
            dispatch.send(cmd);
        }
        // QA (VIKE_DOM_TESTORDER): inject one buy-limit at the DOM venue's best bid — a visual
        // end-to-end proof that an order routes to the SELECTED venue's engine (it appears in
        // `snap.orders` tagged with that venue + renders as a working-order marker). Fired late
        // (near the self-shot frame) so the resting price is still inside the visible ladder.
        // SAFETY (control-enabled observer): hard-disable this QA hook whenever a remote
        // Scope::Control channel exists. `app.live_venues` is ALWAYS EMPTY in observe mode, so
        // the paper-only guard below (`app.live_venues.contains(vstr)`) would NOT catch a
        // control-enabled observer wired to a remote daemon with REAL venues — it would inject a
        // real order with no catch. `app.remote_ctrl().is_none()` is that missing guard.
        if app.dom_test_pending && app.shot_n >= 320 && app.remote_ctrl().is_none() {
            let target =
                app.wins.iter().find(|w| w.kind == workspace::WinKind::Dom && w.open).map(|w| {
                    let venue = app
                        .tool_views
                        .get(&w.id)
                        .map(|tv| tv.dom.venue)
                        .unwrap_or(dom::DomVenue::Binance);
                    (venue, w.symbol.clone())
                });
            if let Some((venue, canonical)) = target {
                let vstr = venue_str(venue);
                let inst = venue_inst(venue, &canonical);
                // SAFETY: this QA hook injects a REAL order — never let it fire against a
                // credential-gated LIVE venue (paper venues only).
                if app.live_venues.contains(vstr) {
                    app.dom_test_pending = false;
                } else if let Some((book, _)) = app.books.get(vstr, &inst)
                    && let Some((bid, _)) = book.best_bid()
                {
                    let req = vike_model::OrderRequest {
                        client_order_id: format!("dom-test-{}", app.shot_n),
                        venue: vstr.to_string(),
                        symbol: inst,
                        side: 1,
                        qty: 0.01,
                        order_type: "limit".to_string(),
                        price: Some(bid),
                        ..Default::default()
                    };
                    // The QA hook is a submit path like any other, so it takes the SAME
                    // local preview the UI paths take (it is the one order write that
                    // never passes through `order_dispatch`, because its price comes from
                    // `app.books`, which the plan does not see). Clear the pending flag
                    // on BOTH arms — a refused injection must not retry every frame.
                    let mult = core_snap.multiplier_of(&req.venue, &req.symbol);
                    match order_entry::validate_with_multiplier(&req, &order_limits, mult) {
                        Ok(()) => dispatch.send(vike_exec::Command::Order(
                            vike_exec::OrderIntent::Submit(Box::new(req)),
                        )),
                        Err(reject) => tracing::warn!(
                            venue = %req.venue, symbol = %req.symbol,
                            "DOM test order rejected by local preview: {reject}"
                        ),
                    }
                    app.dom_test_pending = false;
                }
            }
        }
        // QA (VIKE_TRADE_SEED): seed the Trade panel with a working order and an open position.
        // `.trader/shots/manifest.json`'s `pipeline-3-golive.png` asks for both by name, and until
        // this existed neither was reachable: the only order-injecting hook was the DOM one above,
        // which needs a DOM window, and a DOM window comes from `VIKE_TOOL`, whose planner sets
        // `close_existing` and closes the Trade window. Mutually exclusive by construction — which
        // is why this is its own hook rather than a widening of that one.
        //
        // ⚠ THE WHOLE DECISION, GUARDS INCLUDED, is `vike_app_core::capture_seed`'s pure
        // `plan_trade_seed_commands` — deliberately NOT here, unlike the DOM injection above whose
        // paper-only and remote-control guards sit in this file and are therefore executed by no
        // test in the workspace (`xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI` names `vike-app`).
        // These two guards are the whole of what keeps a marketing screenshot off a real account,
        // so they live where CI runs them. This block is the I/O: read the price, send, warn.
        //
        // ⚠ The outer `if` is REDUNDANT with `SeedInputs::armed` on purpose, and it is not a
        // belt-and-braces guard — it is what keeps an UNSET knob byte-identical. Gathering the
        // inputs costs a `format!`-built series key and a map lookup, and this runs every frame of
        // every ordinary desktop session; paying that sixty times a second for a knob nobody set
        // is a behaviour change, small but real. The field stays because the pure function is the
        // authority on the whole ladder and its tests drive that arm.
        if app.trade_seed_pending {
            let seed = vike_app_core::capture_seed::plan_trade_seed_commands(
                &vike_app_core::capture_seed::SeedInputs {
                    armed: app.trade_seed_pending,
                    frame: app.shot_n,
                    remote_control: app.remote_ctrl().is_some(),
                    venue_is_live: app.live_venues.contains(workspace::DEFAULT_VENUE),
                    last_close: vike_app_core::capture_seed::fill_clock_close(&app.charts),
                    venue: workspace::DEFAULT_VENUE,
                },
                &order_limits,
                &core_snap,
            );
            for w in &seed.warnings {
                tracing::warn!("{w}");
            }
            for cmd in seed.commands {
                dispatch.send(cmd);
            }
            if seed.spend {
                app.trade_seed_pending = false;
            }
        }
    }
    // QA (VIKE_CHART_DRAW): write the capture drawings into every chart series that has bars.
    //
    // OUTSIDE the `dispatch` block above on purpose: a drawing is not an order, it needs no core,
    // and gating it on one would make the chart pose unreachable in an `--observe` client that has
    // no local core at all. The flag is spent on the first frame that draws ANYTHING: the drawings
    // are derived from a snapshot of the series, so a per-frame rewrite would make them crawl as
    // new bars arrive, while spending it on an earlier empty-series frame would draw nothing at
    // all.
    //
    // ⚠ An explicit loop with `|=`, NOT `Iterator::any` — and clippy will suggest `any` again for
    // any expression form of this (`unnecessary_fold` fired on the `fold` that was here first).
    // `any` SHORT-CIRCUITS: it stops at the first chart that draws, so on a multi-chart layout
    // every later chart silently keeps an empty overlay map. `|=` on a `bool` is a plain
    // bitwise-or-assign with no short-circuit, so every chart is visited and the flag still ends
    // up "did anything draw".
    if app.chart_draw_pending {
        let mut drawn = false;
        for state in app.charts.values_mut() {
            drawn |= vike_app_core::capture_seed::apply_drawings(state);
        }
        app.chart_draw_pending = !drawn;
    }
    // DataSets (Symbols tab) mutations — deferred here so `app` is not borrowed by the window loop
    if let Some(mut d) = ds_save
        && !d.name.is_empty()
    {
        d.user = app.datasets.get(&d.name).map(|x| x.user).unwrap_or(true);
        app.datasets.upsert(d);
    }
    if let Some(n) = ds_del {
        app.datasets.delete(&n);
    }
    if let Some(sym) = ds_open {
        // "Test symbol" → open a chart window for it (the clone's analog of vike's backtest test)
        app.apply_spawn(&ctx, window_spawn::SpawnRequest::TestSymbol { symbol: sym });
    }
    // Data Manager "Stored" view (Task 3): per-series Delete. ⚠ It deletes NOTHING now — see the
    // tombstone inside the loop. What survives is the mode gate's refusal and the forced reload,
    // so a stale click is logged rather than acted on.
    if !stored_deletes.is_empty() {
        // The mode gate (the #1378 seam close): `delete_series` acts on the LOCAL store, and
        // in remote mode that is not the store the grid is showing — the UI grays Delete
        // (`stored_mode`'s reason as hover text) so nothing should arrive here; if a stale
        // click does, refuse it loudly rather than damaging the wrong store.
        let stored_delete_gate = vike_app_core::stored_mode::stored_mode(
            app.resolved_datahub_addr().as_deref(),
            // Only `delete_unavailable` is read here, and the coverage answer cannot change it.
            vike_app_core::stored_mode::RemoteCoverage::Unknown,
        )
        .delete_unavailable;
        for (venue, symbol, kind, interval) in stored_deletes {
            if let Some(reason) = stored_delete_gate {
                tracing::warn!("Stored delete {venue}:{symbol} ({kind}) refused: {reason}");
                continue;
            }
            // ⚠ TOMBSTONE — the DELETE stood here. Under `fat` it opened a FRESH
            // `open_local_hist_store()` handle, built a `vike_data::SeriesId::per_symbol` and
            // called `store.delete_series(&id)` — the irreversible act the modal above had already
            // confirmed. There is no local store engine in this binary any more (rulings 1 and 2
            // took the local data plane), and `delete_series` is not a `HistStore` trait verb, so
            // there is nothing to delete THROUGH: the gate above is the only outcome the loop now
            // has, and the grid only ever loads in remote mode anyway. The binding keeps the arm
            // consuming its tuple.
            let _ = (venue, symbol, kind, interval);
            stored_refresh_requested = true;
        }
    }
    // Data Manager "Stored" view (Task 3): Open in chart → point/spawn a chart window at this
    // (venue, symbol, interval). The "1m" default for a tick-only series (quote/trade carries
    // no chart timeframe of its own), the asset-class-free spot routing and the venue override
    // + retitle all moved into the planner —
    // `crates/vike-app-core/src/window_spawn.rs`'s `SpawnRequest` carries the argument for
    // each, and this is the ONE spawn site that overrides a window's venue at all.
    for (venue, symbol, interval) in stored_opens {
        let req = window_spawn::SpawnRequest::StoredOpen { venue, symbol, interval };
        app.apply_spawn(&ctx, req);
    }
    if stored_refresh_requested && !app.stored_loading {
        app.refresh_stored(&ctx);
    }
    // dm-bulk-backfill: spawn the background kline backfill for this frame's bulk
    // Backfill/Update selection, if any (see `App::maybe_spawn_stored_backfill`'s doc; a
    // no-op if a run is already in flight or the drained selection was empty).
    if !stored_backfills.is_empty() {
        app.maybe_spawn_stored_backfill(&ctx, stored_backfills);
    }
    // Connections backend picker (split-plane B1): apply this frame's Connect/Disconnect
    // click — the backend-session-clear switch routine lives in `vike_app_core::backend_conn`.
    if let Some(action) = backend_action {
        // The registry's `active` pointer FOLLOWS the picker, so the node picked here is the one a
        // bare launch dials next (`backend_registry::active_addr`, read by `main`'s
        // `observe_addr_from_registry`). Computed BEFORE the switch, because the decision is about
        // the click and `apply_backend_action` consumes the action.
        //
        // ⚠ Without this the pointer had no writer at all — `backend_editor` only retargets it on a
        // rename and clears it on a delete — so "configure once, launch bare afterwards" was never
        // true: a record added and connected in this tool stayed `"active": null` on disk and the
        // next launch silently observed `DEFAULT_OBSERVE_ADDR` instead.
        let next_active =
            vike_app_core::backend_conn::active_after(&action, &app.backends.backends);
        app.apply_backend_action(&ctx, action);
        if app.backends.active != next_active {
            app.backends.active = next_active;
            if let Err(e) = vike_app_core::backend_registry::save(&app.backends) {
                tracing::warn!("backends.json active-pointer save failed: {e}");
            }
        }
    }
    // Backends editor (split-plane I2): apply this frame's confirmed Add/Edit/Delete. The
    // ordering contract is `apply_registry_update`'s — a delete of the ACTIVE backend runs
    // the DISCONNECT (the same switch machinery as a picker click, backend-session clear and
    // all) BEFORE the save, so no conn ever dangles on a record the registry no longer
    // holds. Assign-and-save in one drain: disk and memory never disagree.
    if let Some(upd) = backend_registry_update {
        let new_file = vike_app_core::backend_editor::apply_registry_update(
            upd,
            || {
                app.apply_backend_action(
                    &ctx,
                    vike_app_core::backend_conn::BackendAction::Disconnect,
                )
            },
            |file| {
                if let Err(e) = vike_app_core::backend_registry::save(file) {
                    tracing::warn!("backends.json save failed: {e}");
                }
            },
        );
        app.backends = new_file;
    }
    // Backend-settings section (split-plane REQ-7, read half): start this frame's requested
    // fetch — the section's own auto-fetch (first sight of a backend), a Refresh click, or
    // the post-save refetch the write drain above requested.
    if backend_settings_refresh {
        app.spawn_backend_settings_fetch(&ctx);
    }
    // …and the WRITE half: the frame-local edit flow back onto `app`, then this frame's
    // accepted Save (if any) onto its worker thread.
    app.backend_settings_edit = backend_settings_edit;
    if let Some(req) = backend_settings_write {
        app.spawn_backend_settings_write(&ctx, req);
    }
}

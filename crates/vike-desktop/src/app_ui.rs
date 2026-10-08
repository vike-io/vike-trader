//! `App::ui`'s per-frame body — everything EXCEPT the two spots that must stay physically in
//! `main.rs`.
//!
//! ⚠ **Why this file is a split rather than a move.** `ui` is one ~2100-line frame body, and two
//! small blocks inside it read the process environment directly:
//!
//! 1. the headless self-screenshot capture (`VIKE_SHOT` / `VIKE_APPMAX` / `VIKE_SHOT_FRAME`), and
//! 2. the first-frame window arrangement's seven-knob `initial_arrange::ArrangeEnv` construction
//!    (`VIKE_ARRANGE` / `VIKE_TOOL` / `VIKE_TOOLS` / `VIKE_CAL_PAGE` / `VIKE_MAX` / `VIKE_MIN` /
//!    `VIKE_POLY_COCKPIT_TOKEN`).
//!
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs` classifies every `env::var` read by FILE PATH:
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
//!
//! [`draw_chrome`] and [`draw_windows`] are themselves short orchestrators over named phase
//! functions (one per visual region or drain), in the original statement order; the locals that
//! used to cross those regions live in [`WindowFrame`] (the frame's reads) and [`WindowOut`] (the
//! collect-then-apply accumulators).

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
    stored_tree: Arc<Vec<vike_data_manager::model::VenueNode>>,
    stored_partials: Arc<vike_data_manager::PartialDayMap>,
    stored_gaps: Arc<vike_data_manager::GapMap>,
    /// What the last background load learned about the remote store's coverage verb (spec §6-Q2).
    stored_coverage: vike_app_core::data::stored_mode::RemoteCoverage,
    /// `true` while a background `refresh_stored` load is in flight.
    stored_loading: bool,
    /// `Some(reason)` → the last load could not READ the store (see `App::stored_error`).
    stored_load_error: Option<String>,
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
    // Adopt the background-fetched symbol catalog. ⚠ NOT a single send and this comment said it
    // was: `CatalogRefresh` publishes the WHOLE merged universe on every adopted refresh, and with
    // the server route wired there are up to nine venues a session can press. So DRAIN to the
    // newest — each send supersedes the one before it, and a `try_recv` taken once per frame would
    // otherwise adopt a backlog one frame at a time while a fresher universe sat behind it.
    let mut newest_catalog = None;
    while let Ok(list) = app.catalog_rx.try_recv() {
        newest_catalog = Some(list);
    }
    if let Some(list) = newest_catalog {
        app.symbols_catalog = Arc::new(list);
    }
    // Data Manager "Stored" view: adopt the latest background inventory load (tree + gap
    // map), if one landed this frame (see `refresh_stored`). `try_recv` drains to the newest
    // send — irrelevant here since only one load is ever in flight at a time.
    if let Ok(load) = app.stored_rx.try_recv() {
        app.stored_tree = Arc::new(load.tree);
        app.stored_gaps = Arc::new(load.gaps);
        app.stored_partials = Arc::new(load.partials);
        // What this load negotiated about the coverage verb — the render-time input to the
        // Partial column's three states (spec §6-Q2).
        app.stored_coverage = load.coverage;
        // ...and its history-channels answer, for the HISTORY column and the Backfill's floors.
        app.stored_history = load.history.map(Arc::new);
        // `None` on a healthy load, which is also how a retry CLEARS a previous reason. The grid
        // renders this instead of an empty tree, so "unreachable" stops looking like "empty".
        app.stored_error = load.error;
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
    let stored_load_error = app.stored_error.clone();
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
    // — vike-desktop has no separate focus-tracking field of its own to prefer.
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
            stored_load_error,
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
///
/// The body is six named phases in the original order — [`draw_caption`], [`apply_menu_actions`],
/// [`draw_save_layout_dialog`], [`draw_min_rail`], [`draw_status_bar`], then the `CentralPanel`
/// written inline below, which is the one statement the rest of the frame hangs on.
pub(crate) fn draw_chrome(app: &mut App, ui: &mut egui::Ui, ctx: egui::Context) {
    let menu = draw_caption(app, ui, &ctx);
    apply_menu_actions(app, &ctx, menu);
    draw_save_layout_dialog(app, &ctx);
    draw_min_rail(app, ui);
    draw_status_bar(app, ui);

    // --- central desktop: the window arena ---
    // Frame::NONE (no 8px inset) so the tiled layout's outer margin == the 2px inter-tile
    // gap (the central inset would otherwise make the outer padding larger than the gaps).
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
        app.desktop = ui.max_rect();
    });
}

/// Chrome phase 1 — the frameless caption bar, then the two things its result can ask for at once:
/// a submitted palette entry and a launcher / Settings window open. Returns the menu's chosen
/// actions for [`apply_menu_actions`] (the caption's other two outputs are consumed here).
fn draw_caption(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context) -> workspace::MenuResult {
    // --- frameless main caption: mark · File/Window/Help · palette · launchers · ─ □ ✕ ---
    // Named layouts for the File → Load/Delete submenus. Recomputed each frame (a read_dir of a
    // small dir is negligible, and only the entries drawn while the menu is open matter) so an
    // externally added/removed layout file is always reflected without a manual refresh.
    let layouts = workspace::persist::list_layouts();
    let launchers: Vec<vike_app_core::ui::caption::Launcher<'_>> = LAUNCHERS
        .iter()
        .map(|&(name, _, kind)| vike_app_core::ui::caption::Launcher {
            name,
            texture: app.launcher_icons.get(name),
            kind,
        })
        .collect();
    let vike_app_core::ui::caption::CaptionActions { menu, mut open_kind, palette_submit } =
        vike_app_core::ui::caption::caption_bar(
            ui,
            &vike_app_core::ui::caption::CaptionInputs {
                display_tz: app.display_tz,
                layouts: &layouts,
                gpu_ok: app.gpu_ok,
                launchers: &launchers,
            },
            &mut app.gpu_render,
            &mut app.palette,
        );
    if palette_submit {
        // The trim / upper-case / `USDT` quote-currency ladder AND the empty-entry refusal
        // (which must spawn nothing and burn no window id — the counter is the `egui::Id`
        // seed) moved into the planner with the rest of the spawn; the buffer is still cleared
        // on EVERY submit, blank or not, exactly as before.
        let raw = app.palette.clone();
        app.palette.clear();
        app.apply_spawn(ctx, window_spawn::SpawnRequest::Palette { raw });
    }
    if menu.open_settings {
        open_kind = Some(workspace::WinKind::Settings);
    }
    if let Some(k) = open_kind {
        // The four-way launcher branch (chart / DOM / cockpit / generic tool) is one
        // exhaustive `match` in the planner now, so a NEW `WinKind` fails to compile there
        // instead of silently inheriting the generic tool geometry the old `else` gave it.
        //
        // The cockpit seed is the ONE environment read this arm makes, and it is still made
        // ONLY for the cockpit kind — so no other launcher gained an `env::var` when the
        // branch moved, and `VIKE_POLY_COCKPIT_TOKEN` keeps its single `vike-desktop` /
        // `Layer::Binary` row in `crates/vike-ops/src/settings.rs`'s `SETTINGS`.
        let poly_seed_token = if k == workspace::WinKind::Polymarket {
            poly_cockpit_seed_token()
        } else {
            String::new()
        };
        // No `data_dest_seed` here: this arm serves every launcher/Window-menu row uniformly by
        // KIND, and two rows share `WinKind::Data` ("data" and "connections") with no way to tell
        // which one was clicked from `k` alone — see the "connections" row's own comment in
        // `main.rs`'s `LAUNCHERS`.
        let req =
            window_spawn::SpawnRequest::Kind { kind: k, poly_seed_token, data_dest_seed: None };
        app.apply_spawn(ctx, req);
    }
    menu
}

/// Chrome phase 2 — every action the menu bar emitted this frame: arrange, rail toggle, timezone,
/// new window, workspace save/load, named-layout save-as / load / delete, quit, and the chart
/// export (which [`drive_chart_export`] carries across frames).
fn apply_menu_actions(app: &mut App, ctx: &egui::Context, menu: workspace::MenuResult) {
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
/// asked for one, and PNG-encode it on the frame the readback event arrives. Called every frame,
/// because the event lands a frame or two after the request.
fn drive_chart_export(app: &mut App, ctx: &egui::Context, export_chart: bool) {
    // File -> Export chart image…: reuse the VIKE_SHOT framebuffer-readback path — grab
    // egui's OWN wgpu framebuffer (the only capture that survives the flip-model swapchain;
    // an OS/GDI grab returns white). The menu action requests a screenshot this frame; the
    // readback event arrives a frame or two later (below), where we PNG-encode it. Guarded so
    // the interactive export never fights the VIKE_SHOT self-shot (that block owns the event
    // and exits under its own env var, so the two are mutually exclusive in practice anyway).
    if export_chart {
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
}

/// Chrome phase 3 — the "Save layout as…" naming modal, while `app.layout_name_prompt` is up.
fn draw_save_layout_dialog(app: &mut App, ctx: &egui::Context) {
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
        }
        if choice.is_some() {
            app.layout_name_prompt = false;
            app.layout_name_input.clear();
        }
    }
}

/// Chrome phase 4 — the left rail of minimized-window tabs, shown only while one is minimized.
fn draw_min_rail(app: &mut App, ui: &mut egui::Ui) {
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
fn draw_status_bar(app: &mut App, ui: &mut egui::Ui) {
    // --- bottom status bar (vike-python-style: connection + workspace info) ---
    // A fixed-height strip pinned to the app's bottom edge, the egui analog of the PySide
    // status bar. It also fills what used to be dead space below the maximized chart window
    // (the window's own bottom frame inset never reached the desktop edge), so "empty space
    // on maximize" now reads as an intentional status strip. Added BEFORE the CentralPanel so
    // `desktop` (the window arena) excludes it.
    //
    // ⚠ That claim is TRUE and was VERIFIED, and it is still not the whole answer to "why is my
    // tool window under the status bar": `egui-0.36.1/src/containers/panel.rs`'s `show_inside_dyn`
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

/// Segment 3 — everything after island 2 (the first-frame `ArrangeEnv` arrangement).
///
/// The window ARENA and its tail: the maximize-edge re-tile, the per-window loop (chart windows'
/// title bar + `chart::draw`, tool windows' `tool_content`, and every action harvested back onto
/// `WinState`), then the whole collect-then-apply tail the loop's `&mut app.wins` borrow forces —
/// clones, feed/depth/book ensures, orderflow registration, feed teardown + reap, the order
/// dispatch lane, DataSet mutations, the Stored view's delete/open/refresh/backfill, the backend
/// picker / editor / settings drains, and the node's directory fetch.
///
/// The body is an orchestrator over named phases, in the original statement order:
/// [`retile_on_maximize`], [`window_frame`] (every read the loop needs, snapshotted once), the
/// window loop ([`draw_chart_window`] / [`draw_tool_window`] / [`apply_window_controls`] per
/// window), then the tail — one function per drain, each named for what it applies.
///
/// ⚠ **`app.wins` is TAKEN OUT of `app` for the loop and put back right after it.** The loop used
/// to hold `app.wins.iter_mut()` and so could reach only the fields it split off by hand (`charts`,
/// `of_aggs`, `indicator_favs`, `appearance`, …); a phase function cannot do that piecemeal without
/// a bundle of a dozen borrows, and taking the vector instead hands each phase the whole `&mut App`
/// with the same field-level borrows the loop already took. Nothing in the loop reads `app.wins`
/// (each phase reads its own `w`), so the empty vector is never observed.
///
/// `snap` is the frame's [`FrameSnapshot`], read through `snap.<field>` where the original body had
/// destructured it into locals.
pub(crate) fn draw_windows(app: &mut App, ctx: egui::Context, snap: FrameSnapshot) {
    retile_on_maximize(app, &ctx);

    // --- floating chart + tool windows ---
    let mut out = WindowOut::default();
    let fr = window_frame(app, &mut out);
    let books = std::sync::Arc::clone(&app.books); // read live L2 books inside the loop
    let mut wins = std::mem::take(&mut app.wins);
    for (wi, w) in wins.iter_mut().enumerate() {
        if fr.maxed_idx.is_some_and(|mi| mi != wi) {
            continue; // another window is maximized — hide this one
        }
        if !w.open || w.minimized {
            continue;
        }
        let act = if w.kind == workspace::WinKind::Chart {
            draw_chart_window(app, &ctx, w, wi, &fr, &snap, &mut out)
        } else {
            draw_tool_window(app, &ctx, w, &fr, &snap, &books, &mut out)
        };
        // common to chart + tool windows
        apply_window_controls(w, &act, fr.desktop);
    }
    app.wins = wins;
    if out.user_moved_any {
        // user hand-placed a window — forget the tiling mode so an app-maximize
        // leaves the manual layout alone (respects the no-auto-arrange rule).
        app.last_arrange = None;
        app.retile_frames = 0;
    }
    // The Settings window's change, applied ONCE and saved, after every window has drawn with the
    // old look — so no frame mixes two (`AppearanceSession::apply_pending`).
    app.appearance.apply_pending(&ctx, credential_home().journal(), vike_model::now_ms());

    // process clones + on-demand feeds (`app` fully borrowable again)
    spawn_cloned_windows(app, &ctx, out.to_clone);
    ensure_wanted_feeds(app, &ctx, out.to_ensure);
    seed_charts_from_store(app, &ctx, &fr);
    fold_backfill_reports(app);
    register_orderflow(app, out.of_wanted);
    apply_trade_frame(app, &ctx, &mut out.trade_frame);
    // Polymarket cockpit windows: start (idempotently) each open window's token book+trade
    // stream, so its book flows into the shared BookStore under venue "polymarket".
    for token in out.poly_book_reqs {
        app.ensure_poly_book(&token);
    }
    teardown_deleted_series(app, out.to_stop);
    dispatch_orders(
        app,
        &fr,
        out.trade_frame,
        out.account_cancels,
        out.cockpit_cmds,
        out.opt_orders,
        out.opt_cancels,
    );
    apply_chart_draw_capture(app);
    apply_dataset_mutations(app, &ctx, out.ds_save, out.ds_del, out.ds_open);
    apply_stored_view_actions(
        app,
        &ctx,
        out.stored_deletes,
        out.stored_opens,
        out.stored_refresh_requested,
        out.stored_backfills,
    );
    apply_backend_actions(app, &ctx, out.backend_action, out.backend_registry_update);
    drain_backend_settings_and_directory(
        app,
        ctx,
        &fr,
        out.backend_settings_refresh,
        out.backend_settings_edit,
        out.backend_settings_write,
    );
}

/// The collect-then-apply accumulators the window loop fills and [`draw_windows`]'s tail drains.
///
/// Anything that needs the whole `&mut App` — a spawn, a feed ensure, a store call, a command
/// send — cannot run while a window is being drawn, so the per-window phases only RECORD their
/// wants here and the tail applies them in a fixed order. Each field's doc is the comment its
/// declaration carried when it was a local of the one big function.
#[derive(Default)]
struct WindowOut {
    /// Cross-venue: (venue, symbol, interval, asset_class) — the primary/compare feeds to
    /// ensure after the loop. Compare overlays and foreign-source study symbols are always
    /// spot (`None` — see their push sites); the primary carries the window's own
    /// `venue`/`asset_class` so a Bybit/OKX chart (or an OKX derivative) subscribes the right
    /// feed (feed-routing slice 1).
    to_ensure: Vec<(String, String, String, Option<vike_model::AssetClass>)>,
    /// SP2 orderflow (Task 7): (venue, symbol, chart key, requested tick size) for every OPEN
    /// chart window whose `WinState::orderflow_on()` is true this frame — collected here (pure
    /// reads of `w`) because the actual `&mut App` work (trade subscribe + `of_aggs`
    /// registration) can only happen AFTER the loop (mirrors `to_ensure`'s collect-then-apply
    /// shape). The window's `venue` is carried so orderflow subscribes the RIGHT venue's tape
    /// (venue-aware — any `subscribe_trades` venue, not just Binance). Pushed unconditionally
    /// every frame a toggle is on — cheap, and idempotent downstream (`ensure_trade_feed_on`'s
    /// `spawned` check, `of_aggs`'s `entry`), same as the Trade windows' depth streams.
    of_wanted: Vec<(String, String, String, Option<f64>)>,
    /// Chart windows whose title-bar clone button was clicked (indices into `app.wins`).
    to_clone: Vec<usize>,
    /// Any title-bar drag this frame → drop the tiling memory.
    user_moved_any: bool,
    /// Data-manager "Delete" → drop these feeds after.
    to_stop: Vec<String>,
    /// DataSet Save → upsert after the loop.
    ds_save: Option<datasets::DataSet>,
    /// The Trade windows' addressed intents, picks, Connect clicks and depth streams — every
    /// decision is `tool_views::TradeFrame`'s, one crate down where CI runs it.
    trade_frame: tool_views::TradeFrame,
    /// Account window ✕ → cancel by coid.
    account_cancels: Vec<String>,
    /// Polymarket cockpit order intents resolved in the window loop, folded onto the command lane
    /// after it (venue is always "polymarket").
    cockpit_cmds: Vec<CockpitCmd>,
    /// Options confirm-ticket → deribit exec.
    opt_orders: Vec<tools::OptOrderTicket>,
    /// Options chain marker → deribit cancel (coid).
    opt_cancels: Vec<String>,
    /// Cockpit → ensure a polymarket token book+trade stream.
    poly_book_reqs: Vec<String>,
    /// DataSet Delete.
    ds_del: Option<String>,
    /// "Test symbol" → open a chart after the loop.
    ds_open: Option<String>,
    /// Data Manager "Stored" view (Task 3) OUT actions, applied after the window loop below.
    /// Refresh click / first-shown / post-delete reload.
    stored_refresh_requested: bool,
    /// (venue, symbol, kind, interval)
    stored_deletes: Vec<(String, String, String, Option<String>)>,
    /// (venue, symbol, interval)
    stored_opens: Vec<(String, String, Option<String>)>,
    /// dm-bulk-backfill: grid v2's bulk Backfill/Update selection, drained below and spawned
    /// after the loop via `App::maybe_spawn_stored_backfill` (same deferred-mutation shape).
    stored_backfills: Vec<vike_data_manager::SeriesKey>,
    /// Connections backend picker (split-plane B1) OUT slot, applied after the loop via
    /// `App::apply_backend_action` (the same deferred-mutation shape as every action above — the
    /// switch routine needs `&mut App` the loop can't give it).
    backend_action: Option<vike_app_core::backend::backend_conn::BackendAction>,
    /// Backends editor (split-plane I2) OUT slot: the drained Add/Edit/Delete registry
    /// update, applied after the loop through `backend_editor::apply_registry_update` (which
    /// pins delete-active's disconnect-BEFORE-save order).
    backend_registry_update: Option<vike_app_core::backend::backend_editor::RegistryUpdate>,
    /// Backend-settings section (split-plane REQ-7, read half): the `refresh` OUT slot, drained
    /// after the loop via `App::spawn_backend_settings_fetch` (the same deferred-mutation shape
    /// as `backend_action` above).
    backend_settings_refresh: bool,
    /// REQ-7 WRITE half: the edit flow is `app` state (typed into across frames) TAKEN into a
    /// frame-local — the window loop borrows `app` piecemeal, so `&mut App` fields can't cross
    /// into it — and written back after the loop.
    backend_settings_edit: vike_app_core::ui::tool_views::SettingsEditState,
    /// `backend_settings_write` is the Save out-slot, drained after the loop into
    /// `spawn_backend_settings_write`.
    backend_settings_write: Option<vike_app_core::ui::tool_views::SettingsWriteRequest>,
}

/// The read-only values the window loop and the tail need, each a CHEAP snapshot of an `App` field
/// taken once per frame by [`window_frame`] before any window draws — the same snapshot POINT the
/// original body had, and for the same reason (the per-window phases hold `w` and cannot re-read
/// `app` for these without seeing a different value mid-frame).
struct WindowFrame {
    desktop: egui::Rect,
    /// The paper trading state for the Trade window.
    core_snap: Arc<vike_core::CoreSnapshot>,
    td: tools::ToolData,
    /// Options Refresh pill → immediate chain re-poll (`Sender` is cheap-Clone).
    opt_refresh: std::sync::mpsc::Sender<()>,
    /// `TextureHandle` clones are Arc-cheap.
    flags: HashMap<String, egui::TextureHandle>,
    logos: HashMap<String, egui::TextureHandle>,
    /// Small; cloned so `app.datasets` is free to mutate after.
    dsets: datasets::Store,
    /// (key "SYM@iv", bar count, first bar open-ms, last bar open-ms) — Data-manager columns.
    feeds: Vec<(String, usize, i64, i64)>,
    /// vike maximize = fill the workspace by HIDING every other live window.
    maxed_idx: Option<usize>,
    backend_switching: bool,
    /// WHICH BOX the connected daemon says it is (`None` from a node that predates the field,
    /// before the first frame, or between a drop and the next connection).
    daemon_self_report: Option<vike_tradehub_client::wire::WireNodeIdentity>,
    /// The ONE datahub resolution (REQ-2) of this frame.
    resolved_datahub_addr: Option<String>,
    datahub_key_name: String,
    /// The CHART-SEED sentence, snapshotted once per frame.
    chart_seed_note: Option<String>,
    /// The ACTIVE backend's backend-settings slot state (`Idle` when the slot belongs to another).
    backend_settings_view: vike_app_core::ui::tool_views::BackendSettingsState,
    last_trade: Option<tool_views::TradePick>,
    active_addr: Option<String>,
    directory: Option<Arc<vike_tradehub_client::wire::WireDirectory>>,
    directory_unavailable: bool,
    control_link: tool_views::ControlLink,
    now_ms: i64,
    display_tz: DisplayTz,
    of_backfill_hours: f64,
    gpu_render: bool,
    gpu_ok: bool,
}

/// Window phase 1 — the maximize-edge re-tile.
fn retile_on_maximize(app: &mut App, ctx: &egui::Context) {
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
}

/// Window phase 2 — every read the window loop and the tail need, snapshotted ONCE before any
/// window draws: the app-state clones, the datahub resolution (with the three side effects that
/// point the market-data session and the venue catalog at it), the backend-settings slot, and the
/// Trade windows' inputs. The statements run in the order the one big body ran them; the only
/// writes to [`WindowOut`] are the two the backend-settings write-result fold makes.
fn window_frame(app: &mut App, out: &mut WindowOut) -> WindowFrame {
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
    // vike maximize = fill the workspace by HIDING every other live window.
    let maxed_idx = app.wins.iter().position(|w| w.open && !w.minimized && w.maximized);
    // The Connections backend picker's availability gate (its OUT slot is `WindowOut::backend_action`).
    let backend_switching =
        vike_app_core::backend::backend_conn::switching_available(app.core.is_some());
    // WHICH BOX the connected daemon says it is — taken ONCE per frame before the windows loop
    // (which borrows `app` piecemeal), exactly like `resolved_datahub_addr` below. It is the one
    // identity fact this side cannot derive: both production listeners bind loopback, so this
    // process reaches them down an SSH tunnel and its own socket address is the tunnel mouth —
    // identical on every box, for every daemon. `None` from a node that predates the field, before
    // the first frame, or between a drop and the next connection: the bridge clears it on a link
    // drop like the rest of the identity, so a stale box name cannot outlive its connection.
    let daemon_self_report = app.active_backend.as_ref().and_then(|b| b.bridge.identity());
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
    // Hoisted to a local because the STORE plane needs the same answer this frame: `tool_content`
    // threads it to `open_studio_store`, so the two datahub planes sign with one name rather than
    // resolving it twice and risking a disagreement across a mid-frame backend switch.
    let datahub_key_name = app.datahub_observe_key_name();
    app.md_session.set_key_name(&datahub_key_name);
    // ...and the Data Manager's venue-catalog route is pointed at the same pair, for the same
    // reason and with the same `None`-holds rule (`CatalogRefresh::dial_is_stale` states both).
    // ⚠ The guard is not an optimisation: RESOLVING a dial opens the credential store, so the
    // resolution runs only when the (address, key name) pair actually changes — which is a handful
    // of frames in a session. `CatalogDial::resolve` is the `chart_seed_dial` twin and lives in
    // `main.rs` because only a binary may read the process environment.
    if app.catalog.dial_is_stale(resolved_datahub_addr.as_deref(), &datahub_key_name) {
        let dial = resolved_datahub_addr
            .as_deref()
            .map(|a| catalog_dial(a.to_string(), &datahub_key_name));
        app.catalog.set_dial(dial);
    }
    // The CHART-SEED sentence, snapshotted once per frame for the same reason as the two above: the
    // windows loop borrows `app` piecemeal, and a lock taken per window would be N uncontended
    // locks for one string. Written by the background read thread (`chart_seed::SeedDial::run`), so
    // this frame paints the PREVIOUS batch's answer — which is right, since a batch that is still in
    // flight has no answer yet and the thread requests a repaint when it does.
    let chart_seed_note: Option<String> = app.chart_seed_note.lock().ok().and_then(|n| n.clone());
    // Backend-settings section (split-plane REQ-7, read half): the ACTIVE backend's slot
    // state this frame — `Idle` when the slot belongs to another (switched-away) backend, so
    // stale rows never render and the section auto-refetches. The `refresh` OUT slot is
    // drained after the loop via `App::spawn_backend_settings_fetch` (the same
    // deferred-mutation shape as `WindowOut::backend_action`).
    let backend_settings_view: vike_app_core::ui::tool_views::BackendSettingsState = {
        match app.active_backend.as_ref().map(|b| b.record.addr.as_str()) {
            Some(addr) => {
                let slot = app.backend_settings.lock().unwrap();
                if slot.0 == addr { slot.1.clone() } else { Default::default() }
            }
            None => Default::default(),
        }
    };
    // REQ-7 WRITE half: the edit flow is `app` state (typed into across frames) TAKEN into a
    // frame-local (`WindowOut::backend_settings_edit`) — the window loop can't hold `&mut App`
    // fields across a window — and written back after the loop. `backend_settings_write` is the
    // Save out-slot, drained after the loop into `spawn_backend_settings_write`.
    out.backend_settings_edit = std::mem::take(&mut app.backend_settings_edit);
    // Fold a finished write first (the worker parked the already-folded flow state, keyed by
    // addr): still-active backend ⇒ show it, and a SAVED write refetches the table so the new
    // file value renders beside the restart note; switched-away ⇒ dropped, never painted
    // under the new backend (the fetch slot's discipline).
    if let Some((addr, folded)) = app.backend_settings_write_result.lock().unwrap().take() {
        if app.active_backend.as_ref().is_some_and(|b| b.record.addr == addr) {
            if vike_app_core::ui::tool_views::should_refetch_after_write(&folded) {
                out.backend_settings_refresh = true;
            }
            out.backend_settings_edit = folded;
        } else {
            out.backend_settings_edit = Default::default();
        }
    }
    // What the Trade windows read this frame, taken once before the loop holds `app.wins`: the
    // window a new one copies (minor 26) and the ACTIVE backend's directory (venue names, accounts).
    let last_trade = app.last_trade.clone();
    let active_addr = app.active_backend.as_ref().map(|b| b.record.addr.clone());
    let directory = tool_views::directory_reply(&app.directory, active_addr.as_deref());
    // ...whether that directory's fetch ended with no list (item 5), and whether this desktop can
    // send at all (item 11): the windows say both up front.
    let directory_unavailable =
        tool_views::directory_unavailable(&app.directory, active_addr.as_deref());
    let control_link = tool_views::ControlLink::of(app.remote_ctrl().map(|c| c.is_connected()));
    let now_ms = chrono::Local::now().timestamp_millis();
    let display_tz = app.display_tz; // Copy; read once (task A6 clocks + tool_content)
    // SP3 follow-up (Task 2): Copy; read once, threaded into every chart window's OF popup
    // this frame (`title_bar`'s `of_backfill_hours` param) — same up-front-copy shape as
    // `display_tz` right above, a plain local the per-window `title_bar` call below can read
    // without going back through `app`.
    let of_backfill_hours = app.of_backfill_hours;
    // GPU candle layer (GPU Phase 2, Task 3): Copy; read once, same up-front-copy shape as
    // `display_tz`/`of_backfill_hours` above — every chart window's `ChartInputs::gpu_candles`
    // wiring reads the pair through the frame.
    let gpu_render = app.gpu_render;
    let gpu_ok = app.gpu_ok;
    WindowFrame {
        desktop,
        core_snap,
        td,
        opt_refresh,
        flags,
        logos,
        dsets,
        feeds,
        maxed_idx,
        backend_switching,
        daemon_self_report,
        resolved_datahub_addr,
        datahub_key_name,
        chart_seed_note,
        backend_settings_view,
        last_trade,
        active_addr,
        directory,
        directory_unavailable,
        control_link,
        now_ms,
        display_tz,
        of_backfill_hours,
        gpu_render,
        gpu_ok,
    }
}

/// Window phase 3a — one CHART window: gather its inputs (compare overlays, orderflow, the
/// indicator fold), take its persisted state out of `w`, draw the title bar and `chart::draw`
/// inside `show_window`, then write the state back and apply what the frame harvested
/// ([`apply_chart_outputs`], [`drive_indicator_picker`], [`apply_title_actions`]). Returns the
/// title bar's actions for the window controls the loop applies to every kind.
fn draw_chart_window(
    app: &mut App,
    ctx: &egui::Context,
    w: &mut workspace::WinState,
    wi: usize,
    fr: &WindowFrame,
    snap: &FrameSnapshot,
    out: &mut WindowOut,
) -> TitleActions {
    let wid = w.id;
    let wid64 = wid.value(); // chart sync seam (task B8): u64 identity for the group registry
    let is_max = w.maximized;
    let mut act = TitleActions::default();
    // The GPU candle layer's shape builder: `gpu_build` captures neither `app` nor anything else
    // (its two params supply everything it needs), so building it per chart window is free.
    let gpu_build =
        |instances: Vec<vike_chart::render::CandleInstance>, rect: egui::Rect| -> egui::Shape {
            eframe::egui_wgpu::Callback::new_paint_callback(
                rect,
                chart_gpu::CandleCallback { instances, rect },
            )
            .into()
        };
    let charts = &app.charts;
    // What the connected node publishes — a disjoint field from `app.sync_next` below, bound here
    // beside `charts` for the same split-borrow reason. Read-only in this phase; it decides
    // what the title-bar badge is entitled to claim and which rows the symbol picker offers.
    let published = &app.published_series;
    let of_aggs = &app.of_aggs; // SP2 orderflow (Task 7): read-only in this phase
    // Indicator favourites (part b): a MUTABLE handle to the global favourites set,
    // bound here (a distinct field from `app.charts` above, so the split borrow is
    // sound) so the per-window ƒx picker can star/unstar into it while `w` is `&mut`.
    let indicator_favs = &mut app.indicator_favs;

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
    // What the `● LIVE` badge is ENTITLED to say. It was an unconditional label, so an
    // empty grid over a node publishing nothing read as a live candle stream — see
    // `series_follow::ChartFeed`.
    let feed = series_follow::chart_feed(!bars_slice.is_empty(), published, &key);
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
        out.to_ensure.push((
            workspace::DEFAULT_VENUE.to_string(),
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
    // `to_ensure`'s collect-then-apply shape).
    let cvd_on = w.cvd_on;
    let profile_on = w.profile_on;
    let of_tick_size_w = w.of_tick_size;
    let of = of_aggs.get(&key);
    let fps = of.map(|(_, _, a)| a.footprints());
    if w.orderflow_on() {
        out.of_wanted.push((venue.clone(), symbol.clone(), key.clone(), of_tick_size_w));
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
    let (closed, _) = bars_slice.split_at(n_closed);
    fold_indicators(&mut inds, charts, bars_slice, n_closed, &interval, &mut out.to_ensure);
    // This frame's `ChartActions` — the hover, nav, scale/invert/options changes, the
    // sync outputs (harvested into the group registry AFTER `show_window` returns, see
    // `sync_harvest` in `apply_chart_outputs`), the CVD / volume ✕ clicks, the study
    // relocation (C1 Task 4) and the pane ↑/↓ reorder (Feature #1) — all applied to
    // `w` after `show_window` returns. Default (every field empty) when there is no
    // chart yet, which is what the per-field `None`/`false` locals this replaced were.
    let mut chart_out = chart::ChartActions::default();
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
    // `_bounds`: a chart is a `fills` kind, so its body sizes itself to the available
    // rect and must NEVER be wrapped — this call site uses no wrapper at all, which
    // is what keeps the chart byte-identical. The other two `fills` kinds (DOM,
    // Polymarket) go through the TOOL call site instead, which DOES call
    // `BodyBounds::show`; that method's `fills` arm is what keeps them unwrapped
    // too. See `WinState::user_sized`.
    if workspace::show_window(ctx, w, fr.desktop, |ui, _bounds| {
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
            fr.of_backfill_hours,
            &compare_syms, // C2a Task 4: current overlays → the Compare popup chips
            &series_pane,  // C2b Task 9: own-pane assignment → the per-chip menu
            &series_scale, // C2b Task 9: secondary-axis pins → the per-chip menu
            &snap.symbols_catalog, // symbol-search: live instrument universe → picker
            published,     // the node's own series → the picker's backend section
            fr.directory.as_deref(), // venue names (`venue.title`) → the picker's chips
            feed,          // what the badge may claim
        );
        act = acts;
        // The EMPTY-CHART SENTENCE. A chart with no bars used to be an empty grid with
        // an autoscaled axis and a `LIVE` badge; `ChartFeed::hint` says which of the
        // three ways it is empty. A `Live` chart paints nothing (the call returns).
        // ...plus the CHART-SEED sentence when there is one: which switch on which
        // server is why this pane is still blank. See `paint_empty_hint`'s `note`.
        series_follow::paint_empty_hint(ui, feed, fr.chart_seed_note.as_deref());
        // OHLC is now an in-plot overlay (chart::draw) so the window can shrink.
        let _ = (prev, last);
        if let Some(c) = chart {
            chart_out = chart::draw(
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
                    panes: &mut panes,      // chart-UX bundle T9: persisted per-window
                    sync: sync_in,          // chart sync seam (task B7 shape, task B8 wiring)
                    // SP2 orderflow (Task 7): live-wired. `footprint` is `None` until
                    // this chart's aggregator exists AND has ingested at least one
                    // trade batch (a one-two-frame startup lag after the toggle
                    // flips) — `chart::draw` treats `None` as "nothing to render yet"
                    // exactly like the pre-Task-7 always-off wiring did.
                    // SP3 Task B #1: `footprints()` now hands out `Arc<Vec<..>>`
                    // (cached — see `bar_agg::OrderflowAgg::cache`'s doc), so
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
                    // `bar_agg::OrderflowAgg::new`'s `<= 0.0 → 1.0` default).
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
                    gpu_candles: (fr.gpu_render && fr.gpu_ok)
                        .then_some(&gpu_build as &dyn Fn(_, _) -> _),
                },
            );
        } else {
            vike_ui_theme::components::state::view(
                ui,
                vike_ui_theme::components::state::Load::Loading("Loading chart"),
            );
        }
        drag
    }) {
        out.user_moved_any = true;
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
    apply_chart_outputs(
        w,
        chart_out,
        closed,
        &mut app.sync_next,
        &mut app.range_leader,
        sync_group,
        wid64,
    );
    drive_indicator_picker(
        ctx,
        w,
        act.open_picker,
        indicator_favs,
        &snap.symbols_catalog,
        bars_slice,
    );
    // A dashed venue chip in the symbol picker was clicked: ask for that venue's catalog list, by
    // the same entry the Data Manager's Refresh button uses, so the cooldown and every refusal
    // apply. The picker fetches nothing itself; this runs on its own thread and the merged catalog
    // arrives over `catalog_rx`. A refusal is logged, not drawn: the Data Manager row says why.
    if let Some(venue) = act.load_venue.take() {
        let (catalog, wake) = (std::sync::Arc::clone(&app.catalog), ctx.clone());
        let asked = catalog.request(&venue, vike_model::now_ms(), move || wake.request_repaint());
        if let Err(block) = asked {
            tracing::warn!("catalog: the picker's load of {venue} was refused: {block:?}");
        }
    }
    apply_title_actions(w, &mut act, wi, &mut app.of_backfill_hours, out);
    act
}

/// Chart phase — the two-tier indicator fold: each active indicator advances over the CLOSED bars
/// only (no-op / one on_bar / refold on structural change), and the live forming bar is previewed
/// on a throwaway clone each frame, so a tick never triggers a full-history refold. A foreign-source
/// study folds over ANOTHER symbol's bars and queues that feed on `to_ensure`.
fn fold_indicators(
    inds: &mut [indicators::Active],
    charts: &HashMap<String, model::ChartState>,
    bars_slice: &[model::Bar],
    n_closed: usize,
    interval: &str,
    to_ensure: &mut Vec<(String, String, String, Option<vike_model::AssetClass>)>,
) {
    let (closed, forming) = bars_slice.split_at(n_closed);
    for a in inds {
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
                to_ensure.push((src.venue.clone(), src.symbol.clone(), interval.to_string(), None));
                let fkey = workspace::series_key(&src.venue, &src.symbol, interval);
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
}

/// Chart phase — apply what `chart::draw` harvested onto `w`, now that its persisted state is back
/// in place: the hover and nav, the CVD / volume ✕ clicks, the study relocation and pane reorder,
/// scale / invert / options changes, the sync-group harvest, the live indicator edit and the
/// indicator removal. `closed` is the chart's closed-bar prefix the indicator refold runs over.
fn apply_chart_outputs(
    w: &mut workspace::WinState,
    chart_out: chart::ChartActions,
    closed: &[model::Bar],
    sync_next: &mut HashMap<u8, GroupFrame>,
    range_leader: &mut HashMap<u8, u64>,
    sync_group: Option<u8>,
    wid64: u64,
) {
    w.hover = chart_out.hovered;
    w.nav = chart_out.nav_out;
    // SP2 orderflow (Task 7): the CVD pane's own ✕ was clicked this frame — write
    // the persisted toggle back to off (mirrors `remove_uid`'s "the caller owns
    // the source of truth" shape; see `ChartActions::cvd_toggle`'s doc).
    if chart_out.cvd_toggle {
        w.cvd_on = false;
    }
    // Volume-as-indicator: the volume pane ✕ turns off show_volume
    // (re-add via the ƒx picker's "Volume" entry). `w.options` was
    // restored above, so this direct write persists like cvd.
    if chart_out.volume_remove {
        w.options.show_volume = false;
    }
    // C1 Task 4: apply the ••• "Move to" relocation onto the authored
    // pane model (`w.study_pane`/`w.pane_order` are both intact here —
    // `study_pane` was written back above, `pane_order` was never
    // taken). `move_study` re-points the study's pane and drops any
    // now-empty pane; like the sibling toggles this is a direct field
    // write (no dirty flag exists — `persist::save` reads live state
    // whenever the user explicitly saves).
    if let Some((uid, target)) = chart_out.move_study {
        w.move_study(uid, target);
    }
    // Feature #1: apply the pane ↑/↓ reorder onto `w.pane_order`
    // (intact here — never taken). Direct write, same shape as
    // `move_study` above. Volume/CVD/Study are peers now.
    if let Some((pane, up)) = chart_out.reorder_pane {
        w.reorder_pane(pane, up);
    }
    if let Some(sm) = chart_out.scale_change {
        w.scale = sm;
    }
    if let Some(iv) = chart_out.invert_change {
        w.invert = iv; // TradingView "Invert scale" toggle from the price-axis menu
    }
    if let Some(o) = chart_out.options_change {
        w.options = o; // T6: OK in the settings dialog committed a new palette/flags
    }
    // Chart sync seam (task B8): harvest this frame's sync outputs into the group
    // registry (no-op when `sync_group` is `None` — see `sync_harvest`'s doc).
    sync_harvest(
        sync_next,
        range_leader,
        sync_group,
        wid64,
        chart_out.interacted,
        chart_out.visible_ts,
        chart_out.hover_ts,
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
    )) = chart_out.indicator_edit
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
    if let Some(uid) = chart_out.remove_uid {
        w.remove_indicator(uid);
    }
}

/// Chart phase — the ƒx indicator picker: toggle it when the title bar asked, draw its popup, and
/// add the indicator it returns (routed to the picker-chosen pane).
fn drive_indicator_picker(
    ctx: &egui::Context,
    w: &mut workspace::WinState,
    open_picker: bool,
    indicator_favs: &mut Vec<String>,
    symbols_catalog: &vike_catalog::Catalog,
    bars_slice: &[model::Bar],
) {
    if open_picker {
        w.picker_open = !w.picker_open;
    }
    if let Some(name) = tool_views::fx_picker_popup(ctx, w, indicator_favs, symbols_catalog) {
        // Part (a): route the add to the picker-chosen target pane
        // (reusing `move_study`); `PaneTarget::Auto` == today's behavior.
        let target = w.picker_target;
        w.add_indicator_to(name, bars_slice, target);
    }
}

/// Chart phase — apply the title bar's actions to `w`: style, the Compare popup (add / remove /
/// own-pane / overlay / scale pin), the orderflow popup, the global backfill-hours field, a new
/// symbol or interval (which queue their feed ensure), the sync-group chip and the clone request.
/// Each `Option<String>` is `take`n because the caller still reads the window-control bools off
/// the same `act` afterwards.
fn apply_title_actions(
    w: &mut workspace::WinState,
    act: &mut TitleActions,
    wi: usize,
    of_backfill_hours: &mut f64,
    out: &mut WindowOut,
) {
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
    if let Some(s) = act.add_compare.take() {
        w.add_compare(&s);
    }
    if let Some(s) = act.remove_compare.take() {
        w.remove_compare(&s);
    }
    // C2b Task 9: apply the per-chip Move-to / Pin-to-scale menu onto the
    // (already-restored above) `w.series_pane`/`w.series_scale`. Each menu
    // item sources `sym` from `w.compare` (the chip loop only iterates
    // compare), so `move_series_to_new_pane` is always called with a live
    // compare symbol — closing Task 5 review Minor-2 by construction.
    if let Some(s) = act.series_to_own_pane.take() {
        w.move_series_to_new_pane(&s);
    }
    if let Some(s) = act.series_to_overlay.take() {
        w.overlay_series(&s);
    }
    if let Some((s, assign)) = act.set_series_scale.take() {
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
    // `TitleActions::new_backfill_hours`'s doc), handed in as `of_backfill_hours`
    // (`&mut app.of_backfill_hours`, a field disjoint from the chart borrows the
    // caller holds), so writing it directly alongside `w`'s own field writes is
    // fine. No separate "dirty"/autosave
    // flag exists anywhere in this file to also set: every one of these toggles
    // just writes the live field, and `workspace::persist::save` (only called
    // from the explicit `menu.save_workspace` action) reads it whenever that
    // actually happens — this field is picked up the exact same way.
    if let Some(v) = act.new_backfill_hours {
        *of_backfill_hours = v;
    }
    if let Some(s) = act.new_symbol.take() {
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
        w.venue = act.new_venue.take().unwrap_or_else(|| workspace::DEFAULT_VENUE.to_string());
        // Feed-routing slice 1: set atomically with symbol/venue so `w.asset_class`
        // never lags the symbol it describes (`take()` leaves `None` for the next
        // frame, mirroring `new_venue`'s take-and-default idiom).
        w.asset_class = act.new_asset_class.take();
        // THE OPERATOR CHOSE THIS SERIES, so `series_follow::follow_backend` may never
        // retarget the window again — see `WinState::series_pinned`. ⚠ The interval
        // arm below sets the OTHER flag: picking a resolution is not picking a series,
        // and conflating the two is what disabled adoption for good on one menu click.
        w.series_pinned = true;
        w.retitle();
        // Auto-scale the new symbol: re-engage follow-live + y-autofit and
        // force a full-range refit of both axes (else a same-bar-count swap
        // inherits the old symbol's zoom/pan — the "doesn't auto-scale" bug).
        w.follow.on_series_change();
        out.to_ensure.push((w.venue.clone(), w.symbol.clone(), w.interval.clone(), w.asset_class));
    }
    if let Some(iv) = act.new_interval.take() {
        w.interval = iv;
        // ⚠ THE RESOLUTION, NOT THE SERIES — `interval_pinned`, never `series_pinned`.
        // This line wrote the latter until 2026-09-15, and because NOTHING in the tree
        // ever clears either flag, one click on the interval menu permanently disabled
        // backend adoption for that window. Dropping the write instead is worse and
        // was measured as such (the chart silently snaps back to the daemon's
        // interval); the fix is the split. See `WinState::interval_pinned`.
        w.interval_pinned = true;
        w.retitle();
        w.follow.on_series_change(); // same refit on a timeframe swap
        out.to_ensure.push((w.venue.clone(), w.symbol.clone(), w.interval.clone(), w.asset_class));
    }
    if let Some(g) = act.new_group {
        w.sync_group = g; // task B8: chip click — None -> 1 -> 2 -> 3 -> 4 -> None
    }
    if act.clone {
        out.to_clone.push(wi);
    }
}

/// Window phase 3b — one TOOL window: the title bar and `tool_content` inside `show_window`
/// (the tool's own state split off `app` and restored after), then every OUT slot the tool left on
/// its view drained into [`WindowOut`] ([`drain_cockpit_intents`], [`drain_tool_view_actions`]).
/// Returns the title bar's actions for the window controls the loop applies to every kind.
fn draw_tool_window(
    app: &mut App,
    ctx: &egui::Context,
    w: &mut workspace::WinState,
    fr: &WindowFrame,
    snap: &FrameSnapshot,
    books: &Arc<data_sink::BookStore>,
    out: &mut WindowOut,
) -> TitleActions {
    // A Trade window's book is "stale" if no update landed within this window (or none has yet).
    const TRADE_STALE_MS: i64 = 2000;
    let wid = w.id;
    let is_max = w.maximized;
    let mut act = TitleActions::default();
    let kind = w.kind;
    // A Trade window's venue and venue-native symbol (spec §4.2), a cockpit's token on
    // "polymarket"; "" for the other tools. Owned: `w` goes `&mut` into `show_window`.
    let symbol = w.symbol.clone();
    let venue = if kind == workspace::WinKind::Polymarket {
        "polymarket".to_string()
    } else {
        w.venue.clone()
    };
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
    // The book a book-backed window reads, keyed (venue, symbol) in the ONE `BookStore`.
    // `None` (no book yet) counts as stale so it never reads as live. A Trade window's
    // depth stream is requested by its drain below (`TradeFrame::books`), after picks;
    // a cockpit's token stream here, unless it is the placeholder still resolving.
    // Cockpit books update far less often than a crypto book, hence `POLY_STALE_MS`.
    // NOTE: books reach this crate over the datahub market-data link
    // (`vike_app_core::data::md_session`), so a cockpit's stays stale until that link
    // populates the store under "polymarket".
    let book_ms = match kind {
        workspace::WinKind::Trade if !symbol.is_empty() => Some(TRADE_STALE_MS),
        workspace::WinKind::Polymarket => {
            if symbol != POLY_PLACEHOLDER_TOKEN {
                out.poly_book_reqs.push(symbol.clone());
            }
            Some(POLY_STALE_MS)
        }
        _ => None,
    };
    let (book, book_stale) = match book_ms.map(|ms| (ms, books.get(&venue, &symbol))) {
        Some((ms, Some((book, ts)))) => (Some(book), fr.now_ms - ts > ms),
        Some((_, None)) => (None, true),
        None => (None, false),
    };
    if workspace::show_window(ctx, w, fr.desktop, |ui, bounds| {
        // ⚠ `tabs` is the rect this bar RESERVED between the window title and its
        // `─ □ ✕` controls for a tabbed tool's segmented control (Connections' tabs, the
        // Trade window's view controls).
        // The bar cannot draw those chips — their labels carry counts the BODY folds
        // one step later — so it hands the geometry down and the body paints into it
        // in the same pass. See `workspace::title_bar`'s module doc.
        let (acts, drag, tabs) = workspace::tool_title_bar(ui, kind, is_max);
        act = TitleActions {
            close: acts.close,
            minimize: acts.minimize,
            toggle_max: acts.toggle_max,
            ..TitleActions::default()
        };
        // vike tool bodies are inset ~8px from the window edges (the title bar stays
        // flush); the chart-title flush fix zeroed the window margin, so re-add it
        // here. ⚠ The inset is `workspace::TOOL_BODY_MARGIN` rather than a literal
        // because it is GEOMETRY the edge-resize bands depend on — that constant's
        // doc carries what each side buys, and the headless harness that gates it
        // models the shipped value rather than a second copy.
        egui::Frame::new().inner_margin(workspace::TOOL_BODY_MARGIN).show(ui, |ui| {
            // `bounds` is `show_window`'s body wrapper: for a NON-`fills` kind it
            // applies a `ScrollArea` whose SIZING follows whether this window is
            // app-owned yet (the user grabbed an edge band — see
            // `WinState::user_sized`), the container itself being present from frame
            // one so the body's id chain never shifts. ⚠ This call site also
            // dispatches the two `fills` TOOL kinds — Trade and Polymarket — and for
            // those `BodyBounds::show` wraps NOTHING and hands the body straight
            // through, so their id chains stay byte-identical to the unwrapped
            // spelling they had before this wrapper existed (an extra `Ui` level
            // would discard every egui-persisted widget state inside the ladder
            // and the cockpit, once, on upgrade). It lives in vike-app-core, not
            // here: `vike-desktop` is CI-excluded, so a wrap spelled at this call
            // site was gated by nothing.
            bounds.show(ui, |ui| {
                tool_content(
                    ui,
                    kind,
                    &symbol,
                    &venue,
                    book.as_ref(),
                    book_stale,
                    &app.trades,
                    &fr.td,
                    &fr.feeds,
                    &fr.flags,
                    &fr.logos,
                    &fr.dsets,
                    &fr.core_snap,
                    &mut tv,
                    fr.display_tz,
                    &mut studio,
                    &mut studio_error,
                    &snap.stored_tree,
                    &snap.stored_gaps,
                    &snap.stored_partials,
                    snap.stored_coverage,
                    app.stored_history.as_deref(),
                    snap.stored_loading,
                    snap.stored_load_error.as_deref(),
                    &snap.stored_backfill_status,
                    &fr.opt_refresh,
                    &app.feed_statuses,
                    &app.poly_names,
                    &tool_views::BackendPicker {
                        backends: &app.backends,
                        active: app.active_backend.as_ref().map(|b| &b.record),
                        available: fr.backend_switching,
                        reported: vike_app_core::backend::backend_identity::SelfReport::of(
                            fr.daemon_self_report.as_ref().map(|id| id.advertise_addr.as_str()),
                        ),
                    },
                    &mut out.backend_action,
                    &mut backend_editor,
                    &mut out.backend_registry_update,
                    &fr.backend_settings_view,
                    &mut out.backend_settings_refresh,
                    &mut out.backend_settings_edit,
                    &mut out.backend_settings_write,
                    fr.resolved_datahub_addr.as_deref(),
                    &fr.datahub_key_name,
                    &tabs,
                    &app.catalog,
                    &mut app.appearance,
                    &snap.symbols_catalog,
                    fr.last_trade.as_ref(),
                    fr.directory.as_deref(),
                    fr.directory_unavailable,
                    fr.control_link,
                );
            });
        });
        drag
    }) {
        out.user_moved_any = true;
    }
    // Trade: the window's picks, notes, seed and size, and its intents addressed with
    // the address it drew — `TradeFrame::drain`'s whole decision.
    if kind == workspace::WinKind::Trade {
        out.trade_frame.drain(w, &mut tv);
    }
    if kind == workspace::WinKind::Polymarket {
        drain_cockpit_intents(&mut tv, book.as_ref(), &symbol, &mut out.cockpit_cmds);
    }
    drain_tool_view_actions(ctx, &mut tv, fr.display_tz, out);
    app.tool_views.insert(wid, tv);
    app.studio = studio;
    app.studio_error = studio_error;
    app.backend_editor = backend_editor;
    act
}

/// Tool phase — the Polymarket cockpit: translate this frame's ladder + ticket intents into
/// `CockpitCmd`s here (where the ticket stake + live book are still in scope) for the post-loop
/// command fold.
fn drain_cockpit_intents(
    tv: &mut tools::ToolView,
    book: Option<&vike_model::L2Book>,
    symbol: &str,
    cockpit_cmds: &mut Vec<CockpitCmd>,
) {
    // Polymarket cockpit: translate this frame's ladder + ticket intents into
    // `CockpitCmd`s here (where the ticket stake + live book are still in scope) for
    // the post-loop command fold. The stake sizes both the ladder limits (shares =
    // stake/price) and the ticket market buys. Buying "Down" is modeled as SELL YES
    // (−1) on the SAME token — the paired NO-token id is not wired yet (see report).
    let stake = tv.cockpit_ticket.size;
    let up_px = book.and_then(|b| b.best_ask()).map(|l| l.price);
    let dn_px = book.and_then(|b| b.best_bid()).map(|l| 1.0 - l.price);
    for a in tv.cockpit_ladder_actions.drain(..) {
        match a {
            cockpit::ProbLadderAction::PlaceLimit { side, price } => {
                let qty = if price > 0.0 { (stake / price).max(0.0) } else { 0.0 };
                cockpit_cmds.push(CockpitCmd::Submit {
                    token: symbol.to_string(),
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
                    token: symbol.to_string(),
                    side: 1,
                    price: None,
                    qty,
                });
            }
            cockpit::TicketAction::BuyDown => {
                let px = dn_px.unwrap_or(0.0);
                let qty = if px > 0.0 { (stake / px).max(0.0) } else { 0.0 };
                cockpit_cmds.push(CockpitCmd::Submit {
                    token: symbol.to_string(),
                    side: -1,
                    price: None,
                    qty,
                });
            }
            // Size/arm are pure UI state already applied by the widget to
            // `tv.cockpit_ticket` — nothing to route to the core.
            cockpit::TicketAction::SetSize(_) | cockpit::TicketAction::ToggleArm => {}
        }
    }
}

/// Tool phase — every OUT slot a tool left on its `ToolView` this frame, drained into
/// [`WindowOut`] (or acted on at once where no `&mut App` is needed): the Options order ticket and
/// marker cancel, News links, Data-manager stop / DataSets save-delete-test, the Account cancel,
/// the Stored view's refresh / proxy box / deletes / backfills / opens.
fn drain_tool_view_actions(
    ctx: &egui::Context,
    tv: &mut tools::ToolView,
    display_tz: DisplayTz,
    out: &mut WindowOut,
) {
    let WindowOut {
        opt_orders,
        opt_cancels,
        to_stop,
        account_cancels,
        ds_save,
        ds_del,
        ds_open,
        stored_refresh_requested,
        stored_deletes,
        stored_opens,
        stored_backfills,
        ..
    } = out;
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
        let ts = vike_chart::to_naive(chrono::Utc::now().timestamp_millis(), display_tz)
            .map(|dt| dt.format("%H:%M:%S").to_string())
            .unwrap_or_default();
        tv.data_log.push(format!("{ts}  Stopped {k}"));
        to_stop.push(k);
    }
    if let Some(coid) = tv.account_cancel.take() {
        account_cancels.push(coid);
    }
    if tv.ds_save {
        tv.ds_save = false;
        let v = &tv;
        *ds_save = Some(datasets::DataSet {
            name: v.ds_name.trim().to_string(),
            symbols: datasets::parse_symbols(&v.ds_symbols_text),
            provider: v.ds_provider.clone(),
            interval: v.ds_interval.clone(),
            benchmark: v.ds_benchmark.trim().to_string(),
            user: true, // corrected in the apply (preserve an existing flag)
        });
    }
    if let Some(n) = tv.ds_delete.take() {
        *ds_del = Some(n);
    }
    if let Some(s) = tv.ds_test.take() {
        *ds_open = Some(s);
    }
    // Data Manager "Stored" view (Task 3): collect this window's OUT actions —
    // applied after the loop below, once `app` is fully borrowable again (same
    // deferred-mutation pattern as `ds_save`/`ds_del`/`ds_open` above).
    if tv.stored_refresh {
        tv.stored_refresh = false;
        *stored_refresh_requested = true;
    }
    // Polymarket proxy box: seed ONCE from the `venue.polymarket.*` rows the data
    // daemon reads, then leave it alone — re-seeding every frame would overwrite
    // whatever the operator is mid-way through typing. A stored `none`/`direct` shows
    // as an empty box (`proxy_display`), so "no proxy" looks like no proxy.
    if !tv.stored_proxy_loaded {
        tv.stored_proxy_loaded = true;
        tv.stored_proxy.buf = vike_app_core::ui::tool_views::seed_polymarket_proxy_box(
            crate::SETTINGS_DIR.get().and_then(Option::as_deref),
        );
    }
    if let Some(v) = tv.stored_proxy_save.take() {
        // A free function borrowing no `app`, so unlike the `stored_*` actions
        // around it this needs no deferral to after the window loop.
        vike_app_core::ui::tool_views::save_polymarket_proxy(
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
}

/// Window-loop tail — the title-bar window controls every window kind shares: close, minimize,
/// and the maximize toggle.
fn apply_window_controls(w: &mut workspace::WinState, act: &TitleActions, desktop: egui::Rect) {
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

/// Tail phase — open the windows the chart title bars' clone buttons asked for (`to_clone` holds
/// indices into `app.wins`, which is back in place by now).
fn spawn_cloned_windows(app: &mut App, ctx: &egui::Context, to_clone: Vec<usize>) {
    for wi in to_clone {
        // The source read stays a scoped borrow: `apply_spawn` takes `&mut App`, and pushing
        // only APPENDS, so `wi` stays valid for the remaining clones exactly as before.
        //
        // ⚠ `src.venue` is read here and then DROPPED by the planner — deliberately, and this
        // is not a widening of the read: a clone has ALWAYS opened on `workspace::DEFAULT_VENUE` and
        // still does, byte for byte. Carrying it puts the whole of the (pinned) defect inside
        // `crates/vike-app-core/src/ui/window_spawn.rs`'s `SpawnRequest`, so fixing it later is
        // one line in a file the merge gate compiles rather than another edit to this one.
        let (venue, symbol, interval) = {
            let src = &app.wins[wi];
            (src.venue.clone(), src.symbol.clone(), src.interval.clone())
        };
        let req = window_spawn::SpawnRequest::CloneWindow { venue, symbol, interval };
        app.apply_spawn(ctx, req);
    }
}

/// Tail phase — adopt the series the backend publishes into the charts that follow it, then fan
/// every feed the frame asked for (compare overlays, foreign-source studies, a changed
/// symbol/interval, the adopted series) out to `ensure_feed_on`.
fn ensure_wanted_feeds(
    app: &mut App,
    ctx: &egui::Context,
    mut to_ensure: Vec<(String, String, String, Option<vike_model::AssetClass>)>,
) {
    // THE CHART FOLLOWS THE SERIES THE BACKEND PUBLISHES: a chart NOBODY CHOSE and that is
    // rendering NOTHING retargets itself onto a series this node actually has. The whole rule —
    // and the argument for how narrow it is — is `vike_app_core::ui::series_follow::follow_backend`;
    // what belongs here is the wiring: ensure the adopted key (so `spawned` passes it through
    // `sync_from_core`'s fold filter) and force ONE refold, because the bars are already sitting in
    // the snapshot and waiting on the node's next publish would leave the adopted chart blank for
    // a whole cadence. Placed BEFORE `to_ensure` is drained so the adopted series rides the same
    // fan-out as every other subscription this frame.
    let adopted = series_follow::follow_backend(&mut app.wins, &app.charts, &app.published_series);
    if !adopted.is_empty() {
        app.last_seq = core_sync::FORCE_REFOLD;
    }
    for s in adopted {
        to_ensure.push((s.venue, s.symbol, s.interval, s.asset_class));
    }
    for (venue, s, iv, ac) in to_ensure {
        app.ensure_feed_on(ctx, &venue, &s, &iv, ac);
    }
}

/// Tail phase — ask the backend's store for the interval each empty chart was given (and, on the
/// same thread, the gap arm that asks the SERVER to fetch what the store answers empty for).
fn seed_charts_from_store(app: &mut App, ctx: &egui::Context, fr: &WindowFrame) {
    // THE CHART ASKS THE BACKEND'S STORE FOR THE INTERVAL IT WAS GIVEN. The daemon streams only
    // the series its mounted strategies hold (the CI box trades 1m), so a 5m/15m/1h/4h/1d chart could
    // never paint anything. The whole rule — kline only, empty only, unpublished only, once only —
    // is `vike_app_core::data::store_bars::plan_store_reads`; what belongs here is the wiring: the
    // resolved address, the dial, and recording each key as asked BEFORE the read is spawned, so
    // "once" survives the flight rather than only the return. Placed AFTER the ensure fan-out so
    // an adopted window's `ChartState` already exists for the seed to land into.
    let reads = store_bars::plan_store_reads(
        &series_follow::chart_targets(&app.wins, &app.charts),
        &app.published_series,
        &app.store_asked,
    );
    if let (Some(addr), Some(bars)) = (fr.resolved_datahub_addr.clone(), app.direct_bars.clone())
        && !reads.is_empty()
    {
        for r in &reads {
            app.store_asked.insert(r.key());
        }
        let wake = ctx.clone();
        // ...AND THE GAP ARM: a series the store answers EMPTY for is one the SERVER can be asked
        // to fetch. The whole rule and every message is `vike_app_core::data::chart_seed`; what belongs
        // here is the same wiring the read above needed — the resolved address, the OBSERVE keys
        // this binary already holds for that address, and the slot the note lands in. It runs on
        // the read's own thread, after it (`spawn_store_seed`'s `seed` doc says why one thread).
        //
        // ⚠ The keys are the DATAHUB OBSERVE pair and that is what makes this reachable at all:
        // `Request::SeedSeries` is `VerbScope::Read`
        // (`docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`), unlike `Backfill`, which
        // this binary can never send to a keyed datahub because no DATA-plane dial of it may hold a
        // control key (Studio's compute key is typed and plane-checked so it cannot be one).
        let dial = chart_seed_dial(addr.clone(), &fr.datahub_key_name, app.chart_seed_note.clone());
        store_bars::spawn_store_seed(
            reads,
            remote_hist_store(addr, &fr.datahub_key_name),
            bars,
            vike_model::now_ms(),
            Some(dial),
            move || wake.request_repaint(),
        );
    }
}

/// Tail phase — fold every finished backfill worker's exit report into `bf_retries`.
fn fold_backfill_reports(app: &mut App) {
    // Fold every finished backfill worker's exit report BEFORE the `of_wanted` block below
    // calls `maybe_spawn_backfill`, so a re-walk whose cooldown elapsed can be claimed on this
    // same frame rather than the next one. Non-blocking; empty on all but a handful of frames
    // in a session (one report per walk, ever). The whole decision — retry, give up, or forget
    // — is `BackfillRetries::note_report`, in the CI-gated `vike-app-core`.
    for report in app.bf_done_rx.try_iter() {
        app.bf_retries.note_report(&report);
    }
}

/// Tail phase — (idempotently) subscribe the trade feed and register the orderflow aggregator for
/// every chart window that wants orderflow this frame, then claim its aggTrades backfill.
fn register_orderflow(app: &mut App, of_wanted: Vec<(String, String, String, Option<f64>)>) {
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
        let tick = tick_size.unwrap_or_else(|| bar_agg::nice_orderflow_tick(px));
        app.of_aggs
            .entry(key.clone())
            .or_insert_with(|| (venue.clone(), symbol.clone(), bar_agg::OrderflowAgg::new(tick)));
        // SP3 Task 3: background aggTrades backfill — Binance-only BY DESIGN. Only Binance
        // exposes the aggTrades REST paging `maybe_spawn_backfill` relies on, so a non-Binance
        // orderflow chart is live-only (no historical CVD), mirroring how the trade feeds
        // themselves are live-only. Idempotent via `bf_spawned` (see `maybe_spawn_backfill`'s
        // doc; `of_backfill_hours <= 0.0` is the master off-switch).
        if venue == workspace::DEFAULT_VENUE {
            app.maybe_spawn_backfill(&symbol, &key);
        }
    }
}

/// Tail phase — the Trade windows' side of the frame: forget the held order of every closed
/// window, start each window's depth stream, open Data Manager when a Connect click asked, and
/// remember the instrument the trader last acted in.
fn apply_trade_frame(app: &mut App, ctx: &egui::Context, trade_frame: &mut tool_views::TradeFrame) {
    // Trade windows: start (idempotently) each one's depth stream, open Data Manager when a
    // window's Connect asked for it, and keep the window the trader acted in as the one a new
    // window copies. A CLOSED one forgets its held prompt, so reopening it never offers the old
    // order (M-8).
    tool_views::forget_held_of_closed_windows(&app.wins, &mut app.tool_views);
    for (venue, inst) in &trade_frame.books {
        app.ensure_depth(venue, inst);
    }
    if trade_frame.open_connections {
        // ⚠ Updated 2026-10-05: the standalone Connections window (`WinKind::Connections`) is
        // deleted outright, so a Connect click now opens `WinKind::Data` — landed directly on
        // `DataDest::Credentials` via `SpawnRequest::Kind`'s `data_dest_seed`
        // (`window_spawn::plan_spawn`'s `WinKind::Data` arm carries it out as
        // `SpawnPlan::data_dest_seed`, and `App::apply_spawn` pre-seeds `app.tool_views` from it —
        // the same pre-seed `main.rs`'s startup planner performs for a restored/QA-captured
        // window). A trader clicking Connect to add keys wants to land where credentials are, not
        // on Data Manager's default Overview.
        let kind = workspace::WinKind::Data;
        app.apply_spawn(
            ctx,
            window_spawn::SpawnRequest::Kind {
                kind,
                poly_seed_token: String::new(),
                data_dest_seed: Some(tool_views::DataDest::Credentials),
            },
        );
    }
    trade_frame.remember(&mut app.last_trade);
}

/// Tail phase — Data-manager Delete: stop the series' feeds and aggregators, then sweep every feed
/// no window references any more.
fn teardown_deleted_series(app: &mut App, to_stop: Vec<String>) {
    // Data-manager Delete: drop the render series, stop syncing it from the core snapshot, AND
    // stop the live feed thread itself via per-key unsubscribe. That teardown is NOT spelled here
    // any more — `vike_app_core::ui::feed_lifecycle::stop_series` is the one implementation, shared
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
}

/// Tail phase — the order lane: every window's drained intents become commands (or refusals on
/// their window's strip), sent through the local core or the remote Scope::Write channel; then the
/// QA trade seed, which rides the same dispatch.
fn dispatch_orders(
    app: &mut App,
    fr: &WindowFrame,
    trade_frame: tool_views::TradeFrame,
    account_cancels: Vec<String>,
    cockpit_cmds: Vec<CockpitCmd>,
    opt_orders: Vec<tools::OptOrderTicket>,
    opt_cancels: Vec<String>,
) {
    // The order lane: every window's intents become commands (or refusals on their window's strip).
    // Route order/session commands to the LOCAL core when one exists, else to a remote
    // Scope::Write channel (a `--observe` observer with control enabled). `None`/`None` — a
    // read-only observer, or one still connecting — drops the whole block, a read-only no-op
    // exactly as before this path existed. Every local-core path is byte-identical
    // (`Dispatch::Local` forwards straight to `core.try_command`).
    // Field-path spelling of `app.remote_ctrl()` (disjoint borrows): the whole-`app` borrow
    // a method call takes would pin `app.next_win_n`'s assignment below for `dispatch`'s
    // lifetime.
    let remote_ctrl = app.active_backend.as_ref().and_then(|b| b.ctrl.as_ref());
    // ⚠ The remote arm now carries THIS FRAME'S SNAPSHOT beside the handle, and it is not
    // decoration: it is the client-side ROUTING gate's evidence about the backend (which venues it
    // publishes an engine for — `Portfolio::venues`, one block per engine). Against a backend that
    // does not advertise `FEATURE_VENUE_ROUTING`, a venue in that list is the ONLY venue an order
    // may name, because every other one silently lands on that backend's primary book. The gate is
    // `vike_app_core::backend::tradehub_control::may_send_to_backend`, applied inside
    // `send_to_backend`, which `Dispatch::send` calls; a command it does not send is latched on the
    // handle, and the status strip's control segment shows it.
    let dispatch = match (&app.core, remote_ctrl) {
        (Some(core), _) => Some(Dispatch::Local(core)),
        (None, Some(ctrl)) => Some(Dispatch::Remote(ctrl, &fr.core_snap)),
        (None, None) => None,
    };
    if let Some(dispatch) = dispatch {
        // Local preview caps for the order-entry safety layer, from the POLICY ceiling
        // (the `policy.max_notional_per_order` row, resolved ONCE in `main` —
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
        // local preview refuses — is `vike_app_core::orders::order_dispatch::plan_dispatch`, a PURE
        // function; this file keeps only the I/O (drain in, `Dispatch::send` out, warn the
        // refusals).
        //
        // WHY IT MOVED. The block that used to sit here validated only THREE of its five submit
        // paths (the DOM ladder `Place`, the Polymarket cockpit `Submit`, the deribit options
        // confirm ticket). The **Trade window** — the manual order-entry panel a human types
        // into — called neither `validate` nor `validate_with_multiplier`, and neither did its
        // TP+SL bracket sub-path nor the DOM Close/Reverse exit, so the one MANUAL path had no
        // local notional cap at all. Fixing that here would have been exactly as unverified as
        // the bug: nothing TESTS this file (`justfile`'s `ci_crates` omits this crate — `vike-app`
        // then, `vike-desktop` now — so `just windows-check` skips it too, and
        // `xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI` lists it; the `app-check` job compiles and
        // clippy-gates it and runs none of this logic) — the same reason `order_entry`'s
        // notional-MULTIPLIER bug shipped. So the
        // decision now lives in a crate CI compiles and tests, behind a structural chokepoint
        // plus an exhaustive `SubmitSource` roster; see that module's doc for the gate.
        //
        // Every Trade window intent arrives with its window's OWN venue, account and symbol (the
        // address it drew, `TradeFrame::drain`), never the snapshot's primary market.
        let plan = vike_app_core::orders::order_dispatch::plan_dispatch(
            vike_app_core::orders::order_dispatch::DispatchInputs {
                trade_actions: trade_frame.orders,
                account_cancels,
                cockpit_cmds,
                opt_orders,
                opt_cancels,
            },
            &order_limits,
            &fr.core_snap,
            app.next_win_n,
            app.shot_n,
        );
        app.next_win_n = plan.next_win_n;
        for reject in &plan.rejects {
            tracing::warn!(
                submit_source = reject.source.label(),
                venue = %reject.venue,
                symbol = %reject.symbol,
                account = ?reject.account,
                "order rejected by local preview: {}",
                reject.reason
            );
        }
        // ...and each Trade window's own refusals on its strip (venue + symbol + account).
        tool_views::route_rejects(&app.wins, &mut app.tool_views, &plan.rejects);
        for cmd in plan.commands {
            dispatch.send(cmd);
        }
        // QA (VIKE_TRADE_SEED): seed the Account window with a working order and an open position.
        // `.trader/shots/manifest.json`'s `pipeline-3-golive.png` asks for both by name.
        //
        // ⚠ THE WHOLE DECISION, GUARDS INCLUDED, is `vike_app_core::ui::capture_seed`'s pure
        // `plan_trade_seed_commands` — deliberately NOT here: this file is executed by no test in
        // the workspace (`xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI` names `vike-desktop`), and
        // its paper-only and remote-control guards are the whole of what keeps a marketing
        // screenshot off a real account, so they live where CI runs them. (The DOM's own QA
        // injection kept its guards here, and was deleted with the DOM window.) This block is the
        // I/O: read the price, send, warn.
        //
        // ⚠ The outer `if` is REDUNDANT with `SeedInputs::armed` on purpose, and it is not a
        // belt-and-braces guard — it is what keeps an UNSET knob byte-identical. Gathering the
        // inputs costs a `format!`-built series key and a map lookup, and this runs every frame of
        // every ordinary desktop session; paying that sixty times a second for a knob nobody set
        // is a behaviour change, small but real. The field stays because the pure function is the
        // authority on the whole ladder and its tests drive that arm.
        if app.trade_seed_pending {
            let seed = vike_app_core::ui::capture_seed::plan_trade_seed_commands(
                &vike_app_core::ui::capture_seed::SeedInputs {
                    armed: app.trade_seed_pending,
                    frame: app.shot_n,
                    remote_control: app.remote_ctrl().is_some(),
                    venue_is_live: app.live_venues.contains(workspace::DEFAULT_VENUE),
                    last_close: vike_app_core::ui::capture_seed::fill_clock_close(&app.charts),
                    venue: workspace::DEFAULT_VENUE,
                },
                &order_limits,
                &fr.core_snap,
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
    } else {
        // No control channel: nothing leaves, and every Trade window that tried says so.
        tool_views::refuse_unsent(&app.wins, &mut app.tool_views, &trade_frame.orders);
    }
}

/// Tail phase — QA (`VIKE_CHART_DRAW`): write the capture drawings into every chart series that has
/// bars. Outside [`dispatch_orders`] on purpose: a drawing is not an order and needs no core.
fn apply_chart_draw_capture(app: &mut App) {
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
            drawn |= vike_app_core::ui::capture_seed::apply_drawings(state);
        }
        app.chart_draw_pending = !drawn;
    }
}

/// Tail phase — the DataSets (Symbols tab) mutations: upsert a saved set, delete one, and open the
/// chart a "Test symbol" click asked for.
fn apply_dataset_mutations(
    app: &mut App,
    ctx: &egui::Context,
    ds_save: Option<datasets::DataSet>,
    ds_del: Option<String>,
    ds_open: Option<String>,
) {
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
        app.apply_spawn(ctx, window_spawn::SpawnRequest::TestSymbol { symbol: sym });
    }
}

/// Tail phase — the Data Manager "Stored" view's OUT actions: the per-series delete (which now only
/// refuses and forces a reload), Open in chart, the refresh, and the bulk backfill spawn.
fn apply_stored_view_actions(
    app: &mut App,
    ctx: &egui::Context,
    stored_deletes: Vec<(String, String, String, Option<String>)>,
    stored_opens: Vec<(String, String, Option<String>)>,
    mut stored_refresh_requested: bool,
    stored_backfills: Vec<vike_data_manager::SeriesKey>,
) {
    // Data Manager "Stored" view (Task 3): per-series Delete. ⚠ It deletes NOTHING now — see the
    // tombstone inside the loop. What survives is the mode gate's refusal and the forced reload,
    // so a stale click is logged rather than acted on.
    if !stored_deletes.is_empty() {
        // The mode gate (the #1378 seam close): `delete_series` acts on the LOCAL store, and
        // in remote mode that is not the store the grid is showing — the UI grays Delete
        // (`stored_mode`'s reason as hover text) so nothing should arrive here; if a stale
        // click does, refuse it loudly rather than damaging the wrong store.
        let stored_delete_gate = vike_app_core::data::stored_mode::stored_mode(
            app.resolved_datahub_addr().as_deref(),
            // Only `delete_unavailable` is read here, and the coverage answer cannot change it.
            vike_app_core::data::stored_mode::RemoteCoverage::Unknown,
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
    // `crates/vike-app-core/src/ui/window_spawn.rs`'s `SpawnRequest` carries the argument for
    // each, and this is the ONE spawn site that overrides a window's venue at all.
    for (venue, symbol, interval) in stored_opens {
        let req = window_spawn::SpawnRequest::StoredOpen { venue, symbol, interval };
        app.apply_spawn(ctx, req);
    }
    if stored_refresh_requested && !app.stored_loading {
        app.refresh_stored(ctx);
    }
    // dm-bulk-backfill: spawn the background kline backfill for this frame's bulk
    // Backfill/Update selection, if any (see `App::maybe_spawn_stored_backfill`'s doc; a
    // no-op if a run is already in flight or the drained selection was empty).
    if !stored_backfills.is_empty() {
        app.maybe_spawn_stored_backfill(ctx, stored_backfills);
    }
}

/// Tail phase — the Connections tool's backend OUT slots: the picker's Connect / Disconnect click
/// (and the registry's `active` pointer that follows it), then the Backends editor's confirmed
/// Add / Edit / Delete.
fn apply_backend_actions(
    app: &mut App,
    ctx: &egui::Context,
    backend_action: Option<vike_app_core::backend::backend_conn::BackendAction>,
    backend_registry_update: Option<vike_app_core::backend::backend_editor::RegistryUpdate>,
) {
    // Connections backend picker (split-plane B1): apply this frame's Connect/Disconnect
    // click — the backend-session-clear switch routine lives in `vike_app_core::backend::backend_conn`.
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
            vike_app_core::backend::backend_conn::active_after(&action, &app.backends.backends);
        app.apply_backend_action(ctx, action);
        if app.backends.active != next_active {
            app.backends.active = next_active;
            if let Err(e) = vike_app_core::backend::backend_registry::save(&app.backends) {
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
        let new_file = vike_app_core::backend::backend_editor::apply_registry_update(
            upd,
            || {
                app.apply_backend_action(
                    ctx,
                    vike_app_core::backend::backend_conn::BackendAction::Disconnect,
                )
            },
            |file| {
                if let Err(e) = vike_app_core::backend::backend_registry::save(file) {
                    tracing::warn!("backends.json save failed: {e}");
                }
            },
        );
        app.backends = new_file;
    }
}

/// Tail phase — the backend-settings section's fetch, the node's directory fetch, and the WRITE
/// half: the frame-local edit flow back onto `app`, then this frame's accepted Save onto its
/// worker thread.
///
/// `ctx` is taken BY VALUE (the last use of the frame's owned handle) so the directory fetch below
/// keeps the spelling `app.spawn_directory_fetch(&ctx)` that
/// `crates/vike-desktop/tests/app_local_core_gate.rs`'s `DIRECTORY_FETCH` pins inside its
/// `if tool_views::directory_due(` block.
fn drain_backend_settings_and_directory(
    app: &mut App,
    ctx: egui::Context,
    fr: &WindowFrame,
    backend_settings_refresh: bool,
    backend_settings_edit: vike_app_core::ui::tool_views::SettingsEditState,
    backend_settings_write: Option<vike_app_core::ui::tool_views::SettingsWriteRequest>,
) {
    // Backend-settings section (split-plane REQ-7, read half): start this frame's requested
    // fetch — the section's own auto-fetch (first sight of a backend), a Refresh click, or
    // the post-save refetch the write drain above requested.
    if backend_settings_refresh {
        app.spawn_backend_settings_fetch(&ctx);
    }
    // The node's directory (venue names, the account list): once per backend, then at most once a
    // minute while a Trade window is open — `tool_views::directory_due` is the whole rule.
    let now = std::time::Instant::now();
    if tool_views::directory_due(&app.directory, fr.active_addr.as_deref(), &app.wins, now) {
        app.spawn_directory_fetch(&ctx);
    }
    // …and the WRITE half: the frame-local edit flow back onto `app`, then this frame's
    // accepted Save (if any) onto its worker thread.
    app.backend_settings_edit = backend_settings_edit;
    if let Some(req) = backend_settings_write {
        app.spawn_backend_settings_write(&ctx, req);
    }
}

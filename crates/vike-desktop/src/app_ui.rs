//! The frame: the crate's one `impl eframe::App for App` (`ui`, `on_exit`, `persist_egui_memory`)
//! and `ui`'s per-frame body.
//!
//! `ui` is one ~2100-line frame body, split into named pieces. Two small blocks stay INLINE in
//! `ui` itself, because each is a QA hook over the process environment rather than a region of the
//! window:
//!
//! 1. the headless self-screenshot capture (`VIKE_SHOT` / `VIKE_APPMAX` / `VIKE_SHOT_FRAME`), and
//! 2. the first-frame window arrangement's seven-knob `initial_arrange::ArrangeEnv` construction
//!    (`VIKE_ARRANGE` / `VIKE_TOOL` / `VIKE_TOOLS` / `VIKE_CAL_PAGE` / `VIKE_MAX` / `VIKE_MIN` /
//!    `VIKE_POLY_COCKPIT_TOKEN`).
//!
//! ⚠ Neither block READS the environment: both consult `App::env`, the [`GuiEnv`] `main.rs` read
//! once. `crates/vike-ops/tests/settings_secrets/settings_registry/observe.rs`'s `layer_for`
//! classifies every `env::var` read by FILE PATH — only `main.rs` / `src/bin/*` / `examples/*` earn
//! `Layer::Binary` — so a raw read in THIS file would silently become a `Layer::Library` row, which
//! the shrink-only `LIBRARY_PIN` ratchet refuses.
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
use vike_chart::ScaleMode;

mod chart_pane;
mod chrome;
mod orders;
mod post_loop;
mod tool_pane;
mod window_frame;

use chart_pane::draw_chart_window;
use chrome::{apply_menu_actions, draw_min_rail, draw_save_layout_dialog, draw_status_bar};
use orders::dispatch_orders;
use post_loop::{
    apply_backend_actions, apply_chart_draw_capture, apply_dataset_mutations,
    apply_stored_view_actions, drain_backend_settings_and_directory, ensure_wanted_feeds,
    register_orderflow, seed_charts_from_store, spawn_cloned_windows, teardown_deleted_series,
};
use tool_pane::{apply_window_controls, draw_tool_window};
use window_frame::{WindowFrame, WindowOut, retile_on_maximize, window_frame};

/// ⚠ **The crate's ONE `impl eframe::App for App`, and it has to stay that way.** Coherence allows
/// a type exactly one impl per trait, so a lifecycle hook can have its BODY in another file but not
/// its `fn` — `app_lifecycle.rs` learned that the hard way (see its module doc) and now exposes the
/// teardown as an inherent `App::run_bounded_teardown` this block forwards to. Splitting `App`'s
/// INHERENT methods across files stays fine, which is what `app_methods.rs` does.
impl eframe::App for App {
    fn persist_egui_memory(&self) -> bool {
        false // we own window placement (cascade + arrange); don't restore stale positions
    }

    /// Window-close teardown. One line by design: the whole sequence — the cooperative signal, the
    /// parallel per-venue feed shutdowns, the load-bearing sequential tail and the 1500 ms deadline
    /// that bounds all of it — is [`App::run_bounded_teardown`] in `app_lifecycle.rs`, which is
    /// what every `on_exit` citation in this crate actually points at.
    fn on_exit(&mut self) {
        self.run_bounded_teardown();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // ⚠ THIS METHOD IS A SPLIT, NOT A STUB — see this module's doc for the whole argument.
        // The ~2000-line frame body lives in `frame_begin`, `draw_chrome` and
        // `draw_windows`; what stays inline here are the two blocks that consult the PROCESS
        // ENVIRONMENT — through `self.env`, the `GuiEnv` `main` read once, never `std::env`
        // (`crates/vike-ops/tests/settings_secrets/settings_registry/observe.rs`'s `layer_for`
        // classifies a read by FILE PATH, and only `main.rs` earns `Layer::Binary`).
        //
        // The five statements are in the ORDER the single body ran them, and the order is
        // load-bearing in one place worth naming: `draw_chrome` is what writes `self.desktop` (the
        // `CentralPanel` rect at its tail), and island 2's guard READS it — so the arrangement
        // island can never move above that call.
        //
        // The frame-rate cap (vsync off only) throttles FIRST: the sleep delays the whole frame, so
        // a repaint loop is held to the cap without any phase below knowing about it.
        if let Some(cap) = &mut self.frame_cap {
            cap.throttle();
        }
        // ── OPT-IN frame-rate instrument (`VIKE_FRAME_LOG=1`) ──────────────────────────────────
        //
        // MEASURED 2026-08-31: the same idle chart costs 10.9–12.5% CPU natively on a GPU and
        // 124–275% under lavapipe in a container. Varying the resolution ruled fill rate OUT — 4.7x
        // the pixels bought only ~1.3x the CPU, with a large floor at 640x480 — so the cost is
        // PER-FRAME, and the open question is how many frames each environment actually draws.
        // egui is reactive and nothing in this workspace requests a repaint per frame, so a high
        // rate would mean the presentation path is not throttling, not that the app asked for it.
        //
        // Off unless the variable is set, so a normal run pays one comparison per frame and prints
        // nothing. It is a MEASUREMENT hook, not a feature: it answers "how many frames" and makes
        // no attempt to cap anything. The EXACT string `"1"` — see `GuiEnv::frame_log`.
        if self.env.frame_log.as_deref() == Some("1") {
            use std::sync::atomic::{AtomicU64, Ordering};
            use std::time::Instant;
            static FRAMES: AtomicU64 = AtomicU64::new(0);
            static WINDOW: std::sync::OnceLock<std::sync::Mutex<Instant>> =
                std::sync::OnceLock::new();
            let n = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
            let start = WINDOW.get_or_init(|| std::sync::Mutex::new(Instant::now()));
            if let Ok(mut t0) = start.lock() {
                let dt = t0.elapsed();
                if dt.as_secs_f64() >= 1.0 {
                    tracing::info!(
                        frames = n,
                        fps = format!("{:.1}", n as f64 / dt.as_secs_f64()),
                        "frame rate"
                    );
                    FRAMES.store(0, Ordering::Relaxed);
                    *t0 = Instant::now();
                }
            }
        }

        let (ctx, snap) = frame_begin(self, ui);

        // Headless self-screenshot for parity QA (VIKE_SHOT=path): grab egui's OWN
        // framebuffer after the live feed paints, then exit. Occlusion-proof — no OS
        // capture race, no focus-steal, captures the real wgpu render.
        if let Some(path) = self.env.shot.clone() {
            self.shot_n += 1;
            ctx.request_repaint();
            // QA: VIKE_APPMAX maximizes the whole app mid-run so the self-shot captures the
            // re-tiled (re-filled) workspace — exercises the maximize-transition re-tile path.
            if self.env.appmax.is_some() && self.shot_n == 80 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
            let dbg = format!("{path}.dbg");
            if self.shot_n == 1 {
                // Stay on top + focused: eframe only processes the screenshot readback on a
                // VISIBLE (non-occluded) frame, and VS Code keeps stealing top z-order.
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                    egui::WindowLevel::AlwaysOnTop,
                ));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            // Request periodically once data's loaded: when the foreground loop finally wins
            // a visible frame, eframe processes the pending request and runs its callback.
            let start: u32 =
                self.env.shot_frame.as_deref().and_then(|s| s.parse().ok()).unwrap_or(180);
            if self.shot_n >= start && self.shot_n.is_multiple_of(6) {
                self.request_shot(&ctx);
            }
            if let Some(img) = self.take_shot() {
                let [w, h] = img.size;
                let mut buf = image::RgbaImage::new(w as u32, h as u32);
                for (i, px) in img.pixels.iter().enumerate() {
                    buf.put_pixel(
                        (i % w) as u32,
                        (i / w) as u32,
                        image::Rgba([px.r(), px.g(), px.b(), 255]),
                    );
                }
                let r = buf.save(&path);
                let _ = std::fs::write(&dbg, format!("got {w}x{h}, save_ok={:?}\n", r.is_ok()));
                std::process::exit(0);
            }
            if self.shot_n > start + 900 {
                let _ = std::fs::write(&dbg, "TIMEOUT\n");
                std::process::exit(1);
            }
        }

        // The caption / menus / rail / status bar / desktop rect (`draw_chrome`). `ctx` is
        // CLONED rather than moved because island 2 below still needs it — an `egui::Context` is an
        // `Arc` handle, so this is a refcount bump, and the original body held exactly one such
        // clone per frame (`ui.ctx().clone()`) for the same reason.
        draw_chrome(self, ui, ctx.clone());

        if !self.did_initial_arrange && self.desktop.width() > 600.0 {
            // The whole first-frame decision — the VIKE_TOOL/VIKE_TOOLS QA capture arms, the
            // VIKE_ARRANGE parse and the open-count arrange-vs-maximize-lone choice — moved down
            // into the CI-gated `initial_arrange` (see that module's doc for why it is a planner
            // of its own rather than a seventh `window_spawn` arm). The seven env READS are
            // `main.rs`'s (`GuiEnv`), injected as `ArrangeEnv` — the same shape `StartupEnv`
            // takes at `App::new` — so every `VIKE_*` settings-registry row stays
            // `vike-desktop`/`Binary`.
            let env = initial_arrange::ArrangeEnv {
                arrange: self.env.arrange.clone(),
                tool: self.env.tool.clone(),
                tools: self.env.tools.is_some(),
                cal_page: self.env.cal_page.clone(),
                max: self.env.max.is_some(),
                min: self.env.min.is_some(),
                poly_cockpit_token: self.env.poly_cockpit_token.clone(),
            };
            let plan = initial_arrange::plan_initial_arrange(
                &env,
                &self.wins,
                self.desktop.min,
                self.next_win_n,
            );
            // Applied in the ORDER `InitialArrangePlan` documents — the exact order the inline
            // block performed these steps: close-all, counter, then per spawn feed → push →
            // cal page, then the arrange, the remember, and the two QA hooks last.
            if plan.close_existing {
                for w in self.wins.iter_mut() {
                    w.open = false;
                }
            }
            self.next_win_n = plan.next_win_n;
            for spawn in plan.spawns {
                if let Some(f) = &spawn.ensure_feed {
                    self.ensure_feed_on(&ctx, &f.venue, &f.symbol, &f.interval, f.asset_class);
                }
                let wid = spawn.win.id;
                self.wins.push(spawn.win);
                if let Some(pg) = spawn.cal_page {
                    self.tool_views.entry(wid).or_default().cal_page = pg;
                }
            }
            initial_arrange::apply_arrange_action(&mut self.wins, self.desktop, plan.arrange);
            if let Some(mode) = plan.remember {
                self.last_arrange = Some(mode);
            }
            initial_arrange::apply_qa_hooks(
                &mut self.wins,
                self.desktop,
                plan.qa_maximize_first_open,
                plan.qa_minimize_next_three,
            );
            self.did_initial_arrange = true;
        }

        // The window arena + every post-loop drain (`draw_windows`). Last use of both
        // `ctx` and the frame snapshot, so both move.
        draw_windows(self, ctx, snap);
    }
}

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
        // The cockpit seed is the ONE environment value this arm consults, and it is still
        // seeded ONLY for the cockpit kind. The value is `GuiEnv::poly_cockpit_token`, read in
        // `main.rs`, so `VIKE_POLY_COCKPIT_TOKEN` keeps its single `vike-desktop` /
        // `Layer::Binary` row in `crates/vike-config/src/settings.rs`'s `SETTINGS`; the trim / empty /
        // placeholder ladder is `vike_app_core::ui::startup::poly_cockpit_seed_token`.
        let poly_seed_token = if k == workspace::WinKind::Polymarket {
            startup::poly_cockpit_seed_token(app.env.poly_cockpit_token.as_deref())
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
/// [`retile_on_maximize`], [`window_frame()`] (every read the loop needs, snapshotted once), the
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
        // `DataDest::Gateway` via `SpawnRequest::Kind`'s `data_dest_seed`
        // (`window_spawn::plan_spawn`'s `WinKind::Data` arm carries it out as
        // `SpawnPlan::data_dest_seed`, and `App::apply_spawn` pre-seeds `app.tool_views` from it —
        // the same pre-seed `app_setup.rs`'s startup planner performs for a restored/QA-captured
        // window). A trader clicking Connect to add keys wants to land where credentials are, not
        // on Data Manager's default Overview.
        let kind = workspace::WinKind::Data;
        app.apply_spawn(
            ctx,
            window_spawn::SpawnRequest::Kind {
                kind,
                poly_seed_token: String::new(),
                data_dest_seed: Some(tool_views::DataDest::Gateway),
            },
        );
    }
    trade_frame.remember(&mut app.last_trade);
}

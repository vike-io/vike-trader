//! The window arena's per-frame bundles (`WindowOut`, `WindowFrame`) and the phases that fill them.

use super::*;

/// The collect-then-apply accumulators the window loop fills and [`draw_windows`]'s tail drains.
///
/// Anything that needs the whole `&mut App` — a spawn, a feed ensure, a store call, a command
/// send — cannot run while a window is being drawn, so the per-window phases only RECORD their
/// wants here and the tail applies them in a fixed order. Each field's doc is the comment its
/// declaration carried when it was a local of the one big function.
#[derive(Default)]
pub(super) struct WindowOut {
    /// Cross-venue: (venue, symbol, interval, asset_class) — the primary/compare feeds to
    /// ensure after the loop. Compare overlays and foreign-source study symbols are always
    /// spot (`None` — see their push sites); the primary carries the window's own
    /// `venue`/`asset_class` so a Bybit/OKX chart (or an OKX derivative) subscribes the right
    /// feed (feed-routing slice 1).
    pub(super) to_ensure: Vec<(String, String, String, Option<vike_model::AssetClass>)>,
    /// SP2 orderflow (Task 7): (venue, symbol, chart key, requested tick size) for every OPEN
    /// chart window whose `WinState::orderflow_on()` is true this frame — collected here (pure
    /// reads of `w`) because the actual `&mut App` work (trade subscribe + `of_aggs`
    /// registration) can only happen AFTER the loop (mirrors `to_ensure`'s collect-then-apply
    /// shape). The window's `venue` is carried so orderflow subscribes the RIGHT venue's tape
    /// (venue-aware — any `subscribe_trades` venue, not just Binance). Pushed unconditionally
    /// every frame a toggle is on — cheap, and idempotent downstream (`ensure_trade_feed_on`'s
    /// `spawned` check, `of_aggs`'s `entry`), same as the Trade windows' depth streams.
    pub(super) of_wanted: Vec<(String, String, String, Option<f64>)>,
    /// Chart windows whose title-bar clone button was clicked (indices into `app.wins`).
    pub(super) to_clone: Vec<usize>,
    /// Any title-bar drag this frame → drop the tiling memory.
    pub(super) user_moved_any: bool,
    /// Data-manager "Delete" → drop these feeds after.
    pub(super) to_stop: Vec<String>,
    /// DataSet Save → upsert after the loop.
    pub(super) ds_save: Option<datasets::DataSet>,
    /// The Trade windows' addressed intents, picks, Connect clicks and depth streams — every
    /// decision is `tool_views::TradeFrame`'s, one crate down where CI runs it.
    pub(super) trade_frame: tool_views::TradeFrame,
    /// Account window ✕ → cancel by coid.
    pub(super) account_cancels: Vec<String>,
    /// Polymarket cockpit order intents resolved in the window loop, folded onto the command lane
    /// after it (venue is always "polymarket").
    pub(super) cockpit_cmds: Vec<CockpitCmd>,
    /// Options confirm-ticket → deribit exec.
    pub(super) opt_orders: Vec<tools::OptOrderTicket>,
    /// Options chain marker → deribit cancel (coid).
    pub(super) opt_cancels: Vec<String>,
    /// Cockpit → ensure a polymarket token book+trade stream.
    pub(super) poly_book_reqs: Vec<String>,
    /// DataSet Delete.
    pub(super) ds_del: Option<String>,
    /// "Test symbol" → open a chart after the loop.
    pub(super) ds_open: Option<String>,
    /// Data Manager "Stored" view (Task 3) OUT actions, applied after the window loop below.
    /// Refresh click / first-shown / post-delete reload.
    pub(super) stored_refresh_requested: bool,
    /// (venue, symbol, kind, interval)
    pub(super) stored_deletes: Vec<(String, String, vike_data::StoreKindId, Option<String>)>,
    /// (venue, symbol, interval)
    pub(super) stored_opens: Vec<(String, String, Option<String>)>,
    /// dm-bulk-backfill: grid v2's bulk Backfill/Update selection, drained below and spawned
    /// after the loop via `App::maybe_spawn_stored_backfill` (same deferred-mutation shape).
    pub(super) stored_backfills: Vec<vike_data_manager::SeriesKey>,
    /// Connections backend picker (split-plane B1) OUT slot, applied after the loop via
    /// `App::apply_backend_action` (the same deferred-mutation shape as every action above — the
    /// switch routine needs `&mut App` the loop can't give it).
    pub(super) backend_action: Option<vike_app_core::backend::backend_conn::BackendAction>,
    /// Backends editor (split-plane I2) OUT slot: the drained Add/Edit/Delete registry
    /// update, applied after the loop through `backend_editor::apply_registry_update` (which
    /// pins delete-active's disconnect-BEFORE-save order).
    pub backend_registry_update: Option<vike_app_core::backend::backend_editor::RegistryUpdate>,
    /// Backend-settings section (split-plane REQ-7, read half): the `refresh` OUT slot, drained
    /// after the loop via `App::spawn_backend_settings_fetch` (the same deferred-mutation shape
    /// as `backend_action` above).
    pub(super) backend_settings_refresh: bool,
    /// REQ-7 WRITE half: the edit flow is `app` state (typed into across frames) TAKEN into a
    /// frame-local — the window loop borrows `app` piecemeal, so `&mut App` fields can't cross
    /// into it — and written back after the loop.
    pub(super) backend_settings_edit: vike_app_core::ui::tool_views::SettingsEditState,
    /// `backend_settings_write` is the Save out-slot, drained after the loop into
    /// `spawn_backend_settings_write`.
    pub(super) backend_settings_write: Option<vike_app_core::ui::tool_views::SettingsWriteRequest>,
}

/// The read-only values the window loop and the tail need, each a CHEAP snapshot of an `App` field
/// taken once per frame by [`window_frame()`] before any window draws — the same snapshot POINT the
/// original body had, and for the same reason (the per-window phases hold `w` and cannot re-read
/// `app` for these without seeing a different value mid-frame).
pub(super) struct WindowFrame {
    pub(super) desktop: egui::Rect,
    /// The paper trading state for the Trade window.
    pub(super) core_snap: Arc<vike_exec::CoreSnapshot>,
    pub(super) td: tools::ToolData,
    /// `TextureHandle` clones are Arc-cheap.
    pub(super) flags: HashMap<String, egui::TextureHandle>,
    pub(super) logos: HashMap<String, egui::TextureHandle>,
    /// Small; cloned so `app.datasets` is free to mutate after.
    pub(super) dsets: datasets::Store,
    /// (key "SYM@iv", bar count, first bar open-ms, last bar open-ms) — Data-manager columns.
    pub(super) feeds: Vec<(String, usize, i64, i64)>,
    /// vike maximize = fill the workspace by HIDING every other live window.
    pub(super) maxed_idx: Option<usize>,
    /// WHICH BOX the connected daemon says it is (`None` from a node that predates the field,
    /// before the first frame, or between a drop and the next connection).
    pub(super) daemon_self_report: Option<vike_tradehub_client::wire::WireNodeIdentity>,
    /// The ONE datahub resolution (REQ-2) of this frame.
    pub(super) resolved_datahub_addr: Option<String>,
    pub(super) datahub_key_name: String,
    /// The CHART-SEED sentence, snapshotted once per frame.
    pub(super) chart_seed_note: Option<String>,
    /// The ACTIVE backend's backend-settings slot state (`Idle` when the slot belongs to another).
    pub(super) backend_settings_view: vike_app_core::ui::tool_views::BackendSettingsState,
    pub(super) last_trade: Option<tool_views::TradePick>,
    pub(super) active_addr: Option<String>,
    pub(super) directory: Option<Arc<vike_tradehub_client::wire::WireDirectory>>,
    pub(super) directory_unavailable: bool,
    pub(super) control_link: tool_views::ControlLink,
    pub(super) now_ms: i64,
    pub(super) display_tz: DisplayTz,
    pub(super) gpu_render: bool,
    pub(super) gpu_ok: bool,
}

/// Window phase 1 — the maximize-edge re-tile.
pub(super) fn retile_on_maximize(app: &mut App, ctx: &egui::Context) {
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
pub(super) fn window_frame(app: &mut App, out: &mut WindowOut) -> WindowFrame {
    let desktop = app.desktop;
    let core_snap = app.snap_cell.load_full(); // the paper trading state for the Trade window
    let td = app.tools.lock().unwrap().clone();
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
    // GPU candle layer (GPU Phase 2, Task 3): Copy; read once, same up-front-copy shape as
    // `display_tz` above — every chart window's `ChartInputs::gpu_candles` wiring reads the pair
    // through the frame.
    let gpu_render = app.gpu_render;
    let gpu_ok = app.gpu_ok;
    WindowFrame {
        desktop,
        core_snap,
        td,
        flags,
        logos,
        dsets,
        feeds,
        maxed_idx,
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
        gpu_render,
        gpu_ok,
    }
}

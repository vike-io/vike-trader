//! The window arena's per-frame bundles (`WindowFrameOutput`, `WindowFrame`) and the phases that fill them.

use super::*;

/// The collect-then-apply accumulators the window loop fills and [`draw_windows`]'s tail drains.
///
/// Anything that needs the whole `&mut App` — a spawn, a feed ensure, a store call, a command
/// send — cannot run while a window is being drawn, so the per-window phases only RECORD their
/// wants here and the tail applies them in a fixed order. Each field's doc is the comment its
/// declaration carried when it was a local of the one big function.
#[derive(Default)]
pub(super) struct WindowFrameOutput {
    /// Cross-venue: (venue, symbol, interval, asset_class) — the primary/compare feeds to
    /// ensure after the loop. Compare overlays and foreign-source study symbols are always
    /// spot (`None` — see their push sites); the primary carries the window's own
    /// `venue`/`asset_class` so a Bybit/OKX chart (or an OKX derivative) subscribes the right
    /// feed (feed-routing slice 1).
    pub(super) to_ensure: Vec<startup::SeriesSpec>,
    /// SP2 orderflow (Task 7): (venue, symbol, chart key, requested tick size) for every OPEN
    /// chart window whose `WindowState::orderflow_on()` is true this frame — collected here (pure
    /// reads of `w`) because the actual `&mut App` work (trade subscribe + `orderflow_aggregators`
    /// registration) can only happen AFTER the loop (mirrors `to_ensure`'s collect-then-apply
    /// shape). The window's `venue` is carried so orderflow subscribes the RIGHT venue's tape
    /// (venue-aware — any `subscribe_trades` venue, not just Binance). Pushed unconditionally
    /// every frame a toggle is on — cheap, and idempotent downstream (`ensure_trade_feed_on`'s
    /// `spawned` check, `orderflow_aggregators`'s `entry`), same as the Trade windows' depth streams.
    pub(super) orderflow_wanted: Vec<(String, String, String, Option<f64>)>,
    /// Chart windows whose title-bar clone button was clicked (indices into `app.windows`).
    pub(super) to_clone: Vec<usize>,
    /// Any title-bar drag this frame → drop the tiling memory.
    pub(super) user_moved_any: bool,
    /// Data-manager "Delete" → drop these feeds after.
    pub(super) to_stop: Vec<String>,
    /// DataSet Save → upsert after the loop.
    pub(super) symbol_set_save: Option<datasets::SymbolSet>,
    /// The Trade windows' addressed intents, picks, Connect clicks and depth streams — every
    /// decision is `windows::TradeWindowFrame`'s, one crate down where CI runs it.
    pub(super) trade_frame: windows::TradeWindowFrame,
    /// Account window ✕ → cancel by order client id.
    pub(super) account_cancels: Vec<String>,
    /// Polymarket cockpit order intents resolved in the window loop, folded onto the command lane
    /// after it (venue is always "polymarket").
    pub(super) cockpit_commands: Vec<CockpitCommand>,
    /// Options confirm-ticket → deribit exec.
    pub(super) opt_orders: Vec<vike_order_entry::option_chain::ticket::OptionChainTicket>,
    /// Options chain marker → deribit cancel (order client id).
    pub(super) opt_cancels: Vec<String>,
    /// Cockpit → ensure a polymarket token book+trade stream.
    pub(super) polymarket_book_requests: Vec<String>,
    /// DataSet Delete.
    pub(super) symbol_set_delete: Option<String>,
    /// "Test symbol" → open a chart on this series (the DataSet's provider and interval) after the loop.
    pub(super) symbol_set_open: Option<startup::SeriesSpec>,
    /// Data Manager "Stored" view (Task 3) OUT actions, applied after the window loop below.
    /// Refresh click / first-shown / post-delete reload.
    pub(super) stored_refresh_requested: bool,
    /// (venue, symbol, kind, interval)
    pub(super) stored_deletes: Vec<(String, String, vike_model::StoreKindId, Option<String>)>,
    /// (venue, symbol, interval)
    pub(super) stored_opens: Vec<(String, String, Option<String>)>,
    /// dm-bulk-backfill: grid v2's bulk Backfill/Update selection, drained below and spawned
    /// after the loop via `App::maybe_spawn_stored_backfill` (same deferred-mutation shape).
    pub(super) stored_backfills: Vec<vike_data_manager::DataManagerInventorySeriesKey>,
    /// Connections backend picker (split-plane B1) OUT slot, applied after the loop via
    /// `App::apply_backend_action` (the same deferred-mutation shape as every action above — the
    /// switch routine needs `&mut App` the loop can't give it).
    pub(super) backend_action: Option<vike_app_core::backend::connection::BackendAction>,
    /// Backends editor (split-plane I2) OUT slot: the drained Add/Edit/Delete registry
    /// update, applied after the loop through `editor::apply_registry_update` (which
    /// pins delete-active's disconnect-BEFORE-save order).
    pub(super) backend_registry_update:
        Option<vike_app_core::backend::editor::BackendRegistryUpdate>,
    /// Backend-settings section (split-plane REQ-7, read half): the `refresh` OUT slot, drained
    /// after the loop via `App::spawn_backend_settings_fetch` (the same deferred-mutation shape
    /// as `backend_action` above).
    pub(super) backend_settings_refresh: bool,
    /// REQ-7 WRITE half: the edit flow is `app` state (typed into across frames) TAKEN into a
    /// frame-local — the window loop borrows `app` piecemeal, so `&mut App` fields can't cross
    /// into it — and written back after the loop.
    pub(super) backend_settings_edit: windows::BackendSettingsEditState,
    /// `backend_settings_write` is the Save out-slot, drained after the loop into
    /// `spawn_backend_settings_write`.
    pub(super) backend_settings_write: Option<windows::BackendSettingsWriteRequest>,
}

/// The read-only values the window loop and the tail need, each a CHEAP snapshot of an `App` field
/// taken once per frame by [`window_frame()`] before any window draws — the same snapshot POINT the
/// original body had, and for the same reason (the per-window phases hold `w` and cannot re-read
/// `app` for these without seeing a different value mid-frame).
pub(super) struct WindowFrame {
    pub(super) desktop: egui::Rect,
    /// The paper trading state for the Trade window.
    pub(super) core_snapshot: Arc<vike_execution::CoreSnapshot>,
    pub(super) tool_feeds: vike_datahub_client::tool_feeds::poll::DatahubPollSnapshot,
    /// `TextureHandle` clones are Arc-cheap.
    pub(super) country_flags: HashMap<String, egui::TextureHandle>,
    pub(super) logos: HashMap<String, egui::TextureHandle>,
    /// Small; cloned so `app.symbol_set_store` is free to mutate after.
    pub(super) symbol_set_store: datasets::SymbolSetStore,
    /// (key "SYM@iv", bar count, first bar open-ms, last bar open-ms) — Data-manager columns.
    pub(super) feeds: Vec<(String, usize, i64, i64)>,
    /// WHICH BOX the connected daemon says it is (`None` from a node that predates the field,
    /// before the first frame, or between a drop and the next connection).
    pub(super) daemon_self_report: Option<vike_tradehub_client::wire::HubIdentityWire>,
    /// What the observe frame says beyond the snapshot (`BackendObserveBridgeHandle::node_words`); empty on a fresh
    /// link.
    pub(super) node_words: Arc<vike_tradehub_client::node_words::BackendNodeWords>,
    /// The ONE datahub resolution (REQ-2) of this frame.
    pub(super) resolved_datahub_address: Option<String>,
    /// The CHART-SEED sentence, snapshotted once per frame.
    pub(super) chart_seed_note: Option<String>,
    /// The ACTIVE backend's backend-settings slot state (`Idle` when the slot belongs to another).
    pub(super) backend_settings_view: windows::BackendSettingsState,
    pub(super) last_trade: Option<windows::TradeWindowPick>,
    pub(super) active_address: Option<String>,
    pub(super) directory: Option<Arc<vike_tradehub_client::wire::AccountDirectoryWire>>,
    pub(super) directory_unavailable: bool,
    pub(super) control_link: vike_app_core::backend::tradehub_control::BackendControlLink,
    /// Whether the connected node carries a post-only order (`node_takes_post_only`).
    pub(super) node_takes_post_only: bool,
    pub(super) now_ms: i64,
    pub(super) display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
    /// `ThemeAppearanceSession::draws_gpu_candles`: the choice, where a GPU candle path exists.
    pub(super) gpu_render: bool,
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
            window::apply_arrange(&mut app.windows, app.desktop, a);
        }
        app.retile_frames -= 1;
        ctx.request_repaint(); // keep ticking while the OS resize settles
    }
}

/// Window phase 2 — every read the window loop and the tail need, snapshotted ONCE before any
/// window draws: the app-state clones, the datahub resolution (with the three side effects that
/// point the market-data session and the venue catalog at it), the backend-settings slot, and the
/// Trade windows' inputs. The statements run in the order the one big body ran them; the only
/// writes to [`WindowFrameOutput`] are the two the backend-settings write-result fold makes.
pub(super) fn window_frame(app: &mut App, out: &mut WindowFrameOutput) -> WindowFrame {
    let desktop = app.desktop;
    let core_snapshot = app.snapshot_cell.load_full(); // the paper trading state for the Trade window
    let tool_feeds = app.tools.lock().unwrap().clone();
    let country_flags = app.country_flags.clone(); // TextureHandle clones are Arc-cheap
    let logos = app.news_logos.clone();
    // The server's DataSets, once the datahub answered (`SymbolSetsLink::poll`); a refusal keeps what is shown.
    match app.symbol_sets.poll() {
        Some(Ok(store)) => app.symbol_set_store = store,
        Some(Err(e)) => tracing::warn!("datasets: {e}"),
        None => {}
    }
    let symbol_set_store = app.symbol_set_store.clone(); // small; cloned so app.symbol_set_store is free to mutate after
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
    // WHICH BOX the connected daemon says it is — taken ONCE per frame before the windows loop
    // (which borrows `app` piecemeal), exactly like `resolved_datahub_address` below. It is the one
    // identity fact this side cannot derive: both production listeners bind loopback, so this
    // process reaches them down an SSH tunnel and its own socket address is the tunnel mouth —
    // identical on every box, for every daemon. `None` from a node that predates the field, before
    // the first frame, or between a drop and the next connection: the bridge clears it on a link
    // drop like the rest of the identity, so a stale box name cannot outlive its connection.
    let daemon_self_report = app.active_backend.as_ref().and_then(|b| b.bridge.identity());
    let node_words = app.active_backend.as_ref().map(|b| b.bridge.node_words()).unwrap_or_default();
    // The ONE datahub resolution (REQ-2), taken once per frame BEFORE the windows loop (the
    // loop borrows `app` piecemeal): explicit `config.datahub_addr` first, else the active
    // backend's Welcome advertisement — see `App::resolved_datahub_address`.
    let resolved_datahub_address = app.resolved_datahub_address();
    // ...and the market-data session is pointed at it HERE rather than at `App::new`, because at
    // `App::new` the answer is not known: the second rung of that resolution is the ACTIVE
    // backend's `Welcome` advertisement, which lands only after the observe bridge handshakes.
    // An equal value is a compare and a return, so the per-frame cost is one uncontended `Mutex`.
    app.market_data_session.set_address(resolved_datahub_address.as_deref());
    // ...and the feeds ARE the venues that datahub advertises (`MarketDataSession::sync_mount`: none until
    // the reconciler's handshake probe lands), so a series ensured before that is re-asked now.
    // Both are a lock and a scan on the steady state. Before every `ensure_*` below, in this order.
    app.market_data_session.sync_mount(&mut app.feeds, &mut app.feed_statuses);
    feed_lifecycle::retry_unroutable(&mut app.feed_slots());
    // The KEY follows too: this PC's device key, which the Server group may make mid-session.
    app.market_data_session.follow_device_key();
    // ...and the Data Manager's venue-catalog route is pointed at the same datahub, for the same
    // reason and with the same `None`-holds rule (`DatahubCatalogRefresh::dial_is_stale` states both).
    // ⚠ The guard is not an optimisation: RESOLVING a dial opens the credential store, so the
    // resolution runs only when the address or the device key actually changes — which is a handful
    // of frames in a session. `catalog_dial` (over `resolve_datahub_dial`) is the `chart_seed_dial`
    // twin and lives in `main.rs` because only a binary holds the boot walk.
    if app.catalog.dial_is_stale(resolved_datahub_address.as_deref()) {
        let dial = resolved_datahub_address.as_deref().map(|a| catalog_dial(a.to_string()));
        app.catalog.set_dial(dial);
    }
    // ...and the DataSets follow the same dial: a new one reads the server's lists once.
    app.symbol_sets.follow(app.catalog.dial_snapshot());
    // The CHART-SEED sentence, snapshotted once per frame for the same reason as the two above: the
    // windows loop borrows `app` piecemeal, and a lock taken per window would be N uncontended
    // locks for one string. Written by the background read thread (`stored::chart_seed::ChartSeedDial::run`), so
    // this frame paints the PREVIOUS batch's answer — which is right, since a batch that is still in
    // flight has no answer yet and the thread requests a repaint when it does.
    let chart_seed_note: Option<String> = app.chart_seed_note.lock().ok().and_then(|n| n.clone());
    // Backend-settings section (split-plane REQ-7, read half): the ACTIVE backend's slot
    // state this frame — `Idle` when the slot belongs to another (switched-away) backend, so
    // stale rows never render and the section auto-refetches. The `refresh` OUT slot is
    // drained after the loop via `App::spawn_backend_settings_fetch` (the same
    // deferred-mutation shape as `WindowFrameOutput::backend_action`).
    let backend_settings_view: windows::BackendSettingsState = {
        match app.active_backend.as_ref().map(|b| b.record.address.as_str()) {
            Some(address) => {
                let slot = app.backend_settings.lock().unwrap();
                if slot.0 == address { slot.1.clone() } else { Default::default() }
            }
            None => Default::default(),
        }
    };
    // REQ-7 WRITE half: the edit flow is `app` state (typed into across frames) TAKEN into a
    // frame-local (`WindowFrameOutput::backend_settings_edit`) — the window loop can't hold `&mut App`
    // fields across a window — and written back after the loop. `backend_settings_write` is the
    // Save out-slot, drained after the loop into `spawn_backend_settings_write`.
    out.backend_settings_edit = std::mem::take(&mut app.backend_settings_edit);
    // Fold a finished write first (the worker parked the already-folded flow state, keyed by
    // addr): still-active backend ⇒ show it, and a SAVED write refetches the table so the new
    // file value renders beside the restart note; switched-away ⇒ dropped, never painted
    // under the new backend (the fetch slot's discipline).
    if let Some((address, folded)) = app.backend_settings_write_result.lock().unwrap().take() {
        if app.active_backend.as_ref().is_some_and(|b| b.record.address == address) {
            if windows::should_refetch_after_write(&folded) {
                out.backend_settings_refresh = true;
            }
            out.backend_settings_edit = folded;
        } else {
            out.backend_settings_edit = Default::default();
        }
    }
    // What the Trade windows read this frame, taken once before the loop holds `app.windows`: the
    // window a new one copies (minor 26) and the ACTIVE backend's directory (venue names, accounts).
    let last_trade = app.last_trade.clone();
    let active_address = app.active_backend.as_ref().map(|b| b.record.address.clone());
    let directory = windows::account_directory_reply(&app.directory, active_address.as_deref());
    // ...whether that directory's fetch ended with no list (item 5), and whether this desktop can
    // send at all (item 11): the windows say both up front.
    let directory_unavailable =
        windows::account_directory_unavailable(&app.directory, active_address.as_deref());
    let control_link = vike_app_core::backend::tradehub_control::BackendControlLink::of(
        app.tradehub_control().map(|c| c.is_connected()),
    );
    // ...and whether the node it sends to carries a post-only order: the ticket's Post only is greyed
    // with that reason while it does not (the node's own gate, asked of the order the ticket would send).
    let node_takes_post_only = app.tradehub_control().is_some_and(|c| {
        vike_tradehub_client::command_lift::node_takes_post_only(|wire| c.takes(wire))
    });
    let now_ms = chrono::Local::now().timestamp_millis();
    // Copies, read once: the clocks and tool bodies' zone, and every chart window's
    // `ChartInputs::gpu_candles` wiring.
    let display_time_zone = app.appearance.display_time_zone();
    let gpu_render = app.appearance.draws_gpu_candles();
    WindowFrame {
        desktop,
        core_snapshot,
        tool_feeds,
        country_flags,
        logos,
        symbol_set_store,
        feeds,
        daemon_self_report,
        node_words,
        resolved_datahub_address,
        chart_seed_note,
        backend_settings_view,
        last_trade,
        active_address,
        directory,
        directory_unavailable,
        control_link,
        node_takes_post_only,
        now_ms,
        display_time_zone,
        gpu_render,
    }
}

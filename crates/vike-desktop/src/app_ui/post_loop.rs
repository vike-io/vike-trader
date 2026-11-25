//! The post-loop tail: clones, feed ensures, store seeding, orderflow, teardown and the drains.

use super::*;

/// Tail phase — open the windows the chart title bars' clone buttons asked for (`to_clone` holds
/// indices into `app.windows`, which is back in place by now).
pub(super) fn spawn_cloned_windows(app: &mut App, ctx: &egui::Context, to_clone: Vec<usize>) {
    for wi in to_clone {
        // The source read stays a scoped borrow: `apply_spawn` takes `&mut App`, and pushing
        // only APPENDS, so `wi` stays valid for the remaining clones exactly as before. The clone
        // keeps all three, venue included (`window::spawn`'s module doc).
        let (venue, symbol, interval) = {
            let src = &app.windows[wi];
            (src.venue.clone(), src.symbol.clone(), src.interval.clone())
        };
        let req = spawn::WindowSpawnRequest::CloneWindow { venue, symbol, interval };
        app.apply_spawn(ctx, req);
    }
}

/// Tail phase — adopt the series the backend publishes into the charts that follow it, then fan
/// every feed the frame asked for (compare overlays, foreign-source studies, a changed
/// symbol/interval, the adopted series) out to `ensure_feed_on`.
pub(super) fn ensure_wanted_feeds(
    app: &mut App,
    ctx: &egui::Context,
    mut to_ensure: Vec<startup::SeriesSpec>,
) {
    // THE CHART FOLLOWS THE SERIES THE BACKEND PUBLISHES: a chart NOBODY CHOSE and that is
    // rendering NOTHING retargets itself onto a series this node actually has. The whole rule —
    // and the argument for how narrow it is — is `vike_app_core::ui::series_follow::follow_backend`;
    // what belongs here is the wiring: ensure the adopted key (so `spawned` passes it through
    // `sync_from_core`'s fold filter) and force ONE refold, because the bars are already sitting in
    // the snapshot and waiting on the node's next publish would leave the adopted chart blank for
    // a whole cadence. Placed BEFORE `to_ensure` is drained so the adopted series rides the same
    // fan-out as every other subscription this frame.
    let adopted =
        series_follow::follow_backend(&mut app.windows, &app.charts, &app.published_series);
    if !adopted.is_empty() {
        app.last_sequence = core_sync::FORCE_REFOLD;
    }
    to_ensure.extend(adopted);
    for spec in &to_ensure {
        app.ensure_feed_on(ctx, spec);
    }
}

/// Tail phase — ask the backend's store for the interval each empty chart was given (and, on the
/// same thread, the gap arm that asks the SERVER to fetch what the store answers empty for).
pub(super) fn seed_charts_from_store(app: &mut App, ctx: &egui::Context, fr: &WindowFrame) {
    // THE CHART ASKS THE BACKEND'S STORE FOR WHAT IT SHOWS. The daemon streams only the series its
    // mounted strategies hold (the CI box trades 1m), so a 5m/15m/1h/4h/1d chart could never paint
    // anything. The whole rule — kline only, published only left of its live tail, the view at
    // its width, once per view — is `vike_app_core::data::stored::plan_store_reads`, which records
    // each key BEFORE the read is spawned; the wiring belongs here: the resolved address, the dial.
    // Placed AFTER the ensure fan-out so an adopted window's `ChartState` exists for the seed.
    if let (Some(hub), Some(bars)) = (fr.resolved_datahub_address.clone(), app.direct_bars.clone())
    {
        let reads = app.chart_history.plan_reads(&app.windows, &app.charts, &app.published_series);
        if reads.is_empty() {
            return;
        }
        let wake = ctx.clone();
        // ...AND THE GAP ARM: a series the store answers EMPTY for is one the SERVER can be asked
        // to fetch. The whole rule and every message is `vike_app_core::data::stored::chart_seed`; what belongs
        // here is the same wiring the read above needed — the resolved address, the device key
        // this binary already holds, and the slot the note lands in. It runs on
        // the read's own thread, after it (`spawn_store_seed`'s `seed` doc says why one thread).
        //
        // ⚠ The key is this PC's device key and the verb is a READ, which is what makes this reachable
        // at all: `Request::SeedSeries` is `VerbScope::Read`
        // (`docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`), unlike `Backfill`, which
        // this binary never sends to a keyed datahub signed.
        let dial = chart_seed_dial(hub.clone(), app.chart_seed_note.clone());
        stored::spawn_store_seed(
            reads,
            open_history_store_datahub(hub),
            bars,
            app.chart_history.cache(),
            vike_model::now_ms(),
            // How deep an OLDER datahub's read goes: the `preferences.chart_history_bars` row (`main.rs`).
            crate::chart_history_bars(),
            Some(dial),
            move || wake.request_repaint(),
        );
    }
}

/// Tail phase — (idempotently) subscribe the trade feed and register the orderflow aggregator for
/// every chart window that wants orderflow this frame. Live-only: no history is backfilled.
pub(super) fn register_orderflow(
    app: &mut App,
    orderflow_wanted: Vec<(String, String, String, Option<f64>)>,
) {
    // SP2 orderflow (Task 7): (idempotently) subscribe the trade feed + register this
    // chart's aggregator for every window that wants orderflow this frame (collected
    // above as `orderflow_wanted` — see its doc for why this can't happen inside the window loop).
    for (venue, symbol, key, tick_size) in orderflow_wanted {
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
        app.orderflow_aggregators
            .entry(key)
            .or_insert_with(|| (venue.clone(), symbol.clone(), bar_agg::OrderflowAgg::new(tick)));
    }
}

/// Tail phase — Data-manager Delete: stop the series' feeds and aggregators, then sweep every feed
/// no window references any more.
pub(super) fn teardown_deleted_series(app: &mut App, to_stop: Vec<String>) {
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
        // Feed-leak fix: also drop this key's tick/vol (`tick_volume_aggregators`) or orderflow (`orderflow_aggregators`)
        // aggregator so its shared trade tape becomes reapable. The Data-manager delete does
        // NOT remove the window from `app.windows` (it stays with `open=false`), so its key would
        // otherwise still count as live and `reap_orphaned_feeds`'s (2a) would keep the
        // aggregator — hence the explicit removal here. `reap_orphaned_feeds` runs right below
        // and stops the underlying `subscribe_trades` feed iff no OTHER aggregator on the same
        // (venue, symbol) remains (a shared feed is never stopped out from under a live chart).
        // Reached through the `SeriesSlots` bundle rather than through `app`, which the two
        // bundles hold mutably borrowed for this loop; they are the same two fields.
        s.tick_volume_aggregators.remove(&k);
        s.orderflow_aggregators.remove(&k);
    }
    // C2 tidy (FIX 3): sweep any feed no window references anymore (an orphaned Compare
    // symbol, or the last window on that symbol's primary changing away from it) — see
    // `reap_orphaned_feeds`'s doc. Placed right after the manual Data-manager teardown
    // above since it's the same kind of cleanup, just automatic every frame.
    app.reap_orphaned_feeds();
}

/// Tail phase — QA (`VIKE_CHART_DRAW`): write the capture drawings into every chart series that has
/// bars. Outside [`dispatch_orders`] on purpose: a drawing is not an order and needs no core.
pub(super) fn apply_chart_draw_capture(app: &mut App) {
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
pub(super) fn apply_symbol_sets(
    app: &mut App,
    ctx: &egui::Context,
    symbol_set_save: Option<datasets::SymbolSet>,
    symbol_set_delete: Option<String>,
    symbol_set_open: Option<startup::SeriesSpec>,
) {
    // DataSets (Symbols tab) mutations — deferred here so `app` is not borrowed by the window loop. Sent to the
    // datahub, whose answer (every list after the act) lands through `SymbolSetsLink::poll` on a later frame.
    if let Some(mut d) = symbol_set_save
        && !d.name.is_empty()
    {
        d.user = app.symbol_set_store.get(&d.name).map(|x| x.user).unwrap_or(true);
        app.symbol_sets.save(app.catalog.dial_snapshot(), d);
    }
    if let Some(n) = symbol_set_delete {
        app.symbol_sets.delete(app.catalog.dial_snapshot(), n);
    }
    if let Some(series) = symbol_set_open {
        // "Test symbol" → open a chart window for it (the clone's analog of vike's backtest test)
        app.apply_spawn(ctx, spawn::WindowSpawnRequest::TestSymbol { series });
    }
}

/// Tail phase — the Data Manager "Stored" view's OUT actions: the per-series delete (which now only
/// refuses and forces a reload), Open in chart, the refresh, and the bulk backfill spawn.
pub(super) fn apply_stored_view_actions(
    app: &mut App,
    ctx: &egui::Context,
    stored_deletes: Vec<(String, String, vike_model::StoreKindId, Option<String>)>,
    stored_opens: Vec<(String, String, Option<String>)>,
    mut stored_refresh_requested: bool,
    stored_backfills: Vec<vike_data_manager::DataManagerInventorySeriesKey>,
) {
    // Data Manager "Stored" view (Task 3): per-series Delete. ⚠ It deletes NOTHING now — see the
    // tombstone inside the loop. What survives is the mode gate's refusal and the forced reload,
    // so a stale click is logged rather than acted on.
    if !stored_deletes.is_empty() {
        // The mode gate (the #1378 seam close): `stored_mode` grays Delete with a datahub
        // resolved and without one (its reason as hover text), so nothing should arrive here;
        // if a stale click does, refuse it loudly rather than act on a store this seat cannot
        // delete from.
        let stored_delete_gate = vike_app_core::data::data_manager::mode::stored_mode(
            app.resolved_datahub_address().as_deref(),
            // Only `delete_unavailable` is read here, and the coverage answer cannot change it.
            vike_app_core::data::data_manager::mode::DatahubCoverage::Unknown,
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
            // took the local data plane), and `delete_series` is not a `HistoryStore` trait verb, so
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
    // `crates/vike-app-core/src/ui/window/spawn.rs`'s `WindowSpawnRequest` carries the argument for
    // each, and this is the ONE spawn site that overrides a window's venue at all.
    for (venue, symbol, interval) in stored_opens {
        let req = spawn::WindowSpawnRequest::StoredOpen { venue, symbol, interval };
        app.apply_spawn(ctx, req);
    }
    if stored_refresh_requested {
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
pub(super) fn apply_backend_actions(
    app: &mut App,
    ctx: &egui::Context,
    backend_action: Option<vike_app_core::backend::connection::BackendAction>,
    backend_registry_update: Option<vike_app_core::backend::editor::BackendRegistryUpdate>,
) {
    // Connections backend picker (split-plane B1): apply this frame's Connect/Disconnect
    // click — the backend-session-clear switch routine lives in `vike_app_core::backend::connection`.
    if let Some(action) = backend_action {
        // The registry's `active` pointer FOLLOWS the picker, so the node picked here is the one a
        // bare launch dials next (`registry::active_record`, read by
        // `connection::startup_observe`). Computed BEFORE the switch, because the decision is about
        // the click and `apply_backend_action` consumes the action.
        //
        // ⚠ Without this the pointer had no writer at all — `backend_editor` only retargets it on a
        // rename and clears it on a delete — so "configure once, launch bare afterwards" was never
        // true: a record added and connected in this tool stayed `"active": null` on disk and the
        // next launch silently observed `vike_config::config::DEFAULT_TRADEHUB_ADDRESS` instead.
        let next_active =
            vike_app_core::backend::connection::active_after(&action, &app.backends.backends);
        app.apply_backend_action(ctx, action);
        if app.backends.active != next_active {
            app.backends.active = next_active;
            if let Err(e) = vike_app_core::backend::registry::save(&app.backends) {
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
        let new_file = vike_app_core::backend::editor::apply_registry_update(
            upd,
            || {
                app.apply_backend_action(
                    ctx,
                    vike_app_core::backend::connection::BackendAction::Disconnect,
                )
            },
            |file| {
                if let Err(e) = vike_app_core::backend::registry::save(file) {
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
/// `crates/vike-desktop/tests/app_local_core_gate/trade_shell.rs`'s `DIRECTORY_FETCH` pins
/// inside its `if windows::account_directory_due(` block.
pub(super) fn drain_backend_settings_and_directory(
    app: &mut App,
    ctx: egui::Context,
    fr: &WindowFrame,
    backend_settings_refresh: bool,
    backend_settings_edit: windows::BackendSettingsEditState,
    backend_settings_write: Option<windows::BackendSettingsWriteRequest>,
) {
    // Backend-settings section (split-plane REQ-7, read half): start this frame's requested
    // fetch — the section's own auto-fetch (first sight of a backend), a Refresh click, or
    // the post-save refetch the write drain above requested.
    if backend_settings_refresh {
        app.spawn_backend_settings_fetch(&ctx);
    }
    // The node's directory (venue names, the account list): once per backend, then at most once a
    // minute while a Trade window is open — `windows::account_directory_due` is the whole rule.
    let now = std::time::Instant::now();
    if windows::account_directory_due(
        &app.directory,
        fr.active_address.as_deref(),
        &app.windows,
        now,
    ) {
        app.spawn_directory_fetch(&ctx);
    }
    // …and the WRITE half: the frame-local edit flow back onto `app`, then this frame's
    // accepted Save (if any) onto its worker thread.
    app.backend_settings_edit = backend_settings_edit;
    if let Some(req) = backend_settings_write {
        app.spawn_backend_settings_write(&ctx, req);
    }
}

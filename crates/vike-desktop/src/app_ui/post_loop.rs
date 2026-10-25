//! The post-loop tail: clones, feed ensures, store seeding, orderflow, teardown and the drains.

use super::*;

/// Tail phase — open the windows the chart title bars' clone buttons asked for (`to_clone` holds
/// indices into `app.wins`, which is back in place by now).
pub(super) fn spawn_cloned_windows(app: &mut App, ctx: &egui::Context, to_clone: Vec<usize>) {
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
pub(super) fn ensure_wanted_feeds(
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
pub(super) fn seed_charts_from_store(app: &mut App, ctx: &egui::Context, fr: &WindowFrame) {
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
        // control key (Studio's backtest key is typed and plane-checked so it cannot be one).
        let dial = chart_seed_dial(addr.clone(), &fr.datahub_key_name, app.chart_seed_note.clone());
        store_bars::spawn_store_seed(
            reads,
            remote_hist_store(addr, &fr.datahub_key_name),
            bars,
            vike_model::now_ms(),
            // How deep the read goes: the `preferences.chart_history_bars` row (read in `main.rs`).
            crate::chart_history_bars(),
            Some(dial),
            move || wake.request_repaint(),
        );
    }
}

/// Tail phase — (idempotently) subscribe the trade feed and register the orderflow aggregator for
/// every chart window that wants orderflow this frame. Live-only: no history is backfilled.
pub fn register_orderflow(app: &mut App, of_wanted: Vec<(String, String, String, Option<f64>)>) {
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
pub(super) fn apply_dataset_mutations(
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
pub(super) fn apply_stored_view_actions(
    app: &mut App,
    ctx: &egui::Context,
    stored_deletes: Vec<(String, String, vike_data::StoreKindId, Option<String>)>,
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
pub(super) fn apply_backend_actions(
    app: &mut App,
    ctx: &egui::Context,
    backend_action: Option<vike_app_core::backend::backend_conn::BackendAction>,
    backend_registry_update: Option<vike_app_core::backend::backend_editor::RegistryUpdate>,
) {
    // Connections backend picker (split-plane B1): apply this frame's Connect/Disconnect
    // click — the backend-session-clear switch routine lives in `vike_app_core::backend::backend_conn`.
    if let Some(action) = backend_action {
        // The registry's `active` pointer FOLLOWS the picker, so the node picked here is the one a
        // bare launch dials next (`backend_registry::active_record`, read by
        // `backend_conn::startup_observe`). Computed BEFORE the switch, because the decision is about
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
pub(super) fn drain_backend_settings_and_directory(
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

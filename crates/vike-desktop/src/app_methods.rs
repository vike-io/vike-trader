//! `App`'s non-constructor methods — feed lifecycle, workspace restore, backend settings and the
//! node's directory, depth subscriptions, core sync. Split out of `main.rs` for size; `App::new` (the
//! constructor) is `app_setup.rs`'s, beside the construction phases it runs.

use super::*;
use vike_app_core::backend::registry::{control_signer, observe_signer};

impl App {
    /// The active backend's Scope::Write write channel, if any — the read every command/status
    /// site takes now that the raw `tradehub_control` field folded into `active_backend` (B1). Same
    /// `None` semantics as the old field: read-only observer, or no backend at all.
    pub(crate) fn tradehub_control(&self) -> Option<&vike_tradehub_client::TradehubControlHandle> {
        self.active_backend.as_ref().and_then(|b| b.control.as_ref())
    }

    /// Ask eframe for one framebuffer readback (`VIKE_SHOT`, File → Export chart image…). egui 0.37
    /// completes it through a callback, which parks the image in `shot_slot` and wakes the frame loop;
    /// [`Self::take_shot`] is the read.
    pub(crate) fn request_shot(&self, ctx: &egui::Context) {
        let slot = self.shot_slot.clone();
        let wake = ctx.clone();
        ctx.request_screenshot(move |image| {
            if let Ok(mut parked) = slot.lock() {
                *parked = Some(image);
            }
            wake.request_repaint();
        });
    }

    /// The readback a [`Self::request_shot`] callback parked, if one has landed since the last take.
    pub(crate) fn take_shot(&self) -> Option<std::sync::Arc<egui::ColorImage>> {
        self.shot_slot.lock().ok().and_then(|mut parked| parked.take())
    }

    /// **The ONE datahub-address resolution point** (split-plane REQ-2): the explicit
    /// `config.datahub_addr` if the operator set one, else the ACTIVE backend's `Welcome`
    /// advertisement (`datahub=<address>`, read off the observe bridge — so a backend switch or a
    /// link drop re-resolves on the next read), else `None` (no datahub, and no store: the local
    /// one went with the fat build). The ranking itself is
    /// `vike_app_core::data::datahub::resolve_datahub_address` (CI-tested; this method is
    /// wiring). EVERY datahub consumer — `open_studio_store`, `maybe_spawn_stored_backfill`,
    /// `stored_mode` — reads through here, never the raw key, so the three can
    /// never disagree about which store the GUI is talking to.
    pub(crate) fn resolved_datahub_address(&self) -> Option<String> {
        vike_app_core::data::datahub::resolve_datahub_address(
            settings().config.datahub_addr.as_deref(),
            self.active_backend.as_ref().and_then(|b| b.bridge.advertised_datahub()).as_deref(),
        )
    }

    /// Apply one Connections-picker click (split-plane B1) — WIRING ONLY: the gate, the
    /// backend-session clear (B2: the local feed plane survives a switch untouched — the ladders,
    /// tape and tick/vol folds keep painting) and the connect all live in
    /// [`vike_app_core::backend::connection::switch_backend`]; this method only assembles `App`'s
    /// fields into the [`vike_app_core::backend::connection::BackendSwitchSlots`] borrow-bundle and hands over
    /// the two GUI-owned closures (arc-swap store + repaint wake), exactly like `App::new`'s
    /// observe arm.
    pub(crate) fn apply_backend_action(
        &mut self,
        ctx: &egui::Context,
        action: vike_app_core::backend::connection::BackendAction,
    ) {
        use vike_app_core::backend::connection::{self, BackendAction};
        let target = match &action {
            BackendAction::Connect(record) => Some(record.clone()),
            BackendAction::Disconnect => None,
        };
        // The connect signs with this PC's device key, which the Server group may have just made.
        refresh_device_key();
        let cell = self.snapshot_cell.clone();
        let ectx = ctx.clone();
        connection::switch_backend(
            &mut self.active_backend,
            target.as_ref(),
            crate::settings().flags.tradehub_control,
            crate::observe_poll(),
            crate::link_patience(),
            connection::BackendSwitchSlots {
                charts: &mut self.charts,
                tick_volume_aggregators: &mut self.tick_volume_aggregators,
                orderflow_aggregators: &mut self.orderflow_aggregators,
                published: &mut self.published_series,
                last_sequence: &mut self.last_sequence,
                status: &mut self.status,
                feeds: &mut self.feeds,
                subscriptions: &mut self.subscriptions,
                spawned: &mut self.spawned,
                unroutable: &mut self.unroutable,
                feed_retries: &mut self.feed_retries,
                trades: &self.trades,
                books: &self.books,
                direct_bars: self.direct_bars.as_deref(),
                bar_plane: BAR_PLANE,
                trade_depth: &mut self.trade_depth,
                polymarket_subscriptions: &mut self.polymarket_subscriptions,
                hidden: &mut self.hidden,
                feed_status: &self.feed_status,
            },
            move |snapshot| cell.store(snapshot),
            move || ectx.request_repaint(),
        );
        // Another backend is another STORE: `clear_session_state` emptied the bar store under
        // `BackendBarPlane::BackendStore`, so the record of what was already asked for must go with it or
        // B's charts would never be fetched at all.
        self.chart_history.clear();
        // ...and the seed note with it: it names a SWITCH on the server that was just swapped out,
        // so carrying it over would send the operator to the wrong box.
        if let Ok(mut n) = self.chart_seed_note.lock() {
            *n = None;
        }
        // ...and every Trade window's held order (drawn or not) and the window a new one copies:
        // neither names a backend, so a confirm would send the old one's order to the new one.
        windows::on_backend_switch(&mut self.window_views, &mut self.last_trade);
    }

    /// Start ONE short-lived Backend-settings fetch for the ACTIVE backend (split-plane REQ-7,
    /// read half): sign with this PC's device key (`vike_app_core::backend::registry::observe_signer`,
    /// as every dial of `connect_backend` does), mark the slot `Pending`, and run
    /// `vike_tradehub_client::settings_show` on a throwaway thread — the verb owns its own
    /// per-call connection and the client-side feature refusal, so an old node lands as the
    /// section's "server predates settings-show" line without a byte on the wire.
    ///
    /// Latest-wins ONLY for the backend it was asked about: the slot is keyed by addr, so a
    /// result racing a backend switch is dropped rather than painted under the new connection
    /// (the same total-clear discipline as `switch_backend`).
    pub(crate) fn spawn_backend_settings_fetch(&mut self, ctx: &egui::Context) {
        let Some(record) = self.active_backend.as_ref().map(|b| b.record.clone()) else {
            return;
        };
        // Without a device key nothing is sent: the section renders `registry::no_device_key`.
        let key = observe_signer(&record.address);
        let address = record.address;
        let slot = Arc::clone(&self.backend_settings);
        *slot.lock().unwrap() = (address.clone(), windows::BackendSettingsState::Pending);
        let ectx = ctx.clone();
        std::thread::Builder::new()
            .name("ui-settings-get".into())
            .spawn(move || {
                let outcome = vike_app_core::ui::windows::settings_fetch_state(
                    key.and_then(|k| vike_tradehub_client::settings_show(address.as_str(), &k)),
                );
                let mut s = slot.lock().unwrap();
                if s.0 == address {
                    s.1 = outcome;
                }
                drop(s);
                ectx.request_repaint();
            })
            .expect("spawn ui-settings-get (backend settings) thread");
    }

    /// Run ONE settings WRITE against the ACTIVE backend (split-plane REQ-7, write half) — the
    /// out-slot drain for [`vike_app_core::ui::windows::BackendSettingsWriteRequest`]:
    /// sign with this PC's device key on an ARMED record (`registry::control_signer`; the server's
    /// access list must give the device the admin role, decision 0288's 1A amendment), run the
    /// synchronous per-call `vike_tradehub_client::set_setting` on a throwaway thread, FOLD the
    /// outcome to its flow state there (`settings_write_state` is pure), and park it in
    /// `backend_settings_write_result` keyed by addr — dropped rather than painted if the
    /// operator switched backends mid-write (the fetch slot's discipline).
    ///
    /// Without a device key, or on an unarmed record, nothing is sent and the section renders why.
    /// The audit `reason` is `None`: the section offers no rationale box, and the audit record
    /// already carries old→new.
    pub(crate) fn spawn_backend_settings_write(
        &mut self,
        ctx: &egui::Context,
        req: windows::BackendSettingsWriteRequest,
    ) {
        let Some(record) = self.active_backend.as_ref().map(|b| b.record.clone()) else {
            // No active backend to write to (it disconnected this frame): fold the refusal
            // locally so the flow never wedges in `Saving` (a refusal: nothing was sent).
            self.backend_settings_edit = windows::BackendSettingsEditState::Failed {
                key: req.key,
                error: "no active backend".to_string(),
            };
            return;
        };
        let key = control_signer(&record);
        let address = record.address;
        let slot = Arc::clone(&self.backend_settings_write_result);
        let ectx = ctx.clone();
        std::thread::Builder::new()
            .name("ui-settings-set".into())
            .spawn(move || {
                let outcome = windows::settings_write_state(
                    &req.key,
                    key.and_then(|key| {
                        vike_tradehub_client::set_setting(
                            address.as_str(),
                            &key,
                            &req.file,
                            &req.key,
                            &req.value,
                            // No typed confirm for any key (`docs/decisions/0086` point 7), and the
                            // daemon never reads one — so the wire's `confirm` is always `None`.
                            None,
                            None,
                        )
                    }),
                );
                *slot.lock().unwrap() = Some((address, outcome));
                ectx.request_repaint();
            })
            .expect("spawn ui-settings-set (backend settings) thread");
    }

    /// The venue-subscription bookkeeping (`feeds`/`subscriptions`/`spawned`/`unroutable`/`feed_retries`)
    /// borrowed as the one bundle every moved feed-lifecycle function takes — see
    /// [`vike_app_core::ui::feed_lifecycle`]'s module doc for why `App`'s fields become parameters.
    /// Disjoint field borrows, so `&mut self` here costs nothing at the call sites below.
    pub(crate) fn feed_slots(&mut self) -> feed_lifecycle::FeedSlots<'_> {
        feed_lifecycle::FeedSlots {
            feeds: &mut self.feeds,
            subscriptions: &mut self.subscriptions,
            spawned: &mut self.spawned,
            unroutable: &mut self.unroutable,
            retries: &mut self.feed_retries,
        }
    }

    /// Both bundles at once — [`feed_lifecycle::FeedSlots`] plus the render-side
    /// [`feed_lifecycle::SeriesSlots`] (the per-key chart state, the Data-manager-deleted set and
    /// the two client-side aggregator maps a feed's lifecycle creates and destroys). They borrow
    /// disjoint `App` fields, so one method has to hand out both.
    pub(crate) fn feed_and_series_slots(
        &mut self,
    ) -> (feed_lifecycle::FeedSlots<'_>, feed_lifecycle::SeriesSlots<'_>) {
        (
            feed_lifecycle::FeedSlots {
                feeds: &mut self.feeds,
                subscriptions: &mut self.subscriptions,
                spawned: &mut self.spawned,
                unroutable: &mut self.unroutable,
                retries: &mut self.feed_retries,
            },
            feed_lifecycle::SeriesSlots {
                charts: &mut self.charts,
                hidden: &mut self.hidden,
                tick_volume_aggregators: &mut self.tick_volume_aggregators,
                orderflow_aggregators: &mut self.orderflow_aggregators,
            },
        )
    }

    /// Subscribe `venue`'s raw trade tape for `symbol`, once. Thin wrapper: the body moved to
    /// [`vike_app_core::ui::feed_lifecycle::ensure_trade_feed_on`] so the merge gate compiles,
    /// clippies and TESTS it (this file is in no gate).
    pub(crate) fn ensure_trade_feed_on(&mut self, venue: &str, symbol: &str) {
        feed_lifecycle::ensure_trade_feed_on(&mut self.feed_slots(), venue, symbol);
    }

    /// Apply ONE window spawn: hand the decision to
    /// [`vike_app_core::ui::window::spawn::plan_window_spawn`] and perform what it returns.
    ///
    /// Thin wrapper, deliberately: the six call sites in the frame loop below differ only in which
    /// `WindowSpawnRequest` they build, so every one of them now reads as an explicit delegation instead
    /// of carrying its own copy of the cascade arithmetic, the `WindowState` construction and the
    /// subscription. The steps run in the order `WindowSpawnPlan` documents — counter, feed, window —
    /// because that is the order each inline site used and a spawn's ORDER is load-bearing: every
    /// site that subscribed anything subscribed BEFORE publishing its window. (A Trade window's
    /// depth stream is not a spawn step: the window loop requests it once the window has an
    /// instrument.)
    ///
    /// `next_window_number` is adopted ahead of the subscribe here, where two of the six sites did it
    /// after. The two are independent: [`Self::ensure_feed_on`] reaches the feed layer through
    /// [`Self::feed_and_series_slots`], whose bundles borrow `feeds`/`subscriptions`/`spawned`/`unroutable`/
    /// `feed_retries` and `charts`/`hidden`/`tick_volume_aggregators`/`orderflow_aggregators` — a set disjoint from `next_window_number` and
    /// `windows`, and nothing below it reads either.
    pub(crate) fn apply_spawn(&mut self, ctx: &egui::Context, req: spawn::WindowSpawnRequest) {
        // The Studio is one window: asked again, the one there is comes to the front, over a
        // maximized window too, which stays maximized (`window::arrange::raise`).
        if let Some(id) = spawn::reopen_studio(&mut self.windows, &req) {
            window::arrange::raise(ctx, id);
            return;
        }
        let trade = spawn::trade_open_size(ctx);
        let default = spawn::default_series(&self.windows, &self.symbol_set_store.sets);
        let plan = spawn::plan_window_spawn(
            req,
            self.desktop.min,
            self.next_window_number,
            trade,
            default.as_ref(),
        );
        self.next_window_number = plan.next_window_number;
        if let Some(f) = &plan.ensure_feed {
            self.ensure_feed_on(ctx, f);
        }
        // The page the new window opens on (a Data Manager destination, an Options tab), seeded
        // BEFORE the window is moved out of `plan` — the same pre-seed `app_setup.rs`'s startup
        // planner performs for a restored/QA-captured window (`App::new`'s `layout.data_destination` arm).
        plan.seed_window_views(&mut self.window_views);
        if let Some(w) = plan.window {
            // On top at once, so a maximized window does not hide it and stays maximized.
            window::arrange::raise(ctx, w.id);
            self.windows.push(w);
        }
    }

    /// Rebuild the whole window layout + the indicator favourites from a loaded
    /// `WindowWorkspace` snapshot — the shared body of File → Open Workspace and the
    /// named-layout Load action. Feeds are (re)ensured per restored window on its
    /// own venue (cross-exchange search).
    pub(crate) fn restore_workspace(
        &mut self,
        ctx: &egui::Context,
        ws: &window::workspace::WindowWorkspace,
    ) {
        self.indicator_favourites = ws.indicator_favourites.clone();
        let wins = window::workspace::apply(ws);
        for w in &wins {
            self.ensure_feed_on(ctx, &startup::SeriesSpec::of_window(w));
        }
        self.next_window_number = wins.len() as u32;
        self.windows = wins;
    }

    /// Spawn a feed for `(venue, symbol, interval)` if not already running, routing the bar
    /// subscription to `venue`'s [`vike_data::DataClient`] (cross-exchange symbol search). Kline
    /// intervals subscribe that venue's live klines; the `charts`/`subscriptions`/`spawned` slots are keyed
    /// by [`chart_key::series_key`], `"venue:SYMBOL@interval"` for every venue, so two venues
    /// never collide on a shared symbol.
    /// Tick/volume intervals (Task B5) have no venue kline feed of their own — they're built
    /// client-side from `venue`'s raw trade tape (venue-aware: any venue whose feed implements
    /// `subscribe_trades` can drive tick/vol charts), so those spawn `venue`'s own trade feed
    /// (`ensure_trade_feed_on`) plus a per-chart-key aggregator in `tick_volume_aggregators`.
    ///
    /// Thin wrapper: the body moved to [`vike_app_core::ui::feed_lifecycle::ensure_feed_on`]. The
    /// `_context` parameter was already unused there and is NOT forwarded (every call site still
    /// passes one, so nothing here changed); the `shutdown` atomic stays owned by this binary and
    /// its already-loaded value is passed down.
    pub(crate) fn ensure_feed_on(&mut self, _context: &egui::Context, spec: &startup::SeriesSpec) {
        let display_time_zone = self.appearance.display_time_zone();
        // Shutting down (window-close requested / `on_exit` running): never start a NEW live feed.
        // Inert on the running path (a single Relaxed load; see `on_exit`).
        let shutting_down = self.shutdown.load(std::sync::atomic::Ordering::Relaxed);
        let (mut f, mut s) = self.feed_and_series_slots();
        feed_lifecycle::ensure_feed_on(&mut f, &mut s, spec, display_time_zone, shutting_down);
    }

    /// C2 tidy (FIX 3) + the feed-leak fix: reap any `spawned` feed no window references anymore,
    /// then the two coupled cleanups that follow it (dead tick/vol + orderflow AGGREGATOR entries,
    /// then any raw trade tape no remaining aggregator needs) — then the depth/cockpit sibling
    /// reap, which stops any depth or cockpit book/trade stream whose windows are gone (the B2
    /// prerequisite: those ledgers used to be insert-only, so a closed DOM leaked its stream
    /// until process exit).
    ///
    /// Thin wrapper: the bodies moved to [`vike_app_core::ui::feed_lifecycle::reap_orphaned_feeds`]
    /// and [`vike_app_core::ui::feed_lifecycle::reap_orphaned_trade_cockpit_streams`] — see their docs
    /// for the full teardown contract, and `feed_slot_tests` for the teardown → re-add round
    /// trips that now run in the merge gate. Called once every frame (see the `update` call site)
    /// rather than threaded into every removal call site.
    ///
    /// The bundles are built inline here rather than through `feed_and_series_slots` because
    /// `windows` must be borrowed (immutably) at the same time — disjoint `App` fields, which the
    /// borrow checker only splits within one function body, not across a `&mut self` method call.
    pub(crate) fn reap_orphaned_feeds(&mut self) {
        let wins = &self.windows;
        let mut f = feed_lifecycle::FeedSlots {
            feeds: &mut self.feeds,
            subscriptions: &mut self.subscriptions,
            spawned: &mut self.spawned,
            unroutable: &mut self.unroutable,
            retries: &mut self.feed_retries,
        };
        let mut s = feed_lifecycle::SeriesSlots {
            charts: &mut self.charts,
            hidden: &mut self.hidden,
            tick_volume_aggregators: &mut self.tick_volume_aggregators,
            orderflow_aggregators: &mut self.orderflow_aggregators,
        };
        feed_lifecycle::reap_orphaned_feeds(&mut f, &mut s, wins);
        feed_lifecycle::reap_orphaned_trade_cockpit_streams(
            &mut self.feeds,
            &mut self.trade_depth,
            &mut self.polymarket_subscriptions,
            &mut self.feed_retries,
            wins,
        );
    }

    /// Background-load the Data Manager's "Stored" inventory tree (Task 3) AND its per-series gap
    /// map (dm-gap-viz) — from WHICHEVER store the mode table says the grid reads
    /// (`vike_app_core::data::data_manager::mode::stored_mode` over the RESOLVED datahub address —
    /// `App::resolved_datahub_address`, the SAME resolution `open_studio_store` and
    /// `maybe_spawn_stored_backfill` read; REQ-2). This is the #1378 seam close: the
    /// wire backfill lands its rows in the SERVER's store, so a grid that kept reading the local
    /// one would never show them.
    ///
    /// - **remote** (`datahub_address` set — the ONLY arm left): walks a fresh `HistoryStoreDatahub` through
    ///   the `HistoryStore` TRAIT verbs (`vike_app_core::data::data_manager::load::load_stored_tree` —
    ///   `inventory` + one cheap `series_gaps` probe per series, all served since B12/PR-6), then
    ///   runs the SAME `load_partials` fold the local arm runs — `coverage_report` is a TRAIT verb
    ///   with a datahub wire verb behind it since spec §6-Q2. Whether the connected server answers
    ///   it is what the load reports back as `DatahubCoverage`: served → the Partial column renders
    ///   from the wire answer, refused (an older server) → an empty map plus
    ///   `stored_mode`'s honest note, never a silent empty.
    /// - **local** (`datahub_address` unset): ⚠ TOMBSTONE — this arm opened a FRESH `HistoryStoreDataFusion`
    ///   handle (`open_local_hist_store`, its own tokio runtime + WAL recovery, never on the UI
    ///   thread) and ran the SAME `load_stored_tree` walk and `load_partials` fold, so neither the
    ///   tree shape nor the Partial map could drift between the two stores. There is no local store
    ///   engine here any more, so an unset address is a no-op: the grid stays empty and
    ///   `open_studio_store`'s `Err` names the fix (set `config.datahub_addr`, or connect a backend
    ///   that advertises one).
    ///
    /// The remote arm delivers each answer as it lands (`DataManagerInventory::start`'s thread; folded by
    /// `DataManagerInventory::drain` in `frame_begin`), so the series list draws the moment the inventory
    /// does. Called on the Stored tab's first render and on every Refresh click / post-delete
    /// reload — never per-frame, and gaps are never (re)computed per-frame either.
    ///
    /// ⚠ **This doc used to end "rather than leaving the UI stuck on 'Loading…' forever", and that
    /// clause was the defect rather than a description of it.** Only a FAILURE degraded; a HANG did
    /// not, because the load thread is detached and there was nothing to time it out — a walk that
    /// never returned never sent, the loading flag never cleared, and the window sat on *Loading
    /// stored data…* for the life of the process, with nothing logged at any level to say which of
    /// "hung", "empty" or "slow" it was. The thread, the bound and the boundary logging now live in
    /// `crates/vike-app-core/src/data/data_manager/mod.rs` — beside the degrade contract they make keepable,
    /// and where CI can RUN them, which this crate's one `app-check` job cannot. This body is the
    /// address, and deliberately nothing else.
    pub(crate) fn refresh_stored(&mut self, ctx: &egui::Context) {
        use vike_app_core::data::data_manager::mode::DatahubCoverage;
        // Only `datahub_address` is read here — the Partial column's state is decided at RENDER time
        // from what this load negotiates, so the coverage input is `Unknown` at this call site.
        // The address is the RESOLVED one (REQ-2): the explicit `config.datahub_addr` when set,
        // else the active backend's advertisement.
        let mode = vike_app_core::data::data_manager::mode::stored_mode(
            self.resolved_datahub_address().as_deref(),
            DatahubCoverage::Unknown,
        );
        if let Some(address) = mode.datahub_address {
            // Resolving the device key stays on the worker, where the file read belongs. `start` is
            // a no-op while a load runs, and clears the previous attempt's reason when it starts one.
            self.stored.start(move || open_history_store_datahub(address.clone()), ctx);
            // ⚠ TOMBSTONE — a `#[cfg(feature = "fat")] return;` stood here, and it earned its own
            // paragraph: without it the LOCAL loader below ALSO spawned, two threads raced to
            // send on the one load channel, and the last to land overwrote the Stored tab with the wrong
            // inventory. It was `#[cfg]`-gated rather than a bare `return` because under thin the
            // block below was compiled out and this would have been the function's last statement
            // — which is exactly how an edition-2024 `clippy --fix` pass run under thin came to
            // DELETE it (`needless_return` was true in the configuration it saw and false in the
            // one that shipped), caught by per-hunk review and by no gate. The local loader is gone
            // now, so there is no second thread to guard against and no statement to remove.
        }
        // ⚠ TOMBSTONE — `refresh_stored`'s LOCAL arm stood here. It set the loading flag, opened a
        // FRESH `HistoryStoreDataFusion` through `open_local_hist_store()` off-thread, and ran the SAME
        // `vike_app_core::data::data_manager::load::load_stored_tree` walk and `load_partials` fold the remote
        // arm runs, delivering a `DataManagerInventoryLoadOutcome` with `DatahubCoverage::Unknown` (local mode negotiates
        // nothing). The desktop has no local store engine any more — the DataFusion engine
        // was a `fat` enable — so the REMOTE arm above is the only load path, which is the same
        // shape `open_studio_store` now has. With no datahub resolved the grid simply stays empty
        // and that function's `Err` names both fixes.
    }

    /// Data Manager "Stored" bulk Backfill/Update (dm-bulk-backfill): plans jobs via
    /// [`vike_app_core::data::backfill::plan::plan_backfill_jobs`] (each selected series' known
    /// gap ranges when it has any, else the default lookback — `preferences.backfill_lookback_days`,
    /// thirty days unless a row says otherwise — ending now) and runs them
    /// off-thread — IF there is a RESOLVED datahub address (`App::resolved_datahub_address` — explicit
    /// `config.datahub_addr` first, else the active backend's advertisement; REQ-2), the SAME
    /// resolution `open_studio_store` reads (split-plane REQ-9, the GUI half).
    ///
    /// ⚠ The PLAN does not depend on the address — 0059 Phase 3 deleted this build's own venue
    /// roster, so `kind == "bar"` is the whole client-side gate and venue support is the server's
    /// question. The address decides whether the plan runs and nothing else:
    ///
    /// - **unset** → the status slot names the missing engine and how to point the app at a
    ///   datahub, and nothing runs. (This was `BackfillRoute::Local` and its executor
    ///   `spawn_stored_backfill_local`, a TOMBSTONE of the `fat` build's keyless kline backfillers
    ///   into a LOCAL store; the route's local arm closed on 2026-11-12, the text is unchanged.)
    /// - **set** → [`Self::spawn_stored_backfill_wire`]: the SAME planned gap requests go through
    ///   the datahub backfill-on-demand verb (both builds — the wire arm is exactly what a
    ///   thin/remote setup needs). The rows land in the SERVER's store by write-through; the
    ///   completion-triggered `refresh_stored` re-reads. Venue support is the SERVER's roster on
    ///   this route (only `kind == "bar"` gates client-side): an off-roster venue surfaces the
    ///   server's own refusal text in the status slot, and an old server (no `backfill`
    ///   capability in its `Welcome`) is refused client-side with a status line saying it
    ///   predates backfill — never a silent no-op.
    ///
    /// `Update` (see the `response.bulk` match in `windows::stored_tool_content`) ALIASES to this
    /// SAME path for MVP: there is no separate "ignore gaps, always refetch to now" mode yet —
    /// both buttons plan and run identically. A call while `stored_backfill_running` is already
    /// true, or with an empty selection, is a silent no-op (the run-once-at-a-time gate — the
    /// grid's bulk bar only shows once something is selected, so an empty call here means a stale
    /// click landed after the selection was cleared). Completion re-triggers `refresh_stored`
    /// (drained in `update`'s `stored_backfill_rx` check) so the grid's coverage bars/gap
    /// cut-outs reflect the newly-ingested data.
    pub(crate) fn maybe_spawn_stored_backfill(
        &mut self,
        ctx: &egui::Context,
        keys: Vec<vike_data_manager::DataManagerInventorySeriesKey>,
    ) {
        if self.stored_backfill_running || keys.is_empty() {
            return;
        }
        // The "never backfilled before" default lookback for a series with no recorded gaps yet:
        // the `preferences.backfill_lookback_days` row (thirty days unless a row says otherwise) —
        // long enough to seed a chart's worth of history, short enough that a bulk backfill over
        // many symbols doesn't page every one of them all the way back to the venue's listing date.
        let lookback_ms = crate::backfill_lookback_ms();
        let datahub_address = self.resolved_datahub_address();
        let selected: std::collections::BTreeSet<_> = keys.into_iter().collect();
        let now_ms = chrono::Local::now().timestamp_millis();
        // The PLAN is address-independent since 0059 Phase 3 deleted the GUI's own venue roster
        // (`vike_app_core::data::backfill::plan`'s module doc carries why); `datahub_address` below still
        // decides whether it RUNS.
        // ...and the default window is CLAMPED to each venue's floor where the datahub's
        // history-channels answer proves one (decision 0102; on today's table, nowhere — the map is
        // empty, and a fallback table no server confirmed never yields one).
        let floors = vike_app_core::data::history_column::lookback_floors(
            self.stored.history.as_ref(),
            now_ms,
        );
        let (jobs, skipped) =
            plan_backfill_jobs(&selected, &self.stored.gaps, now_ms, lookback_ms, &floors);
        if jobs.is_empty() {
            self.stored_backfill_status =
                format!("Backfill: 0 series queued, {skipped} skipped (unsupported)");
            return;
        }
        match datahub_address {
            Some(address) => self.spawn_stored_backfill_wire(ctx, jobs, skipped, address),
            None => {
                self.stored_backfill_status =
                    "Backfill unavailable: this build has no local store engine — run `vike-cli \
                     config set config.datahub_addr <addr>` to point it at a running vike-datahub \
                     server, or connect a backend that advertises one"
                        .to_string();
            }
        }
    }

    /// The one executor of [`Self::maybe_spawn_stored_backfill`] (split-plane REQ-9): a worker
    /// thread — the wire call is blocking TCP, so it stays off the UI thread — whose blocking work
    /// is [`vike_app_core::data::backfill::run_datahub_backfill`], one datahub verb call per
    /// planned range. Nothing streams back through this thread: the rows are IN the server's store
    /// when `BackfillDone` arrives (write-through before serve), so completion just reports the
    /// rendered status through `stored_backfill_tx`, whose drain in `update` re-triggers
    /// `refresh_stored`.
    pub(crate) fn spawn_stored_backfill_wire(
        &mut self,
        ctx: &egui::Context,
        jobs: Vec<DataManagerBackfillJob>,
        skipped: usize,
        hub: String,
    ) {
        self.stored_backfill_running = true;
        self.stored_backfill_status = backfill::backfill_running_status(jobs.len(), skipped, &hub);
        let tx = self.stored_backfill_tx.clone();
        let c = ctx.clone();
        std::thread::Builder::new()
            .name("ui-backfill".into())
            .spawn(move || {
                let report = backfill::run_datahub_backfill(&hub, &jobs, skipped);
                let _ = tx.send(backfill::render_backfill_run_status(&report));
                c.request_repaint();
            })
            .expect("spawn ui-backfill thread");
    }

    /// Start a live L2 depth stream for a Trade window's `(venue, instrument)` (once), `instrument` being the
    /// venue-native symbol the window trades. Called every frame for every Trade window with a
    /// symbol; idempotent. The stream stops when no Trade window names that book any more.
    ///
    /// Thin wrapper: the body is [`vike_app_core::ui::feed_lifecycle::ensure_depth`].
    pub(crate) fn ensure_depth(&mut self, venue: &str, instrument: &str) {
        feed_lifecycle::ensure_depth(
            &mut self.feeds,
            &mut self.trade_depth,
            &mut self.feed_retries,
            venue,
            instrument,
        );
    }

    /// Start ONE `Directory` fetch for the ACTIVE backend, off the UI thread: sign exactly as
    /// [`Self::spawn_backend_settings_fetch`] does and hand the call to
    /// [`vike_app_core::ui::windows::spawn_directory_fetch`], which owns the slot, the thread,
    /// the failure log and the repaint. Without a device key nothing is sent, and the refusal is
    /// the failure it logs.
    pub(crate) fn spawn_directory_fetch(&mut self, ctx: &egui::Context) {
        let Some(record) = self.active_backend.as_ref().map(|b| b.record.clone()) else {
            return;
        };
        let key = observe_signer(&record.address);
        let fetch =
            move |address: &str| key.and_then(|k| vike_tradehub_client::directory(address, &k));
        let ectx = ctx.clone();
        let slot = Arc::clone(&self.directory);
        windows::spawn_directory_fetch(slot, record.address, fetch, move || ectx.request_repaint());
    }

    /// Start (once) a live Polymarket L2 book + trade stream for `token` (the YES-outcome token-id)
    /// so a cockpit window renders that token's real book from the shared `L2BookStore` under venue
    /// `"polymarket"`. The polymarket feed's `subscribe_book` emits `DataClientSink::book`, which
    /// `vike_app_core::data::data_sink::DataClientSinkGuiFeed`'s `book` lands in the `L2BookStore`.
    /// The cockpit's analog of [`Self::ensure_depth`]; dedups via `polymarket_subscriptions`, stopped by the feed's
    /// `shutdown()` on exit. A missing feed, an empty/placeholder token, or a subscribe error is
    /// logged and skipped — feed failure never crashes the GUI (the cockpit just stays STALE).
    ///
    /// The feed runs on its own thread and routes through the crate's proxy (`POLY_*` env, resolved
    /// internally); it connects only when the WS proxy is enabled AND reachable (US-geo-block),
    /// otherwise it retries with backoff and the book stays empty/STALE — graceful degradation.
    ///
    /// Thin wrapper: the body moved to [`vike_app_core::ui::feed_lifecycle::ensure_polymarket_book`].
    pub(crate) fn ensure_polymarket_book(&mut self, token: &str) {
        feed_lifecycle::ensure_polymarket_book(
            &mut self.feeds,
            &mut self.polymarket_subscriptions,
            &mut self.feed_retries,
            token,
        );
    }

    /// Fold the latest CoreSnapshot into the render model. Thin wrapper: the whole fold — the
    /// per-series kline dispatch behind the `seq` gate, the trade-tape drain that feeds the
    /// tick/volume + orderflow aggregators, and the status line — lives in
    /// [`vike_app_core::ui::core_sync::sync_from_core`], moved down so the merge gate compiles,
    /// clippies and TESTS it (this file is in no gate; that fold's history is three
    /// silent-data-loss bugs whose only record was a code comment — see that module's doc). The
    /// arc-swap `load()` stays here so vike-app-core needs no `arc_swap` dependency.
    pub(crate) fn sync_from_core(&mut self) {
        let snapshot = self.snapshot_cell.load();
        // ⚠ A LIVE READER, NOT A SNAPSHOT. `tape_gaps` used to be `&self.market_data_session.tape_gaps()` —
        // a map cloned in the ARGUMENT LIST, i.e. before the fold was entered, while the drain the
        // fold must read it after happens hundreds of lines inside. The `Arc` is cloned so the
        // closure owns it and the `&mut self` borrows below stay free.
        let market_data = std::sync::Arc::clone(&self.market_data_session);
        let tape_gaps = move |venue: &str, symbol: &str| market_data.tape_gap_epoch(venue, symbol);
        core_sync::sync_from_core(
            core_sync::CoreSyncInputs {
                snapshot: &snapshot,
                spawned: &self.spawned,
                hidden: &self.hidden,
                display_time_zone: self.appearance.display_time_zone(),
                trades: &self.trades,
                feed_status: &self.feed_status,
                direct_bars: self.direct_bars.as_deref(),
                bar_plane: BAR_PLANE,
                tape_gaps: &tape_gaps,
            },
            core_sync::CoreSyncState {
                charts: &mut self.charts,
                tick_volume_aggregators: &mut self.tick_volume_aggregators,
                orderflow_aggregators: &mut self.orderflow_aggregators,
                published: &mut self.published_series,
                last_sequence: &mut self.last_sequence,
                last_direct_gen: &mut self.last_direct_gen,
                status: &mut self.status,
            },
        );
        // The series the charts show, for the node to send the bars of those alone; only a change reaches it.
        if let Some(backend) = &self.active_backend {
            backend.bridge.show_series(vike_app_core::ui::series_follow::shown_series(
                &self.published_series,
                &self.spawned,
                &self.hidden,
            ));
        }
    }
}

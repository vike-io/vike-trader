//! `App`'s non-constructor methods — feed lifecycle, workspace restore, backend settings,
//! DOM/depth subscriptions, core sync. Split out of `main.rs` for size; `App::new` (the
//! constructor) stays in `main.rs` — see that plan's Global Constraints for why.

use super::*;

impl App {
    /// The active backend's Scope::Control write channel, if any — the read every command/status
    /// site takes now that the raw `remote_ctrl` field folded into `active_backend` (B1). Same
    /// `None` semantics as the old field: read-only observer, or no backend at all.
    pub(crate) fn remote_ctrl(&self) -> Option<&vike_tradehub_client::RemoteControlHandle> {
        self.active_backend.as_ref().and_then(|b| b.ctrl.as_ref())
    }

    /// **The ONE datahub-address resolution point** (split-plane REQ-2): the explicit
    /// `config.datahub_addr` if the operator set one, else the ACTIVE backend's `Welcome`
    /// advertisement (`datahub=<addr>`, read off the observe bridge — so a backend switch or a
    /// link drop re-resolves on the next read), else `None` (local store, the pre-REQ-2
    /// behavior). The ranking itself is `vike_app_core::datahub_resolve::resolve_datahub_addr`
    /// (CI-tested; this method is wiring). EVERY datahub consumer — `open_studio_store`,
    /// `backfill_route`, `stored_mode` — reads through here, never the raw key, so the three can
    /// never disagree about which store the GUI is talking to.
    pub(crate) fn resolved_datahub_addr(&self) -> Option<String> {
        vike_app_core::datahub_resolve::resolve_datahub_addr(
            settings().config.datahub_addr.as_deref(),
            self.active_backend.as_ref().and_then(|b| b.bridge.advertised_datahub()).as_deref(),
        )
    }

    /// Apply one Connections-picker click (split-plane B1) — WIRING ONLY: the gate, the
    /// backend-session clear (B2: the local feed plane survives a switch untouched — the DOM,
    /// tape and tick/vol folds keep painting) and the connect all live in
    /// [`vike_app_core::backend_conn::switch_backend`]; this method only assembles `App`'s
    /// fields into the [`vike_app_core::backend_conn::SwitchSlots`] borrow-bundle and hands over
    /// the two GUI-owned closures (arc-swap store + repaint wake), exactly like `App::new`'s
    /// observe arm.
    pub(crate) fn apply_backend_action(
        &mut self,
        ctx: &egui::Context,
        action: vike_app_core::backend_conn::BackendAction,
    ) {
        use vike_app_core::backend_conn::{self, BackendAction};
        // The B2 fence: no switching while a local core runs (the picker's buttons are disabled
        // then too — this is the belt to that suspender, since the action crosses a frame).
        if !backend_conn::switching_available(self.core.is_some()) {
            return;
        }
        let target = match &action {
            BackendAction::Connect(record) => Some(record.clone()),
            BackendAction::Disconnect => None,
        };
        let vars = workspace_credentials();
        let cell = self.snap_cell.clone();
        let ectx = ctx.clone();
        backend_conn::switch_backend(
            &mut self.active_backend,
            target.as_ref(),
            &vars,
            vike_app_core::tradehub_control::control_enabled(),
            backend_conn::SwitchSlots {
                charts: &mut self.charts,
                aggs: &mut self.aggs,
                of_aggs: &mut self.of_aggs,
                bf_pending: &mut self.bf_pending,
                last_seq: &mut self.last_seq,
                status: &mut self.status,
                feeds: &mut self.feeds,
                subs: &mut self.subs,
                spawned: &mut self.spawned,
                unroutable: &mut self.unroutable,
                feed_retries: &mut self.feed_retries,
                bf_spawned: &mut self.bf_spawned,
                bf_retries: &mut self.bf_retries,
                earliest_live_ids: &self.earliest_live_ids,
                bf_rx: &self.bf_rx,
                bf_done_rx: &self.bf_done_rx,
                trades: &self.trades,
                books: &self.books,
                direct_bars: self.direct_bars.as_deref(),
                dom_depth: &mut self.dom_depth,
                poly_subs: &mut self.poly_subs,
                hidden: &mut self.hidden,
                feed_status: &self.feed_status,
            },
            move |snap| cell.store(snap),
            move || ectx.request_repaint(),
        );
    }

    /// Start ONE short-lived Backend-settings fetch for the ACTIVE backend (split-plane REQ-7,
    /// read half): resolve the record's observe-key NAME through the credential store
    /// (`vike_app_core::backend_registry::resolve_keys` — names-not-values, the same resolution
    /// `connect_backend` runs), mark the slot `Pending`, and run
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
        let vars = workspace_credentials();
        let (observe, _control) = vike_app_core::backend_registry::resolve_keys(&record, &vars);
        // An absent observe key still fetches: the daemon refuses the handshake and the section
        // renders that refusal — the honest answer, same as `connect_backend`'s bridge.
        let key = observe.unwrap_or_default().as_bytes().to_vec();
        let addr = record.addr;
        let slot = Arc::clone(&self.backend_settings);
        *slot.lock().unwrap() =
            (addr.clone(), vike_app_core::tool_views::BackendSettingsState::Pending);
        let ectx = ctx.clone();
        std::thread::Builder::new()
            .name("vt-backend-settings".into())
            .spawn(move || {
                let outcome = vike_app_core::tool_views::settings_fetch_state(
                    vike_tradehub_client::settings_show(addr.as_str(), &key),
                );
                let mut s = slot.lock().unwrap();
                if s.0 == addr {
                    s.1 = outcome;
                }
                drop(s);
                ectx.request_repaint();
            })
            .expect("spawn vt-backend-settings fetch thread");
    }

    /// Run ONE settings WRITE against the ACTIVE backend (split-plane REQ-7, write half) — the
    /// out-slot drain for [`vike_app_core::tool_views::SettingsWriteRequest`]:
    /// resolve the record's CONTROL-key name through the credential store (a write is a control
    /// action — the observe key cannot sign it), run the synchronous per-call
    /// `vike_tradehub_client::set_setting` on a throwaway thread, FOLD the outcome to its flow
    /// state there (`settings_write_state` is pure), and park it in
    /// `backend_settings_write_result` keyed by addr — dropped rather than painted if the
    /// operator switched backends mid-write (the fetch slot's discipline).
    ///
    /// An absent control key still runs: the daemon refuses the handshake and the section
    /// renders that refusal — the honest answer, same as the fetch's absent-observe-key arm. The
    /// audit `reason` is `None`: the section offers no rationale box, and the audit record
    /// already carries old→new.
    pub(crate) fn spawn_backend_settings_write(
        &mut self,
        ctx: &egui::Context,
        req: vike_app_core::tool_views::SettingsWriteRequest,
    ) {
        let Some(record) = self.active_backend.as_ref().map(|b| b.record.clone()) else {
            // No active backend to write to (it disconnected this frame): fold the refusal
            // locally so the flow never wedges in `Saving`.
            self.backend_settings_edit = vike_app_core::tool_views::settings_write_state(
                &req.key,
                Err(std::io::Error::other("no active backend")),
            );
            return;
        };
        let vars = workspace_credentials();
        let (_observe, control) = vike_app_core::backend_registry::resolve_keys(&record, &vars);
        let key_bytes = control.unwrap_or_default().as_bytes().to_vec();
        let addr = record.addr;
        let slot = Arc::clone(&self.backend_settings_write_result);
        let ectx = ctx.clone();
        std::thread::Builder::new()
            .name("vt-backend-settings-write".into())
            .spawn(move || {
                let outcome = vike_app_core::tool_views::settings_write_state(
                    &req.key,
                    vike_tradehub_client::set_setting(
                        addr.as_str(),
                        &key_bytes,
                        &req.file,
                        &req.key,
                        &req.value,
                        req.confirm.as_deref(),
                        None,
                    ),
                );
                *slot.lock().unwrap() = Some((addr, outcome));
                ectx.request_repaint();
            })
            .expect("spawn vt-backend-settings-write thread");
    }

    /// The venue-subscription bookkeeping (`feeds`/`subs`/`spawned`/`unroutable`/`feed_retries`)
    /// borrowed as the one bundle every moved feed-lifecycle function takes — see
    /// [`vike_app_core::feed_lifecycle`]'s module doc for why `App`'s fields become parameters.
    /// Disjoint field borrows, so `&mut self` here costs nothing at the call sites below.
    pub(crate) fn feed_slots(&mut self) -> feed_lifecycle::FeedSlots<'_> {
        feed_lifecycle::FeedSlots {
            feeds: &mut self.feeds,
            subs: &mut self.subs,
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
                subs: &mut self.subs,
                spawned: &mut self.spawned,
                unroutable: &mut self.unroutable,
                retries: &mut self.feed_retries,
            },
            feed_lifecycle::SeriesSlots {
                charts: &mut self.charts,
                hidden: &mut self.hidden,
                aggs: &mut self.aggs,
                of_aggs: &mut self.of_aggs,
            },
        )
    }

    /// Subscribe `venue`'s raw trade tape for `symbol`, once. Thin wrapper: the body moved to
    /// [`vike_app_core::feed_lifecycle::ensure_trade_feed_on`] so the merge gate compiles,
    /// clippies and TESTS it (this file is in no gate).
    pub(crate) fn ensure_trade_feed_on(&mut self, venue: &str, symbol: &str) {
        feed_lifecycle::ensure_trade_feed_on(&mut self.feed_slots(), venue, symbol);
    }

    /// SP3 Task 3: (idempotently) spawn a background aggTrades backfill thread for `symbol`,
    /// paging strictly OLDER than the live trades feed's warmup and feeding the SAME
    /// `OrderflowAgg` `key` registers in `of_aggs` (via `sync_from_core`'s `bf_rx` drain) — so
    /// CVD/profile fill across history instead of starting empty at whatever moment orderflow
    /// was toggled on. Called every frame from the `of_wanted` apply block, mirroring
    /// `ensure_trade_feed_on`'s own idempotent every-frame call; the actual spawn decision (and its
    /// run-once `bf_spawned` bookkeeping) is factored out into the pure
    /// [`feed_lifecycle::should_spawn_backfill`], and its paging floor into
    /// [`feed_lifecycle::backfill_earliest_ts`], so both are unit-testable without a full
    /// `App`/`eframe::CreationContext` (this file has no precedent for constructing a real `App`
    /// in `cargo test` — see `dom_position_tests` for the established pattern of testing extracted
    /// pure helpers instead). What is left here is the `vike_binance` thread body, which this
    /// crate must keep: `vike-app-core` deliberately links no venue bridge.
    ///
    /// **Not strictly once per symbol any more.** The walk reports how it stopped (#952's
    /// `BackfillOutcome`), the worker flattens that into a [`feed_lifecycle::BackfillReport`] on
    /// EVERY exit path, and [`feed_lifecycle::BackfillRetries`] decides whether that symbol may walk
    /// again — bounded, backed off, and only ever for a walk that delivered nothing (a delivering
    /// walk can never re-run: `OrderflowAgg::ingest` has no per-trade dedup, so a second delivery of
    /// the same band would double-count it). All of that policy is in `vike-app-core`, where CI
    /// compiles and tests it; what is here is the report-on-every-path discipline the policy needs.
    ///
    /// THIN build: no local trades history to backfill — replaced by a no-op stub.
    #[cfg(not(feature = "fat"))]
    pub(crate) fn maybe_spawn_backfill(&mut self, _symbol: &str, _key: &str) {}
    #[cfg(feature = "fat")]
    pub(crate) fn maybe_spawn_backfill(&mut self, symbol: &str, key: &str) {
        if !feed_lifecycle::should_spawn_backfill(
            self.of_backfill_hours,
            &mut self.bf_spawned,
            &mut self.bf_retries,
            symbol,
        ) {
            return;
        }
        // Backfill bound: the more-restrictive (newer) of this chart's oldest ALREADY-LOADED bar
        // and `of_backfill_hours` back from now — the arithmetic (and its saturation guard against
        // a hand-edited `workspace.json`) moved to the CI-gated
        // [`feed_lifecycle::backfill_earliest_ts`], where it is unit-tested; see its doc.
        //
        // Boundary note (T1 review): the pager's page-stop test is `page_min_ts <= earliest_ts`,
        // which can rarely miss trades tied at EXACTLY `earliest_ts` on a page boundary (see
        // `backfill_agg_trades_backward`'s doc comment in vike-binance). `earliest_ts` here is a
        // wall-clock-derived ms (now − hours) or a bar open-time — vanishingly unlikely to land
        // exactly on a real trade's ts — so this is acceptable.
        let oldest_bar_ot = self.charts.get(key).and_then(|c| c.bars.first()).map(|b| b.ot);
        let earliest_ts = feed_lifecycle::backfill_earliest_ts(
            chrono::Local::now().timestamp_millis(),
            self.of_backfill_hours,
            oldest_bar_ot,
        );
        let bf_tx = self.bf_tx.clone();
        let bf_done = self.bf_done_tx.clone();
        let earliest_ids = self.earliest_live_ids.clone();
        let sym = symbol.to_string();
        let spawned =
            std::thread::Builder::new().name(format!("of-backfill-{sym}")).spawn(move || {
                // Poll (≤ ~15s) for the live trades feed to report its startup splice's oldest id
                // (T2) — the strictly-older paging boundary (global-constraints.md's
                // no-double-count invariant).
                let mut min_live: Option<u64> = None;
                for _ in 0..75 {
                    // ~15s at a 200ms poll cadence
                    if let Some(&id) = earliest_ids.lock().unwrap().get(&sym) {
                        min_live = Some(id);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                let mut batch: Vec<vike_model::TradeTick> = Vec::new();
                // EVERY path out of this thread produces exactly one report, so a worker can never
                // leave its symbol stuck in-flight in `bf_retries`. The two id-less paths differ:
                // a `min_live` that never appeared means the live trades feed never came up (a
                // TRANSIENT local condition — that feed has its own `FeedRetries` ladder and may
                // well come up shortly), so it is reported as a zero-page failure and re-walked;
                // `backfill_before_id() == None` means `min_live <= 1`, i.e. there is genuinely no
                // room below the id floor to page into, which no amount of waiting changes.
                let report = match min_live {
                    None => feed_lifecycle::BackfillReport::failed(
                        &sym,
                        0,
                        "the live trades feed reported no earliest aggTrade id within ~15s — \
                         nothing to page back from yet",
                    ),
                    Some(min_live) => match orderflow::backfill_before_id(min_live) {
                        None => feed_lifecycle::BackfillReport::finished(&sym, 0),
                        Some(before_id) => {
                            let outcome = vike_binance::agg_trades_backfill_reported(
                                &sym,
                                before_id,
                                earliest_ts,
                                300,
                                &mut |tick| {
                                    batch.push(tick);
                                    if batch.len() >= 2000 {
                                        let _ =
                                            bf_tx.send((sym.clone(), std::mem::take(&mut batch)));
                                    }
                                },
                            );
                            // The venue's own `warn!` has already fired inside the walk; this maps
                            // its stop reason onto the vocabulary `vike-app-core` folds. `Capped`
                            // rides the `finished` arm deliberately — it is a budget decision the
                            // venue served in full, so re-running it would cap at the same page.
                            match outcome.stop {
                                vike_binance::BackfillStop::Failed(e) => {
                                    feed_lifecycle::BackfillReport::failed(&sym, outcome.pages, &e)
                                }
                                _ => feed_lifecycle::BackfillReport::finished(&sym, outcome.pages),
                            }
                        }
                    },
                };
                if !batch.is_empty() {
                    let _ = bf_tx.send((sym.clone(), batch));
                }
                // Sent AFTER the final batch so the UI thread can never see "this walk is done"
                // before the ticks it produced. (Only a walk that produced NONE is ever retried, so
                // this ordering is belt-and-braces rather than load-bearing.)
                let _ = bf_done.send(report);
            });
        if let Err(e) = spawned {
            // The symbol is already recorded in `bf_spawned` (and, on a retry, marked in-flight),
            // so without a report it would be burned for the session by an OS-level hiccup that
            // never touched the venue. Report it as a zero-page failure: nothing was fetched, so
            // the re-walk is safe, and it rides the same bounded ladder as a REST failure.
            tracing::warn!("of-backfill({key}): thread spawn failed: {e}");
            self.bf_retries.note_report(&feed_lifecycle::BackfillReport::failed(
                symbol,
                0,
                &format!("thread spawn failed: {e}"),
            ));
        }
    }

    /// Apply ONE window spawn: hand the decision to
    /// [`vike_app_core::window_spawn::plan_spawn`] and perform what it returns.
    ///
    /// Thin wrapper, deliberately: the six call sites in the frame loop below differ only in which
    /// `SpawnRequest` they build, so every one of them now reads as an explicit delegation instead
    /// of carrying its own copy of the cascade arithmetic, the `WinState` construction and the
    /// subscription. The steps run in the order `SpawnPlan` documents — counter, feed, depth
    /// stream, window, background resolve — because that is the order each inline site used and a
    /// spawn's ORDER is load-bearing: every site that subscribed anything subscribed BEFORE
    /// publishing its window, and the cockpit spawned its Gamma resolver AFTER pushing.
    ///
    /// `next_win_n` is adopted ahead of the subscribe here, where two of the six sites did it
    /// after. The two are independent: [`Self::ensure_feed_on`] reaches the feed layer through
    /// [`Self::feed_and_series_slots`], whose bundles borrow `feeds`/`subs`/`spawned`/`unroutable`/
    /// `feed_retries` and `charts`/`hidden`/`aggs`/`of_aggs` — a set disjoint from `next_win_n` and
    /// `wins`, and nothing below it reads either.
    pub(crate) fn apply_spawn(&mut self, ctx: &egui::Context, req: window_spawn::SpawnRequest) {
        let plan = window_spawn::plan_spawn(req, self.desktop.min, self.next_win_n);
        self.next_win_n = plan.next_win_n;
        if let Some(f) = &plan.ensure_feed {
            self.ensure_feed_on(ctx, &f.venue, &f.symbol, &f.interval, f.asset_class);
        }
        if let Some((venue, canonical)) = &plan.ensure_depth {
            self.ensure_depth(*venue, canonical);
        }
        // Bound BEFORE the window is moved out of `plan`, so the apply order below reads literally
        // top-to-bottom instead of touching a field of a partially-moved struct after the push.
        let resolve = plan.resolve_poly_token;
        if let Some(w) = plan.win {
            self.wins.push(w);
        }
        if let Some(wid) = resolve {
            self.spawn_poly_token_resolver(wid);
        }
    }

    /// Rebuild the whole window layout + global display settings from a loaded
    /// `Workspace` snapshot — the shared body of File → Open Workspace and the
    /// named-layout Load action. Feeds are (re)ensured per restored window on its
    /// own venue (cross-exchange search), and the display timezone is re-applied to
    /// the live charts the same frame (see the `menu.new_tz` arm for why the
    /// immediate `set_tz` loop is needed).
    pub(crate) fn restore_workspace(
        &mut self,
        ctx: &egui::Context,
        ws: &workspace::persist::Workspace,
    ) {
        self.display_tz = DisplayTz::parse(&ws.display_tz);
        self.of_backfill_hours = ws.of_backfill_hours;
        self.gpu_render = ws.gpu_render;
        self.indicator_favs = ws.indicator_favs.clone();
        let wins = workspace::persist::apply(ws);
        for w in &wins {
            self.ensure_feed_on(ctx, &w.venue, &w.symbol, &w.interval, w.asset_class);
        }
        self.next_win_n = wins.len() as u32;
        self.wins = wins;
        for cs in self.charts.values_mut() {
            cs.set_tz(self.display_tz);
        }
    }

    /// Spawn a feed for `(venue, symbol, interval)` if not already running, routing the bar
    /// subscription to `venue`'s [`vike_data::DataClient`] (cross-exchange symbol search). Kline
    /// intervals subscribe that venue's live klines; the `charts`/`subs`/`spawned` slots are keyed
    /// by [`workspace::series_key`] so Binance stays byte-identical (`"SYMBOL@interval"`) while
    /// other venues are namespaced (`"venue:SYMBOL@interval"`) and never collide on a shared symbol.
    /// Tick/volume intervals (Task B5) have no venue kline feed of their own — they're built
    /// client-side from `venue`'s raw trade tape (venue-aware: any venue whose feed implements
    /// `subscribe_trades` can drive tick/vol charts), so those spawn `venue`'s own trade feed
    /// (`ensure_trade_feed_on`) plus a per-chart-key aggregator in `aggs`.
    ///
    /// Thin wrapper: the body moved to [`vike_app_core::feed_lifecycle::ensure_feed_on`]. The
    /// `_ctx` parameter was already unused there and is NOT forwarded (every call site still
    /// passes one, so nothing here changed); the `shutdown` atomic stays owned by this binary and
    /// its already-loaded value is passed down.
    pub(crate) fn ensure_feed_on(
        &mut self,
        _ctx: &egui::Context,
        venue: &str,
        symbol: &str,
        interval: &str,
        asset_class: Option<vike_catalog::AssetClass>,
    ) {
        let display_tz = self.display_tz;
        // Shutting down (window-close requested / `on_exit` running): never start a NEW live feed.
        // Inert on the running path (a single Relaxed load; see `on_exit`).
        let shutting_down = self.shutdown.load(std::sync::atomic::Ordering::Relaxed);
        let (mut f, mut s) = self.feed_and_series_slots();
        feed_lifecycle::ensure_feed_on(
            &mut f,
            &mut s,
            feed_lifecycle::SeriesSpec { venue, symbol, interval, asset_class },
            display_tz,
            shutting_down,
        );
    }

    /// C2 tidy (FIX 3) + the feed-leak fix: reap any `spawned` feed no window references anymore,
    /// then the two coupled cleanups that follow it (dead tick/vol + orderflow AGGREGATOR entries,
    /// then any raw trade tape no remaining aggregator needs) — then the DOM/cockpit sibling
    /// reap, which stops any depth or cockpit book/trade stream whose windows are gone (the B2
    /// prerequisite: those ledgers used to be insert-only, so a closed DOM leaked its stream
    /// until process exit).
    ///
    /// Thin wrapper: the bodies moved to [`vike_app_core::feed_lifecycle::reap_orphaned_feeds`]
    /// and [`vike_app_core::feed_lifecycle::reap_orphaned_dom_cockpit_streams`] — see their docs
    /// for the full teardown contract, and `feed_slot_tests` for the teardown → re-add round
    /// trips that now run in the merge gate. Called once every frame (see the `update` call site)
    /// rather than threaded into every removal call site.
    ///
    /// The bundles are built inline here rather than through `feed_and_series_slots` because
    /// `wins` must be borrowed (immutably) at the same time — disjoint `App` fields, which the
    /// borrow checker only splits within one function body, not across a `&mut self` method call.
    pub(crate) fn reap_orphaned_feeds(&mut self) {
        let wins = &self.wins;
        let mut f = feed_lifecycle::FeedSlots {
            feeds: &mut self.feeds,
            subs: &mut self.subs,
            spawned: &mut self.spawned,
            unroutable: &mut self.unroutable,
            retries: &mut self.feed_retries,
        };
        let mut s = feed_lifecycle::SeriesSlots {
            charts: &mut self.charts,
            hidden: &mut self.hidden,
            aggs: &mut self.aggs,
            of_aggs: &mut self.of_aggs,
        };
        feed_lifecycle::reap_orphaned_feeds(&mut f, &mut s, wins);
        feed_lifecycle::reap_orphaned_dom_cockpit_streams(
            &mut self.feeds,
            &mut self.dom_depth,
            &mut self.poly_subs,
            &mut self.feed_retries,
            wins,
        );
    }

    /// Background-load the Data Manager's "Stored" inventory tree (Task 3) AND its per-series gap
    /// map (dm-gap-viz) — from WHICHEVER store the mode table says the grid reads
    /// (`vike_app_core::stored_mode::stored_mode` over the RESOLVED datahub address —
    /// `App::resolved_datahub_addr`, the SAME resolution `open_studio_store` and
    /// `backfill_route` read; REQ-2). This is the #1378 seam close: the
    /// wire backfill lands its rows in the SERVER's store, so a grid that kept reading the local
    /// one would never show them.
    ///
    /// - **remote** (`datahub_addr` set — BOTH builds): walks a fresh `RemoteHistStore` through
    ///   the `HistStore` TRAIT verbs (`vike_app_core::stored_load::load_stored_tree` —
    ///   `inventory` + one cheap `series_gaps` probe per series, all served since B12/PR-6), then
    ///   runs the SAME `load_partials` fold the local arm runs — `coverage_report` is a TRAIT verb
    ///   with a datahub wire verb behind it since spec §6-Q2. Whether the connected server answers
    ///   it is what the load reports back as `RemoteCoverage`: served → the Partial column renders
    ///   from the wire answer, refused (an older server) → an empty map plus
    ///   `stored_mode`'s honest note, never a silent empty.
    /// - **local** (`datahub_addr` unset — fat only): opens a FRESH `DataFusionHist` handle
    ///   (`open_local_hist_store` — its own tokio runtime + WAL recovery, never done on the UI
    ///   thread) and runs the SAME `load_stored_tree` walk AND the SAME `load_partials` fold (one
    ///   walk, one fold, two stores: neither the tree shape nor the Partial map can drift between
    ///   modes — pinned by that module's fixture-equality tests). A THIN build has
    ///   no local engine, so its unset arm stays the old no-op: the grid stays empty and
    ///   `open_studio_store`'s thin arm names the fix (set `config.datahub_addr`).
    ///
    /// Both arms deliver over `stored_tx` as one message (drained in `ui`, see the
    /// `stored_rx.try_recv()` call site). Called on the Stored tab's first render and on every
    /// Refresh click / post-delete reload — never per-frame, and gaps are never (re)computed
    /// per-frame either. A failed open/dial or inventory scan degrades to an empty tree + empty
    /// gap map (logged) rather than leaving the UI stuck on "Loading…" forever; a per-series
    /// `series_gaps` failure skips that one series' entry (logged inside the shared walk).
    pub(crate) fn refresh_stored(&mut self, ctx: &egui::Context) {
        use vike_app_core::stored_mode::RemoteCoverage;
        // Only `remote_addr` is read here — the Partial column's state is decided at RENDER time
        // from what this load negotiates, so the coverage input is `Unknown` at this call site.
        // The address is the RESOLVED one (REQ-2): the explicit `config.datahub_addr` when set,
        // else the active backend's advertisement.
        let mode = vike_app_core::stored_mode::stored_mode(
            self.resolved_datahub_addr().as_deref(),
            RemoteCoverage::Unknown,
        );
        if let Some(addr) = mode.remote_addr {
            self.stored_loading = true;
            let tx = self.stored_tx.clone();
            let c = ctx.clone();
            std::thread::spawn(move || {
                let store = vike_datahub_client::RemoteHistStore::new(addr.clone());
                let (tree, gaps) = match vike_app_core::stored_load::load_stored_tree(&store) {
                    Ok(walked) => walked,
                    Err(e) => {
                        tracing::warn!("Stored inventory: remote walk ({addr}) failed: {e}");
                        (Vec::new(), vike_data_manager::GapMap::new())
                    }
                };
                // The §6-Q2 half: the cross-kind report over the wire. An `Err` here is the
                // capability refusal a server older than the verb produces (refused CLIENT-side,
                // nothing sent) or a read failure — either way there is no report, so the column
                // gets the honest note rather than an empty map claiming "nothing is partial".
                let (partials, coverage) = match vike_app_core::stored_load::load_partials(&store) {
                    Ok(map) => (map, RemoteCoverage::Served),
                    Err(e) => {
                        tracing::warn!("Stored inventory: remote coverage ({addr}) failed: {e}");
                        (vike_data_manager::PartialDayMap::new(), RemoteCoverage::Unserved)
                    }
                };
                let _ = tx.send((tree, gaps, partials, coverage));
                c.request_repaint();
            });
            // ⚠ Under `fat` the LOCAL loader below would ALSO spawn without this — two threads
            // racing to `stored_tx.send`, the last to land overwriting the Stored tab with the
            // wrong inventory. `#[cfg]`-gated rather than a bare `return` because under `thin`
            // the block below is compiled out and this would be the function's last statement —
            // which is exactly how the edition-2024 `clippy --fix` pass, run under thin, came to
            // DELETE it: `needless_return` was true in the configuration it saw and false in the
            // one that ships. Caught by the per-hunk review of that patch, not by any gate: no
            // lane both compiles fat and exercises this path.
            #[cfg(feature = "fat")]
            return;
        }
        #[cfg(feature = "fat")]
        {
            self.stored_loading = true;
            let tx = self.stored_tx.clone();
            let c = ctx.clone();
            std::thread::spawn(move || {
                // Local mode negotiates nothing, so the coverage slot stays `Unknown` — and
                // `stored_mode` ignores it on this arm, which computes the column itself.
                let empty: StoredLoad = (
                    Vec::new(),
                    vike_data_manager::GapMap::new(),
                    vike_data_manager::PartialDayMap::new(),
                    RemoteCoverage::Unknown,
                );
                let (tree, gaps, partials, coverage) = match open_local_hist_store() {
                    Ok(store) => match vike_app_core::stored_load::load_stored_tree(&store) {
                        Ok((tree, gaps)) => {
                            // The cross-kind report is a SECOND manifest fold over the same store
                            // (no Parquet scan), so it rides this same load — through the SAME
                            // `load_partials` the remote arm calls, since §6-Q2 put it on the
                            // trait. A failure degrades to an empty map — the Partial column
                            // simply shows nothing — rather than failing the whole refresh and
                            // leaving the tab on "Loading…". (Unlike the remote arm this does NOT
                            // raise the note: a local store that cannot fold its own manifests is
                            // a store fault to read in the log, not a capability answer.)
                            let partials = match vike_app_core::stored_load::load_partials(&store) {
                                Ok(map) => map,
                                Err(e) => {
                                    tracing::warn!("Stored inventory: {e}");
                                    vike_data_manager::PartialDayMap::new()
                                }
                            };
                            (tree, gaps, partials, RemoteCoverage::Unknown)
                        }
                        Err(e) => {
                            tracing::warn!("Stored inventory: scan failed: {e}");
                            empty
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Stored inventory: open store failed: {e}");
                        empty
                    }
                };
                let _ = tx.send((tree, gaps, partials, coverage));
                c.request_repaint();
            });
        }
    }

    /// Data Manager "Stored" bulk Backfill/Update (dm-bulk-backfill): plans jobs via
    /// [`vike_app_core::backfill_plan::plan_routed_backfill_jobs`] (each selected series' known
    /// gap ranges when it has any, else a 30-day default lookback ending now) and runs them
    /// off-thread — WHERE depends on [`vike_app_core::backfill_route::backfill_route`] over
    /// the RESOLVED datahub address (`App::resolved_datahub_addr` — explicit `config.datahub_addr`
    /// first, else the active backend's advertisement; REQ-2), the SAME resolution
    /// `open_studio_store`'s local-vs-remote branch reads (split-plane REQ-9, the GUI half):
    ///
    /// - **unset** → [`Self::spawn_stored_backfill_local`]: today's path untouched —
    ///   `vike-backfill`'s keyless kline backfillers into the LOCAL store (fat only; thin has no
    ///   local engine and reports that in the status slot instead of no-opping). Only
    ///   `kind == "bar"` series on a `binance`/`bybit`/`okx` venue plan; the rest are counted
    ///   skipped.
    /// - **set** → [`Self::spawn_stored_backfill_wire`]: the SAME planned gap requests go through
    ///   the datahub backfill-on-demand verb (both builds — the wire arm is exactly what a
    ///   thin/remote setup needs). The rows land in the SERVER's store by write-through; the
    ///   completion-triggered `refresh_stored` re-reads. Venue support is the SERVER's roster on
    ///   this route (only `kind == "bar"` gates client-side): an off-roster venue surfaces the
    ///   server's own refusal text in the status slot, and an old server (no `backfill`
    ///   capability in its `Welcome`) is refused client-side with a status line saying it
    ///   predates backfill — never a silent no-op.
    ///
    /// `Update` (see the `resp.bulk` match in `tool_views::stored_tool_content`) ALIASES to this
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
        keys: Vec<vike_data_manager::SeriesKey>,
    ) {
        if self.stored_backfill_running || keys.is_empty() {
            return;
        }
        // 30 days: a sensible "never backfilled before" default lookback for a series with no
        // recorded gaps yet — long enough to seed a chart's worth of history, short enough that a
        // bulk backfill over many symbols doesn't page every one of them all the way back to the
        // venue's listing date.
        const DEFAULT_LOOKBACK_MS: i64 = 30 * 24 * 3_600_000;
        let route = backfill_route(self.resolved_datahub_addr().as_deref());
        let selected: std::collections::BTreeSet<_> = keys.into_iter().collect();
        let now_ms = chrono::Local::now().timestamp_millis();
        let (jobs, skipped) = plan_routed_backfill_jobs(
            &selected,
            &self.stored_gaps,
            now_ms,
            DEFAULT_LOOKBACK_MS,
            &route,
        );
        if jobs.is_empty() {
            self.stored_backfill_status =
                format!("Backfill: 0 series queued, {skipped} skipped (unsupported)");
            return;
        }
        match route {
            BackfillRoute::Wire { addr } => {
                self.spawn_stored_backfill_wire(ctx, jobs, skipped, addr)
            }
            BackfillRoute::Local => self.spawn_stored_backfill_local(ctx, jobs, skipped),
        }
    }

    /// The WIRE arm of [`Self::maybe_spawn_stored_backfill`] (split-plane REQ-9): the same
    /// worker-thread shape as the local arm — the wire call is blocking TCP, so it stays off the
    /// UI thread — but the blocking work is [`vike_app_core::backfill_wire::run_wire_backfill`],
    /// one datahub verb call per planned range. Nothing streams back through this thread: the
    /// rows are IN the server's store when `BackfillDone` arrives (write-through before serve),
    /// so completion just reports the rendered status through `stored_backfill_tx`, whose drain
    /// in `update` re-triggers `refresh_stored` exactly as the local arm's completion does.
    pub(crate) fn spawn_stored_backfill_wire(
        &mut self,
        ctx: &egui::Context,
        jobs: Vec<BackfillJob>,
        skipped: usize,
        addr: String,
    ) {
        self.stored_backfill_running = true;
        self.stored_backfill_status =
            backfill_wire::wire_backfill_running_status(jobs.len(), skipped, &addr);
        let tx = self.stored_backfill_tx.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let report = backfill_wire::run_wire_backfill(&addr, &jobs, skipped);
            let _ = tx.send(backfill_wire::render_wire_backfill_status(&report));
            c.request_repaint();
        });
    }

    /// The LOCAL arm of [`Self::maybe_spawn_stored_backfill`] — `vike-backfill`'s keyless kline
    /// backfillers into the local store, byte-identical in behavior to the pre-routing path —
    /// mirroring `refresh_stored`'s own-store-handle-per-run shape (a FRESH `DataFusionHist`
    /// handle, not one kept open across calls).
    ///
    /// THIN build: no local store engine exists, so the stub reports that in the status slot and
    /// names the fix (it used to be a silent no-op; in practice it is unreachable — a thin grid
    /// only ever LOADS in remote mode, `refresh_stored`'s remote arm, and there `backfill_route`
    /// sends the bulk action down the WIRE arm, never here).
    #[cfg(not(feature = "fat"))]
    pub(crate) fn spawn_stored_backfill_local(
        &mut self,
        _ctx: &egui::Context,
        _jobs: Vec<BackfillJob>,
        _skipped: usize,
    ) {
        self.stored_backfill_status = "Backfill unavailable: this thin (--observe) build has no \
                                       local store engine — set config.datahub_addr to a running \
                                       vike-datahub server, or connect a backend that advertises \
                                       one"
        .to_string();
    }
    #[cfg(feature = "fat")]
    pub(crate) fn spawn_stored_backfill_local(
        &mut self,
        ctx: &egui::Context,
        jobs: Vec<BackfillJob>,
        skipped: usize,
    ) {
        self.stored_backfill_running = true;
        self.stored_backfill_status =
            format!("Backfilling {} series ({skipped} skipped)…", jobs.len());
        let tx = self.stored_backfill_tx.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let mut ok = 0usize;
            let mut failed = 0usize;
            match open_local_hist_store() {
                Ok(store) => {
                    for job in &jobs {
                        let mut job_ok = true;
                        for &(start_ms, end_ms) in &job.ranges {
                            let res = match job.venue.as_str() {
                                "binance" => vike_backfill::backfill_binance_klines(
                                    &store,
                                    &job.symbol,
                                    &job.interval,
                                    start_ms,
                                    end_ms,
                                ),
                                "bybit" => vike_backfill::backfill_bybit_klines(
                                    &store,
                                    &job.symbol,
                                    &job.interval,
                                    start_ms,
                                    end_ms,
                                ),
                                "okx" => vike_backfill::backfill_okx_klines(
                                    &store,
                                    &job.symbol,
                                    &job.interval,
                                    start_ms,
                                    end_ms,
                                ),
                                other => {
                                    // Unreachable in practice: `plan_backfill_jobs` already
                                    // filters to `SUPPORTED_BACKFILL_VENUES`. Guarded anyway
                                    // (never a silent drop) since `job.venue` is a plain
                                    // `String`, not an enum the compiler can prove exhaustive
                                    // over.
                                    tracing::warn!("bulk backfill: unsupported venue {other:?}");
                                    job_ok = false;
                                    continue;
                                }
                            };
                            if let Err(e) = res {
                                tracing::warn!(
                                    "bulk backfill {}/{}/{} [{start_ms},{end_ms}] failed: {e}",
                                    job.venue,
                                    job.symbol,
                                    job.interval
                                );
                                job_ok = false;
                            }
                        }
                        if job_ok {
                            ok += 1;
                        } else {
                            failed += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("bulk backfill: open store failed: {e}");
                    failed = jobs.len();
                }
            }
            tracing::info!(
                "bulk backfill done: {ok} series backfilled, {failed} failed, {skipped} skipped (unsupported)"
            );
            let _ = tx.send(format!(
                "Backfill done: {ok} backfilled, {failed} failed, {skipped} skipped"
            ));
            c.request_repaint();
        });
    }

    /// Start a live L2 depth stream for `(venue, canonical)` (once) so the DOM renders that venue's
    /// real book. Routes to the venue's feed; dedups via `dom_depth`; the stream is stopped by that
    /// feed's `shutdown()` on exit. Called every frame for the DOM's selected venue — idempotent, so
    /// a venue switch just starts the new stream while the old one keeps its book warm.
    ///
    /// Thin wrapper: the body moved to [`vike_app_core::feed_lifecycle::ensure_depth`].
    pub(crate) fn ensure_depth(&mut self, venue: dom::DomVenue, canonical: &str) {
        feed_lifecycle::ensure_depth(
            &mut self.feeds,
            &mut self.dom_depth,
            &mut self.feed_retries,
            venue,
            canonical,
        );
    }

    /// Start (once) a live Polymarket L2 book + trade stream for `token` (the YES-outcome token-id)
    /// so a cockpit window renders that token's real book from the shared `BookStore` under venue
    /// `"polymarket"`. The polymarket feed's `subscribe_book` emits `LiveDataSink::book`, which the
    /// composition-root `CoreSinkAdapter::book` now also lands in the `BookStore` (see that method).
    /// The cockpit's analog of [`Self::ensure_depth`]; dedups via `poly_subs`, stopped by the feed's
    /// `shutdown()` on exit. A missing feed, an empty/placeholder token, or a subscribe error is
    /// logged and skipped — feed failure never crashes the GUI (the cockpit just stays STALE).
    ///
    /// The feed runs on its own thread and routes through the crate's proxy (`POLY_*` env, resolved
    /// internally); it connects only when the WS proxy is enabled AND reachable (US-geo-block),
    /// otherwise it retries with backoff and the book stays empty/STALE — graceful degradation.
    ///
    /// Thin wrapper: the body moved to [`vike_app_core::feed_lifecycle::ensure_poly_book`].
    pub(crate) fn ensure_poly_book(&mut self, token: &str) {
        feed_lifecycle::ensure_poly_book(
            &mut self.feeds,
            &mut self.poly_subs,
            &mut self.feed_retries,
            token,
        );
    }

    /// Resolve a real Polymarket market → its YES-outcome token-id AND a short display name OFF the
    /// UI thread (Gamma `list` is a blocking network call, routed through the crate's proxy), sending
    /// `(wid, token, name)` back on `poly_resolve_tx` for `update` to assign to the window's `symbol`
    /// + cache in `poly_names`. Simple first cut: the top-volume active crypto up/down market
    /// ([`pick_updown_token`]). On any error the window keeps its placeholder token and stays STALE —
    /// never a panic, never a blocked UI. Mirrors the off-thread `hl-feed-symbology` load.
    ///
    /// THIN build: no Polymarket bridge linked — replaced by a no-op stub.
    #[cfg(not(feature = "fat"))]
    pub(crate) fn spawn_poly_token_resolver(&self, _wid: egui::Id) {}
    #[cfg(feature = "fat")]
    pub(crate) fn spawn_poly_token_resolver(&self, wid: egui::Id) {
        let tx = self.poly_resolve_tx.clone();
        let _ = std::thread::Builder::new().name("poly-cockpit-resolve".into()).spawn(move || {
            match vike_polymarket::GammaClient::list(true, 200, 0) {
                Ok(markets) => match pick_updown_token(&markets) {
                    Some((token, name)) => {
                        tracing::info!("poly cockpit: resolved up/down YES token {token} ({name})");
                        let _ = tx.send((wid, token, name));
                    }
                    None => tracing::warn!("poly cockpit: no up/down market in the Gamma catalog"),
                },
                Err(e) => tracing::warn!("poly cockpit: Gamma market resolution failed: {e}"),
            }
        });
    }

    /// Fold the latest CoreSnapshot into the render model. Thin wrapper: the whole fold — the
    /// per-series kline dispatch behind the `seq` gate, the trade-tape drain that feeds the
    /// tick/volume + orderflow aggregators, and the status line — lives in
    /// [`vike_app_core::core_sync::sync_from_core`], moved down so the merge gate compiles,
    /// clippies and TESTS it (this file is in no gate; that fold's history is three
    /// silent-data-loss bugs whose only record was a code comment — see that module's doc). The
    /// arc-swap `load()` stays here so vike-app-core needs no `arc_swap` dependency.
    pub(crate) fn sync_from_core(&mut self) {
        let snap = self.snap_cell.load();
        core_sync::sync_from_core(
            core_sync::CoreSyncInputs {
                snap: &snap,
                spawned: &self.spawned,
                hidden: &self.hidden,
                display_tz: self.display_tz,
                trades: &self.trades,
                bf_rx: &self.bf_rx,
                feed_status: &self.feed_status,
                direct_bars: self.direct_bars.as_deref(),
            },
            core_sync::CoreSyncState {
                charts: &mut self.charts,
                aggs: &mut self.aggs,
                of_aggs: &mut self.of_aggs,
                bf_pending: &mut self.bf_pending,
                last_seq: &mut self.last_seq,
                last_direct_gen: &mut self.last_direct_gen,
                status: &mut self.status,
            },
        );
    }
}

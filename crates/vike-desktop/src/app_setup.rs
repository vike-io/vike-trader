//! `App::new` and the construction phases it runs, and the image-fetch threads they spawn.

use super::*;
use vike_datahub_client::tool_feeds::poll::{DatahubPollSnapshot, spawn::spawn_tool_fetchers};

impl App {
    /// Build the whole application — the bridge to the backend's remote core, every background
    /// fetcher, and the restored workspace. (It used to build a LOCAL trading core and a local
    /// venue feed plane too; see the tombstones in the body.)
    ///
    /// ⚠ **It takes the two things it uses, NOT the `eframe::CreationContext` eframe hands its
    /// creation closure.** On every target this app ships to, that type carries three
    /// `pub(crate)` fields (`window`, `raw_window_handle`, `raw_display_handle`), so nothing
    /// outside eframe can construct one — no struct literal, no `Default`, no builder. (Those
    /// three are `#[cfg(not(target_arch = "wasm32"))]` in eframe's `epi.rs`, so the wall is a
    /// native-target property rather than a universal one; this is a desktop binary, and a wasm
    /// build is not a configuration it has.) Naming it here therefore made
    /// `crates/vike-desktop/src/app_state.rs`'s `App` uncallable-into from a test BY CONSTRUCTION,
    /// in exchange for a borrow this function never took: every read was `egui_context` — fonts,
    /// visuals, the startup `content_rect`, the launcher-icon textures, the startup feed mounts,
    /// and the repaint-waker clone each feed and fetcher captures — plus the ONE
    /// `wgpu_render_state` read that builds the GPU candle pipeline below. Both of those members
    /// are public, and an `egui::Context` is `Default`-constructible.
    ///
    /// **This does NOT, on its own, make `App` testable, and pretending otherwise would be the
    /// whole value of the change gone.** The body below still spawns threads, opens stores and
    /// mounts live venues, so a `cargo test` calling it would do all of that for real. There were
    /// two walls; this removes the one the type system imposed incidentally and leaves the one
    /// somebody would have to CHOOSE to remove — splitting the mount out of the construction,
    /// which is a separate decision and not this signature's business.
    ///
    /// `render_state` is eframe's own `cc.wgpu_render_state`, threaded through unchanged: `Some`
    /// under the wgpu backend (eframe's default, and this crate takes eframe's default features —
    /// no feature of this crate's own ever touched that dependency, back when it had any), `None`
    /// otherwise, which degrades the chart to the egui candle painter exactly as before.
    ///
    /// The body is the construction in its ORIGINAL order, one named phase per step —
    /// [`App::mount_observer`], [`App::spawn_tool_data`], [`App::load_flag_images`],
    /// [`App::mount_catalog`], [`App::load_launcher_icons`],
    /// [`App::build_candle_pipeline`], the
    /// `Self { .. }` literal, then [`App::apply_startup_layout`]. It reads no process environment
    /// of its own: `env` is the [`GuiEnv`] `main` read once, kept as [`App::env`], and the settings
    /// rows it takes come through `main.rs`'s `startup_appearance` and `startup_env`, where
    /// `vike_config`'s consumed table pins their needles.
    pub(crate) fn new(
        egui_context: &egui::Context,
        render_state: Option<&eframe::egui_wgpu::RenderState>,
        // ⚠ The whole RECORD, not an address: it carries the credential-store KEY NAMES this
        // connection must sign with. `main` resolves it (flag / registry active record / local
        // default) and this arm connects from it unchanged.
        observe_backend: Option<vike_app_core::backend::registry::BackendRecord>,
        env: GuiEnv,
        // The tail `main` handed `vike_log::init`, which Help → Console shows.
        recent_log: vike_log::recent::RecentLines,
    ) -> Self {
        // The design system's install, from the five `preferences.*` rows this binary loaded at
        // boot (spec §5) — read by `startup_appearance`, mapped one crate down, where CI runs it.
        let appearance = startup_appearance();
        vike_ui_theme::appearance::install(egui_context, &appearance);
        let area = egui_context.content_rect();

        // The core-derived App bindings. There is NO local trading core and no local market-data
        // plane: the account-plane bindings are empty/None read-only twins and a background bridge
        // thread keeps `snapshot_cell` fresh from a remote `vike-tradehub` daemon (see the module
        // comment on `wire_to_core`). ⚠ This used to BRANCH on `--observe`, over three
        // compositions (local core / observe+feeds / observe-only); the `else` arm below is now a
        // tombstone and an `unreachable!`, kept only because `observe_backend` is still an
        // `Option` — `split_plane::observes` answers unconditionally true, so the `Some` arm always
        // wins.
        let BackendMount {
            active_backend,
            snapshot_cell,
            market_data_mount,
            feed_status,
            books,
            trades,
            direct_bars,
        } = Self::mount_observer(egui_context, observe_backend.as_ref());

        // The catalog first: the Options poll thread dials through the dial it holds.
        let (catalog, catalog_rx) = Self::mount_catalog(egui_context);
        let tools = Self::spawn_tool_data(egui_context, &catalog);
        let country_flags = Self::load_flag_images(egui_context);
        let news_logos = Self::load_news_logos(egui_context);
        // dm-bulk-backfill: the bulk-Backfill completion channel (see
        // `App::maybe_spawn_stored_backfill`) — same lazy, nothing-spawned-at-startup shape.
        let (stored_backfill_tx, stored_backfill_rx) = std::sync::mpsc::channel();
        let launcher_icons = Self::load_launcher_icons(egui_context);

        let gpu_ok = Self::build_candle_pipeline(render_state);

        // ⚠ TOMBSTONE — the JOURNAL MATERIALIZER stood here. Under `fat` this read
        // `vike_core::journal_config_from_env()` for the WAL directory, opened a
        // `vike_store::HistoryStoreDataFusion` at `tick_store_root()` and handed both to
        // the journal materializer's `maybe_spawn` (`vike_datahub::materialize` from 2026-10-11 until
        // 2026-11-18), which materialized the local core's journalled fills into the Tier-2 exec-fill log
        // off the fold. The desktop has no local core (rulings 1
        // and 2), so there is no journal here to read; the always-`None` binding, its `App` field
        // and its teardown step were deleted in 2026-10, and with them this crate's vike-journal
        // edge. The WAL is the TRADE daemon's; it sends its executions to the DATA daemon on the
        // execution stream (decision 0295), and `tearsheet --journal DIR` reads it directly.

        // Unified shutdown signal (see the `App::shutdown` field + `on_exit`): raised at window
        // close, then read by the bounded teardown. Inert until then.
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut app = Self {
            charts: HashMap::new(),
            shutdown,
            spawned: HashSet::new(),
            unroutable: HashSet::new(),
            feed_retries: vike_app_core::data::datahub::feed_retry::DatahubFeedRetries::default(),
            hidden: HashSet::new(),
            active_backend,
            backends: vike_app_core::backend::registry::load(),
            backend_editor: Default::default(),
            backend_settings: Arc::new(Mutex::new((String::new(), Default::default()))),
            backend_settings_edit: Default::default(),
            backend_settings_write_result: Arc::new(Mutex::new(None)),
            snapshot_cell,
            last_sequence: 0,
            published_series: Vec::new(),
            feeds: market_data_mount.feeds,
            feed_status,
            feed_statuses: market_data_mount.feed_statuses,
            market_data_session: market_data_mount.session,
            subscriptions: HashMap::new(),
            books,
            trades,
            direct_bars,
            chart_history: stored::ChartHistory::default(),
            chart_seed_note: std::sync::Arc::new(std::sync::Mutex::new(None)),
            last_direct_gen: 0,
            tick_volume_aggregators: HashMap::new(),
            orderflow_aggregators: HashMap::new(),
            trade_depth: HashMap::new(),
            polymarket_subscriptions: HashMap::new(),
            last_trade: None,
            directory: Arc::new(Mutex::new(Default::default())),
            chart_draw_pending: env.chart_draw.is_some(),
            windows: Vec::new(),
            window_views: HashMap::new(),
            sync_previous: HashMap::new(),
            sync_next: HashMap::new(),
            range_leader: HashMap::new(),
            status: DataClientStatus::default(),
            next_window_number: 0,
            show_rail: true,
            desktop: area,
            symbol_set_store: datasets::SymbolSetStore::default(),
            symbol_sets: Default::default(),
            did_initial_arrange: false,
            last_arrange: None,
            was_maximized: false,
            retile_frames: 0,
            tools,
            palette: String::new(),
            shot_n: 0,
            order_client_ids: vike_model::orders::client_id::OrderClientIdGenerator::new_v2(None),
            unanswered_orders: Default::default(),
            export_pending: false,
            export_sequence: 0,
            shot_slot: std::sync::Arc::default(),
            layout_name_prompt: false,
            layout_name_input: String::new(),
            layout_prompt_focus: false,
            help: HelpWindows::new(recent_log, &vike_studio::SHORTCUTS, env.shot_win.as_deref()),
            country_flags,
            news_logos,
            launcher_icons,
            symbols_catalog: Arc::new(vike_catalog::Catalog::from_instruments(Vec::new())),
            catalog_rx,
            catalog,
            indicator_favourites: Vec::new(),
            appearance: ThemeAppearanceSession::new(
                appearance,
                SETTINGS_DIR.get().and_then(Option::clone),
            )
            .with_vsync(startup_vsync())
            .with_display(startup_display(), gpu_ok),
            frame_cap: (!startup_vsync()).then(vike_app_core::ui::frame_cap::FrameCap::default),
            studio: None,
            studio_error: None,
            // Nothing is spawned yet: the first load runs the first time the Stored tab is shown
            // (or on a Refresh click), not at startup.
            stored: Default::default(),
            stored_backfill_running: false,
            stored_backfill_status: String::new(),
            stored_backfill_rx,
            stored_backfill_tx,
            // LAST: the two QA flags above are derived from it before it moves in.
            env,
        };

        Self::apply_startup_layout(&mut app, egui_context, area);
        app
    }

    /// Construction phase 1 — the observer mount: the connection to the backend's observe (and, when
    /// armed, control) channels and every core-derived binding `App` holds. All of those are
    /// empty/`None` read-only twins except the bridge itself. There is one arm; the `else` is a
    /// tombstone and an `unreachable!`. Returned as an [`BackendMount`] for `new`'s literal.
    pub(crate) fn mount_observer(
        egui_context: &egui::Context,
        observe_backend: Option<&vike_app_core::backend::registry::BackendRecord>,
    ) -> BackendMount {
        if let Some(observe_backend) = observe_backend {
            // ── --observe: THE mode — the read-only client ──────────────────────────────────────
            // Connect to a headless `vike-tradehub` daemon's observe server (Scope::Read) and
            // republish its pushed `CoreSnapshotWire`s into a GUI-owned arc-swap cell as
            // `CoreSnapshot`s. The ACCOUNT PLANE is the backend's: no local core, no exec engines,
            // no recorder, no journal materializer, no recon driver, no venue armed live. The
            // command choke point (a `CommandDispatch` built from `self.tradehub_control()`) is a read-only
            // no-op UNLESS a Scope::Write channel is ALSO mounted (`tradehub_control`, built below) —
            // then the GUI's order buttons drive the REMOTE core, which is the ONLY route an order
            // out of this process has (ruling 1).
            // Kline charts render from the daemon's bounded bar tail streamed on the wire
            // (`CoreSnapshotWire::bars` → `wire_to_core`). Connection health surfaces in the status bar
            // via the reused `feed_status` handle (`sync_from_core` shows it, or `snapshot.fault` if
            // the remote daemon faults).
            //
            // ⚠ The MARKET-DATA PLANE used to SPLIT here on the build (split-plane B2's arm
            // table): a fat `--observe` run mounted six keyless direct-to-venue feeds beside the
            // remote account plane. Ruling 2 removed that — see the tombstone at the feed bindings
            // below — so the market-data slots are unconditionally empty and every series either
            // comes off the wire or renders nothing.
            // Every dial below signs with this PC's device key: read it before the first one.
            refresh_device_key();

            let snapshot_cell: Arc<arc_swap::ArcSwap<vike_execution::CoreSnapshot>> =
                Arc::new(arc_swap::ArcSwap::from_pointee(vike_execution::CoreSnapshot::empty(
                    "observing",
                    "",
                )));
            // The bridge's connection-status handle — `App::feed_status` in BOTH observer
            // shapes: the account-plane link is the status bar's headline here; the per-venue
            // feed lines live in `feed_statuses` (Connections tool) in the third mode. It starts
            // as the dial the bridge thread is about to make, so the dot is amber from the first frame.
            let feed_status = Arc::new(std::sync::Mutex::new(DataClientStatus::new(
                DataClientConnectionState::Connecting,
                format!("connecting to {}…", observe_backend.address),
            )));
            let books = std::sync::Arc::new(data_sink::L2BookStore::default());
            let trades = std::sync::Arc::new(data_sink::TickStore::default());
            // ⚠ TOMBSTONE — THE THIRD MODE is gone. A `fat` build launched with `--observe` used
            // to take a SECOND arm here: it mounted `build_local_feeds`'s six keyless
            // direct-to-venue feeds over a core-free `data_sink::DataClientSinkGuiFeed`, so the trade tape,
            // the tick/volume + orderflow folds, DOM depth, the Polymarket cockpit and the
            // five-CEX kline charts were all CLIENT-DIRECT while the account plane
            // stayed remote. Ruling 2 removed it: the desktop opens NO venue socket.
            //
            // ⚠ AND THE MARKET-DATA PLANE IS BACK, from the OTHER side. `market_data_session::build` makes
            // the ONE session to the BACKEND's datahub (`MarketDataSession::sync_mount` then mounts one
            // `DataClientDatahub` per venue that datahub advertises, into the empty maps below), and
            // constructs the `DataClientSinkGuiFeed` that has had no production
            // constructor since the `fat` deletion — so the Trade window's ladder, the trade tape and the
            // tick/volume + orderflow folds paint again, on symbols the daemon is not trading. Two
            // decisions are load-bearing and both live one crate down where CI runs them:
            //
            //  * ⚠ `App::direct_bars` IS `Some` SINCE 2026-09-15, and the objection this comment
            //    used to raise is answered rather than ignored. It read: binding it `Some` "would
            //    reassign every kline series on the five CEX venues to a store nothing fills",
            //    because `core_sync` took `direct_bars.is_some()` AS the render-source mode
            //    signal. That conflation is gone — the signal is
            //    `split_plane::bar_plane(BackendMode::ObserveOnly)` = `BackendBarPlane::BackendStore`, under
            //    which a series renders from the store EXACTLY where the live snapshot does not
            //    publish it, on any venue. So nothing this daemon streams moves, and the store is
            //    filled by `vike_app_core::data::stored`' reads of the BACKEND's own hist store —
            //    which is what lets a 5m/15m/1h/4h/1d chart paint at all. The `market_data_session` wire
            //    still serves no bar lane (design §10) and still writes none of this.
            //  * the ADDRESS is not known yet. `resolved_datahub_address()`'s second rung is the
            //    active backend's `Welcome` advertisement, which lands after the observe bridge
            //    handshakes — i.e. after this function returns. The session is built address-less
            //    and `app_ui` pushes the resolution it already computes, once per frame.
            let market_data_mount = vike_app_core::data::market_data_session::build(
                // The session signs with this PC's device key, which `build` reads from the
                // `device_key` table under this settings directory (decision 0288).
                SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
                books.clone(),
                trades.clone(),
                {
                    let ctx = egui_context.clone();
                    move || ctx.request_repaint()
                },
            );
            let direct_bars: Option<std::sync::Arc<data_sink::StoredBarStore>> =
                Some(std::sync::Arc::new(data_sink::StoredBarStore::default()));

            // The whole connection — the push→CoreSnapshot bridge thread (B6's self-healing
            // reconnect loop) AND the opt-in Scope::Write write path — is now built by
            // [`vike_app_core::backend::connection::connect_backend`] from the record `main` RESOLVED.
            // For `--observe ADDR` (and for the local default) that record is the synthetic
            // [`vike_app_core::backend::connection::cli_observe_record`]; every dial signs with this
            // PC's device key, and control mounts only under the master gate (the
            // `flags.tradehub_control` row, read HERE by the binary) with that key held. ⚠ a mounted
            // control channel can place/cancel REAL orders on the daemon. This file keeps only the
            // two GUI-owned closures — the arc-swap store (so vike-app-core needs no `arc_swap`
            // dep, the same split `sync_from_core` uses) and the egui repaint wake. The returned
            // `BackendConnection` rides the tuple into `App::active_backend` (split-plane B1): held for
            // the app's lifetime unless the Connections picker switches it, its drop/replacement
            // stops-and-joins the bridge thread instead of leaking a dialling loop past the App.
            let active_backend = {
                // ⚠ The RESOLVED record, never a freshly synthesized one. `main` already chose it
                // — the flag and the local default become `cli_observe_record` there, and the
                // registry's active record arrives as itself, carrying the name and the arming the
                // operator configured.
                let record = observe_backend.clone();
                // After a reboot: the tunnel `vike-cli backend connect` recorded is raised again
                // when its mouth is silent, and the bridge below dials on until it answers.
                vike_app_core::backend::server_link::raise_tunnel_at_start(
                    SETTINGS_DIR.get().and_then(Option::as_deref),
                    &record.address,
                );
                let cell = snapshot_cell.clone();
                let ctx = egui_context.clone();
                Some(vike_app_core::backend::connection::connect_backend(
                    &record,
                    // The master gate: the `flags.tradehub_control` row of THIS PC's database.
                    settings().flags.tradehub_control,
                    // The bridge's poll: the `preferences.observe_poll_ms` row (read in `main.rs`).
                    observe_poll(),
                    link_patience(),
                    feed_status.clone(),
                    move |snapshot| cell.store(snapshot),
                    move || ctx.request_repaint(),
                ))
            };

            BackendMount {
                active_backend,
                snapshot_cell,
                market_data_mount,
                feed_status,
                books,
                trades,
                direct_bars,
            }
        } else {
            // ⚠ TOMBSTONE — THE LOCAL TRADING CORE STOOD HERE, and ~820 lines went with it.
            //
            // This was `App::new`'s second arm: the one that ran whenever nobody asked to observe.
            // It is deleted by ruling — orders leave the desktop ONLY through the backend, and the
            // desktop opens NO venue socket at all — so the arm has no second side left and this
            // `else` is unreachable by construction. What it did, named so a future reader does not
            // go looking for it in the history of a file that no longer mentions it:
            //
            //   • the MOUNT (it was `vike_run`'s then; `vike-run` merged into `vike-tradehub-core`,
            //     decision 0098) — `vike_tradehub_core::AdapterPolicy::from(&settings().policy)`,
            //     `vike_tradehub_core::armed_live_venues`, `vike_tradehub_core::venue_arming`, a
            //     `vike_tradehub_core::RuntimeConfig` carrying the GUI's `vike_core::CoreConfig` knobs, and
            //     `vike_tradehub_core::assemble_runtime`,
            //     which assembled every `vike_tradehub_core::make_engine` arm (the twelve-venue mount),
            //     spawned the single-writer core and forwarded live venue events. A assemble_runtime
            //     fault exited the process with code 2 before a window existed.
            //   • the LIVE-ACCOUNT LOCK — `vike_boot::live_lock::LiveLock::acquire` per armed live
            //     route, claimed ONE statement above `assemble_runtime`; that ordering was load-bearing
            //     (claim before anything authenticates) and is pinned by
            //     `crates/vike-tradehub/tests/source_gates/live_lock_claim_order_gate.rs`, whose vike-app root row
            //     was REMOVED with this code (the gate's tombstone says why it was not re-pointed).
            //   • RECONCILIATION — the quarantine-first policy default over the REAL process env
            //     (a helper decision 0111 deleted with that env read), `reconcile_gate` over
            //     `flags.reconcile`/`flags.reconcile_off`
            //     and the armed-live COUNT, `log_reconcile_gate`, the per-venue health map derived
            //     from the feed statuses, the `journal_view_provider` hook, and the
            //     `vike_core::spawn_reconcile` driver mount. Pinned by
            //     `crates/vike-tradehub/tests/source_gates/reconcile_gate_wiring_gate.rs`'s vike-app root, likewise
            //     REMOVED rather than left naming absent code. The daemon is the only reconciling
            //     root left.
            //   • the RUN PROFILE and its guards — `vike_core::resolve_profile` +
            //     `ProfileRun::apply_guards_and_sinks`, the `GuardsReport` unwired-key warning, and
            //     `vike_execution` risk limits derived from each venue's fetched `SymbolProperties`.
            //   • the LOCAL DATA PLANE — `open_tick_recorder` (the DataFusion+Parquet
            //     `RecorderSink` and its equity sampler), the `CoreSinkAdapter`/`DataClientSinkTee`
            //     composition, `vike_data::PropertiesRecorder::open_from_env`, the mmap
            //     health-counters pre-flight (`counters_file_path` +
            //     `vike_core::counters::CountersFile::create`), the opt-in Deribit DVOL recorder
            //     (a `flags.toml` key, since DELETED — `vike_config::DEAD_FLAG_KEYS` →
            //     `spawn_deribit_feed_dvol`), and `build_local_feeds` — the six keyless
            //     direct-to-venue market-data feeds.
            //
            // ⚠ The settings keys those reads consumed lose their consumer in THIS binary with
            // them: `config.state_dir` (via `state_dir_path`),
            // `VIKE_COUNTERS_FILE`, `VIKE_TICK_STORE`. `vike_config::CONSUMPTION` and
            // `vike_config::settings::SETTINGS` both key on that, and neither is updated here.
            unreachable!(
                "the desktop has no local trading core — `split_plane::observes` is \
                 unconditionally true, so `observe_backend` is always Some and the observer arm \
                 always wins"
            )
        }
    }

    /// Construction phase 2 — the tool windows' data (News / Calendar / Options), fetched on
    /// background threads. The Options poll asks the backend's datahub through the CATALOG's dial
    /// (`DatahubCatalogRefresh::dial_snapshot`, re-pointed every frame by `window_frame`), so the two
    /// datahub clients follow one backend switch together; the Refresh pill reaches the thread
    /// through `DatahubPollSnapshot::options_link`. (The option-chain recorder left this binary for the
    /// datahub, which owns the store.)
    pub(crate) fn spawn_tool_data(
        egui_context: &egui::Context,
        catalog: &Arc<vike_app_core::data::catalog_refresh::DatahubCatalogRefresh>,
    ) -> Arc<Mutex<DatahubPollSnapshot>> {
        let tools = Arc::new(Mutex::new(DatahubPollSnapshot::default()));
        let (c, cat) = (egui_context.clone(), Arc::clone(catalog));
        spawn_tool_fetchers(
            tools.clone(),
            move || c.request_repaint(),
            move || cat.dial_snapshot(),
        );
        tools
    }

    /// Construction phase 3 — the country-flag textures, decoded from the PNGs bundled in the
    /// binary (`vike_calendar::FLAG_PNGS`). No download: the desktop opens no socket for a picture.
    fn load_flag_images(egui_context: &egui::Context) -> HashMap<String, egui::TextureHandle> {
        let mut flags = HashMap::new();
        for (iso, bytes) in vike_calendar::FLAG_PNGS {
            if let Some(tex) = load_png_texture(egui_context, &format!("flag_{iso}"), bytes) {
                flags.insert((*iso).to_string(), tex);
            }
        }
        flags
    }

    /// Construction phase 3b — the News providers' icon textures, decoded from the PNGs bundled in
    /// the binary (`vike_news::NEWS_LOGO_PNGS`), keyed by SOURCE NAME as `NewsItem::source` spells
    /// it. No download; a source with no row (or an undecodable file) simply keeps the
    /// coloured-initial avatar the News list draws.
    fn load_news_logos(egui_context: &egui::Context) -> HashMap<String, egui::TextureHandle> {
        let mut logos = HashMap::new();
        for (source, bytes) in vike_news::NEWS_LOGO_PNGS {
            if let Some(tex) = load_png_texture(egui_context, &format!("news_{source}"), bytes) {
                logos.insert((*source).to_string(), tex);
            }
        }
        logos
    }

    /// Construction phase 4 — the symbol-search catalog's owner (`DatahubCatalogRefresh`) with its first
    /// publish already spawned. Returns it with the receiving half the frame drains the universe from.
    pub(crate) fn mount_catalog(
        egui_context: &egui::Context,
    ) -> (
        Arc<vike_app_core::data::catalog_refresh::DatahubCatalogRefresh>,
        Receiver<vike_catalog::Catalog>,
    ) {
        // Symbol-search catalog: the DATAHUB keeps every venue's list (`<project>/instruments` on the
        // server, refreshed there on the operator's command) and `DatahubCatalogRefresh::set_dial` reads them when the dial
        // appears; the universe is published over `catalog_rx` (drained in `update`) and quick-picks
        // stand in until it lands. This binary keeps no catalog file: its old `settings/state/catalog.json`
        // is read by nothing. It links NO catalog provider, so every Refresh is a routed press.
        let (catx, catalog_rx) = std::sync::mpsc::channel();
        let catalog = Arc::new(vike_app_core::data::catalog_refresh::DatahubCatalogRefresh::new(
            Vec::new(),
            catx,
        ));
        {
            let (c, handle) = (egui_context.clone(), Arc::clone(&catalog));
            handle.spawn_initial(move || c.request_repaint());
        }
        (catalog, catalog_rx)
    }

    /// Construction phase 5 — vike's launcher icons: the bundled PNGs decoded into textures.
    fn load_launcher_icons(egui_context: &egui::Context) -> HashMap<String, egui::TextureHandle> {
        // launcher icons (bundled PNGs rendered from vike's icons.py) → textures
        let mut launcher_icons = HashMap::new();
        for (name, bytes, _) in LAUNCHERS {
            if let Some(tex) = load_png_texture(egui_context, name, bytes) {
                launcher_icons.insert(name.to_string(), tex);
            }
        }
        launcher_icons
    }

    /// Construction phase 6 — the GPU candle pipeline. Returns `gpu_ok`: `true` once the pipeline is
    /// registered in the wgpu `callback_resources`, `false` when the backend is absent or the build
    /// failed (the chart then degrades to the egui candle painter).
    fn build_candle_pipeline(render_state: Option<&eframe::egui_wgpu::RenderState>) -> bool {
        // GPU candle layer (GPU Phase 2, Task 2): build the wgpu pipeline ONCE and stash it in
        // egui-wgpu's `callback_resources` so `chart_gpu::CandleCallback::{prepare,paint}` can
        // fetch it by type. `render_state` is eframe's own `cc.wgpu_render_state`, handed in by
        // `main`'s creation closure: `Some` under the wgpu backend (eframe's default), `None`
        // degrades to the egui candle painter. Wrap the build in `catch_unwind` so a driver or
        // shader validation panic also degrades (gpu_ok = false) instead of aborting startup —
        // Task 3 gates the render toggle on `gpu_ok`.
        let mut gpu_ok = false;
        if let Some(rs) = render_state {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                chart_gpu::CandlePipeline::new(&rs.device, rs.target_format)
            })) {
                Ok(pipe) => {
                    rs.renderer.write().callback_resources.insert(pipe);
                    gpu_ok = true;
                }
                Err(_) => tracing::warn!(
                    "GPU candle pipeline build failed; GPU chart layer disabled (egui fallback)."
                ),
            }
        } else {
            tracing::info!("no wgpu render state; GPU chart layer disabled (egui fallback).");
        }
        gpu_ok
    }

    /// Construction phase 7 — the startup layout, applied to the freshly built `app` in the order
    /// `StartupLayout` documents: globals, feeds, the restore log, the window list, the background
    /// Gamma resolve, then `next_window_number`.
    fn apply_startup_layout(app: &mut App, egui_context: &egui::Context, area: egui::Rect) {
        // ── startup layout ──────────────────────────────────────────────────────────────────────
        // The whole decision is [`vike_app_core::ui::startup`]'s; its input is built by `main.rs`'s
        // `startup_env` (the env knobs out of `App::env`, plus the `preferences.chart_style` row).
        let startup_env = startup_env(&app.env);
        let restored = startup::restored_workspace(startup_env.shot_mode);
        let layout = startup::plan(area, &startup_env, restored);
        // A refused (older build's) workspace or layout says so on the status line.
        app.status.text = layout.refused.unwrap_or(std::mem::take(&mut app.status.text));
        // Applied in the ORDER `StartupLayout` documents, which is the exact order this function
        // performed these steps before the move: globals, then the feeds (both arms that ensure
        // anything did so BEFORE publishing their windows), then the restore log, then the window
        // list, then `next_window_number`. A `fn new`'s startup order is load-bearing, so it is reproduced,
        // not tidied.
        if let Some(favourites) = layout.indicator_favourites {
            app.indicator_favourites = favourites;
        }
        for f in &layout.ensure_feeds {
            app.ensure_feed_on(egui_context, f);
        }
        if let Some(n) = layout.restored_windows {
            tracing::info!("workspace restored ({n} windows)");
        }
        app.windows = layout.wins;
        if let Some((wid, destination)) = layout.data_destination {
            // Pre-seed the window's `WindowView` so the Data Manager opens on the requested rail
            // destination. The window loop `entry(..).or_default()`s this map, so writing it here is
            // simply what it will find — no ordering subtlety beyond being before the first frame.
            app.window_views.entry(wid).or_default().data_destination = destination;
        }
        if let Some((wid, account)) = layout.gateway_account {
            // Same pre-seed as the sub-tab above, for the same reason: the window loop
            // `entry(..).or_default()`s this map, so writing it here is simply what it will find.
            // ⚠ It is a PENDING value, not a setting — `data_body`'s `DataManagerDestination::Gateway` arm
            // `take`s it on the first frame it renders, so the operator's own chip clicks are
            // never re-overridden.
            app.window_views.entry(wid).or_default().gateway_account = Some(account);
        }
        app.next_window_number = app.windows.len() as u32;
    }
}

/// What `App::mount_observer` builds: the core-derived `App` bindings, bundled so the observer arm
/// can leave `App::new` without a nine-element tuple. Every field lands in `new`'s `Self { .. }`
/// literal under the same name (`market_data_mount` through its `Self` fields).
pub(crate) struct BackendMount {
    pub(crate) active_backend: Option<vike_app_core::backend::connection::BackendConnection>,
    pub(crate) snapshot_cell: Arc<arc_swap::ArcSwap<vike_execution::CoreSnapshot>>,
    pub(crate) market_data_mount: vike_app_core::data::market_data_session::MarketDataSessionMount,
    pub(crate) feed_status: Arc<std::sync::Mutex<DataClientStatus>>,
    pub(crate) books: std::sync::Arc<data_sink::L2BookStore>,
    pub(crate) trades: std::sync::Arc<data_sink::TickStore>,
    pub(crate) direct_bars: Option<std::sync::Arc<data_sink::StoredBarStore>>,
}

fn load_png_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
    Some(ctx.load_texture(format!("icon_{name}"), ci, egui::TextureOptions::LINEAR))
}

// ⚠ TOMBSTONE — `spawn_catalog_fetcher` stood here. It fetched `CatalogProviderDeribit` on EVERY start,
// sent the result once and cached nothing, so the `vike_catalog::persist` cache — whose own doc has
// said "Loaded instantly at startup; rewritten only on an explicit user refresh" since it was
// written — was referenced by nothing in the tree. `vike_app_core::data::catalog_refresh::DatahubCatalogRefresh`
// replaces it: same channel, plus the cache, the per-venue stamp and the button (and, since the
// options move to the datahub, no local provider at all — deribit is a routed venue like the rest).
// It lives one crate down because `vike-desktop` is outside the derived CI roster, so a refresh
// spelled here would be compiled by `app-check` and tested by nothing.

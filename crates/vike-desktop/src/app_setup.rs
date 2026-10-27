//! `App::new` and the construction phases it runs, and the image-fetch threads they spawn.

use super::*;

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
    /// in exchange for a borrow this function never took: every read was `egui_ctx` — fonts,
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
        egui_ctx: &egui::Context,
        render_state: Option<&eframe::egui_wgpu::RenderState>,
        // ⚠ The whole RECORD, not an address: it carries the credential-store KEY NAMES this
        // connection must sign with. `main` resolves it (flag / registry active record / local
        // default) and this arm connects from it unchanged.
        observe_backend: Option<vike_app_core::backend::backend_registry::BackendRecord>,
        env: GuiEnv,
    ) -> Self {
        // The design system's install, from the five `preferences.*` rows this binary loaded at
        // boot (spec §5) — read by `startup_appearance`, mapped one crate down, where CI runs it.
        let appearance = startup_appearance();
        vike_ui_theme::appearance::install(egui_ctx, &appearance);
        let area = egui_ctx.content_rect();

        // The core-derived App bindings. There is NO local trading core and no local market-data
        // plane: the account-plane bindings are empty/None read-only twins and a background bridge
        // thread keeps `snap_cell` fresh from a remote `vike-tradehub` daemon (see the module
        // comment on `wire_to_core`). ⚠ This used to BRANCH on `--observe`, over three
        // compositions (local core / observe+feeds / observe-only); the `else` arm below is now a
        // tombstone and an `unreachable!`, kept only because `observe_backend` is still an
        // `Option` — `split_plane::observes` answers unconditionally true, so the `Some` arm always
        // wins.
        let ObserverMount {
            active_backend,
            snap_cell,
            md_mount,
            feed_status,
            books,
            trades,
            direct_bars,
        } = Self::mount_observer(egui_ctx, observe_backend.as_ref());

        // The catalog first: the Options poll thread dials through the dial it holds.
        let (catalog, catalog_rx) = Self::mount_catalog(egui_ctx);
        let tools = Self::spawn_tool_data(egui_ctx, &catalog);
        let flags = Self::load_flag_images(egui_ctx);
        let news_logos = Self::load_news_logos(egui_ctx);
        // Data Manager "Stored" view: the inventory-load channel (see `App::refresh_stored`).
        // Nothing is spawned yet — the first load is triggered lazily, the first time the
        // Stored tab is shown (or by a Refresh click), not at startup.
        let (stored_tx, stored_rx) = std::sync::mpsc::channel();
        // dm-bulk-backfill: the bulk-Backfill completion channel (see
        // `App::maybe_spawn_stored_backfill`) — same lazy, nothing-spawned-at-startup shape.
        let (stored_backfill_tx, stored_backfill_rx) = std::sync::mpsc::channel();
        let launcher_icons = Self::load_launcher_icons(egui_ctx);

        let gpu_ok = Self::build_candle_pipeline(render_state);

        // ⚠ TOMBSTONE — the JOURNAL MATERIALIZER stood here. Under `fat` this read
        // `vike_core::journal_config_from_env()` for the WAL directory, opened a
        // `vike_store::DataFusionHist` at `tick_store_root()` and handed both to
        // the journal materializer's `maybe_spawn` (`vike_datahub::materialize` since 2026-10-11),
        // which materialized the local core's journalled fills into the Tier-2 exec-fill log off the
        // fold. The desktop has no local core (rulings 1
        // and 2), so there is no journal here to read; the always-`None` binding, its `App` field
        // and its teardown step were deleted in 2026-10, and with them this crate's vike-journal
        // edge. The WAL is the TRADE daemon's; since 2026-10-11 the DATA daemon materializes it, off
        // by default (the `flags.journal_materializer` row), and `tearsheet --journal DIR` reads it
        // directly.

        // Unified shutdown signal (see the `App::shutdown` field + `on_exit`): raised at window
        // close, then read by the bounded teardown. Inert until then.
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut app = Self {
            charts: HashMap::new(),
            shutdown,
            display_tz: vike_chart::DisplayTz::Local,
            spawned: HashSet::new(),
            unroutable: HashSet::new(),
            feed_retries: feed_lifecycle::FeedRetries::default(),
            hidden: HashSet::new(),
            active_backend,
            backends: vike_app_core::backend::backend_registry::load(),
            backend_editor: Default::default(),
            backend_settings: Arc::new(Mutex::new((String::new(), Default::default()))),
            backend_settings_edit: Default::default(),
            backend_settings_write_result: Arc::new(Mutex::new(None)),
            snap_cell,
            last_seq: 0,
            published_series: Vec::new(),
            feeds: md_mount.feeds,
            feed_status,
            feed_statuses: md_mount.feed_statuses,
            md_session: md_mount.session,
            subs: HashMap::new(),
            books,
            trades,
            direct_bars,
            store_asked: HashSet::new(),
            chart_seed_note: std::sync::Arc::new(std::sync::Mutex::new(None)),
            last_direct_gen: 0,
            aggs: HashMap::new(),
            of_aggs: HashMap::new(),
            trade_depth: HashMap::new(),
            poly_subs: HashMap::new(),
            last_trade: None,
            directory: Arc::new(Mutex::new(Default::default())),
            chart_draw_pending: env.chart_draw.is_some(),
            wins: Vec::new(),
            tool_views: HashMap::new(),
            sync_prev: HashMap::new(),
            sync_next: HashMap::new(),
            range_leader: HashMap::new(),
            status: "connecting to Binance…".into(),
            next_win_n: 0,
            show_rail: true,
            desktop: area,
            datasets: datasets::Store::load(),
            did_initial_arrange: false,
            last_arrange: None,
            was_maximized: false,
            retile_frames: 0,
            tools,
            palette: String::new(),
            shot_n: 0,
            coids: vike_model::orders::client_order_id::ClientOrderIdGenerator::new(None),
            export_pending: false,
            export_seq: 0,
            shot_slot: std::sync::Arc::default(),
            layout_name_prompt: false,
            layout_name_input: String::new(),
            layout_prompt_focus: false,
            flags,
            news_logos,
            launcher_icons,
            symbols_catalog: Arc::new(vike_catalog::Catalog::from_instruments(Vec::new())),
            catalog_rx,
            catalog,
            gpu_ok,
            gpu_render: false,
            indicator_favs: Vec::new(),
            appearance: AppearanceSession::new(
                appearance,
                SETTINGS_DIR.get().and_then(Option::clone),
            )
            .with_vsync(startup_vsync()),
            frame_cap: (!startup_vsync()).then(vike_app_core::ui::frame_cap::FrameCap::default),
            studio: None,
            studio_error: None,
            stored_tree: Arc::new(Vec::new()),
            stored_gaps: Arc::new(HashMap::new()),
            stored_partials: Arc::new(vike_data_manager::PartialDayMap::new()),
            stored_coverage: vike_app_core::data::stored_mode::RemoteCoverage::Unknown,
            stored_history: None,
            stored_error: None,
            stored_rx,
            stored_tx,
            stored_loading: false,
            stored_backfill_running: false,
            stored_backfill_status: String::new(),
            stored_backfill_rx,
            stored_backfill_tx,
            // LAST: the two QA flags above are derived from it before it moves in.
            env,
        };

        Self::apply_startup_layout(&mut app, egui_ctx, area);
        app
    }

    /// Construction phase 1 — the observer mount: the connection to the backend's observe (and, when
    /// armed, control) channels and every core-derived binding `App` holds. All of those are
    /// empty/`None` read-only twins except the bridge itself. There is one arm; the `else` is a
    /// tombstone and an `unreachable!`. Returned as an [`ObserverMount`] for `new`'s literal.
    pub(crate) fn mount_observer(
        egui_ctx: &egui::Context,
        observe_backend: Option<&vike_app_core::backend::backend_registry::BackendRecord>,
    ) -> ObserverMount {
        if let Some(observe_backend) = observe_backend {
            // ── --observe: THE mode — the read-only client ──────────────────────────────────────
            // Connect to a headless `vike-tradehub` daemon's observe server (Scope::Read) and
            // republish its pushed `WireSnapshot`s into a GUI-owned arc-swap cell as
            // `CoreSnapshot`s. The ACCOUNT PLANE is the backend's: no local core, no exec engines,
            // no recorder, no journal materializer, no recon driver, no venue armed live. The
            // command choke point (a `Dispatch` built from `self.remote_ctrl()`) is a read-only
            // no-op UNLESS a Scope::Write channel is ALSO mounted (`remote_ctrl`, built below) —
            // then the GUI's order buttons drive the REMOTE core, which is the ONLY route an order
            // out of this process has (ruling 1).
            // Kline charts render from the daemon's bounded bar tail streamed on the wire
            // (`WireSnapshot::bars` → `wire_to_core`). Connection health surfaces in the status bar
            // via the reused `feed_status` handle (`sync_from_core` shows it, or `snap.fault` if
            // the remote daemon faults).
            //
            // ⚠ The MARKET-DATA PLANE used to SPLIT here on the build (split-plane B2's arm
            // table): a fat `--observe` run mounted six keyless direct-to-venue feeds beside the
            // remote account plane. Ruling 2 removed that — see the tombstone at the feed bindings
            // below — so the market-data slots are unconditionally empty and every series either
            // comes off the wire or renders nothing.
            let addr: String = observe_backend.addr.clone();
            let vars = workspace_credentials();

            let snap_cell: Arc<arc_swap::ArcSwap<vike_exec::CoreSnapshot>> = Arc::new(
                arc_swap::ArcSwap::from_pointee(vike_exec::CoreSnapshot::empty("observing", "")),
            );
            // The bridge's connection-status handle — `App::feed_status` in BOTH observer
            // shapes: the account-plane link is the status bar's headline here; the per-venue
            // feed lines live in `feed_statuses` (Connections tool) in the third mode.
            let feed_status: Arc<std::sync::Mutex<String>> =
                Arc::new(std::sync::Mutex::new(format!("connecting to {addr}…")));
            let books = std::sync::Arc::new(data_sink::BookStore::default());
            let trades = std::sync::Arc::new(data_sink::TradeStore::default());
            // ⚠ TOMBSTONE — THE THIRD MODE is gone. A `fat` build launched with `--observe` used
            // to take a SECOND arm here: it mounted `build_local_feeds`'s six keyless
            // direct-to-venue feeds over a core-free `data_sink::GuiFeedSink`, so the trade tape,
            // the tick/volume + orderflow folds, DOM depth, the Polymarket cockpit and the
            // five-CEX kline charts were all CLIENT-DIRECT while the account plane
            // stayed remote. Ruling 2 removed it: the desktop opens NO venue socket.
            //
            // ⚠ AND THE MARKET-DATA PLANE IS BACK, from the OTHER side. `md_session::build` makes
            // the ONE session to the BACKEND's datahub (`MdSession::sync_mount` then mounts one
            // `DatahubFeed` per venue that datahub advertises, into the empty maps below), and
            // constructs the `GuiFeedSink` that has had no production
            // constructor since the `fat` deletion — so the Trade window's ladder, the trade tape and the
            // tick/volume + orderflow folds paint again, on symbols the daemon is not trading. Two
            // decisions are load-bearing and both live one crate down where CI runs them:
            //
            //  * ⚠ `App::direct_bars` IS `Some` SINCE 2026-09-15, and the objection this comment
            //    used to raise is answered rather than ignored. It read: binding it `Some` "would
            //    reassign every kline series on the five CEX venues to a store nothing fills",
            //    because `core_sync` took `direct_bars.is_some()` AS the render-source mode
            //    signal. That conflation is gone — the signal is
            //    `split_plane::bar_plane(AppMode::ObserveOnly)` = `BarPlane::BackendStore`, under
            //    which a series renders from the store EXACTLY where the live snapshot does not
            //    publish it, on any venue. So nothing this daemon streams moves, and the store is
            //    filled by `vike_app_core::data::store_bars`' reads of the BACKEND's own hist store —
            //    which is what lets a 5m/15m/1h/4h/1d chart paint at all. The `md_session` wire
            //    still serves no bar lane (design §10) and still writes none of this.
            //  * the ADDRESS is not known yet. `resolved_datahub_addr()`'s second rung is the
            //    active backend's `Welcome` advertisement, which lands after the observe bridge
            //    handshakes — i.e. after this function returns. The session is built address-less
            //    and `app_ui` pushes the resolution it already computes, once per frame.
            let md_mount = vike_app_core::data::md_session::build(
                SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
                // ⚠ `PROCESS_ENV`, NOT `vars`. `vars` is `workspace_credentials()` — the VENUE
                // credential store plus the two TRADEHUB hub keys gap-filled from the
                // environment — and handing it here would break this key in BOTH directions:
                // `VIKE_DATAHUB_OBSERVE_KEY` set in the environment (the thin-client container's
                // only way to carry one) would not be found at all, and one filed in the VENUE
                // store would be found SILENTLY, from the wrong store (decision 0051: hub keys live
                // in the `hub_key` table `vike_secrets::CredentialStore::hub_keys` reads).
                process_env(),
                vike_app_core::backend::backend_registry::datahub_observe_key_name(observe_backend),
                books.clone(),
                trades.clone(),
                {
                    let ctx = egui_ctx.clone();
                    move || ctx.request_repaint()
                },
            );
            let direct_bars: Option<std::sync::Arc<data_sink::DirectBarStore>> =
                Some(std::sync::Arc::new(data_sink::DirectBarStore::default()));

            // The whole connection — the push→CoreSnapshot bridge thread (B6's self-healing
            // reconnect loop) AND the opt-in Scope::Write write path — is now built by
            // [`vike_app_core::backend::backend_conn::connect_backend`] from the record `main` RESOLVED.
            // For `--observe ADDR` (and for the local default) that record is the synthetic
            // [`vike_app_core::backend::backend_conn::cli_observe_record`], so the flag behaves
            // byte-identically to pre-B1: observe key from `VIKE_TRADEHUB_OBSERVE_KEY` in the
            // credentials map, control mounted only under `VIKE_TRADEHUB_CONTROL=1` (the master
            // gate, read HERE by the binary) with `VIKE_TRADEHUB_CONTROL_KEY` present. ⚠ a mounted
            // control channel can place/cancel REAL orders on the daemon. This file keeps only the
            // two GUI-owned closures — the arc-swap store (so vike-app-core needs no `arc_swap`
            // dep, the same split `sync_from_core` uses) and the egui repaint wake. The returned
            // `BackendConn` rides the tuple into `App::active_backend` (split-plane B1): held for
            // the app's lifetime unless the Connections picker switches it, its drop/replacement
            // stops-and-joins the bridge thread instead of leaking a dialling loop past the App.
            let active_backend = {
                // ⚠ The RESOLVED record, never a freshly synthesized one. `main` already chose it
                // — the flag and the local default become `cli_observe_record` there, and the
                // registry's active record arrives as itself, carrying the key NAMES the operator
                // configured. Rebuilding a synthetic record here is what made a registry-resolved
                // launch sign with `VIKE_TRADEHUB_OBSERVE_KEY` and fail `bad mac` forever.
                let record = observe_backend.clone();
                let cell = snap_cell.clone();
                let ctx = egui_ctx.clone();
                Some(vike_app_core::backend::backend_conn::connect_backend(
                    &record,
                    &vars,
                    // The master gate: the `flags.tradehub_control` row of THIS PC's database.
                    settings().flags.tradehub_control,
                    // The bridge's poll: the `preferences.observe_poll_ms` row (read in `main.rs`).
                    observe_poll(),
                    link_patience(),
                    feed_status.clone(),
                    move |snap| cell.store(snap),
                    move || ctx.request_repaint(),
                ))
            };

            ObserverMount {
                active_backend,
                snap_cell,
                md_mount,
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
            //     `crates/vike-tradehub/tests/live_lock_claim_order_gate.rs`, whose vike-app root row
            //     was REMOVED with this code (the gate's tombstone says why it was not re-pointed).
            //   • RECONCILIATION — the quarantine-first policy default over the REAL process env
            //     (a helper decision 0111 deleted with that env read), `reconcile_gate` over
            //     `flags.reconcile`/`flags.reconcile_off`
            //     and the armed-live COUNT, `log_reconcile_gate`, the per-venue health map derived
            //     from the feed statuses, the `journal_view_provider` hook, and the
            //     `vike_core::spawn_recon` driver mount. Pinned by
            //     `crates/vike-tradehub/tests/reconcile_gate_wiring_gate.rs`'s vike-app root, likewise
            //     REMOVED rather than left naming absent code. The daemon is the only reconciling
            //     root left.
            //   • the RUN PROFILE and its guards — `vike_core::resolve_profile` +
            //     `RunProfile::apply_guards_and_sinks`, the `GuardsReport` unwired-key warning, and
            //     `vike_exec` risk limits derived from each venue's fetched `SymbolProperties`.
            //   • the LOCAL DATA PLANE — `open_tick_recorder` (the DataFusion+Parquet
            //     `RecorderSink` and its equity sampler), the `CoreSinkAdapter`/`TeeSink`
            //     composition, `vike_data::PropertiesRecorder::open_from_env`, the mmap
            //     health-counters pre-flight (`counters_file_path` +
            //     `vike_core::counters::CountersFile::create`), the opt-in Deribit DVOL recorder
            //     (a `flags.toml` key, since DELETED — `vike_config::DEAD_FLAG_KEYS` →
            //     `spawn_deribit_dvol_feed`), and `build_local_feeds` — the six keyless
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
    /// (`CatalogRefresh::dial_snapshot`, re-pointed every frame by `window_frame`), so the two
    /// datahub clients follow one backend switch together; the Refresh pill reaches the thread
    /// through `ToolData::opt_link`. (The option-chain recorder left this binary for the datahub,
    /// which owns the store.)
    pub(crate) fn spawn_tool_data(
        egui_ctx: &egui::Context,
        catalog: &Arc<vike_app_core::data::catalog_refresh::CatalogRefresh>,
    ) -> Arc<Mutex<tools::ToolData>> {
        let tools = Arc::new(Mutex::new(tools::ToolData::default()));
        let (c, cat) = (egui_ctx.clone(), Arc::clone(catalog));
        tools::spawn_tool_fetchers(
            tools.clone(),
            move || c.request_repaint(),
            move || cat.dial_snapshot(),
        );
        tools
    }

    /// Construction phase 3 — the country-flag textures, decoded from the PNGs bundled in the
    /// binary (`FLAG_PNGS`). No download: the desktop opens no socket for a picture.
    pub fn load_flag_images(egui_ctx: &egui::Context) -> HashMap<String, egui::TextureHandle> {
        let mut flags = HashMap::new();
        for (iso, bytes) in FLAG_PNGS {
            if let Some(tex) = load_png_texture(egui_ctx, &format!("flag_{iso}"), bytes) {
                flags.insert((*iso).to_string(), tex);
            }
        }
        flags
    }

    /// Construction phase 3b — the News providers' icon textures, decoded from the PNGs bundled in
    /// the binary (`NEWS_LOGO_PNGS`), keyed by SOURCE NAME as `NewsItem::source` spells it. No
    /// download; a source with no row (or an undecodable file) simply keeps the coloured-initial
    /// avatar the News list draws.
    pub fn load_news_logos(egui_ctx: &egui::Context) -> HashMap<String, egui::TextureHandle> {
        let mut logos = HashMap::new();
        for (source, bytes) in NEWS_LOGO_PNGS {
            if let Some(tex) = load_png_texture(egui_ctx, &format!("news_{source}"), bytes) {
                logos.insert((*source).to_string(), tex);
            }
        }
        logos
    }

    /// Construction phase 4 — the symbol-search catalog's owner (`CatalogRefresh`) with its disk-cache
    /// read already spawned. Returns it with the receiving half the frame drains the universe from.
    pub(crate) fn mount_catalog(
        egui_ctx: &egui::Context,
    ) -> (Arc<vike_app_core::data::catalog_refresh::CatalogRefresh>, Receiver<vike_catalog::Catalog>)
    {
        // Symbol-search catalog: the DISK CACHE is read off-thread and published over `catalog_rx`
        // (drained in `update`); quick-picks stand in until it lands. A venue the cache has never
        // held is fetched once — nothing else re-fetches, which is what makes the Data Manager's
        // Refresh button the only re-fetch there is. This binary links NO catalog provider: every
        // venue, deribit included, is listed by the backend's datahub (`catalog_wire`), so the
        // startup fetch has nothing local to run and every button is a routed press.
        let (catx, catalog_rx) = std::sync::mpsc::channel();
        let catalog = Arc::new(vike_app_core::data::catalog_refresh::CatalogRefresh::new(
            Vec::new(),
            STATE_DIR.get().and_then(Option::as_ref).map(|d| d.join("catalog.json")),
            // The SHIPPED venue baseline (`docs/decisions/0066` decisions 5-8), under
            // `<project>/bin/catalog/` — the RUNTIME-tool directory, because it ARRIVES as a
            // release asset `scripts/fetch_release_tools.sh` installs rather than sitting in a
            // source tree a deployed binary does not have. `bin_dir_beside` rather than
            // `project_bin_dir_from`: this root has already booted, and the bare walk is
            // `$VIKE_SETTINGS_DIR`-BLIND (`crates/vike-boot/tests/one_owner.rs` is the gate).
            // A `None` here, or a file that is absent, degrades: the venues it covers read
            // exactly as they read today.
            vike_paths::state_path::bin_dir_beside(SETTINGS_DIR.get().and_then(Option::as_deref))
                .map(|d| d.join(vike_catalog::BASELINE_TOOL_DIR).join(vike_catalog::BASELINE_FILE)),
            // …and the OPERATOR's own credentialed lists (`docs/decisions/0066` decision 9),
            // beside `catalog.json` under `state/` rather than under `bin/`: this one is written on
            // this box, by `vike-backend catalog refresh`, rather than arriving with a release.
            // ⚠ READ-ONLY here. That command is its single writer, which is what keeps it safe
            // beside `catalog.json` — this process rewrites THAT file whole from memory on every
            // successful refresh, so a second writer of it would lose rows silently.
            STATE_DIR.get().and_then(Option::as_ref).map(|d| d.join(vike_catalog::LOCAL_FILE)),
            catx,
        ));
        {
            let (c, handle) = (egui_ctx.clone(), Arc::clone(&catalog));
            handle.spawn_initial(move || c.request_repaint());
        }
        (catalog, catalog_rx)
    }

    /// Construction phase 5 — vike's launcher icons: the bundled PNGs decoded into textures.
    pub fn load_launcher_icons(egui_ctx: &egui::Context) -> HashMap<String, egui::TextureHandle> {
        // launcher icons (bundled PNGs rendered from vike's icons.py) → textures
        let mut launcher_icons = HashMap::new();
        for (name, bytes, _) in LAUNCHERS {
            if let Some(tex) = load_png_texture(egui_ctx, name, bytes) {
                launcher_icons.insert(name.to_string(), tex);
            }
        }
        launcher_icons
    }

    /// Construction phase 6 — the GPU candle pipeline. Returns `gpu_ok`: `true` once the pipeline is
    /// registered in the wgpu `callback_resources`, `false` when the backend is absent or the build
    /// failed (the chart then degrades to the egui candle painter).
    pub fn build_candle_pipeline(render_state: Option<&eframe::egui_wgpu::RenderState>) -> bool {
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
    /// Gamma resolve, then `next_win_n`.
    fn apply_startup_layout(app: &mut App, egui_ctx: &egui::Context, area: egui::Rect) {
        // ── startup layout ──────────────────────────────────────────────────────────────────────
        // The whole decision is [`vike_app_core::ui::startup`]'s; its input is built by `main.rs`'s
        // `startup_env` (the env knobs out of `App::env`, plus the `preferences.chart_style` row).
        let startup_env = startup_env(&app.env);
        let restored = startup::restored_workspace(startup_env.shot_mode);
        let layout = startup::plan(area, &startup_env, restored);
        // Applied in the ORDER `StartupLayout` documents, which is the exact order this function
        // performed these steps before the move: globals, then the feeds (both arms that ensure
        // anything did so BEFORE publishing their windows), then the restore log, then the window
        // list, then `next_win_n`. A `fn new`'s startup order is load-bearing, so it is reproduced,
        // not tidied.
        if let Some(d) = layout.display {
            app.display_tz = d.display_tz;
            app.gpu_render = d.gpu_render;
            app.indicator_favs = d.indicator_favs;
        }
        for f in &layout.ensure_feeds {
            app.ensure_feed_on(egui_ctx, &f.venue, &f.symbol, &f.interval, f.asset_class);
        }
        if let Some(n) = layout.restored_windows {
            tracing::info!("workspace restored ({n} windows)");
        }
        app.wins = layout.wins;
        if let Some((wid, dest)) = layout.data_dest {
            // Pre-seed the window's `ToolView` so the Data Manager opens on the requested rail
            // destination. The window loop `entry(..).or_default()`s this map, so writing it here is
            // simply what it will find — no ordering subtlety beyond being before the first frame.
            app.tool_views.entry(wid).or_default().data_dest = dest;
        }
        if let Some((wid, account)) = layout.gateway_account {
            // Same pre-seed as the sub-tab above, for the same reason: the window loop
            // `entry(..).or_default()`s this map, so writing it here is simply what it will find.
            // ⚠ It is a PENDING value, not a setting — `data_body`'s `DataDest::Gateway` arm
            // `take`s it on the first frame it renders, so the operator's own chip clicks are
            // never re-overridden.
            app.tool_views.entry(wid).or_default().gateway_account = Some(account);
        }
        app.next_win_n = app.wins.len() as u32;
    }
}

/// What `App::mount_observer` builds: the core-derived `App` bindings, bundled so the observer arm
/// can leave `App::new` without a nine-element tuple. Every field lands in `new`'s `Self { .. }`
/// literal under the same name (`md_mount` through its `Self` fields).
pub(crate) struct ObserverMount {
    pub(crate) active_backend: Option<vike_app_core::backend::backend_conn::BackendConn>,
    pub(crate) snap_cell: Arc<arc_swap::ArcSwap<vike_exec::CoreSnapshot>>,
    pub(crate) md_mount: vike_app_core::data::md_session::MdMount,
    pub(crate) feed_status: Arc<std::sync::Mutex<String>>,
    pub(crate) books: std::sync::Arc<data_sink::BookStore>,
    pub(crate) trades: std::sync::Arc<data_sink::TradeStore>,
    pub(crate) direct_bars: Option<std::sync::Arc<data_sink::DirectBarStore>>,
}

fn load_png_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
    Some(ctx.load_texture(format!("icon_{name}"), ci, egui::TextureOptions::LINEAR))
}

/// The country flags the Calendar's currency column can show, by lowercase iso2: every code
/// `vike_app_core::tools::currency_country` can yield plus de/fr/it/es. 40 px wide PNGs bundled
/// from `assets/flags/` (source and date: its README), so the desktop downloads nothing for them.
const FLAG_PNGS: &[(&str, &[u8])] = &[
    ("us", include_bytes!("../assets/flags/us.png")),
    ("eu", include_bytes!("../assets/flags/eu.png")),
    ("gb", include_bytes!("../assets/flags/gb.png")),
    ("jp", include_bytes!("../assets/flags/jp.png")),
    ("au", include_bytes!("../assets/flags/au.png")),
    ("nz", include_bytes!("../assets/flags/nz.png")),
    ("ca", include_bytes!("../assets/flags/ca.png")),
    ("ch", include_bytes!("../assets/flags/ch.png")),
    ("cn", include_bytes!("../assets/flags/cn.png")),
    ("in", include_bytes!("../assets/flags/in.png")),
    ("br", include_bytes!("../assets/flags/br.png")),
    ("za", include_bytes!("../assets/flags/za.png")),
    ("kr", include_bytes!("../assets/flags/kr.png")),
    ("mx", include_bytes!("../assets/flags/mx.png")),
    ("ru", include_bytes!("../assets/flags/ru.png")),
    ("tr", include_bytes!("../assets/flags/tr.png")),
    ("id", include_bytes!("../assets/flags/id.png")),
    ("sa", include_bytes!("../assets/flags/sa.png")),
    ("sg", include_bytes!("../assets/flags/sg.png")),
    ("hk", include_bytes!("../assets/flags/hk.png")),
    ("se", include_bytes!("../assets/flags/se.png")),
    ("no", include_bytes!("../assets/flags/no.png")),
    ("de", include_bytes!("../assets/flags/de.png")),
    ("fr", include_bytes!("../assets/flags/fr.png")),
    ("it", include_bytes!("../assets/flags/it.png")),
    ("es", include_bytes!("../assets/flags/es.png")),
];

/// The News providers' icons, by SOURCE NAME exactly as the datahub's `FEEDS` table spells it
/// (`crates/vike-datahub/src/tool_feeds/news.rs`). 64 px PNGs bundled from `assets/news-logos/`
/// (source, date and the licence caveat: its README), so the desktop downloads nothing for them.
/// ⚠ `crates/vike-datahub/tests/news_logo_assets.rs` reads THIS table as text: one row per
/// feed, written `("<name>", include_bytes!("../assets/news-logos/<slug>.png")),`.
const NEWS_LOGO_PNGS: &[(&str, &[u8])] = &[
    ("CoinDesk", include_bytes!("../assets/news-logos/coindesk.png")),
    ("Cointelegraph", include_bytes!("../assets/news-logos/cointelegraph.png")),
    ("Decrypt", include_bytes!("../assets/news-logos/decrypt.png")),
    ("CryptoSlate", include_bytes!("../assets/news-logos/cryptoslate.png")),
    ("BeInCrypto", include_bytes!("../assets/news-logos/beincrypto.png")),
    ("Bitcoin Magazine", include_bytes!("../assets/news-logos/bitcoin-magazine.png")),
    ("NewsBTC", include_bytes!("../assets/news-logos/newsbtc.png")),
    ("CoinJournal", include_bytes!("../assets/news-logos/coinjournal.png")),
    ("ActionForex", include_bytes!("../assets/news-logos/actionforex.png")),
    ("ForexLive", include_bytes!("../assets/news-logos/forexlive.png")),
    ("FXEmpire", include_bytes!("../assets/news-logos/fxempire.png")),
    ("Investing.com", include_bytes!("../assets/news-logos/investing-com.png")),
    ("Investing.com FX", include_bytes!("../assets/news-logos/investing-com-fx.png")),
    ("MarketWatch", include_bytes!("../assets/news-logos/marketwatch.png")),
    ("CNBC", include_bytes!("../assets/news-logos/cnbc.png")),
    ("Seeking Alpha", include_bytes!("../assets/news-logos/seeking-alpha.png")),
];

// ⚠ TOMBSTONE — `spawn_catalog_fetcher` stood here. It fetched `DeribitCatalog` on EVERY start,
// sent the result once and cached nothing, so the `vike_catalog::persist` cache — whose own doc has
// said "Loaded instantly at startup; rewritten only on an explicit user refresh" since it was
// written — was referenced by nothing in the tree. `vike_app_core::data::catalog_refresh::CatalogRefresh`
// replaces it: same channel, plus the cache, the per-venue stamp and the button (and, since the
// options move to the datahub, no local provider at all — deribit is a routed venue like the rest).
// It lives one crate down because `vike-desktop` is outside the derived CI roster, so a refresh
// spelled here would be compiled by `app-check` and tested by nothing.

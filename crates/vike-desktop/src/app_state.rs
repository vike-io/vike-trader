//! `App`, the shell's whole state, and `BAR_PLANE`, the render-source mode signal its folds read.

use super::*;

/// **THE RENDER-SOURCE MODE SIGNAL for this binary** — what fills `App::direct_bars`, bound once
/// from the arm table rather than inferred at each fold from `direct_bars.is_some()`.
///
/// `AppMode::ObserveOnly` is this binary's only arm (rulings 1 and 2: no local core, no venue
/// sockets), and under `BarPlane::BackendStore` a kline series renders from the store exactly
/// where the backend's live snapshot does not publish it. `core_sync` `debug_assert!`s this
/// against the handle, so the two cannot drift.
pub(crate) const BAR_PLANE: vike_app_core::backend::split_plane::BarPlane =
    vike_app_core::backend::split_plane::bar_plane(
        vike_app_core::backend::split_plane::AppMode::ObserveOnly,
    );

pub(crate) struct App {
    pub charts: HashMap<String, vike_chart::model::ChartState>, // render model, keyed "SYMBOL@interval"
    // Global chart/clock display timezone (task A6). Applied to every `ChartState` in `charts`
    // (self-healing in `sync_from_core`, immediate on a menu change) and to the status-bar clocks.
    // Persisted in workspace.json; default `Local` == pre-feature behavior (zero visual change).
    pub display_tz: vike_chart::DisplayTz,
    pub spawned: HashSet<String>,
    /// Series keys whose venue has no registered feed in `feeds` right now — the observable record
    /// behind [`feed_lifecycle::FeedSlots::unroutable`] (a chart that will paint nothing until a
    /// client for its venue exists). Maintained entirely by the moved feed-lifecycle functions:
    /// inserted on a missing-venue `ensure_*`, cleared when the venue turns up, and swept by
    /// `reap_orphaned_feeds` alongside every other slot. Empty on the normal path.
    pub unroutable: HashSet<String>,
    /// Every subscribe leg a venue ANSWERED NO to and that has not since succeeded — the sibling
    /// record of `unroutable` for the other half of "wanted, but not actually subscribed", behind
    /// [`feed_lifecycle::FeedRetries`]. Covers every `ensure_*` site a venue can refuse (the
    /// `feed_lifecycle::RetryLane` variants are the list), each of which used to claim its
    /// idempotency slot on the FAILURE path too and so lost that stream permanently.
    /// Maintained entirely by the feed-lifecycle functions: created on failure, cleared by the
    /// retry that succeeds, and its series lane swept by `reap_orphaned_feeds`. Empty on the
    /// normal path; a non-empty entry is a chart/ladder/tape delivering nothing right now.
    pub feed_retries: feed_lifecycle::FeedRetries,
    pub hidden: HashSet<String>, // Data-manager-deleted keys: feed runs, GUI stops syncing
    /// The ACTIVE remote backend (split-plane B1): the record it was dialed from, the observe
    /// bridge (B6's stop handle — its `Drop` stops-and-joins the reconnect thread), and the
    /// optional Scope::Write WRITE channel (⚠ when `Some`, the GUI's order buttons place/cancel
    /// REAL orders on the remote daemon — see `App::remote_ctrl()` for the read sites). `Some`
    /// ALWAYS `Some` now — at startup from the resolved record (the `--observe` flag, else the
    /// registry's active record, else the local default), or replaced after the Connections tool's
    /// picker connected a different registry backend. It used to be `None` on the local-core path;
    /// there is no such path any more, and this connection is the ONLY route an order out of this
    /// process has. Replaced WHOLESALE by `vike_app_core::backend::backend_conn::switch_backend` (the
    /// backend-session-clear switch routine); taken + dropped in `on_exit`.
    pub active_backend: Option<vike_app_core::backend::backend_conn::BackendConn>,
    /// The loaded `backends.json` registry (split-plane B7/B8) the Connections tool's picker
    /// lists. Loaded once at startup; mutated only by the drained Backends-editor update
    /// (split-plane I2 — see `backend_editor` below), which assigns AND saves in one drain so
    /// disk and memory never disagree.
    pub backends: vike_app_core::backend::backend_registry::BackendsFile,
    /// The Connections tool's Backends EDITOR state machine (split-plane I2): the add/edit/
    /// delete form. Every decision lives in `vike_app_core::backend::backend_editor`; this binary only
    /// threads it into the tool body (`mem::take`/write-back, the `studio` dance) and applies
    /// the drained `RegistryUpdate` after the frame.
    pub backend_editor: vike_app_core::backend::backend_editor::EditorState,
    /// The Connections tool's Backend-settings section state (split-plane REQ-7, read half):
    /// `(backend addr, state)`, keyed by ADDR so a backend switch renders fresh (`Idle` ⇒
    /// auto-refetch) instead of the previous backend's rows. Written by the short-lived
    /// `spawn_backend_settings_fetch` thread, read each frame the Connections tool is visible.
    pub backend_settings: Arc<Mutex<(String, vike_app_core::ui::tool_views::BackendSettingsState)>>,
    /// The section's EDIT flow (REQ-7 write half): UI-thread state (its text buffers are typed
    /// into every frame), taken into a frame-local for the window loop and written back after —
    /// the pure machine lives in `vike_app_core::ui::tool_views::backend_settings`.
    pub backend_settings_edit: vike_app_core::ui::tool_views::SettingsEditState,
    /// One finished settings WRITE, already folded to its flow state by the worker
    /// (`settings_write_state`), keyed by backend ADDR (the fetch slot's latest-wins
    /// discipline). Written by the short-lived `spawn_backend_settings_write` thread, drained at
    /// the top of each frame.
    pub backend_settings_write_result:
        Arc<Mutex<Option<(String, vike_app_core::ui::tool_views::SettingsEditState)>>>,
    pub snap_cell: Arc<arc_swap::ArcSwap<vike_exec::CoreSnapshot>>,
    pub last_seq: u64, // last CoreSnapshot.seq folded into `charts`
    // WHAT THE BACKEND PUBLISHES, rewritten by `sync_from_core` whenever the snapshot advances.
    // Read by three surfaces, all in `vike_app_core::ui::series_follow`: the adoption planner, the
    // symbol picker's backend section, and the title-bar badge.
    pub published_series: Vec<vike_app_core::ui::series_follow::PublishedSeries>,
    // venue-keyed live DataClient map ("binance", "bybit", "okx", "polymarket", …): one
    // `DatahubFeed` per venue the connected datahub ADVERTISES, EMPTY until its handshake lands, and
    // added/dropped by `MdSession::sync_mount` once per frame — no list in this crate says which
    // venues belong in it. `+ Send` so the whole set can be moved onto the throwaway
    // `vt-app-shutdown` thread and each venue's teardown fanned out to run in parallel (see
    // `on_exit`); every `DatahubFeed` is `Send`, the bound just makes that usable off-thread.
    pub feeds: HashMap<&'static str, Box<dyn vike_data::DataClient + Send>>,
    // The status bar's headline handle: binance's concrete `Feeds` status on the local-core
    // path; the BACKEND bridge's connection-status handle in both observer arms (the per-venue
    // feed lines live in `feed_statuses` below there — third mode included).
    pub feed_status: Arc<std::sync::Mutex<String>>,
    // Per-venue live feed-status handles for the Connections tool's Status column: the same
    // human-readable `Arc<Mutex<String>>` the market-data session writes, keyed by the
    // `vike_model::VENUES` slug — one per entry of `feeds`, added and dropped beside it by
    // `MdSession::sync_mount`. A superset of `feed_status` above (which is the observe bridge's
    // headline handle). A venue the datahub does not advertise has no line and renders `Unknown`.
    pub feed_statuses: HashMap<String, Arc<std::sync::Mutex<String>>>,
    // THE MARKET-DATA SESSION (`vike_app_core::data::md_session`): one connection to the BACKEND's
    // datahub, feeding every `feeds` entry above and writing every `feed_statuses` line. Held here
    // for the per-frame calls only this shell can make — `set_addr` (the address is resolved from
    // `settings()` plus the ACTIVE backend's advertisement, neither of which the session can see),
    // `sync_mount` (which fills the two maps above) and `tape_gaps` (whose consumer,
    // `sync_from_core`, takes it as an input) — and for teardown: with no feed mounted yet there is
    // no `DataClient::shutdown` to stop the session, so `run_bounded_teardown` stops it directly.
    pub md_session: std::sync::Arc<vike_app_core::data::md_session::MdSession>,
    pub subs: HashMap<String, vike_data::SubscriptionId>, // "{symbol}@{interval}" -> live subscription
    pub books: std::sync::Arc<data_sink::BookStore>,      // live L2 books, per (venue, symbol)
    // live trade tape for GUI-side tick/volume aggregation (per (venue,symbol)); drained by
    // `sync_from_core`'s per-frame fold (Task B5) into every `aggs` entry sharing that symbol.
    pub trades: std::sync::Arc<data_sink::TradeStore>,
    // The GUI-side bar store. ⚠ `Some` again since 2026-09-15, filled by a DIFFERENT producer: it
    // was `Some` in the THIRD MODE (fat + `--observe`) where `GuiFeedSink`'s bar lanes filled it
    // from LOCAL VENUE FEEDS, went `None` when ruling 2 removed those feeds, and now holds bars
    // read out of the BACKEND'S OWN hist store by `vike_app_core::data::store_bars` — the only way a
    // chart on an interval the daemon does not stream can paint at all. WHAT fills it is
    // `split_plane::BarPlane`, an explicit value, no longer this field's `is_some()`.
    pub direct_bars: Option<std::sync::Arc<data_sink::DirectBarStore>>,
    // Chart keys ALREADY asked of the backend's store this session (`store_bars::plan_store_reads`'
    // `asked`). Inserted BEFORE the read is spawned, which is what makes "once" hold across the
    // flight rather than only across the return — an empty chart stays empty while the read is in
    // the air, so without it every frame would re-ask. Cleared on a backend switch beside the
    // store itself (`apply_backend_action`): another backend is another store.
    pub store_asked: HashSet<String>,
    // The one sentence the CHART-SEED gap arm leaves for the operator
    // (`vike_app_core::data::chart_seed::render_chart_seed_status`), written by the background read
    // thread and painted under the empty-plot hint. `None` = nothing worth saying. It is a shared
    // slot rather than a channel because there is exactly one current answer, not a log: a stale
    // "set VIKE_DATAHUB_CHART_SEED=1" after the operator has set it would be worse than silence,
    // and `SeedDial::run` overwrites on every batch for that reason. Cleared with `store_asked` on
    // a backend switch — another backend is another server with its own configuration.
    pub chart_seed_note: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    // Last `DirectBarStore::generation` folded into `charts` — `last_seq`'s store-side twin, a
    // pure monotonic fold cursor (never reset; see `CoreSyncState::last_direct_gen`).
    pub last_direct_gen: u64,
    // Client-side tick/volume aggregators (Task B5), keyed by chart key "SYM@100t"/"SYM@10v" (the
    // same keyspace as `charts`) -> (venue, symbol, aggregator). `ensure_feed_on`'s Tick/Volume
    // branch creates an entry (once) per chart key; `sync_from_core` feeds it from the drained
    // trade tape (venue-aware, Task: venue-aware tick/vol) and syncs the matching `charts` entry
    // from its `closed`/`forming` output — the tick/volume twin of the venue-kline fold.
    pub aggs: HashMap<String, (String, String, tickvol::TickVolAgg)>,
    // SP2 orderflow (Task 7): per-bar footprint aggregators, keyed by chart key (the venue-aware
    // `workspace::series_key` shape — `"SYM@interval"` for Binance, `"venue:SYM@interval"`
    // otherwise), the same keyspace as `charts`. The value carries `(venue, symbol, aggregator)`
    // — the venue+symbol are stored explicitly (not parsed back out of the key) so the drain in
    // `sync_from_core` groups by `(venue, symbol)` exactly like `aggs`, making orderflow work for
    // ANY venue whose feed implements `subscribe_trades` (OKX/Bybit/…), not just Binance. An entry
    // is created (once) when a window's `WinState::orderflow_on()` goes true (the window loop
    // collects the request; `of_wanted` processing after the loop registers it) and fed every
    // frame from the SAME drained trade tape `aggs` uses (`sync_from_core`). Absent entry ⇒
    // `chart::ChartInputs::footprint = None` (default-off — no window ever reads this until it
    // asks for orderflow).
    pub of_aggs: HashMap<String, (String, String, bar_agg::OrderflowAgg)>,
    // ⚠ TOMBSTONE — the aggTrades BACKFILL's slots stood here: the backfill-hours knob, the
    // run-once spawn gate and its retry lane. The worker they guarded went with the local
    // market-data plane (rulings 1 and 2), its stub and these slots on 2026-10-10;
    // `crates/vike-desktop/tests/app_local_core_gate/dead_leftovers.rs` pins the names. Orderflow
    // is live-only: CVD/profile start when orderflow is toggled on.
    /// `(venue, venue-native symbol)` with a depth stream running — the Trade windows' dedupe
    /// LEDGER, each entry keeping the subscription ids `ensure_depth` minted
    /// (`feed_lifecycle::TradeDepthSubs`) so the per-frame reaper stops the streams when no Trade
    /// window names that book any more.
    pub trade_depth: HashMap<(String, String), feed_lifecycle::TradeDepthSubs>,
    /// Polymarket token-ids with a live cockpit book+trade stream running — the cockpit's analog
    /// of `trade_depth`, same ledger shape (`feed_lifecycle::PolyBookSubs`), reaped by the same
    /// per-frame call. Subscribed via `ensure_poly_book`.
    pub poly_subs: HashMap<String, feed_lifecycle::PolyBookSubs>,
    /// Unified shutdown signal (app-owned) — the single "we are tearing down" flag. Raised once,
    /// either when the window-close is first observed in `ui` or at the top of `on_exit`, and never
    /// cleared. INERT on the running path: it is only ever READ during teardown — `ensure_feed_on`
    /// skips starting a NEW live feed once it is set, so no fresh blocking socket read is opened
    /// while `on_exit`'s bounded teardown runs. Never read in the vike-core hot fold (the p99<10µs
    /// gate is untouched). See `on_exit` for the bounded, parallel teardown it gates.
    pub shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    // ⚠ TOMBSTONE — five slots of the deleted local core stood here, each only ever `None` or
    // empty: `recorder` (the tick recorder), `live_venues` (venues with a LIVE exec client),
    // `forwarder_stop` (the live-event forwarder's drain-stop), `_live_locks` (the B11
    // `vike_boot::live_lock` guards) and `materializer` (the journal materializer). Deleted in
    // 2026-10. ⚠ One ARGUMENT outlives them: the live-account lock set was
    // `vike_tradehub_core::armed_live_venues`, claimed one statement ABOVE `assemble_runtime` and deliberately
    // NOT the mount's own arming record, because three venues post `set_leverage` while their exec
    // client is constructed — a lock refused from that later set is refused after this process has
    // already changed account state. Anything that brings a mount back brings that ordering back.
    // The B6 `_observe_bridge` stop handle folded into `active_backend` above (split-plane B1):
    // the bridge now lives inside `BackendConn`, whose replacement/drop stops-and-joins it.
    /// QA ([`TRADE_SEED_ENV`]): submit the two PAPER orders
    /// [`vike_app_core::ui::capture_seed::plan_trade_seed`] mints — one resting limit, one market —
    /// so an Account-window capture has a working order and an open position instead of empty
    /// tables. Cleared on the frame that submits them, so it is a ONE-SHOT; a frame that could not
    /// act (no dispatch, no price yet) leaves it armed and re-tries.
    pub trade_seed_pending: bool,
    /// The instrument of the Trade window the trader last acted in (spec §3.11): a new Trade
    /// window opens on it. `None` until one acted; the window loop's `TradeFrame::remember` writes
    /// it, never from every window every frame (minor 26).
    pub last_trade: Option<tool_views::TradePick>,
    /// The ACTIVE backend's `Directory` reply (venue names, the account list), kept by the
    /// throwaway fetch thread `spawn_directory_fetch` starts — `tool_views::DirectorySlot`.
    pub directory: Arc<Mutex<tool_views::DirectorySlot>>,
    /// QA ([`CHART_DRAW_ENV`]): write the two capture drawings
    /// [`vike_app_core::ui::capture_seed::trendline_overlay`] derives into every chart's
    /// `ChartState::overlays`, once the series has bars. Same one-shot/re-try shape as
    /// [`Self::trade_seed_pending`].
    pub chart_draw_pending: bool,
    pub wins: Vec<workspace::WinState>,
    pub tool_views: HashMap<egui::Id, tools::ToolView>, // per-tool-window view state, keyed by WinState.id
    /// Chart sync groups (task B8): double-buffered per-frame registry, one [`GroupFrame`]
    /// per group (1..=4). `sync_prev` is what THIS frame's grouped chart windows read (via
    /// [`sync_feed`]) to build their `vike_chart::ChartInputs::sync`; `sync_next` is written
    /// while walking `wins` this frame (via [`sync_harvest`], after each window's
    /// `chart::draw`) and becomes next frame's `sync_prev` via the top-of-`ui` swap.
    pub sync_prev: HashMap<u8, GroupFrame>,
    pub sync_next: HashMap<u8, GroupFrame>,
    /// Sticky range-leader per sync group (task B8): the last window id that had a live
    /// local interaction (drag/zoom/nav) in that group. Only the leader's `visible_ts`
    /// propagates into `sync_next` — see [`sync_harvest`]'s doc for why that's what makes
    /// followers track a leader's live edge instead of freezing.
    pub range_leader: HashMap<u8, u64>,
    pub status: String,
    pub next_win_n: u32,
    pub show_rail: bool,
    pub desktop: egui::Rect,
    pub did_initial_arrange: bool,
    pub last_arrange: Option<workspace::Arrange>, // last tiling mode, re-applied on app maximize
    pub was_maximized: bool,                      // edge-detect the maximize/restore transition
    pub retile_frames: u8, // re-tile for a few frames while the desktop settles
    pub tools: std::sync::Arc<std::sync::Mutex<tools::ToolData>>,
    pub datasets: datasets::Store,
    pub palette: String,
    pub shot_n: u32, // frame counter for the headless self-screenshot (VIKE_SHOT)
    // File -> Export chart image…: set when the user picks the menu action; the next frame
    // requests egui's framebuffer screenshot and, once the readback event arrives, writes a
    // PNG (same capture path as VIKE_SHOT). `export_seq` disambiguates same-second filenames.
    pub export_pending: bool,
    pub export_seq: u32,
    // File -> Save layout as… (TradingView-style named layouts): when `layout_name_prompt` is
    // set the app shows a small modal name-entry field; `layout_name_input` holds the in-progress
    // name. On confirm, `workspace::persist::save_layout` writes `layouts/<sanitized>.json`. Load
    // and Delete are driven directly from the menu submenus (no dialog needed) — see the menu
    // action arms below.
    pub layout_name_prompt: bool,
    pub layout_name_input: String,
    pub layout_prompt_focus: bool, // one-shot: focus the name field the frame the dialog opens
    pub flags: HashMap<String, egui::TextureHandle>, // country flags, decoded from the bundled PNGs
    // Provider icons keyed by source name, decoded from the PNGs bundled in the binary
    // (`App::load_news_logos`); a source without one draws its coloured-initial avatar.
    pub news_logos: HashMap<String, egui::TextureHandle>,
    pub launcher_icons: HashMap<String, egui::TextureHandle>, // vike's vector launcher icons (PNG)
    // Searchable CROSS-VENUE instrument catalog (`vike_catalog::Catalog` over the live Binance +
    // Bybit + OKX keyless public instrument universes, each `Instrument` venue-tagged and
    // asset-class-tagged), fetched ONCE by a background thread at startup and delivered over
    // `catalog_rx`. Held as an `Arc` so the per-window title-bar loop clones it cheaply each frame
    // (never a deep copy). Empty (`Catalog::from_instruments(Vec::new())`) until the fetch lands —
    // the picker falls back to the built-in `SYMS` quick-picks meanwhile; a per-venue fetch failure
    // just contributes nothing (the other venues still populate). Same channel+Arc idiom as the
    // flag/logo rx. The picker ranks/filters via `Catalog::search(query, &SearchFilter, limit)`.
    pub symbols_catalog: Arc<vike_catalog::Catalog>,
    pub catalog_rx: Receiver<vike_catalog::Catalog>,
    /// **Who OWNS that catalog** — the on-disk cache, the per-venue refresh stamps, and the
    /// explicit per-venue refresh the Data Manager's Instruments destination triggers
    /// (`vike_app_core::data::catalog_refresh`). It holds the SENDER half of `catalog_rx`, so a refresh
    /// lands in `symbols_catalog` through the drain that already existed.
    ///
    /// ⚠ It REPLACES `spawn_catalog_fetcher`, which fetched every start and cached nothing — the
    /// policy `crates/vike-catalog/src/persist.rs` has documented since the cache was written
    /// ("rewritten only on an explicit user refresh") had no implementation until this field.
    pub catalog: Arc<vike_app_core::data::catalog_refresh::CatalogRefresh>,
    /// GPU candle layer availability (GPU Phase 2). `true` once `chart_gpu::CandlePipeline` is
    /// registered in the wgpu `callback_resources` at startup; `false` if the wgpu backend is
    /// absent or the pipeline build failed (degrade to the egui candle painter). The Task 3
    /// render toggle (`gpu_render`) is ANDed with this — a user can only turn GPU rendering ON
    /// when it's actually available.
    pub gpu_ok: bool,
    /// GPU candle layer user toggle (GPU Phase 2, Task 3). Default `false` == pre-Task-3
    /// behavior (the egui/LOD candle painter, byte-identical). Persisted in workspace.json
    /// alongside `display_tz` (GLOBAL, not per-window: one knob, not per-chart). The
    /// `chart::draw` call site only
    /// wires `ChartInputs::gpu_candles` when BOTH this and `gpu_ok` are true.
    pub gpu_render: bool,
    /// Indicator favourites (feature part b): the user's ⭐-starred indicator registry keys, GLOBAL
    /// (shared by every chart's ƒx picker — not per-window) and persisted in the workspace file
    /// alongside `display_tz`/`gpu_render` (see `workspace::persist::Workspace::indicator_favs`).
    /// EMPTY (the default) means no Favourites section in the picker — byte-identical to today. The
    /// picker stars/unstars into this `Vec` directly (order = star order = display order).
    pub indicator_favs: Vec<String>,
    /// The appearance this process runs and the Settings window's pending change —
    /// `vike_app_core::ui::appearance_settings::AppearanceSession`, which applies a change once the
    /// frame's windows have drawn and saves it as a row of THIS computer's settings database.
    pub appearance: AppearanceSession,
    /// The frame-rate cap — present exactly when `preferences.vsync` is OFF (with vsync on, the
    /// swap chain blocks on the display and that is the cap): `App::ui` throttles through it at the
    /// top of every frame. `vike_app_core::ui::frame_cap` says why and how.
    pub frame_cap: Option<vike_app_core::ui::frame_cap::FrameCap>,
    /// The Studio tool's singleton editor/backtest session (SP2 `vike-studio` mount, Task 7) — ONE
    /// long-lived instance, not the per-window `tool_views` map: Studio is a single editor session
    /// (like a code-editor tab), not per-window state, even if somehow more than one Studio window
    /// existed. `None` until the FIRST Studio-window render (lazy: most sessions never open it, so
    /// paying `DataFusionHist::open`'s cost — a tokio runtime spin-up + WAL recovery — at every
    /// startup would be wasted work for everyone else). See `studio_store_root`/`open_studio_store`.
    ///
    /// UNCONDITIONAL since split-plane I7: a default `vike-studio` build is DataFusion-free (the
    /// `studio-standalone` suite is the gate), so this client links the Studio and drives it
    /// REMOTE-ONLY via `open_studio_store` — which now has no other arm at all.
    pub studio: Option<vike_studio::StudioState>,
    /// Sticky failure reason from the one-shot `open_studio_store` attempt. `None` means either
    /// "not tried yet" or "already succeeded" (see `studio`); set at most once, so a broken store
    /// (e.g. permission denied) degrades to a static label rather than retrying the (expensive)
    /// open every single frame the Studio window stays visible.
    pub studio_error: Option<String>,
    /// Data Manager "Stored" view: the local `HistStore`'s manifest-derived inventory tree
    /// (venue → symbol → series rollups), fetched off-thread — the first time the Stored tab is
    /// shown, and again on every Refresh click (see `refresh_stored`). Manual-refresh only, no
    /// per-frame polling. `Arc` so the window loop can read it without holding `self` mutably
    /// (same idiom as `symbols_catalog`); empty until the first load lands.
    pub stored_tree: Arc<Vec<vike_data_manager::model::VenueNode>>,
    /// Per-series missing-data gap ranges (`DataFusionHist::series_gaps`, inclusive epoch-ms),
    /// keyed by the same [`vike_data_manager::SeriesKey`] identity as `stored_tree`'s rows —
    /// fetched in the same off-thread `refresh_stored` load (see that fn's doc), never per-frame.
    /// Feeds `stored_catalog_grid`'s coverage-bar gap paint + its `HasGaps` smart-view filter.
    /// `Arc` for the same read-only-in-the-window-loop reason as `stored_tree`. Series with no
    /// gaps are omitted (kept small); an absent key reads as "no known gaps".
    pub stored_gaps: Arc<vike_data_manager::GapMap>,
    /// Per-INSTRUMENT cross-kind partial days (`HistStore::coverage_report` folded through
    /// [`vike_data_manager::partial_days_from_coverage`]), loaded on the same off-thread pass as
    /// `stored_gaps`. The cross-kind sibling of that map: it feeds the grid's Partial column, which
    /// answers a question no per-series view can — "are some of this instrument's kinds missing days
    /// the others have". Instruments with nothing partial are omitted.
    pub stored_partials: Arc<vike_data_manager::PartialDayMap>,
    /// What the LAST `refresh_stored` load learned about the remote store's coverage verb (spec
    /// §6-Q2). It is a NEGOTIATED fact — only a dial can produce it — so it is remembered here and
    /// fed back into `stored_mode` on every render, which is what lets the Partial column show the
    /// wire answer against a current server and the honest note against an older one. Starts
    /// `Unknown` (nothing dialled yet); the local arm never changes it, and `stored_mode` ignores
    /// it in local mode.
    pub stored_coverage: vike_app_core::data::stored_mode::RemoteCoverage,
    /// What the LAST load got from the datahub's history-channels read (decision 0102) — the Data
    /// Manager's HISTORY column and the bulk Backfill's lookback floors. `None` until a load answers
    /// it. Read straight off `App` by the window loop, so no frame snapshot copies it.
    pub stored_history: Option<Arc<vike_app_core::data::history_column::HistoryLoad>>,
    /// `Some(reason)` → the LAST load could not READ the store, and this is what to show instead of
    /// a bare empty grid. `None` → the tree is the store's real answer, INCLUDING when that answer
    /// is "nothing recorded".
    ///
    /// ⚠ The two used to be the same picture, and that was half the defect: an unreachable datahub
    /// and a genuinely empty store both drew an empty grid, so the screen could not tell an
    /// operator which one they were looking at. Cleared when a new load starts (see
    /// `refresh_stored`), so a retry never shows the previous attempt's reason under a fresh
    /// spinner.
    pub stored_error: Option<String>,
    pub stored_rx: Receiver<vike_app_core::data::stored_load::StoredLoadOutcome>,
    pub stored_tx: std::sync::mpsc::Sender<vike_app_core::data::stored_load::StoredLoadOutcome>,
    /// `true` while a background `refresh_stored` load is in flight — drives the Stored tab's
    /// "Loading…" label and disables a second concurrent Refresh click.
    pub stored_loading: bool,
    /// dm-bulk-backfill: `true` while a background `maybe_spawn_stored_backfill` run is in
    /// flight — the run-once-at-a-time gate (a second bulk click while one is running is a no-op,
    /// see that method's doc), same shape as `stored_loading`.
    pub stored_backfill_running: bool,
    /// dm-bulk-backfill: the last backfill run's human-readable status line ("Backfilling 3
    /// series (1 skipped)…" while running, then a final "N backfilled, M failed, K skipped"),
    /// rendered in `tool_views::stored_tool_content` next to the Refresh/Delete row.
    pub stored_backfill_status: String,
    pub stored_backfill_rx: Receiver<String>,
    pub stored_backfill_tx: std::sync::mpsc::Sender<String>,
    /// The process environment, read ONCE by `main` ([`GuiEnv`]) and consulted from here: the
    /// frame loop's capture and first-frame arrangement blocks, the frame-rate instrument, a
    /// launcher opening a cockpit, and the Studio window's QA hooks. Never written after `new`.
    pub env: GuiEnv,
}

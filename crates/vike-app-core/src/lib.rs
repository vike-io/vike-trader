//! `vike-app-core` — the eframe-free / wgpu-free logic extracted from `vike-app` so it runs in CI.
//!
//! `vike-app` is the wgpu GUI shell — CI compile-checks it (ci.yml's `app-check`) but never TESTS or
//! clippy-gates it (build weight); the modules here touch NO eframe/wgpu (egui at most — the
//! [`workspace`]/[`tool_views`] draw modules), so they build AND test on CI exactly like `vike-chart`
//! does. `vike-app` re-imports them and wires them into its frame loop.
//!
//! - [`alerting`] — re-export of [`vike_ops::alerting`]: the alerting engine (persisted rule model,
//!   PURE evaluator, Telegram/webhook delivery seam). MOVED to `vike-ops` — it is an OFF-FOLD
//!   consumer of the published `CoreSnapshot`/event stream with no egui in it, and the headless
//!   daemon must be able to reach that layer without linking the widget stack. Re-exported here so
//!   every historical `vike_app_core::alerting::…` path resolves unchanged. The egui window that
//!   edits rules will live in `vike-app` (an explicit follow-up), not in either crate.
//! - [`backend_conn`] — RUNTIME-MUTABLE backend selection (split-plane B1): `BackendConn` (one
//!   active backend: record + observe bridge + optional control channel), `connect_backend`
//!   (key resolution strictly through [`backend_registry::resolve_keys`], control mounted only
//!   when BOTH the record arms it AND the process-level master gate is on), `cli_observe_record`
//!   (the `--observe ADDR` compat record), and `switch_backend` — the safety-critical
//!   BACKEND-SESSION clear (stop the old bridge, reset the session-owned render state, publish an
//!   empty snapshot) that keeps backend A's data from painting under backend B's connection,
//!   while the local feed plane — venue truth the third mode (B2) runs beside the session —
//!   survives the switch; `teardown_feed_plane` is its own mode-exit teardown, and the two
//!   compose to B1's original total clear. Plus the pure picker decisions the Connections tool
//!   renders from.
//! - [`backend_editor`] — the Backends section's ADD/EDIT/DELETE form state machine (split-plane
//!   I2): form validation (name/addr/duplicate refusals collected, not first-wins), the
//!   `key_presence` typo-catcher (presence only — a key VALUE cannot leave the map through it),
//!   the are-you-sure delete affordance, and the drained `RegistryUpdate` whose
//!   `apply_registry_update` pins delete-active's disconnect-BEFORE-save order. Edit-active
//!   deliberately defers to the next connect (`edit_defers` is the UI's cue).
//! - [`backend_registry`] — the GUI's registry of remote tradehub BACKENDS (split-plane
//!   B7/B8/B9): `BackendRecord` (display name, dial address, credential-store KEY NAMES — never
//!   key material — and the per-backend `control` arming gate), `backends.json` persistence
//!   beside the workspace file, and the pure `resolve_keys` connect-time lookup over a
//!   caller-supplied credentials map.
//! - [`backfill_plan`] — the pure Data-Manager bulk-Backfill job planner: maps a grid selection +
//!   known gap ranges to a list of concrete `(venue, symbol, interval, ranges)` kline-backfill
//!   jobs, filtering to `vike-backfill`'s supported venues.
//! - [`core_sync`] — the per-frame `CoreSnapshot` → render-model fold (`sync_from_core`): the
//!   published snapshot's bars plus the live trade tape folded into the per-chart `ChartState`s and
//!   the tick/volume + orderflow aggregators. Moved down verbatim out of `vike-app`'s `main.rs`
//!   because it is the single densest concentration of silent-data-loss history in the GUI — FOUR
//!   venue-routing/gating/lifetime bugs (venue-discarding bar keys, orderflow grouped by splitting
//!   the chart key at `'@'`, a tick/volume workspace stalled behind the `snap.seq` gate, and an
//!   aggTrades-backfill batch DISCARDED when its orderflow aggregator had been torn down
//!   mid-backfill — permanently, since the run-once spawn gate never refetches), each fixed but
//!   recorded only as a code comment, because nothing compiled the file they lived in. Its
//!   `bug_pin_tests` now pin today's behaviour at all four sites.
//! - [`data_sink`] — the live-data seam onto the core: `CoreSinkAdapter` bridges `LiveDataSink` onto
//!   the `BarSender`/`MarketSender` lanes, plus the `BookStore`/`TradeStore` live caches.
//! - [`dom_math`] — DOM pure math: the signed-position resolver + synthetic-book seed helpers.
//! - [`equity_panel`] — `EquityPanelModel`: shapes a `vike_core::CoreSnapshot` into the cross-venue
//!   equity panel view-model (PR-2). Pure mapping, no egui — CI-tested here, rendered by vike-app.
//! - [`feed_lifecycle`] — the WHOLE start/stop story of a live market-data subscription. Two
//!   halves: the pure gates/diffs that decide whether something should happen
//!   (`should_spawn_backfill`, `backfill_earliest_ts`, `orphaned_feed_keys`/`live_window_keys`/
//!   `orphaned_trade_feed_keys`), and the imperative half that performs it — `ensure_feed_on`,
//!   `ensure_trade_feed_on`, `ensure_depth`, `ensure_poly_book` and `reap_orphaned_feeds`, moved
//!   down out of `vike-app`'s `main.rs` where they were `App` methods no gate compiled. The
//!   asymmetry is the point: the diffs were tested while the code that STARTS AND STOPS SOCKETS
//!   from them was not, and the reaper's teardown ordering (which unsubscribe routes to which
//!   venue, which slot is freed so a later `ensure_*` genuinely restarts the feed) is the sibling
//!   of the silent-data-loss class recorded in [`core_sync`]'s module doc. `App`'s fields arrive
//!   as the `FeedSlots`/`SeriesSlots`/`SeriesSpec` bundles, the same shape `core_sync` uses. It
//!   also owns the two retry lanes that keep a one-off failure from burning a slot for the life of
//!   the process: `FeedRetries` (the four `ensure_*` subscribes) and `BackfillRetries` (the
//!   aggTrades backfill), which share one ladder (`retry_backoff`) and one logging discipline.
//! - [`initial_arrange`] — the FIRST-FRAME layout decision out of the frame loop's
//!   `did_initial_arrange` block: the `VIKE_TOOL`/`VIKE_TOOLS` QA capture arms (the two
//!   window-opening sites [`window_spawn`] deliberately left behind — they spend `tool-{n}` ids at
//!   the desktop origin, cascade-free), the `VIKE_ARRANGE` mode parse, and the open-count
//!   arrange-vs-maximize-lone choice. `plan_initial_arrange` is PURE and the ten env reads stay in
//!   `vike-app` (the `ArrangeEnv` Injected shape, as [`startup`]); its roster test runs the REAL
//!   [`feed_lifecycle`] `live_window_keys` over the QA DOM spawn — the half of the DOM-opening
//!   pair [`window_spawn`]'s own invariant test could not reach.
//! - [`inventory`] — re-export of `vike_data_manager::model`: pure grouping of the HistStore
//!   inventory (`Vec<(SeriesId, SeriesCoverage)>`) into a venue → symbol → series display tree
//!   with rollups, for the Data Manager's "Stored" view. Moved to `vike-data-manager` so the
//!   Studio can reuse it too; re-exported under the old path so existing
//!   `vike_app_core::inventory::…` call sites resolve unchanged.
//! - [`observe_bridge`] — the `--observe` thin-client's `WireSnapshot` → `CoreSnapshot` REVERSE
//!   bridge (`wire_to_core` + the per-shape `map_*` helpers), moved verbatim out of `vike-app`'s
//!   CI-excluded `main.rs` so the mapping — above all `map_order_status`'s hand-maintained
//!   Debug-spelling table — is pinned by an exhaustive CI test instead of silently rendering a
//!   new `OrderStatus` variant as terminal `Rejected`. The forward projection lives in
//!   `vike-tradehub/src/publish.rs`. It now also owns the observer's two pieces of IMPERATIVE
//!   wiring, moved down out of `App::new`: `spawn_bridge` (the self-healing reconnect loop — the
//!   only consumer of `RemoteCoreHandle::{connect,is_connected,snapshot}`) and `connect_control`
//!   (the `Scope::Control` gate that can arm REAL order placement from an observer; its
//!   refuse-paths are pinned).
//! - [`options_books`] — `build_books`: folds the user's own deribit working orders + positions out
//!   of a `CoreSnapshot` into the per-instrument `InstrumentBook` map the options chain paints from.
//! - [`options_greeks`] — `position_greeks`: per-position + net portfolio Δ/Γ/ν/Θ for held Deribit
//!   option positions, priced off the live option chains (spot + IV) the Options tool fetched. Pure,
//!   egui-free — the data source for the Greeks tool window.
//! - [`order_dispatch`] — the ONE place a drained UI order intent becomes a `vike_exec::Command`,
//!   and therefore the ONE place [`order_entry`]'s local preview cap is applied. `plan_dispatch` is
//!   PURE (intents + `OrderLimits` + `CoreSnapshot` in, commands + rejects out); `vike-app`'s frame
//!   loop keeps only the I/O. Moved down out of the CI-excluded `main.rs` because the inline block
//!   there had a real hole: of the five submit paths only three validated, and the **Trade window
//!   — the manual order-entry panel — had no local notional cap at all**, nor did its TP+SL bracket
//!   sub-path or the DOM Close/Reverse exit. Validation is now structural (one private chokepoint)
//!   and CI-pinned by an exhaustive `SubmitSource` roster plus an output invariant asserted on
//!   every emitted order write.
//! - [`order_entry`] — the single order-write seam: `OrderTicket` + `build_order_request` (the ONE
//!   `OrderRequest` constructor), `next_client_order_id`, and a LOCAL preview/safety layer
//!   (`validate`/`OrderLimits`/`OrderReject`) that drops self-evidently-malformed / over-cap orders
//!   before they reach the command lane. Not the venue RiskGate — a preview in front of it.
//! - [`orderflow`] — footprint / CVD orderflow aggregation over the live trade tape.
//! - [`poly_labels`] — pure Polymarket display-label helpers for the scalp cockpit:
//!   `poly_short_label` (token-id elision) + `poly_market_short_name` (Gamma question/slug → the
//!   ≤18-char rail/ladder header name). Moved down from `vike-app`'s `main.rs` (audit F4) so their
//!   unit tests run in CI; the feature-gated `GammaMarket`-typed selector (`pick_updown_token`)
//!   stays in `vike-app` — it names a vike-polymarket type this crate deliberately never links.
//! - `preflight` — MOVED to `vike_mount::preflight` (the mount composition root) by the PR that
//!   WIRED it: `vike_run::build_node` runs it for BOTH the GUI and the headless daemon, and
//!   neither `vike-run` nor `vike-mount` may depend on this (egui-shaped) crate to reach it.
//!   Nothing in this crate referenced it, so the move left no compat shim.
//! - [`reconcile_config`] — re-export of [`vike_ops::reconcile_config`]: the pure
//!   `VIKE_RECONCILE*` env-parsing seam (`reconcile_enabled` + `build_recon_config` +
//!   `health_from_feed_status`). MOVED to `vike-ops` with the rest of the off-fold layer, because
//!   the headless daemon parses the SAME knobs and may not link an egui crate to do it.
//! - [`scan`] — re-export of [`vike_ops::scan`]: the pure source scanner behind the
//!   settings-registry gate. MOVED to `vike-ops` (with [`settings`] and the gate itself).
//! - [`settings`] — re-export of [`vike_ops::settings`]: `SETTINGS`, the machine-gated registry of
//!   every environment variable this workspace reads. MOVED to `vike-ops` — it is a WORKSPACE-wide
//!   operator table, not GUI state, and the rule it enforces ("only binaries read the environment")
//!   is an ops rule. Its gate now lives at `crates/vike-ops/tests/settings_registry.rs`.
//! - [`shutdown`] — re-export of [`vike_ops::shutdown`]: the pure, bounded teardown orchestration
//!   (`run_with_deadline`). MOVED to `vike-ops` — `vike-app`'s window close and `vike-tradehub`'s
//!   daemon stop are the two callers, and the second is headless.
//! - [`split_plane`] — the pure ARM decisions behind `App::new` (split-plane B2, the third mode):
//!   `app_mode` (which of the three arms a `(build, --observe)` pair selects — fat+observe is the
//!   third mode: observe a remote backend AND run the local direct-to-venue feeds),
//!   `feed_venues`/`LOCAL_FEED_VENUES` (the ONE written-down copy of the mounted feed set), and
//!   `series_render_source`/`folds_from_snapshot` — the DOUBLE-FOLD GUARD deciding the single
//!   source every series renders from (kline series: the published snapshot's bars, or — third
//!   mode, on a `DIRECT_BAR_VENUES` venue — the venue-fed [`data_sink`] `DirectBarStore`;
//!   tick/volume/orderflow series: the GUI-side trade tape), consulted by [`core_sync`]'s
//!   snapshot and direct-bar folds and by [`backend_conn`]'s backend-switch clear.
//! - [`startup`] — the STARTUP LAYOUT decision lifted out of `App::new`'s tail: which windows a
//!   fresh app opens (a saved workspace, one of five `VIKE_SHOT_WIN` QA tool windows, or the
//!   default BTCUSDT chart), which market-data feeds must be ensured FOR them, and the
//!   `VIKE_STYLE`/`VIKE_SCALE` capture overrides. Six branches over five environment knobs that no
//!   gate compiled, each deciding both what exists and what subscribes — the same
//!   "an unchecked decision opens the wrong socket, or none" class [`feed_lifecycle`] closes.
//!   `plan` is PURE (data in, a `StartupLayout` of decisions out) and the environment is read by
//!   the BINARY and injected as `StartupEnv`, so no settings-registry row moves and the tests need
//!   no `env::set_var`.
//! - [`stored_load`] — the Stored grid's inventory WALK over the `HistStore` TRAIT (`inventory` →
//!   `build_tree`, one `series_gaps` probe per series) plus its `load_partials` sibling (the
//!   cross-kind coverage fold, spec §6-Q2): ONE pair of functions both of `refresh_stored`'s
//!   arms call, so a local and a remote grid over the same data cannot differ in tree shape or in
//!   the Partial column.
//! - [`stored_mode`] — WHICH store the Data-Manager "Stored" grid reads (local vs the datahub at
//!   `config.datahub_addr` — the #1378 seam close) and the per-mode capability table: what Delete,
//!   still a LOCAL-store operation, shows as its reason in remote mode, and which of the Partial
//!   column's three states (local fold / wire answer / honest note) a mode plus a negotiated
//!   `RemoteCoverage` selects.
//! - [`symbol_row`] — the ONE cross-venue instrument search-result row both symbol pickers paint
//!   (`search_result_row`): the chart window's title-bar dropdown, still in `vike-app`, and
//!   [`tool_views`]'s ƒx picker, which moved down here. Shared rather than copied.
//! - [`sync_group`] — chart sync-group registry math: `sync_feed`/`sync_harvest`/`GroupFrame`, the
//!   cross-window crosshair/visible-range broadcast that links grouped chart windows.
//! - [`tickvol`] — tick- and volume-bar aggregation (`TickVolAgg`, `BarKind`).
//! - [`tool_views`] — the egui-only tool BODIES moved out of `vike-app`'s CI-excluded `main.rs`:
//!   Tearsheet, Greeks, News and Calendar (extraction batch 1) plus Data, Stored, Connections and
//!   the chart window's ƒx picker popup (batch 2). Read-only inputs arrive grouped in `ToolCtx`
//!   (audit F8); per-window view state + outgoing intents stay in the separate `&mut ToolView`
//!   param, so the data-in/actions-out seam is unchanged. egui-but-not-eframe like [`workspace`],
//!   so CI builds it and the pure helpers (`tearsheet_rows`, `cal_day_label`, `column_edges`,
//!   `tree_totals`, `indicator_matches`, …) are unit-tested here.
//! - [`tools`] — the per-tool view/data model + the REST tool-fetchers.
//! - [`trade_sizing`] — pure Trade-ticket sizing math: `SizeMode` (Qty/Amount/Cost) + the
//!   size→qty / buying-power / margin-cost / notional helpers the ticket renders from.
//! - [`venue_routing`] — the four pure venue/instrument resolvers moved down out of `vike-app`'s
//!   `main.rs`: `venue_of_key` (the inverse of [`workspace::series_key`](workspace::series_key)),
//!   `venue_bar_instrument` (the per-venue crypto-derivative kline allowlist), and `venue_str` /
//!   `venue_inst` (the DOM venue selector → venue name / native instrument id). Per-venue mapping
//!   tables that rot silently when a venue is added — now compiled, clippied and unit-tested.
//! - [`tradehub_control`] — the thin-client Scope::Control WRITE-path lowering: `wire_from_command`
//!   (a `vike_exec::Command` → `vike_tradehub_client::WireCommand` converter, the GUI-side inverse
//!   of the daemon's `lower_command`) + the `control_enabled` (`VIKE_TRADEHUB_CONTROL=1`) master
//!   gate. Lets a `vike-app --observe` observer drive a remote headless daemon's core; pure,
//!   egui-free, CI-tested here.
//! - [`window_spawn`] — the NEW-WINDOW decision out of `vike-app`'s frame loop: which cascade slot
//!   a freshly opened window lands in, how its [`workspace::WinState`] is built and seeded, and
//!   which live subscriptions must exist before it can paint. SIX hand copies of that arithmetic
//!   sat in a file no gate compiles, two of them already drifted apart — and one of them must
//!   agree with [`feed_lifecycle`]'s per-frame reaper or the new window's feed is stopped on the
//!   very next frame, with a DOM's one-shot ensure never re-requesting it.
//! - [`workspace`] — the multi-window desktop shell: window state/layout (`WinState`), arrange
//!   (cascade/tile/grid), JSON layout persist, the menu bar, and the minimized-window rail. All
//!   egui-but-not-eframe (renders into a passed `egui::Ui`), so it builds and tests on CI.
//!
//! `dom_math`, `feed_lifecycle`, `sync_group`, `shutdown` and `reconcile_config` were moved down
//! from `vike-app` specifically so their `#[cfg(test)]` unit tests run in CI — they were protecting
//! real bugs (`shutdown`: the >6s window-close hang, PR #589; `reconcile_config`: the ~24-test
//! `VIKE_RECONCILE*` env-parsing + feed-health-map seam) but never ran in any gate while they lived
//! in the CI-excluded GUI crate. The last two of those, plus `alerting`/`journal_mat`/`scan`/
//! `settings`, then moved ONE crate further down into [`vike_ops`]: they are the OFF-FOLD layer with
//! no egui in it, and `vike-tradehub` (the headless daemon on the production trading box) was
//! compiling the ENTIRE egui widget stack purely to reach them through this crate. They are
//! re-exported below, so no call site changed.

// The off-fold operations layer moved to `vike-ops` so the headless daemon can link it WITHOUT the
// egui widget stack this crate pulls in (egui/egui_plot/epaint/vike-chart/vike-panels/…). Every
// module is re-exported under its historical path, so existing `vike_app_core::journal_mat::…` /
// `::alerting::…` / `::reconcile_config::…` / `::settings::…` call sites resolve unchanged — the
// same compat shape as the `inventory` re-export below.
pub use vike_ops::{alerting, journal_mat, reconcile_config, scan, settings, shutdown};

pub mod backend_conn;
pub mod backend_editor;
pub mod backend_registry;
pub mod backfill_plan;
pub mod backfill_route;
pub mod backfill_wire;
// The two SYNTHETIC-STATE capture hooks (a seeded Trade panel, a seeded chart drawing) — the
// content half of the capture vocabulary `startup`/`initial_arrange` own the layout half of. Pure
// and tested HERE rather than in the CI-excluded shell, for the reason every other decision in
// this crate moved down: `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI` names `vike-app`.
pub mod capture_seed;
pub mod core_sync;
pub mod data_sink;
// The client half of the datahub MARKET-DATA wire (design §9). `datahub_feed` is one
// `vike_data::DataClient` per venue slug; `md_session` owns the desired set, the two background
// threads and every rule the wire's §6.3/§7.3/§7.4 place on a client. They live HERE, not in the
// shell, for the reason everything else did: `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI` names
// `vike-desktop`, and a thread-and-socket state machine nobody runs is a state machine nobody has.
pub mod datahub_feed;
pub mod datahub_resolve;
pub mod dom_math;
pub mod equity_panel;
// Where the shell looks for the fonts egui does not bundle — the tables and their guard live here
// rather than in `vike-app`, because a test in the CI-excluded shell is a test nobody runs, which
// is how every candidate path came to name Windows only.
pub mod feed_lifecycle;
pub mod fonts;
// The FIRST-FRAME layout decision (the frame loop's `did_initial_arrange` block): the
// VIKE_TOOL/VIKE_TOOLS QA capture arms `window_spawn`'s planner deliberately excludes (they spend
// `tool-{n}` ids at the desktop origin, no cascade), the VIKE_ARRANGE parse, and the open-count
// arrange-vs-maximize-lone choice. The env reads stay in `vike-app`'s `main.rs` and arrive as
// `ArrangeEnv`; the QA DOM's agreement with `feed_lifecycle`'s reaper is CI-tested here.
pub mod initial_arrange;
// The inventory tree-model moved to `vike-data-manager` (reused by the Studio too). Re-exported
// under the old path so existing `vike_app_core::inventory::…` call sites resolve unchanged.
pub use vike_data_manager::model as inventory;
pub mod md_session;
pub mod observe_bridge;
pub mod options_books;
pub mod options_greeks;
pub mod order_dispatch;
pub mod order_entry;
pub mod orderflow;
pub mod poly_labels;
// The pure ARM decisions behind `App::new` (split-plane B2): the three-mode table, the ONE copy
// of the mounted feed-venue set, and the double-fold guard deciding each series' single render
// source. `vike-app`'s `main.rs` keeps the wiring; the decisions are CI-tested here.
pub mod split_plane;
// The shared cross-venue instrument search-result row, painted by BOTH symbol pickers: the chart
// window's title-bar dropdown (still in vike-app) and `tool_views::fx_picker_popup`'s per-study
// source menu. It moved down with the ƒx picker (tool-view extraction batch 2).
// The startup-layout decision out of `App::new`'s tail: which windows a fresh app opens, which
// feeds they need, and the two QA capture overrides. Six branches over five env knobs that no gate
// compiled — the env READS stay in `vike-app`'s `main.rs` and arrive as `StartupEnv`.
pub mod startup;
// The bottom status strip's two DOT COLOURS, out of `vike-app`'s CI-excluded `status_bar`. The feed
// dot hand-rolled a classifier that disagreed with `vike_model::feed_status::parse_feed_status` —
// the one the Connections tool and the headless health gate both read — and the control dot
// recovered a `bool` by searching the string `tradehub_control` had just built from it.
pub mod status_dot;
pub mod stored_load;
pub mod stored_mode;
pub mod symbol_row;
pub mod sync_group;
pub mod tickvol;
pub mod tool_views;
pub mod tools;
pub mod trade_sizing;
pub mod tradehub_control;
// The four pure venue/instrument resolvers (`venue_of_key`/`venue_bar_instrument`/`venue_str`/
// `venue_inst`) moved down from `vike-app`'s CI-excluded `main.rs` so the merge gate tests them.
pub mod venue_routing;
// The NEW-WINDOW decision out of `vike-app`'s frame loop: the cascade slot, the `WinState`
// construction, and the subscriptions a spawned window needs before it can paint. Six hand copies
// in a file no gate compiles, two of which had already drifted — and one of which must agree with
// `feed_lifecycle`'s reaper or the new window's feed dies on the frame after it opens.
pub mod window_spawn;
pub mod workspace;

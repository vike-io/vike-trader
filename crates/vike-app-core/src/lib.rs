//! `vike-app-core` — the eframe-free / wgpu-free logic extracted from the desktop shell so it runs
//! in CI.
//!
//! The wgpu GUI shell, `vike-desktop` (`vike-app` until 2026-09-09, #1727), sits outside the
//! derived CI roster: ci.yml's `app-check` job checks it, clippy-gates it and runs its unit tests,
//! but only on a PR whose plan reaches it, and its frame loop runs under no test. The modules here
//! touch NO eframe/wgpu (egui at most — the [`ui::window`]/[`ui::windows`] draw modules), so
//! they build AND test with the roster exactly like `vike-chart` does. The shell re-imports them and
//! wires them into its frame loop. Each module's own `//!` is its contract; this list only says
//! where to look.
//!
//! - [`backend`] — connections to backend daemons:
//!   - [`backend::connection`] — the runtime-mutable active backend and the backend-switch clear.
//!   - [`backend::editor`] — the Backends section's add/edit/delete form state machine.
//!   - [`backend::identity`] — how a backend is identified on screen (one authority).
//!   - [`backend::registry`] — the known remote tradehub backends and this desktop's device key.
//!   - [`backend::observe_bridge`] — the `--observe` dial/re-dial thread over `vike_tradehub_client`'s
//!     `CoreSnapshotWire` → `CoreSnapshot` decode.
//!   - [`backend::split_plane`] — the pure arm decisions behind `App::new` and the double-fold guard.
//!   - [`backend::tradehub_control`] — the thin-client `Scope::Write` send path over `vike_tradehub_client`'s
//!     command lift.
//! - [`data`] — market data and history:
//!   - [`data::backfill`] — the datahub verb that backfill runs through (its one route).
//!   - [`data::backfill::plan`] — the Data Manager's bulk-Backfill job planner.
//!   - [`data::catalog_refresh`] — the explicit per-venue instrument-catalog refresh.
//!   - [`data::catalog_refresh::server_route`] — the refresh's second route: the datahub's `VenueCatalog` verb.
//!   - [`data::chart_key`] — the chart key: `series_key`, `venue_of_key` and `venue_bar_instrument`.
//!   - [`data::data_sink`] — the live-data seam (`DataClientSinkGuiFeed`) and its live caches.
//!   - [`data::data_manager`] — the Stored grid's inventory walk over the `HistoryStore` trait.
//!   - [`data::data_manager::mode`] — which store the Stored grid reads, and what each mode can do.
//!   - [`data::datahub`] — the one datahub-address adoption rule.
//!     - [`data::datahub::dial`] — the datahub dial the desktop's datahub clients share.
//!     - [`data::datahub::feed_retry`] — the one retry policy every feed subscribe shares.
//!   - [`data::history_column`] — the Data Manager's HISTORY column.
//!   - [`data::market_data_session`] — the desktop's one market-data connection to a `vike-datahub`.
//!   - [`data::market_data_session::data_client`] — one `vike_data::DataClient` per venue slug.
//!   - [`data::stored`] — a chart asks the backend's own store for its interval.
//!   - [`data::stored::chart_seed`] — when the backend's store holds nothing, ask the server to fetch it.
//! - [`orders`] — order entry:
//!   - `orders::option_chain_orders` — the user's own Deribit orders and positions per
//!     instrument.
//!   - `orders::option_chain_position_greeks` — per-position and net portfolio greeks.
//!   - the dispatcher, the ONE place a UI order intent becomes a `vike_execution::Command`, is
//!     `vike_order_entry::dispatch` (the local preview is the shared client's
//!     `vike_tradehub_client::order_checks`).
//! - [`ui`] — the window shell:
//!   - [`ui::appearance_settings`] — the five appearance preferences.
//!   - [`ui::caption`] — the main window's top bar.
//!   - [`ui::capture_seed`] — the synthetic-state capture hook (`VIKE_CHART_DRAW`).
//!   - [`ui::core_sync`] — the per-frame `CoreSnapshot` → render-model fold.
//!   - [`ui::desktop_entry`] — the Linux launcher entry install/uninstall.
//!   - [`ui::feed_lifecycle`] — the whole start/stop story of a live market-data subscription.
//!   - [`ui::frame_cap`] — the frame-rate cap the desktop adds when vertical sync is off.
//!   - [`ui::gpu_startup`] — the GPU startup ladder (rungs, adapter choice, failure message) and the render target contract.
//!   - [`ui::menu_bar`] — the File / Window / Help menu bar the caption draws.
//!   - [`ui::series_follow`] — the chart follows the series the backend publishes.
//!   - [`ui::startup`] — the startup layout decision (`StartupEnvironment` in, `StartupLayout` out).
//!   - [`ui::status_bar`] — the main window's bottom strip.
//!     - `ui::status_bar::dot` — the status strip's two dot colours.
//!   - [`ui::symbol_picker`] — the chart window's symbol picker.
//!   - [`ui::symbol_picker::row`] — the one instrument search-result row both symbol pickers paint.
//!   - [`ui::window`] — the multi-window desktop: `WindowState`, arrange, the workspace file, rail:
//!     - [`ui::window::initial_arrange`] — the first-frame layout decision.
//!     - [`ui::window::spawn`] — the new-window decision: cascade slot, `WindowState`, subscriptions.
//!     - [`ui::window::sync_group`] — chart sync-group crosshair / visible-range broadcast math.
//!   - [`ui::windows`] — the egui tool-window bodies (`WindowToolContext` in, `&mut WindowView` out;
//!     [`ui::windows::WindowView`] is the per-window view state).
//!
//! This crate re-exports nothing from another crate. Its re-exports of `reconcile_config`,
//! `alerting`, `journal_mat` and `vike_ops::{scan, settings, shutdown}` were deleted between
//! 2026-09-23 and 2026-09-28; callers name each where it lives (`crates/vike-app-core/CLAUDE.md`'s
//! "Where logic belongs").

#![warn(unreachable_pub)]

// The GUI's egui-free logic, grouped by concern (2026-09-27).
pub mod backend;
pub mod data;
pub mod orders;
pub mod ui;

/// The fixtures a `src/` unit test and an integration suite both use (`test-support` only: a
/// shipped build never compiles it).
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

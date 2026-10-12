//! `vike-app-core` — the eframe-free / wgpu-free logic extracted from the desktop shell so it runs
//! in CI.
//!
//! The wgpu GUI shell, `vike-desktop` (`vike-app` until 2026-09-09, #1727), sits outside the
//! derived CI roster: ci.yml's `app-check` job checks it, clippy-gates it and runs its unit tests,
//! but only on a PR whose plan reaches it, and its frame loop runs under no test. The modules here
//! touch NO eframe/wgpu (egui at most — the [`ui::workspace`]/[`ui::tool_views`] draw modules), so
//! they build AND test with the roster exactly like `vike-chart` does. The shell re-imports them and
//! wires them into its frame loop. Each module's own `//!` is its contract; this list only says
//! where to look.
//!
//! - [`backend`] — connections to backend daemons:
//!   - [`backend::backend_conn`] — the runtime-mutable active backend and the backend-switch clear.
//!   - [`backend::backend_editor`] — the Backends section's add/edit/delete form state machine.
//!   - [`backend::backend_identity`] — how a backend is identified on screen (one authority).
//!   - [`backend::backend_registry`] — the known remote tradehub backends and `resolve_keys`.
//!   - [`backend::observe_bridge`] — the `--observe` `WireSnapshot` → `CoreSnapshot` reverse bridge.
//!   - [`backend::split_plane`] — the pure arm decisions behind `App::new` and the double-fold guard.
//!   - [`backend::tradehub_control`] — the thin-client `Scope::Write` command lowering and send path.
//!   - [`backend::venue_routing`] — `venue_of_key` and the per-venue kline instrument allowlist.
//! - [`data`] — market data and history:
//!   - [`data::backfill_plan`] — the Data Manager's bulk-Backfill job planner.
//!   - [`data::backfill_route`] — WHERE a Data-Manager / chart-gap kline backfill runs.
//!   - [`data::backfill_wire`] — the wire arm of that backfill.
//!   - [`data::catalog_refresh`] — the explicit per-venue instrument-catalog refresh.
//!   - [`data::catalog_wire`] — the refresh's second route: the datahub's `VenueCatalog` verb.
//!   - [`data::chart_seed`] — when the backend's store holds nothing, ask the server to fetch it.
//!   - [`data::data_sink`] — the live-data seam (`GuiFeedSink`) and its live caches.
//!   - [`data::datahub_feed`] — one `vike_data::DataClient` per venue slug.
//!   - [`data::datahub_resolve`] — the one datahub-address adoption rule.
//!   - [`data::history_column`] — the Data Manager's HISTORY column.
//!   - [`data::md_session`] — the desktop's one market-data connection to a `vike-datahub`.
//!   - [`data::store_bars`] — a chart asks the backend's own store for its interval.
//!   - [`data::stored_load`] — the Stored grid's inventory walk over the `HistStore` trait.
//!   - [`data::stored_mode`] — which store the Stored grid reads, and what each mode can do.
//! - [`orders`] — order entry:
//!   - [`orders::dom_math`] — the DOM's signed-position resolver and no-book classifier.
//!   - [`orders::equity_panel`] — the cross-venue equity panel view-model.
//!   - [`orders::options_books`] — the user's own Deribit orders and positions per instrument.
//!   - [`orders::options_greeks`] — per-position and net portfolio greeks.
//!   - [`orders::order_dispatch`] — the ONE place a UI order intent becomes a `vike_exec::Command`.
//!   - [`orders::order_entry`] — the ticket, the one `OrderRequest` constructor, the local preview.
//! - [`ui`] — the window shell:
//!   - [`ui::appearance_settings`] — the five appearance preferences.
//!   - [`ui::caption`] — the main window's top bar.
//!   - [`ui::capture_seed`] — the synthetic-state capture hooks.
//!   - [`ui::core_sync`] — the per-frame `CoreSnapshot` → render-model fold.
//!   - [`ui::desktop_entry`] — the Linux launcher entry install/uninstall.
//!   - [`ui::feed_lifecycle`] — the whole start/stop story of a live market-data subscription.
//!   - [`ui::initial_arrange`] — the first-frame layout decision.
//!   - [`ui::poly_labels`] — Polymarket display labels for the scalp cockpit.
//!   - [`ui::series_follow`] — the chart follows the series the backend publishes.
//!   - [`ui::startup`] — the startup layout decision (`StartupEnv` in, `StartupLayout` out).
//!   - [`ui::status_bar`] — the main window's bottom strip.
//!   - [`ui::status_dot`] — the status strip's two dot colours.
//!   - [`ui::symbol_picker`] — the chart window's symbol picker.
//!   - [`ui::symbol_row`] — the one instrument search-result row both symbol pickers paint.
//!   - [`ui::sync_group`] — chart sync-group crosshair / visible-range broadcast math.
//!   - [`ui::tool_views`] — the egui tool-window bodies (`ToolCtx` in, `&mut ToolView` out).
//!   - [`ui::window_spawn`] — the new-window decision: cascade slot, `WinState`, subscriptions.
//!   - [`ui::workspace`] — the multi-window desktop: `WinState`, arrange, persist, menu, rail.
//! - [`tools`] — the per-tool view/data model and the REST tool-fetchers.
//!
//! This crate re-exports nothing from another crate. Its re-exports of `reconcile_config`,
//! `alerting`, `journal_mat` and `vike_ops::{scan, settings, shutdown}` were deleted between
//! 2026-09-23 and 2026-09-28; callers name each where it lives (`crates/vike-app-core/CLAUDE.md`'s
//! "Where logic belongs").

#![warn(unreachable_pub)]

// The GUI's egui-free logic, grouped by concern (2026-09-27); `tools` stays at the root beside them.
pub mod backend;
pub mod data;
pub mod orders;
pub mod ui;

pub mod tools;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Recover a poisoned lock rather than propagating the panic: a panic in one background thread
/// must not convert into a frozen GUI. The crate's ONE spelling of that policy (every guarded value
/// here is plain data that a half-finished writer leaves readable).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

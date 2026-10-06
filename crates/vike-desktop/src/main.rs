//! vike_trader_rust — native egui multi-window shell with live Binance 1m/5m/…
//! spot charts. Each window has a rich title toolbar (chart-style menu, symbol
//! dropdown, interval dropdown, LIVE badge, clone/minimize/maximize icons) — the
//! egui analog of vike's `UnifiedTitleBar`.
//!
//! R5c architecture: market data flows feed threads → vt-core ingest (lossless bar
//! closes, conflated forming/ticks) → the core-owned bar cache → arc-swap
//! `CoreSnapshot` → this GUI converts to its render model per changed snapshot. The
//! GUI is a lossy observer; it never touches the core's state directly.
//!
//! ⚠ **The core it observes is REMOTE, and there is no other shape.** This binary signs nothing,
//! mounts no venue and opens no venue socket: every order leaves through the backend's Scope
//! ::Control channel, and every series it renders comes off the wire (`WireSnapshot`) or does not
//! render. It used to carry a second, DEFAULT configuration — the `fat` feature — that spawned a
//! LOCAL twelve-venue trading core and six direct-to-venue market-data feeds, with
//! `--no-default-features` selecting the "thin" observer instead. Both features are deleted; the
//! observer is the only build. The tombstones through this file name what each site did, and the
//! crate-level `allow(dead_code, unused_imports)` that used to hold the thin build together (it was
//! `#![cfg_attr(not(feature = "fat"), …)]`, right here) is gone with them — nothing is
//! configuration-dependently dead any more.

// The egui-free logic (incl. the egui-but-not-eframe workspace module) moved to the CI-gated
// `vike-app-core` crate; imported here so the existing `data_sink::`/`tools::`/`workspace::` paths
// resolve unchanged.
// (`equity_panel`/`trade_sizing` left with the Trade panel body in tool-view extraction batch 3,
// and the Trade window plan then deleted `trade_sizing` with the old ticket; `order_entry` stays,
// but ONLY for the policy-derived `OrderLimits` — every UI order request is now built and capped
// by `order_dispatch::plan_dispatch`, which this file calls with the frame's drained intents.)
use vike_app_core::data::backfill_plan::{BackfillJob, plan_backfill_jobs};
use vike_app_core::data::backfill_route::{BackfillRoute, backfill_route};
use vike_app_core::data::backfill_wire;
use vike_app_core::{data::data_sink, orders::order_entry, tools, ui::workspace};
use vike_orderflow::{bar_agg, tickvol};
// The pure, unit-tested helpers that used to live in this (CI-excluded) file also moved DOWN into
// vike-app-core so their `#[cfg(test)]` tests finally run in a gate (the dedup-app refactor). Brought
// back into scope by their original bare names so every call site here reads unchanged.
// (`default_price`/`synth_book`/`tick_for` left with the DOM body in tool-view extraction batch 3
// and were then DELETED outright on 2026-09-15 — the DOM stopped fabricating a ladder or a price
// when it had no book and said so instead, and the Trade window that replaced it does the same;
// `vike_app_core::orders::dom_math`'s module doc carries the argument.
// `signed_position_size` left with the DOM Close/Reverse exit in the order-dispatch extraction.)
// The WHOLE feed-lifecycle cluster now lives there — not just the pure gates/diffs
// (`should_spawn_backfill`, `backfill_earliest_ts`, the orphan diffs) but the imperative half too:
// `ensure_feed_on` / `ensure_trade_feed_on` / `ensure_depth` / `ensure_poly_book` /
// `reap_orphaned_feeds`, which this file keeps only as one-line `App` methods that build the
// `FeedSlots`/`SeriesSlots` bundles. The module is imported WHOLE (rather than each function bare,
// the older style below) so every one of those wrappers reads as an explicit delegation.
use vike_app_core::ui::feed_lifecycle;
// The five appearance preferences (design system spec §5): the READS stay here, in `App::new`;
// the mapping from the resolved rows to an `Appearance`, and the session that applies and saves a
// change from the Settings window, are one crate down.
use vike_app_core::ui::appearance_settings::{self, AppearanceSession};
// `App::new`'s startup-layout tail (which windows open, which feeds they need, the two QA capture
// overrides) moved down too — same reason, same shape: this file reads the five env knobs and hands
// them over as `StartupEnv`, then applies the returned `StartupLayout` in its documented order.
use vike_app_core::ui::startup;
use vike_app_core::ui::sync_group::{GroupFrame, sync_feed, sync_harvest};
// The NEW-WINDOW decision moved down beside `startup`, for the same reason and in the same shape:
// SIX places in the frame loop below each re-derived the cascade slot, the `WinState` construction
// and the market-data subscription by hand, in a file `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI`
// keeps out of every roster lane — and two of the six had already drifted apart (one cascades from
// a different origin; a title-bar clone drops its source window's venue). They are one
// `SpawnRequest` roster now, whose branches CI executes, and `App::apply_spawn` below — which
// applies the returned `SpawnPlan` in its documented order — is the whole of what stayed here.
use vike_app_core::ui::window_spawn;
// The FIRST-FRAME arrange decision moved down beside it (actions-out batch 2): the VIKE_TOOL/
// VIKE_TOOLS QA capture arms — the two window-opening sites `window_spawn`'s module doc
// deliberately left behind, because they spend `tool-{n}` ids at the desktop origin with no
// cascade — plus the VIKE_ARRANGE parse and the arrange-vs-maximize-lone choice. This binary
// reads the ten QA knobs and injects them as `ArrangeEnv`; the guarded apply block in `ui` below
// is the whole of what stayed here.
use vike_app_core::ui::initial_arrange;
// (The four pure venue/instrument resolvers of `vike_app_core::backend::venue_routing` were imported
// here by their bare names; their last callers in this crate, the DOM window's routing and its QA
// test order, went with the DOM window.)
// The per-frame CoreSnapshot → render-model fold — see the thin `App::sync_from_core` wrapper
// below, and that module's doc for why it had to leave this CI-excluded file.
use vike_app_core::ui::core_sync;
// The adoption planner, the published-series list and the title-bar badge's state — one module,
// because all three answer the same question (what does this node actually have) and the whole
// defect was three surfaces answering it separately.
use vike_app_core::ui::series_follow;
// The chart's read of the BACKEND'S OWN hist store — the fetch half of the same question, and the
// only reason an interval the daemon does not stream can paint.
use vike_app_core::data::store_bars;

// The chart engine lives in vike-chart (extraction, chart roadmap phase 2); the
// crate-root re-exports keep the historical `crate::chart::…` paths of
// workspace/ and main.rs compiling untouched.
pub(crate) use vike_chart::{DisplayTz, ScaleMode, chart, indicators, model};
// The Polymarket cockpit widgets are re-exported at the crate root (no submodule), so alias the
// crate to `cockpit` for the action-drain match arms. (The cockpit BODY that builds their inputs
// moved to `vike_app_core::ui::tool_views::cockpit` in batch 3; only the drain stays here.)
pub(crate) use vike_cockpit as cockpit;

// DATA layer lives in the layer-correct crates now (R-DataClient slice): symbol universes
// (DataSets) -> vike-data. ⚠ TOMBSTONE — a `use vike_binance::market_feed as marketfeed;` stood
// here, `fat`-gated, and was the alias `build_local_feeds` constructed the Binance kline feed
// through. vike-binance is not a dependency of this crate any more (ruling 2).
use vike_data::datasets; // subscribe_bars/quotes/trades/book/unsubscribe/shutdown verbs

// `App::recorder`'s type. ⚠ TOMBSTONE — this used to be a `#[cfg]` PAIR: under `fat` it aliased
// vike-data's DataFusion-gated `RecorderHandle`, and the uninhabited stand-in below existed only so
// the thin build's always-compiled sites (`App`'s struct literal and `on_exit`'s
// `recorder.shutdown()`) still type-checked. The alias is gone with the tick recorder itself (see
// `open_tick_recorder`'s tombstone), so the uninhabited enum is the only shape: `App::recorder` can
// only ever be `None`, and this `shutdown` is provably unreachable rather than merely unused.
// Whether the field and its teardown step should exist at all is a decision this change did not
// take — see `App::run_bounded_teardown`.
enum RecorderHandleTy {}
impl RecorderHandleTy {
    // `App::run_bounded_teardown` names `recorder.shutdown()` and an `Option<RecorderHandleTy>` is
    // always `None`, so this body is unreachable by construction — `match self {}` says so to the
    // compiler rather than to a reader.
    fn shutdown(self) {
        match self {}
    }
}
/// One `refresh_stored` background load's result: the inventory tree, its per-series gap map, its
/// per-instrument cross-kind partial-day map, what the load LEARNED about the coverage verb (spec
/// §6-Q2 — `RemoteCoverage`, the third input to the Partial column's state) and, when there is one,
/// the REASON the store could not be read.
///
/// ⚠ **It was a four-element TUPLE, aliased here because its type was spelled in three places
/// (`stored_rx`, `stored_tx`, the worker's `tx.send`) and copies would have to be edited in
/// lockstep.** It is now `vike-app-core`'s own struct, and the alias survives only so those three
/// spellings stay one edit. The move is what lets the load carry its failure REASON: a fifth tuple
/// slot would have been an `Option<String>` nothing named, and the whole point of the field is that
/// the render can tell an empty store from an unreachable one.
type StoredLoad = vike_app_core::data::stored_load::StoredLoadOutcome;

use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

// `App`'s non-constructor methods (feed lifecycle, workspace restore, backend settings,
// depth subscriptions, core sync) — split out of this file for size; a second `impl App { .. }`
// block. `App::new` (the constructor) stays here.
mod app_methods;
// `App`'s window-close teardown — the BODY of the `eframe::App::on_exit` hook, split out of this
// file for size as an INHERENT `App::run_bounded_teardown` the trait impl below forwards to in one
// line. ⚠ NOT a second `impl eframe::App for App`: coherence allows exactly one impl per trait
// (`E0119`), unlike the inherent split `app_methods.rs` uses. `persist_egui_memory` (one `false`)
// and `ui` stay here — see `app_lifecycle`'s module doc.
mod app_lifecycle;
// `ui`'s BODY — the per-frame render/drain work, as three free functions (`frame_begin` →
// `draw_chrome` → `draw_windows`). ⚠ `ui` itself deliberately STAYS in this file: two blocks inside
// it read the process environment (the `VIKE_SHOT` capture, the `ArrangeEnv` ten-knob read), and
// `crates/vike-ops/tests/settings/settings_registry.rs` classifies an `env::var` read by FILE PATH — only
// `main.rs`/`src/bin/*`/`examples/*` earn `Layer::Binary`. See `app_ui`'s module doc.
mod app_ui;
// GPU Candle Layer Phase 2 (Task 2): the wgpu candle pipeline + `CandleCallback` behind
// vike-chart's `GpuCandleItem` seam. Registered in `App::new` (see `gpu_ok`); the render toggle
// that feeds it is wired in Task 3.
mod chart_gpu;
// The chart window's title-bar toolbar (chart-style menu, symbol/interval dropdowns, sync/OF/Cmp
// popups, LIVE badge, window controls) — extracted out of this file to shrink it and co-locate
// the chart-window chrome; app-specific, so it stays in vike-desktop rather than vike-chart.
mod chart_window;
// ⚠ TOMBSTONE — `use vike_app_core::reconcile_config;` stood here, and a paragraph explaining that
// `App::new` mounted `vike_core::spawn_recon` through it. That mount left with the local trading
// core: this binary arms no venue account, so there is nothing to reconcile HERE and the driver
// runs in the daemon this shell observes. The `VIKE_RECONCILE*` parsers and their tests stay in
// `vike-app-core` for that daemon's sake; the import went because nothing in this file named it
// any more and `-D unused_imports` said so.
// Tool-view extraction: the egui-only tool bodies live in the CI-gated vike-app-core now —
// `tool_views` (read-only inputs grouped in `ToolCtx`, `&mut ToolView` still a separate param),
// dispatched from `tool_content` below. Batch 1 took Tearsheet/Greeks/News/Calendar; batch 2 took
// Data (incl. its Stored sub-tab), Connections and the chart window's ƒx picker popup; batch 3 took
// the whole Trade panel family plus the DOM and Polymarket-cockpit glue (the widgets themselves
// already lived in vike-panels/vike-cockpit). The pure Polymarket label helpers moved with batch 1
// (their unit tests now run in CI). ⚠ TOMBSTONE — this file used to import one of them
// (`vike_app_core::ui::poly_labels::poly_market_short_name`) for `pick_updown_token`'s display name;
// that function is gone with the Polymarket bridge edge, so the import went with it. The helper
// itself is untouched in vike-app-core and still has consumers there.
use vike_app_core::ui::tool_views;
// The cockpit's tuning constants + the resolved-order-intent enum moved with the cockpit body
// (batch 3); re-imported under their original bare names so the window bookkeeping and the
// action-drain match arms below read unchanged. `poly_cockpit_seed_token` (an env read)
// deliberately STAYED here.
use vike_app_core::ui::tool_views::{CockpitCmd, POLY_PLACEHOLDER_TOKEN, POLY_STALE_MS};

const SYMS: [&str; 3] = ["BTCUSDT", "ETHUSDT", "SOLUSDT"];
/// C2a Task 4: per-overlay line colors for Compare series, indexed by add-order
/// (`WinState::compare` position, wrapping via `% len`). Deliberately distinct
/// from the candle up/down green/red (`palette::UP`/`palette::DOWN`) so an overlay never
/// reads as a primary-series body. The title-bar Compare chip for overlay `i`
/// is tinted the same color, keeping the on-chart line and its legend in sync. The five colours
/// (blue, orange, purple, cyan, pink) are `ui-theme.toml`'s `desktop` rows `COMPARE_1`..`COMPARE_5`.
const COMPARE_COLORS: [egui::Color32; 5] = [
    vike_ui_theme::value::desktop::COMPARE_1,
    vike_ui_theme::value::desktop::COMPARE_2,
    vike_ui_theme::value::desktop::COMPARE_3,
    vike_ui_theme::value::desktop::COMPARE_4,
    vike_ui_theme::value::desktop::COMPARE_5,
];
/// **THE RENDER-SOURCE MODE SIGNAL for this binary** — what fills `App::direct_bars`, bound once
/// from the arm table rather than inferred at each fold from `direct_bars.is_some()`.
///
/// `AppMode::ObserveOnly` is this binary's only arm (rulings 1 and 2: no local core, no venue
/// sockets), and under `BarPlane::BackendStore` a kline series renders from the store exactly
/// where the backend's live snapshot does not publish it. `core_sync` `debug_assert!`s this
/// against the handle, so the two cannot drift.
const BAR_PLANE: vike_app_core::backend::split_plane::BarPlane =
    vike_app_core::backend::split_plane::bar_plane(
        vike_app_core::backend::split_plane::AppMode::ObserveOnly,
    );

/// The venue KLINE intervals the title-bar menu offers.
///
/// ⚠ `4h` and `1d` joined on 2026-09-15 — the owner's own worked example ("i open same ethusdt
/// daily") named an interval this menu could not produce at all. They need nothing from the
/// venues: `vike_model::time::interval_ms` already parses both, `tickvol::BarKind::parse` already
/// treats both as klines, and the collectors already write them.
///
/// ⚠ `1s` STAYS, and it is the one row that cannot paint everywhere: **bybit's kline minimum is
/// 1m**, so no `1s` rows exist in the store for it (or for okx). Whether to keep offering a row
/// two of the three collector venues can never answer is a product call, deliberately not taken
/// here.
const IVLS: [&str; 7] = ["1s", "1m", "5m", "15m", "1h", "4h", "1d"];
/// Tick-count aggregation intervals (Task B5) — client-side, built from the trade tape by
/// `tickvol::TickVolAgg`; parsed by `tickvol::BarKind::parse` (`"<n>t"`). Its own dropdown group,
/// separated from `IVLS` (venue klines) by a `ui.separator()` in `title_bar`.
const TICK_IVLS: [&str; 3] = ["100t", "500t", "1000t"];
/// Volume-threshold aggregation intervals (Task B5) — same client-side path as `TICK_IVLS`,
/// parsed as `"<x>v"`.
const VOLUME_IVLS: [&str; 3] = ["1v", "10v", "100v"];

/// QA capture hook — PRESENT (any value, `is_ok()`): seed the Account window with one resting
/// PAPER limit order and one open PAPER position, so a headless capture of that window shows a
/// session instead of two empty tables.
///
/// Read TWICE from this file and nowhere else: once to arm [`App::trade_seed_pending`] (the frame
/// loop submits the orders) and once into `startup::StartupEnv::trade_seed` (the startup layout
/// opens the bar feed whose closes clock the paper fill). Both halves are needed — see
/// [`vike_app_core::ui::capture_seed`]'s module doc, and `vike_app_core::ui::startup::StartupEnv`'s
/// `trade_seed` field for why a feed with no chart window on screen has to be arranged for.
///
/// ⚠ It can only ever reach a PAPER venue, and that is enforced at the submit site rather than
/// here: the frame loop refuses to seed a venue in `App::live_venues`, and refuses entirely while
/// a remote `Scope::Write` channel is mounted (`vike_app_core::ui::capture_seed`'s
/// `plan_trade_seed_commands` holds both guards, where CI runs them).
const TRADE_SEED_ENV: &str = "VIKE_TRADE_SEED";

/// QA capture hook — PRESENT (any value, same idiom as [`TRADE_SEED_ENV`]): draw the two capture
/// overlays [`vike_app_core::ui::capture_seed::trendline_overlay`] derives (a trendline through the
/// series' extremes and a support level at its low) on every chart whose series has bars.
///
/// ⚠ This is the FIRST producer of `vike_chart::model::ChartState::overlays`, not a headless
/// stand-in for a click. That field is the chart's user-drawing layer, it has always been READ by
/// `crates/vike-chart/src/chart/price_render.rs`'s `paint_price_overlays`, and nothing in the
/// workspace has ever written it — so there is no drawing tool this bypasses. Writes nothing when
/// unset; an unset run leaves every `overlays` map empty exactly as before.
const CHART_DRAW_ENV: &str = "VIKE_CHART_DRAW";

/// Actions a window's title toolbar reports back for one frame.
#[derive(Default)]
struct TitleActions {
    new_symbol: Option<String>,
    /// Cross-exchange symbol search: the venue of the just-selected symbol (`Some("bybit")`, …),
    /// set alongside `new_symbol` — from a search hit's own venue, or `"binance"` for a plain
    /// quick-pick. `None` when no symbol was picked this frame. Applied together with `new_symbol`
    /// so the window's `venue` and `symbol` always change atomically (and the feed re-routes).
    new_venue: Option<String>,
    /// Feed-routing slice 1: the just-selected symbol's asset class (from the search hit's
    /// `Instrument.asset_class`), set alongside `new_symbol`/`new_venue`. `None` for a plain
    /// quick-pick / non-catalog path (spot/legacy — `ensure_feed_on` routes it exactly as
    /// before). Applied atomically with `new_symbol`/`new_venue` so `WinState::asset_class`
    /// never lags behind the symbol it describes.
    new_asset_class: Option<vike_model::AssetClass>,
    new_interval: Option<String>,
    /// The symbol picker's dashed chip was clicked: the venue whose catalog list the shell should
    /// load, through the same `CatalogRefresh::request` the Data Manager's Refresh button uses.
    load_venue: Option<String>,
    /// Chart sync seam (task B8): `Some(new_value)` when the title-bar sync chip was
    /// clicked this frame, cycling `None -> Some(1) -> Some(2) -> Some(3) -> Some(4) ->
    /// None` — an `Option<Option<u8>>` so "no click" (`None`) is distinguishable from
    /// "click set the group to ungrouped" (`Some(None)`), exactly like `new_interval`
    /// distinguishes "no click" from a re-picked value.
    new_group: Option<Option<u8>>,
    set_style: Option<chart::ChartStyle>,
    toggle_max: bool,
    minimize: bool,
    clone: bool,
    close: bool,
    open_picker: bool,
    /// SP2 orderflow (Task 7): new value when the title-bar Orderflow popup's "CVD"
    /// checkbox changed this frame (chart windows only — see `title_bar`).
    cvd_on: Option<bool>,
    /// SP2 orderflow (Task 7): new value when the popup's "Volume Profile" checkbox changed.
    profile_on: Option<bool>,
    /// SP2 orderflow (Task 7): new value when the popup's "Tick size" Auto checkbox or
    /// numeric field changed (`Some(None)` = back to Auto; `Some(Some(x))` = pinned to `x`).
    of_tick_size: Option<Option<f64>>,
    /// SP3 follow-up (Task 2): new value when the popup's "Backfill hours" field changed this
    /// frame. Unlike the three fields above, `of_backfill_hours` is a GLOBAL `App` field, not
    /// per-window — every chart window's OF popup renders/edits the SAME value (there is no
    /// `w.of_backfill_hours`) — but the action still follows the identical `Option<T>` "did it
    /// change this frame" shape as `new_interval`/`cvd_on`/`of_tick_size` so the apply-after-
    /// the-loop block can tell "no edit" from "field dragged to this value" the same way.
    new_backfill_hours: Option<f64>,
    /// C2a Task 4: a symbol to overlay as a %-Compare series (entered/picked in the
    /// title-bar Compare popup this frame). Applied via `WinState::add_compare` after
    /// `show_window` (dedup / own-symbol-ignore / first-overlay auto-Percent live there).
    add_compare: Option<String>,
    /// C2a Task 4: a Compare overlay symbol whose chip ✕ was clicked this frame →
    /// `WinState::remove_compare`.
    remove_compare: Option<String>,
    /// C2b Task 9: a compare symbol whose "Move to own pane" chip-menu item was clicked
    /// this frame → `WinState::move_series_to_new_pane` (offered only while the symbol is
    /// still a price-pane %-overlay).
    series_to_own_pane: Option<String>,
    /// C2b Task 9: a compare symbol whose "Move to price pane (overlay)" chip-menu item
    /// was clicked this frame → `WinState::overlay_series` (offered only while the symbol
    /// is in its own pane).
    series_to_overlay: Option<String>,
    /// C2b Task 9: a compare symbol + the scale picked from its "Pin to scale" chip
    /// submenu. `ScaleAssign::Percent` ⇒ drop the pin (`series_scale.shift_remove`, back
    /// to the shared-% default); `ScaleAssign::Right` ⇒ `series_scale.insert`. (Left is
    /// deliberately NOT offered — Task 7 review: it's a silent alias of Right in the
    /// render.)
    set_series_scale: Option<(String, chart::ScaleAssign)>,
}

/// A channel whose sending half is dropped on the spot — the receiver every drain of a
/// producer-less lane now holds.
///
/// ⚠ Three `App` lanes lost their producers when the local market-data plane left this binary:
/// the backfill batch lane (`bf_rx`), its exit-report twin (`bf_done_rx`) and the cockpit's Gamma
/// token resolver (`poly_resolve_rx`). Their `Sender`s stayed on `App` as fields nothing wrote to,
/// which is exactly what `-D dead_code` refuses. Deleting the RECEIVERS too would be the honest
/// end state, but the batch lane's drain lives in `vike_app_core::ui::core_sync` and its switch
/// slots in `vike_app_core::backend::backend_conn` — a CI-roster crate, 48 sites — and that removal is the
/// market-data-wire design's to make with the whole lane, not the rename's to make halfway.
///
/// So the sender is dropped here, deliberately and visibly, rather than parked under a `_bf_tx`
/// name. What that changes at the drains: `try_recv` answers `Disconnected` instead of `Empty`.
/// Every site was read before this was written — `while let Ok(..) = rx.try_recv()`,
/// `rx.try_iter()`, `while rx.try_recv().is_ok() {}` — and each ends the same way on either
/// error, so the frame does the same nothing it did when the sender was alive and idle.
fn dead_receiver<T>() -> Receiver<T> {
    std::sync::mpsc::channel().1
}

struct App {
    charts: HashMap<String, model::ChartState>, // render model, keyed "SYMBOL@interval"
    // Global chart/clock display timezone (task A6). Applied to every `ChartState` in `charts`
    // (self-healing in `sync_from_core`, immediate on a menu change) and to the status-bar clocks.
    // Persisted in workspace.json; default `Local` == pre-feature behavior (zero visual change).
    display_tz: DisplayTz,
    spawned: HashSet<String>,
    /// Series keys whose venue has no registered feed in `feeds` right now — the observable record
    /// behind [`feed_lifecycle::FeedSlots::unroutable`] (a chart that will paint nothing until a
    /// client for its venue exists). Maintained entirely by the moved feed-lifecycle functions:
    /// inserted on a missing-venue `ensure_*`, cleared when the venue turns up, and swept by
    /// `reap_orphaned_feeds` alongside every other slot. Empty on the normal path.
    unroutable: HashSet<String>,
    /// Every subscribe leg a venue ANSWERED NO to and that has not since succeeded — the sibling
    /// record of `unroutable` for the other half of "wanted, but not actually subscribed", behind
    /// [`feed_lifecycle::FeedRetries`]. Covers every `ensure_*` site a venue can refuse (the
    /// `feed_lifecycle::RetryLane` variants are the list), each of which used to claim its
    /// idempotency slot on the FAILURE path too and so lost that stream permanently.
    /// Maintained entirely by the feed-lifecycle functions: created on failure, cleared by the
    /// retry that succeeds, and its series lane swept by `reap_orphaned_feeds`. Empty on the
    /// normal path; a non-empty entry is a chart/ladder/tape delivering nothing right now.
    feed_retries: feed_lifecycle::FeedRetries,
    hidden: HashSet<String>, // Data-manager-deleted keys: feed runs, GUI stops syncing
    core: Option<vike_core::CoreHandle>, // the vt-core single-writer runtime (R5b/R5c)
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
    active_backend: Option<vike_app_core::backend::backend_conn::BackendConn>,
    /// The loaded `backends.json` registry (split-plane B7/B8) the Connections tool's picker
    /// lists. Loaded once at startup; mutated only by the drained Backends-editor update
    /// (split-plane I2 — see `backend_editor` below), which assigns AND saves in one drain so
    /// disk and memory never disagree.
    backends: vike_app_core::backend::backend_registry::BackendsFile,
    /// The Connections tool's Backends EDITOR state machine (split-plane I2): the add/edit/
    /// delete form. Every decision lives in `vike_app_core::backend::backend_editor`; this binary only
    /// threads it into the tool body (`mem::take`/write-back, the `studio` dance) and applies
    /// the drained `RegistryUpdate` after the frame.
    backend_editor: vike_app_core::backend::backend_editor::EditorState,
    /// The Connections tool's Backend-settings section state (split-plane REQ-7, read half):
    /// `(backend addr, state)`, keyed by ADDR so a backend switch renders fresh (`Idle` ⇒
    /// auto-refetch) instead of the previous backend's rows. Written by the short-lived
    /// `spawn_backend_settings_fetch` thread, read each frame the Connections tool is visible.
    backend_settings: Arc<Mutex<(String, vike_app_core::ui::tool_views::BackendSettingsState)>>,
    /// The section's EDIT flow (REQ-7 write half): UI-thread state (its text buffers are typed
    /// into every frame), taken into a frame-local for the window loop and written back after —
    /// the pure machine lives in `vike_app_core::ui::tool_views::backend_settings`.
    backend_settings_edit: vike_app_core::ui::tool_views::SettingsEditState,
    /// One finished settings WRITE, already folded to its flow state by the worker
    /// (`settings_write_state`), keyed by backend ADDR (the fetch slot's latest-wins
    /// discipline). Written by the short-lived `spawn_backend_settings_write` thread, drained at
    /// the top of each frame.
    backend_settings_write_result:
        Arc<Mutex<Option<(String, vike_app_core::ui::tool_views::SettingsEditState)>>>,
    snap_cell: Arc<arc_swap::ArcSwap<vike_core::CoreSnapshot>>,
    last_seq: u64, // last CoreSnapshot.seq folded into `charts`
    // WHAT THE BACKEND PUBLISHES, rewritten by `sync_from_core` whenever the snapshot advances.
    // Read by three surfaces, all in `vike_app_core::ui::series_follow`: the adoption planner, the
    // symbol picker's backend section, and the title-bar badge.
    published_series: Vec<vike_app_core::ui::series_follow::PublishedSeries>,
    // venue-keyed live DataClient map ("binance", "bybit", "okx", "polymarket", …) — tick-producer
    // T5, extended for the DOM venue switcher (Bybit/OKX are depth-only entries here). Zero behavior
    // change for the pre-existing binance path: `ensure_feed_on`/unsubscribe still route through
    // the "binance" key exactly as the old single-client field did.
    // `+ Send` so the whole set can be moved onto the throwaway `vt-app-shutdown` thread and each
    // venue's teardown fanned out to run in parallel (see `on_exit`). Every concrete venue `Feeds`
    // is already `Send` (Arc/Mutex/JoinHandle-only); the bound just makes that usable off-thread.
    feeds: HashMap<&'static str, Box<dyn vike_data::DataClient + Send>>,
    // The status bar's headline handle: binance's concrete `Feeds` status on the local-core
    // path; the BACKEND bridge's connection-status handle in both observer arms (the per-venue
    // feed lines live in `feed_statuses` below there — third mode included).
    feed_status: Arc<std::sync::Mutex<String>>,
    // Per-venue live feed-status handles for the Connections tool's Status column: the same
    // human-readable `Arc<Mutex<String>>` each bridge feed writes via `set_status`, keyed by the
    // `vike_model::VENUES` slug. A superset of `feed_status` above (which stays the binance-
    // only handle the top status line reads) covering every already-producing feed — binance/bybit/
    // okx/aster/hyperliquid/polymarket. Arc clones captured from each concrete `Feeds` before it is
    // boxed into `feeds`, so this shares the live handles. Venues without a live producer are simply
    // absent and render `Unknown`.
    feed_statuses: HashMap<String, Arc<std::sync::Mutex<String>>>,
    // THE MARKET-DATA SESSION (`vike_app_core::data::md_session`): one connection to the BACKEND's
    // datahub, feeding every `feeds` entry above and writing every `feed_statuses` line. Held here
    // for the two per-frame calls only this shell can make — `set_addr` (the address is resolved
    // from `settings()` plus the ACTIVE backend's advertisement, neither of which the session can
    // see) and `tape_gaps` (whose consumer, `sync_from_core`, takes it as an input). Teardown needs
    // no handle here: every `DatahubFeed` carries a clone and `run_bounded_teardown` already fans
    // `DataClient::shutdown` over `feeds`.
    md_session: std::sync::Arc<vike_app_core::data::md_session::MdSession>,
    // best-effort tick-recording writer actor (tick-producer T3/T5); `None` when the store
    // couldn't be opened at startup — ticks still flow to the core either way (see `on_exit`'s
    // load-bearing shutdown order: feeds -> recorder -> core).
    recorder: Option<RecorderHandleTy>,
    subs: HashMap<String, vike_data::SubscriptionId>, // "{symbol}@{interval}" -> live subscription
    books: std::sync::Arc<data_sink::BookStore>,      // live L2 books, per (venue, symbol)
    // live trade tape for GUI-side tick/volume aggregation (per (venue,symbol)); drained by
    // `sync_from_core`'s per-frame fold (Task B5) into every `aggs` entry sharing that symbol.
    trades: std::sync::Arc<data_sink::TradeStore>,
    // The GUI-side bar store. ⚠ `Some` again since 2026-09-15, filled by a DIFFERENT producer: it
    // was `Some` in the THIRD MODE (fat + `--observe`) where `GuiFeedSink`'s bar lanes filled it
    // from LOCAL VENUE FEEDS, went `None` when ruling 2 removed those feeds, and now holds bars
    // read out of the BACKEND'S OWN hist store by `vike_app_core::data::store_bars` — the only way a
    // chart on an interval the daemon does not stream can paint at all. WHAT fills it is
    // `split_plane::BarPlane`, an explicit value, no longer this field's `is_some()`.
    direct_bars: Option<std::sync::Arc<data_sink::DirectBarStore>>,
    // Chart keys ALREADY asked of the backend's store this session (`store_bars::plan_store_reads`'
    // `asked`). Inserted BEFORE the read is spawned, which is what makes "once" hold across the
    // flight rather than only across the return — an empty chart stays empty while the read is in
    // the air, so without it every frame would re-ask. Cleared on a backend switch beside the
    // store itself (`apply_backend_action`): another backend is another store.
    store_asked: HashSet<String>,
    // The one sentence the CHART-SEED gap arm leaves for the operator
    // (`vike_app_core::data::chart_seed::render_chart_seed_status`), written by the background read
    // thread and painted under the empty-plot hint. `None` = nothing worth saying. It is a shared
    // slot rather than a channel because there is exactly one current answer, not a log: a stale
    // "set VIKE_DATAHUB_CHART_SEED=1" after the operator has set it would be worse than silence,
    // and `SeedDial::run` overwrites on every batch for that reason. Cleared with `store_asked` on
    // a backend switch — another backend is another server with its own configuration.
    chart_seed_note: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    // Last `DirectBarStore::generation` folded into `charts` — `last_seq`'s store-side twin, a
    // pure monotonic fold cursor (never reset; see `CoreSyncState::last_direct_gen`).
    last_direct_gen: u64,
    // Client-side tick/volume aggregators (Task B5), keyed by chart key "SYM@100t"/"SYM@10v" (the
    // same keyspace as `charts`) -> (venue, symbol, aggregator). `ensure_feed_on`'s Tick/Volume
    // branch creates an entry (once) per chart key; `sync_from_core` feeds it from the drained
    // trade tape (venue-aware, Task: venue-aware tick/vol) and syncs the matching `charts` entry
    // from its `closed`/`forming` output — the tick/volume twin of the venue-kline fold.
    aggs: HashMap<String, (String, String, tickvol::TickVolAgg)>,
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
    of_aggs: HashMap<String, (String, String, bar_agg::OrderflowAgg)>,
    // SP3 Task 3: background aggTrades backfill — pages strictly OLDER than the live trades
    // feed's warmup and feeds the SAME `of_aggs` entries, so CVD/profile fill across history
    // instead of starting empty at whatever moment orderflow was toggled on. GLOBAL (one knob
    // for every symbol, not per-window — see `workspace::persist::Workspace::of_backfill_hours`'s
    // doc for why global is simpler); `0.0` is the SP2-identical off-switch (no thread ever
    // spawned — `global-constraints.md`'s "default = SP2 behavior").
    of_backfill_hours: f64,
    /// Symbols whose backfill thread has already been spawned (SP3 T3) — the run-once gate:
    /// re-enabling orderflow on an already-spawned symbol does NOT refetch. Insert-only; nothing
    /// clears it, so a batch this process drops can never be re-fetched — which is why the drain
    /// in `vike_app_core::ui::core_sync` STAGES an undeliverable batch (`bf_pending`) instead of
    /// discarding it. Correct as-is; do not "fix" it by clearing entries here — the ONE sanctioned
    /// way a symbol walks again is `bf_retries` below, which reopens the gate WITHOUT removing
    /// anything from this set.
    bf_spawned: HashSet<String>,
    /// Symbols whose backfill walk reported a RETRYABLE stop (#952's `BackfillStop::Failed`) — the
    /// lane that reopens `bf_spawned` on a bounded backoff. Owned here purely because it is
    /// per-`App` state; the whole policy (when, how often, how many times, what is logged, and the
    /// no-double-count rule that keeps a delivering walk from ever re-running) lives in
    /// `vike_app_core::ui::feed_lifecycle::BackfillRetries`, which is where its tests are.
    bf_retries: feed_lifecycle::BackfillRetries,
    /// ⚠ A RECEIVER WITH NO SENDER — see [`dead_receiver`]. Backfill worker threads (SP3 T3,
    /// spawned from `maybe_spawn_backfill`) used to push completed batches here, drained every
    /// frame in `sync_from_core` before the live trade drain. The workers left with the local
    /// market-data plane and `maybe_spawn_backfill` is an empty stub, so this end is kept only
    /// because the drain in `vike_app_core::ui::core_sync` still takes it by reference; every
    /// `try_recv`/`try_iter` on it answers `Disconnected`, which that drain treats exactly as
    /// `Empty`. Removing the whole lane (this, `bf_done_rx`, `bf_pending`, the staging in
    /// `core_sync` and the switch slots in `backend_conn`) is the market-data-wire design's job,
    /// not the rename's: it is 48 sites across a CI-roster crate.
    bf_rx: Receiver<(String, Vec<vike_model::TradeTick>)>,
    /// ⚠ A RECEIVER WITH NO SENDER — same story as `bf_rx`. The workers' EXIT report — one
    /// `BackfillReport` per walk, sent on every exit path so a worker could never leave its symbol
    /// stuck in-flight — drained on the UI thread in `update` and folded into `bf_retries`. A
    /// SECOND channel rather than a variant on the batch lane because the two needed no ordering
    /// between them. Nothing sends on it any more.
    bf_done_rx: Receiver<feed_lifecycle::BackfillReport>,
    /// Backfill batches drained off `bf_rx` at a frame where the symbol had NO `of_aggs` entry to
    /// land in — held per symbol until one exists, instead of being discarded. Owned here purely
    /// because it is per-`App` state; the whole lane (staging, the cap, the warn, delivery) lives
    /// in `vike_app_core::ui::core_sync`, which is where its tests are. Empty in steady state.
    bf_pending: HashMap<String, Vec<vike_model::TradeTick>>,
    /// The live Binance trades feed's shared earliest-emitted-aggTrade-id-per-symbol map (SP3
    /// T2's `market_feed::Feeds::earliest_live_ids`), cloned once at construction — BEFORE
    /// `binance_feed` is boxed into `feeds` (see `App::new`). Each backfill worker polls this
    /// for its symbol's strictly-older paging boundary (`global-constraints.md`'s
    /// no-double-count invariant: backfill ids must stay `< min_live_id`).
    earliest_live_ids: Arc<std::sync::Mutex<HashMap<String, u64>>>,
    /// `(venue, venue-native symbol)` with a depth stream running — the Trade windows' dedupe
    /// LEDGER, each entry keeping the subscription ids `ensure_depth` minted
    /// (`feed_lifecycle::TradeDepthSubs`) so the per-frame reaper stops the streams when no Trade
    /// window names that book any more.
    trade_depth: HashMap<(String, String), feed_lifecycle::TradeDepthSubs>,
    /// Polymarket token-ids with a live cockpit book+trade stream running — the cockpit's analog
    /// of `trade_depth`, same ledger shape (`feed_lifecycle::PolyBookSubs`), reaped by the same
    /// per-frame call. Subscribed via `ensure_poly_book`.
    poly_subs: HashMap<String, feed_lifecycle::PolyBookSubs>,
    /// ⚠ A RECEIVER WITH NO SENDER — same story as `bf_rx`. Background Gamma token-resolution
    /// results — `(window id, resolved YES token-id, short market name)`. A resolver thread spawned
    /// on cockpit-window-open used to send here; `update` still drains it, assigns the token to
    /// the still-placeholder window's `symbol`, and caches the name in `poly_names` below — and
    /// since `spawn_poly_token_resolver` is an empty stub, that drain sees `Disconnected` on its
    /// first `try_recv` and does nothing. A cockpit window therefore stays on its placeholder
    /// token until resolution moves behind the backend.
    poly_resolve_rx: Receiver<(egui::Id, String, String)>,
    /// Resolved Polymarket market SHORT NAMES, keyed by YES token-id (e.g.
    /// `1071…2933` → `"Bitcoin Up/Down"`). Populated by the Gamma-resolver drain in `update`
    /// (alongside the `symbol` assignment); passed to `tool_views::cockpit_tool_content` (through
    /// `ToolCtx::poly_names`) to label the chain-rail + probability-ladder header with the real
    /// market name instead of the elided token-id. A token with no entry — the seeded
    /// `VIKE_POLY_COCKPIT_TOKEN` case, where no Gamma resolve ran — falls back to
    /// `poly_labels::poly_short_label`.
    poly_names: HashMap<String, String>,
    live_venues: HashSet<String>, // venues with a credential-gated LIVE exec client (else paper)
    // Raised BEFORE core shutdown so the live-event forwarder drains-and-drops (no teardown deadlock).
    forwarder_stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Unified shutdown signal (app-owned) — the single "we are tearing down" flag. Raised once,
    /// either when the window-close is first observed in `ui` or at the top of `on_exit`, and never
    /// cleared. INERT on the running path: it is only ever READ during teardown — `ensure_feed_on`
    /// skips starting a NEW live feed once it is set, so no fresh blocking socket read is opened
    /// while `on_exit`'s bounded teardown runs. Never read in the vike-core hot fold (the p99<10µs
    /// gate is untouched). See `on_exit` for the bounded, parallel teardown it gates.
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Reconcile driver. ⚠ ALWAYS `None` now: it was `Some` once `vike_core::spawn_recon` was
    /// mounted, which needed a LOCAL mount arming a live venue account (the S2 gate,
    /// `vike_tradehub::reconcile_config::reconcile_gate`) and at least one live venue's `ReconClient`.
    /// This process mounts no venue, so it reconciles nothing — the daemon is the only reconciling
    /// root left. Still shut down in `on_exit` alongside `forwarder_stop`, which is now a no-op.
    recon_driver: Option<vike_core::ReconDriver>,
    /// The B11 live-account lock guards (`vike_ops::live_lock`) — one per venue a mount ARMS, held
    /// (never read) for the app's lifetime so a second live process on the same account is refused
    /// for exactly as long as this one can trade.
    ///
    /// ⚠ ALWAYS EMPTY now, and the ARGUMENT for it is worth keeping even so: the set was
    /// `vike_mount::armed_live_venues`, claimed one statement ABOVE `build_node` and deliberately NOT
    /// the [`Self::live_venues`] record beside it. Those two answer the same question at different
    /// times, and the time was the whole point — `live_venues` does not exist until every exec
    /// client has been constructed, and three venues post `set_leverage` while constructing one, so
    /// a lock refused from that set is refused after this process has already changed account
    /// state. The claim site is gone with the mount (see the tombstone in `App::new`); anything
    /// that brings a mount back has to bring that ordering back with it.
    _live_locks: Vec<vike_ops::live_lock::LiveLock>,
    // The B6 `_observe_bridge` stop handle folded into `active_backend` above (split-plane B1):
    // the bridge now lives inside `BackendConn`, whose replacement/drop stops-and-joins it.
    /// Live journal materializer (unified-journaling #2). ⚠ ALWAYS `None` now: it was spawned only
    /// when a LOCAL core's WAL was enabled (VIKE_JOURNAL_DIR / profile), to drain that WAL's fills
    /// into the Tier-2 exec-fill log off the fold, and this process has no local core — so no WAL to
    /// read (the tombstone at its binding in `App::new` says so). No production root spawns one
    /// today: the daemon's `materialize` feature went too, on 2026-09-22 (#2093). Still taken in
    /// `on_exit`'s bounded teardown, where a `None` shuts nothing down. (This described a live
    /// spawn-and-drain, in the present tense, until 2026-09-28.)
    materializer: Option<vike_journal::materialize::MaterializerHandle>,
    /// QA ([`TRADE_SEED_ENV`]): submit the two PAPER orders
    /// [`vike_app_core::ui::capture_seed::plan_trade_seed`] mints — one resting limit, one market —
    /// so an Account-window capture has a working order and an open position instead of empty
    /// tables. Cleared on the frame that submits them, so it is a ONE-SHOT; a frame that could not
    /// act (no dispatch, no price yet) leaves it armed and re-tries.
    trade_seed_pending: bool,
    /// The instrument of the Trade window the trader last acted in (spec §3.11): a new Trade
    /// window opens on it. `None` until one acted; the window loop's `TradeFrame::remember` writes
    /// it, never from every window every frame (minor 26).
    last_trade: Option<tool_views::TradePick>,
    /// The ACTIVE backend's `Directory` reply (venue names, the account list), kept by the
    /// throwaway fetch thread `spawn_directory_fetch` starts — `tool_views::DirectorySlot`.
    directory: Arc<Mutex<tool_views::DirectorySlot>>,
    /// QA ([`CHART_DRAW_ENV`]): write the two capture drawings
    /// [`vike_app_core::ui::capture_seed::trendline_overlay`] derives into every chart's
    /// `ChartState::overlays`, once the series has bars. Same one-shot/re-try shape as
    /// [`Self::trade_seed_pending`].
    chart_draw_pending: bool,
    wins: Vec<workspace::WinState>,
    tool_views: HashMap<egui::Id, tools::ToolView>, // per-tool-window view state, keyed by WinState.id
    /// Chart sync groups (task B8): double-buffered per-frame registry, one [`GroupFrame`]
    /// per group (1..=4). `sync_prev` is what THIS frame's grouped chart windows read (via
    /// [`sync_feed`]) to build their `vike_chart::ChartInputs::sync`; `sync_next` is written
    /// while walking `wins` this frame (via [`sync_harvest`], after each window's
    /// `chart::draw`) and becomes next frame's `sync_prev` via the top-of-`ui` swap.
    sync_prev: HashMap<u8, GroupFrame>,
    sync_next: HashMap<u8, GroupFrame>,
    /// Sticky range-leader per sync group (task B8): the last window id that had a live
    /// local interaction (drag/zoom/nav) in that group. Only the leader's `visible_ts`
    /// propagates into `sync_next` — see [`sync_harvest`]'s doc for why that's what makes
    /// followers track a leader's live edge instead of freezing.
    range_leader: HashMap<u8, u64>,
    status: String,
    next_win_n: u32,
    show_rail: bool,
    desktop: egui::Rect,
    did_initial_arrange: bool,
    last_arrange: Option<workspace::Arrange>, // last tiling mode, re-applied on app maximize
    was_maximized: bool,                      // edge-detect the maximize/restore transition
    retile_frames: u8,                        // re-tile for a few frames while the desktop settles
    tools: std::sync::Arc<std::sync::Mutex<tools::ToolData>>,
    /// Options poll-thread wake channel (from `spawn_tool_fetchers`): the Options tool's Refresh
    /// pill sends `()` here to force an immediate chain re-poll instead of waiting out the 30s
    /// cadence. `try_send`/`send` errors are ignored (a full/closed channel must never panic).
    opt_refresh: std::sync::mpsc::Sender<()>,
    datasets: datasets::Store,
    palette: String,
    shot_n: u32, // frame counter for the headless self-screenshot (VIKE_SHOT)
    // File -> Export chart image…: set when the user picks the menu action; the next frame
    // requests egui's framebuffer screenshot and, once the readback event arrives, writes a
    // PNG (same capture path as VIKE_SHOT). `export_seq` disambiguates same-second filenames.
    export_pending: bool,
    export_seq: u32,
    // File -> Save layout as… (TradingView-style named layouts): when `layout_name_prompt` is
    // set the app shows a small modal name-entry field; `layout_name_input` holds the in-progress
    // name. On confirm, `workspace::persist::save_layout` writes `layouts/<sanitized>.json`. Load
    // and Delete are driven directly from the menu submenus (no dialog needed) — see the menu
    // action arms below.
    layout_name_prompt: bool,
    layout_name_input: String,
    layout_prompt_focus: bool, // one-shot: focus the name field the frame the dialog opens
    flags: HashMap<String, egui::TextureHandle>, // real country flags (flagcdn PNGs)
    flag_rx: Receiver<(String, egui::ColorImage)>,
    news_logos: HashMap<String, egui::TextureHandle>, // provider favicons (keyed by source name)
    logo_rx: Receiver<(String, egui::ColorImage)>,
    launcher_icons: HashMap<String, egui::TextureHandle>, // vike's vector launcher icons (PNG)
    // Searchable CROSS-VENUE instrument catalog (`vike_catalog::Catalog` over the live Binance +
    // Bybit + OKX keyless public instrument universes, each `Instrument` venue-tagged and
    // asset-class-tagged), fetched ONCE by a background thread at startup and delivered over
    // `catalog_rx`. Held as an `Arc` so the per-window title-bar loop clones it cheaply each frame
    // (never a deep copy). Empty (`Catalog::from_instruments(Vec::new())`) until the fetch lands —
    // the picker falls back to the built-in `SYMS` quick-picks meanwhile; a per-venue fetch failure
    // just contributes nothing (the other venues still populate). Same channel+Arc idiom as the
    // flag/logo rx. The picker ranks/filters via `Catalog::search(query, &SearchFilter, limit)`.
    symbols_catalog: Arc<vike_catalog::Catalog>,
    catalog_rx: Receiver<vike_catalog::Catalog>,
    /// **Who OWNS that catalog** — the on-disk cache, the per-venue refresh stamps, and the
    /// explicit per-venue refresh the Data Manager's Instruments destination triggers
    /// (`vike_app_core::data::catalog_refresh`). It holds the SENDER half of `catalog_rx`, so a refresh
    /// lands in `symbols_catalog` through the drain that already existed.
    ///
    /// ⚠ It REPLACES `spawn_catalog_fetcher`, which fetched every start and cached nothing — the
    /// policy `crates/vike-catalog/src/persist.rs` has documented since the cache was written
    /// ("rewritten only on an explicit user refresh") had no implementation until this field.
    catalog: Arc<vike_app_core::data::catalog_refresh::CatalogRefresh>,
    /// GPU candle layer availability (GPU Phase 2). `true` once `chart_gpu::CandlePipeline` is
    /// registered in the wgpu `callback_resources` at startup; `false` if the wgpu backend is
    /// absent or the pipeline build failed (degrade to the egui candle painter). The Task 3
    /// render toggle (`gpu_render`) is ANDed with this — a user can only turn GPU rendering ON
    /// when it's actually available.
    gpu_ok: bool,
    /// GPU candle layer user toggle (GPU Phase 2, Task 3). Default `false` == pre-Task-3
    /// behavior (the egui/LOD candle painter, byte-identical). Persisted in workspace.json
    /// alongside `display_tz`/`of_backfill_hours` (GLOBAL, not per-window — same rationale as
    /// `of_backfill_hours`'s doc: one knob, not per-chart). The `chart::draw` call site only
    /// wires `ChartInputs::gpu_candles` when BOTH this and `gpu_ok` are true.
    gpu_render: bool,
    /// Indicator favourites (feature part b): the user's ⭐-starred indicator registry keys, GLOBAL
    /// (shared by every chart's ƒx picker — not per-window) and persisted in the workspace file
    /// alongside `display_tz`/`gpu_render` (see `workspace::persist::Workspace::indicator_favs`).
    /// EMPTY (the default) means no Favourites section in the picker — byte-identical to today. The
    /// picker stars/unstars into this `Vec` directly (order = star order = display order).
    indicator_favs: Vec<String>,
    /// The appearance this process runs and the Settings window's pending change —
    /// `vike_app_core::ui::appearance_settings::AppearanceSession`, which applies a change once the
    /// frame's windows have drawn and saves it as a row of THIS computer's settings database.
    appearance: AppearanceSession,
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
    studio: Option<vike_studio::StudioState>,
    /// Sticky failure reason from the one-shot `open_studio_store` attempt. `None` means either
    /// "not tried yet" or "already succeeded" (see `studio`); set at most once, so a broken store
    /// (e.g. permission denied) degrades to a static label rather than retrying the (expensive)
    /// open every single frame the Studio window stays visible.
    studio_error: Option<String>,
    /// Data Manager "Stored" view: the local `HistStore`'s manifest-derived inventory tree
    /// (venue → symbol → series rollups), fetched off-thread — the first time the Stored tab is
    /// shown, and again on every Refresh click (see `refresh_stored`). Manual-refresh only, no
    /// per-frame polling. `Arc` so the window loop can read it without holding `self` mutably
    /// (same idiom as `symbols_catalog`); empty until the first load lands.
    stored_tree: Arc<Vec<vike_data_manager::model::VenueNode>>,
    /// Per-series missing-data gap ranges (`DataFusionHist::series_gaps`, inclusive epoch-ms),
    /// keyed by the same [`vike_data_manager::SeriesKey`] identity as `stored_tree`'s rows —
    /// fetched in the same off-thread `refresh_stored` load (see that fn's doc), never per-frame.
    /// Feeds `stored_catalog_grid`'s coverage-bar gap paint + its `HasGaps` smart-view filter.
    /// `Arc` for the same read-only-in-the-window-loop reason as `stored_tree`. Series with no
    /// gaps are omitted (kept small); an absent key reads as "no known gaps".
    stored_gaps: Arc<vike_data_manager::GapMap>,
    /// Per-INSTRUMENT cross-kind partial days (`HistStore::coverage_report` folded through
    /// [`vike_data_manager::partial_days_from_coverage`]), loaded on the same off-thread pass as
    /// `stored_gaps`. The cross-kind sibling of that map: it feeds the grid's Partial column, which
    /// answers a question no per-series view can — "are some of this instrument's kinds missing days
    /// the others have". Instruments with nothing partial are omitted.
    stored_partials: Arc<vike_data_manager::PartialDayMap>,
    /// What the LAST `refresh_stored` load learned about the remote store's coverage verb (spec
    /// §6-Q2). It is a NEGOTIATED fact — only a dial can produce it — so it is remembered here and
    /// fed back into `stored_mode` on every render, which is what lets the Partial column show the
    /// wire answer against a current server and the honest note against an older one. Starts
    /// `Unknown` (nothing dialled yet); the local arm never changes it, and `stored_mode` ignores
    /// it in local mode.
    stored_coverage: vike_app_core::data::stored_mode::RemoteCoverage,
    /// What the LAST load got from the datahub's history-channels read (decision 0102) — the Data
    /// Manager's HISTORY column and the bulk Backfill's lookback floors. `None` until a load answers
    /// it. Read straight off `App` by the window loop, so no frame snapshot copies it.
    stored_history: Option<Arc<vike_app_core::data::history_column::HistoryLoad>>,
    /// `Some(reason)` → the LAST load could not READ the store, and this is what to show instead of
    /// a bare empty grid. `None` → the tree is the store's real answer, INCLUDING when that answer
    /// is "nothing recorded".
    ///
    /// ⚠ The two used to be the same picture, and that was half the defect: an unreachable datahub
    /// and a genuinely empty store both drew an empty grid, so the screen could not tell an
    /// operator which one they were looking at. Cleared when a new load starts (see
    /// `refresh_stored`), so a retry never shows the previous attempt's reason under a fresh
    /// spinner.
    stored_error: Option<String>,
    stored_rx: Receiver<StoredLoad>,
    stored_tx: std::sync::mpsc::Sender<StoredLoad>,
    /// `true` while a background `refresh_stored` load is in flight — drives the Stored tab's
    /// "Loading…" label and disables a second concurrent Refresh click.
    stored_loading: bool,
    /// dm-bulk-backfill: `true` while a background `maybe_spawn_stored_backfill` run is in
    /// flight — the run-once-at-a-time gate (a second bulk click while one is running is a no-op,
    /// see that method's doc), same shape as `stored_loading`.
    stored_backfill_running: bool,
    /// dm-bulk-backfill: the last backfill run's human-readable status line ("Backfilling 3
    /// series (1 skipped)…" while running, then a final "N backfilled, M failed, K skipped"),
    /// rendered in `tool_views::stored_tool_content` next to the Refresh/Delete row.
    stored_backfill_status: String,
    stored_backfill_rx: Receiver<String>,
    stored_backfill_tx: std::sync::mpsc::Sender<String>,
}

/// vike's launcher row, left→right, with each tool's _DRAW icon name + (functional WinKind).
/// The bundled PNGs are rendered from Python's icons.py (exact line-art + TOOL_COLORS), except
/// chart, account, trade, polymarket and greeks: those five were redrawn in-house as 48x48 SVGs on
/// 2026-10-05 (two candles, a person, a bolt, a pie with a slice, a delta triangle) and rasterised
/// to transparent 8-bit RGBA PNGs of the same size.
const LAUNCHERS: &[(&str, &[u8], Option<workspace::WinKind>)] = &[
    ("chart", include_bytes!("../../../assets/icons/chart.png"), Some(workspace::WinKind::Chart)),
    // The old Trade slot opens the Account window (equity, accounts, working orders) and the old
    // DOM slot the new Trade window (spec §3.10, pre-flight C4). Each has its own icon; the name is
    // the hover text and the texture key.
    (
        "account",
        include_bytes!("../../../assets/icons/account.png"),
        Some(workspace::WinKind::Account),
    ),
    ("trade", include_bytes!("../../../assets/icons/trade.png"), Some(workspace::WinKind::Trade)),
    // Polymarket scalp cockpit — its own pie-with-a-slice icon.
    (
        "polymarket",
        include_bytes!("../../../assets/icons/polymarket.png"),
        Some(workspace::WinKind::Polymarket),
    ),
    (
        "studio",
        include_bytes!("../../../assets/icons/studio.png"),
        Some(workspace::WinKind::Studio),
    ),
    ("screener", include_bytes!("../../../assets/icons/screener.png"), None),
    // The journal-book glyph opens the live Tearsheet (performance summary read back from the
    // command journal this session writes when VIKE_JOURNAL_DIR / VIKE_RUN_PROFILE enable it).
    (
        "journal",
        include_bytes!("../../../assets/icons/journal.png"),
        Some(workspace::WinKind::Tearsheet),
    ),
    ("alerts", include_bytes!("../../../assets/icons/alerts.png"), None),
    ("data", include_bytes!("../../../assets/icons/data.png"), Some(workspace::WinKind::Data)),
    // ⚠ REMOVED 2026-10-05 (final-review I3): the standalone Connections window
    // (`WinKind::Connections`) is deleted outright, and this row used to open `WinKind::Data` too
    // — a byte-identical duplicate of the "data" row above it, since the toolbar's generic
    // launcher dispatch (`app_ui.rs`'s `draw_caption`) resolves a click to a `WinKind` alone and
    // cannot tell two same-kind rows apart, so there was no way to land THIS one on
    // `DataDest::Credentials` without the other. No other row in this table duplicates another
    // row's kind; removing this one restores that convention rather than breaking it. The
    // `connections.png` icon asset is unreferenced now. The Trade ticket's Connect click, which
    // genuinely always means "go to Credentials" and has no such ambiguity (one call site, one
    // destination), keeps working — see `apply_trade_frame`'s `SpawnRequest::Kind`
    // `data_dest_seed`. `VIKE_SHOT_WIN=connections` / `VIKE_TOOL=connections` are UNRELATED QA/env
    // spellings resolved in `crate::ui::startup`/`crate::ui::initial_arrange`, not this table, and
    // this removal changes neither (see those modules for their own current behaviour).
    ("news", include_bytes!("../../../assets/icons/news.png"), Some(workspace::WinKind::News)),
    (
        "calendar",
        include_bytes!("../../../assets/icons/calendar.png"),
        Some(workspace::WinKind::Calendar),
    ),
    (
        "options",
        include_bytes!("../../../assets/icons/options.png"),
        Some(workspace::WinKind::Options),
    ),
    // Greeks: the delta triangle (the options row above keeps its own line-art).
    (
        "greeks",
        include_bytes!("../../../assets/icons/greeks.png"),
        Some(workspace::WinKind::Greeks),
    ),
];

// ⚠ TOMBSTONE — `tick_store_root`, `counters_file_path` and `state_dir_path` stood here, and all
// three went with the local core (rulings 1 and 2). Each resolved a path this process WROTE to:
// the tick-recording store the `RecorderSink` filled, the mmap health-counters file `vike_stat`
// reads live, and the strategy-state sidecar directory every mounted strategy saved a
// `<mount_id>.json` into. The desktop mounts no strategy, records no tape and runs no core, so all
// three had zero callers — dead by CALLER rather than by text, which is why they survived the cut
// that removed their consumers and had to be deleted deliberately afterwards.
//
// ⚠ THEY TOOK THREE ENVIRONMENT READS WITH THEM — `VIKE_TICK_STORE`, `VIKE_COUNTERS_FILE` and
// `VIKE_STATE_DIR` — and that is a registry fact, not a comment: `vike_ops::settings::SETTINGS`
// keys a row on `(name, krate)` with the crate DERIVED FROM THE PATH, so a row left behind here
// fails `every_declared_variable_is_read` exactly as loudly as a missing one fails the other
// direction. The first two rows are deleted (`vike-core` and `vike-tradehub` still declare those
// names for their own readers, so neither VARIABLE is undeclared); `VIKE_STATE_DIR` keeps its
// `vike-config` row, because that loader genuinely still looks the name up.
//
// The resolution LADDERS survive where they always lived and are not lost with the callers:
// `crates/vike-model/src/paths/tick_store_path.rs`'s `resolve_tick_store_root` is the shared home the
// daemon still uses, and `crates/vike-tradehub/src/tradehub_cli.rs` is the binary that still writes
// a tape. Recording is the recorder daemon's job now, not the desktop's.

/// **This executable's own directory, spelled ONCE.** `None` when `current_exe()` fails — a process
/// that cannot read its own path — which the caller needing a total answer spells
/// `.unwrap_or_default()`, the empty relative path the hand-rolled copies of this expression
/// produced in that case.
///
/// ⚠ It was a helper because this file carried the same
/// `current_exe().ok().and_then(|p| p.parent()…)` incantation FOUR times. Three of those callers —
/// `tick_store_root`, `counters_file_path` and `state_dir_path` — went with the local core (the
/// tombstone above), so [`export_chart_png`] is the only one left. It stays a function rather than
/// being inlined back into that one call site: the argument that put it here was that a fourth
/// hand-rolled copy is how the third one's semantics start to drift, and a single caller today does
/// not make the next copy any safer.
fn exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

// ⚠ TOMBSTONE — `open_tick_recorder` stood here (tick-producer T5). It opened the
// DataFusion+Parquet store at the tick-store root and spawned the `vike_data::RecorderSink` writer
// actor, which persisted every live quote/trade/book this process saw; on failure `App::new` fell
// back to the bare `CoreSinkAdapter` so ticks still reached the core. Both ends are gone: there are
// no venue feeds producing ticks (ruling 2) and no local core to reach, and `RecorderSink` /
// `RecorderHandle` are `vike-data/hist-datafusion` exports this crate no longer enables. Recording
// live tape is the recorder daemon's job (`vike-recorder`), not the desktop's.

// Unconditional since split-plane I7 (was `fat`-gated): a REMOTE Studio session still needs this
// root as its `state_dir` (saved strategies live beside the local store path even when the bars
// come over the wire — see the Studio arm's `new_with_qa` call), and nothing here names DataFusion.
/// Resolve the Studio tool's bar-data store root: `$VIKE_HIST_STORE` else `<repo>/market_data/hist` —
/// mirrors `vike-backtest`'s `backtest` bin / `vike-backfill`'s backfill bins' `store_root`
/// convention exactly (see `crates/vike-backtest/src/backtest_cli.rs`), so the Studio browses the
/// SAME store those tools populate. Deliberately NOT the live-TICK store — which
/// is now a SIBLING of this one inside the same `<project>/market_data/` folder (`market_data/ticks` beside
/// `market_data/hist`, the layout `vike_model::paths::state_path::PROJECT_DATA_DIR` commits to) rather than the
/// `<exe_dir>`-relative path this line used to name: that one only ever receives recorded
/// quotes/trades/books (`RecorderSink`'s
/// bar-seam methods — `seed_bars`/`close_bar`/`forming_bar` — are no-ops, see
/// `vike_data::rec::live_rec`), so it never has the `kind=bar` series the Studio's `SlicePicker` looks
/// for; the backfill/harness convention is the one that actually has bars in it.
fn studio_store_root() -> std::path::PathBuf {
    // ⚠ The dev-checkout rung is a DEBUG-BUILD rung. `env!("CARGO_MANIFEST_DIR")` is a string
    // literal in the binary, and `release.yml` builds this crate `--release` on a runner whose
    // checkout path names the runner account — so the `vike-app-fat`/`-thin`/`.exe` assets carried
    // that path into every public download, and `scripts/refuse_box_paths.sh` (the release's
    // box-path guard) refused them by name. A release build therefore passes NO rung and resolves
    // from the project walk down, which for anyone standing in a checkout is the same directory by
    // a different rung name; `vike_model::paths::store_path`'s module doc (rung 4) carries the argument.
    // The attribute, not `cfg!()`: a runtime `if` would still compile the literal in.
    #[cfg(debug_assertions)]
    let repo_default = Some(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("market_data")
            .join("hist"),
    );
    #[cfg(not(debug_assertions))]
    let repo_default: Option<std::path::PathBuf> = None;
    // Shared precedence: the compile-time repo path is only meaningful on the machine that BUILT
    // this binary, so an installed app falls through to the PROJECT's own `<project>/market_data/hist`
    // (the same walk that finds `settings/`), and only a binary with no project above it reaches
    // the per-user `…/vike-data`. The platform trio (`XDG_DATA_HOME`/`HOME`/`LOCALAPPDATA`) is
    // spelled in exactly ONE file workspace-wide — `vike_model::paths::store_path` — and reaches it
    // through the environment MAP this binary collects, the same `std::env::vars()` sweep
    // `App::new` already builds elsewhere. The project walk likewise stays in
    // `vike_model::paths::state_path`; this binary only supplies the working directory it starts from.
    //
    // The store root itself now comes from `config.store_root`, still overridden by
    // `VIKE_HIST_STORE` inside the loader — so `resolve_store_root`'s second argument carries the
    // resolved answer rather than a raw variable, and the `config.store_root` row an operator sets
    // finally reaches the Studio. ⚠ The OTHER `VIKE_HIST_STORE` readers are unchanged and still
    // environment-only: vike-datahub's bin, and the map-taking `vike_backtest::binutil::store_root`
    // / `vike_backfill::cli::store_root` (see `vike_config::CONSUMPTION`).
    //
    // ⚠ `resolve_store_root_from`, never the bare `resolve_store_root` ladder: that one's project
    // and per-user defaults are two adjacent `Option<PathBuf>`s, so transposing them here would
    // COMPILE SILENTLY and browse the wrong store. This form has no two arguments of the same type,
    // and `$VIKE_SETTINGS_DIR` reaches the project walk through the same map.
    let cwd = std::env::current_dir().ok();
    let resolved = vike_model::paths::store_path::resolve_store_root_from(
        None,
        settings().config.store_root.as_ref().map(|p| p.display().to_string()),
        repo_default.as_deref(),
        cwd.as_deref(),
        &std::env::vars().collect(),
    );
    // Which root, and by which rung. The Studio silently browsing a DIFFERENT store than the one
    // the backfill bins populate is precisely the confusion this whole precedence exists to end,
    // and it presents as an empty `SlicePicker` rather than as an error.
    tracing::info!(
        store = %resolved.root.display(),
        rung = resolved.rung.as_str(),
        "studio hist store root resolved: {}",
        resolved.rung.why()
    );
    resolved.into_path()
}

// ⚠ TOMBSTONE — `open_local_hist_store` stood here. It opened the LOCAL `DataFusionHist` store at
// [`studio_store_root`] and was the CONCRETE handle the Data-Manager arms needed, because
// `coverage_report`, `delete_series` and the backfill writers are not `HistStore` trait verbs. Its
// four callers went with it: `open_studio_store`'s local arm, `App::refresh_stored`'s local
// inventory load, `App::spawn_stored_backfill_local`, and the Stored view's per-series Delete. The
// desktop takes vike-data with DEFAULT features now — the trait-only seam — because
// `vike-data/hist-datafusion` was a `fat` enable and `fat` is gone (rulings 1 and 2 took the local
// data plane with the local trading core). Everything the Data Manager still does goes over the
// wire, through `vike_datahub_client::RemoteHistStore`.

/// The datahub STORE handle every hist-plane read dials through — **authenticated** when this
/// desktop holds a datahub observe key.
///
/// ⚠ **This exists because the store plane was the half that never got wired.**
/// `crate::backend_registry`'s `datahub_observe_keys` doc records that before it landed, nothing in
/// `vike-app-core` or `vike-desktop` called `node_keys_from_vars`, `RemoteHistStore::with_keys` or
/// `DatahubClient::connect_authed`, and "every dial from the desktop was UNAUTHENTICATED". The
/// MARKET-DATA half was then fixed — `md_session`'s `connect_with` calls `connect_authed` — and the
/// STORE half was not: both `RemoteHistStore::new` call sites stayed on the key-less constructor,
/// so `with_keys` had no production caller at all.
///
/// What that cost, measured against a real deployed server: the Data Manager's Stored tab read
/// `No stored data · 0 rows · 0 B` from a datahub that answered the very same address with five
/// series and 7.49 billion rows seconds earlier on the CLI, and the log said why — *"this datahub
/// REQUIRES authentication (it advertises the `auth` feature) but no node keys were supplied"*.
/// Studio failed the same way through its own `Err`.
///
/// ⚠ **The severity is structural, not situational.** `vike_datahub_client::bind::bind_decision`
/// REFUSES to start a datahub bound to a non-loopback address without keys, so every datahub
/// reachable off-box is necessarily authenticated — which made the GUI's whole store plane
/// unusable against every remote deployment, by construction, while the local dev case kept working
/// and hid it.
///
/// ⚠ **There is no configuration in which the key-less constructor is the better choice**, which is
/// why this helper has no "plain" arm to pick: `RemoteHistStore::with_keys`' own contract is that
/// against a KEY-LESS server it degrades to an ordinary unauthenticated connect, "so one configured
/// GUI works against both a keyed production datahub and a local dev one". The `None` arm below is
/// reached only when this desktop holds NO key to offer, never as a preference.
/// ⚠ **The BODY moved to `vike_app_core::backend::backend_registry::remote_hist_store`** — beside
/// `datahub_observe_keys`, whose resolution is the hard half — so the merge gate compiles and
/// tests it and so the chart's own store read
/// (`vike_app_core::data::store_bars`) dials identically without importing this binary. What is left
/// here is the two composition-root facts a library may not read for itself.
fn remote_hist_store(addr: String, key_name: &str) -> vike_datahub_client::RemoteHistStore {
    vike_app_core::backend::backend_registry::remote_hist_store(
        addr,
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
        // ⚠ `PROCESS_ENV`, NOT `workspace_credentials()` — the same distinction the market-data
        // mount states at its own call site: the VENUE store would miss a key set only in the
        // environment and would find a legacy one SILENTLY, skipping the decision-0051 notice.
        PROCESS_ENV.get_or_init(|| std::env::vars().collect()),
        key_name,
    )
}

/// The OBSERVE node key the Studio's NAMED-RUN backend signs its dial with
/// (`docs/decisions/0064-a-named-run-carries-no-source.md`) — `remote_hist_store`'s twin, built from
/// this binary's two `OnceLock`s for the identical reason: only a BINARY reads the process
/// environment (`crates/vike-ops/tests/settings/settings_registry.rs`), and the resolution behind it opens
/// `<project>/settings/node.env`.
///
/// ⚠ **The notices are dropped here and that is a deliberate difference from `remote_hist_store`'s
/// caller.** `datahub_observe_keys` returns a `Vec<String>` of decision-0051 migration notices, and
/// the store plane surfaces them because the store is where a missing key shows up as an empty
/// catalog somebody then debugs. A named run cannot fail silently the same way: an unauthenticated
/// dial against a keyed compute daemon fails AT CONNECT with a message naming the keys, which
/// `crates/vike-studio/src/backend/remote.rs`'s `connect_observe` puts straight into the Studio's
/// error banner. Re-surfacing the same notice twice per frame would be noise, and this is called
/// once per Studio construction.
fn named_run_observe_keys(key_name: &str) -> Option<vike_node_proto::auth::NodeKeys> {
    vike_app_core::backend::backend_registry::datahub_observe_keys(
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
        // ⚠ `PROCESS_ENV`, NOT `workspace_credentials()` — the same distinction every other dial in
        // this file states: the VENUE store would miss a key set only in the environment and would
        // find a legacy one SILENTLY, skipping the decision-0051 notice.
        PROCESS_ENV.get_or_init(|| std::env::vars().collect()),
        key_name,
    )
    .0
}

/// The CHART-SEED dial, built from this binary's two `OnceLock`s — `remote_hist_store`'s twin, and
/// here for the identical reason: only a BINARY reads the process environment
/// (`crates/vike-ops/tests/settings/settings_registry.rs`), so the SWEEP belongs in the composition root even
/// though everything it feeds lives one crate down in `vike_app_core::data::chart_seed`.
fn chart_seed_dial(
    addr: String,
    key_name: &str,
    note: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> vike_app_core::data::chart_seed::SeedDial {
    vike_app_core::data::chart_seed::SeedDial::resolve(
        addr,
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
        PROCESS_ENV.get_or_init(|| std::env::vars().collect()),
        key_name,
        note,
    )
}

/// The VENUE-CATALOG dial — `chart_seed_dial`'s twin, built from the same two `OnceLock`s and here
/// for the same reason: only a BINARY reads the process environment
/// (`crates/vike-ops/tests/settings/settings_registry.rs`), so the SWEEP belongs in the composition root
/// even though every rule it feeds lives one crate down in `vike_app_core::data::catalog_wire`.
///
/// ⚠ Called only when `CatalogRefresh::dial_is_stale` says the stored dial no longer answers for
/// this (address, key name) pair — resolving a dial opens the credential store, and a per-frame
/// resolution would be a file read on the frame thread sixty times a second.
fn catalog_dial(addr: String, key_name: &str) -> vike_app_core::data::catalog_wire::CatalogDial {
    vike_app_core::data::catalog_wire::CatalogDial::resolve(
        addr,
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
        PROCESS_ENV.get_or_init(|| std::env::vars().collect()),
        key_name,
    )
}

/// Open the STUDIO's store as the TRAIT handle it consumes (split-plane B12), branching on the
/// RESOLVED datahub address (`App::resolved_datahub_addr` — the explicit `config.datahub_addr`,
/// else the active backend's `Welcome` advertisement; REQ-2):
///
/// - **`Some(addr)`** → dial a `vike_datahub_client::RemoteHistStore` at that address — the
///   CLIENT-side address of a `vike-datahub` server (⚠ deliberately distinct from
///   `config.tradehub_addr`, the tradehub DAEMON's BIND address). The constructor never dials
///   (connect-per-read), so a one-shot `list_series` probe runs here: a down/unreachable server
///   degrades to the same "Studio has no data store: …" view a broken local open takes, instead
///   of a Studio whose every pane errors separately. Returns the dialled address as the second
///   element so the caller can arm the remote-store UI rules.
/// - **`None`** → an `Err` NAMING both fixes: set `config.datahub_addr`, or connect a backend that
///   advertises its datahub. ⚠ TOMBSTONE — this arm used to OPEN A LOCAL STORE
///   (`open_local_hist_store`, a concrete `vike_data::DataFusionHist`) under the `fat` feature.
///   The desktop links vike-data with DEFAULT features now — the trait-only `HistStore` seam — so
///   there is no engine here to open and the Studio is REMOTE-ONLY by construction rather than by
///   configuration.
///
/// Best-effort and called at most once, like the local opener (see `App::studio`'s doc: success
/// or failure both stick for the process lifetime). ⚠ The one-shot is also the REQ-2 timing
/// contract's sharp edge: a Studio opened BEFORE any backend connects resolves from the explicit
/// key alone and keeps that store for the session — connect first, then open Studio, to pick the
/// advertisement up. Every other datahub consumer re-resolves per read.
fn open_studio_store(
    datahub_addr: Option<String>,
    datahub_key_name: &str,
) -> Result<(vike_studio::StoreHandle, Option<String>), String> {
    match datahub_addr {
        Some(addr) => {
            use vike_data::HistStore as _; // list_series — RemoteHistStore serves it as a trait verb
            let store = remote_hist_store(addr.clone(), datahub_key_name);
            store
                .list_series()
                .map_err(|e| format!("datahub {addr}: {e} (the resolved datahub address — config.datahub_addr, or the backend's advertisement — unset/disconnect it to use the local store)"))?;
            Ok((Arc::new(store) as vike_studio::StoreHandle, Some(addr)))
        }
        None => Err("this build has no local store engine — run `vike-cli config set \
             config.datahub_addr <addr>` to point it at a running vike-datahub server, or connect \
             a backend whose daemon advertises one (`config.datahub_advertise_addr` on the \
             daemon)"
            .to_string()),
    }
}

fn load_png_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
    Some(ctx.load_texture(format!("icon_{name}"), ci, egui::TextureOptions::LINEAR))
}

// The `recon_mount_tests` module that stood here asserted the grammar of
// `vike_tradehub::reconcile_config::reconcile_enabled` — a function this binary stopped calling long
// before it stopped reconciling at all. ⚠ It now calls NEITHER: with no local mount there is no
// armed-live set, no `reconcile_gate` call and no `ReconDriver` in this process, so the whole
// reconcile story belongs to `vike-tradehub` and its
// `a_live_armed_mount_reconciles_with_nothing_set_and_a_paper_one_does_not` is the composed test
// that covers it. The residual this paragraph used to declare — "nothing executes THIS root's
// wiring, so the GUI calling the gate with the armed set rests on review" — is retired by there
// being no wiring left to review.

// ── `--observe`: WireSnapshot → CoreSnapshot bridge ─────────────────────────────────────────────
// The whole reverse bridge (`wire_to_core` + map_ts/map_order_status/map_pos/map_order/map_held/
// map_venueblock/build_portfolio) moved DOWN into the CI-gated `vike-app-core`
// (`observe_bridge.rs`) so the mapping — above all `map_order_status`'s hand-maintained
// Debug-spelling table — is pinned by an exhaustive OrderStatus round-trip test in CI (this file
// is CI-excluded, so a newly added variant used to silently render as terminal `Rejected` in the
// observer). The observer's two pieces of IMPERATIVE wiring followed it (`spawn_bridge`, the
// reconnect loop that CALLS `wire_to_core`, and `connect_control`, the Scope::Write gate), so the
// module is imported WHOLE rather than that one function bare — the `feed_lifecycle` style, which
// makes each remaining call site here read as an explicit delegation.

// ⚠ TOMBSTONE — `struct LocalFeeds` and `fn build_local_feeds` STOOD HERE (~165 lines), and they
// were the ONLY thing in this crate that opened a venue market-data socket. Ruling 2 removes the
// local market-data plane outright: the desktop opens NO venue sockets at all.
//
// What the function built, over ONE composed `vike_data::LiveDataSink` shared by every venue:
//
//   • binance — `vike_binance::market_feed::Feeds`, whose `earliest_live_ids()` map was the
//     strictly-older paging floor the aggTrades backfill workers polled.
//   • bybit / okx — `market_feed::Feeds`, the DOM depth producers.
//   • aster — `market_feed::Feeds::with_env(.., Environment::Live)`, i.e. MAINNET, deliberately, to
//     match the mainnet catalog and the live exec client's own resolution.
//   • hyperliquid — `market_feed::Feeds` plus an off-thread `hl-feed-symbology` loader that fetched
//     `meta`/`spotMeta` from the mainnet transport so spot symbols resolved to their venue coins.
//   • polymarket — `vike_polymarket::Feeds` with a `vike_data::PropertiesRecorder` (the
//     `VIKE_RECORD_PROPERTIES=1` opt-in) and the `VIKE_POLY_TICKS` startup subscribe loop.
//
// It also assembled the Connections tool's per-venue Status map and carried a `debug_assert_eq!`
// pinning its own key set against `vike_app_core::backend::split_plane::LOCAL_FEED_VENUES` — that constant
// now has no reader in this workspace, and the assert was its only one.
//
// ⚠ `VIKE_POLY_TICKS` was read HERE and nowhere else in this binary; its
// `vike_ops::settings::SETTINGS` row loses its consumer with this deletion, and this change does
// not touch that registry.
//
// Consumers left holding empty maps rather than missing code: `App::feeds` (so `ensure_feed_on` /
// `ensure_trade_feed_on` / `ensure_depth` / `ensure_poly_book` / `reap_orphaned_feeds` are all
// no-ops now), `App::books`, `App::trades`, `App::feed_statuses`, `App::earliest_live_ids`. The DOM
// ladder, the Polymarket cockpit and every tick/volume + orderflow chart therefore have no
// producer at all — `WireSnapshot` carries bars but neither a book nor a trade tape — and what
// happens to those surfaces is a product decision this change deliberately did not take.

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
    /// `crates/vike-desktop/src/main.rs`'s `App` uncallable-into from a test BY CONSTRUCTION, in
    /// exchange for a borrow this function never took: every read was `egui_ctx` — fonts,
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
    /// [`App::mount_observer`], [`App::spawn_tool_data`], [`App::spawn_flag_images`],
    /// [`App::spawn_logo_images`], [`App::mount_catalog`], [`App::load_launcher_icons`],
    /// [`App::build_candle_pipeline`], the
    /// `Self { .. }` literal, then [`App::apply_startup_layout`]. ⚠ The phases that read the process
    /// environment (the observer mount, the startup layout, the literal's two QA knobs) cannot leave
    /// this file: `crates/vike-ops/tests/settings/settings_registry.rs` classifies an environment read by
    /// FILE PATH and only `main.rs` earns `Layer::Binary`.
    fn new(
        egui_ctx: &egui::Context,
        render_state: Option<&eframe::egui_wgpu::RenderState>,
        // ⚠ The whole RECORD, not an address: it carries the credential-store KEY NAMES this
        // connection must sign with. `main` resolves it (flag / registry active record / local
        // default) and this arm connects from it unchanged.
        observe_backend: Option<vike_app_core::backend::backend_registry::BackendRecord>,
    ) -> Self {
        // The design system's install, from the five `preferences.*` rows this binary loaded at
        // boot (spec §5). The five READS stay here so `vike-cli config show` names `desktop` as
        // their reader — a library read would say `yes` — and the mapping is one crate down, where
        // CI runs it.
        let appearance = appearance_settings::appearance_from(
            &settings().preferences.theme,
            &settings().preferences.market_colors,
            settings().preferences.header_gradient,
            &settings().preferences.density,
            &settings().preferences.text_size,
        );
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
            core,
            active_backend,
            snap_cell,
            md_mount,
            feed_status,
            recorder,
            books,
            trades,
            direct_bars,
            bf_rx,
            earliest_live_ids,
            live_venues,
            live_locks,
            forwarder_stop,
            recon_driver,
        } = Self::mount_observer(egui_ctx, observe_backend.as_ref());

        let (tools, opt_refresh) = Self::spawn_tool_data(egui_ctx);
        let flag_rx = Self::spawn_flag_images(egui_ctx);
        let logo_rx = Self::spawn_logo_images(egui_ctx);
        let (catalog, catalog_rx) = Self::mount_catalog(egui_ctx);
        // Data Manager "Stored" view: the inventory-load channel (see `App::refresh_stored`).
        // Nothing is spawned yet — the first load is triggered lazily, the first time the
        // Stored tab is shown (or by a Refresh click), not at startup.
        let (stored_tx, stored_rx) = std::sync::mpsc::channel();
        // dm-bulk-backfill: the bulk-Backfill completion channel (see
        // `App::maybe_spawn_stored_backfill`) — same lazy, nothing-spawned-at-startup shape.
        let (stored_backfill_tx, stored_backfill_rx) = std::sync::mpsc::channel();
        // Cockpit Gamma token-resolution channel (see `App::spawn_poly_token_resolver`): a resolver
        // thread per cockpit-window-open sends the picked YES token-id back here; nothing at startup.
        let poly_resolve_rx = dead_receiver();
        let launcher_icons = Self::load_launcher_icons(egui_ctx);

        let gpu_ok = Self::build_candle_pipeline(render_state);

        // ⚠ TOMBSTONE — the JOURNAL MATERIALIZER is gone, and `App::materializer` is now always
        // `None`. Under `fat` this read `vike_core::journal_config_from_env()` for the WAL
        // directory, opened a `vike_data::DataFusionHist` at `tick_store_root()` and handed both to
        // `vike_journal::materialize::maybe_spawn` (reached as `vike_app_core::journal_mat::…` when
        // this line was written), which materialized the local core's journalled
        // fills into the Tier-2 exec-fill log off the fold. It was already `None` in observe mode —
        // no local core writes a journal to materialize — and the desktop has no local core at all
        // any more (rulings 1 and 2), so there is no journal here to read and no DataFusion engine
        // linked to read it with. ⚠ This went on "The daemon owns this work now" until 2026-09-28,
        // and that stopped being true on 2026-09-22, when #2093 deleted the daemon's `materialize`
        // feature too: NO production root materializes the WAL today — it is still written, and
        // `tearsheet --journal DIR` reads it directly. The field survives only because
        // `App::run_bounded_teardown` still names it (whether it should is the field-level
        // decision this change deliberately did not take).
        let materializer: Option<vike_journal::materialize::MaterializerHandle> = None;

        // Unified shutdown signal (see the `App::shutdown` field + `on_exit`): raised at window
        // close, then read by the bounded teardown. Inert until then.
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        // The backfill workers' exit-report lane (see the `bf_done_tx` field). Built here rather
        // than in the branched core tuple above because it needs nothing from any branch — it is
        // pure `App`-local bookkeeping, present and inert in the one (observing) build there is.
        let bf_done_rx = dead_receiver::<feed_lifecycle::BackfillReport>();

        let mut app = Self {
            charts: HashMap::new(),
            shutdown,
            materializer,
            display_tz: DisplayTz::Local,
            spawned: HashSet::new(),
            unroutable: HashSet::new(),
            feed_retries: feed_lifecycle::FeedRetries::default(),
            hidden: HashSet::new(),
            core,
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
            recorder,
            subs: HashMap::new(),
            books,
            trades,
            direct_bars,
            store_asked: HashSet::new(),
            chart_seed_note: std::sync::Arc::new(std::sync::Mutex::new(None)),
            last_direct_gen: 0,
            aggs: HashMap::new(),
            of_aggs: HashMap::new(),
            of_backfill_hours: workspace::persist::default_backfill_hours(),
            bf_spawned: HashSet::new(),
            bf_retries: feed_lifecycle::BackfillRetries::default(),
            bf_rx,
            bf_done_rx,
            bf_pending: HashMap::new(),
            earliest_live_ids,
            trade_depth: HashMap::new(),
            poly_subs: HashMap::new(),
            poly_resolve_rx,
            poly_names: HashMap::new(),
            live_venues,
            forwarder_stop,
            recon_driver,
            _live_locks: live_locks,
            trade_seed_pending: std::env::var(TRADE_SEED_ENV).is_ok(),
            last_trade: None,
            directory: Arc::new(Mutex::new(Default::default())),
            chart_draw_pending: std::env::var(CHART_DRAW_ENV).is_ok(),
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
            opt_refresh,
            palette: String::new(),
            shot_n: 0,
            export_pending: false,
            export_seq: 0,
            layout_name_prompt: false,
            layout_name_input: String::new(),
            layout_prompt_focus: false,
            flags: HashMap::new(),
            flag_rx,
            news_logos: HashMap::new(),
            logo_rx,
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
            ),
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
        };

        Self::apply_startup_layout(&mut app, egui_ctx, area);
        app
    }

    /// Construction phase 1 — the observer mount: the connection to the backend's observe (and, when
    /// armed, control) channels and every core-derived binding `App` holds. All of those are
    /// empty/`None` read-only twins except the bridge itself. There is one arm; the `else` is a
    /// tombstone and an `unreachable!`. Returned as an [`ObserverMount`] for `new`'s literal.
    fn mount_observer(
        egui_ctx: &egui::Context,
        observe_backend: Option<&vike_app_core::backend::backend_registry::BackendRecord>,
    ) -> ObserverMount {
        if let Some(observe_backend) = observe_backend {
            // ── --observe: THE mode — the read-only client ──────────────────────────────────────
            // Connect to a headless `vike-tradehub` daemon's observe server (Scope::Read) and
            // republish its pushed `WireSnapshot`s into a GUI-owned arc-swap cell as
            // `CoreSnapshot`s. The ACCOUNT PLANE is the backend's: no local core, no exec engines,
            // no recorder, no journal materializer, no recon driver, `live_venues` empty. With
            // `core: None`, the command choke point (gated on a `Dispatch` over
            // `(self.core, self.remote_ctrl)`) is a read-only no-op UNLESS a Scope::Write channel
            // is ALSO mounted (`remote_ctrl`, built below) — then the GUI's order buttons drive the
            // REMOTE core, which is the ONLY route an order out of this process has (ruling 1).
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

            let core: Option<vike_core::CoreHandle> = None;
            let snap_cell: Arc<arc_swap::ArcSwap<vike_core::CoreSnapshot>> = Arc::new(
                arc_swap::ArcSwap::from_pointee(vike_core::CoreSnapshot::empty("observing", "")),
            );
            // The bridge's connection-status handle — `App::feed_status` in BOTH observer
            // shapes: the account-plane link is the status bar's headline here; the per-venue
            // feed lines live in `feed_statuses` (Connections tool) in the third mode.
            let feed_status: Arc<std::sync::Mutex<String>> =
                Arc::new(std::sync::Mutex::new(format!("connecting to {addr}…")));
            let recorder: Option<RecorderHandleTy> = None;
            let books = std::sync::Arc::new(data_sink::BookStore::default());
            let trades = std::sync::Arc::new(data_sink::TradeStore::default());
            let bf_rx = dead_receiver::<(String, Vec<vike_model::TradeTick>)>();
            // ⚠ TOMBSTONE — THE THIRD MODE is gone. A `fat` build launched with `--observe` used
            // to take a SECOND arm here: it mounted `build_local_feeds`'s six keyless
            // direct-to-venue feeds over a core-free `data_sink::GuiFeedSink`, so the trade tape,
            // the tick/volume + orderflow folds, DOM depth, the Polymarket cockpit and the
            // `DIRECT_BAR_VENUES` kline charts were all CLIENT-DIRECT while the account plane
            // stayed remote. Ruling 2 removed it: the desktop opens NO venue socket.
            //
            // ⚠ AND THE MARKET-DATA PLANE IS BACK, from the OTHER side. `md_session::build` mounts
            // one `DatahubFeed` per `split_plane::LOCAL_FEED_VENUES` slug over ONE connection to
            // the BACKEND's datahub, and constructs the `GuiFeedSink` that has had no production
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
                // credential store plus the two TRADEHUB node keys gap-filled from the
                // environment — and handing it here would break this key in BOTH directions:
                // `VIKE_DATAHUB_OBSERVE_KEY` set in the environment (the thin-client container's
                // only way to carry one) would not be found at all, and one left behind in
                // `secrets.env` would be found SILENTLY, skipping the decision-0051 migration
                // notice that `vike_secrets::resolve_node_keys`' legacy rung exists to print.
                PROCESS_ENV.get_or_init(|| std::env::vars().collect()),
                vike_app_core::backend::backend_registry::datahub_observe_key_name(observe_backend),
                books.clone(),
                trades.clone(),
                {
                    let ctx = egui_ctx.clone();
                    move || ctx.request_repaint()
                },
            );
            let earliest_live_ids: Arc<std::sync::Mutex<HashMap<String, u64>>> =
                Arc::new(std::sync::Mutex::new(HashMap::new()));
            let direct_bars: Option<std::sync::Arc<data_sink::DirectBarStore>> =
                Some(std::sync::Arc::new(data_sink::DirectBarStore::default()));
            let live_venues: HashSet<String> = HashSet::new();
            let forwarder_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let recon_driver: Option<vike_core::ReconDriver> = None;

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
                    vike_app_core::backend::tradehub_control::control_enabled(),
                    feed_status.clone(),
                    move |snap| cell.store(snap),
                    move || ctx.request_repaint(),
                ))
            };

            ObserverMount {
                core,
                active_backend,
                snap_cell,
                md_mount,
                feed_status,
                recorder,
                books,
                trades,
                direct_bars,
                bf_rx,
                earliest_live_ids,
                live_venues,
                // No local core in `--observe` ⇒ no exec client, no venue armed, nothing to lock.
                live_locks: Vec::new(),
                forwarder_stop,
                recon_driver,
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
            //   • the MOUNT (it was `vike_run`'s then; `vike-run` merged into `vike-mount`,
            //     decision 0098) — `vike_mount::MountPolicy::from(&settings().policy)`,
            //     `vike_mount::armed_live_venues`, `vike_mount::venue_arming`, a
            //     `vike_mount::NodeConfig` carrying the GUI's `vike_core::CoreConfig` knobs, and
            //     `vike_mount::build_node`,
            //     which assembled every `vike_mount::make_engine` arm (the twelve-venue mount),
            //     spawned the single-writer core and forwarded live venue events. A build_node
            //     fault exited the process with code 2 before a window existed.
            //   • the LIVE-ACCOUNT LOCK — `vike_ops::live_lock::LiveLock::acquire` per armed live
            //     route, claimed ONE statement above `build_node`; that ordering was load-bearing
            //     (claim before anything authenticates) and is pinned by
            //     `crates/vike-ops/tests/wiring/live_lock_claim_order_gate.rs`, whose vike-app root row
            //     was REMOVED with this code (the gate's tombstone says why it was not re-pointed).
            //   • RECONCILIATION — `vike_tradehub::reconcile_config::quarantine_first_default` over the
            //     REAL process env, `reconcile_gate` over `flags.reconcile`/`flags.reconcile_off`
            //     and the armed-live COUNT, `log_reconcile_gate`, the per-venue health map derived
            //     from the feed statuses, the `journal_view_provider` hook, and the
            //     `vike_core::spawn_recon` driver mount. Pinned by
            //     `crates/vike-ops/tests/wiring/reconcile_gate_wiring_gate.rs`'s vike-app root, likewise
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
            // `vike_ops::settings::SETTINGS` both key on that, and neither is updated here.
            unreachable!(
                "the desktop has no local trading core — `split_plane::observes` is \
                 unconditionally true, so `observe_backend` is always Some and the observer arm \
                 always wins"
            )
        }
    }

    /// Construction phase 2 — the tool windows' data (News / Calendar / Options), fetched on
    /// background threads. Returns the shared `ToolData` and the Options Refresh pill's wake channel.
    fn spawn_tool_data(
        egui_ctx: &egui::Context,
    ) -> (Arc<Mutex<tools::ToolData>>, std::sync::mpsc::Sender<()>) {
        // tool-window data (News/Calendar/Options) fetched on background threads
        let tools = std::sync::Arc::new(std::sync::Mutex::new(tools::ToolData::default()));
        let opt_refresh = {
            let c = egui_ctx.clone();
            // ⚠ TOMBSTONE — the opt-in `kind=chain` option-chain RECORDER is gone, so
            // `VIKE_RECORD_CHAINS=1` is inert in this binary. Under `fat` this was
            // `vike_data::ChainRecorder::open_from_env(tick_store_root())`, which persisted every
            // fetched option chain into the local Parquet store. The desktop links vike-data with
            // DEFAULT features now (the trait-only `HistStore` seam — rulings 1 and 2 took the
            // local data plane with the venue plane), and `open_from_env` is one of the
            // `hist-datafusion` constructors, so there is no engine here to open. The `None` stays
            // because `tools::spawn_tool_fetchers` still takes the parameter — the Options tool
            // FETCHES exactly as before, it just records nothing.
            let chain_rec: Option<std::sync::Arc<vike_data::ChainRecorder>> = None;
            tools::spawn_tool_fetchers(
                tools.clone(),
                move || c.request_repaint(),
                chain_rec,
                tool_api_keys(),
            )
        };
        (tools, opt_refresh)
    }

    /// Construction phase 3 — the country-flag images, decoded off-thread and handed back over a
    /// channel the frame drains into textures.
    fn spawn_flag_images(egui_ctx: &egui::Context) -> Receiver<(String, egui::ColorImage)> {
        // real country flags (PNG from flagcdn) decoded off-thread, loaded as textures in ui()
        let (ftx, flag_rx) = std::sync::mpsc::channel();
        {
            let c = egui_ctx.clone();
            spawn_flag_fetcher(ftx, move || c.request_repaint());
        }
        flag_rx
    }

    /// Construction phase 3b — the provider-logo (favicon) images, same shape as the flags.
    fn spawn_logo_images(egui_ctx: &egui::Context) -> Receiver<(String, egui::ColorImage)> {
        let (ltx, logo_rx) = std::sync::mpsc::channel();
        {
            let c = egui_ctx.clone();
            spawn_logo_fetcher(ltx, move || c.request_repaint());
        }
        logo_rx
    }

    /// Construction phase 4 — the symbol-search catalog's owner (`CatalogRefresh`) with its disk-cache
    /// read already spawned. Returns it with the receiving half the frame drains the universe from.
    fn mount_catalog(
        egui_ctx: &egui::Context,
    ) -> (Arc<vike_app_core::data::catalog_refresh::CatalogRefresh>, Receiver<vike_catalog::Catalog>)
    {
        // Symbol-search catalog: the DISK CACHE is read off-thread and published over `catalog_rx`
        // (drained in `update`); quick-picks stand in until it lands. A venue the cache has never
        // held is fetched once — nothing else re-fetches, which is what makes the Data Manager's
        // Refresh button the only re-fetch there is. `DeribitCatalog` is the ONE provider this
        // binary links (the Cargo.toml tombstone beside that edge names the six that went with the
        // venue plane), so it is the one venue whose button is live here.
        let (catx, catalog_rx) = std::sync::mpsc::channel();
        let catalog = Arc::new(vike_app_core::data::catalog_refresh::CatalogRefresh::new(
            vec![Arc::new(vike_deribit::DeribitCatalog)],
            STATE_DIR.get().and_then(Option::as_ref).map(|d| d.join("catalog.json")),
            // The SHIPPED venue baseline (`docs/decisions/0066` decisions 5-8), under
            // `<project>/bin/catalog/` — the RUNTIME-tool directory, because it ARRIVES as a
            // release asset `scripts/fetch_release_tools.sh` installs rather than sitting in a
            // source tree a deployed binary does not have. `bin_dir_beside` rather than
            // `project_bin_dir_from`: this root has already booted, and the bare walk is
            // `$VIKE_SETTINGS_DIR`-BLIND (`crates/vike-boot/tests/one_owner.rs` is the gate).
            // A `None` here, or a file that is absent, degrades: the venues it covers read
            // exactly as they read today.
            vike_model::paths::state_path::bin_dir_beside(
                SETTINGS_DIR.get().and_then(Option::as_deref),
            )
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
    fn load_launcher_icons(egui_ctx: &egui::Context) -> HashMap<String, egui::TextureHandle> {
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
    /// Gamma resolve, then `next_win_n`.
    fn apply_startup_layout(app: &mut App, egui_ctx: &egui::Context, area: egui::Rect) {
        // ── startup layout ──────────────────────────────────────────────────────────────────────
        // The whole decision — restore-vs-QA-window-vs-default-chart, the feeds each of those
        // windows needs, and the VIKE_STYLE/VIKE_SCALE capture overrides — moved DOWN into the
        // CI-gated [`vike_app_core::ui::startup`] (six branches over five env knobs that no gate
        // compiled; see that module's doc). The five env READS stay HERE, in the binary, and are
        // injected as `StartupEnv`: that keeps every `VIKE_*` row in the settings registry
        // classified `vike-desktop` / `Layer::Binary` exactly as today, and leaves `startup::plan` a
        // pure function its tests can drive without mutating process env.
        let startup_env = startup::StartupEnv {
            shot_mode: std::env::var("VIKE_SHOT").is_ok(),
            shot_win: std::env::var("VIKE_SHOT_WIN").ok(),
            // The one knob here that is a real user PREFERENCE rather than a QA-capture override:
            // `preferences.chart_style`, still overridden by `VIKE_STYLE` inside the loader. The
            // other four stay bare env reads — `VIKE_SHOT*`, `VIKE_SCALE` and the cockpit token
            // are screenshot/dev-convenience harness knobs, and `vike_config::flags`' module doc
            // names that family as deliberately NOT settings.
            style: settings().preferences.chart_style.clone(),
            scale: std::env::var("VIKE_SCALE").ok(),
            poly_cockpit_token: std::env::var("VIKE_POLY_COCKPIT_TOKEN").ok(),
            // The SAME read that arms `App::trade_seed_pending` above, spelled twice because the
            // two halves of that hook are decided in different places: this one opens the bar feed
            // whose closes clock the paper fill, the frame loop's one submits the orders. They
            // cannot disagree about WHETHER the hook is on — one variable, both `is_ok()` — and
            // `capture_seed::SEED_SYMBOL` is what stops them disagreeing about WHAT it trades.
            trade_seed: std::env::var(TRADE_SEED_ENV).is_ok(),
        };
        let restored = startup::restored_workspace(startup_env.shot_mode);
        let layout = startup::plan(area, &startup_env, restored);
        // Applied in the ORDER `StartupLayout` documents, which is the exact order this function
        // performed these steps before the move: globals, then the feeds (both arms that ensure
        // anything did so BEFORE publishing their windows), then the restore log, then the window
        // list, then the background Gamma resolve (the cockpit arm spawned it AFTER its push), then
        // `next_win_n`. A `fn new`'s startup order is load-bearing, so it is reproduced, not tidied.
        if let Some(d) = layout.display {
            app.display_tz = d.display_tz;
            app.of_backfill_hours = d.of_backfill_hours;
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
        if let Some((wid, account)) = layout.connections_account {
            // Same pre-seed as the sub-tab above, for the same reason: the window loop
            // `entry(..).or_default()`s this map, so writing it here is simply what it will find.
            // ⚠ It is a PENDING value, not a setting — `data_body`'s `DataDest::Credentials` arm
            // `take`s it on the first frame it renders, so the operator's own chip clicks are
            // never re-overridden.
            app.tool_views.entry(wid).or_default().connections_account = Some(account);
        }
        if let Some(wid) = layout.resolve_poly_token {
            app.spawn_poly_token_resolver(wid);
        }
        app.next_win_n = app.wins.len() as u32;
    }
}

/// What `App::mount_observer` builds: the core-derived `App` bindings, bundled so the observer arm
/// can leave `App::new` without a fifteen-element tuple. Every field lands in `new`'s `Self { .. }`
/// literal under the same name (`md_mount` and `live_locks` through their `Self` fields).
struct ObserverMount {
    core: Option<vike_core::CoreHandle>,
    active_backend: Option<vike_app_core::backend::backend_conn::BackendConn>,
    snap_cell: Arc<arc_swap::ArcSwap<vike_core::CoreSnapshot>>,
    md_mount: vike_app_core::data::md_session::MdMount,
    feed_status: Arc<std::sync::Mutex<String>>,
    recorder: Option<RecorderHandleTy>,
    books: std::sync::Arc<data_sink::BookStore>,
    trades: std::sync::Arc<data_sink::TradeStore>,
    direct_bars: Option<std::sync::Arc<data_sink::DirectBarStore>>,
    bf_rx: Receiver<(String, Vec<vike_model::TradeTick>)>,
    earliest_live_ids: Arc<std::sync::Mutex<HashMap<String, u64>>>,
    live_venues: HashSet<String>,
    live_locks: Vec<vike_ops::live_lock::LiveLock>,
    forwarder_stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    recon_driver: Option<vike_core::ReconDriver>,
}

// ⚠ TOMBSTONE — `tool_title_bar` lived here and is now
// `crates/vike-app-core/src/ui/workspace/title_bar.rs`'s `tool_title_bar`. It moved for the reason
// that module's own doc argues: a title bar that RESERVES a tab slot for whichever window kind
// carries tabs (`WinKind::carries_title_tabs` — no kind does today; Connections-merges-into-
// Data-Manager deleted the one window whose tabs these were, 2026-10-05, and the slot now sits
// idle for the next tabbed kind to paint into) is making geometry decisions, and this crate is in
// `xtask::ci::tables`' `EXCLUDE_FROM_CI`, so every one of them was gated by nothing.
// `crates/vike-app-core/src/ui/workspace/title_bar_tests.rs`'s
// `the_hairline_row_is_scissored_away_by_the_bar_and_survives_its_own_clip` runs them. The
// call site keeps the two-line mapping into `TitleActions`, which carries the CHART bar's
// symbol/venue/indicator fields a tool bar cannot produce.

/// The initial cockpit token-id: `VIKE_POLY_COCKPIT_TOKEN` (a real YES token-id an operator
/// supplies) when set and non-empty, else [`POLY_PLACEHOLDER_TOKEN`], which triggers background
/// Gamma resolution ([`App::spawn_poly_token_resolver`]).
///
/// The env READ stays here — this binary owns it, and the settings-registry row with it. The
/// trim / empty / placeholder ladder is [`vike_app_core::ui::startup::poly_cockpit_seed_token`], where
/// it is unit-tested; `App::new`'s cockpit arm now calls that directly with the same value, so the
/// two seeding paths cannot drift.
fn poly_cockpit_seed_token() -> String {
    startup::poly_cockpit_seed_token(std::env::var("VIKE_POLY_COCKPIT_TOKEN").ok().as_deref())
}

// ⚠ TOMBSTONE — `pick_updown_token` lived here. It picked the YES-outcome token-id and a short
// display name of the top-volume active crypto up/down market out of a Polymarket Gamma catalog
// page (`GammaClient::list` returns volume-DESC, so the first up/down match was the highest-volume
// one), and it was the whole of `App::spawn_poly_token_resolver`'s fat body. Both went with the
// `vike-polymarket` dependency: the desktop opens no venue socket and makes no venue REST call any
// more (rulings 1 and 2), so there is nothing here that can resolve a real token. The cockpit
// window keeps whatever `poly_cockpit_seed_token` seeded it with — `VIKE_POLY_COCKPIT_TOKEN` when
// an operator supplies one, else the placeholder.

#[allow(clippy::too_many_arguments)] // window-loop dispatcher: all tool inputs arrive together
fn tool_content(
    ui: &mut egui::Ui,
    kind: workspace::WinKind,
    symbol: &str,
    book_venue: &str,
    book: Option<&vike_model::L2Book>,
    book_stale: bool,
    trades: &data_sink::TradeStore,
    td: &tools::ToolData,
    feeds: &[(String, usize, i64, i64)],
    flags: &HashMap<String, egui::TextureHandle>,
    logos: &HashMap<String, egui::TextureHandle>,
    dsets: &datasets::Store,
    snap: &vike_core::CoreSnapshot,
    tv: &mut tools::ToolView,
    display_tz: DisplayTz,
    studio: &mut Option<vike_studio::StudioState>,
    studio_error: &mut Option<String>,
    stored_tree: &Arc<Vec<vike_data_manager::model::VenueNode>>,
    stored_gaps: &Arc<vike_data_manager::GapMap>,
    stored_partials: &Arc<vike_data_manager::PartialDayMap>,
    stored_coverage: vike_app_core::data::stored_mode::RemoteCoverage,
    stored_history: Option<&vike_app_core::data::history_column::HistoryLoad>,
    stored_loading: bool,
    // `Some(reason)` → the last load could not READ the store (see `App::stored_error`). A separate
    // parameter rather than a richer `stored_loading` deliberately: every reader of that flag —
    // the Refresh enable, three "Loading…" labels, the rail — asks a question this reason does not
    // answer, and folding the two would have touched all of them to say the same thing.
    stored_load_error: Option<&str>,
    stored_backfill_status: &str,
    opt_refresh: &std::sync::mpsc::Sender<()>,
    feed_statuses: &HashMap<String, Arc<std::sync::Mutex<String>>>,
    poly_names: &HashMap<String, String>,
    backend_picker: &tool_views::BackendPicker<'_>,
    backend_action: &mut Option<vike_app_core::backend::backend_conn::BackendAction>,
    backend_editor: &mut vike_app_core::backend::backend_editor::EditorState,
    backend_registry_update: &mut Option<vike_app_core::backend::backend_editor::RegistryUpdate>,
    backend_settings: &tool_views::BackendSettingsState,
    backend_settings_refresh: &mut bool,
    backend_settings_edit: &mut tool_views::SettingsEditState,
    backend_settings_write: &mut Option<tool_views::SettingsWriteRequest>,
    // The RESOLVED datahub address (`App::resolved_datahub_addr` — explicit `config.datahub_addr`
    // first, else the active backend's Welcome advertisement; REQ-2), threaded as a PARAMETER so
    // this free fn reads the same resolution the App methods do, never the raw key.
    resolved_datahub_addr: Option<&str>,
    // The datahub observe-key NAME to sign the STORE plane's dials with, resolved beside the
    // address by the caller (`App::datahub_observe_key_name`) for the reason that method's own doc
    // gives: both follow the ACTIVE backend, so a name taken at a different instant than the
    // address signs backend B's datahub with backend A's key — `bad mac`, with nothing on screen
    // that could explain it.
    datahub_key_name: &str,
    // The rect `workspace::tool_title_bar` reserved inside this window's title bar for a tabbed
    // tool's segmented control. The Trade arm seats its view controls there; every other kind
    // answers `WinKind::carries_title_tabs() == false`, slot inert. ⚠ The Connections arm used to
    // seat its own tabs here too — that arm and its `WinKind::Connections` are deleted, 2026-10-05.
    title_tabs: &workspace::TitleTabSlot,
    // The instrument catalog's owner (`App::catalog`) — read by the Data Manager's Instruments
    // destination for its rows and pressed by its Refresh button. A `&` because a refresh is a
    // `&self` call that spawns its own thread.
    catalog: &vike_app_core::data::catalog_refresh::CatalogRefresh,
    // The app's one appearance session (`App::appearance`) — the Settings window's body requests a
    // change on it; `draw_windows` applies it after every window has drawn.
    appearance: &mut AppearanceSession,
    // The Trade window's reads (`tool_views::ToolCtx`'s `symbols`/`last_trade`/`directory`/
    // `directory_unavailable`/`control_link`): the instrument catalog as its `Arc` (its pointer is
    // the catalog's identity, for the window's cache), the window a new one copies, the ACTIVE
    // backend's directory (`None` before it) and whether its fetch ended with none, and whether
    // this desktop can send an order at all.
    symbols: &Arc<vike_catalog::Catalog>,
    last_trade: Option<&tool_views::TradePick>,
    directory: Option<&vike_tradehub_client::wire::WireDirectory>,
    directory_unavailable: bool,
    control_link: tool_views::ControlLink,
) {
    // Every arm names its kind by path: a `use WinKind::*` turned an arm for a DELETED variant
    // into a binding that caught every kind (the `Dom` arm, A2's review). A path to a variant that
    // does not exist is a compile error instead.
    use workspace::WinKind;
    // The density's gap between the title bar and the body (design system spec §3.4): 6 at Normal,
    // today's value, 4 at Compact and 8 at Comfortable — none for the Trade window, whose bar ends
    // where its body begins (`workspace::title_bar::body_gap`).
    ui.add_space(workspace::title_bar::body_gap(
        kind,
        vike_ui_theme::appearance::current(ui.ctx()).density.metrics().gap,
    ));
    // The Stored tab's mode gates (the #1378 seam close): which of the grid's actions/columns
    // exist over the store the grid is reading — resolved from the SAME resolved datahub address
    // `refresh_stored` branches on, so the read mode and the offered actions cannot disagree.
    // Two inputs, two features: the RESOLVED datahub address (REQ-2 — explicit key, else the
    // active backend's advertisement) and `stored_coverage`, the §6-Q2 answer about what the last
    // load NEGOTIATED for the coverage verb, which the address alone cannot say (a remote server
    // may be older than the verb).
    let stored_mode_table =
        vike_app_core::data::stored_mode::stored_mode(resolved_datahub_addr, stored_coverage);
    // The moved tool bodies (vike-app-core `tool_views`) take their read-only inputs grouped in a
    // `ToolCtx` (audit F8), built per arm — cheap: a handful of refs + one env read. The
    // VIKE_JOURNAL_DIR read stays in this binary (libraries take configuration as parameters); it
    // feeds the Tearsheet exactly as the in-body read it replaced did.
    let tool_ctx = || tool_views::ToolCtx {
        td,
        snap,
        flags,
        logos,
        journal_dir: std::env::var_os("VIKE_JOURNAL_DIR").map(std::path::PathBuf::from),
        feeds,
        dsets,
        display_tz,
        stored: tool_views::StoredCtx {
            tree: stored_tree.as_slice(),
            gaps: stored_gaps,
            partials: stored_partials,
            loading: stored_loading,
            load_error: stored_load_error,
            backfill_status: stored_backfill_status,
            delete_unavailable: stored_mode_table.delete_unavailable,
            partials_note: stored_mode_table.partials_unavailable,
            history: stored_history,
        },
        feed_statuses,
        // Where a Save in the Connections editor LANDS and where it is RECORDED — the store and the
        // change journal, both off the ONE boot walk. See `credential_write_ctx`.
        credentials: credential_home().write_ctx(vike_model::now_ms()),
        // The two book-backed windows share ONE transport group (a window is EITHER a Trade
        // window or a cockpit, never both): for the cockpit `symbol` is the token-id, `book_venue`
        // is "polymarket" and `book` is that token's live L2 book from the same `BookStore`.
        book: tool_views::BookCtx {
            symbol,
            venue: book_venue,
            book,
            stale: book_stale,
            trades: Some(trades),
        },
        poly_names,
        catalog,
        symbols,
        last_trade,
        directory,
        directory_unavailable,
        control_link,
    };
    match kind {
        WinKind::Trade => tool_views::trade_tool_content(ui, &tool_ctx(), tv, title_tabs),
        WinKind::Account => tool_views::account_tool_content(ui, &tool_ctx(), tv),
        WinKind::Polymarket => tool_views::cockpit_tool_content(ui, &tool_ctx(), tv),
        WinKind::Options => {
            // Resolve the selected underlying's bundle (default = first fetched, i.e. BTC) and
            // reset a now-invalid expiry pick so a BTC→ETH switch never shows an empty grid.
            let bundle = tools::resolve_options_selection(
                &td.opt_by_underlying,
                &td.opt_underlyings,
                &tv.opt_underlying_sel,
                &mut tv.opt_expiry_sel,
            );
            let empty_chains = std::collections::BTreeMap::new();
            let empty_expiries: Vec<vike_options::Expiry> = Vec::new();
            let (chains, default_expiry, expiries) = match bundle {
                Some(b) => (&b.chains, b.default_expiry.as_str(), b.expiries.as_slice()),
                None => (&empty_chains, "", empty_expiries.as_slice()),
            };
            // The user's OWN deribit working orders + positions, keyed by instrument name — the
            // chain paints per-strike markers from this (empty ⇒ grid unchanged). Built fresh each
            // frame from the lossy snapshot (cheap: O(orders+positions), both small).
            let books =
                vike_app_core::orders::options_books::build_books(&snap.orders, &snap.positions);
            let acts = vike_chart::options_chain::draw(
                ui,
                vike_chart::OptionChainInputs {
                    chains,
                    default_expiry,
                    expiries,
                    expiry_sel: &mut tv.opt_expiry_sel,
                    underlyings: &td.opt_underlyings,
                    underlying_sel: &mut tv.opt_underlying_sel,
                    strike_window: &mut tv.opt_strike_window,
                    books: &books,
                    // "deribit" is the chain's data-source KEY (an address, not a spelling).
                    venue_label: tool_views::venue_label(directory, "deribit"),
                },
            );
            // A working-order marker click → stash the coid in the OUT slot; the update loop drains
            // it into an `OrderIntent::Cancel` (mirrors the Trade window's `TradeAction::Cancel`).
            if let Some(coid) = acts.cancel {
                tv.opt_cancel = Some(coid);
            }
            // Refresh pill → wake the options poll thread for an IMMEDIATE chain re-poll instead of
            // waiting out its 30s cadence. `send` errors are ignored: a closed channel (poll thread
            // gone at shutdown) must never panic the UI.
            if acts.refresh_clicked {
                let _ = opt_refresh.send(());
            }

            // A chain bid/ask click prefills a CONFIRM ticket — it NEVER submits an order (see the
            // confirm-gated submit path in `update`). Side/instrument/price come from the click;
            // qty defaults to a sensible deribit option size the user can edit.
            if let Some(click) = acts.order {
                tv.opt_order_ticket = Some(tools::OptOrderTicket {
                    instrument: click.instrument,
                    side: click.side,
                    price: click.price,
                    qty: 0.1,
                    is_call: click.is_call,
                    strike: click.strike,
                });
            }
            // SAFETY: the order is handed to the core (via `tv.opt_submit`) ONLY on Confirm.
            if let Some(mut ticket) = tv.opt_order_ticket.take() {
                match tool_views::order_ticket(ui.ctx(), &mut ticket) {
                    tool_views::TicketChoice::Confirmed => tv.opt_submit = Some(ticket),
                    tool_views::TicketChoice::Open => tv.opt_order_ticket = Some(ticket),
                    tool_views::TicketChoice::Cancelled => {}
                }
            }
        }
        WinKind::Greeks => tool_views::greeks_tool_content(ui, &tool_ctx()),
        WinKind::Tearsheet => tool_views::tearsheet_tool_content(ui, &tool_ctx(), tv),
        WinKind::Settings => tool_views::settings_tool_content(ui, appearance),
        WinKind::News => tool_views::news_tool_content(ui, &tool_ctx(), tv),
        WinKind::Calendar => tool_views::calendar_tool_content(ui, &tool_ctx(), tv),
        // The `VenueArmingInputs` read (the per-frame credential-store + `policy.venues` parse
        // the Credentials destination needs) stays gated by `data_tab_reads_arming` — no `6`
        // literal here, the index moved into that function. `backend_picker.available` is
        // `!has_local_core` (`backend_conn::switching_available`), which is the observe-vs-local
        // fact `split_plane::app_mode` takes.
        //
        // ⚠ **Updated 2026-10-05: the standalone Connections window (`WinKind::Connections`) is
        // deleted outright**, and its real body call is merged onto this arm: the full
        // backend-picker/action/editor/registry/settings/settings_refresh/settings_edit/
        // settings_write argument set that used to run only inside the `Connections` arm is now
        // BUILT and PASSED every frame this window is open, regardless of which Data Manager rail
        // destination is selected — the same tab-independent argument `connections_tool_content`'s
        // own call made becomes a destination-independent one, exactly as `data_body`'s own doc
        // comment already predicts ("these ten are not reachable from `tv`/`ctx` alone").
        // `title_tabs` is dropped from the call: Data Manager navigates by RAIL, not title-bar
        // tabs, so `data_tool_content` takes no slot parameter at all.
        //
        // ⚠ **The CREDENTIAL READ itself is the one thing in that set that is NOT unconditional —
        // see the scoping note below.** Everything else in the ten-param set is cheap to build
        // (references and small owned state) whichever destination is showing; the credential
        // read is the one that opens a file, so it alone earns a gate.
        //
        // ⚠ The CHECKED credential read, not the plain one: this tool RENDERS A COUNT of what the
        // store holds, and the plain loader's empty map is the same for an absent store and one
        // that would not open. One read, both answers — see `workspace_credentials_checked`.
        //
        // ⚠ **Scoped to `DataDest::reads_credentials`'s three destinations, not every frame this
        // window is open.** The merge that deleted `WinKind::Connections` first landed this call
        // UNCONDITIONALLY on `WinKind::Data` — the standalone Connections window was the only
        // thing that opened this read, so nothing had to gate it there, and that scoping did not
        // survive the merge. Left ungated, Overview/AllSeries/Store/… (ten of thirteen
        // destinations, none of which read `creds`/`health`/the catalog's credentialed-venues set)
        // paid a full credential-store open every frame, and an unreadable store's
        // `tracing::error!` (`crates/vike-bridge-core/src/credentials.rs`'s per-frame read path)
        // fired unthrottled regardless of which destination was on screen.
        WinKind::Data => {
            let arming = tool_views::data_tab_reads_arming(tv)
                .then(|| venue_arming_inputs(!backend_picker.available));
            let (creds, health) = if tool_views::data_tab_reads_credentials(tv) {
                let (creds, health) = workspace_credentials_checked();
                // ⚠ **The one place this box's credential set reaches the Instruments screen**, and
                // it is here rather than at `App::new` because credentials are EDITED on this very
                // screen: a set resolved once at startup would tell an operator who had just saved
                // their alpaca keys that no keys were saved (`docs/decisions/0066` decision 9's
                // arming). This arm already performs the FRESH read — the comment above says so —
                // so the push costs one derivation over a map already in hand, and the set is
                // `credential_status`' own, which is what the grid beside it renders. Gated the
                // same as the read it is built from: a destination that does not read credentials
                // this frame leaves the catalog's set at whatever it was last pushed, which is
                // exactly as stale as every OTHER per-frame fact this screen does not currently
                // render.
                catalog.set_credentialed_venues(vike_connections::credentialed_venues(&creds));
                (creds, health)
            } else {
                (HashMap::new(), vike_bridge_core::credentials::StoreHealth::default())
            };
            tool_views::data_tool_content(
                ui,
                &tool_ctx(),
                tv,
                arming.as_ref(),
                &creds,
                &health,
                backend_picker,
                backend_action,
                backend_editor,
                backend_registry_update,
                backend_settings,
                backend_settings_refresh,
                backend_settings_edit,
                backend_settings_write,
            );
        }
        WinKind::Studio => {
            // Lazy, one-shot store open (see `App::studio`'s doc) — tried once; success or
            // failure both stick, so a broken store never retries `DataFusionHist::open` (a real
            // cost: a tokio runtime + WAL recovery) every frame this window stays visible.
            // Unconditional since split-plane I7, and now REMOTE-ONLY: `open_studio_store` has one
            // arm (a resolved datahub — explicit key or backend advertisement) and one Err naming
            // both fixes. The old static "requires the full (fat) build" label went with the gate;
            // the local-store arm went with the `fat` feature itself.
            if studio.is_none() && studio_error.is_none() {
                match open_studio_store(resolved_datahub_addr.map(str::to_string), datahub_key_name)
                {
                    // The two QA capture hooks are read HERE, in the binary: `vike-studio` is a
                    // library and takes them as parameters (the settings-registry rule). Same
                    // family as this binary's other capture hooks; absent/garbage ⇒ the ordinary
                    // workspace-restoring construction, byte-identical to `StudioState::new`.
                    //
                    // The ChatPane's AI-provider keys are resolved here for the SAME rule
                    // (split-plane I8): the pane used to call the workspace `.env` loader itself,
                    // which made a GUI pane the thing that opened `<project>/settings/secrets.env`
                    // and put its file on `CREDENTIAL_STORE_PIN`. They come out of the one
                    // credential map this root already owns — see [`workspace_credentials`].
                    Ok((store, remote_addr)) => {
                        /// Force the Studio's initial tool tab (`sweep|strategy|data|indicators|
                        /// saved|chat`) for a headless per-tab capture; also suppresses workspace
                        /// persistence for the session so a capture cannot clobber the user's file.
                        const STUDIO_TAB_ENV: &str = "VIKE_STUDIO_TAB";
                        /// What the Studio starts on its first frame, so a capture can show a
                        /// RESULTS surface (it cannot click ▶ Run or ▶ Run Sweep): `=1` runs one
                        /// backtest, `=sweep` seeds the parameter grid and runs the sweep.
                        /// Anything else is inert. `vike_studio::QaAutorun::from_qa_str` is the
                        /// parse — the `=1` arm is byte-identical to the `== Ok("1")` read it
                        /// replaced, so an existing capture script is unaffected.
                        const STUDIO_AUTORUN_ENV: &str = "VIKE_STUDIO_AUTORUN";
                        let qa_tab = std::env::var(STUDIO_TAB_ENV).ok();
                        let qa_autorun = vike_studio::QaAutorun::from_qa_str(
                            std::env::var(STUDIO_AUTORUN_ENV).ok().as_deref(),
                        );
                        // state_dir: the resolved LOCAL store root, remote or not — for a local
                        // store this is the exact old `store.root()` colocation; a remote session
                        // keeps its saved strategies at the same stable local path (the trait has
                        // no root() — a filesystem concept an RPC store does not have).
                        let mut st = vike_studio::StudioState::new_with_qa(
                            store,
                            studio_store_root(),
                            vike_studio::ChatApiKeys::resolve(&workspace_credentials()),
                            qa_tab.as_deref(),
                            qa_autorun,
                        );
                        // The Research pane's HOST — where `user_data` is and who is running.
                        // Seeded here, after construction, for the same reason `store_is_remote`
                        // and `backend` are: `vike-studio` is a library and resolves no project
                        // root of its own.
                        st.research.host = studio_study_host();
                        // ⚠ **THE NAMED-RUN KEY** — seeded here for the same composition-root
                        // reason as everything above it: resolving it opens
                        // `<project>/settings/node.env`, and `vike-studio` is a library that
                        // resolves no project root. It is the OBSERVE key this desktop already
                        // holds for its store and market-data planes, and never a Control one —
                        // `vike_app_core::backend::backend_registry`'s `datahub_observe_key` states why no
                        // datahub dial of this process may hold the key that compiles
                        // client-supplied Rhai, and
                        // `docs/decisions/0064-a-named-run-carries-no-source.md` is the verb that
                        // makes the weaker key enough to run a backtest on the backend at all.
                        // (The ONE Control key this process may hold is `compute_key` below, and
                        // it is a different type from this one.)
                        //
                        // ⚠ The same key authenticates against the COMPUTE daemon unchanged:
                        // `vike_backtest::compute_server`'s `serve_authed` verifies under the SAME
                        // `DATAHUB_DOMAIN` and the same observe/control pair, which its own ⚠
                        // argues for. So the client-side gap 0064 names is the ADDRESS, not the
                        // key — and the address is an editable field on the Backend row, defaulting
                        // to `vike_config::DEFAULT_BACKTEST_ADDR`.
                        st.named_run_keys = named_run_observe_keys(datahub_key_name);
                        // The BUILDER's own key — a THIRD service, a THIRD domain separator, and
                        // a WRITE scope, none of which the observe key above can stand in for
                        // (`vike_strategy_builder::builder`'s Decisions 1 and 2 argue both: a key
                        // minted to observe a data plane must not also authenticate a request to
                        // run arbitrary `cargo` on that box). Seeded here for the same reason
                        // every other key in this file is: only a BINARY reads the process
                        // environment, and the one `std::env::vars()` sweep this root already owns
                        // is `PROCESS_ENV`.
                        //
                        // ⚠ **The environment is the ONLY rung read, deliberately and
                        // narrowly.** `named_run_observe_keys` above falls through to the NODE
                        // store when the environment is silent; this does not. Teaching that
                        // ladder a fourth key name means deciding which store a
                        // builder-namespaced name belongs to under decision 0051, and that
                        // record's own ⚠ notes record what a SHARED probe cost when it let
                        // whichever service migrated first decide where another service's pair was
                        // read from. So the key comes from the environment, and
                        // `crates/vike-studio/src/backend/plugin_build.rs` names the variable when
                        // it is absent (from that service's own constant, never a second
                        // spelling); widening it is a decision with a record rather than a rider
                        // on this one.
                        st.builder_keys = vike_strategy_builder::builder::keys_from_vars(
                            PROCESS_ENV.get_or_init(|| std::env::vars().collect()),
                        );
                        // ⚠ **STUDIO'S COMPUTE KEY — the datahub CONTROL key, resolved here and
                        // handed to Studio's compute dial and NOTHING ELSE.** The owner ruled on
                        // 2026-09-26 (`docs/decisions/0083-the-runtime-plugin-join-lands.md`,
                        // question 1, option (a)) that the desktop resolves it for that one dial,
                        // knowing what the key also authorises at `VerbScope::Write`: compiling
                        // Rhai, running any artifact in the plugin directory, `Backfill` and
                        // `DeleteSeries` — a store-mutation authority this process never held
                        // before. So its reach is kept as narrow as the code allows:
                        //
                        // * it is read from its OWN variable (`vike_studio::COMPUTE_KEY_ENV`),
                        //   never the platform `VIKE_DATAHUB_CONTROL_KEY`, and from the
                        //   environment only — no store rung, so a key written into this PC's store
                        //   reaches no dial of this process (and `vike-cli`, which does read the
                        //   store, never sees this name);
                        // * it arrives as a `vike_studio::ComputeKey`, which no datahub dial here
                        //   accepts — the store, the chart seed, the catalog, the named run and the
                        //   trading-node observer all take a `NodeKeys`, and this is not one;
                        // * Studio signs it in one place, toward a server whose pre-auth Welcome is
                        //   the COMPUTE daemon's, and refuses the datahub before sending a byte.
                        //
                        // `crates/vike-ops/tests/gui/studio_compute_key_reach_gate.rs` holds this line
                        // as the resolver's ONE production call. `None` — no key handed over — is
                        // the ordinary state, and Studio's Run then says where the key comes from.
                        st.compute_key = vike_studio::compute_key_from_vars(
                            PROCESS_ENV.get_or_init(|| std::env::vars().collect()),
                        );
                        // A remote STORE says where the bars come from. It says NOTHING about
                        // where compute happens, and this block used to assume it did.
                        //
                        // ⚠ It read: "the same datahub serves the HistStore verbs AND the run
                        // verbs, so seed the Backend to it". That has been false since ruling 7
                        // moved every `Run*` verb onto `Plane::Compute` — the datahub refuses
                        // them BY PLANE, before the scope check — and the address resolved here
                        // is the DATAHUB's. So this line overwrote `Backend`'s corrected default
                        // with 7878 on every session that opened a remote store, and since
                        // `open_studio_store` is remote-only that is every session: the operator
                        // saw 7878 in the Backend row under a tooltip reading "COMPUTE daemon
                        // address", and every ▶ Run came back a wrong-plane error.
                        //
                        // ⚠ It was SURVIVABLE while `Backend::Local` existed — the user clicked
                        // Local and it ran in-process. `Local` is deleted
                        // (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`),
                        // so the workaround is gone and the wrong seed is fatal. That is why a
                        // defect older than this branch is fixed ON it.
                        //
                        // The compute daemon is named by `config.backtest_addr` (ruling 7's
                        // client half); absent that, `Backend::default()` is already
                        // `Remote { DEFAULT_COMPUTE_ADDR }` and is left alone.
                        if remote_addr.is_some() {
                            st.store_is_remote = true;
                        }
                        if let Some(addr) = settings().config.backtest_addr.clone() {
                            st.backend = vike_studio::Backend::Remote { addr };
                        }
                        *studio = Some(st);
                    }
                    Err(e) => *studio_error = Some(e),
                }
            }
            match studio {
                Some(s) => s.ui(ui),
                None => {
                    let msg = studio_error.as_deref().unwrap_or("unknown error");
                    let why = format!("Studio has no data store: {msg}");
                    vike_ui_theme::components::state::view(
                        ui,
                        vike_ui_theme::components::state::Load::Unreachable(&why),
                    );
                }
            }
        }
        WinKind::Chart => {}
    }
}

/// Encode an egui framebuffer screenshot to `<exe_dir>/exports/chart-<ts>-<seq>.png`
/// (or `$VIKE_EXPORT_DIR/…` when set). This is the exact RGBA readback + PNG encode the
/// headless VIKE_SHOT self-shot uses (see the `update` fn), factored out here for the
/// user-triggered File -> Export chart image… action. `seq` disambiguates same-second names.
/// Returns the written path.
fn export_chart_png(img: &egui::ColorImage, seq: u32) -> std::io::Result<std::path::PathBuf> {
    let dir = if let Ok(d) = std::env::var("VIKE_EXPORT_DIR") {
        std::path::PathBuf::from(d)
    } else {
        // Same `<exe_dir>/…` convention as vike-log's `logs` and the workspace file.
        exe_dir().unwrap_or_default().join("exports")
    };
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!("chart-{stamp}-{seq:03}.png"));
    let [w, h] = img.size;
    let mut buf = image::RgbaImage::new(w as u32, h as u32);
    for (i, px) in img.pixels.iter().enumerate() {
        buf.put_pixel((i % w) as u32, (i / w) as u32, image::Rgba([px.r(), px.g(), px.b(), 255]));
    }
    buf.save(&path).map_err(std::io::Error::other)?;
    Ok(path)
}

/// Where one order/session command from the GUI is routed: the LOCAL in-process core, or a remote
/// headless daemon's Scope::Write channel (a `--observe` observer with control enabled). Built
/// once at the command choke point in `App::ui` (its `eframe::App` impl) from `(self.core, self.remote_ctrl)` and borrows
/// the chosen handle for that block, so EVERY command site funnels through one [`Dispatch::send`].
/// The local path stays byte-identical (`Local` forwards straight to `CoreHandle::try_command`);
/// the remote path is `vike_app_core::backend::tradehub_control::send_to_backend`, which lowers each
/// command through `wire_from_command` and reports anything it does not send in the log and on the
/// status strip's control segment (the handle's latch).
enum Dispatch<'a> {
    Local(&'a vike_core::CoreHandle),
    /// The remote channel, plus THIS FRAME'S SNAPSHOT — the second field is the client-side
    /// ROUTING gate's evidence about the backend (which venues it publishes an engine for), and it
    /// rides here rather than being re-read inside [`Dispatch::send`] because the snapshot is
    /// already borrowed for this block. The gate itself is
    /// `vike_app_core::backend::tradehub_control::may_send_to_backend`, one crate down, where CI runs it.
    Remote(&'a vike_tradehub_client::RemoteControlHandle, &'a vike_core::CoreSnapshot),
}

impl Dispatch<'_> {
    /// Fire one command — fire-and-forget on both arms, mirroring `CoreHandle::try_command`. A
    /// REMOTE command that was NOT sent (no thin-wire form, held back by the routing gate, or
    /// refused by the client before the wire: a bracket against a node older than `bracket`) is
    /// LATCHED on the handle, where the status strip's control segment shows it until dismissed.
    /// ⚠ Never into `app.status`: the next frame's `sync_from_core` rewrites that line before the
    /// strip paints it (C-1 of the node-bracket Task 3 review).
    ///
    /// The remote arm DISCARDS the `CommandTicket` deliberately: a GUI button has no synchronous
    /// place to report an outcome (blocking the repaint on a network reply is the one thing this
    /// path must never do), so the observer learns results the same way it always has — the pushed
    /// snapshot for what happened, and the status strip's latched `last_error` for what was
    /// refused. A surface that PRINTS a per-command verdict (`vike-cli trade`/`mcp`) must instead
    /// await the ticket; reading the latch for that is the stale-refusal bug.
    fn send(&self, cmd: vike_exec::Command) {
        match self {
            Dispatch::Local(core) => {
                let _ = core.try_command(cmd);
            }
            // ⚠ **The client-side gates fire here**, at the one choke point rather than at each
            // button, and before the write: a backend that does not advertise
            // `vike_tradehub_client::proto::FEATURE_VENUE_ROUTING` accepts a command naming ANY
            // venue and applies it to its PRIMARY engine — `Ack`, an order in the snapshot, no
            // error anywhere — so there is no reply to check afterwards. The DECISIONS, their log
            // lines and the refusal lines are `send_to_backend`, one crate down, where CI runs
            // them; what stays here is the two facts only this binary holds, the write and the latch.
            Dispatch::Remote(ctrl, snap) => {
                vike_app_core::backend::tradehub_control::send_to_backend(
                    &cmd,
                    snap,
                    ctrl.routes_by_venue(),
                    |wire| ctrl.try_command(wire),
                    |line| ctrl.latch_client_refusal(line),
                )
            }
        }
    }
}

/// ⚠ **The crate's ONE `impl eframe::App for App`, and it has to stay that way.** Coherence allows
/// a type exactly one impl per trait, so a lifecycle hook can have its BODY in another file but not
/// its `fn` — `app_lifecycle.rs` learned that the hard way (see its module doc) and now exposes the
/// teardown as an inherent `App::run_bounded_teardown` this block forwards to. Splitting `App`'s
/// INHERENT methods across files stays fine, which is what `app_methods.rs` does.
/// `VIKE_FRAME_LOG=1` — log the achieved frame rate once a second. See the call site in
/// `App::ui` for what it exists to measure and why it is off by default.
const FRAME_LOG_ENV: &str = "VIKE_FRAME_LOG";

/// Read once, not per frame: `env::var` allocates and takes a lock, and this is called from the
/// hottest loop in the binary. The EXACT string `"1"`, the same idiom `VIKE_RECONCILE` uses — a
/// fuzzy truthy parse would make `VIKE_FRAME_LOG=0` turn the instrument ON.
fn frame_log_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var(FRAME_LOG_ENV).is_ok_and(|v| v == "1"))
}

impl eframe::App for App {
    fn persist_egui_memory(&self) -> bool {
        false // we own window placement (cascade + arrange); don't restore stale positions
    }

    /// Window-close teardown. One line by design: the whole sequence — the cooperative signal, the
    /// parallel per-venue feed shutdowns, the load-bearing sequential tail and the 1500 ms deadline
    /// that bounds all of it — is [`App::run_bounded_teardown`] in `app_lifecycle.rs`, which is
    /// what every `on_exit` citation in this crate actually points at.
    fn on_exit(&mut self) {
        self.run_bounded_teardown();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // ⚠ THIS METHOD IS A SPLIT, NOT A STUB — see `app_ui`'s module doc for the whole argument.
        // The ~2000-line frame body lives in `app_ui::{frame_begin, draw_chrome, draw_windows}`;
        // what stays behind are the two blocks that read the PROCESS ENVIRONMENT directly, because
        // `crates/vike-ops/tests/settings/settings_registry.rs` classifies every `env::var` read by FILE
        // PATH — `main.rs` / `src/bin/*` / `examples/*` earn `Layer::Binary`, and anything else is
        // a `Layer::Library` row the shrink-only `LIBRARY_PIN` ratchet refuses. That is the same
        // rule `App::new`'s `startup::StartupEnv` construction already follows, and it is why the
        // capture block and the `ArrangeEnv` construction below are inline here rather than a
        // fourth `app_ui` function.
        //
        // The five statements are in the ORDER the single body ran them, and the order is
        // load-bearing in one place worth naming: `draw_chrome` is what writes `self.desktop` (the
        // `CentralPanel` rect at its tail), and island 2's guard READS it — so the arrangement
        // island can never move above that call.
        // ── OPT-IN frame-rate instrument (`VIKE_FRAME_LOG=1`) ──────────────────────────────────
        //
        // MEASURED 2026-08-31: the same idle chart costs 10.9–12.5% CPU natively on a GPU and
        // 124–275% under lavapipe in a container. Varying the resolution ruled fill rate OUT — 4.7x
        // the pixels bought only ~1.3x the CPU, with a large floor at 640x480 — so the cost is
        // PER-FRAME, and the open question is how many frames each environment actually draws.
        // egui is reactive and nothing in this workspace requests a repaint per frame, so a high
        // rate would mean the presentation path is not throttling, not that the app asked for it.
        //
        // Off unless the variable is set, so a normal run pays one atomic load per frame and prints
        // nothing. It is a MEASUREMENT hook, not a feature: it answers "how many frames" and makes
        // no attempt to cap anything.
        if frame_log_enabled() {
            use std::sync::atomic::{AtomicU64, Ordering};
            use std::time::Instant;
            static FRAMES: AtomicU64 = AtomicU64::new(0);
            static WINDOW: std::sync::OnceLock<std::sync::Mutex<Instant>> =
                std::sync::OnceLock::new();
            let n = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
            let start = WINDOW.get_or_init(|| std::sync::Mutex::new(Instant::now()));
            if let Ok(mut t0) = start.lock() {
                let dt = t0.elapsed();
                if dt.as_secs_f64() >= 1.0 {
                    tracing::info!(
                        frames = n,
                        fps = format!("{:.1}", n as f64 / dt.as_secs_f64()),
                        "frame rate"
                    );
                    FRAMES.store(0, Ordering::Relaxed);
                    *t0 = Instant::now();
                }
            }
        }

        let (ctx, snap) = app_ui::frame_begin(self, ui);

        // Headless self-screenshot for parity QA (VIKE_SHOT=path): grab egui's OWN
        // framebuffer after the live feed paints, then exit. Occlusion-proof — no OS
        // capture race, no focus-steal, captures the real wgpu render.
        if let Ok(path) = std::env::var("VIKE_SHOT") {
            self.shot_n += 1;
            ctx.request_repaint();
            // QA: VIKE_APPMAX maximizes the whole app mid-run so the self-shot captures the
            // re-tiled (re-filled) workspace — exercises the maximize-transition re-tile path.
            if std::env::var("VIKE_APPMAX").is_ok() && self.shot_n == 80 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
            let dbg = format!("{path}.dbg");
            if self.shot_n == 1 {
                // Stay on top + focused: eframe only processes the screenshot readback on a
                // VISIBLE (non-occluded) frame, and VS Code keeps stealing top z-order.
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                    egui::WindowLevel::AlwaysOnTop,
                ));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            // Request periodically once data's loaded: when the foreground loop finally wins
            // a visible frame, eframe processes the pending request and posts the event.
            let start: u32 =
                std::env::var("VIKE_SHOT_FRAME").ok().and_then(|s| s.parse().ok()).unwrap_or(180);
            if self.shot_n >= start && self.shot_n.is_multiple_of(6) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            let shot = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(img) = shot {
                let [w, h] = img.size;
                let mut buf = image::RgbaImage::new(w as u32, h as u32);
                for (i, px) in img.pixels.iter().enumerate() {
                    buf.put_pixel(
                        (i % w) as u32,
                        (i / w) as u32,
                        image::Rgba([px.r(), px.g(), px.b(), 255]),
                    );
                }
                let r = buf.save(&path);
                let _ = std::fs::write(&dbg, format!("got {w}x{h}, save_ok={:?}\n", r.is_ok()));
                std::process::exit(0);
            }
            if self.shot_n > start + 900 {
                let _ = std::fs::write(&dbg, "TIMEOUT\n");
                std::process::exit(1);
            }
        }

        // The caption / menus / rail / status bar / desktop rect (`app_ui::draw_chrome`). `ctx` is
        // CLONED rather than moved because island 2 below still needs it — an `egui::Context` is an
        // `Arc` handle, so this is a refcount bump, and the original body held exactly one such
        // clone per frame (`ui.ctx().clone()`) for the same reason.
        app_ui::draw_chrome(self, ui, ctx.clone());

        if !self.did_initial_arrange && self.desktop.width() > 600.0 {
            // The whole first-frame decision — the VIKE_TOOL/VIKE_TOOLS QA capture arms, the
            // VIKE_ARRANGE parse and the open-count arrange-vs-maximize-lone choice — moved down
            // into the CI-gated `initial_arrange` (see that module's doc for why it is a planner
            // of its own rather than a seventh `window_spawn` arm). The seven env READS stay HERE,
            // in the binary, injected as `ArrangeEnv` — the same shape `startup_env` takes at
            // `App::new` — so every `VIKE_*` settings-registry row stays `vike-desktop`/`Binary`.
            let env = initial_arrange::ArrangeEnv {
                arrange: std::env::var("VIKE_ARRANGE").ok(),
                tool: std::env::var("VIKE_TOOL").ok(),
                tools: std::env::var("VIKE_TOOLS").is_ok(),
                cal_page: std::env::var("VIKE_CAL_PAGE").ok(),
                max: std::env::var("VIKE_MAX").is_ok(),
                min: std::env::var("VIKE_MIN").is_ok(),
                poly_cockpit_token: std::env::var("VIKE_POLY_COCKPIT_TOKEN").ok(),
            };
            let plan = initial_arrange::plan_initial_arrange(
                &env,
                &self.wins,
                self.desktop.min,
                self.next_win_n,
            );
            // Applied in the ORDER `InitialArrangePlan` documents — the exact order the inline
            // block performed these steps: close-all, counter, then per spawn feed → push →
            // resolver → cal page, then the arrange, the remember, and the two QA hooks last.
            if plan.close_existing {
                for w in self.wins.iter_mut() {
                    w.open = false;
                }
            }
            self.next_win_n = plan.next_win_n;
            for spawn in plan.spawns {
                if let Some(f) = &spawn.ensure_feed {
                    self.ensure_feed_on(&ctx, &f.venue, &f.symbol, &f.interval, f.asset_class);
                }
                let wid = spawn.win.id;
                let resolve = spawn.resolve_poly_token;
                self.wins.push(spawn.win);
                if let Some(r) = resolve {
                    self.spawn_poly_token_resolver(r);
                }
                if let Some(pg) = spawn.cal_page {
                    self.tool_views.entry(wid).or_default().cal_page = pg;
                }
            }
            initial_arrange::apply_arrange_action(&mut self.wins, self.desktop, plan.arrange);
            if let Some(mode) = plan.remember {
                self.last_arrange = Some(mode);
            }
            initial_arrange::apply_qa_hooks(
                &mut self.wins,
                self.desktop,
                plan.qa_maximize_first_open,
                plan.qa_minimize_next_three,
            );
            self.did_initial_arrange = true;
        }

        // The window arena + every post-loop drain (`app_ui::draw_windows`). Last use of both
        // `ctx` and the frame snapshot, so both move.
        app_ui::draw_windows(self, ctx, snap);
    }
}

/// Fetch real country-flag PNGs (flagcdn.com, free, no key) off-thread, decode to ColorImage,
/// and stream them back to the UI to load as textures. Accurate for EVERY country (drawn flags
/// can't be — EU's 12 stars, Australia's Union Jack + Southern Cross, US 50 stars, etc.).
fn spawn_flag_fetcher(
    tx: std::sync::mpsc::Sender<(String, egui::ColorImage)>,
    wake: impl Fn() + Send + 'static,
) {
    std::thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .into();
        // every iso2 the calendar's currency→country map can yield
        const ISOS: &[&str] = &[
            "us", "eu", "gb", "jp", "au", "nz", "ca", "ch", "cn", "in", "br", "za", "kr", "mx",
            "ru", "tr", "id", "sa", "sg", "hk", "se", "no", "de", "fr", "it", "es",
        ];
        for iso in ISOS {
            let url = format!("https://flagcdn.com/w40/{iso}.png");
            let Ok(mut resp) = agent.get(&url).call() else { continue };
            let Ok(bytes) = resp.body_mut().read_to_vec() else { continue };
            let Ok(img) = image::load_from_memory(&bytes) else { continue };
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let ci =
                egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
            let _ = tx.send((iso.to_string(), ci));
            wake();
        }
    });
}

/// Fetch each news provider's favicon (Google s2, 64px) keyed by SOURCE NAME, so the News list
/// can show a real logo instead of the colored-initial fallback.
fn spawn_logo_fetcher(
    tx: std::sync::mpsc::Sender<(String, egui::ColorImage)>,
    wake: impl Fn() + Send + 'static,
) {
    std::thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .into();
        const PROVS: &[(&str, &str)] = &[
            ("CoinDesk", "coindesk.com"),
            ("Cointelegraph", "cointelegraph.com"),
            ("Decrypt", "decrypt.co"),
            ("CryptoSlate", "cryptoslate.com"),
            ("BeInCrypto", "beincrypto.com"),
            ("Bitcoin Magazine", "bitcoinmagazine.com"),
            ("NewsBTC", "newsbtc.com"),
            ("CoinJournal", "coinjournal.net"),
            ("FXStreet", "fxstreet.com"),
            ("ForexLive", "forexlive.com"),
            ("FXEmpire", "fxempire.com"),
            ("Investing.com", "investing.com"),
            ("Investing.com FX", "investing.com"),
            ("MarketWatch", "marketwatch.com"),
            ("CNBC", "cnbc.com"),
            ("Seeking Alpha", "seekingalpha.com"),
        ];
        for (name, domain) in PROVS {
            let url = format!("https://www.google.com/s2/favicons?domain={domain}&sz=64");
            let Ok(mut resp) = agent.get(&url).call() else { continue };
            let Ok(bytes) = resp.body_mut().read_to_vec() else { continue };
            let Ok(img) = image::load_from_memory(&bytes) else { continue };
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            if w < 8 || h < 8 {
                continue; // Google returns a 16px globe placeholder for unknown domains — skip
            }
            let ci =
                egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
            let _ = tx.send((name.to_string(), ci));
            wake();
        }
    });
}

// ⚠ TOMBSTONE — `spawn_catalog_fetcher` stood here. It fetched `DeribitCatalog` on EVERY start,
// sent the result once and cached nothing, so the `vike_catalog::persist` cache — whose own doc has
// said "Loaded instantly at startup; rewritten only on an explicit user refresh" since it was
// written — was referenced by nothing in the tree. `vike_app_core::data::catalog_refresh::CatalogRefresh`
// replaces it: same channel, same providers, plus the cache, the per-venue stamp and the button.
// It lives one crate down because `vike-desktop` is outside the derived CI roster, so a refresh
// spelled here would be compiled by `app-check` and tested by nothing.

/// The order-entry preview caps, derived ONCE from the deployment's policy ceiling
/// (the `policy.max_notional_per_order` row) by [`resolve_settings`] and
/// read by the per-frame dispatch block.
///
/// A process-wide `OnceLock` rather than an `App` field on purpose: the value is a property of the
/// MACHINE (its `policy` rows), not of a window, and it must be identical for every dispatch path
/// in the process. It replaces a per-frame `env::var` read of a variable that no longer exists, so
/// this is strictly less global state than before, not more.
///
/// `get_or_init(default)` at the read site keeps the GUI permissive rather than panicking if some
/// future entry point reaches the dispatch block without having run `main` (a test harness, say) —
/// exactly what an absent `policy.max_notional_per_order` row yields anyway.
static POLICY_ORDER_LIMITS: std::sync::OnceLock<order_entry::OrderLimits> =
    std::sync::OnceLock::new();

/// The WHOLE resolved [`vike_config::Settings`], set by the same [`resolve_settings`] call that
/// fills [`POLICY_ORDER_LIMITS`].
///
/// It held only the `Policy` until the file layer was wired: this GUI reads these settings out of
/// it — `config.store_root` (the Studio's bar store), `config.datahub_addr` and
/// `config.backtest_addr` (the services it dials), `preferences.chart_style`,
/// `preferences.log_level`/`log_file_level` + `config.log_dir` (the `vike_log::LogConfig`), and the
/// five appearance preferences (`preferences.theme` and its four siblings, `App::new`'s install) —
/// on top of the `Policy` the twelve-venue live mount projects onto `vike_mount::MountPolicy`. Each
/// is still overridden by its own environment variable inside the loader, except the five
/// appearance rows, which have none by ruling (design system spec §5).
///
/// Two statics rather than one because they are read from genuinely different places at genuinely
/// different times — the per-frame dispatch block and the one-shot mount — and both are filled by
/// ONE load: a second `vike_config::load` in this process could disagree with the first.
///
/// ⚠ The `Policy` this projects onto a twelve-venue mount is READ but no longer MOUNTED here: the
/// desktop links no `vike_mount` (which `vike-run` merged into, decision 0098), so the ceilings it
/// loads feed the Connections screen and the order-entry preview cap, and the mount they used to
/// govern belongs to the daemon.
static SETTINGS: std::sync::OnceLock<vike_config::Settings> = std::sync::OnceLock::new();

/// **WHICH `<project>/settings` answered** — the directory [`resolve_settings`]'s walk landed on,
/// kept so [`main`]'s startup disclosure describes the one this process really loaded from.
///
/// It is stored rather than re-derived for the reason the disclosure exists at all: on the CI box the
/// walk answered with an unrelated directory and a daemon ran with no policy and NO CREDENTIALS,
/// every venue silently on paper. A second walk in the reporting path could land somewhere else and
/// report a directory nothing read — which is the same defect wearing a reassuring log line.
///
/// It is also what the ROLLING LOG FILE hangs off (`LogConfig::project_dir` in [`main`], as
/// `<this>/state/logs`), what [`STATE_DIR`] is joined from, and what
/// [`install_user_indicators`]'s `user_data/` sibling is taken beside. All three used to be walks
/// of their own — `project_log_dir(&cwd)`, `project_state_dir(&c)`,
/// `project_user_data_dir_from(.., &cwd)` — and none of those honours `VIKE_SETTINGS_DIR`, so under
/// the override the log file, the strategy-state sidecars and the indicator library each named
/// whatever project the working directory sat above while the block describing them named another.
///
/// `None` is a legitimate answer (no project above the working directory) and is exactly what the
/// disclosure has to say out loud, so it is an `Option` inside the `OnceLock` rather than an unset
/// cell.
static SETTINGS_DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();

/// **`<project>/settings/state` as the SAME walk resolved it** — `vike_boot::Booted::state_dir`,
/// stored so nothing walks for it a second time.
///
/// It is a separate cell rather than a `SETTINGS_DIR.join(STATE_SUBDIR)` at the call site because
/// the derivation belongs to `vike-boot`, which already performs it for the log home: spelling it
/// twice is how two answers start. `None` inside the cell is legitimate (no project above the
/// working directory); an UNSET cell means [`resolve_settings`] never ran, which is a test harness,
/// and the readers below fall back exactly as they did before any project existed.
static STATE_DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();

/// The resolved settings, or the pure code defaults for an entry point that never ran [`main`]
/// (a test harness). Permissive-by-default in exactly the way an absent settings directory is,
/// so a missing `main` can never be MORE restrictive than a real one.
fn settings() -> &'static vike_config::Settings {
    SETTINGS.get_or_init(vike_config::Settings::default)
}

/// The REAL process environment, swept ONCE at startup by [`resolve_settings`] — where the settings
/// directory (`VIKE_SETTINGS_DIR`) is named, when a deployment names it. See
/// [`workspace_credentials`].
///
/// A `OnceLock` rather than a value threaded through `App`: the credential reads in this file sit
/// in unrelated places (the `--observe` arm, the per-frame Connections tool, the Studio's chat
/// keys), and threading a map into a per-frame draw call would be a far larger change than the
/// migration itself. What the settings rule actually forbids is a LIBRARY reading process state its
/// caller cannot see; this is the binary, reading it once, at the root. (Two of the readers this
/// paragraph used to name — the FAT twelve-venue mount and the catalog-fetcher THREAD's
/// credential-gated providers — went with the venue bridges.)
static PROCESS_ENV: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();

/// **The credential store (the settings database; `<project>/settings/secrets.env` only on a box
/// that has not migrated), plus the NODE store beside it.**
///
/// The one place this binary asks "what credentials do I have" — the `--observe` arm, the
/// per-frame Connections tool and the Studio's chat keys all come through here. ⚠ It no longer
/// ARMS anything: with no local mount, a venue key present in the store buys this process nothing
/// (the Connections screen reports the policy ceiling and says so).
///
/// ⚠ **The two TRADEHUB node keys do NOT come out of the venue store above** — they come from
/// `<project>/settings/node.env`, with the warned legacy fallback, exactly as `vike-cli` and the
/// daemon resolve them (`docs/decisions/0051-node-keys-live-in-their-own-store.md`). That whole
/// resolution is `vike_app_core::backend::backend_registry::fill_node_keys`, one crate down where CI runs
/// it; what is left here is the call, which needs the boot walk's answer and this binary's one
/// environment sweep. Until it existed the GUI was the one consumer of that decision that had not
/// been migrated: the pair had to be kept duplicated in `secrets.env` or every reconnect answered
/// `tradehub observe auth denied: bad mac`.
///
/// An unreadable store returns an EMPTY map with a `tracing::error!`, so a credential file that
/// cannot be opened drops every venue to paper instead of being papered over.
/// `crates/vike-bridge-core/tests/credential_chain_roots.rs` gates it.
///
/// ⚠ **That degradation is right for an EXEC path and wrong for a path that RENDERS A COUNT**, so
/// this function has a twin that keeps the verdict — [`workspace_credentials_checked`]. Use it
/// wherever the answer is drawn on screen; this one where an empty map simply means paper.
///
/// The FILE read stays per call, deliberately: the Connections tool re-reads every frame so the
/// grid stays live after an in-app save or an external edit. Only the process-environment sweep is
/// cached.
fn workspace_credentials() -> HashMap<String, String> {
    workspace_credentials_checked().0
}

/// [`workspace_credentials`], plus whether the credential store could actually be OPENED.
///
/// ⚠ **ONE read, two answers** — never two reads. The map and the verdict have to describe the
/// same open of the same file, or the Connections tool could render a count taken from one read
/// beside a health taken from another.
///
/// The reason it exists: `load_workspace_secrets_from_env` is documented INFALLIBLE. A store that
/// exists and cannot be opened logs an error and returns an EMPTY map, byte-identical to the map
/// an unconfigured box produces. The Connections tool folds that map into `N set` — so without
/// this, an unreadable store renders a confident `0 set` — a permissions bug wearing the
/// not-configured answer, which the root `CLAUDE.md`'s "Credentials & the live gate" forbids by
/// name, printed as a number.
/// `vike_connections::StoreHealth` is the verdict; the panel renders no count at all for the
/// `Unreadable` arm.
fn workspace_credentials_checked()
-> (HashMap<String, String>, vike_bridge_core::credentials::StoreHealth) {
    let env = PROCESS_ENV.get_or_init(|| std::env::vars().collect());
    let (mut creds, health) =
        vike_bridge_core::credentials::load_workspace_secrets_from_env_checked(env);

    // ⚠ THE OBSERVE KEY MAY ALSO COME FROM THE ENVIRONMENT, and this gap-fill is what makes the
    // shipped thin-client image work at all.
    //
    // `deploy/docker/Dockerfile.thin`'s entrypoint accepts `VIKE_TRADEHUB_OBSERVE_KEY` from the
    // environment, says so ("observe key from the environment") and starts. This binary then read
    // it from the CREDENTIAL STORE only, found nothing, signed with an empty key, and the node
    // answered `bad mac` — for ever, at the reconnect cadence, with the daemon logging
    // `handshake refused` on its side. Measured on the published 0.1.16 image: the container ran,
    // exited nothing, and never connected. The image asked for the key one way and the program
    // read it another.
    //
    // A container is exactly the shape that has no store: the thin client is a VIEWER, and
    // mounting a project folder to carry one key is a heavy answer to a small question. Reading it
    // here is allowed where a library reading it would not be — this is the composition root, and
    // it is the same sweep every other read on this path already comes from.
    //
    // ⚠ The SETTINGS DIRECTORY is the boot walk's answer and nothing else — never a second
    // resolver call, which would be `$VIKE_SETTINGS_DIR`-blind. It is `None` for the ONE call that
    // happens before the walk has answered: `vike_boot::boot`'s own arming refusal, which reads
    // VENUE keys and no node key at all.
    vike_app_core::backend::backend_registry::fill_node_keys(
        &mut creds,
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
        env,
    );
    (creds, health)
}

/// **WHERE the two GUI surfaces that WRITE credentials put them, and where the write is RECORDED** —
/// the Connections editor's Save arm and the Data Manager's Polymarket proxy box.
///
/// Both homes are DERIVED from this binary's ONE boot walk ([`SETTINGS_DIR`] / [`STATE_DIR`]) rather
/// than found again, which is a FIX and not a tidy-up: both sites used to call a `_from`-less,
/// `$VIKE_SETTINGS_DIR`-BLIND resolver while [`workspace_credentials`] — the READ their grids render
/// from — honours the override, so a deployment naming its settings directory showed one project's
/// keys and saved into another's. `vike_connections::CredentialHome` is where that argument lives in
/// full, and it lives THERE rather than here because this crate is outside the CI roster: the
/// resolution is covered by that crate's own suite, and the shell keeps only the wiring.
///
/// `Proc::current` reads `current_exe`, so the whole thing is resolved once per process rather than
/// per write; the INSTANT is not cached, because the journal reads no clock and every record stamps
/// its own.
fn credential_home() -> &'static vike_connections::CredentialHome {
    static HOME: std::sync::OnceLock<vike_connections::CredentialHome> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        vike_connections::CredentialHome::resolve(
            SETTINGS_DIR.get().and_then(Option::as_ref).map(std::path::PathBuf::as_path),
            STATE_DIR.get().and_then(Option::as_ref).map(std::path::PathBuf::as_path),
            vike_model::change_journal::Proc::current(env!("CARGO_PKG_VERSION")),
        )
    })
}

/// **The Data Manager's Venues (arming) tab's inputs** — the one place this binary answers "what
/// would each venue actually mount", built fresh while that sub-tab is visible and nowhere else.
///
/// Three resolutions, each deliberate:
///
/// * the CEILINGS come from a FRESH read of the `policy.venues` rows, not from [`settings`]. That
///   cell is the BOOT-time load, and a tab whose Mode column still showed the pre-write value
///   after a write would read as broken. ⚠ The read is `tool_views::reload_venue_ceilings`, in
///   the library, and that is not a way around `crates/vike-boot/tests/one_owner.rs` — it is the
///   shape that gate is asking for: the directory is a parameter, so the function structurally
///   cannot WALK, which is the defect one-owner exists to prevent. A refused reload falls back to
///   the boot copy.
/// * the ROWS come from `vike_config::venue_arming::ceilings_only` — this binary links no mount
///   and must not CLAIM a tier. (It used to read `vike_mount::venue_arming`, the same
///   `would_mount_live_under` the local mount consulted; see the tombstone in the body.)
/// * the DIRECTORY is [`SETTINGS_DIR`], this binary's ONE boot walk. Never a resolver call from
///   inside the tab: the `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-blind, which is exactly
///   the defect `vike_connections::CredentialHome` exists to have closed for the credential store.
fn venue_arming_inputs(has_local_core: bool) -> tool_views::VenueArmingInputs {
    let dir = SETTINGS_DIR.get().and_then(Option::clone);
    let vars = workspace_credentials();
    let fresh = tool_views::reload_venue_ceilings(dir.as_deref());
    let venues = fresh.as_ref().unwrap_or(&settings().policy.venues);
    // ⚠ TOMBSTONE — this used to be a two-armed `#[cfg]`. Under `fat` the rows came from
    // `vike_mount::venue_arming(&vars, venues)`, the SAME `would_mount_live_under` the local mount
    // consulted, so the screen could claim a TIER (paper/demo/live) for each venue. The desktop
    // links no mount now (rulings 1 and 2), so it must not claim one: `ceilings_only` reports the
    // policy CEILING alone and every row renders `ArmingBlock::NoMountInThisBuild`. Anything that
    // would restore a tier here has to come back through the backend, which is the only thing in
    // this system that still mounts a venue.
    let rows = vike_config::venue_arming::ceilings_only(venues);
    // `false` where this read `cfg!(feature = "fat")` — see the tombstone at the `observes` call in
    // `main`: there is no local-core build for the arm table to distinguish any more, so every
    // desktop is the feed-less observer.
    let mode = vike_app_core::backend::split_plane::app_mode(false, !has_local_core)
        .unwrap_or(vike_app_core::backend::split_plane::AppMode::ObserveOnly);
    tool_views::VenueArmingInputs::new(rows, &vars, dir, mode)
}

/// The tool windows' third-party data-provider keys — `FINNHUB_API_KEY` (earnings) and
/// `FMP_API_KEY` (dividends), for the calendar day-strip. Resolved HERE, at the root, from the two
/// tiers this binary already owns: the process environment first, then [`workspace_credentials`].
///
/// The read is here rather than in `vike_app_core::tools` for the settings rule: a library takes
/// configuration as parameters. Before this, the fetch thread read process env AND a CWD-relative
/// `./.env` — the last store outside `<project>/settings/`, and one that resolved to a different
/// file for every directory the app was launched from. `vike_app_core::tools::ToolApiKeys` carries
/// the migration note; there is deliberately no legacy fallback.
///
/// Called ONCE, from `App::new`. These are read-only market-metadata keys, so a store edit taking
/// effect at the next launch is the whole cost of not re-reading.
fn tool_api_keys() -> tools::ToolApiKeys {
    tools::ToolApiKeys::resolve(
        PROCESS_ENV.get_or_init(|| std::env::vars().collect()),
        &workspace_credentials(),
    )
}

/// Load `<project>/user_data/indicators/*.rhai` ONCE and install the result on the ONE seam that
/// still consumes it: the chart's ƒx picker.
///
/// ⚠ **ONE load, deliberately — this is why the crate does not call
/// `vike_script::load_and_install_user_indicators`.** That helper hands back only messages; this
/// root needs the REPORT (`chart_studies()` for the chart, and the diagnostics for the log below).
/// Loading twice would compile every user file twice and — the visible half — log every rejection
/// twice, which reads as two broken files rather than one.
///
/// ⚠ **ONE directory resolution, and it stays load-bearing even with one consumer.** A user who
/// points `VIKE_USER_DATA_DIR` at a relocated library must see the same set here as `vike-cli`
/// does; an absent row and a file that failed to compile look identical from the picker, so the
/// load side is the only place that can say which.
///
/// ⚠ **TOMBSTONE — there used to be a SECOND install, and this doc used to argue the split.** The
/// Rhai STRATEGY bindings (`vike_script::install_user_indicators(report.prototypes())`) were
/// installed here under `fat`, on a reachability argument: only a build with a local trading core
/// had a strategy runner an indicator could be called from. The desktop has no local core any more
/// (rulings 1 and 2), so that consumer is gone and only the chart half remains. vike-script stays
/// an unconditional dependency — that is what makes the chart half possible, and it always was.
///
/// ⚠ **Called AFTER [`vike_log::init`], unlike every other startup resolution in this file.** Every
/// rejected file must be REPORTED — from inside a strategy an indicator that failed to load is
/// indistinguishable from a typo in the call, and from inside the picker it is indistinguishable
/// from one that was never written; both are a silent absence, so the load side is the only place
/// that can say which — and `tracing` needs a subscriber. [`resolve_settings`] carries its warnings
/// as DATA instead precisely because it CANNOT wait: it resolves the log destination and both log
/// levels, so it has to run first. Nothing here does: no script is compiled between the subscriber
/// and the window (the Studio is built inside `App::new`, and only when a user opens it), so the
/// report is simply emitted where it is produced. Still BEFORE any window, so the picker is
/// complete the first time it opens.
///
/// ⚠ **Nothing here is fatal.** One half-edited file must not stop the application from starting,
/// which is strictly worse than starting without that one study.
///
/// `VIKE_USER_DATA_DIR` names the directory outright and beats the walk, exactly as it does for
/// `vike-cli` — a user who points both at one strategy library must not get their indicators in one
/// and not the other. The read is a `.get` on the sweep [`resolve_settings`] already owns, per the
/// settings-registry rule that only a binary reads the environment.
///
/// ⚠ **The directory is the SIBLING of [`SETTINGS_DIR`], not a fresh walk.**
/// `project_user_data_dir_from`'s fallback walks, and that walk does not honour
/// `$VIKE_SETTINGS_DIR` — so an operator who relocated the project got their settings and
/// credentials from the named directory and their indicator library from whatever the working
/// directory sat under. `user_data_dir_beside` takes the answer this process already has.
fn install_user_indicators() {
    let vars = PROCESS_ENV.get_or_init(|| std::env::vars().collect());
    // `None` = no project above the working directory, which is the ordinary state of a binary run
    // from somewhere else entirely. Nothing to read, nothing to say.
    let Some(user_data) = vike_model::paths::state_path::user_data_dir_beside(
        vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
        SETTINGS_DIR.get().and_then(|d| d.as_deref()),
    ) else {
        return;
    };
    let report = vike_script::load_user_indicators(
        &user_data.join(vike_model::paths::state_path::INDICATORS_SUBDIR),
    );
    for d in &report.diagnostics {
        tracing::warn!("indicator not loaded — {}", d.message());
    }

    // (1) the CHART. `chart_studies()` leaks one descriptor per indicator, so it is called here,
    // once, and never per frame or per window.
    let studies = report.chart_studies();
    let count = studies.len();
    if let Err(e) = vike_chart::indicators::install_user_studies(studies) {
        // Only reachable when something else in this process installed first — a once-per-process
        // call made twice means two places believe they own the set, and the message says so.
        tracing::error!("user chart studies: {e}");
    } else if count > 0 {
        let dir = report.root.display();
        tracing::info!(count, %dir, "user indicators mounted in the ƒx picker");
    }

    // ⚠ TOMBSTONE — (2) the STRATEGY bindings are GONE. This function used to make a second
    // install, `vike_script::install_user_indicators(report.prototypes())`, which bound the user's
    // indicators into the Rhai STRATEGY namespace. It was gated on the `fat` feature by
    // reachability: only a build with a local trading core had a strategy runner an indicator
    // could be called from. The desktop has no local core any more (rulings 1 and 2 — orders leave
    // only through the backend, and this binary opens no venue socket), so nothing in this process
    // can call a strategy indicator and the install had no consumer left. `report.prototypes()`
    // consequently has no reader in this crate; the REPORT is still what the load returns, so the
    // seam is one call away if a strategy runner ever comes back.
}

/// **Where the Studio's Research pane reads studies from and mints runs into** — everything
/// `vike_studio::StudyHost` carries, or `None` when this process has no project above it.
///
/// Resolved exactly the way [`install_user_indicators`] resolves the indicator library, and for its
/// two reasons: `VIKE_USER_DATA_DIR` names the directory outright and beats the walk, and the
/// directory is the SIBLING of [`SETTINGS_DIR`] rather than a fresh walk —
/// `crates/vike-boot/tests/one_owner.rs` forbids a booted root from walking a second time and names
/// `user_data_dir_beside` as the answer. The read is a `.get` on the sweep [`resolve_settings`]
/// already owns, per the settings-registry rule that only a binary reads the environment.
///
/// `None` here is not a failure: `vike_studio::NO_HOST` is the sentence the pane renders instead,
/// and it names the two ways to fix it.
fn studio_study_host() -> Option<vike_studio::StudyHost> {
    use vike_model::paths::state_path::{
        PROJECT_TMP_DIR, RESEARCH_SUBDIR, RUNS_SUBDIR, STUDIES_SUBDIR,
    };
    let vars = PROCESS_ENV.get_or_init(|| std::env::vars().collect());
    let user_data = vike_model::paths::state_path::user_data_dir_beside(
        vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
        SETTINGS_DIR.get().and_then(|d| d.as_deref()),
    )?;
    // `<project>` is `user_data`'s parent BY CONSTRUCTION, so the scratch home is derived from an
    // already-resolved path rather than by calling `project_tmp_dir_from` — which is a WALK row in
    // `crates/vike-boot/tests/one_owner.rs` and therefore not a booted root's to call. Same
    // component-strip `user_data_dir_beside` itself performs, which is why that function is in
    // that file's NOT_A_SECOND_ANSWER table rather than its WALK one.
    let project = user_data.parent()?.to_path_buf();
    Some(vike_studio::StudyHost {
        studies_root: user_data.join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR),
        runs_root: user_data.join(RUNS_SUBDIR),
        // A study's scratch, under the project scratch home — `<project>/tmp` is where everything
        // this program touches and does not keep belongs (#1487). The runner creates it; nothing
        // deletes it, because the owner is whoever chose the path, which is this line.
        scratch: project.join(PROJECT_TMP_DIR).join("study"),
        // The BINARY, spelled literally: `vike_model::runs::RunManifest::produced_by` carries
        // why that is not `CARGO_PKG_NAME`.
        produced_by: "vike-app".to_string(),
        // ⚠ ...and no commit, deliberately. This binary reaches `vike-buildinfo` only THROUGH
        // `vike-boot` (its own manifest says the direct edge was replaced by that one), so it
        // cannot name `GIT_SHA` without taking the edge back. `RunManifest::git_sha`'s doc calls
        // `None` the honest answer for exactly this shape and names `backtest` as the other
        // producer in it. The day this binary takes that edge, this is one line.
        git_sha: None,
    })
}

/// Startup step 0, before ANYTHING else — INCLUDING the log subscriber: refuse a stale environment,
/// then resolve this machine's settings into [`SETTINGS`] and its order-entry preview cap into
/// [`POLICY_ORDER_LIMITS`].
///
/// Two separable jobs, deliberately in this order:
///
/// 1. **Refuse** — `VIKE_MAX_ORDER_NOTIONAL` is no longer read (Phase 5 of the
///    settings-unification design). An operator who set it believes a ceiling is active; a build
///    that quietly ignored it would trade UNCAPPED while they believed otherwise, which is worse
///    than either keeping the variable or refusing to start. `vike_config::refuse_removed_env`
///    produces the message, naming the file and key that replace it.
/// 2. **Resolve** — load `<project>/settings`. A missing file is not an error (it is the permissive
///    default, byte-identical to a pre-Phase-5 run with nothing set); a file that exists and is
///    BROKEN is, because a deployment that wrote a ceiling and typo'd it must not silently run
///    without one.
///
/// The BINARY owns the environment read (`std::env::vars()`), per the settings-registry rule;
/// `vike_config` itself never touches `std::env`.
///
/// ⚠ **It logs NOTHING**, and that is why it can run before `vike_log::init`: the log DIRECTORY and
/// both log LEVELS are themselves settings, so a subscriber built before this could only ever honour
/// the environment. Everything it would have said is emitted by [`main`] the instant the subscriber
/// exists — including the `warnings` `vike_config` deliberately returns as DATA rather than logging
/// (a library must not write to a caller's stderr on its own initiative), because a clamp nobody is
/// told about is a limit the operator believes they set and does not have.
fn resolve_settings() -> Result<vike_boot::Booted, String> {
    let vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    // The SAME sweep serves the credential chain (see [`PROCESS_ENV`] / [`workspace_credentials`]),
    // so the settings directory is resolved once, here, at the root — and no later credential read
    // has to touch `std::env` again. It is `set` BEFORE the boot below because
    // [`workspace_credentials`], which the boot calls, reads it.
    let _ = PROCESS_ENV.set(vars.clone());
    let cwd = std::env::current_dir().ok();
    // ⚠ THE ORDER BELOW IS `vike-boot`'s, not this file's, and four other composition roots run the
    // same one. It used to be written out here — refuse, arm-check, walk, load — and in four other
    // roots besides, which is what made the CI box's failure expensive: the walk happening in five
    // places is five places to fix and five chances for two of them to disagree.
    let booted = vike_boot::boot(&vike_boot::BootSpec {
        env: &vars,
        cwd: cwd.as_deref(),
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Refuse,
        // ⚠ REPORT rather than refuse: this root mounts no venue, signs no order and runs no core
        // (rulings 1 and 2 of the desktop/daemon rename), so refusing to open a window buys
        // nothing an operator can use. The finding rides `Settings::warnings` and
        // `Settings::store_refusal`, which this binary surfaces once a subscriber exists.
        settings: vike_boot::SettingsLoad::Load,
        // the Venues tab renders these ceilings and the Connections editor writes this store; a
        // store the GUI cannot migrate is a mark and a warning, never a window that will not open.
        ceilings: vike_boot::Ceilings::InterpretOrMark { now_ms: vike_model::now_ms() },

        // ...and the credential file may not ARM REAL MONEY. `secrets.env` is plaintext and parsed
        // last-wins, so appending ONE line to it — with no read access at all — would otherwise be
        // enough to flip this app's twelve-venue mount onto a live venue. `vike-boot` runs the
        // refusal at step 0, before the window, the feeds, the core and every mount, for that
        // reason. It does NOT change which sources arm a venue (decision 0095: the ceiling alone,
        // for binance/bybit/okx/hyperliquid); it makes an arming credential file STOP the process
        // rather than run it. See `vike_config::arming`.
        //
        // The LOADER is this binary's own ([`workspace_credentials`]), passed as a function:
        // `vike-boot` owns WHEN the store is opened and this binary owns HOW, which is what keeps
        // that crate free of `vike_bridge_core`'s transport stack — and what lets the FILE read
        // stay per call here, so the Connections tool's grid stays live after an external edit.
        //
        // ⚠ REPORTING, not refusing, for a venue setting still filed as a credential row (decision
        // 0095's Task 7): every root that mounts a venue refuses to start on one, and this root
        // mounts none — the same stance as the two arms above.
        credentials: vike_boot::Credentials::LoadReportingStrandedSettings(
            &workspace_credentials,
            "this root mounts no venue and reads no venue setting, so a stranded row changes \
             nothing it does; refusing would close a window over a row only the daemon reads, \
             whose repair (`vike-cli secrets move-venue-config`) another process runs",
        ),
        log_home: vike_boot::LogHome::UnderSettings,
        disclosure: vike_boot::Disclosure::Render,
    })?;
    let _ = POLICY_ORDER_LIMITS.set(order_entry::OrderLimits::with_max_notional(
        booted.settings.policy.max_notional_per_order,
    ));
    let _ = SETTINGS.set(booted.settings.clone());
    // …and the DIRECTORY it came from, for the rest of this file — see [`SETTINGS_DIR`]. It is the
    // boot's OWN walk, so the log file, the disclosure and every consumer below name one project.
    let _ = SETTINGS_DIR.set(booted.settings_dir.clone());
    // …and its `state/` sub-directory, likewise off that one walk — see [`STATE_DIR`]. The
    // strategy-state sidecar used to re-walk for it with a `$VIKE_SETTINGS_DIR`-BLIND resolver.
    let _ = STATE_DIR.set(booted.state_dir.clone());
    Ok(booted)
}

fn main() -> eframe::Result<()> {
    // `--install-desktop-entry` / `--uninstall-desktop-entry` (Linux): write or remove the launcher
    // entry and its icons, then exit — before the settings, the log and the window, none of which
    // it needs. `vike_app_core::ui::desktop_entry` is the whole of it.
    if let Some(code) =
        vike_app_core::ui::desktop_entry::run(std::env::args(), &std::env::vars().collect())
    {
        std::process::exit(code);
    }
    // SETTINGS FIRST — before the log subscriber, the window, any feed and any core.
    //
    // Two reasons, in order of weight: the log DIRECTORY (`config.log_dir`) and both log LEVELS are
    // settings, so a subscriber built before the load could only ever honour the environment; and a
    // stale risk-ceiling variable must stop the process before it does anything at all. `eprintln!`
    // rather than `tracing::error!` on the failure path because there is deliberately no subscriber
    // yet — the message reaches stderr either way. Exit code 2 matches the thin-build guard below.
    let booted = match resolve_settings() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("vike-app: {e}");
            std::process::exit(2);
        }
    };

    // THREE layers of this config are settings rather than defaults now, and each is still beaten by
    // its environment variable INSIDE `vike_log::init` — the same precedence stated twice, since
    // `vike-config` already resolved env > file > default and the two orders agree.
    //
    // `project_dir` is the DEFAULT log directory: `<project>/settings/state/logs`, beside every
    // other file the program writes and no human edits. It sits BELOW `config.log_dir` (a value a
    // human wrote beats one the program derived) and above vike-log's `<exe_dir>/logs` last resort —
    // `target/debug/logs/vike-app.<date>` in a checkout, which `cargo clean` deletes.
    //
    // ⚠ It is [`vike_boot::Booted::log_home`] — derived from the directory [`resolve_settings`]
    // ALREADY resolved — rather than a second `project_log_dir(&cwd)` walk, which is
    // `VIKE_SETTINGS_DIR`-BLIND. Under that override the two disagreed: the startup block below
    // would name the overridden project while this file landed under whatever the CWD happened to
    // sit above. A disclosure that does not describe the file it is written into is the same "two
    // walks, two answers" defect the block exists to surface, so ONE walk DECIDES in this binary
    // and every project-relative path is derived from it — this log home, [`STATE_DIR`]'s strategy
    // -state sidecar and [`install_user_indicators`]'s `user_data/` sibling alike. (The credential
    // store is the declared residual: [`workspace_credentials`] resolves it inside
    // `vike_bridge_core`, from the same `$VIKE_SETTINGS_DIR` through the same pure resolver, so it
    // is one more CALL and never a second ANSWER — see `crates/vike-boot/src/lib.rs`'s module doc.)
    let _log_guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "vike-app".to_string(),
        console_level: settings().preferences.log_level.clone(),
        file_level: settings().preferences.log_file_level.clone(),
        dir: settings().config.log_dir.clone(),
        project_dir: booted.log_home.clone(),
        ..Default::default()
    });
    // WHICH BINARY IS THIS — the first line in the log, and it REPLACES a bare `vike-app starting`.
    // `Booted::identity_line` still starts with the name, so the marker is unchanged, and it now
    // also names the COMMIT this build came from: a release binary was once built on a test clone
    // from a bare repo four commits behind `main` and nearly installed on the live recorder
    // (`crates/vike-buildinfo/src/lib.rs` carries the incident).
    tracing::info!("{}", booted.identity_line);

    // …and the KILL SWITCH's default path takes its project from that SAME one walk.
    //
    // `vike_bridge_core::halt` is a library and may not read `$VIKE_SETTINGS_DIR` for itself
    // (`crates/vike-ops/tests/settings/settings_registry.rs`'s `LIBRARY_PIN` is a ratchet that may shrink
    // and never grow), so the override reaches rung 2 — `<project>/settings/state/HALT` — only as a
    // PARAMETER from here. Without this the sentinel's default was decided by the WORKING DIRECTORY
    // even under the override, which is the one thing that variable exists to make irrelevant.
    //
    // Placed HERE rather than inside [`resolve_settings`] for one reason: it can fail (a
    // declaration that arrives after the path was already resolved changes nothing, and saying so
    // is the whole point), and this is the first line at which there is a subscriber to say it
    // through. Nothing between the boot and this point resolves the sentinel — every caller of
    // `halt_path_from_env` is a venue mount or the paper book, all of them below.
    if let Err(e) = vike_bridge_core::halt::declare_project_state_dir(booted.state_dir.clone()) {
        tracing::error!("{e}");
    }

    // …and so does the WORKSPACE family — `workspace.json`, `layouts/`, `last_layout.txt` and the
    // backend registry's `backends.json`, which all hang off one base directory.
    //
    // Same defect, same cure, and it was LIVE rather than latent: launching with
    // `VIKE_SETTINGS_DIR=<project>/settings` from another directory read the CREDENTIALS out of
    // that project while resolving the BACKEND REGISTRY from wherever the working directory sat
    // above — the client reported "no --observe and no active backend" with a valid
    // `backends.json` sitting in the named settings directory. `$VIKE_STATE_ROOT` still outranks
    // this, and a declaration that MOVES the family returns the notice below rather than reading a
    // different file in silence. Placed here for `declare_project_state_dir`'s own reason: this is
    // the first line at which a diagnostic has a subscriber to go to, and nothing above it reads
    // the family (`App::new`'s registry load and the `--observe` scan are both below).
    match vike_app_core::ui::workspace::persist::declare_project_state_dir(booted.state_dir.clone())
    {
        Ok(Some(moved)) => tracing::warn!("{moved}"),
        Ok(None) => {}
        Err(e) => tracing::error!("{e}"),
    }

    // THE STUDIO'S SWEEP POOL IS NO LONGER THIS PROCESS'S TO CONFIGURE, and the call that
    // stood here is gone with it. `preferences.sweep_threads` is NOT gone.
    //
    // It used to be installed here because the Studio's `Backend::Local` ran the sweep IN THIS
    // PROCESS, entering `vike_backtest::harness`'s bounded rayon pool through
    // `vike_studio_core::run_paramscan_slice` -> `map_bounded` - a pool that reads its worker
    // count at CONSTRUCTION, deep inside a fan-out no caller here could reach, so the number
    // had to arrive through a process-wide handle a root installed rather than as a parameter.
    // That is why `vike-studio` re-exposed the harness's `install_sweep_threads` at all: this
    // binary does not link vike-backtest and could not name the crate owning the pool.
    //
    // `Backend::Local` is deleted. Every sweep now runs in the COMPUTE DAEMON, which installs
    // the same preference in its own process (`vike_backtest::backtest_cli`'s
    // `install_sweep_threads(settings.preferences.sweep_threads)`), and `run_paramscan_slice`
    // is still what executes it - reached over the wire through `vike_studio_core::wire_run`
    // rather than by a direct call from here. So the knob did not lose its pool; the pool
    // moved to the process that owns the work, and the setting followed it.
    //
    // A cap installed HERE would configure a pool this process never enters - the defect
    // `vike_config::CONSUMPTION` exists to refuse, wearing its other face: not a key nothing
    // reads, but a read that reaches nothing. That table's needle names the daemon's call and
    // always did (it argued at the row that the evidence should stand whether or not the GUI
    // is built), so it is unaffected by this deletion.

    // …and only NOW can the loader's own non-fatal resolutions be emitted: `resolve_settings` runs
    // before there is a subscriber, so it returns them rather than logging them.
    for w in &settings().warnings {
        tracing::warn!("settings: {w}");
    }
    // WHAT WAS RESOLVED, and FROM WHERE. This REPLACES the four-field line that stood here: that one
    // named the three values this binary happens to consume, and the failure it exists for is the
    // one where NOTHING was consumed — where the useful facts are the settings DIRECTORY, whether
    // each file was there at all, and whether a credential store sits beside them (an absent one is
    // why every venue would be on paper). `Booted::boot_lines` (`vike_config::boot_lines`) renders
    // all of it, the same rows `vike-cli config show` prints, from the directory `resolve_settings`
    // actually loaded from — `vike-boot` hands the renderer its OWN walk's answer, so nothing here
    // can walk a second time. It NEVER opens the credential store, so no key name or value can
    // reach this log.
    for line in &booted.boot_lines {
        tracing::info!("{line}");
    }

    // …and the DURABLE anchor for the ceilings those lines just disclosed — one JSONL line per
    // start in `<state>/changes/`, actor origin `boot`.
    //
    // The disclosure above is the console/file copy and it does not survive:
    // `vike_log::DEFAULT_MAX_LOG_FILES` prunes the rolling file daily, and `VIKE_LOG_FILE_LEVEL`
    // (which every shipped unit on the server side sets to `warn`) silences the `info` layer the
    // boot lines ride. "What was the ceiling on the 14th" is not answerable from a log that is
    // gone; it is answerable from an append-only ledger.
    //
    // ⚠ **A BRACKET, not a detector.** Nothing here observes a hand edit of the `policy` rows of
    // the settings database — there is no watcher on it in the tree — and this record does not
    // pretend otherwise. Two consecutive anchors that DISAGREE prove something changed
    // between them, without claiming to know who or when;
    // `vike_boot::journal_boot_settings` carries the argument and the rate arithmetic.
    //
    // ⚠ It is [`vike_boot::Booted::state_dir`] and NOT `config.state_dir`: that one was the
    // STRATEGY-STATE sidecar knob (`config.state_dir`, still overridden by `$VIKE_STATE_DIR`
    // inside the loader), which relocates ONE family of files and is not this binary's state root.
    // The anchor belongs in the tree the rolling log is already in — `booted.log_home` is
    // `<state_dir>/logs`, one join under the very directory passed here. `None` — no project above
    // the working directory — writes NOTHING rather than inventing a ledger location.
    if let Some(Err(e)) = vike_boot::journal_boot_settings(
        booted.state_dir.as_deref(),
        &settings().policy,
        env!("CARGO_PKG_VERSION"),
        vike_model::now_ms(),
    ) {
        tracing::warn!("the boot anchor was not journalled: {e}");
    }

    // …and the user's own indicators, for the same reason the warnings above are emitted here: this
    // is the first moment a diagnostic has somewhere to go. See [`install_user_indicators`] for why
    // it is the one startup resolution that runs AFTER the subscriber rather than before it.
    //
    // ⚠ It runs BEFORE the `--observe` scan below, and that ordering used to matter for a reason
    // that is now moot: an observe session installed a set it would never use, which was accepted
    // rather than made mode-dependent, because a startup whose diagnostics depend on which mode
    // was requested is a worse failure than a wasted read. Every session is an observe session now,
    // and the one surviving install (the chart's ƒx picker) is used by all of them.
    //
    // ⚠ UNCONDITIONAL, where this call used to be `#[cfg(feature = "fat")]` — and the function's
    // own `fat` gate, which was around the STRATEGY half of the install, is gone with the feature.
    install_user_indicators();

    // `--observe [ADDR]`: the read-only observer — watch a headless `vike-tradehub` daemon's
    // observe server (see `App::new`'s observer arm). It is the only mode this binary has.
    // ONE call answers BOTH startup facts off one registry read —
    // `vike_app_core::backend::backend_conn::startup_observe`, whose `StartupObserve` doc carries the
    // address ladder AND the regression that made the second fact necessary. The argv scan lives
    // there rather than here so a bare `--observe` (no address after the word) is a case CI runs.
    let startup = vike_app_core::backend::backend_conn::startup_observe(std::env::args());
    // ⚠ THE MODE — and there is only ONE of them left. `vike_app_core::backend::split_plane::observes`
    // still ANSWERS the question, but its first argument is now a literal `false` where it read
    // `cfg!(feature = "fat")`: with no local-core configuration to contrast against,
    // `observes(false, _)` is unconditionally true, so `observe_backend` is always `Some` and
    // `App::new` always takes the observer arm.
    //
    // ⚠ TOMBSTONE — what the `false` stands in for. This used to be the BUILD fact that decided
    // whether a launch spawned a LOCAL trading core; the desktop no longer has one (orders leave
    // only through the backend, and this binary opens no venue socket), so the question the
    // argument asked has no second answer. #1610's regression was wrapping the resolved address in
    // `Some` UNCONDITIONALLY, which cost this app its local core for three days — that shape is
    // now the DESIGN, by ruling, rather than a defect. The distinction the ladder still protects
    // ("was observing REQUESTED" vs "did an address resolve") is unchanged and still load-bearing:
    // `StartupObserve::requested` is what a first-run state has to read.
    let observe_backend: Option<vike_app_core::backend::backend_registry::BackendRecord> =
        vike_app_core::backend::split_plane::observes(false, startup.requested)
            .then_some(startup.record);
    // ⚠ There used to be an `observe_addr: Option<String>` here — the ADDRESS lifted out on its own
    // "for the places that only NAME the node". That phrasing was the defect in miniature: an
    // address names no node. Its one consumer was the window title, which takes the whole record
    // now, so the binding is gone rather than left to be reached for again.
    //
    // ⚠ The desktop never REFUSES to start over a missing address. It has no local trading core at
    // all, so it must have one — and `observes` above is unconditionally true, so `observe_backend`
    // is always `Some` (flag, else the active backend, else the local default) and the arm it
    // could have fallen into is unreachable by construction rather than by an early exit. The
    // window opens and the status bar says whether it connected, which is the thing a person can
    // act on; `exit(2)` before a window existed was not. ⚠ `backend_identity::window_title`'s
    // `None` arm is therefore unreachable now; it is left standing because `observe_backend` is
    // still an `Option` and its disposition (a first-run "not connected" title) belongs to
    // ruling 6, not here.
    // The OS window title: the product's name, then the backend observed, its NAME leading —
    // `vike_app_core::backend::backend_identity::window_title` carries why, and is tested there.
    let window_title =
        vike_app_core::backend::backend_identity::window_title(observe_backend.as_ref());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // Open MAXIMIZED (fill the screen) so the whole app — top caption AND bottom status
            // bar — is visible on any monitor. A fixed 1400x900 with no position let the OS drop
            // the frameless window partly off-screen (top-left clipped, status bar below the
            // edge) on smaller displays. `inner_size` is the restored-down size.
            .with_inner_size(vike_ui_theme::value::desktop::WINDOW_RESTORED_SIZE)
            .with_maximized(true)
            .with_title(window_title.clone())
            // The Vike icon (the taskbar, Alt-Tab, X11's window list, the macOS Dock) and the app
            // id a Wayland taskbar matches to the installed desktop entry (spec §6).
            .with_icon(vike_ui_theme::brand::window_icon())
            .with_app_id(vike_ui_theme::brand::APP_ID)
            .with_decorations(false) // frameless — our own caption (vike-style)
            .with_resizable(true),
        ..Default::default()
    };
    eframe::run_native(
        &window_title,
        options,
        // The ONE place a real `eframe::CreationContext` exists in this binary — which is exactly
        // why `App::new` no longer takes one (see its doc): this closure is the only code that
        // could ever hold one, so it does the unwrapping and hands `App::new` the two public
        // members it reads.
        Box::new(move |cc| {
            Ok(Box::new(App::new(
                &cc.egui_ctx,
                cc.wgpu_render_state.as_ref(),
                observe_backend.clone(),
            )))
        }),
    )
}

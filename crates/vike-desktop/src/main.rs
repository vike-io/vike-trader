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
// (`equity_panel`/`trade_sizing` left with the Trade panel body in tool-view extraction batch 3 —
// only its `tool_views` home names them now; `order_entry` stays, but ONLY for the policy-derived
// `OrderLimits` + the QA test-order's preview — every UI order request is now built and capped by
// `order_dispatch::plan_dispatch`, which this file calls with the frame's drained intents.)
use vike_app_core::backfill_plan::{BackfillJob, plan_routed_backfill_jobs};
use vike_app_core::backfill_route::{BackfillRoute, backfill_route};
use vike_app_core::backfill_wire;
use vike_app_core::{data_sink, inventory, order_entry, orderflow, tickvol, tools, workspace};
// The pure, unit-tested helpers that used to live in this (CI-excluded) file also moved DOWN into
// vike-app-core so their `#[cfg(test)]` tests finally run in a gate (the dedup-app refactor). Brought
// back into scope by their original bare names so every call site here reads unchanged.
// (`default_price`/`synth_book`/`tick_for` left with the DOM body in tool-view extraction batch 3
// — the synthetic-book fallback is inside `tool_views::dom_tool_content` now; `signed_position_size`
// left with the DOM Close/Reverse exit in the order-dispatch extraction.)
// The WHOLE feed-lifecycle cluster now lives there — not just the pure gates/diffs
// (`should_spawn_backfill`, `backfill_earliest_ts`, the orphan diffs) but the imperative half too:
// `ensure_feed_on` / `ensure_trade_feed_on` / `ensure_depth` / `ensure_poly_book` /
// `reap_orphaned_feeds`, which this file keeps only as one-line `App` methods that build the
// `FeedSlots`/`SeriesSlots` bundles. The module is imported WHOLE (rather than each function bare,
// the older style below) so every one of those wrappers reads as an explicit delegation.
use vike_app_core::feed_lifecycle;
// `App::new`'s startup-layout tail (which windows open, which feeds they need, the two QA capture
// overrides) moved down too — same reason, same shape: this file reads the five env knobs and hands
// them over as `StartupEnv`, then applies the returned `StartupLayout` in its documented order.
use vike_app_core::startup;
use vike_app_core::sync_group::{GroupFrame, sync_feed, sync_harvest};
// The NEW-WINDOW decision moved down beside `startup`, for the same reason and in the same shape:
// SIX places in the frame loop below each re-derived the cascade slot, the `WinState` construction
// and the market-data subscription by hand, in a file `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI`
// keeps out of every roster lane — and two of the six had already drifted apart (one cascades from
// a different origin; a title-bar clone drops its source window's venue). They are one
// `SpawnRequest` roster now, whose branches CI executes, and `App::apply_spawn` below — which
// applies the returned `SpawnPlan` in its documented order — is the whole of what stayed here.
use vike_app_core::window_spawn;
// The FIRST-FRAME arrange decision moved down beside it (actions-out batch 2): the VIKE_TOOL/
// VIKE_TOOLS QA capture arms — the two window-opening sites `window_spawn`'s module doc
// deliberately left behind, because they spend `tool-{n}` ids at the desktop origin with no
// cascade — plus the VIKE_ARRANGE parse and the arrange-vs-maximize-lone choice. This binary
// reads the ten QA knobs and injects them as `ArrangeEnv`; the guarded apply block in `ui` below
// is the whole of what stayed here.
use vike_app_core::initial_arrange;
// The four pure venue/instrument resolvers moved down too (`venue_str`/`venue_inst` were `App::`
// associated fns that never read `self`, so they are free functions there); imported under their
// original bare names so every call site below reads unchanged. `venue_of_key`/
// `venue_bar_instrument` left this file entirely with the feed-lifecycle move — their only callers
// were `reap_orphaned_feeds`/`ensure_feed_on`.
use vike_app_core::venue_routing::{venue_inst, venue_str};
// The per-frame CoreSnapshot → render-model fold — see the thin `App::sync_from_core` wrapper
// below, and that module's doc for why it had to leave this CI-excluded file.
use vike_app_core::core_sync;

// The chart engine lives in vike-chart (extraction, chart roadmap phase 2); the
// crate-root re-exports keep the historical `crate::chart::…` paths of
// workspace/ and main.rs compiling untouched.
pub(crate) use vike_chart::{DisplayTz, ScaleMode, chart, indicators, model};
// The DOM (depth-of-market) ladder moved to vike-panels (shared egui widgets); the
// crate-root re-export keeps the historical `dom::…` paths in main.rs compiling untouched.
pub(crate) use vike_panels::dom;
// The Polymarket cockpit widgets are re-exported at the crate root (no submodule), so alias the
// crate to `cockpit` for the action-drain match arms. (The cockpit BODY that builds their inputs
// moved to `vike_app_core::tool_views::cockpit` in batch 3; only the drain stays here.)
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
/// per-instrument cross-kind partial-day map, and what the load LEARNED about the coverage verb
/// (spec §6-Q2 — `RemoteCoverage`, the third input to the Partial column's state; always
/// `Unknown` on the local arm, which negotiates nothing). Named because it rides an `mpsc` channel
/// whose type is spelled in three places (`stored_rx`, `stored_tx`, the worker's `tx.send`) —
/// copies of a bare tuple would have to be edited in lockstep every time the load grows a product.
type StoredLoad = (
    Vec<inventory::VenueNode>,
    vike_data_manager::GapMap,
    vike_data_manager::PartialDayMap,
    vike_app_core::stored_mode::RemoteCoverage,
);

// UNCONDITIONAL since split-plane I7: a default vike-studio build is DataFusion-free (the
// `studio-standalone` suite is the gate), so this client links the Studio and drives it
// REMOTE-ONLY via `open_studio_store` — which now has no other arm at all. The uninhabited stub
// that used to sit here went with the `fat` gate.
type StudioStateTy = vike_studio::StudioState;

use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

// `App`'s non-constructor methods (feed lifecycle, workspace restore, backend settings,
// DOM/depth subscriptions, core sync) — split out of this file for size; a second `impl App { .. }`
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
// `crates/vike-ops/tests/settings_registry.rs` classifies an `env::var` read by FILE PATH — only
// `main.rs`/`src/bin/*`/`examples/*` earn `Layer::Binary`. See `app_ui`'s module doc.
mod app_ui;
// GPU Candle Layer Phase 2 (Task 2): the wgpu candle pipeline + `CandleCallback` behind
// vike-chart's `GpuCandleItem` seam. Registered in `App::new` (see `gpu_ok`); the render toggle
// that feeds it is wired in Task 3.
mod chart_gpu;
// The chart window's title-bar toolbar (chart-style menu, symbol/interval dropdowns, sync/OF/Cmp
// popups, LIVE badge, window controls) — extracted out of this file to shrink it and co-locate
// the chart-window chrome; app-specific, so it stays in vike-app rather than vike-chart.
mod chart_window;
// The ONE canonical color palette (vike theme.py). Every per-function local `const` block, every
// tool arm and `install_visuals` now reference `theme::` so a color exists in exactly one place.
mod theme;
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
// (`vike_app_core::poly_labels::poly_market_short_name`) for `pick_updown_token`'s display name;
// that function is gone with the Polymarket bridge edge, so the import went with it. The helper
// itself is untouched in vike-app-core and still has consumers there.
use vike_app_core::tool_views;
// The cockpit's tuning constants + the resolved-order-intent enum moved with the cockpit body
// (batch 3); re-imported under their original bare names so the window bookkeeping and the
// action-drain match arms below read unchanged. `poly_cockpit_seed_token` (an env read)
// deliberately STAYED here.
use vike_app_core::tool_views::{CockpitCmd, POLY_PLACEHOLDER_TOKEN, POLY_STALE_MS};

// The most-referenced palette colors, kept as bare module-level aliases of `theme::` so the dozens
// of existing `UP`/`ACCENT` call sites in this file (and in `chart_window.rs`, which imports them
// from here) read unchanged. ⚠ `DOWN` was a third alias and is GONE: its last two bare uses were
// the status strip's two dots, whose colours moved into
// `crates/vike-app-core/src/status_dot.rs`, and an unused private const is a `dead_code` failure
// under the `app-check` job's `-D warnings`. The remaining red in this file is spelled
// `theme::DOWN`, which is where the value lives anyway.
const UP: egui::Color32 = theme::UP; // hsl(128,49,49)
const ACCENT: egui::Color32 = theme::ACCENT; // hsl(148,72,56)
const SYMS: [&str; 3] = ["BTCUSDT", "ETHUSDT", "SOLUSDT"];
/// The default chart venue — re-exported from the workspace model so `main.rs`/`chart_window.rs`
/// and `WinState` name the exact same string (`"binance"`). Cross-exchange symbol search hangs
/// off this: `"binance"` is the zero-behavior-change default everywhere.
const DEFAULT_VENUE: &str = workspace::DEFAULT_VENUE;

/// Height (pts) of the bottom status bar strip.
const STATUS_BAR_H: f32 = 22.0;
/// C2a Task 4: per-overlay line colors for Compare series, indexed by add-order
/// (`WinState::compare` position, wrapping via `% len`). Deliberately distinct
/// from the candle up/down green/red (`UP`/`DOWN` above) so an overlay never
/// reads as a primary-series body. The title-bar Compare chip for overlay `i`
/// is tinted the same color, keeping the on-chart line and its legend in sync.
const COMPARE_COLORS: [egui::Color32; 5] = [
    egui::Color32::from_rgb(87, 165, 255),  // blue
    egui::Color32::from_rgb(240, 149, 40),  // orange
    egui::Color32::from_rgb(175, 122, 255), // purple
    egui::Color32::from_rgb(38, 198, 218),  // cyan
    egui::Color32::from_rgb(236, 64, 122),  // pink
];
const IVLS: [&str; 5] = ["1s", "1m", "5m", "15m", "1h"];
/// Tick-count aggregation intervals (Task B5) — client-side, built from the trade tape by
/// `tickvol::TickVolAgg`; parsed by `tickvol::BarKind::parse` (`"<n>t"`). Its own dropdown group,
/// separated from `IVLS` (venue klines) by a `ui.separator()` in `title_bar`.
const TICK_IVLS: [&str; 3] = ["100t", "500t", "1000t"];
/// Volume-threshold aggregation intervals (Task B5) — same client-side path as `TICK_IVLS`,
/// parsed as `"<x>v"`.
const VOLUME_IVLS: [&str; 3] = ["1v", "10v", "100v"];

/// QA capture hook — PRESENT (any value, `is_ok()`, the `VIKE_DOM_TESTORDER` idiom): seed the
/// Trade panel with one resting PAPER limit order and one open PAPER position, so a headless
/// capture of that window shows a session instead of two empty tables.
///
/// Read TWICE from this file and nowhere else: once to arm [`App::trade_seed_pending`] (the frame
/// loop submits the orders) and once into `startup::StartupEnv::trade_seed` (the startup layout
/// opens the bar feed whose closes clock the paper fill). Both halves are needed — see
/// [`vike_app_core::capture_seed`]'s module doc, and `vike_app_core::startup::StartupEnv`'s
/// `trade_seed` field for why a feed with no chart window on screen has to be arranged for.
///
/// ⚠ It can only ever reach a PAPER venue, and that is enforced at the submit site rather than
/// here: the frame loop refuses to seed a venue in `App::live_venues`, and refuses entirely while
/// a remote `Scope::Control` channel is mounted. Both guards are the `VIKE_DOM_TESTORDER` block's,
/// for the reasons its own comment gives.
const TRADE_SEED_ENV: &str = "VIKE_TRADE_SEED";

/// QA capture hook — PRESENT (any value, same idiom as [`TRADE_SEED_ENV`]): draw the two capture
/// overlays [`vike_app_core::capture_seed::trendline_overlay`] derives (a trendline through the
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
    new_asset_class: Option<vike_catalog::AssetClass>,
    new_interval: Option<String>,
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
/// end state, but the batch lane's drain lives in `vike_app_core::core_sync` and its switch
/// slots in `vike_app_core::backend_conn` — a CI-roster crate, 48 sites — and that removal is the
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
    /// [`feed_lifecycle::FeedRetries`]. Covers all four `ensure_*` sites (chart bars, the shared
    /// trade tape, the DOM's inner 1m bar leg, the cockpit's trade leg), each of which used to
    /// claim its idempotency slot on the FAILURE path too and so lost that stream permanently.
    /// Maintained entirely by the feed-lifecycle functions: created on failure, cleared by the
    /// retry that succeeds, and its series lane swept by `reap_orphaned_feeds`. Empty on the
    /// normal path; a non-empty entry is a chart/ladder/tape delivering nothing right now.
    feed_retries: feed_lifecycle::FeedRetries,
    hidden: HashSet<String>, // Data-manager-deleted keys: feed runs, GUI stops syncing
    core: Option<vike_core::CoreHandle>, // the vt-core single-writer runtime (R5b/R5c)
    /// The ACTIVE remote backend (split-plane B1): the record it was dialed from, the observe
    /// bridge (B6's stop handle — its `Drop` stops-and-joins the reconnect thread), and the
    /// optional Scope::Control WRITE channel (⚠ when `Some`, the GUI's order buttons place/cancel
    /// REAL orders on the remote daemon — see `App::remote_ctrl()` for the read sites). `Some`
    /// ALWAYS `Some` now — at startup from the resolved record (the `--observe` flag, else the
    /// registry's active record, else the local default), or replaced after the Connections tool's
    /// picker connected a different registry backend. It used to be `None` on the local-core path;
    /// there is no such path any more, and this connection is the ONLY route an order out of this
    /// process has. Replaced WHOLESALE by `vike_app_core::backend_conn::switch_backend` (the
    /// backend-session-clear switch routine); taken + dropped in `on_exit`.
    active_backend: Option<vike_app_core::backend_conn::BackendConn>,
    /// The loaded `backends.json` registry (split-plane B7/B8) the Connections tool's picker
    /// lists. Loaded once at startup; mutated only by the drained Backends-editor update
    /// (split-plane I2 — see `backend_editor` below), which assigns AND saves in one drain so
    /// disk and memory never disagree.
    backends: vike_app_core::backend_registry::BackendsFile,
    /// The Connections tool's Backends EDITOR state machine (split-plane I2): the add/edit/
    /// delete form. Every decision lives in `vike_app_core::backend_editor`; this binary only
    /// threads it into the tool body (`mem::take`/write-back, the `studio` dance) and applies
    /// the drained `RegistryUpdate` after the frame.
    backend_editor: vike_app_core::backend_editor::EditorState,
    /// The Connections tool's Backend-settings section state (split-plane REQ-7, read half):
    /// `(backend addr, state)`, keyed by ADDR so a backend switch renders fresh (`Idle` ⇒
    /// auto-refetch) instead of the previous backend's rows. Written by the short-lived
    /// `spawn_backend_settings_fetch` thread, read each frame the Connections tool is visible.
    backend_settings: Arc<Mutex<(String, vike_app_core::tool_views::BackendSettingsState)>>,
    /// The section's EDIT flow (REQ-7 write half): UI-thread state (its text buffers are typed
    /// into every frame), taken into a frame-local for the window loop and written back after —
    /// the pure machine lives in `vike_app_core::tool_views::backend_settings`.
    backend_settings_edit: vike_app_core::tool_views::SettingsEditState,
    /// One finished settings WRITE, already folded to its flow state by the worker
    /// (`settings_write_state`), keyed by backend ADDR (the fetch slot's latest-wins
    /// discipline). Written by the short-lived `spawn_backend_settings_write` thread, drained at
    /// the top of each frame.
    backend_settings_write_result:
        Arc<Mutex<Option<(String, vike_app_core::tool_views::SettingsEditState)>>>,
    snap_cell: Arc<arc_swap::ArcSwap<vike_core::CoreSnapshot>>,
    last_seq: u64, // last CoreSnapshot.seq folded into `charts`
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
    // `vike_connections::VENUES` slug. A superset of `feed_status` above (which stays the binance-
    // only handle the top status line reads) covering every already-producing feed — binance/bybit/
    // okx/aster/hyperliquid/polymarket. Arc clones captured from each concrete `Feeds` before it is
    // boxed into `feeds`, so this shares the live handles. Venues without a live producer are simply
    // absent and render `Unknown`.
    feed_statuses: HashMap<String, Arc<std::sync::Mutex<String>>>,
    // best-effort tick-recording writer actor (tick-producer T3/T5); `None` when the store
    // couldn't be opened at startup — ticks still flow to the core either way (see `on_exit`'s
    // load-bearing shutdown order: feeds -> recorder -> core).
    recorder: Option<RecorderHandleTy>,
    subs: HashMap<String, vike_data::SubscriptionId>, // "{symbol}@{interval}" -> live subscription
    books: std::sync::Arc<data_sink::BookStore>, // live L2 books for DOM windows (per (venue,symbol))
    // live trade tape for GUI-side tick/volume aggregation (per (venue,symbol)); drained by
    // `sync_from_core`'s per-frame fold (Task B5) into every `aggs` entry sharing that symbol.
    trades: std::sync::Arc<data_sink::TradeStore>,
    // The direct-bar store (split-plane B2's kline follow-up). ⚠ ALWAYS `None` now: it was `Some`
    // exactly in the THIRD MODE (fat + `--observe`), where `GuiFeedSink`'s bar lanes filled it from
    // the local venue feeds and `sync_from_core`'s direct-bar fold rendered those klines
    // venue-direct instead of from the backend tail. Ruling 2 removed the local feeds, so nothing
    // fills it and every kline comes off the wire. Its presence IS the mode signal the
    // render-source decision takes, which is why the field stays rather than the decision changing.
    direct_bars: Option<std::sync::Arc<data_sink::DirectBarStore>>,
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
    of_aggs: HashMap<String, (String, String, orderflow::OrderflowAgg)>,
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
    /// in `vike_app_core::core_sync` STAGES an undeliverable batch (`bf_pending`) instead of
    /// discarding it. Correct as-is; do not "fix" it by clearing entries here — the ONE sanctioned
    /// way a symbol walks again is `bf_retries` below, which reopens the gate WITHOUT removing
    /// anything from this set.
    bf_spawned: HashSet<String>,
    /// Symbols whose backfill walk reported a RETRYABLE stop (#952's `BackfillStop::Failed`) — the
    /// lane that reopens `bf_spawned` on a bounded backoff. Owned here purely because it is
    /// per-`App` state; the whole policy (when, how often, how many times, what is logged, and the
    /// no-double-count rule that keeps a delivering walk from ever re-running) lives in
    /// `vike_app_core::feed_lifecycle::BackfillRetries`, which is where its tests are.
    bf_retries: feed_lifecycle::BackfillRetries,
    /// ⚠ A RECEIVER WITH NO SENDER — see [`dead_receiver`]. Backfill worker threads (SP3 T3,
    /// spawned from `maybe_spawn_backfill`) used to push completed batches here, drained every
    /// frame in `sync_from_core` before the live trade drain. The workers left with the local
    /// market-data plane and `maybe_spawn_backfill` is an empty stub, so this end is kept only
    /// because the drain in `vike_app_core::core_sync` still takes it by reference; every
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
    /// in `vike_app_core::core_sync`, which is where its tests are. Empty in steady state.
    bf_pending: HashMap<String, Vec<vike_model::TradeTick>>,
    /// The live Binance trades feed's shared earliest-emitted-aggTrade-id-per-symbol map (SP3
    /// T2's `market_feed::Feeds::earliest_live_ids`), cloned once at construction — BEFORE
    /// `binance_feed` is boxed into `feeds` (see `App::new`). Each backfill worker polls this
    /// for its symbol's strictly-older paging boundary (`global-constraints.md`'s
    /// no-double-count invariant: backfill ids must stay `< min_live_id`).
    earliest_live_ids: Arc<std::sync::Mutex<HashMap<String, u64>>>,
    /// `(venue, canonical)` with a depth stream running — the DOM's dedupe LEDGER, each entry
    /// keeping the subscription ids `ensure_depth` minted (`feed_lifecycle::DomDepthSubs`) so
    /// `reap_orphaned_dom_cockpit_streams` stops the streams when the last DOM window on the
    /// symbol is deleted (before the ids were kept, only feed `shutdown()` on exit stopped them).
    dom_depth: HashMap<(String, String), feed_lifecycle::DomDepthSubs>,
    /// Polymarket token-ids with a live cockpit book+trade stream running — the cockpit's analog
    /// of `dom_depth`, same ledger shape (`feed_lifecycle::PolyBookSubs`), reaped by the same
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
    /// `vike_ops::reconcile_config::reconcile_gate`) and at least one live venue's `ReconClient`.
    /// This process mounts no venue, so it reconciles nothing — the daemon is the only reconciling
    /// root left. Still shut down in `on_exit` alongside `forwarder_stop`, which is now a no-op.
    recon_driver: Option<vike_core::ReconDriver>,
    /// The B11 live-account lock guards (`vike_ops::live_lock`) — one per venue a mount ARMS, held
    /// (never read) for the app's lifetime so a second live process on the same account is refused
    /// for exactly as long as this one can trade.
    ///
    /// ⚠ ALWAYS EMPTY now, and the ARGUMENT for it is worth keeping even so: the set was
    /// `vike_run::armed_live_venues`, claimed one statement ABOVE `build_node` and deliberately NOT
    /// the [`Self::live_venues`] record beside it. Those two answer the same question at different
    /// times, and the time was the whole point — `live_venues` does not exist until every exec
    /// client has been constructed, and three venues post `set_leverage` while constructing one, so
    /// a lock refused from that set is refused after this process has already changed account
    /// state. The claim site is gone with the mount (see the tombstone in `App::new`); anything
    /// that brings a mount back has to bring that ordering back with it.
    _live_locks: Vec<vike_ops::live_lock::LiveLock>,
    // The B6 `_observe_bridge` stop handle folded into `active_backend` above (split-plane B1):
    // the bridge now lives inside `BackendConn`, whose replacement/drop stops-and-joins it.
    /// Live journal materializer (unified-journaling #2) — spawned ONLY when the WAL is enabled
    /// (VIKE_JOURNAL_DIR / profile); drains the WAL's fills into the Tier-2 exec-fill log off the
    /// fold. `None` (the default) = no materialization, zero overhead. Dropped in `on_exit` (final
    /// drain + join), so a clean shutdown loses nothing.
    materializer: Option<vike_app_core::journal_mat::MaterializerHandle>,
    dom_test_pending: bool, // QA (VIKE_DOM_TESTORDER): inject one buy-limit to the DOM venue, once
    /// QA ([`TRADE_SEED_ENV`]): submit the two PAPER orders
    /// [`vike_app_core::capture_seed::plan_trade_seed`] mints — one resting limit, one market —
    /// so a Trade-panel capture has a working order and an open position instead of empty tables.
    /// Cleared on the frame that submits them, so it is a ONE-SHOT like `dom_test_pending`; a
    /// frame that could not act (no dispatch, no price yet) leaves it armed and re-tries.
    trade_seed_pending: bool,
    /// QA ([`CHART_DRAW_ENV`]): write the two capture drawings
    /// [`vike_app_core::capture_seed::trendline_overlay`] derives into every chart's
    /// `ChartState::overlays`, once the series has bars. Same one-shot/re-try shape as
    /// [`Self::trade_seed_pending`].
    chart_draw_pending: bool,
    wins: Vec<workspace::WinState>,
    tool_views: HashMap<egui::Id, tools::ToolView>, // per-tool-window view state, keyed by WinState.id
    /// Chart sync groups (task B8): double-buffered per-frame registry, one [`GroupFrame`]
    /// per group (1..=4). `sync_prev` is what THIS frame's grouped chart windows read (via
    /// [`sync_feed`]) to build their `chart::ChartInputs::sync`; `sync_next` is written
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
    /// The Studio tool's singleton editor/backtest session (SP2 `vike-studio` mount, Task 7) — ONE
    /// long-lived instance, not the per-window `tool_views` map: Studio is a single editor session
    /// (like a code-editor tab), not per-window state, even if somehow more than one Studio window
    /// existed. `None` until the FIRST Studio-window render (lazy: most sessions never open it, so
    /// paying `DataFusionHist::open`'s cost — a tokio runtime spin-up + WAL recovery — at every
    /// startup would be wasted work for everyone else). See `studio_store_root`/`open_studio_store`.
    studio: Option<StudioStateTy>,
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
    stored_tree: Arc<Vec<inventory::VenueNode>>,
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
    stored_coverage: vike_app_core::stored_mode::RemoteCoverage,
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
/// The bundled PNGs are rendered from Python's icons.py (exact line-art + TOOL_COLORS).
const LAUNCHERS: &[(&str, &[u8], Option<workspace::WinKind>)] = &[
    ("chart", include_bytes!("../../../assets/icons/chart.png"), Some(workspace::WinKind::Chart)),
    ("trade", include_bytes!("../../../assets/icons/chart.png"), Some(workspace::WinKind::Trade)),
    ("dom", include_bytes!("../../../assets/icons/chart.png"), Some(workspace::WinKind::Dom)),
    // Polymarket scalp cockpit — reuses the DOM/chart glyph until a dedicated icon exists.
    (
        "polymarket",
        include_bytes!("../../../assets/icons/chart.png"),
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
    // dedicated plug glyph (generated to match the 🔌 already used for this WinKind's window
    // title and its orange accent, WinKind::color) — no longer reuses alerts.png's bell.
    (
        "connections",
        include_bytes!("../../../assets/icons/connections.png"),
        Some(workspace::WinKind::Connections),
    ),
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
    // Greeks reuses the options line-art (no dedicated PNG); its own accent lives in WinKind::color.
    (
        "greeks",
        include_bytes!("../../../assets/icons/options.png"),
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
// `crates/vike-model/src/tick_store_path.rs`'s `resolve_tick_store_root` is the shared home the
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
/// `market_data/hist`, the layout `vike_model::state_path::PROJECT_DATA_DIR` commits to) rather than the
/// `<exe_dir>`-relative path this line used to name: that one only ever receives recorded
/// quotes/trades/books (`RecorderSink`'s
/// bar-seam methods — `seed_bars`/`close_bar`/`forming_bar` — are no-ops, see
/// `vike_data::live_rec`), so it never has the `kind=bar` series the Studio's `SlicePicker` looks
/// for; the backfill/harness convention is the one that actually has bars in it.
fn studio_store_root() -> std::path::PathBuf {
    // ⚠ The dev-checkout rung is a DEBUG-BUILD rung. `env!("CARGO_MANIFEST_DIR")` is a string
    // literal in the binary, and `release.yml` builds this crate `--release` on a runner whose
    // checkout path names the runner account — so the `vike-app-fat`/`-thin`/`.exe` assets carried
    // that path into every public download, and `scripts/refuse_box_paths.sh` (the release's
    // box-path guard) refused them by name. A release build therefore passes NO rung and resolves
    // from the project walk down, which for anyone standing in a checkout is the same directory by
    // a different rung name; `vike_model::store_path`'s module doc (rung 4) carries the argument.
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
    // spelled in exactly ONE file workspace-wide — `vike_model::store_path` — and reaches it
    // through the environment MAP this binary collects, the same `std::env::vars()` sweep
    // `App::new` already builds elsewhere. The project walk likewise stays in
    // `vike_model::state_path`; this binary only supplies the working directory it starts from.
    //
    // The store root itself now comes from `config.store_root` (`config.toml`), still overridden by
    // `VIKE_HIST_STORE` inside the loader — so `resolve_store_root`'s second argument carries the
    // resolved answer rather than a raw variable, and the `store_root = "…"` an operator writes in a
    // file finally reaches the Studio. ⚠ The OTHER `VIKE_HIST_STORE` readers are unchanged and still
    // environment-only: vike-datahub's bin, and the map-taking `vike_backtest::binutil::store_root`
    // / `vike_backfill::cli::store_root` (see `vike_config::CONSUMPTION`).
    //
    // ⚠ `resolve_store_root_from`, never the bare `resolve_store_root` ladder: that one's project
    // and per-user defaults are two adjacent `Option<PathBuf>`s, so transposing them here would
    // COMPILE SILENTLY and browse the wrong store. This form has no two arguments of the same type,
    // and `$VIKE_SETTINGS_DIR` reaches the project walk through the same map.
    let cwd = std::env::current_dir().ok();
    let resolved = vike_model::store_path::resolve_store_root_from(
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

/// Open the STUDIO's store as the TRAIT handle it consumes (split-plane B12), branching on the
/// RESOLVED datahub address (`App::resolved_datahub_addr` — the explicit `config.datahub_addr`,
/// else the active backend's `Welcome` advertisement; REQ-2):
///
/// - **`Some(addr)`** → dial a `vike_datahub_client::RemoteHistStore` at that address — the
///   CLIENT-side address of a `vike-datahub` server (⚠ deliberately distinct from
///   `config.tradehub_addr`, the tradehub DAEMON's BIND address). The constructor never dials
///   (connect-per-read), so a one-shot `list_series` probe runs here: a down/unreachable server
///   degrades to the same "Studio: no data store (…)" label a broken local open takes, instead
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
) -> Result<(vike_studio::StoreHandle, Option<String>), String> {
    match datahub_addr {
        Some(addr) => {
            use vike_data::HistStore as _; // list_series — RemoteHistStore serves it as a trait verb
            let store = vike_datahub_client::RemoteHistStore::new(addr.clone());
            store
                .list_series()
                .map_err(|e| format!("datahub {addr}: {e} (the resolved datahub address — config.datahub_addr, or the backend's advertisement — unset/disconnect it to use the local store)"))?;
            Ok((Arc::new(store) as vike_studio::StoreHandle, Some(addr)))
        }
        None => Err("this build has no local store engine — set config.datahub_addr in \
             <project>/settings/config.toml to a running vike-datahub server, or connect a \
             backend whose daemon advertises one (config.toml `datahub_advertise_addr` on the \
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
// `vike_ops::reconcile_config::reconcile_enabled` — a function this binary stopped calling long
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
// reconnect loop that CALLS `wire_to_core`, and `connect_control`, the Scope::Control gate), so the
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
// pinning its own key set against `vike_app_core::split_plane::LOCAL_FEED_VENUES` — that constant
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
    fn new(
        egui_ctx: &egui::Context,
        render_state: Option<&eframe::egui_wgpu::RenderState>,
        // ⚠ The whole RECORD, not an address: it carries the credential-store KEY NAMES this
        // connection must sign with. `main` resolves it (flag / registry active record / local
        // default) and this arm connects from it unchanged.
        observe_backend: Option<vike_app_core::backend_registry::BackendRecord>,
    ) -> Self {
        install_fonts(egui_ctx); // so ✕ ─ □ ❐ ● etc. aren't tofu
        install_visuals(egui_ctx); // dark canvas matching vike theme.BG
        let area = egui_ctx.content_rect();

        // The core-derived App bindings. There is NO local trading core and no local market-data
        // plane: the account-plane bindings are empty/None read-only twins and a background bridge
        // thread keeps `snap_cell` fresh from a remote `vike-tradehub` daemon (see the module
        // comment on `wire_to_core`). ⚠ This used to BRANCH on `--observe`, over three
        // compositions (local core / observe+feeds / observe-only); the `else` arm below is now a
        // tombstone and an `unreachable!`, kept only because `observe_backend` is still an
        // `Option` — `split_plane::observes` answers unconditionally true, so the `Some` arm always
        // wins.
        let (
            core,
            active_backend,
            snap_cell,
            feeds,
            feed_status,
            feed_statuses,
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
        ) = if let Some(observe_backend) = observe_backend.as_ref() {
            // ── --observe: THE mode — the read-only client ──────────────────────────────────────
            // Connect to a headless `vike-tradehub` daemon's observe server (Scope::Observe) and
            // republish its pushed `WireSnapshot`s into a GUI-owned arc-swap cell as
            // `CoreSnapshot`s. The ACCOUNT PLANE is the backend's: no local core, no exec engines,
            // no recorder, no journal materializer, no recon driver, `live_venues` empty. With
            // `core: None`, the command choke point (gated on a `Dispatch` over
            // `(self.core, self.remote_ctrl)`) is a read-only no-op UNLESS a Scope::Control channel
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
            // stayed remote (a `DirectBarStore` held those bars, and
            // `split_plane::series_render_source` assigned each kline series to exactly one of
            // `snap.bars` and that store). Ruling 2 removes it: the desktop opens NO venue socket,
            // so every one of those surfaces now reads from the backend or reads nothing.
            // `split_plane::AppMode::ObserveWithFeeds` and `direct_bars_mount` consequently have no
            // producer in this binary — the arm table itself is vike-app-core's and is untouched
            // here. The feed-less arm below is the only one left; its `debug_assert_eq!` is exactly
            // the claim that now holds unconditionally, so it stays.
            let (feeds, feed_statuses, earliest_live_ids, direct_bars) = {
                debug_assert_eq!(
                    vike_app_core::split_plane::app_mode(false, true),
                    Some(vike_app_core::split_plane::AppMode::ObserveOnly),
                    "the arm table and this wiring must agree: the desktop is feed-less"
                );
                let feeds: HashMap<&'static str, Box<dyn vike_data::DataClient + Send>> =
                    HashMap::new();
                let feed_statuses: HashMap<String, Arc<std::sync::Mutex<String>>> = HashMap::new();
                let earliest_live_ids: Arc<std::sync::Mutex<HashMap<String, u64>>> =
                    Arc::new(std::sync::Mutex::new(HashMap::new()));
                // No local feeds ⇒ no bar lane ⇒ no store: klines keep the backend tail
                // (`split_plane::direct_bars_mount(ObserveOnly)` is false).
                let direct_bars: Option<std::sync::Arc<data_sink::DirectBarStore>> = None;
                (feeds, feed_statuses, earliest_live_ids, direct_bars)
            };
            let live_venues: HashSet<String> = HashSet::new();
            let forwarder_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let recon_driver: Option<vike_core::ReconDriver> = None;

            // The whole connection — the push→CoreSnapshot bridge thread (B6's self-healing
            // reconnect loop) AND the opt-in Scope::Control write path — is now built by
            // [`vike_app_core::backend_conn::connect_backend`] from the record `main` RESOLVED.
            // For `--observe ADDR` (and for the local default) that record is the synthetic
            // [`vike_app_core::backend_conn::cli_observe_record`], so the flag behaves
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
                Some(vike_app_core::backend_conn::connect_backend(
                    &record,
                    &vars,
                    vike_app_core::tradehub_control::control_enabled(),
                    feed_status.clone(),
                    move |snap| cell.store(snap),
                    move || ctx.request_repaint(),
                ))
            };

            (
                core,
                active_backend,
                snap_cell,
                feeds,
                feed_status,
                feed_statuses,
                recorder,
                books,
                trades,
                direct_bars,
                bf_rx,
                earliest_live_ids,
                live_venues,
                // No local core in `--observe` ⇒ no exec client, no venue armed, nothing to lock.
                Vec::new(),
                forwarder_stop,
                recon_driver,
            )
        } else {
            // ⚠ TOMBSTONE — THE LOCAL TRADING CORE STOOD HERE, and ~820 lines went with it.
            //
            // This was `App::new`'s second arm: the one that ran whenever nobody asked to observe.
            // It is deleted by ruling — orders leave the desktop ONLY through the backend, and the
            // desktop opens NO venue socket at all — so the arm has no second side left and this
            // `else` is unreachable by construction. What it did, named so a future reader does not
            // go looking for it in the history of a file that no longer mentions it:
            //
            //   • the MOUNT — `vike_run::MountPolicy::from(&settings().policy)`,
            //     `vike_run::armed_live_venues`, `vike_run::venue_arming`, a `vike_run::NodeConfig`
            //     carrying the GUI's `vike_core::CoreConfig` knobs, and `vike_run::build_node`,
            //     which assembled every `vike_mount::make_engine` arm (the twelve-venue mount),
            //     spawned the single-writer core and forwarded live venue events. A build_node
            //     fault exited the process with code 2 before a window existed.
            //   • the LIVE-ACCOUNT LOCK — `vike_ops::live_lock::LiveLock::acquire` per armed live
            //     route, claimed ONE statement above `build_node`; that ordering was load-bearing
            //     (claim before anything authenticates) and is pinned by
            //     `crates/vike-ops/tests/live_lock_claim_order_gate.rs`, whose vike-app root row
            //     now names code that does not exist.
            //   • RECONCILIATION — `vike_ops::reconcile_config::quarantine_first_default` over the
            //     REAL process env, `reconcile_gate` over `flags.reconcile`/`flags.reconcile_off`
            //     and the armed-live COUNT, `log_reconcile_gate`, the per-venue health map derived
            //     from the feed statuses, the `journal_view_provider` hook, and the
            //     `vike_core::spawn_recon` driver mount. Pinned by
            //     `crates/vike-ops/tests/reconcile_gate_wiring_gate.rs`'s vike-app root, likewise
            //     now naming absent code. The daemon is the only reconciling root left.
            //   • the RUN PROFILE and its guards — `vike_core::resolve_profile` +
            //     `RunProfile::apply_guards_and_sinks`, the `GuardsReport` unwired-key warning, and
            //     `vike_exec` risk limits derived from each venue's fetched `SymbolProperties`.
            //   • the LOCAL DATA PLANE — `open_tick_recorder` (the DataFusion+Parquet
            //     `RecorderSink` and its equity sampler), the `CoreSinkAdapter`/`TeeSink`
            //     composition, `vike_data::PropertiesRecorder::open_from_env`, the mmap
            //     health-counters pre-flight (`counters_file_path` +
            //     `vike_core::counters::CountersFile::create`), the opt-in Deribit DVOL recorder
            //     (`flags.record_dvol` → `vike_deribit::DvolRecorder` +
            //     `spawn_deribit_dvol_feed`), and `build_local_feeds` — the six keyless
            //     direct-to-venue market-data feeds.
            //
            // ⚠ The settings keys those reads consumed lose their consumer in THIS binary with
            // them: `flags.record_dvol`, `config.state_dir` (via `state_dir_path`),
            // `VIKE_COUNTERS_FILE`, `VIKE_TICK_STORE`. `vike_config::CONSUMPTION` and
            // `vike_ops::settings::SETTINGS` both key on that, and neither is updated here.
            unreachable!(
                "the desktop has no local trading core — `split_plane::observes` is \
                 unconditionally true, so `observe_backend` is always Some and the observer arm \
                 always wins"
            )
        };

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
        // real country flags (PNG from flagcdn) decoded off-thread, loaded as textures in ui()
        let (ftx, flag_rx) = std::sync::mpsc::channel();
        {
            let c = egui_ctx.clone();
            spawn_flag_fetcher(ftx, move || c.request_repaint());
        }
        let (ltx, logo_rx) = std::sync::mpsc::channel();
        {
            let c = egui_ctx.clone();
            spawn_logo_fetcher(ltx, move || c.request_repaint());
        }
        // Symbol-search catalog: fetch the live Binance spot instrument universe ONCE, off-thread,
        // delivered over `catalog_rx` (drained in `update`); quick-picks stand in until it lands.
        let (catx, catalog_rx) = std::sync::mpsc::channel();
        {
            let c = egui_ctx.clone();
            spawn_catalog_fetcher(catx, move || c.request_repaint());
        }
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
        // launcher icons (bundled PNGs rendered from vike's icons.py) → textures
        let mut launcher_icons = HashMap::new();
        for (name, bytes, _) in LAUNCHERS {
            if let Some(tex) = load_png_texture(egui_ctx, name, bytes) {
                launcher_icons.insert(name.to_string(), tex);
            }
        }

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

        // ⚠ TOMBSTONE — the JOURNAL MATERIALIZER is gone, and `App::materializer` is now always
        // `None`. Under `fat` this read `vike_core::journal_config_from_env()` for the WAL
        // directory, opened a `vike_data::DataFusionHist` at `tick_store_root()` and handed both to
        // `vike_app_core::journal_mat::maybe_spawn`, which materialized the local core's journalled
        // fills into the Tier-2 exec-fill log off the fold. It was already `None` in observe mode —
        // no local core writes a journal to materialize — and the desktop has no local core at all
        // any more (rulings 1 and 2), so there is no journal here to read and no DataFusion engine
        // linked to read it with. The daemon owns this work now, and the field survives only
        // because `App::run_bounded_teardown` still names it (whether it should is the field-level
        // decision this change deliberately did not take).
        let materializer: Option<vike_app_core::journal_mat::MaterializerHandle> = None;

        // Unified shutdown signal (see the `App::shutdown` field + `on_exit`): raised at window
        // close, then read by the bounded teardown. Inert until then.
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        // The backfill workers' exit-report lane (see the `bf_done_tx` field). Built here rather
        // than in the branched core tuple above because it needs nothing from any branch — it is
        // pure `App`-local bookkeeping, present and inert in `--observe`/thin builds alike.
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
            backends: vike_app_core::backend_registry::load(),
            backend_editor: Default::default(),
            backend_settings: Arc::new(Mutex::new((String::new(), Default::default()))),
            backend_settings_edit: Default::default(),
            backend_settings_write_result: Arc::new(Mutex::new(None)),
            snap_cell,
            last_seq: 0,
            feeds,
            feed_status,
            feed_statuses,
            recorder,
            subs: HashMap::new(),
            books,
            trades,
            direct_bars,
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
            dom_depth: HashMap::new(),
            poly_subs: HashMap::new(),
            poly_resolve_rx,
            poly_names: HashMap::new(),
            live_venues,
            forwarder_stop,
            recon_driver,
            _live_locks: live_locks,
            dom_test_pending: std::env::var("VIKE_DOM_TESTORDER").is_ok(),
            trade_seed_pending: std::env::var(TRADE_SEED_ENV).is_ok(),
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
            gpu_ok,
            gpu_render: false,
            indicator_favs: Vec::new(),
            studio: None,
            studio_error: None,
            stored_tree: Arc::new(Vec::new()),
            stored_gaps: Arc::new(HashMap::new()),
            stored_partials: Arc::new(vike_data_manager::PartialDayMap::new()),
            stored_coverage: vike_app_core::stored_mode::RemoteCoverage::Unknown,
            stored_rx,
            stored_tx,
            stored_loading: false,
            stored_backfill_running: false,
            stored_backfill_status: String::new(),
            stored_backfill_rx,
            stored_backfill_tx,
        };

        // ── startup layout ──────────────────────────────────────────────────────────────────────
        // The whole decision — restore-vs-QA-window-vs-default-chart, the feeds each of those
        // windows needs, and the VIKE_STYLE/VIKE_SCALE capture overrides — moved DOWN into the
        // CI-gated [`vike_app_core::startup`] (six branches over five env knobs that no gate
        // compiled; see that module's doc). The five env READS stay HERE, in the binary, and are
        // injected as `StartupEnv`: that keeps every `VIKE_*` row in the settings registry
        // classified `vike-app` / `Layer::Binary` exactly as today, and leaves `startup::plan` a
        // pure function its tests can drive without mutating process env.
        let startup_env = startup::StartupEnv {
            shot_mode: std::env::var("VIKE_SHOT").is_ok(),
            shot_win: std::env::var("VIKE_SHOT_WIN").ok(),
            // The one knob here that is a real user PREFERENCE rather than a QA-capture override:
            // `chart_style` in `<project>/settings/preferences.toml`, still overridden by
            // `VIKE_STYLE` inside the loader. The other four stay bare env reads — `VIKE_SHOT*`,
            // `VIKE_SCALE` and the cockpit token are screenshot/dev-convenience harness knobs, and
            // `vike_config::flags`' module doc names that family as deliberately NOT settings.
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
        if let Some((wid, tab)) = layout.data_subtab {
            // Pre-seed the window's `ToolView` so the Data Manager opens on the requested sub-tab.
            // The window loop `entry(..).or_default()`s this map, so writing it here is simply
            // what it will find — no ordering subtlety beyond being before the first frame.
            app.tool_views.entry(wid).or_default().data_subtab = tab;
        }
        if let Some((wid, account)) = layout.connections_account {
            // Same pre-seed as the sub-tab above, for the same reason: the window loop
            // `entry(..).or_default()`s this map, so writing it here is simply what it will find.
            // ⚠ It is a PENDING value, not a setting — `connections_tool_content` `take`s it on the
            // first frame it renders, so the operator's own chip clicks are never re-overridden.
            app.tool_views.entry(wid).or_default().connections_account = Some(account);
        }
        if let Some(wid) = layout.resolve_poly_token {
            app.spawn_poly_token_resolver(wid);
        }
        app.next_win_n = app.wins.len() as u32;
        app
    }
}

/// Simple title bar for tool windows: [icon] name … ─ □ ✕ (draggable). Returns
/// the frame's actions + drag delta.
fn tool_title_bar(
    ui: &mut egui::Ui,
    kind: workspace::WinKind,
    is_max: bool,
) -> (TitleActions, egui::Vec2) {
    use egui::{Align, Button, Layout, RichText, Sense, UiBuilder, Vec2};
    let mut a = TitleActions::default();
    const BAR_H: f32 = 30.0;
    let (bar_rect, bar_resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), BAR_H), Sense::click_and_drag());
    // (no bottom divider — vike's title bar has none)
    if bar_resp.double_clicked() {
        a.toggle_max = true;
    }
    let drag = if bar_resp.dragged() { bar_resp.drag_delta() } else { Vec2::ZERO };
    let ctrl_w = 3.0 * 30.0 + 6.0;
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(egui::Rect::from_min_max(
                bar_rect.min + egui::vec2(8.0, 1.0),
                egui::pos2(bar_rect.max.x - ctrl_w, bar_rect.max.y - 1.0),
            ))
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.label(RichText::new(kind.icon()).size(15.0));
            ui.add_space(6.0);
            ui.label(
                font::extralight(kind.label()) // vike title font-weight:200
                    .size(15.0)
                    .color(theme::TEXT_UI),
            );
        },
    );
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(bar_rect.shrink2(egui::vec2(0.0, 1.0)))
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.visuals_mut().button_frame = false;
            let ctrl = |ui: &mut egui::Ui, g: &str, tip: &str| -> bool {
                ui.add_sized([30.0, BAR_H - 2.0], Button::new(RichText::new(g).size(15.0)))
                    .on_hover_text(tip)
                    .clicked()
            };
            if ctrl(ui, "✕", "Close") {
                a.close = true;
            }
            if ctrl(ui, if is_max { "❐" } else { "□" }, "Maximize / restore") {
                a.toggle_max = true;
            }
            if ctrl(ui, "─", "Minimize to rail") {
                a.minimize = true;
            }
        },
    );
    (a, drag)
}

/// The initial cockpit token-id: `VIKE_POLY_COCKPIT_TOKEN` (a real YES token-id an operator
/// supplies) when set and non-empty, else [`POLY_PLACEHOLDER_TOKEN`], which triggers background
/// Gamma resolution ([`App::spawn_poly_token_resolver`]).
///
/// The env READ stays here — this binary owns it, and the settings-registry row with it. The
/// trim / empty / placeholder ladder is [`vike_app_core::startup::poly_cockpit_seed_token`], where
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
    dom_venue: &str,
    dom_book: Option<&vike_model::L2Book>,
    dom_stale: bool,
    dom_live: bool,
    td: &tools::ToolData,
    feeds: &[(String, usize, i64, i64)],
    flags: &HashMap<String, egui::TextureHandle>,
    logos: &HashMap<String, egui::TextureHandle>,
    dsets: &datasets::Store,
    snap: &vike_core::CoreSnapshot,
    tv: &mut tools::ToolView,
    display_tz: DisplayTz,
    studio: &mut Option<StudioStateTy>,
    studio_error: &mut Option<String>,
    stored_tree: &Arc<Vec<inventory::VenueNode>>,
    stored_gaps: &Arc<vike_data_manager::GapMap>,
    stored_partials: &Arc<vike_data_manager::PartialDayMap>,
    stored_coverage: vike_app_core::stored_mode::RemoteCoverage,
    stored_loading: bool,
    stored_backfill_status: &str,
    opt_refresh: &std::sync::mpsc::Sender<()>,
    feed_statuses: &HashMap<String, Arc<std::sync::Mutex<String>>>,
    poly_names: &HashMap<String, String>,
    backend_picker: &tool_views::BackendPicker<'_>,
    backend_action: &mut Option<vike_app_core::backend_conn::BackendAction>,
    backend_editor: &mut vike_app_core::backend_editor::EditorState,
    backend_registry_update: &mut Option<vike_app_core::backend_editor::RegistryUpdate>,
    backend_settings: &tool_views::BackendSettingsState,
    backend_settings_refresh: &mut bool,
    backend_settings_edit: &mut tool_views::SettingsEditState,
    backend_settings_write: &mut Option<tool_views::SettingsWriteRequest>,
    // The RESOLVED datahub address (`App::resolved_datahub_addr` — explicit `config.datahub_addr`
    // first, else the active backend's Welcome advertisement; REQ-2), threaded as a PARAMETER so
    // this free fn reads the same resolution the App methods do, never the raw key.
    resolved_datahub_addr: Option<&str>,
) {
    use workspace::WinKind::*;
    ui.add_space(6.0);
    // The Stored tab's mode gates (the #1378 seam close): which of the grid's actions/columns
    // exist over the store the grid is reading — resolved from the SAME resolved datahub address
    // `refresh_stored` branches on, so the read mode and the offered actions cannot disagree.
    // Two inputs, two features: the RESOLVED datahub address (REQ-2 — explicit key, else the
    // active backend's advertisement) and `stored_coverage`, the §6-Q2 answer about what the last
    // load NEGOTIATED for the coverage verb, which the address alone cannot say (a remote server
    // may be older than the verb).
    let stored_mode_table =
        vike_app_core::stored_mode::stored_mode(resolved_datahub_addr, stored_coverage);
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
            backfill_status: stored_backfill_status,
            delete_unavailable: stored_mode_table.delete_unavailable,
            partials_note: stored_mode_table.partials_unavailable,
        },
        feed_statuses,
        // Where a Save in the Connections editor LANDS and where it is RECORDED — the store and the
        // change journal, both off the ONE boot walk. See `credential_write_ctx`.
        credentials: credential_home().write_ctx(vike_model::now_ms()),
        // The two book-backed windows share ONE transport group (a window is EITHER a DOM or a
        // cockpit, never both): for the cockpit `symbol` is the token-id, `dom_venue` is
        // "polymarket" and `dom_book` is that token's live L2 book from the same `BookStore`.
        book: tool_views::BookCtx {
            symbol,
            venue: dom_venue,
            book: dom_book,
            stale: dom_stale,
            live: dom_live,
        },
        poly_names,
    };
    match kind {
        Trade => tool_views::trade_tool_content(ui, &tool_ctx(), tv),
        Dom => tool_views::dom_tool_content(ui, &tool_ctx(), tv),
        Polymarket => tool_views::cockpit_tool_content(ui, &tool_ctx(), tv),
        Options => {
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
            let books = vike_app_core::options_books::build_books(&snap.orders, &snap.positions);
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
                },
            );
            // A working-order marker click → stash the coid in the OUT slot; the update loop drains
            // it into an `OrderIntent::Cancel` (mirrors the DOM's `DomAction::Cancel` routing).
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
            // Prefilled CONFIRM ticket modal (centre-anchored). SAFETY: the order is handed to the
            // core (via `tv.opt_submit`) ONLY on Confirm — never on the chain click that opened it.
            if let Some(mut ticket) = tv.opt_order_ticket.take() {
                let mut confirmed = false;
                let mut cancelled = false;
                let side_txt = if ticket.side > 0 { "BUY" } else { "SELL" };
                let cp = if ticket.is_call { "C" } else { "P" };
                egui::Window::new(format!("Order · {}", ticket.instrument))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                    .show(ui.ctx(), |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{side_txt} {} · strike {} {cp}",
                                ticket.instrument, ticket.strike
                            ))
                            .strong(),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("Limit price");
                            ui.add(egui::DragValue::new(&mut ticket.price).speed(0.0001));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Quantity");
                            ui.add(
                                egui::DragValue::new(&mut ticket.qty)
                                    .speed(0.01)
                                    .range(0.0..=f64::MAX),
                            );
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let (fill, label) = if ticket.side > 0 {
                                (theme::UP, "Confirm Buy")
                            } else {
                                (theme::DOWN, "Confirm Sell")
                            };
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new(label).color(egui::Color32::WHITE),
                                    )
                                    .fill(fill),
                                )
                                .clicked()
                            {
                                confirmed = true;
                            }
                            if ui.button("Cancel").clicked() {
                                cancelled = true;
                            }
                        });
                    });
                if confirmed {
                    // Hand the finalized (edited) order to the update loop, which submits it to the
                    // deribit engine right after the DOM actions. Ticket cleared (not restored).
                    tv.opt_submit = Some(ticket);
                } else if !cancelled {
                    // still open — persist edits (price/qty) for the next frame
                    tv.opt_order_ticket = Some(ticket);
                }
                // cancelled → leave both None (ticket dropped, nothing submitted)
            }
        }
        Greeks => tool_views::greeks_tool_content(ui, &tool_ctx()),
        Tearsheet => tool_views::tearsheet_tool_content(ui, &tool_ctx(), tv),
        News => tool_views::news_tool_content(ui, &tool_ctx(), tv),
        Calendar => tool_views::calendar_tool_content(ui, &tool_ctx(), tv),
        // The Venues sub-tab is the ONLY one that reads the credential store and `policy.toml`, so
        // only it pays for the per-frame read (`data_tab_reads_arming` owns the index — no `6`
        // literal here). `backend_picker.available` is `!has_local_core`
        // (`backend_conn::switching_available`), which is the observe-vs-local fact
        // `split_plane::app_mode` takes.
        Data => {
            let arming = tool_views::data_tab_reads_arming(tv)
                .then(|| venue_arming_inputs(!backend_picker.available));
            tool_views::data_tool_content(ui, &tool_ctx(), tv, arming.as_ref())
        }
        // The credential read stays in this binary (libraries take configuration as parameters) —
        // a FRESH per-frame read, exactly as the in-body read it replaced, so the grid stays live
        // after an in-app save or an external edit. Only this arm pays for it. It now reports on
        // the project store, which is the one file every venue mount reads.
        Connections => tool_views::connections_tool_content(
            ui,
            &tool_ctx(),
            tv,
            &workspace_credentials(),
            backend_picker,
            backend_action,
            backend_editor,
            backend_registry_update,
            backend_settings,
            backend_settings_refresh,
            backend_settings_edit,
            backend_settings_write,
        ),
        Studio => {
            // Lazy, one-shot store open (see `App::studio`'s doc) — tried once; success or
            // failure both stick, so a broken store never retries `DataFusionHist::open` (a real
            // cost: a tokio runtime + WAL recovery) every frame this window stays visible.
            // Unconditional since split-plane I7, and now REMOTE-ONLY: `open_studio_store` has one
            // arm (a resolved datahub — explicit key or backend advertisement) and one Err naming
            // both fixes. The old static "requires the full (fat) build" label went with the gate;
            // the local-store arm went with the `fat` feature itself.
            if studio.is_none() && studio_error.is_none() {
                match open_studio_store(resolved_datahub_addr.map(str::to_string)) {
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
                        if let Some(addr) = remote_addr {
                            // A remote store makes remote compute the norm: the same datahub
                            // serves the HistStore verbs AND the run verbs, so seed the Backend
                            // to it, and arm the split-plane tick rule (a tick slice on the
                            // Local backend would pull the tape over the wire — the Studio
                            // refuses it with a one-liner).
                            st.store_is_remote = true;
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
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 110, 130),
                        format!("Studio: no data store ({msg})"),
                    );
                }
            }
        }
        Chart => {}
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
/// headless daemon's Scope::Control channel (a `--observe` observer with control enabled). Built
/// once at the command choke point in [`App::ui`] from `(self.core, self.remote_ctrl)` and borrows
/// the chosen handle for that block, so EVERY command site funnels through one [`Dispatch::send`].
/// The local path stays byte-identical (`Local` forwards straight to `CoreHandle::try_command`);
/// the remote path lowers each command through `vike_app_core::tradehub_control::wire_from_command`
/// and drops (with a log) anything that has no thin-wire form.
enum Dispatch<'a> {
    Local(&'a vike_core::CoreHandle),
    Remote(&'a vike_tradehub_client::RemoteControlHandle),
}

impl Dispatch<'_> {
    /// Fire one command — fire-and-forget on both arms, mirroring `CoreHandle::try_command`. A
    /// remote command with no thin-wire form (`wire_from_command` → `None`) is logged and dropped.
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
            Dispatch::Remote(ctrl) => {
                match vike_app_core::tradehub_control::wire_from_command(&cmd) {
                    Some(wire) => {
                        let _ = ctrl.try_command(wire);
                    }
                    None => {
                        tracing::warn!("remote control: command has no wire form yet; dropped");
                    }
                }
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
/// [`App::ui`] for what it exists to measure and why it is off by default.
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
        // `crates/vike-ops/tests/settings_registry.rs` classifies every `env::var` read by FILE
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
            // of its own rather than a seventh `window_spawn` arm). The ten env READS stay HERE,
            // in the binary, injected as `ArrangeEnv` — the same shape `startup_env` takes at
            // `App::new` — so every `VIKE_*` settings-registry row stays `vike-app`/`Binary`.
            let env = initial_arrange::ArrangeEnv {
                arrange: std::env::var("VIKE_ARRANGE").ok(),
                tool: std::env::var("VIKE_TOOL").ok(),
                tools: std::env::var("VIKE_TOOLS").is_ok(),
                dom_mode: std::env::var("VIKE_DOM_MODE").ok(),
                dom_group: std::env::var("VIKE_DOM_GROUP").ok(),
                dom_venue: std::env::var("VIKE_DOM_VENUE").ok(),
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
            // block performed these steps: close-all, counter, then per spawn feed → depth →
            // push → resolver → DOM view seed → cal page, then the arrange, the remember, and
            // the two QA hooks last.
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
                if let Some((venue, canonical)) = &spawn.ensure_depth {
                    self.ensure_depth(*venue, canonical);
                }
                let wid = spawn.win.id;
                let resolve = spawn.resolve_poly_token;
                self.wins.push(spawn.win);
                if let Some(r) = resolve {
                    self.spawn_poly_token_resolver(r);
                }
                if let Some(seed) = &spawn.dom_view {
                    let tv = self.tool_views.entry(wid).or_default();
                    initial_arrange::seed_dom_view(&mut tv.dom, seed);
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

/// One theme mapped from vike theme.py — unifies menus, combos, buttons, popups,
/// panels and the chart canvas (colors / padding / rounding / text weights).
fn install_visuals(ctx: &egui::Context) {
    use egui::{CornerRadius, Margin, Stroke};
    let bg = theme::BG; // hsl(215,28,7)
    let surface = theme::SURFACE;
    let hover = theme::HOVER;
    let border = theme::BORDER;
    let text = theme::TEXT_UI;
    let text2 = theme::TEXT2;
    let cr = CornerRadius::same(4);

    let mut v = egui::Visuals::dark();
    v.panel_fill = bg;
    // vike: windows / title bars / rail / dialogs are BG (13,17,23) — same dark as the
    // plot, only a 1px border distinguishes them. SURFACE is for panels/tables/menus only.
    v.window_fill = bg;
    v.extreme_bg_color = bg; // egui_plot canvas
    v.faint_bg_color = surface;
    v.window_stroke = Stroke::new(1.0, border);
    v.window_corner_radius = CornerRadius::same(4);
    v.menu_corner_radius = CornerRadius::same(4);
    v.selection.bg_fill = hover;
    v.selection.stroke = Stroke::new(1.0, text);
    // No drop-shadow / popup halo — that soft dark halo is what made tiled windows
    // look gutter-padded vs vike's flush 2px tiling.
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;

    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = cr;
        w.bg_stroke = Stroke::new(1.0, border);
    }
    v.widgets.noninteractive.bg_fill = surface;
    v.widgets.noninteractive.weak_bg_fill = surface;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text2);
    v.widgets.inactive.bg_fill = surface;
    v.widgets.inactive.weak_bg_fill = surface;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, text2);
    v.widgets.hovered.bg_fill = hover;
    v.widgets.hovered.weak_bg_fill = hover;
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, text);
    v.widgets.active.bg_fill = hover;
    v.widgets.active.weak_bg_fill = hover;
    v.widgets.active.fg_stroke = Stroke::new(1.0, text);
    v.widgets.open.bg_fill = hover;
    v.widgets.open.weak_bg_fill = hover;
    v.widgets.open.fg_stroke = Stroke::new(1.0, text);

    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.button_padding = egui::vec2(8.0, 4.0);
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.menu_margin = Margin::same(4);
        // Keep a 6px window body inset: egui's edge resize-grab zone is ~5px, so a
        // tighter margin lets the chart plot cover the resize handles (drag gets eaten
        // by the plot's pan). 6px is still far tighter than vike's 16px chart inset;
        // the killed window_shadow is what actually tightens the grid gutters.
        s.spacing.window_margin = Margin::same(0); // flush title bar like vike (no window inset)
    });
}

/// Register Segoe UI Symbol as a fallback font so the title-bar control glyphs
/// (✕ ─ □ ❐ ●, the ▦/∿ style icons) render instead of tofu boxes. egui's bundled
/// font lacks them; Qt/Python gets them from the system fonts for free.
fn install_fonts(ctx: &egui::Context) {
    fn try_load(fonts: &mut egui::FontDefinitions, name: &str, path: &str) -> bool {
        match std::fs::read(path) {
            Ok(data) => {
                fonts
                    .font_data
                    .insert(name.to_owned(), std::sync::Arc::new(egui::FontData::from_owned(data)));
                true
            }
            Err(_) => false,
        }
    }
    let mut fonts = egui::FontDefinitions::default();
    // Match Python's theme.py exactly. UI text FONT_UI = "Inter","Segoe UI",… — Inter
    // isn't installed here, so Python falls back to Segoe UI **Regular** (NOT Light — an
    // earlier guess). egui's bundled Ubuntu-Light is a different face, so we register
    // Segoe UI Regular as the primary proportional. Semibold = the bold face (egui has no
    // synthetic-weight axis). NUMBERS use FONT_MONO = "JetBrains Mono","Cascadia Code",
    // Consolas — JetBrains isn't installed → Cascadia Code is what Python renders for every
    // price/table figure; we map it onto egui's Monospace family.
    // One Segoe .ttf per weight Python requests (200/300→Light, 400→Regular, 600→Semibold,
    // 700→Bold) — egui has no numeric weight axis, so each is a separately-registered face
    // exposed as a named family (see the `font` module). Numbers → Cascadia Code (FONT_MONO).
    // ⚠ The candidate TABLES live in `vike_app_core::fonts`, not here, and that is where their
    // guard lives too. Every path used to be `C:\Windows\Fonts\…`, so on Linux all eight loads
    // failed — silently, because a missing font file is a `false` and not an error — and the
    // title-bar controls came out as TOFU BOXES for every Linux user and for the container image.
    // A test in this crate would not have caught it: `vike-app` is CI-excluded and executed by
    // almost nothing. Down there, `some_symbol_font_exists` runs on every PR, on Linux.
    use vike_app_core::fonts as fc;
    let load = |fonts: &mut egui::FontDefinitions, face: &fc::FaceCandidates| -> bool {
        fc::first_existing(face).is_some_and(|p| try_load(fonts, face.name, p))
    };
    let r400 = load(&mut fonts, &fc::PROPORTIONAL_400);
    let w600 = load(&mut fonts, &fc::PROPORTIONAL_600);
    let w700 = load(&mut fonts, &fc::PROPORTIONAL_700);
    let w300 = load(&mut fonts, &fc::PROPORTIONAL_300);
    let w350 = load(&mut fonts, &fc::PROPORTIONAL_350);
    let sym = load(&mut fonts, &fc::SYMBOLS); // ✕ ─ □ ❐ ● — the reported defect
    let emj = load(&mut fonts, &fc::EMOJI);
    let mono = load(&mut fonts, &fc::MONOSPACE);

    let glyphs: Vec<String> = [(sym, "seguisym"), (emj, "seguiemj")]
        .into_iter()
        .filter(|&(ok, _)| ok)
        .map(|(_, n)| n.to_string())
        .collect();
    // a family list = [the weight face, …glyph fallbacks, Regular as last resort]
    let fam = |face: &str| -> Vec<String> {
        let mut v = vec![face.to_string()];
        v.extend(glyphs.iter().cloned());
        if r400 && face != "segoe_400" {
            v.push("segoe_400".to_string());
        }
        v
    };
    // Proportional primary = Regular (400) — covers default + font-weight:400
    if r400 {
        let prop = fonts.families.entry(egui::FontFamily::Proportional).or_default();
        for (i, n) in fam("segoe_400").into_iter().enumerate() {
            prop.insert(i, n);
        }
    }
    // named weight families used via the `font` helpers
    if w300 {
        // Segoe has no static ExtraLight face → 200 maps to Light, exactly as Qt resolves it
        // for Python (font_compare.png: Py 200 → resolved weight 300). Both share segoe_300.
        fonts.families.insert(egui::FontFamily::Name("extralight".into()), fam("segoe_300"));
        fonts.families.insert(egui::FontFamily::Name("light".into()), fam("segoe_300"));
    }
    if w350 {
        fonts.families.insert(egui::FontFamily::Name("semilight".into()), fam("segoe_350"));
    }
    if w600 {
        fonts.families.insert(egui::FontFamily::Name("semibold".into()), fam("segoe_600"));
    }
    // 700 → Bold face if present, else fall back to Semibold
    let bold_face = if w700 { "segoe_700" } else { "segoe_600" };
    if w700 || w600 {
        fonts.families.insert(egui::FontFamily::Name("bold".into()), fam(bold_face));
    }
    if mono {
        let m = fonts.families.entry(egui::FontFamily::Monospace).or_default();
        m.insert(0, "cascadia".to_string());
        if sym {
            m.push("seguisym".to_string());
        }
    }

    // ── EVERY named family must be bound by the time this returns, on EVERY platform ────────────
    //
    // ⚠ This is a PANIC fix, not a cosmetic fallback. Every face above is loaded from a
    // `C:\Windows\Fonts\…` path, so on Linux and macOS all seven `try_load`s fail, every
    // `if w300` / `if w350` / `if w600` guard above is false, and the named families are never
    // inserted at all. epaint then panics on the FIRST use — `FontFamily::Name("extralight") is not
    // bound to any fonts` — which is `crates/vike-desktop/src/main.rs`'s own title bar, so the process
    // dies in the first frame.
    //
    // That means `vike-app` could not start on Linux, and nothing in the tree caught it: the
    // `png-export` lane renders `vike-chart` and `vike-studio` EXAMPLES rather than this binary,
    // and `app-check` only compiles. The release publishes a Linux GUI asset that had therefore
    // never been run. Found 2026-08-30 by running it — in a container, on lavapipe.
    //
    // The binding is to whatever `Proportional` resolved to, which is egui's own bundled face when
    // no platform font loaded. Weights collapse to one face off Windows; that is a LOOK regression
    // and the alternative is a crash. On Windows every family is already present, so this loop
    // inserts nothing and the appearance is byte-identical.
    let proportional: Vec<String> =
        fonts.families.get(&egui::FontFamily::Proportional).cloned().unwrap_or_default();
    for named in ["extralight", "light", "semilight", "semibold", "bold"] {
        fonts
            .families
            .entry(egui::FontFamily::Name(named.into()))
            .or_insert_with(|| proportional.clone());
    }

    ctx.set_fonts(fonts);
}

/// Centralized font helpers — set weight via these, never inline `FontFamily::Name(...)`
/// at call sites. Each maps to a Segoe UI face registered in `install_fonts`, matching a
/// Python `font-weight` (200→extralight, 600→semibold, 700→bold). (`reg`/`light`/`semilight`/`mono`
/// helpers existed but were never called — dropped in the dedup-app cleanup; add them back if a
/// call site ever needs one. The plain-`RichText`/`.monospace()` defaults cover 400 and mono.)
mod font {
    // The font-weight RichText helpers moved to the shared `vike_ui_theme::font` leaf crate; re-
    // exported here so every `font::extralight` call site in this crate is unchanged. The named
    // `FontFamily` faces they reference are still registered by `install_fonts` below.
    // (`bold`/`semibold` left this crate with the Data tool body — tool-view extraction batch 2 —
    // so only `extralight` is re-exported now; the moved bodies name `vike_ui_theme::font`
    // directly. Re-add a name here the moment a vike-app call site needs it again.)
    pub use vike_ui_theme::font::extralight;
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

/// The status strip's remote-control segment: the line to paint, plus the two facts painted AROUND
/// it. `None` at the call site means there is no control channel at all — the default local path,
/// where the strip renders byte-identically to a build that never had one.
///
/// ⚠ It is a struct rather than three loose parameters for a reason that is NOT the argument count.
/// The loose form admitted a state that means nothing — a `connected` answer, or a latched error,
/// with no segment to paint either on (`(None, true, true)` type-checked and was silently ignored)
/// — and `Option<ControlSegment>` cannot express it. `clippy::too_many_arguments` falling quiet is
/// a consequence; an `#[allow]` was the alternative and would have kept the nonsense state.
///
/// ⚠ `connected` is the handle's OWN `RemoteControlHandle::is_connected` bool, and it travels HERE,
/// beside the line it was built from, precisely so nobody re-derives it by searching that line for
/// `"disconnected"` — the round trip through prose this segment's dot was just fixed for. The two
/// fields are constructed together, from one read, in `app_ui.rs`.
#[derive(Clone, Copy)]
struct ControlSegment<'a> {
    /// `vike_app_core::tradehub_control::control_status_line`'s output — it already carries the
    /// daemon identity tag and the truncated `last_error` tail.
    line: &'a str,
    connected: bool,
    /// Whether a LATCHED server error is showing, which is what makes the segment click-to-dismiss.
    has_error: bool,
}

/// The bottom status bar: a live connection dot + status text on the left, workspace summary
/// (open charts / venues / timezone / clock) on the right — the egui analog of the PySide status
/// bar. Purely presentational.
/// Returns `true` when the operator CLICKED the remote-control segment to dismiss its latched
/// error (only possible while [`ControlSegment::has_error`]); the caller then calls
/// `RemoteControlHandle::clear_last_error`. Nothing else clears it — a refusal stays on the strip
/// until it has been seen, which is what a status-bar "last error" is for.
fn status_bar(
    ui: &mut egui::Ui,
    status: &str,
    display_tz: DisplayTz,
    n_charts: usize,
    n_venues: usize,
    control: Option<ControlSegment<'_>>,
) -> bool {
    use egui::{Align, Color32, Layout, RichText};
    // 1px top divider so the strip reads as a distinct bar against the desktop.
    let r = ui.max_rect();
    ui.painter().hline(r.x_range(), r.top(), egui::Stroke::new(1.0, theme::BORDER));

    // Connection state → dot color, through the ONE shared classifier
    // (`vike_model::feed_status::parse_feed_status`, which the Connections tool and the headless
    // reconcile health gate also read). This block used to hand-roll a second one; what that cost —
    // and exactly which strings changed colour when it went — is
    // `crates/vike-app-core/src/status_dot.rs`'s module doc.
    let dot_col = vike_app_core::status_dot::feed_dot_color(status);

    let mut dismissed = false;
    ui.horizontal_centered(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new("●").color(dot_col).size(10.0));
        ui.add_space(2.0);
        ui.label(RichText::new(status).color(Color32::from_gray(180)).size(12.0));

        // Remote Scope::Control channel summary (observe mode only; `None` — the default local
        // path — renders nothing, so the strip is byte-identical off this path). Painted in a loud
        // amber to flag that this observer can drive REAL orders on the remote daemon, with a red
        // dot when the control link is down — from `control.connected`, the handle's OWN
        // `is_connected` bool. It used to be recovered by searching the rendered line for
        // `"disconnected"`, i.e. from a string `control_status_line` had just built out of that same
        // bool; a connected channel whose latched `last_error` tail carried the word painted red.
        if let Some(control) = control {
            ui.add_space(12.0);
            ui.label(RichText::new("│").color(theme::BORDER).size(12.0));
            ui.add_space(12.0);
            let ctrl_col = vike_app_core::status_dot::control_dot_color(control.connected);
            let text = RichText::new(control.line).color(ctrl_col).size(12.0).strong();
            if control.has_error {
                // The segment carries a LATCHED server error, so make it click-to-dismiss: it
                // persists across repaints by design, and without this it would sit on the bar for
                // the rest of the session. A dismiss clears only the banner — it says nothing about
                // any command (each command's own outcome rides its `CommandTicket`).
                let resp =
                    ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text(
                        "click to dismiss this error (the command outcomes are unaffected)",
                    );
                dismissed = resp.clicked();
            } else {
                ui.label(text);
            }
        }

        // Right cluster: clock · timezone · venues · charts (laid out right-to-left).
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(8.0);
            let dim = Color32::from_gray(150);
            // Clock in the active display timezone (blank on tz-conversion failure).
            let now_ms = chrono::Utc::now().timestamp_millis();
            let clock = vike_chart::tz::to_naive(now_ms, display_tz)
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_default();
            ui.label(RichText::new(clock).color(dim).size(12.0).monospace());
            ui.add_space(10.0);
            ui.label(RichText::new(display_tz.label()).color(dim).size(12.0));
            ui.add_space(10.0);
            ui.label(RichText::new("│").color(theme::BORDER).size(12.0));
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!("{n_charts} chart{}", if n_charts == 1 { "" } else { "s" }))
                    .color(dim)
                    .size(12.0),
            );
            ui.add_space(10.0);
            ui.label(RichText::new(format!("{n_venues} venues")).color(dim).size(12.0));
        });
    });
    dismissed
}

/// Fetch the instrument catalog ONCE, off the UI thread, and send the aggregated
/// `vike_catalog::Catalog` over `tx` (drained in `update` into `App::symbols_catalog`). The one
/// remaining `CatalogProvider` — `DeribitCatalog`, keyless, and linked because the Options tool's
/// chain provider already brings that bridge in — hands over its instrument universe via
/// `list_instruments()`; a failure is logged and simply contributes nothing, so the search degrades
/// rather than failing. Wakes the UI once at the end so the newly-populated catalog is visible
/// without waiting for the next repaint. If nothing is yielded, nothing is sent (the picker keeps
/// its `SYMS` quick-picks).
///
/// ⚠ **This used to be the CROSS-VENUE catalog and no longer is** — eleven venue providers left
/// with the venue bridges (see the tombstone in the body).
fn spawn_catalog_fetcher(
    tx: std::sync::mpsc::Sender<vike_catalog::Catalog>,
    wake: impl Fn() + Send + 'static,
) {
    std::thread::spawn(move || {
        // ⚠ TOMBSTONE — this list used to carry TWELVE providers and now carries one.
        // `BinanceCatalog`, `BybitCatalog`, `OkxCatalog`, `AsterCatalog`, `HyperliquidCatalog`,
        // `AlpacaCatalog`, `PolymarketCatalog`, `OandaCatalog`, `DukascopyCatalog`, `FxcmCatalog`
        // and `CtraderCatalog` were each `#[cfg(feature = "fat")]` elements of this `vec!`, and the
        // credential map they were constructed from (`workspace_credentials()`, for the three
        // credential-gated ones) was gated the same way. Every one of them lives in a venue bridge
        // crate this binary no longer links — rulings 1 and 2 took the venue bridges out of the
        // desktop's dependency graph entirely — so the Symbol picker now falls back to the built-in
        // `SYMS` quick-picks plus whatever Deribit lists. Restoring any of them means restoring a
        // venue edge, which is the thing the rulings removed; the CROSS-VENUE universe belongs to
        // the backend now.
        let providers: Vec<Box<dyn vike_catalog::CatalogProvider>> =
            vec![Box::new(vike_deribit::DeribitCatalog)];
        let instruments: Vec<vike_catalog::Instrument> = providers
            .iter()
            .flat_map(|p| match p.list_instruments() {
                Ok(list) => {
                    tracing::info!("symbol catalog: {} {} instruments", p.venue(), list.len());
                    list
                }
                Err(e) => {
                    tracing::warn!("symbol catalog: {} fetch failed: {e}", p.venue());
                    Vec::new()
                }
            })
            .collect();

        if instruments.is_empty() {
            tracing::warn!("symbol catalog: all venue fetches failed — keeping quick-picks");
            return;
        }
        tracing::info!("symbol catalog: {} instruments across venues", instruments.len());
        let _ = tx.send(vike_catalog::Catalog::from_instruments(instruments));
        wake();
    });
}

/// The order-entry preview caps, derived ONCE from the deployment's policy ceiling
/// (`max_notional_per_order` in `<vike home>/policy.toml`) by [`resolve_policy`] and
/// read by the per-frame dispatch block.
///
/// A process-wide `OnceLock` rather than an `App` field on purpose: the value is a property of the
/// MACHINE (its `policy.toml`), not of a window, and it must be identical for every dispatch path
/// in the process. It replaces a per-frame `env::var` read of a variable that no longer exists, so
/// this is strictly less global state than before, not more.
///
/// `get_or_init(default)` at the read site keeps the GUI permissive rather than panicking if some
/// future entry point reaches the dispatch block without having run `main` (a test harness, say) —
/// exactly what an absent `policy.toml` yields anyway.
static POLICY_ORDER_LIMITS: std::sync::OnceLock<order_entry::OrderLimits> =
    std::sync::OnceLock::new();

/// The WHOLE resolved [`vike_config::Settings`], set by the same [`resolve_settings`] call that
/// fills [`POLICY_ORDER_LIMITS`].
///
/// It held only the `Policy` until the file layer was wired: this GUI reads six settings out of it
/// now — `config.store_root` (the Studio's bar store), `config.state_dir` (the strategy-state
/// sidecar), `preferences.chart_style`, `preferences.log_level`/`log_file_level` +
/// `config.log_dir` (the `vike_log::LogConfig`), and `flags.reconcile` /
/// `flags.oco_cancel_sibling_on_dead_exit` — on top of the `Policy` the twelve-venue live mount
/// projects onto `vike_run::MountPolicy`. Every one is still overridden by its own environment
/// variable, inside the loader.
///
/// Two statics rather than one because they are read from genuinely different places at genuinely
/// different times — the per-frame dispatch block and the one-shot mount — and both are filled by
/// ONE load: a second `vike_config::load` in this process could disagree with the first.
///
/// ⚠ The `Policy` this projects onto a twelve-venue mount is READ but no longer MOUNTED here: the
/// desktop links no `vike_run`/`vike_mount`, so the ceilings it loads feed the Connections screen
/// and the order-entry preview cap, and the mount they used to govern belongs to the daemon.
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

/// **The credential store: `<project>/settings/secrets.env`.**
///
/// The one place this binary asks "what credentials do I have" — the `--observe` arm, the
/// per-frame Connections tool and the Studio's chat keys all come through here. ⚠ It no longer
/// ARMS anything: with no local mount, a venue key present in the store buys this process nothing
/// (the Connections screen reports the policy ceiling and says so).
///
/// An unreadable store returns an EMPTY map with a `tracing::error!`, so a credential file that
/// cannot be opened drops every venue to paper instead of being papered over.
/// `crates/vike-bridge-core/tests/credential_chain_roots.rs` gates it.
///
/// The FILE read stays per call, deliberately: the Connections tool re-reads every frame so the
/// grid stays live after an in-app save or an external edit. Only the process-environment sweep is
/// cached.
fn workspace_credentials() -> HashMap<String, String> {
    let env = PROCESS_ENV.get_or_init(|| std::env::vars().collect());
    let mut creds = vike_bridge_core::credentials::load_workspace_secrets_from_env(env);

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
    vike_app_core::backend_registry::fill_node_keys_from_env(&mut creds, env);
    creds
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
/// * the CEILINGS come from a FRESH read of `policy.toml`, not from [`settings`]. That cell is the
///   BOOT-time load, and a tab whose Mode column still showed the pre-write value after a write
///   would read as broken. ⚠ The read is `tool_views::reload_venue_ceilings`, in the library, and
///   that is not a way around `crates/vike-boot/tests/one_owner.rs` — it is the shape that gate is
///   asking for: the directory is a parameter, so the function structurally cannot WALK, which is
///   the defect one-owner exists to prevent. A refused reload falls back to the boot copy.
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
    // `vike_run::venue_arming(&vars, venues)`, the SAME `would_mount_live_under` the local mount
    // consulted, so the screen could claim a TIER (paper/demo/live) for each venue. The desktop
    // links no mount now (rulings 1 and 2), so it must not claim one: `ceilings_only` reports the
    // policy CEILING alone and every row renders `ArmingBlock::NoMountInThisBuild`. Anything that
    // would restore a tier here has to come back through the backend, which is the only thing in
    // this system that still mounts a venue.
    let rows = vike_config::venue_arming::ceilings_only(venues);
    // `false` where this read `cfg!(feature = "fat")` — see the tombstone at the `observes` call in
    // `main`: there is no local-core build for the arm table to distinguish any more, so every
    // desktop is the feed-less observer.
    let mode = vike_app_core::split_plane::app_mode(false, !has_local_core)
        .unwrap_or(vike_app_core::split_plane::AppMode::ObserveOnly);
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
    let Some(user_data) = vike_model::state_path::user_data_dir_beside(
        vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
        SETTINGS_DIR.get().and_then(|d| d.as_deref()),
    ) else {
        return;
    };
    let report = vike_script::load_user_indicators(
        &user_data.join(vike_model::state_path::INDICATORS_SUBDIR),
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
    use vike_model::state_path::{PROJECT_TMP_DIR, RESEARCH_SUBDIR, RUNS_SUBDIR, STUDIES_SUBDIR};
    let vars = PROCESS_ENV.get_or_init(|| std::env::vars().collect());
    let user_data = vike_model::state_path::user_data_dir_beside(
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
        // The BINARY, spelled literally: `vike_backtest::runs::RunManifest::produced_by` carries
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
        settings: vike_boot::SettingsLoad::Load,
        // ...and the credential file may not ARM REAL MONEY. `secrets.env` is plaintext and parsed
        // last-wins, so appending ONE line to it — with no read access at all — would otherwise be
        // enough to flip this app's twelve-venue mount onto a live venue. `vike-boot` runs the
        // refusal at step 0, before the window, the feeds, the core and every mount, for that
        // reason. It does NOT change which sources arm (`vike_bridge_core::mainnet`'s converged
        // rule still reads both, deliberately); it makes an arming credential file STOP the process
        // rather than run it. See `vike_config::arming`.
        //
        // The LOADER is this binary's own ([`workspace_credentials`]), passed as a function:
        // `vike-boot` owns WHEN the store is opened and this binary owns HOW, which is what keeps
        // that crate free of `vike_bridge_core`'s transport stack — and what lets the FILE read
        // stay per call here, so the Connections tool's grid stays live after an external edit.
        credentials: vike_boot::Credentials::LoadWith(&workspace_credentials),
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
    // (`crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` is a ratchet that may shrink
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
    // ⚠ **A BRACKET, not a detector.** Nothing here observes a hand edit of
    // `<project>/settings/policy.toml` — there is no file watcher in the tree — and this record
    // does not pretend otherwise. Two consecutive anchors that DISAGREE prove something changed
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
    // `vike_app_core::backend_conn::startup_observe`, whose `StartupObserve` doc carries the
    // address ladder AND the regression that made the second fact necessary. The argv scan lives
    // there rather than here so a bare `--observe` (no address after the word) is a case CI runs.
    let startup = vike_app_core::backend_conn::startup_observe(std::env::args());
    // ⚠ THE MODE — and there is only ONE of them left. `vike_app_core::split_plane::observes`
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
    let observe_backend: Option<vike_app_core::backend_registry::BackendRecord> =
        vike_app_core::split_plane::observes(false, startup.requested).then_some(startup.record);
    // The address alone, for the places that only NAME the node (the window title, the
    // observer-arm branch): the connection itself takes the whole record.
    let observe_addr: Option<String> = observe_backend.as_ref().map(|r| r.addr.clone());
    // ⚠ The desktop never REFUSES to start over a missing address. It has no local trading core at
    // all, so it must have one — and `observes` above is unconditionally true, so `observe_addr`
    // always yields one (flag, else the active backend, else the local default) and the arm it
    // could have fallen into is unreachable by construction rather than by an early exit. The
    // window opens and the status bar says whether it connected, which is the thing a person can
    // act on; `exit(2)` before a window existed was not. ⚠ The `None` title arm below is therefore
    // unreachable now; it is left standing because `observe_addr` is still an `Option` and its
    // disposition (a first-run "not connected" title) belongs to ruling 6, not here.
    // Make observe mode unmistakable in the OS window title / taskbar (the IN-GUI indicator is the
    // bottom status bar, driven by `feed_status`); a normal run keeps the historic "— live" title.
    let window_title = match &observe_addr {
        Some(addr) => format!("Vike Trader — OBSERVING {addr}"),
        None => "Vike Trader — live".to_string(),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // Open MAXIMIZED (fill the screen) so the whole app — top caption AND bottom status
            // bar — is visible on any monitor. A fixed 1400x900 with no position let the OS drop
            // the frameless window partly off-screen (top-left clipped, status bar below the
            // edge) on smaller displays. `inner_size` is the restored-down size.
            .with_inner_size([1400.0, 900.0])
            .with_maximized(true)
            .with_title(window_title.clone())
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

#[cfg(test)]
mod font_binding_tests {
    /// Every `FontFamily::Name` this binary asks for must be BOUND after `install_fonts`, on the
    /// platform the test runs on.
    ///
    /// ⚠ This test exists because its absence shipped a `vike-app` that could not start on Linux at
    /// all. `install_fonts` loads every face from a `C:\Windows\Fonts\…` path; off Windows all of
    /// them fail, the `if w300` / `if w350` / `if w600` guards are false, and the named families are
    /// never inserted — so epaint panics on the FIRST use with `FontFamily::Name("extralight") is
    /// not bound to any fonts`. That is the title bar, in frame one.
    ///
    /// Nothing caught it: `png-export` renders `vike-chart`/`vike-studio` EXAMPLES rather than this
    /// binary, `app-check` only compiles, and the release published a Linux GUI asset that had
    /// never been run. It was found by running the binary in a container on lavapipe, 2026-08-30.
    ///
    /// The assertion is deliberately platform-BLIND: it names no font file and no OS, so it passes
    /// on Windows for the real reason (the Segoe faces loaded) and on Linux/macOS for the fallback
    /// reason, and it fails anywhere a future edit reintroduces an unbound family.
    #[test]
    fn every_named_family_is_bound_on_this_platform() {
        let ctx = egui::Context::default();
        super::install_fonts(&ctx);

        // Render a REAL frame that uses each family: the panic comes from epaint's layout, reached
        // through a widget, not from registration, so this is the path that actually killed the
        // process. The per-label width assertion below covers the second half — see its note.
        // ⚠ The `FullOutput` must be CLEARED, not dropped. `TexturesDelta` panics on drop with
        // unapplied deltas — "Deltas need to be handled" — which is the same trap
        // `CLAUDE.md` records against the egui 0.36 / wgpu 30 bump, where both PNG harnesses
        // dropped a harvested `FullOutput` and died on the first real GPU run. A test that renders
        // a frame and throws it away is that shape exactly, so it fails for a reason with nothing
        // to do with fonts unless it is handled here.
        let mut out = ctx.run_ui(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for named in ["extralight", "light", "semilight", "semibold", "bold"] {
                    let r = ui.label(
                        egui::RichText::new("vike")
                            .family(egui::FontFamily::Name(named.into()))
                            .size(14.0),
                    );
                    // ⚠ The WIDTH is the assertion, not the absence of a panic. Rendering alone
                    // catches only an ABSENT family — which is the bug this fixes, but it is half
                    // the surface. MEASURED by mutation on 2026-08-30: binding every family to an
                    // EMPTY list still rendered and still passed, because epaint declines an empty
                    // family gracefully and only panics on one that is not in the map at all. A
                    // zero-width galley is what an empty binding actually produces, so measuring it
                    // is what closes that half.
                    assert!(
                        r.rect.width() > 0.0,
                        "family {named:?} laid out to ZERO width — it is bound, but to no usable \
                         face, so every call site renders nothing"
                    );
                }
            });
        });
        out.textures_delta.clear();
    }
}

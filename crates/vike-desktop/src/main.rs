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
// The WHOLE feed-lifecycle cluster now lives there — not just the pure diffs (the orphan diffs)
// but the imperative half too:
// `ensure_feed_on` / `ensure_trade_feed_on` / `ensure_depth` / `ensure_poly_book` /
// `reap_orphaned_feeds`, which this file keeps only as one-line `App` methods that build the
// `FeedSlots`/`SeriesSlots` bundles. The module is imported WHOLE (rather than each function bare,
// the older style below) so every one of those wrappers reads as an explicit delegation.
use vike_app_core::ui::feed_lifecycle;
// The five appearance preferences (design system spec §5): the READS stay here, in
// `startup_appearance` (which `App::new` calls); the mapping from the resolved rows to an
// `Appearance`, and the session that applies and saves a change from the Settings window, are one
// crate down.
use vike_app_core::ui::appearance_settings::{self, AppearanceSession};
// `App::new`'s startup-layout tail (which windows open, which feeds they need, the two QA capture
// overrides) moved down too — same reason, same shape: this file reads the env knobs (into
// `GuiEnv`) and hands them over as `StartupEnv`; `App::apply_startup_layout` applies the returned
// `StartupLayout` in its documented order.
use vike_app_core::ui::startup;
use vike_app_core::ui::sync_group::{GroupFrame, sync_feed, sync_harvest};
// The NEW-WINDOW decision moved down beside `startup`, for the same reason and in the same shape:
// SIX places in the frame loop below each re-derived the cascade slot, the `WinState` construction
// and the market-data subscription by hand, in a file `xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI`
// keeps out of every roster lane — and two of the six had already drifted apart (one cascades from
// a different origin; a title-bar clone drops its source window's venue). They are one
// `SpawnRequest` roster now, whose branches CI executes, and `App::apply_spawn` below — which
// applies the returned `SpawnPlan` in its documented order — is the whole of what stayed here.
use vike_app_core::ui::window_spawn;
// The FIRST-FRAME arrange decision moved down beside it (actions-out batch 2): the VIKE_TOOL/
// VIKE_TOOLS QA capture arms — the two window-opening sites `window_spawn`'s module doc
// deliberately left behind, because they spend `tool-{n}` ids at the desktop origin with no
// cascade — plus the VIKE_ARRANGE parse and the arrange-vs-maximize-lone choice. This binary
// reads the QA knobs (into `GuiEnv`) and the frame loop injects them as `ArrangeEnv`; the guarded
// apply block in `ui` (`app_ui.rs`) is the whole of what stayed in this crate.
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

// DATA layer lives in the layer-correct crates now (R-DataClient slice): symbol universes
// (DataSets) -> vike-data. ⚠ TOMBSTONE — a `use vike_binance::market_feed as marketfeed;` stood
// here, `fat`-gated, and was the alias `build_local_feeds` constructed the Binance kline feed
// through. vike-binance is not a dependency of this crate any more (ruling 2).
use vike_data::datasets; // subscribe_bars/quotes/trades/book/unsubscribe/shutdown verbs

// ⚠ TOMBSTONE — `App::recorder`'s type stood here: an uninhabited enum (under `fat`, an alias of
// vike-store's `RecorderHandle`), so the field could only ever be `None` and its
// `shutdown` was unreachable by construction. The field, the type and the teardown step that took
// it went together with the GUI's other local-core leftovers (2026-10);
// `crates/vike-desktop/tests/app_local_core_gate/dead_leftovers.rs` keeps them out.

use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

// `App`'s non-constructor methods (feed lifecycle, workspace restore, backend settings,
// depth subscriptions, core sync) — split out of this file for size; a second `impl App { .. }`
// block. `App::new` (the constructor) is in `app_setup.rs`.
mod app_methods;
// `App`'s window-close teardown — the BODY of the `eframe::App::on_exit` hook, split out of this
// file for size as an INHERENT `App::run_bounded_teardown` the trait impl (`app_ui.rs`) forwards
// to in one line. ⚠ NOT a second `impl eframe::App for App`: coherence allows exactly one impl
// per trait (`E0119`), unlike the inherent split `app_methods.rs` uses. `persist_egui_memory`
// (one `false`) and `ui` stay in that impl — see `app_lifecycle`'s module doc.
mod app_lifecycle;
// The frame: the crate's one `impl eframe::App for App` (`ui`, `on_exit`, `persist_egui_memory`)
// and `ui`'s body as three free functions (`frame_begin` → `draw_chrome` → `draw_windows`). The
// environment the frame consults (the `VIKE_SHOT` capture, the `ArrangeEnv` knobs) arrives as
// `App::env`, the `GuiEnv` this file reads once. See `app_ui`'s module doc.
mod app_ui;
// GPU Candle Layer Phase 2 (Task 2): the wgpu candle pipeline + `CandleCallback` behind
// vike-chart's `GpuCandleItem` seam. Registered in `App::new` (see `gpu_ok`); the render toggle
// that feeds it is wired in Task 3.
mod chart_gpu;
// The chart window's title-bar toolbar (chart-style menu, symbol/interval dropdowns, sync/OF/Cmp
// popups, LIVE badge, window controls) — extracted out of this file to shrink it and co-locate
// the chart-window chrome; app-specific, so it stays in vike-desktop rather than vike-chart.
mod chart_window;
// The wgpu/eframe half of the GPU startup ladder (`vike_app_core::ui::gpu_startup` decides): the
// options of each attempt, the adapter selector, and the loop around `eframe::run_native`.
mod gpu_startup;

mod app_setup;
mod app_state;
mod launchers;
mod studio_wiring;
mod tool_window;
mod wiring;
// The LIVE smoke that drives this binary's own `App` against a running node (`#[ignore]`d, by hand
// only). A `*_smoke.rs` lives under `tests/`, and it joins the binary here because this crate has no
// library an integration test could reach `App` through.
#[cfg(test)]
#[path = "../tests/live/desktop_live_smoke.rs"]
mod desktop_live_smoke;

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
// already lived in vike-panels's `cockpit` module). The pure Polymarket label helpers moved with batch 1
// (their unit tests now run in CI). ⚠ TOMBSTONE — this file used to import one of them
// (`vike_app_core::ui::poly_labels::poly_market_short_name`) for `pick_updown_token`'s display name;
// that function is gone with the Polymarket bridge edge, so the import went with it, and the
// helper itself (and its tests) was deleted from vike-app-core with its last caller.
use vike_app_core::ui::tool_views;
// The cockpit's tuning constants + the resolved-order-intent enum moved with the cockpit body
// (batch 3); re-imported under their original bare names so the window bookkeeping and the
// action-drain match arms below read unchanged. The cockpit seed token's env READ deliberately
// STAYED here (`GuiEnv::poly_cockpit_token`).
use vike_app_core::ui::tool_views::{CockpitCmd, POLY_PLACEHOLDER_TOKEN, POLY_STALE_MS};

use app_state::{App, BAR_PLANE};
use chart_window::{COMPARE_COLORS, SYMS, TitleActions};
use launchers::LAUNCHERS;
use studio_wiring::open_studio;
use tool_window::tool_content;
use wiring::{chart_seed_dial, credential_home, named_run_observe_keys, remote_hist_store};

/// QA capture hook — PRESENT (any value, `is_ok()`): draw the two capture
/// overlays [`vike_app_core::ui::capture_seed::trendline_overlay`] derives (a trendline through the
/// series' extremes and a support level at its low) on every chart whose series has bars.
///
/// ⚠ This is the FIRST producer of `vike_chart::model::ChartState::overlays`, not a headless
/// stand-in for a click. That field is the chart's user-drawing layer, it has always been READ by
/// `crates/vike-chart/src/chart/price_render.rs`'s `paint_price_overlays`, and nothing in the
/// workspace has ever written it — so there is no drawing tool this bypasses. Writes nothing when
/// unset; an unset run leaves every `overlays` map empty exactly as before.
const CHART_DRAW_ENV: &str = "VIKE_CHART_DRAW";

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

/// `VIKE_FRAME_LOG=1` — log the achieved frame rate once a second. See the call site in
/// `App::ui` for what it exists to measure and why it is off by default.
const FRAME_LOG_ENV: &str = "VIKE_FRAME_LOG";

/// **Every process-environment variable the GUI's own code consults, read ONCE** — by
/// [`gui_env`], in [`main`], and kept as `App::env` for the code that uses it.
///
/// ⚠ Why a snapshot, and why HERE:
/// `crates/vike-ops/tests/settings_secrets/settings_registry/observe.rs`'s `layer_for` classifies
/// an environment read by FILE PATH, and only this file earns
/// `Layer::Binary`; a read anywhere else in the crate is a `Layer::Library` row the shrink-only
/// `LIBRARY_PIN` refuses. Reading every variable here and handing the values over is what lets the
/// code that USES them — `App::new`, the startup layout, the frame loop, the Studio window — live
/// in the file its role says. (The `std::env::vars()` sweep is the other route in, through
/// [`process_env`]; the credential chain and the dials read that map, not this struct.)
///
/// Every field is the RAW `std::env::var(NAME).ok()` of one variable, and every consumer applies
/// the parse its read site always applied: `is_ok()` is `.is_some()`, the frame-log switch is the
/// exact string `"1"`, `VIKE_SHOT_FRAME` is parsed in the capture block, and the Studio's autorun
/// hook is parsed when a Studio window first opens. The process environment does not change
/// while this binary runs (nothing in the workspace calls `set_var`), so the value read here is
/// the value every one of those later reads returned.
///
/// `Clone` because the GPU startup ladder builds the application creator once per attempt, and a
/// failed attempt never runs it.
#[derive(Clone)]
pub(crate) struct GuiEnv {
    /// [`CHART_DRAW_ENV`] — present ⇒ the two capture drawings (`App::chart_draw_pending`).
    pub(crate) chart_draw: Option<String>,
    /// `VIKE_SHOT` — the headless self-screenshot's output PATH; present ⇒ the frame loop's
    /// capture block runs and the startup layout is in `shot_mode`.
    pub(crate) shot: Option<String>,
    /// `VIKE_SHOT_WIN` — the window kind a capture opens (`startup::StartupEnv::shot_win`).
    pub(crate) shot_win: Option<String>,
    /// `VIKE_SHOT_FRAME` — the frame the capture starts requesting a screenshot at; parsed in the
    /// capture block, `180` when absent or unparsable.
    pub(crate) shot_frame: Option<String>,
    /// `VIKE_APPMAX` — present ⇒ the capture maximizes the whole app at frame 80.
    pub(crate) appmax: Option<String>,
    /// `VIKE_SCALE` — the capture's chart-scale override (`startup::StartupEnv::scale`).
    pub(crate) scale: Option<String>,
    /// `VIKE_POLY_COCKPIT_TOKEN` — the cockpit's initial token-id, through
    /// `vike_app_core::ui::startup::poly_cockpit_seed_token` at each of its three consumers (the
    /// startup layout, the first-frame arrangement, a launcher opening a cockpit).
    pub(crate) poly_cockpit_token: Option<String>,
    /// `VIKE_ARRANGE`, `VIKE_TOOL`, `VIKE_TOOLS`, `VIKE_CAL_PAGE`, `VIKE_MAX`, `VIKE_MIN` — the
    /// first-frame arrangement's QA knobs (`initial_arrange::ArrangeEnv`, field for field).
    pub(crate) arrange: Option<String>,
    pub(crate) tool: Option<String>,
    pub(crate) tools: Option<String>,
    pub(crate) cal_page: Option<String>,
    pub(crate) max: Option<String>,
    pub(crate) min: Option<String>,
    /// [`STUDIO_TAB_ENV`] and [`STUDIO_AUTORUN_ENV`] — the Studio's two QA capture hooks, handed
    /// to `vike_studio::StudioState::new_with_qa` when a Studio window first opens.
    pub(crate) studio_tab: Option<String>,
    pub(crate) studio_autorun: Option<String>,
    /// [`FRAME_LOG_ENV`] — the EXACT string `"1"` turns the frame-rate instrument on, the same
    /// idiom `VIKE_RECONCILE` uses: a fuzzy truthy parse would make `VIKE_FRAME_LOG=0` turn it ON.
    /// It used to be a per-process `OnceLock` read on the first frame, because `env::var` allocates
    /// and takes a lock and `ui` is the hottest loop in the binary; reading it here keeps both out
    /// of the frame.
    pub(crate) frame_log: Option<String>,
}

/// Read [`GuiEnv`] — the ONE place this crate's code calls `std::env::var`.
fn gui_env() -> GuiEnv {
    GuiEnv {
        chart_draw: std::env::var(CHART_DRAW_ENV).ok(),
        shot: std::env::var("VIKE_SHOT").ok(),
        shot_win: std::env::var("VIKE_SHOT_WIN").ok(),
        shot_frame: std::env::var("VIKE_SHOT_FRAME").ok(),
        appmax: std::env::var("VIKE_APPMAX").ok(),
        scale: std::env::var("VIKE_SCALE").ok(),
        poly_cockpit_token: std::env::var("VIKE_POLY_COCKPIT_TOKEN").ok(),
        arrange: std::env::var("VIKE_ARRANGE").ok(),
        tool: std::env::var("VIKE_TOOL").ok(),
        tools: std::env::var("VIKE_TOOLS").ok(),
        cal_page: std::env::var("VIKE_CAL_PAGE").ok(),
        max: std::env::var("VIKE_MAX").ok(),
        min: std::env::var("VIKE_MIN").ok(),
        studio_tab: std::env::var(STUDIO_TAB_ENV).ok(),
        studio_autorun: std::env::var(STUDIO_AUTORUN_ENV).ok(),
        frame_log: std::env::var(FRAME_LOG_ENV).ok(),
    }
}

/// The design system's appearance, from the five `preferences.*` rows this binary loaded at boot
/// (spec §5) — what `App::new` installs first. The five READS are spelled in THIS file so
/// `vike-cli config show` names `desktop` as their reader — a library read would say `yes`, and
/// `vike_config`'s consumed table pins each needle to `main.rs` — and the mapping is one crate
/// down, where CI runs it.
fn startup_appearance() -> vike_ui_theme::appearance::Appearance {
    appearance_settings::appearance_from(
        &settings().preferences.theme,
        &settings().preferences.market_colors,
        settings().preferences.header_gradient,
        &settings().preferences.density,
        &settings().preferences.text_size,
    )
}

/// The observe bridge's sleep between reads of the snapshot the node pushed — the
/// `preferences.observe_poll_ms` row, which also caps how often a chart can refresh from the node.
/// Read HERE (like the appearance rows above) so `vike-cli config show` names `desktop` as its
/// reader; both `connect_backend` call sites take it, and the loop that sleeps it runs on the
/// bridge's own thread, never the frame thread.
fn observe_poll() -> std::time::Duration {
    std::time::Duration::from_millis(u64::from(settings().preferences.observe_poll_ms))
}

/// How many bars back one chart read of the backend's store asks for — the
/// `preferences.chart_history_bars` row, handed to `store_bars::spawn_store_seed`.
fn chart_history_bars() -> i64 {
    i64::from(settings().preferences.chart_history_bars)
}

/// The Data Manager's default backfill window for a series with no recorded gap, in milliseconds —
/// the `preferences.backfill_lookback_days` row, still clamped per venue by the datahub's floors.
fn backfill_lookback_ms() -> i64 {
    vike_app_core::data::backfill_plan::default_lookback_ms(
        settings().preferences.backfill_lookback_days,
    )
}

/// The Studio chat's model ids and request timeout — the three `preferences.ai_*` rows, assigned to
/// `ChatPane::settings` when the Studio window first opens (`studio_wiring.rs`).
fn chat_settings() -> vike_studio::ChatSettings {
    vike_studio::ChatSettings {
        anthropic_model: settings().preferences.ai_anthropic_model.clone(),
        cerebras_model: settings().preferences.ai_cerebras_model.clone(),
        timeout: std::time::Duration::from_secs(u64::from(settings().preferences.ai_timeout_secs)),
    }
}

/// The operator's patience on a bad node link, from the two `preferences.*` rows this PC's database
/// holds (raise-only windows, refused below their built-in values at the write and the load): the
/// observe stream's silence limit and a control reply's wait. The reads are spelled HERE so
/// `vike-cli config show` names `desktop` as their reader; `vike_app_core`'s bridge takes the
/// result as a parameter and reads no setting. `settings()` is this process's boot snapshot, so a
/// changed row takes a restart.
fn link_patience() -> vike_tradehub_client::liveness::LinkPatience {
    vike_tradehub_client::liveness::LinkPatience::from_millis(
        settings().preferences.observe_stall_timeout_ms,
        settings().preferences.control_reply_timeout_ms,
    )
}

/// `preferences.vsync`, as the resolved settings hold it: the present mode the window opens with
/// (`gpu_startup`'s ladder) and what the Settings window's Graphics section calls "running". The
/// read is spelled here so `vike-cli config show` names `desktop` as its reader.
fn startup_vsync() -> bool {
    settings().preferences.vsync
}

/// `App::apply_startup_layout`'s input: the startup layout's env knobs, out of [`GuiEnv`],
/// plus the one settings row it takes.
///
/// The whole decision — restore-vs-QA-window-vs-default-chart, the feeds each of those windows
/// needs, and the VIKE_STYLE/VIKE_SCALE capture overrides — lives in the CI-gated
/// [`vike_app_core::ui::startup`] (six branches over five env knobs that no gate compiled; see that
/// module's doc). The READS stay in this binary and arrive as `StartupEnv`: that keeps every
/// `VIKE_*` row in the settings registry classified `vike-desktop` / `Layer::Binary`, and leaves
/// `startup::plan` a pure function its tests can drive without mutating process env.
fn startup_env(env: &GuiEnv) -> startup::StartupEnv {
    startup::StartupEnv {
        shot_mode: env.shot.is_some(),
        shot_win: env.shot_win.clone(),
        // The one knob here that is a real user PREFERENCE rather than a QA-capture override:
        // `preferences.chart_style`, still overridden by `VIKE_STYLE` inside the loader. The
        // other four are env values — `VIKE_SHOT*`, `VIKE_SCALE` and the cockpit token are
        // screenshot/dev-convenience harness knobs, and `vike_config::flags`' module doc names
        // that family as deliberately NOT settings.
        style: settings().preferences.chart_style.clone(),
        scale: env.scale.clone(),
        poly_cockpit_token: env.poly_cockpit_token.clone(),
    }
}

// ⚠ TOMBSTONE — `tick_store_root`, `counters_file_path` and `state_dir_path` stood here, and all
// three went with the local core (rulings 1 and 2). Each resolved a path this process WROTE to:
// the tick-recording store the `RecorderSink` filled, the mmap health-counters file `vike_stat`
// reads live, and the strategy-state sidecar directory every mounted strategy saved a
// `<mount_id>.json` into. The desktop mounts no strategy, records no tape and runs no core, so all
// three had zero callers — dead by CALLER rather than by text, which is why they survived the cut
// that removed their consumers and had to be deleted deliberately afterwards.
//
// ⚠ THEY TOOK THREE ENVIRONMENT READS WITH THEM — `VIKE_TICK_STORE`, `VIKE_COUNTERS_FILE` and
// `VIKE_STATE_DIR` — and that is a registry fact, not a comment: `vike_config::settings::SETTINGS`
// keys a row on `(name, krate)` with the crate DERIVED FROM THE PATH, so a row left behind here
// fails `every_declared_variable_is_read` exactly as loudly as a missing one fails the other
// direction. The first two rows are deleted (`vike-core` and `vike-tradehub` still declare those
// names for their own readers, so neither VARIABLE is undeclared); `VIKE_STATE_DIR` keeps its
// `vike-config` row, because that loader genuinely still looks the name up.
//
// The resolution LADDERS survive where they always lived and are not lost with the callers:
// `crates/vike-paths/src/tick_store_path.rs`'s `resolve_tick_store_root` is the shared home the
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
// DataFusion+Parquet store at the tick-store root and spawned the `vike_store::RecorderSink` writer
// actor, which persisted every live quote/trade/book this process saw; on failure `App::new` fell
// back to the bare `CoreSinkAdapter` so ticks still reached the core. Both ends are gone: there are
// no venue feeds producing ticks (ruling 2) and no local core to reach, and `RecorderSink` /
// `RecorderHandle` are `vike-store` exports this crate does not depend on. Recording
// live tape is the recorder daemon's job (`vike-recorder`), not the desktop's.

// Unconditional since split-plane I7 (was `fat`-gated): a REMOTE Studio session still needs this
// root as its `state_dir` (saved strategies live beside the local store path even when the bars
// come over the wire — see the Studio arm's `new_with_qa` call), and nothing here names DataFusion.
/// Resolve the Studio tool's bar-data store root: the `config.store_root` row else `<repo>/market_data/hist` —
/// mirrors `vike-backtest`'s `backtesthub` bin / `vike-backfill`'s backfill bins' `store_root`
/// convention exactly (see `crates/vike-backtesthub/src/backtest_cli.rs`), so the Studio browses the
/// SAME store those tools populate. Deliberately NOT the live-TICK store — which
/// is now a SIBLING of this one inside the same `<project>/market_data/` folder (`market_data/ticks` beside
/// `market_data/hist`, the layout `vike_paths::state_path::PROJECT_DATA_DIR` commits to) rather than the
/// `<exe_dir>`-relative path this line used to name: that one only ever receives recorded
/// quotes/trades/books (`RecorderSink`'s
/// bar-seam methods — `seed_bars`/`close_bar`/`forming_bar` — are no-ops, see
/// `vike_store::live_rec`), so it never has the `kind=bar` series the Studio's `SlicePicker` looks
/// for; the backfill/harness convention is the one that actually has bars in it.
fn studio_store_root() -> std::path::PathBuf {
    // ⚠ The dev-checkout rung is a DEBUG-BUILD rung. `env!("CARGO_MANIFEST_DIR")` is a string
    // literal in the binary, and `release.yml` builds this crate `--release` on a runner whose
    // checkout path names the runner account — so the `vike-app-fat`/`-thin`/`.exe` assets carried
    // that path into every public download, and `scripts/refuse_box_paths.sh` (the release's
    // box-path guard) refused them by name. A release build therefore passes NO rung and resolves
    // from the project walk down, which for anyone standing in a checkout is the same directory by
    // a different rung name; `vike_paths::store_path`'s module doc (rung 4) carries the argument.
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
    // spelled in exactly ONE file workspace-wide — `vike_paths::store_path` — and reaches it
    // through the environment MAP this binary collects, the same `std::env::vars()` sweep
    // `App::new` already builds elsewhere. The project walk likewise stays in
    // `vike_paths::state_path`; this binary only supplies the working directory it starts from.
    //
    // The store root itself comes from the `config.store_root` row (decision 0111: no variable
    // overrides it), so `resolve_store_root`'s second argument carries the row and the value an
    // operator sets reaches the Studio. The data daemon reads the same row; the collector bins and
    // `vike_backtesthub::binutil::store_root` take `--store` instead.
    //
    // ⚠ `resolve_store_root_from`, never the bare `resolve_store_root` ladder: that one's project
    // and per-user defaults are two adjacent `Option<PathBuf>`s, so transposing them here would
    // COMPILE SILENTLY and browse the wrong store. This form has no two arguments of the same type,
    // and `$VIKE_SETTINGS_DIR` reaches the project walk through the same map.
    let cwd = std::env::current_dir().ok();
    let resolved = vike_paths::store_path::resolve_store_root_from(
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
// the DataFusion engine was a `fat` enable and `fat` is gone (rulings 1 and 2 took the local
// data plane with the local trading core). Everything the Data Manager still does goes over the
// wire, through `vike_datahub_client::RemoteHistStore`.

/// The VENUE-CATALOG dial — `chart_seed_dial`'s twin, built from the same two `OnceLock`s and here
/// for the same reason: only a BINARY reads the process environment
/// (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`), so the SWEEP belongs in the composition root
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

// The `recon_mount_tests` module that stood here asserted the grammar of the reconcile master
// variable's exact-`"1"` check — a function this binary stopped calling long before it stopped
// reconciling at all, and that decision 0111 deleted (the gate is the `flags.reconcile` row, read by
// the daemon). ⚠ This binary now calls NOTHING of it: with no local mount there is no
// armed-live set, no `reconcile_gate` call and no `ReconDriver` in this process, so the whole
// reconcile story belongs to `vike-tradehub` and its
// `a_live_armed_start_reconciles_with_nothing_set_and_a_paper_one_does_not` is the composed test
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
//   • binance — `vike_binance::market_feed::Feeds`, whose earliest-live-id map was the
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
// pinning its own key set against a hand-written venue list in `vike_app_core::backend::split_plane`
// — that list is gone now: `MdSession::sync_mount` mounts one datahub feed (and one Status line)
// per venue the connected datahub advertises, and the assert went with the list.
//
// ⚠ `VIKE_POLY_TICKS` was read HERE and nowhere else in this binary; its
// `vike_config::settings::SETTINGS` row loses its consumer with this deletion, and this change does
// not touch that registry.
//
// Consumers left holding empty maps rather than missing code: `App::feeds` (so `ensure_feed_on` /
// `ensure_trade_feed_on` / `ensure_depth` / `ensure_poly_book` / `reap_orphaned_feeds` are all
// no-ops now), `App::books`, `App::trades`, `App::feed_statuses`, and the paging-floor map the
// backfill workers polled (deleted 2026-10-10 with the backfill lane's sender-less receivers and
// the GUI's backfill staging). The DOM
// ladder, the Polymarket cockpit and every tick/volume + orderflow chart therefore have no
// producer at all — `WireSnapshot` carries bars but neither a book nor a trade tape — and what
// happens to those surfaces is a product decision this change deliberately did not take.

// ⚠ TOMBSTONE — `tool_title_bar` lived here and is now
// `crates/vike-app-core/src/ui/workspace/title_bar.rs`'s `tool_title_bar`. It moved for the reason
// that module's own doc argues: a title bar that RESERVES a tab slot for whichever window kind
// carries tabs (`WinKind::carries_title_tabs` — no kind does today; Connections-merges-into-
// Data-Manager deleted the one window whose tabs these were, 2026-10-05, and the slot now sits
// idle for the next tabbed kind to paint into) is making geometry decisions, and this crate is in
// `xtask::ci::tables::roster`'s `EXCLUDE_FROM_CI`, so every one of them was gated by nothing.
// `crates/vike-app-core/src/ui/workspace/title_bar_tests.rs`'s
// `the_hairline_row_is_scissored_away_by_the_bar_and_survives_its_own_clip` runs them. The
// call site keeps the two-line mapping into `TitleActions`, which carries the CHART bar's
// symbol/venue/indicator fields a tool bar cannot produce.

// ⚠ TOMBSTONE — `pick_updown_token` lived here. It picked the YES-outcome token-id and a short
// display name of the top-volume active crypto up/down market out of a Polymarket Gamma catalog
// page (`GammaClient::list` returns volume-DESC, so the first up/down match was the highest-volume
// one), and it was the whole of the cockpit's Gamma resolver's fat body. Both went with the
// `vike-polymarket` dependency: the desktop opens no venue socket and makes no venue REST call any
// more (rulings 1 and 2), so there is nothing here that can resolve a real token; the resolver's
// empty stub, the planners' order to call it and the market-name cache it would have filled were
// deleted on 2026-10-10. ⚠ The cockpit has no BOOK either: `build_local_feeds` was its only book
// producer, so the window renders STALE; whether it is removed, hidden or pointed at the backend is
// a product decision not taken here. The cockpit
// window keeps whatever `vike_app_core::ui::startup::poly_cockpit_seed_token` seeded it with —
// `VIKE_POLY_COCKPIT_TOKEN` (`GuiEnv::poly_cockpit_token`) when an operator supplies one, else the
// placeholder.

/// Encode an egui framebuffer screenshot to `<exe_dir>/exports/chart-<ts>-<seq>.png`
/// (or `<preferences.export_dir>/…` when this PC's database has the row). This is the exact RGBA
/// readback + PNG encode the headless VIKE_SHOT self-shot uses (see the `update` fn), factored out
/// here for the user-triggered File -> Export chart image… action. `seq` disambiguates same-second
/// names. Returns the written path.
fn export_chart_png(img: &egui::ColorImage, seq: u32) -> std::io::Result<std::path::PathBuf> {
    let dir = match &settings().preferences.export_dir {
        Some(d) => d.clone(),
        // Same `<exe_dir>/…` convention as vike-log's `logs` and the workspace file.
        None => exe_dir().unwrap_or_default().join("exports"),
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
/// on top of the `Policy` the twelve-venue live mount projects onto `vike_tradehub_core::AdapterPolicy`. Each
/// is still overridden by its own environment variable inside the loader, except the five
/// appearance rows, which have none by ruling (design system spec §5).
///
/// Two statics rather than one because they are read from genuinely different places at genuinely
/// different times — the per-frame dispatch block and the one-shot mount — and both are filled by
/// ONE load: a second `vike_config::load` in this process could disagree with the first.
///
/// ⚠ The `Policy` this projects onto a twelve-venue mount is READ but no longer MOUNTED here: the
/// desktop links no `vike_tradehub_core` (which `vike-run` merged into, decision 0098), so the ceilings it
/// loads feed the order-entry preview cap, and the mount they used to govern belongs to the daemon.
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

/// [`PROCESS_ENV`], swept on first use — the one spelling the modules of this binary call.
fn process_env() -> &'static HashMap<String, String> {
    PROCESS_ENV.get_or_init(|| std::env::vars().collect())
}

/// **THE credential store of this process** — `vike_boot::Booted::store`, opened by the boot over
/// the SAME one walk [`SETTINGS_DIR`] holds, under the scope `VIKE_NO_LIVE_KEYS` named. Stored so
/// every later read ([`workspace_credentials_checked`], [`workspace_account_grids`]) goes through
/// that one handle rather than resolving the store again. An UNSET cell means
/// [`resolve_settings`] never ran.
static CREDENTIAL_STORE: std::sync::OnceLock<vike_secrets::CredentialStore> =
    std::sync::OnceLock::new();

/// **The credential store (the settings database), plus the NODE store beside it (its `hub_key`
/// table).**
///
/// The one place this binary asks "what credentials do I have" — the `--observe` arm, the
/// per-frame Connections tool and the Studio's chat keys all come through here. ⚠ It no longer
/// ARMS anything: with no local mount, a venue key present in the store buys this process nothing.
///
/// ⚠ **The two TRADEHUB hub keys do NOT come out of the venue store above** — they come from the
/// `hub_key` table, exactly as `vike-cli` and the daemon resolve them
/// (`docs/decisions/0051-node-keys-live-in-their-own-store.md`). That whole
/// resolution is `vike_app_core::backend::backend_registry::fill_hub_keys`, one crate down where CI runs
/// it; what is left here is the call, which needs the boot walk's answer and this binary's one
/// environment sweep.
///
/// An unreadable store returns an EMPTY map (`vike_secrets::Resolved::unreadable`) and its
/// `tracing::error!` is written by `vike_secrets::Resolved::report`, so a credential store that
/// cannot be opened drops every venue to paper instead of being papered over.
/// `crates/vike-secrets/tests/credential_chain_roots.rs` pins the root's read shape.
///
/// ⚠ **That degradation is right for an EXEC path and wrong for a path that RENDERS A COUNT**, so
/// this function has a twin that keeps the verdict — [`workspace_credentials_checked`] — and the
/// Gateway destination's grid carries the verdict of its OWN reads
/// ([`workspace_account_grids`]).
///
/// The FILE read stays per call, deliberately: the Data Manager's credential destinations re-read
/// every frame so they stay live after an in-app save or an external edit. Only the
/// process-environment sweep is cached.
fn workspace_credentials() -> HashMap<String, String> {
    workspace_credentials_checked().0
}

/// [`workspace_credentials`], plus whether the credential store could actually be OPENED.
///
/// ⚠ **ONE read, two answers** — never two reads. The map and the verdict have to describe the
/// same open of the same file, or the Connections tool could render a count taken from one read
/// beside a health taken from another.
///
/// The reason it exists: this read DEGRADES rather than refuses. A store that exists and cannot be
/// opened answers `vike_secrets::Resolved::unreadable` — an EMPTY map, byte-identical to the map
/// an unconfigured box produces, and a count folded from it would be a permissions bug wearing the
/// not-configured answer, which the root `CLAUDE.md`'s "Credentials & the live gate" forbids by
/// name. `vike_secrets::credentials::StoreHealth` is the verdict. (The credential panel no longer
/// folds this map: its grid is the account rows, read by [`workspace_account_grids`] with their
/// own verdict.)
///
/// ⚠ **The read itself logs nothing; `vike_secrets::Resolved::report` is called right after it**,
/// on every read, so each finding is written exactly as the loader this replaced wrote it from
/// inside: the unreadable store's `tracing::error!` per read, the permission finding once per
/// process (`vike_secrets::warn_once`), and the `VIKE_NO_LIVE_KEYS` withheld-count line.
fn workspace_credentials_checked()
-> (HashMap<String, String>, vike_secrets::credentials::StoreHealth) {
    let env = PROCESS_ENV.get_or_init(|| std::env::vars().collect());
    let Some(store) = CREDENTIAL_STORE.get() else {
        // Only reachable before [`resolve_settings`] has run, which nothing in this binary does:
        // LOUD rather than an empty map that would read as an unconfigured box.
        let why = "the credential store is not open yet: the boot has not run".to_string();
        return (HashMap::new(), vike_secrets::credentials::StoreHealth::Unreadable(why));
    };
    let resolved =
        store.read_all().unwrap_or_else(|e| vike_secrets::Resolved::unreadable(store.scope(), &e));
    resolved.report();
    let vike_secrets::Resolved { secrets, health, .. } = resolved;
    let mut creds = secrets.into_map();

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
    // resolver call, which would be `$VIKE_SETTINGS_DIR`-blind. (`vike_boot::boot`'s own credential
    // read, the one read that happens before the walk has answered, goes through `Booted::store`
    // and never comes through here.)
    vike_app_core::backend::backend_registry::fill_hub_keys(
        &mut creds,
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
        env,
    );
    (creds, health)
}

/// **The Gateway destination's grid** — the account rows, their key names and their
/// credentials by field, read through THE handle ([`CREDENTIAL_STORE`], under its scope) and
/// carrying the verdict of those very reads (decision 0156 item 6: a credential is its account
/// row's). Per frame, like the map beside it. `names` is that map: only the venue-wide attribution
/// rows' public values are kept from it. Before the boot has run (which nothing in this binary
/// does) nothing was read, and the grid says so rather than reading as empty.
fn workspace_account_grids(names: &HashMap<String, String>) -> vike_gateway_panel::AccountGrids {
    match CREDENTIAL_STORE.get() {
        Some(store) => vike_gateway_panel::AccountGrids::read(store, names),
        None => vike_gateway_panel::AccountGrids::unread(),
    }
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
    let Some(user_data) = vike_paths::state_path::user_data_dir_beside(
        vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
        SETTINGS_DIR.get().and_then(|d| d.as_deref()),
    ) else {
        return;
    };
    let report = vike_script::load_user_indicators(
        &user_data.join(vike_paths::state_path::INDICATORS_SUBDIR),
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
    use vike_paths::state_path::{PROJECT_TMP_DIR, RESEARCH_SUBDIR, RUNS_SUBDIR, STUDIES_SUBDIR};
    let vars = PROCESS_ENV.get_or_init(|| std::env::vars().collect());
    let user_data = vike_paths::state_path::user_data_dir_beside(
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
        // The BINARY, spelled literally: `vike_runs::RunManifest::produced_by` carries
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
    // has to touch `std::env` again.
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
        // The boot reads the whole map through `Booted::store` (under the `VIKE_NO_LIVE_KEYS`
        // scope) and logs nothing: [`main`] reports its findings once the subscriber exists. Every
        // LATER read goes through the same handle ([`CREDENTIAL_STORE`]) per call, so the
        // Connections tool's grid stays live after an external edit.
        credentials: vike_boot::Credentials::Read,
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
    // …and THE credential store, opened by the boot over that same walk — see [`CREDENTIAL_STORE`].
    let _ = CREDENTIAL_STORE.set(booted.store.clone());
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

    // THREE layers of this config are settings rather than defaults: the two levels and the log
    // directory are rows the boot resolved. Two environment variables still reach `vike_log::init`,
    // and neither is a settings key: `RUST_LOG` (the console-only developer switch) and
    // `$VIKE_LOG_DIR` (the logger's own bootstrap directory).
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
    // -state sidecar and [`install_user_indicators`]'s `user_data/` sibling alike — and the credential
    // store too: the boot opened `Booted::store` over this same walk, and every credential read in
    // this binary goes through that one handle ([`CREDENTIAL_STORE`]).
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
    // …and the boot's credential read, whose findings (an unreadable store, a store readable beyond
    // its owner, the `VIKE_NO_LIVE_KEYS` withheld count) it returned as DATA: the read logs nothing,
    // and this is the first line with a subscriber to say them through.
    if let Some(resolved) = &booted.credentials {
        resolved.report();
    }

    // The WORKSPACE family — `workspace.json`, `layouts/`, `last_layout.txt` and the backend
    // registry's `backends.json`, which all hang off one base directory — takes its project from
    // the boot's ONE walk (`Booted::state_dir`), not from the working directory.
    //
    // The defect this cures was LIVE rather than latent: launching with
    // `VIKE_SETTINGS_DIR=<project>/settings` from another directory read the CREDENTIALS out of
    // that project while resolving the BACKEND REGISTRY from wherever the working directory sat
    // above — the client reported "no --observe and no active backend" with a valid
    // `backends.json` sitting in the named settings directory. Nothing outranks this declaration
    // (decision 0111 retired `VIKE_STATE_ROOT`), and a declaration that MOVES the family returns
    // the notice below rather than reading a different file in silence. Placed here because this is
    // the first line at which a diagnostic has a subscriber to go to, and nothing above it reads
    // the family (`App::new`'s registry load and the `--observe` scan are both below).
    match vike_app_core::ui::workspace::persist::declare_project_state_dir(booted.state_dir.clone())
    {
        Ok(Some(moved)) => tracing::warn!("{moved}"),
        Ok(None) => {}
        Err(e) => tracing::error!("{e}"),
    }
    // …and the Studio's `studio_workspace.json`, off the SAME walk — the Studio is constructed
    // later, when its tool opens.
    if let Err(e) = vike_studio::declare_workspace_state_dir(booted.state_dir.clone()) {
        tracing::error!("{e}");
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
    // `Backend::Local` is deleted. Every sweep now runs in the backtesthub, which installs
    // the same preference in its own process (`vike_backtesthub::backtest_cli`'s
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
    // one where NOTHING was consumed — where the useful facts are the settings DIRECTORY and
    // whether a credential store sits in it (an absent one is why every venue would be on paper).
    // `Booted::boot_lines` (`vike_config::boot_lines`) renders all of it, the same rows `vike-cli
    // config show` prints, from the directory `resolve_settings` actually loaded from — `vike-boot`
    // hands the renderer its OWN walk's answer, so nothing here can walk a second time. It NEVER
    // opens the credential store, so no key name or value can reach this log.
    for line in &booted.boot_lines {
        // ⚠ Not all INFO: a `credential store: ⚠` finding is a WARN line
        // (`vike_config::boot_line_level`).
        match vike_config::boot_line_level(line) {
            vike_config::BootLineLevel::Warn => tracing::warn!("{line}"),
            vike_config::BootLineLevel::Info => tracing::info!("{line}"),
        }
    }

    // …and the DURABLE anchor for the ceilings those lines just disclosed — one JSONL line per
    // start in `<state>/changes/`, actor origin `boot`.
    //
    // The disclosure above is the console/file copy and it does not survive:
    // `vike_log::DEFAULT_MAX_LOG_FILES` prunes the rolling file daily, and a `warn`
    // `preferences.log_file_level` row (what the server-side boxes run) silences the `info` layer
    // the boot lines ride. "What was the ceiling on the 14th" is not answerable from a log that is
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
    let options = gpu_startup::base_options(
        egui::ViewportBuilder::default()
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
    );
    // The process environment the GUI consults, read ONCE — see [`GuiEnv`]. `App::new` keeps it
    // for the frame loop and the Studio window.
    let gui_env = gui_env();
    // Where the log is, for the failure message of a GPU startup that ran out of rungs: the
    // explicit `config.log_dir` row, else the project's log home (`$VIKE_LOG_DIR`, when set, beats
    // both inside the logger — the message says so).
    let log_dir = settings().config.log_dir.clone().or_else(|| booted.log_home.clone());
    // The window opens through `gpu_startup`'s ladder (configured settings, the library's
    // defaults, one backend at a time, a software renderer) instead of one bare `run_native`.
    gpu_startup::run(
        &window_title,
        &options,
        startup_vsync(),
        log_dir.as_deref(),
        _log_guards,
        // The ONE place a real `eframe::CreationContext` exists in this binary — which is exactly
        // why `App::new` no longer takes one (see its doc): this closure is the only code that
        // could ever hold one, so it does the unwrapping and hands `App::new` the two public
        // members it reads. It is built once per ATTEMPT, because a failed attempt never runs it.
        || -> eframe::AppCreator<'static> {
            let (backend, env) = (observe_backend.clone(), gui_env.clone());
            Box::new(move |cc| {
                Ok(Box::new(App::new(&cc.egui_ctx, cc.wgpu_render_state.as_ref(), backend, env)))
            })
        },
    )
}

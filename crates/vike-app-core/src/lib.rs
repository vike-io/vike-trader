//! `vike-app-core` — the eframe-free / wgpu-free logic extracted from `vike-app` so it runs in CI.
//!
//! The wgpu GUI shell — `vike-desktop`, renamed from `vike-app` on 2026-09-09 (#1727) — sits outside
//! the derived CI roster: ci.yml's `app-check` job checks it, clippy-gates it and runs its unit
//! tests, but only on a PR whose plan reaches it (build weight). The modules here touch NO
//! eframe/wgpu (egui at most — the [`ui::workspace`]/[`ui::tool_views`] draw modules), so they
//! build AND test with the roster exactly like `vike-chart` does. The shell re-imports them and
//! wires them into its frame loop. (This said CI "never TESTS or clippy-gates" `vike-app` until
//! 2026-09-28.)
//!
//! - ⚠ `alerting` — the re-export is **DELETED** (2026-09-25). It was a pass-through to
//!   `vike_ops::alerting`, which was itself a pass-through to the `vike-alerting` crate, and
//!   nothing outside those two files ever named `vike_app_core::alerting`. The engine is unchanged
//!   (persisted rule model, PURE evaluator, Telegram/webhook delivery seam) and lives where it
//!   always did; the one consumer that wanted it names `vike-alerting` directly.
//! - [`backend::backend_conn`] — RUNTIME-MUTABLE backend selection (split-plane B1): `BackendConn` (one
//!   active backend: record + observe bridge + optional control channel), `connect_backend`
//!   (key resolution strictly through [`backend::backend_registry::resolve_keys`], control mounted only
//!   when BOTH the record arms it AND the process-level master gate is on), `cli_observe_record`
//!   (the `--observe ADDR` compat record), and `switch_backend` — the safety-critical
//!   BACKEND-SESSION clear (stop the old bridge, reset the session-owned render state, publish an
//!   empty snapshot) that keeps backend A's data from painting under backend B's connection,
//!   while the local feed plane — venue truth the third mode (B2) ran beside the session until the
//!   `fat` build went — survives the switch; `teardown_feed_plane` is its own mode-exit teardown,
//!   and the two compose to B1's original total clear. Plus the pure picker decisions the
//!   Connections tool renders from.
//! - [`backend::backend_editor`] — the Backends section's ADD/EDIT/DELETE form state machine (split-plane
//!   I2): form validation (name/addr/duplicate refusals collected, not first-wins), the
//!   `key_presence` typo-catcher (presence only — a key VALUE cannot leave the map through it),
//!   the are-you-sure delete affordance, and the drained `RegistryUpdate` whose
//!   `apply_registry_update` pins delete-active's disconnect-BEFORE-save order. Edit-active
//!   deliberately defers to the next connect (`edit_defers` is the UI's cue).
//! - [`backend::backend_registry`] — the GUI's registry of remote tradehub BACKENDS (split-plane
//!   B7/B8/B9): `BackendRecord` (display name, dial address, credential-store KEY NAMES — never
//!   key material — and the per-backend `control` arming gate), `backends.json` persistence
//!   beside the workspace file, and the pure `resolve_keys` connect-time lookup over a
//!   caller-supplied credentials map.
//! - [`data::backfill_plan`] — the pure Data-Manager bulk-Backfill job planner: maps a grid selection +
//!   known gap ranges to a list of concrete `(venue, symbol, interval, ranges)` kline-backfill
//!   jobs, filtering to `kind == "bar"` and nothing else. ⚠ It "filtered to `vike-backfill`'s
//!   supported venues" until 0059 Phase 3, which deleted this build's copy of that roster: which
//!   venues have collectors is the SERVER's question, and this crate takes no `vike-backfill` edge.
//! - [`ui::core_sync`] — the per-frame `CoreSnapshot` → render-model fold (`sync_from_core`): the
//!   published snapshot's bars plus the live trade tape folded into the per-chart `ChartState`s and
//!   the tick/volume + orderflow aggregators. Moved down verbatim out of `vike-app`'s `main.rs`
//!   because it is the single densest concentration of silent-data-loss history in the GUI — FOUR
//!   venue-routing/gating/lifetime bugs (venue-discarding bar keys, orderflow grouped by splitting
//!   the chart key at `'@'`, a tick/volume workspace stalled behind the `snap.seq` gate, and an
//!   aggTrades-backfill batch DISCARDED when its orderflow aggregator had been torn down
//!   mid-backfill — permanently, since the run-once spawn gate never refetches), each fixed but
//!   recorded only as a code comment, because nothing compiled the file they lived in. Its
//!   `bug_pin_tests` now pin today's behaviour at the first three sites; the fourth's drain and
//!   staging were deleted with the backfill lane that fed them (its module doc's tombstone).
//! - [`data::data_sink`] — the live-data seam: `GuiFeedSink`, the core-free `LiveDataSink` the
//!   market-data session's feeds write into, plus the `BookStore`/`TradeStore`/`DirectBarStore`
//!   live caches it lands in.
//! - [`ui::desktop_entry`] — `vike-desktop --install-desktop-entry` / `--uninstall-desktop-entry`:
//!   the Linux launcher entry and icons, written from the files compiled into the binary.
//! - [`orders::dom_math`] — DOM pure math: the signed-position resolver + the no-book classifier that
//!   decides what the ladder SAYS when it has nothing to draw. ⚠ The synthetic-book seed helpers
//!   are DELETED; that module's own doc carries what they were and why nothing replaced them.
//! - [`orders::equity_panel`] — `EquityPanelModel`: shapes a `vike_exec::CoreSnapshot` into the cross-venue
//!   equity panel view-model (PR-2). Pure mapping, no egui — CI-tested here, rendered by the shell.
//! - [`ui::feed_lifecycle`] — the WHOLE start/stop story of a live market-data subscription. Two
//!   halves: the pure diffs that decide whether something should happen (`orphaned_feed_keys`/
//!   `live_window_keys`/`orphaned_trade_feed_keys`), and the imperative half that performs it —
//!   `ensure_feed_on`, `ensure_trade_feed_on`, `ensure_depth`, `ensure_poly_book` and `reap_orphaned_feeds`, moved
//!   down out of `vike-app`'s `main.rs` where they were `App` methods no gate compiled. The
//!   asymmetry is the point: the diffs were tested while the code that STARTS AND STOPS SOCKETS
//!   from them was not, and the reaper's teardown ordering (which unsubscribe routes to which
//!   venue, which slot is freed so a later `ensure_*` genuinely restarts the feed) is the sibling
//!   of the silent-data-loss class recorded in [`ui::core_sync`]'s module doc. `App`'s fields arrive
//!   as the `FeedSlots`/`SeriesSlots`/`SeriesSpec` bundles, the same shape `core_sync` uses. It
//!   also owns the retry lane that keeps a one-off failure from burning a slot for the life of the
//!   process: `FeedRetries` (the four `ensure_*` subscribes), on one ladder (`retry_backoff`) and
//!   one logging discipline.
//! - [`ui::initial_arrange`] — the FIRST-FRAME layout decision out of the frame loop's
//!   `did_initial_arrange` block: the `VIKE_TOOL`/`VIKE_TOOLS` QA capture arms (the two
//!   window-opening sites [`ui::window_spawn`] deliberately left behind — they spend `tool-{n}` ids at
//!   the desktop origin, cascade-free), the `VIKE_ARRANGE` mode parse, and the open-count
//!   arrange-vs-maximize-lone choice. `plan_initial_arrange` is PURE and the seven env reads stay in
//!   the shell, `vike-desktop` (the `ArrangeEnv` Injected shape, as [`ui::startup`]); its roster
//!   test runs the REAL [`ui::feed_lifecycle`] `live_window_keys` over every QA spawn — the half
//!   of the window-opening pair [`ui::window_spawn`]'s own invariant test could not reach.
//! - The inventory tree-model (venue → symbol → series with rollups, for the Data Manager's
//!   "Stored" view) lives in `vike_data_manager::model`, reused by the Studio too, and is named
//!   there. The `inventory` alias this crate kept for it was deleted 2026-09-27 (the owner's
//!   no-alias ruling).
//! - [`backend::observe_bridge`] — the `--observe` thin-client's `WireSnapshot` → `CoreSnapshot` REVERSE
//!   bridge (`wire_to_core` + the per-shape `map_*` helpers), moved verbatim out of `vike-app`'s
//!   CI-excluded `main.rs` so the mapping — above all `map_order_status`'s hand-maintained
//!   Debug-spelling table — is pinned by an exhaustive CI test instead of silently rendering a
//!   new `OrderStatus` variant as terminal `Rejected`. The forward projection lives in
//!   `vike-tradehub/src/publish.rs`. It now also owns the observer's two pieces of IMPERATIVE
//!   wiring, moved down out of `App::new`: `spawn_bridge` (the self-healing reconnect loop — the
//!   only consumer of `RemoteCoreHandle::{connect,is_connected,snapshot}`) and `connect_control`
//!   (the `Scope::Write` gate that can arm REAL order placement from an observer; its
//!   refuse-paths are pinned).
//! - [`orders::options_books`] — `build_books`: folds the user's own deribit working orders + positions out
//!   of a `CoreSnapshot` into the per-instrument `InstrumentBook` map the options chain paints from.
//! - [`orders::options_greeks`] — `position_greeks`: per-position + net portfolio Δ/Γ/ν/Θ for held Deribit
//!   option positions, priced off the live option chains (spot + IV) the Options tool fetched. Pure,
//!   egui-free — the data source for the Greeks tool window.
//! - [`orders::order_dispatch`] — the ONE place a drained UI order intent becomes a `vike_exec::Command`,
//!   and therefore the ONE place [`orders::order_entry`]'s local preview cap is applied. `plan_dispatch` is
//!   PURE (intents + `OrderLimits` + `CoreSnapshot` in, commands + rejects out); the shell's frame
//!   loop keeps only the I/O. Moved down out of the CI-excluded `main.rs` because the inline block
//!   there had a real hole: of the five submit paths only three validated, and the **Trade window
//!   — the manual order-entry panel — had no local notional cap at all**, nor did its TP+SL bracket
//!   sub-path or the DOM Close/Reverse exit. Validation is now structural (one private chokepoint)
//!   and CI-pinned by an exhaustive `SubmitSource` roster plus an output invariant asserted on
//!   every emitted order write.
//! - [`orders::order_entry`] — the single order-write seam: `OrderTicket` + `build_order_request` (the ONE
//!   `OrderRequest` constructor), `next_client_order_id`, and a LOCAL preview/safety layer
//!   (`validate`/`OrderLimits`/`OrderReject`) that drops self-evidently-malformed / over-cap orders
//!   before they reach the command lane. Not the venue RiskGate — a preview in front of it.
//! - Footprint / CVD orderflow aggregation over the live trade tape lives in `vike_orderflow::bar_agg`
//!   (moved there 2026-09-27; it names only market-data types).
//! - [`ui::poly_labels`] — pure Polymarket display-label helpers for the scalp cockpit:
//!   `poly_short_label` (token-id elision) + `poly_market_short_name` (Gamma question/slug → the
//!   ≤18-char rail/ladder header name). Moved down from `vike-app`'s `main.rs` (audit F4) so their
//!   unit tests run in CI; the feature-gated `GammaMarket`-typed selector (`pick_updown_token`)
//!   stayed in the shell — it named a vike-polymarket type this crate deliberately never links —
//!   and went with the `fat` build (its tombstone is in `crates/vike-desktop/src/main.rs`).
//! - `preflight` — MOVED to `vike_mount::preflight` (the mount composition root) by the PR that
//!   WIRED it: `vike_mount::build_node` runs it for the headless daemon (and ran it for the GUI too,
//!   while the desktop built a node), and
//!   `vike-mount` (which vike-run merged into) may not depend on this (egui-shaped) crate to reach it.
//!   Nothing in this crate referenced it, so the move left no compat shim.
//! - ⚠ `reconcile_config` — the re-export is **DELETED** (2026-09-23), not re-pointed. It was a
//!   pass-through to `vike_ops::reconcile_config`, and that module is
//!   `crates/vike-tradehub/src/reconcile_config.rs` now, living with its only code caller. Nothing
//!   on THIS side ever used it — `crates/vike-desktop/src/main.rs` names it zero times — which is
//!   what made the "both live roots" ground for sharing it stale rather than merely unused. The
//!   seam lives on there: `build_recon_config` over the daemon's `ReconSettings` rows (decision
//!   0111 retired the environment map it once read) and `health_from_feed_status`.
//! - ⚠ `scan`, `settings` and `shutdown` — the re-exports are **DELETED** (2026-09-28; the note
//!   after this doc says why), and callers name `vike_model::scan` (it moved to `vike-model` on 2026-10-08, behind
//!   that crate's `test-support` feature, so it is not linked here), [`vike_ops::settings`] and
//!   [`vike_ops::shutdown`]. `scan` is the pure source scanner behind the settings-registry gate;
//!   `settings` is `SETTINGS`, the machine-gated registry of every environment variable this
//!   workspace reads — a WORKSPACE-wide operator table, not GUI state, whose gate lives at
//!   `crates/vike-ops/tests/settings_secrets/settings_registry.rs`; `shutdown` is the pure, bounded teardown
//!   orchestration (`run_with_deadline`), whose two callers are the desktop's window close and
//!   `vike-tradehub`'s daemon stop, the second headless. (These three bullets described live
//!   re-exports and named `vike-app` as the first caller until 2026-09-28.)
//! - [`backend::split_plane`] — the pure ARM decisions behind `App::new` (split-plane B2, the third
//!   mode; only the observe arm is reachable since the `fat` build went, which that module's doc
//!   states):
//!   `observes` (whether a launch observes — always, since the `fat` build went; `app_mode`, the
//!   three-arm table whose fat+observe row was the third mode, went with the local-core arm in
//!   2026-10),
//!   `feed_venues`/`LOCAL_FEED_VENUES` (the ONE written-down copy of the mounted feed set), and
//!   `series_render_source`/`folds_from_snapshot` — the DOUBLE-FOLD GUARD deciding the single
//!   source every series renders from (kline series: the published snapshot's bars, or — third
//!   mode, on a `DIRECT_BAR_VENUES` venue — the venue-fed [`data::data_sink`] `DirectBarStore`;
//!   tick/volume/orderflow series: the GUI-side trade tape), consulted by [`ui::core_sync`]'s
//!   snapshot and direct-bar folds and by [`backend::backend_conn`]'s backend-switch clear.
//! - [`ui::startup`] — the STARTUP LAYOUT decision lifted out of `App::new`'s tail: which windows a
//!   fresh app opens (a saved workspace, one of five `VIKE_SHOT_WIN` QA tool windows, or the
//!   default BTCUSDT chart), which market-data feeds must be ensured FOR them, and the
//!   `VIKE_STYLE`/`VIKE_SCALE` capture overrides. Six branches over five environment knobs that no
//!   gate compiled, each deciding both what exists and what subscribes — the same
//!   "an unchecked decision opens the wrong socket, or none" class [`ui::feed_lifecycle`] closes.
//!   `plan` is PURE (data in, a `StartupLayout` of decisions out) and the environment is read by
//!   the BINARY and injected as `StartupEnv`, so no settings-registry row moves and the tests need
//!   no `env::set_var`.
//! - [`data::stored_load`] — the Stored grid's inventory WALK over the `HistStore` TRAIT (`inventory` →
//!   `build_tree`, one `series_gaps` probe per series) plus its `load_partials` sibling (the
//!   cross-kind coverage fold, spec §6-Q2): ONE pair of functions both of `refresh_stored`'s
//!   arms call, so a local and a remote grid over the same data cannot differ in tree shape or in
//!   the Partial column.
//! - [`data::stored_mode`] — WHICH store the Data-Manager "Stored" grid reads (local vs the datahub at
//!   `config.datahub_addr` — the #1378 seam close) and the per-mode capability table: what Delete,
//!   still a LOCAL-store operation, shows as its reason in remote mode, and which of the Partial
//!   column's three states (local fold / wire answer / honest note) a mode plus a negotiated
//!   `RemoteCoverage` selects.
//! - [`ui::symbol_row`] — the ONE cross-venue instrument search-result row both symbol pickers paint
//!   (`search_result_row`): the chart window's title-bar dropdown, still in the shell
//!   (`crates/vike-desktop/src/chart_window.rs`), and
//!   [`ui::tool_views`]'s ƒx picker, which moved down here. Shared rather than copied.
//! - [`ui::sync_group`] — chart sync-group registry math: `sync_feed`/`sync_harvest`/`GroupFrame`, the
//!   cross-window crosshair/visible-range broadcast that links grouped chart windows.
//! - Tick- and volume-bar aggregation (`TickVolAgg`, `BarKind`) lives in `vike_orderflow::tickvol`.
//! - [`ui::tool_views`] — the egui-only tool BODIES moved out of `vike-app`'s CI-excluded `main.rs`:
//!   Tearsheet, Greeks, News and Calendar (extraction batch 1) plus Data, Stored, Connections and
//!   the chart window's ƒx picker popup (batch 2). Read-only inputs arrive grouped in `ToolCtx`
//!   (audit F8); per-window view state + outgoing intents stay in the separate `&mut ToolView`
//!   param, so the data-in/actions-out seam is unchanged. egui-but-not-eframe like [`ui::workspace`],
//!   so CI builds it and the pure helpers (`tearsheet_rows`, `cal_day_label`, `feed_cell`,
//!   `tree_totals`, `indicator_matches`, …) are unit-tested here.
//! - [`tools`] — the per-tool view/data model + the REST tool-fetchers.
//! - [`backend::venue_routing`] — the pure venue/instrument resolvers moved down out of `vike-app`'s
//!   `main.rs`: `venue_of_key` (the inverse of [`ui::workspace::series_key`]) and
//!   `venue_bar_instrument` (the per-venue crypto-derivative kline allowlist). Per-venue mapping
//!   tables that rot silently when a venue is added — now compiled, clippied and unit-tested.
//! - [`backend::tradehub_control`] — the thin-client Scope::Write WRITE-path lowering: `wire_from_command`
//!   (a `vike_exec::Command` → `vike_tradehub_client::wire::WireCommand` converter, the GUI-side inverse
//!   of the daemon's `lower_command`) + the routing gate and the one send path; the master gate is
//!   the PC's `flags.tradehub_control` row, which the binary passes in. Lets the desktop observer (every launch; `vike-app --observe` when this was written)
//!   drive a remote headless daemon's core; pure, egui-free, CI-tested here.
//! - [`ui::window_spawn`] — the NEW-WINDOW decision out of the shell's frame loop: which cascade slot
//!   a freshly opened window lands in, how its [`ui::workspace::WinState`] is built and seeded, and
//!   which live subscriptions must exist before it can paint. SIX hand copies of that arithmetic
//!   sat in a file no gate then compiled, two of them already drifted apart — and one of them must
//!   agree with [`ui::feed_lifecycle`]'s per-frame reaper or the new window's feed is stopped on the
//!   very next frame, with a DOM's one-shot ensure never re-requesting it.
//! - [`ui::workspace`] — the multi-window desktop shell: window state/layout (`WinState`), arrange
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
//! compiling the ENTIRE egui widget stack purely to reach them through this crate. Callers name
//! [`vike_ops`] directly.

#![warn(unreachable_pub)]

// ⚠ This crate re-exported `vike_ops::{scan, settings, shutdown}` under their historical
// `vike_app_core::…` paths until 2026-09-28, the last of a list that also carried
// `reconcile_config` (left 2026-09-23 for `vike-tradehub`), `alerting` (left 2026-09-25; the
// crate is `vike-alerting`) and `journal_mat` (left 2026-09-25; it is `vike_journal::materialize`).
// MEASURED on the day it went: the ONE code caller was `crates/vike-desktop/src/app_lifecycle.rs`'s
// shutdown call, which names `vike_ops::shutdown` now — a second name for a module that already
// has one is exactly what the 2026-09-18 ruling refuses.
// The GUI's egui-free logic, grouped by concern (2026-09-27): backend connections, data and
// history, order entry, and the window shell. `tools` stays at the root beside them.
pub mod backend;
pub mod data;
pub mod orders;
pub mod ui;

pub mod tools;

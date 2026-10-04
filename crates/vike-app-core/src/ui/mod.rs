//! The window shell: feed lifecycle, core sync, startup, window spawning and arrangement, capture
//! seeding, series following, status widgets, and the `workspace` and `tool_views` trees.

// The five appearance preferences: read at start by the shell, applied and saved by the Settings
// window (design system spec §5).
pub mod appearance_settings;
// The app's top bar, out of `vike-desktop`'s `draw_chrome` (design system step 7): its colours and
// sizes are tested here, where the CI roster runs them.
pub mod caption;
// The two SYNTHETIC-STATE capture hooks (a seeded Trade panel, a seeded chart drawing) — the
// content half of the capture vocabulary `startup`/`initial_arrange` own the layout half of. Pure
// and tested HERE rather than in the CI-excluded shell, for the reason every other decision in
// this crate moved down: `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI` names the shell
// (`vike-desktop`; `vike-app` when these moved).
pub mod capture_seed;
pub mod core_sync;
pub mod desktop_entry;
pub mod feed_lifecycle;
// The FIRST-FRAME layout decision (the frame loop's `did_initial_arrange` block): the
// VIKE_TOOL/VIKE_TOOLS QA capture arms `window_spawn`'s planner deliberately excludes (they spend
// `tool-{n}` ids at the desktop origin, no cascade), the VIKE_ARRANGE parse, and the open-count
// arrange-vs-maximize-lone choice. The env reads stay in `vike-desktop`'s `main.rs` and arrive as
// `ArrangeEnv`; every QA spawn's agreement with `feed_lifecycle`'s reaper is CI-tested here.
pub mod initial_arrange;
pub mod poly_labels;
// THE CHART FOLLOWS THE SERIES THE BACKEND PUBLISHES. The GUI names series from its workspace and
// the node names them from what it mounts, and until this module the two only met when a human
// guessed the venue: a daemon mounting bybit published `bybit:BTCUSDT@1m` while the startup chart
// was subscribed to `BTCUSDT@1m`, so `core_sync` dropped every frame and the empty grid was badged
// `● LIVE`. Adoption, reach (the picker's backend section) and the badge's honesty all read ONE
// list — the snapshot's own series — so no two of them can disagree about what the node has.
pub mod series_follow;
// The shared cross-venue instrument search-result row, painted by BOTH symbol pickers: the chart
// window's title-bar dropdown (still in the shell, `crates/vike-desktop/src/chart_window.rs`) and
// `tool_views::fx_picker_popup`'s per-study source menu. It moved down with the ƒx picker
// (tool-view extraction batch 2).
// The startup-layout decision out of `App::new`'s tail: which windows a fresh app opens, which
// feeds they need, and the two QA capture overrides. Six branches over five env knobs that no gate
// compiled — the env READS stay in `vike-desktop`'s `main.rs` and arrive as `StartupEnv`.
pub mod startup;
// The app's bottom strip, out of `vike-desktop` (design system step 7): tested here.
pub mod status_bar;
// The bottom status strip's two DOT COLOURS, out of `vike-app`'s CI-excluded `status_bar`. The feed
// dot hand-rolled a classifier that disagreed with `vike_model::feed_status::parse_feed_status` —
// the one the Connections tool and the headless health gate both read — and the control dot
// recovered a `bool` by searching the string `tradehub_control` had just built from it.
pub mod status_dot;
pub mod symbol_row;
pub mod sync_group;
pub mod tool_views;
// The NEW-WINDOW decision out of `vike-app`'s frame loop: the cascade slot, the `WinState`
// construction, and the subscriptions a spawned window needs before it can paint. Six hand copies
// in a file no gate then compiled, two of which had already drifted — and one of which must agree with
// `feed_lifecycle`'s reaper or the new window's feed dies on the frame after it opens.
pub mod window_spawn;
pub mod workspace;

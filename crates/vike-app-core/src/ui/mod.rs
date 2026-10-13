//! The window shell: feed lifecycle, core sync, startup, window spawning and arrangement, capture
//! seeding, series following, status widgets, and the `workspace` and `tool_views` trees.

// The five appearance preferences (design system spec §5).
pub mod appearance_settings;
// The app's top bar, out of `vike-desktop`'s `draw_chrome` (design system step 7).
pub mod caption;
// The two SYNTHETIC-STATE capture hooks: the content half of the capture vocabulary.
pub mod capture_seed;
pub mod core_sync;
pub mod desktop_entry;
pub mod feed_lifecycle;
// The FIRST-FRAME layout decision; the env reads stay in the shell and arrive as `ArrangeEnv`.
pub mod initial_arrange;
pub mod poly_labels;
// THE CHART FOLLOWS THE SERIES THE BACKEND PUBLISHES: adoption, reach and badge read one list.
pub mod series_follow;
// The startup-layout decision out of `App::new`'s tail; the env reads arrive as `StartupEnv`.
pub mod startup;
// The app's bottom strip, out of `vike-desktop` (design system step 7).
pub mod status_bar;
// The bottom status strip's two DOT COLOURS, as pure functions.
pub mod status_dot;
pub mod symbol_picker;
// The shared cross-venue instrument search-result row BOTH symbol pickers paint.
pub mod symbol_row;
pub mod sync_group;
pub mod tool_views;
// The NEW-WINDOW decision: the cascade slot, the `WinState`, the subscriptions it needs.
pub mod window_spawn;
pub mod workspace;

//! The window shell: feed lifecycle, core sync, startup, window spawning and arrangement, capture
//! seeding, series following, status widgets, and the `window` and `windows` trees.

// The five appearance preferences (design system spec §5).
pub mod appearance_settings;
// The app's top bar, out of `vike-desktop`'s `draw_chrome` (design system step 7).
pub mod caption;
// The two SYNTHETIC-STATE capture hooks: the content half of the capture vocabulary.
pub mod capture_seed;
// Settings > AI's Connect Claude: Claude Desktop's config entry that starts this app with `--mcp`.
pub mod claude_connect;
pub mod core_sync;
pub mod desktop_entry;
pub mod feed_lifecycle;
// The desktop's own frame-rate cap, for the one case the GUI library has none (vsync off).
pub mod frame_cap;
// The GPU startup LADDER's decisions and the render-target contract; the thin wgpu half is the shell's.
pub mod gpu_startup;
// The Help menu's About, Keyboard shortcuts and Console windows.
pub mod help;
// A press whose release another window took: ended at a focus loss or a bare return.
pub mod lost_release;
// The File / Window / Help menu bar the caption draws (command emission only).
pub mod menu_bar;
// THE CHART FOLLOWS THE SERIES THE BACKEND PUBLISHES: adoption, reach and badge read one list.
pub mod series_follow;
// The startup-layout decision out of `App::new`'s tail; the env reads arrive as `StartupEnvironment`.
pub mod startup;
// The app's bottom strip, out of `vike-desktop` (design system step 7).
pub mod status_bar;
pub mod symbol_picker;
pub mod window;
pub mod windows;

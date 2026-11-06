//! The tool-window and kittest suites as ONE test binary (`cargo test -p vike-app-core --test screens`).
//!
//! Each suite is still its own file, `tests/screens/<suite>.rs`, and here a MODULE named after it, so a
//! test's id is `<suite>::<test>` and `cargo test -p vike-app-core --test screens -- settings_window::`
//! runs one suite. Every test and every assertion is the one the suite's own binary ran.
//!
//! Why one binary: the suites share `tests/common/`'s `tool_ctx` (the `ToolCtx` owner), `a11y` (the
//! accessibility-tree readers), `trade_frames` (the Trade window rig) and `poll`, and as nine binaries
//! each compiled its own copy of the ones it read; declared ONCE below, each compiles once and a suite
//! reaches it as `crate::<helper>` (as `trade_frames` always reached `tool_ctx` and `poll`). The
//! grouping rule is `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": every member is
//! plain (no feature gate, no proptest sidecar, no process-global mutation).

#[path = "common/a11y.rs"]
mod a11y;
#[path = "common/poll.rs"]
mod poll;
#[path = "common/tool_ctx.rs"]
mod tool_ctx;
#[path = "common/trade_frames.rs"]
mod trade_frames;

#[path = "screens/account_window_screens.rs"]
mod account_window_screens;
#[path = "screens/backend_settings_section_rendering.rs"]
mod backend_settings_section_rendering;
#[path = "screens/catalog_refresh_screen.rs"]
mod catalog_refresh_screen;
#[path = "screens/data_manager_backend_strip.rs"]
mod data_manager_backend_strip;
#[path = "screens/data_manager_screens.rs"]
mod data_manager_screens;
#[path = "screens/settings_window.rs"]
mod settings_window;
#[path = "screens/store_bar_screen.rs"]
mod store_bar_screen;
#[path = "screens/trade_cause_line.rs"]
mod trade_cause_line;
#[path = "screens/trade_window_screens.rs"]
mod trade_window_screens;

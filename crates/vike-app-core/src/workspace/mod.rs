//! The desktop workspace: multi-window management, arrangement, and layout
//! persistence — the egui analog of vike's MDI shell (`chartwin.py`,
//! `MinimizedRail`), split by concern:
//!
//!   - [`state`]   — `WinState`/`WinKind` + the per-window chrome loop (`show_window`)
//!   - [`arrange`] — cascade / tile / grid geometry, maximize/restore
//!   - [`persist`] — capture/restore of the chart layout as JSON (`workspace.json`)
//!   - [`menu`]    — the File / Window / Help menu bar (command emission only)
//!   - [`rail`]    — the left rail of minimized windows (vertical tabs)
//!
//! Everything here is egui-only (no eframe/wgpu) and free of app content state —
//! tool-window view state is `crate::tools::ToolView`, held by the App keyed by
//! window id — which keeps this module a candidate for extraction into a
//! CI-tested crate (the vike-chart precedent) once workspace features grow.

pub mod arrange;
pub mod menu;
pub mod persist;
pub mod rail;
pub mod state;

pub use arrange::{Arrange, apply_arrange, maximize, unmaximize};
pub use menu::{MenuResult, menu_bar};
pub use rail::left_rail;
pub use state::{DEFAULT_VENUE, WinKind, WinState, series_key, show_window};

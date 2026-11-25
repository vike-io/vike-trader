//! The desktop's on-screen windows: multi-window management, arrangement, and layout
//! persistence — the egui analog of vike's MDI shell (`chartwin.py`,
//! `MinimizedRail`), split by concern:
//!
//!   - [`state`]   — `WindowState`/`WindowKind` + the per-window chrome loop (`show_window`)
//!   - [`title_bar`] — a tool window's title bar, and the tab slot a tabbed tool seats its
//!     segmented control in (moved down out of the CI-excluded shell)
//!   - [`arrange`] — cascade / tile / grid geometry, maximize/restore, which windows a maximized one hides
//!   - [`workspace`] — capture/restore of the chart layout as JSON (`workspace.json`), and its
//!     "Save layout as…" naming dialog ([`mod@workspace::save_layout`], on the component kit)
//!   - [`rail`]    — the left rail of minimized windows (vertical tabs)
//!   - [`spawn`]   — the NEW-WINDOW decision: the cascade slot, the `WindowState`, the subscriptions it needs
//!   - [`initial_arrange`] — the FIRST-FRAME layout decision; the env reads stay in the shell and
//!     arrive as `WindowInitialArrangeEnvironment`
//!   - [`sync_group`] — the chart sync groups' per-frame feed and harvest
//!
//! The File / Window / Help menu bar the caption draws is `crate::ui::menu_bar`, not a window's.
//!
//! Everything here is egui-only (no eframe/wgpu) and free of app content state —
//! tool-window view state is `crate::ui::windows::WindowView`, held by the App keyed by
//! window id — which keeps this module a candidate for extraction into a
//! crate of its own (the vike-chart precedent) once workspace features grow.

pub mod arrange;
pub mod initial_arrange;
pub mod rail;
pub mod spawn;
pub mod state;
pub mod sync_group;
pub mod title_bar;
pub mod workspace;

pub use arrange::{WindowArrange, apply_arrange, maximize, unmaximize};
pub use rail::left_rail;
pub use state::{
    ChartCompareSymbol, TOOL_BODY_MARGIN, WindowBodyBounds, WindowKind, WindowState, focused_chart,
    show_window, symbol_label,
};
pub use title_bar::{WindowTitleBarTabSlot, tool_title_bar};

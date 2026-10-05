//! The geometry every window's title bar shares: the owner's v3 design draws ONE bar for every
//! window — 25 pt tall at every density, the window's name small at the left, its controls 20 pt
//! squares at the right, a hairline parting them from whatever a tabbed window seats between.
//!
//! These are DESIGN sizes and not density sizes: a window's chrome is not content, so the density
//! (`crate::metrics`) scales the controls inside a window and never the bar around it. Before this
//! module the tool windows' bar was 30 pt, the chart window's own copy of it another 30, and the Trade
//! window's the v3 design's 25 — a trader laying the windows side by side saw three heights.
//!
//! The consumers are the shell's tool-window bar
//! (`crates/vike-app-core/src/ui/workspace/title_bar.rs`), the chart window's bar
//! (`crates/vike-desktop/src/chart_window.rs`), and the Trade window's layout
//! (`crates/vike-panels/src/trade/layout.rs`'s `chrome`, which counts the bar in the size it opens at).

// The base measures: GENERATED from `crates/vike-ui-theme/ui-theme.toml` (`[[chrome]]`), each with its
// doc comment.
include!("chrome_tokens.rs");

/// What the three controls take, with their gaps and the pad after them.
pub const CONTROLS_W: f32 = 3.0 * WINDOW_CONTROL + 2.0 * WINDOW_CONTROL_GAP + WINDOW_CONTROLS_PAD;

// The v3 design's numbers, measured off the export (`.w-title` 25, its controls 20 × 20 four apart and
// four from the right edge), and what must hold between them: a control fits the bar with room to
// spare — the bar does not scale with the density, so nothing density-sized may be seated in it
// (`IconButton::sized` is how the Trade window's view toggles stay 24 × 20 at Comfortable) — and the
// hairline's two sides are equal.
const _: () = assert!(TITLE_BAR_H == 25.0);
const _: () = assert!(CONTROLS_W == 72.0);
const _: () = assert!(WINDOW_CONTROL + 2.0 <= TITLE_BAR_H);
const _: () = assert!(SEPARATOR_H < TITLE_BAR_H);
const _: () = assert!(SLOT_TO_CONTROLS == 2.0 * SLOT_TO_SEPARATOR + 1.0);

//! Module-level constants split out of `chart.rs` (chart refactor PR-1):
//! the pane-layout geometry budgets + the orderflow-overlay palette. The
//! values and their doc-comments are verbatim; only the location changed
//! (and `pub(crate)` so `chart::draw` and `panes_layout` still read them).

use egui::Color32;

/// y-axis tick budget passed to `scale::nice_ticks` for the price plot's
/// custom `y_grid_spacer` in the MAPPED modes (Log/Percent; Linear delegates
/// to egui_plot's own default spacer — chart-UX bundle T2). A display tuning
/// constant (not derived from a spec number), chosen to land in the same
/// density class as that default.
pub(crate) const Y_TICK_BUDGET: usize = 10;

/// Right price-axis gutter width — uniform across every pane (vike parity;
/// see the `y_axis_min_width` call sites below) and reused as the hand-rolled
/// y-drag-zoom interact rect's width (chart-UX bundle T4): native
/// `allow_axis_zoom_drag` is disabled, so this rect over the SAME pixels is
/// the only thing left that responds to a gutter drag.
pub(crate) const Y_AXIS_GUTTER_W: f32 = 72.0;

/// Bottom time-axis label strip height — the extra height budgeted onto the
/// bottom-most sub-pane so its "frame" (plot area, excluding axis labels)
/// stays the same size as its siblings (see `axis_h` below), and reused as
/// the hand-rolled x-drag-zoom interact rect's height (chart-UX bundle T4).
pub(crate) const AXIS_LABEL_H: f32 = 18.0;

/// Floor (px) for every pane's PLOT height passed to [`PaneFractions::layout`]/
/// [`PaneFractions::drag`] (chart-UX bundle T9) — the old sub-pane clamp floor.
/// The old formula additionally floored price at 72px specifically; a single
/// `min_px` can't express two different floors (the layout/drag interface
/// takes one), so price now shares the 44px floor with every other pane —
/// a deliberate simplification: price's DEFAULT share (see
/// `panes::default_share`) keeps it comfortably above 72px in any
/// reasonably-sized window, and a user who deliberately drags price down
/// that far is exercising the same "shrink a pane to make room" affordance
/// sub-panes already had.
pub(crate) const MIN_PANE_PX: f32 = 44.0;

/// Draggable separator strip height (chart-UX bundle T9) between two
/// vertically-stacked panes — `Sense::drag` + a resize cursor on hover.
pub(crate) const PANE_SEP_H: f32 = 5.0;

/// Subtle 1px horizontal divider painted at the center of each [`PANE_SEP_H`]
/// separator strip (TradingView parity): the pane boundary was invisible
/// before (the strip only sensed drags, it painted nothing), so stacked
/// sub-panes blended together. Slightly brighter than the faint grid so the
/// boundary reads without competing with the data.
pub(crate) const PANE_DIVIDER: Color32 = Color32::from_rgb(36, 41, 49);

/// CVD sub-pane line color (SP2) — the vike palette's BLUE accent
/// (`indicators.rs`'s private `PALETTE[0]`, the default first-indicator-line
/// color), reused here as a fixed single-purpose color rather than pulling in
/// that rotation.
pub(crate) const CVD_COLOR: Color32 = Color32::from_rgb(87, 165, 255);

/// Volume-profile overlay (SP2, T4) — same blue hue as [`CVD_COLOR`], translucent.
/// `BarChart::color` derives both the histogram bars' fill (further faded, `linear_multiply`d
/// internally) and their outline stroke from this one value. `_const`: the plain
/// `from_rgba_unmultiplied` isn't a `const fn` (E0015).
pub(crate) const PROFILE_COLOR: Color32 = Color32::from_rgba_unmultiplied_const(87, 165, 255, 90);
/// Value-area band fill (SP2, T4) — the same blue family as [`PROFILE_COLOR`], near-invisible
/// so it reads as a soft highlight behind price action rather than an opaque overlay.
pub(crate) const PROFILE_VA_COLOR: Color32 =
    Color32::from_rgba_unmultiplied_const(87, 165, 255, 18);
/// Point-of-control line (SP2, T4) — the vike palette's amber accent (same hex as `dom.rs`'s
/// private `LAST`, the DOM ladder's last-trade row), duplicated rather than shared for the same
/// reason [`CVD_COLOR`] is: a fixed single-purpose color, not a rotation array.
pub(crate) const PROFILE_POC_COLOR: Color32 = Color32::from_rgb(240, 180, 41);

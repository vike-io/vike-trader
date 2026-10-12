//! Module-level constants split out of `chart.rs` (chart refactor PR-1):
//! the y-tick budget. The orderflow-overlay colours and the pane-layout geometry (gutter width, axis strip,
//! pane floor, separator) live in `ui-theme.toml` (`vike_ui_theme::value::chart`). The values and their
//! doc-comments are verbatim; only the location changed.

/// y-axis tick budget passed to `scale::nice_ticks` for the price plot's
/// custom `y_grid_spacer` in the MAPPED modes (Log/Percent; Linear delegates
/// to egui_plot's own default spacer — chart-UX bundle T2). A display tuning
/// constant (not derived from a spec number), chosen to land in the same
/// density class as that default.
pub(crate) const Y_TICK_BUDGET: usize = 10;

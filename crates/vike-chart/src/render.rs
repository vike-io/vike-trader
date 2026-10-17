//! Per-style series renderers + indicator overlay/oscillator painting + the
//! chart-STYLE menu icon, split out of chart.rs (chart-UX bundle T0). Pure
//! egui_plot painting — no interaction state, no layout; `chart::draw`
//! orchestrates these against the price/volume/oscillator panes it owns.

// The former per-style color consts (UP/DOWN/UP_S/DOWN_S/LINE/GRID/CROSS) became
// `ChartOptions` fields (chart-UX bundle T6), and every painter below now reads the frame's
// RESOLVED colours, `c: &ChartColors`, threaded in by `chart::draw` (`crate::colors`, design
// system spec §3.5). Alpha-FILL colours derive from a resolved colour through
// `vike_ui_theme::color::with_alpha`; the alphas, and the candle, footprint-cell, OHLC-tick and bar
// widths (in bar widths, the plot's x-unit), are the `chart` group of `ui-theme.toml`.

pub mod candles;
pub(crate) mod footprint;
pub(crate) mod lines;
pub mod oscillator;
pub(crate) mod overlays;
pub(crate) mod style_icon;
pub(crate) mod synthetic;

#[path = "render_tests.rs"]
#[cfg(test)]
mod render_tests;

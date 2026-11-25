//! The vike chart engine — extracted from vike-app (chart roadmap phase 2).
//!
//! Owns the GUI-local render model (integer bar-index [`chart::state::bar::ChartBar`] /
//! [`chart::state::ChartState`] fed per CoreSnapshot by the app), the egui_plot
//! renderer ([`chart::draw`]: 20 styles, volume + oscillator sub-panes,
//! follow-live edge, visible-range y-autofit), the synthetic style transforms
//! (Renko/Range/LineBreak/Kagi/PnF, ports of the Python `chart_transforms.py`),
//! the per-bar orderflow substrate rendering ([`chart::style::ChartStyle::Footprint`] +
//! the CVD/volume-profile overlays, SP2), and the indicator paint adapter
//! ([`chart::study::ChartStudy`]) over the parity-gated vike-indicators compute core.
//!
//! Dependency contract: {vike-desktop, vike-app-core} → vike-chart → the `vike-*` crates its
//! manifest names under `[dependencies]` (`crates/vike-chart/Cargo.toml` is the authority, not this
//! sentence; today they are vike-marketdata, vike-indicators, vike-orderflow and vike-ui-theme — `vike-model` is not among them), `vike-app` → vike-chart until the
//! GUI's 2026-09-09 rename and split. No GUI windowing or GPU layers (this crate rides the CI roster; the desktop binary does not)
//! and no dependency on the execution core — data arrives as plain `ChartState`s.

pub mod chart;
pub(crate) mod lod;
pub(crate) mod orderflow;
/// The fixtures the integration tests share with the unit tests (`test-support` only: a shipped
/// build never compiles it).
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use chart::interact::ChartFollowLive;
pub use chart::pane::ChartPaneFractions;
pub use chart::scale::ChartScaleMode;
pub use chart::settings::{ChartSettings, ChartSettingsDialog};
pub use chart::study::{ChartStudy, ChartStudyDialog, ChartStudyEdit};
pub use chart::sync::ChartSyncInput;
pub use chart::{ChartActions, ChartInputs, ChartNavigation};

#[cfg(test)]
mod testutil;

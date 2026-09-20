//! The vike chart engine — extracted from vike-app (chart roadmap phase 2).
//!
//! Owns the GUI-local render model (integer bar-index [`model::Bar`] /
//! [`model::ChartState`] fed per CoreSnapshot by the app), the egui_plot
//! renderer ([`chart::draw`]: 20 styles, volume + oscillator sub-panes,
//! follow-live edge, visible-range y-autofit), the synthetic style transforms
//! (Renko/Range/LineBreak/Kagi/PnF, ports of the Python `chart_transforms.py`),
//! the per-bar orderflow substrate rendering ([`chart::ChartStyle::Footprint`] +
//! the CVD/volume-profile overlays, SP2), and the indicator paint adapter
//! ([`indicators::Active`]) over the parity-gated vike-indicators compute core.
//!
//! Dependency contract: vike-app → vike-chart → {vike-indicators, vike-model}.
//! No GUI windowing or GPU layers (this crate is CI-gated; the app is not) and no
//! dependency on the execution core — data arrives as plain `ChartState`s.

pub mod chart;
pub mod indicators;
pub mod interact;
pub mod lod;
pub mod model;
pub mod options;
pub mod options_chain;
pub mod orderflow;
pub mod panes;
pub mod render;
pub mod scale;
pub mod studies;
pub mod sync;
pub mod transforms;
pub mod tz;

pub use chart::{ChartActions, ChartInputs, ChartStyle, Nav, draw};
pub use indicators::Active;
pub use interact::FollowLive;
pub use lod::lod_decimate;
pub use model::{Bar, ChartState};
pub use options::{ChartOptions, IndicatorDialog, IndicatorEdit, SettingsDialog};
pub use options_chain::{
    InstrumentBook, OptionChainActions, OptionChainInputs, OptionOrderClick, WorkingOrderLite,
};
pub use orderflow::{cell_text_legible, cvd_from_footprints};
pub use panes::{PaneFractions, PaneKey};
pub use scale::ScaleMode;
pub use studies::{ActiveStudy, StudyKind, StudyMeta, get_study, push_study_panes, study_registry};
pub use sync::SyncIn;
pub use tz::{DisplayTz, to_naive};

//! Shared egui trading widgets — the future home for reusable egui panels across the vike
//! GUIs. Chart-engine-free by construction: no egui_plot / ChartState / indicators, and no
//! dependency on the execution core.
//!
//! Today this crate holds only the DOM (depth-of-market) order-entry ladder + the Elite
//! order-flow liquidity heatmap ([`dom`]), moved verbatim out of vike-chart. It is pure
//! `egui` painting over `vike_model::{L2Book, Level, VenueCaps}` — data in, neutral actions
//! out, the same seam the chart uses.

pub mod dom;

pub use dom::{DomAction, DomInputs, DomMode, DomOrder, DomPosition, DomState, DomVenue};

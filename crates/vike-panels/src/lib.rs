//! Shared egui trading widgets — the future home for reusable egui panels across the vike
//! GUIs. Chart-engine-free by construction: no egui_plot / ChartState / indicators, and no
//! dependency on the execution core.
//!
//! Today this crate holds the Trade window ([`trade`]): the price ladder, the order ticket, the
//! instrument bar and a status strip in one window. It replaced the DOM (depth-of-market) ladder
//! that lived here before it. It is pure `egui` painting over `vike_model::{L2Book, BookLevel,
//! VenueCaps}` — data in, neutral actions out, the same seam the chart uses. It paints from the
//! design system — `vike_ui_theme::components::Tokens` and the component kit — so it follows the
//! installed theme, market colours, text size and density.

#![warn(unreachable_pub)]

pub mod trade;

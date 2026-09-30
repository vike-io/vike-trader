//! Shared egui trading widgets — the future home for reusable egui panels across the vike
//! GUIs. Chart-engine-free by construction: no egui_plot / ChartState / indicators, and no
//! dependency on the execution core.
//!
//! Today this crate holds only the DOM (depth-of-market) order-entry ladder + the Elite
//! order-flow liquidity heatmap ([`dom`]), moved verbatim out of vike-chart. It is pure
//! `egui` painting over `vike_model::{L2Book, BookLevel, VenueCaps}` — data in, neutral actions
//! out, the same seam the chart uses. It paints from the design system —
//! `vike_ui_theme::components::Tokens` and the component kit — so it follows the installed
//! theme, market colours, text size and density.

pub mod dom;

pub use dom::{
    BookAbsence, DomAction, DomInputs, DomMode, DomOrder, DomPosition, DomState, DomVenue,
    NO_BOOK_HEADLINE, NO_BOOK_SUBLINE,
};

//! The Data-Manager cached-series catalog, extracted from vike-app for reuse (Studio + app) and CI.
//! `model` groups the flat `HistStore` inventory into a venue→symbol→series tree (pure); `view`
//! renders it (egui). No wgpu — in CI like vike-chart/vike-studio.

pub mod model;
pub mod view;

pub use model::{RollUp, SeriesRow, SymbolNode, VenueNode, build_tree};
pub use view::{
    BulkAction, GapMap, GridResponse, GridState, PROXY_DIRECT, PROXY_HELP, PROXY_HINT,
    PartialDayMap, ProxyEdit, SeriesKey, SortColumn, SortState, StoredSelection, ViewFilter,
    coverage_label, filter_tree, has_gaps, has_partial_days, partial_days_from_coverage,
    partial_days_label, polymarket_proxy_ui, proxy_display, proxy_to_store, stored_catalog_grid,
    stored_catalog_ui, views_sidebar,
};

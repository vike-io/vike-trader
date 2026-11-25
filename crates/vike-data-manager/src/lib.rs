//! The Data-Manager cached-series catalog, mounted by vike-app-core (the desktop's Data Manager) and
//! by the Studio. `inventory` groups the flat `HistoryStore` inventory into a venue→symbol→series
//! tree (pure) and draws it (egui); `polymarket_proxy` is the Data Manager's proxy box. No wgpu — in
//! CI like vike-chart/vike-studio.

mod inventory;
mod polymarket_proxy;
#[cfg(test)]
mod test_support;

pub use inventory::filter::{
    CHECKING_MARK, DataManagerInventoryFilter, DataManagerInventoryGapMap, GAPS_CHECKING,
    GAPS_NOT_CHECKED, PARTIALS_CHECKING, filter_tree, gaps_unknown, global_span, has_gaps,
    is_stale, stale_cutoff_ms,
};
pub use inventory::grid::{
    DataManagerInventoryGridBulkAction, DataManagerInventoryGridState, inventory_grid,
};
pub use inventory::list::inventory_list;
pub use inventory::partial_days::{
    DataManagerInventoryPartialDayMap, instrument_key_of, partial_days_from_coverage,
    partial_days_label, symbol_partial_label,
};
pub use inventory::table::{
    DataManagerInventorySort, DataManagerInventorySortColumn, coverage_label, coverage_legend,
};
pub use inventory::{
    DataManagerInventoryRollup, DataManagerInventorySeries, DataManagerInventorySeriesKey,
    DataManagerInventorySymbol, DataManagerInventoryVenue, build_inventory_tree,
};
pub use polymarket_proxy::{DataManagerPolymarketProxyEdit, polymarket_proxy_ui};

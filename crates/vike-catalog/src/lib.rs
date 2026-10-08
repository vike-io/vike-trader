//! Universal cross-venue instrument catalog. Bridge-free: depends on `vike-model` only, so the
//! per-venue bridge crates and `vike-chart` can depend on it down-only. Holds the `Instrument`
//! model, a two-mode `CatalogProvider` trait, the tiered fuzzy `Catalog` search the Symbol picker
//! renders from, and the per-venue capability tables (addressing, intervals, session, history channels).
//!
//! ⚠ The `AssetClass` taxonomy itself is `vike_model::AssetClass` and no longer lives here — the
//! hist store has to record whether a series is spot or perp, and `vike-data` is this crate's own
//! layer, which the layer gate refuses. What stayed is the picker's `Tab` grouping (see `tab`).

// The picker's own drawers (`tab_for` is a free function: its module doc argues the split).
mod tab;
pub use tab::{Tab, tab_for};

mod instrument;
pub use instrument::Instrument;

mod field_map;
pub use field_map::{FieldMap, parse_with};

mod provider;
pub use provider::{CatalogError, CatalogMode, CatalogProvider, CatalogSource, SearchFilter};

// WHICH roster venues can be enumerated, and at whose expense (`docs/decisions/0062`).
mod availability;
pub use availability::{CatalogAvailability, catalog_availability, catalog_source_for};

// WHETHER a process serves the venue-catalog verb, and why (`docs/decisions/0066`).
mod gate;
pub use gate::{VenueCatalogGate, venue_catalog_gate, venue_catalog_gate_line};

// The SHIPPED baseline list: a separate read-only source from `CatalogCache`, on purpose (`docs/decisions/0066`).
mod baseline;
pub use baseline::{
    BASELINE_FILE, BASELINE_SOURCE, BASELINE_TOOL_DIR, BaselineCatalog, BaselineError,
    BaselineVenue, CatalogAnswer, LOCAL_FILE, LocalUpsert, Provenance, catalog_answer,
    merge_sources, upsert_local,
};

mod catalog;
pub use catalog::{Catalog, merge_ranked};

// What the symbol pickers draw: one line per underlying with a listing per venue.
mod underlying;
pub use underlying::{
    Kind, Listing, PickerQuery, Underlying, group_by_underlying, kind_of, picker_flat,
    picker_results, venue_counts,
};

mod persist;
pub use persist::{CatalogCache, VenueStamp, load_cache, save_cache};

mod session;
pub use session::session_calendar_for;

// Per-venue instrument-ADDRESSING table; FAILS CLOSED (`docs/decisions/0061`, Phase 1).
mod addressing;
pub use addressing::{BareSymbol, Naming, VenueAddressing, addressing_for};

// Per-venue bar-INTERVAL table, keyed like addressing (`docs/decisions/0061`, Phase 4).
mod intervals;
pub use intervals::{
    IntervalEvidence, IntervalVerdict, PROBED_AXIS, VenueIntervals, intervals_for,
};

// Per-venue HISTORY-CHANNELS table and its rendering (the page, the JSON, the sentences).
mod history;
pub use history::history_reference;
pub use history::{
    Access, ChannelClass, ChannelState, EvidenceSource, HistoryChannel, HistoryDepth,
    HistoryEvidence, HistoryKind, HistoryLane, Pace, PerRequest, StepLookback,
    history_channels_for,
};

// CORE-symbol <-> EXCHANGE-symbol conversion, the `.P` split and the fee lane it keys.
mod symbol;
pub use symbol::{
    DEX_MARKER, PAIR_SEPARATOR, PERP_SUFFIX, engine_lane_holds_stop, fee_lane, split_perp,
    split_perp_at, to_core_symbol, to_exchange_symbol, uses_perp_suffix,
    writes_a_pair_with_a_slash,
};

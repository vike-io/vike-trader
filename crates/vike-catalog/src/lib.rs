//! Universal cross-venue instrument catalog. Bridge-free: depends on `vike-model` only, so the
//! per-venue bridge crates and `vike-chart` can depend on it down-only. Holds the `Instrument`
//! model, the `AssetClass` taxonomy, a two-mode `CatalogProvider` trait, a declarative venue
//! registry, and the tiered fuzzy `Catalog` search that the Symbol picker renders from.

mod asset_class;
pub use asset_class::{AssetClass, Tab};

mod instrument;
pub use instrument::Instrument;

mod field_map;
pub use field_map::{FieldMap, parse_with};

mod provider;
pub use provider::{CatalogError, CatalogMode, CatalogProvider, CatalogSource, SearchFilter};

mod catalog;
pub use catalog::{Catalog, merge_ranked};

mod persist;
pub use persist::{CatalogCache, VenueStamp, load_cache, save_cache};

mod venues;
pub use venues::BRIDGE_VENUES;

mod session;
pub use session::session_calendar_for;

// The CORE-symbol ⇄ EXCHANGE-symbol conversion. `Instrument::id` already CONSUMES the `.P` marker a
// venue's catalog provider sets; this is the other half — the split every wire-facing adapter needs,
// which was an eight-times-repeated idiom until a ninth site forgot it entirely.
// `fee_lane` is the same split applied to the FEE table's key: a venue whose exec routes spot vs
// perp on `.P` needs its perp lane keyed separately in `vike_model::fee_schedule_for`, and the split
// lives here rather than there because vike-model sits below this crate.
mod symbol;
pub use symbol::{PERP_SUFFIX, fee_lane, split_perp, uses_perp_suffix};

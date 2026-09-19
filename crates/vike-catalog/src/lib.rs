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

// WHICH roster venues can be enumerated, and at whose expense — the per-venue capability map a
// process consults about venues whose bridge crates it does not link. `CatalogMode` answers a
// neighbouring question only where the provider IS linked, which the data daemon (six of fourteen)
// and the desktop (one) mostly are not. `docs/decisions/0062`'s decisions 3 and 5 rest on it.
mod availability;
pub use availability::{CatalogAvailability, catalog_availability, catalog_source_for};

// WHETHER a process serves the venue-catalog verb, and why — `docs/decisions/0066`. It lives here
// for the reason `availability` above does: the daemon that arms the lane and the surfaces that
// report on it must not be able to word one verdict differently, and both already depend on this
// crate while neither depends on the other.
mod gate;
pub use gate::{VenueCatalogGate, venue_catalog_gate, venue_catalog_gate_line};

// The SHIPPED baseline instrument list for the venues a server may never list — fetched out of
// band, carrying its own fetch date and the account-shaped qualifier that makes it honest
// (`docs/decisions/0066`, decisions 5-8). It is a SEPARATE read-only source from `CatalogCache`,
// deliberately: that type's `VenueStamp` records when and how many and never FROM WHERE, so a
// merged baseline would render as an operator's own refresh age.
mod baseline;
pub use baseline::{
    BASELINE_FILE, BASELINE_SOURCE, BASELINE_TOOL_DIR, BaselineCatalog, BaselineError,
    BaselineVenue, CatalogAnswer, LOCAL_FILE, LocalUpsert, Provenance, catalog_answer,
    merge_sources, upsert_local,
};

mod catalog;
pub use catalog::{Catalog, merge_ranked};

mod persist;
pub use persist::{CatalogCache, VenueStamp, load_cache, save_cache};

mod venues;
pub use venues::BRIDGE_VENUES;

mod session;
pub use session::session_calendar_for;

// The per-venue INSTRUMENT-ADDRESSING table (`docs/decisions/0061`, Phase 1): which classes each
// venue's data path can address, how a caller names one, and whether a bare symbol at that venue
// can silently resolve to another book. It lives here rather than in `vike-model` for the reason
// `session_calendar_for`'s module doc already gives: the key is a CLASS, and the class is a catalog
// concept. Unlike that function, this table FAILS CLOSED.
mod addressing;
pub use addressing::{BareSymbol, Naming, VenueAddressing, addressing_for};

// The CORE-symbol ⇄ EXCHANGE-symbol conversion. `Instrument::id` already CONSUMES the `.P` marker a
// venue's catalog provider sets; this is the other half — the split every wire-facing adapter needs,
// which was an eight-times-repeated idiom until a ninth site forgot it entirely.
// `fee_lane` is the same split applied to the FEE table's key: a venue whose exec routes spot vs
// perp on `.P` needs its perp lane keyed separately in `vike_model::fee_schedule_for`, and the split
// lives here rather than there because vike-model sits below this crate.
mod symbol;
pub use symbol::{PERP_SUFFIX, fee_lane, split_perp, uses_perp_suffix};

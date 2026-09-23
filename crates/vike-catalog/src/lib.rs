//! Universal cross-venue instrument catalog. Bridge-free: depends on `vike-model` only, so the
//! per-venue bridge crates and `vike-chart` can depend on it down-only. Holds the `Instrument`
//! model, a two-mode `CatalogProvider` trait, a declarative venue registry, and the tiered fuzzy
//! `Catalog` search that the Symbol picker renders from.
//!
//! ⚠ The `AssetClass` taxonomy itself is `vike_model::AssetClass` and no longer lives here — the
//! hist store has to record whether a series is spot or perp, and `vike-data` is this crate's own
//! layer, which the layer gate refuses. What stayed is the picker's `Tab` grouping (see `tab`).

// The picker's own drawers. The class the tab is computed FROM is a vocabulary concept one layer
// down; which drawer it belongs in is a claim about this window, so it stayed. `tab`'s module doc
// argues the split and why `tab_for` is a free function.
mod tab;
pub use tab::{Tab, tab_for};

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

mod session;
pub use session::session_calendar_for;

// The per-venue INSTRUMENT-ADDRESSING table (`docs/decisions/0061`, Phase 1): which classes each
// venue's data path can address, how a caller names one, and whether a bare symbol at that venue
// can silently resolve to another book. It lives here rather than in `vike-model` for the reason
// `session_calendar_for`'s module doc already gives — ⚠ which is no longer the LAYERING one it was
// written as: `AssetClass` moved down, so both facts are visible in `vike-model` now and nothing
// forces this table up here. What survives is that it is a CATALOG table — a claim about what a
// venue's catalog and data path can address — and 0061's own second reason, that nothing forces a
// per-venue table into the vocabulary crate. Unlike that function, this table FAILS CLOSED.
mod addressing;
pub use addressing::{BareSymbol, Naming, VenueAddressing, addressing_for};

// The per-venue INTERVAL table (`docs/decisions/0061`, Phase 4), keyed on the SAME `(venue, class)`
// pair the addressing table above establishes — which is why it lives beside it rather than beside
// the bridges' own code tables. STEP 1 in the playbook's strict sense: nothing consumes it yet, and
// the divergences it exposes are PINNED rather than fixed.
mod intervals;
pub use intervals::{
    IntervalEvidence, IntervalVerdict, PROBED_AXIS, VenueIntervals, intervals_for,
};

// The CORE-symbol ⇄ EXCHANGE-symbol conversion. `Instrument::id` already CONSUMES the `.P` marker a
// venue's catalog provider sets; this is the other half — the split every wire-facing adapter needs,
// which was an eight-times-repeated idiom until a ninth site forgot it entirely.
// `fee_lane` is the same split applied to the FEE table's key: a venue whose exec routes spot vs
// perp on `.P` needs its perp lane keyed separately in `vike_model::fee_schedule_for`, and the split
// lives here rather than there because vike-model sits below this crate.
// `split_perp_at` is the split asked of a VENUE rather than of a bare string — the seam
// `docs/decisions/0061`'s Phase 1 pins as missing, since every bridge stripped the suffix without
// ever asking whether its venue has one. It reads `uses_perp_suffix`, which in turn reads the
// addressing table above rather than carrying a second copy of the same three venue ids.
mod symbol;
pub use symbol::{
    DEX_MARKER, PAIR_SEPARATOR, PERP_SUFFIX, fee_lane, split_perp, split_perp_at, to_core_symbol,
    to_exchange_symbol, uses_perp_suffix, writes_a_pair_with_a_slash,
};

//! **The venue half of the per-symbol grid wiring**: reading a bridge's STEP-1 grid-source
//! declaration from its registry row. The pure half (`declared_symbol_grids`, `warn_ungridded_legs`,
//! the `GridSource` word the fold carries) is `crates/vike-tradehub-core/src/symbol_grid.rs`, whose
//! module doc is the authority for which mounts are wired and why the rest are not; on the wire the word
//! is `vike_gatewayhub_client::proto::GridSource`.

use vike_bridge_core::venue_adapter::DeclaredGridSource;

use super::registry::{VenueRow, row_of};
use vike_gatewayhub_client::proto::GridSource;

/// The grid-source declaration for `venue`, from its registry row.
#[must_use]
pub fn declared_grid_source(registry: &'static [VenueRow], venue: &str) -> DeclaredGridSource {
    match row_of(registry, venue) {
        Some(VenueRow::Adapter(row)) => row.declaration().grid_source,
        Some(VenueRow::FeatureAbsent { .. }) | None => DeclaredGridSource::NoGrid,
    }
}

/// The bridge enum as the word the fold carries, variant for variant.
#[must_use]
pub fn grid_source_of(declared: DeclaredGridSource) -> GridSource {
    match declared {
        DeclaredGridSource::InHand => GridSource::InHand,
        DeclaredGridSource::PerSymbolFetch => GridSource::PerSymbolFetch,
        DeclaredGridSource::NoGrid => GridSource::NoGrid,
    }
}

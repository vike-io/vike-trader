//! **The venue half of the per-symbol grid wiring**: reading a bridge's STEP-1 grid-source
//! declaration from its registry row. The pure half (`declared_symbol_grids`, `warn_ungridded_legs`,
//! the `GridSource` word the fold carries) is `crates/vike-tradehub-core/src/symbol_grid.rs`, whose
//! module doc is the authority for which mounts are wired and why the rest are not; on the wire the word
//! is `vike_model::GridSource` itself, the declaration's own type.

use super::registry::{VenueAdapterRow, row_of};
use vike_model::GridSource;

/// The grid-source declaration for `venue`, from its registry row.
#[must_use]
pub fn declared_grid_source(registry: &'static [VenueAdapterRow], venue: &str) -> GridSource {
    match row_of(registry, venue) {
        Some(VenueAdapterRow::Adapter(row)) => row.declaration().grid_source,
        Some(VenueAdapterRow::FeatureAbsent { .. }) | None => GridSource::NoGrid,
    }
}

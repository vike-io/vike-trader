//! **THE VENUE REGISTRY** — one row per `vike_model::VENUES` id: which bridge mounts it, or why
//! this build cannot. `vike-mount` names no venue; this table is where the names live — in the
//! one composition root that mounts venues, which already names the bridges for its feed arms —
//! and `vike_run::build_node` is handed it as `NodeConfig::registry`
//! (`docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`, amended 2026-09-29:
//! the table was first placed in `vike-run`, which would then have had to name every bridge).
//!
//! ⚠ A `VenueRow::Legacy` row is TRANSITIONAL: that venue is still mounted by `vike-mount`'s legacy
//! match. Each port flips its row here and in `vike_mount::transition::LEGACY_REGISTRY`, and
//! removes the venue from `vike_mount::transition::LEGACY_ARMS`; `tests/daemon/registry.rs` holds
//! all three in step.

use vike_mount::VenueRow;

/// Roster order. The optional bridges' rows become `#[cfg]` pairs on THIS crate's features as
/// those features move here.
#[rustfmt::skip]
pub const REGISTRY: &[VenueRow] = &[
    VenueRow::Legacy("binance"),
    VenueRow::Legacy("bybit"),
    VenueRow::Legacy("okx"),
    VenueRow::Legacy("deribit"),
    VenueRow::Legacy("oanda"),
    VenueRow::Legacy("ig"),
    VenueRow::Legacy("fxcm"),
    VenueRow::Legacy("dukascopy"),
    VenueRow::Legacy("polymarket"),
    VenueRow::Legacy("ibkr"),
    VenueRow::Legacy("ctrader"),
    VenueRow::Legacy("alpaca"),
    VenueRow::Legacy("aster"),
    VenueRow::Legacy("hyperliquid"),
    // vike:new-venue:row VenueRow::Mount(&{crate_}::mount::{Venue}VenueMount), // TODO(new-venue: {venue}): its scaffolded impl arms nothing until resolve/mount are written
];

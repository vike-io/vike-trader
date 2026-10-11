//! **THE VENUE REGISTRY** — one row per `vike_model::VENUES` id: which bridge mounts it, or why
//! this build cannot. `vike-mount` names no venue; this table is where the names live — in the
//! one composition root that mounts venues, which already names the bridges for its feed arms —
//! and `vike_mount::build_node` is handed it as `NodeConfig::registry`
//! (`docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`, amended 2026-09-29:
//! the table was first placed in `vike-run`, which would then have had to name every bridge).

use vike_mount::VenueRow;

/// Roster order. The optional bridges' rows are `#[cfg]` pairs on THIS crate's features.
#[rustfmt::skip]
pub const REGISTRY: &[VenueRow] = &[
    VenueRow::Mount(&vike_binance::mount::BinanceVenueMount),
    VenueRow::Mount(&vike_bybit::mount::BybitVenueMount),
    VenueRow::Mount(&vike_okx::mount::OkxVenueMount),
    VenueRow::Mount(&vike_deribit::mount::DeribitVenueMount),
    VenueRow::Mount(&vike_oanda::mount::OandaVenueMount),
    VenueRow::Mount(&vike_ig::mount::IgVenueMount),
    #[cfg(feature = "fxcm")]
    VenueRow::Mount(&vike_fxcm::mount::FxcmVenueMount),
    #[cfg(not(feature = "fxcm"))]
    VenueRow::FeatureAbsent { venue: "fxcm", feature: "fxcm" },
    VenueRow::Mount(&vike_dukascopy::mount::DukascopyVenueMount),
    #[cfg(feature = "polymarket")]
    VenueRow::Mount(&vike_polymarket::exec_plane::mount::PolymarketVenueMount),
    #[cfg(not(feature = "polymarket"))]
    VenueRow::FeatureAbsent { venue: "polymarket", feature: "polymarket" },
    #[cfg(feature = "ibkr")]
    VenueRow::Mount(&vike_ibkr::mount::IbkrVenueMount),
    #[cfg(not(feature = "ibkr"))]
    VenueRow::FeatureAbsent { venue: "ibkr", feature: "ibkr" },
    VenueRow::Mount(&vike_ctrader::mount::CtraderVenueMount),
    VenueRow::Mount(&vike_alpaca::mount::AlpacaVenueMount),
    VenueRow::Mount(&vike_aster::mount::AsterVenueMount),
    VenueRow::Mount(&vike_hyperliquid::mount::HyperliquidVenueMount),
    // vike:new-venue:row VenueRow::Mount(&{crate_}::mount::{Venue}VenueMount), // TODO(new-venue: {venue}): its scaffolded impl arms nothing until resolve/mount are written
];

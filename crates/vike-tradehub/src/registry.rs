//! **THE VENUE REGISTRY** — one row per `vike_model::VENUES` id: which bridge mounts it, or why
//! this build cannot. `vike-tradehub-core` names no venue; this table is where the names live — in the
//! one composition root that mounts venues, which already names the bridges for its feed arms —
//! and `vike_tradehub_core::assemble_runtime` is handed it as `RuntimeConfig::registry`
//! (`docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`, amended 2026-09-29:
//! the table was first placed in `vike-run`, which would then have had to name every bridge).

use vike_tradehub_core::VenueRow;

/// Roster order. The optional bridges' rows are `#[cfg]` pairs on THIS crate's features.
#[rustfmt::skip]
pub const REGISTRY: &[VenueRow] = &[
    VenueRow::Adapter(&vike_binance::adapter::BinanceVenueAdapter),
    VenueRow::Adapter(&vike_bybit::adapter::BybitVenueAdapter),
    VenueRow::Adapter(&vike_okx::adapter::OkxVenueAdapter),
    VenueRow::Adapter(&vike_deribit::adapter::DeribitVenueAdapter),
    VenueRow::Adapter(&vike_oanda::adapter::OandaVenueAdapter),
    VenueRow::Adapter(&vike_ig::adapter::IgVenueAdapter),
    #[cfg(feature = "fxcm")]
    VenueRow::Adapter(&vike_fxcm::adapter::FxcmVenueAdapter),
    #[cfg(not(feature = "fxcm"))]
    VenueRow::FeatureAbsent { venue: "fxcm", feature: "fxcm" },
    VenueRow::Adapter(&vike_dukascopy::adapter::DukascopyVenueAdapter),
    #[cfg(feature = "polymarket")]
    VenueRow::Adapter(&vike_polymarket::exec_plane::adapter::PolymarketVenueAdapter),
    #[cfg(not(feature = "polymarket"))]
    VenueRow::FeatureAbsent { venue: "polymarket", feature: "polymarket" },
    #[cfg(feature = "ibkr")]
    VenueRow::Adapter(&vike_ibkr::adapter::IbkrVenueAdapter),
    #[cfg(not(feature = "ibkr"))]
    VenueRow::FeatureAbsent { venue: "ibkr", feature: "ibkr" },
    VenueRow::Adapter(&vike_ctrader::adapter::CtraderVenueAdapter),
    VenueRow::Adapter(&vike_alpaca::adapter::AlpacaVenueAdapter),
    VenueRow::Adapter(&vike_aster::adapter::AsterVenueAdapter),
    VenueRow::Adapter(&vike_hyperliquid::adapter::HyperliquidVenueAdapter),
    // vike:new-venue:row VenueRow::Adapter(&{crate_}::mount::{Venue}VenueAdapter), // TODO(new-venue: {venue}): its scaffolded impl arms nothing until resolve/mount are written
];

//! **THE TRADE VENUE TABLE** — one row per `vike_model::VENUES` id: which bridge starts it, or why this
//! build cannot. It lived in `vike-tradehub` as `REGISTRY` (`crates/vike-gatewayhub/src/registry/trade.rs`) until
//! the venue half moved here (wave 2, W9b): this daemon is the one process that links the bridges, so
//! it is the one place that names them. `crate::trade::adapters::bridge_trade::BridgeTrade` is handed
//! [`REGISTRY`] and answers every `TradeLane` verb from it
//! (`docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`, amended 2026-09-29, and
//! `docs/decisions/0165-vike-mount-splits-and-no-other-hub-gets-a-core-crate.md`).
//!
//! The optional bridges' rows are `#[cfg]` pairs on THIS crate's `ibkr`, `fxcm` and `polymarket`
//! features, which each pull the bridge's own feature in (an off feature registers the venue
//! `FeatureAbsent`: no account of it runs `demo`/`live`, with the same answer in every build).

use crate::trade::adapters::registry::VenueRow;

/// Roster order.
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

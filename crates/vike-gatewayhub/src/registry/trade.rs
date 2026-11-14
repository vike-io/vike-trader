//! **THE TRADE VENUE TABLE** — one row per `vike_model::VENUES` id: which bridge starts it, or why this
//! build cannot. It lived in `vike-tradehub` as `REGISTRY` until
//! the venue half moved here (wave 2, W9b): this daemon is the one process that links the bridges, so
//! it is the one place that names them. `crate::trade::adapters::bridge_trade::TradeLaneBridge` is handed
//! [`REGISTRY`] and answers every `TradeLane` verb from it
//! (`docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`, amended 2026-09-29, and
//! `docs/decisions/0165-vike-mount-splits-and-no-other-hub-gets-a-core-crate.md`).
//!
//! The optional bridges' rows are `#[cfg]` pairs on THIS crate's `ibkr`, `fxcm` and `polymarket`
//! features, which each pull the bridge's own feature in (an off feature registers the venue
//! `FeatureAbsent`: no account of it runs `demo`/`live`, with the same answer in every build).

use crate::trade::adapters::registry::VenueAdapterRow;

/// Roster order.
#[rustfmt::skip]
pub const REGISTRY: &[VenueAdapterRow] = &[
    VenueAdapterRow::Adapter(&vike_binance::adapter::VenueAdapterBinance),
    VenueAdapterRow::Adapter(&vike_bybit::adapter::VenueAdapterBybit),
    VenueAdapterRow::Adapter(&vike_okx::adapter::VenueAdapterOkx),
    VenueAdapterRow::Adapter(&vike_deribit::adapter::VenueAdapterDeribit),
    VenueAdapterRow::Adapter(&vike_oanda::adapter::VenueAdapterOanda),
    VenueAdapterRow::Adapter(&vike_ig::adapter::VenueAdapterIg),
    #[cfg(feature = "fxcm")]
    VenueAdapterRow::Adapter(&vike_fxcm::adapter::VenueAdapterFxcm),
    #[cfg(not(feature = "fxcm"))]
    VenueAdapterRow::FeatureAbsent { venue: "fxcm", feature: "fxcm" },
    VenueAdapterRow::Adapter(&vike_dukascopy::adapter::VenueAdapterDukascopy),
    #[cfg(feature = "polymarket")]
    VenueAdapterRow::Adapter(&vike_polymarket::adapter::VenueAdapterPolymarket),
    #[cfg(not(feature = "polymarket"))]
    VenueAdapterRow::FeatureAbsent { venue: "polymarket", feature: "polymarket" },
    #[cfg(feature = "ibkr")]
    VenueAdapterRow::Adapter(&vike_ibkr::adapter::VenueAdapterIbkr),
    #[cfg(not(feature = "ibkr"))]
    VenueAdapterRow::FeatureAbsent { venue: "ibkr", feature: "ibkr" },
    VenueAdapterRow::Adapter(&vike_ctrader::adapter::VenueAdapterCtrader),
    VenueAdapterRow::Adapter(&vike_alpaca::adapter::VenueAdapterAlpaca),
    VenueAdapterRow::Adapter(&vike_aster::adapter::VenueAdapterAster),
    VenueAdapterRow::Adapter(&vike_hyperliquid::adapter::VenueAdapterHyperliquid),
    // vike:new-venue:row VenueRow::Adapter(&{crate_}::mount::{Venue}VenueAdapter), // TODO(new-venue: {venue}): its scaffolded impl arms nothing until resolve/mount are written
];

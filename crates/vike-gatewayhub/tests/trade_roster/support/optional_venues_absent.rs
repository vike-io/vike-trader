//! **The venue table with ibkr, fxcm and polymarket registered `FeatureAbsent`**: what `REGISTRY` is in a build
//! without those three features. It is the table the roster members' assertions were written against, and every
//! test build of this crate now has the features on (its self dev-dependency: decisions 0131 and 0132, as amended
//! 2026-11-06), so the members take this table as their argument and their assertions stay as they were. What they
//! prove is the arming fold over the real bridges and the `FeatureAbsent` row; each venue's feature-on half is
//! `ibkr_adapter.rs`, `fxcm_adapter.rs` and `polymarket_adapter.rs`, over the real table.
//!
//! Declared once by the `trade_roster` group's `main.rs`, and by `server_time_smoke.rs`, which is its own binary.
//! `registry_shape/registry.rs`'s `the_absent_table_is_the_daemons_but_for_the_three_optional_venues` holds it to the
//! table the daemon serves.

use vike_gatewayhub::trade::adapters::registry::VenueAdapterRow;

/// `crates/vike-gatewayhub/src/registry/trade.rs`'s `REGISTRY`, row for row, but for the three.
#[rustfmt::skip]
pub const REGISTRY: &[VenueAdapterRow] = &[
    VenueAdapterRow::Adapter(&vike_binance::adapter::VenueAdapterBinance),
    VenueAdapterRow::Adapter(&vike_bybit::adapter::VenueAdapterBybit),
    VenueAdapterRow::Adapter(&vike_okx::adapter::VenueAdapterOkx),
    VenueAdapterRow::Adapter(&vike_deribit::adapter::VenueAdapterDeribit),
    VenueAdapterRow::Adapter(&vike_oanda::adapter::VenueAdapterOanda),
    VenueAdapterRow::Adapter(&vike_ig::adapter::VenueAdapterIg),
    VenueAdapterRow::FeatureAbsent { venue: "fxcm", feature: "fxcm" },
    VenueAdapterRow::Adapter(&vike_dukascopy::adapter::VenueAdapterDukascopy),
    VenueAdapterRow::FeatureAbsent { venue: "polymarket", feature: "polymarket" },
    VenueAdapterRow::FeatureAbsent { venue: "ibkr", feature: "ibkr" },
    VenueAdapterRow::Adapter(&vike_ctrader::adapter::VenueAdapterCtrader),
    VenueAdapterRow::Adapter(&vike_alpaca::adapter::VenueAdapterAlpaca),
    VenueAdapterRow::Adapter(&vike_aster::adapter::VenueAdapterAster),
    VenueAdapterRow::Adapter(&vike_hyperliquid::adapter::VenueAdapterHyperliquid),
    // vike:new-venue:row VenueAdapterRow::Adapter(&{crate_}::adapter::VenueAdapter{Venue}), // TODO(new-venue: {venue}): the row `crates/vike-gatewayhub/src/registry/trade.rs` got
];

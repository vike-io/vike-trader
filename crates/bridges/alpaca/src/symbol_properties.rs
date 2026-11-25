//! **What the gatewayhub asks of alpaca to admit a symbol the engine was not started on** (decision 0173, amended for
//! this venue by `docs/ops/any-symbol-remaining-venues-plan.md`): the venue's half of the list-answered symbol facts
//! ([`SymbolPropertiesListSpecAlpaca`]): ONE asset read at a time with the asking account's key, the venue's own words
//! for an unknown asset, and the price grid this adapter can write (whole cents). The per-account listing, its bounds,
//! the grid checks every venue shares and the wire are the gatewayhub's
//! (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`'s `SymbolPropertiesCacheAccount`).

use vike_bridge_core::symbol_properties::SymbolPropertiesListSpec;
use vike_model::SymbolProperties;
use vike_utility::errors::CarriedRefusal;

use crate::refusals::AlpacaPropertiesRefusal;

/// **alpaca's symbol facts**: the engine's USER name (a share `AAPL`, a coin `BTCUSD`) is asked and listed under its
/// kept row's spelling (`AAPL`, `BTC/USD`; decision 0290), read ONE asset at a time with the asking account's key
/// (`GET /v1/assets/{symbol}`, against the several-MB active list); an unknown asset is refused in the venue's words,
/// and a price grid that is not whole cents is refused.
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesListSpecAlpaca;

impl SymbolPropertiesListSpec for SymbolPropertiesListSpecAlpaca {
    fn venue(&self) -> &'static str {
        "alpaca"
    }

    fn covers(&self, _list_key: &str, _account_listed: bool) -> Option<&'static str> {
        Some("US equities and crypto pairs")
    }

    /// The venue's spelling of the USER name, its kept row's (`crate::names::wire_of`): the asset asked for and the
    /// key its row is listed under. With no kept row the name itself, which the door has refused already.
    fn key(&self, symbol: &str) -> String {
        crate::names::wire_of(symbol).map_or_else(|_| symbol.to_string(), |(wire, _)| wire)
    }

    fn one_at_a_time(&self, _key: &str) -> bool {
        true
    }

    fn not_found(&self, symbol: &str) -> Option<CarriedRefusal> {
        Some(AlpacaPropertiesRefusal::UnknownAsset { symbol: symbol.to_string() }.carried())
    }

    /// The adapter writes every limit and stop price to the CENT (`crate::execution::event_mapper::build_order_body` formats on a fixed 0.01 step),
    /// so a price grid finer than a cent would be rounded after the engine judged the price at its own tick (a coin
    /// quoted at 0.0002 would rest at 0.00). Only a grid of whole cents is admitted; the mounted symbol's start is
    /// not touched.
    fn grid_refusal(&self, symbol: &str, properties: &SymbolProperties) -> Option<CarriedRefusal> {
        let cents = properties.tick_size / 0.01;
        (cents.round() < 1.0 || (cents - cents.round()).abs() > 1e-6).then(|| {
            let tick = properties.tick_size;
            AlpacaPropertiesRefusal::NotWholeCents { symbol: symbol.to_string(), tick }.carried()
        })
    }
}

//! **Which bridge's spec answers a list-answered venue's symbol facts** (`SymbolFacts::facts_via`, generic over
//! `vike_bridge_core::symbol_properties::SymbolPropertiesListSpec`): one table, found by the venue's slug. A venue with
//! no row here is answered `NOT_ANSWERED`. Each venue's spellings, list keys, classes and refusals are its bridge's
//! (owner ruling 2026-11-12: one-venue code lives in its bridge); ctrader's and oanda's list spec is their account
//! spec's (`SymbolPropertiesCacheAccountListed` runs the same type's fences), and only an account's own listing
//! answers them.

use vike_bridge_core::symbol_properties::SymbolPropertiesListSpec;

/// Every list-answered venue's spec.
static SPECS: &[&dyn SymbolPropertiesListSpec] = &[
    &vike_binance::symbol_properties::SymbolPropertiesListSpecBinance,
    &vike_aster::symbol_properties::SymbolPropertiesListSpecAster,
    &vike_bybit::symbol_properties::SymbolPropertiesListSpecBybit,
    &vike_okx::symbol_properties::SymbolPropertiesListSpecOkx,
    &vike_hyperliquid::symbol_properties::SymbolPropertiesListSpecHyperliquid,
    &vike_polymarket::symbol_properties::SymbolPropertiesListSpecPolymarket,
    &vike_deribit::symbol_properties::SymbolPropertiesListSpecDeribit,
    &vike_alpaca::symbol_properties::SymbolPropertiesListSpecAlpaca,
    &vike_ctrader::symbol_properties::SymbolPropertiesAccountSpecCtrader,
    &vike_oanda::SymbolPropertiesAccountSpecOanda,
];

/// The spec of `venue`, or `None` (the venue is not answered from a list).
pub(crate) fn list_spec(venue: &str) -> Option<&'static dyn SymbolPropertiesListSpec> {
    SPECS.iter().copied().find(|spec| spec.venue() == venue)
}

/// Every spec answers under its own slug, and no slug has two specs.
#[cfg(test)]
#[test]
fn every_spec_is_found_by_its_own_venue_and_only_once() {
    for spec in SPECS {
        let venue = spec.venue();
        assert_eq!(SPECS.iter().filter(|s| s.venue() == venue).count(), 1, "{venue}");
        assert_eq!(list_spec(venue).map(|s| s.venue()), Some(venue));
    }
    assert!(list_spec("ig").is_none(), "ig's facts are keyed, not listed");
}

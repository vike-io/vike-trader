//! Public spot + USDS-M perp instrument catalog — the searchable symbol universe behind the
//! chart's symbol picker, exposed as the venue's [`CatalogProvider`] contribution: the venue face
//! of the shared [`crate::family::catalog`].
//!
//! Reads the KEYLESS public `/api/v3/exchangeInfo` (spot) and `fapi /fapi/v1/exchangeInfo` (perp),
//! keeping what a symbol SEARCH needs (symbol id + base/quote + tradable-state), mapped to
//! universal [`Instrument`]s via the declarative `FieldMap`. Aster serves the same `exchangeInfo`
//! grammar, so the parse itself — previously a near-verbatim copy in each crate — lives once in
//! `family` and both venues pass their own venue string (F14, dedup rung 3). What stays HERE is the
//! genuine per-venue delta: the two endpoint URLs (plain `const`s — Binance's hosts are NOT
//! env-resolved) and this crate's own provider struct. Pure parse (`parse_spot`/`parse_perp`,
//! fixture-tested) is split from the blocking fetch so the mapping stays testable without network —
//! mirrors data.rs's parse/fetch split. The tests below stay HERE, exercising the shared code
//! through Binance's own wrapper: Aster's twin does the same through its own.

use serde_json::Value;
use vike_catalog::{AssetClass, CatalogError, CatalogMode, CatalogProvider, Instrument};

const SPOT_URL: &str = "https://api.binance.com/api/v3/exchangeInfo";
const PERP_URL: &str = "https://fapi.binance.com/fapi/v1/exchangeInfo";

/// Parse `/api/v3/exchangeInfo` into binance `CryptoSpot` [`Instrument`]s.
pub fn parse_spot(payload: &Value) -> Vec<Instrument> {
    crate::family::catalog::parse_spot(payload, "binance")
}

/// Parse `fapi /fapi/v1/exchangeInfo` into binance `CryptoPerp` [`Instrument`]s (`PERPETUAL`-gated
/// — delivery futures share the endpoint).
pub fn parse_perp(payload: &Value) -> Vec<Instrument> {
    crate::family::catalog::parse_perp(payload, "binance")
}

/// The binance venue's `CatalogProvider` contribution: spot (`CryptoSpot`) + USDS-M perp
/// (`CryptoPerp`), both bulk-enumerable off the keyless public `exchangeInfo` endpoints.
pub struct BinanceCatalog;

impl CatalogProvider for BinanceCatalog {
    fn venue(&self) -> &str {
        "binance"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        crate::family::catalog::ASSET_CLASSES
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        crate::family::catalog::list_instruments("binance", SPOT_URL, PERP_URL)
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use vike_catalog::AssetClass;

    #[test]
    fn spot_exchange_info_maps_to_cryptospot_instruments() {
        let payload = serde_json::json!({
            "symbols": [
                { "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT", "status": "TRADING" },
                { "symbol": "OLDUSDT", "baseAsset": "OLD", "quoteAsset": "USDT", "status": "BREAK" }
            ]
        });
        let out = parse_spot(&payload);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].raw_symbol, "BTCUSDT");
        assert_eq!(out[0].asset_class, AssetClass::CryptoSpot);
    }

    #[test]
    fn perp_exchange_info_maps_to_cryptoperp() {
        let payload = serde_json::json!({
            "symbols": [
                { "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT",
                  "status": "TRADING", "contractType": "PERPETUAL" }
            ]
        });
        let out = parse_perp(&payload);
        assert_eq!(out[0].asset_class, AssetClass::CryptoPerp);
        assert_eq!(out[0].raw_symbol, "BTCUSDT.P", "perp gets the .P distinct-symbol suffix");
        assert_eq!(out[0].base, "BTC");
        assert_eq!(out[0].quote, "USDT");
    }
}

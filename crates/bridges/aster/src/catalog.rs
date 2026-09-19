//! Public spot + USDⓈ-M perp instrument catalog — the searchable symbol universe behind the
//! chart's symbol picker, exposed as the venue's [`CatalogProvider`] contribution: the venue face
//! of the shared [`vike_binance::family::catalog`].
//!
//! Reads the KEYLESS public `/api/v3/exchangeInfo` (spot) and `fapi /fapi/v3/exchangeInfo` (perp)
//! on MAINNET — the picker always lists live mainnet instruments, regardless of which environment
//! (`Demo`/`Live`) execution happens to be trading against — keeping what a symbol SEARCH needs
//! (symbol id + base/quote + tradable-state), mapped to universal [`Instrument`]s via the
//! declarative `FieldMap`.
//!
//! Aster's `exchangeInfo` is a Binance fork (identical `symbols[]` shape and filters), so this
//! module's parse — previously a near-verbatim copy of Binance's — now lives once in the
//! Binance-wire-grammar core and this crate passes `"aster"` (F14, dedup rung 3). What stays HERE is
//! the genuine per-venue delta: the mainnet-always URL resolution below (Aster's own `env` threading
//! is invisible to the shared core) and this crate's own provider struct. Pure parse
//! (`parse_spot`/`parse_perp`, fixture-tested) is split from the blocking fetch so the mapping stays
//! testable without network — mirrors data.rs's parse/fetch split. The tests below stay HERE,
//! exercising the shared code through Aster's own wrapper: Binance's twin does the same through its
//! own, so the one shared implementation is proven twice.

use serde_json::Value;
use vike_bridge_core::Environment;
use vike_catalog::{AssetClass, CatalogError, CatalogMode, CatalogProvider, Instrument};

use crate::urls;

/// The public spot exchangeInfo endpoint — MAINNET always (see the module doc for why).
fn spot_url() -> String {
    format!("{}/api/v3/exchangeInfo", urls::urls_for(Environment::Live).sapi_rest)
}

/// The public USDⓈ-M perp exchangeInfo endpoint — MAINNET always (see [`spot_url`]).
fn perp_url() -> String {
    format!("{}/fapi/v3/exchangeInfo", urls::urls_for(Environment::Live).fapi_rest)
}

/// Parse `/api/v3/exchangeInfo` into aster `CryptoSpot` [`Instrument`]s.
pub fn parse_spot(payload: &Value) -> Vec<Instrument> {
    vike_binance::family::catalog::parse_spot(payload, "aster")
}

/// Parse `fapi /fapi/v3/exchangeInfo` into aster `CryptoPerp` [`Instrument`]s (`PERPETUAL`-gated —
/// delivery futures share the endpoint).
pub fn parse_perp(payload: &Value) -> Vec<Instrument> {
    vike_binance::family::catalog::parse_perp(payload, "aster")
}

/// The aster venue's `CatalogProvider` contribution: spot (`CryptoSpot`) + USDⓈ-M perp
/// (`CryptoPerp`), both bulk-enumerable off the keyless public MAINNET `exchangeInfo` endpoints.
pub struct AsterCatalog;

impl CatalogProvider for AsterCatalog {
    fn venue(&self) -> &str {
        "aster"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        vike_binance::family::catalog::ASSET_CLASSES
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        vike_binance::family::catalog::list_instruments("aster", &spot_url(), &perp_url())
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

    #[test]
    fn perp_requires_perpetual_contract_type() {
        // TRADING but a delivery future (not PERPETUAL) must be excluded — the extra gate beyond
        // spot's plain status check.
        let payload = serde_json::json!({
            "symbols": [
                { "symbol": "BTCUSDT_240329", "baseAsset": "BTC", "quoteAsset": "USDT",
                  "status": "TRADING", "contractType": "CURRENT_QUARTER" }
            ]
        });
        assert!(
            parse_perp(&payload).is_empty(),
            "delivery futures must not appear in the perp catalog"
        );
    }

    #[test]
    fn spot_and_perp_urls_are_aster_mainnet() {
        assert_eq!(spot_url(), "https://sapi.asterdex.com/api/v3/exchangeInfo");
        assert_eq!(perp_url(), "https://fapi.asterdex.com/fapi/v3/exchangeInfo");
    }
}

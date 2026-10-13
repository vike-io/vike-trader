//! Public spot + USDⓈ-M perp instrument catalog — the searchable symbol universe behind the
//! chart's symbol picker, exposed as the venue's [`CatalogProvider`] contribution: the venue face
//! of the shared [`vike_binance::family::catalog`].
//!
//! Reads the KEYLESS public `/api/v3/exchangeInfo` (spot) and `fapi /fapi/v3/exchangeInfo` (perp)
//! on MAINNET — the picker always lists live mainnet instruments, regardless of which environment
//! (`Demo`/`Live`) execution happens to be trading against — mapped to universal [`Instrument`]s,
//! each a typed `Contract` read off the row's own fields (`Contract::CryptoSpot` for spot; a
//! `Contract::Perp` whose margining and settle asset come from `marginAsset` for perp) and carrying
//! the row's tick/step/min/max/min-notional grid.
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
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::AccountTier;
use vike_model::{AssetClass, Instrument};

use crate::urls;

/// The public spot exchangeInfo endpoint — MAINNET always (see the module doc for why).
fn spot_url() -> String {
    format!("{}/api/v3/exchangeInfo", urls::urls_for(AccountTier::Live).sapi_rest)
}

/// The public USDⓈ-M perp exchangeInfo endpoint — MAINNET always (see [`spot_url`]).
fn perp_url() -> String {
    format!("{}/fapi/v3/exchangeInfo", urls::urls_for(AccountTier::Live).fapi_rest)
}

/// Parse `/api/v3/exchangeInfo` into aster `Contract::CryptoSpot` [`Instrument`]s with their grid.
pub fn parse_spot(payload: &Value) -> Vec<Instrument> {
    vike_binance::family::catalog::parse_spot(payload, "aster")
}

/// Parse `fapi /fapi/v3/exchangeInfo` into aster `Contract::Perp` [`Instrument`]s with their grid
/// and the venue's `contractType` word (`TRADING` `PERPETUAL`/`PERPETUAL_DELIVERING` rows only —
/// dated futures share the endpoint and are not listed).
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

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;

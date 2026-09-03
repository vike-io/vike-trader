//! Public spot + USDⓈ-M perp instrument catalog for the Binance-grammar venues — the searchable
//! symbol universe behind the chart's symbol picker. Shared by vike-binance (`crate::catalog`) and
//! vike-aster (`vike_aster::catalog`).
//!
//! Both venues serve the same `exchangeInfo` grammar (`symbols[]` carrying `symbol`/`baseAsset`/
//! `quoteAsset`/`status`, plus `contractType` on the futures endpoint), so the mapping — the
//! declarative [`FieldMap`] for spot, the `PERPETUAL`-gated loop for perp — lives once here with
//! `venue` a caller-supplied parameter, exactly like the rung-1 mappers.
//!
//! **The endpoint URLs stay OUT of this module.** They are the one real per-venue delta, passed in
//! as plain `&str` by each venue's provider: Binance holds them as `const`s (`api.binance.com` +
//! `fapi.binance.com/fapi/v1`), Aster builds them per call off `urls::urls_for(Environment::Live)`
//! — its picker deliberately lists MAINNET instruments regardless of which environment execution
//! trades against, which is Aster's own concern and invisible here. Nothing in this module decides
//! which network it is on, and nothing here became env-resolved.
//!
//! `CatalogProvider` is deliberately NOT implemented here: each venue owns its own unit struct and
//! its own `venue()`/`mode()`, and only the `list_instruments` body — the part that is identical —
//! delegates here. This module is wire grammar, never a registered venue.
//!
//! Pure parse is split from the blocking `fetch_json` so the mapping stays testable without network
//! (mirrors data.rs's parse/fetch split). Each venue keeps its OWN fixture tests over its OWN
//! wrapper, so this one implementation is proven twice.

use serde_json::Value;
use vike_catalog::{parse_with, AssetClass, CatalogError, FieldMap, Instrument};

/// The spot `exchangeInfo` shape both venues serve, keeping only what a symbol SEARCH needs.
const SPOT_MAP: FieldMap = FieldMap {
    list_path: &["symbols"],
    symbol: "symbol",
    base: "baseAsset",
    quote: "quoteAsset",
    active: Some(("status", "TRADING")),
};

/// The asset classes both venues' providers contribute — spot off `/api/v3/exchangeInfo`, USDⓈ-M
/// perp off the `fapi` twin. Each venue's `CatalogProvider::asset_classes` returns this.
pub const ASSET_CLASSES: &[AssetClass] = &[AssetClass::CryptoSpot, AssetClass::CryptoPerp];

/// Parse `/api/v3/exchangeInfo` into `CryptoSpot` [`Instrument`]s via the declarative [`FieldMap`],
/// stamped with the caller's `venue`.
pub fn parse_spot(payload: &Value, venue: &str) -> Vec<Instrument> {
    parse_with(&SPOT_MAP, venue, AssetClass::CryptoSpot, payload)
}

/// Parse the `fapi` `exchangeInfo` into `CryptoPerp` [`Instrument`]s, stamped with the caller's
/// `venue`. Perp needs the extra `contractType == "PERPETUAL"` gate (delivery futures share the
/// same endpoint), so it's a small fn rather than a pure [`FieldMap`] — the escape-hatch case for
/// venues whose "active" test needs more than one field.
pub fn parse_perp(payload: &Value, venue: &str) -> Vec<Instrument> {
    let Some(list) = payload.get("symbols").and_then(|s| s.as_array()) else { return Vec::new() };
    let mut out = Vec::new();
    for e in list {
        if e.get("status").and_then(|v| v.as_str()) != Some("TRADING") {
            continue;
        }
        if e.get("contractType").and_then(|v| v.as_str()) != Some("PERPETUAL") {
            continue;
        }
        let symbol = e.get("symbol").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        if symbol.is_empty() {
            continue;
        }
        out.push(Instrument {
            venue: venue.into(),
            // `.P` suffix (TradingView `BINANCE:BTCUSDT.P` convention): a distinct vike symbol so a
            // perp doesn't collide with its spot twin (both are `BTCUSDT` on the exchange). The
            // venue's market feed strips the `.P` back to the exchange symbol at the fstream/fapi
            // boundary; everywhere else in vike the perp IS `BTCUSDT.P`.
            raw_symbol: format!("{symbol}.P"),
            asset_class: AssetClass::CryptoPerp,
            base: e.get("baseAsset").and_then(|v| v.as_str()).unwrap_or("").to_uppercase(),
            quote: e.get("quoteAsset").and_then(|v| v.as_str()).unwrap_or("").to_uppercase(),
            description: String::new(),
            properties: Default::default(),
        });
    }
    out
}

/// Blocking `GET url` → parsed JSON via the shared [`vike_bridge_core::http::get_json`] (no
/// signer/credentials — both exchangeInfo endpoints are keyless public reads); called once per
/// `AssetClass` by [`list_instruments`]. Only the `CatalogError` wrap is ours — the call, the
/// 200-range gate and the per-stage error strings are the shared seam's.
fn fetch_json(url: &str) -> Result<Value, CatalogError> {
    vike_bridge_core::http::get_json(url).map_err(CatalogError)
}

/// The body of each venue's `CatalogProvider::list_instruments`: fetch + parse spot off `spot_url`,
/// then USDⓈ-M perp off `perp_url`, both stamped with `venue`. Per-endpoint TOLERANT (mirrors
/// okx's per-instType accumulate): one endpoint failing must NOT drop the other — a `?` here used
/// to short-circuit, so a spot outage wiped the venue's perps from the Symbol picker too. A miss is
/// warned (never silent) and the surviving endpoint's instruments still populate.
pub fn list_instruments(
    venue: &str,
    spot_url: &str,
    perp_url: &str,
) -> Result<Vec<Instrument>, CatalogError> {
    Ok(list_tolerant(venue, spot_url, perp_url, fetch_json))
}

/// [`list_instruments`]'s fold over an injectable fetch — split out so the one-endpoint-down
/// tolerance is testable without network.
fn list_tolerant(
    venue: &str,
    spot_url: &str,
    perp_url: &str,
    fetch: impl Fn(&str) -> Result<Value, CatalogError>,
) -> Vec<Instrument> {
    let mut out = Vec::new();
    match fetch(spot_url) {
        Ok(v) => out.extend(parse_spot(&v, venue)),
        Err(e) => tracing::warn!(
            target: "vike_binance::family::catalog",
            "{venue} spot fetch failed (skipped): {e}"
        ),
    }
    match fetch(perp_url) {
        Ok(v) => out.extend(parse_perp(&v, venue)),
        Err(e) => tracing::warn!(
            target: "vike_binance::family::catalog",
            "{venue} perp fetch failed (skipped): {e}"
        ),
    }
    out
}

#[cfg(test)]
mod tolerant_tests {
    use super::*;

    /// The (g) robustness contract, mirroring okx's per-instType accumulate: the spot endpoint
    /// failing must not wipe the venue — the perps still come back (and vice versa).
    #[test]
    fn one_failed_endpoint_still_yields_the_other_endpoints_instruments() {
        let spot = serde_json::json!({ "symbols": [
            { "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT", "status": "TRADING" }
        ]});
        let perp = serde_json::json!({ "symbols": [
            { "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT",
              "status": "TRADING", "contractType": "PERPETUAL" }
        ]});
        // spot down → perps survive
        let out = list_tolerant("binance", "spot://u", "perp://u", |url| {
            if url == "spot://u" {
                Err(CatalogError("HTTP 500: simulated".into()))
            } else {
                Ok(perp.clone())
            }
        });
        assert_eq!(out.len(), 1, "the perp page must survive the spot failure");
        assert_eq!(out[0].raw_symbol, "BTCUSDT.P");
        // perp down → spot survives
        let out = list_tolerant("binance", "spot://u", "perp://u", |url| {
            if url == "perp://u" {
                Err(CatalogError("HTTP 500: simulated".into()))
            } else {
                Ok(spot.clone())
            }
        });
        assert_eq!(out.len(), 1, "the spot page must survive the perp failure");
        assert_eq!(out[0].raw_symbol, "BTCUSDT");
        // both failing yields an EMPTY catalog (warned), never an error
        assert!(list_tolerant("binance", "s", "p", |_| Err(CatalogError("down".into()))).is_empty());
    }
}

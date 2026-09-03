//! Public spot + linear-perp instrument catalog — the Bybit twin of binance's `catalog` module,
//! exposed as the venue's [`CatalogProvider`] contribution to the chart's cross-venue symbol
//! search. Reads the KEYLESS public `/v5/market/instruments-info` (`category=spot` +
//! `category=linear`, UNSIGNED GETs — no credentials, no signer), mapped to universal
//! [`Instrument`]s via the declarative [`FieldMap`]. The chart's live bar feed
//! ([`crate::market_feed`]) streams the same concatenated symbol id (`BTCUSDT`), so a
//! searched-and-selected Bybit symbol subscribes cleanly.
//!
//! Pure parse (`parse_spot`/`parse_perp`, fixture-tested) is split from the blocking `fetch_json`
//! so the mapping stays testable without network — mirrors binance/catalog.rs's shape exactly.

use serde_json::Value;
use vike_catalog::{
    parse_with, AssetClass, CatalogError, CatalogMode, CatalogProvider, FieldMap, Instrument,
};

const SPOT_MAP: FieldMap = FieldMap {
    list_path: &["result", "list"],
    symbol: "symbol",
    base: "baseCoin",
    quote: "quoteCoin",
    active: Some(("status", "Trading")),
};

const SPOT_URL: &str = "https://api.bybit.com/v5/market/instruments-info?category=spot";
const PERP_URL: &str = "https://api.bybit.com/v5/market/instruments-info?category=linear";

/// Parse the V5 `{result:{list}}` spot envelope into `CryptoSpot` [`Instrument`]s via the
/// declarative [`FieldMap`].
pub fn parse_spot(payload: &Value) -> Vec<Instrument> {
    parse_with(&SPOT_MAP, "bybit", AssetClass::CryptoSpot, payload)
}

/// Parse the V5 `{result:{list}}` linear-category envelope into `CryptoPerp` [`Instrument`]s.
/// Perp needs the extra `contractType == "LinearPerpetual"` gate (the `linear` category also
/// lists inverse/other contract types), so it's a small fn rather than a pure `FieldMap` — the
/// escape-hatch case for venues whose "active" test needs more than one field.
pub fn parse_perp(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.get("result").and_then(|r| r.get("list")).and_then(|l| l.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        if e.get("status").and_then(|v| v.as_str()) != Some("Trading") {
            continue;
        }
        if e.get("contractType").and_then(|v| v.as_str()) != Some("LinearPerpetual") {
            continue;
        }
        let symbol = e.get("symbol").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        if symbol.is_empty() {
            continue;
        }
        out.push(Instrument {
            venue: "bybit".into(),
            // `.P` suffix (TradingView convention): a distinct vike symbol so a linear perp doesn't
            // collide with its spot twin (both are `BTCUSDT` on Bybit). The feed strips the `.P` back
            // to the exchange symbol at the linear WS/REST boundary (feed wiring is a follow-up).
            raw_symbol: format!("{symbol}.P"),
            asset_class: AssetClass::CryptoPerp,
            base: e.get("baseCoin").and_then(|v| v.as_str()).unwrap_or("").to_uppercase(),
            quote: e.get("quoteCoin").and_then(|v| v.as_str()).unwrap_or("").to_uppercase(),
            description: String::new(),
            properties: Default::default(),
        });
    }
    out
}

/// Blocking `GET url` → parsed JSON via the shared [`vike_bridge_core::http::get_json`] (no
/// signer/credentials — both instruments-info endpoints are keyless public reads); called once per
/// `AssetClass`. Only the `CatalogError` wrap is venue-local.
fn fetch_json(url: &str) -> Result<Value, CatalogError> {
    vike_bridge_core::http::get_json(url).map_err(CatalogError)
}

/// The body of `list_instruments` over an injectable fetch — per-category TOLERANT: one endpoint
/// failing must NOT drop the other (okx's catalog documents the bug shape this prevents: a `?` on
/// one endpoint wiped the whole venue from the Symbol picker). Accumulate what succeeds; warn (not
/// fail) on each miss. Split from the provider so the tolerance is testable without network.
fn list_tolerant(fetch: impl Fn(&str) -> Result<Value, CatalogError>) -> Vec<Instrument> {
    let mut out = Vec::new();
    for (label, url, parse) in [
        ("spot", SPOT_URL, parse_spot as fn(&Value) -> Vec<Instrument>),
        ("linear", PERP_URL, parse_perp as fn(&Value) -> Vec<Instrument>),
    ] {
        match fetch(url) {
            Ok(v) => out.extend(parse(&v)),
            Err(e) => {
                tracing::warn!(target: "vike_bybit::catalog", "{label} fetch failed (skipped): {e}")
            }
        }
    }
    out
}

/// The bybit venue's `CatalogProvider` contribution: spot (`CryptoSpot`) + linear perp
/// (`CryptoPerp`), both bulk-enumerable off the keyless public `instruments-info` endpoint.
/// Per-category tolerant (see [`list_tolerant`]) — mirrors okx's per-instType accumulate.
pub struct BybitCatalog;

impl CatalogProvider for BybitCatalog {
    fn venue(&self) -> &str {
        "bybit"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::CryptoSpot, AssetClass::CryptoPerp]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        Ok(list_tolerant(fetch_json))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use vike_catalog::AssetClass;

    #[test]
    fn spot_maps_to_cryptospot() {
        let payload = serde_json::json!({ "result": { "list": [
            { "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT", "status": "Trading" }
        ]}});
        let out = parse_spot(&payload);
        assert_eq!(out[0].asset_class, AssetClass::CryptoSpot);
        assert_eq!(out[0].raw_symbol, "BTCUSDT");
    }

    #[test]
    fn perp_linear_category_maps_to_cryptoperp() {
        let payload = serde_json::json!({ "result": { "list": [
            { "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT",
              "status": "Trading", "contractType": "LinearPerpetual" },
            { "symbol": "BTCUSDT-25JUL26", "baseCoin": "BTC", "quoteCoin": "USDT",
              "status": "Trading", "contractType": "LinearFutures" }
        ]}});
        let out = parse_perp(&payload);
        assert_eq!(out.len(), 1, "only the LinearPerpetual row survives");
        assert_eq!(out[0].asset_class, AssetClass::CryptoPerp);
        assert_eq!(out[0].raw_symbol, "BTCUSDT.P", "perp gets the .P distinct-symbol suffix");
    }

    /// The (g) robustness contract, mirroring okx's per-instType accumulate: the spot endpoint
    /// failing must not wipe the venue — the linear perps still come back (and vice versa).
    #[test]
    fn one_failed_endpoint_still_yields_the_other_endpoints_instruments() {
        let perp_payload = serde_json::json!({ "result": { "list": [
            { "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT",
              "status": "Trading", "contractType": "LinearPerpetual" }
        ]}});
        let out = list_tolerant(|url| {
            if url == SPOT_URL {
                Err(CatalogError("HTTP 500: simulated".into()))
            } else {
                Ok(perp_payload.clone())
            }
        });
        assert_eq!(out.len(), 1, "the linear page must survive the spot failure");
        assert_eq!(out[0].raw_symbol, "BTCUSDT.P");
        // both failing yields an EMPTY catalog (warned), never an error
        assert!(list_tolerant(|_| Err(CatalogError("down".into()))).is_empty());
    }
}

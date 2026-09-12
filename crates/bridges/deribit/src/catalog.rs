//! Public options + futures/perp instrument catalog — the Deribit twin of okx's `catalog`
//! module, exposed as the venue's [`CatalogProvider`] contribution to the chart's cross-venue
//! symbol search. Reads the KEYLESS public `/api/v2/public/get_instruments` (one UNSIGNED GET
//! per currency — no credentials, no signer), mapped to universal [`Instrument`]s.
//!
//! Pure parse (`parse_instruments`, fixture-tested) is split from the blocking `fetch_json` so
//! the mapping stays testable without network — mirrors binance/bybit/okx's catalog modules
//! exactly. Deribit has no single "all currencies" endpoint, so `list_instruments` fetches a
//! fixed currency set (BTC, ETH) and concatenates.

use serde_json::Value;
use vike_catalog::{AssetClass, CatalogError, CatalogMode, CatalogProvider, Instrument};

const BASE: &str = "https://www.deribit.com/api/v2/public/get_instruments";
/// Currencies fetched to cover the tradable universe (crate-reorg twin of `chain.rs`'s
/// `list_underlyings`, minus SOL — SOL has no options; it only appears under the shared USDC
/// book, out of scope for this venue-native currency-keyed enumeration).
const CURRENCIES: [&str; 2] = ["BTC", "ETH"];

/// Parse a Deribit `{result:[…]}` payload (one currency's instruments) into universal
/// [`Instrument`]s. `is_active == false` rows are skipped. `kind == "option"` maps to
/// [`AssetClass::Option`]; `kind == "future"` maps to [`AssetClass::CryptoPerp`] when
/// `settlement_period == "perpetual"`, else [`AssetClass::CryptoFuture`]. Unknown kinds
/// (e.g. `future_combo`/`spot`) are skipped.
pub fn parse_instruments(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.get("result").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        if e.get("is_active").and_then(|v| v.as_bool()) == Some(false) {
            continue;
        }
        let class = match e.get("kind").and_then(|v| v.as_str()) {
            Some("option") => AssetClass::Option,
            Some("future") => {
                if e.get("settlement_period").and_then(|v| v.as_str()) == Some("perpetual") {
                    AssetClass::CryptoPerp
                } else {
                    AssetClass::CryptoFuture
                }
            }
            _ => continue,
        };
        let name = e.get("instrument_name").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        if name.is_empty() {
            continue;
        }
        let base = e.get("base_currency").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        let quote = e.get("quote_currency").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        out.push(Instrument {
            venue: "deribit".into(),
            raw_symbol: name,
            asset_class: class,
            base,
            quote,
            description: String::new(),
            properties: Default::default(),
        });
    }
    out
}

/// Blocking `GET url` → parsed JSON via the shared [`vike_bridge_core::http::get_json`] (no
/// signer/credentials — `public/get_instruments` is a keyless public read); called once per
/// currency. Only the `CatalogError` wrap is venue-local.
fn fetch_json(url: &str) -> Result<Value, CatalogError> {
    vike_bridge_core::http::get_json(url).map_err(CatalogError)
}

/// The body of `list_instruments` over an injectable fetch — per-currency TOLERANT: one currency
/// failing must NOT drop the others (okx's catalog documents the bug shape this prevents: a `?` on
/// one endpoint wiped the whole venue from the Symbol picker). Accumulate what succeeds; warn (not
/// fail) on each miss. Split from the provider so the tolerance is testable without network.
fn list_tolerant(fetch: impl Fn(&str) -> Result<Value, CatalogError>) -> Vec<Instrument> {
    let mut out = Vec::new();
    for ccy in CURRENCIES {
        match fetch(&format!("{BASE}?currency={ccy}")) {
            Ok(v) => out.extend(parse_instruments(&v)),
            Err(e) => {
                tracing::warn!(target: "vike_deribit::catalog", "{ccy} fetch failed (skipped): {e}")
            }
        }
    }
    out
}

/// The deribit venue's `CatalogProvider` contribution: options + futures + perps, fetched one
/// currency at a time (BTC, ETH) and tagged via [`parse_instruments`]. Keyless. Per-currency
/// tolerant (see [`list_tolerant`]) — mirrors okx's per-instType accumulate.
pub struct DeribitCatalog;

impl CatalogProvider for DeribitCatalog {
    fn venue(&self) -> &str {
        "deribit"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::Option, AssetClass::CryptoFuture, AssetClass::CryptoPerp]
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

    fn row(
        name: &str,
        kind: &str,
        settlement_period: &str,
        is_active: bool,
        base: &str,
        quote: &str,
    ) -> Value {
        serde_json::json!({
            "instrument_name": name,
            "kind": kind,
            "settlement_period": settlement_period,
            "is_active": is_active,
            "base_currency": base,
            "quote_currency": quote,
        })
    }

    #[test]
    fn maps_option_perp_future_and_skips_inactive() {
        let payload = serde_json::json!({ "result": [
            row("BTC-27JUN25-60000-C", "option", "month", true, "BTC", "BTC"),
            row("BTC-PERPETUAL", "future", "perpetual", true, "BTC", "USD"),
            row("BTC-27JUN25", "future", "month", true, "BTC", "USD"),
            row("ETH-27JUN25-3000-P", "option", "month", false, "ETH", "ETH"),
        ]});
        let out = parse_instruments(&payload);
        assert_eq!(out.len(), 3);

        assert_eq!(out[0].raw_symbol, "BTC-27JUN25-60000-C");
        assert_eq!(out[0].asset_class, AssetClass::Option);
        assert_eq!(out[0].base, "BTC");
        assert_eq!(out[0].quote, "BTC");

        assert_eq!(out[1].raw_symbol, "BTC-PERPETUAL");
        assert_eq!(out[1].asset_class, AssetClass::CryptoPerp);

        assert_eq!(out[2].raw_symbol, "BTC-27JUN25");
        assert_eq!(out[2].asset_class, AssetClass::CryptoFuture);
    }

    #[test]
    fn venue_and_uppercases_raw_symbol() {
        let payload = serde_json::json!({ "result": [
            row("btc-perpetual", "future", "perpetual", true, "BTC", "USD"),
        ]});
        let out = parse_instruments(&payload);
        assert_eq!(out[0].venue, "deribit");
        assert_eq!(out[0].raw_symbol, "BTC-PERPETUAL");
    }

    #[test]
    fn empty_or_missing_result_yields_empty() {
        assert!(parse_instruments(&serde_json::json!({})).is_empty());
        assert!(parse_instruments(&serde_json::json!({"result": []})).is_empty());
    }

    /// The (g) robustness contract, mirroring okx's per-instType accumulate: one currency's fetch
    /// failing must not wipe the venue — the OTHER currency's instruments still come back.
    #[test]
    fn one_failed_currency_still_yields_the_other_currencys_instruments() {
        let out = list_tolerant(|url| {
            if url.contains("currency=BTC") {
                Err(CatalogError("HTTP 400: simulated".into()))
            } else {
                Ok(serde_json::json!({ "result": [
                    row("ETH-PERPETUAL", "future", "perpetual", true, "ETH", "USD"),
                ]}))
            }
        });
        assert_eq!(out.len(), 1, "the ETH page must survive the BTC failure");
        assert_eq!(out[0].raw_symbol, "ETH-PERPETUAL");
        // all currencies failing yields an EMPTY catalog (warned), never an error
        assert!(list_tolerant(|_| Err(CatalogError("down".into()))).is_empty());
    }
}

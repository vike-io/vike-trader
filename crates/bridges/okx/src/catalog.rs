//! Public spot + swap/futures/option instrument catalog — the OKX twin of binance's `catalog`
//! module, exposed as the venue's [`CatalogProvider`] contribution to the chart's cross-venue
//! symbol search. Reads the KEYLESS public `/api/v5/public/instruments` (one UNSIGNED GET per
//! `instType` — no credentials, no signer), mapped to universal [`Instrument`]s. The chart's live
//! bar feed ([`crate::market_feed`]) streams `candle<bar>` on the same dashed instId (`BTC-USDT`),
//! so a searched-and-selected OKX symbol subscribes cleanly. OKX's dashed instIds also naturally
//! sit in a distinct key namespace from binance's concatenated ones, so chart keys never collide.
//!
//! Pure parse (`parse_insts`, fixture-tested) is split from the blocking `fetch_json` so the
//! mapping stays testable without network — mirrors binance and bybit's catalog modules exactly.
//! OKX's `instType`s (SPOT/SWAP/FUTURES/OPTION) each need a separate fetch (the venue has no
//! single "all instruments" endpoint), so `parse_insts` takes the target [`AssetClass`] as a
//! parameter and is called once per instType. ⚠ OPTION needs one MORE level than the other three:
//! it refuses a bare `instType=OPTION` listing, so it is fetched once per UNDERLYING off the
//! separate keyless `/public/underlying` endpoint — see [`parse_underlyings`].

use serde_json::Value;
use vike_catalog::{AssetClass, CatalogError, CatalogMode, CatalogProvider, Instrument};

const BASE: &str = "https://www.okx.com/api/v5/public/instruments";

/// OKX's OPTION `instType` is the ONE that refuses a bare listing: `?instType=OPTION` answers HTTP
/// 400 code `50015` ("Either parameter uly or instFamily is required"). The tradable families are
/// published by this separate KEYLESS endpoint, whose `data` is a single-element array WRAPPING
/// the list — `{"data": [["SOL-USD", "BTC-USD", …]]}` — not a flat array of strings.
const UNDERLYING: &str = "https://www.okx.com/api/v5/public/underlying";

/// Parse the underlying endpoint's doubly-nested `data`. A shapeless payload yields NOTHING rather
/// than failing: a missing family list must skip options, never drop the rest of the catalog.
pub fn parse_underlyings(payload: &Value) -> Vec<String> {
    payload
        .get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|inner| inner.as_array())
        .flatten()
        .filter_map(|f| f.as_str())
        .filter(|f| !f.is_empty())
        .map(str::to_string)
        .collect()
}

/// The per-underlying OPTION listing URL. ⚠ `uly`, NOT `instFamily`: the venue's 50015 message
/// offers both, but only `uly` serves every underlying `/public/underlying` lists — see
/// [`parse_underlyings`] and this function's test.
fn option_instruments_url(uly: &str) -> String {
    format!("{BASE}?instType=OPTION&uly={uly}")
}

/// Parse an OKX `{data:[…]}` payload, tagging every row with `class`. `state == "live"` only.
/// base/quote come from baseCcy/quoteCcy when present, else the first two dash segments of
/// instId (SWAP/FUTURES/OPTION rows carry empty baseCcy/quoteCcy).
pub fn parse_insts(payload: &Value, class: AssetClass) -> Vec<Instrument> {
    let Some(list) = payload.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        if e.get("state").and_then(|v| v.as_str()) != Some("live") {
            continue;
        }
        let inst_id = e.get("instId").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        if inst_id.is_empty() {
            continue;
        }
        let base = e
            .get("baseCcy")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_uppercase())
            .unwrap_or_else(|| inst_id.split('-').next().unwrap_or("").to_string());
        let quote = e
            .get("quoteCcy")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_uppercase())
            .unwrap_or_else(|| inst_id.split('-').nth(1).unwrap_or("").to_string());
        out.push(Instrument {
            venue: "okx".into(),
            raw_symbol: inst_id,
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
/// signer/credentials — every `public/instruments` instType is a keyless public read); called once
/// per instType. Only the `CatalogError` wrap is venue-local.
fn fetch_json(url: &str) -> Result<Value, CatalogError> {
    vike_bridge_core::http::get_json(url).map_err(CatalogError)
}

/// The okx venue's `CatalogProvider` contribution: spot + swap(perp) + futures + options, each a
/// separate `instType` fetch tagged with its [`AssetClass`] via [`parse_insts`].
pub struct OkxCatalog;

impl CatalogProvider for OkxCatalog {
    fn venue(&self) -> &str {
        "okx"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[
            AssetClass::CryptoSpot,
            AssetClass::CryptoPerp,
            AssetClass::CryptoFuture,
            AssetClass::Option,
        ]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        // Per-instType TOLERANT: one endpoint failing must NOT drop the others. Accumulate what
        // succeeds; warn (not fail) on each miss so OKX's spot/perp/futures always populate.
        let mut out = Vec::new();
        for (inst_type, class) in [
            ("SPOT", AssetClass::CryptoSpot),
            ("SWAP", AssetClass::CryptoPerp),
            ("FUTURES", AssetClass::CryptoFuture),
        ] {
            match fetch_json(&format!("{BASE}?instType={inst_type}")) {
                Ok(v) => out.extend(parse_insts(&v, class)),
                Err(e) => {
                    tracing::warn!(target: "vike_okx::catalog", "{inst_type} fetch failed (skipped): {e}")
                }
            }
        }
        // OPTION is the one instType with no single-call listing (see `parse_underlyings`): one
        // keyless fetch for the families, then one listing per family. Fetching it the way the
        // others are fetched 400s, which is why OKX shipped an empty option catalog and a WARN on
        // every app start until 2026-08-22.
        match fetch_json(&format!("{UNDERLYING}?instType=OPTION")) {
            Ok(v) => {
                for uly in parse_underlyings(&v) {
                    match fetch_json(&option_instruments_url(&uly)) {
                        Ok(v) => out.extend(parse_insts(&v, AssetClass::Option)),
                        Err(e) => tracing::warn!(
                            target: "vike_okx::catalog",
                            "OPTION fetch failed for underlying {uly} (skipped): {e}"
                        ),
                    }
                }
            }
            Err(e) => tracing::warn!(
                target: "vike_okx::catalog",
                "OPTION underlying fetch failed (skipped, no options in the catalog): {e}"
            ),
        }
        Ok(out)
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    /// OKX's OPTION instType REFUSES a bare `instType=OPTION` (HTTP 400, code 50015 "Either
    /// parameter uly or instFamily is required"), so options were never in the catalog AT ALL:
    /// `asset_classes()` advertised `AssetClass::Option` while the picker received zero OKX
    /// options and a WARN fired on every app start. The module already knew — it made the loop
    /// tolerant so spot/perp/futures survive — but never fixed the call. This is the family
    /// endpoint's real payload, captured from the live venue 2026-08-22.
    #[test]
    fn option_underlyings_parse_from_the_nested_data_array() {
        let payload = serde_json::json!({
            "code": "0", "msg": "",
            "data": [["SOL-USD", "BTC-USD", "ETH-USD", "XAU-USD"]]
        });
        assert_eq!(parse_underlyings(&payload), ["SOL-USD", "BTC-USD", "ETH-USD", "XAU-USD"]);
    }

    /// …and a shapeless payload yields nothing rather than panicking — one endpoint failing must
    /// never drop the others, which is the tolerance this module already documents.
    #[test]
    fn option_underlyings_tolerate_a_shapeless_payload() {
        assert!(parse_underlyings(&serde_json::json!({})).is_empty());
        assert!(parse_underlyings(&serde_json::json!({ "data": [] })).is_empty());
        assert!(parse_underlyings(&serde_json::json!({ "data": [[]] })).is_empty());
        assert!(parse_underlyings(&serde_json::json!({ "data": [[""]] })).is_empty());
    }

    /// The URL that produced the 400: an OPTION listing MUST name its underlying. ⚠ It must do so
    /// as `uly`, NOT `instFamily` — the 50015 message offers both, but only `uly` works for every
    /// family the `/public/underlying` endpoint lists. Measured against the live venue 2026-08-22:
    /// `instFamily` serves BTC-USD/ETH-USD and answers `51000 "Parameter instFamily error"` for
    /// SOL-USD and XAU-USD, so pairing the underlying endpoint with `instFamily` reinstated the
    /// forever-warning this fix exists to remove, on two families out of four.
    #[test]
    fn the_option_listing_url_names_its_underlying() {
        let url = option_instruments_url("BTC-USD");
        assert!(url.contains("instType=OPTION"), "{url}");
        assert!(url.contains("uly=BTC-USD"), "the bare form is what 400s: {url}");
        assert!(!url.contains("instFamily="), "instFamily 400s on SOL-USD/XAU-USD: {url}");
    }
    #[test]
    fn swap_maps_to_cryptoperp_with_dashed_symbol() {
        let payload = serde_json::json!({ "data": [
            { "instId": "BTC-USDT-SWAP", "instType": "SWAP", "baseCcy": "", "quoteCcy": "",
              "settleCcy": "USDT", "state": "live" }
        ]});
        let out = parse_insts(&payload, AssetClass::CryptoPerp);
        assert_eq!(out[0].raw_symbol, "BTC-USDT-SWAP");
        assert_eq!(out[0].asset_class, AssetClass::CryptoPerp);
    }

    #[test]
    fn spot_maps_base_quote_from_ccy_fields() {
        let payload = serde_json::json!({ "data": [
            { "instId": "BTC-USDT", "instType": "SPOT", "baseCcy": "BTC", "quoteCcy": "USDT", "state": "live" }
        ]});
        let out = parse_insts(&payload, AssetClass::CryptoSpot);
        assert_eq!(out[0].base, "BTC");
        assert_eq!(out[0].quote, "USDT");
    }
}

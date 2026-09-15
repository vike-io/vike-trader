//! The declarative parse for flat-JSON Enumerable venues: a `FieldMap` names the list path + the
//! symbol/base/quote/active fields, and `parse_with` turns a venue payload into `Instrument`s.
//! Quirky venues (nested/computed/protobuf) skip this and provide their own parse fn instead.

use crate::{AssetClass, Instrument};
use serde_json::Value;

/// Field names for a flat venue instrument list. `active`: an optional `(field, expected)` gate —
/// rows whose `field != expected` are skipped (e.g. `("status", "Trading")`).
pub struct FieldMap {
    pub list_path: &'static [&'static str],
    pub symbol: &'static str,
    pub base: &'static str,
    pub quote: &'static str,
    pub active: Option<(&'static str, &'static str)>,
}

/// Parse a venue payload into `Instrument`s via `fm`. Missing/malformed → empty (never panics).
pub fn parse_with(
    fm: &FieldMap,
    venue: &str,
    class: AssetClass,
    payload: &Value,
) -> Vec<Instrument> {
    let mut node = payload;
    for seg in fm.list_path {
        match node.get(seg) {
            Some(n) => node = n,
            None => return Vec::new(),
        }
    }
    let Some(list) = node.as_array() else { return Vec::new() };
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        if let Some((field, expected)) = fm.active
            && entry.get(field).and_then(|v| v.as_str()) != Some(expected)
        {
            continue;
        }
        let symbol = entry.get(fm.symbol).and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        if symbol.is_empty() {
            continue;
        }
        out.push(Instrument {
            venue: venue.to_string(),
            raw_symbol: symbol,
            asset_class: class,
            base: entry.get(fm.base).and_then(|v| v.as_str()).unwrap_or("").to_uppercase(),
            quote: entry.get(fm.quote).and_then(|v| v.as_str()).unwrap_or("").to_uppercase(),
            description: String::new(),
            properties: Default::default(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AssetClass;

    const BYBIT_MAP: FieldMap = FieldMap {
        list_path: &["result", "list"],
        symbol: "symbol",
        base: "baseCoin",
        quote: "quoteCoin",
        active: Some(("status", "Trading")),
    };

    #[test]
    fn parses_active_rows_and_skips_others() {
        let payload = serde_json::json!({
            "result": { "list": [
                { "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT", "status": "Trading" },
                { "symbol": "OLDUSDT", "baseCoin": "OLD", "quoteCoin": "USDT", "status": "Delivering" }
            ]}
        });
        let out = parse_with(&BYBIT_MAP, "bybit", AssetClass::CryptoSpot, &payload);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].raw_symbol, "BTCUSDT");
        assert_eq!(out[0].base, "BTC");
        assert_eq!(out[0].venue, "bybit");
        assert_eq!(out[0].asset_class, AssetClass::CryptoSpot);
    }

    #[test]
    fn missing_list_yields_empty_never_panics() {
        let out = parse_with(&BYBIT_MAP, "bybit", AssetClass::CryptoSpot, &serde_json::json!({}));
        assert!(out.is_empty());
    }
}

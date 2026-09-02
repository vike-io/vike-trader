//! US equities + crypto instrument catalog — the alpaca twin of binance/bybit/okx's `catalog`
//! module, exposed as the venue's [`CatalogProvider`] contribution to the chart's cross-venue
//! symbol search. Unlike the keyless crypto venues, Alpaca's instrument list
//! (`GET /v1/assets?status=active`) sits behind the same OAuth2 Bearer every other Alpaca REST
//! call needs (see [`crate::auth::TokenSource`]) — so `AlpacaCatalog` holds an `Option<AlpacaConfig>`
//! rather than being a zero-arg unit struct, and `list_instruments` returns `Ok(vec![])` (never an
//! error) when credentials are absent, mirroring the bridge-wide "absent credentials is the live
//! gate" rule (`config::load_alpaca_config_from`). The credential map is INJECTED by the caller
//! (this repo stores them in the gitignored workspace `.env`, which never reaches process env —
//! `new` must never sweep `std::env::vars()` itself).
//!
//! Pure parse (`parse_assets`, fixture-tested) is split from the authed `fetch_assets` so the
//! mapping stays testable without network or a token — mirrors binance/bybit/okx's catalog
//! modules exactly. `class == "us_equity"` maps to `AssetClass::Equity` (bare ticker, quote
//! "USD"); `class == "crypto"` maps to `AssetClass::CryptoSpot` (slashed pair, e.g. `BTC/USD`,
//! base/quote split on `/`). `name` becomes `description`. Only `tradable == true` rows survive.

use std::sync::Arc;

use serde_json::Value;
use vike_bridge_core::credentials::Environment;
use vike_catalog::{AssetClass, CatalogError, CatalogMode, CatalogProvider, Instrument};

use crate::auth::TokenSource;
use crate::config::{load_alpaca_config_from, AlpacaConfig};
use crate::rest::AlpacaRest;

/// Parse a `/v1/assets` payload (a plain JSON array) into universal [`Instrument`]s.
/// `tradable == true` only; `class` selects [`AssetClass::Equity`] (`us_equity`) vs
/// [`AssetClass::CryptoSpot`] (`crypto`) — any other/missing class is skipped.
pub fn parse_assets(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for e in list {
        if !e.get("tradable").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }
        let symbol = e.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        if symbol.is_empty() {
            continue;
        }
        let class = e.get("class").and_then(|v| v.as_str()).unwrap_or("");
        let asset_class = match class {
            "us_equity" => AssetClass::Equity,
            "crypto" => AssetClass::CryptoSpot,
            _ => continue,
        };
        let (base, quote) = if asset_class == AssetClass::CryptoSpot {
            let mut parts = symbol.splitn(2, '/');
            (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string())
        } else {
            (symbol.to_string(), "USD".to_string())
        };
        let description = e.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        out.push(Instrument {
            venue: "alpaca".into(),
            raw_symbol: symbol.to_string(),
            asset_class,
            base,
            quote,
            description,
            properties: Default::default(),
        });
    }
    out
}

/// Authed blocking `GET /v1/assets?status=active` → parsed JSON. Mints its own [`TokenSource`]
/// (mirrors `AlpacaExecutionClient::spawn`'s construction) — a catalog fetch is a one-shot,
/// not a long-lived actor, so there's no reason to share the exec/data threads' token.
fn fetch_assets(config: &AlpacaConfig) -> Result<Value, CatalogError> {
    let token = Arc::new(TokenSource::new(
        config.client_id.clone(),
        config.client_secret.clone(),
        config.hosts.authx.to_string(),
    ));
    let rest = AlpacaRest::new(token);
    rest.get(config.hosts.broker, "/v1/assets", "status=active")
        .map_err(|e| CatalogError(format!("alpaca /v1/assets: {e}")))
}

/// The alpaca venue's `CatalogProvider` contribution: US equities + crypto, both off the one
/// `/v1/assets` endpoint. Credential-gated: [`AlpacaCatalog::new`] loads the sandbox config once
/// (absent credentials → `None`, held for the life of the provider); `list_instruments` returns
/// an empty catalog rather than erroring when there's nothing to authenticate with.
pub struct AlpacaCatalog {
    config: Option<AlpacaConfig>,
}

impl AlpacaCatalog {
    /// Resolves the sandbox config (same tier `AlpacaExecutionClient`/`AlpacaDataClient` use for
    /// paper trading) from the caller-supplied var map — the already-loaded workspace `.env`, NOT
    /// the process environment (which never holds these credentials in this repo). `None` when
    /// credentials are absent — the live gate.
    pub fn new(vars: &std::collections::HashMap<String, String>) -> Self {
        Self { config: load_alpaca_config_from(Environment::Demo, vars) }
    }
}

impl CatalogProvider for AlpacaCatalog {
    fn venue(&self) -> &str {
        "alpaca"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::Equity, AssetClass::CryptoSpot]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        let Some(config) = &self.config else { return Ok(Vec::new()) };
        Ok(parse_assets(&fetch_assets(config)?))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn maps_equity_and_crypto_skips_non_tradable() {
        let payload = serde_json::json!([
            { "symbol": "AAPL", "name": "Apple Inc. Common Stock", "class": "us_equity",
              "status": "active", "tradable": true },
            { "symbol": "BTC/USD", "name": "Bitcoin / US Dollar", "class": "crypto",
              "status": "active", "tradable": true },
            { "symbol": "XXXX", "name": "Halted Co", "class": "us_equity",
              "status": "inactive", "tradable": false }
        ]);
        let out = parse_assets(&payload);
        assert_eq!(out.len(), 2);

        let aapl = out.iter().find(|i| i.raw_symbol == "AAPL").expect("AAPL present");
        assert_eq!(aapl.asset_class, AssetClass::Equity);
        assert_eq!(aapl.base, "AAPL");
        assert_eq!(aapl.quote, "USD");
        assert_eq!(aapl.description, "Apple Inc. Common Stock");
        assert_eq!(aapl.venue, "alpaca");

        let btc = out.iter().find(|i| i.raw_symbol == "BTC/USD").expect("BTC/USD present");
        assert_eq!(btc.asset_class, AssetClass::CryptoSpot);
        assert_eq!(btc.base, "BTC");
        assert_eq!(btc.quote, "USD");
        assert_eq!(btc.description, "Bitcoin / US Dollar");

        assert!(out.iter().all(|i| i.raw_symbol != "XXXX"));
    }

    #[test]
    fn unknown_class_is_skipped() {
        let payload = serde_json::json!([
            { "symbol": "ODD", "name": "Odd thing", "class": "option", "tradable": true }
        ]);
        assert!(parse_assets(&payload).is_empty());
    }

    #[test]
    fn empty_payload_is_empty() {
        assert!(parse_assets(&serde_json::json!([])).is_empty());
        assert!(parse_assets(&serde_json::json!({})).is_empty());
    }

    /// The catalog must read the credentials it is HANDED, not the process environment.
    /// Before this fix `new()` swept `std::env::vars()`, so a `.env`-only credential set — which
    /// is the ONLY place this repo stores them — resolved to `None` and the venue silently
    /// vanished from the Symbol picker.
    #[test]
    fn catalog_resolves_credentials_from_the_injected_map() {
        let vars: std::collections::HashMap<String, String> = [
            ("ALPACA_SANDBOX_CLIENT_ID", "id"),
            ("ALPACA_SANDBOX_CLIENT_SECRET", "sec"),
            ("ALPACA_SANDBOX_ACCOUNT_ID", "acct"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert!(
            AlpacaCatalog::new(&vars).config.is_some(),
            "injected credentials must resolve without touching process env"
        );
    }

    /// An empty map means no credentials — never a panic, and never a silent fallback to process
    /// env.
    #[test]
    fn catalog_with_no_credentials_is_configless() {
        let vars = std::collections::HashMap::new();
        assert!(AlpacaCatalog::new(&vars).config.is_none());
    }
}

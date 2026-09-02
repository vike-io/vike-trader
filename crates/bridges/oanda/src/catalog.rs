//! FX + CFD instrument catalog — the oanda twin of `crates/bridges/alpaca/src/catalog.rs`.
//! OANDA's instrument list (`GET /v3/accounts/{account_id}/instruments`) sits behind the same
//! Bearer token every other OANDA REST call needs (see [`crate::rest::OandaRest`]), so
//! `OandaCatalog` holds an `Option<OandaConfig>` rather than being a zero-arg unit struct, and
//! `list_instruments` returns `Ok(vec![])` (never an error) when credentials are absent —
//! mirrors the bridge-wide "absent credentials is the live gate" rule
//! (`config::load_oanda_config_from`). The credential map is INJECTED by the caller (this repo
//! stores them in the gitignored workspace `.env`, which never reaches process env — `new` must
//! never sweep `std::env::vars()` itself).
//!
//! Pure parse (`parse_instruments`, fixture-tested) is split from the authed fetch so the mapping
//! stays testable without network or a token — mirrors alpaca's `catalog` module exactly.
//! `type == "CURRENCY"` maps to [`AssetClass::Fx`]; anything else (`CFD`, `METAL`, ...) maps to
//! [`AssetClass::Cfd`]. `name` (e.g. `EUR_USD`) becomes `raw_symbol`, split on `_` for base/quote
//! (base = first part, quote = second — also correct for a CFD like `SPX500_USD`).
//! `displayName` becomes `description`.

use serde_json::Value;
use vike_bridge_core::credentials::Environment;
use vike_catalog::{AssetClass, CatalogError, CatalogMode, CatalogProvider, Instrument};

use crate::config::{load_oanda_config_from, OandaConfig};
use crate::rest::OandaRest;

/// Parse a `/v3/accounts/{id}/instruments` payload (`{"instruments": [...]}`) into universal
/// [`Instrument`]s. `type == "CURRENCY"` -> [`AssetClass::Fx`]; anything else -> [`AssetClass::Cfd`].
pub fn parse_instruments(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.get("instruments").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let raw_symbol = name.to_uppercase();
        let kind = e.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let asset_class = if kind == "CURRENCY" { AssetClass::Fx } else { AssetClass::Cfd };
        let mut parts = raw_symbol.splitn(2, '_');
        let base = parts.next().unwrap_or("").to_string();
        let quote = parts.next().unwrap_or("").to_string();
        let description = e.get("displayName").and_then(|v| v.as_str()).unwrap_or("").to_string();
        out.push(Instrument {
            venue: "oanda".into(),
            raw_symbol,
            asset_class,
            base,
            quote,
            description,
            properties: Default::default(),
        });
    }
    out
}

/// Authed blocking `GET /v3/accounts/{account_id}/instruments` -> parsed JSON. Mints its own
/// [`OandaRest`] (mirrors alpaca's `fetch_assets`) — a catalog fetch is a one-shot, not a
/// long-lived actor, so there's no reason to share the exec/data threads' client.
fn fetch_instruments(config: &OandaConfig) -> Result<Value, CatalogError> {
    let rest = OandaRest::new(config.api_token.clone());
    rest.get(&config.rest_base, &format!("/v3/accounts/{}/instruments", config.account_id), "")
        .map_err(|e| CatalogError(format!("oanda /v3/instruments: {e}")))
}

/// The oanda venue's `CatalogProvider` contribution: FX + CFD, both off the one
/// `/v3/accounts/{id}/instruments` endpoint. Credential-gated: [`OandaCatalog::new`] loads the
/// demo config once (absent credentials -> `None`, held for the life of the provider);
/// `list_instruments` returns an empty catalog rather than erroring when there's nothing to
/// authenticate with.
pub struct OandaCatalog {
    config: Option<OandaConfig>,
}

impl OandaCatalog {
    /// Resolves the demo config (same tier the exec/data clients use for practice trading) from
    /// the caller-supplied var map — the already-loaded workspace `.env`, NOT the process
    /// environment (which never holds these credentials in this repo). `None` when credentials
    /// are absent — the live gate.
    pub fn new(vars: &std::collections::HashMap<String, String>) -> Self {
        Self { config: load_oanda_config_from(Environment::Demo, vars) }
    }
}

impl CatalogProvider for OandaCatalog {
    fn venue(&self) -> &str {
        "oanda"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::Fx, AssetClass::Cfd]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        let Some(config) = &self.config else { return Ok(Vec::new()) };
        Ok(parse_instruments(&fetch_instruments(config)?))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn maps_currency_and_cfd() {
        let payload = serde_json::json!({
            "instruments": [
                { "name": "EUR_USD", "type": "CURRENCY", "displayName": "EUR/USD" },
                { "name": "SPX500_USD", "type": "CFD", "displayName": "US SPX 500" }
            ]
        });
        let out = parse_instruments(&payload);
        assert_eq!(out.len(), 2);

        let eurusd = out.iter().find(|i| i.raw_symbol == "EUR_USD").expect("EUR_USD present");
        assert_eq!(eurusd.asset_class, AssetClass::Fx);
        assert_eq!(eurusd.base, "EUR");
        assert_eq!(eurusd.quote, "USD");
        assert_eq!(eurusd.description, "EUR/USD");
        assert_eq!(eurusd.venue, "oanda");

        let spx = out.iter().find(|i| i.raw_symbol == "SPX500_USD").expect("SPX500_USD present");
        assert_eq!(spx.asset_class, AssetClass::Cfd);
        assert_eq!(spx.base, "SPX500");
        assert_eq!(spx.quote, "USD");
        assert_eq!(spx.description, "US SPX 500");
    }

    #[test]
    fn metal_and_unknown_type_map_to_cfd() {
        let payload = serde_json::json!({
            "instruments": [
                { "name": "XAU_USD", "type": "METAL", "displayName": "Gold" },
                { "name": "WEIRD_THING", "type": "", "displayName": "Weird" }
            ]
        });
        let out = parse_instruments(&payload);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|i| i.asset_class == AssetClass::Cfd));
    }

    #[test]
    fn empty_payload_is_empty() {
        assert!(parse_instruments(&serde_json::json!({})).is_empty());
        assert!(parse_instruments(&serde_json::json!({ "instruments": [] })).is_empty());
    }

    /// The catalog must read the credentials it is HANDED, not the process environment.
    /// Before this fix `new()` swept `std::env::vars()`, so a `.env`-only credential set — which
    /// is the ONLY place this repo stores them — resolved to `None` and the venue silently
    /// vanished from the Symbol picker.
    #[test]
    fn catalog_resolves_credentials_from_the_injected_map() {
        let vars: std::collections::HashMap<String, String> = [
            ("OANDA_DEMO_API_KEY", "tok-abc-123"),
            ("OANDA_DEMO_ACCOUNT_ID", "101-004-1234567-001"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert!(
            OandaCatalog::new(&vars).config.is_some(),
            "injected credentials must resolve without touching process env"
        );
    }

    /// An empty map means no credentials — never a panic, and never a silent fallback to process
    /// env.
    #[test]
    fn catalog_with_no_credentials_is_configless() {
        let vars = std::collections::HashMap::new();
        assert!(OandaCatalog::new(&vars).config.is_none());
    }
}

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
//! modules exactly. What an entry IS and its grid are NOT decided here: both come from
//! `crates/bridges/alpaca/src/instruments.rs` (`contract_of`, `parse_asset_properties`), the same
//! two readers the startup grid pre-fetch uses, so a picker row and the mount's risk grid cannot
//! disagree about one entry. `us_equity` becomes `Contract::Equity` (bare ticker, quote "USD",
//! the entry's `exchange`); `crypto` becomes `Contract::CryptoSpot` (slashed pair, e.g. `BTC/USD`,
//! base/quote split on `/`); every other class — `us_option` included, which `/v1/assets` does not
//! list — is skipped. `name` becomes `description`. Only `tradable == true` rows survive.

use std::sync::Arc;

use serde_json::Value;
use vike_bridge_core::credentials::Environment;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{AssetClass, Contract, Instrument};

use crate::auth::TokenSource;
use crate::config::{AlpacaConfig, load_alpaca_config_from};
use crate::instruments::{contract_of, parse_asset_properties};
use crate::rest::AlpacaRest;

/// Parse a `/v1/assets` payload (a plain JSON array) into universal [`Instrument`]s.
/// `tradable == true` only (the gate `parse_asset_properties` applies); the contract is
/// `contract_of`'s, and an entry it answers `None` for is skipped. The row's `properties` is the
/// entry's own grid (`price_increment`, `min_trade_increment`, `min_order_size`, with the
/// equity defaults `parse_asset_properties` documents).
pub fn parse_assets(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for e in list {
        let Some((symbol, properties)) = parse_asset_properties(e) else { continue };
        if symbol.is_empty() {
            continue;
        }
        let Some(contract) = contract_of(e) else { continue };
        let (base, quote) = if contract == Contract::CryptoSpot {
            let mut parts = symbol.splitn(2, '/');
            (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string())
        } else {
            (symbol.clone(), "USD".to_string())
        };
        let description = e.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        out.push(
            Instrument::new("alpaca", symbol, base, quote, contract)
                .with_description(description)
                .with_properties(properties),
        );
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
        assert_eq!(aapl.asset_class(), AssetClass::Equity);
        assert_eq!(aapl.base, "AAPL");
        assert_eq!(aapl.quote, "USD");
        assert_eq!(aapl.description, "Apple Inc. Common Stock");
        assert_eq!(aapl.venue, "alpaca");

        let btc = out.iter().find(|i| i.raw_symbol == "BTC/USD").expect("BTC/USD present");
        assert_eq!(btc.asset_class(), AssetClass::CryptoSpot);
        assert_eq!(btc.base, "BTC");
        assert_eq!(btc.quote, "USD");
        assert_eq!(btc.description, "Bitcoin / US Dollar");

        assert!(out.iter().all(|i| i.raw_symbol != "XXXX"));
    }

    /// The `/v1/assets` fixture (shaped after Alpaca's documented asset object) through the
    /// catalog: the exact contract per class, the entry's own grid in the row, and the row's
    /// `properties` identical to what the startup grid pre-fetch reads off the same entry.
    #[test]
    fn fixture_rows_carry_the_contract_and_the_grid() {
        let payload: Value =
            serde_json::from_str(include_str!("../tests/fixtures/assets.json")).unwrap();
        let out = parse_assets(&payload);
        assert_eq!(
            out.iter().map(|i| i.raw_symbol.as_str()).collect::<Vec<_>>(),
            ["AAPL", "SPY", "BTC/USD"],
            "the untradable row is dropped, order kept"
        );

        let equity = |exchange: &str| {
            Contract::Equity(vike_model::EquitySpec {
                exchange: Some(exchange.to_string()),
                currency: Some("USD".to_string()),
            })
        };
        assert_eq!(out[0].contract, equity("NASDAQ"));
        // an ETF is `us_equity` on this venue: `Equity`, never a guessed `Etf`
        assert_eq!(out[1].contract, equity("ARCA"));
        assert_eq!(out[2].contract, Contract::CryptoSpot);

        let btc = &out[2].properties;
        assert_eq!(btc.tick_size, 1.0);
        assert_eq!(btc.step_size, 0.000000001);
        assert_eq!(btc.min_qty, 0.000016);
        assert_eq!(btc.asset_class, Some(AssetClass::CryptoSpot));
        let aapl = &out[0].properties;
        assert_eq!((aapl.tick_size, aapl.step_size), (0.01, 1.0), "equity defaults");
        assert_eq!(aapl.asset_class, Some(AssetClass::Equity));

        for (row, entry) in out.iter().zip(payload.as_array().unwrap()) {
            let (_, grid) = parse_asset_properties(entry).unwrap();
            assert_eq!(row.properties, grid, "{}: one reading of the entry", row.raw_symbol);
        }
    }

    /// `us_option` is not a class `/v1/assets` lists (option contracts come from
    /// `/options/contracts`), and the catalog cannot build a `Contract::Option` from this entry —
    /// skipped, as every unknown class is.
    #[test]
    fn unknown_class_is_skipped() {
        let payload = serde_json::json!([
            { "symbol": "ODD", "name": "Odd thing", "class": "option", "tradable": true },
            { "symbol": "AAPL250117C00150000", "name": "AAPL call", "class": "us_option",
              "tradable": true }
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

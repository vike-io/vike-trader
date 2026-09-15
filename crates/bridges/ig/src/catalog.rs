//! IG market-search catalog — the FIRST `QueryBacked` `CatalogProvider`. IG lists thousands of
//! epics (FX, indices, shares, commodities, crypto CFDs, options); there is no bulk "list every
//! market" endpoint worth caching, so instead of enumerating up front we search IG's server per
//! query (`GET /markets?searchTerm={query}`) — mirrors the credentialed-provider shape of
//! `crates/bridges/oanda/src/catalog.rs` (holds `Option<IgConfig>`, empty when creds are absent)
//! but implements `search_remote` instead of `list_instruments`.
//!
//! Pure parse (`parse_markets`, fixture-tested) is split from the authed fetch (which needs a
//! live `IgSession::login` handshake) so the mapping stays testable without network or a token.
//! `epic` (IG's tradeable id, e.g. `CS.D.EURUSD.MINI.IP`) becomes `raw_symbol` verbatim — IG
//! epics aren't a clean base/quote pair, so `base`/`quote` are left empty. `instrumentName`
//! becomes `description`. `instrumentType` maps: `CURRENCIES` -> [`AssetClass::Fx`], `INDICES` ->
//! [`AssetClass::Index`], `SHARES` -> [`AssetClass::Equity`]; everything else (`COMMODITIES`,
//! `CRYPTOCURRENCY`, `OPT_*`, ...) -> [`AssetClass::Cfd`].

use serde_json::Value;
use vike_bridge_core::credentials::Environment;
use vike_catalog::{
    AssetClass, CatalogError, CatalogMode, CatalogProvider, Instrument, SearchFilter,
};

use crate::config::{IgConfig, load_ig_config_from};
use crate::rest::IgSession;

/// Parse a `/markets?searchTerm=...` payload (`{"markets": [...]}`) into universal
/// [`Instrument`]s. Entries with an empty/missing `epic` are skipped. Never panics on missing
/// fields.
pub fn parse_markets(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.get("markets").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        let epic = e.get("epic").and_then(|v| v.as_str()).unwrap_or("");
        if epic.is_empty() {
            continue;
        }
        let kind = e.get("instrumentType").and_then(|v| v.as_str()).unwrap_or("");
        let asset_class = match kind {
            "CURRENCIES" => AssetClass::Fx,
            "INDICES" => AssetClass::Index,
            "SHARES" => AssetClass::Equity,
            _ => AssetClass::Cfd,
        };
        let description =
            e.get("instrumentName").and_then(|v| v.as_str()).unwrap_or("").to_string();
        out.push(Instrument {
            venue: "ig".into(),
            raw_symbol: epic.to_string(),
            asset_class,
            base: String::new(),
            quote: String::new(),
            description,
            properties: Default::default(),
        });
    }
    out
}

/// Log in, then authed `GET /markets?searchTerm={query}` -> parsed JSON. Mints its own
/// [`IgSession`] per search (a catalog search is a one-shot, not a long-lived actor — mirrors
/// `OandaCatalog::fetch_instruments` minting its own `OandaRest`).
fn fetch_markets(config: &IgConfig, query: &str) -> Result<Value, CatalogError> {
    let session =
        IgSession::login(config).map_err(|e| CatalogError(format!("ig /session login: {e}")))?;
    let q = format!("searchTerm={}", urlencode(query));
    session.get("/markets", "1", &q).map_err(|e| CatalogError(format!("ig /markets: {e}")))
}

/// Minimal query-string percent-encoding for a search term (space + reserved chars only — IG
/// search terms are plain instrument names/tickers, never arbitrary binary data).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The ig venue's `CatalogProvider` contribution: `QueryBacked` — thousands of epics, searched
/// live per query rather than enumerated. Credential-gated: [`IgCatalog::new`] loads the demo
/// config once (absent credentials -> `None`, held for the life of the provider); `search_remote`
/// returns an empty result rather than erroring when there's nothing to authenticate with.
pub struct IgCatalog {
    config: Option<IgConfig>,
}

impl IgCatalog {
    /// Resolves the demo config (same tier the exec/data clients use for practice trading) from
    /// the caller-supplied var map — the already-loaded workspace `.env`, NOT the process
    /// environment (which never holds these credentials in this repo). `None` when credentials
    /// are absent — the live gate.
    pub fn new(vars: &std::collections::HashMap<String, String>) -> Self {
        Self { config: load_ig_config_from(Environment::Demo, vars) }
    }
}

impl CatalogProvider for IgCatalog {
    fn venue(&self) -> &str {
        "ig"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::Fx, AssetClass::Cfd, AssetClass::Equity, AssetClass::Index]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::QueryBacked
    }
    // list_instruments: NOT overridden — QueryBacked venues use the trait default (empty).
    fn search_remote(
        &self,
        query: &str,
        _filter: &SearchFilter,
    ) -> Result<Vec<Instrument>, CatalogError> {
        let Some(config) = &self.config else { return Ok(Vec::new()) };
        Ok(parse_markets(&fetch_markets(config, query)?))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn maps_currencies_indices_shares_and_skips_empty_epic() {
        let payload = serde_json::json!({
            "markets": [
                { "epic": "CS.D.EURUSD.MINI.IP", "instrumentName": "EUR/USD Mini", "instrumentType": "CURRENCIES", "expiry": "-" },
                { "epic": "IX.D.FTSE.DAILY.IP", "instrumentName": "FTSE 100", "instrumentType": "INDICES", "expiry": "-" },
                { "epic": "UA.D.AAPL.CASH.IP", "instrumentName": "Apple Inc", "instrumentType": "SHARES", "expiry": "-" },
                { "epic": "", "instrumentName": "Nameless", "instrumentType": "CURRENCIES", "expiry": "-" }
            ]
        });
        let out = parse_markets(&payload);
        assert_eq!(out.len(), 3);

        let eurusd = out
            .iter()
            .find(|i| i.raw_symbol == "CS.D.EURUSD.MINI.IP")
            .expect("EURUSD epic present");
        assert_eq!(eurusd.asset_class, AssetClass::Fx);
        assert_eq!(eurusd.description, "EUR/USD Mini");
        assert_eq!(eurusd.venue, "ig");
        assert_eq!(eurusd.base, "");
        assert_eq!(eurusd.quote, "");

        let ftse =
            out.iter().find(|i| i.raw_symbol == "IX.D.FTSE.DAILY.IP").expect("FTSE epic present");
        assert_eq!(ftse.asset_class, AssetClass::Index);
        assert_eq!(ftse.description, "FTSE 100");

        let aapl =
            out.iter().find(|i| i.raw_symbol == "UA.D.AAPL.CASH.IP").expect("AAPL epic present");
        assert_eq!(aapl.asset_class, AssetClass::Equity);
        assert_eq!(aapl.description, "Apple Inc");

        assert!(!out.iter().any(|i| i.description == "Nameless"));
    }

    #[test]
    fn commodities_and_crypto_and_unknown_map_to_cfd() {
        let payload = serde_json::json!({
            "markets": [
                { "epic": "CC.D.GOLD.UNC.IP", "instrumentName": "Gold", "instrumentType": "COMMODITIES" },
                { "epic": "CS.D.BITCOIN.CFD.IP", "instrumentName": "Bitcoin", "instrumentType": "CRYPTOCURRENCY" },
                { "epic": "OP.D.WEIRD.CALL.IP", "instrumentName": "Weird Option", "instrumentType": "OPT_CALL" }
            ]
        });
        let out = parse_markets(&payload);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|i| i.asset_class == AssetClass::Cfd));
    }

    #[test]
    fn empty_payload_is_empty() {
        assert!(parse_markets(&serde_json::json!({})).is_empty());
        assert!(parse_markets(&serde_json::json!({ "markets": [] })).is_empty());
    }

    #[test]
    fn missing_fields_never_panic() {
        let payload = serde_json::json!({ "markets": [ {} ] });
        assert!(parse_markets(&payload).is_empty());
    }

    /// The catalog must read the credentials it is HANDED, not the process environment.
    /// Before this fix `new()` swept `std::env::vars()`, so a `.env`-only credential set — which
    /// is the ONLY place this repo stores them — resolved to `None` and the venue silently
    /// vanished from the Symbol picker.
    #[test]
    fn catalog_resolves_credentials_from_the_injected_map() {
        let vars: std::collections::HashMap<String, String> = [
            ("IG_DEMO_API_KEY", "key-xyz"),
            ("IG_DEMO_IDENTIFIER", "myuser"),
            ("IG_DEMO_PASSWORD", "s3cr3t"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert!(
            IgCatalog::new(&vars).config.is_some(),
            "injected credentials must resolve without touching process env"
        );
    }

    /// An empty map means no credentials — never a panic, and never a silent fallback to process
    /// env.
    #[test]
    fn catalog_with_no_credentials_is_configless() {
        let vars = std::collections::HashMap::new();
        assert!(IgCatalog::new(&vars).config.is_none());
    }
}

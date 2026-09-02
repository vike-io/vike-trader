//! FX + CFD instrument catalog — the cTrader twin of `crates/bridges/oanda/src/catalog.rs`.
//! cTrader has NO REST symbol-list endpoint: the symbol universe is enumerated over the Open API
//! protobuf SESSION (`ProtoOASymbolsListReq`, sent during the connect handshake — see
//! `conn::open_and_handshake`). So `CtraderCatalog` holds an `Option<CtraderConfig>` (the OAuth
//! token + account credential gate, resolved from the CALLER-supplied vars map via
//! `config::CtraderConfig::from_vars` — the settings-registry convention: libraries take
//! configuration as parameters, only binaries read the process env or the workspace `.env`) rather
//! than being a zero-arg unit struct, and `list_instruments` returns `Ok(vec![])` (never an error)
//! when credentials are absent — mirroring the bridge-wide "absent credentials is the live gate"
//! rule.
//!
//! The one-shot fetch ([`conn::fetch_symbols`]) runs the FULL handshake and returns just the built
//! `SymbolMap`, dropping the socket without ever spawning the live data/exec actor thread. The pure
//! [`classify`] mapping (fixture-tested) is split from that authed session fetch so the asset-class
//! heuristic stays testable without network or a token — mirrors every other venue's catalog module.
//!
//! Class heuristic (cTrader light symbols carry a `name` but no clean asset-class field): a name of
//! exactly 6 ASCII uppercase letters is treated as an FX pair (`Fx`, base = first 3, quote = last 3);
//! anything else is a CFD (`Cfd`, base = the whole name, quote = ""). So `EURUSD` -> Fx EUR/USD,
//! `US500`/`Apple.US` -> Cfd. An accepted imperfection: a 6-letter crypto name like `BTCUSD` also
//! reads as Fx. `raw_symbol` and `description` are both the name verbatim.

use vike_bridge_core::credentials::Environment;
use vike_catalog::{AssetClass, CatalogError, CatalogMode, CatalogProvider, Instrument};

use crate::config::CtraderConfig;
use crate::conn;

/// Classify a cTrader symbol name into `(asset_class, base, quote)`. Pure — the fixture-tested core
/// of the catalog mapping. A name of exactly 6 ASCII uppercase letters is an FX pair (base = first
/// three chars, quote = last three); everything else is a CFD (base = the full name, quote empty).
pub fn classify(name: &str) -> (AssetClass, String, String) {
    let is_fx = name.len() == 6 && name.bytes().all(|b| b.is_ascii_uppercase());
    if is_fx {
        (AssetClass::Fx, name[..3].to_string(), name[3..].to_string())
    } else {
        (AssetClass::Cfd, name.to_string(), String::new())
    }
}

/// Map an iterator of cTrader symbol names into universal [`Instrument`]s via [`classify`].
fn instruments_from_names<'a>(names: impl Iterator<Item = &'a str>) -> Vec<Instrument> {
    names
        .filter(|n| !n.is_empty())
        .map(|name| {
            let (asset_class, base, quote) = classify(name);
            Instrument {
                venue: "ctrader".into(),
                raw_symbol: name.to_string(),
                asset_class,
                base,
                quote,
                description: name.to_string(),
                properties: Default::default(),
            }
        })
        .collect()
}

/// The cTrader venue's `CatalogProvider` contribution: FX + CFD, enumerated over the Open API
/// protobuf session (there is no REST list). Credential-gated: [`CtraderCatalog::new`] resolves
/// the demo config once (absent credentials -> `None`, held for the life of the provider);
/// `list_instruments` returns an empty catalog rather than erroring when there's nothing to
/// authenticate with.
pub struct CtraderCatalog {
    config: Option<CtraderConfig>,
}

impl CtraderCatalog {
    /// Resolves the demo config (same tier the exec/data clients use for practice trading) from
    /// the caller-supplied var map — the already-loaded workspace `.env`, NOT the process
    /// environment (which never holds these credentials in this repo). `None` when credentials are
    /// absent — the live gate. Mirrors `AlpacaCatalog`/`IgCatalog`/`OandaCatalog`'s conversion.
    pub fn new(vars: &std::collections::HashMap<String, String>) -> Self {
        Self { config: CtraderConfig::from_vars(Environment::Demo, vars) }
    }
}

impl CatalogProvider for CtraderCatalog {
    fn venue(&self) -> &str {
        "ctrader"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::Fx, AssetClass::Cfd]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        let Some(config) = &self.config else { return Ok(Vec::new()) };
        // One-shot connect -> handshake -> read the SymbolMap -> drop the socket (no actor spawned).
        let symbols = conn::fetch_symbols(config.to_conn_config())
            .map_err(|e| CatalogError(format!("ctrader symbols: {e}")))?;
        Ok(instruments_from_names(symbols.names()))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn classify_six_upper_letters_is_fx_pair() {
        let (class, base, quote) = classify("EURUSD");
        assert_eq!(class, AssetClass::Fx);
        assert_eq!(base, "EUR");
        assert_eq!(quote, "USD");
    }

    #[test]
    fn classify_non_pair_is_cfd() {
        let (class, base, quote) = classify("US500");
        assert_eq!(class, AssetClass::Cfd);
        assert_eq!(base, "US500");
        assert_eq!(quote, "");

        // A name with a dot / digits / lowercase is never 6-upper-letters -> Cfd.
        assert_eq!(classify("Apple.US").0, AssetClass::Cfd);
        assert_eq!(classify("XAUUSD.").0, AssetClass::Cfd);
        assert_eq!(classify("eurusd").0, AssetClass::Cfd); // lowercase -> not FX
    }

    #[test]
    fn classify_six_letter_crypto_reads_as_fx_accepted_imperfection() {
        // Documented imperfection: a 6-uppercase-letter crypto name also classifies as Fx.
        let (class, base, quote) = classify("BTCUSD");
        assert_eq!(class, AssetClass::Fx);
        assert_eq!(base, "BTC");
        assert_eq!(quote, "USD");
    }

    #[test]
    fn instruments_from_names_maps_and_tags_venue() {
        let out = instruments_from_names(["EURUSD", "US500", ""].into_iter());
        assert_eq!(out.len(), 2); // empty name skipped

        let eurusd = out.iter().find(|i| i.raw_symbol == "EURUSD").expect("EURUSD present");
        assert_eq!(eurusd.venue, "ctrader");
        assert_eq!(eurusd.asset_class, AssetClass::Fx);
        assert_eq!(eurusd.base, "EUR");
        assert_eq!(eurusd.quote, "USD");
        assert_eq!(eurusd.description, "EURUSD");

        let us500 = out.iter().find(|i| i.raw_symbol == "US500").expect("US500 present");
        assert_eq!(us500.asset_class, AssetClass::Cfd);
        assert_eq!(us500.base, "US500");
        assert_eq!(us500.quote, "");
    }

    /// The catalog must read the credentials it is HANDED, not the process environment.
    /// `new()` used to sweep the workspace `.env` itself (`CtraderConfig::from_env`) — a library
    /// doing its own I/O the caller could neither see nor override (settings-registry rule);
    /// mirrors the `AlpacaCatalog`/`IgCatalog`/`OandaCatalog` conversions.
    #[test]
    fn catalog_resolves_credentials_from_the_injected_map() {
        let vars: std::collections::HashMap<String, String> = [
            ("CTRADER_CLIENT_ID", "cid"),
            ("CTRADER_CLIENT_SECRET", "sec"),
            ("CTRADER_DEMO_ACCESS_TOKEN", "tok"),
            ("CTRADER_DEMO_REFRESH_TOKEN", "ref"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert!(
            CtraderCatalog::new(&vars).config.is_some(),
            "injected credentials must resolve without touching process env"
        );
    }

    /// An empty map means no credentials — never a panic, and never a silent fallback to process
    /// env.
    #[test]
    fn catalog_with_no_credentials_is_configless() {
        let vars = std::collections::HashMap::new();
        assert!(CtraderCatalog::new(&vars).config.is_none());
    }
}

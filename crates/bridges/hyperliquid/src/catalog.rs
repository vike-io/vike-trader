//! Hyperliquid instrument catalog — `HyperliquidCatalog: vike_catalog::CatalogProvider`, the
//! venue's contribution to the chart's cross-venue Symbol picker. The HL twin of the Bybit/OKX
//! `*Catalog` providers: an `Enumerable` provider over the keyless `meta` (perp) + `spotMeta`
//! (spot) universe, mapped to venue+asset-class-tagged [`Instrument`]s.
//!
//! Unlike Bybit/binance — where a spot and its perp SHARE one exchange symbol (`BTCUSDT`) and the
//! perp needs a `.P` suffix to stay distinct — HL's unified symbols are already in disjoint
//! namespaces: a perp is the bare coin (`"BTC"`), a spot pair is `"BASE/QUOTE"` (`"HYPE/USDC"`), so
//! their [`Instrument::id`]s (`BTC.HYPERLIQUID` vs `HYPE/USDC.HYPERLIQUID`) never collide and no
//! suffix is applied.
//!
//! The universe is sourced through [`crate::instruments::HyperliquidInstruments::load`] (two keyless
//! `/info` reads) and its resolved [`Symbology`] — we REUSE [`InstrumentRef`] rather than re-parsing
//! the meta JSON. The pure `Symbology → Vec<Instrument>` conversion ([`instruments_from_symbology`])
//! is split from the networked load so it's fixture-testable with NO network (the `Symbology::from_meta`
//! seam), mirroring the parse/fetch split in the Bybit/OKX catalog modules. Symbology rules:
//! `docs/research/2026-07-16-hyperliquid-adapters/README.md` §5.

use vike_bridge_core::transport::VenueApiError;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider, Instrument};
use vike_model::AssetClass;

use crate::config::{Network, Product};
use crate::consts::VENUE;
use crate::instruments::HyperliquidInstruments;
use crate::symbology::{InstrumentRef, Symbology};
use crate::transport::HyperliquidTransport;

/// Every Hyperliquid perpetual is USD-denominated and USDC-margined/settled, and its unified symbol
/// drops the quote (`"BTC"`, not `"BTCUSD"`). We surface `USDC` as the catalog quote so a
/// quote-currency search still finds perps and the picker shows a settlement currency.
const PERP_QUOTE: &str = "USDC";

/// Convert one resolved [`InstrumentRef`] into a venue+asset-class-tagged catalog [`Instrument`].
/// Perp → [`AssetClass::CryptoPerp`] (`base` = the coin, `quote` = [`PERP_QUOTE`]); spot →
/// [`AssetClass::CryptoSpot`] with `base`/`quote` split positionally from the `"BASE/QUOTE"` symbol.
/// `properties` stays `Default` (the catalog is for SEARCH — the live grid is attached from
/// [`HyperliquidInstruments::properties`] at order/selection time), matching the sibling catalogs.
fn instrument_from_ref(inst: &InstrumentRef) -> Instrument {
    let (asset_class, base, quote) = match inst.product {
        Product::Perp => {
            // A HIP-3 builder perp's symbol is the dex-qualified `{dex}:{coin}`; surface the bare
            // coin as `base` for search. A core perp has no `:`, so `base` is the whole symbol —
            // byte-identical to the pre-HIP-3 mapping.
            let base = inst.symbol.rsplit_once(':').map_or(inst.symbol.as_str(), |(_, c)| c);
            (AssetClass::CryptoPerp, base.to_string(), PERP_QUOTE.to_string())
        }
        Product::Spot => {
            // Spot symbol is always `BASE/QUOTE` (built by `Symbology::load_spot`); split on `/`.
            let (base, quote) = inst.symbol.split_once('/').unwrap_or((inst.symbol.as_str(), ""));
            (AssetClass::CryptoSpot, base.to_string(), quote.to_string())
        }
    };
    Instrument {
        venue: VENUE.into(),
        raw_symbol: inst.symbol.clone(),
        asset_class,
        base,
        quote,
        // HIP-3 rows carry their deployer dex here so downstream risk/caps can branch per-dex; a
        // core perp/spot row leaves it empty (`""`) — byte-identical to the pre-HIP-3 catalog.
        description: inst.dex.clone().unwrap_or_default(),
        properties: Default::default(),
        contract_type: None,
        settle_asset: None,
    }
}

/// Map a resolved [`Symbology`] into the venue's catalog [`Instrument`]s (perps then spot, in the
/// symbology's insertion order). Pure — the fixture seam that keeps the mapping testable off a
/// synthetic `meta`/`spotMeta` via [`Symbology::from_meta`], with no network.
pub fn instruments_from_symbology(symbology: &Symbology) -> Vec<Instrument> {
    symbology.iter().map(instrument_from_ref).collect()
}

/// The Hyperliquid venue's [`CatalogProvider`] contribution: crypto spot (`BASE/QUOTE`) + crypto
/// perp (`BTC`), both bulk-enumerable off the keyless `meta` + `spotMeta` reads. Holds the target
/// [`Network`] (mainnet by default — the picker's live universe; testnet for the demo universe) and
/// whether the HIP-3 builder dexs join the universe ([`Self::with_hip3`]).
pub struct HyperliquidCatalog {
    network: Network,
    hip3: bool,
}

impl HyperliquidCatalog {
    /// A mainnet catalog — the picker's default universe, core markets only.
    pub fn new() -> Self {
        HyperliquidCatalog { network: Network::Mainnet, hip3: false }
    }

    /// A catalog over an explicit network (`Network::Testnet` for the demo universe), core markets
    /// only.
    pub fn for_network(network: Network) -> Self {
        HyperliquidCatalog { network, hip3: false }
    }

    /// Fold HIP-3 builder-dex perps into the listed universe — the caller's resolved
    /// `flags.hyperliquid_hip3` (decision 0095). Off unless set.
    #[must_use]
    pub fn with_hip3(mut self, hip3: bool) -> Self {
        self.hip3 = hip3;
        self
    }

    /// Whether this catalog lists HIP-3 builder-dex perps.
    #[must_use]
    pub fn hip3(&self) -> bool {
        self.hip3
    }

    /// [`CatalogProvider::list_instruments`]'s body over an `/info` fetch seam, so what the HIP-3
    /// switch does to the universe is observable with NO network: two keyless reads (`meta` +
    /// `spotMeta`) via the canonical universe loader, plus — with `self.hip3` — the builder dexs'
    /// `perpDexs` and per-dex `meta` reads, then the pure Symbology → Instrument conversion.
    fn list_via<F>(&self, fetch: F) -> Result<Vec<Instrument>, CatalogError>
    where
        F: Fn(&serde_json::Value) -> Result<serde_json::Value, VenueApiError>,
    {
        let universe = HyperliquidInstruments::load_from(fetch, None, self.hip3)
            .map_err(|e| CatalogError(format!("{VENUE} meta/spotMeta load: {e}")))?;
        Ok(instruments_from_symbology(universe.symbology()))
    }
}

impl Default for HyperliquidCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl CatalogProvider for HyperliquidCatalog {
    fn venue(&self) -> &str {
        VENUE
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::CryptoSpot, AssetClass::CryptoPerp]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        // Two keyless `/info` reads (meta + spotMeta) via the canonical universe loader, then the
        // pure Symbology → Instrument conversion (`list_via`, which takes the `/info` fetch as a
        // seam so a test can watch the request set). A fresh transport/gate is fine: this is a
        // one-shot startup bulk fetch, not on the exec/feed IP-budget hot path. With `self.hip3`,
        // the loader ALSO folds in builder-deployed HIP-3 perp dexs (dex-qualified `{dex}:{coin}`
        // rows, dex-tagged in `description`); off ⇒ core-only, byte-identical to the pre-HIP-3
        // catalog. The HIP-3 answer is `self.hip3`, the same flag the daemon's mount reads
        // (decision 0095: the datahub hands its resolved `flags.hyperliquid_hip3` to `with_hip3`).
        let transport = HyperliquidTransport::new(self.network);
        self.list_via(|body| transport.info(body))
    }
}

#[cfg(test)]
mod catalog_tests {
    use std::cell::RefCell;

    use super::*;
    use serde_json::json;

    /// Synthetic core `meta` (BTC/ETH perps).
    fn core_meta() -> serde_json::Value {
        json!({
            "universe": [
                {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
                {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
            ]
        })
    }

    /// Synthetic `spotMeta` (PURR/USDC & HYPE-via-`@107` spot).
    fn core_spot() -> serde_json::Value {
        json!({
            "tokens": [
                {"name": "USDC", "szDecimals": 8, "index": 0},
                {"name": "PURR", "szDecimals": 0, "index": 1},
                {"name": "HYPE", "szDecimals": 2, "index": 150}
            ],
            "universe": [
                {"name": "PURR/USDC", "tokens": [1, 0], "index": 0, "isCanonical": true},
                {"name": "@107", "tokens": [150, 0], "index": 107, "isCanonical": true}
            ]
        })
    }

    /// Synthetic `meta`/`spotMeta` mirroring the `symbology`/`instruments` fixtures — drives the
    /// catalog mapping with NO network.
    fn symbology() -> Symbology {
        Symbology::from_meta(&core_meta(), &core_spot())
    }

    #[test]
    fn every_instrument_is_venue_tagged_hyperliquid() {
        let out = instruments_from_symbology(&symbology());
        assert_eq!(out.len(), 4, "BTC + ETH perps, PURR/USDC + HYPE/USDC spot");
        assert!(out.iter().all(|i| i.venue == "hyperliquid"), "every row is venue-tagged");
    }

    #[test]
    fn perp_maps_to_cryptoperp_with_bare_symbol_and_usdc_quote() {
        let out = instruments_from_symbology(&symbology());
        let btc = out.iter().find(|i| i.raw_symbol == "BTC").expect("BTC perp");
        assert_eq!(btc.asset_class, AssetClass::CryptoPerp);
        assert_eq!(btc.base, "BTC");
        assert_eq!(btc.quote, "USDC");
        // No `.P` suffix — the bare coin is already a distinct id namespace from any spot pair.
        assert_eq!(btc.id(), "BTC.HYPERLIQUID");
    }

    #[test]
    fn spot_maps_to_cryptospot_with_positional_base_quote() {
        let out = instruments_from_symbology(&symbology());
        // `@107` resolves to the unified symbol HYPE/USDC (positional base/quote via token indices).
        let hype = out.iter().find(|i| i.raw_symbol == "HYPE/USDC").expect("HYPE/USDC spot");
        assert_eq!(hype.asset_class, AssetClass::CryptoSpot);
        assert_eq!(hype.base, "HYPE");
        assert_eq!(hype.quote, "USDC");
        assert_eq!(hype.id(), "HYPE/USDC.HYPERLIQUID");
        // PURR/USDC (the one literal-name spot pair) maps identically.
        let purr = out.iter().find(|i| i.raw_symbol == "PURR/USDC").expect("PURR/USDC spot");
        assert_eq!(purr.asset_class, AssetClass::CryptoSpot);
        assert_eq!(purr.base, "PURR");
        assert_eq!(purr.quote, "USDC");
    }

    #[test]
    fn provider_declares_enumerable_crypto_spot_and_perp() {
        let cat = HyperliquidCatalog::new();
        assert_eq!(cat.venue(), "hyperliquid");
        assert_eq!(cat.mode(), CatalogMode::Enumerable);
        assert_eq!(cat.asset_classes(), &[AssetClass::CryptoSpot, AssetClass::CryptoPerp]);
        // Enumerable ⇒ the defaulted live-search seam is empty.
        assert!(cat.search_remote("btc", &Default::default()).unwrap().is_empty());
    }

    #[test]
    fn core_rows_have_no_dex_description_byte_identical() {
        // Every pre-HIP-3 row leaves `description` empty (byte-identical to before this change).
        let out = instruments_from_symbology(&symbology());
        assert!(out.iter().all(|i| i.description.is_empty()), "no core row is dex-tagged");
        let btc = out.iter().find(|i| i.raw_symbol == "BTC").unwrap();
        assert_eq!(btc.base, "BTC", "a core perp base is the whole symbol (no `:`)");
    }

    #[test]
    fn hip3_perp_maps_with_bare_base_and_dex_tagged_description() {
        let mut s = symbology();
        // Fold a builder dex "test" (perp_dex_index 1) with one market test:ABC.
        s.extend_with_perp_dex(
            &json!({"universe": [{"name": "test:ABC", "szDecimals": 2, "maxLeverage": 10}]}),
            1,
            "test",
        );
        let out = instruments_from_symbology(&s);
        let abc = out.iter().find(|i| i.raw_symbol == "test:ABC").expect("test:ABC row");
        assert_eq!(abc.asset_class, AssetClass::CryptoPerp);
        assert_eq!(abc.base, "ABC", "the `{{dex}}:` prefix is stripped for the searchable base");
        assert_eq!(abc.quote, "USDC");
        assert_eq!(abc.description, "test", "row tagged with its deployer dex");
        // The dex-qualified raw_symbol keeps the id distinct from any core coin.
        assert_eq!(abc.id(), "test:ABC.HYPERLIQUID");
        // The core rows are still present and untagged.
        assert_eq!(out.iter().filter(|i| i.description.is_empty()).count(), 4);
    }

    /// Decision 0095: the HIP-3 universe is the CALLER's resolved `flags.hyperliquid_hip3` — the
    /// datahub passes it — and a plain construction lists the core universe only.
    ///
    /// ⚠ **Observed through the listing, not the getter.** This test used to assert `hip3()` after
    /// `with_hip3`, a setter and its getter, which stays green when [`HyperliquidCatalog::list_via`]
    /// stops passing the flag on. Here the catalog lists over a fake `/info` fetch (the seam
    /// `list_instruments` runs over the real transport) and the test reads the REQUESTS it made and
    /// the ROWS it returned: a plain catalog asks for `meta` + `spotMeta` and returns the core
    /// universe; one built with `with_hip3(true)` also asks `perpDexs` and each builder dex's
    /// `meta`, and returns the dex-tagged rows.
    #[test]
    fn the_hip3_universe_is_the_callers_choice() {
        let calls = RefCell::new(Vec::<String>::new());
        let fetch = |body: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
            let ty = body.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let dex = body.get("dex").and_then(|d| d.as_str()).map(str::to_string);
            calls.borrow_mut().push(match &dex {
                Some(d) => format!("meta:{d}"),
                None => ty.clone(),
            });
            Ok(match (ty.as_str(), dex.as_deref()) {
                ("meta", None) => core_meta(),
                ("spotMeta", _) => core_spot(),
                ("perpDexs", _) => {
                    json!([null, {"name": "test", "fullName": "test dex", "deployer": "0xabc"}])
                }
                ("meta", Some("test")) => {
                    json!({"universe": [{"name": "test:ABC", "szDecimals": 2, "maxLeverage": 10}]})
                }
                _ => json!({}),
            })
        };

        // A plain construction — and one over the testnet — is the core universe only.
        for plain in [HyperliquidCatalog::new(), HyperliquidCatalog::for_network(Network::Testnet)]
        {
            calls.borrow_mut().clear();
            let core = plain.list_via(fetch).expect("the core universe lists");
            assert_eq!(*calls.borrow(), ["meta", "spotMeta"], "OFF asks for the core reads only");
            assert_eq!(core.len(), 4, "BTC + ETH perps, PURR/USDC + HYPE/USDC spot");
            assert!(core.iter().all(|i| i.description.is_empty()), "no builder-dex row");
        }

        // The caller's choice adds the builder dexs.
        calls.borrow_mut().clear();
        let catalog = HyperliquidCatalog::new().with_hip3(true);
        assert!(catalog.hip3());
        let all = catalog.list_via(fetch).expect("the HIP-3 universe lists");
        assert_eq!(
            *calls.borrow(),
            ["meta", "spotMeta", "perpDexs", "meta:test"],
            "ON also reads the builder dexs"
        );
        assert_eq!(all.len(), 5, "the four core rows plus the builder dex's market");
        let abc = all.iter().find(|i| i.raw_symbol == "test:ABC").expect("the dex-qualified row");
        assert_eq!(abc.description, "test", "tagged with its deployer dex");

        // …and the setter is a plain switch: the same catalog turned back off lists core only.
        calls.borrow_mut().clear();
        let back_off = catalog.with_hip3(false).list_via(fetch).expect("core again");
        assert_eq!(*calls.borrow(), ["meta", "spotMeta"]);
        assert_eq!(back_off.len(), 4);
    }
}

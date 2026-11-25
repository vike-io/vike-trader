//! Hyperliquid instrument catalog — `CatalogProviderHyperliquid: vike_catalog::CatalogProvider`, the
//! venue's contribution to the chart's cross-venue Symbol picker. The HL twin of the Bybit/OKX
//! `*Catalog` providers: an `Enumerable` provider over the keyless `meta` (perp) + `spotMeta`
//! (spot) universe, mapped to [`Instrument`]s that carry a typed [`Contract`] (decision 0126):
//!
//! - a perp (core or HIP-3) is `Contract::Perp` — `Margining::Linear`, settled in `PERP_QUOTE`
//!   (`USDC`), funding every [`FUNDING_INTERVAL_MS`], no settlement clock the payload states;
//! - a spot pair is `Contract::CryptoSpot`.
//!
//! **The three identities ride on the row** (`Instrument.wire`), so no consumer parses a symbol to
//! learn them: `asset_id` = the ORDER asset id [`Symbology`] already resolved (core perp = the
//! `meta.universe` array position, delisted rows included; spot = `10000 + pairIndex`; HIP-3 =
//! `100000 + dexIndex * 10000 + index`), `coin` = the venue's WS/info string (`"BTC"`, `"@107"`,
//! `"test:ABC"`), `group` = the HIP-3 deployer dex (`None` for core perps and spot). **The grid is
//! decided once**: `properties` is `crate::instrument::properties`'s `properties_for` over the same
//! [`InstrumentRef`] the exec path derives its grid from, so identity and grid share one source.
//! HIP-4 outcome markets (`crate::execution::settlement`) are NOT catalogued here.
//!
//! Unlike Bybit/binance — where a spot and its perp SHARE one exchange symbol (`BTCUSDT`) and the
//! perp needs a `.P` suffix to stay distinct — HL's listing names are already in disjoint
//! namespaces: a perp is the bare coin (`"BTC"`), a spot pair is `"BASE/QUOTE"` (`"HYPE/USDC"`), so
//! their [`Instrument::id`]s (`BTC.HYPERLIQUID` vs `HYPE/USDC.HYPERLIQUID`) never collide and no
//! suffix is applied. A row's `raw_symbol` is that listing name, the venue's own; its USER name
//! (`BTCUSDC.P`, `HYPEUSDC`, `XYZ:TSLAUSDC.P`, decision 0290) is stored on it by the kept list's writer
//! (`vike_catalog::spelled`), never by this catalog, which lists every row with or without a kept list. A builder-dex
//! row's `quote` is its dex's collateral token (`USDC`, `USDT0`, `USDH`).
//!
//! The universe is sourced through [`crate::instrument::properties::HyperliquidInstruments::load`] (two keyless
//! `/info` reads) and its resolved [`Symbology`] — we REUSE [`InstrumentRef`] rather than re-parsing
//! the meta JSON. The pure `Symbology → Vec<Instrument>` conversion ([`instruments_from_symbology`])
//! is split from the networked load so it's fixture-testable with NO network (the `Symbology::from_meta`
//! seam), mirroring the parse/fetch split in the Bybit/OKX catalog modules. Symbology rules:
//! `docs/research/2026-07-16-hyperliquid-adapters/README.md` §5.

use vike_bridge_core::transport::VenueApiError;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{AssetClass, Contract, ContractPerp, Instrument, InstrumentVenueIds, Margining};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, VenueUrls};

use crate::config::unlisted_class;
use crate::instrument::properties::{HyperliquidInstruments, properties_for};
use crate::instrument::symbology::{InstrumentRef, Symbology, parse_perp_dexs};
use crate::refusals::HyperliquidCatalogRefusal;
use crate::transport::HyperliquidTransport;
use crate::transport::consts::{FUNDING_INTERVAL_MS, VENUE};
use vike_model::VenueNetwork;

/// Every Hyperliquid perpetual is USD-denominated and USDC-margined/settled, and its unified symbol
/// drops the quote (`"BTC"`, not `"BTCUSD"`). It is the perp's `ContractPerp::settle` and also its
/// catalog quote, so a quote-currency search still finds perps and the picker shows a settlement
/// currency. `meta` carries no per-row settle field: this is the venue's one settle coin, its
/// `vike_venues::venue_currency` row's.
const PERP_QUOTE: &str = vike_venues::venue_currency::HYPERLIQUID.settle_coins[0];

/// The venue word on the row of a DELISTED perp (`meta.universe[].isDelisted`, carried by name): the
/// gatewayhub's symbol-facts verb refuses such a coin (decision 0174). `gatewayhub`'s
/// `symbol_facts` reads the same spelling, and a feature-gated test there holds the two equal.
pub const VENUE_WORD_DELISTED: &str = "isDelisted";

/// The venue word on the row of a perp whose cross margin is unavailable (`onlyIsolated`, or a
/// `marginMode` of `strictIsolated` / `noCross`): the account's cross-margin engine cannot hold it, so
/// the symbol-facts verb refuses it by name. A delisted row says [`VENUE_WORD_DELISTED`] instead.
pub const VENUE_WORD_ISOLATED_ONLY: &str = "onlyIsolated";

/// The typed [`Contract`] for one resolved row. The class comes from the venue's own split of its
/// metadata endpoint ([`InstrumentRef::asset_class`]: `meta` vs `spotMeta`), never from the symbol text;
/// any other class is refused in words (`unlisted_class`).
fn contract_for(instrument: &InstrumentRef) -> Result<Contract, String> {
    Ok(match instrument.asset_class {
        AssetClass::CryptoPerp => Contract::Perp(ContractPerp {
            // Every HL perp is USDC-margined and quoted in USD: linear, by the venue's definition.
            margining: Margining::Linear,
            settle: PERP_QUOTE.to_string(),
            // HL sizes a perp in the base coin with no contract multiplier, so there is no
            // separate contract-value currency to state.
            value_currency: None,
            funding_interval_ms: Some(FUNDING_INTERVAL_MS),
            // `meta` publishes no next-funding / settlement timestamp.
            settling_ms: None,
        }),
        AssetClass::CryptoSpot => Contract::CryptoSpot,
        other => return Err(unlisted_class(other)),
    })
}

/// **The row the venue's kept list holds for one listed [`InstrumentRef`]**, as far as its names go: the
/// listing name as `raw_symbol`, the typed contract ([`contract_for`]) and `base`/`quote` read off the payload by
/// [`Symbology`] (a core perp's quote = [`PERP_QUOTE`], a builder-dex perp's its dex's collateral token): the start of
/// every catalog row ([`instrument_from_ref`]), what the datahub's writer names (`vike_catalog::spelled`).
fn listed_row(instrument: &InstrumentRef) -> Result<Instrument, String> {
    let quote = instrument.quote.clone().unwrap_or_else(|| PERP_QUOTE.to_string());
    Ok(Instrument::new(
        VENUE,
        instrument.raw_symbol.clone(),
        instrument.base.clone(),
        quote,
        contract_for(instrument)?,
    ))
}

/// Convert one resolved [`InstrumentRef`] into a catalog [`Instrument`]: its [`listed_row`], the three wire
/// identities, and the order-placement grid from `crate::instrument::properties`'s `properties_for` — the SAME
/// derivation the exec path uses.
fn instrument_from_ref(instrument: &InstrumentRef) -> Result<Instrument, String> {
    let row = listed_row(instrument)?
        // HIP-3 rows keep their deployer dex as the display description too; a core perp/spot row
        // leaves it empty (`""`), as before.
        .with_description(instrument.dex.clone().unwrap_or_default())
        .with_properties(properties_for(instrument)?)
        .with_wire(InstrumentVenueIds {
            asset_id: Some(u64::from(instrument.asset_id)),
            coin: Some(instrument.coin.clone()),
            group: instrument.dex.clone(),
        });
    // A row the account cannot open the plain way says so in the venue's own word; every other row
    // states none, byte-identical to before. Informational: only the symbol-facts verb reads it.
    Ok(match (instrument.delisted, instrument.only_isolated || instrument.no_cross) {
        (true, _) => row.with_venue_word(VENUE_WORD_DELISTED),
        (false, true) => row.with_venue_word(VENUE_WORD_ISOLATED_ONLY),
        (false, false) => row,
    })
}

/// Map EVERY row a [`Symbology`] lists ([`Symbology::listed`], named or not: a listing never needs a kept list, whose
/// writer names these rows) into the venue's catalog [`Instrument`]s (perps then spot, in listing order). Pure — the
/// fixture seam that keeps the mapping testable off a synthetic `meta`/`spotMeta` via [`Symbology::from_meta`], with
/// no network. A row of a class this venue lists none of (never built by [`Symbology`]) is left out, said at WARN.
pub fn instruments_from_symbology(symbology: &Symbology) -> Vec<Instrument> {
    symbology
        .listed()
        .iter()
        .filter_map(|instrument| {
            instrument_from_ref(instrument)
                .inspect_err(|why| {
                    tracing::warn!(venue = VENUE, symbol = %instrument.raw_symbol, %why, "row left out")
                })
                .ok()
        })
        .collect()
}

/// The Hyperliquid venue's [`CatalogProvider`] contribution: crypto spot (`BASE/QUOTE`) + crypto
/// perp (`BTC`), both bulk-enumerable off the keyless `meta` + `spotMeta` reads. Holds the target
/// [`VenueNetwork`] (mainnet by default — the picker's live universe; testnet for the demo universe) and
/// whether the HIP-3 builder dexs join the universe ([`Self::with_hip3`]). It reads that network's
/// `data` REST row ([`crate::config::base_url`]); a refused row is the listing's error, naming the row.
pub struct CatalogProviderHyperliquid {
    network: VenueNetwork,
    rest: Result<String, UrlRefusal>,
    hip3: bool,
}

impl CatalogProviderHyperliquid {
    /// A mainnet catalog on hyperliquid's address rows `urls` — the picker's default universe, core
    /// markets only.
    pub fn new(urls: &VenueUrls) -> Self {
        Self::for_network(VenueNetwork::Mainnet, urls)
    }

    /// A catalog over an explicit network (`VenueNetwork::Testnet` for the demo universe) on `urls`, core
    /// markets only.
    pub fn for_network(network: VenueNetwork, urls: &VenueUrls) -> Self {
        let rest = crate::config::base_url(network, urls, UrlChannel::Data, UrlProtocol::Rest)
            .map(str::to_string);
        CatalogProviderHyperliquid { network, rest, hip3: false }
    }

    /// This catalog's transport: its network on its `data` row, or the row's refusal.
    fn transport(&self) -> Result<HyperliquidTransport, CatalogError> {
        let rest = self.rest.as_ref().map_err(|e| CatalogError(e.to_string().into()))?;
        Ok(HyperliquidTransport::new(self.network, rest))
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

    /// **The rows of ONE builder dex, read with that dex's own `meta` alone** (decision 0218): the symbol-facts verb's
    /// way to answer `dex:COIN` with no dependence on the `flags.hyperliquid_hip3` universe flag (which folds EVERY
    /// dex, hundreds on the testnet). Three keyless reads on `network`: `perpDexs` (for the dex's position, which is the
    /// multiplier of its asset ids), `meta` with the `dex` param and `spotMeta` (which names the dex's collateral
    /// token, the quote of its coins' user names). `dex` is matched ignoring ASCII case, so the dex of a user name
    /// (`XYZ` of `XYZ:TSLAUSDC.P`) finds the venue's `xyz`. A dex that is not listed answers an EMPTY list, "no such
    /// symbol", never an error; a read that fails is an `Err`. `urls` are hyperliquid's address rows (the network's
    /// `data` row is read).
    pub fn dex_instruments(
        network: VenueNetwork,
        urls: &VenueUrls,
        dex: &str,
    ) -> Result<Vec<Instrument>, CatalogError> {
        let transport = Self::for_network(network, urls).transport()?;
        Self::dex_instruments_via(dex, |body| transport.info(body))
    }

    /// [`Self::dex_instruments`] over an `/info` fetch seam (no network in a test).
    pub(crate) fn dex_instruments_via<F>(
        dex: &str,
        fetch: F,
    ) -> Result<Vec<Instrument>, CatalogError>
    where
        F: Fn(&serde_json::Value) -> Result<serde_json::Value, VenueApiError>,
    {
        let failed = |what: &'static str, e: VenueApiError| listing_unread(what, &e);
        let listed = fetch(&serde_json::json!({ "type": "perpDexs" }))
            .map_err(|e| failed("perpDexs read", e))?;
        let Some(found) =
            parse_perp_dexs(&listed).into_iter().find(|d| d.name.eq_ignore_ascii_case(dex))
        else {
            return Ok(Vec::new());
        };
        let meta = fetch(&serde_json::json!({ "type": "meta", "dex": found.name }))
            .map_err(|e| failed("builder-dex meta read", e))?;
        let spot_meta = fetch(&serde_json::json!({ "type": "spotMeta" }))
            .map_err(|e| failed("spotMeta read", e))?;
        // A listing names nothing: no kept list is read.
        let mut symbology = Symbology::named_by(None);
        symbology.extend_with_perp_dex(&meta, found.index, &found.name, &spot_meta);
        Ok(instruments_from_symbology(&symbology))
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
            .map_err(|e| listing_unread("meta/spotMeta load", &e))?;
        Ok(instruments_from_symbology(universe.symbology()))
    }
}

/// A failed listing read `what`, as the catalog's error ([`HyperliquidCatalogRefusal::Unread`]).
fn listing_unread(what: &'static str, e: &VenueApiError) -> CatalogError {
    CatalogError(HyperliquidCatalogRefusal::Unread { what, why: e.to_string() }.carried())
}

impl CatalogProvider for CatalogProviderHyperliquid {
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
        // catalog. The HIP-3 answer is `self.hip3`, the same flag the daemon's start reads
        // (decision 0095: the datahub hands its resolved `flags.hyperliquid_hip3` to `with_hip3`).
        let transport = self.transport()?;
        self.list_via(|body| transport.info(body))
    }
}

#[cfg(test)]
pub(crate) mod catalog_tests {
    use std::cell::RefCell;

    use super::*;
    use serde_json::json;

    /// Synthetic core `meta` (BTC/ETH perps).
    pub(crate) fn core_meta() -> serde_json::Value {
        json!({
            "universe": [
                {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
                {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
            ]
        })
    }

    /// Synthetic `spotMeta` (PURR/USDC & HYPE-via-`@107` spot).
    pub(crate) fn core_spot() -> serde_json::Value {
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

    /// The exact contract every HL perp row carries: linear, USDC-settled, hourly funding.
    fn hl_perp() -> Contract {
        Contract::Perp(ContractPerp {
            margining: Margining::Linear,
            settle: "USDC".to_string(),
            value_currency: None,
            funding_interval_ms: Some(3_600_000),
            settling_ms: None,
        })
    }

    #[test]
    fn perp_maps_to_a_linear_usdc_perp_with_bare_symbol_and_usdc_quote() {
        let out = instruments_from_symbology(&symbology());
        let btc = out.iter().find(|i| i.raw_symbol == "BTC").expect("BTC perp");
        assert_eq!(btc.contract, hl_perp());
        assert_eq!(btc.asset_class(), AssetClass::CryptoPerp);
        assert_eq!(btc.settle_asset(), Some("USDC"));
        assert_eq!(btc.base, "BTC");
        assert_eq!(btc.quote, "USDC");
        // No `.P` suffix — the bare coin is already a distinct id namespace from any spot pair.
        assert_eq!(btc.id(), "BTC.HYPERLIQUID");
        // The three identities ride on the row: order asset id = array position, coin = name.
        assert_eq!(
            btc.wire,
            InstrumentVenueIds { asset_id: Some(0), coin: Some("BTC".to_string()), group: None }
        );
        assert_eq!(out.iter().find(|i| i.raw_symbol == "ETH").unwrap().wire.asset_id, Some(1));
        assert_eq!(btc.contract_type, None, "HL publishes no contract-type word");
    }

    /// A delisted perp still occupies its `meta.universe` slot, so the order asset id of every row
    /// after it is its ARRAY POSITION — the scheme `Symbology::load_perps` owns; the catalog only
    /// carries it.
    #[test]
    fn a_delisted_perp_keeps_its_index_and_the_rows_after_it_keep_theirs() {
        let meta = json!({"universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
            {"name": "OLD", "szDecimals": 1, "maxLeverage": 3, "isDelisted": true},
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
        ]});
        let out = instruments_from_symbology(&Symbology::from_meta(&meta, &json!({})));
        let ids: Vec<_> = out.iter().map(|i| (i.raw_symbol.as_str(), i.wire.asset_id)).collect();
        assert_eq!(ids, [("BTC", Some(0)), ("OLD", Some(1)), ("ETH", Some(2))]);
    }

    /// Identity and grid are decided once: the catalog row's `properties` IS the exec-path grid
    /// `HyperliquidInstruments` derives from the same payload, not a `Default` placeholder.
    #[test]
    fn the_catalog_grid_is_the_execution_grid() {
        let out = vike_catalog::spelled(instruments_from_symbology(&symbology()));
        let fetch = |body: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
            Ok(match body.get("type").and_then(|t| t.as_str()) {
                Some("meta") => core_meta(),
                Some("spotMeta") => core_spot(),
                _ => json!({}),
            })
        };
        let exec = HyperliquidInstruments::load_from(fetch, None, false).expect("loads");
        for row in &out {
            let user = vike_catalog::user_name_of_row(row).expect("a user name");
            assert_eq!(Some(&row.properties), exec.properties(&user), "{user}");
        }
        // BTC: szDecimals 5 → step 1e-5, tick 10^-(6-5) = 0.1, the venue's $10 floor.
        let btc = out.iter().find(|i| i.raw_symbol == "BTC").unwrap();
        assert_eq!(btc.properties.step_size, crate::instrument::properties::pow10_neg(5));
        assert_eq!(btc.properties.tick_size, crate::instrument::properties::pow10_neg(1));
        assert_eq!(btc.properties.min_notional, crate::transport::consts::MIN_NOTIONAL_USD);
        assert_eq!(btc.properties.asset_class, Some(AssetClass::CryptoPerp));
    }

    #[test]
    fn spot_maps_to_cryptospot_with_token_base_quote_and_its_wire_ids() {
        let out = instruments_from_symbology(&symbology());
        // `@107` resolves to the unified symbol HYPE/USDC (positional base/quote via token indices).
        let hype = out.iter().find(|i| i.raw_symbol == "HYPE/USDC").expect("HYPE/USDC spot");
        assert_eq!(hype.contract, Contract::CryptoSpot);
        assert_eq!(hype.asset_class(), AssetClass::CryptoSpot);
        assert_eq!(hype.base, "HYPE");
        assert_eq!(hype.quote, "USDC");
        assert_eq!(hype.id(), "HYPE/USDC.HYPERLIQUID");
        assert_eq!(
            hype.wire,
            InstrumentVenueIds {
                asset_id: Some(10_107),
                coin: Some("@107".to_string()),
                group: None
            },
            "spot asset id = 10000 + pair index; coin = the `@N` wire string"
        );
        // PURR/USDC (the one literal-name spot pair) maps identically; its coin is the literal.
        let purr = out.iter().find(|i| i.raw_symbol == "PURR/USDC").expect("PURR/USDC spot");
        assert_eq!(purr.contract, Contract::CryptoSpot);
        assert_eq!(purr.base, "PURR");
        assert_eq!(purr.quote, "USDC");
        assert_eq!(
            purr.wire,
            InstrumentVenueIds {
                asset_id: Some(10_000),
                coin: Some("PURR/USDC".to_string()),
                group: None
            }
        );
    }

    /// The release's rows (`VENUE_URL_SEED`).
    fn seed() -> VenueUrls {
        VenueUrls::seed(VENUE)
    }

    /// Each network's catalog reads that network's `data` row: mainnet's for the picker, the testnet's
    /// for a demo account's facts, byte for byte what they read before; no row is the listing's error.
    #[test]
    fn each_network_reads_its_own_data_row() {
        let main = CatalogProviderHyperliquid::new(&seed());
        assert_eq!(main.rest.as_deref(), Ok("https://api.hyperliquid.xyz"));
        let test = CatalogProviderHyperliquid::for_network(VenueNetwork::Testnet, &seed());
        assert_eq!(test.rest.as_deref(), Ok("https://api.hyperliquid-testnet.xyz"));
        let none =
            CatalogProviderHyperliquid::new(&VenueUrls::from_rows(VENUE, std::iter::empty()));
        let why = none.list_instruments().expect_err("no row");
        assert!(why.0.text().contains("no address row for live data rest"), "{}", why.0);
    }

    #[test]
    fn provider_declares_enumerable_crypto_spot_and_perp() {
        let cat = CatalogProviderHyperliquid::new(&seed());
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
        assert!(out.iter().all(|i| i.wire.group.is_none()), "no core row has a wire group");
        let btc = out.iter().find(|i| i.raw_symbol == "BTC").unwrap();
        assert_eq!(btc.base, "BTC", "a core perp base is the whole symbol (no `:`)");
    }

    #[test]
    fn hip3_perp_maps_with_bare_base_and_dex_tagged_description() {
        let mut s = symbology();
        // Fold a builder dex "test" (perp_dex_index 1) with one market test:ABC, its collateral USDC (token 0).
        s.extend_with_perp_dex(
            &json!({"universe": [{"name": "test:ABC", "szDecimals": 2, "maxLeverage": 10}],
                    "collateralToken": 0}),
            1,
            "test",
            &core_spot(),
        );
        let out = instruments_from_symbology(&s);
        let abc = out.iter().find(|i| i.raw_symbol == "test:ABC").expect("test:ABC row");
        assert_eq!(abc.contract, hl_perp(), "a HIP-3 perp is the same linear USDC perp");
        assert_eq!(abc.base, "ABC", "the `{{dex}}:` prefix is stripped for the searchable base");
        assert_eq!(abc.quote, "USDC");
        assert_eq!(abc.description, "test", "row tagged with its deployer dex");
        assert_eq!(
            abc.wire,
            InstrumentVenueIds {
                asset_id: Some(110_000),
                coin: Some("test:ABC".to_string()),
                group: Some("test".to_string()),
            },
            "HIP-3 asset id = 100000 + dexIndex*10000 + index; the dex is the wire group"
        );
        // The dex-qualified raw_symbol keeps the id distinct from any core coin.
        assert_eq!(abc.id(), "test:ABC.HYPERLIQUID");
        // The core rows are still present and untagged.
        assert_eq!(out.iter().filter(|i| i.description.is_empty()).count(), 4);
    }

    /// Decision 0095: the HIP-3 universe is the CALLER's resolved `flags.hyperliquid_hip3` — the
    /// datahub passes it — and a plain construction lists the core universe only.
    ///
    /// ⚠ **Observed through the listing, not the getter.** This test used to assert `hip3()` after
    /// `with_hip3`, a setter and its getter, which stays green when [`CatalogProviderHyperliquid::list_via`]
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
                ("meta", Some("test")) => json!({
                    "universe": [{"name": "test:ABC", "szDecimals": 2, "maxLeverage": 10}],
                    "collateralToken": 0
                }),
                _ => json!({}),
            })
        };

        // A plain construction — and one over the testnet — is the core universe only.
        for plain in [
            CatalogProviderHyperliquid::new(&seed()),
            CatalogProviderHyperliquid::for_network(VenueNetwork::Testnet, &seed()),
        ] {
            calls.borrow_mut().clear();
            let core = plain.list_via(fetch).expect("the core universe lists");
            assert_eq!(*calls.borrow(), ["meta", "spotMeta"], "OFF asks for the core reads only");
            assert_eq!(core.len(), 4, "BTC + ETH perps, PURR/USDC + HYPE/USDC spot");
            assert!(core.iter().all(|i| i.description.is_empty()), "no builder-dex row");
        }

        // The caller's choice adds the builder dexs.
        calls.borrow_mut().clear();
        let catalog = CatalogProviderHyperliquid::new(&seed()).with_hip3(true);
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

    /// **One builder dex is read ALONE** (decision 0218): `perpDexs` for its position, then its own `meta`, and the
    /// rows carry the HIP-3 asset ids of THAT position, the dex as their wire group and the isolated-only word. A
    /// dex the venue does not list answers an empty list after the one read, and a failed read is an error.
    #[test]
    fn one_builder_dex_is_read_alone_with_its_own_asset_ids() {
        let calls = RefCell::new(Vec::<String>::new());
        let fetch = |body: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
            let ty = body.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let dex = body.get("dex").and_then(|d| d.as_str()).map(str::to_string);
            calls.borrow_mut().push(match &dex {
                Some(d) => format!("meta:{d}"),
                None => ty.clone(),
            });
            Ok(match (ty.as_str(), dex.as_deref()) {
                ("perpDexs", _) => json!([
                    null,
                    {"name": "test", "fullName": "test dex", "deployer": "0xabc"},
                    {"name": "other", "fullName": "other dex", "deployer": "0xdef"}
                ]),
                ("meta", Some("other")) => json!({"universe": [
                    {"name": "other:XYZ", "szDecimals": 2, "maxLeverage": 5, "onlyIsolated": true,
                     "marginMode": "strictIsolated"},
                    {"name": "other:CROSS", "szDecimals": 1, "maxLeverage": 10}
                ], "collateralToken": 0}),
                ("spotMeta", _) => core_spot(),
                _ => json!({}),
            })
        };
        // The dex of a user name is upper case (`OTHER:XYZUSDC.P`): it finds the venue's `other`.
        let rows =
            CatalogProviderHyperliquid::dex_instruments_via("OTHER", fetch).expect("the dex lists");
        assert_eq!(
            *calls.borrow(),
            ["perpDexs", "meta:other", "spotMeta"],
            "three reads, never the whole universe"
        );
        let xyz = rows.iter().find(|i| i.raw_symbol == "other:XYZ").expect("other:XYZ");
        assert_eq!(xyz.wire.asset_id, Some(120_000), "100000 + dex position 2 * 10000 + index 0");
        assert_eq!(xyz.wire.group.as_deref(), Some("other"));
        assert_eq!(xyz.contract_type.as_deref(), Some(VENUE_WORD_ISOLATED_ONLY));
        assert_eq!(
            vike_catalog::user_name_of_row(&vike_catalog::spelled(vec![xyz.clone()])[0]).as_deref(),
            Ok("OTHER:XYZUSDC.P")
        );
        let cross = rows.iter().find(|i| i.raw_symbol == "other:CROSS").expect("other:CROSS");
        assert_eq!(cross.wire.asset_id, Some(120_001));
        assert_eq!(cross.contract_type, None, "a cross-capable dex coin states no word");

        calls.borrow_mut().clear();
        let none =
            CatalogProviderHyperliquid::dex_instruments_via("nodex", fetch).expect("no such dex");
        assert!(none.is_empty(), "a dex the venue does not list answers no rows");
        assert_eq!(*calls.borrow(), ["perpDexs"], "and reads nothing more");

        let down = |_: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
            Err(VenueApiError { code: 0, msg: "refused".to_string() })
        };
        assert!(CatalogProviderHyperliquid::dex_instruments_via("other", down).is_err());
    }

    /// A perp the account cannot trade the plain way carries the venue's own word on its row, and a
    /// plain row carries none (byte-identical to before): the gatewayhub's symbol-facts verb reads the
    /// word to refuse an isolated-only or a delisted coin by name (decision 0174).
    #[test]
    fn an_isolated_only_or_delisted_perp_carries_the_venues_word_and_a_plain_one_none() {
        let meta = json!({"universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
            {"name": "RLB", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true},
            {"name": "STRICT", "szDecimals": 0, "maxLeverage": 3, "marginMode": "strictIsolated"},
            {"name": "NOCROSS", "szDecimals": 0, "maxLeverage": 3, "marginMode": "noCross"},
            {"name": "OLD", "szDecimals": 1, "maxLeverage": 3, "isDelisted": true,
             "onlyIsolated": true}
        ]});
        let out = instruments_from_symbology(&Symbology::from_meta(&meta, &core_spot()));
        let word = |symbol: &str| {
            out.iter()
                .find(|i| i.raw_symbol == symbol)
                .unwrap_or_else(|| panic!("{symbol}"))
                .contract_type
                .as_deref()
        };
        assert_eq!(word("BTC"), None);
        assert_eq!(word("RLB"), Some(VENUE_WORD_ISOLATED_ONLY));
        assert_eq!(word("STRICT"), Some(VENUE_WORD_ISOLATED_ONLY));
        assert_eq!(word("NOCROSS"), Some(VENUE_WORD_ISOLATED_ONLY));
        assert_eq!(word("OLD"), Some(VENUE_WORD_DELISTED), "delisted outranks isolated-only");
        assert_eq!(word("HYPE/USDC"), None, "a spot pair carries no such word");
        assert_eq!((VENUE_WORD_DELISTED, VENUE_WORD_ISOLATED_ONLY), ("isDelisted", "onlyIsolated"));
    }

    /// The shared spot row (`vike_venues::spot_pair_row`: what the fee lane and `vike_catalog::names_spot_pair` read a
    /// name by) spells a spot pair as THIS bridge's kept rows name one: every row's user name reads as a spot pair
    /// exactly when its row is one, and as a derivative exactly when its row is a perpetual (decision 0290).
    #[test]
    fn the_shared_spot_row_reads_every_kept_rows_user_name_as_its_row_is() {
        let row = vike_venues::spot_pair_row(VENUE).expect("hyperliquid states its spot spelling");
        for kept in crate::test_support::kept_rows() {
            let user = vike_catalog::user_name_of_row(&kept).expect("a user name");
            let spot = matches!(kept.contract, Contract::CryptoSpot);
            assert_eq!(row.spelling.names_spot(&user), spot, "{user}");
            assert_eq!(row.spelling.names_derivative(&user), !spot, "{user}");
            assert_eq!(vike_catalog::names_spot_pair(VENUE, &user), spot, "{user}");
        }
        assert_eq!(vike_venues::fee_lane(VENUE, "HYPEUSDC"), row.fee_lane);
    }

    /// **A builder-dex coin's user name ends in its dex's collateral token** (decision 0290, the owner's "1.A"): the
    /// dex's `collateralToken` named through `spotMeta`'s token index is the row's `quote`, so `xyz:TSLA` on a USDC
    /// dex is `XYZ:TSLAUSDC.P` and `cash:TSLA` on a USDT0 dex `CASH:TSLAUSDT0.P`; the wire stays the venue's coin. A
    /// dex whose collateral token no `spotMeta` row names lists no coin: its user names cannot be spelled.
    #[test]
    fn a_builder_dex_coin_is_named_by_its_dexs_collateral_token() {
        let spot = json!({"tokens": [
            {"name": "USDC", "szDecimals": 8, "index": 0},
            {"name": "USDT0", "szDecimals": 2, "index": 268},
            {"name": "USDH", "szDecimals": 2, "index": 360}
        ], "universe": []});
        let dex = |name: &str, collateral: serde_json::Value| {
            json!({"universe": [{"name": format!("{name}:TSLA"), "szDecimals": 3, "maxLeverage": 20}],
                   "collateralToken": collateral})
        };
        // Named by the test kept list (`crate::test_support::kept_rows`, which keeps `xyz:TSLA` and `cash:TSLA`).
        let mut s = Symbology::from_meta(&json!({}), &spot);
        s.extend_with_perp_dex(&dex("xyz", json!(0)), 1, "xyz", &spot);
        s.extend_with_perp_dex(&dex("cash", json!(268)), 6, "cash", &spot);
        s.extend_with_perp_dex(&dex("gone", json!(999)), 7, "gone", &spot);
        let out = vike_catalog::spelled(instruments_from_symbology(&s));
        let names: Vec<(String, String, Option<String>)> = out
            .iter()
            .map(|row| {
                let names = vike_catalog::names_of_row(row).expect("a user name");
                (names.user.clone(), row.quote.clone(), row.wire.coin.clone())
            })
            .collect();
        assert_eq!(
            names,
            [
                ("XYZ:TSLAUSDC.P".to_string(), "USDC".to_string(), Some("xyz:TSLA".to_string())),
                (
                    "CASH:TSLAUSDT0.P".to_string(),
                    "USDT0".to_string(),
                    Some("cash:TSLA".to_string())
                ),
            ],
            "the dex whose collateral token spotMeta does not name lists no coin"
        );
        let xyz = s.by_symbol("XYZ:TSLAUSDC.P").expect("held under its user name");
        assert_eq!((xyz.coin.as_str(), xyz.asset_id), ("xyz:TSLA", 110_000));
        assert_eq!(s.by_coin("cash:TSLA").map(|i| i.symbol.as_str()), Some("CASH:TSLAUSDT0.P"));
        assert_eq!(
            vike_paths::store_path::store_name_of_user_name("XYZ:TSLAUSDC.P"),
            "XYZ-TSLAUSDC.P",
            "the store writes the dex's `:` as `-`"
        );
    }
}

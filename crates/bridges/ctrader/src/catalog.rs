//! FX + CFD instrument catalog — the cTrader twin of `crates/bridges/oanda/src/catalog.rs`.
//! cTrader has NO REST symbol-list endpoint: the symbol universe is enumerated over the Open API
//! protobuf SESSION (`ProtoOASymbolsListReq`, sent during the connect handshake — see
//! `conn::open_and_handshake`). So `CtraderCatalog` holds an `Option<CtraderConfig>` (the OAuth
//! token + account credential gate, resolved from the CALLER-supplied account credentials via
//! `config::CtraderConfig::from_credentials` — the settings-registry convention: libraries take
//! configuration as parameters, only binaries read the process env or the credential store) rather
//! than being a zero-arg unit struct, and `list_instruments` returns `Ok(vec![])` (never an error)
//! when credentials are absent — mirroring the bridge-wide "absent credentials is the live gate"
//! rule.
//!
//! The one-shot fetch ([`conn::fetch_symbols`]) runs the FULL handshake and returns just the built
//! `SymbolMap`, dropping the socket without ever spawning the live data/exec actor thread. The pure
//! [`kind_of`] / [`contract_for`] / [`instruments_from_map`] mapping is split from that authed
//! session fetch so it stays testable without network or a token — mirrors every other venue's
//! catalog module.
//!
//! # What is READ
//!
//! Beside the symbols, the handshake reads the broker's three reference lists (symbol categories,
//! asset classes, assets — `crate::symbols::SymbolLists`). From them and the full `ProtoOASymbol`
//! each row READS:
//!
//! - **its class**: the light symbol's `symbol_category_id` -> that category's `asset_class_id` ->
//!   the asset class NAME, through [`ASSET_CLASS_KEYWORDS`] ([`kind_of`]). A forex class is
//!   `Contract::Fx`; every other row is `Contract::Cfd` with the `CfdUnderlying` its row names.
//!   The category's OWN name is not consulted: on the captured demo broker every one of the eleven
//!   categories is named `Default Category`
//!   (`crates/bridges/ctrader/tests/fixtures/ctrader_catalog_capture.json`), so the asset class is
//!   the only name that says what a symbol is;
//! - **base / quote**: the asset NAMES behind `base_asset_id` / `quote_asset_id` (`EURUSD` ->
//!   `EUR`/`USD`, `AUS200` -> `AUS200`/`AUD`, `BTCUSD` -> `BTC`/`USD`);
//! - the pip (`10^-pip_position`), the lot in base units (`lot_size / 100`, centi-units descaled),
//!   the long/short swap, and — through `crate::symbols::SymbolMap::risk_properties` —
//!   tick/step/min/max into `SymbolProperties`.
//!
//! # What stays unknown, and is NOT guessed
//!
//! - A symbol whose category or asset class the lists do not resolve, or whose asset class name
//!   matches no [`ASSET_CLASS_KEYWORDS`] row, is `Cfd { underlying_kind: Other }`. Its NAME is never
//!   read for a class: the heuristic that used to do that (`classify`: six uppercase letters =>
//!   FX) called `BTCUSD` an FX pair and is deleted, with no fallback to it.
//! - A base/quote asset the asset list does not name is the empty string.
//! - A CFD's `expiry` is `Unknown`: `ProtoOASymbol` has no expiry field, and the dated futures CFDs
//!   the demo broker lists (`DXY_H6`, `Wheat_H6`, `EURBBL_H6`) carry their month only in the
//!   symbol NAME and description. `underlying` and `margin_rate` are `None` for the same reason.
//! - A row whose id the `SymbolById` answer did not cover (the light-symbol-only path) keeps its
//!   class and base/quote but every detail `None` and `SymbolProperties::default()`.
//!
//! `raw_symbol` and `description` are both the name verbatim. `wire` stays empty on purpose: a
//! cTrader `symbol_id` is per SERVER (demo and live differ), and this catalog is fetched on the
//! demo tier, so storing it would hand a live consumer the wrong id.

use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{
    AccountTier, AssetClass, CfdExpiry, CfdProduct, CfdSpec, CfdUnderlying, Contract, FxSpec,
    Instrument,
};
use vike_secrets::account_credentials::AccountCredentials;

use crate::config::CtraderConfig;
use crate::conn;
use crate::symbols::{SymbolMap, SymbolTerms, VolumeGrid};

/// What a cTrader symbol IS, as its broker asset class names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    /// A forex pair: `Contract::Fx`.
    Forex,
    /// Anything else the broker lists: `Contract::Cfd` on this underlying.
    Cfd(CfdUnderlying),
}

impl SymbolKind {
    /// The stored class word this kind projects to: `Fx` for a forex pair, `Cfd` otherwise.
    pub fn asset_class(self) -> AssetClass {
        match self {
            SymbolKind::Forex => AssetClass::Fx,
            SymbolKind::Cfd(_) => AssetClass::Cfd,
        }
    }
}

/// The asset-class keyword table: `(lowercase substring of the asset class NAME, kind)`, FIRST
/// match wins, so ORDER MATTERS. Every row is backed by a name the demo broker really published
/// (captured by `crates/bridges/ctrader/tests/ctrader_catalog_smoke.rs` on 2026-10-11, committed as
/// `crates/bridges/ctrader/tests/fixtures/ctrader_catalog_capture.json`); the comment names the
/// captured class and some of its symbols. A name no row matches is `Cfd(Other)` ([`kind_of`]).
pub const ASSET_CLASS_KEYWORDS: &[(&str, SymbolKind)] = &[
    // `Forex` (class 1, 69 symbols: EURUSD, GBPUSD, USDJPY, EURJPY).
    ("forex", SymbolKind::Forex),
    // `Metals` (class 2, 24 symbols: XAUUSD, XAGUSD, XAUEUR, XPDUSD).
    ("metal", SymbolKind::Cfd(CfdUnderlying::Metal)),
    // `Indices` (class 3, 31 symbols: AUS200, STOXX50, F40, DE30, HK50).
    ("indices", SymbolKind::Cfd(CfdUnderlying::Index)),
    // `Oil` (class 4, 6 symbols: BRENT, WTI, XBRUSD, XTIUSD, USCrude100).
    ("oil", SymbolKind::Cfd(CfdUnderlying::Commodity)),
    // `EU Equities (CFDs)` and `US Equities (CFDs)` (classes 5 and 6, NO symbols on the demo
    // account — listed by the broker all the same).
    ("equities", SymbolKind::Cfd(CfdUnderlying::Share)),
    // `Commodities` (class 7: XNGUSD) and `Futures Commodities` (class 11, 64 symbols: Wheat_H6,
    // Wheat_U6). Before the `futures` row so the second lands here.
    ("commodit", SymbolKind::Cfd(CfdUnderlying::Commodity)),
    // `Cryptocurrencies` (class 8, 105 symbols: BTCUSD, ETHUSD, BCHUSD, LTCUSD, ALGOUSD).
    ("crypto", SymbolKind::Cfd(CfdUnderlying::Crypto)),
    // `Bonds` (class 10, 36 symbols: EURBBL_H6 "Euro Bobl -March 26 CFD", EURSCA_H6).
    ("bond", SymbolKind::Cfd(CfdUnderlying::Bond)),
    // `Futures` (class 9, 16 symbols: DXY_H6 "US Dollar Index -March 26 CFD", VIX_F6). The name
    // says the WRAPPER (a dated futures CFD), not what it tracks, so it is `Other` on purpose: the
    // two captured underlyings happen to be indices, but nothing in the payload says the next one
    // the broker adds to this class will be.
    ("futures", SymbolKind::Cfd(CfdUnderlying::Other)),
];

/// The [`ASSET_CLASS_KEYWORDS`] row an asset class NAME matches: the first whose keyword the
/// lowercased name contains, or `None` when no row does.
pub fn listed_kind(asset_class: &str) -> Option<SymbolKind> {
    let name = asset_class.to_ascii_lowercase();
    ASSET_CLASS_KEYWORDS.iter().find(|(keyword, _)| name.contains(keyword)).map(|(_, kind)| *kind)
}

/// The [`SymbolKind`] for a symbol whose asset class is named `asset_class` (resolved through its
/// category, `crate::symbols::SymbolMap::category`): its [`listed_kind`] row. `None` (no category,
/// a category the list did not carry, or an asset class without a name) and a name no row matches
/// are both `Cfd(Other)` — the honest "the broker did not say", never a guess from the symbol's
/// name.
pub fn kind_of(asset_class: Option<&str>) -> SymbolKind {
    asset_class.and_then(listed_kind).unwrap_or(SymbolKind::Cfd(CfdUnderlying::Other))
}

/// The [`Contract`] for a symbol of kind `kind` (from [`kind_of`]): `Fx` for
/// [`SymbolKind::Forex`], `Cfd` on the named underlying otherwise. `terms`/`grid` are the
/// full-entity fields when the handshake's `SymbolById` covered the id; `None` leaves every detail
/// `None` (the honest light-symbol-only answer). Swaps are carried in the venue's
/// `swap_calculation_type` unit (pips by default) — see `crate::symbols::SymbolTerms`.
pub fn contract_for(
    kind: SymbolKind,
    terms: Option<SymbolTerms>,
    grid: Option<VolumeGrid>,
) -> Contract {
    let swap_long = terms.and_then(|t| t.swap_long);
    let swap_short = terms.and_then(|t| t.swap_short);
    match kind {
        SymbolKind::Forex => Contract::Fx(FxSpec {
            pip_size: terms.and_then(|t| t.pip_size()),
            lot_size: grid.and_then(|g| g.lot_units()),
            swap_long,
            swap_short,
        }),
        SymbolKind::Cfd(underlying_kind) => Contract::Cfd(CfdSpec {
            underlying_kind,
            underlying: None,
            // cTrader brokers list CFDs; there is no spread-bet product on this API.
            product: CfdProduct::Cfd,
            // `ProtoOASymbol` has no expiry field: a rolling CFD and a dated one look alike.
            expiry: CfdExpiry::Unknown,
            margin_rate: None,
            swap_long,
            swap_short,
        }),
    }
}

/// Map every named symbol of a handshake-built [`SymbolMap`] into a universal [`Instrument`]:
/// [`kind_of`] over the symbol's asset class for the class, the asset names for base/quote,
/// [`contract_for`] over the full-entity fields, and `SymbolMap::risk_properties` as the row's
/// `SymbolProperties` when the full entity is known.
pub fn instruments_from_map(map: &SymbolMap) -> Vec<Instrument> {
    let mut ids = map.ids();
    ids.sort_unstable();
    ids.into_iter()
        .filter_map(|id| {
            let name = map.name_of(id).filter(|n| !n.is_empty())?;
            let kind = kind_of(map.category(id).and_then(|c| c.asset_class));
            let base = map.base_asset(id).unwrap_or_default();
            let quote = map.quote_asset(id).unwrap_or_default();
            let grid = map.volume_grid(id);
            let contract = contract_for(kind, map.terms(id), grid);
            let properties =
                grid.is_some().then(|| map.risk_properties(name)).flatten().unwrap_or_default();
            Some(
                Instrument::new("ctrader", name, base, quote, contract)
                    .with_description(name)
                    .with_properties(properties),
            )
        })
        .collect()
}

/// **One account's listing for the any-symbol facts**: its deposit currency and its instrument rows,
/// both off the SAME handshake-built `map` (so the two cannot come from different sessions). `Err`
/// when the broker's asset list does not name the account's deposit asset: the currency fence needs it,
/// and a currency nobody could name is never read as "any currency will do".
pub fn listing_of(map: &SymbolMap) -> Result<(String, Vec<Instrument>), CatalogError> {
    let deposit = map.deposit_currency().ok_or_else(|| {
        CatalogError(
            "ctrader: the broker's asset list does not name this account's deposit currency, so a \
             quote currency cannot be checked against it"
                .into(),
        )
    })?;
    Ok((deposit.to_string(), instruments_from_map(map)))
}

/// **Why an instrument may not be admitted on demand** into an engine whose account deposits in
/// `deposit`, or `None` when it may. The engine's ledger is ONE currency and values an order as
/// `qty x price` (decision 0170; `docs/ops/any-symbol-remaining-venues-plan.md`, blocker 1), so an
/// instrument is admitted only where that product IS the real exposure:
///
/// * **a forex pair only.** What one unit of volume means for a CFD (an index, a metal, a share, a
///   crypto: its contract size) is asserted in `crate::symbols::VolumeGrid`'s doc and never measured
///   against a demo fill, so a CFD is refused by name rather than valued on a guess. A forex pair's
///   volume is `units x 100` (live-verified for EURUSD).
/// * **quoted in the account's deposit currency.** Nothing in the engine converts a quote currency:
///   USDJPY on a USD account would book 100k units at 150 as about 15 million "USD" of notional and a
///   0.10 yen move as 10,000 "USD" of PnL against about 66 real, and an overstated PnL feeds the
///   margin-call sweep and the drawdown latch. A broker that names no quote asset is refused too.
/// * **a stated minimum volume.** The adapter snaps every volume onto the symbol's step and refuses one
///   below its minimum; with no minimum published the engine could not judge a sub-minimum order, so
///   the instrument is not admitted. (A missing step or tick is refused by the facts' own generic
///   check, which runs first.)
///
/// The mounted symbol's start is not touched by any of this.
pub fn any_symbol_refusal(row: &Instrument, deposit: &str) -> Option<String> {
    let name = &row.raw_symbol;
    if !matches!(row.contract, Contract::Fx(_)) {
        return Some(format!(
            "ctrader: {name} is a CFD, not a forex pair, and this adapter admits forex pairs only on \
             demand: what one unit of volume means for a CFD (its contract size) has not been measured, \
             so a position in it would be valued on a guess"
        ));
    }
    let quote = row.quote.trim();
    if quote.is_empty() {
        return Some(format!(
            "ctrader: {name} names no quote currency, so it cannot be checked against this account's \
             currency ({deposit})"
        ));
    }
    if !quote.eq_ignore_ascii_case(deposit) {
        return Some(format!(
            "ctrader: {name} is quoted in {quote} but this account's currency is {deposit}, and the \
             engine books money in ONE currency (nothing converts a quote currency), so a position in \
             it would be mis-valued; only instruments quoted in {deposit} are admitted"
        ));
    }
    if !(row.properties.min_qty.is_finite() && row.properties.min_qty > 0.0) {
        return Some(format!(
            "ctrader: {name}: the broker publishes no minimum volume for it (missing or zero), so an \
             order below the minimum could not be judged"
        ));
    }
    None
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
    /// the caller-supplied credentials of ONE account — the composition root's store read, NOT the
    /// process environment (which never holds these credentials in this repo). `None` when
    /// credentials are absent — the live gate. Mirrors `AlpacaCatalog`/`IgCatalog`/`OandaCatalog`'s
    /// conversion.
    pub fn new(credentials: &AccountCredentials<'_>) -> Self {
        Self { config: CtraderConfig::from_credentials(AccountTier::Demo, credentials) }
    }

    /// **The account's own listing for the any-symbol facts**: its deposit currency and its instrument
    /// rows, from ONE one-shot handshake ([`conn::fetch_symbol_lists`]; the shared grant is followed,
    /// never spent). Unlike [`CatalogProvider::list_instruments`] this ERRORS when the account holds no
    /// credentials: an unknown currency must never read as "no fence needed". The caller bounds the cost
    /// (the gatewayhub's `AccountFacts`: one listing per ten minutes, single-flight, a failure
    /// remembered), because each call is a full authenticated session beside the exec, data and
    /// reconcile sockets.
    pub fn account_listing(&self) -> Result<(String, Vec<Instrument>), CatalogError> {
        let Some(config) = &self.config else {
            return Err(CatalogError(
                "ctrader: this account holds no usable credentials (the OAuth grant and the \
                 application pair)"
                    .into(),
            ));
        };
        let lists = conn::fetch_symbol_lists(config.to_conn_config())
            .map_err(|e| CatalogError(format!("ctrader symbols: {e}")))?;
        listing_of(&SymbolMap::from_lists(&lists))
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
        Ok(instruments_from_map(&symbols))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use crate::proto::{
        ProtoOaAsset, ProtoOaAssetClass, ProtoOaLightSymbol, ProtoOaSymbol, ProtoOaSymbolCategory,
    };
    use crate::symbols::SymbolLists;

    /// Every captured asset class name lands on the kind its table row documents, and the old
    /// heuristic's victims do not: the class comes from the broker's word, not the ticker.
    #[test]
    fn kind_of_maps_every_captured_asset_class_name() {
        let cfd = SymbolKind::Cfd;
        for (name, want) in [
            ("Forex", SymbolKind::Forex),
            ("Metals", cfd(CfdUnderlying::Metal)),
            ("Indices", cfd(CfdUnderlying::Index)),
            ("Oil", cfd(CfdUnderlying::Commodity)),
            ("EU Equities (CFDs)", cfd(CfdUnderlying::Share)),
            ("US Equities (CFDs)", cfd(CfdUnderlying::Share)),
            ("Commodities", cfd(CfdUnderlying::Commodity)),
            ("Cryptocurrencies", cfd(CfdUnderlying::Crypto)),
            ("Futures", cfd(CfdUnderlying::Other)),
            ("Bonds", cfd(CfdUnderlying::Bond)),
            ("Futures Commodities", cfd(CfdUnderlying::Commodity)),
        ] {
            assert_eq!(kind_of(Some(name)), want, "asset class {name:?}");
        }
    }

    /// No class, or a class no row names, is `Cfd(Other)` — never a guess.
    #[test]
    fn kind_of_an_unknown_or_absent_class_is_cfd_other() {
        let other = SymbolKind::Cfd(CfdUnderlying::Other);
        assert_eq!(kind_of(None), other);
        assert_eq!(kind_of(Some("")), other);
        assert_eq!(kind_of(Some("Default Category")), other);
        assert_eq!(kind_of(Some("Something New")), other);
    }

    fn light(
        id: i64,
        name: &str,
        category: Option<i64>,
        base: i64,
        quote: i64,
    ) -> ProtoOaLightSymbol {
        ProtoOaLightSymbol {
            symbol_id: id,
            symbol_name: Some(name.to_string()),
            symbol_category_id: category,
            base_asset_id: Some(base),
            quote_asset_id: Some(quote),
            ..Default::default()
        }
    }

    /// The handshake's lists for EURUSD (the live-verified demo grid of
    /// `crate::symbols::VolumeGrid`'s doc, plus its pip position and a pips swap), US500 in an
    /// `Indices` class, a GBPJPY the `SymbolById` answer did not cover, a BTCUSD in a
    /// `Cryptocurrencies` class, and an XYZ whose category the list does not carry.
    fn fixture_map() -> SymbolMap {
        let category = |id, asset_class_id| ProtoOaSymbolCategory {
            id,
            asset_class_id,
            name: "Default Category".to_string(),
            ..Default::default()
        };
        let class = |id, name: &str| ProtoOaAssetClass {
            id: Some(id),
            name: Some(name.to_string()),
            ..Default::default()
        };
        let asset = |asset_id, name: &str| ProtoOaAsset {
            asset_id,
            name: name.to_string(),
            ..Default::default()
        };
        let lists = SymbolLists {
            light: vec![
                light(1, "EURUSD", Some(10), 100, 101),
                light(2, "US500", Some(30), 300, 101),
                light(3, "GBPJPY", Some(10), 102, 103),
                light(4, "", Some(10), 100, 101),
                light(5, "BTCUSD", Some(80), 800, 101),
                light(6, "XYZ", Some(99), 900, 101),
            ],
            full: vec![
                ProtoOaSymbol {
                    symbol_id: 1,
                    digits: 5,
                    pip_position: 4,
                    swap_long: Some(-7.31),
                    swap_short: Some(2.12),
                    swap_calculation_type: Some(0),
                    lot_size: Some(10_000_000),
                    min_volume: Some(100_000),
                    step_volume: Some(100_000),
                    max_volume: Some(10_000_000_000),
                    ..Default::default()
                },
                ProtoOaSymbol {
                    symbol_id: 2,
                    digits: 1,
                    pip_position: 0,
                    swap_long: Some(-4.5),
                    swap_short: Some(-1.25),
                    lot_size: Some(100),
                    min_volume: Some(10),
                    step_volume: Some(10),
                    max_volume: Some(1_000_000),
                    ..Default::default()
                },
            ],
            categories: vec![category(10, 1), category(30, 3), category(80, 8)],
            asset_classes: vec![
                class(1, "Forex"),
                class(3, "Indices"),
                class(8, "Cryptocurrencies"),
            ],
            assets: vec![
                asset(100, "EUR"),
                asset(101, "USD"),
                asset(102, "GBP"),
                asset(103, "JPY"),
                asset(300, "US500"),
                asset(800, "BTC"),
            ],
            deposit_asset_id: Some(101),
        };
        SymbolMap::from_lists(&lists)
    }

    fn find<'a>(out: &'a [Instrument], name: &str) -> &'a Instrument {
        out.iter().find(|i| i.raw_symbol == name).unwrap_or_else(|| panic!("{name} present"))
    }

    #[test]
    fn full_entity_fx_reads_pip_lot_swap_grid_and_asset_names() {
        let out = instruments_from_map(&fixture_map());
        assert_eq!(out.len(), 5, "the empty name is skipped");

        let eurusd = find(&out, "EURUSD");
        assert_eq!(eurusd.venue, "ctrader");
        assert_eq!(eurusd.asset_class(), AssetClass::Fx);
        assert_eq!((eurusd.base.as_str(), eurusd.quote.as_str()), ("EUR", "USD"));
        assert_eq!(eurusd.description, "EURUSD");
        assert_eq!(
            eurusd.contract,
            Contract::Fx(FxSpec {
                pip_size: Some(0.0001),
                lot_size: Some(100_000.0),
                swap_long: Some(-7.31),
                swap_short: Some(2.12),
            })
        );
        assert_eq!(eurusd.properties.tick_size, 1.0 / 100_000.0);
        assert_eq!(eurusd.properties.step_size, 1000.0);
        assert_eq!(eurusd.properties.min_qty, 1000.0);
        assert_eq!(eurusd.properties.max_qty, 100_000_000.0);
        assert_eq!(eurusd.properties.asset_class, Some(AssetClass::Fx), "read from `Forex`");
    }

    #[test]
    fn full_entity_cfd_reads_its_underlying_from_the_asset_class() {
        let out = instruments_from_map(&fixture_map());
        let us500 = find(&out, "US500");
        assert_eq!(us500.asset_class(), AssetClass::Cfd);
        assert_eq!((us500.base.as_str(), us500.quote.as_str()), ("US500", "USD"));
        assert_eq!(
            us500.contract,
            Contract::Cfd(CfdSpec {
                underlying_kind: CfdUnderlying::Index,
                underlying: None,
                product: CfdProduct::Cfd,
                expiry: CfdExpiry::Unknown,
                margin_rate: None,
                swap_long: Some(-4.5),
                swap_short: Some(-1.25),
            })
        );
        assert_eq!(us500.properties.tick_size, 0.1);
        assert_eq!(us500.properties.step_size, 0.1);
        assert_eq!(us500.properties.max_qty, 10_000.0);
    }

    /// The old name heuristic's KNOWN misclassification is gone: a six-uppercase-letter crypto
    /// ticker in a `Cryptocurrencies` class is a crypto CFD, not an FX pair.
    #[test]
    fn btcusd_is_a_crypto_cfd_not_an_fx_pair() {
        let out = instruments_from_map(&fixture_map());
        let btc = find(&out, "BTCUSD");
        assert_eq!((btc.base.as_str(), btc.quote.as_str()), ("BTC", "USD"));
        assert_eq!(contract_for(SymbolKind::Cfd(CfdUnderlying::Crypto), None, None), btc.contract);
    }

    /// A light symbol the `SymbolById` answer did not cover keeps its class and base/quote, with
    /// every detail `None` and no grid; a symbol whose category the list does not carry is
    /// `Cfd(Other)` with no asset it cannot name.
    #[test]
    fn light_only_and_uncategorised_symbols_say_only_what_was_read() {
        let out = instruments_from_map(&fixture_map());
        let gbpjpy = find(&out, "GBPJPY");
        assert_eq!((gbpjpy.base.as_str(), gbpjpy.quote.as_str()), ("GBP", "JPY"));
        assert_eq!(
            gbpjpy.contract,
            Contract::Fx(FxSpec {
                pip_size: None,
                lot_size: None,
                swap_long: None,
                swap_short: None
            })
        );
        assert_eq!(gbpjpy.properties, vike_model::SymbolProperties::default());

        let xyz = find(&out, "XYZ");
        assert_eq!((xyz.base.as_str(), xyz.quote.as_str()), ("", "USD"), "asset 900 is unnamed");
        assert_eq!(
            xyz.contract,
            Contract::Cfd(CfdSpec {
                underlying_kind: CfdUnderlying::Other,
                underlying: None,
                product: CfdProduct::Cfd,
                expiry: CfdExpiry::Unknown,
                margin_rate: None,
                swap_long: None,
                swap_short: None,
            })
        );
    }

    /// The account's listing is its deposit currency and its rows off ONE map; a map whose deposit asset
    /// the asset list does not name has NO listing (the currency fence cannot run, so it must not pass).
    #[test]
    fn the_listing_carries_the_deposit_currency_or_is_an_error() {
        let (deposit, rows) = listing_of(&fixture_map()).expect("a named deposit asset");
        assert_eq!(deposit, "USD");
        assert_eq!(rows.len(), 5);
        let none = SymbolMap::from_symbols(&[light(1, "EURUSD", None, 1, 2)], &[]);
        let why = listing_of(&none).expect_err("no deposit currency named");
        assert!(why.0.contains("deposit currency"), "{}", why.0);
        let why = CtraderCatalog::new(&AccountCredentials::none())
            .account_listing()
            .expect_err("no credentials");
        assert!(why.0.contains("no usable credentials"), "{}", why.0);
    }

    /// The any-symbol fence: a forex pair quoted in the deposit currency passes; a CFD, another quote
    /// currency, a quote nobody named and a missing minimum volume are each refused, BY NAME.
    #[test]
    fn the_any_symbol_fence_admits_a_deposit_quoted_forex_pair_and_names_every_refusal() {
        let out = instruments_from_map(&fixture_map());
        let eurusd = find(&out, "EURUSD").clone();
        assert_eq!(any_symbol_refusal(&eurusd, "USD"), None);
        assert_eq!(any_symbol_refusal(&eurusd, "usd"), None, "currency names compare without case");

        let cfd = any_symbol_refusal(find(&out, "US500"), "USD").expect("a CFD is refused");
        assert!(cfd.contains("US500 is a CFD") && cfd.contains("not been measured"), "{cfd}");
        let crypto = any_symbol_refusal(find(&out, "BTCUSD"), "USD").expect("a crypto CFD");
        assert!(crypto.contains("is a CFD"), "{crypto}");

        // Another quote currency: EURUSD on a EUR account, GBPJPY on a USD account.
        let eur = any_symbol_refusal(&eurusd, "EUR").expect("quoted in USD, account in EUR");
        assert!(eur.contains("quoted in USD") && eur.contains("currency is EUR"), "{eur}");
        let jpy = any_symbol_refusal(find(&out, "GBPJPY"), "USD").expect("a JPY quote");
        assert!(jpy.contains("quoted in JPY") && jpy.contains("only instruments quoted in USD"));

        let mut unnamed = eurusd.clone();
        unnamed.quote = String::new();
        let why = any_symbol_refusal(&unnamed, "USD").expect("no quote asset named");
        assert!(why.contains("names no quote currency"), "{why}");

        let mut no_min = eurusd;
        no_min.properties.min_qty = 0.0;
        let why = any_symbol_refusal(&no_min, "USD").expect("no minimum volume");
        assert!(why.contains("no minimum volume"), "{why}");
    }

    /// The catalog must read the credentials it is HANDED, not the process environment.
    /// `new()` used to sweep the workspace `.env` itself (`CtraderConfig::from_env`) — a library
    /// doing its own I/O the caller could neither see nor override (settings-registry rule);
    /// mirrors the `AlpacaCatalog`/`IgCatalog`/`OandaCatalog` conversions.
    #[test]
    fn catalog_resolves_credentials_from_the_handed_account() {
        use vike_secrets::account_credentials::CredentialFields;
        let account =
            CredentialFields::from_pairs([("ACCESS_TOKEN", "tok"), ("REFRESH_TOKEN", "ref")]);
        let venue = CredentialFields::from_pairs([("CLIENT_ID", "cid"), ("CLIENT_SECRET", "sec")]);
        let credentials = AccountCredentials::new(Some(1), Some(&account), Some(&venue));
        assert!(
            CtraderCatalog::new(&credentials).config.is_some(),
            "handed credentials must resolve without touching process env"
        );
    }

    /// No credentials — never a panic, and never a silent fallback to process env.
    #[test]
    fn catalog_with_no_credentials_is_configless() {
        assert!(CtraderCatalog::new(&AccountCredentials::none()).config.is_none());
    }
}

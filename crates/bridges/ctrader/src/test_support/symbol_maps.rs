//! Hand-built `SymbolMap`s the mapper tests descale against, the unit tests in
//! `src/feed/decode_tests.rs` and the offline suite `tests/offline/data_mapper.rs` alike, and the KEPT list every
//! test of this crate that names an instrument holds ([`kept_list_for_tests`], decision 0290).

use vike_model::Instrument;

use crate::instrument::symbols::{SymbolLists, SymbolMap};
use crate::transport::proto::{
    ProtoOaAsset, ProtoOaAssetClass, ProtoOaLightSymbol, ProtoOaSymbol, ProtoOaSymbolCategory,
};

/// EURUSD id=1, digits=5 (scale 1e5) — mirrors the fake server's scripted `SymbolById`.
pub fn eurusd_symbol_map() -> SymbolMap {
    one_symbol_map(1, "EURUSD", 5)
}

/// USDJPY id=4, digits=3 — the live-verified regression fixture. cTrader's relative price scale
/// is a FIXED 1e5 for every symbol regardless of `digits`; a `digits=3` symbol is exactly the case
/// the old (wrong) `symbols.scale(id)` (`10^digits`) descale got 100x wrong.
pub fn usdjpy_symbol_map() -> SymbolMap {
    one_symbol_map(4, "USDJPY", 3)
}

/// The map of the one symbol `name`, id `symbol_id`, at `digits`.
fn one_symbol_map(symbol_id: i64, name: &str, digits: i32) -> SymbolMap {
    let light = vec![ProtoOaLightSymbol {
        symbol_id,
        symbol_name: Some(name.to_string()),
        ..Default::default()
    }];
    let full = vec![ProtoOaSymbol { symbol_id, digits, ..Default::default() }];
    SymbolMap::from_symbols(&light, &full)
}

/// One listed symbol: `(symbol id, the broker's name, the broker's asset class, base asset, quote asset)`.
pub type ListedSymbol = (i64, &'static str, &'static str, &'static str, &'static str);

/// **What the handshake reads of a broker listing `symbols`**: the light symbols and the three reference lists that
/// name their class, base and quote (one category per asset class, every category `Default Category` as the captured
/// broker names them), and no full symbol (no grid).
pub fn listed(symbols: &[ListedSymbol]) -> SymbolLists {
    let (mut classes, mut assets) = (Vec::new(), Vec::new());
    let light = symbols
        .iter()
        .map(|&(symbol_id, name, class, base, quote)| ProtoOaLightSymbol {
            symbol_id,
            symbol_name: Some(name.to_string()),
            enabled: Some(true),
            symbol_category_id: Some(number_of(&mut classes, class)),
            base_asset_id: Some(number_of(&mut assets, base)),
            quote_asset_id: Some(number_of(&mut assets, quote)),
            ..Default::default()
        })
        .collect();
    let categories = (1..=classes.len() as i64)
        .map(|id| ProtoOaSymbolCategory {
            id,
            asset_class_id: id,
            name: "Default Category".to_string(),
            ..Default::default()
        })
        .collect();
    let asset_classes = (1..)
        .zip(&classes)
        .map(|(id, name)| ProtoOaAssetClass {
            id: Some(id),
            name: Some(name.clone()),
            ..Default::default()
        })
        .collect();
    let assets = (1..)
        .zip(&assets)
        .map(|(asset_id, name)| ProtoOaAsset { asset_id, name: name.clone(), ..Default::default() })
        .collect();
    SymbolLists { light, categories, asset_classes, assets, ..Default::default() }
}

/// The 1-based number of `name` in `names`, added at the end when it is new.
fn number_of(names: &mut Vec<String>, name: &str) -> i64 {
    let at = names.iter().position(|n| n == name).unwrap_or_else(|| {
        names.push(name.to_string());
        names.len() - 1
    });
    at as i64 + 1
}

/// **The rows the catalog keeps of a broker listing `symbols`** (`crate::catalog::instruments_from_map`, the parser the
/// account's listing goes through on its way to `<project>/instruments/ctrader.json`), named as the datahub's write
/// stores them (`vike_catalog::spelled`).
pub fn listed_rows(symbols: &[ListedSymbol]) -> Vec<Instrument> {
    vike_catalog::spelled(crate::catalog::instruments_from_map(&SymbolMap::from_lists(&listed(
        symbols,
    ))))
}

/// The broker of every kept list a test of this crate holds: the fake venue's three pairs (`EURUSD`, `GBPUSD`,
/// `USDJPY`), a pair of a broker that suffixes its names (`AUDCADm`, user name `AUDCAD`), an index CFD (`US500`, user
/// name `US500.CFD`) and a coin CFD (`BTCUSD`, user name `BTCUSD.CFD`).
pub const KEPT_FOR_TESTS: [ListedSymbol; 6] = [
    (1, "EURUSD", "Forex", "EUR", "USD"),
    (2, "GBPUSD", "Forex", "GBP", "USD"),
    (3, "USDJPY", "Forex", "USD", "JPY"),
    (4, "AUDCADm", "Forex", "AUD", "CAD"),
    (5, "US500", "Indices", "US500", "USD"),
    (6, "BTCUSD", "Cryptocurrencies", "BTC", "USD"),
];

/// **Hold the ctrader kept list of [`KEPT_FOR_TESTS`] in this test process** (`vike_catalog::hold_kept_list`), the list
/// an order, an amend, a feed subscription and the mount's grids read a USER name through
/// (`crate::instrument::names`). ⚠ Process-wide: every caller holds the SAME rows, so tests running as threads of one
/// binary cannot disagree; a test of "no list" calls the pure `rows`-taking core with `&[]` instead. Reading the fake
/// venue's address holds it (`crate::test_support::FakeCtrader::addr`): the account's own session is what keeps this
/// venue's list. A second call changes nothing.
pub fn kept_list_for_tests() {
    vike_catalog::hold_kept_list("ctrader", listed_rows(&KEPT_FOR_TESTS));
}

/// **Hold this account's own listing as ctrader's kept list** in a live smoke's process (decision 0290): what the
/// datahub keeps of the listing the account's login hands over (`crate::catalog::instruments_from_map` of the
/// handshake's map), so the bridge reads a USER name through it as a production start does. A smoke installs no
/// project, so no kept file is read; the offline tests hold [`kept_list_for_tests`] instead.
pub fn keep_the_accounts_listing(symbols: &SymbolMap) {
    vike_catalog::hold_kept_list("ctrader", crate::catalog::instruments_from_map(symbols));
}

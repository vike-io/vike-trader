//! **Every sentence and every answer of `SymbolPropertiesCache`, byte for byte**, through its public entries (`facts`, `facts_on`,
//! `SymbolPropertiesCacheAccount::facts`, the account-listed cache), pinned before each venue's arm moved into its bridge (owner ruling
//! 2026-11-12: one-venue code lives in its bridge, the shared crate holds the one generic type). The older tests check a
//! word of most refusals (`contains`); these check the WHOLE text and the WHOLE answer, which key each list and lister
//! is asked under, and the order the checks run in where two could refuse one row. Hand-built rows; no network.

use std::sync::{Arc, Mutex};

use vike_gatewayhub_client::proto::{
    SymbolPropertiesRefusal, SymbolPropertiesRefusalCode, SymbolPropertiesWire,
};
use vike_gatewayhub_client::test_support::fake_trade_peer::grid;
use vike_model::{
    Contract, ContractFuture, ContractFutureDelivery, ContractFutureFamily, ContractPerp,
    ContractValue, Instrument, MS_PER_DAY, MarginMode, Margining, SymbolProperties,
};

use super::{
    AccountLister, CurrencyLister, InstrumentList, NOT_ANSWERED, SymbolLister,
    SymbolPropertiesCache, SymbolPropertiesCacheAccount, SymbolPropertiesCacheAccountListed,
};

fn perp(
    venue: &str,
    raw: &str,
    margining: Margining,
    settle: &str,
    value_ccy: Option<&str>,
    p: SymbolProperties,
) -> Instrument {
    Instrument::new(
        venue,
        raw,
        "ETH",
        "USDT",
        Contract::Perp(ContractPerp {
            margining,
            settle: settle.into(),
            value_currency: value_ccy.map(Into::into),
            funding_interval_ms: None,
            settling_ms: None,
        }),
    )
    .with_properties(p)
}

fn future(
    venue: &str,
    raw: &str,
    margining: Margining,
    settle: &str,
    value_ccy: Option<&str>,
    expiry_ms: i64,
    p: SymbolProperties,
) -> Instrument {
    Instrument::new(
        venue,
        raw,
        "ETH",
        "USDT",
        Contract::Future(ContractFuture {
            margining,
            settle: settle.into(),
            value_currency: value_ccy.map(Into::into),
            expiry_ms,
            tenor: None,
            delivery: ContractFutureDelivery::Unknown,
            family: ContractFutureFamily::Crypto,
        }),
    )
    .with_properties(p)
}

fn option(venue: &str, raw: &str, margining: Margining, settle: &str) -> Instrument {
    Instrument::new(
        venue,
        raw,
        "BTC",
        "USD",
        Contract::Option(vike_model::ContractOption {
            right: vike_marketdata::OptionKind::Call,
            strike: 100_000.0,
            expiry_ms: 1_800_000_000_000,
            style: vike_model::ContractOptionExerciseStyle::European,
            margining,
            settle: settle.into(),
            value_currency: None,
            underlying: Some("BTC".into()),
        }),
    )
    .with_properties(grid(0.0001, 1.0))
}

fn spot(venue: &str, raw: &str, quote: &str, p: SymbolProperties) -> Instrument {
    Instrument::new(venue, raw, "BTC", quote, Contract::CryptoSpot).with_properties(p)
}

fn equity(venue: &str, raw: &str, p: SymbolProperties) -> Instrument {
    Instrument::new(
        venue,
        raw,
        raw,
        "USD",
        Contract::Equity(vike_model::ContractEquity {
            exchange: Some("NASDAQ".into()),
            currency: Some("USD".into()),
        }),
    )
    .with_properties(p)
}

/// A row that answers nothing asked here, so a whole list is never empty.
fn filler() -> Instrument {
    spot("any", "FILLER", "USDT", grid(0.1, 1.0))
}

/// Who was asked for what: `list:<key>` for a whole-list read, `one:<key>:<wanted>` for a one-symbol read.
type Calls = Arc<Mutex<Vec<String>>>;

/// Facts over `rows`: every whole-list key and every one-symbol read is served all of them, and each read is logged.
fn facts(rows: Vec<Instrument>) -> (SymbolPropertiesCache, Calls) {
    let calls: Calls = Arc::default();
    let (list_rows, list_log) = (rows.clone(), Arc::clone(&calls));
    let list: InstrumentList = Arc::new(move |key| {
        list_log.lock().unwrap().push(format!("list:{key}"));
        Ok(list_rows.clone())
    });
    let one_log = Arc::clone(&calls);
    let one: SymbolLister = Arc::new(move |key, wanted| {
        one_log.lock().unwrap().push(format!("one:{key}:{wanted}"));
        Ok(rows.clone())
    });
    (SymbolPropertiesCache::new(list).with_symbol_lister(one), calls)
}

/// The same with no one-symbol lister: every venue reads its whole list.
fn listed_only(rows: Vec<Instrument>) -> SymbolPropertiesCache {
    let list: InstrumentList = Arc::new(move |_| Ok(rows.clone()));
    SymbolPropertiesCache::new(list)
}

fn taken(calls: &Calls) -> Vec<String> {
    std::mem::take(&mut *calls.lock().unwrap())
}

fn plain(properties: SymbolProperties) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
    Ok(SymbolPropertiesWire { properties, ..SymbolPropertiesWire::default() })
}

fn cash(properties: SymbolProperties) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
    Ok(SymbolPropertiesWire {
        properties,
        margin_mode: Some(MarginMode::Cash),
        margin_rate: Some(1.0),
        ..SymbolPropertiesWire::default()
    })
}

/// A verdict on the symbol, in exactly these words.
fn refused(why: &str) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
    Err(super::verdict(why))
}

/// The venue lists no such instrument, in exactly these words.
fn gone(why: &str) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
    Err(super::coded(SymbolPropertiesRefusalCode::NoInstrument, why))
}

/// A bridge's verdict on the symbol, in exactly these words, under its refusal id (the wire's `refusal_id`).
fn refused_by(id: &str, why: &str) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
    Err(super::verdict(carried(id, why)))
}

/// The bridge's "no such instrument", in exactly these words, under its refusal id.
fn gone_by(id: &str, why: &str) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
    Err(super::coded(SymbolPropertiesRefusalCode::NoInstrument, carried(id, why)))
}

/// The bridge's "delivered", in exactly these words, under its refusal id.
fn delivered_by(id: &str, why: &str) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
    Err(super::coded(SymbolPropertiesRefusalCode::Expired, carried(id, why)))
}

fn carried(id: &str, why: &str) -> vike_utility::errors::CarriedRefusal {
    vike_utility::errors::CarriedRefusal::new(Some(id.to_owned()), why.to_owned())
}

// ---- the venues no arm answers ----------------------------------------------------------------------------------

#[test]
fn a_venue_with_no_arm_and_an_account_listed_venue_asked_plainly_are_not_answered() {
    assert_eq!(NOT_ANSWERED, "this venue's adapter does not answer symbol facts");
    let (f, calls) = facts(vec![filler()]);
    #[rustfmt::skip]
    let not_answered = [
        "ig", "ibkr", "dukascopy", "fxcm", "nope", "ctrader", "oanda",
        // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): when its bridge's spec joins `symbol_facts_specs.rs`'s SPECS, pin its answers instead
    ];
    for venue in not_answered {
        assert_eq!(f.facts(venue, "EURUSD"), refused(NOT_ANSWERED), "{venue}");
    }
    assert!(taken(&calls).is_empty(), "nothing is read for a venue with no arm");
}

// ---- the not-found sentences: each list's own words ----------------------------------------------------------------

#[test]
fn every_lists_not_found_sentence() {
    crate::testkit::fixtures::hold_hyperliquid_kept_rows();
    let (f, _) = facts(vec![filler()]);
    let lists = listed_only(vec![filler()]);
    let cases: [(&str, Result<SymbolPropertiesWire, SymbolPropertiesRefusal>, &str); 13] = [
        (
            "binance mainnet",
            f.facts_on("binance", "mainnet", "nope.p"),
            "binance: no instrument NOPE.P in the venue's instrument list (spot, USDS-M perpetuals (TradFi included) \
             and dated USDS-M futures that are TRADING; a contract that is pending, settling or being delivered is \
             not in the list)",
        ),
        (
            "binance demo",
            f.facts_on("binance", "testnet", "NOPE.P"),
            "binance: no instrument NOPE.P in the venue's instrument list (spot, and the USDS-M perpetuals (TradFi \
             included) and dated futures of the DEMO futures host, the network this account trades on; that list is \
             shorter than mainnet's)",
        ),
        (
            "binance spot, one at a time",
            f.facts("binance", "nope"),
            "binance: no instrument NOPE in the venue's instrument list (spot, USDS-M perpetuals (TradFi included) \
             and dated USDS-M futures that are TRADING; a contract that is pending, settling or being delivered is \
             not in the list)",
        ),
        (
            "aster",
            f.facts("aster", "NOPE.P"),
            "aster: no instrument NOPE.P in the venue's instrument list (spot and USDS-M perpetuals)",
        ),
        (
            "bybit",
            f.facts("bybit", "nope"),
            "bybit: no instrument NOPE in the venue's instrument list (linear perpetuals and dated linear futures)",
        ),
        (
            "okx mainnet",
            f.facts_on("okx", "mainnet", "nope-usdt-swap"),
            "okx: no instrument NOPE-USDT-SWAP in the venue's instrument list (USDT-quoted spot pairs, linear \
             USDT-settled SWAP perpetuals and dated futures)",
        ),
        (
            "okx demo",
            f.facts_on("okx", "testnet", "NOPE-USDT-SWAP"),
            "okx: no instrument NOPE-USDT-SWAP in the venue's instrument list (USDT-quoted spot pairs, linear \
             USDT-settled SWAP perpetuals and dated futures of OKX's simulated-trading network, the network this \
             account trades on; that list is not mainnet's)",
        ),
        (
            "hyperliquid mainnet, a user name with a kept row, upper-cased",
            f.facts_on("hyperliquid", "mainnet", "nopeusdc.p"),
            "hyperliquid: no instrument NOPEUSDC.P in the venue's instrument list (perpetuals, builder-dex coins and \
             USDC spot pairs)",
        ),
        (
            "hyperliquid testnet",
            f.facts_on("hyperliquid", "testnet", "NOPEUSDC.P"),
            "hyperliquid: no instrument NOPEUSDC.P in the venue's instrument list (perpetuals, builder-dex coins and \
             USDC spot pairs of Hyperliquid's testnet, the network this account trades on; the testnet list is not \
             mainnet's)",
        ),
        (
            "deribit future, upper-cased",
            f.facts("deribit", "nope-perpetual"),
            "deribit: no instrument NOPE-PERPETUAL in the venue's instrument list (perpetuals, dated futures and USDC \
             options of the testnet book)",
        ),
        (
            "deribit option, case kept",
            f.facts("deribit", "BTC-1JAN27-100d5-C"),
            "deribit: no instrument BTC-1JAN27-100d5-C in the venue's instrument list (perpetuals, dated futures and \
             USDC options of the testnet book)",
        ),
        (
            "alpaca from a whole list",
            lists.facts("alpaca", "nope"),
            "alpaca: no instrument NOPE in the venue's instrument list (US equities and crypto pairs)",
        ),
        (
            "polymarket",
            f.facts("polymarket", "999"),
            "polymarket: no instrument 999 in the venue's instrument list (the outcome tokens of the open markets \
             Gamma lists)",
        ),
    ];
    for (what, got, want) in cases {
        assert_eq!(got, gone(want), "{what}");
    }
    // alpaca's one-asset read says it in its own words, a verdict.
    assert_eq!(
        SymbolPropertiesCacheAccount::new().facts("A/demo", "alpaca", "nope", {
            let lister: AccountLister = Arc::new(|_| Ok(vec![filler()]));
            lister
        }),
        refused_by(
            "alpaca.properties.unknown_asset",
            "alpaca: unknown asset NOPE: the venue has no tradable US equity or crypto pair of that name"
        ),
        "alpaca, one asset"
    );
}

fn currency_lister(rows: Vec<Instrument>) -> CurrencyLister {
    Arc::new(move || Ok(("USD".to_string(), rows.clone())))
}

#[test]
fn the_account_listed_venues_not_found_sentences() {
    let ctrader = SymbolPropertiesCacheAccountListed::<
        vike_ctrader::symbol_properties::SymbolPropertiesAccountSpecCtrader,
    >::default();
    // A USER name is upper case (decision 0290); no kept row answers it, so it is looked up as asked and not found.
    assert_eq!(
        ctrader.facts("nOPE", currency_lister(vec![filler()])),
        gone(
            "ctrader: no instrument NOPE in the venue's instrument list (the account's own symbol list)"
        )
    );
    // oanda asks a USER name its kept row names (decision 0290): `eurusd` is kept, and this listing lacks it.
    vike_oanda::test_support::kept_list_for_tests();
    let oanda =
        SymbolPropertiesCacheAccountListed::<vike_oanda::SymbolPropertiesAccountSpecOanda>::default(
        );
    assert_eq!(
        oanda.facts("eurusd", currency_lister(vec![filler()])),
        gone(
            "oanda: no instrument EURUSD in the venue's instrument list (the instruments this account's own OANDA \
             list carries; a CFD, metal or index is listed only if OANDA offers it to the account)"
        )
    );
}

// ---- which key each list and lister is asked under -----------------------------------------------------------------

#[test]
fn each_ask_reads_the_list_or_the_lister_under_its_own_key() {
    crate::testkit::fixtures::hold_hyperliquid_kept_rows();
    let rows = vec![
        spot("binance", "SOLUSDT", "USDT", grid(0.01, 0.001)),
        perp("binance", "SOLUSDT.P", Margining::Linear, "USDT", None, grid(0.1, 1.0)),
        perp(
            "binance",
            "BTCUSD_PERP.P",
            Margining::Inverse,
            "BTC",
            None,
            SymbolProperties { contract_size: 100.0, ..grid(0.1, 1.0) },
        ),
        perp("bybit", "SOLUSDT.P", Margining::Linear, "USDT", None, grid(0.1, 1.0)),
        spot("bybit", "SOLUSDT", "USDT", grid(0.01, 0.001)),
        spot("okx", "BTC-USDT", "USDT", grid(0.1, 0.0001)),
        perp(
            "okx",
            "ETH-USDT-SWAP",
            Margining::Linear,
            "USDT",
            Some("ETH"),
            SymbolProperties { contract_size: 0.1, ..grid(0.01, 0.01) },
        ),
        perp("hyperliquid", "test:ABC", Margining::Linear, "USDC", None, grid(0.01, 0.01)),
        perp("hyperliquid", "BTC", Margining::Linear, "USDC", None, grid(0.1, 0.001)),
        option("deribit", "BTC-1JAN37-100000-C", Margining::Linear, "USDC"),
        perp("deribit", "BTC-PERPETUAL", Margining::Inverse, "BTC", None, grid(0.5, 10.0)),
    ];
    let (f, calls) = facts(rows);
    let asks: [(&str, &str, &str, &[&str]); 13] = [
        ("binance", "testnet", "SOLUSDT", &["one:binance:SOLUSDT"]),
        ("binance", "testnet", "SOLUSDT.P", &["list:binance@demo"]),
        ("binance", "mainnet", "SOLUSDT.P", &["list:binance"]),
        ("binance", "testnet", "BTCUSD_PERP.P", &["list:binance@demo-coin-m"]),
        ("binance", "mainnet", "BTCUSD_PERP.P", &["list:binance@coin-m"]),
        // bybit's lister is asked the USER name (decision 0290), which it reads off the kept row itself.
        ("bybit", "mainnet", "solusdt.p", &["one:bybit:SOLUSDT.P"]),
        ("bybit", "mainnet", "SOLUSDT", &["one:bybit:SOLUSDT"]),
        ("okx", "testnet", "BTC-USDT", &["one:okx@demo:BTC-USDT"]),
        ("okx", "testnet", "ETH-USDT-SWAP", &["list:okx@demo"]),
        // hyperliquid's lister and list are asked the kept row's listing name of the USER name (decision 0290).
        ("hyperliquid", "testnet", "TEST:ABCUSDC.P", &["one:hyperliquid@testnet:test:ABC"]),
        ("hyperliquid", "testnet", "btcusdc.p", &["list:hyperliquid@testnet"]),
        ("deribit", "mainnet", "BTC-1JAN37-100000-C", &["one:deribit:BTC-1JAN37-100000-C"]),
        ("deribit", "mainnet", "btc-perpetual", &["list:deribit"]),
    ];
    for (venue, network, symbol, want) in asks {
        let got = f.facts_on(venue, network, symbol);
        assert!(got.is_ok(), "{venue} {network} {symbol}: {got:?}");
        assert_eq!(taken(&calls), want, "{venue} {network} {symbol}");
    }
    // aster and polymarket on a network other than mainnet are answered as `facts` answers them
    let (f, calls) =
        facts(vec![perp("aster", "SOLUSDT.P", Margining::Linear, "USDT", None, grid(0.1, 1.0))]);
    assert!(f.facts_on("aster", "testnet", "SOLUSDT.P").is_ok());
    assert_eq!(taken(&calls), ["list:aster"]);
}

// ---- the shared checks --------------------------------------------------------------------------------------------

#[test]
fn the_linear_lane_and_the_grid_checks_word_the_venue_and_the_symbol() {
    crate::testkit::fixtures::hold_hyperliquid_kept_rows();
    let (f, _) = facts(vec![
        perp("binance", "INVUSDT.P", Margining::Inverse, "BTC", None, grid(0.1, 1.0))
            .with_venue_word("PERPETUAL"),
        perp("aster", "ASTINVUSDT.P", Margining::Inverse, "BTC", None, grid(0.1, 1.0)),
        perp("binance", "NOTICKUSDT.P", Margining::Linear, "USDT", None, grid(0.0, 1.0)),
        perp("binance", "NOSTEPUSDT.P", Margining::Linear, "USDT", None, grid(0.1, f64::NAN)),
        perp("bybit", "NOTICK.P", Margining::Linear, "USDT", None, grid(0.0, 1.0)),
        spot("bybit", "NOSTEP", "USDT", grid(0.1, 0.0)),
        perp("hyperliquid", "NOTICK", Margining::Linear, "USDC", None, grid(-1.0, 1.0)),
    ]);
    assert_eq!(
        f.facts("binance", "INVUSDT.P"),
        refused(
            "binance: INVUSDT.P is PERPETUAL, not a linear perpetual; the gatewayhub trades linear perpetuals only"
        )
    );
    assert_eq!(
        f.facts("aster", "ASTINVUSDT.P"),
        refused(
            "aster: ASTINVUSDT.P is a non-linear contract, not a linear perpetual; the gatewayhub trades linear \
             perpetuals only"
        )
    );
    assert_eq!(
        f.facts("binance", "NOTICKUSDT.P"),
        refused(
            "binance: instrument NOTICKUSDT.P: the venue's instrument list carries no tick size (missing or zero)"
        )
    );
    assert_eq!(
        f.facts("binance", "NOSTEPUSDT.P"),
        refused(
            "binance: instrument NOSTEPUSDT.P: the venue's instrument list carries no step size (missing or zero)"
        )
    );
    assert_eq!(
        f.facts("bybit", "NOTICK.P"),
        refused(
            "bybit: instrument NOTICK.P: the venue's instrument list carries no tick size (missing or zero)"
        )
    );
    assert_eq!(
        f.facts("bybit", "NOSTEP"),
        refused(
            "bybit: instrument NOSTEP: the venue's instrument list carries no step size (missing or zero)"
        )
    );
    assert_eq!(
        f.facts("hyperliquid", "NOTICKUSDC.P"),
        refused(
            "hyperliquid: instrument NOTICKUSDC.P: the venue's instrument list carries no tick size (missing or \
             zero)"
        )
    );
}

// ---- binance and aster ----------------------------------------------------------------------------------------------

#[test]
fn binance_and_aster_answer_spot_cash_and_a_perpetual_plainly() {
    let rows = vec![
        spot("binance", "SOLUSDT", "USDT", grid(0.01, 0.001)),
        perp("binance", "SOLUSDT.P", Margining::Linear, "USDT", None, grid(0.1, 1.0)),
        spot("aster", "SOLUSDT", "USDT", grid(0.01, 0.001)),
        perp("aster", "SOLUSDT.P", Margining::Linear, "USDT", None, grid(0.1, 1.0)),
    ];
    let (f, _) = facts(rows);
    for venue in ["binance", "aster"] {
        assert_eq!(f.facts(venue, "solusdt"), cash(grid(0.01, 0.001)), "{venue}");
        assert_eq!(f.facts(venue, "SOLUSDT.P"), plain(grid(0.1, 1.0)), "{venue}");
    }
}

/// A delivered dated future is refused by the day its NAME states (`crates/vike-venues/src/venue_delivery_guard.rs`'s
/// `delivery_verdict`, against the real clock); one before its delivery is answered plainly (inside its last day it is
/// close-only, which the bridge enforces at send: `symbol_facts_tests.rs` pins that admission).
#[test]
fn binance_refuses_a_delivered_dated_future_by_name() {
    let rows = vec![
        future(
            "binance",
            "BTCUSDT_250101.P",
            Margining::Linear,
            "USDT",
            None,
            1_735_718_400_000,
            grid(0.1, 0.001),
        ),
        future(
            "binance",
            "BTCUSDT_370326.P",
            Margining::Linear,
            "USDT",
            None,
            2_121_667_200_000,
            grid(0.1, 0.001),
        ),
    ];
    let (f, _) = facts(rows);
    assert_eq!(
        f.facts("binance", "BTCUSDT_250101.P"),
        delivered_by(
            "binance.properties.delivered",
            "binance: BTCUSDT_250101.P was delivered on 2025-01-01; the venue's list still shows it as TRADING, but \
             the contract has expired, so the symbol is not admitted"
        )
    );
    assert_eq!(f.facts("binance", "BTCUSDT_370326.P"), plain(grid(0.1, 0.001)));
}

#[test]
fn a_binance_coin_m_symbol_is_answered_and_refused_from_its_own_list() {
    let face = |p: SymbolProperties| SymbolProperties { contract_size: 100.0, ..p };
    let linear_shaped =
        perp("binance", "ETHUSD_PERP.P", Margining::Linear, "USDT", None, face(grid(0.1, 1.0)));
    let rows = vec![
        perp("binance", "BTCUSD_PERP.P", Margining::Inverse, "BTC", None, face(grid(0.1, 1.0))),
        perp("binance", "XRPUSD_PERP.P", Margining::Inverse, "XRP", None, grid(0.1, 1.0)),
        linear_shaped.clone(),
        perp("binance", "LTCUSD_PERP.P", Margining::Inverse, "LTC", None, face(grid(0.0, 1.0))),
        perp("binance", "DOTUSD_PERP.P", Margining::Inverse, "DOT", None, face(grid(0.1, 0.0))),
        future(
            "binance",
            "BTCUSD_250101.P",
            Margining::Inverse,
            "BTC",
            None,
            1_735_718_400_000,
            face(grid(0.1, 1.0)),
        ),
    ];
    let (f, _) = facts(rows.clone());
    assert_eq!(
        f.facts_on("binance", "mainnet", "btcusd_perp.p"),
        Ok(SymbolPropertiesWire {
            properties: grid(0.1, 1.0),
            contract_value: Some(ContractValue::inverse(100.0)),
            ..SymbolPropertiesWire::default()
        })
    );
    assert_eq!(
        f.facts_on("binance", "mainnet", "ADAUSD_PERP.P"),
        gone_by(
            "binance.properties.coin_m_not_listed",
            "binance: no instrument ADAUSD_PERP.P in the venue's instrument list (the COIN-M contracts that are \
             TRADING on mainnet's COIN-M host)"
        )
    );
    assert_eq!(
        f.facts_on("binance", "testnet", "ADAUSD_PERP.P"),
        gone_by(
            "binance.properties.coin_m_not_listed",
            "binance: no instrument ADAUSD_PERP.P in the venue's instrument list (the COIN-M contracts that are \
             TRADING on the DEMO COIN-M host, the network this account trades on)"
        )
    );
    let xrp = &rows[1];
    assert_eq!(
        f.facts("binance", "XRPUSD_PERP.P"),
        refused_by(
            "binance.properties.coin_m_value_unknown",
            &format!(
                "binance: XRPUSD_PERP.P is a COIN-M row whose value is unknown (contractSize 0, {:?}): its face is the \
             row's alone, so it is not admitted",
                xrp.contract
            )
        )
    );
    assert_eq!(
        f.facts("binance", "ETHUSD_PERP.P"),
        refused_by(
            "binance.properties.coin_m_value_unknown",
            &format!(
                "binance: ETHUSD_PERP.P is a COIN-M row whose value is unknown (contractSize 100, {:?}): its face is \
             the row's alone, so it is not admitted",
                linear_shaped.contract
            )
        )
    );
    assert_eq!(
        f.facts("binance", "LTCUSD_PERP.P"),
        refused(
            "binance: instrument LTCUSD_PERP.P: the venue's instrument list carries no tick size (missing or zero)"
        )
    );
    assert_eq!(
        f.facts("binance", "DOTUSD_PERP.P"),
        refused(
            "binance: instrument DOTUSD_PERP.P: the venue's instrument list carries no step size (missing or zero)"
        )
    );
    assert_eq!(
        f.facts("binance", "BTCUSD_250101.P"),
        delivered_by(
            "binance.properties.delivered",
            "binance: BTCUSD_250101.P was delivered on 2025-01-01; the venue's list still shows it as TRADING, but \
             the contract has expired, so the symbol is not admitted"
        )
    );
}

// ---- bybit ----------------------------------------------------------------------------------------------------------

/// Every symbol is a USER name (decision 0290), keyed by its kept row (`crate::testkit::fixtures::hold_bybit_kept_rows`:
/// `ETHUSDT_371127` is the venue's `ETHUSDT-27NOV37`) or, with none, by the name the catalog spells a perpetual or a
/// pair with; what the venue's ROW is decides the answer.
#[test]
fn bybit_answers_by_what_the_row_is_and_refuses_the_rest_by_name() {
    crate::testkit::fixtures::hold_bybit_kept_rows();
    // The rows' expiries are the kept rows' (the guard reads the day off the user name, against the real clock).
    let delivery = 1_793_520_000_000;
    let rows = vec![
        perp("bybit", "SOLUSDT.P", Margining::Linear, "USDT", None, grid(0.1, 1.0)),
        perp("bybit", "BTCUSD.P", Margining::Inverse, "BTC", None, grid(0.5, 1.0)),
        perp("bybit", "QNTUSDT.P", Margining::Quanto, "USDT", None, grid(0.5, 1.0)),
        spot("bybit", "SOLUSDT", "USDT", grid(0.01, 0.001)),
        spot("bybit", "SPOTONLY", "USDT", grid(0.01, 0.001)),
        future(
            "bybit",
            "ETHUSDT-27NOV37",
            Margining::Linear,
            "USDT",
            None,
            2_142_921_600_000,
            grid(0.1, 0.01),
        )
        .with_venue_word("LinearFutures"),
        future(
            "bybit",
            "BTCUSDZ37",
            Margining::Inverse,
            "BTC",
            None,
            2_145_340_800_000,
            grid(0.5, 1.0),
        ),
        future(
            "bybit",
            "ETHUSDT-01NOV26",
            Margining::Linear,
            "USDT",
            None,
            delivery,
            grid(0.1, 0.01),
        ),
        option("bybit", "BTC-1JAN27-100000-C", Margining::Linear, "USDC").with_venue_word("Option"),
    ];
    let (f, _) = facts(rows);
    assert_eq!(f.facts("bybit", "SOLUSDT.P"), plain(grid(0.1, 1.0)));
    assert_eq!(
        f.facts("bybit", "BTCUSD.P"),
        Ok(SymbolPropertiesWire {
            properties: grid(0.5, 1.0),
            contract_value: Some(ContractValue::inverse(1.0)),
            ..SymbolPropertiesWire::default()
        })
    );
    assert_eq!(f.facts("bybit", "SOLUSDT"), cash(grid(0.01, 0.001)), "the bare name is the pair");
    assert_eq!(f.facts("bybit", "solusdt"), cash(grid(0.01, 0.001)));
    assert_eq!(f.facts("bybit", "ETHUSDT_371127"), plain(grid(0.1, 0.01)));
    assert_eq!(
        f.facts("bybit", "BTCUSD_371225"),
        Ok(SymbolPropertiesWire {
            properties: grid(0.5, 1.0),
            contract_value: Some(ContractValue::inverse(1.0)),
            ..SymbolPropertiesWire::default()
        })
    );
    assert_eq!(
        f.facts("bybit", "ETHUSDT_261101"),
        refused_by(
            "bybit.symbol.delivered",
            &format!(
                "bybit: ETHUSDT_261101 delivers in 0 h (at {delivery} ms): the venue closes every position at delivery \
             and this adapter does not fold that settlement, so no order opens on a contract within 24 h of it; trade \
             the next one"
            )
        )
    );
    assert_eq!(
        f.facts("bybit", "QNTUSDT.P"),
        refused_by(
            "bybit.symbol.quanto",
            "bybit: QNTUSDT.P is a quanto contract, whose notional is not valued"
        )
    );
    assert_eq!(
        f.facts("bybit", "BTC-1JAN27-100000-C"),
        refused_by(
            "bybit.symbol.not_traded",
            "bybit: BTC-1JAN27-100000-C is Option, which this gatewayhub does not trade (it trades linear and \
             coin-margined perpetuals, dated futures of either, and spot pairs; an option needs a payoff and \
             short-margin model the engine does not have)"
        )
    );
    // A pair and a derivative are two rows under two keys, so the row a name keys is what it is: a pair with no
    // perpetual is answered as the pair. The old spot spelling keys no row at all (and the gatewayhub's own lister
    // refuses any name with no kept row before the venue is asked: `vike_bybit::instrument::class::venue_symbol`).
    assert_eq!(f.facts("bybit", "SPOTONLY"), cash(grid(0.01, 0.001)));
    for old in ["SOLUSDT.S", "ETHUSDT-27NOV37.S"] {
        assert!(f.facts("bybit", old).is_err(), "{old}: the old spot spelling is no instrument");
    }
}

// ---- okx --------------------------------------------------------------------------------------------------------------

#[test]
fn okx_answers_each_class_and_refuses_the_rest_by_name() {
    let now = vike_model::now_ms();
    let later = now + 90 * MS_PER_DAY;
    let base =
        |ct: f64| SymbolProperties { contract_size: ct, max_qty: 15_000.0, ..grid(0.01, 0.01) };
    let rows = vec![
        perp("okx", "ETH-USDT-SWAP", Margining::Linear, "USDT", Some("ETH"), base(0.1)),
        perp("okx", "BTC-USD-SWAP", Margining::Inverse, "BTC", Some("USD"), base(100.0)),
        perp("okx", "BTC-BTC-SWAP", Margining::Inverse, "BTC", Some("BTC"), base(100.0)),
        perp("okx", "BTC-NONE-SWAP", Margining::Inverse, "BTC", None, base(100.0)),
        perp("okx", "BTC-ZERO-SWAP", Margining::Inverse, "BTC", Some("USD"), base(0.0)),
        perp("okx", "ETH-USDC-SWAP", Margining::Linear, "USDC", Some("ETH"), base(0.1)),
        perp("okx", "ETH-QUOTE-SWAP", Margining::Linear, "USDT", Some("USDT"), base(0.1)),
        perp("okx", "ETH-NOCT-SWAP", Margining::Linear, "USDT", Some("ETH"), base(0.0)),
        perp("okx", "ETH-QNT-SWAP", Margining::Quanto, "USDT", None, base(0.1))
            .with_venue_word("quanto"),
        perp("okx", "ETH-QNT2-SWAP", Margining::Quanto, "USDT", None, base(0.1)),
        // A dated future asked by its instId: its delivery guard (the day of its USER name) is the okx bridge's own
        // tests' to pin, since no okx list is kept here to name it.
        future("okx", "ETH-USDT-270326", Margining::Linear, "USDT", Some("ETH"), later, base(0.1)),
        future("okx", "BTC-USD-270326", Margining::Inverse, "BTC", Some("USD"), later, base(100.0)),
        spot("okx", "BTC-USDT", "USDT", grid(0.1, 0.00000001)),
        spot("okx", "BTC-USDC", "USDC", grid(0.1, 0.00000001)),
        option("okx", "BTC-USD-261106-76000-C", Margining::Inverse, "BTC"),
        equity("okx", "AAPL-X", grid(0.01, 1.0)),
    ];
    let (f, _) = facts(rows.clone());
    let in_base = SymbolProperties {
        step_size: 0.01 * 0.1,
        min_qty: 0.01 * 0.1,
        max_qty: 15_000.0 * 0.1,
        contract_size: 0.0,
        ..base(0.1)
    };
    assert_eq!(f.facts("okx", "eth-usdt-swap"), plain(in_base));
    assert_eq!(f.facts("okx", "ETH-USDT-270326"), plain(in_base));
    let inverse = Ok(SymbolPropertiesWire {
        properties: SymbolProperties { contract_size: 0.0, ..base(100.0) },
        contract_value: Some(ContractValue::inverse(100.0)),
        ..SymbolPropertiesWire::default()
    });
    assert_eq!(f.facts("okx", "BTC-USD-SWAP"), inverse);
    assert_eq!(f.facts("okx", "BTC-USD-270326"), inverse);
    assert_eq!(f.facts("okx", "btc-usdt"), cash(grid(0.1, 0.00000001)));
    let pins: [(&str, &str, &str); 11] = [
        (
            "BTC-BTC-SWAP",
            "okx.properties.inverse_not_usd",
            "okx: BTC-BTC-SWAP is coin-margined and its contract is counted in BTC, not in US dollars, so its \
             notional is not valued",
        ),
        (
            "BTC-NONE-SWAP",
            "okx.properties.inverse_not_usd",
            "okx: BTC-NONE-SWAP is coin-margined and its contract is counted in no stated currency, not in US \
             dollars, so its notional is not valued",
        ),
        (
            "BTC-ZERO-SWAP",
            "okx.properties.no_contract_size",
            "okx: instrument BTC-ZERO-SWAP: the venue's instrument list carries no contract size (missing or zero)",
        ),
        (
            "ETH-USDC-SWAP",
            "okx.properties.settle_not_usdt",
            "okx: ETH-USDC-SWAP settles in USDC; the gatewayhub admits USDT-settled swaps and futures only for now",
        ),
        (
            "ETH-QUOTE-SWAP",
            "okx.properties.not_counted_in_base",
            "okx: ETH-QUOTE-SWAP: a contract is counted in USDT, not in the base asset ETH",
        ),
        (
            "ETH-NOCT-SWAP",
            "okx.properties.no_contract_size",
            "okx: instrument ETH-NOCT-SWAP: the venue's instrument list carries no contract size (missing or zero)",
        ),
        (
            "ETH-QNT-SWAP",
            "okx.properties.not_linear",
            "okx: ETH-QNT-SWAP is quanto, neither a linear nor a coin-margined perpetual or future, so its notional \
             is not valued",
        ),
        (
            "ETH-QNT2-SWAP",
            "okx.properties.not_linear",
            "okx: ETH-QNT2-SWAP is a contract of no stated kind, neither a linear nor a coin-margined perpetual or \
             future, so its notional is not valued",
        ),
        (
            "BTC-USDC",
            "okx.properties.spot_not_usdt_quoted",
            "okx: BTC-USDC is quoted in USDC, and the engine's ledger is USDT, so only a USDT-quoted pair is \
             admitted (a pair quoted in another asset would be valued at par)",
        ),
        (
            "BTC-USD-261106-76000-C",
            "okx.properties.option_not_admitted",
            "okx: BTC-USD-261106-76000-C is an option, which the gatewayhub does not admit: its premium is quoted \
             in the underlying coin (tick 0.0001 BTC on the demo's BTC-USD family) and a contract is 0.01 of it, \
             so the engine's quantity times price would value it in the wrong money, and the engine has no option \
             payoff or short-option margin model. The venue also wants options trading activated by hand on its \
             app or web before it takes an order (code 51198)",
        ),
        ("AAPL-X", "okx.properties.class_not_traded", ""),
    ];
    for (symbol, id, want) in pins {
        let want = if want.is_empty() {
            format!(
                "okx: {symbol} is a {} instrument, which the gatewayhub does not trade (spot pairs, SWAPs and \
                 dated futures only)",
                rows.last().expect("the equity row").asset_class().sql_word()
            )
        } else {
            want.to_string()
        };
        assert_eq!(f.facts("okx", symbol), refused_by(id, &want), "{symbol}");
    }
}

// ---- hyperliquid ------------------------------------------------------------------------------------------------------

#[test]
fn hyperliquid_answers_and_refuses_by_the_rows_class_and_the_symbols_text() {
    crate::testkit::fixtures::hold_hyperliquid_kept_rows();
    let rows = vec![
        perp("hyperliquid", "kPEPE", Margining::Linear, "USDC", None, grid(0.000001, 1.0)),
        perp("hyperliquid", "RLB", Margining::Linear, "USDC", None, grid(0.0001, 1.0))
            .with_venue_word("onlyIsolated"),
        perp("hyperliquid", "OLD", Margining::Linear, "USDC", None, grid(0.01, 0.1))
            .with_venue_word("isDelisted"),
        spot("hyperliquid", "HYPE/USDC", "USDC", grid(0.001, 0.01)),
        spot("hyperliquid", "ABC/USDH", "USDH", grid(0.01, 1.0)),
        future(
            "hyperliquid",
            "FUT",
            Margining::Linear,
            "USDC",
            None,
            1_900_000_000_000,
            grid(0.1, 1.0),
        ),
    ];
    let (f, calls) = facts(rows);
    // Asked by USER name (decision 0290), in any case: the row is the kept row's listing name, case kept (`kPEPE`).
    assert_eq!(f.facts("hyperliquid", "kpepeusdc.p"), plain(grid(0.000001, 1.0)));
    assert_eq!(
        f.facts("hyperliquid", "RLBUSDC.P"),
        Ok(SymbolPropertiesWire {
            properties: grid(0.0001, 1.0),
            margin_mode: Some(MarginMode::Isolated),
            ..SymbolPropertiesWire::default()
        })
    );
    assert_eq!(f.facts("hyperliquid", "HYPEUSDC"), cash(grid(0.001, 0.01)));
    assert_eq!(
        f.facts("hyperliquid", "OLDUSDC.P"),
        refused_by(
            "hyperliquid.properties.delisted",
            "hyperliquid: `OLDUSDC.P` is delisted; no order can open on it"
        )
    );
    assert_eq!(
        f.facts("hyperliquid", "ABCUSDH"),
        refused_by(
            "hyperliquid.properties.spot_not_usdc_quoted",
            "hyperliquid: `ABCUSDH` is quoted in USDH, and only a pair quoted in USDC is admitted (the engine's \
             ledger is one currency)"
        )
    );
    assert_eq!(
        f.facts("hyperliquid", "FUTUSDC.P"),
        refused_by(
            "hyperliquid.properties.neither_perp_nor_spot",
            "this venue's adapter does not answer symbol facts: hyperliquid `FUTUSDC.P` is neither a perpetual nor \
             a spot pair"
        )
    );
    taken(&calls);
    assert_eq!(
        f.facts("hyperliquid", "@107"),
        refused_by(
            "hyperliquid.properties.wire_name",
            "this venue's adapter does not answer symbol facts: hyperliquid `@107` is the venue's wire name of a \
             spot pair; write the pair's base and quote (for example HYPEUSDC)"
        )
    );
    // An old spelling (the listing name itself) names no kept row: refused in the translator's words.
    for old in ["kPEPE", "HYPE/USDC", "test:ABC"] {
        let why = f.facts("hyperliquid", old).expect_err(old).reason;
        assert!(why.starts_with(NOT_ANSWERED) && why.contains("carries no row"), "{old}: {why}");
    }
    assert!(taken(&calls).is_empty(), "`@N` and an old spelling are refused before any read");
}

// ---- polymarket -------------------------------------------------------------------------------------------------------

#[test]
fn polymarket_answers_an_outcome_cash_and_refuses_a_market_that_takes_no_orders() {
    let outcome = |token: &str| {
        Instrument::new(
            "polymarket",
            token,
            "Yes",
            "USDC",
            Contract::PredictionOutcome(vike_model::ContractPredictionOutcome {
                condition_id: "0xrain".into(),
                outcome: "Yes".into(),
                token_id: token.into(),
                resolution_ms: None,
                neg_risk: Some(true),
            }),
        )
        .with_description("Will it rain? — Yes")
        .with_properties(grid(0.001, 0.01))
    };
    let (f, _) = facts(vec![outcome("123"), outcome("456").with_venue_word("notAcceptingOrders")]);
    assert_eq!(f.facts("polymarket", "123"), cash(grid(0.001, 0.01)));
    assert_eq!(
        f.facts("polymarket", "456"),
        refused_by(
            "polymarket.properties.not_accepting_orders",
            "polymarket: the market of `456` («Will it rain? — Yes») takes no orders right now (Gamma lists it \
             closed, inactive or with acceptingOrders false)"
        )
    );
}

// ---- deribit ----------------------------------------------------------------------------------------------------------

#[test]
fn deribit_answers_by_the_money_model_and_refuses_the_rest_by_name() {
    let btc_spot = spot("deribit", "BTC_USDC", "USDC", grid(0.5, 0.0001));
    let rows = vec![
        perp("deribit", "BTC-PERPETUAL", Margining::Inverse, "BTC", None, grid(0.5, 10.0)),
        perp("deribit", "BTC_USDC-PERPETUAL", Margining::Linear, "USDC", None, grid(0.5, 0.001)),
        future(
            "deribit",
            "BTC-27NOV26",
            Margining::Inverse,
            "BTC",
            None,
            1_900_000_000_000,
            grid(2.5, 10.0),
        ),
        perp("deribit", "BTC_QNT-PERPETUAL", Margining::Quanto, "BTC", None, grid(0.5, 10.0)),
        // Options deliver in 2037: the delivery guard reads an option's day off its name against the real clock.
        option("deribit", "BTC_USDC-1JAN37-100000-C", Margining::Linear, "USDC"),
        option("deribit", "BTC-1JAN37-100000-C", Margining::Inverse, "btc"),
        btc_spot.clone(),
    ];
    let (f, _) = facts(rows);
    let inverse = |p| {
        Ok(SymbolPropertiesWire {
            properties: p,
            contract_value: Some(ContractValue::inverse(1.0)),
            ..SymbolPropertiesWire::default()
        })
    };
    assert_eq!(f.facts("deribit", "btc-perpetual"), inverse(grid(0.5, 10.0)));
    assert_eq!(f.facts("deribit", "BTC-27NOV26"), inverse(grid(2.5, 10.0)));
    assert_eq!(f.facts("deribit", "BTC_USDC-PERPETUAL"), plain(grid(0.5, 0.001)));
    assert_eq!(
        f.facts("deribit", "BTC_USDC-1JAN37-100000-C"),
        Ok(SymbolPropertiesWire {
            properties: grid(0.0001, 1.0),
            margin_rate: Some(1.0),
            ..SymbolPropertiesWire::default()
        })
    );
    assert_eq!(
        f.facts("deribit", "BTC-1JAN37-100000-C"),
        refused_by(
            "deribit.properties.coin_settled_option",
            "deribit: BTC-1JAN37-100000-C is a coin-settled option: its premium is quoted in BTC, so its notional \
             and PnL are coin amounts that an account's one-currency ledger would book as dollars. Trade the \
             USDC-settled options of the same underlying"
        )
    );
    assert_eq!(
        f.facts("deribit", "BTC_USDC"),
        refused_by(
            "deribit.properties.not_a_derivative",
            &format!(
                "deribit: BTC_USDC is {}, not a perpetual, a dated future or a USDC option; a spot pair has no position \
             at the venue (a holding is only a currency balance), so the gatewayhub does not admit one",
                btc_spot.asset_class().sql_word()
            )
        )
    );
    assert_eq!(
        f.facts("deribit", "BTC_QNT-PERPETUAL"),
        refused_by(
            "deribit.properties.quanto",
            "deribit: BTC_QNT-PERPETUAL is a quanto contract, whose notional is not valued"
        )
    );
}

// ---- alpaca -----------------------------------------------------------------------------------------------------------

#[test]
fn alpaca_answers_whole_cents_and_refuses_a_finer_price_grid_after_the_grid_check() {
    let rows = vec![
        equity("alpaca", "AAPL", grid(0.01, 1.0)),
        equity("alpaca", "NOTICK", grid(0.0, 1.0)),
        Instrument::new("alpaca", "BTC/USD", "BTC", "USD", Contract::CryptoSpot)
            .with_properties(grid(1.0, 0.000000001)),
        Instrument::new("alpaca", "SHIB/USD", "SHIB", "USD", Contract::CryptoSpot)
            .with_properties(grid(0.000001, 1.0)),
    ];
    let lister: AccountLister = Arc::new(move |_| Ok(rows.clone()));
    let ask = |symbol: &str| {
        SymbolPropertiesCacheAccount::new().facts("A/demo", "alpaca", symbol, Arc::clone(&lister))
    };
    assert_eq!(ask("aapl"), plain(grid(0.01, 1.0)));
    assert_eq!(ask("BTC/USD"), plain(grid(1.0, 0.000000001)));
    assert_eq!(
        ask("SHIB/USD"),
        refused_by(
            "alpaca.properties.not_whole_cents",
            "alpaca: SHIB/USD has a price increment of 0.000001, and this adapter writes a price to the cent only, \
             so an instrument whose price grid is not whole cents is not admitted"
        )
    );
    assert_eq!(
        ask("NOTICK"),
        refused(
            "alpaca: instrument NOTICK: the venue's instrument list carries no tick size (missing or zero)"
        )
    );
}

// ---- fxcm's converted answer on the wire (its facts are `vike_fxcm::symbol_properties`) ------------------------------

/// An Offers row as the shim's `forex_connect_offers` writes it (docs-shaped, as `vike_fxcm`'s own tests build it): a forex
/// pair, or (`cfd`) a metal or CFD quoted in USD with the venue's own smallest and largest order and no margin stated.
fn fxcm_offer(name: &str, kind: i64, cfd: bool) -> serde_json::Value {
    let mut row = serde_json::json!({
        "instrument": name, "bid": 1.0998, "ask": 1.1002, "digits": 5, "instrument_type": kind,
        "contract_currency": name.split('/').next().unwrap_or_default(), "contract_multiplier": 1.0,
        "subscription": "T", "trading_status": "O", "bid_tradable": "T", "ask_tradable": "T",
        "base_unit": 1000, "quote_age_ms": 0
    });
    if cfd {
        row["contract_currency"] = serde_json::json!("USD");
        row["base_unit"] = serde_json::json!(1);
        row["min_qty"] = serde_json::json!(1);
        row["max_qty"] = serde_json::json!(4000);
        row["mmr"] = serde_json::json!(-1.0);
    }
    row
}

/// **What the wire writes of an fxcm answer** (moved here from the old `fxcm_rate_tests.rs`, the wire being this
/// crate's): a pair at par writes no quote fields (byte-identical to an older daemon's answer), a metal of multiplier 1
/// writes its absent contract value as `null` and no margin rate; the bridge's refusals reach the wire through the one
/// `worded`, the tier's as its own sentence and the rate's with the wire's not-now marker.
#[test]
fn an_fxcm_answer_is_written_as_the_wire_writes_every_venues() {
    use vike_fxcm::account::quote_rate::{
        FxcmPropertiesError, PropertiesRead, account_ccy, facts_from_offers, parse_offers,
    };
    // A USER name finds its Offers row through the kept list (decision 0290).
    vike_fxcm::instrument::names::kept_list_for_tests();
    let usd = account_ccy(PropertiesRead::Json(r#"{"BASE_CRNCY":"USD"}"#.to_string())).unwrap();
    let body = serde_json::Value::Array(vec![
        fxcm_offer("EUR/USD", 1, false),
        fxcm_offer("XAU/USD", 5, true),
    ])
    .to_string();
    let read = |symbol: &str| {
        vike_fxcm::symbol_properties::quoted_facts::<super::WireAnswer>("", symbol, |_, symbol| {
            let offers = parse_offers(&body).map_err(FxcmPropertiesError::Rate)?;
            facts_from_offers(&offers, symbol, &usd, 1_700_000_000_000)
        })
        .map(|answer| answer.0)
    };
    let par = serde_json::to_string(&read("EURUSD").expect("facts")).unwrap();
    for key in ["quote_currency", "quote_rate", "rate_as_of_ms"] {
        assert!(!par.contains(key), "{key} is not written at par: {par}");
    }
    let gold = serde_json::to_string(&read("XAUUSD").expect("facts")).unwrap();
    assert!(gold.contains("\"contract_value\":null") && !gold.contains("margin_rate"), "{gold}");
    let facts_for = |tier, account| {
        vike_fxcm::symbol_properties::facts_for::<super::WireAnswer>(tier, account, "USDJPY")
            .map(|answer| answer.0)
            .map_err(|e| super::worded("fxcm", e))
    };
    assert_eq!(
        facts_for(vike_model::AccountTier::Live, None),
        refused_by(
            "fxcm.symbol.demo_only",
            "fxcm: this adapter trades its demo account only (a `demo` account), so no instrument is looked up \
             for any other tier"
        )
    );
    let why = facts_for(vike_model::AccountTier::Demo, Some("ZZPINNONE")).expect_err("no session");
    assert_eq!(why.code, SymbolPropertiesRefusalCode::RateUnavailable, "{why}");
    assert!(
        why.reason.starts_with("fxcm: the venue's quote-currency rate could not be fetched: "),
        "{why}"
    );
}

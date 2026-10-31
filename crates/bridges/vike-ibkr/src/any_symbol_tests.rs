//! The name-to-facts resolution over a SCRIPTED gateway: every refusal by name, the request budget, and the
//! grid.

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;

const AAPL: i32 = 265_598;

fn m(conid: i32, symbol: &str, sec_type: &str, exchange: &str, currency: &str) -> MatchRow {
    MatchRow {
        conid,
        symbol: symbol.into(),
        sec_type: sec_type.into(),
        primary_exchange: exchange.into(),
        currency: currency.into(),
    }
}

fn details(conid: i32) -> DetailsRow {
    DetailsRow {
        conid,
        symbol: "AAPL".into(),
        sec_type: "STK".into(),
        currency: "USD".into(),
        multiplier: String::new(),
        price_magnifier: 1,
        min_tick: 0.0001,
        size_increment: Some(1.0),
        min_size: Some(1.0),
        market_rule_ids: vec!["26".into(), "26".into(), "26".into()],
    }
}

fn bands(pairs: &[(f64, f64)]) -> Vec<PriceBand> {
    pairs
        .iter()
        .map(|(low_edge, increment)| PriceBand { low_edge: *low_edge, increment: *increment })
        .collect()
}

/// A gateway that answers what it was told to, and counts what it was asked.
struct Scripted {
    matches: Result<Vec<MatchRow>, String>,
    details: Result<Vec<DetailsRow>, String>,
    rules: HashMap<i32, Result<Vec<PriceBand>, String>>,
    /// What the gateway's price read answers (not a request: it is never listed in `asked`).
    price: Option<f64>,
    asked: Mutex<Vec<String>>,
}

impl Scripted {
    fn aapl() -> Self {
        Self {
            matches: Ok(vec![
                m(AAPL, "AAPL", "STK", "NASDAQ", "USD"),
                m(7_001, "AAPL", "STK", "MEXI", "MXN"),
                m(7_002, "AAPL", "OPT", "", "USD"),
                m(7_003, "AAPLX", "STK", "NASDAQ", "USD"),
            ]),
            details: Ok(vec![details(AAPL)]),
            rules: HashMap::from([(26, Ok(bands(&[(0.0, 0.0001), (1.0, 0.01)])))]),
            price: Some(190.0),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("asked").clone()
    }

    fn note(&self, what: String) {
        self.asked.lock().expect("asked").push(what);
    }
}

impl ContractLookup for Scripted {
    fn matching_symbols(&self, pattern: &str) -> Result<Vec<MatchRow>, String> {
        self.note(format!("match {pattern}"));
        self.matches.clone()
    }
    fn contract_details(&self, contract: &MatchRow) -> Result<Vec<DetailsRow>, String> {
        self.note(format!("details {}", contract.conid));
        self.details.clone()
    }
    fn market_rule(&self, id: i32) -> Result<Vec<PriceBand>, String> {
        self.note(format!("rule {id}"));
        self.rules.get(&id).cloned().unwrap_or_else(|| Err("no such rule".into()))
    }
    fn reference_price(&self, _contract: &MatchRow) -> Option<f64> {
        self.price
    }
}

fn run(gateway: &Scripted, name: &str) -> Result<StockFacts, FactsError> {
    stock_facts(gateway, name, "USD")
}

fn refused(result: Result<StockFacts, FactsError>) -> String {
    match result {
        Err(FactsError::Refused(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The ticker resolves to the one USD stock; the grid is the band of its PRICE (190: the cent band, 0.01), not the
/// finest `minTick`; the engine name is the upper-cased request; the non-USD listing, the option and the longer
/// ticker are not candidates.
#[test]
fn a_ticker_resolves_to_its_one_stock_on_the_grid_of_its_price_band() {
    let gateway = Scripted::aapl();
    let facts = run(&gateway, "aapl.smart.usd").expect("AAPL resolves");
    assert_eq!(facts.symbol, "AAPL.SMART.USD");
    assert_eq!(facts.conid, AAPL);
    assert_eq!(facts.properties.tick_size, 0.01);
    assert_eq!(facts.properties.step_size, 1.0);
    assert_eq!(facts.properties.min_qty, 1.0);
    assert_eq!(facts.properties.contract_size, 0.0, "a share: the inert multiplier");
    assert_eq!(facts.properties.asset_class, Some(AssetClass::Equity));
    // One search, one details request, ONE rule fetch for the three listed exchanges naming rule 26.
    assert_eq!(gateway.asked(), ["match AAPL", "details 265598", "rule 26"]);
}

/// A name the grammar does not admit costs no request: forex, a future, an option, an explicit exchange,
/// a dotted ticker, a bad currency.
#[test]
fn a_name_of_another_form_is_refused_before_any_request() {
    for name in [
        "AAPL",
        "AAPL.NASDAQ.USD",
        "EUR.IDEALPRO.USD",
        "EUR.USD.IDEALPRO",
        "ES.GLOBEX.USD.FUT.20251219",
        "SPY.SMART.USD.OPT.20251219.500.C",
        "AAPL.SMART.USD.STK",
        "BRK.B.SMART.USD",
        "AAPL.SMART.US",
        ".SMART.USD",
        "AAPL.SMART.",
        "A".repeat(13).as_str(),
    ] {
        let gateway = Scripted::aapl();
        let why = refused(run(&gateway, name));
        assert!(
            why.contains("not an IBKR stock name") || why.contains("TICKER.SMART.CCY"),
            "{name}: {why}"
        );
        assert!(gateway.asked().is_empty(), "{name} asked the gateway");
    }
}

/// **A name that PINS a contract id skips the symbol search** (decision 0218): the contract is named, so the first
/// request is that contract's own details, the engine name is the pinned spelling, and a ticker listed on several
/// exchanges (refused as ambiguous when unpinned) resolves to exactly the one the id is.
#[test]
fn a_pinned_contract_id_resolves_without_a_search_and_keeps_the_pinned_name() {
    let gateway = Scripted::aapl();
    let facts = run(&gateway, "aapl.smart.usd.stk.265598").expect("a pinned AAPL");
    assert_eq!(facts.symbol, "AAPL.SMART.USD.STK.265598", "the engine holds the name it was asked");
    assert_eq!(facts.conid, AAPL);
    assert_eq!(gateway.asked(), ["details 265598", "rule 26"], "no paced symbol search");

    // The ambiguous ticker a bare name cannot resolve is resolved by the id.
    let mut dual = Scripted::aapl();
    dual.matches =
        Ok(vec![m(11, "DUAL", "STK", "NYSE", "USD"), m(12, "DUAL", "STK", "PINK", "USD")]);
    dual.details = Ok(vec![DetailsRow { symbol: "DUAL".into(), ..details(12) }]);
    refused(run(&dual, "DUAL.SMART.USD"));
    let facts = run(&dual, "DUAL.SMART.USD.STK.12").expect("the id picks the listing");
    assert_eq!((facts.conid, facts.symbol.as_str()), (12, "DUAL.SMART.USD.STK.12"));
}

/// The id and the ticker must agree: a pin whose details name another contract, another ticker or another
/// currency is refused "on a disagreement", never admitted as the id's contract under the ticker's name.
#[test]
fn a_pinned_contract_id_that_disagrees_with_its_ticker_or_currency_is_refused() {
    let wrong_id = Scripted::aapl();
    let why = refused(run(&wrong_id, "AAPL.SMART.USD.STK.7777"));
    assert!(why.contains("conId 265598") && why.contains("7777"), "{why}");

    let mut other_ticker = Scripted::aapl();
    other_ticker.details = Ok(vec![DetailsRow { symbol: "MSFT".into(), ..details(AAPL) }]);
    let why = refused(run(&other_ticker, "AAPL.SMART.USD.STK.265598"));
    assert!(why.contains("MSFT") && why.contains("nothing is admitted on a disagreement"), "{why}");

    let mut other_ccy = Scripted::aapl();
    other_ccy.details = Ok(vec![DetailsRow { currency: "EUR".into(), ..details(AAPL) }]);
    refused(run(&other_ccy, "AAPL.SMART.USD.STK.265598"));
}

/// A contract id that is not a positive whole number, and a field after it, cost no request.
#[test]
fn a_malformed_contract_id_is_refused_before_any_request() {
    for name in [
        "AAPL.SMART.USD.STK.0",
        "AAPL.SMART.USD.STK.-5",
        "AAPL.SMART.USD.STK.abc",
        "AAPL.SMART.USD.STK.99999999999",
        "AAPL.SMART.USD.STK.265598.X",
        "AAPL.SMART.USD.FUT.265598",
    ] {
        let gateway = Scripted::aapl();
        let why = refused(run(&gateway, name));
        assert!(why.contains(name), "{name}: {why}");
        assert!(gateway.asked().is_empty(), "{name} asked the gateway");
    }
}

/// **A class share is `BRK_B` in the engine and `BRK B` at IB** (decision 0218): the search takes the first word
/// (`BRK`), the rows are held to the WHOLE ticker (IB's `BRK B`, never `BRK A` or `BRKX`), the name keeps its
/// `_`, and the details of the chosen id are checked against the same ticker.
#[test]
fn a_class_share_is_found_by_its_first_word_and_matched_whole() {
    let mut gateway = Scripted::aapl();
    gateway.matches = Ok(vec![
        m(72_063_691, "BRK B", "STK", "NYSE", "USD"),
        m(72_063_690, "BRK A", "STK", "NYSE", "USD"),
        m(5, "BRKX", "STK", "NASDAQ", "USD"),
    ]);
    gateway.details = Ok(vec![DetailsRow { symbol: "BRK B".into(), ..details(72_063_691) }]);
    let facts = run(&gateway, "BRK_B.SMART.USD").expect("BRK B resolves");
    assert_eq!((facts.symbol.as_str(), facts.conid), ("BRK_B.SMART.USD", 72_063_691));
    assert_eq!(gateway.asked(), ["match BRK", "details 72063691", "rule 26"]);
    // Pinned, the same share needs no search and is held to its ticker all the same.
    let pinned = run(&gateway, "BRK_B.SMART.USD.STK.72063691").expect("pinned");
    assert_eq!(pinned.conid, 72_063_691);
    gateway.details = Ok(vec![DetailsRow { symbol: "BRK A".into(), ..details(72_063_691) }]);
    refused(run(&gateway, "BRK_B.SMART.USD.STK.72063691"));
    // A `_` at either end is no class share.
    for bad in ["_B.SMART.USD", "BRK_.SMART.USD"] {
        assert!(refused(run(&gateway, bad)).contains("not an IBKR stock name"), "{bad}");
    }
}

/// The ambiguity refusal says how to pin the listing: the spelling, with a real conId from the answer.
#[test]
fn the_ambiguity_refusal_names_the_pinned_spelling() {
    let mut gateway = Scripted::aapl();
    gateway.matches =
        Ok(vec![m(11, "DUAL", "STK", "NYSE", "USD"), m(12, "DUAL", "STK", "PINK", "USD")]);
    let why = refused(run(&gateway, "DUAL.SMART.USD"));
    assert!(
        why.contains("DUAL.SMART.USD.STK.<conId>") && why.contains("DUAL.SMART.USD.STK.11"),
        "{why}"
    );
}

/// The currency fence: the name states a currency other than the account's money, refused with no request.
#[test]
fn a_stock_in_another_currency_than_the_accounts_is_refused_before_any_request() {
    let gateway = Scripted::aapl();
    let why = refused(stock_facts(&gateway, "VOD.SMART.GBP", "USD"));
    assert!(
        why.contains("GBP") && why.contains("USD") && why.contains("nothing converts"),
        "{why}"
    );
    assert!(gateway.asked().is_empty());
    // And the other way round: an account in EUR does not admit a USD name.
    let why = refused(stock_facts(&gateway, "AAPL.SMART.USD", "EUR"));
    assert!(why.contains("EUR"), "{why}");
    assert!(gateway.asked().is_empty());
}

/// Several stocks in the name's own currency are refused by name, with each conId and exchange - never picked.
#[test]
fn an_ambiguous_ticker_is_refused_naming_each_contract() {
    let mut gateway = Scripted::aapl();
    gateway.matches = Ok(vec![
        m(11, "DUAL", "STK", "NYSE", "USD"),
        m(12, "DUAL", "STK", "PINK", "USD"),
        m(12, "DUAL", "STK", "PINK", "USD"),
    ]);
    let why = refused(run(&gateway, "DUAL.SMART.USD"));
    assert!(why.contains("ambiguous") && why.contains("2 stocks"), "{why}");
    assert!(why.contains("conId 11 on NYSE") && why.contains("conId 12 on PINK"), "{why}");
    assert_eq!(gateway.asked(), ["match DUAL"], "nothing after the search");
}

/// A ticker listed only in other currencies, only as other kinds of contract, or not at all: each refusal says so.
#[test]
fn a_ticker_with_no_stock_in_the_name_s_currency_is_refused_by_what_it_is() {
    let mut gateway = Scripted::aapl();
    gateway.matches = Ok(vec![m(5, "VOD", "STK", "LSE", "GBP"), m(6, "VOD", "STK", "AEB", "EUR")]);
    let why = refused(run(&gateway, "VOD.SMART.USD"));
    assert!(why.contains("EUR, GBP") && why.contains("not in USD"), "{why}");

    gateway.matches = Ok(vec![m(8, "ES", "FUT", "GLOBEX", "USD"), m(9, "ES", "IND", "", "USD")]);
    let why = refused(run(&gateway, "ES.SMART.USD"));
    assert!(why.contains("FUT, IND") && why.contains("no stock"), "{why}");

    gateway.matches = Ok(vec![m(3, "AAPLX", "STK", "NASDAQ", "USD")]);
    let why = refused(run(&gateway, "AAPL.SMART.USD"));
    assert!(why.contains("names no contract"), "{why}");

    gateway.matches = Ok(Vec::new());
    assert!(refused(run(&gateway, "NOPE.SMART.USD")).contains("names no contract"));
}

/// A request that does not answer is "unavailable" (ask again), never a verdict, at each of the three steps.
#[test]
fn a_request_that_does_not_answer_is_unavailable_not_a_refusal() {
    let unavailable = |gateway: &Scripted| match run(gateway, "AAPL.SMART.USD") {
        Err(FactsError::Unavailable(why)) => why,
        other => panic!("expected unavailable, got {other:?}"),
    };
    let mut gateway = Scripted::aapl();
    gateway.matches = Err("timed out".into());
    assert!(
        unavailable(&gateway).contains("symbol search")
            && unavailable(&gateway).contains("timed out")
    );

    let mut gateway = Scripted::aapl();
    gateway.details = Err("no link".into());
    assert!(unavailable(&gateway).contains("contract details"));

    let mut gateway = Scripted::aapl();
    gateway.rules.insert(26, Err("no link".into()));
    assert!(unavailable(&gateway).contains("market rule 26"));
}

/// Details that disagree with the match, a multiplier other than one, an empty reply.
#[test]
fn contract_details_that_disagree_or_carry_a_multiplier_are_refused() {
    let with = |change: fn(&mut DetailsRow)| {
        let mut gateway = Scripted::aapl();
        let mut row = details(AAPL);
        change(&mut row);
        gateway.details = Ok(vec![row]);
        refused(run(&gateway, "AAPL.SMART.USD"))
    };
    assert!(with(|r| r.conid = 1).contains("disagreement"));
    assert!(with(|r| r.sec_type = "FUT".into()).contains("disagreement"));
    assert!(with(|r| r.currency = "EUR".into()).contains("disagreement"));
    assert!(with(|r| r.multiplier = "100".into()).contains("multiplier of 100"));
    assert!(with(|r| r.multiplier = "abc".into()).contains("multiplier of abc"));
    // A multiplier of exactly one is a share.
    let mut gateway = Scripted::aapl();
    let mut row = details(AAPL);
    row.multiplier = "1".into();
    gateway.details = Ok(vec![row]);
    assert!(run(&gateway, "AAPL.SMART.USD").is_ok());

    let mut gateway = Scripted::aapl();
    gateway.details = Ok(Vec::new());
    assert!(refused(run(&gateway, "AAPL.SMART.USD")).contains("no contract details"));
}

/// A grid the engine cannot hold is refused naming the missing piece: no minimum tick, no size increment, no
/// market rule, an unreadable id, too many rules, a rule with no band, bands that do not nest.
#[test]
fn a_grid_that_cannot_be_read_is_refused_by_name() {
    let with = |change: fn(&mut DetailsRow), rules: Vec<(i32, Vec<PriceBand>)>| {
        let mut gateway = Scripted::aapl();
        let mut row = details(AAPL);
        change(&mut row);
        gateway.details = Ok(vec![row]);
        gateway.rules = rules.into_iter().map(|(id, b)| (id, Ok(b))).collect();
        refused(run(&gateway, "AAPL.SMART.USD"))
    };
    let ok_rule = || vec![(26, bands(&[(0.0, 0.01)]))];
    assert!(with(|r| r.min_tick = 0.0, ok_rule()).contains("no minimum tick"));
    assert!(with(|r| r.min_tick = f64::NAN, ok_rule()).contains("no minimum tick"));
    assert!(with(|r| r.size_increment = None, ok_rule()).contains("no size increment"));
    assert!(with(|r| r.size_increment = Some(0.0), ok_rule()).contains("no size increment"));
    assert!(with(|r| r.min_size = Some(-1.0), ok_rule()).contains("minimum size"));
    // TWS reported no size rules at all: refused by name, not admitted on an unknown minimum.
    let none = |r: &mut DetailsRow| {
        r.size_increment = None;
        r.min_size = None;
    };
    assert!(with(none, ok_rule()).contains("size rules unavailable"));
    let zero = |r: &mut DetailsRow| {
        r.size_increment = Some(0.0);
        r.min_size = Some(0.0);
    };
    assert!(with(zero, ok_rule()).contains("size rules unavailable"));
    assert!(with(|r| r.market_rule_ids.clear(), ok_rule()).contains("names no market rule"));
    assert!(with(|r| r.market_rule_ids = vec!["x".into()], ok_rule()).contains("not a number"));
    assert!(
        with(|r| r.market_rule_ids = vec!["1,2,3,4,5".into()], ok_rule())
            .contains("5 distinct market rules")
    );
    assert!(with(|_| {}, vec![(26, Vec::new())]).contains("no price band"));
    assert!(with(|_| {}, vec![(26, bands(&[(0.0, 0.0)]))]).contains("increment of 0"));
    // With no price the grid is the finest band's and every increment must be a multiple of it: 0.05 is not a multiple
    // of 0.02. (With a price, one rule's other bands are not the grid's business: only the band the price is in is.)
    let mut unpriced = Scripted::aapl();
    unpriced.price = None;
    unpriced.rules = HashMap::from([(26, Ok(bands(&[(0.0, 0.05), (1.0, 0.02)])))]);
    let why = refused(run(&unpriced, "AAPL.SMART.USD"));
    assert!(why.contains("do not nest"), "{why}");
}

/// One usable size rule is enough: a `sizeIncrement` without a `minSize` is admitted (minimum 0, the step is
/// the floor), and a `minSize` without an increment keeps its existing refusal, naming the increment.
#[test]
fn one_usable_size_rule_is_enough_to_admit() {
    let mut gateway = Scripted::aapl();
    let mut row = details(AAPL);
    row.min_size = None;
    gateway.details = Ok(vec![row]);
    let facts = run(&gateway, "AAPL.SMART.USD").expect("an increment alone admits");
    assert_eq!((facts.properties.step_size, facts.properties.min_qty), (1.0, 0.0));
}

/// Several rules (one per exchange) are all read, once each; the grid is the coarsest of THEIR increments at the
/// price (0.05 at 190); a comma-joined id list is split.
#[test]
fn every_distinct_rule_is_read_once_and_the_coarsest_of_them_at_the_price_wins() {
    let mut gateway = Scripted::aapl();
    let mut row = details(AAPL);
    row.market_rule_ids = vec!["26, 240".into(), "26".into(), "240".into(), String::new()];
    gateway.details = Ok(vec![row]);
    gateway.rules = HashMap::from([
        (26, Ok(bands(&[(0.0, 0.0001), (1.0, 0.01)]))),
        (240, Ok(bands(&[(0.0, 0.01), (100.0, 0.05)]))),
    ]);
    let facts = run(&gateway, "AAPL.SMART.USD").expect("resolves");
    assert_eq!(facts.properties.tick_size, 0.05);
    assert_eq!(gateway.asked(), ["match AAPL", "details 265598", "rule 26", "rule 240"]);
}

// ---- N08: the grid of the band the price is in, over the REAL market rule of SAP.SMART.EUR ------------------

/// Market rule 1874 (the same table as 1943 and 3055, the three ids the contract names) as the paper gateway answered
/// it on 2026-10-30 19:57 UTC (`tests/ibkr_data_probe_smoke.rs`): `(low_edge, increment)`. Its coarsest band, 10, starts
/// at 50000 EUR.
const SAP_RULE: [(f64, f64); 19] = [
    (0.0, 0.0001),
    (0.1, 0.0001),
    (0.2, 0.0001),
    (0.5, 0.0001),
    (1.0, 0.0002),
    (2.0, 0.0005),
    (5.0, 0.001),
    (10.0, 0.002),
    (20.0, 0.005),
    (50.0, 0.01),
    (100.0, 0.02),
    (200.0, 0.05),
    (500.0, 0.1),
    (1000.0, 0.2),
    (2000.0, 0.5),
    (5000.0, 1.0),
    (10000.0, 2.0),
    (20000.0, 5.0),
    (50000.0, 10.0),
];

/// SAP on the EUR listing as the gateway described it: three market rule ids naming the one table, `minTick` 0.0001.
fn real_sap(price: Option<f64>) -> Scripted {
    let mut gateway = Scripted::aapl();
    gateway.matches = Ok(vec![m(14_204, "SAP", "STK", "IBIS", "EUR")]);
    let mut row = details(14_204);
    row.symbol = "SAP".into();
    row.currency = "EUR".into();
    row.market_rule_ids = vec!["1874".into(), "1874".into(), "1943".into(), "3055".into()];
    gateway.details = Ok(vec![row]);
    gateway.rules = HashMap::from([
        (1874, Ok(bands(&SAP_RULE))),
        (1943, Ok(bands(&SAP_RULE))),
        (3055, Ok(bands(&SAP_RULE))),
    ]);
    gateway.price = price;
    gateway
}

/// **N08, reproduced on the captured rule**: the facts of `SAP.SMART.EUR` answered a grid tick of 10.0, the COARSEST
/// band, which is the band from 50000 EUR; the engine would have rounded a 213 EUR limit to a multiple of 10. The tick is
/// the increment of the band the PRICE falls in.
#[test]
fn the_tick_of_sap_is_the_band_of_its_price_never_the_coarsest() {
    for (price, tick) in [
        (0.05, 0.0001),
        (0.7, 0.0001),
        (1.5, 0.0002),
        (7.0, 0.001),
        (49.99, 0.005),
        (99.99, 0.01),
        (100.0, 0.02),
        (213.0, 0.05),
        (480.0, 0.05),
        (500.0, 0.1),
        (60_000.0, 10.0),
    ] {
        let facts =
            stock_facts_any_currency(&real_sap(Some(price)), "SAP.SMART.EUR").expect("resolves");
        assert_eq!(facts.properties.tick_size, tick, "the tick at {price} EUR");
        assert_eq!(
            facts.tick_grid.price,
            Some(price),
            "the facts say what price the band was read for"
        );
    }
    let facts =
        stock_facts_any_currency(&real_sap(Some(213.0)), "SAP.SMART.EUR").expect("resolves");
    assert_ne!(
        facts.properties.tick_size, 10.0,
        "the coarsest band is not the tick of a 213 EUR stock"
    );
    assert_eq!(
        facts.tick_grid.rules.len(),
        3,
        "every rule the contract names is kept for the exec's grid check"
    );
    assert!(facts.tick_grid.rules.iter().all(|r| r.len() == 19));
}

/// With NO price the grid is the finest band's (0.0001), never the coarsest; the facts say no price was known.
#[test]
fn a_stock_with_no_price_takes_the_finest_band_and_says_so() {
    let facts = stock_facts_any_currency(&real_sap(None), "SAP.SMART.EUR").expect("resolves");
    assert_eq!(facts.properties.tick_size, 0.0001);
    assert_eq!(facts.tick_grid.price, None);
    // A price the gateway sent that is not a price (zero, negative, NaN) is no price.
    for odd in [0.0, -3.0, f64::NAN, f64::INFINITY] {
        let facts =
            stock_facts_any_currency(&real_sap(Some(odd)), "SAP.SMART.EUR").expect("resolves");
        assert_eq!((facts.properties.tick_size, facts.tick_grid.price), (0.0001, None), "{odd}");
    }
}

/// Rules that disagree at the price in a way no one grid serves are refused by name, with the price.
#[test]
fn rules_that_do_not_nest_at_the_price_are_refused_naming_it() {
    let mut gateway = Scripted::aapl();
    let mut row = details(AAPL);
    row.market_rule_ids = vec!["26".into(), "240".into()];
    gateway.details = Ok(vec![row]);
    gateway.rules =
        HashMap::from([(26, Ok(bands(&[(0.0, 0.01)]))), (240, Ok(bands(&[(0.0, 0.015)])))]);
    let why = refused(run(&gateway, "AAPL.SMART.USD"));
    assert!(why.contains("do not nest at the price 190"), "{why}");
}

// ---- an account that CONVERTS a quote currency (decision 0183) --------------------------------------------

/// A EUR-listed gateway (DOCS-SHAPED: the rows `reqMatchingSymbols` / `reqContractDetails` describe, with
/// illustrative ids; <https://interactivebrokers.github.io/tws-api/contract_details.html>).
fn sap() -> Scripted {
    let mut gateway = Scripted::aapl();
    gateway.matches =
        Ok(vec![m(14_000, "SAP", "STK", "IBIS", "EUR"), m(14_001, "SAP", "STK", "NYSE", "USD")]);
    let mut row = details(14_000);
    row.symbol = "SAP".into();
    row.currency = "EUR".into();
    row.min_tick = 0.01;
    gateway.details = Ok(vec![row]);
    gateway.rules = HashMap::from([(26, Ok(bands(&[(0.0, 0.01)])))]);
    gateway
}

/// **The converting variant lifts the currency fence and nothing else**: a EUR stock resolves on a USD account
/// (the plain variant refuses it before any request), to the one EUR contract (never the USD listing of the same
/// ticker), on its grid.
#[test]
fn a_stock_of_another_currency_resolves_when_the_account_converts_and_is_refused_when_it_does_not()
{
    let gateway = sap();
    let why = refused(stock_facts(&gateway, "SAP.SMART.EUR", "USD"));
    assert!(why.contains("nothing converts"), "{why}");
    assert!(gateway.asked().is_empty(), "the plain variant asks nothing");

    let facts = stock_facts_any_currency(&gateway, "sap.smart.eur").expect("resolves");
    assert_eq!((facts.symbol.as_str(), facts.conid), ("SAP.SMART.EUR", 14_000));
    assert_eq!(facts.properties.tick_size, 0.01);
    assert_eq!(gateway.asked(), ["match SAP", "details 14000", "rule 26"]);
}

/// The converting variant keeps every OTHER refusal: the form, an ambiguous ticker in the name's currency, a
/// ticker with no stock in it, a multiplier.
#[test]
fn the_converting_variant_keeps_every_other_refusal() {
    let gateway = sap();
    for bad in ["SAP", "SAP.NASDAQ.EUR", "EUR.IDEALPRO.USD", "SAP.SMART.EU"] {
        assert!(
            matches!(stock_facts_any_currency(&gateway, bad), Err(FactsError::Refused(_))),
            "{bad}"
        );
    }
    assert!(gateway.asked().is_empty());
    let why = refused(stock_facts_any_currency(&gateway, "SAP.SMART.GBP"));
    assert!(why.contains("USD, EUR") || why.contains("EUR, USD"), "{why}");
    assert!(why.contains("not in GBP"), "{why}");

    let mut dual = sap();
    dual.matches = Ok(vec![m(1, "SAP", "STK", "IBIS", "EUR"), m(2, "SAP", "STK", "FWB", "EUR")]);
    assert!(refused(stock_facts_any_currency(&dual, "SAP.SMART.EUR")).contains("ambiguous"));
}

/// **The resolution RECORDS the price magnifier and judges nothing about it** (V2 of the vendor-doc verification,
/// narrowed after the docs review): the PLAIN path (`stock_facts`, an account that converts nothing) answers
/// exactly what v0.1.80 did whatever the gateway reports (a `0` it left unset, a `100`, a `1`), and the converting
/// resolution (`stock_facts_any_currency`) answers the same facts with the value recorded, for the converting
/// path to judge once it knows the stock is FOREIGN (`price_magnifier_refusal`).
#[test]
fn the_magnifier_is_recorded_and_never_refuses_the_plain_path() {
    let with = |magnifier: i32| {
        let mut gateway = Scripted::aapl();
        let mut row = details(AAPL);
        row.price_magnifier = magnifier;
        gateway.details = Ok(vec![row]);
        gateway
    };
    let baseline = run(&Scripted::aapl(), "AAPL.SMART.USD").expect("main's behaviour");
    for magnifier in [0, 1, 100, -3] {
        let plain = run(&with(magnifier), "AAPL.SMART.USD").expect("the plain path is unchanged");
        assert_eq!(plain.price_magnifier, magnifier, "recorded as the gateway said it");
        assert_eq!(
            (plain.symbol, plain.conid, plain.properties),
            (baseline.symbol.clone(), baseline.conid, baseline.properties),
            "{magnifier}: the facts are byte-identical to the unmagnified ones"
        );
        let converting = stock_facts_any_currency(&with(magnifier), "AAPL.SMART.USD")
            .expect("the converting resolution records it too");
        assert_eq!(converting.price_magnifier, magnifier);
    }
}

/// **`price_magnifier_refusal` is the converting path's judgement of a FOREIGN stock**: only a magnifier of exactly
/// 1 passes; `0` (unset by the gateway), a pence-style `100` and anything else are refused in words naming the
/// stock, the value and the 100x risk.
#[test]
fn only_a_magnifier_of_one_passes_the_foreign_stock_judgement() {
    assert_eq!(price_magnifier_refusal("SAP.SMART.EUR", "EUR", 1), None);
    for magnifier in [0, 100, 2, -1] {
        let why = price_magnifier_refusal("SAP.SMART.EUR", "EUR", magnifier).expect("refused");
        assert!(
            why.contains("SAP.SMART.EUR")
                && why.contains(&format!("price magnifier of {magnifier}"))
                && why.contains("100 times too high")
                && why.contains("another currency than the account's"),
            "{magnifier}: {why}"
        );
    }
}

/// **A forex pair is refused as a CASH position, in words**, by every spelling that names one, and a stock whose
/// TICKER is `CASH` is not mistaken for one.
#[test]
fn a_forex_pair_is_refused_as_a_cash_position_not_converted() {
    for forex in
        ["EUR.IDEALPRO.USD", "EUR.USD.IDEALPRO", "EUR.IDEALPRO.USD.CASH", "eur.idealpro.gbp"]
    {
        let why = parse_stock_name(forex).expect_err(forex);
        assert!(
            why.contains("not an IBKR stock name") && why.contains("TICKER.SMART.CCY"),
            "{forex}: {why}"
        );
        assert!(why.contains("CASH position") && why.contains("not converted"), "{forex}: {why}");
    }
    assert_eq!(
        parse_stock_name("CASH.SMART.USD"),
        Ok(StockName { ticker: "CASH".to_string(), currency: "USD".to_string(), con_id: None })
    );
    let why = parse_stock_name("AAPL.SMART.US.EXTRA").expect_err("a fourth field");
    assert!(!why.contains("CASH position"), "{why}");
}

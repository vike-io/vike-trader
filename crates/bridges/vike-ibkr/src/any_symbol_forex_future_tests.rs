//! The forex and futures facts over a SCRIPTED gateway whose answers are the paper account's own, captured
//! 2026-10-30 (`tests/ibkr_forex_futures_probe.rs`): the contract details, the market rules, the what-if orders and the
//! delayed quote of EUR.USD, USD.JPY, ES and CL.

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;

/// 2026-10-30 20:00 UTC, the day of the captures.
const NOW_MS: i64 = 1_793_390_400_000;

fn eurusd_row() -> ClassRow {
    ClassRow {
        conid: 12_087_792,
        symbol: "EUR".into(),
        sec_type: "CASH".into(),
        exchange: "IDEALPRO".into(),
        currency: "USD".into(),
        multiplier: String::new(),
        real_expiration: String::new(),
        last_trade: String::new(),
        price_magnifier: 1,
        min_tick: 0.00005,
        size_increment: Some(0.01),
        min_size: Some(0.01),
        market_rule_ids: vec!["3188".into()],
    }
}

fn usdjpy_row() -> ClassRow {
    ClassRow {
        conid: 15_016_059,
        symbol: "USD".into(),
        currency: "JPY".into(),
        min_tick: 0.005,
        size_increment: Some(1.0),
        min_size: Some(1.0),
        market_rule_ids: vec!["145".into()],
        ..eurusd_row()
    }
}

fn es_row() -> ClassRow {
    ClassRow {
        conid: 515_416_632,
        symbol: "ES".into(),
        sec_type: "FUT".into(),
        exchange: "CME".into(),
        currency: "USD".into(),
        multiplier: "50".into(),
        real_expiration: "20261218".into(),
        last_trade: "20261218 08:30:00 US/Central".into(),
        price_magnifier: 1,
        min_tick: 0.25,
        size_increment: Some(1.0),
        min_size: Some(1.0),
        market_rule_ids: vec!["67".into(), "67".into()],
    }
}

fn cl_row() -> ClassRow {
    ClassRow {
        conid: 296_574_787,
        symbol: "CL".into(),
        exchange: "NYMEX".into(),
        multiplier: "1000".into(),
        real_expiration: "20261120".into(),
        last_trade: "20261120 14:30:00 US/Eastern".into(),
        min_tick: 0.01,
        market_rule_ids: vec!["32".into(), "32".into()],
        ..es_row()
    }
}

const EUR_WARNING: &str = "Important Note: Your order size is below the EUR 20000 IdealPro minimum.\nYou may cancel \
and increase the order size if you wish to route to IdealPro\nor select the Transmit button to route as an odd lot \
order.\nNote that odd lot orders are not guaranteed executions at the IdealPro displayed quotes.";

const USD_WARNING: &str = "Important Note: Your order size is below the USD 25000 IdealPro minimum.\nYou may cancel \
and increase the order size if you wish to route to IdealPro\nor select the Transmit button to route as an odd lot \
order.\nNote that odd lot orders are not guaranteed executions at the IdealPro displayed quotes.";

fn bands(increment: f64) -> Vec<PriceBand> {
    vec![PriceBand { low_edge: 0.0, increment }]
}

fn what_if(initial: Option<f64>, warning: &str) -> WhatIfRow {
    WhatIfRow {
        initial_margin_change: initial,
        maintenance_margin_change: initial,
        margin_currency: if initial.is_some() { "USD".into() } else { String::new() },
        commission: Some(2.0),
        commission_currency: "USD".into(),
        warning_text: warning.into(),
    }
}

/// A gateway that answers what it was told to, and counts what it was asked.
struct Scripted {
    rows: Result<Vec<ClassRow>, String>,
    rules: HashMap<i32, Vec<PriceBand>>,
    buy: Result<WhatIfRow, String>,
    sell: Result<WhatIfRow, String>,
    price: Result<Option<f64>, String>,
    asked: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(rows: Vec<ClassRow>) -> Self {
        Self {
            rows: Ok(rows),
            rules: HashMap::from([
                (3188, bands(0.00005)),
                (145, bands(0.005)),
                (67, bands(0.25)),
                (32, bands(0.01)),
            ]),
            buy: Ok(what_if(None, EUR_WARNING)),
            sell: Ok(what_if(None, EUR_WARNING)),
            price: Ok(Some(7588.75)),
            asked: Mutex::new(Vec::new()),
        }
    }
    fn eurusd() -> Self {
        Self::new(vec![eurusd_row()])
    }
    fn es() -> Self {
        let mut g = Self::new(vec![es_row()]);
        g.buy = Ok(what_if(Some(23_986.46), ""));
        g.sell = Ok(what_if(Some(19_778.57), ""));
        g
    }
    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("asked").clone()
    }
    fn note(&self, what: String) {
        self.asked.lock().expect("asked").push(what);
    }
}

impl ClassLookup for Scripted {
    fn details_of(&self, contract: &IbkrContract) -> Result<Vec<ClassRow>, String> {
        self.note(format!("details {} {}", contract.symbol, contract.sec_type.as_ib_code()));
        self.rows.clone()
    }
    fn market_rule(&self, id: i32) -> Result<Vec<PriceBand>, String> {
        self.note(format!("rule {id}"));
        self.rules.get(&id).cloned().ok_or_else(|| "no such rule".to_string())
    }
    fn what_if(&self, row: &ClassRow, buy: bool, qty: f64) -> Result<WhatIfRow, String> {
        self.note(format!("whatif {} {} {qty}", row.conid, if buy { "buy" } else { "sell" }));
        if buy { self.buy.clone() } else { self.sell.clone() }
    }
    fn reference_price(&self, row: &ClassRow) -> Result<Option<f64>, String> {
        self.note(format!("price {}", row.conid));
        self.price.clone()
    }
}

fn forex(name: &str) -> ForexName {
    match parse_class_name(name) {
        Some(Ok(ClassName::Forex(f))) => f,
        other => panic!("{name} is not a forex name: {other:?}"),
    }
}

fn future(name: &str) -> FutureName {
    match parse_class_name(name) {
        Some(Ok(ClassName::Future(f))) => f,
        other => panic!("{name} is not a future name: {other:?}"),
    }
}

fn refusal<T: std::fmt::Debug>(r: Result<T, FactsError>) -> String {
    match r {
        Err(FactsError::Refused(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The names: the three spellings of one pair are one engine name, the stock form is nobody's, an option is refused by
/// name and the class of a future is read from its fourth field.
#[test]
fn the_names_of_the_two_classes_are_read_and_an_option_is_refused_by_name() {
    for spelling in
        ["EUR.USD.IDEALPRO", "eur.idealpro.usd", "EUR.IDEALPRO.USD.CASH", "EUR.USD.IDEALPRO"]
    {
        assert_eq!(forex(spelling).engine_name(), "EUR.USD.IDEALPRO", "{spelling}");
    }
    let es = future("es.cme.usd.fut.20261218");
    assert_eq!(
        (es.root.as_str(), es.exchange.as_str(), es.expiry.as_str()),
        ("ES", "CME", "20261218")
    );
    assert_eq!(future("ES.GLOBEX.USD.FUT.202612.50").multiplier.as_deref(), Some("50"));
    assert!(parse_class_name("AAPL.SMART.USD").is_none(), "a stock name is not ours");
    assert!(parse_class_name("AAPL.SMART.USD.STK.265598").is_none(), "a pinned stock is not ours");
    let option =
        parse_class_name("SPY.SMART.USD.OPT.20261218.500.C").expect("ours").expect_err("refused");
    assert!(option.contains("no option payoff"), "{option}");
    let bad = parse_class_name("EUR.IDEALPRO.USD.CASH.12087792")
        .expect("ours")
        .expect_err("a trailing field is refused");
    assert!(bad.contains("takes no field"), "{bad}");
    assert!(
        parse_class_name("EUR.USD.IDEALPRO.CASH").expect("ours").is_err(),
        "the legacy order takes no claim"
    );
    let same = parse_class_name("EUR.IDEALPRO.EUR").expect("ours").expect_err("one currency twice");
    assert!(same.contains("two different"), "{same}");
    let other_route =
        parse_class_name("EUR.PAXOS.USD.CASH").expect("ours").expect_err("not IDEALPRO");
    assert!(other_route.contains("IDEALPRO"), "{other_route}");
    assert!(
        parse_class_name("ES.CME.USD.FUT").expect("ours").is_err(),
        "a future without its expiry"
    );
}

/// The cash-moving classes: a pair and a future, in every spelling the tree writes; a stock and a malformed name not.
#[test]
fn a_forex_pair_and_a_future_move_the_cash_a_stock_name_does_not() {
    for name in [
        "EUR.USD.IDEALPRO",
        "EUR.IDEALPRO.USD",
        "ES.CME.USD.FUT.20261218",
        "MES.CME.USD.FUT.20261218.5",
    ] {
        assert!(moves_cash(name), "{name}");
    }
    for name in [
        "AAPL.SMART.USD",
        "AAPL.SMART.USD.STK.265598",
        "SPY.SMART.USD.OPT.20261218.500.C",
        "garbage",
        "",
    ] {
        assert!(!moves_cash(name), "{name}");
    }
}

/// The venue's own warning is the IdealPro minimum: the captured EUR and USD texts, and a text that is not one.
#[test]
fn the_idealpro_minimum_is_read_out_of_the_venues_warning() {
    assert_eq!(idealpro_minimum(EUR_WARNING), Some(("EUR".to_string(), 20_000.0)));
    assert_eq!(idealpro_minimum(USD_WARNING), Some(("USD".to_string(), 25_000.0)));
    assert_eq!(idealpro_minimum(""), None);
    assert_eq!(idealpro_minimum("Your order size is below the minimum"), None);
    assert_eq!(idealpro_minimum("below the EUR 0 IdealPro minimum"), None);
    assert_eq!(idealpro_minimum("below the EURO 20000 IdealPro minimum"), None);
}

/// EUR.USD on the grid the paper gateway stated: tick 0.00005 from rule 3188, step 0.01, and the IdealPro minimum of
/// 20000 EUR (not the details' 0.01 odd-lot floor) as the minimum quantity; a class of `Fx`, no multiplier.
#[test]
fn eur_usd_is_admitted_on_the_venues_grid_with_the_idealpro_minimum() {
    let gateway = Scripted::eurusd();
    let facts = forex_facts(&gateway, &forex("EUR.USD.IDEALPRO")).expect("EUR.USD resolves");
    assert_eq!(facts.symbol, "EUR.USD.IDEALPRO");
    assert_eq!(facts.conid, 12_087_792);
    assert_eq!((facts.base.as_str(), facts.quote.as_str()), ("EUR", "USD"));
    assert_eq!(facts.properties.tick_size, 0.00005);
    assert_eq!(facts.properties.step_size, 0.01);
    assert_eq!(facts.properties.min_qty, 20_000.0);
    assert_eq!(
        facts.properties.contract_size, 0.0,
        "units of the base currency: the inert multiplier"
    );
    assert_eq!(facts.properties.asset_class, Some(AssetClass::Fx));
    assert_eq!(
        gateway.asked(),
        ["details EUR CASH", "rule 3188", "whatif 12087792 buy 1"],
        "one details request, one rule, one what-if of a single unit"
    );
}

/// USD.JPY: the base is the dollar, so the minimum is the dollar's 25000, in units of USD, and prices tick 0.005 yen.
#[test]
fn usd_jpy_takes_the_dollars_minimum_and_the_yen_tick() {
    let mut gateway = Scripted::new(vec![usdjpy_row()]);
    gateway.buy = Ok(what_if(None, USD_WARNING));
    let facts = forex_facts(&gateway, &forex("USD.JPY.IDEALPRO")).expect("USD.JPY resolves");
    assert_eq!(facts.symbol, "USD.JPY.IDEALPRO");
    assert_eq!(facts.properties.tick_size, 0.005);
    assert_eq!(facts.properties.step_size, 1.0);
    assert_eq!(facts.properties.min_qty, 25_000.0);
}

/// A venue that states no minimum for a size of one unit leaves the details' own minimum; one that states it in
/// another currency than the pair's base, or a gateway that does not answer the what-if, does not admit a guess.
#[test]
fn a_missing_or_foreign_minimum_is_not_guessed() {
    let mut none = Scripted::eurusd();
    none.buy = Ok(what_if(None, ""));
    assert_eq!(
        forex_facts(&none, &forex("EUR.USD.IDEALPRO")).expect("admitted").properties.min_qty,
        0.01
    );
    let mut foreign = Scripted::eurusd();
    foreign.buy = Ok(what_if(None, USD_WARNING));
    assert!(
        refusal(forex_facts(&foreign, &forex("EUR.USD.IDEALPRO")))
            .contains("not the pair's base currency")
    );
    let mut silent = Scripted::eurusd();
    silent.buy = Err("timed out".into());
    match forex_facts(&silent, &forex("EUR.USD.IDEALPRO")) {
        Err(FactsError::Unavailable(why)) => assert!(why.contains("what-if"), "{why}"),
        other => panic!("a silent what-if is 'not now', got {other:?}"),
    }
}

/// Every verdict of a pair's details: none, several (each conId named), another secType, another currency, no tick,
/// no size step, rules that do not nest.
#[test]
fn a_pair_whose_details_disagree_with_its_name_is_refused() {
    let name = forex("EUR.USD.IDEALPRO");
    assert!(refusal(forex_facts(&Scripted::new(vec![]), &name)).contains("no contract details"));
    let two = Scripted::new(vec![eurusd_row(), ClassRow { conid: 9, ..eurusd_row() }]);
    let why = refusal(forex_facts(&two, &name));
    assert!(
        why.contains("ambiguous") && why.contains("12087792") && why.contains("conId 9"),
        "{why}"
    );
    let stock = Scripted::new(vec![ClassRow { sec_type: "STK".into(), ..eurusd_row() }]);
    assert!(refusal(forex_facts(&stock, &name)).contains("not a CASH"));
    let other = Scripted::new(vec![ClassRow { currency: "GBP".into(), ..eurusd_row() }]);
    assert!(refusal(forex_facts(&other, &name)).contains("nothing is admitted on a disagreement"));
    let no_tick = Scripted::new(vec![ClassRow { min_tick: 0.0, ..eurusd_row() }]);
    assert!(refusal(forex_facts(&no_tick, &name)).contains("no minimum tick"));
    let no_step =
        Scripted::new(vec![ClassRow { size_increment: None, min_size: None, ..eurusd_row() }]);
    assert!(refusal(forex_facts(&no_step, &name)).contains("no size increment"));
    let magnified = Scripted::new(vec![ClassRow { price_magnifier: 100, ..eurusd_row() }]);
    assert!(refusal(forex_facts(&magnified, &name)).contains("price magnifier"));
    let mut ragged = Scripted::eurusd();
    ragged.rules.insert(
        3188,
        vec![
            PriceBand { low_edge: 0.0, increment: 0.0001 },
            PriceBand { low_edge: 1.0, increment: 0.00007 },
        ],
    );
    assert!(refusal(forex_facts(&ragged, &name)).contains("do not nest"));
}

/// ES December 2026 on the captured details: the multiplier is the contract value, the grid is the rule's tick and one
/// contract, the day is `realExpiration`, and the margin rate is the LARGER side's initial margin over the notional at the
/// delayed last (23986.46 / (7588.75 * 50)).
#[test]
fn an_e_mini_resolves_with_its_multiplier_expiry_and_the_venues_margin_rate() {
    let gateway = Scripted::es();
    let facts =
        future_facts(&gateway, &future("ES.CME.USD.FUT.20261218"), NOW_MS).expect("ES resolves");
    assert_eq!(facts.symbol, "ES.CME.USD.FUT.20261218");
    assert_eq!(facts.conid, 515_416_632);
    assert_eq!(facts.contract_value, ContractValue::linear(50.0));
    assert_eq!(facts.properties.contract_size, 50.0);
    assert_eq!(facts.properties.tick_size, 0.25);
    assert_eq!((facts.properties.step_size, facts.properties.min_qty), (1.0, 1.0));
    assert_eq!(facts.properties.asset_class, Some(AssetClass::Future));
    assert_eq!(facts.currency, "USD");
    assert_eq!(facts.last_trade_ms, 1_797_552_000_000, "2026-12-18 00:00 UTC");
    let rate = facts.margin_rate.expect("both sides and a price: a rate");
    assert!((rate - 23_986.46 / (7_588.75 * 50.0)).abs() < 1e-12, "{rate}");
    assert_eq!(
        gateway.asked(),
        [
            "details ES FUT",
            "rule 67",
            "price 515416632",
            "whatif 515416632 buy 1",
            "whatif 515416632 sell 1"
        ]
    );
}

/// A month or a legacy exchange is NOT admitted under a second spelling: the refusal names the one canonical name (the
/// exchange the venue names and the last trading day), and spelling it that way is admitted.
#[test]
fn a_month_and_a_legacy_exchange_are_refused_with_the_venues_own_spelling() {
    let month = refusal(future_facts(&Scripted::es(), &future("ES.CME.USD.FUT.202612"), NOW_MS));
    assert!(month.contains("is named `ES.CME.USD.FUT.20261218` here"), "{month}");
    let legacy =
        refusal(future_facts(&Scripted::es(), &future("ES.GLOBEX.USD.FUT.20261218"), NOW_MS));
    assert!(legacy.contains("is named `ES.CME.USD.FUT.20261218` here"), "{legacy}");
    let claimed = future_facts(&Scripted::es(), &future("ES.CME.USD.FUT.20261218.50"), NOW_MS)
        .expect("resolves");
    assert_eq!(
        claimed.symbol, "ES.CME.USD.FUT.20261218.50",
        "a claimed multiplier stays in the name"
    );
    let wrong =
        refusal(future_facts(&Scripted::es(), &future("ES.CME.USD.FUT.20261218.5"), NOW_MS));
    assert!(wrong.contains("claims a multiplier of 5"), "{wrong}");
    let day = refusal(future_facts(&Scripted::es(), &future("ES.CME.USD.FUT.20261219"), NOW_MS));
    assert!(day.contains("last trades on 20261218"), "{day}");
}

/// The expiry guard: five whole days to the last trading day are needed. The captured details are for 2026-12-18; on the
/// 13th (5 days) it is admitted, on the 14th (4 days) and on the day itself it is not.
#[test]
fn a_future_close_to_its_last_trading_day_is_not_admitted() {
    let last = 1_797_552_000_000_i64;
    let name = future("ES.CME.USD.FUT.20261218");
    assert!(future_facts(&Scripted::es(), &name, last - 5 * 86_400_000).is_ok());
    let four = refusal(future_facts(&Scripted::es(), &name, last - 4 * 86_400_000 - 1));
    assert!(four.contains("4 day(s) from now") && four.contains("name the next month"), "{four}");
    assert!(future_facts(&Scripted::es(), &name, last).is_err());
    assert!(
        future_facts(&Scripted::es(), &name, last + 86_400_000).is_err(),
        "an expired contract"
    );
}

/// Crude oil settles by delivery: refused by name before ANY request is made.
#[test]
fn a_future_that_settles_by_delivery_is_refused_before_a_request() {
    let gateway = Scripted::new(vec![cl_row()]);
    let why = refusal(future_facts(&gateway, &future("CL.NYMEX.USD.FUT.20261120"), NOW_MS));
    assert!(why.contains("settles by delivery") && why.contains("ES, MES, NQ, MNQ"), "{why}");
    assert!(gateway.asked().is_empty(), "no dial for a refused root");
}

/// A future's details are held to the name: none, several contracts for the month, no day, no multiplier. A margin or a
/// quote that cannot be had costs the margin rate and nothing else.
#[test]
fn a_futures_details_are_judged_and_a_missing_margin_only_costs_its_rate() {
    let name = future("ES.CME.USD.FUT.20261218");
    assert!(
        refusal(future_facts(&Scripted::new(vec![]), &name, NOW_MS))
            .contains("no contract details")
    );
    let two = Scripted::new(vec![es_row(), ClassRow { conid: 7, ..es_row() }]);
    assert!(refusal(future_facts(&two, &name, NOW_MS)).contains("ambiguous"));
    let no_day = Scripted::new(vec![ClassRow {
        real_expiration: String::new(),
        last_trade: "202612".into(),
        ..es_row()
    }]);
    assert!(refusal(future_facts(&no_day, &name, NOW_MS)).contains("no last trading day"));
    let no_mult = Scripted::new(vec![ClassRow { multiplier: String::new(), ..es_row() }]);
    assert!(refusal(future_facts(&no_mult, &name, NOW_MS)).contains("no usable multiplier"));
    let other_root = Scripted::new(vec![ClassRow { symbol: "NQ".into(), ..es_row() }]);
    assert!(refusal(future_facts(&other_root, &name, NOW_MS)).contains("no contract details"));

    let mut quiet = Scripted::es();
    quiet.price = Ok(None);
    assert_eq!(
        future_facts(&quiet, &name, NOW_MS).expect("admitted").margin_rate,
        None,
        "no quote: no rate"
    );
    let mut one_side = Scripted::es();
    one_side.sell = Err("timed out".into());
    let rate = future_facts(&one_side, &name, NOW_MS)
        .expect("admitted")
        .margin_rate
        .expect("the side that answered");
    assert!((rate - 23_986.46 / (7_588.75 * 50.0)).abs() < 1e-12);
}

/// The margin rate's arithmetic and its bounds: the larger side, the notional is price times multiplier, and a rate
/// outside (0, 1], a missing side or a bad price is no rate at all (the engine keeps its own).
#[test]
fn the_margin_rate_is_the_larger_side_over_the_notional_and_never_a_guess() {
    let mes = future_margin_rate(Some(2_398.33), Some(1_977.59), 7_588.75, 5.0).expect("a rate");
    assert!((mes - 2_398.33 / 37_943.75).abs() < 1e-12, "{mes}");
    assert_eq!(future_margin_rate(None, None, 100.0, 1.0), None);
    assert_eq!(future_margin_rate(Some(0.0), Some(-1.0), 100.0, 1.0), None);
    assert_eq!(future_margin_rate(Some(10.0), None, 0.0, 1.0), None);
    assert_eq!(future_margin_rate(Some(10.0), None, 100.0, 0.0), None);
    assert_eq!(
        future_margin_rate(Some(500.0), None, 100.0, 1.0),
        None,
        "5.0 is more than the notional"
    );
}

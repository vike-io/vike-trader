//! The assembly of a converting IBKR facts answer over SCRIPTED reads: the orientation goldens end to end
//! (EURUSD, USDJPY, EURGBP), the two ages (a kept grid, a fresh rate), and every refusal in words.
//!
//! DOCS-SHAPED, not measured: the scripted rows are what IBKR's pages describe (a forex pair is quoted
//! `BASE.QUOTE`, its price is quote units per base unit:
//! <https://interactivebrokers.github.io/tws-api/basic_contracts.html>; the account summary states the base
//! currency in a row's currency attribute: <https://interactivebrokers.github.io/tws-api/account_summary.html>).
//! No quote was captured from a gateway; the numbers are illustrative.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_gatewayhub_client::proto::facts_refusal_is_transient;
use vike_ibkr::fx_rate::{AccountPerQuote, QuoteRate};
use vike_model::SymbolProperties;

use super::*;

const NOW: i64 = 1_700_000_000_123;

fn stock(name: &str) -> StockFacts {
    StockFacts {
        symbol: name.into(),
        conid: 14_000,
        properties: SymbolProperties {
            tick_size: 0.01,
            step_size: 1.0,
            min_qty: 1.0,
            ..SymbolProperties::default()
        },
        price_magnifier: 1,
        tick_grid: vike_ibkr::any_symbol::TickGrid::default(),
    }
}

fn stock_with_magnifier(name: &str, price_magnifier: i32) -> StockFacts {
    StockFacts { price_magnifier, ..stock(name) }
}

/// **A FOREIGN stock whose contract price magnifier is not 1 is refused on the converting path, in words**, whether
/// the gateway reported a pence-style 100 or left the field unset (0); the same stock at 1 is granted. The grid
/// kept from the first ask carries its record into the second: a refusal does not depend on whether the contract
/// was looked up on this ask.
#[test]
fn a_foreign_stock_with_a_price_magnifier_is_refused_on_the_converting_path() {
    for magnifier in [0, 100, 2] {
        let facts = IbkrFacts::default();
        for ask in 0..2 {
            let why = quoted_facts(
                &facts,
                "SAP.SMART.EUR",
                || Ok(stock_with_magnifier("SAP.SMART.EUR", magnifier)),
                |_| Ok(rate("EUR", 1.10, "EUR.USD")),
            )
            .expect_err("refused");
            assert!(
                why.starts_with("ibkr: ")
                    && why.contains(&format!("price magnifier of {magnifier}"))
                    && why.contains("100 times too high"),
                "{magnifier} ask {ask}: {why}"
            );
            assert!(!facts_refusal_is_transient(&why), "a verdict, not a moment: {why}");
        }
    }
    let facts = IbkrFacts::default();
    let got = quoted_facts(
        &facts,
        "SAP.SMART.EUR",
        || Ok(stock_with_magnifier("SAP.SMART.EUR", 1)),
        |_| Ok(rate("EUR", 1.10, "EUR.USD")),
    )
    .expect("a magnifier of 1 is granted");
    assert_eq!(got.quote_rate, Some(1.10));
}

/// **A stock in the account's BASE currency is never refused for its magnifier**, 0 (unset) or 100 alike: it is
/// the account's own money, nothing converts, and it is admitted as before (finding of the docs review: a gateway
/// that reports 0 for every ordinary stock must not refuse them on a converting account either).
#[test]
fn a_stock_in_the_base_currency_is_admitted_whatever_its_price_magnifier() {
    for magnifier in [0, 1, 100] {
        let facts = IbkrFacts::default();
        let got = quoted_facts(
            &facts,
            "AAPL.SMART.USD",
            || Ok(stock_with_magnifier("AAPL.SMART.USD", magnifier)),
            |_| Ok(own_currency()),
        )
        .unwrap_or_else(|why| panic!("{magnifier}: {why}"));
        assert_eq!((got.quote_ccy, got.quote_rate, got.account_ccy), (None, None, None));
        assert_eq!(got.properties.tick_size, 0.01);
    }
}

fn rate(ccy: &str, account_per_quote: f64, pair: &str) -> RateAnswer {
    RateAnswer {
        base: "USD".into(),
        rate: Some(QuoteRate {
            quote_ccy: ccy.into(),
            account_per_quote: AccountPerQuote::new(account_per_quote).expect("a rate"),
            as_of_ms: NOW,
            pair: pair.into(),
        }),
    }
}

fn own_currency() -> RateAnswer {
    RateAnswer { base: "USD".into(), rate: None }
}

/// **A EUR stock on a USD account carries its currency and the rate as it stands**: a euro is MORE than a
/// dollar, so 10,000 euros are 11,000 dollars.
#[test]
fn a_euro_stock_carries_more_than_a_dollar_per_euro() {
    let facts = IbkrFacts::default();
    let got = quoted_facts(
        &facts,
        "SAP.SMART.EUR",
        || Ok(stock("SAP.SMART.EUR")),
        |ccy| {
            assert_eq!(ccy, "EUR", "the rate is asked for the NAME's currency");
            Ok(rate("EUR", 1.10, "EUR.USD"))
        },
    )
    .expect("granted");
    assert_eq!(got.quote_ccy.as_deref(), Some("EUR"));
    assert_eq!(got.quote_rate, Some(1.10));
    assert_eq!(got.rate_as_of_ms, Some(NOW));
    assert_eq!(got.account_ccy.as_deref(), Some("USD"), "the rate says which ledger it is in");
    assert!((got.quote_rate.unwrap() * 10_000.0 - 11_000.0).abs() < 1e-9);
    assert_eq!(got.properties.tick_size, 0.01);
    assert_eq!(got.contract_value, None, "a share: linear, 1.0");
}

/// **A JPY stock carries `1/150.10`, never `150.10`**: 10,000 yen are 66.62 dollars. The design's USDJPY golden.
#[test]
fn a_yen_stock_carries_a_fraction_of_a_dollar_per_yen_not_yen_per_dollar() {
    let facts = IbkrFacts::default();
    let got = quoted_facts(
        &facts,
        "7203.SMART.JPY",
        || Ok(stock("7203.SMART.JPY")),
        |_| Ok(rate("JPY", 1.0 / 150.10, "USD.JPY")),
    )
    .expect("granted");
    let per_yen = got.quote_rate.expect("a rate");
    assert!(per_yen < 0.01 && per_yen > 0.006, "{per_yen} is yen-per-dollar, the flipped rate");
    assert!((per_yen * 10_000.0 - 66.622).abs() < 1e-3);
    assert!((per_yen * 100_000.0 * 150.10 - 100_000.0).abs() < 1e-6, "100,000 dollars of yen");
}

/// A stock quoted in the account's own base currency carries NO currency and NO rate: nothing converts, and the
/// node reads that as the ledger's own money.
#[test]
fn a_stock_in_the_base_currency_carries_no_conversion() {
    let facts = IbkrFacts::default();
    let got = quoted_facts(
        &facts,
        "AAPL.SMART.USD",
        || Ok(stock("AAPL.SMART.USD")),
        |_| Ok(own_currency()),
    )
    .expect("granted");
    assert_eq!((got.quote_ccy, got.quote_rate, got.rate_as_of_ms), (None, None, None));
    assert_eq!(got.account_ccy, None, "no rate, no ledger currency to state");
}

/// **The two ages**: the grid is kept (one fetch for two asks), the rate is read on EVERY ask and the second
/// answer carries the second reading.
#[test]
fn the_grid_is_kept_and_the_rate_is_read_fresh_on_every_ask() {
    let facts = IbkrFacts::default();
    let fetches = AtomicUsize::new(0);
    let reads = Cell::new(0.0_f64);
    for want in [1.10, 1.12] {
        reads.set(want);
        let got = quoted_facts(
            &facts,
            "SAP.SMART.EUR",
            || {
                fetches.fetch_add(1, Ordering::SeqCst);
                Ok(stock("SAP.SMART.EUR"))
            },
            |_| Ok(rate("EUR", reads.get(), "EUR.USD")),
        )
        .expect("granted");
        assert_eq!(got.quote_rate, Some(want));
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 1, "the grid is looked up once");
}

/// The converting answers are kept apart from the plain ones: a refusal the plain path remembered for a name
/// (a stock of another currency) is not served to the converting path.
#[test]
fn the_converting_keep_is_not_the_plain_ones() {
    let plain = IbkrFacts::default();
    let converting = IbkrFacts::default();
    let refused = plain.facts("SAP.SMART.EUR", || Err(FactsError::Refused("a EUR stock".into())));
    assert!(refused.is_err());
    let got = quoted_facts(
        &converting,
        "SAP.SMART.EUR",
        || Ok(stock("SAP.SMART.EUR")),
        |_| Ok(rate("EUR", 1.10, "EUR.USD")),
    );
    assert!(got.is_ok());
}

/// A name that is no stock name is refused in the plain path's words BEFORE the budget is spent, anything is
/// fetched or any rate is read; a forex pair is refused as a CASH position.
#[test]
fn a_bad_name_asks_nothing() {
    let facts = IbkrFacts::default();
    for bad in ["SAP", "SAP.NASDAQ.EUR", "EUR.IDEALPRO.USD", "SAP.SMART.EU"] {
        let why = quoted_facts(
            &facts,
            bad,
            || panic!("{bad}: nothing is fetched"),
            |_| panic!("{bad}: no rate is read"),
        )
        .expect_err(bad);
        assert!(why.contains("not an IBKR stock name"), "{bad}: {why}");
        assert!(!facts_refusal_is_transient(&why), "{bad}: {why}");
    }
    let why =
        quoted_facts(&facts, "EUR.IDEALPRO.USD", || panic!(), |_| panic!()).expect_err("forex");
    assert!(why.contains("CASH position"), "{why}");
}

/// **A minor-unit stock is refused BEFORE its grid is fetched** (finding 6 of the second IBKR currency review): the
/// grid is a dial of the data lane's hourly budget, and for a London, Tel Aviv or Johannesburg name it was spent
/// every ten minutes on an answer the rate then refused. Nothing is fetched and no rate is read; the words are
/// the rate read's own (so the node reads one sentence whichever refused it) and are a verdict, not a failure.
#[test]
fn a_minor_unit_stock_is_refused_before_its_grid_is_fetched() {
    let facts = IbkrFacts::default();
    for name in ["VOD.SMART.GBP", "TEVA.SMART.ILS", "NPN.SMART.ZAR"] {
        let why = quoted_facts(
            &facts,
            name,
            || panic!("{name}: the grid is not fetched"),
            |_| panic!("{name}: no rate is read"),
        )
        .expect_err(name);
        assert!(why.starts_with("ibkr: ") && why.contains("100 times too high"), "{name}: {why}");
        assert!(!facts_refusal_is_transient(&why), "{name}: {why}");
    }
}

/// A grid that could not be had reads no rate; a refused stock is the plain refusal.
#[test]
fn no_grid_no_rate_read() {
    let facts = IbkrFacts::default();
    let why = quoted_facts(
        &facts,
        "SAP.SMART.EUR",
        || Err(FactsError::Refused("SAP is ambiguous".into())),
        |_| panic!("no rate is read for a stock that is not admitted"),
    )
    .expect_err("refused");
    assert_eq!(why, "SAP is ambiguous");
    let why = quoted_facts(
        &facts,
        "SAP2.SMART.EUR",
        || Err(FactsError::Unavailable("no link".into())),
        |_| panic!("no rate is read"),
    )
    .expect_err("unavailable");
    assert!(facts_refusal_is_transient(&why) && why.contains("no link"), "{why}");
}

/// **A rate the gateway could not give is a failure of the RATE** (transient: the node does not remember it as
/// a verdict on the symbol), naming the currency and the reason; a rate refused by name (no forex pair, a
/// minor-unit currency) is the definitive sentence, unchanged.
#[test]
fn a_rate_that_cannot_be_had_is_worded_as_the_rates_failure() {
    let facts = IbkrFacts::default();
    let why = quoted_facts(
        &facts,
        "SAP.SMART.EUR",
        || Ok(stock("SAP.SMART.EUR")),
        |_| Err(FactsError::Unavailable("the EUR.USD quote is delayed data, not real-time".into())),
    )
    .expect_err("no rate");
    assert!(why.contains(FACTS_RATE_UNAVAILABLE) && why.contains("no rate for EUR"), "{why}");
    assert!(why.contains("delayed data"), "{why}");
    assert!(facts_refusal_is_transient(&why), "{why}");

    let why = quoted_facts(
        &facts,
        "SAP.SMART.EUR",
        || Ok(stock("SAP.SMART.EUR")),
        |_| Err(FactsError::Refused("ibkr: IBKR lists no forex pair between EUR and USD".into())),
    )
    .expect_err("refused");
    assert_eq!(why, "ibkr: IBKR lists no forex pair between EUR and USD");
    assert!(!facts_refusal_is_transient(&why));
}

/// The base currency is asked until the venue states it, a refusal is final, and a venue that never answers
/// leaves the account unanchored when the bound is spent.
#[test]
fn the_base_currency_is_asked_until_stated_and_given_up_on_when_the_bound_is_spent() {
    let roomy = Duration::from_secs(5);
    let mut asked = 0;
    let got = read_base_currency(
        |_| {
            asked += 1;
            if asked < 3 {
                Err(FactsError::Unavailable("gate busy".into()))
            } else {
                Ok("EUR".into())
            }
        },
        roomy,
        Duration::ZERO,
    );
    assert_eq!((got.as_deref(), asked), (Some("EUR"), 3));

    let mut asked = 0;
    let got = read_base_currency(
        |_| {
            asked += 1;
            Err(FactsError::Refused("a cpapi account opens no ibapi socket".into()))
        },
        roomy,
        Duration::ZERO,
    );
    assert_eq!((got, asked), (None, 1), "a refusal is final");
}

/// **The start's read is bounded as a whole** (the start holds the adapter lock and the node waits for its
/// reply): asks are spaced by the pause, each is handed only the time that is left, no pause is slept that
/// leaves no room for another ask, and the total stays inside the bound however slow the gateway is. The real
/// bound is [`BASE_BOUND`] (8 s); before it the read could take 187 s (five asks of 35 s and the pauses).
#[test]
fn the_start_s_base_currency_read_never_outlives_its_bound() {
    assert!(BASE_BOUND <= Duration::from_secs(8), "the start waits for this");
    let total = Duration::from_millis(300);
    let pause = Duration::from_millis(100);

    // A refused dial answers at once: asks fit at 0, 100 and 200 ms, and the pause that would leave nothing
    // is not slept.
    let mut asked = 0;
    let began = Instant::now();
    let got = read_base_currency(
        |_| {
            asked += 1;
            Err(FactsError::Unavailable("gate busy".into()))
        },
        total,
        pause,
    );
    assert_eq!(got, None);
    assert!((2..=3).contains(&asked), "{asked} asks inside the bound");
    assert!(began.elapsed() < total, "no sleep past the last ask: {:?}", began.elapsed());

    // A gateway that takes the whole of what it is given: one ask, and it was given the bound, not 35 s.
    let mut given = Vec::new();
    let began = Instant::now();
    let got = read_base_currency(
        |left| {
            given.push(left);
            std::thread::sleep(left);
            Err(FactsError::Unavailable("silent".into()))
        },
        total,
        pause,
    );
    assert_eq!(got, None);
    assert_eq!(given.len(), 1, "{given:?}");
    assert!(given[0] <= total, "the ask was handed at most the bound: {given:?}");
    assert!(began.elapsed() < total + Duration::from_millis(400), "{:?}", began.elapsed());
}

fn pair_facts(quote: &str) -> ClassFacts {
    ClassFacts::Forex(vike_ibkr::any_symbol_forex_future::ForexFacts {
        symbol: format!("USD.{quote}.IDEALPRO"),
        conid: 15_016_059,
        base: "USD".into(),
        quote: quote.into(),
        properties: SymbolProperties {
            tick_size: 0.005,
            step_size: 1.0,
            min_qty: 25_000.0,
            ..SymbolProperties::default()
        },
    })
}

/// **A pair quoted in the account's own money carries no conversion; one quoted in another carries the rate as it
/// stands** (`USD.JPY` on a USD account is `1/157.58` dollars per yen, never 157.58), read for the PAIR'S QUOTE
/// currency, and a minor-unit money is refused before the grid is fetched.
#[test]
fn a_pair_is_converted_by_its_quote_currency() {
    let facts = IbkrFacts::default();
    let own = quoted_class_facts(
        &facts,
        "EUR.USD.IDEALPRO",
        "USD",
        || Ok(pair_facts("USD")),
        |ccy| {
            assert_eq!(ccy, "USD");
            Ok(own_currency())
        },
    )
    .expect("granted");
    assert_eq!((own.quote_ccy, own.quote_rate, own.account_ccy), (None, None, None));

    let jpy = quoted_class_facts(
        &facts,
        "USD.JPY.IDEALPRO",
        "JPY",
        || Ok(pair_facts("JPY")),
        |ccy| {
            assert_eq!(ccy, "JPY", "the rate is asked for the quote currency");
            Ok(rate("JPY", 1.0 / 157.582, "USD.JPY"))
        },
    )
    .expect("granted");
    assert_eq!(jpy.quote_ccy.as_deref(), Some("JPY"));
    let per_yen = jpy.quote_rate.expect("a rate");
    assert!(per_yen < 0.01 && per_yen > 0.006, "{per_yen} is yen-per-dollar, the flipped rate");
    assert_eq!(jpy.account_ccy.as_deref(), Some("USD"));
    assert_eq!(jpy.properties.min_qty, 25_000.0);

    let fetched = Cell::new(false);
    let why = quoted_class_facts(
        &IbkrFacts::default(),
        "USD.ZAR.IDEALPRO",
        "ZAR",
        || {
            fetched.set(true);
            Ok(pair_facts("ZAR"))
        },
        |_| Ok(own_currency()),
    )
    .expect_err("a minor-unit money");
    assert!(why.starts_with("ibkr: ") && !fetched.get(), "{why}");
}

/// A rate the gateway cannot give (the paper account streams no forex quote through the API: `[10089]`) is a failure
/// of the RATE, worded as the node's "not now"; a refusal of the pair itself is a verdict.
#[test]
fn a_pair_whose_rate_cannot_be_read_is_not_now_and_a_refused_pair_is_a_verdict() {
    let facts = IbkrFacts::default();
    let why = quoted_class_facts(
        &facts,
        "USD.JPY.IDEALPRO",
        "JPY",
        || Ok(pair_facts("JPY")),
        |_| Err(FactsError::Unavailable("the USD.JPY quote stream ended with [10089]".into())),
    )
    .expect_err("no rate");
    assert!(facts_refusal_is_transient(&why), "{why}");
    assert!(why.contains("no rate for JPY") && why.contains("10089"), "{why}");
    let verdict = quoted_class_facts(
        &IbkrFacts::default(),
        "CL.NYMEX.USD.FUT.20261120",
        "USD",
        || Err(FactsError::Refused("settles by delivery".into())),
        |_| Ok(own_currency()),
    )
    .expect_err("refused");
    assert!(!facts_refusal_is_transient(&verdict), "{verdict}");
}

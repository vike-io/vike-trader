//! The rate builder over a SCRIPTED gateway: the orientation (the 22,500x bug) with the design's goldens,
//! the order IBKR lists a pair in, and every quote that is not a rate.
//!
//! DOCS-SHAPED, not measured: the scripted books are what IBKR's pages describe (a forex pair is
//! `symbol = BASE`, `currency = QUOTE`, `exchange = IDEALPRO`, its price is QUOTE units per BASE unit:
//! <https://interactivebrokers.github.io/tws-api/basic_contracts.html>; error 200 is "No security definition
//! has been found for the request": <https://interactivebrokers.github.io/tws-api/message_codes.html>;
//! delayed data arrives when no real-time subscription is held:
//! <https://interactivebrokers.github.io/tws-api/market_data_type.html>). No quote was captured from a
//! gateway. The numbers are illustrative.

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;

const T: i64 = 1_700_000_000_123;

fn book(bid: f64, ask: f64) -> FxBook {
    FxBook { bid, ask, delay: DataDelay::RealTime }
}

/// A gateway that lists the pairs it was told to, answers each with its book, and counts what it was asked.
struct Scripted {
    listed: HashMap<(String, String), Result<FxBook, FxFail>>,
    asked: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(pairs: &[(&str, &str, FxBook)]) -> Self {
        Self {
            listed: pairs
                .iter()
                .map(|(base, quote, b)| (((*base).to_string(), (*quote).to_string()), Ok(*b)))
                .collect(),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn failing(mut self, base: &str, quote: &str, why: &str) -> Self {
        self.listed.insert(
            (base.to_string(), quote.to_string()),
            Err(FxFail::Unavailable(why.to_string())),
        );
        self
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("asked").clone()
    }
}

impl FxQuotes for Scripted {
    fn book(&self, base: &str, quote: &str) -> Result<FxBook, FxFail> {
        self.asked.lock().expect("asked").push(format!("{base}.{quote}"));
        self.listed
            .get(&(base.to_string(), quote.to_string()))
            .cloned()
            .unwrap_or(Err(FxFail::NoSuchPair))
    }
}

fn rate(fx: &Scripted, account: &str, quote: &str) -> QuoteRate {
    quote_rate(fx, account, quote, T).unwrap_or_else(|e| panic!("{quote} on {account}: {e:?}"))
}

fn unavailable_words(result: Result<QuoteRate, FactsError>) -> String {
    match result {
        Err(FactsError::Unavailable(why)) => why,
        other => panic!("expected an unavailable answer, got {other:?}"),
    }
}

fn refused_words(result: Result<QuoteRate, FactsError>) -> String {
    match result {
        Err(FactsError::Refused(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// **A EUR stock on a USD ledger: the pair `EUR.USD` is read as it stands.** A euro is worth MORE than a
/// dollar (1.10 dollars): 10,000 euros of PnL are 11,000 dollars, not 9,090.
#[test]
fn eur_on_a_usd_ledger_is_more_than_a_dollar_per_euro() {
    let fx = Scripted::new(&[("EUR", "USD", book(1.0999, 1.1001))]);
    let r = rate(&fx, "USD", "EUR");
    assert_eq!(fx.asked(), ["EUR.USD"], "the market's order is asked first and answers");
    assert!((r.account_per_quote.get() - 1.10).abs() < 1e-12, "{}", r.account_per_quote.get());
    assert!((r.account_per_quote.to_account(10_000.0) - 11_000.0).abs() < 1e-8);
    assert_eq!((r.quote_ccy.as_str(), r.pair.as_str(), r.as_of_ms), ("EUR", "EUR.USD", T));
}

/// **A JPY stock on a USD ledger: the pair `USD.JPY` is INVERTED.** A yen is a FRACTION of a dollar:
/// `1/150.10`, never `150.10`. The design's golden: 10,000 yen of PnL are 66.62 dollars, not 10,000 and not
/// 1,501,000 (the flipped rate).
#[test]
fn jpy_on_a_usd_ledger_is_a_fraction_of_a_dollar_per_yen_not_yen_per_dollar() {
    let fx = Scripted::new(&[("USD", "JPY", book(150.09, 150.11))]);
    let r = rate(&fx, "USD", "JPY");
    assert_eq!(fx.asked(), ["USD.JPY"]);
    let per_yen = r.account_per_quote.get();
    assert!(per_yen < 0.01 && per_yen > 0.006, "{per_yen} is yen-per-dollar, the flipped rate");
    assert!((per_yen - 1.0 / 150.10).abs() < 1e-12);
    let dollars = r.account_per_quote.to_account(10_000.0);
    assert!((dollars - 66.622).abs() < 1e-3, "{dollars}");
    assert!(dollars < 100.0, "nowhere near the 10,000 an unconverted ledger books");
    assert!((r.account_per_quote.to_account(100_000.0 * 150.10) - 100_000.0).abs() < 1e-6);
    assert_eq!(r.pair, "USD.JPY");
}

/// **A USD stock on a EUR ledger: the same pair `EUR.USD`, now INVERTED**: a dollar is worth `1/1.10` euros.
#[test]
fn usd_on_a_eur_ledger_inverts_eur_usd() {
    let fx = Scripted::new(&[("EUR", "USD", book(1.0999, 1.1001))]);
    let r = rate(&fx, "EUR", "USD");
    assert!((r.account_per_quote.get() - 1.0 / 1.10).abs() < 1e-12);
    assert!(
        (r.account_per_quote.to_account(1_100.0) - 1_000.0).abs() < 1e-8,
        "1,100 USD = 1,000 EUR"
    );
}

/// Orientation across a cross and across the convention list, both directions of every pair: the rate of
/// `A per Q` and the rate of `Q per A` are reciprocals, whichever way IBKR lists the pair. (GBP is a ledger or
/// a quote of the RATE function here; a GBP STOCK is refused apart, by `minor_unit_refusal`.)
#[test]
fn the_two_directions_of_every_pair_are_reciprocals() {
    let fx = Scripted::new(&[
        ("EUR", "GBP", book(0.8499, 0.8501)),
        ("GBP", "USD", book(1.3499, 1.3501)),
        ("USD", "JPY", book(150.09, 150.11)),
        ("EUR", "JPY", book(164.0, 164.2)),
    ]);
    for (a, q) in [("USD", "GBP"), ("EUR", "GBP"), ("USD", "JPY"), ("EUR", "JPY")] {
        let forward = rate(&fx, a, q).account_per_quote.get();
        let back = rate(&fx, q, a).account_per_quote.get();
        assert!(
            (forward * back - 1.0).abs() < 1e-12,
            "{a} per {q} = {forward}, {q} per {a} = {back}"
        );
    }
    // EURGBP's design golden: 85,000 pounds are 114,750 dollars at 1.35.
    let pounds = rate(&fx, "USD", "GBP").account_per_quote;
    assert!(pounds.get() > 1.0, "{}: pounds-per-dollar, the flipped rate", pounds.get());
    assert!((pounds.to_account(85_000.0) - 114_750.0).abs() < 1e-6);
    // GBP per EUR on a GBP ledger is the EUR.GBP price as it stands (0.85), EUR per GBP its inverse.
    assert!((rate(&fx, "GBP", "EUR").account_per_quote.get() - 0.85).abs() < 1e-12);
    assert!((rate(&fx, "EUR", "GBP").account_per_quote.get() - 1.0 / 0.85).abs() < 1e-12);
}

/// The market convention is only the first ask: a pair IBKR lists the OTHER way round is found by the second,
/// with the orientation still right, and a pair listed neither way is a refusal in words (never a rate).
#[test]
fn a_pair_listed_the_other_way_round_is_found_and_one_listed_neither_way_is_refused() {
    // Two currencies the convention does not rank order alphabetically: the first ask is `NOK.SEK`.
    let listed_as_asked = Scripted::new(&[("NOK", "SEK", book(0.9499, 0.9501))]);
    let r = rate(&listed_as_asked, "NOK", "SEK");
    assert_eq!(listed_as_asked.asked(), ["NOK.SEK"]);
    assert!((r.account_per_quote.get() - 1.0 / 0.95).abs() < 1e-12, "NOK per SEK is the inverse");

    // The script lists the opposite order from the convention: two asks, the right orientation.
    let reversed = Scripted::new(&[("JPY", "USD", book(0.006659, 0.006661))]);
    let r = rate(&reversed, "USD", "JPY");
    assert_eq!(reversed.asked(), ["USD.JPY", "JPY.USD"]);
    assert!(
        (r.account_per_quote.get() - 0.00666).abs() < 1e-9,
        "JPY.USD as it stands: dollars per yen"
    );
    assert_eq!(r.pair, "JPY.USD");

    let none = Scripted::new(&[]);
    let why = refused_words(quote_rate(&none, "USD", "XYZ", T));
    assert!(why.contains("lists no forex pair between XYZ and USD"), "{why}");
    assert_eq!(none.asked(), ["USD.XYZ", "XYZ.USD"], "both orders were asked before giving up");
}

/// The convention ranks EUR, GBP, AUD, NZD, USD, CAD, CHF, CNH, JPY and puts every other code after them.
#[test]
fn the_conventional_pair_follows_the_markets_order() {
    for (a, b, base, quote) in [
        ("USD", "EUR", "EUR", "USD"),
        ("EUR", "USD", "EUR", "USD"),
        ("JPY", "USD", "USD", "JPY"),
        ("GBP", "EUR", "EUR", "GBP"),
        ("CHF", "JPY", "CHF", "JPY"),
        ("SEK", "USD", "USD", "SEK"),
        ("NOK", "SEK", "NOK", "SEK"),
        ("SEK", "NOK", "NOK", "SEK"),
    ] {
        assert_eq!(conventional_pair(a, b), (base.to_string(), quote.to_string()), "{a} {b}");
    }
}

/// Every quote that is not a rate is an `Unavailable` answer that says which: a missing side, a zero or
/// negative or non-finite price, a crossed book, a spread past [`MAX_SPREAD`], and a stream that is delayed or
/// frozen. None of them is a rate, and none is `1.0`.
#[test]
fn a_quote_that_is_not_a_market_is_never_a_rate() {
    let words = |b: FxBook| {
        let fx = Scripted::new(&[("EUR", "USD", b)]);
        unavailable_words(quote_rate(&fx, "USD", "EUR", T))
    };
    for (b, needle) in [
        (book(0.0, 1.1), "no usable two-sided price"),
        (book(1.1, 0.0), "no usable two-sided price"),
        (book(-1.0, -1.0), "no usable two-sided price"),
        (book(f64::NAN, 1.1), "no usable two-sided price"),
        (book(1.1, f64::INFINITY), "no usable two-sided price"),
        (book(1.1002, 1.1001), "crossed"),
        (book(1.0, 1.1), "wider than"),
        (FxBook { delay: DataDelay::DELAYED, ..book(1.0999, 1.1001) }, "delayed data"),
        (FxBook { delay: DataDelay::Frozen, ..book(1.0999, 1.1001) }, "frozen data"),
    ] {
        let why = words(b);
        assert!(why.contains(needle), "{b:?}: {why}");
        assert!(why.contains("EUR.USD"), "names the pair: {why}");
    }
    // the edge: just inside the limit is a market, just outside is not
    let mid = 1.1;
    for (scale, inside) in [(0.99, true), (1.01, false)] {
        let half = mid * MAX_SPREAD * scale / 2.0;
        let fx = Scripted::new(&[("EUR", "USD", book(mid - half, mid + half))]);
        assert_eq!(quote_rate(&fx, "USD", "EUR", T).is_ok(), inside, "{scale} of the limit");
    }
}

/// A read that failed is "not now" with the venue's words and the pair named; the second order is not tried
/// after a failure that is not "no such pair".
#[test]
fn a_failed_read_is_not_now_and_does_not_try_the_other_order() {
    let fx = Scripted::new(&[]).failing("EUR", "USD", "not subscribed to market data");
    let why = unavailable_words(quote_rate(&fx, "USD", "EUR", T));
    assert!(why.contains("EUR.USD") && why.contains("not subscribed"), "{why}");
    assert_eq!(fx.asked(), ["EUR.USD"], "an outage is not a missing pair");
    // ...and it also fails when the SECOND order is the one that fails
    let fx = Scripted::new(&[]).failing("JPY", "USD", "no link");
    let why = unavailable_words(quote_rate(&fx, "USD", "JPY", T));
    assert!(why.contains("JPY.USD") && why.contains("no link"), "{why}");
    assert_eq!(fx.asked(), ["USD.JPY", "JPY.USD"]);
}

/// What is not a currency pair question is refused before any ask: a code that is not three letters, and one
/// currency twice. Codes are read trimmed and upper-cased.
#[test]
fn bad_codes_and_one_currency_twice_are_refused_before_any_ask() {
    let fx = Scripted::new(&[("EUR", "USD", book(1.0999, 1.1001))]);
    for (account, quote) in
        [("US", "EUR"), ("USD", "EU"), ("", "EUR"), ("USD", ""), ("USD1", "EUR")]
    {
        let why = refused_words(quote_rate(&fx, account, quote, T));
        assert!(why.contains("not a currency code"), "{account}/{quote}: {why}");
    }
    let why = refused_words(quote_rate(&fx, "usd", " USD ", T));
    assert!(why.contains("nothing converts"), "{why}");
    assert!(fx.asked().is_empty());
    let r = quote_rate(&fx, " usd", "eur ", T).expect("read trimmed and upper-cased");
    assert_eq!(r.quote_ccy, "EUR");
}

/// A rate cannot be built from a non-rate.
#[test]
fn a_rate_cannot_be_built_from_a_non_rate() {
    for bad in [0.0, -1.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(AccountPerQuote::new(bad), None, "{bad}");
    }
    assert_eq!(AccountPerQuote::new(0.0066).map(AccountPerQuote::get), Some(0.0066));
}

fn jpy(per_yen: f64) -> QuoteRate {
    QuoteRate {
        quote_ccy: "JPY".into(),
        account_per_quote: AccountPerQuote::new(per_yen).expect("a rate"),
        as_of_ms: T,
        pair: "USD.JPY".into(),
    }
}

/// **The sanity gate** (V1 of the vendor-doc verification): a read of a currency that moved by more than the
/// ENGINE'S step (1.5x) since the one accepted a moment ago is not a rate. The yen golden: `1/150` accepted, then
/// `150` (the flipped pair, 22,500x) is refused in words; a market's ordinary moves and the engine's own step
/// pass.
#[test]
fn a_flipped_read_between_two_reads_of_one_currency_is_refused() {
    let was = jpy(1.0 / 150.0);
    let minute = Duration::from_secs(60);
    for ordinary in [1.0 / 150.4, 1.0 / 149.2, 1.0 / 159.0] {
        assert_eq!(check_against_previous(&was, minute, &jpy(ordinary)), Ok(()), "{ordinary}");
    }
    let FactsError::Unavailable(why) =
        check_against_previous(&was, minute, &jpy(150.0)).expect_err("the flipped pair")
    else {
        panic!("a quote that is not a rate is the moment's, not a verdict")
    };
    assert!(why.contains("22500.0 times") && why.contains("USD.JPY"), "{why}");
    assert!(why.contains(&format!("{}x step", vike_exec::MAX_QUOTE_RATE_STEP)), "{why}");
    // The boundary is the engine's: 1.4x passes, 1.6x does not, in either direction.
    for (step, ok) in [(1.4, true), (1.6, false), (1.0 / 1.4, true), (1.0 / 1.6, false)] {
        let got = check_against_previous(&was, minute, &jpy(was.account_per_quote.get() * step));
        assert_eq!(got.is_ok(), ok, "step {step}");
    }
}

/// A reference older than the engine's staleness window is not compared (a real move is possible after that
/// long, and nothing may lock the source out), and one currency's rate is never judged against another's.
#[test]
fn an_old_reference_and_another_currency_are_not_compared() {
    let was = jpy(1.0 / 150.0);
    let window =
        Duration::from_millis(u64::try_from(vike_exec::DEFAULT_QUOTE_RATE_TTL_MS).unwrap());
    assert!(check_against_previous(&was, window, &jpy(150.0)).is_err(), "inside the window");
    assert_eq!(
        check_against_previous(&was, window + Duration::from_secs(1), &jpy(150.0)),
        Ok(()),
        "past the window the engine's own guard judges"
    );
    let eur = QuoteRate { quote_ccy: "EUR".into(), pair: "EUR.USD".into(), ..jpy(1.10) };
    assert_eq!(check_against_previous(&was, Duration::from_secs(1), &eur), Ok(()));
}

/// The three currencies IBKR may quote in a minor unit are refused by name; the whole-unit ones are not.
#[test]
fn a_stock_in_a_minor_unit_currency_is_refused_by_name() {
    for (ccy, minor) in [("GBP", "pence"), ("gbp", "pence"), ("ILS", "agorot"), ("ZAR", "cents")] {
        let why = minor_unit_refusal(ccy).unwrap_or_else(|| panic!("{ccy} is refused"));
        assert!(why.contains(minor) && why.contains("100 times too high"), "{ccy}: {why}");
    }
    for ccy in ["EUR", "JPY", "CHF", "CAD", "AUD", "HKD", "SEK", "NOK", "DKK", "SGD", "MXN", "USD"]
    {
        assert_eq!(minor_unit_refusal(ccy), None, "{ccy}");
    }
}

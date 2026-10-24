//! The IG rate source over market-details bodies in the shape IG's reference DOCUMENTS
//! (<https://labs.ig.com/reference/markets-epic.html>: `instrument.currencies[]` with `code`, `symbol`,
//! `baseExchangeRate`, `exchangeRate`, `isDefault`; `snapshot.bid` / `offer` / `marketStatus`). DOCS-SHAPED:
//! no body was ever captured from the demo gateway, and the reference states neither the direction nor the
//! meaning of the two rates, so every number below is illustrative and what these tests prove is the HANDLING
//! of the shape and of the orientation CHECK, never IG's own figures. The goldens are decision 0183's: a yen is
//! a fraction of a dollar (`1/150`, never `150`), a pound is more than one.

use serde_json::{Value, json};

use crate::market_facts::{IgFactsError, IgQuotedFacts, market_facts, quoted_market_facts};
use crate::quote_rate::Reading;

const ASKED_MS: i64 = 1_700_000_000_123;
const USDJPY: &str = "CS.D.USDJPY.MINI.IP";
const EURGBP: &str = "CS.D.EURGBP.MINI.IP";
const EURUSD: &str = "CS.D.EURUSD.MINI.IP";
const GBPUSD: &str = "CS.D.GBPUSD.MINI.IP";

/// A market body: `rows` are `(code, exchangeRate)` (the `baseExchangeRate` is made up beside them),
/// `price` the `(bid, offer)` of the snapshot.
fn market(
    epic: &str,
    pip_means: &str,
    contract: &str,
    per_pip: &str,
    rows: &[(&str, f64)],
    price: Option<(f64, f64)>,
) -> Value {
    let currencies: Vec<Value> = rows
        .iter()
        .map(|(code, rate)| {
            json!({
                "code": code, "symbol": "x", "baseExchangeRate": rate * 1.001,
                "exchangeRate": rate, "isDefault": false
            })
        })
        .collect();
    let snapshot = price.map_or_else(
        || json!({ "marketStatus": "CLOSED", "bid": null, "offer": null }),
        |(bid, offer)| json!({ "marketStatus": "TRADEABLE", "bid": bid, "offer": offer }),
    );
    json!({
        "instrument": {
            "epic": epic, "expiry": "-", "type": "CURRENCIES", "unit": "CONTRACTS", "lotSize": 1.0,
            "currencies": currencies, "onePipMeans": pip_means, "contractSize": contract,
            "valueOfOnePip": per_pip
        },
        "dealingRules": { "minDealSize": { "unit": "POINTS", "value": 0.1 } },
        "snapshot": snapshot
    })
}

/// USD/JPY at `mid` +- 0.01, the yen row stating `rate` dollars per yen, the dollar row 1. One pip is 0.01
/// yen of a 10,000-dollar contract: 100 yen.
fn usdjpy(rate: f64, mid: f64) -> Value {
    market(
        USDJPY,
        "0.01 JPY/USD",
        "10000",
        "100",
        &[("JPY", rate), ("USD", 1.0)],
        Some((mid - 0.01, mid + 0.01)),
    )
}

/// EUR/GBP, the pound row stating `rate` dollars per pound. One pip is 0.0001 pounds of a 10,000-euro contract.
fn eurgbp(rate: f64) -> Value {
    market(
        EURGBP,
        "0.0001 GBP/EUR",
        "10000",
        "1",
        &[("GBP", rate), ("USD", 1.0)],
        Some((0.8499, 0.8501)),
    )
}

/// GBP/USD around `mid`: the conversion pair of a pound on a dollar account.
fn gbpusd(mid: f64) -> Value {
    market(
        GBPUSD,
        "0.0001 USD/GBP",
        "10000",
        "1",
        &[("USD", 1.0)],
        Some((mid - 0.0001, mid + 0.0001)),
    )
}

/// What a scripted gateway answers for a conversion pair, and the pairs it was asked for.
struct Gateway {
    pairs: Vec<(&'static str, Result<Option<Value>, String>)>,
    asked: Vec<String>,
}

impl Gateway {
    fn new(pairs: Vec<(&'static str, Result<Option<Value>, String>)>) -> Self {
        Self { pairs, asked: Vec::new() }
    }

    fn read(&mut self, name: &str) -> Result<Option<Value>, String> {
        self.asked.push(name.to_string());
        self.pairs.iter().find(|(n, _)| *n == name).map_or(Ok(None), |(_, answer)| answer.clone())
    }
}

fn quoted(
    epic: &str,
    body: &Value,
    account: &str,
    gateway: &mut Gateway,
) -> Result<IgQuotedFacts, IgFactsError> {
    quoted_market_facts(epic, body, account, ASKED_MS, &mut |name| gateway.read(name))
}

fn no_rate(result: Result<IgQuotedFacts, IgFactsError>) -> String {
    match result.expect_err("must not be admitted") {
        IgFactsError::NoRate(why) => why,
        other => panic!("expected a refusal of the RATE, got {other:?}"),
    }
}

/// **The orientation, by value (a yen quote)**: USDJPY's base is the account currency, so the epic's own price
/// witnesses the rate and no other pair is read. A yen is a FRACTION of a dollar, and the figures are the
/// decision's own: 100,000 dollars of notional (not 15,010,000), and a 0.10 move on ten contracts realizes
/// 66.62 dollars (not 10,000).
#[test]
fn a_yen_is_a_fraction_of_a_dollar_and_the_epics_own_price_witnesses_it() {
    let mut gateway = Gateway::new(vec![]);
    let admitted = quoted(USDJPY, &usdjpy(1.0 / 150.10, 150.10), "USD", &mut gateway).expect("ok");
    let rate = admitted.quote.expect("a yen quote is converted");
    assert_eq!((rate.quote_ccy.as_str(), rate.as_of_ms), ("JPY", ASKED_MS));
    let r = rate.account_per_quote;
    assert!(r < 0.01 && r > 0.006, "{r} is yen-per-dollar, the flipped rate");
    // ten contracts of 10,000 dollars at 150.10: 15,010,000 yen = 100,000 dollars
    assert!((r * 10.0 * 10_000.0 * 150.10 - 100_000.0).abs() < 1.0);
    // a 0.10 yen move on ten contracts: 10,000 yen = 66.62 dollars
    assert!((r * 10.0 * 10_000.0 * 0.10 - 66.62).abs() < 0.01);
    assert!(gateway.asked.is_empty(), "no other pair is read: {:?}", gateway.asked);
    assert_eq!(admitted.facts.contract_value, vike_model::ContractValue::linear(10_000.0));
    assert_eq!(admitted.facts.properties.tick_size, 0.01, "the yen pip, on the wire's grid");
}

/// **The orientation, by value (a pound quote)**: EURGBP's base is not the account currency, so GBPUSD is read,
/// its direction taken from ITS `onePipMeans`; a pound is MORE than a dollar and 85,000 pounds are 114,835 dollars
/// at the 1.351 the table states.
#[test]
fn a_pound_is_more_than_a_dollar_and_a_cross_is_witnessed_by_its_conversion_pair() {
    let mut gateway = Gateway::new(vec![(GBPUSD, Ok(Some(gbpusd(1.35))))]);
    let admitted = quoted(EURGBP, &eurgbp(1.351), "USD", &mut gateway).expect("ok");
    let rate = admitted.quote.expect("a pound quote is converted");
    assert_eq!((rate.quote_ccy.as_str(), rate.account_per_quote), ("GBP", 1.351));
    assert!(rate.account_per_quote > 1.0, "pounds-per-dollar is the flipped rate");
    assert!((rate.account_per_quote * 85_000.0 - 114_835.0).abs() < 1e-6);
    assert_eq!(gateway.asked, [GBPUSD], "one pair, the account being the quote of GBPUSD");
}

/// A quote whose conversion pair is spelled the other way (`USDCHF`: the account is the BASE, so `1/mid`)
/// is found by trying the first spelling and then the other, and the direction is read from the answer.
#[test]
fn a_conversion_pair_spelled_the_other_way_is_inverted() {
    let eurchf = "CS.D.EURCHF.MINI.IP";
    let body = market(
        eurchf,
        "0.0001 CHF/EUR",
        "10000",
        "1",
        &[("CHF", 1.11), ("USD", 1.0)],
        Some((0.9399, 0.9401)),
    );
    let usdchf = market(
        "CS.D.USDCHF.MINI.IP",
        "0.0001 CHF/USD",
        "10000",
        "1",
        &[("USD", 1.0)],
        Some((0.8999, 0.9001)),
    );
    let mut gateway = Gateway::new(vec![("CS.D.USDCHF.MINI.IP", Ok(Some(usdchf)))]);
    let rate = quoted(eurchf, &body, "USD", &mut gateway).expect("ok").quote.expect("converted");
    assert_eq!(rate.account_per_quote, 1.11);
    assert_eq!(
        gateway.asked,
        ["CS.D.CHFUSD.MINI.IP", "CS.D.USDCHF.MINI.IP"],
        "the first spelling is not an IG epic, the second is"
    );
}

/// A USDJPY body whose `currencies` table is exactly `rows` (`code`, `exchangeRate`), at a 150.00 mid.
fn usdjpy_table(rows: &[(&str, f64)]) -> Value {
    market(USDJPY, "0.01 JPY/USD", "10000", "100", rows, Some((149.99, 150.01)))
}

/// **The four readings of IG's table, each held against the price** (module doc): the same yen at 1/150 dollars
/// is found whichever way IG states it, the rate used is ALWAYS the dollar fraction (never 150), and the
/// reading that held is named, which is what the first live read teaches.
#[test]
fn every_coherent_reading_of_the_table_lands_on_the_same_dollar_fraction() {
    let r = 1.0 / 150.0;
    for (what, rows, reading) in [
        ("the yen row is dollars per yen", vec![("JPY", r), ("USD", 1.0)], Reading::QuoteRow),
        (
            "the yen row is yen per dollar",
            vec![("JPY", 150.0), ("USD", 1.0)],
            Reading::QuoteRowInverse,
        ),
        ("the dollar row is dollars per yen", vec![("USD", r), ("JPY", 1.0)], Reading::AccountRow),
        ("... and the yen row is not listed", vec![("USD", r)], Reading::AccountRow),
        (
            "the dollar row is yen per dollar",
            vec![("USD", 150.0), ("JPY", 1.0)],
            Reading::AccountRowInverse,
        ),
    ] {
        let admitted = quoted(USDJPY, &usdjpy_table(&rows), "USD", &mut Gateway::new(vec![]))
            .unwrap_or_else(|e| panic!("{what}: {e:?}"));
        let rate = admitted.quote.expect("converted");
        assert_eq!(rate.reading, reading, "{what}");
        assert!((rate.account_per_quote - r).abs() < 1e-12, "{what}: {}", rate.account_per_quote);
        assert!(
            rate.account_per_quote < 0.01,
            "{what}: the yen per dollar would be the flipped rate"
        );
    }
    // the same for a pound: more than a dollar, found from either spelling of the table
    for (what, rows) in [
        ("pound row direct", vec![("GBP", 1.35), ("USD", 1.0)]),
        ("pound row inverted", vec![("GBP", 1.0 / 1.35), ("USD", 1.0)]),
        ("dollar row direct", vec![("USD", 1.35), ("GBP", 1.0)]),
        ("dollar row inverted", vec![("USD", 1.0 / 1.35), ("GBP", 1.0)]),
    ] {
        let body = market(EURGBP, "0.0001 GBP/EUR", "10000", "1", &rows, Some((0.8499, 0.8501)));
        let mut gateway = Gateway::new(vec![(GBPUSD, Ok(Some(gbpusd(1.35))))]);
        let rate = quoted(EURGBP, &body, "USD", &mut gateway)
            .unwrap_or_else(|e| panic!("{what}: {e:?}"))
            .quote
            .expect("converted");
        assert!(
            (rate.account_per_quote - 1.35).abs() < 1e-12,
            "{what}: {}",
            rate.account_per_quote
        );
    }
}

/// **A table no reading of which agrees with the price is refused with every candidate in the words**, so the
/// first live read says what IG meant: a number near neither the price nor its reciprocal.
#[test]
fn a_rate_that_no_reading_confirms_is_refused_and_the_words_carry_every_candidate() {
    let mut gateway = Gateway::new(vec![]);
    let why =
        no_rate(quoted(USDJPY, &usdjpy_table(&[("JPY", 0.5), ("USD", 1.0)]), "USD", &mut gateway));
    assert!(
        why.contains("no reading of IG's currencies table") && why.contains("NOT admitted"),
        "{why}"
    );
    assert!(why.contains("the quote currency's row, as stated = 0.5"), "{why}");
    assert!(why.contains("the quote currency's row, inverted = 2"), "{why}");
    assert!(why.contains("never valued at par") && why.contains("scalingFactor"), "{why}");
}

/// The tolerance is exactly 2% of the price-derived rate: a stated rate a conversion margin away passes, one
/// further does not (a rate is never admitted on a field that is merely nearby).
#[test]
fn the_stated_rate_may_differ_from_the_price_by_a_conversion_margin_and_no_more() {
    let near = 1.0 / 150.0 * 0.985;
    let far = 1.0 / 150.0 * 1.03;
    let mut gateway = Gateway::new(vec![]);
    assert!(quoted(USDJPY, &usdjpy(near, 150.0), "USD", &mut gateway).is_ok());
    no_rate(quoted(USDJPY, &usdjpy(far, 150.0), "USD", &mut gateway));
}

const EURCHF: &str = "CS.D.EURCHF.MINI.IP";
const CHFUSD: &str = "CS.D.CHFUSD.MINI.IP";

/// EUR/CHF with `rows` as its currencies table, and the gateway answering CHF/USD at `mid`: the cross whose
/// franc rate is near 1.0 on a dollar account, where a rate and its reciprocal are both within 2% of the price.
fn eurchf_near_par(rows: &[(&str, f64)], mid: f64) -> (Value, Gateway) {
    let body = market(EURCHF, "0.0001 CHF/EUR", "10000", "1", rows, Some((0.9399, 0.9401)));
    let pair = market(
        CHFUSD,
        "0.0001 USD/CHF",
        "10000",
        "1",
        &[("USD", 1.0)],
        Some((mid - 0.0001, mid + 0.0001)),
    );
    (body, Gateway::new(vec![(CHFUSD, Ok(Some(pair)))]))
}

/// **Near parity the price cannot tell a rate from its reciprocal, so the order the readings are tried in must
/// not decide**: a franc row of 1.015 and its inverse 0.985 are both within 2% of a conversion pair at 1.000,
/// and taking the first would book a franc at +1.5% on the strength of nothing. Refused, with both candidates in
/// the words.
#[test]
fn a_rate_near_par_that_agrees_with_its_own_reciprocal_is_refused_not_guessed() {
    let (body, mut gateway) = eurchf_near_par(&[("CHF", 1.015), ("USD", 1.0)], 1.0);
    let why = no_rate(quoted(EURCHF, &body, "USD", &mut gateway));
    assert!(why.contains("2 readings") && why.contains("cannot be told apart near par"), "{why}");
    assert!(
        why.contains("the quote currency's row, as stated = 1.015")
            && why.contains("the quote currency's row, inverted = 0.985"),
        "{why}"
    );
    assert!(why.contains("NOT admitted"), "{why}");

    // the dollar row stating the same number gives the same two readings: refused likewise
    let (body, mut gateway) = eurchf_near_par(&[("USD", 1.015), ("CHF", 1.0)], 1.0);
    let why = no_rate(quoted(EURCHF, &body, "USD", &mut gateway));
    assert!(why.contains("cannot be told apart near par"), "{why}");
}

/// The ambiguity margin is a conversion margin (0.5%): two agreeing readings closer than that are one number for
/// money's sake and the first is taken, two further apart are a guess.
#[test]
fn agreeing_readings_within_the_margin_are_the_same_number_and_beyond_it_a_guess() {
    // 1.002 and 1 / 1.002 = 0.998 differ by 0.4%: the first (the row as stated) is taken
    let (body, mut gateway) = eurchf_near_par(&[("CHF", 1.002), ("USD", 1.0)], 1.0);
    let rate = quoted(EURCHF, &body, "USD", &mut gateway).expect("ok").quote.expect("converted");
    assert_eq!((rate.account_per_quote, rate.reading), (1.002, Reading::QuoteRow));
    // 1.003 and 0.997 differ by 0.6%
    let (body, mut gateway) = eurchf_near_par(&[("CHF", 1.003), ("USD", 1.0)], 1.0);
    no_rate(quoted(EURCHF, &body, "USD", &mut gateway));
}

/// A normal rate has ONE reading that agrees with the price (its reciprocal is far off), so nothing is
/// ambiguous and the answer is the same as before.
#[test]
fn a_rate_far_from_par_has_one_agreeing_reading_and_is_admitted() {
    let (body, mut gateway) = eurchf_near_par(&[("CHF", 1.11), ("USD", 1.0)], 1.11);
    let rate = quoted(EURCHF, &body, "USD", &mut gateway).expect("ok").quote.expect("converted");
    assert_eq!((rate.account_per_quote, rate.reading), (1.11, Reading::QuoteRow));
}

/// **Par is never a rate**: a table that names only the account's own row (at 1), or both rows at 1, states
/// nothing about the franc, and a conversion pair that happens to sit within 2% of 1.000 must not "confirm" a
/// franc worth exactly one dollar.
#[test]
fn par_is_never_a_rate_whatever_the_price_near_one() {
    for (what, rows) in [
        ("only the account's row, at 1", vec![("USD", 1.0)]),
        ("both rows at 1", vec![("CHF", 1.0), ("USD", 1.0)]),
    ] {
        let (body, mut gateway) = eurchf_near_par(&rows, 1.0);
        let why = no_rate(quoted(EURCHF, &body, "USD", &mut gateway));
        assert!(
            why.contains("no coherent table") && why.contains("state no rate"),
            "{what}: {why}"
        );
        assert!(
            gateway.asked.is_empty(),
            "{what}: refused before a pair is read: {:?}",
            gateway.asked
        );
    }
}

/// A table in which neither the account's row nor the quote's says 1 is not a table of rates into (or out of)
/// one currency: nothing is read from it.
#[test]
fn a_table_with_no_identity_row_is_not_read() {
    let mut gateway = Gateway::new(vec![]);
    let why = no_rate(quoted(
        USDJPY,
        &usdjpy_table(&[("JPY", 1.0 / 150.0), ("USD", 150.0)]),
        "USD",
        &mut gateway,
    ));
    assert!(
        why.contains("no coherent table") && why.contains("one of the two must say 1"),
        "{why}"
    );
}

/// A missing, zero, negative, non-finite or non-numeric rate is no rate; none is read as par.
#[test]
fn a_missing_zero_negative_or_non_finite_rate_is_no_rate() {
    for bad in [
        json!(0),
        json!(-0.0066),
        json!("NaN"),
        json!("inf"),
        json!(""),
        json!("cheap"),
        Value::Null,
    ] {
        let mut body = usdjpy(1.0 / 150.0, 150.0);
        body["instrument"]["currencies"][0]["exchangeRate"] = bad.clone();
        let mut gateway = Gateway::new(vec![]);
        let why = no_rate(quoted(USDJPY, &body, "USD", &mut gateway));
        assert!(why.contains("no coherent table") && why.contains("JPY row"), "{bad}: {why}");
    }
    // the yen row is absent altogether: the dollar row (1) is the identity and states nothing about the yen
    // (it used to be read as a rate of 1, and only a price far from 1 refused it)
    let mut body = usdjpy(1.0 / 150.0, 150.0);
    body["instrument"]["currencies"] = json!([{ "code": "USD", "exchangeRate": 1.0 }]);
    let mut gateway = Gateway::new(vec![]);
    let why = no_rate(quoted(USDJPY, &body, "USD", &mut gateway));
    assert!(why.contains("no coherent table") && why.contains("state no rate"), "{why}");
    // a numeric string is a number, as everywhere else in IG's payloads
    let mut text = usdjpy(1.0 / 150.0, 150.0);
    text["instrument"]["currencies"][0]["exchangeRate"] = json!(format!("{}", 1.0 / 150.0));
    assert!(quoted(USDJPY, &text, "USD", &mut Gateway::new(vec![])).is_ok());
}

/// An epic QUOTED in the account's currency must still list it (the wire says the account's currency): that
/// fence is unchanged and stays the facts' own refusal. (A CONVERTED epic that lists no row for the account's
/// currency is the vendor-doc review's case, below.)
#[test]
fn an_epic_quoted_in_the_accounts_currency_that_does_not_list_it_is_still_refused() {
    let mut body =
        market(EURUSD, "0.0001 USD/EUR", "10000", "1", &[("USD", 1.0)], Some((1.08, 1.0801)));
    body["instrument"]["currencies"] = json!([{ "code": "JPY", "exchangeRate": 1.0 }]);
    let err = quoted(EURUSD, &body, "USD", &mut Gateway::new(vec![])).expect_err("not dealable");
    assert!(
        matches!(&err, IgFactsError::Refused(why) if why.contains("lists no USD among its dealing currencies")),
        "{err:?}"
    );
}

// ---- the vendor-doc review: the base column, an absent account row, the scale of the levels ------------------
//
// The numbers are the three public captures of `instrument.currencies[]` the review found (labelled COMMUNITY:
// a 2020 `GET /markets?epics=` of ~50 epics, an R package's 2025 mock, a Python library's test data), NOT IG's
// reference, which states no orientation, unit or timestamp for either rate column.

/// One `currencies[]` row: `(code, exchangeRate, baseExchangeRate)`, either column possibly absent.
type Row = (&'static str, Option<f64>, Option<f64>);

/// A USDJPY body as the 2020 capture lists it, with the table `rows`, the snapshot `(bid, offer)` and its
/// `scalingFactor`. One pip is 0.01 yen of a 10,000-dollar contract: 100 yen. (The capture's own `lotSize` is 100,
/// which the lot fence refuses by design, so these bodies carry the 1.0 every other FX Mini has.)
fn capture(rows: &[Row], price: (f64, f64), scaling: Option<f64>) -> Value {
    let currencies: Vec<Value> = rows
        .iter()
        .map(|(code, exchange, base)| {
            let mut row = json!({ "code": code, "symbol": "x", "isDefault": false });
            if let Some(x) = exchange {
                row["exchangeRate"] = json!(x);
            }
            if let Some(x) = base {
                row["baseExchangeRate"] = json!(x);
            }
            row
        })
        .collect();
    let mut snapshot = json!({ "marketStatus": "TRADEABLE", "bid": price.0, "offer": price.1 });
    if let Some(factor) = scaling {
        snapshot["scalingFactor"] = json!(factor);
    }
    json!({
        "instrument": {
            "epic": USDJPY, "expiry": "-", "type": "CURRENCIES", "unit": "CONTRACTS", "lotSize": 1.0,
            "currencies": currencies, "onePipMeans": "0.01 JPY/USD", "contractSize": "10000",
            "valueOfOnePip": "100.00"
        },
        "dealingRules": { "minDealSize": { "unit": "POINTS", "value": 1.0 } },
        "snapshot": snapshot
    })
}

/// **`baseExchangeRate` is read, and held to the price like every other reading**: the 2020 capture's JPY row says
/// base 108.036 beside a USDJPY mid of 108.09 (units of the ROW's currency per unit of the account's), and its
/// `exchangeRate` column is coarse (JPY 0.007 against a true 0.00925, the account's own USD row 0.66 instead of 1).
/// With the USD row listed or not, the yen is `1 / 108.036` dollars: a fraction, never 108.
#[test]
fn the_base_column_the_captures_agree_on_gives_the_dollar_fraction() {
    let mid = (108.08, 108.10);
    for (what, rows) in [
        (
            "the capture's table as listed (USD row beside it)",
            vec![("USD", Some(0.66), Some(1.0)), ("JPY", Some(0.007), Some(108.036))],
        ),
        (
            "... and USDJPY as the capture lists it: the yen row ALONE",
            vec![("JPY", Some(0.007), Some(108.036))],
        ),
        ("... the base column alone", vec![("JPY", None, Some(108.036))]),
    ] {
        let admitted =
            quoted(USDJPY, &capture(&rows, mid, Some(100.0)), "USD", &mut Gateway::new(vec![]))
                .unwrap_or_else(|e| panic!("{what}: {e:?}"));
        let rate = admitted.quote.expect("a yen quote is converted");
        assert_eq!(rate.reading, Reading::BaseRowInverse, "{what}");
        assert!(
            (rate.account_per_quote - 1.0 / 108.036).abs() < 1e-12,
            "{what}: {}",
            rate.account_per_quote
        );
        assert!(
            rate.account_per_quote < 0.01,
            "{what}: a yen per dollar would be the flipped rate"
        );
    }
}

/// **A coarse `exchangeRate` is refused, never accepted within 2%**: the capture's JPY row of 0.007 is 24% off the
/// price-derived 0.00925 (and its reciprocal, 142.9, is 54% off the other way), so the table that states ONLY that
/// column is `NoRate` with the candidates in the words.
#[test]
fn a_coarse_exchange_rate_is_refused_not_taken_within_two_percent() {
    let rows = [("JPY", Some(0.007), None), ("USD", Some(1.0), None)];
    let why = no_rate(quoted(
        USDJPY,
        &capture(&rows, (108.08, 108.10), Some(100.0)),
        "USD",
        &mut Gateway::new(vec![]),
    ));
    assert!(
        why.contains("no reading of IG's currencies table") && why.contains("NOT admitted"),
        "{why}"
    );
    assert!(why.contains("the quote currency's row, as stated = 0.007"), "{why}");
    // the same coarse column beside the base column that IS market-consistent: the base column's reading wins
    // because it is the one that agrees with the price (the other two are not close)
    let rows = [("JPY", Some(0.007), Some(108.036)), ("USD", Some(1.0), Some(1.0))];
    let rate = quoted(
        USDJPY,
        &capture(&rows, (108.08, 108.10), Some(100.0)),
        "USD",
        &mut Gateway::new(vec![]),
    )
    .expect("the base column agrees")
    .quote
    .expect("converted");
    assert_eq!(rate.reading, Reading::BaseRowInverse);
}

/// **The base column near par is as ambiguous as the other one**, and the existing 2% window and 0.5% ambiguity
/// refusal apply to it unchanged: 1.015 and its reciprocal 0.985 both sit within 2% of a pair at 1.000.
#[test]
fn the_base_column_near_par_is_refused_not_guessed() {
    let (mut body, mut gateway) = eurchf_near_par(&[], 1.0);
    body["instrument"]["currencies"] = json!([
        { "code": "USD", "baseExchangeRate": 1.0 },
        { "code": "CHF", "baseExchangeRate": 1.015 }
    ]);
    let why = no_rate(quoted(EURCHF, &body, "USD", &mut gateway));
    assert!(why.contains("2 readings") && why.contains("cannot be told apart near par"), "{why}");
    assert!(why.contains("baseExchangeRate"), "{why}");
}

/// **No USD row (two captures: USDJPY and USDCHF Mini list only the quote currency) falls back to the PRICE**: the
/// epic's own mid when its base is the account currency, the conversion pair's otherwise; the epic is admitted
/// with `Reading::PriceOnly` instead of being refused for the missing row, ONLY when the levels say they are price
/// units (`scalingFactor` x pip = 1, as the 2025 capture's USDCHF: 0.79715 beside 10000 and 0.0001). Neither route
/// working is a refusal still.
#[test]
fn an_epic_that_lists_no_row_for_the_accounts_currency_is_rated_by_its_price_alone() {
    let rows = [("JPY", None, None)];
    let admitted = quoted(
        USDJPY,
        &capture(&rows, (108.08, 108.10), Some(100.0)),
        "USD",
        &mut Gateway::new(vec![]),
    )
    .expect("the price route");
    let rate = admitted.quote.expect("converted");
    assert_eq!(rate.reading, Reading::PriceOnly);
    assert!((rate.account_per_quote - 1.0 / 108.09).abs() < 1e-12, "{}", rate.account_per_quote);

    // a cross: the table lists neither USD nor GBP; GBPUSD (pip 0.0001: scalingFactor 10000) witnesses the pound
    let mut pound = eurgbp(1.35);
    pound["instrument"]["currencies"] = json!([{ "code": "EUR" }]);
    let mut pair = gbpusd(1.35);
    pair["snapshot"]["scalingFactor"] = json!(10000.0);
    let mut gateway = Gateway::new(vec![(GBPUSD, Ok(Some(pair)))]);
    let rate = quoted(EURGBP, &pound, "USD", &mut gateway)
        .expect("the pair route")
        .quote
        .expect("converted");
    assert_eq!(rate.reading, Reading::PriceOnly);
    assert!((rate.account_per_quote - 1.35).abs() < 1e-9, "{}", rate.account_per_quote);
    assert_eq!(gateway.asked, [GBPUSD]);

    // neither route works: the pair does not say its levels are price units, or is not listed at all
    let mut gateway = Gateway::new(vec![(GBPUSD, Ok(Some(gbpusd(1.35))))]);
    let why = no_rate(quoted(EURGBP, &pound, "USD", &mut gateway));
    assert!(
        why.contains(GBPUSD) && why.contains("no scalingFactor") && why.contains("NOT admitted"),
        "{why}"
    );
    let why = no_rate(quoted(EURGBP, &pound, "USD", &mut Gateway::new(vec![])));
    assert!(why.contains("lists neither"), "{why}");
}

/// **A snapshot level off by the `scalingFactor` is REFUSED and never rescaled by a guess** (the 2020 capture's USDJPY
/// bid of 10806.1 beside `scalingFactor` 1: pip-scaled levels; the 2025 capture's USDCHF 0.79715 beside 10000:
/// price units). Where the table is the evidence, the 100x disagreement refuses it; where the price is the whole
/// evidence, the unit check does; an absent factor is not assumed to be 1.
#[test]
fn levels_that_are_not_price_units_are_refused_and_never_rescaled() {
    let scaled = (10806.1, 10806.9);
    // with a table: the base column says 108.036, the level says 10806.5: 100x apart
    let rows = [("JPY", None, Some(108.036))];
    let why = no_rate(quoted(
        USDJPY,
        &capture(&rows, scaled, Some(1.0)),
        "USD",
        &mut Gateway::new(vec![]),
    ));
    assert!(why.contains("no reading") && why.contains("scalingFactor 1"), "{why}");
    // price only: scalingFactor 1 x pip 0.01 is not 1, whatever the level looks like
    let rows = [("JPY", None, None)];
    for (what, factor) in
        [("pip-scaled", Some(1.0)), ("absent", None), ("off by 10x", Some(1000.0))]
    {
        let why = no_rate(quoted(
            USDJPY,
            &capture(&rows, if what == "pip-scaled" { scaled } else { (108.08, 108.10) }, factor),
            "USD",
            &mut Gateway::new(vec![]),
        ));
        assert!(
            why.contains("cannot stand in for it") && why.contains("NOT admitted"),
            "{what}: {why}"
        );
        assert!(
            if factor.is_none() {
                why.contains("no scalingFactor")
            } else {
                why.contains("not price units")
            },
            "{what}: {why}"
        );
    }
}

/// The 2020 capture lists USDJPY Mini with `lotSize` 100 (every other FX Mini 1.0): what one unit of `size` buys
/// there is unmeasured, so the lot fence refuses it BY DESIGN whatever the rate says.
#[test]
fn the_lot_size_of_the_2020_usdjpy_capture_is_refused_by_design() {
    let mut body = capture(&[("JPY", Some(0.007), Some(108.036))], (108.08, 108.10), Some(100.0));
    body["instrument"]["lotSize"] = json!(100.0);
    let err = quoted(USDJPY, &body, "USD", &mut Gateway::new(vec![])).expect_err("lot of 100");
    assert!(
        matches!(&err, IgFactsError::Refused(why) if why.contains("with a lot of 100")),
        "{err:?}"
    );
}

/// A shut market cannot witness a rate: nothing is admitted on a stated rate that no price checks.
#[test]
fn a_market_that_cannot_witness_the_rate_gives_none() {
    for (what, price, status) in [
        ("closed, no price", None, "CLOSED"),
        ("crossed", Some((150.05, 150.00)), "TRADEABLE"),
        ("zero bid", Some((0.0, 150.0)), "TRADEABLE"),
        ("edits only", Some((149.99, 150.01)), "EDITS_ONLY"),
    ] {
        let mut body = usdjpy(1.0 / 150.0, 150.0);
        body["snapshot"] = match price {
            Some((bid, offer)) => json!({ "marketStatus": status, "bid": bid, "offer": offer }),
            None => json!({ "marketStatus": status, "bid": null, "offer": null }),
        };
        let mut gateway = Gateway::new(vec![]);
        let why = no_rate(quoted(USDJPY, &body, "USD", &mut gateway));
        assert!(why.contains("cannot witness its own rate"), "{what}: {why}");
    }
}

/// A cross whose conversion pair IG does not list, answers the wrong pair, or cannot be read.
#[test]
fn a_cross_without_a_usable_conversion_pair_gives_no_rate() {
    let mut none = Gateway::new(vec![]);
    let why = no_rate(quoted(EURGBP, &eurgbp(1.35), "USD", &mut none));
    assert!(why.contains("lists neither") && why.contains(GBPUSD), "{why}");
    assert_eq!(none.asked, [GBPUSD, "CS.D.USDGBP.MINI.IP"]);

    // an answer for another epic, one of another type and a pair that does not relate GBP and USD
    for (what, answer) in [
        (
            "another epic",
            market(EURUSD, "0.0001 USD/EUR", "10000", "1", &[("USD", 1.0)], Some((1.0, 1.0))),
        ),
        (
            "unrelated",
            market(GBPUSD, "0.0001 JPY/GBP", "10000", "1", &[("USD", 1.0)], Some((190.0, 190.1))),
        ),
        ("closed", {
            let mut closed = gbpusd(1.35);
            closed["snapshot"] = json!({ "marketStatus": "CLOSED" });
            closed
        }),
        (
            "unreadable direction",
            market(GBPUSD, "one pip", "10000", "1", &[("USD", 1.0)], Some((1.0, 1.1))),
        ),
    ] {
        let mut gateway = Gateway::new(vec![(GBPUSD, Ok(Some(answer)))]);
        let why = no_rate(quoted(EURGBP, &eurgbp(1.35), "USD", &mut gateway));
        assert!(why.contains("the conversion pair CS.D.GBPUSD.MINI.IP"), "{what}: {why}");
    }

    // a read that failed is "not now", not a verdict on the rate
    let mut down = Gateway::new(vec![(GBPUSD, Err("ig GET /markets/x: IG error 503".into()))]);
    let err = quoted(EURGBP, &eurgbp(1.35), "USD", &mut down).expect_err("503");
    assert!(matches!(&err, IgFactsError::Unavailable(why) if why.contains("503")), "{err:?}");
}

/// The wire writes USD, so an account in another currency is refused BEFORE a conversion pair is read, in the
/// words the facts' own account fence uses.
#[test]
fn an_account_that_is_not_in_the_wire_currency_is_refused_before_any_read() {
    let eurusd =
        market(EURUSD, "0.0001 USD/EUR", "10000", "1", &[("USD", 1.0)], Some((1.08, 1.09)));
    let mut gateway = Gateway::new(vec![(GBPUSD, Ok(Some(gbpusd(1.35))))]);
    // a dollar quote on a pound account IS foreign: the conversion is refused before a pair is read
    for (epic, body) in [(EURUSD, eurusd), (EURGBP, eurgbp(1.35))] {
        let err = quoted(epic, &body, "GBP", &mut gateway).expect_err("GBP account");
        assert!(
            matches!(&err, IgFactsError::Refused(why) if why.contains("only a USD account is admitted")),
            "{epic}: {err:?}"
        );
    }
    assert!(gateway.asked.is_empty());
}

/// An epic quoted in the account's own currency needs no rate and reads no other pair, and its facts are
/// exactly what [`market_facts`] says (a node that converts changes nothing for it).
#[test]
fn an_epic_quoted_in_the_accounts_currency_is_answered_as_before_with_no_rate() {
    let body =
        market(EURUSD, "0.0001 USD/EUR", "10000", "1", &[("USD", 1.0)], Some((1.08, 1.0801)));
    let mut gateway = Gateway::new(vec![]);
    let admitted = quoted(EURUSD, &body, "USD", &mut gateway).expect("ok");
    assert_eq!(admitted.quote, None);
    assert_eq!(admitted.facts, market_facts(EURUSD, &body, "USD").expect("the same facts"));
    assert!(gateway.asked.is_empty());
}

/// The node that does not convert is still refused a foreign quote, in the words it always read.
#[test]
fn the_plain_facts_still_refuse_a_foreign_quote() {
    let why = market_facts(USDJPY, &usdjpy(1.0 / 150.0, 150.0), "USD").expect_err("fence");
    assert!(why.contains("quoted in JPY but this account's currency is USD"), "{why}");
}

/// `valueOfOnePip` is "Value of one pip" in no stated currency: the quote-currency product (100 yen) and the
/// account-currency one (100 yen x the rate) are both the same semantics, anything else is not.
#[test]
fn value_of_one_pip_may_be_stated_in_either_currency_and_in_no_other_way() {
    let rate = 1.0 / 150.0;
    for per_pip in ["100", "0.6667", "0.67"] {
        let mut body = usdjpy(rate, 150.0);
        body["instrument"]["valueOfOnePip"] = json!(per_pip);
        quoted(USDJPY, &body, "USD", &mut Gateway::new(vec![]))
            .unwrap_or_else(|e| panic!("{per_pip}: {e:?}"));
    }
    let mut body = usdjpy(rate, 150.0);
    body["instrument"]["valueOfOnePip"] = json!("5");
    let err = quoted(USDJPY, &body, "USD", &mut Gateway::new(vec![])).expect_err("neither");
    assert!(
        matches!(&err, IgFactsError::Refused(why)
            if why.contains("valueOfOnePip says 5") && why.contains("nor, converted")),
        "{err:?}"
    );
}

/// Levels that are not in pair units (a scaled `snapshot`) cannot witness the rate: refused in words, never
/// rescaled on a guess.
#[test]
fn scaled_levels_are_refused_and_not_rescaled() {
    let mut gateway = Gateway::new(vec![]);
    let body = usdjpy(1.0 / 150.0, 15_000.0); // 100x: levels in hundredths of a yen
    let why = no_rate(quoted(USDJPY, &body, "USD", &mut gateway));
    assert!(why.contains("scalingFactor"), "{why}");
}

/// The conversion pair is named in the epic's own family, and an epic that is not five dotted segments names none.
#[test]
fn the_conversion_pair_is_named_in_the_epics_own_family() {
    use super::helper_epics;
    assert_eq!(
        helper_epics("CS.D.EURGBP.CFD.IP", "GBP", "USD"),
        ["CS.D.GBPUSD.CFD.IP", "CS.D.USDGBP.CFD.IP"]
    );
    assert!(helper_epics("EURGBP", "GBP", "USD").is_empty());
    assert!(helper_epics("CS.D.EURGBP.MINI", "GBP", "USD").is_empty());
}

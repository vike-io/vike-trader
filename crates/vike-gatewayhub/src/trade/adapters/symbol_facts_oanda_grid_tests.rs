//! oanda's facts on the instrument's OWN price grid: the rows are parsed from scripted OANDA JSON by the
//! bridge's own `vike_oanda::parse_instruments`, so the whole chain `GET /instruments` body -> catalog row ->
//! admission fence -> wire facts is the one the gatewayhub runs.
//!
//! ⚠ **Docs-shaped, not captured.** The rows follow OANDA's documentation of the endpoint
//! (<https://developer.oanda.com/rest-live-v20/account-ep/>) and of `Instrument`
//! (<https://developer.oanda.com/rest-live-v20/primitives-df/>); no practice-account payload has been
//! recorded, and the precisions below are the documented examples (5 for `EUR_USD`, 3 for a JPY pair).
//! The older rows of `symbol_facts_tests.rs` (hand-built, no OANDA `type`) are the "class unknown" case.

use std::sync::Arc;

use serde_json::{Value, json};
use vike_gatewayhub_client::proto::facts_refusal_is_transient;
use vike_model::Instrument;

use super::{CurrencyLister, OandaFacts};

/// One docs-shaped `Instrument` of the list: its OANDA `type`, `displayPrecision` and `tradeUnitsPrecision`
/// as the JSON values given (so a garbage one can be a string, negative or absent).
fn row(name: &str, kind: &str, display: Option<Value>, units: Value) -> Value {
    let mut row = json!({
        "name": name,
        "type": kind,
        "displayName": name.replace('_', "/"),
        "pipLocation": -4,
        "tradeUnitsPrecision": units,
        "minimumTradeSize": "1",
        "maximumOrderUnits": "100000000",
        "marginRate": "0.03",
    });
    if let Some(display) = display {
        row["displayPrecision"] = display;
    }
    row
}

fn currency(name: &str, display: i64) -> Value {
    row(name, "CURRENCY", Some(json!(display)), json!(0))
}

fn rows() -> Vec<Instrument> {
    let list = json!({ "instruments": [
        currency("EUR_USD", 5),
        currency("AUD_USD", 5),
        currency("USD_JPY", 3),
        currency("EUR_JPY", 3),
        currency("GBP_JPY", 3),
        row("XAU_USD", "METAL", Some(json!(3)), json!(0)),
        row("XAG_USD", "METAL", Some(json!(5)), json!(0)),
        row("SPX500_USD", "CFD", Some(json!(1)), json!(1)),
        row("SUGAR_USD", "CFD", Some(json!(5)), json!(1)),
        row("FRACT_USD", "CURRENCY", Some(json!(5)), json!(1)),
        row("NEGPR_USD", "CURRENCY", Some(json!(-1)), json!(0)),
        row("STRPR_USD", "CURRENCY", Some(json!("5")), json!(0)),
        row("BIGPR_USD", "CURRENCY", Some(json!(12)), json!(0)),
        row("NOPR_USD", "CURRENCY", None, json!(0)),
    ]});
    vike_oanda::parse_instruments(&list)
}

/// A lister for an account whose home currency is `currency`, serving the scripted list.
fn lister(currency: &str) -> CurrencyLister {
    let currency = currency.to_string();
    let rows = rows();
    Arc::new(move || Ok((currency.clone(), rows.clone())))
}

/// **The point of the change**: on an account whose currency is the yen, the JPY pairs are admitted, and the
/// grid the engine is told is THREE decimals (a tick of 0.001), not the five the adapter used to write for
/// everything. Whole units, linear.
#[test]
fn a_jpy_pair_is_admitted_on_its_own_three_decimal_grid() {
    let facts = OandaFacts::default();
    let lister = lister("JPY");
    for typed in ["USDJPY", "EURJPY", "GBPJPY", "eurjpy", "EUR_JPY"] {
        let got =
            facts.facts(typed, Arc::clone(&lister)).unwrap_or_else(|e| panic!("{typed}: {e}"));
        assert_eq!(got.properties.tick_size, 0.001, "{typed}: three decimals");
        assert_eq!(got.properties.step_size, 1.0, "{typed}: whole units");
        assert_eq!(got.properties.min_qty, 1.0, "{typed}");
        assert_eq!(got.contract_value, None, "{typed}");
    }
}

/// The five-decimal majors keep exactly the grid they always had.
#[test]
fn a_five_decimal_pair_keeps_its_grid() {
    let facts = OandaFacts::default();
    let lister = lister("USD");
    for typed in ["EURUSD", "AUDUSD"] {
        let got = facts.facts(typed, Arc::clone(&lister)).expect(typed);
        assert_eq!((got.properties.tick_size, got.properties.step_size), (0.00001, 1.0), "{typed}");
    }
}

/// The currency fence still comes first and is unchanged: a JPY pair on a USD account is refused for its
/// QUOTE currency (the node half of multi-currency lifts that, not this fence), and a USD pair on a yen
/// account likewise.
#[test]
fn the_quote_currency_fence_is_unchanged_and_runs_before_the_grid() {
    let facts = OandaFacts::default();
    let why = facts.facts("USDJPY", lister("USD")).expect_err("quoted in JPY on a USD account");
    assert!(why.contains("USD_JPY is quoted in JPY") && !why.contains("decimals"), "{why}");

    let facts = OandaFacts::default();
    let why = facts.facts("EURUSD", lister("JPY")).expect_err("quoted in USD on a yen account");
    assert!(why.contains("EUR_USD is quoted in USD") && why.contains("currency is JPY"), "{why}");
}

/// A currency pair whose precision is garbage has NO grid: a sentence, never five decimals by default. A
/// string, an absent field and a missing tick are refused by the generic "no tick size" first; a negative
/// and an out-of-range integer reach the adapter fence and are named there.
#[test]
fn a_currency_pair_with_a_garbage_precision_is_refused_with_a_sentence() {
    let facts = OandaFacts::default();
    let lister = lister("USD");
    for symbol in ["STRPR_USD", "NOPR_USD"] {
        let why = facts.facts(symbol, Arc::clone(&lister)).expect_err(symbol);
        assert!(why.contains("no tick size"), "{symbol}: {why}");
        assert!(!facts_refusal_is_transient(&why), "{symbol}: a verdict: {why}");
    }
    for symbol in ["NEGPR_USD", "BIGPR_USD"] {
        let why = facts.facts(symbol, Arc::clone(&lister)).expect_err(symbol);
        assert!(
            why.contains(symbol)
                && why.contains("not a whole number of decimal places")
                && why.contains("displayPrecision")
                && why.contains("not admitted"),
            "{symbol}: {why}"
        );
        assert!(!facts_refusal_is_transient(&why), "{symbol}: a verdict: {why}");
    }
}

/// A currency pair is whole-unit only: one that states a fractional unit precision is refused by name, as
/// it was, even though its price grid is fine.
#[test]
fn a_currency_pair_in_fractional_units_is_still_refused() {
    let why = OandaFacts::default().facts("FRACT_USD", lister("USD")).expect_err("units of 0.1");
    assert!(why.contains("WHOLE units") && why.contains("not admitted"), "{why}");
}

/// **Metals and CFDs stay exactly where they were.** The new grid is for a CURRENCY row only: a metal's or a
/// CFD's amount unit is unmeasured. What the old five-decimal / whole-unit rule refused is refused (with the
/// same words), and what it admitted (a five-decimal, whole-unit row, here silver) is admitted unchanged.
#[test]
fn metals_and_cfds_keep_the_old_rule() {
    let facts = OandaFacts::default();
    let lister = lister("USD");
    for (symbol, word) in [("XAUUSD", "five decimals"), ("SPX500_USD", "five decimals")] {
        let why = facts.facts(symbol, Arc::clone(&lister)).expect_err(symbol);
        assert!(why.contains(word) && why.contains("not admitted"), "{symbol}: {why}");
    }
    let why = facts.facts("SUGAR_USD", Arc::clone(&lister)).expect_err("a CFD in tenths of a unit");
    assert!(why.contains("WHOLE units"), "{why}");

    let silver =
        facts.facts("XAG_USD", lister).expect("five decimals, whole units: admitted before");
    assert_eq!((silver.properties.tick_size, silver.properties.step_size), (0.00001, 1.0));
}

/// **An instrument the account's list does not carry is refused without promising CFDs.** MEASURED
/// 2026-10-26: the practice account's list holds 68 currency pairs and no CFD, metal or index, and OANDA
/// answers an order on `XAU_USD` / `SPX500_USD` with `INSTRUMENT_NOT_TRADEABLE`. The refusal used to say the
/// list covered "FX and CFD instruments of the practice account", which sent an operator looking for a
/// switch that is not ours. It is still a verdict (not a retry), and it names the instrument.
#[test]
fn an_unlisted_cfd_is_refused_as_not_offered_to_the_account() {
    let facts = OandaFacts::default();
    let lister = lister("USD");
    for symbol in ["DE30_EUR", "UK100_GBP"] {
        let why = facts.facts(symbol, Arc::clone(&lister)).expect_err(symbol);
        assert!(
            why.contains(&format!("no instrument {symbol} in the venue's instrument list"))
                && why.contains("only if OANDA offers it to the account")
                && !why.contains("FX and CFD instruments"),
            "{symbol}: {why}"
        );
        assert!(!facts_refusal_is_transient(&why), "{symbol}: a verdict: {why}");
    }
}

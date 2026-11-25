//! oanda's facts on the instrument's OWN price grid: the rows are parsed from scripted OANDA JSON by the
//! bridge's own `vike_oanda::parse_instruments`, so the whole chain `GET /instruments` body -> catalog row ->
//! admission fence -> wire facts is the one the gatewayhub runs.
//!
//! ⚠ **Docs-shaped, not captured.** The rows follow OANDA's documentation of the endpoint
//! (<https://developer.oanda.com/rest-live-v20/account-ep/>) and of `Instrument`
//! (<https://developer.oanda.com/rest-live-v20/primitives-df/>); no practice-account payload has been
//! recorded, and the precisions below are the documented examples (5 for `EUR_USD`, 3 for a JPY pair).
//! The older rows of `symbol_facts_tests.rs` (hand-built, no OANDA `type`) are the "class unknown" case.
//!
//! A symbol is asked by its USER name (decision 0290: `EURJPY`, `XAUUSD`, `SPX500.CFD`), looked up as its kept row's
//! wire name in the listing, over the bridge's test kept list (`vike_oanda::test_support::kept_list_for_tests`).

use std::sync::Arc;

use serde_json::json;
use vike_gatewayhub_client::proto::SymbolPropertiesRefusalCode;
use vike_model::Instrument;
use vike_oanda::test_support::{currency, row};

use super::{CurrencyLister, SymbolPropertiesCacheAccountListed};

/// oanda's account-listed facts: the generic per-account facts over the bridge's spec, the bridge's test kept list
/// held first (the one every test of this binary that names an oanda instrument holds).
fn oanda_facts() -> SymbolPropertiesCacheAccountListed<vike_oanda::SymbolPropertiesAccountSpecOanda>
{
    vike_oanda::test_support::kept_list_for_tests();
    SymbolPropertiesCacheAccountListed::default()
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
    let facts = oanda_facts();
    let lister = lister("JPY");
    for typed in ["USDJPY", "EURJPY", "GBPJPY", "eurjpy"] {
        let got =
            facts.facts(typed, Arc::clone(&lister)).unwrap_or_else(|e| panic!("{typed}: {e}"));
        assert_eq!(got.properties.tick_size, 0.001, "{typed}: three decimals");
        assert_eq!(got.properties.step_size, 1.0, "{typed}: whole units");
        assert_eq!(got.properties.min_qty, 1.0, "{typed}");
        assert_eq!(got.contract_value, None, "{typed}");
    }
    // The venue's own spelling is no user name (decision 0290): refused in the kept list's words, never read.
    let old = facts.facts("EUR_JPY", lister).expect_err("an old spelling");
    assert!(old.reason.contains("EUR_JPY") && old.reason.contains("kept list"), "{old}");
    assert_eq!(old.code, SymbolPropertiesRefusalCode::Verdict, "a verdict: {old}");
}

/// The five-decimal majors keep exactly the grid they always had.
#[test]
fn a_five_decimal_pair_keeps_its_grid() {
    let facts = oanda_facts();
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
    let facts = oanda_facts();
    let why =
        facts.facts("USDJPY", lister("USD")).expect_err("quoted in JPY on a USD account").reason;
    assert!(why.contains("USD_JPY is quoted in JPY") && !why.contains("decimals"), "{why}");

    let facts = oanda_facts();
    let why =
        facts.facts("EURUSD", lister("JPY")).expect_err("quoted in USD on a yen account").reason;
    assert!(why.contains("EUR_USD is quoted in USD") && why.contains("currency is JPY"), "{why}");
}

/// A currency pair whose precision is garbage has NO grid: a sentence, never five decimals by default. A
/// string, an absent field and a missing tick are refused by the generic "no tick size" first; a negative
/// and an out-of-range integer reach the adapter fence and are named there.
#[test]
fn a_currency_pair_with_a_garbage_precision_is_refused_with_a_sentence() {
    let facts = oanda_facts();
    let lister = lister("USD");
    for symbol in ["STRPRUSD", "NOPRUSD"] {
        let why = facts.facts(symbol, Arc::clone(&lister)).expect_err(symbol);
        assert!(why.reason.contains("no tick size"), "{symbol}: {why}");
        assert_eq!(why.code, SymbolPropertiesRefusalCode::Verdict, "{symbol}: a verdict: {why}");
    }
    // The adapter's fence names the instrument by the venue's own word for it (the row's).
    for (symbol, wire) in [("NEGPRUSD", "NEGPR_USD"), ("BIGPRUSD", "BIGPR_USD")] {
        let why = facts.facts(symbol, Arc::clone(&lister)).expect_err(symbol);
        assert!(
            why.reason.contains(wire)
                && why.reason.contains("not a whole number of decimal places")
                && why.reason.contains("displayPrecision")
                && why.reason.contains("not admitted"),
            "{symbol}: {why}"
        );
        assert_eq!(why.code, SymbolPropertiesRefusalCode::Verdict, "{symbol}: a verdict: {why}");
    }
}

/// A currency pair is whole-unit only: one that states a fractional unit precision is refused by name, as
/// it was, even though its price grid is fine.
#[test]
fn a_currency_pair_in_fractional_units_is_still_refused() {
    let why = oanda_facts().facts("FRACTUSD", lister("USD")).expect_err("units of 0.1").reason;
    assert!(why.contains("WHOLE units") && why.contains("not admitted"), "{why}");
}

/// **Metals and CFDs stay exactly where they were.** The new grid is for a CURRENCY row only: a metal's or a
/// CFD's amount unit is unmeasured. What the old five-decimal / whole-unit rule refused is refused (with the
/// same words), and what it admitted (a five-decimal, whole-unit row, here silver) is admitted unchanged.
#[test]
fn metals_and_cfds_keep_the_old_rule() {
    let facts = oanda_facts();
    let lister = lister("USD");
    for (symbol, word) in [("XAUUSD", "five decimals"), ("SPX500.CFD", "five decimals")] {
        let why = facts.facts(symbol, Arc::clone(&lister)).expect_err(symbol).reason;
        assert!(why.contains(word) && why.contains("not admitted"), "{symbol}: {why}");
    }
    let why = facts.facts("SUGAR.CFD", Arc::clone(&lister)).expect_err("a CFD in tenths of a unit");
    assert!(why.reason.contains("WHOLE units"), "{why}");

    let silver =
        facts.facts("XAGUSD", lister).expect("five decimals, whole units: admitted before");
    assert_eq!((silver.properties.tick_size, silver.properties.step_size), (0.00001, 1.0));
}

/// **An instrument the account's list does not carry is refused without promising CFDs.** MEASURED
/// 2026-10-26: the practice account's list holds 68 currency pairs and no CFD, metal or index, and OANDA
/// answers an order on `XAU_USD` / `SPX500_USD` with `INSTRUMENT_NOT_TRADEABLE`. The refusal used to say the
/// list covered "FX and CFD instruments of the practice account", which sent an operator looking for a
/// switch that is not ours. It is still a verdict (not a retry), and it names the instrument. (The kept list names
/// both CFDs, as another account's listing would: this account's own list is what decides.)
#[test]
fn an_unlisted_cfd_is_refused_as_not_offered_to_the_account() {
    let facts = oanda_facts();
    let lister = lister("USD");
    for symbol in ["DE30.CFD", "UK100.CFD"] {
        let why = facts.facts(symbol, Arc::clone(&lister)).expect_err(symbol);
        assert!(
            why.reason.contains(&format!("no instrument {symbol} in the venue's instrument list"))
                && why.reason.contains("only if OANDA offers it to the account")
                && !why.reason.contains("FX and CFD instruments"),
            "{symbol}: {why}"
        );
        assert_eq!(
            why.code,
            SymbolPropertiesRefusalCode::NoInstrument,
            "{symbol}: not offered, not a retry: {why}"
        );
    }
}

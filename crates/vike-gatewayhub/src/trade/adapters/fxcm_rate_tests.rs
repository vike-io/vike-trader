//! Tests of `fxcm_rate`: FXCM's converted facts and its account currency as the wire carries them. The Offers
//! table is a DOCS-SHAPED fixture (the ForexConnect Offers page: `ContractCurrency` is the BASE currency,
//! `InstrumentType` 1 is forex) scripted through the bridge's real parser and judge; no FXCM session is
//! involved and nothing here has been read off one.

use super::*;
use serde_json::json;
use vike_fxcm::home_conversion::{
    AccountCcy, PropertiesRead, account_ccy, facts_from_offers, parse_offers,
};

fn row(name: &str, bid: f64, ask: f64, digits: i64, kind: i64) -> serde_json::Value {
    json!({
        "instrument": name, "bid": bid, "ask": ask, "digits": digits, "instrument_type": kind,
        "contract_currency": name.split('/').next().unwrap_or_default(), "contract_multiplier": 1.0,
        "subscription": "T", "trading_status": "O", "bid_tradable": "T", "ask_tradable": "T",
        "base_unit": 1000, "quote_age_ms": 0
    })
}

fn table(rows: Vec<serde_json::Value>) -> String {
    serde_json::Value::Array(rows).to_string()
}

fn standard() -> String {
    table(vec![
        row("EUR/USD", 1.0998, 1.1002, 5, 1),
        row("USD/JPY", 150.08, 150.12, 3, 1),
        row("EUR/GBP", 0.8148, 0.8152, 5, 1),
        row("GBP/USD", 1.3498, 1.3502, 5, 1),
        row("XAU/USD", 2000.0, 2000.5, 2, 5),
    ])
}

fn stated(code: &str) -> AccountCcy {
    account_ccy(PropertiesRead::Json(json!({ "BASE_CRNCY": code }).to_string())).unwrap()
}

fn assumed() -> AccountCcy {
    account_ccy(PropertiesRead::Unreadable("the shim exports no fc_system_properties".into()))
        .unwrap()
}

/// The production path with the session scripted: parse, judge, wire.
fn answer(account: &AccountCcy, body: &str, symbol: &str) -> Result<WireSymbolFacts, String> {
    quoted_facts("", symbol, |_, symbol| {
        let offers = parse_offers(body).map_err(FactsError::Rate)?;
        facts_from_offers(&offers, symbol, account, 1_700_000_000_000)
    })
}

/// **Orientation on the wire.** A yen pair on a USD account carries `1/150.10` account units per yen (never
/// 150), with the currency beside it and the ask's stamp, and its own three-decimal grid.
#[test]
fn a_yen_pair_carries_a_fraction_of_a_dollar_per_yen() {
    let wire = answer(&stated("USD"), &standard(), "USDJPY").expect("facts");
    assert_eq!(wire.quote_ccy.as_deref(), Some("JPY"));
    let rate = wire.quote_rate.expect("a rate");
    assert!((rate - 1.0 / 150.10).abs() < 1e-15, "{rate}");
    assert!(rate < 0.01, "a yen is a fraction of a dollar, not 150: {rate}");
    assert_eq!(wire.rate_as_of_ms, Some(1_700_000_000_000));
    assert!((wire.properties.tick_size - 0.001).abs() < 1e-15);
    assert_eq!(wire.properties.step_size, 1000.0);
}

/// A cross: EUR/GBP on a USD account is converted through GBP/USD, 1.35 dollars per pound.
#[test]
fn a_cross_carries_the_pounds_rate() {
    let wire = answer(&stated("USD"), &standard(), "EURGBP").expect("facts");
    assert_eq!(wire.quote_ccy.as_deref(), Some("GBP"));
    assert!((wire.quote_rate.expect("a rate") - 1.35).abs() < 1e-12);
}

/// **An API-reported EUR account is converted in EUR**, not USD: EUR/USD is `1/1.10` euros per dollar.
#[test]
fn a_reported_eur_account_is_converted_in_euro() {
    let wire = answer(&stated("EUR"), &standard(), "EURUSD").expect("facts");
    assert_eq!(wire.quote_ccy.as_deref(), Some("USD"));
    assert!((wire.quote_rate.expect("a rate") - 1.0 / 1.10).abs() < 1e-12);
}

/// **An ASSUMED-USD account still converts a non-USD-quoted pair** through the conversion pair's rate, with
/// the orientation intact (the assumption changes how the currency is known, not how a rate is derived).
#[test]
fn an_assumed_usd_account_still_converts_through_the_pair() {
    let wire = answer(&assumed(), &standard(), "USDJPY").expect("facts");
    assert!((wire.quote_rate.expect("a rate") - 1.0 / 150.10).abs() < 1e-15);
    assert_eq!(wire.quote_ccy.as_deref(), Some("JPY"));
}

/// A symbol quoted in the account's own money converts nothing, and says nothing: byte-identical on the wire to
/// an older daemon's answer.
#[test]
fn a_symbol_at_par_carries_no_quote_fields() {
    let wire = answer(&stated("USD"), &standard(), "EURUSD").expect("facts");
    assert_eq!((&wire.quote_ccy, wire.quote_rate, wire.rate_as_of_ms), (&None, None, None));
    let text = serde_json::to_string(&wire).unwrap();
    for key in ["quote_ccy", "quote_rate", "rate_as_of_ms"] {
        assert!(!text.contains(key), "{key} is not written at par: {text}");
    }
}

/// **A CFD-class row carries its contract value and its margin rate on the wire's EXISTING optional fields**: an index
/// with a multiplier of a tenth in a dollar ledger (par: no quote fields), the venue's maximum order, and a margin rate only
/// when the row states a margin; a metal with a multiplier of 1 carries no contract value at all (the default).
#[test]
fn a_cfd_class_row_carries_its_contract_value_and_margin_on_the_existing_wire_fields() {
    let cfd = |name: &str, kind: i64, mult: f64, mmr: f64| {
        let mut r = row(name, 51_725.0, 51_727.0, 2, kind);
        r["contract_currency"] = json!("USD");
        r["contract_multiplier"] = json!(mult);
        r["base_unit"] = json!(1);
        r["min_qty"] = json!(1);
        r["max_qty"] = json!(4000);
        r["mmr"] = json!(mmr);
        r
    };
    let table = table(vec![
        row("EUR/USD", 1.0998, 1.1002, 5, 1),
        cfd("US30", 2, 0.1, -1.0),
        cfd("XAU/USD", 5, 1.0, -1.0),
        cfd("SPX500", 2, 0.1, 5.0),
    ]);
    let dow = answer(&stated("USD"), &table, "US30").expect("facts");
    assert_eq!(dow.contract_value, Some(vike_model::ContractValue::linear(0.1)));
    assert_eq!(
        (dow.properties.step_size, dow.properties.min_qty, dow.properties.max_qty),
        (1.0, 1.0, 4000.0)
    );
    assert_eq!(
        (dow.quote_ccy, dow.quote_rate, dow.margin_rate),
        (None, None, None),
        "par, and no margin stated"
    );
    let gold = answer(&stated("USD"), &table, "XAUUSD").expect("facts");
    assert_eq!(gold.contract_value, None, "a multiplier of 1 is the wire's default");
    let text = serde_json::to_string(&gold).unwrap();
    // (the wire writes an absent `contract_value` as `null`, as every venue's answer does; `margin_rate` is not written)
    assert!(text.contains("\"contract_value\":null") && !text.contains("margin_rate"), "{text}");
    let margined = answer(&stated("USD"), &table, "SPX500").expect("facts");
    let rate = margined.margin_rate.expect("a stated margin is a rate");
    assert!((rate - 5.0 / (51_726.0 * 0.1)).abs() < 1e-12, "margin over the lot's value: {rate}");
}

/// **A rate that is unavailable now is worded as the rate's failure; a verdict on the symbol is its own
/// words**: the node forgets the first and remembers the second.
#[test]
fn a_rate_failure_and_a_symbol_verdict_are_worded_apart() {
    let mut closed = vec![row("EUR/GBP", 0.8148, 0.8152, 5, 1), row("GBP/USD", 1.35, 1.35, 5, 1)];
    closed[1]["trading_status"] = json!("C");
    let why = answer(&stated("USD"), &table(closed), "EURGBP").unwrap_err();
    assert!(why.contains(FACTS_RATE_UNAVAILABLE) && why.contains("not open"), "{why}");

    // a type the SDK does not name is a verdict on the symbol (a metal, an index or a CFD is admitted now)
    let mut unnamed = vec![row("EUR/USD", 1.0998, 1.1002, 5, 1)];
    unnamed.push(row("ZZZ/USD", 2000.0, 2000.5, 2, 42));
    let why = answer(&stated("USD"), &table(unnamed), "ZZZUSD").unwrap_err();
    assert!(
        !why.contains(FACTS_RATE_UNAVAILABLE),
        "an unnamed type is a verdict on the symbol: {why}"
    );
    assert!(why.contains("not determinable"), "{why}");

    let why = answer(&stated("USD"), "garbage", "USDJPY").unwrap_err();
    assert!(
        why.contains(FACTS_RATE_UNAVAILABLE),
        "an unreadable table is the rate's failure: {why}"
    );

    let why = quoted_facts("", "USDJPY", |_, _| {
        Err(FactsError::Rate("fxcm: no reconcile session is running for this account".to_string()))
    })
    .unwrap_err();
    assert!(why.contains(FACTS_RATE_UNAVAILABLE) && why.contains("no reconcile session"), "{why}");
}

// ---- the account's currency at a start ----------------------------------------------------------------------

#[test]
fn a_stated_currency_is_told_as_stated_and_an_assumed_one_is_flagged() {
    assert_eq!(account_currency("", |_| Ok(stated("EUR"))), (Some("EUR".to_string()), false));
    assert_eq!(account_currency("", |_| Ok(assumed())), (Some("USD".to_string()), true));
    assert_eq!(
        account_currency("ACC2", |account| {
            assert_eq!(account, "ACC2", "the lane asks about the account it was started for");
            Ok(stated("GBP"))
        }),
        (Some("GBP".to_string()), false)
    );
}

/// A statement that is not a currency is told as NOTHING: neither a guess nor an assumption over it.
#[test]
fn a_malformed_statement_is_told_as_nothing() {
    assert_eq!(account_currency("", |_| Err("BASE_CRNCY is `US$`".to_string())), (None, false));
}

// ---- the lane's entry points, over the real registry ---------------------------------------------------------

/// A scripted FXCM session behind the REAL registry `vike_fxcm::reads` (labels are unique per test: the
/// registry is process-wide and the tests run in parallel).
struct Scripted {
    properties: String,
    offers: String,
}

impl vike_fxcm::reads::SessionReads for Scripted {
    fn system_properties(&self) -> PropertiesRead {
        PropertiesRead::Json(self.properties.clone())
    }
    fn offers(&self) -> Result<String, String> {
        Ok(self.offers.clone())
    }
}

#[test]
fn the_registry_key_is_the_label_the_adapter_registers_under() {
    assert_eq!(registry_key(None).unwrap(), "", "the default account is the empty label");
    assert_eq!(registry_key(Some("ZZKEY1")).unwrap(), "ZZKEY1");
    assert_eq!(
        registry_key(Some("DEFAULT")).unwrap(),
        "",
        "the wire's spelling of the default account"
    );
    for bad in ["zzkey1", "A_B", ""] {
        assert!(registry_key(Some(bad)).is_err(), "{bad:?} names no account");
    }
}

/// Only a `demo` account has an FXCM session: any other tier is refused in words and states no currency.
#[test]
fn only_a_demo_account_is_looked_up() {
    for tier in [AccountTier::Live, AccountTier::Paper] {
        let why = facts_for(tier, None, "USDJPY").unwrap_err();
        assert!(why.contains("demo account only"), "{tier:?}: {why}");
        assert_eq!(currency_for(tier, None), (None, false), "{tier:?}");
    }
}

/// With no reconcile session behind the account the API cannot be asked: the currency is the ruling's USD,
/// ASSUMED (flagged), and a rate is unavailable (the rate's failure, in words), never a guess at par.
#[test]
fn an_account_with_no_session_is_assumed_usd_and_has_no_rate() {
    assert_eq!(currency_for(AccountTier::Demo, Some("ZZNONE1")), (Some("USD".to_string()), true));
    let why = facts_for(AccountTier::Demo, Some("ZZNONE1"), "USDJPY").unwrap_err();
    assert!(why.contains(FACTS_RATE_UNAVAILABLE) && why.contains("no reconcile session"), "{why}");
}

/// **End to end through the registry**: a EUR account that the API states converts the yen pair in EUR through
/// EUR/JPY, never in USD (the facts path), and the start is told EUR, unflagged, nothing falling back to USD. (Whether
/// the ledger may be anchored to it under a USD-quoted mount is the node's fence's decision.)
#[test]
fn a_registered_eur_account_converts_in_eur_and_is_told_eur() {
    let table =
        table(vec![row("USD/JPY", 150.08, 150.12, 3, 1), row("EUR/JPY", 164.98, 165.02, 3, 1)]);
    let entry = vike_fxcm::reads::register(
        "ZZEUR1",
        std::sync::Arc::new(Scripted {
            properties: json!({ "BASE_CRNCY": "EUR" }).to_string(),
            offers: table,
        }),
    );
    let wire = facts_for(AccountTier::Demo, Some("ZZEUR1"), "USDJPY").expect("facts");
    assert_eq!(wire.quote_ccy.as_deref(), Some("JPY"));
    assert!((wire.quote_rate.expect("a rate") - 1.0 / 165.0).abs() < 1e-12);
    assert_eq!(
        currency_for(AccountTier::Demo, Some("ZZEUR1")),
        (Some("EUR".to_string()), false),
        "EUR is the venue's word: told as stated, never erased, never overridden by USD"
    );
    vike_fxcm::reads::unregister("ZZEUR1", &entry);
}

/// **A silent API on a registered session: USD, ASSUMED, flagged, and the yen pair still converts.**
#[test]
fn a_registered_session_whose_api_is_silent_is_assumed_usd() {
    let entry = vike_fxcm::reads::register(
        "ZZUSD1",
        std::sync::Arc::new(Scripted { properties: "{}".to_string(), offers: standard() }),
    );
    assert_eq!(currency_for(AccountTier::Demo, Some("ZZUSD1")), (Some("USD".to_string()), true));
    let wire = facts_for(AccountTier::Demo, Some("ZZUSD1"), "USDJPY").expect("facts");
    assert!((wire.quote_rate.expect("a rate") - 1.0 / 150.10).abs() < 1e-15);
    vike_fxcm::reads::unregister("ZZUSD1", &entry);
}

/// **Whether FXCM's currency is ASSUMED is what withholds its reconcile balance** (the figure is the account's
/// home-currency cash with no currency column, and a ledger labelled with an assumed USD must not adopt a EUR figure
/// as dollars): a registered session whose API is silent is assumed; one that states a currency is not; an
/// account with no session (it has no reconcile client either) is not, and neither is a label the wire spelling
/// does not admit.
#[test]
fn the_currency_of_a_silent_session_is_assumed_and_a_stated_or_absent_one_is_not() {
    let silent = vike_fxcm::reads::register(
        "ZZASM1",
        std::sync::Arc::new(Scripted { properties: "{}".to_string(), offers: standard() }),
    );
    let stated = vike_fxcm::reads::register(
        "ZZASM2",
        std::sync::Arc::new(Scripted {
            properties: json!({ "BASE_CRNCY": "EUR" }).to_string(),
            offers: standard(),
        }),
    );
    assert!(currency_is_assumed(Some("ZZASM1")));
    assert!(!currency_is_assumed(Some("ZZASM2")));
    assert!(!currency_is_assumed(Some("ZZASM3")), "no session registered");
    assert!(!currency_is_assumed(Some("not a label")));
    // The proof a stamp needs is the mirror image, and "no session" is NOT "stated": only the venue's own word counts.
    assert_eq!(currency_stated(Some("ZZASM1")), None, "an assumed USD is no statement");
    assert_eq!(currency_stated(Some("ZZASM2")), Some("EUR".to_string()));
    assert_eq!(currency_stated(Some("ZZASM3")), None, "no session registered");
    assert_eq!(currency_stated(Some("not a label")), None);
    vike_fxcm::reads::unregister("ZZASM1", &silent);
    vike_fxcm::reads::unregister("ZZASM2", &stated);
}

// ---- review 2: the ledger the facts are in, and a currency the venue STATES is never erased ---------------------

/// **The facts STATE the ledger their answer is in** (review 2, HIGH-1 (a)): the stamp is the account's currency
/// as the session decided it, on a converted answer (a yen pair on a EUR account, in EUR) and on a symbol quoted in
/// the account's own money alike, so the node, which holds the engine at the currency it was started with,
/// refuses an answer in another one. An assumed USD is stamped USD.
#[test]
fn the_facts_state_the_ledger_currency_their_answer_is_in() {
    let table =
        table(vec![row("USD/JPY", 150.08, 150.12, 3, 1), row("EUR/JPY", 164.98, 165.02, 3, 1)]);
    let eur = answer(&stated("EUR"), &table, "USDJPY").expect("facts");
    assert_eq!(eur.quote_ccy.as_deref(), Some("JPY"));
    assert_eq!(eur.account_ccy.as_deref(), Some("EUR"), "the rate is in EUR");
    let par = answer(&stated("USD"), &standard(), "EURUSD").expect("facts");
    assert_eq!(par.quote_ccy, None);
    assert_eq!(par.account_ccy.as_deref(), Some("USD"), "a symbol at par says its ledger too");
    let assumed = answer(&assumed(), &standard(), "USDJPY").expect("facts");
    assert_eq!(assumed.account_ccy.as_deref(), Some("USD"));
}

/// **A currency the API STATES is told as stated, whatever the engine is mounted on** (review 2, HIGH-2; the same
/// bug class as dukascopy's HIGH-1): EUR on the USD-quoted `EURUSD` mount used to be answered `(None, false)`, and
/// the node turned "the lane told nothing" into "USD, assumed" for an account the venue had just said is EUR. The
/// lane now tells EUR, unflagged, and takes no mount into account at all: the node's own mount fence
/// (`unanchorable_mount`, the one decision every other venue goes through, pinned on the wire by
/// `a_stated_eur_on_a_usd_quoted_mount_is_anchored_or_declined_never_defaulted_to_usd`) decides whether a ledger
/// the mount does not count in is anchored (the mount declared foreign-quoted) or declined. Every currency a
/// session states is told as stated.
#[test]
fn a_stated_currency_is_told_as_stated_whatever_the_mount_is_quoted_in() {
    for (label, code) in
        [("ZZEUR2", "EUR"), ("ZZGBP2", "GBP"), ("ZZJPY2", "JPY"), ("ZZUSD2", "USD")]
    {
        let entry = vike_fxcm::reads::register(
            label,
            std::sync::Arc::new(Scripted {
                properties: json!({ "BASE_CRNCY": code }).to_string(),
                offers: standard(),
            }),
        );
        assert_eq!(
            currency_for(AccountTier::Demo, Some(label)),
            (Some(code.to_string()), false),
            "{code}: the venue's word, never erased, never USD"
        );
        vike_fxcm::reads::unregister(label, &entry);
    }
}

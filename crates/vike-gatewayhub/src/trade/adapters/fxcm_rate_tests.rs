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

/// **A rate that is unavailable now is worded as the rate's failure; a verdict on the symbol is its own
/// words**: the node forgets the first and remembers the second.
#[test]
fn a_rate_failure_and_a_symbol_verdict_are_worded_apart() {
    let mut closed = vec![row("EUR/GBP", 0.8148, 0.8152, 5, 1), row("GBP/USD", 1.35, 1.35, 5, 1)];
    closed[1]["trading_status"] = json!("C");
    let why = answer(&stated("USD"), &table(closed), "EURGBP").unwrap_err();
    assert!(why.contains(FACTS_RATE_UNAVAILABLE) && why.contains("not open"), "{why}");

    let why = answer(&stated("USD"), &standard(), "XAUUSD").unwrap_err();
    assert!(!why.contains(FACTS_RATE_UNAVAILABLE), "a CFD is a verdict on the symbol: {why}");
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
        assert_eq!(currency_for(tier, None, "EURUSD"), (None, false), "{tier:?}");
    }
}

/// With no reconcile session behind the account the API cannot be asked: the currency is the ruling's USD,
/// ASSUMED (flagged), and a rate is unavailable (the rate's failure, in words), never a guess at par.
#[test]
fn an_account_with_no_session_is_assumed_usd_and_has_no_rate() {
    assert_eq!(
        currency_for(AccountTier::Demo, Some("ZZNONE1"), "EURUSD"),
        (Some("USD".to_string()), true)
    );
    let why = facts_for(AccountTier::Demo, Some("ZZNONE1"), "USDJPY").unwrap_err();
    assert!(why.contains(FACTS_RATE_UNAVAILABLE) && why.contains("no reconcile session"), "{why}");
}

/// **End to end through the registry**: a EUR account that the API states converts the yen pair in EUR through
/// EUR/JPY, never in USD (the facts path), but its ledger is NOT anchored to EUR while the engine is mounted on a
/// USD-quoted pair ([`anchorable`]), and nothing falls back to USD: the lane states nothing.
#[test]
fn a_registered_eur_account_converts_in_eur_and_is_not_anchored_under_a_usd_mount() {
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
        currency_for(AccountTier::Demo, Some("ZZEUR1"), "EURUSD"),
        (None, false),
        "EUR is the venue's word, not overridden by USD; and the USD-quoted mount keeps the ledger unanchored"
    );
    vike_fxcm::reads::unregister("ZZEUR1", &entry);
}

/// The mount fence, as a pure rule: a currency is handed to the node only when it IS the money of the engine's
/// mounted symbol (fxcm mounts EURUSD: USD), whether the venue stated it or it is assumed; a flag with no
/// currency stays a flag with no currency; a mount whose money is unknown anchors nothing.
#[test]
fn a_currency_is_anchorable_only_when_it_is_the_mounts_money() {
    let s = |c: &str| Some(c.to_string());
    assert_eq!(anchorable((s("USD"), false), Some("USD")), (s("USD"), false));
    assert_eq!(
        anchorable((s("USD"), true), Some("USD")),
        (s("USD"), true),
        "assumed stays assumed"
    );
    assert_eq!(
        anchorable((s("usd"), false), Some("USD")),
        (s("usd"), false),
        "codes compare case-blind"
    );
    assert_eq!(anchorable((s("EUR"), false), Some("USD")), (None, false));
    assert_eq!(anchorable((s("USD"), true), Some("JPY")), (None, false));
    assert_eq!(
        anchorable((s("USD"), false), None),
        (None, false),
        "an unknown mount anchors nothing"
    );
    assert_eq!(anchorable((None, false), Some("USD")), (None, false));
    // The mounted symbol of the roster's fxcm row is EURUSD, which counts its money in USD.
    let money = vike_venues::venue_wiring::wired_symbol("fxcm")
        .and_then(vike_fxcm::home_conversion::quote_of_symbol);
    assert_eq!(money.as_deref(), Some("USD"));
}

/// **A silent API on a registered session: USD, ASSUMED, flagged, and the yen pair still converts.**
#[test]
fn a_registered_session_whose_api_is_silent_is_assumed_usd() {
    let entry = vike_fxcm::reads::register(
        "ZZUSD1",
        std::sync::Arc::new(Scripted { properties: "{}".to_string(), offers: standard() }),
    );
    assert_eq!(
        currency_for(AccountTier::Demo, Some("ZZUSD1"), "EURUSD"),
        (Some("USD".to_string()), true)
    );
    let wire = facts_for(AccountTier::Demo, Some("ZZUSD1"), "USDJPY").expect("facts");
    assert!((wire.quote_rate.expect("a rate") - 1.0 / 150.10).abs() < 1e-15);
    vike_fxcm::reads::unregister("ZZUSD1", &entry);
}

/// **The fence reads the symbol the engine was STARTED on, not the roster's wired one** (finding 1 of the second
/// review). A labelled account `ALT` on a USD account mounted on USDJPY books yen PnL at par: 1,000 units moving
/// 150.10 -> 150.20 earn 100 yen, which would be booked as 100 dollars (the real figure is 0.67). The default
/// account on EURUSD (the roster's wired symbol) is unchanged, stated or assumed. A mount whose money cannot be
/// told (a spelling that is no pair) anchors nothing, as does an empty one.
#[test]
fn the_fence_reads_the_symbol_the_engine_was_started_on() {
    let entry = vike_fxcm::reads::register(
        "ZZALT1",
        std::sync::Arc::new(Scripted {
            properties: json!({ "BASE_CRNCY": "USD" }).to_string(),
            offers: standard(),
        }),
    );
    let alt = |mount: &str| currency_for(AccountTier::Demo, Some("ZZALT1"), mount);
    assert_eq!(alt("USDJPY"), (None, false), "yen fills would be booked as dollars");
    assert_eq!(alt("USD/JPY"), (None, false), "the venue's own spelling of the same mount");
    assert_eq!(alt("EURGBP"), (None, false), "a pound-quoted mount on a USD account");
    assert_eq!(alt("EURUSD"), (Some("USD".to_string()), false), "the wired symbol, unchanged");
    assert_eq!(alt("BTCUSDT"), (None, false), "no pair of two currencies: money unknown");
    assert_eq!(alt(""), (None, false), "no mount at all anchors nothing");
    vike_fxcm::reads::unregister("ZZALT1", &entry);

    // An ASSUMED-USD account is fenced the same way: the assumption is no licence to anchor a yen mount.
    assert_eq!(
        currency_for(AccountTier::Demo, Some("ZZNONE2"), "USDJPY"),
        (None, false),
        "assumed USD under a yen mount is not handed to the node"
    );
    assert_eq!(
        currency_for(AccountTier::Demo, Some("ZZNONE2"), "EURUSD"),
        (Some("USD".to_string()), true)
    );
}

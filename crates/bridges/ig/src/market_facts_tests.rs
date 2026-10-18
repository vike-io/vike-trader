use serde_json::{Value, json};
use vike_model::{ContractValue, SymbolProperties};

use super::*;

const EPIC: &str = "CS.D.EURUSD.MINI.IP";

/// EUR/USD mini in the documented `GET /markets/{epic}` shape. HAND-WRITTEN: no capture of this
/// endpoint is checked in, so what these tests prove is the handling of the shape, never IG's numbers.
fn eurusd() -> Value {
    json!({
        "instrument": {
            "epic": EPIC, "expiry": "-", "name": "EUR/USD Mini", "type": "CURRENCIES",
            "unit": "CONTRACTS", "lotSize": 1.0, "marketId": "EURUSD",
            "currencies": [{ "code": "USD", "symbol": "$", "isDefault": true }],
            "onePipMeans": "0.0001 USD/EUR", "contractSize": "10000", "valueOfOnePip": "1"
        },
        "dealingRules": { "minDealSize": { "unit": "POINTS", "value": 0.1 } },
        "snapshot": { "marketStatus": "TRADEABLE" }
    })
}

fn edited(edit: impl FnOnce(&mut Value)) -> Value {
    let mut market = eurusd();
    edit(&mut market);
    market
}

fn refused(epic: &str, market: &Value, account: &str) -> String {
    market_facts(epic, market, account).expect_err("must be refused")
}

#[test]
fn a_usd_quoted_currencies_epic_on_a_usd_account_is_admitted_on_the_pip_grid() {
    let facts = market_facts(EPIC, &eurusd(), "USD").expect("admitted");
    assert_eq!(
        facts.properties,
        SymbolProperties {
            tick_size: 0.0001,
            step_size: 0.1,
            min_qty: 0.1,
            ..SymbolProperties::default()
        },
        "tick = the pip, step = minimum = minDealSize, nothing else claimed (contract_size 0: the \
         multiplier rides the contract value, so it is not scaled twice)"
    );
    assert_eq!(facts.contract_value, ContractValue::linear(10_000.0));
    // numbers or strings, as the venue quotes either
    let numeric = edited(|m| {
        m["instrument"]["contractSize"] = json!(10_000);
        m["instrument"]["valueOfOnePip"] = json!(1.0);
    });
    assert_eq!(market_facts(EPIC, &numeric, "usd").expect("admitted"), facts);
}

/// The size semantics of every other class are UNMEASURED, so each is refused by its own name.
#[test]
fn every_class_but_currencies_is_refused_by_name() {
    for (class, said) in [
        ("INDICES", "stake per index point"),
        ("SHARES", "pence"),
        ("COMMODITIES", "market-specific contract size"),
        ("CRYPTOCURRENCY", "coin amount"),
        ("BONDS", "contracts"),
        ("OPT_SHARES", "not a linear position"),
        ("SPRINT_MARKET", "not known"),
        ("", "not known"),
    ] {
        let why = refused(EPIC, &edited(|m| m["instrument"]["type"] = json!(class)), "USD");
        assert!(why.contains(said) && why.contains("UNMEASURED"), "{class}: {why}");
    }
}

#[test]
fn every_other_fence_has_its_own_words() {
    let jpy = "CS.D.USDJPY.MINI.IP";
    let usdjpy = edited(|m| {
        m["instrument"]["epic"] = json!(jpy);
        m["instrument"]["onePipMeans"] = json!("0.01 JPY/USD");
        m["instrument"]["valueOfOnePip"] = json!("100");
    });
    let eurgbp = "CS.D.EURGBP.MINI.IP";
    let gbp = edited(|m| {
        m["instrument"]["epic"] = json!(eurgbp);
        m["instrument"]["onePipMeans"] = json!("0.0001 GBP/EUR");
    });
    let rows: Vec<(&str, &str, Value, &str, &str)> = vec![
        ("no instrument block", EPIC, json!({}), "USD", "no `instrument` block"),
        ("another epic answered", "CS.D.GBPUSD.MINI.IP", eurusd(), "USD", "answered for"),
        (
            "another case answered",
            EPIC,
            edited(|m| m["instrument"]["epic"] = json!(EPIC.to_lowercase())),
            "USD",
            "answered for",
        ),
        (
            "spread bet",
            EPIC,
            edited(|m| m["instrument"]["expiry"] = json!("DFB")),
            "USD",
            "expiry `DFB`",
        ),
        (
            "dated",
            EPIC,
            edited(|m| m["instrument"]["expiry"] = json!("JUN-26")),
            "USD",
            "expiry `JUN-26`",
        ),
        (
            "stake unit",
            EPIC,
            edited(|m| m["instrument"]["unit"] = json!("AMOUNT")),
            "USD",
            "single CONTRACTS",
        ),
        (
            "a lot of 100",
            EPIC,
            edited(|m| m["instrument"]["lotSize"] = json!(100.0)),
            "USD",
            "single CONTRACTS",
        ),
        (
            "unreadable pip",
            EPIC,
            edited(|m| m["instrument"]["onePipMeans"] = json!("one pip")),
            "USD",
            "`onePipMeans`",
        ),
        (
            "pair swapped",
            EPIC,
            edited(|m| m["instrument"]["onePipMeans"] = json!("0.0001 EUR/USD")),
            "USD",
            "does not name the pair",
        ),
        ("quoted in JPY", jpy, usdjpy, "USD", "quoted in JPY but this account's currency is USD"),
        ("a GBP account", eurgbp, gbp, "GBP", "only a USD account is admitted"),
        (
            "not dealt in USD",
            EPIC,
            edited(|m| m["instrument"]["currencies"] = json!([{ "code": "EUR" }])),
            "USD",
            "lists no USD",
        ),
        (
            "no contract size",
            EPIC,
            edited(|m| m["instrument"]["contractSize"] = json!(0)),
            "USD",
            "no contractSize",
        ),
        (
            "no value of a pip",
            EPIC,
            edited(|m| {
                m["instrument"].as_object_mut().unwrap().remove("valueOfOnePip");
            }),
            "USD",
            "no contractSize or no valueOfOnePip",
        ),
        (
            "size semantics disagree",
            EPIC,
            edited(|m| m["instrument"]["valueOfOnePip"] = json!("0.5")),
            "USD",
            "valueOfOnePip says 0.5",
        ),
        (
            "no minimum deal size",
            EPIC,
            edited(|m| m["dealingRules"] = json!({})),
            "USD",
            "no minimum deal size",
        ),
        (
            "a pip finer than the wire",
            EPIC,
            edited(|m| {
                m["instrument"]["onePipMeans"] = json!("0.000001 USD/EUR");
                m["instrument"]["valueOfOnePip"] = json!("0.01");
            }),
            "USD",
            "whole multiple of 0.00001",
        ),
        (
            "a pip off the wire's grid",
            EPIC,
            edited(|m| {
                m["instrument"]["onePipMeans"] = json!("0.000025 USD/EUR");
                m["instrument"]["valueOfOnePip"] = json!("0.25");
            }),
            "USD",
            "whole multiple of 0.00001",
        ),
    ];
    for (what, epic, market, account, said) in rows {
        let why = refused(epic, &market, account);
        assert!(why.contains(said), "{what}: {why}");
        assert!(why.starts_with("ig: "), "{what}: {why}");
    }
}

/// The currency fence is stated before the account fence and before any grid word: a JPY pair on a JPY
/// account is refused for the wire currency, a JPY pair on a USD account for the quote.
#[test]
fn the_quote_currency_is_judged_before_the_wire_currency() {
    let usdjpy = edited(|m| {
        m["instrument"]["epic"] = json!("CS.D.USDJPY.MINI.IP");
        m["instrument"]["onePipMeans"] = json!("0.01 JPY/USD");
        m["instrument"]["valueOfOnePip"] = json!("100");
    });
    let on_jpy = refused("CS.D.USDJPY.MINI.IP", &usdjpy, "JPY");
    assert!(on_jpy.contains("only a USD account is admitted"), "{on_jpy}");
    let on_usd = refused("CS.D.USDJPY.MINI.IP", &usdjpy, "USD");
    assert!(on_usd.contains("quoted in JPY"), "{on_usd}");
}

#[test]
fn the_account_currency_is_the_rows_own_and_never_a_guess() {
    let accounts = json!({ "accounts": [
        { "accountId": "A", "currency": "GBP" },
        { "accountId": "B", "currency": " usd " },
        { "accountId": "C" },
        { "accountId": "D", "currency": "dollars" },
    ] });
    assert_eq!(parse_account_currency(&accounts, "B"), Ok("USD".to_string()));
    assert_eq!(parse_account_currency(&accounts, "A"), Ok("GBP".to_string()));
    for (id, said) in
        [("C", "no readable currency"), ("D", "no readable currency"), ("Z", "lists no account")]
    {
        let why = parse_account_currency(&accounts, id).expect_err(id);
        assert!(why.contains(said), "{id}: {why}");
    }
    assert!(parse_account_currency(&json!({}), "A").is_err(), "no first-row fallback, no default");
}

#[test]
fn only_an_epics_own_characters_stand_in_a_path() {
    for ok in [EPIC, "IX.D.SPTRD.DAILY.IP", "CS.D.EUR_USD.1.IP"] {
        assert!(wire_safe(ok), "{ok}");
    }
    let long = "A".repeat(65);
    for bad in ["", "a b", "CS.D.EURUSD/../x", "A?x=1", "é", long.as_str()] {
        assert!(!wire_safe(bad), "{bad:?}");
    }
}

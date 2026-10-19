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

/// The account's type is the row's own, upper-cased, and absent when the row states none (it is a refusal
/// only when it says something other than CFD, on the door that converts).
#[test]
fn the_account_type_is_the_rows_own_or_absent() {
    let accounts = json!({ "accounts": [
        { "accountId": "A", "accountType": "CFD" },
        { "accountId": "B", "accountType": " spreadbet " },
        { "accountId": "C" },
        { "accountId": "D", "accountType": "" },
    ] });
    assert_eq!(parse_account_type(&accounts, "A").as_deref(), Some("CFD"));
    assert_eq!(parse_account_type(&accounts, "B").as_deref(), Some("SPREADBET"));
    for id in ["C", "D", "Z"] {
        assert_eq!(parse_account_type(&accounts, id), None, "{id}");
    }
    assert_eq!(parse_account_type(&json!({}), "A"), None);
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

// ---- the conversion-pair memory, the key's allowance, the failure classes (vendor-doc review) -----------------

fn source() -> IgFactsSource {
    IgFactsSource::new(IgConfig {
        api_key: "k".to_string(),
        identifier: "i".to_string(),
        password: "p".to_string(),
        rest_base: "http://127.0.0.1:1".to_string(),
    })
}

/// What a read of a pair that was asked for and must not have been would be.
fn not_asked() -> Result<Value, IgFactsError> {
    panic!("the pair was remembered: IG must not be asked")
}

/// **IG's "no such epic" for a conversion pair is believed for an hour only when its `errorCode` SAYS the epic is
/// missing**; any other 400/404 (a generic `invalid.input`, a gateway's bare 404, a code this adapter has never
/// seen) is believed for five minutes. It used to be an hour for ANY 400/404 by status alone, so one hiccup on
/// `CS.D.GBPUSD.MINI.IP` made every pound-quoted cross `NoRate` for 3,600 s against an engine that denies opening
/// orders at 120 s. The codes are the documented `error.public-api.epic-not-found` (labs.ig.com, markets-epic) and
/// the older spelling this crate's tests were written against.
#[test]
fn a_pair_ig_says_is_no_epic_is_remembered_for_an_hour_and_any_other_400_or_404_for_five_minutes() {
    let name = "CS.D.GBPUSD.MINI.IP";
    let t0 = Instant::now();
    for (what, text, keep) in [
        (
            "the documented code",
            "ig GET /markets/CS.D.GBPUSD.MINI.IP: IG error 404: error.public-api.epic-not-found",
            MISSING_SPELLING_KEEP,
        ),
        (
            "the older spelling",
            "ig GET /markets/x: IG error 404: error.service.marketdata.instrument.epic.unavailable",
            MISSING_SPELLING_KEEP,
        ),
        ("a generic 400", "ig GET /markets/x: IG error 400: invalid.input", UNCODED_MISSING_KEEP),
        ("a bare 404", "ig GET /markets/x: IG error 404: {}", UNCODED_MISSING_KEEP),
        (
            "a gateway page",
            "ig GET /markets/x: IG error 404: <html>Not Found</html>",
            UNCODED_MISSING_KEEP,
        ),
    ] {
        let src = source();
        let said = || Err(IgFactsError::Refused(text.to_string()));
        assert_eq!(src.remembered_pair(name, t0, said), Ok(None), "{what}");
        let just_inside = t0 + keep - Duration::from_secs(1);
        assert_eq!(src.remembered_pair(name, just_inside, not_asked), Ok(None), "{what}");
        // past the keep IG is asked again: and answers this time
        let past = t0 + keep + Duration::from_secs(1);
        let body = json!({ "instrument": { "epic": name } });
        let answered = src.remembered_pair(name, past, || Ok(body.clone()));
        assert_eq!(answered, Ok(Some(body)), "{what}");
    }
    assert!(UNCODED_MISSING_KEEP <= Duration::from_secs(300));
    assert!(UNCODED_MISSING_KEEP < MISSING_SPELLING_KEEP);
}

/// **A successful read of the name forgets what was remembered about it**: the five minutes are not a sentence.
/// (`market` does the forgetting for every path that reads a name, the pair's own refresh included.)
#[test]
fn a_successful_read_of_a_pair_forgets_the_memory_of_its_absence() {
    let name = "CS.D.GBPUSD.MINI.IP";
    let t0 = Instant::now();
    let src = source();
    assert_eq!(
        src.remembered_pair(name, t0, || Err(IgFactsError::Refused("IG error 404: {}".into()))),
        Ok(None)
    );
    assert!(src.conversion.lock().unwrap().missing.contains_key(name));
    // a read of the same name by another path succeeded meanwhile (a held GBPUSD position's own refresh)
    src.conversion.lock().unwrap().missing.remove(name);
    let body = json!({ "instrument": { "epic": name } });
    assert_eq!(
        src.remembered_pair(name, t0 + Duration::from_secs(1), || Ok(body.clone())),
        Ok(Some(body))
    );
    // other failures (a 503, a cap spent) are not an answer about the pair at all: nothing is remembered
    let src = source();
    let err =
        src.remembered_pair(name, t0, || Err(IgFactsError::Unavailable("IG error 503".into())));
    assert_eq!(err, Err("IG error 503".to_string()));
    assert!(src.conversion.lock().unwrap().missing.is_empty());
}

/// **The cap follows the key's own allowance**: two thirds of `allowanceAccountOverall` (30 by default is the 20 this
/// adapter chose), read from the object or from the list row of OUR key, and a figure that is not a positive number
/// is no figure (the fallback stands).
#[test]
fn the_keys_allowance_is_read_from_either_shape_and_never_guessed() {
    assert_eq!(parse_account_allowance(&json!({ "allowanceAccountOverall": 30 }), "k"), Some(30));
    assert_eq!(
        parse_account_allowance(
            &json!([
                { "apiKey": "other", "allowanceAccountOverall": 5 },
                { "apiKey": "K", "allowanceAccountOverall": "60" }
            ]),
            "k"
        ),
        Some(60),
        "our key's row, the key compared without case, a numeric string a number"
    );
    for (what, body) in [
        ("our key is not listed", json!([{ "apiKey": "other", "allowanceAccountOverall": 5 }])),
        ("an empty list", json!([])),
        ("no figure", json!({ "allowanceApplicationOverall": 60 })),
        ("zero", json!({ "allowanceAccountOverall": 0 })),
        ("negative", json!({ "allowanceAccountOverall": -3 })),
        ("text", json!({ "allowanceAccountOverall": "plenty" })),
        ("a string body", json!("busy")),
        ("null", Value::Null),
    ] {
        assert_eq!(parse_account_allowance(&body, "k"), None, "{what}");
    }
    assert_eq!(
        allowance_cap(30),
        MAX_READS_PER_MINUTE,
        "IG's default allowance is this adapter's 20"
    );
    assert_eq!(allowance_cap(60), 40);
    assert_eq!(
        allowance_cap(1),
        MIN_READS_PER_MINUTE,
        "a tiny figure does not switch the reads off"
    );
    assert_eq!(allowance_cap(1_000_000), MAX_READS_CEILING);
}

/// **Which login failures only a CHANGE cures**: the details refused (401), a malformed request (400) and a key
/// refused for another reason than its allowance (403). A timeout, a reset, a 429, a 5xx and an allowance 403
/// are "not now".
#[test]
fn only_a_refused_login_is_a_failure_to_remember() {
    assert!(login_refused_for_good(401, "error.security.invalid-details"));
    assert!(login_refused_for_good(401, "error.security.too-many-failed-attempts"));
    assert!(login_refused_for_good(400, "invalid.input"));
    assert!(login_refused_for_good(403, "error.security.api-key-disabled"));
    assert!(!login_refused_for_good(403, "error.public-api.exceeded-api-key-allowance"));
    for status in [0, 1, 429, 500, 502, 503, 504] {
        assert!(!login_refused_for_good(status, "x"), "{status}");
    }
}

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

/// IG's `marginFactor` rides the facts as the epic's own margin rate (a fraction of notional), and only when it is
/// stated as a PERCENTAGE of a rate anybody could hold: an absent factor, an absent or other unit (`POINTS`) and a
/// figure outside (0, 100] percent state no rate, and the epic is margined at the engine's own, as before.
///
/// MUTATION PROOF: make `margin_rate` return `None` - the stated cases below come back `None`.
#[test]
fn the_margin_factor_is_carried_as_a_fraction_only_when_it_is_a_percentage() {
    let rate = |factor: Value, unit: Option<&str>| {
        let market = edited(|m| {
            m["instrument"]["marginFactor"] = factor;
            if let Some(unit) = unit {
                m["instrument"]["marginFactorUnit"] = json!(unit);
            }
        });
        market_facts(EPIC, &market, "USD").expect("admitted").margin_rate
    };
    let near = |got: Option<f64>, want: f64| got.is_some_and(|x| (x - want).abs() < 1e-12);
    assert!(near(rate(json!(3.33), Some("PERCENTAGE")), 0.0333));
    assert!(near(rate(json!("5"), Some("percentage")), 0.05), "a numeric string, any case");
    assert!(near(rate(json!(100), Some("PERCENTAGE")), 1.0), "no leverage at all is a rate");
    for (factor, unit) in [
        (json!(3.33), None),
        (json!(3.33), Some("POINTS")),
        (json!(0), Some("PERCENTAGE")),
        (json!(-1), Some("PERCENTAGE")),
        (json!(100.5), Some("PERCENTAGE")),
        (json!("lots"), Some("PERCENTAGE")),
        (json!(null), Some("PERCENTAGE")),
    ] {
        assert_eq!(rate(factor.clone(), unit), None, "{factor} {unit:?}");
    }
    assert_eq!(
        market_facts(EPIC, &eurusd(), "USD").expect("admitted").margin_rate,
        None,
        "a table that states no factor states no rate"
    );
}

/// The type words whose size was MEASURED (2026-10-28 and 2026-10-30) pass to the payload checks; every other word is
/// refused by its own name, with what is measured of it, and the sentence lists the admitted words.
#[test]
fn every_type_whose_size_was_not_measured_is_refused_by_name() {
    for (class, said) in [
        ("SHARES", "15.00 USD"),
        ("OPT_SHARES", "premium"),
        ("OPT_INDICES", "premium"),
        ("UNKNOWN", "premium"),
        ("SPRINT_MARKET", "not a linear position"),
        ("BINARY", "not a linear position"),
        ("BUNGEE_INDICES", "not a linear position"),
        ("CRYPTOCURRENCY", "not one whose size was measured"),
        ("BONDS", "not one whose size was measured"),
        ("", "not one whose size was measured"),
    ] {
        let why = refused(EPIC, &edited(|m| m["instrument"]["type"] = json!(class)), "USD");
        assert!(
            why.contains(said)
                && why.contains("admitted types are CURRENCIES, INDICES, COMMODITIES, RATES"),
            "{class}: {why}"
        );
    }
    for class in ["CURRENCIES", "INDICES", "COMMODITIES", "RATES"] {
        assert!(
            market_facts(EPIC, &edited(|m| m["instrument"]["type"] = json!(class)), "USD").is_ok(),
            "{class}"
        );
    }
}

/// An epic IG says it does not stream to this account (`instrument.streamingPricesAvailable` is `false`)
/// would be admitted, subscribed, ACKed or refused, and priced never: the door would hold every order for
/// a price that cannot come. It is refused by name at admission instead. Only an explicit `false` is a
/// verdict (the measured FX Minis say `true`; the hand-written fixtures here omit the key).
#[test]
fn an_epic_with_no_streaming_price_on_this_account_is_refused_by_name() {
    let none = edited(|m| m["instrument"]["streamingPricesAvailable"] = json!(false));
    let why = refused(EPIC, &none, "USD");
    assert!(why.contains(EPIC) && why.contains("streamingPricesAvailable"), "{why}");
    let streamed = edited(|m| m["instrument"]["streamingPricesAvailable"] = json!(true));
    assert!(market_facts(EPIC, &streamed, "USD").is_ok());
    assert!(market_facts(EPIC, &eurusd(), "USD").is_ok(), "an absent key is no verdict");
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
            "an expiry that is no contract's word",
            EPIC,
            edited(|m| m["instrument"]["expiry"] = json!("jun 26")),
            "USD",
            "expiry `jun 26`",
        ),
        (
            "stake unit",
            EPIC,
            edited(|m| m["instrument"]["unit"] = json!("AMOUNT")),
            "USD",
            "not in CONTRACTS",
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

// ---- the CAPTURED bodies (IG demo gateway, 2026-10-28 and 2026-10-30): the classes beyond a currency pair ------------

/// A `GET /markets/{epic}` (v3) body as the demo gateway answered it (`tests/fixtures/markets/`; the market bodies
/// carry no account id).
macro_rules! captured {
    ($epic:literal) => {
        serde_json::from_str::<Value>(include_str!(concat!(
            "../tests/fixtures/markets/",
            $epic,
            ".json"
        )))
        .expect("a captured body parses")
    };
}

/// The conversion pairs the demo gateway answered, for the converting door's `helper`: EUR/USD and GBP/USD Mini.
fn captured_pairs(name: &str) -> Result<Option<Value>, String> {
    Ok(match name {
        "CS.D.EURUSD.MINI.IP" => Some(captured!("CS.D.EURUSD.MINI.IP")),
        "CS.D.GBPUSD.MINI.IP" => Some(captured!("CS.D.GBPUSD.MINI.IP")),
        _ => None,
    })
}

fn near(got: f64, want: f64) -> bool {
    (got - want).abs() <= 1e-9 * want.abs().max(1.0)
}

/// **Every class whose minimum deal was booked on the demo is admitted on a USD account, on the numbers it booked.**
/// An index, a commodity, a coin and a bond future (typed CURRENCIES, INDICES, COMMODITIES or RATES), undated or
/// dated: the grid is one level step (`1 / scalingFactor`), the minimum is `minDealSize`, one unit of size is worth
/// `contractSize` of the epic's money per level, and the margin is IG's base factor. Each row is a deal that was
/// placed and closed 2026-10-28 / 2026-10-30 (the margin and the P/L matched `size x contractSize x level`), except
/// the two marked: same shape, never dealt.
///
/// RED on the base: `class_refusal` refused every type but CURRENCIES, and an epic that is no pair was refused for
/// its `onePipMeans`.
#[test]
fn a_usd_contract_epic_of_every_measured_class_is_admitted_on_the_numbers_the_demo_booked() {
    for (epic, market, min, multiplier, margin) in [
        ("IX.D.SPTRD.IFD.IP", captured!("IX.D.SPTRD.IFD.IP"), 1.0, 250.0, 0.005),
        ("IX.D.SPTRD.IFMM.IP", captured!("IX.D.SPTRD.IFMM.IP"), 1.0, 1.0, 0.005),
        ("IX.D.DOW.BMU.IP", captured!("IX.D.DOW.BMU.IP"), 0.1, 1.0, 0.005),
        ("IX.D.DOW.IFD.IP", captured!("IX.D.DOW.IFD.IP"), 0.2, 10.0, 0.005), // never dealt
        ("IX.D.DAX.IFMS.IP", captured!("IX.D.DAX.IFMS.IP"), 0.5, 1.0, 0.005),
        ("CC.D.CL.BMU.IP", captured!("CC.D.CL.BMU.IP"), 0.01, 1.0, 0.015),
        ("CC.D.NG.BMU.IP", captured!("CC.D.NG.BMU.IP"), 0.5, 1.0, 0.03), // never dealt
        ("CS.D.BITCOIN.CFD.IP", captured!("CS.D.BITCOIN.CFD.IP"), 0.001, 1.0, 0.10),
        ("CS.D.CFDGOLD.BMU.IP", captured!("CS.D.CFDGOLD.BMU.IP"), 0.1, 1.0, 0.005),
        ("IR.D.BOND100.FBMU2.IP", captured!("IR.D.BOND100.FBMU2.IP"), 1.0, 1.0, 0.005),
        ("MT.D.GC.FBMU3.IP", captured!("MT.D.GC.FBMU3.IP"), 0.5, 1.0, 0.005),
    ] {
        let facts =
            market_facts(epic, &market, "USD").unwrap_or_else(|why| panic!("{epic}: {why}"));
        assert_eq!(
            facts.properties,
            SymbolProperties {
                tick_size: 1.0,
                step_size: min,
                min_qty: min,
                ..SymbolProperties::default()
            },
            "{epic}"
        );
        assert_eq!(facts.contract_value, ContractValue::linear(multiplier), "{epic}");
        assert!(
            facts.margin_rate.is_some_and(|m| near(m, margin)),
            "{epic}: {:?}",
            facts.margin_rate
        );
    }
}

/// **An index or a bond quoted in another money is rated through IG's FX Mini family**: the DAX and the Bund in euros,
/// the FTSE in pounds, on a USD account, each converted at the inverse of its own table's `baseExchangeRate` (the
/// reading the demo gateway's tables fit), believed because the conversion pair's mid (EUR/USD, GBP/USD Mini) agrees
/// within 2%. The plain door still refuses them in words (nothing converts a quote currency there).
///
/// RED on the base: none of them was admitted.
#[test]
fn an_index_in_euros_or_pounds_is_rated_through_the_fx_mini_conversion_pair() {
    for (epic, market, quote, min, multiplier) in [
        ("IX.D.DAX.IFD.IP", captured!("IX.D.DAX.IFD.IP"), "EUR", 0.1, 25.0),
        ("IX.D.DAX.FBMU1.IP", captured!("IX.D.DAX.FBMU1.IP"), "EUR", 0.5, 1.0),
        ("IR.D.FGBL.FWM2.IP", captured!("IR.D.FGBL.FWM2.IP"), "EUR", 1.0, 2.0),
        ("IX.D.FTSE.CFD.IP", captured!("IX.D.FTSE.CFD.IP"), "GBP", 0.5, 10.0),
    ] {
        let mut helper = |name: &str| captured_pairs(name);
        let got = quoted_market_facts(epic, &market, "USD", 5_000, &mut helper)
            .unwrap_or_else(|why| panic!("{epic}: {}", why.text()));
        assert_eq!(got.account_ccy, "USD");
        assert_eq!(got.facts.contract_value, ContractValue::linear(multiplier), "{epic}");
        assert_eq!(got.facts.properties.min_qty, min, "{epic}");
        assert_eq!(got.facts.properties.tick_size, 1.0, "{epic}");
        let rate = got.quote.unwrap_or_else(|| panic!("{epic}: a foreign money needs a rate"));
        assert_eq!(rate.quote_ccy, quote, "{epic}");
        let base = market["instrument"]["currencies"][0]["baseExchangeRate"].as_f64().unwrap();
        assert!(near(rate.account_per_quote, 1.0 / base), "{epic}: {}", rate.account_per_quote);
        assert_eq!(rate.reading, quote_rate::Reading::BaseRowInverse, "{epic}");
        assert_eq!(rate.as_of_ms, 5_000, "{epic}: stamped with the ask");

        let why = refused(epic, &market, "USD");
        assert!(
            why.contains(&format!("quoted in {quote} but this account's currency is USD")),
            "{why}"
        );
    }
    // no conversion pair to witness the rate: refused in words, never valued at par
    let mut none = |_: &str| Ok(None);
    let why = quoted_market_facts(
        "IX.D.FTSE.CFD.IP",
        &captured!("IX.D.FTSE.CFD.IP"),
        "USD",
        1,
        &mut none,
    )
    .expect_err("no witness");
    assert!(
        why.text().contains("IX.D.FTSE.CFD.IP is quoted in GBP on a USD account")
            && why.text().contains("CS.D.GBPUSD.MINI.IP")
            && why.text().contains("CS.D.USDGBP.MINI.IP"),
        "{}",
        why.text()
    );
}

/// **USDJPY Mini is admitted** (its `lotSize` 100 is no fence: measured 2026-10-28, margin 0.2 x 10000 x 0.5% and the
/// P/L 0.2 x 10000 JPY per 1.0 of price, `lotSize` playing no part) on the converting door, in yen at the price's rate.
///
/// RED on the base: refused for `lotSize` 100.
#[test]
fn usdjpy_mini_with_its_lot_of_100_is_admitted_in_yen() {
    let epic = "CS.D.USDJPY.MINI.IP";
    let market = captured!("CS.D.USDJPY.MINI.IP");
    let mut helper = |name: &str| captured_pairs(name);
    let got = quoted_market_facts(epic, &market, "USD", 1, &mut helper).expect("admitted");
    assert_eq!(got.facts.contract_value, ContractValue::linear(10_000.0));
    assert_eq!((got.facts.properties.tick_size, got.facts.properties.min_qty), (0.01, 0.2));
    let rate = got.quote.expect("yen need a rate");
    assert_eq!(rate.quote_ccy, "JPY");
    assert!(
        rate.account_per_quote > 0.006 && rate.account_per_quote < 0.007,
        "{}",
        rate.account_per_quote
    );
}

/// **What is refused is refused by name, from the bodies IG answered**: a share (the commission measured on one Apple
/// share, and no stream), a UK share, the options (type UNKNOWN), a spread-bet epic (its DFB deal answered 403 on this
/// CFD login) and gold's DFB epic (named for the spread bet, not for its missing stream).
///
/// RED on the base for the spread bets (the sentence did not carry the 403) and the shares (no commission figure).
#[test]
fn a_share_an_option_and_a_spread_bet_are_refused_by_name_from_the_captured_bodies() {
    for (epic, market, said) in [
        ("UA.D.AAPL.CASH.IP", captured!("UA.D.AAPL.CASH.IP"), "15.00 USD"),
        ("UD.D.VODUS.CASH.IP", captured!("UD.D.VODUS.CASH.IP"), "MINIMUM COMMISSION"),
        ("KA.D.VOD.CASH.IP", captured!("KA.D.VOD.CASH.IP"), "MINIMUM COMMISSION"),
        ("OP.D.FTSE2.8300C.IP", captured!("OP.D.FTSE2.8300C.IP"), "premium"),
        ("OP.D.SPX1.5000P.IP", captured!("OP.D.SPX1.5000P.IP"), "premium"),
        ("IX.D.SPTRD.DAILY.IP", captured!("IX.D.SPTRD.DAILY.IP"), "HTTP 403"),
        ("CS.D.USCGC.TODAY.IP", captured!("CS.D.USCGC.TODAY.IP"), "HTTP 403"),
    ] {
        let why = refused(epic, &market, "USD");
        assert!(
            why.starts_with("ig: ") && why.contains(epic) && why.contains(said),
            "{epic}: {why}"
        );
    }
}

/// **The identity the contract shape rests on holds on every body captured**: `contractSize` equals `valueOfOnePip`
/// x `snapshot.scalingFactor` (IG quotes levels in pips, so a pip of one contract is worth `valueOfOnePip`). It is why
/// the cross-check needs no `onePipMeans` words ("1 Index Point", "1 cent per barrel", "0.001 $ per MMBtu", "1").
#[test]
fn the_contract_size_is_the_value_of_a_pip_times_the_scaling_factor_on_every_captured_epic() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/markets");
    let mut seen = 0;
    for entry in std::fs::read_dir(dir).expect("the fixture directory") {
        let path = entry.expect("entry").path();
        let body: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let i = &body["instrument"];
        if i["type"] == "SHARES" {
            continue; // a share carries neither a contract size nor a value of a pip
        }
        let (cs, vop, sf) = (
            get_f64(i, "contractSize"),
            get_f64(i, "valueOfOnePip"),
            body["snapshot"]["scalingFactor"].as_f64().unwrap(),
        );
        assert!(
            near(cs, vop * sf),
            "{}: contractSize {cs} vs valueOfOnePip {vop} x scalingFactor {sf}",
            path.display()
        );
        seen += 1;
    }
    assert!(seen >= 20, "{seen} bodies read");
}

/// The contract shape's own refusals, from the DAX body: an epic that is no pair needs its snapshot's scaling factor
/// (the level grid), exactly one dealing currency (the money its levels are in), and a `valueOfOnePip` that agrees.
#[test]
fn a_contract_epic_whose_grid_or_money_is_not_stated_is_refused_in_words() {
    let epic = "IX.D.DAX.IFMS.IP"; // quoted in USD: the plain door is enough
    let edited = |edit: &dyn Fn(&mut Value)| {
        let mut market = captured!("IX.D.DAX.IFMS.IP");
        edit(&mut market);
        market
    };
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "no scaling factor",
            edited(&|m| {
                m["snapshot"].as_object_mut().unwrap().remove("scalingFactor");
            }),
            "states no scalingFactor",
        ),
        (
            "a zero scaling factor",
            edited(&|m| m["snapshot"]["scalingFactor"] = json!(0)),
            "states no scalingFactor",
        ),
        (
            "no currency",
            edited(&|m| m["instrument"]["currencies"] = json!([])),
            "lists no dealing currency",
        ),
        (
            "two currencies",
            edited(&|m| {
                m["instrument"]["currencies"] = json!([{ "code": "USD" }, { "code": "EUR" }]);
            }),
            "lists 2 dealing currencies (USD, EUR)",
        ),
        (
            "a value of a pip that disagrees",
            edited(&|m| m["instrument"]["valueOfOnePip"] = json!("25.00")),
            "valueOfOnePip says 25",
        ),
        (
            "a scaling factor that breaks the identity",
            edited(&|m| m["snapshot"]["scalingFactor"] = json!(100)),
            "valueOfOnePip says 1",
        ),
        (
            "sized in shares",
            edited(&|m| m["instrument"]["unit"] = json!("SHARES")),
            "not in CONTRACTS",
        ),
    ];
    for (what, market, said) in rows {
        let why = refused(epic, &market, "USD");
        assert!(why.contains(said), "{what}: {why}");
    }
}

/// A market that is not `TRADEABLE` is named in words at the admission (and admitted all the same); a tradeable
/// one and one that states no status say nothing.
#[test]
fn a_market_that_is_not_tradeable_is_named_and_a_tradeable_one_is_silent() {
    assert_eq!(not_tradeable_words(EPIC, &eurusd()), None, "TRADEABLE");
    let mut closed = eurusd();
    closed["snapshot"]["marketStatus"] = json!("CLOSED");
    let words = not_tradeable_words(EPIC, &closed).expect("CLOSED is named");
    assert!(words.contains(EPIC) && words.contains("`CLOSED`"), "{words}");
    assert!(market_facts(EPIC, &closed, "USD").is_ok(), "the facts are the same: it is only said");
    let mut unstated = eurusd();
    unstated["snapshot"] = json!({});
    assert_eq!(not_tradeable_words(EPIC, &unstated), None, "no status is no verdict");
    assert_eq!(not_tradeable_words(EPIC, &json!({})), None);
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
        account_pin: None,
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

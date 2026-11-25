//! The operator's lookup, pure halves: which epic a search body yields for a pair, and the row its market details
//! make. Over hand-written search bodies (`tests/fixtures/markets_search_pairs.json`, its `_provenance` says so) and
//! the CAPTURED market details the admission tests read (`tests/fixtures/markets/`).

use serde_json::Value;
use vike_model::{Contract, ContractFx};

use super::*;
use crate::refusals::IgSymbolRefusalId;

const SEARCHES: &str = include_str!("../tests/fixtures/markets_search_pairs.json");
const EURUSD_MINI: &str = include_str!("../tests/fixtures/markets/CS.D.EURUSD.MINI.IP.json");
const EURUSD_CFD: &str = include_str!("../tests/fixtures/markets/CS.D.EURUSD.CFD.IP.json");
const USDJPY_MINI: &str = include_str!("../tests/fixtures/markets/CS.D.USDJPY.MINI.IP.json");

fn search(term: &str) -> Value {
    let all: Value = serde_json::from_str(SEARCHES).expect("the fixture parses");
    all[term].clone()
}

/// Of the pair's five rows, the FX Mini: the standard CFD and the DFB are passed over by name, the dated forward
/// too, and a row whose epic only contains the word is not the pair at all.
#[test]
fn the_fx_mini_is_chosen_over_every_other_form_and_each_is_named() {
    let pick = pick_fx_epic("eurusd", &search("EURUSD")).expect("a pick");
    assert_eq!(pick.epic, "CS.D.EURUSD.MINI.IP");
    assert_eq!(pick.name, "EUR/USD Mini");
    assert_eq!(
        pick.passed_over,
        ["CS.D.EURUSD.CFD.IP", "CS.D.EURUSD.TODAY.IP", "CS.D.EURUSD.FWM2.IP"],
        "every other epic of the pair, rolling first"
    );
    let rows = passed_over_rows(&pick, &search("EURUSD"));
    let names: Vec<&str> = rows.iter().map(|r| r.raw_symbol.as_str()).collect();
    assert_eq!(names, ["CS.D.EURUSD.CFD.IP", "CS.D.EURUSD.TODAY.IP", "CS.D.EURUSD.FWM2.IP"]);
}

/// **`EURUSD.CFD` is the standard lot** (decision 0290 item 6, 2026-11-17): the search asks for the pair alone, the
/// `.CFD.IP` form is chosen over the Mini, and every other form is passed over.
#[test]
fn the_cfd_name_chooses_the_standard_lot() {
    assert_eq!(search_term(" eurusd.cfd "), "EURUSD");
    assert_eq!(search_term("EURUSD"), "EURUSD");
    let pick = pick_fx_epic("eurusd.cfd", &search("EURUSD")).expect("a pick");
    assert_eq!(pick.epic, "CS.D.EURUSD.CFD.IP");
    assert_eq!(pick.name, "EUR/USD");
    assert_eq!(
        pick.passed_over,
        ["CS.D.EURUSD.MINI.IP", "CS.D.EURUSD.TODAY.IP", "CS.D.EURUSD.FWM2.IP"],
        "every other epic of the pair, rolling first"
    );
}

/// No Mini: the bare pair is the one rolling CFD form, the spread bet passed over (its row is then named
/// `GBPCHF.CFD`, the only name such a pair has); `GBPCHF.CFD` chooses the same epic.
#[test]
fn with_no_mini_the_one_rolling_cfd_is_chosen() {
    for typed in ["GBPCHF", "GBPCHF.CFD"] {
        let pick = pick_fx_epic(typed, &search("GBPCHF")).expect("a pick");
        assert_eq!(pick.epic, "CS.D.GBPCHF.CFD.IP", "{typed}");
        assert_eq!(pick.passed_over, ["CS.D.GBPCHF.TODAY.IP"], "{typed}");
    }
}

/// Two rolling CFD forms and no Mini: nothing is chosen, and the refusal names both; no form of the pair at all is
/// a refusal naming what IG did list, and so is a `.CFD` name whose standard lot IG does not list (never its Mini).
#[test]
fn an_uncertain_or_missing_pair_is_refused_naming_what_was_listed() {
    let why = pick_fx_epic("AUDNZD", &search("AUDNZD")).expect_err("two forms, no Mini");
    let IgSymbolRefusal::SeveralRollingForms { count, listed, .. } = &why else {
        panic!("{why:?}")
    };
    assert_eq!(*count, 2, "{why}");
    assert!(
        listed.starts_with("CS.D.AUDNZD.CFD.IP, CS.D.AUDNZD.CFE.IP"),
        "both are named: {listed}"
    );
    let why = pick_fx_epic("XAUXAG", &search("XAUXAG")).expect_err("no such pair");
    assert!(
        matches!(&why, IgSymbolRefusal::NoRollingPair { pair, .. } if pair == "XAUXAG"),
        "{why:?}"
    );
    let why = pick_fx_epic("EURUSD", &serde_json::json!({})).expect_err("an empty body");
    assert_eq!(
        why,
        IgSymbolRefusal::NoRollingPair { pair: "EURUSD".to_string(), listed: String::new() },
        "nothing listed, nothing named"
    );
    let only_mini = serde_json::json!({ "markets": [
        { "epic": "CS.D.EURUSD.MINI.IP", "instrumentName": "EUR/USD Mini", "instrumentType": "CURRENCIES", "expiry": "-" }
    ] });
    let why = pick_fx_epic("EURUSD.CFD", &only_mini).expect_err("no standard lot listed");
    assert_eq!(
        why,
        IgSymbolRefusal::NoStandardLot {
            pair: "EURUSD".to_string(),
            named: "CS.D.EURUSD.CFD.IP".to_string(),
            listed: "CS.D.EURUSD.MINI.IP".to_string(),
        }
    );
}

/// The row out of the captured EUR/USD Mini details: the pip as the tick, `minDealSize` as the size grid,
/// `contractSize` as what one unit of size is, the pair off `onePipMeans`, IG's name and type word, and its name
/// `EURUSD` (IG's rule, decision 0290 item 6).
#[test]
fn the_row_carries_the_facts_the_market_details_state() {
    let pick = pick_fx_epic("EURUSD", &search("EURUSD")).expect("a pick");
    let market: Value = serde_json::from_str(EURUSD_MINI).expect("the capture parses");
    let row = describe(&pick, &market).expect("a row");
    assert_eq!((row.venue.as_str(), row.raw_symbol.as_str()), ("ig", "CS.D.EURUSD.MINI.IP"));
    assert_eq!(vike_catalog::user_name_of_row(&row).as_deref(), Ok("EURUSD"));
    assert_eq!((row.base.as_str(), row.quote.as_str()), ("EUR", "USD"));
    assert_eq!(row.description, "EUR/USD Mini");
    assert_eq!(row.contract_type.as_deref(), Some("CURRENCIES"));
    assert_eq!(
        row.contract,
        Contract::Fx(ContractFx {
            pip_size: Some(0.0001),
            lot_size: Some(10_000.0),
            swap_long: None,
            swap_short: None
        })
    );
    assert_eq!(row.properties.tick_size, 0.0001);
    assert_eq!((row.properties.step_size, row.properties.min_qty), (0.1, 0.1));
    assert_eq!(row.properties.contract_size, 10_000.0);
    assert_eq!(row.asset_class(), vike_model::AssetClass::Fx);
}

/// **The standard lot's row** out of its captured details (demo gateway, 2026-11-17): the same pair, pip and quote as
/// the Mini, ten times its unit (`contractSize` 100,000) and a minimum deal of 1.0 (the Mini's 0.1), named
/// `EURUSD.CFD`, an FX row like the Mini's.
#[test]
fn the_standard_lots_row_is_ten_minis_and_named_eurusd_cfd() {
    let pick = pick_fx_epic("EURUSD.CFD", &search("EURUSD")).expect("a pick");
    let market: Value = serde_json::from_str(EURUSD_CFD).expect("the capture parses");
    let row = describe(&pick, &market).expect("a row");
    assert_eq!(row.raw_symbol, "CS.D.EURUSD.CFD.IP");
    assert_eq!(vike_catalog::user_name_of_row(&row).as_deref(), Ok("EURUSD.CFD"));
    assert_eq!((row.base.as_str(), row.quote.as_str()), ("EUR", "USD"));
    assert_eq!(row.description, "EUR/USD");
    assert_eq!(row.properties.tick_size, 0.0001);
    assert_eq!((row.properties.step_size, row.properties.min_qty), (1.0, 1.0));
    assert_eq!(row.properties.contract_size, 100_000.0);
    assert_eq!(row.asset_class(), vike_model::AssetClass::Fx);
}

/// A yen pair: the pip is 0.01 and the quote JPY, read off the pair, never off the account.
#[test]
fn a_yen_pair_is_quoted_in_yen() {
    let pick =
        Pick { epic: "CS.D.USDJPY.MINI.IP".into(), name: String::new(), passed_over: Vec::new() };
    let market: Value = serde_json::from_str(USDJPY_MINI).expect("the capture parses");
    let row = describe(&pick, &market).expect("a row");
    assert_eq!((row.base.as_str(), row.quote.as_str()), ("USD", "JPY"));
    assert_eq!(row.properties.tick_size, 0.01);
    assert_eq!(row.properties.min_qty, 0.2);
    assert_eq!(
        row.description, "USD/JPY Mini",
        "the details' name wins over the search's empty one"
    );
}

/// A row with a missing fact is refused, never stored with a default: the answer for another epic, a pair the epic
/// does not name, no contract size, no minimum deal.
#[test]
fn a_row_with_a_missing_fact_is_refused() {
    let pick = pick_fx_epic("EURUSD", &search("EURUSD")).expect("a pick");
    let good: Value = serde_json::from_str(EURUSD_MINI).expect("the capture parses");
    let broken = |pointer: &str, value: Value| {
        let mut market = good.clone();
        *market.pointer_mut(pointer).expect("the field exists") = value;
        describe(&pick, &market).expect_err(pointer).id()
    };
    assert_eq!(
        broken("/instrument/epic", "CS.D.GBPUSD.MINI.IP".into()),
        IgSymbolRefusalId::RowAnsweredForAnotherEpic
    );
    assert_eq!(
        broken("/instrument/onePipMeans", "0.0001 USD/GBP".into()),
        IgSymbolRefusalId::RowPairNotNamed
    );
    assert_eq!(
        broken("/instrument/contractSize", Value::Null),
        IgSymbolRefusalId::RowNoContractSize
    );
    assert_eq!(
        broken("/dealingRules/minDealSize/value", 0.into()),
        IgSymbolRefusalId::RowNoMinimumDeal
    );
    let mut index = good.clone();
    index["instrument"]["type"] = "INDICES".into();
    assert!(
        matches!(describe(&pick, &index), Err(IgSymbolRefusal::RowNotCurrencies { kind, .. }) if kind == "INDICES"),
        "the type word is named"
    );
}

/// No account has handed this process a session: the lookup says so and asks nothing.
#[test]
fn with_no_held_session_nothing_is_asked() {
    if HELD.lock().unwrap_or_else(PoisonError::into_inner).is_some() {
        return; // another test of this binary held one; the offline suite proves the held path
    }
    assert_eq!(
        operator_lookup("EURUSD").expect_err("no session"),
        IgSymbolRefusal::NoHeldSession.to_string()
    );
}

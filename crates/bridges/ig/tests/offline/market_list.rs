//! IG's market list (`vike_ig::IgPropertiesSource::list_markets`, `crates/bridges/ig/src/market_list.rs`) over `fake_ig`
//! (a loopback gateway, never IG), served the pages the demo gateway answered on 2026-11-16
//! (`tests/fixtures/categories/pages.json`, its `_provenance` says what is captured): the list rides the source's kept
//! session, its rows state no grid, and ONE epic's facts are then one market read, the decision-0227 path.

use serde_json::{Value, json};
use vike_ig::IgPropertiesSource;
use vike_model::{ContractValue, SymbolProperties};

use crate::fake_ig::{Route, accounts_route, login_route, serve};

const PAGES: &str = include_str!("../fixtures/categories/pages.json");
const EURUSD_MINI: &str = include_str!("../fixtures/markets/CS.D.EURUSD.MINI.IP.json");
const USDJPY_MINI: &str = include_str!("../fixtures/markets/CS.D.USDJPY.MINI.IP.json");

/// **The list is the categories' pages over the kept session, and one picked epic's facts are one market read**:
/// two categories as the demo gateway answered them (FX and CRYPTOCURRENCY, page 0 each, `totalPages` 1), one login
/// for the list and the facts alike, three list reads, no market read until an epic is asked, then exactly one.
///
/// MUTATION PROOF: read each listed epic's details in `list_markets` and `GET /markets/` is six before the ask.
#[test]
fn the_list_rides_the_kept_session_and_one_epics_facts_are_one_market_read() {
    let doc: Value = serde_json::from_str(PAGES).expect("the fixture parses");
    let ig = serve(vec![
        login_route(),
        accounts_route("USD"),
        Route::ok(
            "GET /categories/FX/instruments?pageNumber=0&pageSize=1000",
            doc["pages"]["FX"][0].clone(),
        ),
        Route::ok(
            "GET /categories/CRYPTOCURRENCY/instruments?pageNumber=0&pageSize=1000",
            doc["pages"]["CRYPTOCURRENCY"][0].clone(),
        ),
        Route::ok(
            "GET /categories HTTP",
            json!({ "categories": [
                { "code": "FX", "nonTradeable": false },
                { "code": "CRYPTOCURRENCY", "nonTradeable": false },
            ] }),
        ),
        Route::ok(
            "GET /markets/CS.D.EURUSD.MINI.IP",
            serde_json::from_str(EURUSD_MINI).expect("the capture parses"),
        ),
    ]);
    let source = IgPropertiesSource::new(ig.config());
    let rows = source.list_markets().expect("the list");
    let epics: Vec<&str> = rows.iter().map(|r| r.raw_symbol.as_str()).collect();
    assert_eq!(
        epics,
        [
            "CS.D.EURILS.MINI.IP",
            "CS.D.EURUSD.MINI.IP",
            "CS.D.EURUSD.CFD.IP",
            "CS.D.BITCOIN.CFM.IP",
            "CS.D.BITCOIN.CFD.IP",
            "CS.D.BSVUSD.CFD.IP"
        ]
    );
    assert!(
        rows.iter().all(|r| r.properties == SymbolProperties::default()),
        "a list row states no grid: the facts are read on demand"
    );
    assert_eq!(ig.count("GET /categories"), 3, "{:?}", ig.served());
    assert_eq!(ig.count("GET /markets/"), 0, "nothing is read per epic for the list");

    let facts = source.facts("CS.D.EURUSD.MINI.IP").expect("the picked epic's facts");
    assert_eq!(facts.properties.tick_size, 0.0001);
    assert_eq!(facts.contract_value, ContractValue::linear(10_000.0));
    assert_eq!(ig.count("GET /markets/CS.D.EURUSD.MINI.IP"), 1, "one market read, on demand");
    assert_eq!(ig.count("POST /session"), 1, "one login for the list and the facts");
}

/// **An epic's facts are fetched ONCE and kept** (the owner's ruling of 2026-11-16, "what if 1k symbols, will you
/// check each every time?"): two uses of one epic, on the plain door and on the converting one, make exactly one
/// market read; only a list refresh forgets the kept details, and the next use then reads the epic once more. An epic
/// that answers with a RATE (USDJPY on a USD account) is read on every ask: its rate must be fresh.
///
/// MUTATION PROOF: drop `keep_market` and the second use reads again (2, not 1).
#[test]
fn two_uses_of_one_epic_make_exactly_one_request_until_a_list_refresh() {
    let doc: Value = serde_json::from_str(PAGES).expect("the fixture parses");
    let ig = serve(vec![
        login_route(),
        Route::ok(
            "GET /accounts",
            json!({ "accounts": [{ "accountId": "ACC1", "currency": "USD", "accountType": "CFD" }] }),
        ),
        Route::ok(
            "GET /categories/CRYPTOCURRENCY/instruments?pageNumber=0&pageSize=1000",
            doc["pages"]["CRYPTOCURRENCY"][0].clone(),
        ),
        Route::ok(
            "GET /categories HTTP",
            json!({ "categories": [{ "code": "CRYPTOCURRENCY", "nonTradeable": false }] }),
        ),
        Route::ok(
            "GET /markets/CS.D.EURUSD.MINI.IP",
            serde_json::from_str(EURUSD_MINI).expect("the capture parses"),
        ),
        Route::ok(
            "GET /markets/CS.D.USDJPY.MINI.IP",
            serde_json::from_str(USDJPY_MINI).expect("the capture parses"),
        ),
    ]);
    let source = IgPropertiesSource::new(ig.config());
    const EPIC: &str = "CS.D.EURUSD.MINI.IP";
    let first = source.facts(EPIC).expect("the first use");
    let second = source.facts(EPIC).expect("the second use");
    assert_eq!(first, second);
    let quoted = source.facts_quoted(EPIC).expect("the converting door");
    assert!(quoted.quote.is_none(), "a dollar epic on a dollar account needs no rate");
    assert_eq!(quoted.properties, first.properties);
    source.facts_quoted(EPIC).expect("again");
    assert_eq!(ig.count("GET /markets/CS.D.EURUSD.MINI.IP"), 1, "{:?}", ig.served());

    source.list_markets().expect("a list refresh");
    source.facts(EPIC).expect("after the refresh");
    assert_eq!(
        ig.count("GET /markets/CS.D.EURUSD.MINI.IP"),
        2,
        "the refresh forgot it: read once more"
    );
    source.facts(EPIC).expect("and kept again");
    assert_eq!(ig.count("GET /markets/CS.D.EURUSD.MINI.IP"), 2);

    for _ in 0..2 {
        assert!(source.facts_quoted("CS.D.USDJPY.MINI.IP").expect("converted").quote.is_some());
    }
    assert_eq!(ig.count("GET /markets/CS.D.USDJPY.MINI.IP"), 2, "a rate is read on every ask");
}

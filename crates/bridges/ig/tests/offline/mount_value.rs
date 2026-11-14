//! **What one unit of the MOUNTED epic is worth** (`vike_ig::mount_contract_value`, read by the adapter's start)
//! over the SCRIPTED LOOPBACK IG double (`fake_ig`): the real session, the real account and market reads, and the
//! market body the demo gateway answered for the wired epic (`tests/fixtures/markets/CS.D.EURUSD.MINI.IP.json`).
//! Nothing here touches IG's network. Until this read the mount stated no value and the engine booked the epic at
//! 1 where an admitted one was booked at its `contractSize`.

use serde_json::{Value, json};
use vike_model::ContractValue;

use crate::fake_ig::{Route, login_route, serve};

/// The epic the ig mount is wired to (`vike_venues::venue_wiring`'s ig row).
const WIRED: &str = "CS.D.EURUSD.MINI.IP";

/// A CFD account row in `currency`, the shape the converting facts read requires.
fn cfd_account(currency: &str) -> Route {
    Route::ok(
        "GET /accounts",
        json!({ "accounts": [{
            "accountId": "ACC1", "accountType": "CFD", "currency": currency,
            "balance": { "balance": 1000.0 }
        }] }),
    )
}

fn captured_wired() -> Value {
    serde_json::from_str(include_str!("../fixtures/markets/CS.D.EURUSD.MINI.IP.json"))
        .expect("a captured body parses")
}

/// The mounted EUR/USD Mini is worth its row's `contractSize`, 10,000 units a contract, the value an admitted
/// epic of the same row is booked by: one login, one account read, one market read.
#[test]
fn the_mounted_epic_is_worth_its_captured_rows_contract_size() {
    let ig = serve(vec![
        login_route(),
        cfd_account("USD"),
        Route::ok("GET /markets/CS.D.EURUSD.MINI.IP", captured_wired()),
    ]);
    let value = vike_ig::mount_contract_value(&ig.config(), WIRED).expect("the row is readable");
    assert_eq!(value, ContractValue::linear(10_000.0));
    assert_eq!(ig.count("POST /session"), 1);
    assert_eq!(ig.count("GET /markets/"), 1, "{:?}", ig.served());
}

/// A market row that cannot be read (IG down, the epic unknown) is an ERROR with IG's words, never a value: the
/// adapter's start then leaves the account NOT RUNNING.
#[test]
fn a_mounted_epic_whose_row_cannot_be_read_has_no_value() {
    for (status, said) in [(503, "503"), (404, "404")] {
        let ig = serve(vec![
            login_route(),
            cfd_account("USD"),
            Route::status(
                "GET /markets/",
                status,
                json!({ "errorCode": "error.public-api.failure" }),
            ),
        ]);
        let why = vike_ig::mount_contract_value(&ig.config(), WIRED).expect_err("no row, no value");
        assert!(why.contains(said), "{status}: {why}");
    }
}

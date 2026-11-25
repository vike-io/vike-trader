//! **What one unit of the MOUNTED epic is worth** (`vike_ig::mount_contract_value`, read by the adapter's start)
//! over the SCRIPTED LOOPBACK IG double (`fake_ig`): the real session, the real account and market reads, and the
//! market body the demo gateway answered for the wired epic (`tests/fixtures/markets/CS.D.EURUSD.MINI.IP.json`).
//! Nothing here touches IG's network. Until this read the mount stated no value and the engine booked the epic at
//! 1 where an admitted one was booked at its `contractSize`.

use serde_json::{Value, json};
use vike_ig::IgPropertiesError;
use vike_ig::refusals::IgSymbolRefusal;
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

/// The mounted EUR/USD Mini (`EURUSD`) is worth its row's `contractSize`, 10,000 units a contract, the value an
/// admitted epic of the same row is booked by: one login, one account read, one market read. Its standard lot
/// (`EURUSD.CFD`, captured on the demo gateway on 2026-11-17) is worth ten times that.
#[test]
fn the_mounted_epic_is_worth_its_captured_rows_contract_size() {
    let standard: Value =
        serde_json::from_str(include_str!("../fixtures/markets/CS.D.EURUSD.CFD.IP.json"))
            .expect("a captured body parses");
    for (epic, key, body, unit) in [
        (WIRED, "GET /markets/CS.D.EURUSD.MINI.IP", captured_wired(), 10_000.0),
        ("CS.D.EURUSD.CFD.IP", "GET /markets/CS.D.EURUSD.CFD.IP", standard, 100_000.0),
    ] {
        let ig = serve(vec![login_route(), cfd_account("USD"), Route::ok(key, body)]);
        let value = vike_ig::mount_contract_value(&ig.config(), epic).expect("the row is readable");
        assert_eq!(value, ContractValue::linear(unit), "{epic}");
        assert_eq!(ig.count("POST /session"), 1, "{epic}");
        assert_eq!(ig.count("GET /markets/"), 1, "{epic}: {:?}", ig.served());
    }
}

/// A market row that cannot be read (IG down, the epic unknown) is an ERROR with IG's words, never a value: the
/// adapter's start then leaves the account NOT RUNNING, by the error's kind. A 503 is one a later try can pass (the
/// start is asked again by itself); a 404 says the epic is no market, a refusal that waits for the operator.
#[test]
fn a_mounted_epic_whose_row_cannot_be_read_has_no_value() {
    for (status, transient) in [(503, true), (404, false)] {
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
        assert!(
            matches!(
                &why,
                IgPropertiesError::Refused(IgSymbolRefusal::MarketReadFailed { .. })
                    | IgPropertiesError::Unavailable(IgSymbolRefusal::MarketReadFailed { .. }, _)
            ),
            "{status}: the market read failed: {why:?}"
        );
        assert_eq!(why.kind().is_terminal(), !transient, "{status}: {why:?}");
    }
}

/// **The LOGIN of the mount's read, classified**: a login IG never answered (the socket closed with the request
/// unanswered: a network failure) is transient, so the start is asked again by itself; a login IG answered and
/// declined (401, the details refused) is a refusal that waits for the operator.
#[test]
fn a_mount_login_that_never_reached_ig_is_transient_and_a_declined_one_is_not() {
    let unanswered = serve(vec![Route::hang_up("POST /session")]);
    let why = vike_ig::mount_contract_value(&unanswered.config(), WIRED).expect_err("no login");
    assert!(!why.kind().is_terminal(), "no answer is no verdict: {why:?}");

    let declined = serve(vec![Route::status(
        "POST /session",
        401,
        json!({ "errorCode": "error.security.invalid-details" }),
    )]);
    let why = vike_ig::mount_contract_value(&declined.config(), WIRED).expect_err("declined");
    assert!(
        matches!(&why, IgPropertiesError::Account(IgSymbolRefusal::LoginFailed { why })
            if why.contains("invalid-details")),
        "IG's own code is carried: {why:?}"
    );
    assert!(why.kind().is_terminal(), "a declined login parks: {why:?}");
}

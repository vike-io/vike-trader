//! **A login trades on a USD CFD account, not on whichever account IG made the login's default**, driven through
//! the real `IgSession::login` / `relogin` and the real `IgFactsSource` over `fake_ig` (a hand-written loopback
//! gateway in the documented shapes, never IG): `POST /session` lands on the default, `GET /accounts` lists the
//! login's accounts, and `PUT /session` moves the session when the rule (`crate::account_pick`) says so.
//!
//! These prove this crate's handling of the documented shape. What IG itself answers to a `PUT /session` (whether
//! its answer carries a fresh `X-SECURITY-TOKEN`) is not measured here: both are handled and both are tested.

use serde_json::{Value, json};
use vike_ig::{IgFactsSource, IgSession};

use crate::fake_ig::{FakeIg, Route, serve};

/// `POST /session` landing on `current`.
fn login_on(current: &str) -> Route {
    Route {
        key: "POST /session",
        status: 200,
        body: json!({ "currentAccountId": current, "lightstreamerEndpoint": "" }).to_string(),
        headers: &[("CST", "cst-1"), ("X-SECURITY-TOKEN", "xst-1")],
        delay: std::time::Duration::ZERO,
    }
}

/// One `GET /accounts` row.
fn row(id: &str, kind: &str, currency: &str) -> Value {
    json!({ "accountId": id, "accountType": kind, "currency": currency, "status": "ENABLED",
            "preferred": false, "balance": { "balance": 1000.0 } })
}

fn accounts(rows: Vec<Value>) -> Route {
    Route::ok("GET /accounts", json!({ "accounts": rows }))
}

/// `PUT /session` answering with a fresh token pair, as IG may.
fn switch_with_new_tokens() -> Route {
    Route {
        key: "PUT /session",
        status: 200,
        body: json!({ "dealingEnabled": true }).to_string(),
        headers: &[("CST", "cst-2"), ("X-SECURITY-TOKEN", "xst-2")],
        delay: std::time::Duration::ZERO,
    }
}

/// `PUT /session` answering with no token headers.
fn switch_keeping_tokens() -> Route {
    Route::ok("PUT /session", json!({ "dealingEnabled": true }))
}

fn the_switch_body(ig: &FakeIg) -> Vec<Value> {
    ig.bodies_of("PUT /session")
}

/// **The headline**: a login whose default is a spread-bet account is moved to the login's one USD CFD account,
/// with `defaultAccount: false` (the login's default, and every other session of it, is left alone), and the
/// session's account, currency and tokens are the new account's.
#[test]
fn a_spreadbet_default_is_moved_to_the_one_usd_cfd_account() {
    let ig = serve(vec![
        login_on("SB1"),
        accounts(vec![row("SB1", "SPREADBET", "USD"), row("CFD1", "CFD", "USD")]),
        switch_with_new_tokens(),
    ]);
    let session = IgSession::login(&ig.config()).expect("the login settles");
    assert_eq!(session.account_id, "CFD1", "the session trades the USD CFD account");
    assert_eq!(session.account_currency.as_deref(), Some("USD"));
    assert_eq!(the_switch_body(&ig), vec![json!({ "accountId": "CFD1", "defaultAccount": false })]);
    assert_eq!(session.ls_password(), "CST-cst-2|XST-xst-2", "the fresh pair the switch answered");
    assert_eq!((ig.count("POST /session"), ig.count("GET /accounts")), (1, 1));
}

/// IG's reference does not say the switch answers with new tokens: when it sends none the session keeps its pair.
#[test]
fn a_switch_that_answers_no_tokens_keeps_the_pair() {
    let ig = serve(vec![
        login_on("G1"),
        accounts(vec![row("G1", "CFD", "GBP"), row("U1", "CFD", "USD")]),
        switch_keeping_tokens(),
    ]);
    let session = IgSession::login(&ig.config()).expect("the login settles");
    assert_eq!(session.account_id, "U1");
    assert_eq!(session.ls_password(), "CST-cst-1|XST-xst-1");
}

/// Idempotent: a default that is already a USD CFD account is not touched, and costs no `PUT`.
#[test]
fn a_default_that_is_a_usd_cfd_account_is_not_switched() {
    let ig = serve(vec![
        login_on("CFD1"),
        accounts(vec![row("CFD1", "CFD", "USD"), row("SB1", "SPREADBET", "GBP")]),
        switch_with_new_tokens(),
    ]);
    let session = IgSession::login(&ig.config()).expect("the login settles");
    assert_eq!(session.account_id, "CFD1");
    assert_eq!(ig.count("PUT /session"), 0, "nothing to switch: {:?}", ig.served());
}

/// A default that is fine stays when a SECOND USD CFD account exists: the choice does not move under a
/// working account.
#[test]
fn a_fitting_default_stays_when_another_account_fits_too() {
    let ig = serve(vec![
        login_on("CFD2"),
        accounts(vec![row("CFD1", "CFD", "USD"), row("CFD2", "CFD", "USD")]),
        switch_with_new_tokens(),
    ]);
    let session = IgSession::login(&ig.config()).expect("the login settles");
    assert_eq!(session.account_id, "CFD2");
    assert_eq!(ig.count("PUT /session"), 0);
}

/// **None fits: refused by name**, listing every account found with its type and currency, and nothing is
/// switched to a spread-bet or physical account. The refusal is not an HTTP status, so the lanes do not retry it.
#[test]
fn a_login_with_no_usd_cfd_account_is_refused_by_name_and_never_switched_to_another_kind() {
    let ig = serve(vec![
        login_on("G1"),
        accounts(vec![
            row("G1", "CFD", "GBP"),
            row("SB1", "SPREADBET", "USD"),
            row("PH1", "PHYSICAL", "USD"),
        ]),
        switch_with_new_tokens(),
    ]);
    let err = IgSession::login(&ig.config()).err().expect("no account to trade");
    assert_ne!(err.status, 0, "a refusal, not a transport failure: it is not retried");
    for id in ["`G1`", "`SB1`", "`PH1`"] {
        assert!(err.message.contains(id), "{id} is named: {}", err.message);
    }
    assert!(
        err.message.contains("CFD GBP") && err.message.contains("SPREADBET USD"),
        "{}",
        err.message
    );
    assert_eq!(ig.count("PUT /session"), 0, "never moved to a spread-bet or physical account");
}

/// Several fit and the default is not one of them: refused by name, listing the candidates.
#[test]
fn several_usd_cfd_accounts_and_a_default_that_is_none_of_them_is_refused_by_name() {
    let ig = serve(vec![
        login_on("SB1"),
        accounts(vec![
            row("SB1", "SPREADBET", "USD"),
            row("CFD1", "CFD", "USD"),
            row("CFD2", "CFD", "USD"),
        ]),
        switch_with_new_tokens(),
    ]);
    let err = IgSession::login(&ig.config()).err().expect("two fit, nothing says which");
    assert!(err.message.contains("`CFD1`") && err.message.contains("`CFD2`"), "{}", err.message);
    assert!(err.message.contains("set-book"), "it says what to write: {}", err.message);
    assert_eq!(ig.count("PUT /session"), 0);
}

/// A switch IG refuses fails the login: the session must never carry on, unannounced, on the account the rule
/// just ruled out.
#[test]
fn a_switch_ig_refuses_fails_the_login() {
    let ig = serve(vec![
        login_on("SB1"),
        accounts(vec![row("SB1", "SPREADBET", "USD"), row("CFD1", "CFD", "USD")]),
        Route::status(
            "PUT /session",
            403,
            json!({ "errorCode": "error.security.account-not-allowed" }),
        ),
    ]);
    let err = IgSession::login(&ig.config()).err().expect("the switch was refused");
    assert_eq!(err.status, 403);
    assert!(
        err.message.contains("`SB1`")
            && err.message.contains("`CFD1`")
            && err.message.contains("account-not-allowed"),
        "{}",
        err.message
    );
}

/// A list that cannot be read changes nothing (what every login did before the rule existed): the session stays
/// on the default and the login succeeds.
#[test]
fn an_unreadable_account_list_leaves_the_session_where_ig_put_it() {
    let ig = serve(vec![
        login_on("SB1"),
        Route::status("GET /accounts", 500, json!({ "errorCode": "error.public-api.failure" })),
        switch_with_new_tokens(),
    ]);
    let session = IgSession::login(&ig.config()).expect("the login still succeeds");
    assert_eq!(session.account_id, "SB1");
    assert_eq!(ig.count("PUT /session"), 0);
}

/// **A re-login settles the same way** (a 401 mid-session lands on the default again): the session moves to the
/// same account again and keeps its generation counting, and the account never changes under a live session.
#[test]
fn a_relogin_lands_on_the_same_account_again() {
    let ig = serve(vec![
        login_on("SB1"),
        accounts(vec![row("SB1", "SPREADBET", "USD"), row("CFD1", "CFD", "USD")]),
        switch_with_new_tokens(),
    ]);
    let session = IgSession::login(&ig.config()).expect("the login settles");
    assert_eq!(session.relogin(session.token_generation()), Ok(2));
    assert_eq!(session.account_id, "CFD1");
    assert_eq!((ig.count("POST /session"), ig.count("PUT /session")), (2, 2));
}

/// **The account row's book wins** (`IgConfig::account_pin`, the row's `venue_account_id`): the session goes to
/// the named account even when the login's default is a fitting one, and even when it is a spread-bet account,
/// because the row says so; the book already in force costs no `PUT`.
#[test]
fn the_rows_book_decides_even_over_a_fitting_default_and_a_spreadbet_book_is_honoured() {
    let rows = vec![row("CFD1", "CFD", "USD"), row("SB1", "SPREADBET", "GBP")];
    let ig = serve(vec![login_on("CFD1"), accounts(rows.clone()), switch_keeping_tokens()]);
    let mut book = ig.config();
    book.account_pin = Some("SB1".to_string());
    let session = IgSession::login(&book).expect("the book is listed and enabled");
    assert_eq!(session.account_id, "SB1");
    assert_eq!(session.account_currency.as_deref(), Some("GBP"), "the book's own currency");
    assert_eq!(the_switch_body(&ig), vec![json!({ "accountId": "SB1", "defaultAccount": false })]);

    let ig = serve(vec![login_on("CFD1"), accounts(rows), switch_keeping_tokens()]);
    let mut book = ig.config();
    book.account_pin = Some("CFD1".to_string());
    assert_eq!(IgSession::login(&book).expect("already there").account_id, "CFD1");
    assert_eq!(ig.count("PUT /session"), 0, "idempotent");
}

/// A book the login does not list is refused by name, and a book that cannot be CHECKED (the list unreadable) is
/// an error too: the operator's word is never silently replaced by the default.
#[test]
fn a_book_that_is_not_listed_or_cannot_be_checked_fails_the_login() {
    let ig = serve(vec![
        login_on("CFD1"),
        accounts(vec![row("CFD1", "CFD", "USD")]),
        switch_keeping_tokens(),
    ]);
    let mut book = ig.config();
    book.account_pin = Some("NOPE".to_string());
    let err = IgSession::login(&book).err().expect("not listed");
    assert!(err.message.contains("`NOPE`") && err.message.contains("`CFD1`"), "{}", err.message);

    let ig = serve(vec![
        login_on("CFD1"),
        Route::status("GET /accounts", 500, json!({ "errorCode": "error.public-api.failure" })),
    ]);
    let mut book = ig.config();
    book.account_pin = Some("CFD1".to_string());
    let err = IgSession::login(&book).err().expect("the book cannot be checked");
    assert_eq!(err.status, 500, "{err:?}");
}

/// The facts source reads the account the session is MOVED to (its currency decides what the node is told), from
/// the list the login already read: one `GET /accounts` for the whole thing.
#[test]
fn the_facts_source_reads_the_currency_of_the_account_the_session_was_moved_to() {
    let ig = serve(vec![
        login_on("G1"),
        accounts(vec![row("G1", "CFD", "GBP"), row("U1", "CFD", "USD")]),
        switch_keeping_tokens(),
    ]);
    let source = IgFactsSource::new(ig.config());
    assert_eq!(source.account_currency().expect("read"), "USD", "not the pound default's");
    assert_eq!(ig.count("GET /accounts"), 1, "the login's list is the source's row");
    assert_eq!(ig.count("PUT /session"), 1);
}

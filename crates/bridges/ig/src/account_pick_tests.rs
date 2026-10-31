//! The account rule over hand-written `GET /accounts` bodies in the documented shape
//! (<https://labs.ig.com/reference/accounts.html>): none is a capture, so what these prove is the rule, not IG.

use serde_json::{Value, json};

use super::*;

fn row(id: &str, kind: &str, currency: &str, status: &str) -> Value {
    json!({ "accountId": id, "accountType": kind, "currency": currency, "status": status,
            "preferred": false, "balance": { "balance": 1000.0 } })
}

fn body(rows: Vec<Value>) -> Value {
    json!({ "accounts": rows })
}

fn pick_of(rows: Vec<Value>, current: &str, pin: Option<&str>) -> Result<Pick, String> {
    pick(&listed_accounts(&body(rows)), current, pin)
}

#[test]
fn a_body_reads_into_rows_and_an_unreadable_field_is_not_stated() {
    let rows = listed_accounts(&body(vec![
        json!({ "accountId": " abc1 ", "accountType": "cfd", "currency": "usd", "status": "enabled",
                "preferred": true }),
        json!({ "accountId": "B", "accountType": "", "currency": "dollars" }),
        json!({ "accountName": "no id" }),
        json!({ "accountId": "  " }),
    ]));
    assert_eq!(rows.len(), 2, "a row with no id is no account");
    assert_eq!(rows[0].id, "abc1");
    assert_eq!(rows[0].kind.as_deref(), Some("CFD"));
    assert_eq!(rows[0].currency.as_deref(), Some("USD"));
    assert_eq!(rows[0].status.as_deref(), Some("ENABLED"));
    assert!(rows[0].preferred);
    assert_eq!((rows[1].kind.as_deref(), rows[1].currency.as_deref()), (None, None));
    assert!(listed_accounts(&json!({})).is_empty());
}

#[test]
fn a_default_that_is_a_usd_cfd_account_stays() {
    let rows = vec![row("A", "CFD", "USD", "ENABLED"), row("B", "SPREADBET", "GBP", "ENABLED")];
    assert_eq!(pick_of(rows, "A", None), Ok(Pick::Stay));
}

#[test]
fn a_spreadbet_default_moves_to_the_one_usd_cfd_account() {
    let rows = vec![row("S", "SPREADBET", "USD", "ENABLED"), row("C", "CFD", "USD", "ENABLED")];
    assert_eq!(pick_of(rows, "S", None), Ok(Pick::Switch("C".to_string())));
}

#[test]
fn a_pound_default_moves_to_the_one_usd_cfd_account() {
    let rows = vec![row("G", "CFD", "GBP", "ENABLED"), row("U", "CFD", "USD", "ENABLED")];
    assert_eq!(pick_of(rows, "G", None), Ok(Pick::Switch("U".to_string())));
}

/// Two accounts fit and the default is one of them: it is not moved (the behaviour every login had), and the
/// other is not preferred over it.
#[test]
fn a_fitting_default_stays_even_when_another_account_fits() {
    let rows = vec![row("A", "CFD", "USD", "ENABLED"), row("B", "CFD", "USD", "ENABLED")];
    assert_eq!(pick_of(rows.clone(), "A", None), Ok(Pick::Stay));
    assert_eq!(pick_of(rows, "B", None), Ok(Pick::Stay));
}

#[test]
fn a_session_is_never_moved_to_a_spreadbet_or_physical_or_foreign_or_disabled_account() {
    let rows = vec![
        row("G", "CFD", "GBP", "ENABLED"),
        row("S", "SPREADBET", "USD", "ENABLED"),
        row("P", "PHYSICAL", "USD", "ENABLED"),
        row("D", "CFD", "USD", "DISABLED"),
        row("X", "CFD", "USD", "SUSPENDED_FROM_DEALING"),
    ];
    let why = pick_of(rows, "G", None).expect_err("nothing fits");
    for id in ["`G`", "`S`", "`P`", "`D`", "`X`"] {
        assert!(why.contains(id), "every account found is named: {why}");
    }
    assert!(why.contains("SPREADBET USD ENABLED") && why.contains("CFD GBP ENABLED"), "{why}");
    assert!(why.contains("none of its accounts"), "{why}");
    assert!(why.contains("vike-cli secrets set-book"), "it says what to write: {why}");
}

#[test]
fn several_candidates_and_a_default_that_does_not_fit_is_a_refusal_naming_them() {
    let rows = vec![
        row("S", "SPREADBET", "USD", "ENABLED"),
        row("C1", "CFD", "USD", "ENABLED"),
        row("C2", "CFD", "USD", "ENABLED"),
    ];
    let why = pick_of(rows, "S", None).expect_err("two fit and nothing says which");
    assert!(why.contains("SEVERAL") && why.contains("`C1`") && why.contains("`C2`"), "{why}");
    assert!(why.contains("default account is `S` SPREADBET"), "{why}");
}

/// A STATED fact rules a default out; an unstated one does not (the 2026-10-19 ruling: unstated currency is
/// USD), but an account is moved TO only on stated facts.
#[test]
fn an_unstated_field_is_not_a_verdict_on_the_default_but_is_not_a_target() {
    let bare = json!({ "accountId": "BARE" });
    assert_eq!(pick_of(vec![bare.clone()], "BARE", None), Ok(Pick::Stay));
    let rows = vec![row("S", "SPREADBET", "USD", "ENABLED"), bare];
    assert!(pick_of(rows, "S", None).is_err(), "an account that states nothing is not moved to");
}

#[test]
fn no_information_stays() {
    assert_eq!(pick(&[], "A", None), Ok(Pick::Stay), "an empty list judges nothing");
    let rows = vec![row("S", "SPREADBET", "USD", "ENABLED")];
    assert_eq!(
        pick_of(rows, "NOT-LISTED", None),
        Ok(Pick::Stay),
        "the session's account is not listed"
    );
}

#[test]
fn the_rows_book_wins_over_the_rule() {
    let rows = vec![row("A", "CFD", "USD", "ENABLED"), row("B", "CFD", "USD", "ENABLED")];
    assert_eq!(pick_of(rows.clone(), "A", Some("B")), Ok(Pick::Switch("B".to_string())));
    assert_eq!(pick_of(rows.clone(), "B", Some("B")), Ok(Pick::Stay), "idempotent: already there");
    assert_eq!(
        pick_of(rows, "A", Some(" b ")),
        Ok(Pick::Switch("B".to_string())),
        "trimmed, any case"
    );
}

/// The row says so: a spread-bet book is honoured, the facts fences judge each epic on it afterwards.
#[test]
fn a_book_that_is_a_spreadbet_account_is_honoured_because_the_row_says_so() {
    let rows = vec![row("C", "CFD", "USD", "ENABLED"), row("S", "SPREADBET", "GBP", "ENABLED")];
    assert_eq!(pick_of(rows, "C", Some("S")), Ok(Pick::Switch("S".to_string())));
}

#[test]
fn a_book_the_login_does_not_list_or_that_cannot_deal_is_a_refusal_by_name() {
    let rows = vec![row("A", "CFD", "USD", "ENABLED"), row("D", "CFD", "USD", "DISABLED")];
    let missing = pick_of(rows.clone(), "A", Some("ZZZ")).expect_err("not listed");
    assert!(
        missing.contains("`ZZZ`") && missing.contains("`A`") && missing.contains("`D`"),
        "{missing}"
    );
    let disabled = pick_of(rows, "A", Some("D")).expect_err("disabled");
    assert!(
        disabled.contains("does not let it deal") && disabled.contains("DISABLED"),
        "{disabled}"
    );
    // an empty list names itself
    let empty = pick(&[], "A", Some("ZZZ")).expect_err("empty list");
    assert!(empty.contains("listed no account"), "{empty}");
}

/// A blank book is no book: the rule applies.
#[test]
fn a_blank_book_is_no_book() {
    let rows = vec![row("S", "SPREADBET", "USD", "ENABLED"), row("C", "CFD", "USD", "ENABLED")];
    assert_eq!(pick_of(rows, "S", Some("  ")), Ok(Pick::Switch("C".to_string())));
}

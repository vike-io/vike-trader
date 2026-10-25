//! [`api_credentials`] reads ONE account's fields and nothing else: two accounts holding the same
//! fields never answer for each other, and no value reaches `Debug`.

use super::*;
use crate::account_credentials::{CredentialBook, CredentialFields};

/// **Each account signs with its OWN keys.** Two accounts of one venue holding the same fields
/// resolve to their own values, and an account with no row resolves to nothing — never to another
/// account's keys.
#[test]
fn each_account_reads_its_own_fields_and_never_another_accounts() {
    let mut book = CredentialBook::new();
    book.insert_account(7, API_KEY, "key-seven");
    book.insert_account(7, API_SECRET, "secret-seven");
    book.insert_account(8, API_KEY, "key-eight");
    book.insert_account(8, API_SECRET, "secret-eight");
    let seven = api_credentials(&book.view("bybit", Some(7)), "bybit").expect("7 arms");
    assert_eq!(seven.api_key.expose(), "key-seven");
    assert_eq!(seven.api_secret.expose(), "secret-seven");
    let eight = api_credentials(&book.view("bybit", Some(8)), "bybit").expect("8 arms");
    assert_eq!(eight.api_key.expose(), "key-eight");
    assert!(api_credentials(&book.view("bybit", Some(9)), "bybit").is_none(), "9 holds nothing");
    assert!(api_credentials(&book.view("bybit", None), "bybit").is_none(), "no row, no keys");
}

/// Values come back TRIMMED, and a blank secret is an absent one.
#[test]
fn values_are_trimmed_and_a_blank_secret_is_absent() {
    let padded = CredentialFields::from_pairs([(API_KEY, " k "), (API_SECRET, "\ts\n")]);
    let c = api_credentials(&AccountCredentials::new(Some(1), Some(&padded), None), "binance")
        .expect("padded values arm");
    assert_eq!(c.api_key.expose(), "k");
    assert_eq!(c.api_secret.expose(), "s");
    let blank = CredentialFields::from_pairs([(API_KEY, "k"), (API_SECRET, "   ")]);
    assert!(
        api_credentials(&AccountCredentials::new(Some(1), Some(&blank), None), "binance").is_none()
    );
}

/// **The credentials never reach `Debug`** — the key's four-character tail and the passphrase's
/// presence are all it prints.
#[test]
fn the_credentials_never_reach_debug() {
    let fields = CredentialFields::from_pairs([
        (API_KEY, "AKIA-very-secret-1234"),
        (API_SECRET, "the-secret-value"),
        (API_PASSPHRASE, "the-passphrase"),
    ]);
    let c = api_credentials(&AccountCredentials::new(Some(1), Some(&fields), None), "okx")
        .expect("arms");
    let printed = format!("{c:?}");
    assert_eq!(printed, "Credentials(api_key=***1234, passphrase=set)");
    assert!(!printed.contains("the-secret-value") && !printed.contains("the-passphrase"));
}

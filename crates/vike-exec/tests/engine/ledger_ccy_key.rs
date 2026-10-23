//! `ExecutionEngine::ledger_ccy_key`: the currency GROUP a cross-engine total sums an engine in
//! (decision 0183, item 13). EMPTY for the dollar group, else the upper-cased code.
//!
//! The dollar group is what every node of today is in, so a wrong answer here splits a one-currency
//! node into two groups and the published totals drop an engine: the cases that must stay EMPTY are
//! pinned as hard as the ones that must not.

use crate::support::EngineBuilder;

fn key_after_telling(code: &str) -> String {
    let mut e = EngineBuilder::default().build();
    e.set_account_ccy(code).expect("a currency code");
    e.ledger_ccy_key().to_string()
}

#[test]
fn an_engine_nobody_told_is_in_the_dollar_group() {
    let e = EngineBuilder::default().build();
    assert!(!e.account_ccy_known(), "PREMISE: the constructor's guess is not a statement");
    assert_eq!(e.ledger_ccy_key(), "");
}

#[test]
fn every_dollar_spelling_is_one_group_with_the_unsaid() {
    for code in ["USD", "usd", " Usd ", "USDT", "usdt"] {
        assert_eq!(key_after_telling(code), "", "{code:?} is a dollar ledger");
    }
}

#[test]
fn another_currency_is_its_own_upper_cased_group() {
    assert_eq!(key_after_telling("EUR"), "EUR");
    assert_eq!(key_after_telling(" eur "), "EUR", "trimmed and upper-cased");
    assert_eq!(key_after_telling("JPY"), "JPY");
    // Two engines told one currency in different spellings are one group: the key is the value.
    assert_eq!(key_after_telling("gbp"), key_after_telling("GBP"));
}

/// A RESTORED engine takes the group of the currency its snapshot states (`from_snapshot` derives the
/// key where it writes `quote_asset`), however the code is spelled; one told "USDT" or never told reads
/// the dollar group.
#[test]
fn a_restored_engine_takes_the_group_of_its_snapshots_currency() {
    use vike_exec::ExecutionEngine;
    use vike_exec::testing::RecordingClient;

    let restored = |quote_asset: &str, told: bool| {
        let mut snap = EngineBuilder::default().build().snapshot_state();
        snap.quote_asset = quote_asset.to_string();
        snap.account_ccy_set = told;
        ExecutionEngine::from_snapshot(&snap, RecordingClient::default())
    };
    assert_eq!(
        restored("eur", false).ledger_ccy_key(),
        "EUR",
        "a code that is not the guess is told"
    );
    assert_eq!(restored("USDT", true).ledger_ccy_key(), "", "told USDT: the dollar group");
    assert_eq!(restored("USDT", false).ledger_ccy_key(), "", "never told: the dollar group");
    assert!(restored("eur", false).account_ccy_known());
}

/// The key is a copy of what the engine cached where its currency is SAID, so a publish interns nothing:
/// it follows `set_account_ccy`, a refused word changes nothing, and a direct write to `quote_asset` is
/// not a statement (it does not move the group).
#[test]
fn the_key_follows_set_account_ccy_and_a_refused_change_keeps_it() {
    let mut e = EngineBuilder::default().build();
    e.set_account_ccy("EUR").expect("told");
    assert_eq!(e.ledger_ccy_key(), "EUR");
    // The key is the statement the engine cached when it was told, NOT a re-reading of `quote_asset`: a
    // direct write to the pub field is not a statement and does not move the group (the documented
    // footgun of `ledger_ccy_key`). Written back so the rest of the test reads the told currency.
    e.quote_asset = "JPY".to_string();
    assert_eq!(e.ledger_ccy_key(), "EUR", "a direct write does not refresh the cached key");
    e.quote_asset = "EUR".to_string();
    e.set_account_ccy("gbp").expect("no foreign symbol is held, so the label may change");
    assert_eq!(e.ledger_ccy_key(), "GBP");
    assert_eq!(e.set_account_ccy("  "), Err("account-ccy-empty"));
    assert_eq!(e.ledger_ccy_key(), "GBP", "a refused word changes nothing");
}

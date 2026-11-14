//! **`account_fact`, the facts an account states about how its engine is mounted** (decision 0156's amendment of
//! 2026-11-13: the per-venue constants beside the symbol are rows of the account they belong to).
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_fact_round_trips_clears_and_repeats_as_no_change`] | `AccountEdit::SetFact` storing nothing, moving another row's fact, or reporting a re-run as a change |
//! | [`the_fact_words_are_the_stored_names`] | a renamed word leaving every stored row unread |
//! | [`a_lane_twin_symbol_is_refused_off_the_twins_unlabelled_row_or_its_lane`] | a twin stated on a row nothing derives it from, or on its parent's own lane (a second signing engine on one market) |
//! | [`a_malformed_value_and_an_unknown_row_are_refused_and_not_echoed`] | a pasted line stored as a fact, or a fact filed against no account |
//! | [`removing_an_account_removes_its_facts`] | `AccountEdit::Remove` refused by the foreign key, or a fact left naming a removed id |
//! | [`a_store_without_the_table_states_no_fact_and_gains_it_on_the_next_write`] | ⚠ the daemon's READ-ONLY boot read failing `no such table` on a store no writer of this build has carried |
//! | [`the_stated_symbol_is_the_active_row_of_the_tier_with_the_lowest_id`] | the engine mounted on a row the mount does not pick |
//!
//! ⚠ Every value here is FICTIONAL.

use std::assert_matches;
use vike_model::AccountTier;
use vike_secrets::{AccountEdit, AccountFact, AccountRows, StoreErrorKind};

use crate::support;
use support::Fixture;

#[test]
fn a_fact_round_trips_clears_and_repeats_as_no_change() {
    let fx = Fixture::seeded();
    let id = fx.create_account("fxcm", AccountTier::Demo, None);
    let other = fx.create_account("ig", AccountTier::Demo, None);
    let fact = AccountFact::CurrencySymbol;
    assert_eq!(fx.row(id).expect("the row").fact(fact), None, "a new row states none");

    let w = fx.edit(AccountEdit::SetFact { id, fact, value: Some(" EURGBP ") }).expect("set one");
    assert_eq!(w.verb, "set-fact");
    assert!(w.changed);
    assert_eq!(w.after.expect("the row after").fact(fact), Some("EURGBP"), "trimmed");
    assert_eq!(fx.row(id).expect("the row").fact(fact), Some("EURGBP"));
    let again = fx.edit(AccountEdit::SetFact { id, fact, value: Some("EURGBP") }).expect("again");
    assert!(!again.changed, "a repeat of the stated value changes nothing and says so");

    let expiry = AccountFact::FallbackSymbolExpiry;
    fx.edit(AccountEdit::SetFact { id: other, fact: expiry, value: Some("-") }).expect("a dash");
    assert_eq!(fx.row(other).expect("the other row").fact(expiry), Some("-"));
    assert_eq!(fx.row(id).expect("the row").facts.len(), 1, "another row's fact is not this one's");

    let cleared = fx.edit(AccountEdit::SetFact { id, fact, value: None }).expect("clear it");
    assert!(cleared.changed);
    assert_eq!(fx.row(id).expect("the row").fact(fact), None, "`None` clears it");
    let again = fx.edit(AccountEdit::SetFact { id, fact, value: None }).expect("clear again");
    assert!(!again.changed);
    assert_eq!(fx.row(other).expect("the other row").fact(expiry), Some("-"), "untouched");
}

#[test]
fn the_fact_words_are_the_stored_names() {
    assert_eq!(
        AccountFact::ALL.map(AccountFact::as_str),
        [
            "lane_twin_symbol",
            "currency_symbol",
            "fallback_symbol",
            "fallback_symbol_expiry",
            "fallback_symbol_currency"
        ]
    );
    for fact in AccountFact::ALL {
        assert_eq!(AccountFact::parse(fact.as_str()), Some(fact));
    }
    assert_eq!(AccountFact::parse("lane_twin"), None);
    assert_eq!(AccountFact::parse("LANE_TWIN_SYMBOL"), None, "a stored word is matched exactly");
}

#[test]
fn a_lane_twin_symbol_is_refused_off_the_twins_unlabelled_row_or_its_lane() {
    let fx = Fixture::initialised();
    let parent = fx.create_account("binance", AccountTier::Demo, None);
    let labelled = fx.create_account("binance", AccountTier::Demo, Some("ALT"));
    let elsewhere = fx.create_account("bybit", AccountTier::Demo, None);
    let fact = AccountFact::LaneTwinSymbol;
    let stated = fx
        .edit(AccountEdit::SetFact { id: parent, fact, value: Some("BTCUSDT.P") })
        .expect("the unlabelled binance row, on the perpetual lane");
    assert!(stated.changed);
    for (id, value, word) in [
        (parent, "BTCUSDT", "not on the twin's lane"),
        (labelled, "BTCUSDT.P", "this row is labelled"),
        (elsewhere, "BTCUSDT.P", "has no lane twin"),
    ] {
        let err = fx
            .edit(AccountEdit::SetFact { id, fact, value: Some(value) })
            .expect_err("a twin's symbol where no twin reads it");
        assert_matches!(
            &err.kind,
            StoreErrorKind::AccountFactRefused { id: got, fact: "lane_twin_symbol", .. } if *got == id,
            "{err}"
        );
        assert!(err.to_string().contains(word), "`{word}` in: {err}");
    }
    assert_eq!(fx.row(parent).expect("the row").fact(fact), Some("BTCUSDT.P"), "nothing moved");
    let cleared = fx
        .edit(AccountEdit::SetFact { id: labelled, fact, value: None })
        .expect("clearing is never refused");
    assert!(!cleared.changed);
}

#[test]
fn a_malformed_value_and_an_unknown_row_are_refused_and_not_echoed() {
    let fx = Fixture::seeded();
    let id = fx.create_account("ig", AccountTier::Demo, None);
    let fact = AccountFact::FallbackSymbol;
    let err = fx
        .edit(AccountEdit::SetFact { id, fact, value: Some("CS.D SECRETISH") })
        .expect_err("two tokens are no value");
    assert_matches!(err.kind, StoreErrorKind::AccountFactMalformed, "{err}");
    assert!(!err.to_string().contains("SECRETISH"), "the token is not echoed: {err}");
    assert_eq!(fx.row(id).expect("the row").fact(fact), None, "nothing was written");
    let err = fx
        .edit(AccountEdit::SetFact { id: 9_999, fact, value: Some("CS.D.EURUSD.MINI.IP") })
        .expect_err("no row carries that id");
    assert_matches!(err.kind, StoreErrorKind::NoSuchAccount { id: 9_999 }, "{err}");
}

#[test]
fn removing_an_account_removes_its_facts() {
    let fx = Fixture::seeded();
    let id = fx.create_account("ig", AccountTier::Demo, None);
    let fact = AccountFact::FallbackSymbol;
    fx.edit(AccountEdit::SetFact { id, fact, value: Some("CS.D.EURUSD.MINI.IP") }).expect("stated");
    let removed = fx
        .edit(AccountEdit::Remove { id })
        .expect("a row with no credentials goes, its facts with it (the foreign key would refuse)");
    assert_eq!(
        removed.before.expect("the row before").fact(fact),
        Some("CS.D.EURUSD.MINI.IP"),
        "the record carries what went"
    );
    assert!(fx.row(id).is_none());
    let next = fx.create_account("ig", AccountTier::Demo, None);
    assert!(fx.row(next).expect("the new row").facts.is_empty(), "a new row inherits nothing");
}

/// The shipped DDL as a store born before the table holds it.
fn old_ddl() -> String {
    let ddl = vike_secrets::DDL.to_string();
    let start = ddl.find("CREATE TABLE IF NOT EXISTS account_fact (").expect("the shipped table");
    let end = start + ddl[start..].find(") STRICT;\n").expect("its end") + ") STRICT;\n".len();
    let old = format!("{}{}", &ddl[..start], &ddl[end..]);
    assert!(!old.contains("account_fact"), "the planted DDL carries no account_fact");
    old
}

/// ⚠ The daemon reads its accounts READ-ONLY at boot, before any writer runs: a store without the table states no
/// fact, never `no such table`, and the next write of the fact creates the table and takes it.
#[test]
fn a_store_without_the_table_states_no_fact_and_gains_it_on_the_next_write() {
    let fx = Fixture::empty();
    let db = fx.db();
    let conn = support::sql::plant_ddl(&db, &old_ddl());
    conn.execute_batch(
        "INSERT INTO venue (name) VALUES ('fxcm');
         INSERT INTO account (venue_id, tier) VALUES (1, 'demo');",
    )
    .expect("the old row");
    support::sql::stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    assert!(!support::sql::has_column(&conn, "account_fact", "value"), "the planted store is OLD");
    drop(conn);

    let AccountRows::Known(rows) = vike_secrets::read_accounts(&db).expect("the read-only read")
    else {
        panic!("an old store with an `account` table answers")
    };
    assert!(rows.iter().all(|a| a.facts.is_empty()), "every row states none: {rows:?}");

    let (id, fact) = (rows[0].id, AccountFact::CurrencySymbol);
    vike_secrets::edit_account(&db, AccountEdit::SetFact { id, fact, value: Some("EURGBP") })
        .expect("the write creates the table and takes the fact");
    let AccountRows::Known(rows) = vike_secrets::read_accounts(&db).expect("read again") else {
        panic!("the store answers")
    };
    assert_eq!(rows.iter().find(|a| a.id == id).expect("the row").fact(fact), Some("EURGBP"));
}

/// The symbol an account's engine is mounted on is the ACTIVE row's at the asked tier, the lowest id among several
/// (dukascopy's two books, the row the mount picks); another tier, an inactive row, another label or venue never
/// answers.
#[test]
fn the_stated_symbol_is_the_active_row_of_the_tier_with_the_lowest_id() {
    let row = |id, tier, label, active, symbol: Option<&str>| vike_secrets::AccountRow {
        symbol: symbol.map(str::to_string),
        ..vike_secrets::account_row(id, "dukascopy", tier, label, active)
    };
    let rows = AccountRows::Known(vec![
        row(3, AccountTier::Demo, None, false, Some("USDJPY")),
        row(5, AccountTier::Demo, None, true, Some("EURUSD")),
        row(7, AccountTier::Demo, None, true, Some("GBPUSD")),
        row(8, AccountTier::Paper, None, true, Some("AUDUSD")),
        row(9, AccountTier::Demo, Some("SWISS"), true, None),
    ]);
    assert_eq!(rows.stated_symbol("dukascopy", None, AccountTier::Demo), Some("EURUSD"));
    assert_eq!(rows.stated_symbol("dukascopy", None, AccountTier::Paper), Some("AUDUSD"));
    assert_eq!(rows.stated_symbol("dukascopy", None, AccountTier::Live), None);
    assert_eq!(rows.stated_symbol("dukascopy", Some("SWISS"), AccountTier::Demo), None);
    assert_eq!(rows.stated_symbol("fxcm", None, AccountTier::Demo), None);
}

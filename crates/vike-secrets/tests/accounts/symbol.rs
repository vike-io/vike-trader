//! **`account.symbol`, the instrument an account's engine is mounted on** (every row, the unlabelled one included:
//! decision 0156's amendment of 2026-11-13; a strategy mount that names a labelled account still decides).
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_symbol_round_trips_clears_and_repeats_as_no_change`] | `AccountEdit::SetSymbol` storing nothing, moving another row, or reporting a re-run as a change |
//! | [`a_symbol_on_the_default_account_is_stored_and_an_unknown_row_is_refused`] | the default account's engine left with no symbol to be mounted on (it would not run), or a symbol filed against no row |
//! | [`a_malformed_symbol_is_refused_before_the_store_and_not_echoed`] | a pasted line or an empty token stored as an instrument |
//! | [`an_old_store_reads_every_row_at_none_and_gains_the_column_on_the_next_write`] | ⚠ the daemon's READ-ONLY boot read failing `no such column` on a store no writer has carried |
//!
//! ⚠ Every value here is FICTIONAL.

use std::assert_matches;
use vike_model::AccountTier;
use vike_secrets::{AccountEdit, AccountRows, StoreErrorKind};

use crate::support;
use support::Fixture;

#[test]
fn a_symbol_round_trips_clears_and_repeats_as_no_change() {
    let fx = Fixture::seeded();
    let id = fx.create_account("bybit", AccountTier::Demo, Some("INV"));
    assert_eq!(fx.row(id).expect("the row").symbol, None, "a new row states none");

    let w = fx.edit(AccountEdit::SetSymbol { id, symbol: Some(" BTCUSD ") }).expect("set one");
    assert_eq!(w.verb, "set-symbol");
    assert!(w.changed);
    assert_eq!(w.after.expect("the row after").symbol.as_deref(), Some("BTCUSD"), "trimmed");
    assert_eq!(fx.row(id).expect("the row").symbol.as_deref(), Some("BTCUSD"));

    let again = fx.edit(AccountEdit::SetSymbol { id, symbol: Some("BTCUSD") }).expect("again");
    assert!(!again.changed, "a repeat of the stored symbol changes nothing and says so");

    // Case is the venue's: nothing upper-cases it.
    fx.edit(AccountEdit::SetSymbol { id, symbol: Some("kPEPE") }).expect("case kept");
    assert_eq!(fx.row(id).expect("the row").symbol.as_deref(), Some("kPEPE"));

    let cleared = fx.edit(AccountEdit::SetSymbol { id, symbol: None }).expect("clear it");
    assert!(cleared.changed);
    assert_eq!(fx.row(id).expect("the row").symbol, None, "`None` clears: the account states none");

    fx.edit(AccountEdit::SetSymbol { id, symbol: Some("BTCUSD") }).expect("set one");
    let others: Vec<_> =
        fx.accounts().into_iter().filter(|a| a.id != id).map(|a| a.symbol).collect();
    assert!(others.iter().all(Option::is_none), "no other row gained a symbol: {others:?}");
}

#[test]
fn a_symbol_on_the_default_account_is_stored_and_an_unknown_row_is_refused() {
    let fx = Fixture::seeded();
    let id = fx.create_account("bybit", AccountTier::Demo, None);
    let stated = fx
        .edit(AccountEdit::SetSymbol { id, symbol: Some("BTCUSDT") })
        .expect("the default account states its engine's symbol like any other");
    assert!(stated.changed);
    assert_eq!(fx.row(id).expect("the row").symbol.as_deref(), Some("BTCUSDT"));
    let cleared = fx.edit(AccountEdit::SetSymbol { id, symbol: None }).expect("a clear is allowed");
    assert!(cleared.changed);
    assert_eq!(fx.row(id).expect("the row").symbol, None);

    let err = fx
        .edit(AccountEdit::SetSymbol { id: 9_999, symbol: Some("BTCUSD") })
        .expect_err("no row carries that id");
    assert_matches!(err.kind, StoreErrorKind::NoSuchAccount { id: 9_999 }, "{err}");
}

#[test]
fn a_malformed_symbol_is_refused_before_the_store_and_not_echoed() {
    let long = "X".repeat(vike_secrets::ACCOUNT_SYMBOL_MAX_BYTES + 1);
    for bad in ["", "   ", "BTC USD", "BTC\u{200B}USD", "BTCÜSD", long.as_str()] {
        assert_eq!(vike_secrets::normalized_account_symbol(bad), None, "{bad:?}");
    }
    for good in ["BTCUSD", "ETH-PERPETUAL", "CS.D.EURUSD.MINI.IP", "xyz:TSLA", "HYPE/USDC"] {
        assert_eq!(vike_secrets::normalized_account_symbol(good).as_deref(), Some(good));
    }
    let fx = Fixture::seeded();
    let id = fx.create_account("bybit", AccountTier::Demo, Some("INV"));
    let err = fx
        .edit(AccountEdit::SetSymbol { id, symbol: Some("BTC SECRETISH") })
        .expect_err("two tokens are no symbol");
    assert_matches!(err.kind, StoreErrorKind::AccountSymbolMalformed, "{err}");
    assert!(!err.to_string().contains("SECRETISH"), "the token is not echoed: {err}");
}

/// The shipped DDL as a store born before the column holds it.
fn old_ddl() -> String {
    let from = "    symbol           TEXT,\n";
    let ddl = vike_secrets::DDL.to_string();
    assert_eq!(ddl.matches(from).count(), 1, "the shipped DDL carries the column exactly once");
    ddl.replace(from, "")
}

/// ⚠ The daemon reads its accounts READ-ONLY at boot, before any writer runs: on a store without the
/// column every row reads `None` (it states none), never `no such column`, and the next write of ANY
/// kind adds it.
#[test]
fn an_old_store_reads_every_row_at_none_and_gains_the_column_on_the_next_write() {
    let fx = Fixture::empty();
    let db = fx.db();
    let conn = support::sql::plant_ddl(&db, &old_ddl());
    conn.execute_batch(
        "INSERT INTO venue (name) VALUES ('bybit');
         INSERT INTO account (venue_id, tier, label) VALUES (1, 'demo', 'INV');",
    )
    .expect("the old row");
    support::sql::stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    assert!(!support::sql::has_column(&conn, "account", "symbol"), "the planted store is OLD");
    drop(conn);

    let AccountRows::Known(rows) = vike_secrets::read_accounts(&db).expect("the read-only read")
    else {
        panic!("an old store with an `account` table answers")
    };
    assert!(rows.iter().all(|a| a.symbol.is_none()), "every row reads none: {rows:?}");

    let id = rows[0].id;
    vike_secrets::edit_account(&db, AccountEdit::SetSymbol { id, symbol: Some("BTCUSD") })
        .expect("the write adds the column and takes the symbol");
    let AccountRows::Known(rows) = vike_secrets::read_accounts(&db).expect("read again") else {
        panic!("the store answers")
    };
    assert_eq!(
        rows.iter().find(|a| a.id == id).expect("the row").symbol.as_deref(),
        Some("BTCUSD")
    );
}

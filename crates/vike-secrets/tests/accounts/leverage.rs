//! **`account.leverage`, one account's own margin leverage** (decision 0181).
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_leverage_round_trips_clears_and_repeats_as_no_change`] | `AccountEdit::SetLeverage` storing nothing, moving another row, or reporting a re-run as a change |
//! | [`a_leverage_on_an_unknown_id_is_refused`] | a typo inventing a leverage on nobody |
//! | [`the_type_and_the_column_refuse_the_same_figures`] | a sub-1, huge, `NaN` or infinite figure reaching the store as a leverage |
//! | [`an_old_store_reads_every_row_at_none_and_gains_the_column_on_the_next_write`] | ⚠ the daemon's READ-ONLY boot read failing `no such column` on a store no writer has carried, which mounts every account PAPER |
//! | [`leverage_of_is_the_lowest_active_figure_of_one_account`] | a larger, an inactive or another account's figure reaching an engine |
//!
//! ⚠ Every value here is FICTIONAL.

use std::assert_matches;
use vike_secrets::{Account, AccountEdit, AccountLeverage, Accounts, DbErrorKind};

use crate::support;
use support::Fixture;

fn lev(figure: f64) -> Option<AccountLeverage> {
    Some(AccountLeverage::new(figure).expect("a usable figure"))
}

#[test]
fn a_leverage_round_trips_clears_and_repeats_as_no_change() {
    let fx = Fixture::seeded();
    let id = fx.id_of("binance", "demo");
    assert_eq!(fx.row(id).expect("the row").leverage, None, "a seeded row states none");

    let w = fx.edit(AccountEdit::SetLeverage { id, leverage: lev(10.0) }).expect("set one");
    assert_eq!(w.verb, "set-leverage");
    assert!(w.changed);
    assert_eq!(w.before.expect("the row before").leverage, None);
    assert_eq!(w.after.expect("the row after").leverage, lev(10.0));
    assert_eq!(
        fx.row(id).expect("the row").leverage,
        lev(10.0),
        "the reader answers what was written"
    );

    let again = fx.edit(AccountEdit::SetLeverage { id, leverage: lev(10.0) }).expect("again");
    assert!(!again.changed, "a repeat of the stored figure changes nothing and says so");

    let cleared = fx.edit(AccountEdit::SetLeverage { id, leverage: None }).expect("clear it");
    assert!(cleared.changed);
    assert_eq!(fx.row(id).expect("the row").leverage, None, "`None` clears: 1x");

    // Only this row moved, and no exposure ceiling with it.
    fx.edit(AccountEdit::SetLeverage { id, leverage: lev(3.0) }).expect("set one");
    let others: Vec<_> =
        fx.accounts().into_iter().filter(|a| a.id != id).map(|a| a.leverage).collect();
    assert!(others.iter().all(Option::is_none), "no other row gained a leverage: {others:?}");
    assert_eq!(fx.row(id).expect("the row").max_exposure, None, "the exposure column is untouched");
}

#[test]
fn a_leverage_on_an_unknown_id_is_refused() {
    let fx = Fixture::seeded();
    let err = fx
        .edit(AccountEdit::SetLeverage { id: 9_999, leverage: lev(2.0) })
        .expect_err("no row carries that id");
    assert_matches!(err.kind, DbErrorKind::NoSuchAccount { id: 9_999 }, "{err}");
}

#[test]
fn the_type_and_the_column_refuse_the_same_figures() {
    for bad in [0.0, 0.99, -1.0, 1000.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(AccountLeverage::new(bad), None, "{bad} is not a leverage");
    }
    for good in [1.0, 2.5, 10.0, 1000.0] {
        assert_eq!(AccountLeverage::new(good).map(AccountLeverage::get), Some(good), "{good}");
    }
    let fx = Fixture::seeded();
    let id = fx.id_of("binance", "demo");
    for bad in ["0", "0.5", "-5", "1000.5"] {
        let err = fx
            .conn()
            .execute(&format!("UPDATE account SET leverage = {bad} WHERE id = ?1"), [id])
            .expect_err("the column CHECK refuses it");
        assert!(err.to_string().contains("CHECK constraint failed"), "{bad}: {err}");
    }
    fx.conn()
        .execute("UPDATE account SET leverage = 12.5 WHERE id = ?1", [id])
        .expect("an in-range figure is admitted");
    assert_eq!(fx.row(id).expect("the row").leverage, lev(12.5));
}

/// The shipped DDL as a store born before decision 0181 holds it: no `leverage` column.
fn old_ddl() -> String {
    let from = "    leverage         REAL CHECK (leverage IS NULL OR (leverage >= 1.0 AND leverage <= 1000.0)),\n";
    let ddl = vike_secrets::DDL.to_string();
    assert_eq!(ddl.matches(from).count(), 1, "the shipped DDL carries the column exactly once");
    ddl.replace(from, "")
}

/// ⚠ The daemon reads its accounts READ-ONLY at boot, before any writer runs, and a read-only open
/// cannot `ALTER`: on a store without the column every row must read `None` (1x), never
/// `no such column`. The next write of ANY kind adds the column.
#[test]
fn an_old_store_reads_every_row_at_none_and_gains_the_column_on_the_next_write() {
    let fx = Fixture::empty();
    let db = fx.db();
    let conn = support::sql::plant_ddl(&db, &old_ddl());
    conn.execute_batch(
        "INSERT INTO venue (name) VALUES ('binance');
         INSERT INTO account (venue_id, tier) VALUES (1, 'demo');",
    )
    .expect("the old row");
    support::sql::stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    assert!(!support::sql::has_column(&conn, "account", "leverage"), "the planted store is OLD");
    drop(conn);

    let Accounts::Known(rows) = vike_secrets::read_accounts(&db).expect("the read-only read")
    else {
        panic!("an old store with an `account` table answers")
    };
    assert!(rows.iter().all(|a| a.leverage.is_none()), "every row reads none: {rows:?}");
    assert!(
        !support::sql::has_column(&support::sql::open(&db), "account", "leverage"),
        "a READ never alters the store"
    );

    let id = rows[0].id;
    vike_secrets::edit_account(&db, AccountEdit::SetActive { id, active: true })
        .expect("a no-op write over the old store");
    assert!(
        support::sql::has_column(&support::sql::open(&db), "account", "leverage"),
        "the write added it"
    );
    vike_secrets::edit_account(&db, AccountEdit::SetLeverage { id, leverage: lev(4.0) })
        .expect("the altered column takes a leverage");
    let Accounts::Known(rows) = vike_secrets::read_accounts(&db).expect("read again") else {
        panic!("the store answers")
    };
    assert_eq!(rows.iter().find(|a| a.id == id).expect("the row").leverage, lev(4.0));
}

fn row(id: i64, venue: &str, label: Option<&str>, active: bool, leverage: Option<f64>) -> Account {
    Account {
        id,
        venue: venue.to_string(),
        tier: "live".parse().expect("a tier"),
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
        leverage: leverage.and_then(AccountLeverage::new),
    }
}

#[test]
fn leverage_of_is_the_lowest_active_figure_of_one_account() {
    let rows = Accounts::Known(vec![
        row(1, "hyperliquid", None, true, Some(10.0)),
        row(2, "hyperliquid", None, true, Some(4.0)),
        row(3, "hyperliquid", None, false, Some(2.0)),
        row(4, "hyperliquid", Some("ALT"), true, Some(20.0)),
        row(5, "bybit", None, true, Some(3.0)),
        row(6, "okx", None, true, None),
    ]);
    let of = |venue, label| rows.leverage_of(venue, label).map(AccountLeverage::get);
    assert_eq!(
        of("hyperliquid", None),
        Some(4.0),
        "the lowest ACTIVE figure, an inactive 2 ignored"
    );
    assert_eq!(of("hyperliquid", Some("ALT")), Some(20.0), "another label's figure is its own");
    assert_eq!(of("bybit", None), Some(3.0));
    assert_eq!(of("okx", None), None, "a row stating none answers none");
    assert_eq!(of("binance", None), None, "no row answers none");
}

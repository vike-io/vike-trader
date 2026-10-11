//! **`account.max_exposure` and the account table's own answer to *which mode*** (decision 0119).
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`a_ceiling_round_trips_clears_and_repeats_as_no_change`] | the writer `AccountEdit::SetMaxExposure` storing nothing, or reporting a re-run as a change |
//! | [`a_ceiling_on_an_unknown_id_is_refused`] | a typo inventing a ceiling on nobody |
//! | [`max_exposure_refuses_every_figure_the_column_refuses`] | a zero, negative, `NaN` or infinite figure reaching the store as a "ceiling" |
//! | [`the_column_check_refuses_a_hand_written_zero`] | the store accepting a ceiling no reader could honour |
//! | [`an_old_store_reads_every_row_unbounded_and_gains_the_column_on_the_next_write`] | ⚠ the daemon's READ-ONLY boot read failing `no such column` on an un-`ALTER`ed store, which mounts every account PAPER |
//! | [`fresh_and_altered_stores_store_the_same_column_text`] | the `ALTER` and the DDL declaring two different columns |
//! | [`active_tier_answers_every_shape_of_rows`] | a mode read wrongly from the rows: a conflict armed, an inactive row armed, paper conflicting |
//! | [`max_exposure_of_is_the_tightest_active_figure`] | a looser or an inactive row's figure winning |
//!
//! ⚠ Every value here is FICTIONAL.

use std::assert_matches;
use vike_secrets::{Account, AccountEdit, Accounts, ActiveTier, DbErrorKind, MaxExposure};

use crate::support;
use support::Fixture;

fn ceiling(figure: f64) -> Option<MaxExposure> {
    Some(MaxExposure::new(figure).expect("a usable figure"))
}

// ---------------------------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------------------------

#[test]
fn a_ceiling_round_trips_clears_and_repeats_as_no_change() {
    let fx = Fixture::seeded();
    let id = fx.id_of("binance", "demo");
    assert_eq!(fx.row(id).expect("the row").max_exposure, None, "a seeded row states none");

    let w = fx
        .edit(AccountEdit::SetMaxExposure { id, max_exposure: ceiling(5000.0) })
        .expect("set a ceiling");
    assert_eq!(w.verb, "set-exposure");
    assert!(w.changed);
    assert_eq!(w.before.expect("the row before").max_exposure, None);
    assert_eq!(w.after.expect("the row after").max_exposure, ceiling(5000.0));
    let read = fx.row(id).expect("the row").max_exposure.expect("the ceiling was stored");
    assert_eq!(read.get(), 5000.0, "the reader answers the figure that was written");

    let again = fx
        .edit(AccountEdit::SetMaxExposure { id, max_exposure: ceiling(5000.0) })
        .expect("the same ceiling again");
    assert!(!again.changed, "a repeat of the stored figure changes nothing and says so");

    let cleared =
        fx.edit(AccountEdit::SetMaxExposure { id, max_exposure: None }).expect("clear it");
    assert!(cleared.changed);
    assert_eq!(fx.row(id).expect("the row").max_exposure, None, "`None` clears: UNBOUNDED");

    // Only this row moved.
    let others: Vec<_> =
        fx.accounts().into_iter().filter(|a| a.id != id).map(|a| a.max_exposure).collect();
    assert!(others.iter().all(Option::is_none), "no other row gained a ceiling: {others:?}");
}

#[test]
fn a_ceiling_on_an_unknown_id_is_refused() {
    let fx = Fixture::seeded();
    let err = fx
        .edit(AccountEdit::SetMaxExposure { id: 9_999, max_exposure: ceiling(10.0) })
        .expect_err("no row carries that id");
    assert_matches!(err.kind, DbErrorKind::NoSuchAccount { id: 9_999 }, "{err}");
}

#[test]
fn max_exposure_refuses_every_figure_the_column_refuses() {
    for bad in [0.0, -0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(MaxExposure::new(bad), None, "{bad} is not a ceiling");
    }
    for good in [f64::MIN_POSITIVE, 0.01, 1.0, 5000.0] {
        assert_eq!(MaxExposure::new(good).map(MaxExposure::get), Some(good), "{good} is one");
    }
}

#[test]
fn the_column_check_refuses_a_hand_written_zero() {
    let fx = Fixture::seeded();
    let id = fx.id_of("binance", "demo");
    for bad in ["0", "-5"] {
        let err = fx
            .conn()
            .execute(&format!("UPDATE account SET max_exposure = {bad} WHERE id = ?1"), [id])
            .expect_err("the column CHECK refuses a non-positive figure");
        assert!(err.to_string().contains("CHECK constraint failed"), "{bad}: {err}");
    }
    fx.conn()
        .execute("UPDATE account SET max_exposure = 12.5 WHERE id = ?1", [id])
        .expect("a positive figure is admitted");
    assert_eq!(fx.row(id).expect("the row").max_exposure, ceiling(12.5));
}

// ---------------------------------------------------------------------------------------------
// An OLD store: the column arrives by `ALTER`, and a read-only reader never needs it
// ---------------------------------------------------------------------------------------------

/// The shipped DDL as a store born before decision 0119 holds it: no `max_exposure`, and the old
/// `armed` column (with its check) still in position. Each replacement is asserted, so a DDL edit
/// that moved one of these lines fails here rather than planting the shipped shape.
fn old_ddl() -> String {
    let mut ddl = vike_secrets::DDL.to_string();
    let mut replace = |from: &str, to: &str| {
        assert_eq!(ddl.matches(from).count(), 1, "the shipped DDL carries {from:?} exactly once");
        ddl = ddl.replace(from, to);
    };
    replace("    max_exposure     REAL CHECK (max_exposure IS NULL OR max_exposure > 0.0),\n", "");
    replace(
        "    tier             TEXT    NOT NULL,\n",
        "    tier             TEXT    NOT NULL,\n    armed            INTEGER NOT NULL DEFAULT 0,\n",
    );
    replace(
        "    CHECK (active IN (0, 1))\n",
        "    CHECK (armed IN (0, 1)),\n    CHECK (active IN (0, 1))\n",
    );
    ddl
}

/// An old store at `fx.db()`, stamped, holding two binance accounts (one of them `armed`).
fn plant_old_store(fx: &Fixture) -> std::path::PathBuf {
    let db = fx.db();
    let conn = support::sql::plant_ddl(&db, &old_ddl());
    conn.execute_batch(
        "INSERT INTO venue (name) VALUES ('binance');
         INSERT INTO account (venue_id, tier, armed) VALUES (1, 'demo', 1);
         INSERT INTO account (venue_id, tier, label) VALUES (1, 'live', 'ALT');",
    )
    .expect("the old rows");
    support::sql::stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    drop(conn);
    db
}

/// ⚠ **R7 of the contract, the load-bearing one.** The daemon reads its accounts READ-ONLY at
/// boot, before any writer runs, and a read-only open cannot `ALTER`. On a store no writer has
/// carried since the column was added, the reader must answer every row with `None` (UNBOUNDED)
/// rather than `no such column` — which the mount would take as an unreadable store and mount
/// every account PAPER. The next write of ANY kind then adds the column, and the dead `armed`
/// column is left exactly as it was.
#[test]
fn an_old_store_reads_every_row_unbounded_and_gains_the_column_on_the_next_write() {
    let fx = Fixture::empty();
    let db = plant_old_store(&fx);
    let conn = support::sql::open(&db);
    assert!(
        !support::sql::has_column(&conn, "account", "max_exposure"),
        "the planted store is OLD"
    );
    assert!(support::sql::has_column(&conn, "account", "armed"), "…and carries the dead column");
    drop(conn);

    let Accounts::Known(rows) = vike_secrets::read_accounts(&db).expect("the read-only read")
    else {
        panic!("an old store with an `account` table answers")
    };
    assert_eq!(rows.len(), 2, "both rows read: {rows:?}");
    assert!(rows.iter().all(|a| a.max_exposure.is_none()), "every row is UNBOUNDED: {rows:?}");
    assert!(
        !support::sql::has_column(&support::sql::open(&db), "account", "max_exposure"),
        "a READ never alters the store"
    );

    // Any writer runs the funnel: a no-op activate is enough.
    let demo = rows.iter().find(|a| a.tier == "demo").expect("the demo row").id;
    let w = vike_secrets::edit_account(&db, AccountEdit::SetActive { id: demo, active: true })
        .expect("a no-op write over the old store");
    assert!(!w.changed);
    let conn = support::sql::open(&db);
    assert!(support::sql::has_column(&conn, "account", "max_exposure"), "the write added it");
    let armed: i64 = conn
        .query_row("SELECT armed FROM account WHERE id = ?1", [demo], |r| r.get(0))
        .expect("the dead column is still there");
    assert_eq!(armed, 1, "the dead `armed` column is untouched");
    drop(conn);

    let Accounts::Known(rows) = vike_secrets::read_accounts(&db).expect("read again") else {
        panic!("the store answers")
    };
    assert!(rows.iter().all(|a| a.max_exposure.is_none()), "no figure was written: {rows:?}");
    vike_secrets::edit_account(
        &db,
        AccountEdit::SetMaxExposure { id: demo, max_exposure: ceiling(250.0) },
    )
    .expect("the altered column takes a ceiling");
    let Accounts::Known(rows) = vike_secrets::read_accounts(&db).expect("read again") else {
        panic!("the store answers")
    };
    let read = rows.iter().find(|a| a.id == demo).expect("the demo row").max_exposure;
    assert_eq!(read, ceiling(250.0));
}

/// The `ALTER` and the DDL must declare the SAME column: the same type and the same `CHECK`, so a
/// fresh store and an altered one refuse and admit the same figures.
#[test]
fn fresh_and_altered_stores_store_the_same_column_text() {
    let fresh = Fixture::seeded();
    let altered = Fixture::empty();
    let db = plant_old_store(&altered);
    let demo = vike_secrets::read_accounts(&db)
        .expect("read")
        .known()
        .and_then(|rows| rows.iter().find(|a| a.tier == "demo").map(|a| a.id))
        .expect("the demo row");
    vike_secrets::edit_account(&db, AccountEdit::SetActive { id: demo, active: true })
        .expect("the write that alters the old store");

    let column_type = |conn: &rusqlite::Connection| -> String {
        conn.query_row(
            "SELECT type FROM pragma_table_info('account') WHERE name = 'max_exposure'",
            [],
            |r| r.get(0),
        )
        .expect("the column is there")
    };
    // The declaration as the engine stores it, whitespace collapsed (the DDL aligns its columns).
    let declaration = |conn: &rusqlite::Connection| -> bool {
        let sql = support::sql::table_sql(conn, "account");
        let collapsed = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        collapsed.contains("max_exposure REAL CHECK (max_exposure IS NULL OR max_exposure > 0.0)")
    };
    for (what, conn) in [("fresh", fresh.conn()), ("altered", support::sql::open(&db))] {
        assert_eq!(column_type(&conn), "REAL", "{what}: the column type");
        assert!(declaration(&conn), "{what}: the column carries the CHECK");
        let refused = conn.execute("UPDATE account SET max_exposure = 0 WHERE id = 1", []);
        assert!(refused.is_err(), "{what}: the CHECK refuses a zero");
    }
}

// ---------------------------------------------------------------------------------------------
// `Accounts::active_tier` and `Accounts::max_exposure_of`
// ---------------------------------------------------------------------------------------------

fn row(id: i64, venue: &str, tier: &str, label: Option<&str>, active: bool) -> Account {
    Account {
        id,
        venue: venue.to_string(),
        tier: tier.to_string(),
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
    }
}

#[test]
fn active_tier_answers_every_shape_of_rows() {
    let of = |rows: Vec<Account>, label: Option<&str>| -> Option<String> {
        let accounts = Accounts::Known(rows);
        Some(match accounts.active_tier("binance", label)? {
            ActiveTier::NoRow => "no row".to_string(),
            ActiveTier::Inactive => "inactive".to_string(),
            ActiveTier::Tier(t) => t.to_string(),
            ActiveTier::Conflict => "conflict".to_string(),
        })
    };
    let ans = |s: &str| Some(s.to_string());

    assert_eq!(of(vec![], None), ans("no row"));
    // Another venue's row is not this venue's row.
    assert_eq!(of(vec![row(1, "okx", "live", None, true)], None), ans("no row"));
    assert_eq!(of(vec![row(1, "binance", "live", None, false)], None), ans("inactive"));
    assert_eq!(of(vec![row(1, "binance", "paper", None, true)], None), ans("paper"));
    assert_eq!(
        of(
            vec![row(1, "binance", "demo", None, true), row(2, "binance", "paper", None, true)],
            None
        ),
        ans("demo"),
        "a paper row never conflicts"
    );
    assert_eq!(
        of(
            vec![row(1, "binance", "demo", None, true), row(2, "binance", "demo", None, true)],
            None
        ),
        ans("demo"),
        "two rows of the SAME tier (dukascopy's two books) are one answer"
    );
    assert_eq!(
        of(
            vec![row(1, "binance", "demo", None, true), row(2, "binance", "live", None, true)],
            None
        ),
        ans("conflict"),
        "two active non-paper tiers: no automatic pick"
    );
    assert_eq!(
        of(
            vec![row(1, "binance", "demo", None, true), row(2, "binance", "live", None, false)],
            None
        ),
        ans("demo"),
        "an inactive row states nothing"
    );
    // A labelled row vs the DEFAULT: each answers only for its own label.
    let labelled = || {
        vec![row(1, "binance", "demo", None, true), row(2, "binance", "live", Some("ALT"), true)]
    };
    assert_eq!(of(labelled(), None), ans("demo"), "the DEFAULT sees only the unlabelled row");
    assert_eq!(of(labelled(), Some("ALT")), ans("live"), "ALT sees only its own row");
    assert_eq!(of(labelled(), Some("HEDGE")), ans("no row"), "a label no row carries");

    let unanswerable = Accounts::Unanswerable(vike_secrets::NoAccountTable::NoStore {
        db: std::path::PathBuf::from("vike.db"),
    });
    assert_eq!(unanswerable.active_tier("binance", None), None, "an unanswerable store: None");
}

#[test]
fn max_exposure_of_is_the_tightest_active_figure() {
    let with = |mut a: Account, figure: f64| {
        a.max_exposure = ceiling(figure);
        a
    };
    let accounts = Accounts::Known(vec![
        with(row(1, "dukascopy", "demo", None, true), 900.0),
        with(row(2, "dukascopy", "demo", None, true), 400.0),
        with(row(3, "dukascopy", "demo", None, false), 10.0),
        with(row(4, "dukascopy", "demo", Some("B2"), true), 5.0),
        row(5, "binance", "live", None, true),
    ]);
    assert_eq!(
        accounts.max_exposure_of("dukascopy", None),
        ceiling(400.0),
        "the min over the ACTIVE rows of that label: not the inactive 10, not ALT's 5"
    );
    assert_eq!(accounts.max_exposure_of("dukascopy", Some("B2")), ceiling(5.0));
    assert_eq!(accounts.max_exposure_of("binance", None), None, "no figure stated: UNBOUNDED");
    assert_eq!(accounts.max_exposure_of("okx", None), None, "no row: UNBOUNDED");
}

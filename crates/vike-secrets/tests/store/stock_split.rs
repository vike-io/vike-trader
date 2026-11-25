//! **`stock_split`: one split per row, replaced by its venue, symbol and date, removed by them, what is no split and a
//! box with no store refused** — through the public writers and reader.

use vike_model::StockSplit;
use vike_secrets::StoreErrorKind;
use vike_secrets::stock_split::{
    delete_stock_split, read_sourced_stock_splits, read_stock_splits, read_ticker_renames,
    replace_sourced_splits, write_stock_split, write_ticker_rename,
};

use crate::support::Fixture;

fn split(venue: &str, symbol: &str, date: &str, ratio: f64) -> StockSplit {
    StockSplit::new(venue, symbol, date, ratio).expect("a split")
}

fn nflx(date: &str, ratio: f64) -> StockSplit {
    split("dukascopy", "NFLXUSUSD", date, ratio)
}

/// Every row as `(date, ratio, source)`.
fn rows(fx: &Fixture) -> Vec<(String, f64, String)> {
    read_sourced_stock_splits(fx.dir())
        .expect("read")
        .into_iter()
        .map(|(s, source)| (s.date, s.ratio, source))
        .collect()
}

fn row(date: &str, ratio: f64, source: &str) -> (String, f64, String) {
    (date.to_string(), ratio, source.to_string())
}

/// **A vendor's rows never touch the operator's** (decision 0289's amendment of 2026-11-16): the refresh writes and
/// removes only vendor rows; a date the operator holds keeps the operator's ratio; an unchanged verdict writes
/// nothing; a changed ratio or source (`yahoo` to `yahoo+fmp`) is written; an operator's add takes a vendor's row
/// over, and no later verdict removes it.
#[test]
fn a_vendors_rows_never_touch_the_operators() {
    let fx = Fixture::initialised();
    write_stock_split(fx.dir(), &nflx("2025-11-17", 9.0)).expect("the operator's row");
    let replace = |verdict: &[(StockSplit, &str)]| {
        replace_sourced_splits(fx.dir(), "dukascopy", "NFLXUSUSD", verdict).expect("write")
    };
    let verdict = [(nflx("2015-07-15", 7.0), "yahoo"), (nflx("2025-11-17", 10.0), "yahoo+fmp")];
    assert_eq!(replace(&verdict), (1, 0), "the operator's date is not written");
    assert_eq!(rows(&fx), [row("2015-07-15", 7.0, "yahoo"), row("2025-11-17", 9.0, "operator")]);
    assert_eq!(replace(&verdict), (0, 0), "an unchanged verdict writes nothing");
    assert_eq!(
        replace(&[(nflx("2015-07-15", 6.0), "yahoo")]),
        (1, 0),
        "a changed ratio is written"
    );
    assert_eq!(
        replace(&[(nflx("2015-07-15", 6.0), "yahoo+fmp")]),
        (1, 0),
        "a changed source is written"
    );
    assert_eq!(
        rows(&fx),
        [row("2015-07-15", 6.0, "yahoo+fmp"), row("2025-11-17", 9.0, "operator")]
    );

    assert_eq!(
        replace(&[]),
        (0, 1),
        "the vendor row it no longer carries goes; the operator's stays"
    );
    assert_eq!(rows(&fx), [row("2025-11-17", 9.0, "operator")]);

    assert_eq!(replace(&[(nflx("2004-02-12", 2.0), "yahoo")]), (1, 0));
    write_stock_split(fx.dir(), &nflx("2004-02-12", 2.0)).expect("the operator takes it over");
    assert_eq!(replace(&[]), (0, 0), "no vendor row is left to remove");
    assert_eq!(rows(&fx), [row("2004-02-12", 2.0, "operator"), row("2025-11-17", 9.0, "operator")]);
}

/// A vendor's write names a vendor and the one series it replaces; refused before the store is opened.
#[test]
fn a_vendors_write_names_a_vendor_and_its_one_series() {
    let fx = Fixture::initialised();
    for source in ["operator", " "] {
        replace_sourced_splits(
            fx.dir(),
            "dukascopy",
            "NFLXUSUSD",
            &[(nflx("2025-11-17", 10.0), source)],
        )
        .expect_err("no vendor");
    }
    replace_sourced_splits(
        fx.dir(),
        "dukascopy",
        "AAPLUSUSD",
        &[(nflx("2025-11-17", 10.0), "yahoo")],
    )
    .expect_err("another series");
    assert!(rows(&fx).is_empty());
}

/// **A store born before `source` reads its rows as the operator's**, and the next write carries the column in
/// (`ensure_venue_rows`) without touching them.
#[test]
fn a_store_born_before_the_source_column_reads_the_operators_rows() {
    let fx = Fixture::initialised();
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    conn.execute_batch(
        "DROP TABLE IF EXISTS stock_split;
         CREATE TABLE stock_split (id INTEGER PRIMARY KEY AUTOINCREMENT, venue_id INTEGER NOT NULL REFERENCES venue(id),
             symbol TEXT NOT NULL, date TEXT NOT NULL, ratio REAL NOT NULL, UNIQUE (venue_id, symbol, date)) STRICT;
         INSERT INTO stock_split (venue_id, symbol, date, ratio)
             VALUES ((SELECT id FROM venue WHERE name = 'dukascopy'), 'NFLXUSUSD', '2025-11-17', 10.0);",
    )
    .expect("the older table");
    drop(conn);
    assert_eq!(rows(&fx), [row("2025-11-17", 10.0, "operator")]);
    assert_eq!(
        replace_sourced_splits(fx.dir(), "dukascopy", "NFLXUSUSD", &[]).expect("write"),
        (0, 0)
    );
    assert_eq!(rows(&fx), [row("2025-11-17", 10.0, "operator")]);
}

#[test]
fn a_split_is_replaced_by_its_address_and_removed_by_it() {
    let fx = Fixture::initialised();
    assert!(read_stock_splits(fx.dir()).expect("read").is_empty(), "starts empty");

    write_stock_split(fx.dir(), &split("dukascopy", "AAPLUSUSD", "2020-08-31", 2.0))
        .expect("first");
    write_stock_split(fx.dir(), &split("dukascopy", "AAPLUSUSD", "2014-06-09", 7.0))
        .expect("an older one");
    // The same address again: ONE row, the newer ratio.
    write_stock_split(fx.dir(), &split("dukascopy", "AAPLUSUSD", "2020-08-31", 4.0))
        .expect("again");
    assert_eq!(
        read_stock_splits(fx.dir()).expect("read"),
        vec![
            split("dukascopy", "AAPLUSUSD", "2014-06-09", 7.0),
            split("dukascopy", "AAPLUSUSD", "2020-08-31", 4.0)
        ],
        "one row per (venue, symbol, date), in date order"
    );

    assert!(
        !delete_stock_split(fx.dir(), "alpaca", "AAPLUSUSD", "2020-08-31").expect("not alpaca's")
    );
    assert!(
        delete_stock_split(fx.dir(), "dukascopy", "AAPLUSUSD", "2020-08-31").expect("by address")
    );
    assert_eq!(
        read_stock_splits(fx.dir()).expect("read"),
        vec![split("dukascopy", "AAPLUSUSD", "2014-06-09", 7.0)]
    );
}

#[test]
fn what_is_no_split_and_a_venue_off_the_roster_are_refused() {
    let fx = Fixture::initialised();
    let no_split = StockSplit {
        venue: "dukascopy".into(),
        symbol: "AAPLUSUSD".into(),
        date: "2020-08-31".into(),
        ratio: 1.0,
    };
    write_stock_split(fx.dir(), &no_split).expect_err("a ratio of 1 is no split");
    write_stock_split(fx.dir(), &split("nowhere", "X", "2020-08-31", 4.0))
        .expect_err("no venue row");
    assert!(read_stock_splits(fx.dir()).expect("read").is_empty());
}

#[test]
fn no_store_is_refused_and_none_is_created() {
    let fx = Fixture::empty();
    let err = write_stock_split(fx.dir(), &split("dukascopy", "AAPLUSUSD", "2020-08-31", 4.0))
        .expect_err("no store");
    assert!(matches!(err.kind, StoreErrorKind::NoSettingsDatabase), "{err}");
    let err =
        delete_stock_split(fx.dir(), "dukascopy", "AAPLUSUSD", "2020-08-31").expect_err("no store");
    assert!(matches!(err.kind, StoreErrorKind::NoSettingsDatabase), "{err}");
    assert!(!fx.db().exists(), "nothing created the database");
    assert!(read_stock_splits(fx.dir()).expect("no store reads empty").is_empty());
}

/// **A ticker rename is one row per venue and ticker, replaced by it; a spelling that is no ticker never reaches the
/// store** (each rides a vendor's URL: an `&` would add a query parameter to FMP's).
#[test]
fn a_ticker_rename_is_replaced_by_its_address_and_a_bad_spelling_is_refused() {
    let fx = Fixture::initialised();
    assert!(read_ticker_renames(fx.dir()).expect("read").is_empty(), "starts empty");
    write_ticker_rename(fx.dir(), "dukascopy", "FB", "FB").expect("first");
    write_ticker_rename(fx.dir(), "dukascopy", "FB", "META").expect("again");
    write_ticker_rename(fx.dir(), "alpaca", "BRK.B", "BRK-B").expect("another venue");
    let pair = |v: &str, t: &str, r: &str| (v.to_string(), t.to_string(), r.to_string());
    assert_eq!(
        read_ticker_renames(fx.dir()).expect("read"),
        [pair("alpaca", "BRK.B", "BRK-B"), pair("dukascopy", "FB", "META")]
    );
    for (ticker, renamed) in
        [("FB", "META&apikey=x"), ("fb", "META"), ("", "META"), ("FB", "MET A")]
    {
        write_ticker_rename(fx.dir(), "dukascopy", ticker, renamed)
            .expect_err(&format!("{ticker} {renamed}"));
    }
    write_ticker_rename(fx.dir(), "nowhere", "FB", "META").expect_err("no venue row");
    assert_eq!(read_ticker_renames(fx.dir()).expect("read").len(), 2);

    let empty = Fixture::empty();
    let err = write_ticker_rename(empty.dir(), "dukascopy", "FB", "META").expect_err("no store");
    assert!(matches!(err.kind, StoreErrorKind::NoSettingsDatabase), "{err}");
    assert!(read_ticker_renames(empty.dir()).expect("no store reads empty").is_empty());
}

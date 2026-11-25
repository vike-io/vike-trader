//! **`operator_instrument_list`: one venue's row per write, the others untouched, and a box with no store refused**
//! — through the public writer and reader.

use vike_model::{Contract, Instrument};
use vike_secrets::StoreErrorKind;
use vike_secrets::operator_instrument_list::{
    OperatorInstrumentList, read_operator_instrument_lists, write_operator_instrument_list,
};

use crate::support::Fixture;

fn list(venue: &str, n: usize) -> OperatorInstrumentList {
    OperatorInstrumentList {
        venue: venue.into(),
        fetched: "2026-11-12".into(),
        qualifier: "demo".into(),
        instruments: (0..n)
            .map(|i| Instrument::new(venue, format!("SYM{i}"), "AAA", "USD", Contract::CryptoSpot))
            .collect(),
    }
}

#[test]
fn a_write_replaces_one_venue_and_leaves_the_others() {
    let fx = Fixture::initialised();
    assert!(read_operator_instrument_lists(fx.dir()).expect("read").is_empty(), "starts empty");

    write_operator_instrument_list(fx.dir(), &list("alpaca", 3)).expect("alpaca");
    write_operator_instrument_list(fx.dir(), &list("oanda", 2)).expect("oanda");
    // A second writer of ANOTHER venue's list, then of alpaca again: neither erases the other.
    let mut newer = list("alpaca", 5);
    newer.fetched = "2026-11-13".into();
    write_operator_instrument_list(fx.dir(), &newer).expect("alpaca again");

    let back = read_operator_instrument_lists(fx.dir()).expect("read");
    assert_eq!(back, vec![newer, list("oanda", 2)], "one row per venue, in first-written order");
}

#[test]
fn an_empty_list_is_a_row() {
    let fx = Fixture::initialised();
    write_operator_instrument_list(fx.dir(), &list("ctrader", 0)).expect("a measured zero");
    assert_eq!(read_operator_instrument_lists(fx.dir()).expect("read"), vec![list("ctrader", 0)]);
}

#[test]
fn no_store_is_refused_and_none_is_created() {
    let fx = Fixture::empty();
    let err = write_operator_instrument_list(fx.dir(), &list("alpaca", 1)).expect_err("no store");
    assert!(matches!(err.kind, StoreErrorKind::NoSettingsDatabase), "{err}");
    assert!(!fx.db().exists(), "nothing created the database");
    assert!(read_operator_instrument_lists(fx.dir()).expect("no store reads empty").is_empty());
}

#[test]
fn a_venue_off_the_roster_is_refused() {
    let fx = Fixture::initialised();
    write_operator_instrument_list(fx.dir(), &list("nowhere", 1)).expect_err("no venue row");
    assert!(read_operator_instrument_lists(fx.dir()).expect("read").is_empty());
}

#[test]
fn a_row_that_does_not_parse_is_an_error_not_an_empty_answer() {
    let fx = Fixture::initialised();
    write_operator_instrument_list(fx.dir(), &list("alpaca", 1)).expect("alpaca");
    fx.conn()
        .execute("UPDATE operator_instrument_list SET instruments = '{ not json'", [])
        .expect("corrupt the row by hand");
    let err = read_operator_instrument_lists(fx.dir()).expect_err("must not read as empty");
    assert!(err.to_string().contains("alpaca"), "{err}");
}

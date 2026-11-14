//! **`operator_instrument`: one instrument per row, replaced by its venue's own name, removed by that name or by the
//! typed word, and a box with no store refused** — through the public writers and reader.

use vike_model::{Contract, ContractFx, Instrument};
use vike_secrets::StoreErrorKind;
use vike_secrets::operator_instrument::{
    OperatorInstrument, delete_operator_instrument, read_operator_instruments,
    write_operator_instrument,
};
use vike_secrets::operator_instrument_list::{
    OperatorInstrumentList, write_operator_instrument_list,
};

use crate::support::Fixture;

fn fx_contract() -> Contract {
    Contract::Fx(ContractFx { pip_size: None, lot_size: None, swap_long: None, swap_short: None })
}

fn row(venue: &str, asked: &str, raw: &str) -> OperatorInstrument {
    OperatorInstrument {
        asked: asked.into(),
        added: "2026-11-13".into(),
        instrument: Instrument::new(venue, raw, "EUR", "USD", fx_contract()),
    }
}

#[test]
fn a_row_is_replaced_by_its_venue_name_and_removed_by_either_name() {
    let fx = Fixture::initialised();
    assert!(read_operator_instruments(fx.dir()).expect("read").is_empty(), "starts empty");

    write_operator_instrument(fx.dir(), &row("ig", "EURUSD", "CS.D.EURUSD.MINI.IP")).expect("ig");
    write_operator_instrument(fx.dir(), &row("dukascopy", "AAPL", "AAPL.US/USD")).expect("duka");
    // The same instrument again, typed another way: ONE row, the newer word.
    let again = row("ig", "EUR/USD", "CS.D.EURUSD.MINI.IP");
    write_operator_instrument(fx.dir(), &again).expect("ig again");
    assert_eq!(
        read_operator_instruments(fx.dir()).expect("read"),
        vec![again, row("dukascopy", "AAPL", "AAPL.US/USD")],
        "one row per (venue, raw_symbol), in first-added order"
    );

    // By the typed word, then by the venue's own name; another venue's row of the same word is not touched.
    assert!(delete_operator_instrument(fx.dir(), "ig", "EUR/USD").expect("by the word"));
    assert!(!delete_operator_instrument(fx.dir(), "ig", "AAPL").expect("not ig's"));
    assert!(delete_operator_instrument(fx.dir(), "dukascopy", "AAPL.US/USD").expect("by name"));
    assert!(read_operator_instruments(fx.dir()).expect("read").is_empty());
}

#[test]
fn a_catalog_refresh_of_the_venue_list_touches_no_row_added_by_hand() {
    let fx = Fixture::initialised();
    write_operator_instrument(fx.dir(), &row("alpaca", "SPY", "SPY")).expect("by hand");
    let list = OperatorInstrumentList {
        venue: "alpaca".into(),
        fetched: "2026-11-13".into(),
        qualifier: "demo".into(),
        instruments: Vec::new(),
    };
    write_operator_instrument_list(fx.dir(), &list).expect("the whole-row refresh writer");
    assert_eq!(
        read_operator_instruments(fx.dir()).expect("read"),
        vec![row("alpaca", "SPY", "SPY")]
    );
}

#[test]
fn no_store_is_refused_and_none_is_created() {
    let fx = Fixture::empty();
    let err = write_operator_instrument(fx.dir(), &row("ig", "EURUSD", "CS.D.EURUSD.MINI.IP"))
        .expect_err("no store");
    assert!(matches!(err.kind, StoreErrorKind::NoSettingsDatabase), "{err}");
    let err = delete_operator_instrument(fx.dir(), "ig", "EURUSD").expect_err("no store");
    assert!(matches!(err.kind, StoreErrorKind::NoSettingsDatabase), "{err}");
    assert!(!fx.db().exists(), "nothing created the database");
    assert!(read_operator_instruments(fx.dir()).expect("no store reads empty").is_empty());
}

#[test]
fn a_venue_off_the_roster_is_refused() {
    let fx = Fixture::initialised();
    write_operator_instrument(fx.dir(), &row("nowhere", "X", "X")).expect_err("no venue row");
    assert!(read_operator_instruments(fx.dir()).expect("read").is_empty());
}

#[test]
fn a_row_that_does_not_parse_is_an_error_not_an_empty_answer() {
    let fx = Fixture::initialised();
    write_operator_instrument(fx.dir(), &row("ig", "EURUSD", "CS.D.EURUSD.MINI.IP")).expect("ig");
    fx.conn()
        .execute("UPDATE operator_instrument SET instrument = '{ not json'", [])
        .expect("corrupt the row by hand");
    let err = read_operator_instruments(fx.dir()).expect_err("must not read as empty");
    assert!(err.to_string().contains("EURUSD"), "{err}");
}

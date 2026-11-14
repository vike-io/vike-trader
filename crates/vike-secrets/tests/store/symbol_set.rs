//! **`symbol_set`: one row per name, saved over by name, deleted by name, and a box with no store refused** —
//! through the public writers and reader.

use vike_secrets::StoreErrorKind;
use vike_secrets::symbol_set::{
    SymbolSetRow, delete_symbol_set_row, read_symbol_set_rows, write_symbol_set_row,
};

use crate::support::Fixture;

fn set(name: &str, symbols: &[&str]) -> SymbolSetRow {
    SymbolSetRow {
        name: name.into(),
        symbols: symbols.iter().map(|s| (*s).to_string()).collect(),
        provider: "binance".into(),
        interval: "1h".into(),
        benchmark: "BTCUSDT".into(),
        user: true,
    }
}

#[test]
fn a_set_is_saved_over_by_name_and_deleted_by_name() {
    let fx = Fixture::initialised();
    assert!(read_symbol_set_rows(fx.dir()).expect("read").is_empty(), "starts empty");

    write_symbol_set_row(fx.dir(), &set("Alts", &["SOLUSDT", "AVAXUSDT"])).expect("alts");
    write_symbol_set_row(fx.dir(), &set("Majors", &["BTCUSDT"])).expect("majors");
    let newer = set("Alts", &["SOLUSDT"]);
    write_symbol_set_row(fx.dir(), &newer).expect("alts again");
    assert_eq!(
        read_symbol_set_rows(fx.dir()).expect("read"),
        vec![newer, set("Majors", &["BTCUSDT"])]
    );

    assert!(delete_symbol_set_row(fx.dir(), "Alts").expect("delete"));
    assert!(
        !delete_symbol_set_row(fx.dir(), "Alts").expect("a second delete"),
        "nothing left to delete"
    );
    assert_eq!(read_symbol_set_rows(fx.dir()).expect("read"), vec![set("Majors", &["BTCUSDT"])]);
}

#[test]
fn no_store_is_refused_and_none_is_created() {
    let fx = Fixture::empty();
    let err = write_symbol_set_row(fx.dir(), &set("Alts", &["SOLUSDT"])).expect_err("no store");
    assert!(matches!(err.kind, StoreErrorKind::NoSettingsDatabase), "{err}");
    let err = delete_symbol_set_row(fx.dir(), "Alts").expect_err("no store");
    assert!(matches!(err.kind, StoreErrorKind::NoSettingsDatabase), "{err}");
    assert!(!fx.db().exists(), "nothing created the database");
    assert!(read_symbol_set_rows(fx.dir()).expect("no store reads empty").is_empty());
}

//! **Test support, never a door: the RECORDED kept lists** (`tests/fixtures/instruments/`, laid out as the datahub's
//! `<project>/instruments`). binance's four COIN-M rows and okx's two inverse rows, exactly as each bridge's catalog
//! parser reads them off that bridge's committed venue payload; the bridge holds them equal to its parse, row for row
//! (`crates/bridges/binance/src/catalog_class_tests.rs`'s and `crates/bridges/okx/src/catalog.rs`'s
//! `the_name_rule_agrees_with_every_row`). A coin-margined face on these two venues is only its row's
//! (`crate::kept_contract_value`), so a test that holds no datahub hands its reader these rows: a backtest's kept rows,
//! a paper book's [`hold_recorded_kept_lists`].

use crate::VenueInstrumentList;

/// The recorded kept lists, binance then okx, each row's names spelled as the datahub's write stores them
/// (`crate::spelled`: the fixtures are the bridges' parse, which stores none).
///
/// # Panics
/// A fixture that does not parse as a kept list: a test-support defect, never a run-time one.
#[must_use]
pub fn recorded_kept_lists() -> Vec<VenueInstrumentList> {
    [
        include_str!("../tests/fixtures/instruments/binance.json"),
        include_str!("../tests/fixtures/instruments/okx.json"),
    ]
    .into_iter()
    .map(|text| {
        let mut list: VenueInstrumentList =
            serde_json::from_str(text).expect("a recorded kept list parses");
        list.instruments = crate::spelled(list.instruments);
        list
    })
    .collect()
}

/// **Hold every recorded kept list in this test process** (`crate::hold_kept_list`), what a reader of
/// `crate::kept_row_of` (a paper book) then finds. A second call changes nothing.
pub fn hold_recorded_kept_lists() {
    for list in recorded_kept_lists() {
        crate::hold_kept_list(&list.venue, list.instruments);
    }
}

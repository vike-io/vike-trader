//! The ONE ig kept list this test binary holds (decision 0290: the engine's USER names reach the bridge, and the epic
//! IG is asked about is the name's KEPT row's). `vike_catalog::hold_kept_list` is process-wide and the tests run in
//! parallel threads, so every test of this binary that holds ig's list holds exactly these rows: the FX rows of four
//! Minis and of EUR/USD's standard lot, each the row IG's own catalog makes of it (`vike_ig::parse_markets`: the pair
//! off the epic, `CURRENCIES`, and IG's naming, decision 0290 item 6: the Mini `EURUSD`, the standard lot
//! `EURUSD.CFD`) with the pip and unit its market details state, and one index CFD whose name the operator wrote on his
//! row (`DE40`, so `DE40.CFD`).

use serde_json::json;
use vike_model::{
    Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying,
    ContractFx, Instrument,
};

/// Every FX row the suites order, subscribe to, look up or reconcile: its epic, IG's market name, its pip and its unit
/// (`contractSize`).
const PAIRS: [(&str, &str, f64, f64); 5] = [
    ("CS.D.EURUSD.MINI.IP", "EUR/USD Mini", 0.0001, 10_000.0),
    ("CS.D.GBPUSD.MINI.IP", "GBP/USD Mini", 0.0001, 10_000.0),
    ("CS.D.EURGBP.MINI.IP", "EUR/GBP Mini", 0.0001, 10_000.0),
    ("CS.D.USDJPY.MINI.IP", "USD/JPY Mini", 0.01, 10_000.0),
    ("CS.D.EURUSD.CFD.IP", "EUR/USD", 0.0001, 100_000.0),
];

/// The dated DAX future CFD `dated_epics.rs` deals (Germany 40 (E1), `DEC-26`).
const DAX_DEC: &str = "IX.D.DAX.FBMU1.IP";

/// The kept rows, named as the datahub's write stores them (`vike_catalog::spelled` keeps IG's stated naming).
pub(crate) fn rows() -> Vec<Instrument> {
    let mut rows: Vec<Instrument> = PAIRS
        .iter()
        .map(|&(epic, name, pip, unit)| {
            let search = json!({ "markets": [{
                "epic": epic, "instrumentName": name, "instrumentType": "CURRENCIES", "expiry": "-"
            }] });
            let mut row = vike_ig::parse_markets(&search).remove(0);
            let fx =
                ContractFx { pip_size: Some(pip), lot_size: Some(unit), ..ContractFx::default() };
            row.contract = Contract::Fx(fx);
            row
        })
        .collect();
    let index = ContractCfd {
        underlying_kind: ContractCfdUnderlying::Index,
        underlying: None,
        product: ContractCfdProduct::Cfd,
        expiry: ContractCfdExpiry::Unknown,
        margin_rate: None,
        swap_long: None,
        swap_short: None,
    };
    rows.push(
        Instrument::new("ig", DAX_DEC, "DE40", "EUR", Contract::Cfd(index))
            .with_venue_word("INDICES"),
    );
    vike_catalog::spelled(rows)
}

/// Hold [`rows`] as this process's ig list.
pub(crate) fn hold() {
    vike_catalog::hold_kept_list("ig", rows());
}

/// The USER name of `epic`'s row (`EURUSD`, `DE40.CFD`): what the engine hands the bridge for it.
pub(crate) fn user_name(epic: &str) -> String {
    let rows = rows();
    let row = rows
        .iter()
        .find(|r| r.raw_symbol == epic)
        .unwrap_or_else(|| panic!("{epic} has a test row"));
    vike_catalog::user_name_of_row(row).expect("a user name")
}

/// The epic of the row whose USER name is `user`: what IG's bodies name it by.
pub(crate) fn epic(user: &str) -> String {
    let rows = rows();
    vike_catalog::kept_row_of_user_name(&rows, "ig", user).expect("a test row").raw_symbol.clone()
}

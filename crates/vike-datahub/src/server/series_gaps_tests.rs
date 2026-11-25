use super::*;

use vike_catalog::{VenueInstrumentList, kept_file};
use vike_model::time::MS_PER_DAY;
use vike_model::{
    Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying,
    ContractFx, Instrument,
};

/// The store's range for the whole UTC day `y-m-d`.
fn day(y: i64, m: u32, d: u32) -> (i64, i64) {
    let start = vike_utility::calendar::days_from_civil(y, m, d) * MS_PER_DAY;
    (start, start + MS_PER_DAY - 1)
}

/// Saturday 2026-11-14 and Wednesday 2026-11-11, both missing from a series.
fn saturday_and_wednesday() -> Vec<(i64, i64)> {
    vec![day(2026, 11, 11), day(2026, 11, 14)]
}

fn fx(symbol: &str) -> Instrument {
    let contract = Contract::Fx(ContractFx {
        pip_size: Some(0.0001),
        lot_size: None,
        swap_long: None,
        swap_short: None,
    });
    Instrument::new("dukascopy", symbol, "USD", "CHF", contract)
}

/// A dukascopy kept list holding `rows`, named as the datahub's write stores them (`vike_catalog::spelled`), written
/// where the datahub keeps it.
fn kept(rows: Vec<Instrument>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let list = VenueInstrumentList {
        venue: "dukascopy".into(),
        refreshed_ms: Some(1),
        truncated: false,
        last_error: None,
        last_try_ms: None,
        instruments: vike_catalog::spelled(rows),
        added_by_operator: Vec::new(),
    };
    let body = serde_json::to_vec(&list).expect("list serialises");
    std::fs::write(kept_file(dir.path(), "dukascopy"), body).expect("write the kept list");
    dir
}

fn series(venue: &str, symbol: &str) -> SeriesId {
    SeriesId::per_symbol("bar".parse().unwrap(), venue, symbol, Some("1m".into()))
}

/// An FX pair its kept row states on an FX-week broker: the Saturday is closed time, the Wednesday is
/// a hole. The store names the pair by its user name (`USDCHF` for the row `USD/CHF`, decision 0290).
#[test]
fn an_fx_pair_keeps_the_wednesday_and_drops_the_saturday() {
    let dir = kept(vec![fx("USD/CHF")]);
    let gaps =
        trading_gaps(&series("dukascopy", "USDCHF"), saturday_and_wednesday(), Some(dir.path()));
    assert_eq!(gaps, vec![day(2026, 11, 11)]);
}

/// A crypto venue trades every day: its Saturday is a hole, and no list is read for it.
#[test]
fn a_crypto_series_keeps_its_saturday() {
    let gaps = trading_gaps(&series("binance", "BTCUSDT"), saturday_and_wednesday(), None);
    assert_eq!(gaps, saturday_and_wednesday());
}

fn cfd(symbol: &str, base: &str, underlying_kind: ContractCfdUnderlying) -> Instrument {
    let contract = Contract::Cfd(ContractCfd {
        underlying_kind,
        underlying: None,
        product: ContractCfdProduct::Cfd,
        expiry: ContractCfdExpiry::Rolling,
        margin_rate: None,
        swap_long: None,
        swap_short: None,
    });
    Instrument::new("dukascopy", symbol, base, "USD", contract)
}

/// A share CFD runs whole weekdays (its row names no region): a series with only weekend holes has no
/// gap, and its missing Wednesday is one gap. Saturday 14 and Sunday 15 November 2026 are the weekend.
#[test]
fn a_share_cfd_drops_its_weekend_and_keeps_the_wednesday() {
    let dir = kept(vec![cfd("SPY.US/USD", "", ContractCfdUnderlying::Share)]);
    let id = series("dukascopy", "SPY.CFD");
    let weekend = vec![day(2026, 11, 14), day(2026, 11, 15)];
    assert_eq!(trading_gaps(&id, weekend, Some(dir.path())), Vec::new());
    assert_eq!(
        trading_gaps(&id, saturday_and_wednesday(), Some(dir.path())),
        vec![day(2026, 11, 11)]
    );
}

/// Any other CFD (an index, named on the operator's row by the base it states: decision 0290) and a series with no
/// kept list keep every missing day: the calendar is never read off a name.
#[test]
fn an_unknown_class_keeps_every_missing_day() {
    let dir = kept(vec![cfd("USA500.IDX/USD", "USA500", ContractCfdUnderlying::Index)]);
    let id = series("dukascopy", "USA500.CFD");
    assert_eq!(
        trading_gaps(&id, saturday_and_wednesday(), Some(dir.path())),
        saturday_and_wednesday()
    );
    let id = series("dukascopy", "USDCHF");
    assert_eq!(trading_gaps(&id, saturday_and_wednesday(), None), saturday_and_wednesday());
}

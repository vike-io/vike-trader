use super::*;
use vike_data::SeriesId;
use vike_data::coverage::join_coverage;

fn cov(venue: &str, sym: &str, rows: &[(&str, Vec<i64>)]) -> Vec<vike_data::InstrumentCoverage> {
    join_coverage(
        &rows
            .iter()
            .map(|(k, d)| (SeriesId::per_symbol(*k, venue, sym, None), d.clone()))
            .collect::<Vec<_>>(),
    )
}

/// **The case per-series gaps cannot show.** A Polymarket venue-fill restores the trade tape and
/// nothing else, so trades are contiguous and the book series simply lacks that day — neither
/// series looks wrong on its own, and `GapMap` reports nothing.
#[test]
fn a_trades_only_day_is_partial_even_though_no_series_has_a_gap() {
    let report = cov(
        "polymarket",
        "TOK",
        &[("trade", vec![10, 11, 12]), ("quote", vec![10, 12]), ("book", vec![10, 12])],
    );
    let map = partial_days_from_coverage(&report);
    let key = report[0].key.clone();

    assert!(has_partial_days(&map, &key));
    assert_eq!(partial_days_label(&map, &key), "1 partial day (book, quote)");
}

/// A fully covered instrument produces NO entry — so an absent key means "fine", and a caller
/// renders nothing without a branch.
#[test]
fn a_complete_instrument_has_no_entry_and_an_empty_label() {
    let report = cov(
        "polymarket",
        "TOK",
        &[("trade", vec![1, 2]), ("quote", vec![1, 2]), ("book", vec![1, 2])],
    );
    let map = partial_days_from_coverage(&report);
    let key = report[0].key.clone();

    assert!(map.is_empty());
    assert!(!has_partial_days(&map, &key));
    assert_eq!(partial_days_label(&map, &key), "");
}

/// An unknown instrument is "fine", not a panic — the map is a hint the caller may not have
/// fetched yet.
#[test]
fn an_unfetched_instrument_reads_as_nothing_partial() {
    let map = PartialDayMap::new();
    let key = vike_data::InstrumentKey {
        venue: "binance".into(),
        label: "BTCUSDT".into(),
        grouped: false,
    };
    assert!(!has_partial_days(&map, &key));
    assert_eq!(partial_days_label(&map, &key), "");
}

/// The label pluralises and de-duplicates the kind list ACROSS days: day 1 lacks book, day 2
/// lacks quote, days 3-4 lack both — four days, one line naming two kinds.
#[test]
fn the_label_summarises_many_days_into_one_line() {
    let report = cov(
        "binance",
        "BTCUSDT",
        &[("trade", vec![1, 2, 3, 4]), ("quote", vec![1]), ("book", vec![2])],
    );
    let map = partial_days_from_coverage(&report);
    assert_eq!(partial_days_label(&map, &report[0].key), "4 partial days (book, quote)");
}

/// **The false positive the live screenshot exposed.** binance `BTCUSDT.P` records `trade` and
/// `depth` and nothing else — its L2 lives in the `depth` lane, not `book`. Held against the
/// global kind list it read "missing book, quote" on every day it would ever have: a permanent
/// ⚠ carrying no information. Held against the kinds it actually records, it is complete.
#[test]
fn a_trade_plus_depth_instrument_is_not_marked() {
    let report = cov("binance", "BTCUSDT.P", &[("trade", vec![1, 2, 3]), ("depth", vec![1, 2, 3])]);
    let map = partial_days_from_coverage(&report);
    assert!(map.is_empty(), "{map:?}");
    assert_eq!(partial_days_label(&map, &report[0].key), "");
}

/// …and the same instrument IS marked the day its depth lane stops while trades keep flowing —
/// the half-failed recording this column exists to surface. Narrowing the comparison set does
/// not weaken the signal.
#[test]
fn a_depth_outage_beside_a_live_trade_tape_is_marked() {
    let report = cov("binance", "BTCUSDT.P", &[("trade", vec![1, 2, 3]), ("depth", vec![1, 3])]);
    let map = partial_days_from_coverage(&report);
    assert_eq!(partial_days_label(&map, &report[0].key), "1 partial day (depth)");
}

/// Grouped and per-symbol series of one name are DIFFERENT instruments (different directories,
/// different manifests), and the map keys them apart — merging would hide a mid-migration split.
#[test]
fn grouped_and_per_symbol_are_keyed_apart() {
    // Each side records trade AND quote, and each has one day where quote fell behind — so
    // both are partial. (A trade-ONLY instrument is complete at what it records, so it would
    // produce no entry at all and this test would prove nothing.)
    let report = join_coverage(&[
        (SeriesId::per_symbol("trade", "polymarket", "fam", None), vec![1, 2]),
        (SeriesId::per_symbol("quote", "polymarket", "fam", None), vec![1]),
        (SeriesId::grouped("trade", "polymarket", "fam"), vec![5, 6]),
        (SeriesId::grouped("quote", "polymarket", "fam"), vec![5]),
    ]);
    let map = partial_days_from_coverage(&report);
    // Two entries, not one merged: same venue, same label, different layout.
    assert_eq!(map.len(), 2, "{map:?}");
    assert!(map.keys().any(|k| k.grouped));
    assert!(map.keys().any(|k| !k.grouped));
}

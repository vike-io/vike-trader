//! `HistoryStoreCsv`: range reads through the file lane, the catalog, and every refusal.

use super::*;

const MIN: i64 = 60_000;
const T0: i64 = 1_700_000_040_000; // a whole minute, 2023-11-14T22:14:00Z

/// Four 1m bars at minutes 0, 1, 2 and 4 (minute 3 is missing), each `open = close - 0.5 = minute`.
fn text() -> String {
    let mut s = String::from("time,open,high,low,close,volume\n");
    for m in [0, 1, 2, 4] {
        s.push_str(&format!("{},{m},{},{},{},1\n", T0 + m * MIN, m + 1, m, m as f64 + 0.5));
    }
    s
}

fn store() -> HistoryStoreCsv {
    HistoryStoreCsv::from_csv(
        "bars.csv",
        &text(),
        &CsvBarsColumns::Header,
        &CsvBarsFormat::default(),
        "binance",
        "BTCUSDT",
        "1m",
    )
    .unwrap()
}

fn read(s: &HistoryStoreCsv, range: TimestampRange) -> Vec<i64> {
    s.load_bars_sourced_capped("binance", FILE_SOURCE, "BTCUSDT", "1m", range, None)
        .unwrap()
        .iter()
        .map(|b| b.timestamp)
        .collect()
}

#[test]
fn the_file_lane_reads_the_bars_over_a_range() {
    let s = store();
    assert_eq!(read(&s, TimestampRange::all()), [T0, T0 + MIN, T0 + 2 * MIN, T0 + 4 * MIN]);
    assert_eq!(read(&s, TimestampRange::of(T0 + MIN, T0 + 2 * MIN)), [T0 + MIN, T0 + 2 * MIN]);
    assert_eq!(read(&s, TimestampRange { start: Some(T0 + 3 * MIN), end: None }), [T0 + 4 * MIN]);
    assert!(read(&s, TimestampRange::of(0, T0 - 1)).is_empty());
    let first = &s
        .load_bars_sourced_capped(
            "binance",
            FILE_SOURCE,
            "BTCUSDT",
            "1m",
            TimestampRange::all(),
            Some(1),
        )
        .unwrap()[0];
    assert_eq!(*first, Bar::ohlcv(T0, 0.0, 1.0, 0.0, 0.5, 1.0), "the decoded bar, whole");
    // Another series of the lane is the missing series' answer: empty.
    for (venue, symbol, interval) in
        [("bybit", "BTCUSDT", "1m"), ("binance", "ETHUSDT", "1m"), ("binance", "BTCUSDT", "5m")]
    {
        let other = s.load_bars_sourced_capped(
            venue,
            FILE_SOURCE,
            symbol,
            interval,
            TimestampRange::all(),
            None,
        );
        assert!(other.unwrap().is_empty(), "{venue} {symbol} {interval}");
    }
}

/// It lists the one series it holds, in the file lane, with the bars' coverage and the missing minute.
#[test]
fn it_lists_its_one_series_in_the_file_lane() {
    let s = store();
    let id = SeriesId::per_symbol(StoreKindId::Bar, "binance", "BTCUSDT", Some("1m".into()))
        .with_source(FILE_SOURCE);
    assert_eq!(s.series(), &id);
    assert_eq!(s.list_series().unwrap(), std::slice::from_ref(&id));
    let (listed, cov) = &s.inventory().unwrap()[0];
    assert_eq!(listed, &id);
    assert_eq!((cov.first_timestamp, cov.last_timestamp, cov.rows), (T0, T0 + 4 * MIN, 4));
    assert_eq!(s.series_gaps(&id).unwrap(), [(T0 + 3 * MIN, T0 + 3 * MIN)]);
    let sourceless =
        SeriesId::per_symbol(StoreKindId::Bar, "binance", "BTCUSDT", Some("1m".into()));
    assert!(s.series_gaps(&sourceless).unwrap().is_empty());
    assert!(s.coverage_report().unwrap().is_empty(), "it holds no tick lane to line up");
}

/// The venue's own series is never answered from a file, and neither is another lane.
#[test]
fn the_venue_series_and_every_other_lane_are_refused() {
    let s = store();
    let why =
        s.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap_err().to_string();
    assert!(why.contains("never a venue's own series") && why.contains("bars.csv"), "{why}");
    assert!(s.bar_edges("binance", "BTCUSDT", "1m", TimestampRange::all()).is_err());
    assert!(s.load_bars_head("binance", "BTCUSDT", "1m", TimestampRange::all(), 1).is_err());
    let why = s
        .load_bars_sourced_capped(
            "binance",
            "aggtrades",
            "BTCUSDT",
            "1m",
            TimestampRange::all(),
            None,
        )
        .unwrap_err()
        .to_string();
    assert!(why.contains("not `source=aggtrades`"), "{why}");
    assert!(
        s.scan_quotes_sourced_capped(
            "binance",
            FILE_SOURCE,
            "BTCUSDT",
            TimestampRange::all(),
            None
        )
        .is_err()
    );
}

/// Every write and every other kind is refused, with the reason, and nothing reads as an empty store.
#[test]
fn every_write_and_every_other_kind_is_refused() {
    let s = store();
    let r = TimestampRange::all();
    let bar = [Bar::ohlcv(T0, 1.0, 1.0, 1.0, 1.0, 1.0)];
    let refusals: Vec<(&str, Result<usize, HistoryStoreError>)> = vec![
        ("append_bars", s.append_bars("binance", "BTCUSDT", "1m", &bar, None)),
        ("append_quotes", s.append_quotes("binance", "BTCUSDT", &[], None)),
        ("append_trades", s.append_trades("binance", "BTCUSDT", &[], None)),
        ("append_book_updates", s.append_book_updates("binance", "BTCUSDT", &[], None)),
        ("append_depth", s.append_depth("binance", "BTCUSDT", &[], None)),
        ("append_symbol_properties", s.append_symbol_properties("binance", "BTCUSDT", &[], None)),
        ("append_equity", s.append_equity("binance", "BTCUSDT", &[], None)),
        ("append_exec_fills", s.append_exec_fills("binance", "BTCUSDT", &[], None)),
        ("append_exec_orders", s.append_exec_orders("binance", "BTCUSDT", &[], None)),
        ("append_funding", s.append_funding("binance", "BTC", &[], None)),
        ("append_chain_snapshot", s.append_chain_snapshot("deribit", "BTC", &[], None)),
        ("append_cohort", s.append_cohort("binance", "BTC", &[], None)),
        ("append_perp_metrics", s.append_perp_metrics("binance", "BTCUSDT", &[], None)),
        ("resample_quotes_to_bars", s.resample_quotes_to_bars("binance", "BTCUSDT", "1m", r, None)),
        ("resample_trades_to_bars", s.resample_trades_to_bars("binance", "BTCUSDT", "1m", r, None)),
        ("scan_quotes", s.scan_quotes("binance", "BTCUSDT", r).map(|v| v.len())),
        ("scan_trades", s.scan_trades("binance", "BTCUSDT", r).map(|v| v.len())),
        ("scan_book_updates", s.scan_book_updates("binance", "BTCUSDT", r).map(|v| v.len())),
        ("scan_depth", s.scan_depth("binance", "BTCUSDT", r).map(|v| v.len())),
        (
            "scan_symbol_properties",
            s.scan_symbol_properties("binance", "BTCUSDT", r).map(|v| v.len()),
        ),
        (
            "properties_as_of",
            s.properties_as_of("binance", "BTCUSDT", T0).map(|p| p.into_iter().count()),
        ),
        ("scan_equity", s.scan_equity("binance", "BTCUSDT", r).map(|v| v.len())),
        ("scan_exec_fills", s.scan_exec_fills("binance", "BTCUSDT").map(|v| v.len())),
        ("scan_exec_orders", s.scan_exec_orders("binance", "BTCUSDT").map(|v| v.len())),
        ("scan_funding", s.scan_funding("binance", "BTC", r).map(|v| v.len())),
        ("scan_chain", s.scan_chain("deribit", "BTC", r).map(|v| v.len())),
        ("scan_cohort", s.scan_cohort("binance", "BTC", r).map(|v| v.len())),
        ("scan_perp_metrics", s.scan_perp_metrics("binance", "BTCUSDT", r).map(|v| v.len())),
        ("series_commits", s.series_commits(s.series()).map(|v| v.len())),
        ("delete_series_checked", s.delete_series_checked(s.series(), None).map(|()| 0)),
    ];
    for (verb, got) in refusals {
        let why = got.expect_err(verb).to_string();
        assert!(!why.is_empty(), "{verb} refuses with a reason");
    }
    let why = s.scan_cohort("binance", "BTC", r).unwrap_err().to_string();
    assert!(why.contains("bars.csv") && why.contains("kind=cohort"), "{why}");
    let why = s.append_bars("binance", "BTCUSDT", "1m", &bar, None).unwrap_err().to_string();
    assert!(why.contains("append_bars") && why.contains("writes nothing"), "{why}");
}

/// A file that breaks a decoder rule, or whose step is not the series' interval, opens nothing.
#[test]
fn a_file_the_decoder_refuses_or_of_another_step_is_refused() {
    let open = |text: &str, interval: &str| {
        let plain = CsvBarsFormat::default();
        HistoryStoreCsv::from_csv(
            "bars.csv",
            text,
            &CsvBarsColumns::Header,
            &plain,
            "binance",
            "BTCUSDT",
            interval,
        )
        .unwrap_err()
        .to_string()
    };
    let why = open(&text(), "5m");
    assert!(why.contains("60000 ms apart") && why.contains("5m"), "{why}");
    let why = open(&text().replace("\n1700000100000,", "\n1700000100000,x"), "1m");
    assert!(why.contains("bars.csv: line 3: number"), "{why}");
    let why = open(&text(), "1w");
    assert!(why.contains("no fixed width"), "{why}");
    let missing = std::path::Path::new("no-such-dir-for-csv-store/none.csv");
    let err = HistoryStoreCsv::open(
        missing,
        &CsvBarsColumns::Header,
        &CsvBarsFormat::default(),
        "binance",
        "BTCUSDT",
        "1m",
    )
    .unwrap_err();
    assert!(matches!(err, HistoryStoreError::Io(_)), "{err}");
}

//! The split warning over a REAL store with the watch installed as the boot installs it: a share series' split-like
//! overnight jump warns once, in the log and in the open list; a covering split row, a 7% gap and a crypto series do
//! not; the list drops a warning once a row covers it; a restarted watch reads the previous day from the store.

use std::path::Path;
use std::sync::Arc;

use vike_marketdata::test_support::{flat_bar_unit_volume, quote};
use vike_model::{Contract, Instrument, StockSplit};
use vike_store::HistoryStoreDataFusion;

use super::*;

/// 00:00 UTC of 2026-11-19 (a Thursday) and of the next day.
fn day(n: i64) -> i64 {
    (vike_utility::calendar::days_from_civil(2026, 11, 19) + n) * MS_PER_DAY
}

const HOUR: i64 = 3_600_000;

/// The kept rows the class is read from: dukascopy's share CFD `AAPL.US/USD` (the store name `AAPL.CFD`) and its FX
/// pair `EURUSD` (`crate::test_support::hold_history_lists`, the rows every suite of this process holds), and a bybit
/// spot pair.
fn hold_rows() {
    crate::test_support::hold_history_lists();
    vike_catalog::hold_kept_list(
        "bybit",
        vec![Instrument::new("bybit", "BTCUSDT", "BTC", "USDT", Contract::CryptoSpot)],
    );
}

/// A settings store with no split row yet, and the watch over it.
fn settings() -> (tempfile::TempDir, Arc<SplitWatch>) {
    let dir = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(dir.path().to_str().expect("utf-8"))).expect("a store");
    let handle = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir.path())),
        vike_secrets::CredentialScope::Full,
    );
    (dir, Arc::new(SplitWatch::new(Some(handle))))
}

/// A store handle at `dir` that shows its writes to `watch`, as the boot's.
fn watched_store(dir: &Path, watch: &Arc<SplitWatch>) -> HistoryStoreDataFusion {
    let store = HistoryStoreDataFusion::open(dir).expect("open");
    assert!(store.watch_prices(watch.price_watch()).is_ok());
    store
}

/// Two 5m bars at `price` from 14:30 UTC of `on`.
fn session(on: i64, price: f64) -> [vike_marketdata::Bar; 2] {
    let open = on + 14 * HOUR + HOUR / 2;
    [flat_bar_unit_volume(open, price), flat_bar_unit_volume(open + 300_000, price)]
}

fn bars(
    store: &HistoryStoreDataFusion,
    venue: &str,
    symbol: &str,
    rows: &[vike_marketdata::Bar],
    key: &str,
) {
    assert!(store.append_bars(venue, symbol, "5m", rows, Some(key)).expect("append") > 0, "{key}");
}

fn split_row(settings: &Path, venue: &str, symbol: &str, date: &str, ratio: f64) {
    let split = StockSplit::new(venue, symbol, date, ratio).expect("a split");
    vike_secrets::stock_split::write_stock_split(settings, &split).expect("row");
}

/// The owner's first case: a share series falls 10:1 overnight with no split row: ONE warning, logged and listed, with
/// the split in words and the command; the rest of that day, and the day stored again under another key, warn nothing.
#[test]
fn a_ten_for_one_overnight_drop_of_a_share_warns_once_in_the_log_and_the_list() {
    hold_rows();
    let (_settings, watch) = settings();
    let dir = tempfile::tempdir().expect("temp");
    let store = watched_store(dir.path(), &watch);
    bars(&store, "dukascopy", "AAPL.CFD", &session(day(0), 1400.0), "d0");
    let ((), events) = vike_log::capture::captured(|| {
        bars(&store, "dukascopy", "AAPL.CFD", &session(day(1), 140.0), "d1");
        let later = [flat_bar_unit_volume(day(1) + 16 * HOUR, 141.0)];
        bars(&store, "dukascopy", "AAPL.CFD", &later, "d1-later");
        bars(&store, "dukascopy", "AAPL.CFD", &session(day(1), 140.0), "d1-again");
    });
    let warned: Vec<&str> = events
        .iter()
        .filter(|e| e.level == tracing::Level::WARN && e.target.ends_with("split_watch"))
        .map(|e| e.message.as_str())
        .collect();
    let line = "AAPL.CFD: looks like a 10-for-1 split on 2026-11-20 (price 1400 → 140); run vike-cli data \
                catalog refresh --venue dukascopy";
    assert_eq!(warned, [line], "one warning, in words");
    let open = watch.open();
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(
        (open[0].new_shares, open[0].old_shares, open[0].date.as_str()),
        (10, 1, "2026-11-20")
    );
    assert_eq!(open[0].message(), line, "the list says what the log says");
}

/// A split row of that series and date (any source) covers the jump: nothing is warned or listed.
#[test]
fn a_split_row_of_that_date_means_no_warning() {
    hold_rows();
    let (settings, watch) = settings();
    split_row(settings.path(), "dukascopy", "AAPL.CFD", "2026-11-20", 10.0);
    let dir = tempfile::tempdir().expect("temp");
    let store = watched_store(dir.path(), &watch);
    bars(&store, "dukascopy", "AAPL.CFD", &session(day(0), 1400.0), "d0");
    let ((), events) = vike_log::capture::captured(|| {
        bars(&store, "dukascopy", "AAPL.CFD", &session(day(1), 140.0), "d1");
    });
    assert!(events.iter().all(|e| !e.target.ends_with("split_watch")), "{events:?}");
    assert!(watch.open().is_empty());
}

/// A 3:2 jump warns as a 3-for-2 split; a 7% overnight gap is no split.
#[test]
fn a_three_for_two_jump_warns_and_a_seven_percent_gap_or_a_31_percent_drop_does_not() {
    hold_rows();
    let (_settings, watch) = settings();
    let dir = tempfile::tempdir().expect("temp");
    let store = watched_store(dir.path(), &watch);
    bars(&store, "dukascopy", "AAPL.CFD", &session(day(0), 150.0), "d0");
    bars(&store, "dukascopy", "AAPL.CFD", &session(day(1), 100.0), "d1");
    bars(&store, "dukascopy", "AAPL.CFD", &session(day(2), 93.0), "d2");
    // Monday: an earnings-sized drop of 31% (93 → 64.17) is no 3-for-2.
    bars(&store, "dukascopy", "AAPL.CFD", &session(day(4), 64.17), "d4");
    let open = watch.open();
    assert_eq!(open.len(), 1, "the 7% gap and the 31% drop are none: {open:?}");
    assert_eq!(open[0].date, "2026-11-20");
    assert_eq!(open[0].split_words(), "3-for-2 split");
}

/// Only a share's series is judged, by its KEPT row: a crypto pair that halves overnight warns nothing, nor does an FX
/// pair.
#[test]
fn a_crypto_series_that_halves_overnight_does_not_warn() {
    hold_rows();
    let rows = vike_catalog::kept_list("bybit").expect("the bybit rows");
    let row =
        vike_catalog::kept_row_of_store_name(&rows, "bybit", "BTCUSDT").expect("its kept row");
    assert!(!row.contract.is_share(), "the row is found, and is no share");
    let (_settings, watch) = settings();
    let dir = tempfile::tempdir().expect("temp");
    let store = watched_store(dir.path(), &watch);
    bars(&store, "bybit", "BTCUSDT", &session(day(0), 90_000.0), "d0");
    bars(&store, "bybit", "BTCUSDT", &session(day(1), 45_000.0), "d1");
    bars(&store, "dukascopy", "EURUSD", &session(day(0), 1.2), "fx0");
    bars(&store, "dukascopy", "EURUSD", &session(day(1), 0.6), "fx1");
    assert!(watch.open().is_empty(), "{:?}", watch.open());
}

/// The Data Manager's read: an open warning is listed until a split row covers it, then dropped, with no restart.
#[test]
fn the_open_list_drops_a_warning_once_a_split_row_covers_it() {
    hold_rows();
    let (settings, watch) = settings();
    let dir = tempfile::tempdir().expect("temp");
    let store = watched_store(dir.path(), &watch);
    bars(&store, "dukascopy", "AAPL.CFD", &session(day(0), 400.0), "d0");
    bars(&store, "dukascopy", "AAPL.CFD", &session(day(1), 100.0), "d1");
    assert_eq!(watch.open().len(), 1, "a 4-for-1 split, listed");
    split_row(settings.path(), "dukascopy", "AAPL.CFD", "2026-11-20", 4.0);
    assert!(watch.open().is_empty(), "the refresh's row ends it: {:?}", watch.open());
}

/// A restarted datahub's watch knows nothing: the day's first write reads the previous stored day from the store
/// (quotes, a day at a time), so a 1-for-10 reverse split across the restart still warns, named as one.
#[test]
fn a_restarted_watch_reads_the_previous_day_from_the_store() {
    hold_rows();
    let dir = tempfile::tempdir().expect("temp");
    {
        let store = HistoryStoreDataFusion::open(dir.path()).expect("open");
        let day0 = [quote(day(0) + 15 * HOUR, 0.99, 1.01), quote(day(0) + 19 * HOUR, 0.99, 1.01)];
        store.append_quotes("dukascopy", "AAPL.CFD", &day0, Some("q0")).expect("day 0");
    }
    let (_settings, watch) = settings();
    let store = watched_store(dir.path(), &watch);
    let monday = [quote(day(4) + 15 * HOUR, 9.9, 10.1)];
    store.append_quotes("dukascopy", "AAPL.CFD", &monday, Some("q4")).expect("day 4");
    let open = watch.open();
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].date, "2026-11-23");
    assert!(
        open[0].message().contains("looks like a 1-for-10 reverse split on 2026-11-23"),
        "{open:?}"
    );
}

/// The ratio rule's edges: within 3% of a split of 2-for-1 or above (or its inverse), within 1% of a 3-for-2 or a
/// 2-for-3, and no further.
#[test]
fn a_split_like_ratio_is_within_its_tolerance_of_a_split() {
    assert_eq!(split_like(1400.0, 140.0), Some((10, 1)));
    assert_eq!(split_like(1.0, 10.0), Some((1, 10)));
    assert_eq!(split_like(2.05, 1.0), Some((2, 1)), "2.5% off a 2-for-1");
    assert_eq!(split_like(2.1, 1.0), None, "5% off");
    assert_eq!(split_like(150.0, 100.0), Some((3, 2)), "an exact 3-for-2");
    assert_eq!(split_like(100.0, 66.67), Some((3, 2)), "a 3-for-2 within rounding");
    assert_eq!(split_like(100.0, 150.5), Some((2, 3)), "0.3% off a 2-for-3");
    assert_eq!(split_like(100.0, 69.0), None, "a 31% drop");
    assert_eq!(split_like(100.0, 68.0), None, "a 32% drop: 2% off 3-for-2, outside its 1%");
    assert_eq!(split_like(100.0, 152.0), None, "1.3% off a 2-for-3");
    assert_eq!(split_like(100.0, 93.0), None, "a 7% gap");
    assert_eq!(split_like(0.0, 1.0), None);
    assert_eq!(split_like(f64::NAN, 1.0), None);
}

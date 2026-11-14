//! `HistoryStoreDataFusion::replace_bars_day`: one UTC day of a bar series replaced whole, every other day
//! untouched, and every key it removed still spent.

use vike_data::{HistoryStore, TimestampRange};
use vike_marketdata::Bar;
use vike_store::HistoryStoreDataFusion;

use crate::common::{DAY, bar, bars_id, bars_series, count_parquets};

const MIN: i64 = 60_000;

fn day_bars(day: i64, minutes: impl Iterator<Item = i64>, close: f64) -> Vec<Bar> {
    minutes.map(|m| bar(day * DAY + m * MIN, close, None)).collect()
}

fn closes(store: &HistoryStoreDataFusion, day: i64) -> Vec<f64> {
    let range = TimestampRange::of(day * DAY, day * DAY + DAY - 1);
    store.load_bars("binance", "BTCUSDT", "1m", range).unwrap().iter().map(|b| b.close).collect()
}

/// Two days under ONE key, as a REST window spanning both writes them, plus a second key on the
/// first day: the replacement of day 1 leaves day 2 exactly as it was, and every old key is spent.
#[test]
fn a_replaced_day_holds_the_new_bars_alone_and_the_next_day_is_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(tmp.path()).unwrap();
    let both: Vec<Bar> = day_bars(1, 0..3, 1.0).into_iter().chain(day_bars(2, 0..3, 2.0)).collect();
    store.append_bars("binance", "BTCUSDT", "1m", &both, Some("rest:window")).unwrap();
    store
        .append_bars("binance", "BTCUSDT", "1m", &day_bars(1, 5..6, 1.0), Some("live:bar"))
        .unwrap();
    assert_eq!(closes(&store, 1).len(), 4);

    let archive = day_bars(1, 0..10, 9.0);
    let written = store
        .replace_bars_day("binance", "BTCUSDT", "1m", "1970-01-02", &archive, "archive:day1")
        .unwrap();

    assert_eq!(written, 10);
    assert_eq!(closes(&store, 1), vec![9.0; 10], "the day is the archive's alone");
    assert_eq!(closes(&store, 2), vec![2.0; 3], "the next day is untouched");
    let keys = store.series_commits(&bars_id()).unwrap();
    for key in ["rest:window", "live:bar", "archive:day1"] {
        assert!(keys.iter().any(|k| k == key), "{key} stays spent: {keys:?}");
    }
    // A repeat of an old writer stores nothing, and so does a repeat of the replacement.
    let again =
        store.append_bars("binance", "BTCUSDT", "1m", &day_bars(1, 5..6, 1.0), Some("live:bar"));
    assert_eq!(again.unwrap(), 0);
    let repeat = store
        .replace_bars_day("binance", "BTCUSDT", "1m", "1970-01-02", &archive, "archive:day1")
        .unwrap();
    assert_eq!(repeat, 0);
    assert_eq!(closes(&store, 1).len(), 10);
    // The removed parts are unlinked: day 2's one part and the new one are all that is on disk.
    assert_eq!(count_parquets(&bars_series(tmp.path())), 2);

    let reopened = HistoryStoreDataFusion::open(tmp.path()).unwrap();
    assert_eq!(closes(&reopened, 1), vec![9.0; 10]);
    assert_eq!(closes(&reopened, 2), vec![2.0; 3]);
}

/// A bar of another date, or none at all, is refused and nothing is written.
#[test]
fn a_bar_off_the_date_or_an_empty_day_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(tmp.path()).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &day_bars(1, 0..3, 1.0), Some("rest")).unwrap();
    let stray = day_bars(2, 0..1, 9.0);
    assert!(store.replace_bars_day("binance", "BTCUSDT", "1m", "1970-01-02", &stray, "a").is_err());
    assert!(store.replace_bars_day("binance", "BTCUSDT", "1m", "1970-01-02", &[], "a").is_err());
    assert_eq!(closes(&store, 1), vec![1.0; 3]);
}

//! `HistoryStoreDataFusion::replace_bars_day` and `replace_quotes_day`: one UTC day of a series replaced
//! whole, every other day untouched, and every key it removed still spent.

use vike_data::{HistoryStore, SeriesId, TimestampRange};
use vike_marketdata::{Bar, TickQuote};
use vike_model::StoreKindId;
use vike_store::HistoryStoreDataFusion;

use crate::common::{DAY, bar, bars_id, bars_series, count_parquets, qt};

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

/// The quote twin: a tick lane's ragged key over the second half of day 1 and the start of day 2. The
/// replacement of day 1 holds the new ticks alone, day 2 keeps its tick, and both keys stay spent.
#[test]
fn a_replaced_quote_day_holds_the_new_ticks_alone_and_the_next_day_is_untouched() {
    const HOUR: i64 = 3_600_000;
    let tmp = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(tmp.path()).unwrap();
    let ragged: Vec<TickQuote> =
        (12..24).map(|h| qt(DAY + h * HOUR, 1.0)).chain([qt(2 * DAY, 2.0)]).collect();
    store.append_quotes("dukascopy", "EURUSD", &ragged, Some("http:ragged")).unwrap();
    let ticks = |day: i64| {
        let range = TimestampRange::of(day * DAY, day * DAY + DAY - 1);
        let q = store.scan_quotes("dukascopy", "EURUSD", range).unwrap();
        q.iter().map(|q| q.bid).collect::<Vec<f64>>()
    };

    let archive: Vec<TickQuote> = (0..24).map(|h| qt(DAY + h * HOUR, 9.0)).collect();
    let written = store
        .replace_quotes_day("dukascopy", "EURUSD", "1970-01-02", &archive, "archive:day1")
        .unwrap();

    assert_eq!(written, 24);
    assert_eq!(ticks(1), vec![9.0; 24], "the day is the archive's alone");
    assert_eq!(ticks(2), vec![2.0], "the next day is untouched");
    let id = SeriesId::per_symbol(StoreKindId::Quote, "dukascopy", "EURUSD", None);
    let keys = store.series_commits(&id).unwrap();
    for key in ["http:ragged", "archive:day1"] {
        assert!(keys.iter().any(|k| k == key), "{key} stays spent: {keys:?}");
    }
    let repeat = store
        .replace_quotes_day("dukascopy", "EURUSD", "1970-01-02", &archive, "archive:day1")
        .unwrap();
    assert_eq!(repeat, 0);
    assert!(store.replace_quotes_day("dukascopy", "EURUSD", "1970-01-02", &[], "b").is_err());
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

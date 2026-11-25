//! The price watch on a real store: every bar and quote append that writes rows is shown once, with its series and
//! rows; a spent commit key writes none and shows nothing; a store with no watch shows nothing.

use std::sync::{Arc, Mutex};

use vike_marketdata::test_support::{flat_bar_unit_volume, quote};

use super::*;

const T0: i64 = 1_700_000_000_000;

/// What the watch saw: `(venue, source, symbol, interval or "quote", rows, first timestamp)`.
type Seen = Arc<Mutex<Vec<(String, Option<String>, String, String, usize, i64)>>>;

fn watched() -> (tempfile::TempDir, HistoryStoreDataFusion, Seen) {
    let dir = tempfile::tempdir().expect("a scratch store");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("open");
    let seen: Seen = Arc::default();
    let into = Arc::clone(&seen);
    let watch: PriceWatch = Arc::new(move |_store: &dyn HistoryStore, write: &PriceWrite<'_>| {
        let (interval, rows, first) = match &write.prices {
            WrittenPrices::Bars { interval, bars } => {
                (interval.to_string(), bars.len(), bars[0].timestamp)
            }
            WrittenPrices::Quotes(quotes) => ("quote".into(), quotes.len(), quotes[0].timestamp),
        };
        into.lock().expect("seen").push((
            write.venue.into(),
            write.source.map(str::to_string),
            write.symbol.into(),
            interval,
            rows,
            first,
        ));
    });
    assert!(store.watch_prices(Arc::clone(&watch)).is_ok(), "the first watch is set");
    assert!(store.watch_prices(watch).is_err(), "a second is refused");
    (dir, store, seen)
}

#[test]
fn every_price_write_with_rows_is_shown_once_and_a_spent_key_is_not() {
    let (_dir, store, seen) = watched();
    let bars = [flat_bar_unit_volume(T0, 10.0), flat_bar_unit_volume(T0 + 60_000, 11.0)];
    assert_eq!(store.append_bars("dukascopy", "AAPL.CFD", "1m", &bars, Some("k1")).unwrap(), 2);
    assert_eq!(store.append_bars("dukascopy", "AAPL.CFD", "1m", &bars, Some("k1")).unwrap(), 0);
    let quotes = [quote(T0, 10.0, 10.2)];
    store.append_quotes("dukascopy", "AAPL.CFD", &quotes, Some("q1")).unwrap();
    store.append_quotes_superseding("dukascopy", "AAPL.CFD", &quotes, Some("q2"), None).unwrap();
    store.append_quotes_sourced("dukascopy", "recorder", "AAPL.CFD", &quotes, None).unwrap();
    store.append_bars_superseding("dukascopy", "AAPL.CFD", "1m", &bars, Some("k2"), None).unwrap();
    let seen = seen.lock().expect("seen").clone();
    let s = |source: Option<&str>, interval: &str, rows| {
        (
            "dukascopy".into(),
            source.map(str::to_string),
            "AAPL.CFD".into(),
            interval.into(),
            rows,
            T0,
        )
    };
    assert_eq!(
        seen,
        [
            s(None, "1m", 2),
            s(None, "quote", 1),
            s(None, "quote", 1),
            s(Some("recorder"), "quote", 1),
            s(None, "1m", 2),
        ],
        "each write that stored rows, once; the spent key `k1` shows nothing"
    );
}

#[test]
fn a_store_with_no_watch_shows_nothing_and_writes_as_before() {
    let dir = tempfile::tempdir().expect("a scratch store");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("open");
    let bars = [flat_bar_unit_volume(T0, 10.0)];
    assert_eq!(store.append_bars("dukascopy", "AAPL.CFD", "1m", &bars, Some("k1")).unwrap(), 1);
    assert_eq!(
        store.load_bars("dukascopy", "AAPL.CFD", "1m", TimestampRange::all()).unwrap(),
        bars
    );
}

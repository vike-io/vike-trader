use super::MemHistStore;
use crate::hist::{BarEdges, HistStore, TsRange};
use crate::series::SeriesId;
use vike_model::Bar;

/// A bar with every field spelled (`Bar` has no `Default`), already in the persisted shape —
/// bid/ask/symbol `None` — so round-trip assertions compare against the helper's own output.
/// The erasure test builds its `Some`-bearing bar explicitly, on top of this one.
fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close - 1.0,
        high: close + 1.0,
        low: close - 2.0,
        close,
        volume: 10.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[test]
fn bars_roundtrip_ts_ascending_after_an_unordered_append() {
    let store = MemHistStore::new();
    let unordered = vec![bar(2_000, 101.0), bar(1_000, 100.0), bar(3_000, 102.0)];
    assert_eq!(store.append_bars("binance", "BTCUSDT", "1m", &unordered, None).unwrap(), 3);
    assert_eq!(
        store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap(),
        vec![bar(1_000, 100.0), bar(2_000, 101.0), bar(3_000, 102.0)],
        "load re-sorts ts-ascending, as the real store's scan does"
    );
}

/// Pins the two claims the `load_bars` doc makes about equal-ts rows, against the refactor
/// that would silently break both: re-keying storage by ts (a `BTreeMap<i64, Bar>` "cleanup")
/// collapses duplicates last-wins, while the real store NEVER dedups by row value (the trait's
/// ingest contract) — `datafusion_multiple_parts_merge_ordered` keeps every overlapping row.
/// Both duplicate-ts bars must survive, in append order (the stable sort's `(ts, 0)` tiebreak).
#[test]
fn duplicate_ts_bars_both_survive_in_append_order() {
    let store = MemHistStore::new();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 1.0)], None).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 2.0), bar(500, 3.0)], None).unwrap();
    let got = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(
        got.iter().map(|b| (b.ts, b.close)).collect::<Vec<_>>(),
        vec![(500, 3.0), (1_000, 1.0), (1_000, 2.0)],
        "no value dedup, and equal-ts rows keep append order across batches"
    );
}

#[test]
fn load_bars_respects_the_inclusive_range_bounds() {
    let store = MemHistStore::new();
    let bars = vec![bar(1_000, 1.0), bar(2_000, 2.0), bar(3_000, 3.0)];
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    let ts_in = |r: TsRange| -> Vec<i64> {
        store.load_bars("binance", "BTCUSDT", "1m", r).unwrap().iter().map(|b| b.ts).collect()
    };
    assert_eq!(ts_in(TsRange::of(1_000, 2_000)), vec![1_000, 2_000], "both ends inclusive");
    assert_eq!(ts_in(TsRange { start: Some(2_000), end: None }), vec![2_000, 3_000]);
    assert_eq!(ts_in(TsRange { start: None, end: Some(1_999) }), vec![1_000]);
    assert!(ts_in(TsRange::of(10_000, 20_000)).is_empty(), "a disjoint range is empty");
}

#[test]
fn bars_are_keyed_by_the_raw_interval_string() {
    let store = MemHistStore::new();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 1.0)], None).unwrap();
    store.append_bars("binance", "BTCUSDT", "1h", &[bar(1_000, 2.0)], None).unwrap();
    let m = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    let h = store.load_bars("binance", "BTCUSDT", "1h", TsRange::all()).unwrap();
    assert_eq!((m.len(), h.len()), (1, 1), "each interval loads only its own series");
    assert_eq!(m[0].close, 1.0);
    assert_eq!(h[0].close, 2.0);
    // "60s" IS one minute semantically, but the raw string is the key — exactly as the real
    // store's `bars_dir` path segment behaves (no interval validation on append or load).
    assert!(store.load_bars("binance", "BTCUSDT", "60s", TsRange::all()).unwrap().is_empty());
}

#[test]
fn bar_series_are_isolated_per_venue_and_symbol() {
    let store = MemHistStore::new();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 1.0)], None).unwrap();
    store.append_bars("bybit", "BTCUSDT", "1m", &[bar(1_000, 2.0)], None).unwrap();
    store.append_bars("binance", "ETHUSDT", "1m", &[bar(1_000, 3.0)], None).unwrap();
    let got = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].close, 1.0, "no cross-talk from the other venue or the other symbol");
}

/// The one contract every pre-existing consumer already leans on (the CLI/datahub suites
/// assert their unseeded stores load no bars) — the control that stays green through the
/// stub-to-real change, proving real storage widened nothing for a store nobody seeded.
#[test]
fn an_empty_store_answers_load_bars_with_an_honest_empty() {
    let store = MemHistStore::new();
    assert!(store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().is_empty());
}

#[test]
fn bars_commit_key_is_idempotent() {
    let store = MemHistStore::new();
    let rows = vec![bar(1_000, 1.0)];
    assert_eq!(store.append_bars("binance", "BTCUSDT", "1m", &rows, Some("k")).unwrap(), 1);
    assert_eq!(store.append_bars("binance", "BTCUSDT", "1m", &rows, Some("k")).unwrap(), 0);
    assert_eq!(store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 1);
}

#[test]
fn an_empty_bar_batch_never_burns_the_commit_key() {
    let store = MemHistStore::new();
    assert_eq!(store.append_bars("binance", "BTCUSDT", "1m", &[], Some("k")).unwrap(), 0);
    // The later REAL append under the same key still lands — `DataFusionHist::commit_rows`
    // checks emptiness before registering the key, and so must the double.
    let rows = vec![bar(1_000, 1.0)];
    assert_eq!(store.append_bars("binance", "BTCUSDT", "1m", &rows, Some("k")).unwrap(), 1);
    assert_eq!(store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 1);
}

#[test]
fn load_bars_erases_bid_ask_symbol_like_the_real_codec() {
    let store = MemHistStore::new();
    let appended = Bar {
        bid: Some(99.5),
        ask: Some(100.5),
        symbol: Some("BTCUSDT.BINANCE".into()),
        funding: Some(0.01),
        ..bar(1_000, 100.0)
    };
    store.append_bars("binance", "BTCUSDT", "1m", &[appended], None).unwrap();
    let got = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 1);
    let b = &got[0];
    assert_eq!(
        (b.bid, b.ask, b.symbol.as_deref()),
        (None, None, None),
        "the three fields the bar schema never persists must not survive the round trip"
    );
    assert_eq!(b.funding, Some(0.01), "funding IS a persisted column and survives");
    assert_eq!(
        (b.ts, b.open, b.high, b.low, b.close, b.volume),
        (1_000, 99.0, 101.0, 98.0, 100.0, 10.0),
        "OHLCV rides through untouched"
    );
}

#[test]
fn seeded_bars_join_the_catalog_with_their_interval() {
    let store = MemHistStore::new();
    store
        .append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 1.0), bar(2_000, 2.0)], None)
        .unwrap();
    assert_eq!(
        store.list_series().unwrap(),
        vec![SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".into()))],
        "bar series carry their interval, like the real store's `interval=` path segment"
    );
    let inv = store.inventory().unwrap();
    assert_eq!(inv.len(), 1, "one held series, one coverage row");
    assert_eq!(inv[0].1.rows, 2, "rows is the held row count");
    assert_eq!(inv[0].1.first_ts, 1_000, "first_ts is the earliest held ts");
    assert_eq!(inv[0].1.last_ts, 2_000, "last_ts is the latest held ts");
    assert_eq!(inv[0].1.bytes, 0, "an in-memory store truly occupies zero bytes on disk");
}

/// `MemHistStore` overrides nothing here, so this pins the TRAIT DEFAULT of `HistStore::bar_edges` —
/// the answer every store that does not override it gets (the flat archive store, the RPC seam, the
/// doubles across the tree): the ends and the size of the very rows `load_bars` returns for the same
/// arguments. Every expectation below is also derived from a real `load_bars` by hand, so the
/// default cannot drift from the read it stands in for without one of the two lines going red.
#[test]
fn the_default_bar_edges_is_what_load_bars_says_about_itself() {
    let store = MemHistStore::new();
    let derived = |r: TsRange| -> BarEdges {
        let bars = store.load_bars("binance", "BTCUSDT", "1m", r).unwrap();
        BarEdges {
            first_ts: bars.first().map(|b| b.ts),
            last_ts: bars.last().map(|b| b.ts),
            rows: bars.len() as u64,
        }
    };
    let edges = |r: TsRange| store.bar_edges("binance", "BTCUSDT", "1m", r).unwrap();

    // An unknown series: an honest empty answer, both ends `None` and no rows.
    assert_eq!(edges(TsRange::all()), BarEdges::default());

    // Unordered on the way in, and 2_000 stored twice.
    store
        .append_bars(
            "binance",
            "BTCUSDT",
            "1m",
            &[bar(3_000, 3.0), bar(1_000, 1.0), bar(2_000, 2.0), bar(2_000, 2.5)],
            None,
        )
        .unwrap();
    let held = BarEdges { first_ts: Some(1_000), last_ts: Some(3_000), rows: 4 };
    assert_eq!(
        edges(TsRange::all()),
        held,
        "the smallest and largest ts, with the duplicate counted"
    );
    assert_eq!(
        edges(TsRange::of(0, 10_000)),
        held,
        "a range WIDER than the data reports the data's ends, not the range's"
    );
    assert_eq!(
        edges(TsRange::of(1_500, 2_000)),
        BarEdges { first_ts: Some(2_000), last_ts: Some(2_000), rows: 2 },
        "inclusive at the top, and both copies of the duplicated ts are rows"
    );
    assert_eq!(
        edges(TsRange { start: Some(2_000), end: None }),
        BarEdges { first_ts: Some(2_000), last_ts: Some(3_000), rows: 3 },
        "an open end is open"
    );
    assert_eq!(edges(TsRange::of(4_000, 5_000)), BarEdges::default(), "no row in range");

    for r in [
        TsRange::all(),
        TsRange::of(0, 10_000),
        TsRange::of(1_500, 2_000),
        TsRange { start: Some(2_000), end: None },
        TsRange { start: None, end: Some(1_999) },
        TsRange::of(4_000, 5_000),
    ] {
        assert_eq!(edges(r), derived(r), "the default IS the load-derived edges for {r:?}");
    }
}

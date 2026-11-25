//! The TICK half of the cache over a REAL store (`vike_store::HistoryStoreDataFusion` in a temp dir, so the stamps are
//! its `series_days` and the reads its scans): quotes, trades and book updates over several days, a gap, a partial
//! today, duplicate timestamps across parts and layouts, two symbols and a `group=` series beside them. A miss and a
//! hit answer exactly what the store answers, a hit reads no store, a changed day is read again alone, and the tick
//! budget is its own.

use std::fmt::Debug;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_data::{BarDay, HistoryStore, HistoryStoreError, TimestampRange};
use vike_datahub_client::proto::{TRADE_COLUMNS, encode_columns_into};
use vike_marketdata::{Bar, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_store::HistoryStoreDataFusion;

use super::CachedHistoryStore;

const VENUE: &str = "binance";
const SYMBOLS: [&str; 2] = ["BTCUSDT", "ETHUSDT"];
/// The first planted day; the store holds `D0..=D0 + 6`, day `D0 + 3` a gap, day `D0 + 6` today (partial).
const D0: i64 = 20_000;
const TODAY: i64 = D0 + 6;

fn at(day: i64, second: i64) -> i64 {
    (D0 + day) * MS_PER_DAY + second * 1_000
}

/// The store, its tick reads counted (each with its range) and its stamp reads counted.
struct Counted {
    _dir: tempfile::TempDir,
    inner: HistoryStoreDataFusion,
    scans: Mutex<Vec<TimestampRange>>,
    stamp_reads: AtomicUsize,
}

impl Counted {
    fn scans(&self) -> usize {
        self.scans.lock().unwrap().len()
    }
}

impl HistoryStore for Counted {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.inner.load_bars(venue, symbol, interval, range)
    }
    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.scans.lock().unwrap().push(range);
        self.inner.scan_quotes(venue, symbol, range)
    }
    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.scans.lock().unwrap().push(range);
        self.inner.scan_trades(venue, symbol, range)
    }
    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.scans.lock().unwrap().push(range);
        self.inner.scan_book_updates(venue, symbol, range)
    }
    fn series_days(
        &self,
        kind: StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: TimestampRange,
    ) -> Result<Option<Vec<BarDay>>, HistoryStoreError> {
        self.stamp_reads.fetch_add(1, Ordering::SeqCst);
        HistoryStore::series_days(&self.inner, kind, venue, source, symbol, interval, range)
    }
    vike_data::hist_store_stubs!(inert: writes, scan_symbol_properties, scan_equity,
        scan_exec_fills, scan_exec_orders);
}

fn trade(symbol: &str, timestamp: i64, i: i64) -> TickTrade {
    TickTrade {
        timestamp,
        local_timestamp: timestamp + 7,
        price: 60_000.0 + (i % 97) as f64 / 8.0,
        size: 0.001 * (1 + i % 13) as f64,
        is_buyer_maker: i % 3 == 0,
        symbol: symbol.into(),
    }
}

fn quote(symbol: &str, timestamp: i64, i: i64) -> TickQuote {
    let bid = 60_000.0 + (i % 89) as f64 / 4.0;
    TickQuote {
        timestamp,
        local_timestamp: 0,
        bid,
        ask: bid + 0.5,
        bid_size: (1 + i % 5) as f64,
        ask_size: (2 + i % 7) as f64,
        symbol: symbol.into(),
    }
}

fn book(timestamp: i64, sequence: u64) -> L2BookUpdate {
    let mid = 60_000.0 + (sequence % 31) as f64;
    L2BookUpdate {
        timestamp,
        local_timestamp: timestamp + 2,
        sequence,
        kind: if sequence.is_multiple_of(10) {
            L2BookUpdateKind::Snapshot
        } else {
            L2BookUpdateKind::Delta
        },
        tick_size: 0.1,
        bids: vec![L2BookLevel::new(mid - 0.5, 1.5), L2BookLevel::new(mid - 1.0, 2.0)],
        asks: vec![L2BookLevel::new(mid + 0.5, 0.75)],
        symbol: "BTCUSDT".to_string(),
    }
}

/// The planted store: every planted day but the gap, each symbol's trades and quotes written as TWO parts a day whose
/// timestamps run in threes (so equal timestamps meet within a part and across parts), a `group=` series holding both
/// symbols on days 1 and 5 at the same timestamps, and BTCUSDT's book on days 1 and 2.
fn planted() -> Counted {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("the store");
    for day in [0, 1, 2, 4, 5, 6] {
        for (s, symbol) in SYMBOLS.iter().enumerate() {
            for half in 0..2_i64 {
                let rows = 0..1_000_i64;
                let ts = |i: i64| at(day, 100 + (i + 1_000 * half) / 3 * 7 + s as i64);
                let trades: Vec<_> = rows.clone().map(|i| trade(symbol, ts(i), i + day)).collect();
                let quotes: Vec<_> = rows.map(|i| quote(symbol, ts(i), i * 3 + day)).collect();
                let key = format!("{symbol}:{day}:{half}");
                store.append_trades(VENUE, symbol, &trades, Some(&key)).expect("trades");
                store.append_quotes(VENUE, symbol, &quotes, Some(&key)).expect("quotes");
            }
        }
    }
    for day in [1, 5] {
        let rows = 0..60_i64;
        let trades: Vec<_> = rows
            .clone()
            .map(|i| trade(SYMBOLS[(i % 2) as usize], at(day, 100 + i / 2 * 7), i))
            .collect();
        let quotes: Vec<_> =
            rows.map(|i| quote(SYMBOLS[(i % 2) as usize], at(day, 100 + i / 2 * 7), i)).collect();
        let key = format!("group:{day}");
        store.append_trades_grouped(VENUE, "majors", &trades, Some(&key)).expect("grouped");
        store.append_quotes_grouped(VENUE, "majors", &quotes, Some(&key)).expect("grouped");
    }
    for day in [1, 2] {
        let events: Vec<_> =
            (0..40_u64).map(|n| book(at(day, 200 + n as i64 * 11), n + 1)).collect();
        store
            .append_book_updates(VENUE, "BTCUSDT", &events, Some(&format!("book:{day}")))
            .expect("book");
    }
    Counted {
        _dir: dir,
        inner: store,
        scans: Mutex::new(Vec::new()),
        stamp_reads: AtomicUsize::new(0),
    }
}

fn cache(store: &std::sync::Arc<Counted>, tick_mib: u32) -> CachedHistoryStore {
    CachedHistoryStore::new(
        store.clone(),
        64,
        tick_mib,
        Box::new(|| TODAY * MS_PER_DAY + 5 * 3_600_000),
    )
}

/// Two answers are the same rows in the same order, every field (`Debug` prints each float exactly).
fn same<T: Debug>(a: &[T], b: &[T]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| format!("{a:?}") == format!("{b:?}"))
}

/// The windows read: inside one day, across whole days over the gap, cut at both ends, the gap alone, unbounded at
/// the start, and one reaching today; with whether a repeat of it is a HIT (no store read).
fn windows() -> [(&'static str, TimestampRange, bool); 6] {
    [
        ("inside one day", TimestampRange::of(at(1, 300), at(1, 900)), true),
        ("whole days over the gap", TimestampRange::of(at(0, 0), at(4, 0) - 1), true),
        ("cut at both ends", TimestampRange::of(at(0, 400), at(5, 700)), true),
        ("the gap alone", TimestampRange::of(at(3, 0), at(4, 0) - 1), false),
        ("unbounded start", TimestampRange { start: None, end: Some(at(2, 500)) }, true),
        ("reaching today", TimestampRange::of(at(4, 0), at(6, 400)), false),
    ]
}

/// One read of every kind and symbol over `range`, through `store`, as debug rows (book updates for BTCUSDT only).
fn read_all(store: &dyn HistoryStore, range: TimestampRange) -> Vec<String> {
    let mut out = Vec::new();
    for symbol in SYMBOLS {
        out.extend(
            store.scan_quotes(VENUE, symbol, range).unwrap().iter().map(|r| format!("{r:?}")),
        );
        out.extend(
            store.scan_trades(VENUE, symbol, range).unwrap().iter().map(|r| format!("{r:?}")),
        );
    }
    out.extend(
        store.scan_book_updates(VENUE, "BTCUSDT", range).unwrap().iter().map(|r| format!("{r:?}")),
    );
    out
}

/// **A miss and a hit answer exactly what the store answers**, for every kind, symbol and window; a repeat of a past
/// window reads no store, the gap and a window reaching today read through and keep nothing of their own, and the
/// encoded answers (`read_columns`) hold the same rows as the owned ones.
#[test]
fn a_tick_miss_and_hit_answer_what_the_store_answers() {
    let store = std::sync::Arc::new(planted());
    for (what, range, hit) in windows() {
        let cached = cache(&store, 64);
        let fresh = read_all(&store.inner, range);
        assert!(!fresh.is_empty() || what == "the gap alone", "{what}: the planted rows are read");
        let missed = read_all(&cached, range);
        assert_eq!(missed, fresh, "{what}: a miss answers the store's rows");
        let before = store.scans();
        let again = read_all(&cached, range);
        assert_eq!(again, fresh, "{what}: a hit answers the store's rows");
        if hit {
            assert_eq!(store.scans(), before, "{what}: a hit reads no store");
            assert!(cached.kept_tick_bytes() > 0, "{what}: days kept");
        } else {
            assert_eq!(store.scans(), before + 5, "{what}: read through, every time");
        }
        assert_eq!(cached.kept_bytes(), 0, "{what}: ticks never fill the bar budget");
        let kept = cached.read_columns(StoreKindId::Trade, VENUE, None, "BTCUSDT", None, range);
        let kept: Vec<TickTrade> = super::decoded(kept.unwrap().unwrap()).unwrap();
        let owned = store.inner.scan_trades(VENUE, "BTCUSDT", range).unwrap();
        assert!(same(&kept, &owned), "{what}: the encoded answer's rows");
    }
}

/// A day written again after it was kept misses ALONE: the read fetches that day and nothing else, and answers the
/// store's new rows; the other days stay hits.
#[test]
fn a_changed_tick_day_is_read_again_alone() {
    let store = std::sync::Arc::new(planted());
    let cached = cache(&store, 64);
    let range = TimestampRange::of(at(0, 0), at(3, 0) - 1);
    let first = cached.scan_trades(VENUE, "BTCUSDT", range).unwrap();
    assert!(same(&first, &store.inner.scan_trades(VENUE, "BTCUSDT", range).unwrap()));
    let late = [trade("BTCUSDT", at(1, 150), 5), trade("BTCUSDT", at(1, 150), 6)];
    store.inner.append_trades(VENUE, "BTCUSDT", &late, Some("late")).unwrap();
    let before = store.scans();
    let again = cached.scan_trades(VENUE, "BTCUSDT", range).unwrap();
    assert!(
        same(&again, &store.inner.scan_trades(VENUE, "BTCUSDT", range).unwrap()),
        "the new rows"
    );
    assert_eq!(again.len(), first.len() + 2);
    let scans = store.scans.lock().unwrap()[before..].to_vec();
    assert_eq!(
        scans,
        vec![TimestampRange::of(at(1, 0), at(2, 0) - 1)],
        "day 1 alone is read again"
    );
}

/// The tick budget is its own and holds: a small one never passes its cap (the least recently used day leaving
/// first) and every answer is still the store's; a zero budget asks no stamp and keeps nothing.
#[test]
fn the_tick_budget_holds_and_zero_keeps_nothing() {
    let store = std::sync::Arc::new(planted());
    let tight = CachedHistoryStore::new(
        store.clone(),
        0,
        1,
        Box::new(|| TODAY * MS_PER_DAY + 5 * 3_600_000),
    );
    let range = TimestampRange::of(at(0, 0), at(6, 0) - 1);
    let mut scans = Vec::new();
    for _ in 0..3 {
        assert_eq!(read_all(&tight, range), read_all(&store.inner, range));
        assert!(tight.kept_tick_bytes() <= 1 << 20, "{} kept", tight.kept_tick_bytes());
        assert!(tight.kept_tick_bytes() > 0, "the budget keeps what fits");
        scans.push(store.scans());
    }
    // About 3.6 MB of planted ticks against 1 MiB: each pass evicts days the next one reads again.
    assert!(scans[2] > scans[1], "a repeat over more than the budget reads the store: {scans:?}");
    let off = CachedHistoryStore::new(store.clone(), 64, 0, Box::new(|| TODAY * MS_PER_DAY));
    let stamps = store.stamp_reads.load(Ordering::SeqCst);
    assert_eq!(read_all(&off, range), read_all(&store.inner, range));
    assert_eq!(store.stamp_reads.load(Ordering::SeqCst), stamps, "a zero budget asks no stamp");
    assert_eq!(off.kept_tick_bytes(), 0);
}

/// **A kept tick day longer than one body is cut right inside a body and across a body's edges**: two days of a trade
/// every two seconds in the real store, `2 × CHUNK_ROWS + 1_000` a day (three bodies), read whole once (so kept), then
/// windows cut inside one body, across one edge, across both and across the two days: each a hit that answers the
/// store's trades, and whose kept columns splice to the body those trades encode to.
#[test]
fn a_long_kept_tick_day_is_cut_inside_a_body_and_across_its_edges() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let inner = HistoryStoreDataFusion::open(dir.path()).expect("the store");
    let (per_day, edge) = (2 * super::CHUNK_ROWS as i64 + 1_000, super::CHUNK_ROWS as i64);
    let ts = |day: i64, row: i64| at(day, 0) + row * 2_000;
    for day in 0..2 {
        let trades: Vec<_> = (0..per_day).map(|i| trade("BTCUSDT", ts(day, i), i)).collect();
        inner.append_trades(VENUE, "BTCUSDT", &trades, Some(&format!("long:{day}"))).unwrap();
    }
    let store = std::sync::Arc::new(Counted {
        _dir: dir,
        inner,
        scans: Mutex::new(Vec::new()),
        stamp_reads: AtomicUsize::new(0),
    });
    let cached = cache(&store, 64);
    cached.scan_trades(VENUE, "BTCUSDT", TimestampRange::of(ts(0, 0), at(2, 0) - 1)).unwrap();
    for (what, range) in [
        ("inside one body", TimestampRange::of(ts(0, 100), ts(0, edge - 100))),
        ("across one edge", TimestampRange::of(ts(0, edge - 50), ts(0, edge + 50))),
        ("across both edges", TimestampRange::of(ts(0, 10), ts(0, 2 * edge + 500))),
        ("across the days", TimestampRange::of(ts(0, edge + 7), ts(1, edge - 7))),
    ] {
        let before = store.scans();
        let got = cached.scan_trades(VENUE, "BTCUSDT", range).unwrap();
        let kept = cached.read_columns(StoreKindId::Trade, VENUE, None, "BTCUSDT", None, range);
        assert_eq!(store.scans(), before, "{what}: a hit");
        let fresh = store.inner.scan_trades(VENUE, "BTCUSDT", range).unwrap();
        assert!(!fresh.is_empty() && same(&got, &fresh), "{what}: the store's trades");
        let mut encoded = Vec::new();
        encode_columns_into(&mut encoded, &TRADE_COLUMNS, &fresh);
        let kept = super::bar_cache_tests::spliced::<TickTrade>(&kept.unwrap().unwrap());
        assert!(kept == encoded, "{what}: the kept columns");
    }
}

/// The answers the relay reads by `read_columns` are cut from the kept days' bodies: a window cut mid-day at both
/// ends is several runs, and their rows decoded are the owned answer's.
#[test]
fn an_encoded_tick_hit_is_runs_of_the_kept_days() {
    let store = std::sync::Arc::new(planted());
    let cached = cache(&store, 64);
    let range = TimestampRange::of(at(0, 400), at(5, 700));
    cached.scan_quotes(VENUE, "ETHUSDT", range).unwrap();
    let kept = cached.read_columns(StoreKindId::Quote, VENUE, None, "ETHUSDT", None, range);
    let kept = kept.unwrap().unwrap();
    assert_eq!(kept.runs().count(), 5, "one run per planted day of the window");
    let owned = store.inner.scan_quotes(VENUE, "ETHUSDT", range).unwrap();
    assert_eq!(kept.len(), owned.len());
    assert!(same(&super::decoded::<TickQuote>(kept).unwrap(), &owned));
}

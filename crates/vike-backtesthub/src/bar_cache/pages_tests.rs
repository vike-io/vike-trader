//! A cold day kept from the store's OWN columns (`HistoryStore::read_columns`: the datahub's pages, as its handle hands
//! them over) against the rows path it replaces: the days it cuts are the bodies the rows encode to, byte for byte and
//! byte for byte counted, however the pages fall (a day edge inside a page, rows sharing the edge's timestamps, a page
//! of one row, a cut inside a body); a cache over pages answers and keeps exactly what a cache over rows does, reads
//! no row, holds its budget and keeps single flight; a page the cut cannot lean on (a time outside its piece or out of
//! order, a time at `i64::MAX`, a layout no splice copies) is refused with nothing kept, and nothing loops.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use vike_datahub_client::proto::columns_rows;
use vike_marketdata::{L2BookLevel, L2BookUpdateKind};

use super::*;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
/// The day the cache's clock is in: every stored day is past.
const TODAY: i64 = 100;

/// `n` trades of `day`: timestamps in threes from its midnight (so its first three rows share the midnight), the last
/// five at its last millisecond.
fn day_of_trades(day: i64, n: usize) -> Vec<TickTrade> {
    (0..n)
        .map(|i| {
            let at = if i + 5 >= n { MS_PER_DAY - 1 } else { (i / 3) as i64 * 1_000 };
            TickTrade {
                timestamp: day * MS_PER_DAY + at,
                local_timestamp: day * MS_PER_DAY + at + 3,
                price: 60_000.0 + (i % 97) as f64 / 8.0,
                size: 0.001 * (1 + i % 13) as f64,
                is_buyer_maker: i.is_multiple_of(3),
                symbol: SYMBOL.into(),
            }
        })
        .collect()
}

/// Trades of `days` (day, rows), ascending.
fn trades(days: &[(i64, usize)]) -> Vec<TickTrade> {
    days.iter().flat_map(|&(day, n)| day_of_trades(day, n)).collect()
}

/// `n` book events of `day`, `levels` a side, two to a timestamp, the last two at its last millisecond.
fn day_of_books(day: i64, n: usize, levels: usize) -> Vec<L2BookUpdate> {
    (0..n)
        .map(|i| {
            let at = if i + 2 >= n { MS_PER_DAY - 1 } else { (i / 2) as i64 * 5_000 };
            let side = |sign: f64| -> Vec<L2BookLevel> {
                (0..levels + i % 3)
                    .map(|k| L2BookLevel::new(60_000.0 + sign * k as f64, 1.5))
                    .collect()
            };
            L2BookUpdate {
                timestamp: day * MS_PER_DAY + at,
                local_timestamp: day * MS_PER_DAY + at + 2,
                sequence: i as u64 + 1,
                kind: if i.is_multiple_of(9) {
                    L2BookUpdateKind::Snapshot
                } else {
                    L2BookUpdateKind::Delta
                },
                tick_size: 0.5,
                bids: side(-1.0),
                asks: side(1.0),
                symbol: SYMBOL.to_string(),
            }
        })
        .collect()
}

/// `rows` as the pages a store hands over: consecutive pages of `page` rows, each a body of exactly them, or, with
/// `padded`, each a body holding two rows more on either side, cut to its own.
fn pages<R: DayRow>(rows: &[R], page: usize, padded: bool) -> SharedColumns {
    let mut out = SharedColumns::default();
    for at in (0..rows.len()).step_by(page) {
        let end = (at + page).min(rows.len());
        let (from, to) =
            if padded { (at.saturating_sub(2), (end + 2).min(rows.len())) } else { (at, end) };
        out.push(Arc::new(body_of(&R::MAGIC, &rows[from..to])), at - from..end - from);
    }
    out
}

/// What a cut kept: each day, its rows, its bodies' bytes and the bytes the budget counts of them.
fn kept(days: &[(i64, DayColumns)]) -> Vec<(i64, usize, Vec<Vec<u8>>, usize)> {
    days.iter()
        .map(|(day, columns)| {
            let bodies = columns.bodies.iter().map(|body| body.to_vec()).collect();
            (*day, columns.rows, bodies, columns.bytes())
        })
        .collect()
}

/// `rows`' days spliced from pages of every size in `sizes` (whole bodies and cut ones) are the days [`encode_days`]
/// makes of them, byte for byte and counted the same.
fn splices_as_it_encodes<R: DayRow + Clone>(what: &str, rows: &[R], sizes: &[usize]) {
    let encoded = kept(&encode_days(rows.to_vec()));
    for &page in sizes {
        for padded in [false, true] {
            let spliced = splice_days::<R>(pages(rows, page, padded), TimestampRange::all());
            let spliced = spliced.expect("a splice");
            assert!(kept(&spliced) == encoded, "{what}: pages of {page}, padded {padded}");
        }
    }
}

/// **The days cut from pages are the days the rows encode to, byte for byte**: trades over a gap, days of more than
/// two bodies, pages of one row, of a body less one, more one, a page holding several days and one holding all; book
/// events (their levels); bars. The cut lands EXACTLY on the first row at or past midnight: rows sharing the last
/// millisecond stay in their day, and rows sharing the midnight open the next.
#[test]
fn spliced_days_are_the_encoded_days_byte_for_byte() {
    let rows = trades(&[(0, 20_000), (1, 2 * CHUNK_ROWS + 7), (3, 11)]);
    let sizes = [1, 7, CHUNK_ROWS - 1, CHUNK_ROWS + 5, 30_000, rows.len()];
    splices_as_it_encodes("trades", &rows, &sizes);
    let books: Vec<_> = [0, 1, 2].iter().flat_map(|&day| day_of_books(day, 300, 3)).collect();
    splices_as_it_encodes("books", &books, &[1, 7, 299, 300, 301, books.len()]);
    let bars: Vec<Bar> = (0..3 * 1_440)
        .map(|m| Bar::ohlcv(m * 60_000, 1.0 + m as f64, 2.0, 0.5, 1.5, 3.0))
        .collect();
    splices_as_it_encodes("bars", &bars, &[1, 100, 1_440, bars.len()]);

    let spliced = splice_days::<TickTrade>(pages(&rows, 30_000, false), TimestampRange::all());
    let spliced = spliced.expect("a splice");
    let days: Vec<(i64, usize)> = spliced.iter().map(|(day, d)| (*day, d.rows)).collect();
    assert_eq!(days, [(0, 20_000), (1, 2 * CHUNK_ROWS + 7), (3, 11)], "every row in its day");
    let (last, next) = (&spliced[0].1, &spliced[1].1);
    assert_eq!(last.time(last.rows - 5), MS_PER_DAY - 1, "the last millisecond's five rows stay");
    assert_eq!(
        (next.time(0), next.time(2), next.time(3)),
        (MS_PER_DAY, MS_PER_DAY, MS_PER_DAY + 1_000)
    );
}

/// A store of trades and book events that hands its reads over as PAGES of `page` rows (`read_columns`, the datahub's
/// handle standing in), or answers rows only with `page` 0 (a datahub without the columns); every read is counted.
#[derive(Default)]
struct Paged {
    trades: Vec<TickTrade>,
    books: Vec<L2BookUpdate>,
    page: usize,
    /// A page handed over after the good ones, with its cut: a malformed one.
    bad: Mutex<Option<(Vec<u8>, Range<usize>)>>,
    /// A columns read waits until this many stamp reads were answered (every reader past its stamps), then a little.
    hold: AtomicUsize,
    column_reads: AtomicUsize,
    row_reads: AtomicUsize,
    stamp_reads: AtomicUsize,
}

fn within<T: DayRow>(rows: &[T], range: TimestampRange) -> &[T] {
    let from = rows.partition_point(|row| range.start.is_some_and(|s| row.timestamp() < s));
    let to = rows.partition_point(|row| range.end.is_none_or(|e| row.timestamp() <= e));
    &rows[from..to.max(from)]
}

/// Every day of `range`'s whole days that holds a row, stamped once and settled.
fn stamped<T: DayRow>(rows: &[T], range: TimestampRange) -> Vec<BarDay> {
    let whole = TimestampRange {
        start: range.start.map(|s| s.div_euclid(MS_PER_DAY) * MS_PER_DAY),
        end: range.end.map(|e| (e.div_euclid(MS_PER_DAY) + 1) * MS_PER_DAY - 1),
    };
    let mut days: Vec<BarDay> = Vec::new();
    for row in within(rows, whole) {
        let day = row.timestamp().div_euclid(MS_PER_DAY);
        if days.last().is_none_or(|last| last.day != day) {
            days.push(BarDay { day, stamp: 1, provisional: false });
        }
    }
    days
}

impl Paged {
    fn pages_of<T: DayRow>(&self, rows: &[T], range: TimestampRange) -> SharedColumns {
        let mut out = pages(within(rows, range), self.page, false);
        if let Some((body, cut)) = self.bad.lock().unwrap().clone() {
            out.push(Arc::new(body), cut);
        }
        out
    }
}

impl HistoryStore for Paged {
    fn load_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        Ok(Vec::new())
    }
    fn scan_quotes(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        Ok(Vec::new())
    }
    fn scan_trades(
        &self,
        _venue: &str,
        _symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.row_reads.fetch_add(1, Ordering::SeqCst);
        Ok(within(&self.trades, range).to_vec())
    }
    fn scan_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.row_reads.fetch_add(1, Ordering::SeqCst);
        Ok(within(&self.books, range).to_vec())
    }
    fn series_days(
        &self,
        kind: StoreKindId,
        _venue: &str,
        _source: Option<&str>,
        _symbol: &str,
        _interval: Option<&str>,
        range: TimestampRange,
    ) -> Result<Option<Vec<BarDay>>, HistoryStoreError> {
        self.stamp_reads.fetch_add(1, Ordering::SeqCst);
        Ok(Some(match kind {
            StoreKindId::Trade => stamped(&self.trades, range),
            StoreKindId::Book => stamped(&self.books, range),
            other => return Err(HistoryStoreError::Query(format!("no read of {other}"))),
        }))
    }
    fn read_columns(
        &self,
        kind: StoreKindId,
        _venue: &str,
        _source: Option<&str>,
        _symbol: &str,
        _interval: Option<&str>,
        range: TimestampRange,
    ) -> Option<Result<SharedColumns, HistoryStoreError>> {
        if self.page == 0 {
            return None;
        }
        let hold = self.hold.load(Ordering::SeqCst);
        if hold > 0 {
            for _ in 0..2_000 {
                if self.stamp_reads.load(Ordering::SeqCst) >= hold {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.column_reads.fetch_add(1, Ordering::SeqCst);
        Some(Ok(match kind {
            StoreKindId::Trade => self.pages_of(&self.trades, range),
            StoreKindId::Book => self.pages_of(&self.books, range),
            _ => return None,
        }))
    }
    vike_data::hist_store_stubs!(inert: writes, scan_symbol_properties, scan_equity,
        scan_exec_fills, scan_exec_orders);
}

/// The tick cache over `store`, `tick_mib` of budget, its clock past every stored day.
fn cache(store: &Arc<Paged>, tick_mib: u32) -> CachedHistoryStore {
    CachedHistoryStore::new(store.clone(), 0, tick_mib, Box::new(|| TODAY * MS_PER_DAY))
}

/// Trades over days 0 to 4 but day 2, each day more than one body.
fn paged(page: usize) -> Arc<Paged> {
    let days = [(0, 20_000), (1, 17_000), (3, 18_000), (4, 300)];
    Arc::new(Paged { trades: trades(&days), page, ..Paged::default() })
}

/// A read's trades as one body: the answer, every field bit for bit.
fn read_trades(store: &dyn HistoryStore, range: TimestampRange) -> Vec<u8> {
    body_of(&TRADE_COLUMNS, &store.scan_trades(VENUE, SYMBOL, range).expect("a read"))
}

/// The kept bodies a read of `range` is answered from (`read_columns` of the cache), each with its cut.
fn kept_runs(cached: &CachedHistoryStore, range: TimestampRange) -> Vec<(Vec<u8>, Range<usize>)> {
    let read = cached.read_columns(StoreKindId::Trade, VENUE, None, SYMBOL, None, range);
    read.expect("kept").expect("read").runs().map(|(body, cut)| (body.to_vec(), cut)).collect()
}

/// **A cache over pages answers and keeps exactly what a cache over rows does, and reads no row**: windows inside a day,
/// across the gap, cut at both ends and over every day; the misses and the hits answer the store's rows, the kept bodies
/// (and their counted bytes) are the rows path's, a hit reads no store, and the pages path asks one columns read a piece.
/// A store that answers rows only is read for them, as before.
#[test]
fn a_cache_over_pages_answers_and_keeps_what_one_over_rows_does() {
    let at = |day: i64, second: i64| day * MS_PER_DAY + second * 1_000;
    let windows = [
        TimestampRange::of(at(1, 600), at(1, 3_000)),
        TimestampRange::of(at(0, 0), at(5, 0) - 1),
        TimestampRange::of(at(0, 1_234), at(4, 50)),
        TimestampRange { start: None, end: Some(at(3, 900)) },
    ];
    for range in windows {
        let (by_rows, by_pages) = (paged(0), paged(5_000));
        let (over_rows, over_pages) = (cache(&by_rows, 64), cache(&by_pages, 64));
        let fresh = read_trades(by_rows.as_ref(), range);
        for pass in ["miss", "hit"] {
            assert!(read_trades(&over_pages, range) == fresh, "{range:?} {pass}: the store's rows");
            assert!(read_trades(&over_rows, range) == fresh, "{range:?} {pass}: over rows too");
        }
        assert!(kept_runs(&over_pages, range) == kept_runs(&over_rows, range), "{range:?}: kept");
        assert_eq!(over_pages.kept_tick_bytes(), over_rows.kept_tick_bytes(), "{range:?}: counted");
        assert!(over_pages.kept_tick_bytes() > 0);
        let pieces = stamped(&by_pages.trades, range).len().min(COLD_READS_IN_FLIGHT);
        assert_eq!(by_pages.column_reads.load(Ordering::SeqCst), pieces, "{range:?}: one a piece");
        assert_eq!(by_pages.row_reads.load(Ordering::SeqCst), 0, "{range:?}: no row read");
        assert_eq!(by_rows.row_reads.load(Ordering::SeqCst), 1 + pieces, "{range:?}: rows only");
    }
}

/// **The budget holds over pages**: at 1 MiB (one trade day of ~0.6 MB fits, two do not) the kept bytes never pass it
/// and are the rows path's after every read, and a day larger than the whole budget is answered and never kept.
#[test]
fn the_budget_holds_over_pages_and_a_day_over_it_is_never_kept() {
    let days = [(0, 20_000), (1, 17_000), (3, 18_000), (6, 40_000)];
    let store = |page| Arc::new(Paged { trades: trades(&days), page, ..Paged::default() });
    let (by_rows, by_pages) = (store(0), store(5_000));
    let (over_rows, over_pages) = (cache(&by_rows, 1), cache(&by_pages, 1));
    let one_day = |day: i64| TimestampRange::of(day * MS_PER_DAY, (day + 1) * MS_PER_DAY - 1);
    for day in [0, 1, 3, 0, 6, 6] {
        assert!(
            read_trades(&over_pages, one_day(day)) == read_trades(by_rows.as_ref(), one_day(day))
        );
        read_trades(&over_rows, one_day(day));
        assert!(
            over_pages.kept_tick_bytes() <= 1 << 20,
            "day {day}: {}",
            over_pages.kept_tick_bytes()
        );
        assert_eq!(over_pages.kept_tick_bytes(), over_rows.kept_tick_bytes(), "day {day}");
    }
    let before = by_pages.column_reads.load(Ordering::SeqCst);
    read_trades(&over_pages, one_day(6));
    assert_eq!(
        by_pages.column_reads.load(Ordering::SeqCst),
        before + 1,
        "the big day is read again"
    );
    assert!(over_pages.kept_tick_bytes() > 0, "a day that fits is still kept");
}

/// **Single flight over pages**: eight reads of the same cold window at once ask the store's columns once a piece,
/// and every one answers the store's rows.
#[test]
fn eight_reads_at_once_ask_the_pages_once() {
    let store = paged(5_000);
    let range = TimestampRange::of(0, 5 * MS_PER_DAY - 1);
    let fresh = read_trades(store.as_ref(), range);
    store.hold.store(8, Ordering::SeqCst);
    let cached = cache(&store, 64);
    std::thread::scope(|scope| {
        let reads: Vec<_> = (0..8).map(|_| scope.spawn(|| read_trades(&cached, range))).collect();
        for read in reads {
            assert!(read.join().expect("a reader") == fresh);
        }
    });
    assert_eq!(
        store.column_reads.load(Ordering::SeqCst),
        COLD_READS_IN_FLIGHT,
        "one a piece, for all"
    );
}

/// One trade of the planted shape at `timestamp`.
fn trade_at(timestamp: i64) -> TickTrade {
    TickTrade { timestamp, local_timestamp: timestamp, ..day_of_trades(0, 1)[0].clone() }
}

/// **A time near the end of `i64` is cut by DAYS and never loops**: rows on the last day `i64` can hold (its midnight,
/// times just under `i64::MAX` and `i64::MAX` itself) and the day before it are cut into those two days, the bodies the
/// rows encode to, where a next midnight would overflow; and a piece whose page carries a row at `i64::MAX`, outside the
/// span it asked, is refused with nothing kept.
#[test]
fn a_time_near_the_end_of_i64_is_cut_by_days_and_never_loops() {
    let last = i64::MAX.div_euclid(MS_PER_DAY);
    let times =
        [last * MS_PER_DAY - 1, last * MS_PER_DAY, last * MS_PER_DAY, i64::MAX - 1, i64::MAX];
    let rows: Vec<TickTrade> = times.map(trade_at).into();
    let span = TimestampRange::of((last - 1) * MS_PER_DAY, i64::MAX);
    for page in [1, 2, rows.len()] {
        let spliced = splice_days::<TickTrade>(pages(&rows, page, false), span).expect("a splice");
        assert!(kept(&spliced) == kept(&encode_days(rows.clone())), "pages of {page}");
        let days: Vec<(i64, usize)> = spliced.iter().map(|(day, d)| (*day, d.rows)).collect();
        assert_eq!(days, [(last - 1, 1), (last, 4)], "pages of {page}: each row in its day");
    }
    let store = Arc::new(Paged { trades: day_of_trades(0, 2_000), page: 500, ..Paged::default() });
    *store.bad.lock().unwrap() = Some((body_of(&TRADE_COLUMNS, &[trade_at(i64::MAX)]), 0..1));
    refuses_and_keeps_nothing(&store, StoreKindId::Trade, "a row at i64::MAX");
}

/// **A page the cut cannot lean on is refused and nothing is kept**: a row outside the piece's span or before the row
/// ahead of it, a cut past a body's rows, a head stating `u32::MAX` rows, a body cut short, a flag bit a trade has no use
/// for and a quote body on a trade read. Each refuses the read, keeps nothing, leaves no place held and no flight
/// waiting; once the store hands good pages, the next read answers and keeps. (What only a decode of each row could
/// refuse, a run of no symbol, a book kind no event has, a run's text past one frame, is the store's to refuse where
/// the page arrives: `vike-datahub-client`'s `tests/remote_tick_columns.rs` and `proto/bar_columns_tests.rs`.)
#[test]
fn a_page_the_cut_cannot_lean_on_is_refused_and_nothing_is_kept() {
    // The bad page goes after the good ones, its rows at the planted day's last millisecond: in order, inside the span.
    let at_the_end = vec![trade_at(MS_PER_DAY - 1); 50];
    let body = body_of(&TRADE_COLUMNS, &at_the_end);
    let mut every_row = body.clone();
    every_row[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut stray = body.clone();
    stray[8 + 8 * 4 * at_the_end.len()] |= 0b10;
    let quote = TickQuote {
        timestamp: MS_PER_DAY - 1,
        local_timestamp: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: SYMBOL.into(),
    };
    let malformed: [(&str, Vec<u8>, Range<usize>); 7] = [
        ("a row past the span", body_of(&TRADE_COLUMNS, &[trade_at(MS_PER_DAY)]), 0..1),
        ("a row before the one ahead", body_of(&TRADE_COLUMNS, &[trade_at(0)]), 0..1),
        ("a cut past its rows", body.clone(), 0..51),
        ("a head stating u32::MAX rows", every_row, 0..50),
        ("cut short", body[..body.len() - 3].to_vec(), 0..50),
        ("a stray flag bit", stray, 0..50),
        ("a quote body", body_of(&QUOTE_COLUMNS, &[quote]), 0..1),
    ];
    for (what, bad, cut) in malformed {
        let store =
            Arc::new(Paged { trades: day_of_trades(0, 2_000), page: 500, ..Paged::default() });
        *store.bad.lock().unwrap() = Some((bad, cut));
        refuses_and_keeps_nothing(&store, StoreKindId::Trade, what);
    }
    assert_eq!(columns_rows(&body), Some(50));
}

/// One read of day 0 through a cache over `store`, whose pages hold a bad one: refused, nothing kept, no place held, no
/// flight left; then, the bad page gone, the read answers and keeps.
fn refuses_and_keeps_nothing(store: &Arc<Paged>, kind: StoreKindId, what: &str) {
    let range = TimestampRange::of(0, MS_PER_DAY - 1);
    let read = |cached: &CachedHistoryStore| match kind {
        StoreKindId::Book => cached.scan_book_updates(VENUE, SYMBOL, range).map(|rows| rows.len()),
        _ => cached.scan_trades(VENUE, SYMBOL, range).map(|rows| rows.len()),
    };
    let cached = cache(store, 64);
    let error = read(&cached).expect_err(what).to_string();
    assert!(error.contains("the store's columns are refused"), "{what}: {error}");
    assert_eq!(cached.kept_tick_bytes(), 0, "{what}: nothing kept");
    assert_eq!(*cached.cold.open.lock().unwrap(), 0, "{what}: every place given back");
    assert!(cached.ticks.kept.lock().unwrap().flying.is_empty(), "{what}: no flight left");
    *store.bad.lock().unwrap() = None;
    let rows = match kind {
        StoreKindId::Book => store.books.len(),
        _ => store.trades.len(),
    };
    assert_eq!(read(&cached).expect("the good pages"), rows, "{what}: then answered");
    assert!(cached.kept_tick_bytes() > 0, "{what}: and kept");
}

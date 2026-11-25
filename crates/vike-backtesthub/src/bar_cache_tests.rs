use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use super::*;

/// A store whose days carry a version the test bumps on every write to that day, standing in for the
/// datahub's stamps, and which records every bar read it answers.
#[derive(Default)]
struct StampedStore {
    bars: Mutex<Vec<Bar>>,
    versions: Mutex<BTreeMap<i64, u64>>,
    provisional: Mutex<BTreeSet<i64>>,
    /// The bar reads, in the order they answered, and the name of the thread each ran on.
    reads: Mutex<Vec<TimestampRange>>,
    threads: Mutex<Vec<Option<String>>>,
    /// How many `series_days` calls were answered.
    stamp_reads: AtomicUsize,
    /// A bar read waits until this many `series_days` calls were answered (so every concurrent reader is past
    /// its stamps), then a little longer; 0 = no wait.
    hold: AtomicUsize,
    /// A bar read fails with this message.
    fail: Mutex<Option<String>>,
    /// A bar read whose range holds this day's midnight fails, naming the day.
    fail_day: Mutex<Option<i64>>,
    /// A bar read from day 0's midnight waits until this many bar reads finished (0 = no wait).
    first_waits_until: AtomicUsize,
    /// Every bar read takes at least this many milliseconds.
    linger_ms: AtomicU64,
    /// Bar reads finished, inside the store now, and the most inside it at once.
    done: AtomicUsize,
    in_flight: AtomicUsize,
    most_in_flight: AtomicUsize,
    /// A day the stamps list for every range though it holds no bar of it: a garbled stamp.
    phantom: Mutex<Option<i64>>,
}

impl StampedStore {
    /// Store one bar a minute over `minutes` of `day`, each closing at `close`, as one write of that day.
    fn put(&self, day: i64, minutes: i64, close: f64) {
        let mut bars = self.bars.lock().unwrap();
        for m in 0..minutes {
            let ts = day * MS_PER_DAY + m * 60_000;
            bars.retain(|b| b.timestamp != ts);
            bars.push(Bar::ohlcv(ts, close, close, close, close, 1.0));
        }
        bars.sort_by_key(|b| b.timestamp);
        *self.versions.lock().unwrap().entry(day).or_default() += 1;
    }

    fn reads(&self) -> usize {
        self.reads.lock().unwrap().len()
    }

    /// One bar read: held, delayed and failed as the test set, recorded as it answers.
    fn answer(&self, range: TimestampRange) -> Result<Vec<Bar>, HistoryStoreError> {
        std::thread::sleep(Duration::from_millis(self.linger_ms.load(Ordering::SeqCst)));
        let hold = self.hold.load(Ordering::SeqCst);
        if hold > 0 {
            for _ in 0..2_000 {
                if self.stamp_reads.load(Ordering::SeqCst) >= hold {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            // ...and time for every reader past its stamps to reach the flight board.
            std::thread::sleep(Duration::from_millis(100));
        }
        let until = self.first_waits_until.load(Ordering::SeqCst);
        if until > 0 && range.start == Some(0) {
            for _ in 0..2_000 {
                if self.done.load(Ordering::SeqCst) >= until {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        self.reads.lock().unwrap().push(range);
        self.threads.lock().unwrap().push(std::thread::current().name().map(str::to_string));
        if let Some(why) = self.fail.lock().unwrap().clone() {
            return Err(HistoryStoreError::Query(why));
        }
        if let Some(day) = *self.fail_day.lock().unwrap()
            && within(range, day * MS_PER_DAY)
        {
            return Err(HistoryStoreError::Query(format!("day {day} went away")));
        }
        let bars = self.bars.lock().unwrap();
        Ok(bars.iter().filter(|b| within(range, b.timestamp)).cloned().collect())
    }
}

fn within(range: TimestampRange, ts: i64) -> bool {
    range.start.is_none_or(|s| ts >= s) && range.end.is_none_or(|e| ts <= e)
}

impl HistoryStore for StampedStore {
    fn load_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_in_flight.fetch_max(now, Ordering::SeqCst);
        let answer = self.answer(range);
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.done.fetch_add(1, Ordering::SeqCst);
        answer
    }

    fn series_days(
        &self,
        _kind: StoreKindId,
        _venue: &str,
        _source: Option<&str>,
        _symbol: &str,
        _interval: Option<&str>,
        range: TimestampRange,
    ) -> Result<Option<Vec<BarDay>>, HistoryStoreError> {
        self.stamp_reads.fetch_add(1, Ordering::SeqCst);
        let bars = self.bars.lock().unwrap();
        let mut days: BTreeSet<i64> = bars
            .iter()
            .filter(|b| within(range, b.timestamp))
            .map(|b| b.timestamp.div_euclid(MS_PER_DAY))
            .collect();
        days.extend(*self.phantom.lock().unwrap());
        let versions = self.versions.lock().unwrap();
        let provisional = self.provisional.lock().unwrap();
        Ok(Some(
            days.into_iter()
                .map(|day| BarDay {
                    day,
                    stamp: versions.get(&day).copied().unwrap_or(1),
                    provisional: provisional.contains(&day),
                })
                .collect(),
        ))
    }

    /// The lane read, budgeted as `HistoryStoreDatahub` budgets it: `Some(0)` answers EMPTY, and `Some(n)` the
    /// first `n` rows.
    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        _source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        let mut bars = self.load_bars(venue, symbol, interval, range)?;
        if let Some(n) = budget {
            bars.truncate(n);
        }
        Ok(bars)
    }

    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// The day the tests' clock is in: every stored day below it is past.
const TODAY: i64 = 100;

fn cache(inner: &Arc<StampedStore>, cap_mib: u32) -> CachedHistoryStore {
    CachedHistoryStore::new(inner.clone(), cap_mib, 0, Box::new(|| TODAY * MS_PER_DAY + 3_600_000))
}

fn read(store: &dyn HistoryStore, range: TimestampRange) -> Vec<Bar> {
    store.load_bars("dukascopy", "SPX500.CFD", "1m", range).unwrap()
}

/// From midday of day 0 to the early hours of day 2: a range that cuts its first and last day.
fn ragged() -> TimestampRange {
    TimestampRange::of(MS_PER_DAY / 2, 2 * MS_PER_DAY + 6 * 3_600_000)
}

/// The store reads one cold read of [`ragged`]'s three days makes: one per piece.
const RAGGED_PIECES: usize = if COLD_READS_IN_FLIGHT < 3 { COLD_READS_IN_FLIGHT } else { 3 };

/// Two identical reads reach the store once (one read per piece of the cold days), and both answer exactly what the
/// store answers itself, row for row, for a range that cuts its first and last day.
#[test]
fn two_identical_reads_hit_the_datahub_once() {
    let inner = Arc::new(StampedStore::default());
    for day in 0..3 {
        inner.put(day, 1_440, 1.0 + day as f64);
    }
    let fresh = read(inner.as_ref(), ragged());
    let cached = cache(&inner, 64);
    let reads_before = inner.reads();
    assert_eq!(read(&cached, ragged()), fresh);
    assert_eq!(read(&cached, ragged()), fresh);
    assert_eq!(inner.reads() - reads_before, RAGGED_PIECES, "the first read's pieces, for both");
    assert!(cached.kept_bytes() > 0);
}

/// A bar landing on a LATER day than the range leaves every day of the range valid: still a hit.
#[test]
fn an_append_after_the_range_keeps_it_a_hit() {
    let inner = Arc::new(StampedStore::default());
    for day in 0..3 {
        inner.put(day, 1_440, 1.0);
    }
    let cached = cache(&inner, 64);
    let first = read(&cached, ragged());
    inner.put(3, 60, 9.0);
    inner.put(10, 60, 9.0);
    let reads_before = inner.reads();
    assert_eq!(read(&cached, ragged()), first);
    assert_eq!(inner.reads(), reads_before, "the later days are not the range's");
}

/// A write to one day INSIDE the range makes that day (and only it) miss, and the answer is the new one.
#[test]
fn a_changed_day_inside_the_range_misses() {
    let inner = Arc::new(StampedStore::default());
    for day in 0..3 {
        inner.put(day, 1_440, 1.0);
    }
    let cached = cache(&inner, 64);
    read(&cached, ragged());
    inner.put(1, 30, 7.0);
    let reads_before = inner.reads();
    let again = read(&cached, ragged());
    assert_eq!(again, read(inner.as_ref(), ragged()), "the changed day is read again");
    let reads = inner.reads.lock().unwrap();
    assert_eq!(reads.len() - reads_before, 2, "one cache miss and the fresh read above");
    assert_eq!(
        reads[reads_before],
        TimestampRange::of(MS_PER_DAY, 2 * MS_PER_DAY - 1),
        "only day 1 is read again, whole"
    );
}

/// A provisional last day is never served from the cache: each read reaches the store and answers what
/// it holds now; and a range reaching today's open day reads through too.
#[test]
fn a_provisional_last_day_is_not_served_stale() {
    let inner = Arc::new(StampedStore::default());
    for day in 0..3 {
        inner.put(day, 1_440, 1.0);
    }
    inner.provisional.lock().unwrap().insert(2);
    let cached = cache(&inner, 64);
    read(&cached, ragged());
    inner.put(2, 60, 5.0);
    let reads_before = inner.reads();
    assert_eq!(read(&cached, ragged()), read(inner.as_ref(), ragged()), "the settled rows");
    assert_eq!(inner.reads() - reads_before, 2, "read through, then the fresh read above");
    assert_eq!(cached.kept_bytes(), 0, "nothing of a provisional range is kept");

    inner.put(TODAY, 10, 1.0);
    let open = TimestampRange::of(TODAY * MS_PER_DAY, (TODAY + 1) * MS_PER_DAY - 1);
    let reads_before = inner.reads();
    read(&cached, open);
    read(&cached, open);
    assert_eq!(inner.reads() - reads_before, 2, "the open day is never kept");
}

/// The kept bytes never pass the cap, each day counted as its ENCODED body's length, and the least recently used day
/// is the one that left.
#[test]
fn eviction_respects_the_cap() {
    let inner = Arc::new(StampedStore::default());
    let days = 12;
    for day in 0..days {
        inner.put(day, 1_440, 1.0);
    }
    let cap = 1 << 20;
    let day_bytes = vike_datahub_client::proto::columns_len(&inner.bars.lock().unwrap()[..1_440]);
    assert!(days as usize * day_bytes > cap, "the days do not all fit");
    let cached = cache(&inner, 1);
    let one_day = |day: i64| TimestampRange::of(day * MS_PER_DAY, (day + 1) * MS_PER_DAY - 1);
    for day in 0..days {
        read(&cached, one_day(day));
        assert!(cached.kept_bytes() <= cap, "day {day}: {} kept", cached.kept_bytes());
        let held = (day as usize + 1).min(cap / day_bytes);
        assert_eq!(cached.kept_bytes(), held * day_bytes, "day {day}: its body's bytes, counted");
    }
    let reads_before = inner.reads();
    read(&cached, one_day(days - 1));
    assert_eq!(inner.reads(), reads_before, "the newest day is held");
    read(&cached, one_day(0));
    assert_eq!(inner.reads(), reads_before + 1, "the oldest day was evicted");
}

/// **A day whose body is larger than the whole budget is answered and never kept**: twenty thousand bars of one day
/// (about 1.5 MB of columns) against 1 MiB are read from the store every time, and a small day beside them is kept.
#[test]
fn a_day_over_the_budget_is_answered_and_never_kept() {
    let inner = Arc::new(StampedStore::default());
    inner.put(0, 1_440, 1.0);
    let big: Vec<Bar> =
        (0..20_000).map(|i| Bar::ohlcv(MS_PER_DAY + i * 4_000, 2.0, 2.0, 2.0, 2.0, 1.0)).collect();
    assert!(vike_datahub_client::proto::columns_len(&big) > 1 << 20, "guard: over the budget");
    inner.bars.lock().unwrap().extend(big.iter().cloned());
    inner.versions.lock().unwrap().insert(1, 1);
    let cached = cache(&inner, 1);
    let day = |d: i64| TimestampRange::of(d * MS_PER_DAY, (d + 1) * MS_PER_DAY - 1);
    let before = inner.reads();
    for _ in 0..2 {
        assert_eq!(read(&cached, day(1)), big);
    }
    assert_eq!(inner.reads(), before + 2, "the big day is read every time");
    assert_eq!(cached.kept_bytes(), 0, "the big day is never kept");
    read(&cached, day(0));
    assert!(cached.kept_bytes() > 0, "a day that fits is kept");
}

/// An answer's runs spliced into ONE body: what the relay's parts of it join to.
pub(super) fn spliced<R: DayRow>(answer: &SharedColumns) -> Vec<u8> {
    let runs: Vec<_> = answer.runs().collect();
    let mut body = Vec::new();
    splice_columns_into::<R>(&mut body, &R::MAGIC, &runs).expect("kept bodies splice");
    body
}

/// **A kept day longer than one body is cut right inside a body and across a body's edges**: two days of a bar every
/// two seconds, `2 × CHUNK_ROWS + 1_000` a day (three bodies), read whole once (so kept), then windows cut inside one
/// body, across one edge, across both and across the two days: each a hit that answers the store's bars, and whose
/// kept columns splice to the body those bars encode to.
#[test]
fn a_long_kept_day_is_cut_inside_a_body_and_across_its_edges() {
    let inner = Arc::new(StampedStore::default());
    let (per_day, edge) = (2 * CHUNK_ROWS as i64 + 1_000, CHUNK_ROWS as i64);
    let at = |day: i64, row: i64| day * MS_PER_DAY + row * 2_000;
    for day in 0..2 {
        let bars = (0..per_day).map(|i| Bar::ohlcv(at(day, i), i as f64, 2.0, 0.5, 1.5, 3.0));
        inner.bars.lock().unwrap().extend(bars);
        inner.versions.lock().unwrap().insert(day, 1);
    }
    let cached = cache(&inner, 64);
    read(&cached, TimestampRange::of(at(0, 0), at(2, 0) - 1));
    let (venue, symbol) = ("dukascopy", "SPX500.CFD");
    for (what, range) in [
        ("inside one body", TimestampRange::of(at(0, 100), at(0, edge - 100))),
        ("across one edge", TimestampRange::of(at(0, edge - 50), at(0, edge + 50))),
        ("across both edges", TimestampRange::of(at(0, 10), at(0, 2 * edge + 500))),
        ("across the days", TimestampRange::of(at(0, edge + 7), at(1, edge - 7))),
    ] {
        let before = inner.reads();
        let got = read(&cached, range);
        let kept = cached.read_columns(StoreKindId::Bar, venue, None, symbol, Some("1m"), range);
        assert_eq!(inner.reads(), before, "{what}: a hit");
        let fresh = read(inner.as_ref(), range);
        assert!(!fresh.is_empty() && got == fresh, "{what}: the store's bars");
        let mut encoded = Vec::new();
        encode_columns_into(&mut encoded, &BAR_COLUMNS, &fresh);
        assert!(spliced::<Bar>(&kept.unwrap().unwrap()) == encoded, "{what}: the kept columns");
    }
}

/// **A kept day's time is its body's FIRST word**, for every kind the cache keeps (what `DayColumns::time` reads).
#[test]
fn every_kept_kind_carries_its_time_first() {
    fn first_word<T: DayRow>(row: T) {
        let body = body_of(&T::MAGIC, std::slice::from_ref(&row));
        assert_eq!(columns_word(&body, 0, 0), Some(row.timestamp() as u64), "{}", T::MAGIC.name);
    }
    let ts = 1_700_000_123_456;
    first_word(Bar::ohlcv(ts, 1.0, 2.0, 0.5, 1.5, 3.0));
    let symbol = "BTCUSDT".to_string();
    first_word(TickQuote {
        timestamp: ts,
        local_timestamp: ts + 1,
        bid: 1.0,
        ask: 2.0,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: symbol.as_str().into(),
    });
    first_word(TickTrade {
        timestamp: ts,
        local_timestamp: ts + 1,
        price: 1.0,
        size: 2.0,
        is_buyer_maker: true,
        symbol: symbol.as_str().into(),
    });
    first_word(L2BookUpdate {
        timestamp: ts,
        local_timestamp: ts + 1,
        sequence: 7,
        kind: vike_marketdata::L2BookUpdateKind::Delta,
        tick_size: 0.1,
        bids: vec![vike_marketdata::L2BookLevel::new(1.0, 2.0)],
        asks: Vec::new(),
        symbol,
    });
}

/// `readers` reads of `range` at once, each on its own thread, through one cache.
fn read_together(
    cached: &CachedHistoryStore,
    readers: usize,
    range: TimestampRange,
) -> Vec<Result<Vec<Bar>, HistoryStoreError>> {
    std::thread::scope(|scope| {
        let reads: Vec<_> = (0..readers)
            .map(|_| scope.spawn(|| cached.load_bars("dukascopy", "SPX500.CFD", "1m", range)))
            .collect();
        reads.into_iter().map(|read| read.join().expect("a reader thread")).collect()
    })
}

/// SINGLE FLIGHT: eight reads of the same past range at once reach the store once, and every one of them
/// answers exactly the rows the store answers itself.
#[test]
fn concurrent_identical_reads_hit_the_datahub_once() {
    let inner = Arc::new(StampedStore::default());
    for day in 0..3 {
        inner.put(day, 1_440, 1.0 + day as f64);
    }
    let fresh = read(inner.as_ref(), ragged());
    let readers = 8;
    inner.hold.store(readers, Ordering::SeqCst);
    let cached = cache(&inner, 64);
    let reads_before = inner.reads();
    for answer in read_together(&cached, readers, ragged()) {
        assert_eq!(answer.expect("every reader answers"), fresh);
    }
    assert_eq!(inner.reads() - reads_before, RAGGED_PIECES, "one read's pieces for all eight");
}

/// A fetch that fails fails every read waiting on it with the same error, and nothing of it is kept: the
/// next read asks the store again and answers its rows.
#[test]
fn a_failing_read_fails_every_waiter_and_keeps_nothing() {
    let inner = Arc::new(StampedStore::default());
    for day in 0..3 {
        inner.put(day, 1_440, 1.0);
    }
    let fresh = read(inner.as_ref(), ragged());
    let readers = 4;
    inner.hold.store(readers, Ordering::SeqCst);
    *inner.fail.lock().unwrap() = Some("the datahub went away".to_string());
    let cached = cache(&inner, 64);
    let reads_before = inner.reads();
    for answer in read_together(&cached, readers, ragged()) {
        let error = answer.expect_err("every reader fails").to_string();
        assert_eq!(error, "hist query: the datahub went away", "the leader's own error");
    }
    assert_eq!(
        inner.reads() - reads_before,
        RAGGED_PIECES,
        "one read's failed pieces for all four"
    );
    assert_eq!(cached.kept_bytes(), 0, "a failure is not kept");

    *inner.fail.lock().unwrap() = None;
    inner.hold.store(0, Ordering::SeqCst);
    assert_eq!(read(&cached, ragged()), fresh);
    assert_eq!(
        inner.reads() - reads_before,
        2 * RAGGED_PIECES,
        "the next read asked the store again"
    );
}

/// `days` days of one bar a minute, each closing at a price of its own.
fn days_of_minutes(days: i64) -> Arc<StampedStore> {
    let inner = Arc::new(StampedStore::default());
    for day in 0..days {
        inner.put(day, 1_440, 1.0 + day as f64);
    }
    inner
}

/// From midday of day 0 to the early hours of `last`.
fn ragged_to(last: i64) -> TimestampRange {
    TimestampRange::of(MS_PER_DAY / 2, last * MS_PER_DAY + 6 * 3_600_000)
}

/// **Cold pieces landing OUT OF ORDER are answered in day order**: nine cold days are read as
/// [`COLD_READS_IN_FLIGHT`] pieces at once, the first of them held until every other one answered, and the read
/// answers the store's own rows, row for row; the pieces' spans join into the run's one span of whole days, and a
/// repeat reads no store.
#[test]
fn cold_pieces_landing_out_of_order_answer_in_day_order() {
    let inner = days_of_minutes(9);
    let fresh = read(inner.as_ref(), ragged_to(8));
    let cached = cache(&inner, 64);
    let reads_before = inner.reads();
    let others = COLD_READS_IN_FLIGHT - 1;
    inner.first_waits_until.store(inner.done.load(Ordering::SeqCst) + others, Ordering::SeqCst);
    assert_eq!(read(&cached, ragged_to(8)), fresh, "the store's rows, in day order");
    let reads = inner.reads.lock().unwrap()[reads_before..].to_vec();
    assert_eq!(reads.len(), COLD_READS_IN_FLIGHT, "one store read per piece");
    assert_eq!(reads.last().unwrap().start, Some(0), "the first piece answered last: {reads:?}");
    let mut names: Vec<Option<String>> = inner.threads.lock().unwrap()[reads_before..].to_vec();
    names.sort();
    let named: Vec<Option<String>> =
        (0..COLD_READS_IN_FLIGHT).map(|n| Some(format!("bt-cold-read-{n}"))).collect();
    assert_eq!(names, named, "each piece on a thread of its own name");
    let mut spans = reads.clone();
    spans.sort_by_key(|span| span.start);
    assert_eq!(spans[0].start, Some(0), "from day 0's midnight");
    assert_eq!(spans[spans.len() - 1].end, Some(9 * MS_PER_DAY - 1), "to day 8's last millisecond");
    for pair in spans.windows(2) {
        assert_eq!(pair[0].end.map(|end| end + 1), pair[1].start, "the spans join: {spans:?}");
    }
    assert_eq!(read(&cached, ragged_to(8)), fresh);
    assert_eq!(inner.reads(), reads_before + COLD_READS_IN_FLIGHT, "a repeat reads no store");
}

/// **A piece failing MID-WINDOW refuses the read in the store's words and frees every place**: four reads of nine cold
/// days at once all fail with the error of the piece holding day 4; afterwards no place is held and no flight is
/// left on the board, the days of the piece before it are kept and the failed piece's and later ones are not, so the
/// next read fetches only those and answers the store's rows.
#[test]
fn a_failed_piece_mid_window_refuses_and_frees_every_place() {
    let inner = days_of_minutes(9);
    let range = ragged_to(8);
    let fresh = read(inner.as_ref(), range);
    let readers = 4;
    inner.hold.store(readers, Ordering::SeqCst);
    *inner.fail_day.lock().unwrap() = Some(4);
    let cached = cache(&inner, 64);
    for answer in read_together(&cached, readers, range) {
        let error = answer.expect_err("every reader is refused").to_string();
        assert_eq!(error, "hist query: day 4 went away", "the failed piece's own words");
    }
    assert_eq!(*cached.cold.open.lock().unwrap(), 0, "every place given back");
    assert!(cached.bars.kept.lock().unwrap().flying.is_empty(), "no flight left waiting");

    *inner.fail_day.lock().unwrap() = None;
    inner.hold.store(0, Ordering::SeqCst);
    let reads_before = inner.reads();
    assert_eq!(read(&cached, range), fresh);
    let listed: Vec<BarDay> =
        (0..9).map(|day| BarDay { day, stamp: 1, provisional: false }).collect();
    let failed_from = pieces(&listed, COLD_READS_IN_FLIGHT)
        .into_iter()
        .rfind(|(span, _)| span.start <= Some(4 * MS_PER_DAY))
        .map(|(span, _)| span.start)
        .expect("a piece holds day 4");
    let reads = inner.reads.lock().unwrap()[reads_before..].to_vec();
    assert!(!reads.is_empty(), "the failed days are read again");
    for span in &reads {
        assert!(span.start >= failed_from, "a kept day read again: {reads:?}");
    }
}

/// **The bound holds across reads**: four reads of ten cold days each, of four symbols at once, want twelve pieces,
/// and at most [`COLD_READS_IN_FLIGHT`] of them are in the store together (and that many are); every answer is the
/// store's, and every place is given back.
#[test]
fn cold_reads_never_pass_the_bound() {
    let inner = days_of_minutes(10);
    let range = ragged_to(9);
    let fresh = read(inner.as_ref(), range);
    inner.most_in_flight.store(0, Ordering::SeqCst);
    inner.linger_ms.store(40, Ordering::SeqCst);
    let cached = cache(&inner, 64);
    std::thread::scope(|scope| {
        let cached = &cached;
        let reads: Vec<_> = ["A", "B", "C", "D"]
            .into_iter()
            .map(|symbol| scope.spawn(move || cached.load_bars("dukascopy", symbol, "1m", range)))
            .collect();
        for answer in reads {
            assert_eq!(answer.join().expect("a reader thread").expect("an answer"), fresh);
        }
    });
    assert_eq!(
        inner.most_in_flight.load(Ordering::SeqCst),
        COLD_READS_IN_FLIGHT,
        "the bound, reached"
    );
    assert_eq!(
        inner.reads(),
        1 + 4 * COLD_READS_IN_FLIGHT,
        "each read's pieces, and the fresh read"
    );
    assert_eq!(*cached.cold.open.lock().unwrap(), 0, "every place given back");
}

/// [`pieces`] cuts a run as evenly as its days allow, never into more pieces than days, and its spans join into the
/// run's one span over days no stamp listed (a weekend between two listed days).
#[test]
fn pieces_cover_the_run_exactly() {
    let day = |day| BarDay { day, stamp: 1, provisional: false };
    let run: Vec<BarDay> = [3, 4, 7, 8, 9, 10, 11].map(day).into();
    let cut = pieces(&run, 3);
    let lengths: Vec<usize> = cut.iter().map(|(_, days)| days.len()).collect();
    assert_eq!(lengths, [2, 2, 3]);
    let spans: Vec<TimestampRange> = cut.iter().map(|(span, _)| *span).collect();
    assert_eq!(
        spans,
        [
            TimestampRange::of(3 * MS_PER_DAY, 7 * MS_PER_DAY - 1),
            TimestampRange::of(7 * MS_PER_DAY, 9 * MS_PER_DAY - 1),
            TimestampRange::of(9 * MS_PER_DAY, 12 * MS_PER_DAY - 1),
        ]
    );
    assert_eq!(pieces(&run[..1], 3).len(), 1, "one day is one piece");
    assert_eq!(
        pieces(&run, 1),
        [(TimestampRange::of(3 * MS_PER_DAY, 12 * MS_PER_DAY - 1), &run[..])]
    );
}

/// **A stamp naming the last day `i64` holds is read without wrapping**: the stamps list that day beside the range's
/// own (a garbled stamp: no past range reaches it, so only a wrong stamp names it), and the cold read's pieces span to
/// it: the last piece reads to `i64::MAX` exactly, no span is inverted, and the read answers the store's rows and keeps
/// its days. A stamp naming the day after it, whose midnight `i64` cannot hold, reads through and keeps nothing.
#[test]
fn a_stamp_on_the_last_day_i64_holds_is_read_without_wrapping() {
    let last = i64::MAX.div_euclid(MS_PER_DAY);
    let range = TimestampRange::of(0, 2 * MS_PER_DAY - 1);
    let inner = days_of_minutes(2);
    let fresh = read(inner.as_ref(), range);
    *inner.phantom.lock().unwrap() = Some(last);
    let cached = cache(&inner, 64);
    let reads_before = inner.reads();
    assert_eq!(read(&cached, range), fresh, "the store's rows");
    let reads = inner.reads.lock().unwrap()[reads_before..].to_vec();
    assert!(reads.iter().all(|span| span.start <= span.end), "no span wrapped: {reads:?}");
    let whole_last_day = TimestampRange::of(last * MS_PER_DAY, i64::MAX);
    assert!(reads.contains(&whole_last_day), "the last day read whole: {reads:?}");
    assert!(cached.kept_bytes() > 0, "the range's days are kept");

    *inner.phantom.lock().unwrap() = Some(last + 1);
    let cached = cache(&inner, 64);
    let reads_before = inner.reads();
    assert_eq!(read(&cached, range), fresh, "read through");
    assert_eq!(inner.reads.lock().unwrap()[reads_before..], [range], "the read's own range, once");
    assert_eq!(cached.kept_bytes(), 0, "nothing kept");
}

/// A lane read with ANY budget goes to the store and is kept nowhere: `Some(0)` answers empty there (as
/// `HistoryStoreDatahub` answers it), and a later unbudgeted read of the same lane still answers the real bars,
/// never the empty days a zero budget would have planted under their real stamps.
#[test]
fn a_zero_budget_read_keeps_nothing_and_the_next_read_has_the_bars() {
    let inner = Arc::new(StampedStore::default());
    for day in 0..3 {
        inner.put(day, 1_440, 1.0 + day as f64);
    }
    let lane = |store: &dyn HistoryStore, budget| {
        store.load_bars_sourced_capped("dukascopy", "user-x", "SPX500.CFD", "1m", ragged(), budget)
    };
    let fresh = lane(inner.as_ref(), None).unwrap();
    assert!(!fresh.is_empty());
    let cached = cache(&inner, 64);
    assert!(lane(&cached, Some(0)).unwrap().is_empty(), "the store's own answer to a zero budget");
    assert_eq!(lane(&cached, Some(5)).unwrap().len(), 5, "a budget is the store's head, uncached");
    assert_eq!(cached.kept_bytes(), 0, "no budgeted read is kept");
    assert_eq!(lane(&cached, None).unwrap(), fresh, "the real bars");
    assert_eq!(lane(&cached, None).unwrap(), fresh, "...and again, from the cache");
}

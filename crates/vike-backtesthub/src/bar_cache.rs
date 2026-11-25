//! The backtesthub's BAR and TICK cache: [`CachedHistoryStore`], the `HistoryStore` in front of this daemon's one
//! datahub handle (`backtest_cli/serve.rs`'s `run_serve`), keeping the rows of past UTC days a run read, so a
//! repeated run, or the next weekly chunk of one, reads them from memory instead of the datahub. It lives in
//! the PARENT: the sandboxed strategy child reads through the parent's relay, so it is served from here too.
//!
//! - **What is kept**: bars (`load_bars`, an unbudgeted `load_bars_sourced_capped`) and the three tick reads a tick
//!   replay makes (`scan_quotes`, `scan_trades`, `scan_book_updates`), and `read_columns` of each, one machine for all
//!   of them over a row type ([`DayRow`]). Every other verb is forwarded (`forward.rs`).
//! - **Kept ENCODED** ([`DayColumns`]): a day is the plain columns body the datahub's codec writes for its read
//!   (`vike_datahub_client::proto`, [`DayRow::MAGIC`]), cut into bodies of [`CHUNK_ROWS`] rows: 33 bytes a trade
//!   where the rows took 96, so a budget holds about three times the days. An owned read decodes its rows from them;
//!   the sandboxed child's relay asks them as they are (`HistoryStore::read_columns`) and splices its parts from them
//!   (`vike_datahub_client::proto::splice_columns_into`), so a kept day crosses the pipe with no row decoded or
//!   encoded. A miss keeps the store's OWN columns where it holds them (`HistoryStore::read_columns`: the datahub's
//!   handle, its pages as they came, LZ4 ones decompressed, each checked there as its decode would check it:
//!   `vike_datahub_client::proto::check_columns`) and cut into days and bodies by splicing, only its time column read,
//!   every time inside the piece and in order ([`splice_days`]); from a store that answers only rows (a datahub without
//!   the columns), it encodes the rows once.
//!   A read the cache keeps nothing of answers them as the store gave them, encoded only for the relay.
//! - **Two budgets, one per family** (`config.backtest_bar_cache_mib`, `config.backtest_tick_cache_mib`): a tick
//!   day is hundreds of megabytes where a 1m bar day is a few hundred kilobytes, so one shared budget would let a
//!   single tick window evict every bar day the bar runs re-read. Quotes, trades and book updates share the tick
//!   budget, the least recently used day of any of them leaving first.
//! - **Valid per DAY, by the store's own stamp** (`HistoryStore::series_days`, the datahub's `SeriesDays`, of the
//!   read's kind: a tick read's is stamped over every layout that read reads): every
//!   read first asks the stamps of the days its range reads, and a kept day is served only while its stamp is the
//!   one it was kept under. A row landing on a later day changes no earlier day's stamp, so a past range stays a hit
//!   while the datahub appends; a changed day is read again, alone.
//! - **Read through, keeping nothing**, when the range ends at or after today's UTC midnight (an open day),
//!   when a day of it is provisional, when the store cannot say (`None`: a datahub without the verb), when the
//!   stamps cannot be read (the read itself then meets the same failure and answers it), when the range
//!   lists no day (an empty answer, or the read's own refusal, comes from the store), when a listed day's midnight
//!   does not fit `i64` (a stamp no piece can read whole), or when the family's budget is 0.
//! - **Single flight**: a day one read is fetching (under the same stamp) is not fetched again by a read that
//!   needs it meanwhile; that read waits for the first one's rows, or for its error, which it answers too.
//!   Nothing failed is kept. One lock decides every day of a read at once (held, joined or fetched by it), so
//!   a read only ever waits on reads that decided before it, and the waits cannot form a cycle.
//! - **The same answer as a fresh read.** A kept day is the whole day's rows as the store answered them; a
//!   read answers each listed day's rows in day order, cut to its range. A row's time lives in one UTC day's
//!   parts and the store answers in time order (whole timestamps on every page), so a day's rows do not depend on
//!   its neighbours, and the stamps are read BEFORE the rows, so a write landing in between makes the next read
//!   miss rather than keep a stale day. Every field of a kept row crosses the codec bit for bit, as it crosses the
//!   datahub's own columns reply: the one rule is the codec's, a non-finite OPTIONAL float of a bar reads back
//!   absent, which is how the daemon's datahub handle reads it in the first place.
//! - **Bounded**: at most its budget of ENCODED bytes per family, the least recently used day leaving first; a day
//!   larger than the whole budget is never kept. Rows are encoded a body at a time from the tail, each body's rows
//!   freed as soon as it is made, and the store's pages are spliced a day at a time from the head, each page dropped
//!   once its last row is spliced, so neither a miss nor a read kept nothing of holds a read's rows (or its pages) and
//!   their bodies whole at once.
//! - **Served without a copy** to a reader that asks `HistoryStore::read_columns` (the sandboxed child's relay): the
//!   answer holds each kept body's `Arc` cut to the range, so a day evicted meanwhile lives until that answer is
//!   dropped, and no lock is held while it is read. The owned reads decode the same runs once, into a `Vec` of
//!   exactly their length.
//!
//! - **Cold days are read several at once** ([`COLD_READS_IN_FLIGHT`]): a run of days the cache neither holds nor
//!   sees another read fetching is cut into at most that many pieces of consecutive days, each one store read of its
//!   whole days, all in flight together; their spans join into the one span a single read of the run would read, so
//!   the rows are the same. Each piece is kept and answered in day order whichever lands first, and a failed piece
//!   fails the read (and every flight it leads) with its own error. The bound is the whole cache's, across reads and
//!   families: a piece waits for a place before it asks the store and gives it back when its read returns, a failure or
//!   a panic included. Memory: the pieces hold the run's rows (or pages) a single read of it held, each piece only its
//!   own days, and each piece cuts its days once its read returned (its place given back first), so a cold window
//!   holds no more than a single read of it held, and keeps its bodies (a 7-day BTCUSDT.P trade window is ~1 GB at 33 B
//!   a trade, inside the 4 GiB tick budget; three months of `1m` bars are ~10 MB against the 8 GiB bar one).
//!
//! A range is cut into day-sized runs: the days the cache holds or another read is fetching, and the runs of the rest,
//! each read in pieces as above.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::Range;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use vike_data::{BarDay, HistoryStore, HistoryStoreError, SharedColumns, TimestampRange};
use vike_datahub_client::HistoryStoreDatahub;
use vike_datahub_client::proto::{
    BAR_COLUMNS, BOOK_UPDATE_COLUMNS, ColumnRow, ColumnsMagic, QUOTE_COLUMNS,
    RANGE_READS_PER_CLIENT, TRADE_COLUMNS, columns_word, decode_columns_into, encode_columns_into,
    splice_columns_into,
};
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::StoreKindId;
use vike_model::{Clock, MS_PER_DAY};

mod forward;

/// Rows per body a kept day is cut into ([`DayColumns`]): a relay part (`strategy_sandbox/wire.rs`'s
/// `TICKS_PER_FRAME`) spans at most two bodies of a day, a cut's book levels are summed over one body at most
/// (`splice_columns_into`'s ⚠), and a body decodes under the codec's bound on a run's text (one frame).
const CHUNK_ROWS: usize = 16_384;

/// The most store reads of cold days in flight at once, across every read of both families: no more than the datahub
/// lets one client hold ([`RANGE_READS_PER_CLIENT`], 3 of its 4 places, so the 4th stays for every other client), and
/// one less than this daemon's handle holds ([`HistoryStoreDatahub::POOL_CONNECTIONS`], 4), so the stamps and every
/// forwarded verb of the runs beside it still find a connection. Both bounds are read, never restated: 3 today.
const COLD_READS_IN_FLIGHT: usize = {
    let spare_a_connection = HistoryStoreDatahub::POOL_CONNECTIONS - 1;
    if RANGE_READS_PER_CLIENT < spare_a_connection {
        RANGE_READS_PER_CLIENT
    } else {
        spare_a_connection
    }
};

/// A [`HistoryStore`] that keeps the bars and ticks of past UTC days it read through `inner` (the module doc).
pub struct CachedHistoryStore {
    inner: Arc<dyn HistoryStore + Send + Sync>,
    /// What today is: the composition root hands in the wall clock (this crate reads none of its own).
    clock: Box<dyn Clock + Send + Sync>,
    /// The kept bar days, under the bar budget.
    bars: DayCache,
    /// The kept quote, trade and book-update days, under the one tick budget.
    ticks: DayCache,
    /// The places the cold reads of both families share.
    cold: ColdReads,
}

/// The store reads of cold days in flight: at most `most` ([`COLD_READS_IN_FLIGHT`]), the rest waiting for a place.
struct ColdReads {
    most: usize,
    open: Mutex<usize>,
    freed: Condvar,
}

impl ColdReads {
    /// Wait for a place; it is held until the guard drops, so a read that fails or panics gives it back too.
    fn enter(&self) -> ColdRead<'_> {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let mut open = self
            .freed
            .wait_while(open, |open| *open >= self.most)
            .unwrap_or_else(PoisonError::into_inner);
        *open += 1;
        ColdRead(self)
    }
}

/// One place of [`ColdReads`], given back on drop.
struct ColdRead<'a>(&'a ColdReads);

impl Drop for ColdRead<'_> {
    fn drop(&mut self) {
        *self.0.open.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        self.0.freed.notify_one();
    }
}

/// A row the cache keeps by UTC day, as the columns the datahub's codec writes for its read.
trait DayRow: ColumnRow + Send + Sync + 'static {
    /// The read's columns reply (`vike_datahub_client::proto`'s table): what its days are kept as.
    const MAGIC: ColumnsMagic;
    /// The time its UTC day is taken from: the store's `ts`, what its parts' edges and a range filter read, and the
    /// FIRST word of its columns, where [`DayColumns::time`] and [`splice_days`] read it (`bar_cache_tests.rs` pins it
    /// per kind).
    fn timestamp(&self) -> i64;
}

impl DayRow for Bar {
    const MAGIC: ColumnsMagic = BAR_COLUMNS;
    fn timestamp(&self) -> i64 {
        self.timestamp
    }
}

impl DayRow for TickQuote {
    const MAGIC: ColumnsMagic = QUOTE_COLUMNS;
    fn timestamp(&self) -> i64 {
        self.timestamp
    }
}

impl DayRow for TickTrade {
    const MAGIC: ColumnsMagic = TRADE_COLUMNS;
    fn timestamp(&self) -> i64 {
        self.timestamp
    }
}

impl DayRow for L2BookUpdate {
    const MAGIC: ColumnsMagic = BOOK_UPDATE_COLUMNS;
    fn timestamp(&self) -> i64 {
        self.timestamp
    }
}

/// One series' UTC day: what a kept day is filed under. `interval` is empty and `source` `None` for a tick day.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DayKey {
    kind: StoreKindId,
    venue: String,
    source: Option<String>,
    symbol: String,
    interval: String,
    day: i64,
}

/// One whole day's rows as the store answered them, ENCODED: the plain columns body of its key's kind
/// ([`DayRow::MAGIC`]) of each [`CHUNK_ROWS`] rows of it, in order, the last body holding the rest (no body for a day
/// of no row). Bytes for every kind, so the one budget holds every kind of a family.
struct DayColumns {
    rows: usize,
    bodies: Vec<Arc<Vec<u8>>>,
}

/// A kept day, shared by the cache and every read of it.
type DayRows = Arc<DayColumns>;

impl DayColumns {
    /// The bytes it holds: what the budget counts.
    fn bytes(&self) -> usize {
        self.bodies.iter().map(|body| body.capacity()).sum()
    }

    /// Row `i`'s time, read where it lies: its body's first word ([`DayRow::timestamp`]).
    fn time(&self, i: usize) -> i64 {
        let word = columns_word(&self.bodies[i / CHUNK_ROWS], 0, i % CHUNK_ROWS);
        word.expect("a row of a kept day") as i64
    }
}

/// `rows` as one plain `magic` body, its room cut to its length.
fn body_of<R: ColumnRow>(magic: &ColumnsMagic, rows: &[R]) -> Vec<u8> {
    let mut body = Vec::new();
    encode_columns_into(&mut body, magic, rows);
    body.shrink_to_fit();
    body
}

/// `rows[from..]` encoded as `magic` bodies of [`CHUNK_ROWS`] rows counted from `from`, the last holding the rest,
/// each with its row count, in order: made from the TAIL, each chunk's rows freed and their room handed back as soon
/// as its body is made, so the rows and their bodies are never held whole at once (`rows` keeps `..from`). The frees
/// are tens of milliseconds a tick day, on a path that reads seconds of it from the datahub.
fn encode_from_tail<R: ColumnRow>(
    rows: &mut Vec<R>,
    from: usize,
    magic: &ColumnsMagic,
) -> Vec<(Arc<Vec<u8>>, usize)> {
    let mut bodies = Vec::new();
    while rows.len() > from {
        let at = from + (rows.len() - from - 1) / CHUNK_ROWS * CHUNK_ROWS;
        bodies.push((Arc::new(body_of(magic, &rows[at..])), rows.len() - at));
        rows.truncate(at);
        rows.shrink_to_fit();
    }
    bodies.reverse();
    bodies
}

/// `rows` as an answer of whole `magic` bodies of at most [`CHUNK_ROWS`] rows ([`encode_from_tail`]): a read the
/// cache keeps nothing of, and a store's owned answer the sandbox relay sends (`strategy_sandbox/relay.rs`'s
/// `send_columns`).
pub(crate) fn encoded<R: ColumnRow>(magic: &ColumnsMagic, mut rows: Vec<R>) -> SharedColumns {
    let mut out = SharedColumns::default();
    for (body, n) in encode_from_tail(&mut rows, 0, magic) {
        out.push(body, 0..n);
    }
    out
}

/// What one read through the cache answers: the store's own rows where the cache keeps nothing of the read (read
/// through), else the kept days' bodies cut to its range.
enum Answer<T> {
    Fresh(Vec<T>),
    Kept(SharedColumns),
}

impl<T: DayRow> Answer<T> {
    /// The rows, what an owned read answers: a read through's as the store gave them, kept bodies [`decoded`].
    fn rows(self) -> Result<Vec<T>, HistoryStoreError> {
        match self {
            Self::Fresh(rows) => Ok(rows),
            Self::Kept(columns) => decoded(columns),
        }
    }

    /// The bodies, what `read_columns` answers: kept ones as they are, a read through's rows [`encoded`] once.
    fn columns(self) -> SharedColumns {
        match self {
            Self::Fresh(rows) => encoded(&T::MAGIC, rows),
            Self::Kept(columns) => columns,
        }
    }
}

/// An answer's rows, what an owned read answers: each run (a cut of one body, at most [`CHUNK_ROWS`] rows) spliced
/// into one body and decoded, into a `Vec` of exactly the answer's length.
fn decoded<R: DayRow>(answer: SharedColumns) -> Result<Vec<R>, HistoryStoreError> {
    let (mut rows, mut body) = (Vec::with_capacity(answer.len()), Vec::new());
    for run in answer.runs() {
        body.clear();
        splice_columns_into::<R>(&mut body, &R::MAGIC, &[run])
            .and_then(|()| decode_columns_into(&body, &R::MAGIC, &mut rows))
            .map_err(|e| HistoryStoreError::Query(format!("a kept day does not decode: {e}")))?;
    }
    Ok(rows)
}

/// One family's kept days, under one budget.
struct DayCache {
    cap_bytes: usize,
    kept: Mutex<Kept>,
}

/// The kept days, their recency (`by_use` maps each day's last use to it, oldest first) and the days some
/// read is fetching right now.
#[derive(Default)]
struct Kept {
    days: HashMap<DayKey, KeptDay>,
    by_use: BTreeMap<u64, DayKey>,
    uses: u64,
    bytes: usize,
    flying: HashMap<DayKey, Arc<Flight>>,
}

struct KeptDay {
    stamp: u64,
    rows: DayRows,
    bytes: usize,
    used: u64,
}

/// One day a read is fetching: the reads that need it meanwhile wait here for its rows or its error.
struct Flight {
    stamp: u64,
    landed: Mutex<Option<Result<DayRows, HistoryStoreError>>>,
    signal: Condvar,
}

impl Flight {
    fn new(stamp: u64) -> Self {
        Self { stamp, landed: Mutex::new(None), signal: Condvar::new() }
    }

    fn land(&self, answer: Result<DayRows, HistoryStoreError>) {
        *self.landed.lock().unwrap_or_else(PoisonError::into_inner) = Some(answer);
        self.signal.notify_all();
    }

    fn wait(&self) -> Result<DayRows, HistoryStoreError> {
        let landed = self.landed.lock().unwrap_or_else(PoisonError::into_inner);
        let landed = self
            .signal
            .wait_while(landed, |answer| answer.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        landed.clone().expect("the wait ends on a landed answer")
    }
}

/// What one listed day of a read is.
enum Slot {
    /// Kept under the same stamp.
    Held(DayRows),
    /// Another read is fetching it under the same stamp: wait for its answer.
    Joined(Arc<Flight>),
    /// This read fetches it. `Some` = it leads the day's flight; `None` = another read is fetching ANOTHER
    /// stamp of it, so this one reads its own and leads nothing.
    Missing(BarDay, Option<Arc<Flight>>),
}

/// The flights one read leads, by day. Each one lands exactly once: with its rows, with the read's error,
/// or, should the read never finish (a panic), with an error saying so, so no waiter is left waiting.
struct Leading<'a> {
    cache: &'a DayCache,
    flights: HashMap<i64, (DayKey, Arc<Flight>)>,
}

impl Leading<'_> {
    fn land(&mut self, day: i64, answer: Result<DayRows, HistoryStoreError>) {
        if let Some((key, flight)) = self.flights.remove(&day) {
            self.cache.ground(&key, &flight);
            flight.land(answer);
        }
    }

    fn fail_all(&mut self, error: &HistoryStoreError) {
        for (_, (key, flight)) in self.flights.drain() {
            self.cache.ground(&key, &flight);
            flight.land(Err(error.clone()));
        }
    }
}

impl Drop for Leading<'_> {
    fn drop(&mut self) {
        if self.flights.is_empty() {
            return;
        }
        self.fail_all(&HistoryStoreError::Query(
            "the read fetching this day did not finish".to_string(),
        ));
    }
}

/// `mib` MiB as bytes.
fn mib_bytes(mib: u32) -> usize {
    usize::try_from(u64::from(mib) << 20).unwrap_or(usize::MAX)
}

impl CachedHistoryStore {
    /// Keep at most `bar_cap_mib` MiB of bars and `tick_cap_mib` MiB of ticks read through `inner` (0 keeps none of
    /// that family); `clock` says when today began.
    pub fn new(
        inner: Arc<dyn HistoryStore + Send + Sync>,
        bar_cap_mib: u32,
        tick_cap_mib: u32,
        clock: Box<dyn Clock + Send + Sync>,
    ) -> Self {
        let cache = |mib| DayCache { cap_bytes: mib_bytes(mib), kept: Mutex::new(Kept::default()) };
        let cold =
            ColdReads { most: COLD_READS_IN_FLIGHT, open: Mutex::new(0), freed: Condvar::new() };
        Self { inner, clock, bars: cache(bar_cap_mib), ticks: cache(tick_cap_mib), cold }
    }

    /// How many bytes of bars are kept now.
    pub fn kept_bytes(&self) -> usize {
        self.bars.kept_bytes()
    }

    /// How many bytes of ticks (quotes, trades and book updates together) are kept now.
    pub fn kept_tick_bytes(&self) -> usize {
        self.ticks.kept_bytes()
    }

    /// One bar read of `range` through the bar cache: `load_bars` (`source: None`) or an unbudgeted
    /// `load_bars_sourced_capped`, stamped by `series_days` of bars.
    fn read_bars(
        &self,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Answer<Bar>, HistoryStoreError> {
        let series = DayKey {
            kind: StoreKindId::Bar,
            venue: venue.to_string(),
            source: source.map(str::to_string),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            day: 0,
        };
        let inner = &self.inner;
        self.read(
            &self.bars,
            &series,
            range,
            |r| inner.series_days(StoreKindId::Bar, venue, source, symbol, Some(interval), r),
            |r| match source {
                None => inner.load_bars(venue, symbol, interval, r),
                Some(lane) => {
                    inner.load_bars_sourced_capped(venue, lane, symbol, interval, r, None)
                }
            },
            |r| inner.read_columns(StoreKindId::Bar, venue, source, symbol, Some(interval), r),
        )
    }

    /// `scan_quotes` of `range` through the tick cache.
    fn read_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Answer<TickQuote>, HistoryStoreError> {
        self.read_ticks(StoreKindId::Quote, venue, symbol, range, |r| {
            self.inner.scan_quotes(venue, symbol, r)
        })
    }

    /// `scan_trades` of `range` through the tick cache.
    fn read_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Answer<TickTrade>, HistoryStoreError> {
        self.read_ticks(StoreKindId::Trade, venue, symbol, range, |r| {
            self.inner.scan_trades(venue, symbol, r)
        })
    }

    /// `scan_book_updates` of `range` through the tick cache.
    fn read_books(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Answer<L2BookUpdate>, HistoryStoreError> {
        self.read_ticks(StoreKindId::Book, venue, symbol, range, |r| {
            self.inner.scan_book_updates(venue, symbol, r)
        })
    }

    /// One tick read of `kind` over `range` through the tick cache, `fresh` being the inner store's read of it (its
    /// columns read beside it), stamped by `series_days` of that kind.
    fn read_ticks<T: DayRow>(
        &self,
        kind: StoreKindId,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        fresh: impl Fn(TimestampRange) -> Result<Vec<T>, HistoryStoreError> + Sync,
    ) -> Result<Answer<T>, HistoryStoreError> {
        let series = DayKey {
            kind,
            venue: venue.to_string(),
            source: None,
            symbol: symbol.to_string(),
            interval: String::new(),
            day: 0,
        };
        let inner = &self.inner;
        let stamps = |r| inner.series_days(kind, venue, None, symbol, None, r);
        let columns = |r| inner.read_columns(kind, venue, None, symbol, None, r);
        self.read(&self.ticks, &series, range, stamps, fresh, columns)
    }

    /// One read of `range` of `series` (its `day` unused), served from `cache`'s kept days that are still valid, the
    /// days another read is fetching, and the store for the rest (`columns`, the store's own columns of a range where it
    /// holds them, else `fresh`'s rows: [`DayCache::fetch_run`]): each day's bodies cut to the range and SHARED with the
    /// cache, never copied ([`decoded`] decodes them once for an owned read); a read the cache keeps nothing of is
    /// `fresh`'s own rows. `stamps` is the store's stamped days of a range.
    fn read<T: DayRow, C>(
        &self,
        cache: &DayCache,
        series: &DayKey,
        range: TimestampRange,
        stamps: impl FnOnce(TimestampRange) -> Result<Option<Vec<BarDay>>, HistoryStoreError>,
        fresh: impl Fn(TimestampRange) -> Result<Vec<T>, HistoryStoreError> + Sync,
        columns: C,
    ) -> Result<Answer<T>, HistoryStoreError>
    where
        C: Fn(TimestampRange) -> Option<Result<SharedColumns, HistoryStoreError>> + Sync,
    {
        let Some(days) = self.keepable_days(cache, range, stamps) else {
            return fresh(range).map(Answer::Fresh);
        };
        let key = |day: i64| DayKey { day, ..series.clone() };
        let slots = cache.look_up(&days, &key);
        let mut leading = Leading { cache, flights: HashMap::new() };
        for slot in &slots {
            if let Slot::Missing(day, Some(flight)) = slot {
                leading.flights.insert(day.day, (key(day.day), flight.clone()));
            }
        }
        let mut out = SharedColumns::default();
        let mut at = 0;
        while at < slots.len() {
            let rows = match &slots[at] {
                Slot::Held(rows) => rows.clone(),
                Slot::Joined(flight) => match flight.wait() {
                    Ok(rows) => rows,
                    Err(e) => {
                        leading.fail_all(&e);
                        return Err(e);
                    }
                },
                Slot::Missing(..) => {
                    let run_end = slots[at..]
                        .iter()
                        .position(|slot| !matches!(slot, Slot::Missing(..)))
                        .map_or(slots.len(), |n| at + n);
                    let run = &slots[at..run_end];
                    for rows in
                        cache.fetch_run(run, &key, &mut leading, &fresh, &columns, &self.cold)?
                    {
                        push_cut(&mut out, &rows, range);
                    }
                    at = run_end;
                    continue;
                }
            };
            push_cut(&mut out, &rows, range);
            at += 1;
        }
        Ok(Answer::Kept(out))
    }

    /// The stamped days of `range` when it may be served from `cache`, else `None` (the module doc's read-through
    /// cases).
    fn keepable_days(
        &self,
        cache: &DayCache,
        range: TimestampRange,
        stamps: impl FnOnce(TimestampRange) -> Result<Option<Vec<BarDay>>, HistoryStoreError>,
    ) -> Option<Vec<BarDay>> {
        if cache.cap_bytes == 0 {
            return None;
        }
        let today_starts = self.clock.now_ms().div_euclid(MS_PER_DAY) * MS_PER_DAY;
        if range.end.is_none_or(|end| end >= today_starts) {
            return None;
        }
        let days = stamps(range).ok()??;
        // A day whose midnight `i64` cannot hold is a stamp no piece reads whole: read through, as for any unused stamp.
        let unusable = |day: &BarDay| day.provisional || midnight(day.day).is_none();
        if days.is_empty() || days.iter().any(unusable) {
            return None;
        }
        Some(days)
    }
}

impl DayCache {
    /// How many bytes of rows are kept now.
    fn kept_bytes(&self) -> usize {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner).bytes
    }

    /// Fetch a run of missing days in [`pieces`] read at once under `cold`'s places, each one store read of its whole
    /// days that its own thread cuts by day once its place is given back: the store's own columns where it holds them
    /// (`columns`, the datahub's pages: [`splice_days`]), else `fresh`'s rows encoded ([`encode_days`]). Keep each
    /// listed day, land the flights this read leads for them, and answer the days in day order (the caller cuts), each
    /// piece as soon as it and every piece before it landed. A day a piece found that was not listed (a write landed
    /// after the stamps) is answered, not kept. The first piece in day order that failed lands every flight this read
    /// still leads with its error (the pieces after it are read and dropped), and a piece whose thread could not start
    /// fails the same way, in words. Each piece's thread is named `bt-cold-read-<n>` (within Linux's 15 bytes), so a
    /// panic in one names it.
    fn fetch_run<T: DayRow, C>(
        &self,
        run: &[Slot],
        key: &impl Fn(i64) -> DayKey,
        leading: &mut Leading<'_>,
        fresh: &(impl Fn(TimestampRange) -> Result<Vec<T>, HistoryStoreError> + Sync),
        columns: &C,
        cold: &ColdReads,
    ) -> Result<Vec<DayRows>, HistoryStoreError>
    where
        C: Fn(TimestampRange) -> Option<Result<SharedColumns, HistoryStoreError>> + Sync,
    {
        let missing: Vec<BarDay> = run
            .iter()
            .filter_map(|slot| match slot {
                Slot::Missing(day, _) => Some(*day),
                Slot::Held(_) | Slot::Joined(_) => None,
            })
            .collect();
        let cut = pieces(&missing, cold.most);
        std::thread::scope(|scope| {
            let reads: Vec<_> = cut
                .iter()
                .enumerate()
                .map(|(n, &(span, _))| {
                    std::thread::Builder::new().name(format!("bt-cold-read-{n}")).spawn_scoped(
                        scope,
                        move || {
                            // The place is held for the store read alone: the days are cut once it is given back.
                            let place = cold.enter();
                            match columns(span) {
                                Some(read) => {
                                    drop(place);
                                    read.and_then(|columns| splice_days::<T>(columns, span))
                                }
                                None => {
                                    let read = fresh(span);
                                    drop(place);
                                    read.map(encode_days)
                                }
                            }
                        },
                    )
                })
                .collect();
            let mut answered = Vec::new();
            for ((_, listed), read) in cut.iter().zip(reads) {
                let answer = match read {
                    Ok(read) => {
                        read.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                    }
                    Err(e) => Err(HistoryStoreError::Query(format!(
                        "the backtesthub could not start a thread to read cold days: {e}"
                    ))),
                };
                let days = match answer {
                    Ok(days) => days,
                    Err(e) => {
                        leading.fail_all(&e);
                        return Err(e);
                    }
                };
                let mut by_day: BTreeMap<i64, DayRows> =
                    days.into_iter().map(|(day, rows)| (day, Arc::new(rows))).collect();
                // A listed day the read found empty is kept empty: its stamp says it holds nothing.
                let stamps: HashMap<i64, u64> =
                    listed.iter().map(|day| (day.day, day.stamp)).collect();
                for day in stamps.keys() {
                    by_day
                        .entry(*day)
                        .or_insert_with(|| Arc::new(DayColumns { rows: 0, bodies: Vec::new() }));
                }
                for (day, rows) in by_day {
                    if let Some(stamp) = stamps.get(&day) {
                        self.keep(key(day), *stamp, rows.clone());
                        leading.land(day, Ok(rows.clone()));
                    }
                    answered.push(rows);
                }
            }
            Ok(answered)
        })
    }

    /// Each listed day as held, joined or missing, under ONE lock, so two reads of a day agree on which of
    /// them fetches it. A kept day under another stamp is dropped here: it can never be served again.
    fn look_up(&self, days: &[BarDay], key: &impl Fn(i64) -> DayKey) -> Vec<Slot> {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        days.iter()
            .map(|day| {
                let wanted = key(day.day);
                match kept.days.get(&wanted).map(|held| held.stamp == day.stamp) {
                    Some(true) => {
                        kept.touch(&wanted);
                        return Slot::Held(kept.days[&wanted].rows.clone());
                    }
                    Some(false) => kept.forget(&wanted),
                    None => {}
                }
                match kept.flying.get(&wanted) {
                    Some(flight) if flight.stamp == day.stamp => Slot::Joined(flight.clone()),
                    Some(_) => Slot::Missing(*day, None),
                    None => {
                        let flight = Arc::new(Flight::new(day.stamp));
                        kept.flying.insert(wanted, flight.clone());
                        Slot::Missing(*day, Some(flight))
                    }
                }
            })
            .collect()
    }

    /// Take a landed flight off the board, if it is still the one filed under `key`.
    fn ground(&self, key: &DayKey, flight: &Arc<Flight>) {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        if kept.flying.get(key).is_some_and(|filed| Arc::ptr_eq(filed, flight)) {
            kept.flying.remove(key);
        }
    }

    /// Keep one day's bodies under `stamp`, then evict the least recently used days past the cap.
    fn keep(&self, key: DayKey, stamp: u64, rows: DayRows) {
        let bytes = rows.bytes();
        if bytes > self.cap_bytes {
            return;
        }
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.forget(&key);
        kept.uses += 1;
        let used = kept.uses;
        kept.by_use.insert(used, key.clone());
        kept.days.insert(key, KeptDay { stamp, rows, bytes, used });
        kept.bytes += bytes;
        while kept.bytes > self.cap_bytes {
            let Some((_, oldest)) = kept.by_use.pop_first() else { break };
            if let Some(gone) = kept.days.remove(&oldest) {
                kept.bytes -= gone.bytes;
            }
        }
    }
}

/// `missing` (a run's listed days, ascending, at least one) cut into at most `most` pieces of consecutive days, as even
/// as the days allow, each with the span it reads: from its first day's midnight to just before the next piece's first
/// day, the last to the end of the run's last day. The spans join into the run's one span, so a day no stamp listed
/// between two listed ones is read by exactly one piece, as a single read of the run read it. Every day's midnight fits
/// `i64` ([`CachedHistoryStore::keepable_days`] reads through a stamp that names one that does not), and the edges are
/// computed without wrapping ([`last_millisecond`]): the last day `i64` holds reads to `i64::MAX`.
fn pieces(missing: &[BarDay], most: usize) -> Vec<(TimestampRange, &[BarDay])> {
    let count = most.clamp(1, missing.len());
    let end = last_millisecond(missing[missing.len() - 1].day);
    let cuts: Vec<usize> = (0..=count).map(|k| k * missing.len() / count).collect();
    cuts.windows(2)
        .map(|cut| {
            let days = &missing[cut[0]..cut[1]];
            let to = missing.get(cut[1]).map_or(end, |next| last_millisecond(next.day - 1));
            (TimestampRange::of(days[0].day.saturating_mul(MS_PER_DAY), to), days)
        })
        .collect()
}

/// UTC `day`'s first millisecond, `None` for a day whose midnight `i64` cannot hold.
fn midnight(day: i64) -> Option<i64> {
    day.checked_mul(MS_PER_DAY)
}

/// UTC `day`'s last millisecond: the one before the next midnight, or `i64::MAX` on the last day `i64` holds, whose next
/// midnight does not fit (its times end there, so that is still the whole day).
fn last_millisecond(day: i64) -> i64 {
    day.checked_add(1).and_then(midnight).map_or(i64::MAX, |next| next - 1)
}

/// `rows` (the store's answer, in time order) as its UTC days, ascending, each encoded as [`DayColumns`]: each day
/// taken off the TAIL a body at a time ([`encode_from_tail`]), so the cut never holds the rows and their bodies whole
/// at once.
fn encode_days<T: DayRow>(mut rows: Vec<T>) -> Vec<(i64, DayColumns)> {
    let day_of = |row: &T| row.timestamp().div_euclid(MS_PER_DAY);
    let mut days = Vec::new();
    while let Some(day) = rows.last().map(day_of) {
        let from = rows.partition_point(|row| day_of(row) < day);
        let count = rows.len() - from;
        let bodies = encode_from_tail(&mut rows, from, &T::MAGIC).into_iter().map(|(body, _)| body);
        days.push((day, DayColumns { rows: count, bodies: bodies.collect() }));
    }
    days.reverse();
    days
}

/// A piece's columns as the store holds them (`HistoryStore::read_columns`: the datahub's pages) as its UTC days,
/// ascending, each [`DayColumns`] spliced from them ([`spliced`]): byte for byte the bodies [`encode_days`] makes of the
/// same rows, with no row decoded or encoded. The store checked each body as its decode would (that read's contract:
/// the datahub's handle checks every page where it arrives), and the splice checks each layout it copies; what the cut
/// leans on is checked here first, every row's time read where it lies: inside the piece's `span` and never before the
/// row ahead of it, so a garbled time refuses the piece and nothing of it is kept. A day is its first row's
/// `div_euclid` day and runs while rows share it (DAYS are compared, never a midnight, which a time near `i64::MAX`
/// would overflow), so rows sharing a timestamp stay in one day, and a body is dropped once its last row is spliced.
fn splice_days<T: DayRow>(
    columns: SharedColumns,
    span: TimestampRange,
) -> Result<Vec<(i64, DayColumns)>, HistoryStoreError> {
    let refused =
        |e: String| HistoryStoreError::Query(format!("the store's columns are refused: {e}"));
    let mut runs: VecDeque<(Arc<Vec<u8>>, Range<usize>)> = columns.into_runs().into();
    let mut ahead = i64::MIN;
    for (body, cut) in &runs {
        for i in cut.clone() {
            let Some(time) = columns_word(body, 0, i).map(|word| word as i64) else {
                return Err(refused(format!("a cut {cut:?} past its body's rows")));
            };
            let within = span.start.is_none_or(|s| time >= s) && span.end.is_none_or(|e| time <= e);
            if !within || time < ahead {
                let why =
                    format!("a row at {time}, outside {span:?} or before the row ahead of it");
                return Err(refused(why));
            }
            ahead = time;
        }
    }
    let time = |body: &[u8], i: usize| columns_word(body, 0, i).expect("a row read above") as i64;
    let day_of = |body: &[u8], i: usize| time(body, i).div_euclid(MS_PER_DAY);
    let mut days = Vec::new();
    while let Some((body, cut)) = runs.front() {
        let day = day_of(body.as_slice(), cut.start);
        let mut rows = SharedColumns::default();
        while let Some((body, cut)) = runs.front_mut() {
            let page = body.as_slice();
            let before = partition_point(cut.len(), |i| day_of(page, cut.start + i) <= day);
            let end = cut.start + before;
            rows.push(Arc::clone(body), cut.start..end);
            if end < cut.end {
                cut.start = end;
                break;
            }
            runs.pop_front();
        }
        days.push((day, spliced::<T>(&rows).map_err(refused)?));
    }
    Ok(days)
}

/// One day's `rows` as [`DayColumns`]: bodies of [`CHUNK_ROWS`] rows counted from its first, the last holding the rest
/// ([`encode_from_tail`]'s cut), each spliced from its runs into a body of exactly its length (the splice counts it
/// first). Bounded by rows, as an encoded day is, not by bytes: a relay part is bounded by the frame where it is cut
/// (`strategy_sandbox/wire.rs`'s `columns_part_into`).
fn spliced<T: DayRow>(rows: &SharedColumns) -> Result<DayColumns, String> {
    let mut bodies = Vec::with_capacity(rows.len().div_ceil(CHUNK_ROWS));
    for at in (0..rows.len()).step_by(CHUNK_ROWS) {
        let (runs, mut body) = (runs_of(rows, at, (at + CHUNK_ROWS).min(rows.len())), Vec::new());
        splice_columns_into::<T>(&mut body, &T::MAGIC, &runs)?;
        body.shrink_to_fit();
        bodies.push(Arc::new(body));
    }
    Ok(DayColumns { rows: rows.len(), bodies })
}

/// The rows `at..end` of `columns`, as the cuts of the bodies they span: a relay part's (`strategy_sandbox/relay.rs`'s
/// `send_columns`) and a spliced body's ([`spliced`]).
pub(crate) fn runs_of(
    columns: &SharedColumns,
    at: usize,
    end: usize,
) -> Vec<(&[u8], Range<usize>)> {
    let (mut runs, mut start) = (Vec::new(), 0);
    for (body, rows) in columns.runs() {
        let (from, to) = (at.max(start), end.min(start + rows.len()));
        if from < to {
            runs.push((body, rows.start + from - start..rows.start + to - start));
        }
        start += rows.len();
        if start >= end {
            break;
        }
    }
    runs
}

/// One day's rows cut to `range`, as the next runs of `out`, one per body the cut spans: the rows are ascending (the
/// store's order), so the range's rows are one stretch of them.
fn push_cut(out: &mut SharedColumns, day: &DayColumns, range: TimestampRange) {
    let from = partition_point(day.rows, |i| range.start.is_some_and(|s| day.time(i) < s));
    let to = partition_point(day.rows, |i| range.end.is_none_or(|e| day.time(i) <= e));
    // An inverted range (end before start) holds nothing.
    let to = to.max(from);
    for (k, body) in day.bodies.iter().enumerate() {
        let first = k * CHUNK_ROWS;
        let (a, b) = (from.max(first), to.min(first + CHUNK_ROWS));
        if a < b {
            out.push(body.clone(), a - first..b - first);
        }
    }
}

/// The first of `0..n` where `holds` stops holding, `slice::partition_point` over indices: `holds` holds for a head
/// of them and for none after.
fn partition_point(n: usize, holds: impl Fn(usize) -> bool) -> usize {
    let (mut low, mut high) = (0, n);
    while low < high {
        let mid = low + (high - low) / 2;
        if holds(mid) {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    low
}

impl Kept {
    /// Mark a kept day as used now.
    fn touch(&mut self, key: &DayKey) {
        self.uses += 1;
        let now = self.uses;
        if let Some(day) = self.days.get_mut(key) {
            self.by_use.remove(&day.used);
            day.used = now;
            self.by_use.insert(now, key.clone());
        }
    }

    /// Drop a kept day, if there is one.
    fn forget(&mut self, key: &DayKey) {
        if let Some(gone) = self.days.remove(key) {
            self.by_use.remove(&gone.used);
            self.bytes -= gone.bytes;
        }
    }
}

#[path = "bar_cache_tests.rs"]
#[cfg(test)]
mod bar_cache_tests;

#[path = "bar_cache/tick_cache_tests.rs"]
#[cfg(test)]
mod tick_cache_tests;

#[path = "bar_cache/pages_tests.rs"]
#[cfg(test)]
mod pages_tests;

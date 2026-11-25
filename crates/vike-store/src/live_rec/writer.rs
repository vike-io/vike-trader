//! The writer thread: per-series buffers, the flush policy and its retry, the `RecKind` recipes.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use vike_marketdata::interval::bar_step_ms;
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::EquitySample;
use vike_model::time::epoch_ms_to_utc_date;

use super::drops::LossMeter;
use super::{HistoryStore, LiveMap, Liveness, Msg, RecorderConfig};
use crate::datafusion_hist::{GroupResolver, HistoryStoreDataFusion};
use vike_data::store::hist::HistoryStoreError;
use vike_data::store::store_kind::{QUOTE_HOLES_LANE, RECORDER_LANE};
use vike_model::StoreKindId;

/// Delay between a failed `append_*` and its ONE retry (see the module doc's "Flush retry").
const FLUSH_RETRY_DELAY: Duration = Duration::from_millis(100);

/// A first `append_*` attempt that took at least this long is NOT retried: it has already spent the
/// store's own internal retry budget (`SeriesLock::acquire` spins ~4s on a contended lock), so a
/// second attempt would mostly just double this single writer thread's stall.
const SLOW_ATTEMPT: Duration = Duration::from_secs(1);

/// What [`flush_buffer`] writes through: the store it appends to, plus the counter a permanently
/// failed flush bumps.
///
/// Bundled into one value rather than passed as two arguments so every flush call site keeps its
/// argument count (`ingest` was already at clippy's 7-argument limit).
struct FlushContext<'a> {
    store: &'a HistoryStoreDataFusion,
    discarded: &'a AtomicU64,
    /// `Some` ⇒ merge same-group buffers into one commit (see `RecorderConfig::grouping`).
    grouping: Option<GroupResolver>,
    /// Per-series row-arrival tracking — see [`Liveness`]. Shared with [`RecorderHandle::liveness`].
    live: LiveMap,
    /// See `RecorderConfig::source_of`.
    source_of: Option<fn(&str) -> Option<&'static str>>,
    /// See `RecorderConfig::write_floor`.
    write_floor: Option<crate::live_rec::RecorderWriteFloor>,
    /// Whether the last flush found the store's disk below `write_floor`: the state is said on each change only.
    below_floor: std::cell::Cell<bool>,
}

impl FlushContext<'_> {
    /// The `source=` lane `venue`'s rows of `kind` go to, and the verb that writes it, when they have one.
    fn lane<T>(
        &self,
        venue: &str,
        kind: &RecKind<T>,
    ) -> Option<(&'static str, SourcedAppendFn<T>)> {
        self.source_of.and_then(|of| of(venue)).zip(kind.append_sourced)
    }
}

/// One `(venue, symbol)` buffer of unflushed rows, all sharing the same UTC date (a date
/// mismatch on an incoming row triggers a rollover flush before the row is buffered).
struct Buffered<T> {
    rows: Vec<T>,
    date: String,
    opened_at: Instant,
}

impl<T> Buffered<T> {
    fn new(date: String) -> Self {
        Self { rows: Vec::new(), date, opened_at: Instant::now() }
    }
}

/// Per-writer-thread state: one buffer map per tick kind, keyed by `(venue, symbol)`, plus the
/// monotonic flush counter appended to every commit key (see the module doc for why).
struct WriterState {
    quotes: HashMap<(String, String), Buffered<TickQuote>>,
    trades: HashMap<(String, String), Buffered<TickTrade>>,
    books: HashMap<(String, String), Buffered<L2BookUpdate>>,
    /// The CONFLATING depth lane — separate buffer, separate series. See `HistoryStore::append_depth`.
    depth: HashMap<(String, String), Buffered<L2BookUpdate>>,
    equity: HashMap<(String, String), Buffered<EquitySample>>,
    /// Closed live bars, keyed `(venue, symbol/interval)` ([`bar_buffer_symbol`]).
    bars: HashMap<(String, String), Buffered<Bar>>,
    /// The last close [`take_bar`] took for each bar buffer key: a replay of it is dropped, a gap after it is a hole.
    last_bar: HashMap<(String, String), Bar>,
    flush_sequence: u64,
}

impl WriterState {
    fn new() -> Self {
        Self {
            quotes: HashMap::new(),
            trades: HashMap::new(),
            books: HashMap::new(),
            depth: HashMap::new(),
            equity: HashMap::new(),
            bars: HashMap::new(),
            last_bar: HashMap::new(),
            flush_sequence: 0,
        }
    }
}

/// Draw the next flush-sequence value from `counter`, monotonic for the lifetime of the writer
/// thread. Every flush (max-rows, age, rollover, or shutdown) draws one, even across different
/// `(venue, symbol)` series and different tick kinds (every `ingest`/`flush_aged`/`flush_all_of`
/// call below threads the SAME `&mut state.flush_sequence`) — this is what makes the commit key unique
/// when two flushes share `(first_timestamp, last_timestamp)`.
fn next_sequence(counter: &mut u64) -> u64 {
    let seq = *counter;
    *counter += 1;
    seq
}

/// The writer thread body: drain the channel, buffer rows, flush on policy, exit on shutdown.
///
/// **Age-check cadence is decoupled from `recv`'s outcome (final-review fix).** `recv_timeout`
/// returns `Ok` immediately whenever ANY message is queued, so with N series multiplexed on this
/// one channel, a steadily-active series (always has a message ready) would starve the age check
/// forever if it only ran in the `Err(Timeout)` arm — a quiet series' buffered rows would then sit
/// unflushed (and unprotected by WAL) indefinitely. Instead `last_age_check` is tracked
/// independently: after handling EITHER arm, if it has been at least `wait` since the last scan,
/// run the aged-flush pass and reset the clock. This bounds the age scan to once per `wait`
/// regardless of how busy the channel is.
pub(crate) fn writer_loop(
    store: &HistoryStoreDataFusion,
    rx: Receiver<Msg>,
    config: &RecorderConfig,
    mut meter: LossMeter,
    live: LiveMap,
) {
    let mut state = WriterState::new();
    // An independent `Arc` clone (not a borrow of `meter`) so `ctx` and the `&mut meter` reporting
    // calls below can coexist without fighting over one borrow. `&discarded` (NOT `&*discarded` —
    // that trips `clippy::explicit_auto_deref`) reaches the `&AtomicU64` field type by deref
    // coercion, which a struct-literal field is a coercion site for.
    let discarded = meter.discarded.clone();
    let ctx = FlushContext {
        store,
        discarded: &discarded,
        grouping: config.grouping.clone(),
        live,
        source_of: config.source_of,
        write_floor: config.write_floor.clone(),
        below_floor: std::cell::Cell::new(false),
    };
    // Wake at least once a second (so age flushes fire on a quiet feed even with a long
    // `max_age`), but no less often than `max_age` itself so a short test/tuned `max_age` is
    // still honored promptly.
    let wait = config.max_age.min(Duration::from_secs(1)).max(Duration::from_millis(1));
    let mut last_age_check = Instant::now();

    loop {
        match rx.recv_timeout(wait) {
            Ok(Msg::Quote { venue, symbol, quote }) => {
                ingest(
                    &ctx,
                    &mut state.quotes,
                    &mut state.flush_sequence,
                    config,
                    (venue, symbol),
                    quote,
                    &QUOTE,
                );
            }
            Ok(Msg::Trade { venue, symbol, trade }) => {
                ingest(
                    &ctx,
                    &mut state.trades,
                    &mut state.flush_sequence,
                    config,
                    (venue, symbol),
                    trade,
                    &TRADE,
                );
            }
            Ok(Msg::Book { venue, symbol, update }) => {
                ingest(
                    &ctx,
                    &mut state.books,
                    &mut state.flush_sequence,
                    config,
                    (venue, symbol),
                    update,
                    &BOOK,
                );
            }
            Ok(Msg::Depth { venue, symbol, update }) => {
                ingest(
                    &ctx,
                    &mut state.depth,
                    &mut state.flush_sequence,
                    config,
                    (venue, symbol),
                    update,
                    &DEPTH,
                );
            }
            Ok(Msg::Equity { venue, symbol, sample }) => {
                ingest(
                    &ctx,
                    &mut state.equity,
                    &mut state.flush_sequence,
                    config,
                    (venue, symbol),
                    sample,
                    &EQUITY,
                );
            }
            Ok(Msg::QuoteHole { venue, symbol, from_ms, to_ms }) => {
                write_quote_hole(&ctx, &venue, &symbol, from_ms, to_ms);
            }
            Ok(Msg::Bar { venue, symbol, interval, bar }) => {
                let key = (venue, bar_buffer_symbol(&symbol, &interval));
                if take_bar(&mut state.last_bar, &key, &interval, &bar) {
                    ingest(
                        &ctx,
                        &mut state.bars,
                        &mut state.flush_sequence,
                        config,
                        key,
                        bar,
                        &BAR,
                    );
                }
            }
            Ok(Msg::Shutdown) => {
                flush_all(&ctx, &mut state);
                // AFTER the final flush: it is the last thing that can bump `discarded`.
                meter.report_final();
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                // Every RecorderSink (and its Arc clones) is gone — nothing left to flush for.
                flush_all(&ctx, &mut state);
                meter.report_final();
                return;
            }
        }

        if age_scan_due(last_age_check.elapsed(), wait) {
            flush_aged(&ctx, &mut state.quotes, &mut state.flush_sequence, config.max_age, &QUOTE);
            flush_aged(&ctx, &mut state.trades, &mut state.flush_sequence, config.max_age, &TRADE);
            flush_aged(&ctx, &mut state.books, &mut state.flush_sequence, config.max_age, &BOOK);
            flush_aged(&ctx, &mut state.depth, &mut state.flush_sequence, config.max_age, &DEPTH);
            flush_aged(&ctx, &mut state.equity, &mut state.flush_sequence, config.max_age, &EQUITY);
            flush_aged(&ctx, &mut state.bars, &mut state.flush_sequence, config.max_age, &BAR);
            last_age_check = Instant::now();
        }
        // Rides the same wake-up as the age scan: cheap (two relaxed loads) when nothing was lost,
        // and silent unless a delta is non-zero.
        meter.report_if_due();
    }
}

/// Whether the periodic age scan is due — the whole rule behind "a steadily-active series must not
/// starve a quiet one's age flush", named so it can be gated on its own.
///
/// **What it does NOT take is the rule.** There is deliberately no `recv` outcome here:
/// `recv_timeout` returns `Ok` the instant ANY series has a message queued, so with N series
/// multiplexed on one channel, a cadence gated on `Err(Timeout)` never fires at all while one
/// series is steadily active — a quiet series' buffered rows would then sit unflushed (and
/// unprotected by the WAL) for as long as the busy one keeps talking. Driving the cadence off
/// elapsed wall time alone is what bounds the scan to once per `wait` no matter how busy the
/// channel is. `age_scan_rule_tests::the_age_scan_rule_rejects_a_timeout_only_cadence` pins that
/// by requiring this rule and the historical broken one to disagree.
pub(crate) fn age_scan_due(since_last_scan: Duration, wait: Duration) -> bool {
    since_last_scan >= wait
}

/// One tick kind's writer-thread recipe: the commit-key tag, the row ts-accessor (rollover check +
/// flush-key `first_timestamp`/`last_timestamp`), and the `HistoryStore::append_*` method it flushes through.
/// `quote`/`trade`/`book`/`equity` (the [`QUOTE`]/[`TRADE`]/[`BOOK`]/[`EQUITY`] consts below) differ
/// ONLY in these three — bundling them into one value is what lets `ingest`/`flush_aged`/
/// `flush_all_of`/`flush_buffer` be written once, generic over `T`, instead of once per kind (and
/// keeps each of those four under clippy's argument-count limit).
pub(crate) struct RecKind<T> {
    pub(crate) tag: StoreKindId,
    timestamp: fn(&T) -> i64,
    append: AppendFn<T>,
    /// The GROUPED twin of `append` — same shape, but its second argument is a group rather than a
    /// symbol. `None` for kinds that have no grouped form (equity, whose series is the portfolio).
    append_grouped: Option<AppendFn<T>>,
    /// Stamp the buffer's symbol onto a row that carries none. REQUIRED before a grouped append:
    /// a per-symbol caller may legally leave a row's symbol empty (the store tags it from the path),
    /// but a grouped series has no path to tag from and REJECTS unattributable rows.
    stamp_symbol: Option<fn(&mut T, &str)>,
    /// The `source=` twin of `append`: `(store, venue, source, symbol, rows, key)`. Quotes alone have one
    /// (`RecorderConfig::source_of`).
    append_sourced: Option<SourcedAppendFn<T>>,
}

/// The `HistoryStore::append_*` method shape every [`RecKind::append`] points at.
type AppendFn<T> =
    fn(&HistoryStoreDataFusion, &str, &str, &[T], Option<&str>) -> Result<usize, HistoryStoreError>;

/// [`AppendFn`] with the lane after the venue.
type SourcedAppendFn<T> = fn(
    &HistoryStoreDataFusion,
    &str,
    &str,
    &str,
    &[T],
    Option<&str>,
) -> Result<usize, HistoryStoreError>;

pub(crate) const QUOTE: RecKind<TickQuote> = RecKind {
    tag: StoreKindId::Quote,
    timestamp: |q| q.timestamp,
    append: HistoryStoreDataFusion::append_quotes,
    append_grouped: Some(HistoryStoreDataFusion::append_quotes_grouped),
    stamp_symbol: Some(|q, s| q.symbol = s.into()),
    append_sourced: Some(HistoryStoreDataFusion::append_quotes_sourced),
};
pub(crate) const TRADE: RecKind<TickTrade> = RecKind {
    tag: StoreKindId::Trade,
    timestamp: |t| t.timestamp,
    append: HistoryStoreDataFusion::append_trades,
    append_grouped: Some(HistoryStoreDataFusion::append_trades_grouped),
    stamp_symbol: Some(|t, s| t.symbol = s.into()),
    append_sourced: None,
};
pub(crate) const BOOK: RecKind<L2BookUpdate> = RecKind {
    tag: StoreKindId::Book,
    timestamp: |u| u.timestamp,
    append: HistoryStoreDataFusion::append_book_updates,
    append_grouped: Some(HistoryStoreDataFusion::append_book_updates_grouped),
    stamp_symbol: Some(|u, s| u.symbol = s.to_string()),
    append_sourced: None,
};
/// Equity has no grouped form: its series IS the portfolio (`venue = "portfolio"`), so there is
/// nothing to group and no symbol to tell rows apart by.
pub(crate) const DEPTH: RecKind<L2BookUpdate> = RecKind {
    tag: StoreKindId::Depth,
    timestamp: |u| u.timestamp,
    append: HistoryStoreDataFusion::append_depth,
    // No grouped twin yet: `append_depth_grouped` does not exist, so a depth family records
    // per-symbol. That is a deliberate first step, not an oversight — the grouped write path is
    // additive and the lane has no live producer wired to it until a venue depth feed is subscribed.
    append_grouped: None,
    stamp_symbol: Some(|u, s| u.symbol = s.to_string()),
    append_sourced: None,
};
/// Closed live bars (`RecorderConfig::record_bars`), into the recorder's lane, batched by the same flush
/// rules as every tick kind. A bar series is named by its interval too, and the buffer key has no slot for
/// one, so the symbol slot carries `symbol/interval` ([`bar_buffer_symbol`]) and [`append_recorded_bars`]
/// splits it back. No grouped form: a `group=` leaf would be unioned into every read of the symbol.
pub(crate) const BAR: RecKind<Bar> = RecKind {
    tag: StoreKindId::Bar,
    timestamp: |b| b.timestamp,
    append: append_recorded_bars,
    append_grouped: None,
    stamp_symbol: None,
    append_sourced: None,
};

/// The buffer key's symbol slot of a bar series: `symbol/interval`. A store symbol holds no `/` (the store
/// refuses one) and an interval never does, so [`append_recorded_bars`] splits at the LAST one, and a symbol
/// that does hold one is refused by the store as it would be anywhere else.
fn bar_buffer_symbol(symbol: &str, interval: &str) -> String {
    format!("{symbol}/{interval}")
}

/// [`BAR`]'s append: the buffer's bars into `kind=bar/venue=…/source=recorder/symbol=…/interval=…`, never the
/// sourceless series, which holds the venue's candles from the REST backfill and the archive import (a read
/// of it answers one bar per time, the newest write's, so a live close filed there would silently replace a
/// candle or be hidden by one: decision 0255's amendment of 2026-11-17).
fn append_recorded_bars(
    store: &HistoryStoreDataFusion,
    venue: &str,
    slot: &str,
    bars: &[Bar],
    commit_key: Option<&str>,
) -> Result<usize, HistoryStoreError> {
    let (symbol, interval) = slot.rsplit_once('/').ok_or_else(|| {
        HistoryStoreError::Query(format!(
            "{slot:?} is not a recorded bar series' `symbol/interval`"
        ))
    })?;
    store.append_bars_sourced(venue, RECORDER_LANE, symbol, interval, bars, commit_key)
}

/// Whether the close `bar` of the bar buffer `key` is taken, by the live core's own rule for a close
/// (`vike-core`'s `Ingest::BarClose`: a newer close appends, one of the same time replaces, an older one is
/// dropped): a close OLDER than the last one taken, or the very same close again, is a REPLAY and adds no
/// row, whether its twin still sits in the open buffer or has flushed; a flush's own retry is idempotent by
/// its key, as for every kind. A close of the same time with other values is taken, and a read keeps the
/// newest. Across a restart nothing is held here, and a read and a compaction keep one bar per time anyway.
/// A close more than one step after the last one is a HOLE (a socket that was down, or a closed market): it is
/// logged and stays a hole, because nothing here fetches the bars that never arrived.
fn take_bar(
    last_bar: &mut HashMap<(String, String), Bar>,
    key: &(String, String),
    interval: &str,
    bar: &Bar,
) -> bool {
    let previous = last_bar.get(key);
    if previous.is_some_and(|p| bar.timestamp < p.timestamp || p == bar) {
        return false;
    }
    if let (Some(previous), Some(step)) = (previous, bar_step_ms(interval))
        && bar.timestamp - previous.timestamp > step
    {
        tracing::warn!(
            venue = key.0.as_str(),
            series = key.1.as_str(),
            from = previous.timestamp,
            to = bar.timestamp,
            missing = (bar.timestamp - previous.timestamp) / step - 1,
            "RecorderSink: the recorded live bars have a hole (a socket that was down, or a closed \
             market); it stays a hole, nothing fetches the bars that never arrived"
        );
    }
    last_bar.insert(key.clone(), bar.clone());
    true
}

pub(crate) const EQUITY: RecKind<EquitySample> = RecKind {
    tag: StoreKindId::Equity,
    timestamp: |s| s.timestamp,
    append: HistoryStoreDataFusion::append_equity,
    append_grouped: None,
    stamp_symbol: None,
    append_sourced: None,
};

/// Ingest one row into its `(venue, symbol)` buffer — the shared shape behind the `Msg::*` match
/// arms in [`writer_loop`] above (every kind does the SAME rollover-check / push / max-rows-check
/// dance, differing only in `kind`): a UTC-date rollover flushes the old buffer first, then the row
/// is pushed, then a max-rows-full buffer flushes too.
///
/// ⚠ Both of those flushes go through [`flush_batch`], NOT `flush_buffer`, so that **`flush_batch` is
/// the ONE place deciding grouped-vs-per-symbol**. They used to call `flush_buffer` directly — the
/// per-symbol append, which never consults `ctx.grouping` — so a buffer that filled to `max_rows`
/// before `max_age` elapsed silently wrote itself PER-SYMBOL while its quieter siblings in the same
/// family wrote grouped. That is exactly backwards: a high-volume symbol is the one grouping exists
/// for, and it was the only one opting out. Found by the first live recorder run (2026-08-02): two
/// busy Polymarket tokens produced `symbol=<token>/` book series of 620 KB each ALONGSIDE the
/// family's `group=btc-updown-5m/` series of 136 KB, in one process, for the whole session — a
/// customer would then have to read two layouts to see one family, and the maintenance/retention
/// paths would treat them as unrelated series.
///
/// A one-element batch merges nothing, so this costs one `Vec` allocation per immediate flush and
/// buys the invariant that the routing decision cannot be made in two places again.
fn ingest<T>(
    ctx: &FlushContext<'_>,
    map: &mut HashMap<(String, String), Buffered<T>>,
    flush_sequence: &mut u64,
    config: &RecorderConfig,
    key: (String, String),
    item: T,
    kind: &RecKind<T>,
) {
    let date = epoch_ms_to_utc_date((kind.timestamp)(&item));

    if map.get(&key).is_some_and(|b| b.date != date)
        && let Some(buffer) = map.remove(&key)
    {
        flush_batch(ctx, vec![(key.clone(), buffer)], flush_sequence, kind);
    }

    // Liveness BEFORE the buffer push: this counts rows that ARRIVED, which is the question
    // ("is this series receiving anything?"). Whether they have flushed yet is a different one.
    {
        let mut live = ctx.live.lock().unwrap();
        let e = live
            .entry(vike_data::store::series::series_key(kind.tag, &key.0, &key.1))
            .or_insert(Liveness { rows: 0, last_ms: 0 });
        e.rows += 1;
        e.last_ms = vike_model::now_ms();
    }

    let buffer = map.entry(key.clone()).or_insert_with(|| Buffered::new(date));
    buffer.rows.push(item);

    if buffer.rows.len() >= config.max_rows
        && let Some(buffer) = map.remove(&key)
    {
        flush_batch(ctx, vec![(key, buffer)], flush_sequence, kind);
    }
}

/// Flush every buffer in `map` whose oldest row has aged past `max_age` — the shared shape behind
/// the four age-check call sites in [`writer_loop`] above.
fn flush_aged<T>(
    ctx: &FlushContext<'_>,
    map: &mut HashMap<(String, String), Buffered<T>>,
    flush_sequence: &mut u64,
    max_age: Duration,
    kind: &RecKind<T>,
) {
    let stale: Vec<(String, String)> = map
        .iter()
        .filter(|(_, b)| b.opened_at.elapsed() >= max_age)
        .map(|(k, _)| k.clone())
        .collect();
    let entries: Vec<_> =
        stale.into_iter().filter_map(|k| map.remove(&k).map(|b| (k, b))).collect();
    flush_batch(ctx, entries, flush_sequence, kind);
}

fn flush_all(ctx: &FlushContext<'_>, state: &mut WriterState) {
    flush_all_of(ctx, &mut state.quotes, &mut state.flush_sequence, &QUOTE);
    flush_all_of(ctx, &mut state.trades, &mut state.flush_sequence, &TRADE);
    flush_all_of(ctx, &mut state.books, &mut state.flush_sequence, &BOOK);
    flush_all_of(ctx, &mut state.depth, &mut state.flush_sequence, &DEPTH);
    flush_all_of(ctx, &mut state.equity, &mut state.flush_sequence, &EQUITY);
    flush_all_of(ctx, &mut state.bars, &mut state.flush_sequence, &BAR);
}

/// Drain and flush every buffer in `map` — the shared shape behind [`flush_all`]'s four calls
/// (shutdown / sender-disconnected teardown: flush every remaining buffer of one kind, in
/// whatever order the map yields them).
fn flush_all_of<T>(
    ctx: &FlushContext<'_>,
    map: &mut HashMap<(String, String), Buffered<T>>,
    flush_sequence: &mut u64,
    kind: &RecKind<T>,
) {
    flush_batch(ctx, map.drain().collect(), flush_sequence, kind);
}

/// Run `attempt`, and on failure run it EXACTLY once more after `retry_delay`. Returns the last
/// result and how many attempts were made (1 or 2).
///
/// Pure with respect to the store (it only calls the closure), which is what makes the retry policy
/// testable without an I/O-failing `HistoryStoreDataFusion`.
///
/// **The retry is skipped when the first attempt itself took `slow_attempt` or longer.** That is the
/// discriminator between the two failure shapes, and it needs no fragile matching on `HistoryStoreError`'s
/// opaque strings: a transient I/O error fails fast and is worth a second go, while a lock timeout
/// only returns after `SeriesLock::acquire` has already spun ~4s, so retrying it would mostly just
/// double the single writer thread's stall — and every millisecond stalled here is more rows dropped
/// at the channel behind it.
pub(crate) fn append_with_retry<F>(
    mut attempt: F,
    retry_delay: Duration,
    slow_attempt: Duration,
) -> (Result<usize, HistoryStoreError>, u32)
where
    F: FnMut() -> Result<usize, HistoryStoreError>,
{
    let started = Instant::now();
    let first = attempt();
    if first.is_ok() || started.elapsed() >= slow_attempt {
        return (first, 1);
    }
    thread::sleep(retry_delay);
    (attempt(), 2)
}

/// Flush one buffered `(venue, symbol)` series through `kind.append` — the shared shape behind
/// every flush call site above (`quote`/`trade`/`book`/`equity` differ only in `kind`, see
/// [`RecKind`]).
fn flush_buffer<T>(
    ctx: &FlushContext<'_>,
    venue: &str,
    symbol: &str,
    buffer: Buffered<T>,
    seq: u64,
    kind: &RecKind<T>,
) {
    match ctx.lane(venue, kind) {
        Some((source, append)) => {
            flush_buffer_with(
                ctx,
                venue,
                symbol,
                buffer.rows,
                seq,
                kind,
                |store, v, s, rows, key| append(store, v, source, s, rows, key),
            );
        }
        None => flush_buffer_with(ctx, venue, symbol, buffer.rows, seq, kind, kind.append),
    }
}

/// [`flush_buffer`] over an explicit row vec and append fn, so the per-symbol and GROUPED paths share
/// one implementation of the retry, the commit-key scheme and the loss accounting. `target` is the
/// symbol on the per-symbol path and the group on the grouped one — it is the second argument of
/// the store append either way, and what makes the commit key unique per series per flush.
fn flush_buffer_with<T>(
    ctx: &FlushContext<'_>,
    venue: &str,
    symbol: &str,
    rows_in: Vec<T>,
    seq: u64,
    kind: &RecKind<T>,
    append: impl Fn(
        &HistoryStoreDataFusion,
        &str,
        &str,
        &[T],
        Option<&str>,
    ) -> Result<usize, HistoryStoreError>,
) {
    let buffer = Buffered { rows: rows_in, date: String::new(), opened_at: Instant::now() };
    if buffer.rows.is_empty() {
        return;
    }
    // The free-space floor (`RecorderConfig::write_floor`), asked before every write: below it the rows are
    // discarded and counted, and the change of state is said once each way.
    if let Some(floor) = &ctx.write_floor {
        match floor() {
            Some(why) => {
                ctx.discarded.fetch_add(buffer.rows.len() as u64, Ordering::Relaxed);
                if !ctx.below_floor.replace(true) {
                    tracing::warn!(
                        venue,
                        symbol,
                        "RecorderSink: {} rows are DISCARDED, not written, until the disk has room: {why}",
                        kind.tag
                    );
                }
                return;
            }
            None if ctx.below_floor.replace(false) => {
                tracing::info!(
                    "RecorderSink: the disk has room again; {} rows are written",
                    kind.tag
                );
            }
            None => {}
        }
    }
    let first_timestamp = (kind.timestamp)(buffer.rows.first().expect("checked non-empty"));
    let last_timestamp = (kind.timestamp)(buffer.rows.last().expect("checked non-empty"));
    // `seq` (this writer thread's monotonic flush counter) guarantees uniqueness even when two
    // flushes of the same series share (first_timestamp, last_timestamp) — see the module doc for why that
    // matters (a repeated commit key is a silent no-op in the store, not an error). The RETRY below
    // reuses this same key on purpose: that no-op is exactly what makes retrying safe, since an
    // attempt that failed after committing cannot be double-written.
    let key =
        format!("live-{venue}-{symbol}-{}-{first_timestamp}-{last_timestamp}-{seq}", kind.tag);
    let rows = buffer.rows.len();
    let (result, attempts) = append_with_retry(
        || append(ctx.store, venue, symbol, &buffer.rows, Some(&key)),
        FLUSH_RETRY_DELAY,
        SLOW_ATTEMPT,
    );
    match result {
        Ok(_) if attempts > 1 => {
            tracing::info!(
                venue,
                symbol,
                rows,
                "RecorderSink: {} flush succeeded on retry — no rows lost",
                kind.tag
            );
        }
        Ok(_) => {}
        Err(err) => {
            // Nowhere left to put them: the rows are gone. Count them so the loss shows up in the
            // periodic `DropReport` instead of only as this one log line.
            ctx.discarded.fetch_add(rows as u64, Ordering::Relaxed);
            tracing::warn!(
                venue,
                symbol,
                %err,
                rows,
                attempts = attempts as u64,
                "RecorderSink: {} flush failed after {attempts} attempt(s) — DISCARDING {rows} rows \
                 (this series' tape now has a hole). A PERSISTENT failure means another writer is \
                 holding this series' lock for longer than the ~4s spin — check for a second \
                 process writing the same store root, or a compaction stuck on a huge `date=`.",
                kind.tag
            );
        }
    }
}

/// Commit one mark of a quote tape's hole (`vike_data::store::store_kind::QUOTE_HOLES_LANE`, whose doc says
/// what the row means) as it arrives, so it outlives any restart that comes after it.
///
/// It takes no buffer because it needs none (a hole is rare) and no [`ingest`], whose [`Liveness`] entry would
/// read a mark as a row of the very tape it says has stopped. The key is the row's identity, so a mark said
/// twice is stored once.
fn write_quote_hole(ctx: &FlushContext<'_>, venue: &str, symbol: &str, from_ms: i64, to_ms: i64) {
    let row = TickQuote {
        timestamp: from_ms,
        local_timestamp: to_ms,
        bid: 0.0,
        ask: 0.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: Default::default(),
    };
    let key = format!("quote-hole-{venue}-{symbol}-{from_ms}-{to_ms}");
    let (result, attempts) = append_with_retry(
        || {
            ctx.store.append_quotes_sourced(
                venue,
                QUOTE_HOLES_LANE,
                symbol,
                std::slice::from_ref(&row),
                Some(&key),
            )
        },
        FLUSH_RETRY_DELAY,
        SLOW_ATTEMPT,
    );
    if let Err(err) = result {
        ctx.discarded.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            venue,
            symbol,
            from_ms,
            to_ms,
            %err,
            attempts = u64::from(attempts),
            "RecorderSink: a quote tape's hole was not marked, its stamps are in this line"
        );
    }
}

/// Flush a batch of drained buffers, MERGING any that resolve to the same group into one commit.
///
/// This is the live half of `BulkIngestSession`'s `merge_and_commit`, and it exists for the same
/// reason: routing per-symbol buffers at a shared group directory without merging them would be
/// strictly WORSE than not grouping — each buffer would still be its own commit, now all contending
/// on ONE `SeriesLock` instead of N independent ones. The merge is the feature.
///
/// Ungrouped (`ctx.grouping` is `None`, or the resolver returns `None`, or the kind has no grouped
/// form) each buffer flushes exactly as before — byte-identical to the pre-grouping path.
fn flush_batch<T>(
    ctx: &FlushContext<'_>,
    entries: Vec<((String, String), Buffered<T>)>,
    flush_sequence: &mut u64,
    kind: &RecKind<T>,
) {
    let grouped_append = kind.append_grouped;
    // (venue, group) -> merged rows. BTreeMap so commit order is deterministic run to run.
    let mut groups: BTreeMap<(String, String), Vec<T>> = BTreeMap::new();

    for ((venue, symbol), mut buffer) in entries {
        let group = grouped_append
            .and(ctx.grouping.as_ref())
            .and_then(|f| f(&venue, &symbol))
            .filter(|_| kind.stamp_symbol.is_some() && ctx.lane(&venue, kind).is_none());
        match group {
            Some(g) => {
                // A grouped series tells rows apart by their symbol column, so every row must carry
                // one. A per-symbol caller may legally have left it empty.
                if let Some(stamp) = kind.stamp_symbol {
                    for row in &mut buffer.rows {
                        stamp(row, &symbol);
                    }
                }
                groups.entry((venue, g)).or_default().extend(buffer.rows);
            }
            None => {
                let seq = next_sequence(flush_sequence);
                flush_buffer(ctx, &venue, &symbol, buffer, seq, kind);
            }
        }
    }

    for ((venue, group), rows) in groups {
        if rows.is_empty() {
            continue;
        }
        let seq = next_sequence(flush_sequence);
        // `flush_buffer` over the GROUP: same retry, same key shape, same loss accounting — the group
        // simply takes the symbol's place, which is also what makes the commit key unique per flush.
        flush_buffer_with(
            ctx,
            &venue,
            &group,
            rows,
            seq,
            kind,
            grouped_append.expect("group only set when the kind has a grouped append"),
        );
    }
}

//! The writer thread: per-series buffers, the flush policy and its retry, the `RecKind` recipes.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use vike_model::time::epoch_ms_to_utc_date;
use vike_model::{BookUpdate, EquitySample, QuoteTick, TradeTick};

use super::drops::LossMeter;
use super::{HistStore, LiveMap, Liveness, Msg, RecorderConfig};
use crate::store::datafusion_hist::{DataFusionHist, GroupResolver};
use crate::store::hist::DataError;

/// Delay between a failed `append_*` and its ONE retry (see the module doc's "Flush retry").
const FLUSH_RETRY_DELAY: Duration = Duration::from_millis(100);

/// A first `append_*` attempt that took at least this long is NOT retried: it has already spent the
/// store's own internal retry budget (`SeriesLock::acquire` spins ~4s on a contended lock), so a
/// second attempt would mostly just double this single writer thread's stall.
const SLOW_ATTEMPT: Duration = Duration::from_secs(1);

/// What [`flush_buf`] writes through: the store it appends to, plus the counter a permanently
/// failed flush bumps.
///
/// Bundled into one value rather than passed as two arguments so every flush call site keeps its
/// argument count (`ingest` was already at clippy's 7-argument limit).
struct FlushCtx<'a> {
    store: &'a DataFusionHist,
    discarded: &'a AtomicU64,
    /// `Some` ⇒ merge same-group buffers into one commit (see `RecorderConfig::grouping`).
    grouping: Option<GroupResolver>,
    /// Per-series row-arrival tracking — see [`Liveness`]. Shared with [`RecorderHandle::liveness`].
    live: LiveMap,
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
    quotes: HashMap<(String, String), Buffered<QuoteTick>>,
    trades: HashMap<(String, String), Buffered<TradeTick>>,
    books: HashMap<(String, String), Buffered<BookUpdate>>,
    /// The CONFLATING depth lane — separate buffer, separate series. See `HistStore::append_depth`.
    depth: HashMap<(String, String), Buffered<BookUpdate>>,
    equity: HashMap<(String, String), Buffered<EquitySample>>,
    flush_seq: u64,
}

impl WriterState {
    fn new() -> Self {
        Self {
            quotes: HashMap::new(),
            trades: HashMap::new(),
            books: HashMap::new(),
            depth: HashMap::new(),
            equity: HashMap::new(),
            flush_seq: 0,
        }
    }
}

/// Draw the next flush-sequence value from `counter`, monotonic for the lifetime of the writer
/// thread. Every flush (max-rows, age, rollover, or shutdown) draws one, even across different
/// `(venue, symbol)` series and different tick kinds (every `ingest`/`flush_aged`/`flush_all_of`
/// call below threads the SAME `&mut state.flush_seq`) — this is what makes the commit key unique
/// when two flushes share `(first_ts, last_ts)`.
fn next_seq(counter: &mut u64) -> u64 {
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
    store: &DataFusionHist,
    rx: Receiver<Msg>,
    cfg: &RecorderConfig,
    mut meter: LossMeter,
    live: LiveMap,
) {
    let mut state = WriterState::new();
    // An independent `Arc` clone (not a borrow of `meter`) so `ctx` and the `&mut meter` reporting
    // calls below can coexist without fighting over one borrow. `&discarded` (NOT `&*discarded` —
    // that trips `clippy::explicit_auto_deref`) reaches the `&AtomicU64` field type by deref
    // coercion, which a struct-literal field is a coercion site for.
    let discarded = meter.discarded.clone();
    let ctx = FlushCtx { store, discarded: &discarded, grouping: cfg.grouping.clone(), live };
    // Wake at least once a second (so age flushes fire on a quiet feed even with a long
    // `max_age`), but no less often than `max_age` itself so a short test/tuned `max_age` is
    // still honored promptly.
    let wait = cfg.max_age.min(Duration::from_secs(1)).max(Duration::from_millis(1));
    let mut last_age_check = Instant::now();

    loop {
        match rx.recv_timeout(wait) {
            Ok(Msg::Quote { venue, symbol, quote }) => {
                ingest(
                    &ctx,
                    &mut state.quotes,
                    &mut state.flush_seq,
                    cfg,
                    (venue, symbol),
                    quote,
                    &QUOTE,
                );
            }
            Ok(Msg::Trade { venue, symbol, trade }) => {
                ingest(
                    &ctx,
                    &mut state.trades,
                    &mut state.flush_seq,
                    cfg,
                    (venue, symbol),
                    trade,
                    &TRADE,
                );
            }
            Ok(Msg::Book { venue, symbol, update }) => {
                ingest(
                    &ctx,
                    &mut state.books,
                    &mut state.flush_seq,
                    cfg,
                    (venue, symbol),
                    update,
                    &BOOK,
                );
            }
            Ok(Msg::Depth { venue, symbol, update }) => {
                ingest(
                    &ctx,
                    &mut state.depth,
                    &mut state.flush_seq,
                    cfg,
                    (venue, symbol),
                    update,
                    &DEPTH,
                );
            }
            Ok(Msg::Equity { venue, symbol, sample }) => {
                ingest(
                    &ctx,
                    &mut state.equity,
                    &mut state.flush_seq,
                    cfg,
                    (venue, symbol),
                    sample,
                    &EQUITY,
                );
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
            flush_aged(&ctx, &mut state.quotes, &mut state.flush_seq, cfg.max_age, &QUOTE);
            flush_aged(&ctx, &mut state.trades, &mut state.flush_seq, cfg.max_age, &TRADE);
            flush_aged(&ctx, &mut state.books, &mut state.flush_seq, cfg.max_age, &BOOK);
            flush_aged(&ctx, &mut state.depth, &mut state.flush_seq, cfg.max_age, &DEPTH);
            flush_aged(&ctx, &mut state.equity, &mut state.flush_seq, cfg.max_age, &EQUITY);
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
/// flush-key `first_ts`/`last_ts`), and the `HistStore::append_*` method it flushes through.
/// `quote`/`trade`/`book`/`equity` (the [`QUOTE`]/[`TRADE`]/[`BOOK`]/[`EQUITY`] consts below) differ
/// ONLY in these three — bundling them into one value is what lets `ingest`/`flush_aged`/
/// `flush_all_of`/`flush_buf` be written once, generic over `T`, instead of once per kind (and
/// keeps each of those four under clippy's argument-count limit).
pub(crate) struct RecKind<T> {
    pub(crate) tag: &'static str,
    ts: fn(&T) -> i64,
    append: AppendFn<T>,
    /// The GROUPED twin of `append` — same shape, but its second argument is a group rather than a
    /// symbol. `None` for kinds that have no grouped form (equity, whose series is the portfolio).
    append_grouped: Option<AppendFn<T>>,
    /// Stamp the buffer's symbol onto a row that carries none. REQUIRED before a grouped append:
    /// a per-symbol caller may legally leave a row's symbol empty (the store tags it from the path),
    /// but a grouped series has no path to tag from and REJECTS unattributable rows.
    stamp_symbol: Option<fn(&mut T, &str)>,
}

/// The `HistStore::append_*` method shape every [`RecKind::append`] points at.
type AppendFn<T> = fn(&DataFusionHist, &str, &str, &[T], Option<&str>) -> Result<usize, DataError>;

pub(crate) const QUOTE: RecKind<QuoteTick> = RecKind {
    tag: "quote",
    ts: |q| q.ts,
    append: DataFusionHist::append_quotes,
    append_grouped: Some(DataFusionHist::append_quotes_grouped),
    stamp_symbol: Some(|q, s| q.symbol = s.to_string()),
};
pub(crate) const TRADE: RecKind<TradeTick> = RecKind {
    tag: "trade",
    ts: |t| t.ts,
    append: DataFusionHist::append_trades,
    append_grouped: Some(DataFusionHist::append_trades_grouped),
    stamp_symbol: Some(|t, s| t.symbol = s.to_string()),
};
pub(crate) const BOOK: RecKind<BookUpdate> = RecKind {
    tag: "book",
    ts: |u| u.ts,
    append: DataFusionHist::append_book_updates,
    append_grouped: Some(DataFusionHist::append_book_updates_grouped),
    stamp_symbol: Some(|u, s| u.symbol = s.to_string()),
};
/// Equity has no grouped form: its series IS the portfolio (`venue = "portfolio"`), so there is
/// nothing to group and no symbol to tell rows apart by.
pub(crate) const DEPTH: RecKind<BookUpdate> = RecKind {
    tag: "depth",
    ts: |u| u.ts,
    append: DataFusionHist::append_depth,
    // No grouped twin yet: `append_depth_grouped` does not exist, so a depth family records
    // per-symbol. That is a deliberate first step, not an oversight — the grouped write path is
    // additive and the lane has no live producer wired to it until a venue depth feed is subscribed.
    append_grouped: None,
    stamp_symbol: Some(|u, s| u.symbol = s.to_string()),
};
pub(crate) const EQUITY: RecKind<EquitySample> = RecKind {
    tag: "equity",
    ts: |s| s.ts,
    append: DataFusionHist::append_equity,
    append_grouped: None,
    stamp_symbol: None,
};

/// Ingest one row into its `(venue, symbol)` buffer — the shared shape behind the `Msg::*` match
/// arms in [`writer_loop`] above (every kind does the SAME rollover-check / push / max-rows-check
/// dance, differing only in `kind`): a UTC-date rollover flushes the old buffer first, then the row
/// is pushed, then a max-rows-full buffer flushes too.
///
/// ⚠ Both of those flushes go through [`flush_batch`], NOT `flush_buf`, so that **`flush_batch` is
/// the ONE place deciding grouped-vs-per-symbol**. They used to call `flush_buf` directly — the
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
    ctx: &FlushCtx<'_>,
    map: &mut HashMap<(String, String), Buffered<T>>,
    flush_seq: &mut u64,
    cfg: &RecorderConfig,
    key: (String, String),
    item: T,
    kind: &RecKind<T>,
) {
    let date = epoch_ms_to_utc_date((kind.ts)(&item));

    if map.get(&key).is_some_and(|b| b.date != date)
        && let Some(buf) = map.remove(&key)
    {
        flush_batch(ctx, vec![(key.clone(), buf)], flush_seq, kind);
    }

    // Liveness BEFORE the buffer push: this counts rows that ARRIVED, which is the question
    // ("is this series receiving anything?"). Whether they have flushed yet is a different one.
    {
        let mut live = ctx.live.lock().unwrap();
        let e = live
            .entry(format!("{}/{}/{}", kind.tag, key.0, key.1))
            .or_insert(Liveness { rows: 0, last_ms: 0 });
        e.rows += 1;
        e.last_ms = vike_model::now_ms();
    }

    let buf = map.entry(key.clone()).or_insert_with(|| Buffered::new(date));
    buf.rows.push(item);

    if buf.rows.len() >= cfg.max_rows
        && let Some(buf) = map.remove(&key)
    {
        flush_batch(ctx, vec![(key, buf)], flush_seq, kind);
    }
}

/// Flush every buffer in `map` whose oldest row has aged past `max_age` — the shared shape behind
/// the four age-check call sites in [`writer_loop`] above.
fn flush_aged<T>(
    ctx: &FlushCtx<'_>,
    map: &mut HashMap<(String, String), Buffered<T>>,
    flush_seq: &mut u64,
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
    flush_batch(ctx, entries, flush_seq, kind);
}

fn flush_all(ctx: &FlushCtx<'_>, state: &mut WriterState) {
    flush_all_of(ctx, &mut state.quotes, &mut state.flush_seq, &QUOTE);
    flush_all_of(ctx, &mut state.trades, &mut state.flush_seq, &TRADE);
    flush_all_of(ctx, &mut state.books, &mut state.flush_seq, &BOOK);
    flush_all_of(ctx, &mut state.depth, &mut state.flush_seq, &DEPTH);
    flush_all_of(ctx, &mut state.equity, &mut state.flush_seq, &EQUITY);
}

/// Drain and flush every buffer in `map` — the shared shape behind [`flush_all`]'s four calls
/// (shutdown / sender-disconnected teardown: flush every remaining buffer of one kind, in
/// whatever order the map yields them).
fn flush_all_of<T>(
    ctx: &FlushCtx<'_>,
    map: &mut HashMap<(String, String), Buffered<T>>,
    flush_seq: &mut u64,
    kind: &RecKind<T>,
) {
    flush_batch(ctx, map.drain().collect(), flush_seq, kind);
}

/// Run `attempt`, and on failure run it EXACTLY once more after `retry_delay`. Returns the last
/// result and how many attempts were made (1 or 2).
///
/// Pure with respect to the store (it only calls the closure), which is what makes the retry policy
/// testable without an I/O-failing `DataFusionHist`.
///
/// **The retry is skipped when the first attempt itself took `slow_attempt` or longer.** That is the
/// discriminator between the two failure shapes, and it needs no fragile matching on `DataError`'s
/// opaque strings: a transient I/O error fails fast and is worth a second go, while a lock timeout
/// only returns after `SeriesLock::acquire` has already spun ~4s, so retrying it would mostly just
/// double the single writer thread's stall — and every millisecond stalled here is more rows dropped
/// at the channel behind it.
pub(crate) fn append_with_retry<F>(
    mut attempt: F,
    retry_delay: Duration,
    slow_attempt: Duration,
) -> (Result<usize, DataError>, u32)
where
    F: FnMut() -> Result<usize, DataError>,
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
fn flush_buf<T>(
    ctx: &FlushCtx<'_>,
    venue: &str,
    symbol: &str,
    buf: Buffered<T>,
    seq: u64,
    kind: &RecKind<T>,
) {
    flush_buf_with(ctx, venue, symbol, buf.rows, seq, kind, kind.append);
}

/// [`flush_buf`] over an explicit row vec and append fn, so the per-symbol and GROUPED paths share
/// one implementation of the retry, the commit-key scheme and the loss accounting. `target` is the
/// symbol on the per-symbol path and the group on the grouped one — it is the second argument of
/// the store append either way, and what makes the commit key unique per series per flush.
fn flush_buf_with<T>(
    ctx: &FlushCtx<'_>,
    venue: &str,
    symbol: &str,
    rows_in: Vec<T>,
    seq: u64,
    kind: &RecKind<T>,
    append: AppendFn<T>,
) {
    let buf = Buffered { rows: rows_in, date: String::new(), opened_at: Instant::now() };
    if buf.rows.is_empty() {
        return;
    }
    let first_ts = (kind.ts)(buf.rows.first().expect("checked non-empty"));
    let last_ts = (kind.ts)(buf.rows.last().expect("checked non-empty"));
    // `seq` (this writer thread's monotonic flush counter) guarantees uniqueness even when two
    // flushes of the same series share (first_ts, last_ts) — see the module doc for why that
    // matters (a repeated commit key is a silent no-op in the store, not an error). The RETRY below
    // reuses this same key on purpose: that no-op is exactly what makes retrying safe, since an
    // attempt that failed after committing cannot be double-written.
    let key = format!("live-{venue}-{symbol}-{}-{first_ts}-{last_ts}-{seq}", kind.tag);
    let rows = buf.rows.len();
    let (result, attempts) = append_with_retry(
        || append(ctx.store, venue, symbol, &buf.rows, Some(&key)),
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
    ctx: &FlushCtx<'_>,
    entries: Vec<((String, String), Buffered<T>)>,
    flush_seq: &mut u64,
    kind: &RecKind<T>,
) {
    let grouped_append = kind.append_grouped;
    // (venue, group) -> merged rows. BTreeMap so commit order is deterministic run to run.
    let mut groups: BTreeMap<(String, String), Vec<T>> = BTreeMap::new();

    for ((venue, symbol), mut buf) in entries {
        let group = grouped_append
            .and(ctx.grouping.as_ref())
            .and_then(|f| f(&venue, &symbol))
            .filter(|_| kind.stamp_symbol.is_some());
        match group {
            Some(g) => {
                // A grouped series tells rows apart by their symbol column, so every row must carry
                // one. A per-symbol caller may legally have left it empty.
                if let Some(stamp) = kind.stamp_symbol {
                    for row in &mut buf.rows {
                        stamp(row, &symbol);
                    }
                }
                groups.entry((venue, g)).or_default().extend(buf.rows);
            }
            None => {
                let seq = next_seq(flush_seq);
                flush_buf(ctx, &venue, &symbol, buf, seq, kind);
            }
        }
    }

    for ((venue, group), rows) in groups {
        if rows.is_empty() {
            continue;
        }
        let seq = next_seq(flush_seq);
        // `flush_buf` over the GROUP: same retry, same key shape, same loss accounting — the group
        // simply takes the symbol's place, which is also what makes the commit key unique per flush.
        flush_buf_with(
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

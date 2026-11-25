//! The Account window's History READ: the account's own fills (`kind=exec_fill`) and order-lifecycle
//! snapshots (`kind=execution_order`) out of the DATAHUB's store, newest first, over a time window. The
//! widget (`vike_account_window::history`) RETURNS the [`ExecutionHistoryQuery`] it wants and draws the
//! [`ExecutionHistoryLoad`] it finds in its slot (the types are `vike_account_window::history::page`'s);
//! this module is what reads it, because a widget crate opens no connection. It was
//! `vike_account_window::history::load` until 2026-11-22; it sits beside the Data Manager's loader
//! ([`crate::data::data_manager`]) now, whose deadline it runs under.
//!
//! One source per fact (the design's §2.3): what is working NOW and can be cancelled is the trade
//! hub's read model, shown by the Account window; what WAS executed and how an order lived is the
//! store, shown by its History tab. Nothing here reads the snapshot, and nothing here can write: the only verbs it
//! sends are the two bounded reads, `HistoryStore::scan_execution_fills_head` and
//! `HistoryStore::scan_execution_orders_head`, plus the catalog of the window,
//! `HistoryStore::execution_series` (a few KB where the whole inventory was 212 KB on the CI box; a datahub
//! too old for it is asked the inventory and the client folds it), and once per load the accounts' position
//! snapshots at the page's edge (`vike_datahub_client::HistoryStoreDatahub::execution_account_positions`), what
//! the Summary opens an account from.
//!
//! # Newest first, over an ascending wire
//!
//! A head read answers the OLDEST rows of a range, and a person wants the newest. So the walk goes
//! BACKWARDS in windows from the end of the range — a day, a week, a month, a quarter, a year, then
//! a year at a time back to the series' first row (`WALK_DAYS`) — and stops once a page
//! (`HISTORY_PAGE_ROWS`) is in. A window holding more than the page still needs is BISECTED
//! toward its newer half, so memory stays bounded by the page and never by the window. Every read
//! is a complete whole-`ts` prefix (the store's contract), so no tied timestamp is split, and
//! "Older" continues from the oldest loaded `ts`. Which series are read at all, and from where, the
//! CATALOG decides (`live_series`): a read is a round trip, and a series that holds nothing in the
//! window costs none. Up to `READ_LANES` series are read at once, each over a handle of its own, and
//! the window is told how many of how many are in (`ExecutionHistorySlot::progress`).
//!
//! # Every RPC is bounded
//!
//! Each read runs through this module's `bounded` under the deadline [`spawn_load`] is handed (the
//! Data Manager's `RPC_DEADLINE`): the datahub client clears its read timeout past the handshake, so a
//! dead peer would otherwise hang the load for the life of the process
//! (`crates/vike-app-core/src/data/data_manager/mod.rs`'s module doc is the argument).
//!
//! What a load can SAY (no datahub, could not read, nothing in range, what the server serves) is the
//! page types' (`crates/vike-account-window/src/history/page.rs`'s module doc).
//!
//! Q1 of `docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md` (a range on the
//! execution-fill read) was deferred "until a series nears its ceiling"; the owner reopened it on
//! 2026-10-11 for THIS reader's time windows, and the wire's `scan_execution_fills_range` capability is
//! the result.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use vike_account_window::history::page::{
    ExecutionHistoryFailure, ExecutionHistoryLoad, ExecutionHistoryPage, ExecutionHistoryQuery,
    ExecutionHistoryServed, ExecutionHistorySlot, WALK_DAYS,
};
use vike_data::{
    ExecFillRow, ExecOrderRow, HistoryStore, SeriesCoverage, SeriesId, TimestampRange,
};
use vike_model::StoreKindId;
use vike_utility::thread::ThreadNoAnswer;

/// The rows one load (and one "Older" click) adds, per kind. A page, not a cap: the walk stops once
/// it is in.
pub(crate) const HISTORY_PAGE_ROWS: usize = 500;

/// How many series one load reads AT ONCE, each over a handle of its own and so one connection each
/// (`crates/vike-app-core/src/data/data_manager/mod.rs`'s `PROBE_WORKERS` argues the same), and every
/// read is a round trip through the operator's link. Few on purpose: each is a connection, with its
/// own keyed handshake, to a server that caps them.
pub(crate) const READ_LANES: usize = 4;

/// Run one blocking read on a SHORT-LIVED helper thread and give up on it after `deadline`: a small
/// answer only (a handshake, a catalog, one bounded head read), and abandoning the helper is safe
/// because it borrows nothing, so a late answer is dropped with its socket.
///
/// The thread, the one-slot channel and the wait are `vike_utility::thread::bounded`; this caller keeps
/// its thread name (`ui-history-read`, the GUI's `ui-` form, D32) and its words; the `what` names the read.
fn bounded<T: Send + 'static>(
    what: &str,
    deadline: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    vike_utility::thread::bounded("ui-history-read", deadline, f).map_err(|e| match e {
        ThreadNoAnswer::NotStarted(e) => {
            format!("the OS refused the execution-history helper thread: {e}")
        }
        ThreadNoAnswer::TimedOut | ThreadNoAnswer::Panicked => {
            format!("{what} did not answer within {deadline:?}; the link is treated as down")
        }
    })
}

/// Run one load on a background thread and leave it in `slot`, then `wake` the UI. `open` dials the
/// datahub — `None` when none is configured, which answers [`ExecutionHistoryFailure::NoDatahub`] without a
/// socket — and is called once per read lane (`READ_LANES`); a handle dials on its first read. The
/// shell's dial is handed in because the key resolution needs the binary's settings directory and
/// environment, which a library does not read. `deadline` bounds each read (`bounded`); the window's
/// glue (`crate::ui::windows::account_tool_content`) passes the Data Manager's `RPC_DEADLINE`.
pub fn spawn_load(
    query: ExecutionHistoryQuery,
    slot: ExecutionHistorySlot,
    deadline: Duration,
    open: impl Fn() -> Option<vike_datahub_client::HistoryStoreDatahub> + Send + 'static,
    wake: impl FnOnce() + Send + 'static,
) {
    let (failed_slot, failed_query) = (slot.clone(), query.clone());
    let spawned = std::thread::Builder::new().name("ui-history".into()).spawn(move || {
        let generation = query.generation;
        let report = |read: usize, total: usize| slot.report(generation, read, total);
        let outcome = load_remote(&open, &query, deadline, &report);
        slot.put(ExecutionHistoryLoad { query, outcome });
        wake();
    });
    if let Err(e) = spawned {
        // Said in the window too: a load that never lands is a spinner that never stops.
        tracing::warn!("execution history: the OS refused the load thread: {e}");
        failed_slot.put(ExecutionHistoryLoad {
            query: failed_query,
            outcome: Err(ExecutionHistoryFailure::Unreadable(format!(
                "the OS refused the load thread: {e}"
            ))),
        });
    }
}

/// The load against a datahub: its `Welcome` first (what it serves), then [`load_execution_history`]
/// over up to [`READ_LANES`] handles.
fn load_remote(
    open: &dyn Fn() -> Option<vike_datahub_client::HistoryStoreDatahub>,
    query: &ExecutionHistoryQuery,
    deadline: Duration,
    report: &(dyn Fn(usize, usize) + Sync),
) -> Result<ExecutionHistoryPage, ExecutionHistoryFailure> {
    let Some(first) = open() else { return Err(ExecutionHistoryFailure::NoDatahub) };
    let first = Arc::new(first);
    let r = Arc::clone(&first);
    let features = bounded("the datahub's handshake", deadline, move || r.advertised_features())
        .and_then(|got| got.map_err(|e| e.to_string()))
        .map_err(ExecutionHistoryFailure::Unreadable)?;
    let positions = Arc::clone(&first);
    let mut lanes: Vec<Arc<dyn HistoryStore + Send + Sync>> = vec![first];
    lanes.extend(
        (1..READ_LANES)
            .map_while(|_| open())
            .map(|s| Arc::new(s) as Arc<dyn HistoryStore + Send + Sync>),
    );
    let mut page = load_execution_history(
        &lanes,
        ExecutionHistoryServed::from_features(&features),
        query,
        deadline,
        report,
    )
    .map_err(ExecutionHistoryFailure::Unreadable)?;
    // Every account's position snapshot at the page's edge (its oldest fill): what the Summary opens from.
    if let Some(edge) = page.fills.iter().map(|r| r.ts).min() {
        let read = bounded("the accounts' position snapshots", deadline, move || {
            positions.execution_account_positions(edge)
        });
        match read {
            Ok(Ok(rows)) => page.account_positions = rows.unwrap_or_default(),
            Ok(Err(e)) => page.positions_unread = Some(e.to_string()),
            Err(e) => page.positions_unread = Some(e),
        }
    }
    Ok(page)
}

/// THE load over any `HistoryStore` — the catalog, then the newest page of fills and (when served) of
/// order snapshots inside `query`'s bounds. Pure over the trait: the CI suite drives it over the
/// in-memory store double. An `Err` is a read that failed; an empty page is a real answer.
///
/// `lanes` are handles to ONE store (the catalog is read on the first); up to that many series are
/// read at once, one per handle. `report(read, to_read)` is told how many series are read so far,
/// once the catalog has said how many will be.
pub(crate) fn load_execution_history(
    lanes: &[Arc<dyn HistoryStore + Send + Sync>],
    served: ExecutionHistoryServed,
    query: &ExecutionHistoryQuery,
    deadline: Duration,
    report: &(dyn Fn(usize, usize) + Sync),
) -> Result<ExecutionHistoryPage, String> {
    let Some(store) = lanes.first() else { return Err("no store to read".to_string()) };
    let (lower, upper) = query.bounds();
    let range = TimestampRange { start: lower, end: Some(upper) };
    let (s, venue, symbol) = (Arc::clone(store), query.venue.clone(), query.symbol.clone());
    let catalog = bounded("the execution-history catalog", deadline, move || {
        s.execution_series(venue.as_deref(), symbol.as_deref(), range)
    })?
    .map_err(|e| format!("execution-history catalog: {e}"))?;
    let mut page =
        ExecutionHistoryPage { served, venues: catalog.venues, ..ExecutionHistoryPage::default() };
    let (fill_series, order_series): (Vec<_>, Vec<_>) =
        catalog.series.into_iter().partition(|(id, _)| id.kind == StoreKindId::ExecFill);
    let fill_series = live_series(fill_series, range);
    let order_series = if served.orders { live_series(order_series, range) } else { Vec::new() };
    let to_read = fill_series.len() + order_series.len();
    let read_so_far = std::sync::atomic::AtomicUsize::new(0);
    let tick = || {
        let read = read_so_far.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        report(read, to_read);
    };
    report(0, to_read);
    let lane = |at: usize| Arc::clone(&lanes[at % lanes.len()]);

    if served.fills_range {
        let read = |at: usize, id: &SeriesId, range: TimestampRange, n: usize| {
            let (s, v, symbol) = (lane(at), id.venue.clone(), id.symbol.clone());
            bounded("an execution-fill read", deadline, move || {
                s.scan_execution_fills_head(&v, &symbol, range, n)
            })?
            .map_err(|e| e.to_string())
        };
        let timestamp: fn(&ExecFillRow) -> i64 = |r| r.ts;
        page.fills =
            newest_across(&fill_series, lanes.len(), &read, timestamp, &mut page.more, &tick)?;
    } else {
        let mut fills: Vec<ExecFillRow> = Vec::new();
        for series in &fill_series {
            // A datahub older than the window answers every windowed read with the WHOLE series
            // (`HistoryStoreDatahub` then cuts it), so the walk would fetch it once per window: read
            // it ONCE and cut here instead.
            let (s, v, symbol) = (lane(0), series.id.venue.clone(), series.id.symbol.clone());
            let mut rows = bounded("an execution-fill read", deadline, move || {
                s.scan_exec_fills(&v, &symbol)
            })?
            .map_err(|e| e.to_string())?;
            rows.retain(|r| r.ts >= series.floor && r.ts <= series.top);
            page.more |= cut_newest(&mut rows, |r| r.ts, HISTORY_PAGE_ROWS);
            fills.extend(rows);
            tick();
        }
        page.more |= cut_newest(&mut fills, |r| r.ts, HISTORY_PAGE_ROWS);
        page.fills = fills;
    }

    if served.orders {
        let read = |at: usize, id: &SeriesId, range: TimestampRange, n: usize| {
            let (s, v, symbol) = (lane(at), id.venue.clone(), id.symbol.clone());
            bounded("an execution-order read", deadline, move || {
                s.scan_execution_orders_head(&v, &symbol, range, n)
            })?
            .map_err(|e| e.to_string())
        };
        let timestamp: fn(&ExecOrderRow) -> i64 = |r| r.ts;
        page.orders =
            newest_across(&order_series, lanes.len(), &read, timestamp, &mut page.more, &tick)?;
    }
    if let Some(edge) = page.fills.iter().map(|r| r.ts).min() {
        match read_before(lanes, &fill_series, &page.fills, edge, deadline) {
            Ok(before) => page.before = before,
            Err(e) => page.before_unread = Some(e),
        }
    }
    page.oldest_timestamp =
        page.fills.iter().map(|r| r.ts).chain(page.orders.iter().map(|r| r.ts)).min();
    Ok(page)
}

/// **The fills before `edge`** (the oldest loaded fill) of every series of `live` a loaded fill is on, each from
/// its first row, up to `lanes` series at once: [`ExecutionHistoryPage::before`]. A series the catalog says
/// starts at or after `edge` holds none and costs no read.
// shortcut: a datahub without the ranged fill read answers it with the whole series (cut client-side), one more
// whole read per series on the page; read its series once in the walk the day such a datahub is still deployed.
fn read_before(
    lanes: &[Arc<dyn HistoryStore + Send + Sync>],
    live: &[LiveSeries],
    fills: &[ExecFillRow],
    edge: i64,
    deadline: Duration,
) -> Result<BTreeMap<(String, String), Vec<ExecFillRow>>, String> {
    let on_page: BTreeSet<(&str, &str)> =
        fills.iter().map(|r| (r.venue.as_str(), r.symbol.as_str())).collect();
    let todo: Vec<&SeriesId> = live
        .iter()
        .filter(|s| on_page.contains(&(s.id.venue.as_str(), s.id.symbol.as_str())))
        .filter(|s| !(s.known() && s.coverage.first_timestamp >= edge))
        .map(|s| &s.id)
        .collect();
    let range = TimestampRange { start: None, end: Some(edge.saturating_sub(1)) };
    let mut out = BTreeMap::new();
    for batch in todo.chunks(lanes.len().max(1)) {
        let answers: Vec<Result<Vec<ExecFillRow>, String>> = std::thread::scope(|scope| {
            let running: Vec<_> = batch
                .iter()
                .enumerate()
                .map(|(lane, id)| {
                    let (s, v, symbol) =
                        (Arc::clone(&lanes[lane]), id.venue.clone(), id.symbol.clone());
                    scope.spawn(move || {
                        bounded("an execution-fill read", deadline, move || {
                            s.scan_execution_fills_head(&v, &symbol, range, usize::MAX)
                        })?
                        .map_err(|e| e.to_string())
                    })
                })
                .collect();
            running
                .into_iter()
                .map(|t| {
                    t.join().unwrap_or_else(|_| Err("an execution-history read panicked".into()))
                })
                .collect()
        });
        for (id, rows) in batch.iter().zip(answers) {
            out.insert((id.venue.clone(), id.symbol.clone()), rows?);
        }
    }
    Ok(out)
}

/// One series the window reaches, with the bounds its walk reads.
struct LiveSeries {
    id: SeriesId,
    coverage: SeriesCoverage,
    floor: i64,
    top: i64,
}

impl LiveSeries {
    /// The catalog counted its rows, so its newest row bounds the walk. A coverage with no rows
    /// says nothing about where they end.
    fn known(&self) -> bool {
        self.coverage.rows > 0
    }
}

/// The series of `series` whose coverage reaches `range`, NEWEST row first, each with the bounds its
/// walk reads ([`SeriesCoverage::span_in`]): from its own newest row (not from the range's end) down to
/// the window's floor. A series whose coverage misses the window costs no read at all. MEASURED on
/// the CI box, 2026-11-16: a 7-day load over every venue sent two empty reads per series to 180 exec series,
/// ~360 round trips that took 3 min 50 s through the owner's tunnel, while 21 of those series held a
/// row that week.
fn live_series(series: Vec<(SeriesId, SeriesCoverage)>, range: TimestampRange) -> Vec<LiveSeries> {
    let mut live: Vec<LiveSeries> = series
        .into_iter()
        .filter_map(|(id, coverage)| {
            let (floor, top) = coverage.span_in(range)?;
            Some(LiveSeries { id, coverage, floor, top })
        })
        .collect();
    live.sort_by_key(|s| std::cmp::Reverse(s.coverage.last_timestamp));
    live
}

/// The newest page of ONE kind across `live` ([`live_series`]), newest first, reading up to `lanes`
/// series AT ONCE (`read`'s first argument is the lane, `0..lanes`), and `tick`ing once per series
/// read or skipped.
///
/// One that fits in a page is read in ONE request. The series go in newest-first batches, so once
/// the page is full a series whose newest row is older than the page's edge is skipped (it sets
/// `more`: "Older" reads it).
fn newest_across<T: Send, R>(
    live: &[LiveSeries],
    lanes: usize,
    read: &R,
    timestamp: fn(&T) -> i64,
    more: &mut bool,
    tick: &(dyn Fn() + Sync),
) -> Result<Vec<T>, String>
where
    R: Fn(usize, &SeriesId, TimestampRange, usize) -> Result<Vec<T>, String> + Sync,
{
    let mut out: Vec<T> = Vec::new();
    for batch in live.chunks(lanes.max(1)) {
        let edge = (out.len() >= HISTORY_PAGE_ROWS).then(|| out.last().map(timestamp)).flatten();
        let (skipped, todo): (Vec<&LiveSeries>, Vec<&LiveSeries>) = batch
            .iter()
            .partition(|s| s.known() && edge.is_some_and(|e| s.coverage.last_timestamp < e));
        for _ in skipped {
            *more = true;
            tick();
        }
        let answers: Vec<Result<(Vec<T>, bool), String>> = std::thread::scope(|scope| {
            let running: Vec<_> = todo
                .into_iter()
                .enumerate()
                .map(|(lane, s)| {
                    scope.spawn(move || {
                        let one = |range: TimestampRange, n: usize| read(lane, &s.id, range, n);
                        let walked = if s.known() && s.coverage.rows <= HISTORY_PAGE_ROWS as u64 {
                            window_newest(&one, timestamp, s.floor, s.top, HISTORY_PAGE_ROWS)
                        } else {
                            newest(&one, timestamp, s.floor, s.top, HISTORY_PAGE_ROWS)
                        };
                        tick();
                        walked
                    })
                })
                .collect();
            running
                .into_iter()
                .map(|t| {
                    t.join().unwrap_or_else(|_| Err("an execution-history read panicked".into()))
                })
                .collect()
        });
        for answer in answers {
            let (rows, cut) = answer?;
            *more |= cut;
            out.extend(rows);
        }
        *more |= cut_newest(&mut out, timestamp, HISTORY_PAGE_ROWS);
    }
    Ok(out)
}

/// Sort `rows` newest first and keep about `page` of them, never splitting a `ts` (the rows of the
/// timestamp that straddles the cut are all kept). Says whether anything was cut.
fn cut_newest<T>(rows: &mut Vec<T>, timestamp: impl Fn(&T) -> i64, page: usize) -> bool {
    rows.sort_by_key(|r| std::cmp::Reverse(timestamp(r)));
    if rows.len() <= page || page == 0 {
        return false;
    }
    let edge = timestamp(&rows[page - 1]);
    let keep = rows.iter().position(|r| timestamp(r) < edge).unwrap_or(rows.len()).max(page);
    let cut = keep < rows.len();
    rows.truncate(keep);
    cut
}

/// The newest `budget` rows of `[floor, upper]`, newest first, walking backwards in [`WALK_DAYS`]
/// windows. The flag says older rows exist inside `[floor, upper]` beyond the ones returned.
fn newest<T>(
    read: &dyn Fn(TimestampRange, usize) -> Result<Vec<T>, String>,
    timestamp: fn(&T) -> i64,
    floor: i64,
    upper: i64,
    budget: usize,
) -> Result<(Vec<T>, bool), String> {
    let mut out: Vec<T> = Vec::new();
    let mut hi = upper;
    let mut step = 0usize;
    while hi >= floor && out.len() < budget {
        let span = WALK_DAYS[step.min(WALK_DAYS.len() - 1)].saturating_mul(vike_model::MS_PER_DAY);
        let lo = hi.saturating_sub(span).saturating_add(1).max(floor);
        let (rows, more) = window_newest(read, timestamp, lo, hi, budget - out.len())?;
        out.extend(rows);
        if more {
            return Ok((out, true));
        }
        if lo == floor {
            return Ok((out, false));
        }
        hi = lo - 1;
        step += 1;
    }
    Ok((out, hi >= floor))
}

/// The newest `need` rows of `[lo, hi]`, newest first: one head read of `need + 1`; a window
/// holding more is bisected toward its newer half. The flag says the window holds older rows
/// beyond the ones returned. A single timestamp is never split.
fn window_newest<T>(
    read: &dyn Fn(TimestampRange, usize) -> Result<Vec<T>, String>,
    timestamp: fn(&T) -> i64,
    lo: i64,
    hi: i64,
    need: usize,
) -> Result<(Vec<T>, bool), String> {
    if need == 0 {
        return Ok((Vec::new(), true));
    }
    let mut rows = read(TimestampRange { start: Some(lo), end: Some(hi) }, need.saturating_add(1))?;
    if rows.len() <= need || hi <= lo {
        rows.reverse();
        return Ok((rows, false));
    }
    let mid = lo + (hi - lo) / 2;
    let (mut newer, newer_more) = window_newest(read, timestamp, mid + 1, hi, need)?;
    if newer_more || newer.len() >= need {
        return Ok((newer, true));
    }
    let (older, older_more) = window_newest(read, timestamp, lo, mid, need - newer.len())?;
    newer.extend(older);
    debug_assert!(newer.windows(2).all(|w| timestamp(&w[0]) >= timestamp(&w[1])), "newest first");
    Ok((newer, older_more))
}

#[path = "execution_history_tests.rs"]
#[cfg(test)]
mod execution_history_tests;

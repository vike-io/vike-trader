//! The execution HISTORY the Tearsheet window reads: the account's own fills (`kind=exec_fill`) and
//! order-lifecycle snapshots (`kind=exec_order`) out of the DATAHUB's store, newest first, over a
//! time window — the load half; `crate::ui::tool_views::tearsheet` renders it.
//!
//! One source per fact (the design's §2.3): what is working NOW and can be cancelled is the trade
//! hub's read model, shown by the Account window; what WAS executed and how an order lived is the
//! store, shown here. Nothing here reads the snapshot, and nothing here can write: the only verbs it
//! sends are the two bounded reads, `HistStore::scan_exec_fills_head` and
//! `HistStore::scan_exec_orders_head`, plus the catalog's `inventory`.
//!
//! # Newest first, over an ascending wire
//!
//! A head read answers the OLDEST rows of a range, and a person wants the newest. So the walk goes
//! BACKWARDS in windows from the end of the range — a day, a week, a month, a quarter, a year, then
//! a year at a time back to the series' first row (`WALK_DAYS`) — and stops once a page
//! ([`HISTORY_PAGE_ROWS`]) is in. A window holding more than the page still needs is BISECTED
//! toward its newer half, so memory stays bounded by the page and never by the window. Every read
//! is a complete whole-`ts` prefix (the store's contract), so no tied timestamp is split, and
//! "Older" continues from the oldest loaded `ts`.
//!
//! # Every RPC is bounded
//!
//! Each read runs through `crate::data::stored_load::bounded` under [`RPC_DEADLINE`]: the
//! datahub client clears its read timeout past the handshake, so a dead peer would otherwise hang
//! the load for the life of the process (`crate::data::stored_load`'s module doc is the argument).
//!
//! # What a load can say
//!
//! "No datahub", "could not read" and "nothing in range" are three different answers and stay
//! three: [`HistoryFailure`] for the first two, an EMPTY [`HistoryPage`] for the third. What the
//! server serves rides in [`ExecServed`], read off its `Welcome`: a datahub older than the
//! order read answers fills only (and, older than the window, the whole fill series, which
//! `vike_datahub_client::RemoteHistStore` then cuts on this side), and one whose journal
//! materializer is not mounted says so through the absence of `exec_materialize`.
//!
//! Q1 of `docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md` (a range on the
//! exec-fill read) was deferred "until a series nears its ceiling"; the owner reopened it on
//! 2026-10-11 for THIS reader's time windows, and the wire's `scan_exec_fills_range` capability is
//! the result.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_data::{
    ExecFillRow, ExecOrderRow, HistStore, SeriesCoverage, SeriesId, StoreKindId, TsRange,
};
use vike_datahub_client::proto::{
    FEATURE_EXEC_MATERIALIZE, FEATURE_SCAN_EXEC_FILLS_RANGE, FEATURE_SCAN_EXEC_ORDERS,
};

use crate::data::stored_load::{RPC_DEADLINE, bounded};

/// The rows one load (and one "Older" click) adds, per kind. A page, not a cap: the walk stops once
/// it is in.
pub const HISTORY_PAGE_ROWS: usize = 500;

/// The most rows the window holds in memory across "Older" clicks. Reaching it is SAID on screen
/// ("Showing the newest … rows; narrow the range"), never a silent cut.
pub const HISTORY_MAX_ROWS: usize = 5_000;

/// One day in epoch-ms, the unit of [`WALK_DAYS`] and of the time filters.
const DAY_MS: i64 = 86_400_000;

/// The backwards walk's window sizes, in days; the last repeats until the series' first row.
const WALK_DAYS: [i64; 5] = [1, 7, 30, 90, 365];

/// The time filter: how far back from now the history reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HistoryWindow {
    /// The last 24 hours.
    Day,
    /// The last 7 days — the default.
    #[default]
    Week,
    /// The last 30 days.
    Month,
    /// Everything the store holds.
    All,
}

impl HistoryWindow {
    /// Every filter, in the order the control shows them.
    pub const ALL: [HistoryWindow; 4] =
        [HistoryWindow::Day, HistoryWindow::Week, HistoryWindow::Month, HistoryWindow::All];

    /// The control's word.
    pub fn label(self) -> &'static str {
        match self {
            HistoryWindow::Day => "24h",
            HistoryWindow::Week => "7d",
            HistoryWindow::Month => "30d",
            HistoryWindow::All => "All",
        }
    }

    /// The inclusive lower bound for a window ending at `now_ms`; `None` for [`HistoryWindow::All`].
    pub fn start(self, now_ms: i64) -> Option<i64> {
        let days = match self {
            HistoryWindow::Day => WALK_DAYS[0],
            HistoryWindow::Week => WALK_DAYS[1],
            HistoryWindow::Month => WALK_DAYS[2],
            HistoryWindow::All => return None,
        };
        Some(now_ms.saturating_sub(days * DAY_MS).saturating_add(1))
    }
}

/// What the datahub serves of this history, read off its `Welcome`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecServed {
    /// `scan_exec_fills_range`: fills are read over a window. Without it the whole fill series is
    /// read and cut on this side — correct, and as costly as the series is long.
    pub fills_range: bool,
    /// `scan_exec_orders`: the order-lifecycle read exists. Without it there is no Orders tab.
    pub orders: bool,
    /// `exec_materialize`: the datahub's journal materializer is mounted, so live executions are
    /// written at all. ⚠ Its absence is AMBIGUOUS on a datahub too old for the other two, so a
    /// reader only concludes "off" when `orders` is served.
    pub recording: bool,
}

impl ExecServed {
    /// The three facts out of a `Welcome.features` list.
    pub fn from_features(features: &[String]) -> Self {
        let has = |f: &str| features.iter().any(|x| x == f);
        Self {
            fills_range: has(FEATURE_SCAN_EXEC_FILLS_RANGE),
            orders: has(FEATURE_SCAN_EXEC_ORDERS),
            recording: has(FEATURE_EXEC_MATERIALIZE),
        }
    }

    /// The datahub predates the execution-history reads (no order read).
    pub fn predates_history(self) -> bool {
        !self.orders
    }

    /// The materializer is KNOWN to be off: the datahub is new enough to say, and does not.
    pub fn materializer_off(self) -> bool {
        self.orders && !self.recording
    }
}

/// One load's question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryQuery {
    /// `None` = every venue with an exec series.
    pub venue: Option<String>,
    /// `None` = every symbol of the chosen venue(s).
    pub symbol: Option<String>,
    pub window: HistoryWindow,
    /// The instant the window ends at, epoch-ms (taken once, when the load is asked for).
    pub now_ms: i64,
    /// `Some(ts)`: the "Older" page — only rows strictly older than `ts`.
    pub older_than: Option<i64>,
    /// The asking window's request counter, so a load that lands after a newer request is dropped
    /// instead of replacing fresher rows.
    pub generation: u64,
}

impl HistoryQuery {
    /// The inclusive `[lower, upper]` this load reads; `lower` is `None` for an open start.
    pub fn bounds(&self) -> (Option<i64>, i64) {
        let upper = self.older_than.map_or(self.now_ms, |t| t.saturating_sub(1));
        (self.window.start(self.now_ms), upper)
    }
}

/// Why a load has no page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryFailure {
    /// No datahub address is configured or advertised: nothing was asked.
    NoDatahub,
    /// The datahub could not be read; the reason verbatim.
    Unreadable(String),
}

/// What one load read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistoryPage {
    /// Fills, NEWEST first.
    pub fills: Vec<ExecFillRow>,
    /// Order-lifecycle snapshots, NEWEST first — every snapshot, so a row can be expanded into its
    /// lifecycle; `vike_data::exec_index::latest_order_snapshots` collapses them.
    pub orders: Vec<ExecOrderRow>,
    /// What the datahub serves.
    pub served: ExecServed,
    /// Every venue with an exec series, and its symbols — the filters' choices, from the catalog
    /// and from nowhere in code.
    pub venues: BTreeMap<String, BTreeSet<String>>,
    /// Older rows exist inside the window beyond this page.
    pub more: bool,
    /// The oldest `ts` on the page — where "Older" continues from.
    pub oldest_ts: Option<i64>,
}

impl HistoryPage {
    /// Append an OLDER page below this one (an "Older" click). The newer page's catalog and served
    /// facts stay; the older page decides whether still more exist.
    pub fn extend_older(&mut self, older: HistoryPage) {
        self.fills.extend(older.fills);
        self.orders.extend(older.orders);
        self.more = older.more;
        self.oldest_ts = older.oldest_ts.or(self.oldest_ts);
    }

    /// Rows held, both kinds.
    pub fn rows(&self) -> usize {
        self.fills.len() + self.orders.len()
    }
}

/// One finished load, as the window adopts it.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecHistoryLoad {
    pub query: HistoryQuery,
    pub outcome: Result<HistoryPage, HistoryFailure>,
}

/// Where a background load leaves its answer for the window that asked: one slot per window, read
/// on its next frame.
#[derive(Clone, Default)]
pub struct ExecHistorySlot(Arc<Mutex<Option<ExecHistoryLoad>>>);

impl ExecHistorySlot {
    /// Leave `load` for the window (a newer put replaces an unread older one).
    pub fn put(&self, load: ExecHistoryLoad) {
        *crate::lock(&self.0) = Some(load);
    }

    /// The landed load, if any.
    pub fn take(&self) -> Option<ExecHistoryLoad> {
        crate::lock(&self.0).take()
    }
}

/// Run one load on a background thread and leave it in `slot`, then `wake` the UI. `open` dials the
/// datahub — `None` when none is configured, which answers [`HistoryFailure::NoDatahub`] without a
/// socket. The shell's dial is handed in because the key resolution needs the binary's settings
/// directory and environment, which a library does not read.
pub fn spawn_load(
    query: HistoryQuery,
    slot: ExecHistorySlot,
    open: impl FnOnce() -> Option<vike_datahub_client::RemoteHistStore> + Send + 'static,
    wake: impl FnOnce() + Send + 'static,
) {
    let spawned = std::thread::Builder::new().name("exec-history".into()).spawn(move || {
        let outcome = match open() {
            None => Err(HistoryFailure::NoDatahub),
            Some(remote) => load_remote(remote, &query, RPC_DEADLINE),
        };
        slot.put(ExecHistoryLoad { query, outcome });
        wake();
    });
    if let Err(e) = spawned {
        tracing::warn!("exec history: the OS refused the load thread: {e}");
    }
}

/// The load against a datahub: its `Welcome` first (what it serves), then [`load_exec_history`].
fn load_remote(
    remote: vike_datahub_client::RemoteHistStore,
    query: &HistoryQuery,
    deadline: Duration,
) -> Result<HistoryPage, HistoryFailure> {
    let remote = Arc::new(remote);
    let r = Arc::clone(&remote);
    let features = bounded("the datahub's handshake", deadline, move || r.advertised_features())
        .and_then(|got| got.map_err(|e| e.to_string()))
        .map_err(HistoryFailure::Unreadable)?;
    let store: Arc<dyn HistStore + Send + Sync> = remote;
    load_exec_history(&store, ExecServed::from_features(&features), query, deadline)
        .map_err(HistoryFailure::Unreadable)
}

/// THE load over any `HistStore` — the catalog, then the newest page of fills and (when served) of
/// order snapshots inside `query`'s bounds. Pure over the trait: the CI suite drives it over the
/// in-memory store double. An `Err` is a read that failed; an empty page is a real answer.
pub fn load_exec_history(
    store: &Arc<dyn HistStore + Send + Sync>,
    served: ExecServed,
    query: &HistoryQuery,
    deadline: Duration,
) -> Result<HistoryPage, String> {
    let s = Arc::clone(store);
    let inventory = bounded("the execution-history catalog", deadline, move || s.inventory())?
        .map_err(|e| format!("inventory scan: {e}"))?;
    let mut page = HistoryPage { served, ..HistoryPage::default() };
    let mut fill_series: Vec<(SeriesId, SeriesCoverage)> = Vec::new();
    let mut order_series: Vec<(SeriesId, SeriesCoverage)> = Vec::new();
    for (id, cov) in inventory {
        let wanted = query.venue.as_ref().is_none_or(|v| *v == id.venue)
            && query.symbol.as_ref().is_none_or(|s| *s == id.symbol);
        match id.kind {
            StoreKindId::ExecFill | StoreKindId::ExecOrder => {
                page.venues.entry(id.venue.clone()).or_default().insert(id.symbol.clone());
            }
            _ => continue,
        }
        if !wanted {
            continue;
        }
        if id.kind == StoreKindId::ExecFill {
            fill_series.push((id, cov))
        } else {
            order_series.push((id, cov))
        }
    }

    let (lower, upper) = query.bounds();
    let mut fills: Vec<ExecFillRow> = Vec::new();
    for (id, cov) in &fill_series {
        let read = |range: TsRange, n: usize| {
            let (s, v, sym) = (Arc::clone(store), id.venue.clone(), id.symbol.clone());
            bounded("an exec-fill read", deadline, move || {
                s.scan_exec_fills_head(&v, &sym, range, n)
            })?
            .map_err(|e| e.to_string())
        };
        let floor = lower.map_or(cov.first_ts, |l| l.max(cov.first_ts));
        if !served.fills_range {
            // A datahub older than the window answers every windowed read with the WHOLE series
            // (`RemoteHistStore` then cuts it), so the walk would fetch it once per window: read
            // it ONCE and cut here instead.
            let (s, v, sym) = (Arc::clone(store), id.venue.clone(), id.symbol.clone());
            let mut rows =
                bounded("an exec-fill read", deadline, move || s.scan_exec_fills(&v, &sym))?
                    .map_err(|e| e.to_string())?;
            rows.retain(|r| r.ts >= floor && r.ts <= upper);
            page.more |= cut_newest(&mut rows, |r| r.ts, HISTORY_PAGE_ROWS);
            fills.extend(rows);
            continue;
        }
        let (rows, more) = newest(&read, |r: &ExecFillRow| r.ts, floor, upper, HISTORY_PAGE_ROWS)?;
        page.more |= more;
        fills.extend(rows);
    }
    page.more |= cut_newest(&mut fills, |r| r.ts, HISTORY_PAGE_ROWS);
    page.fills = fills;

    if served.orders {
        let mut orders: Vec<ExecOrderRow> = Vec::new();
        for (id, cov) in &order_series {
            let read = |range: TsRange, n: usize| {
                let (s, v, sym) = (Arc::clone(store), id.venue.clone(), id.symbol.clone());
                bounded("an exec-order read", deadline, move || {
                    s.scan_exec_orders_head(&v, &sym, range, n)
                })?
                .map_err(|e| e.to_string())
            };
            let floor = lower.map_or(cov.first_ts, |l| l.max(cov.first_ts));
            let (rows, more) =
                newest(&read, |r: &ExecOrderRow| r.ts, floor, upper, HISTORY_PAGE_ROWS)?;
            page.more |= more;
            orders.extend(rows);
        }
        page.more |= cut_newest(&mut orders, |r| r.ts, HISTORY_PAGE_ROWS);
        page.orders = orders;
    }
    page.oldest_ts = page.fills.iter().map(|r| r.ts).chain(page.orders.iter().map(|r| r.ts)).min();
    Ok(page)
}

/// Sort `rows` newest first and keep about `page` of them, never splitting a `ts` (the rows of the
/// timestamp that straddles the cut are all kept). Says whether anything was cut.
fn cut_newest<T>(rows: &mut Vec<T>, ts: impl Fn(&T) -> i64, page: usize) -> bool {
    rows.sort_by_key(|r| std::cmp::Reverse(ts(r)));
    if rows.len() <= page || page == 0 {
        return false;
    }
    let edge = ts(&rows[page - 1]);
    let keep = rows.iter().position(|r| ts(r) < edge).unwrap_or(rows.len()).max(page);
    let cut = keep < rows.len();
    rows.truncate(keep);
    cut
}

/// The newest `budget` rows of `[floor, upper]`, newest first, walking backwards in [`WALK_DAYS`]
/// windows. The flag says older rows exist inside `[floor, upper]` beyond the ones returned.
fn newest<T>(
    read: &dyn Fn(TsRange, usize) -> Result<Vec<T>, String>,
    ts: fn(&T) -> i64,
    floor: i64,
    upper: i64,
    budget: usize,
) -> Result<(Vec<T>, bool), String> {
    let mut out: Vec<T> = Vec::new();
    let mut hi = upper;
    let mut step = 0usize;
    while hi >= floor && out.len() < budget {
        let span = WALK_DAYS[step.min(WALK_DAYS.len() - 1)].saturating_mul(DAY_MS);
        let lo = hi.saturating_sub(span).saturating_add(1).max(floor);
        let (rows, more) = window_newest(read, ts, lo, hi, budget - out.len())?;
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
    read: &dyn Fn(TsRange, usize) -> Result<Vec<T>, String>,
    ts: fn(&T) -> i64,
    lo: i64,
    hi: i64,
    need: usize,
) -> Result<(Vec<T>, bool), String> {
    if need == 0 {
        return Ok((Vec::new(), true));
    }
    let mut rows = read(TsRange { start: Some(lo), end: Some(hi) }, need.saturating_add(1))?;
    if rows.len() <= need || hi <= lo {
        rows.reverse();
        return Ok((rows, false));
    }
    let mid = lo + (hi - lo) / 2;
    let (mut newer, newer_more) = window_newest(read, ts, mid + 1, hi, need)?;
    if newer_more || newer.len() >= need {
        return Ok((newer, true));
    }
    let (older, older_more) = window_newest(read, ts, lo, mid, need - newer.len())?;
    newer.extend(older);
    debug_assert!(newer.windows(2).all(|w| ts(&w[0]) >= ts(&w[1])), "newest first");
    Ok((newer, older_more))
}

#[path = "exec_history_tests.rs"]
#[cfg(test)]
mod exec_history_tests;

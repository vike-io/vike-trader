//! What the Account window's History tab is HANDED: the question one load answers
//! ([`ExecutionHistoryQuery`]), the page it read ([`ExecutionHistoryPage`]: the account's own fills,
//! `kind=exec_fill`, and order-lifecycle snapshots, `kind=execution_order`, out of the DATAHUB's store,
//! newest first, over a time window), what the datahub serves of it ([`ExecutionHistoryServed`]), and the
//! slot a background load leaves its answer in ([`ExecutionHistorySlot`]); [`crate::history`] renders it.
//!
//! The READ is not this crate's: a widget opens no connection. `crates/vike-app-core/src/data/execution_history.rs`'s
//! `spawn_load` dials the datahub over the shell's dial, walks the store and leaves an [`ExecutionHistoryLoad`]
//! in the window's slot; its module doc is the argument for the walk (newest first over an ascending wire, every
//! RPC bounded). The load was this module's, as `history::load`, until 2026-11-22.
//!
//! # What a load can say
//!
//! "No datahub", "could not read" and "nothing in range" are three different answers and stay
//! three: [`ExecutionHistoryFailure`] for the first two, an EMPTY [`ExecutionHistoryPage`] for the third. What the
//! server serves rides in [`ExecutionHistoryServed`], read off its `Welcome`: a datahub older than the
//! order read answers fills only (and, older than the window, the whole fill series, which
//! `vike_datahub_client::HistoryStoreDatahub` then cuts on the reader's side), and one that records no
//! live executions says so through the absence of `exec_materialize`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use vike_data::{ExecFillRow, ExecOrderRow};
use vike_datahub_client::proto::{
    FEATURE_EXECUTIONS_RECORDED, FEATURE_SCAN_EXECUTION_FILLS_RANGE, FEATURE_SCAN_EXECUTION_ORDERS,
};

/// The most rows the window holds in memory across "Older" clicks. Reaching it is SAID on screen
/// ("Showing the newest … rows; narrow the range"), never a silent cut.
pub(crate) const HISTORY_MAX_ROWS: usize = 5_000;

/// The backwards walk's window sizes, in days (each `vike_model::MS_PER_DAY` long, the unit of the
/// time filters too); the last repeats until the series' first row. The filters are its first three
/// ([`ExecutionHistoryWindow::start`]); the walk that reads it is vike-app-core's
/// (`crates/vike-app-core/src/data/execution_history.rs`'s `newest`).
pub const WALK_DAYS: [i64; 5] = [1, 7, 30, 90, 365];

/// The time filter: how far back from now the history reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExecutionHistoryWindow {
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

impl ExecutionHistoryWindow {
    /// Every filter, in the order the control shows them.
    pub const ALL: [ExecutionHistoryWindow; 4] = [
        ExecutionHistoryWindow::Day,
        ExecutionHistoryWindow::Week,
        ExecutionHistoryWindow::Month,
        ExecutionHistoryWindow::All,
    ];

    /// The control's word.
    pub fn label(self) -> &'static str {
        match self {
            ExecutionHistoryWindow::Day => "24h",
            ExecutionHistoryWindow::Week => "7d",
            ExecutionHistoryWindow::Month => "30d",
            ExecutionHistoryWindow::All => "All",
        }
    }

    /// The inclusive lower bound for a window ending at `now_ms`; `None` for [`ExecutionHistoryWindow::All`].
    pub fn start(self, now_ms: i64) -> Option<i64> {
        let days = match self {
            ExecutionHistoryWindow::Day => WALK_DAYS[0],
            ExecutionHistoryWindow::Week => WALK_DAYS[1],
            ExecutionHistoryWindow::Month => WALK_DAYS[2],
            ExecutionHistoryWindow::All => return None,
        };
        Some(now_ms.saturating_sub(days * vike_model::MS_PER_DAY).saturating_add(1))
    }
}

/// What the datahub serves of this history, read off its `Welcome`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecutionHistoryServed {
    /// `scan_execution_fills_range`: fills are read over a window. Without it the whole fill series is
    /// read and cut on this side — correct, and as costly as the series is long.
    pub fills_range: bool,
    /// `scan_exec_orders`: the order-lifecycle read exists. Without it there is no Orders tab.
    pub orders: bool,
    /// `exec_materialize` (`FEATURE_EXECUTIONS_RECORDED`): the datahub records the live executions a
    /// trade hub streams, so they are written at all. ⚠ Its absence is AMBIGUOUS on a datahub too old
    /// for the other two, so a reader only concludes "not recording" when `orders` is served.
    pub recording: bool,
}

impl ExecutionHistoryServed {
    /// The three facts out of a `Welcome.features` list.
    pub fn from_features(features: &[String]) -> Self {
        let has = |f: &str| features.iter().any(|x| x == f);
        Self {
            fills_range: has(FEATURE_SCAN_EXECUTION_FILLS_RANGE),
            orders: has(FEATURE_SCAN_EXECUTION_ORDERS),
            recording: has(FEATURE_EXECUTIONS_RECORDED),
        }
    }

    /// The datahub predates the execution-history reads (no order read).
    pub fn predates_history(self) -> bool {
        !self.orders
    }

    /// The datahub is KNOWN to record no live executions: it is new enough to say, and does not.
    pub fn not_recording(self) -> bool {
        self.orders && !self.recording
    }
}

/// One load's question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionHistoryQuery {
    /// `None` = every venue with an exec series.
    pub venue: Option<String>,
    /// `None` = every symbol of the chosen venue(s).
    pub symbol: Option<String>,
    pub window: ExecutionHistoryWindow,
    /// The instant the window ends at, epoch-ms (taken once, when the load is asked for).
    pub now_ms: i64,
    /// `Some(ts)`: the "Older" page — only rows strictly older than `ts`.
    pub older_than: Option<i64>,
    /// The asking window's request counter, so a load that lands after a newer request is dropped
    /// instead of replacing fresher rows.
    pub generation: u64,
}

impl ExecutionHistoryQuery {
    /// The inclusive `[lower, upper]` this load reads; `lower` is `None` for an open start.
    pub fn bounds(&self) -> (Option<i64>, i64) {
        let upper = self.older_than.map_or(self.now_ms, |t| t.saturating_sub(1));
        (self.window.start(self.now_ms), upper)
    }
}

/// Why a load has no page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionHistoryFailure {
    /// No datahub address is configured or advertised: nothing was asked.
    NoDatahub,
    /// The datahub could not be read; the reason verbatim.
    Unreadable(String),
}

/// What one load read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecutionHistoryPage {
    /// Fills, NEWEST first.
    pub fills: Vec<ExecFillRow>,
    /// Order-lifecycle snapshots, NEWEST first — every snapshot, so a row can be expanded into its
    /// lifecycle; `vike_data::execution_index::latest_order_snapshots` collapses them.
    pub orders: Vec<ExecOrderRow>,
    /// What the datahub serves.
    pub served: ExecutionHistoryServed,
    /// Every venue with an exec series, and its symbols — the filters' choices, from the catalog
    /// and from nowhere in code.
    pub venues: BTreeMap<String, BTreeSet<String>>,
    /// Older rows exist inside the window beyond this page.
    pub more: bool,
    /// The oldest `ts` on the page — where "Older" continues from.
    pub oldest_timestamp: Option<i64>,
    /// Per `(venue, symbol)` series a loaded fill is on, its fills OLDER than every loaded fill, from the series'
    /// first row: what the Summary folds the position it opens with from (`crate::history::summary`), so an
    /// exit of an older position is measured against its cost basis. Read once per load, off the UI thread;
    /// the Fills list never shows them. A series the catalog says starts inside the page is not read.
    pub before: BTreeMap<(String, String), Vec<ExecFillRow>>,
    /// Why [`Self::before`] could not be read, when it could not: the Summary then refuses in words.
    pub before_unread: Option<String>,
    /// Every account's latest position snapshot at or before the oldest loaded fill
    /// (`vike_datahub_client::HistoryStoreDatahub::execution_account_positions`): what the Summary opens an account
    /// from, with only the fills journaled after it. Empty with no fill loaded, from a datahub that stores none, and
    /// when the read failed ([`Self::positions_unread`]).
    pub account_positions:
        Vec<vike_datahub_client::execution_stream::ExecutionAccountPositionsWire>,
    /// Why [`Self::account_positions`] could not be read, when it could not: the Summary then refuses in words.
    pub positions_unread: Option<String>,
}

impl ExecutionHistoryPage {
    /// Append an OLDER page below this one (an "Older" click). The newer page's catalog and served
    /// facts stay; the older page decides whether still more exist. A series' earlier fills read with
    /// the newer page hold the older page's ones (the reader keeps those older than its fills).
    pub fn extend_older(&mut self, older: ExecutionHistoryPage) {
        let moves_the_edge = !older.fills.is_empty();
        self.fills.extend(older.fills);
        self.orders.extend(older.orders);
        self.more = older.more;
        self.oldest_timestamp = older.oldest_timestamp.or(self.oldest_timestamp);
        for (series, rows) in older.before {
            self.before.entry(series).or_insert(rows);
        }
        self.before_unread = self.before_unread.take().or(older.before_unread);
        // The older page's fills move the edge back, and its snapshots are the ones at or before the new edge.
        if moves_the_edge {
            self.account_positions = older.account_positions;
            self.positions_unread = older.positions_unread;
        }
    }

    /// Rows held, both kinds.
    pub fn rows(&self) -> usize {
        self.fills.len() + self.orders.len()
    }
}

/// One finished load, as the window adopts it.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionHistoryLoad {
    pub query: ExecutionHistoryQuery,
    pub outcome: Result<ExecutionHistoryPage, ExecutionHistoryFailure>,
}

/// Where a background load leaves its answer for the window that asked — one slot per window, read
/// on its next frame — and how far the load in flight has read.
#[derive(Clone, Default)]
pub struct ExecutionHistorySlot {
    load: Arc<Mutex<Option<ExecutionHistoryLoad>>>,
    /// `(generation, series read, series to read)` of the load in flight; "to read" is 0 until its
    /// catalog is in.
    progress: Arc<Mutex<(u64, usize, usize)>>,
}

impl ExecutionHistorySlot {
    /// Leave `load` for the window (a newer put replaces an unread older one).
    pub fn put(&self, load: ExecutionHistoryLoad) {
        *vike_utility::mutex::lock(&self.load) = Some(load);
    }

    /// The landed load, if any.
    pub fn take(&self) -> Option<ExecutionHistoryLoad> {
        vike_utility::mutex::lock(&self.load).take()
    }

    /// How far the load asked as `generation` has read, `(series read, series to read)`; `None`
    /// before its catalog is in (or for another generation's load).
    pub fn progress(&self, generation: u64) -> Option<(usize, usize)> {
        let (of, read, total) = *vike_utility::mutex::lock(&self.progress);
        (of == generation && total > 0).then_some((read, total))
    }

    /// Say how far the load asked as `generation` has read: `read` of `total` series, `total` 0 until its
    /// catalog is in. The load calls it; [`Self::progress`] is the window's read.
    pub fn report(&self, generation: u64, read: usize, total: usize) {
        *vike_utility::mutex::lock(&self.progress) = (generation, read, total);
    }
}

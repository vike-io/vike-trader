//! The Stored grid's inventory WALK, spelled over the `HistoryStore` TRAIT — the load half of the
//! #1378 seam close (see [`crate::data::data_manager::mode`] for the decision half).
//!
//! One function walks a store's catalog into the Data-Manager's render inputs: `inventory()` →
//! [`vike_data_manager::build_inventory_tree`], then one `series_gaps` probe per listed series whose day count leaves room for a
//! gap (`may_have_gaps`) into a [`DataManagerInventoryGapMap`] (cheap —
//! manifest-only on both backends, no Parquet scan; over the wire it is one small RPC per series).
//! `crates/vike-desktop/src/app_methods.rs`'s `refresh_stored` calls [`DataManagerInventory::start`], which
//! walks a REMOTE `HistoryStoreDatahub` (the only arm left since the desktop lost its local core), and
//! the walk itself is CI-tested here against a seeded trait-store (which that binary, compile-checked
//! only, could never give it).
//!
//! # ⚠ The grid draws as soon as the INVENTORY lands
//!
//! The walk reports each answer as it arrives ([`DataManagerInventoryLoadEvent`]): the tree the moment the
//! inventory answers, then every gap probe and the coverage report one by one, then the whole load.
//! [`DataManagerInventory`] folds them on the frame thread, so the series list, its counts and its totals
//! draw while the probes are still out, each pending cell marked `checking…`
//! (`vike_data_manager::CHECKING_MARK`). It used to wait for the WHOLE load: over the owner's tunnel the
//! inventory alone took 1.5 to 4.5 s and the probes and the report seconds more, all of it spent on
//! *Loading stored data…* and 0 series. Nothing an answer brings moves a row already drawn: no sort
//! key reads a gap or a partial day, and a cell's mark takes the place of the answer's own.
//!
//! # Degrade contract
//!
//! - an inventory failure is fatal to the LOAD — there is no grid without it — and comes back as
//!   [`DataManagerInventoryLoadOutcome::error`], which the caller RENDERS as the reason rather than as an empty
//!   tree;
//! - a PER-SERIES gap-probe failure lists that series with an EMPTY gap list (logged here), which reads
//!   "not known" and never "gap-free" (`vike_data_manager::gaps_unknown`), rather than failing the
//!   whole refresh;
//! - an unanswerable coverage report degrades to a rendered NOTE
//!   (`crate::data::data_manager::mode::PARTIALS_UNSERVED`, or `crate::data::data_manager::mode::PARTIALS_OLDER_SERVER`
//!   for a server that predates the verb) while the tree still renders.
//!
//! # ⚠ THE WALK IS BOUNDED, and the two whole-store answers by PROGRESS
//!
//! A HANG produces no `Err` at all: `refresh_stored` spawns a DETACHED thread and drops the handle,
//! so a walk that never returns never sets a result, the loading flag never clears, and the window
//! sits on *Loading stored data…* for the life of the process. So every RPC of the walk is bounded,
//! in one of two ways.
//!
//! **The inventory and the coverage report are bounded by PROGRESS, not by a clock.** Their answers
//! are the whole store's (measured on the CI box, 2026-11-16: 112 KB and 332 KB of JSON for 493 series),
//! and a hard deadline cut a LIVE answer on a thin link: the owner's desktop, behind an ssh tunnel
//! measured at 13 to 40 KB/s with about 23% loss, reported "could not be read" after exactly 10 s
//! while the server took 3.2 to 5.2 s to start answering (a full manifest walk per request, since
//! answered from `crates/vike-store/src/datafusion_hist/inventory.rs`'s `CoverageMemo`). The load
//! builds its handles with `HistoryStoreDatahub::with_reply_timeout(RPC_DEADLINE)`: a socket read
//! timeout is per syscall, so the deadline restarts with every chunk that arrives and fires only after
//! `RPC_DEADLINE` with no byte at all. A store that cannot be dialled fails at the client's own
//! connect and handshake bounds; either way the answer is the honest "could not be read".
//!
//! **The gap probes keep a per-RPC thread bound** (`load::bounded`, the shape
//! [`crate::data::market_data_session`] uses for `market_data_subscribe`/`market_data_update`), because
//! their answers are a few bytes to a few KB and the dead-peer STREAK below needs to tell a probe that
//! went UNANSWERED from one the store REFUSED, which a failed socket read and a server's `Error` cannot
//! tell apart through the `HistoryStore` trait. The same idle bound sits under it, so an abandoned
//! probe frees its connection within `RPC_DEADLINE` instead of the client's 300 s default.
//!
//! **Why PER RPC and not per WALK.** The walk is one RPC per series, and
//! `crates/vike-datahub-client/src/remote.rs`'s connection model sends each one over the handle's
//! one connection — so the walk's duration scales with the CATALOG SIZE and no constant could be
//! both short and safe for it: short enough to make a dead peer honest is short enough to clip a
//! live store with a large catalog, and a clipped walk reports "unreachable" about a store that was
//! answering.
//! The RPCs themselves are all the same cheap class, and that class is MEASURED (2026-09-21, against
//! the datahub over loopback: `inventory` 1.0 s and `coverage` 1.01 s, each INCLUDING a CLI process
//! start, so the RPC is well under it; TCP connect through an SSH tunnel from a dev box, 9 ms). So
//! the unit that a constant can honestly bound is the RPC, and `RPC_DEADLINE` bounds exactly one.
//!
//! ⚠ **Do not reinstate "the store holds billions of rows, so loads are legitimately slow" as the
//! reason for a long bound.** It was believed, it was measured FALSE — the walk is manifest-only on
//! both backends and never touches a Parquet row — and it is what kept a short bound off this path
//! while the window hung.
//!
//! **Why a streak as well.** A per-RPC bound alone leaves `series × deadline`, which for a peer
//! that dies MID-walk (a tunnel dropping is the ordinary way) is hours rather than minutes. So the
//! probe loop also stops after `DEAD_PEER_STREAK` unanswered probes IN A ROW — the shape of the
//! failure rather than a second wall-clock number that would have to be traded off against catalog
//! size all over again. It caps the leaked helper threads at the same count.
//!
//! **What the caller gets.** The abandoned helper thread is detached and finishes whenever the peer
//! finally answers; its value is dropped (the socket inside it closes with it) because the channel
//! is a `sync_channel(1)` whose send can never block. The late answer is deliberately NOT adopted:
//! the `mpsc` this load rides carries no generation tag, so a straggler from an abandoned load
//! could land AFTER a newer load's result and overwrite fresh data with stale. Adopting late
//! answers needs that tag first, and it is not built here. Every [`DataManagerInventoryLoadEvent`] is sent by the
//! load thread or a thread it joins before it sends [`DataManagerInventoryLoadEvent::Done`], and
//! [`DataManagerInventory::start`] starts no second load while one runs, so none lands after its load's end.
//!
//! The cross-kind partial-day map is a SECOND fold (`load_partials`) rather than part of the
//! walk, but it is spelled over the trait exactly like the walk is: `coverage_report` used to be a
//! concrete `HistoryStoreDataFusion` method that only a local caller could reach, and spec §6-Q2 promoted
//! it onto the trait with a wire verb behind it, so BOTH callers run the same fold over whichever
//! store they hold. It stays a separate function because its failure mode is separate: an
//! unanswerable coverage report degrades to a NOTE while the tree still renders, whereas an
//! unanswerable inventory means there is no grid at all.

use crate::data::data_manager::mode::DatahubCoverage;
use crate::data::history_column::HistoryChannelColumnLoad;
use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;
use vike_data_manager::DataManagerInventoryVenue;
use vike_data_manager::{
    DataManagerInventoryGapMap, DataManagerInventoryPartialDayMap, DataManagerInventorySeriesKey,
};

pub(crate) mod load;
pub mod mode;

/// How long ONE gap probe gets to answer before the walk stops waiting on it, and how long ANY reply
/// of the walk's handles may go without a byte ([`load::load_remote_within`]'s idle bound, which is what bounds
/// the inventory and the coverage report) — the production value every caller passes; the functions
/// below take it as a PARAMETER so a test can drive the same code path in milliseconds (see this
/// module's `bounded`).
///
/// ⚠ **ADOPTED rather than invented, and sized against the failure rather than against the work.**
/// `crates/vike-hub-protocol/src/limits.rs` already bounds a client's two "is anybody there" legs —
/// `CLIENT_CONNECT_TIMEOUT` and `CLIENT_HANDSHAKE_DEADLINE` — at ten seconds each, with the
/// argument that a live peer completes them in milliseconds and the only thing that takes seconds
/// is a host that is not going to answer. A catalog RPC is in that family (see the module doc's
/// measurements: the whole answer is well under a second, over a tunnel whose connect is 9 ms), so
/// it takes that number rather than adding a fourth timing argument to this wire. This crate names
/// its own constant rather than reading one — the two bound different waits and may be tuned
/// apart, which is the same call `CLIENT_HANDSHAKE_DEADLINE`'s own doc makes about the server's.
pub(crate) const RPC_DEADLINE: Duration = Duration::from_secs(10);

/// How many gap probes may go unanswered IN A ROW before the walk stops probing and keeps the tree
/// it already has.
///
/// Three is chosen as the shape of the failure, not as a duration: a store answering a manifest
/// read in milliseconds does not miss three [`RPC_DEADLINE`]s consecutively, while a peer that has
/// gone away misses every one from now on. A single timeout stays a per-series skip, exactly as a
/// single `Err` does — one unreadable manifest must not end a walk over a catalog of thousands.
pub(crate) const DEAD_PEER_STREAK: usize = 3;

/// How many gap probes are in flight at once, once the peer has proven itself alive.
///
/// ⚠ **Measured 2026-10-04 against the real datahub through an SSH tunnel: 36 series, 135 ms per
/// probe, ONE AT A TIME — the walk took 4.8 s of a 5.9 s load**, because each probe is a full
/// round trip and one handle serialized them on its one connection (as `HistoryStoreDatahub` then did).
/// The probes are independent manifest reads, so they overlap: with this many in flight the same
/// walk costs a few round trips. It is a small number on purpose: each lane is a connection of its
/// own to a server that caps them, and the point is to hide the round trips, not to open a flood.
///
/// **A lane is a SEPARATE HANDLE.** `HistoryStoreDatahub` held ONE connection per handle and
/// serialized its readers on it until its pool (`crates/vike-datahub-client/src/remote.rs`'s
/// `POOL_CONNECTIONS`), so six workers on one handle queued behind a lock and finished exactly when
/// one worker would; the first version of this overlap did precisely that and measured no faster.
/// The caller hands `load_stored` this many handles, each dialing its own connection on first use,
/// which keeps the walk at one connection per lane.
///
/// The FIRST [`DEAD_PEER_STREAK`] probes still go one at a time, so a peer that answers nothing is
/// found out at exactly the cost it always was (that many deadlines, at most that many probes
/// reaching it); only a peer that has ANSWERED is trusted with the overlap.
pub(crate) const PROBE_WORKERS: usize = 6;

// A compile-time RANGE on the bound, the idiom of `crates/vike-datahub-client/src/client.rs` and of
// `crates/vike-telegram/src/confirm.rs`: a deliberate tweak stays free, while "the bound
// was effectively removed" and "the bound clips every healthy store" both fail to COMPILE instead
// of shipping. The upper end is the module doc's "seconds, not minutes" written down.
const _: () = assert!(
    RPC_DEADLINE.as_secs() > 0 && RPC_DEADLINE.as_secs() <= 60,
    "RPC_DEADLINE bounds ONE cheap catalog RPC: zero disables the bound the window hung without, \
     and a minute is longer than any measurement on this path justifies"
);
const _: () = assert!(
    PROBE_WORKERS > 0 && PROBE_WORKERS <= 16,
    "PROBE_WORKERS opens one connection each: zero probes nothing, and a flood is what the \
     datahub's connection cap exists to refuse"
);
const _: () = assert!(
    DEAD_PEER_STREAK > 0 && DEAD_PEER_STREAK <= 10,
    "DEAD_PEER_STREAK must abandon a dead peer (not zero, which would end a walk on one slow \
     probe) and must not be so long that series × RPC_DEADLINE is back"
);

/// Everything one background load produces, plus WHY it produced nothing when it did.
///
/// It is a struct rather than the tuple this used to be for one reason: [`Self::error`] is the
/// product that did not exist before, and a caller that renders an empty tree without it cannot
/// tell an EMPTY store from an UNREADABLE one — both draw an empty grid, which is half the defect
/// this module's bound closes. The field is the render's input, not just a log line.
pub struct DataManagerInventoryLoadOutcome {
    /// The venue-grouped inventory tree — EMPTY when [`Self::error`] is set.
    pub tree: Vec<DataManagerInventoryVenue>,
    /// Per-series gap ranges; a gap-free series gets no entry (an absent key already reads as "no
    /// known gaps"), and one whose probe failed, timed out or was never reached gets an EMPTY list.
    pub gaps: DataManagerInventoryGapMap,
    /// The cross-kind partial-day map (spec §6-Q2), empty when [`Self::coverage`] is not
    /// [`DatahubCoverage::Served`].
    pub partials: DataManagerInventoryPartialDayMap,
    /// What this load NEGOTIATED about the coverage verb — the render-time input to the Partial
    /// column's three states.
    pub coverage: DatahubCoverage,
    /// `Some(reason)` → the catalog could not be READ at all, and the reason is the one to show.
    /// `None` → the tree is the store's real answer, including when that answer is "nothing".
    pub error: Option<String>,
    /// The HISTORY column's answer (`load_history_channel_column`) — `None` from `load_stored` itself, which
    /// walks a `HistoryStore` and the history-channels read is not one of its verbs; the caller that
    /// holds the concrete datahub handle fills it on the same background thread.
    pub history: Option<HistoryChannelColumnLoad>,
}

/// One answer of a load, sent the moment it lands; [`DataManagerInventory::apply`] folds it.
pub enum DataManagerInventoryLoadEvent {
    /// The inventory answered: the tree, and every series whose gap probe is still to answer.
    Listed(Vec<DataManagerInventoryVenue>, BTreeSet<DataManagerInventorySeriesKey>),
    /// One series' gap probe ended: `None` when it answered whole, else its [`DataManagerInventoryGapMap`] entry (an
    /// EMPTY list reads "not known"), and the cause when it did not answer.
    Probed(DataManagerInventorySeriesKey, Option<Vec<(i64, i64)>>, Option<String>),
    /// The coverage report ended: the Partial column's map, what was negotiated, and the cause when
    /// it could not be read.
    Covered(DataManagerInventoryPartialDayMap, DatahubCoverage, Option<String>),
    /// The whole load as `load::load_stored` returned it, and the authority over every step before it.
    Done(DataManagerInventoryLoadOutcome),
}

/// What the running load has NOT answered yet, and why a pass that ended could not answer.
#[derive(Debug, Clone, Default)]
pub struct DataManagerInventoryLoadProgress {
    /// The series whose gap probe is still out: each row says `checking…`, and the Has-gaps count is
    /// not final until it is empty.
    pub(crate) gaps_pending: BTreeSet<DataManagerInventorySeriesKey>,
    /// The coverage report is still out: the Partial column says `checking…`.
    pub(crate) coverage_pending: bool,
    /// Why a gap probe did not answer (the last such), shown beside the series it left "not known".
    // shortcut: one cause for the whole pass, not one per series; keep one per key if probes of one
    // load start failing for different reasons.
    pub(crate) gaps_cause: Option<String>,
    /// Why the coverage report could not be read, shown beside the Partial column's note.
    pub coverage_cause: Option<String>,
}

/// **The Stored grid's inputs as a shell holds them**: the last load's answers, folded on the frame
/// thread as they land, and the one channel a running load sends them on. The shell calls
/// [`Self::start`] for a load and [`Self::drain`] once a frame, and renders the fields; a load's
/// answers replace the previous load's only as they arrive, so a Refresh keeps the old grid until the
/// new inventory lands.
pub struct DataManagerInventory {
    /// The venue-grouped inventory tree.
    pub tree: Vec<DataManagerInventoryVenue>,
    /// Per-series gap ranges, as [`DataManagerInventoryLoadOutcome::gaps`].
    pub gaps: DataManagerInventoryGapMap,
    /// The cross-kind partial-day map, as [`DataManagerInventoryLoadOutcome::partials`].
    pub partials: DataManagerInventoryPartialDayMap,
    /// What the coverage verb negotiated: [`DatahubCoverage::Unknown`] while the report is out.
    pub coverage: DatahubCoverage,
    /// The HISTORY column's answer, which lands with the whole load.
    pub history: Option<HistoryChannelColumnLoad>,
    /// `Some(reason)` → the last load could not READ the store (see [`DataManagerInventoryLoadOutcome::error`]).
    pub error: Option<String>,
    /// A load is running; [`Self::start`] starts no second one beside it.
    pub loading: bool,
    /// What the running load has not answered yet.
    pub progress: DataManagerInventoryLoadProgress,
    tx: Sender<DataManagerInventoryLoadEvent>,
    rx: Receiver<DataManagerInventoryLoadEvent>,
}

impl Default for DataManagerInventory {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            tree: Vec::new(),
            gaps: DataManagerInventoryGapMap::new(),
            partials: DataManagerInventoryPartialDayMap::new(),
            coverage: DatahubCoverage::Unknown,
            history: None,
            error: None,
            loading: false,
            progress: DataManagerInventoryLoadProgress::default(),
            tx,
            rx,
        }
    }
}

impl DataManagerInventory {
    /// Start one background load against the datahub `make` dials (one `HistoryStoreDatahub` handle
    /// per call — the shell's, which knows the address and the key), unless one is running. Every
    /// answer wakes `context`, so the frame that folds it follows at once.
    ///
    /// ⚠ The thread is DETACHED and nothing joins it, which is why every RPC of the walk is bounded
    /// (this module's doc): a walk that never returned would leave [`Self::loading`] set for the life
    /// of the process.
    pub fn start(
        &mut self,
        make: impl Fn() -> vike_datahub_client::HistoryStoreDatahub + Send + 'static,
        context: &egui::Context,
    ) {
        if self.loading {
            return;
        }
        self.loading = true;
        // A retry must not show the previous attempt's reason beside a load that may well succeed.
        self.error = None;
        let (tx, context) = (self.tx.clone(), context.clone());
        let spawned = std::thread::Builder::new().name("ui-stored-load".into()).spawn(move || {
            let emit = |event: DataManagerInventoryLoadEvent| {
                let _ = tx.send(event);
                context.request_repaint();
            };
            let load = load::load_remote_within(make, RPC_DEADLINE, &emit);
            emit(DataManagerInventoryLoadEvent::Done(load));
        });
        if let Err(e) = spawned {
            self.loading = false;
            self.error = Some(format!("the OS refused the stored-load thread: {e}"));
        }
    }

    /// Fold every answer that has landed since the last frame.
    pub fn drain(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.apply(event);
        }
    }

    /// Fold one answer. [`DataManagerInventoryLoadEvent::Listed`] replaces the previous load's tree and clears its
    /// gap and partial maps, which this load's answers then fill.
    pub fn apply(&mut self, event: DataManagerInventoryLoadEvent) {
        let progress = &mut self.progress;
        match event {
            DataManagerInventoryLoadEvent::Listed(tree, probing) => {
                self.tree = tree;
                self.gaps = DataManagerInventoryGapMap::new();
                self.partials = DataManagerInventoryPartialDayMap::new();
                self.coverage = DatahubCoverage::Unknown;
                *progress = DataManagerInventoryLoadProgress {
                    gaps_pending: probing,
                    coverage_pending: true,
                    ..Default::default()
                };
            }
            DataManagerInventoryLoadEvent::Probed(key, entry, cause) => {
                progress.gaps_pending.remove(&key);
                if let Some(ranges) = entry {
                    self.gaps.insert(key, ranges);
                }
                if cause.is_some() {
                    progress.gaps_cause = cause;
                }
            }
            DataManagerInventoryLoadEvent::Covered(partials, coverage, cause) => {
                (self.partials, self.coverage) = (partials, coverage);
                progress.coverage_pending = false;
                progress.coverage_cause = cause;
            }
            DataManagerInventoryLoadEvent::Done(load) => {
                self.tree = load.tree;
                self.gaps = load.gaps;
                self.partials = load.partials;
                self.coverage = load.coverage;
                self.history = load.history;
                self.error = load.error;
                self.loading = false;
                progress.gaps_pending.clear();
                progress.coverage_pending = false;
            }
        }
    }
}

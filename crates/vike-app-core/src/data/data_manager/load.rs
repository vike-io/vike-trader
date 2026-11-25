//! The Data Manager's inventory walk ([`load_stored`]): the bounded helper thread, the gap probes and
//! the coverage fold, and [`load_remote_within`], the handles a background load dials. The module
//! doc of `crates/vike-app-core/src/data/data_manager/mod.rs` is the contract.

use super::*;
use crate::data::history_column::load_history_channel_column;
use std::sync::Arc;
use std::time::Instant;
use vike_data::HistoryStore;
use vike_data_manager::{build_inventory_tree, partial_days_from_coverage};
use vike_utility::mutex::lock;
use vike_utility::thread::ThreadNoAnswer;

/// Run one blocking catalog RPC on a SHORT-LIVED helper thread and give up on it after `deadline`.
///
/// ⚠ **A HARD deadline, so only for a SMALL answer**: the gap probes and the reads that call it from
/// other modules. A whole-store answer is bounded by the handle's idle bound instead (the module
/// doc). Abandoning the helper is safe because it borrows NOTHING — it returns a value, and a value
/// nobody receives is dropped, closing the socket inside it.
///
/// The thread, the one-slot channel and the wait are `vike_utility::thread::bounded`, which the
/// market-data control exchange (`crates/vike-app-core/src/data/market_data_session/connection.rs`'s
/// `bounded`) also runs on; what stays here is this caller's thread name and its words. The
/// execution history's reads are the same cheap class against the same datahub, and called this one
/// until they moved into `vike-account-window`; back in this crate since 2026-11-22, their own `bounded`
/// (`crates/vike-app-core/src/data/execution_history.rs`) still names its own thread and words.
///
/// `deadline` is a parameter rather than [`RPC_DEADLINE`] read directly so the CI suite exercises
/// this exact code path in milliseconds. A ten-second unit test is a test somebody deletes, and a
/// bound proven only by the constant it reads is the shape of assertion that cannot fail for its
/// stated reason.
pub(crate) fn bounded<T: Send + 'static>(
    what: &str,
    deadline: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    vike_utility::thread::bounded("ui-data-manager", deadline, f).map_err(|e| match e {
        ThreadNoAnswer::NotStarted(e) => {
            format!("the OS refused the stored-catalog helper thread: {e}")
        }
        ThreadNoAnswer::TimedOut | ThreadNoAnswer::Panicked => {
            format!("{what} did not answer within {deadline:?}; the link is treated as down")
        }
    })
}

/// THE load: the walk, the §6-Q2 fold, the boundary logging and the degrade decisions, in the one
/// place a caller has to reach for. `store_kind` is a short LABEL for the log — "datahub", not an
/// address: a host, a path or a machine name in a log line is a string this workspace's publication
/// guard has to chase, and the kind is the half an operator reading the log actually needs.
///
/// Every binary that loads this grid calls THIS, not the two functions below, so the boundary log
/// lines and the degrade decisions cannot differ between callers. The two halves are
/// crate-private and separately tested (the walk alone is test-only), never a door of their own.
///
/// ⚠ **The coverage fold is SKIPPED when the walk failed**, and that is deliberate rather than an
/// early return somebody tidied: a peer that could not answer `inventory` — its first and cheapest
/// question — will not answer `coverage_report` either, and asking would spend a second
/// [`RPC_DEADLINE`] to learn what is already known. The Partial column reads
/// [`DatahubCoverage::Unknown`] for it: the verb was never asked, so no note may say the server did
/// not answer it, and the page already says the store could not be read.
///
/// `report` is the handle the coverage report is asked on, a connection of its own: it runs beside
/// the gap probes, and on `store` its whole-store answer would hold the warm-up probes' connection
/// (and their hard deadline) for as long as a thin link takes to carry it.
///
/// `emit` is handed each answer the moment it lands (the module doc's "draws as soon as the
/// INVENTORY lands"); the returned outcome is the same answers, whole.
pub(crate) fn load_stored(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    lanes: &[Arc<dyn HistoryStore + Send + Sync>],
    report: &Arc<dyn HistoryStore + Send + Sync>,
    store_kind: &str,
    deadline: Duration,
    emit: &(dyn Fn(DataManagerInventoryLoadEvent) + Sync),
) -> DataManagerInventoryLoadOutcome {
    let started = Instant::now();
    // ⚠ The load used to be COMPLETELY silent — a full run at `vike_app_core=debug` logged not one
    // line about it, which is why "hung", "empty" and "slow" were indistinguishable from outside
    // and cost a whole debugging session. These three lines (start, the walk's own detail below,
    // finish-with-elapsed) are the half that makes the other two diagnosable.
    tracing::info!(store = store_kind, "stored catalog: load starting");
    // The inventory first and alone (its failure is the whole load's), then the gap probes and the
    // coverage report TOGETHER: they are independent manifest reads, and the report used to wait
    // for the last probe to finish. A failed inventory still skips the coverage fold — see the
    // note on this function.
    let (tree, gaps, error, (partials, coverage, _)) = match inventory(store) {
        Ok(inventory_value) => {
            let tree = build_inventory_tree(inventory_value.clone());
            let probing = inventory_value.iter().filter(|(_, c)| may_have_gaps(c));
            emit(DataManagerInventoryLoadEvent::Listed(
                tree.clone(),
                probing.map(|(id, _)| gap_key(id)).collect(),
            ));
            let (gaps, covered) = std::thread::scope(|scope| {
                let fold = scope.spawn(|| {
                    let covered = coverage_answer(load_partials(report), store_kind);
                    let (map, coverage, cause) = covered.clone();
                    emit(DataManagerInventoryLoadEvent::Covered(map, coverage, cause));
                    covered
                });
                let gaps = probe_gaps(store, lanes, &inventory_value, deadline, emit);
                let covered = fold.join().unwrap_or_else(|_| {
                    let panicked =
                        vike_data::HistoryStoreError::Query("the coverage fold panicked".into());
                    coverage_answer(Err(panicked), store_kind)
                });
                (gaps, covered)
            });
            (tree, gaps, None, covered)
        }
        Err(e) => {
            tracing::warn!(
                store = store_kind,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "stored catalog: the walk could not be read: {e}"
            );
            (
                Vec::new(),
                DataManagerInventoryGapMap::new(),
                Some(e),
                (DataManagerInventoryPartialDayMap::new(), DatahubCoverage::Unknown, None),
            )
        }
    };
    tracing::info!(
        store = store_kind,
        elapsed_ms = started.elapsed().as_millis() as u64,
        venues = tree.len(),
        gap_series = gaps.len(),
        partial_instruments = partials.len(),
        // The word an operator greps for. "unreadable" is the state the grid now NAMES on screen;
        // "read" covers a genuinely empty store, which is a different fact and must read as one.
        outcome = if error.is_some() { "unreadable" } else { "read" },
        "stored catalog: load finished"
    );
    DataManagerInventoryLoadOutcome { tree, gaps, partials, coverage, error, history: None }
}

/// The coverage report's answer as the Partial column reads it: the map, what was negotiated, and
/// the cause when the report could not be read — logged here, once.
fn coverage_answer(
    answer: Result<DataManagerInventoryPartialDayMap, vike_data::HistoryStoreError>,
    store_kind: &str,
) -> (DataManagerInventoryPartialDayMap, DatahubCoverage, Option<String>) {
    match answer {
        Ok(map) => (map, DatahubCoverage::Served, None),
        // The capability refusal of a server older than the verb (refused CLIENT-side, nothing
        // sent): the column says the server is older, and guesses nothing else.
        Err(e @ vike_data::HistoryStoreError::Unsupported(_)) => {
            tracing::info!(store = store_kind, "stored catalog: no coverage report: {e}");
            (DataManagerInventoryPartialDayMap::new(), DatahubCoverage::Older, Some(e.to_string()))
        }
        Err(e) => {
            // A read that failed or went silent: the honest note, never an empty map claiming
            // "nothing is partial".
            tracing::warn!(store = store_kind, "stored catalog: the coverage report failed: {e}");
            (
                DataManagerInventoryPartialDayMap::new(),
                DatahubCoverage::Unserved,
                Some(e.to_string()),
            )
        }
    }
}

/// **The whole background load a desktop runs for the Stored grid against a datahub**
/// ([`DataManagerInventory::start`]'s thread): the handles, the walk (`load_stored`) and the history read
/// ([`load_history_channel_column`]), with its idle window as a parameter so the tests drive the real socket path in
/// milliseconds. `make` builds one `HistoryStoreDatahub` handle — the shell's, which knows the address
/// and the key — and is called once for the serial work, once for the coverage report and once per
/// lane; every handle is given `window` ([`RPC_DEADLINE`] in production) as its idle reply bound (the
/// module doc's progress rule).
///
/// The walk's overlapped probes run on handles of their own, one connection each ([`PROBE_WORKERS`]'
/// doc has the measurement and why); each dials on first use. The history-channels read is not a
/// `HistoryStore` verb, so it is asked through a CONCRETE
/// handle's own inherent method, with the same device key (decision 0102), on a connection of its
/// own BESIDE the walk: it is the server's slowest catalog answer (0.9 s on the CI box, 2026-11-17) and it
/// used to start after the walk's last probe. Its answer is dropped when the walk could not read the
/// store, whose first and cheapest question this was not.
pub(super) fn load_remote_within(
    make: impl Fn() -> vike_datahub_client::HistoryStoreDatahub,
    window: Duration,
    emit: &(dyn Fn(DataManagerInventoryLoadEvent) + Sync),
) -> DataManagerInventoryLoadOutcome {
    let dial = || make().with_reply_timeout(window);
    let handle = || Arc::new(dial()) as Arc<dyn HistoryStore + Send + Sync>;
    let lanes: Vec<_> = (0..PROBE_WORKERS).map(|_| handle()).collect();
    let remote = dial();
    let (mut load, history) = std::thread::scope(|scope| {
        let history = scope.spawn(move || {
            load_history_channel_column(
                move || remote.history_channels().map_err(|e| e.to_string()),
                vike_model::now_ms(),
                window,
            )
        });
        // ⚠ The LABEL, never the address: a host, a port or a path in a log line is a string
        // `scripts/publish_mirror.sh`'s forbidden-token scan has to chase.
        let load = load_stored(&handle(), &lanes, &handle(), "datahub", window, emit);
        (load, history.join().ok().flatten())
    });
    if load.error.is_none() {
        load.history = history;
    }
    load
}

/// Walk `store`'s catalog through the TRAIT verbs into the Stored grid's
/// `(tree, per-series gap map)` — see the module doc for the shared-walk argument, the degrade
/// contract and why every RPC below is bounded. A gap-free series gets NO map entry (an absent key
/// already reads as "no known gaps").
///
/// Test-only: it is the walk half of [`load_stored`] (`inventory`, the gap probes, `build_inventory_tree`),
/// spelled alone so the tests can drive it; [`load_stored`] runs the same three steps with the
/// coverage fold overlapped, and no caller outside the tests may reach the walk without it.
#[cfg(test)]
fn load_stored_tree(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    lanes: &[Arc<dyn HistoryStore + Send + Sync>],
    deadline: Duration,
) -> Result<(Vec<DataManagerInventoryVenue>, DataManagerInventoryGapMap), String> {
    let built_inventory = inventory(store)?;
    let gaps = probe_gaps(store, lanes, &built_inventory, deadline, &|_| {});
    Ok((build_inventory_tree(built_inventory), gaps))
}

/// What `HistoryStore::inventory` answers: every series with its coverage.
type Inventory = Vec<(vike_data::SeriesId, vike_data::SeriesCoverage)>;

/// The reachability question, and the one whose failure means there is no grid at all. It is
/// asked FIRST and alone: a peer that does not answer this answers nothing, and every probe after
/// it would otherwise spend its own deadline learning that again. No thread bounds it: its answer
/// is the whole store's, so the handle's idle bound does ([`load_remote_within`]), and a slow link
/// that keeps delivering is never cut.
///
/// ⚠ **ACCOUNT series are dropped here, before the tree, the gap probes and the counts see them**
/// (every `vike_model::StorePlane::Account` kind of `vike_model::StoreKindId::plane`). The Data
/// Manager is the MARKET plane (§9.3.2 of `docs/superpowers/specs/2026-09-20-cli-data-surface-design.md`,
/// the same split the CLI's `data history ls` makes); an account's own executions are the
/// Account window's History tab's, which reads them through `crates/vike-app-core/src/data/execution_history.rs` (the owner's ruling of
/// 2026-10-11). Until then they showed in the grid as rows of market data.
fn inventory(store: &Arc<dyn HistoryStore + Send + Sync>) -> Result<Inventory, String> {
    let mut inventory_value =
        store.inventory().map_err(|e| format!("the stored catalog's inventory: {e}"))?;
    inventory_value.retain(|(id, _)| id.kind.plane() != vike_model::StorePlane::Account);
    tracing::debug!(
        series = inventory_value.len(),
        "stored catalog: inventory answered; probing gaps"
    );
    Ok(inventory_value)
}

/// The grid's key of series `id`.
///
/// ⚠ `label()`, NOT `symbol`. A GROUPED series carries an EMPTY `symbol` — it holds many, told apart
/// by a row column — and its identity on disk and in the display tree is its GROUP NAME.
/// `crates/vike-data-manager/src/inventory/mod.rs`'s `build_inventory_tree` keys the tree on `id.label()` for exactly
/// that reason, and the grid's own `series_key` looks up this map with the node label it got from there.
///
/// Keyed on `symbol`, every grouped series produced a key that matched no row, so the Stored grid
/// painted every polymarket group as gap-FREE and the "Has gaps" smart view could never list one. No
/// test caught it: every gap test builds its fixture with `SeriesId::per_symbol`, where
/// `symbol == label` and the two spellings are indistinguishable. `grouped` and `source` carry the
/// rest of the identity, so a grouped and a per-symbol series of one label keep two entries.
fn gap_key(id: &vike_data::SeriesId) -> vike_data_manager::DataManagerInventorySeriesKey {
    vike_data_manager::DataManagerInventorySeriesKey {
        venue: id.venue.clone(),
        symbol: id.label().to_string(),
        kind: id.kind,
        interval: id.interval.clone(),
        grouped: id.group.is_some(),
        source: id.source.clone(),
    }
}

/// Whether some day of the series' span may hold no part, the only way a gap probe answers a gap: a
/// store gap is a missing UTC day between the first and the last (a trading calendar only drops
/// some). `dates` counts the days that hold a part, so `dates` equal to the span's days rules one
/// out. A store that counts no days (`dates == 0`: the in-memory double) is probed as before.
///
/// ⚠ **The probe of such a series was a round trip for nothing**: 632 of the CI box's 820 series
/// (2026-11-17), each a round trip through the owner's tunnel, where the walk took 8.7 s.
fn may_have_gaps(coverage: &vike_data::SeriesCoverage) -> bool {
    let day = |ms: i64| ms.div_euclid(vike_model::MS_PER_DAY);
    coverage.dates == 0
        || (coverage.dates as i64)
            < day(coverage.last_timestamp) - day(coverage.first_timestamp) + 1
}

/// One series' gap probe, bounded, and what to do with its answer. Shared by the one-at-a-time
/// warm-up and the overlapped rest, so the two cannot disagree about what an answer, a refusal
/// and a silence mean. A series that answered whole gets no entry; ⚠ one that did not answer gets an
/// EMPTY list, which reads "not known": its absence would read as gap-free.
///
/// Returns `true` when the probe went UNANSWERED (the streak's unit); a refusal is an answer. The
/// answer is also handed to `emit`, with the cause when there was no answer to give.
fn probe_one(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    id: &vike_data::SeriesId,
    deadline: Duration,
    gaps: &std::sync::Mutex<DataManagerInventoryGapMap>,
    emit: &(dyn Fn(DataManagerInventoryLoadEvent) + Sync),
) -> bool {
    // One `SeriesId` clone per probe, because the helper thread needs an owned value — the price of
    // bounding the call at all, and small beside the round trip it is bounding.
    let (s, wanted) = (Arc::clone(store), id.clone());
    let (entry, cause, unanswered) =
        match bounded("a stored-catalog gap probe", deadline, move || s.series_gaps(&wanted)) {
            Ok(Ok(ranges)) => ((!ranges.is_empty()).then_some(ranges), None, false),
            Ok(Err(e)) => {
                tracing::warn!(
                    "stored catalog: series_gaps({}/{}/{}) failed: {e}",
                    id.venue,
                    id.symbol,
                    id.kind
                );
                (Some(Vec::new()), Some(e.to_string()), false)
            }
            Err(deadline_hit) => {
                tracing::warn!(
                    "stored catalog: series_gaps({}/{}/{}) {deadline_hit}",
                    id.venue,
                    id.symbol,
                    id.kind
                );
                (Some(Vec::new()), Some(deadline_hit), true)
            }
        };
    if let Some(ranges) = &entry {
        lock(gaps).insert(gap_key(id), ranges.clone());
    }
    emit(DataManagerInventoryLoadEvent::Probed(gap_key(id), entry, cause));
    unanswered
}

/// One `series_gaps` probe per listed series into a [`DataManagerInventoryGapMap`]: the first [`DEAD_PEER_STREAK`] one
/// at a time, the rest [`PROBE_WORKERS`] at a time once the peer has answered. See those two
/// constants for why the walk is shaped this way.
///
/// The dead-peer rule is unchanged in what it MEANS: [`DEAD_PEER_STREAK`] probes unanswered IN A
/// ROW stop the walk and the remaining series read "not known". With the overlap, probes already in
/// flight when the streak is reached finish (at most [`PROBE_WORKERS`] of them, each bounded by its
/// own deadline) rather than being cancelled; an ANSWER, a refusal included, resets the streak.
///
/// A series whose parts cover every day of its span ([`may_have_gaps`]) is not probed: its answer
/// would be no gap, and it gets none. Every series probed or skipped by the streak is handed to
/// `emit` once, so the caller's `checking…` set empties exactly when this returns.
fn probe_gaps(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    lanes: &[Arc<dyn HistoryStore + Send + Sync>],
    inventory: &Inventory,
    deadline: Duration,
    emit: &(dyn Fn(DataManagerInventoryLoadEvent) + Sync),
) -> DataManagerInventoryGapMap {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let inventory: Inventory =
        inventory.iter().filter(|(_, coverage)| may_have_gaps(coverage)).cloned().collect();
    let inventory = &inventory;
    let gaps = std::sync::Mutex::new(DataManagerInventoryGapMap::new());
    // Unanswered probes IN A ROW — reset by any answer, refusal included (a store that REFUSES is
    // a store that is talking). See [`DEAD_PEER_STREAK`].
    let streak = AtomicUsize::new(0);
    let stopped = |probed: usize, streak: usize| {
        tracing::warn!(
            probed,
            series = inventory.len(),
            streak,
            "stored catalog: {DEAD_PEER_STREAK} gap probes in a row went unanswered — the store \
             stopped answering mid-walk, so the remaining series read 'not checked'. The TREE is \
             still complete and still renders, which is why this is a warning rather than a failed \
             load"
        );
        // A series nobody asked about is not a whole one.
        let cause = format!(
            "{DEAD_PEER_STREAK} gap probes in a row went unanswered, so the store is treated as gone \
             and the series after them were not asked"
        );
        let mut known = lock(&gaps);
        for (id, _coverage) in inventory.iter().skip(probed) {
            known.entry(gap_key(id)).or_default();
            emit(DataManagerInventoryLoadEvent::Probed(
                gap_key(id),
                Some(Vec::new()),
                Some(cause.clone()),
            ));
        }
    };
    let warm = DEAD_PEER_STREAK.min(inventory.len());
    for (probed, (id, _coverage)) in inventory.iter().take(warm).enumerate() {
        if streak.load(Ordering::Acquire) >= DEAD_PEER_STREAK {
            stopped(probed, DEAD_PEER_STREAK);
            return gaps.into_inner().unwrap_or_else(|e| e.into_inner());
        }
        if probe_one(store, id, deadline, &gaps, emit) {
            streak.fetch_add(1, Ordering::AcqRel);
        } else {
            streak.store(0, Ordering::Release);
        }
    }
    if streak.load(Ordering::Acquire) >= DEAD_PEER_STREAK {
        stopped(warm, DEAD_PEER_STREAK);
        return gaps.into_inner().unwrap_or_else(|e| e.into_inner());
    }
    // The peer has answered: overlap the rest, one worker per lane (a handle with a connection of
    // its own — see [`PROBE_WORKERS`]; with none supplied the one handle does it alone). Each worker
    // takes the next unprobed series until the list is spent or the streak says the peer went away.
    let handles: Vec<&Arc<dyn HistoryStore + Send + Sync>> =
        if lanes.is_empty() { vec![store] } else { lanes.iter().take(PROBE_WORKERS).collect() };
    let next = AtomicUsize::new(warm);
    std::thread::scope(|scope| {
        for lane in handles.iter().take(inventory.len().saturating_sub(warm)) {
            let (lane, next, streak, gaps) = (*lane, &next, &streak, &gaps);
            scope.spawn(move || {
                loop {
                    if streak.load(Ordering::Acquire) >= DEAD_PEER_STREAK {
                        return;
                    }
                    let i = next.fetch_add(1, Ordering::AcqRel);
                    let Some((id, _coverage)) = inventory.get(i) else { return };
                    if probe_one(lane, id, deadline, gaps, emit) {
                        streak.fetch_add(1, Ordering::AcqRel);
                    } else {
                        streak.store(0, Ordering::Release);
                    }
                }
            });
        }
    });
    if streak.load(Ordering::Acquire) >= DEAD_PEER_STREAK {
        stopped(next.load(Ordering::Acquire).min(inventory.len()), DEAD_PEER_STREAK);
    }
    gaps.into_inner().unwrap_or_else(|e| e.into_inner())
}

/// The Stored grid's cross-kind PARTIAL-day map, walked through the TRAIT verb — the §6-Q2 sibling
/// of the walk (`load_stored_tree`), and the one fold BOTH the local and the remote arm now run.
///
/// `store.coverage_report()` is a manifest fold on either backend (no Parquet scan, one small RPC
/// over the wire), and [`partial_days_from_coverage`] is the same pure rendering step in both
/// cases — which is exactly the property worth having: the map a remote grid draws is folded from
/// the same value type, by the same function, as the map a local grid draws. There is no second
/// shape for the two modes to drift apart in.
///
/// An `Err` means the store could not answer AT ALL — over the wire, that is
/// [`vike_data::HistoryStoreError::Unsupported`] from a server older than the verb (refused before
/// anything is sent; `HistoryStoreDatahub` surfaces it rather than inheriting the trait's empty
/// default, precisely so this stays distinguishable), or a read that failed or went silent past the
/// handle's idle bound. The caller renders a note for it and keeps the rest of the grid; it must
/// NOT substitute an empty map, which would claim "nothing is partial". Like the inventory, a
/// whole-store answer, so no thread bounds it (the module doc).
pub(crate) fn load_partials(
    store: &Arc<dyn HistoryStore + Send + Sync>,
) -> Result<DataManagerInventoryPartialDayMap, vike_data::HistoryStoreError> {
    Ok(partial_days_from_coverage(&store.coverage_report()?))
}

#[path = "load_tests.rs"]
#[cfg(test)]
mod load_tests;

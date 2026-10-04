//! The Stored grid's inventory WALK, spelled over the `HistStore` TRAIT — the load half of the
//! #1378 seam close (see [`crate::data::stored_mode`] for the decision half).
//!
//! One function walks a store's catalog into the Data-Manager's render inputs: `inventory()` →
//! [`build_tree`], then one `series_gaps` probe per listed series into a [`GapMap`] (cheap —
//! manifest-only on both backends, no Parquet scan; over the wire it is one small RPC per series).
//! `crates/vike-desktop/src/app_methods.rs`'s `refresh_stored` calls [`load_stored`] with a REMOTE
//! `RemoteHistStore` (the only arm left since the desktop lost its local core), and the walk itself
//! is CI-tested here against a seeded trait-store (which that binary, compile-checked only, could
//! never give it).
//!
//! # Degrade contract
//!
//! - an inventory failure is fatal to the LOAD — there is no grid without it — and comes back as
//!   [`StoredLoadOutcome::error`], which the caller RENDERS as the reason rather than as an empty
//!   tree;
//! - a PER-SERIES gap-probe failure skips that one series' entry (logged here) rather than failing
//!   the whole refresh;
//! - an unanswerable coverage report degrades to a rendered NOTE
//!   ([`crate::data::stored_mode::PARTIALS_UNSERVED`]) while the tree still renders.
//!
//! # ⚠ THE WALK IS BOUNDED HERE, because the client crate deliberately does not bound it
//!
//! **That contract used to be unsatisfiable in the commonest failure, and this section is the fix.**
//! A HANG produces no `Err` at all: `refresh_stored` spawns a DETACHED thread and drops the handle,
//! so a walk that never returns never sets a result, the loading flag never clears, and the window
//! sits on *Loading stored data…* for the life of the process. There is no `Err` for the caller to
//! log and no empty tree to render — the degrade contract above simply never runs. In this tree a
//! doc promising behaviour the code cannot deliver IS the defect, so the bound lives here, beside
//! the promise, rather than being left to a caller that has no way to take it.
//!
//! **Why not a socket option.** `crates/vike-datahub-client/src/client.rs`'s
//! `DatahubClient::arm_request_timeouts` CLEARS the post-handshake read timeout, and its own module
//! doc argues why: past the handshake a read waits on SERVER-SIDE COMPUTATION — a paramscan, a
//! walk-forward, a backfill — whose legitimate duration that crate cannot name, and the rule it
//! cites is that clipping a slow-but-live client is worse than leaking a thread. That choice is
//! PINNED by `arm_request_timeouts_clears_the_read_and_bounds_the_write`, so reaching in would fail
//! a test rather than production. **The catalog verbs are not in that family** — `inventory`,
//! `series_gaps` and `coverage_report` are manifest reads that COMPUTE nothing, which is the exact
//! argument that crate uses to refuse a ceiling on the verbs that do. This is the same split
//! [`crate::data::md_session`] already makes for `md_subscribe`/`md_update`, and the same answer: a
//! SHORT-LIVED helper thread the caller `recv_timeout`s, because the socket option is not ours to
//! set. The narrower fix belongs upstream — a bounded reply read for the catalog verbs, or a hatch
//! to re-arm the read timeout for one verb — and is REPORTED rather than taken here.
//!
//! **Why PER RPC and not per WALK.** The walk is one RPC per series, and
//! `crates/vike-datahub-client/src/remote.rs`'s connection model dials a FRESH client for each one
//! — so the walk's duration scales with the CATALOG SIZE and no constant could be both short and
//! safe for it: short enough to make a dead peer honest is short enough to clip a live store with a
//! large catalog, and a clipped walk reports "unreachable" about a store that was answering. The
//! RPCs themselves are all the same cheap class, and that class is MEASURED (2026-09-21, against
//! the datahub over loopback: `inventory` 1.0 s and `coverage` 1.01 s, each INCLUDING a CLI process
//! start, so the RPC is well under it; TCP connect through an SSH tunnel from a dev box, 9 ms). So
//! the unit that a constant can honestly bound is the RPC, and [`RPC_DEADLINE`] bounds exactly one.
//!
//! ⚠ **Do not reinstate "the store holds billions of rows, so loads are legitimately slow" as the
//! reason for a long bound.** It was believed, it was measured FALSE — the walk is manifest-only on
//! both backends and never touches a Parquet row — and it is what kept a short bound off this path
//! while the window hung.
//!
//! **Why a streak as well.** A per-RPC bound alone leaves `series × deadline`, which for a peer
//! that dies MID-walk (a tunnel dropping is the ordinary way) is hours rather than minutes. So the
//! probe loop also stops after [`DEAD_PEER_STREAK`] unanswered probes IN A ROW — the shape of the
//! failure rather than a second wall-clock number that would have to be traded off against catalog
//! size all over again. It caps the leaked helper threads at the same count.
//!
//! **What the caller gets.** The abandoned helper thread is detached and finishes whenever the peer
//! finally answers; its value is dropped (the socket inside it closes with it) because the channel
//! is a `sync_channel(1)` whose send can never block. The late answer is deliberately NOT adopted:
//! the `mpsc` this load rides carries no generation tag, so a straggler from an abandoned load
//! could land AFTER a newer load's result and overwrite fresh data with stale. Adopting late
//! answers needs that tag first, and it is not built here.
//!
//! The cross-kind partial-day map is a SECOND fold ([`load_partials`]) rather than part of the
//! walk, but it is spelled over the trait exactly like the walk is: `coverage_report` used to be a
//! concrete `DataFusionHist` method that only a local caller could reach, and spec §6-Q2 promoted
//! it onto the trait with a wire verb behind it, so BOTH callers run the same fold over whichever
//! store they hold. It stays a separate function because its failure mode is separate: an
//! unanswerable coverage report degrades to a NOTE while the tree still renders, whereas an
//! unanswerable inventory means there is no grid at all.

use crate::data::history_column::HistoryLoad;
use crate::data::stored_mode::RemoteCoverage;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vike_data::HistStore;
use vike_data_manager::model::VenueNode;
use vike_data_manager::{GapMap, PartialDayMap, build_tree, partial_days_from_coverage};
use vike_datahub_client::history::HistoryChannelsReport;

/// How long ONE catalog RPC gets to answer before the walk stops waiting on it — the production
/// value every caller passes; the functions below take it as a PARAMETER so a test can drive the
/// same code path in milliseconds (see this module's `bounded`).
///
/// ⚠ **ADOPTED rather than invented, and sized against the failure rather than against the work.**
/// `crates/vike-datahub-client/src/client.rs` already bounds its two "is anybody there" legs —
/// `CONNECT_TIMEOUT` and `HANDSHAKE_DEADLINE` — at ten seconds each, with the argument that a live
/// peer completes them in milliseconds and the only thing that takes seconds is a host that is not
/// going to answer. A catalog RPC is in that family (see the module doc's measurements: the whole
/// answer is well under a second, over a tunnel whose connect is 9 ms), so it takes that crate's
/// number rather than adding a fourth timing argument to this wire. This crate names its own
/// constant rather than importing one — they are private there, and the two bound different waits
/// and may be tuned apart, which is the same call `HANDSHAKE_DEADLINE`'s own doc makes about the
/// server's.
pub const RPC_DEADLINE: Duration = Duration::from_secs(10);

/// How many gap probes may go unanswered IN A ROW before the walk stops probing and keeps the tree
/// it already has.
///
/// Three is chosen as the shape of the failure, not as a duration: a store answering a manifest
/// read in milliseconds does not miss three [`RPC_DEADLINE`]s consecutively, while a peer that has
/// gone away misses every one from now on. A single timeout stays a per-series skip, exactly as a
/// single `Err` does — one unreadable manifest must not end a walk over a catalog of thousands.
pub const DEAD_PEER_STREAK: usize = 3;

// A compile-time RANGE on the bound, the idiom of `crates/vike-datahub-client/src/client.rs` and of
// `crates/vike-tradehub/src/telegram/confirm.rs`: a deliberate tweak stays free, while "the bound
// was effectively removed" and "the bound clips every healthy store" both fail to COMPILE instead
// of shipping. The upper end is the module doc's "seconds, not minutes" written down.
const _: () = assert!(
    RPC_DEADLINE.as_secs() > 0 && RPC_DEADLINE.as_secs() <= 60,
    "RPC_DEADLINE bounds ONE cheap catalog RPC: zero disables the bound the window hung without, \
     and a minute is longer than any measurement on this path justifies"
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
pub struct StoredLoadOutcome {
    /// The venue-grouped inventory tree — EMPTY when [`Self::error`] is set.
    pub tree: Vec<VenueNode>,
    /// Per-series gap ranges; a gap-free series gets no entry (an absent key already reads as "no
    /// known gaps"), and so does one whose probe failed or timed out.
    pub gaps: GapMap,
    /// The cross-kind partial-day map (spec §6-Q2), empty when [`Self::coverage`] is not
    /// [`RemoteCoverage::Served`].
    pub partials: PartialDayMap,
    /// What this load NEGOTIATED about the coverage verb — the render-time input to the Partial
    /// column's three states.
    pub coverage: RemoteCoverage,
    /// `Some(reason)` → the catalog could not be READ at all, and the reason is the one to show.
    /// `None` → the tree is the store's real answer, including when that answer is "nothing".
    pub error: Option<String>,
    /// The HISTORY column's answer ([`load_history`]) — `None` from [`load_stored`] itself, which
    /// walks a `HistStore` and the history-channels read is not one of its verbs; the caller that
    /// holds the concrete datahub handle fills it on the same background thread.
    pub history: Option<HistoryLoad>,
}

/// Run one blocking catalog RPC on a SHORT-LIVED helper thread and give up on it after `deadline`.
///
/// ⚠ **This is the module doc's bounded-RPC rule, and it is a THREAD because the socket option is
/// not ours to set**: `DatahubClient` clears its read timeout at the end of every connect,
/// deliberately, and exposes no hatch to re-arm it for one verb. Abandoning the helper is safe
/// because it borrows NOTHING — it returns a value, and a value nobody receives is dropped, closing
/// the socket inside it.
///
/// The twin of `crates/vike-app-core/src/data/md_session.rs`'s `bounded`, which bounds the market-data
/// control exchange for the identical reason. It is a SIBLING rather than a call: that one carries
/// its own deadline (adopted from the stream's read timeout, a different argument) and its own
/// message. Two is a coincidence; a third site is when they should become one parameterised helper,
/// and this doc is the note that says so.
///
/// `deadline` is a parameter rather than [`RPC_DEADLINE`] read directly so the CI suite exercises
/// this exact code path in milliseconds. A ten-second unit test is a test somebody deletes, and a
/// bound proven only by the constant it reads is the shape of assertion that cannot fail for its
/// stated reason.
fn bounded<T: Send + 'static>(
    what: &str,
    deadline: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    // ⚠ `sync_channel(1)`, so the helper's `send` NEVER blocks: a rendezvous channel would park the
    // abandoned thread for ever on the very path this function exists to bound.
    let (tx, rx) = std::sync::mpsc::sync_channel::<T>(1);
    std::thread::Builder::new()
        .name("dm-catalog".to_string())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|e| format!("the OS refused the stored-catalog helper thread: {e}"))?;
    rx.recv_timeout(deadline).map_err(|_| {
        format!(
            "{what} did not answer within {deadline:?} — treating the store as unreachable (the \
             datahub client clears its read timeout past the handshake, so this deadline is the \
             only one there is)"
        )
    })
}

/// THE load: the walk, the §6-Q2 fold, the boundary logging and the degrade decisions, in the one
/// place a caller has to reach for. `store_kind` is a short LABEL for the log — "datahub", not an
/// address: a host, a path or a machine name in a log line is a string this workspace's publication
/// guard has to chase, and the kind is the half an operator reading the log actually needs.
///
/// Every binary that loads this grid calls THIS, not the two functions below, so the boundary log
/// lines and the degrade decisions cannot differ between callers. The two halves stay public
/// because they are separately meaningful and separately tested.
///
/// ⚠ **The coverage fold is SKIPPED when the walk failed**, and that is deliberate rather than an
/// early return somebody tidied: a peer that could not answer `inventory` — its first and cheapest
/// question — will not answer `coverage_report` either, and asking would spend a second
/// [`RPC_DEADLINE`] to learn what is already known. The Partial column reads
/// [`RemoteCoverage::Unserved`] for it, which is exactly the state its honest note exists for.
pub fn load_stored(
    store: &Arc<dyn HistStore + Send + Sync>,
    store_kind: &str,
    deadline: Duration,
) -> StoredLoadOutcome {
    let started = Instant::now();
    // ⚠ The load used to be COMPLETELY silent — a full run at `vike_app_core=debug` logged not one
    // line about it, which is why "hung", "empty" and "slow" were indistinguishable from outside
    // and cost a whole debugging session. These three lines (start, the walk's own detail below,
    // finish-with-elapsed) are the half that makes the other two diagnosable.
    tracing::info!(store = store_kind, "stored catalog: load starting");
    let (tree, gaps, error) = match load_stored_tree(store, deadline) {
        Ok((tree, gaps)) => (tree, gaps, None),
        Err(e) => {
            tracing::warn!(
                store = store_kind,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "stored catalog: the walk could not be read: {e}"
            );
            (Vec::new(), GapMap::new(), Some(e))
        }
    };
    let (partials, coverage) = if error.is_some() {
        (PartialDayMap::new(), RemoteCoverage::Unserved)
    } else {
        match load_partials(store, deadline) {
            Ok(map) => (map, RemoteCoverage::Served),
            Err(e) => {
                // An `Err` here is the capability refusal a server older than the verb produces
                // (refused CLIENT-side, nothing sent), a read failure, or now a timeout — from the
                // grid's seat all three are the same fact, so the column gets the honest note
                // rather than an empty map claiming "nothing is partial".
                tracing::warn!(
                    store = store_kind,
                    "stored catalog: the coverage report failed: {e}"
                );
                (PartialDayMap::new(), RemoteCoverage::Unserved)
            }
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
    StoredLoadOutcome { tree, gaps, partials, coverage, error, history: None }
}

/// **The HISTORY column's answer**: one history-channels read, bounded like every catalog RPC here
/// ([`bounded`] — a manifest-class read that computes nothing), turned into what the By-venue table
/// renders. `fetch` is the datahub handle's own call (`RemoteHistStore::history_channels`), taken
/// as a closure so the walk's tests drive this exact path without a socket.
///
/// | `fetch` answers | the column gets |
/// |---|---|
/// | the report | [`HistoryLoad::served`] — the server's rows and overlay |
/// | `None` (a datahub OLDER than the read; nothing was sent) | [`HistoryLoad::compiled`] — this binary's table, under its caption, never clamping |
/// | an error, or nothing inside `deadline` | `None` — the column says it is not loaded; logged here |
///
/// `now_ms` resolves the fallback's rolling windows; a served report carries the server's clock.
pub fn load_history(
    fetch: impl FnOnce() -> Result<Option<HistoryChannelsReport>, String> + Send + 'static,
    now_ms: i64,
    deadline: Duration,
) -> Option<HistoryLoad> {
    match bounded("the history-channels read", deadline, fetch) {
        Ok(Ok(Some(report))) => Some(HistoryLoad::served(report)),
        Ok(Ok(None)) => {
            tracing::info!(
                "stored catalog: the datahub is older than the history-channels read; the HISTORY \
                 column shows this build's own table"
            );
            Some(HistoryLoad::compiled(now_ms))
        }
        Ok(Err(e)) | Err(e) => {
            tracing::warn!("stored catalog: the history-channels read failed: {e}");
            None
        }
    }
}

/// Walk `store`'s catalog through the TRAIT verbs into the Stored grid's
/// `(tree, per-series gap map)` — see the module doc for the shared-walk argument, the degrade
/// contract and why every RPC below is bounded. A gap-free series gets NO map entry (an absent key
/// already reads as "no known gaps").
pub fn load_stored_tree(
    store: &Arc<dyn HistStore + Send + Sync>,
    deadline: Duration,
) -> Result<(Vec<VenueNode>, GapMap), String> {
    // The reachability question, and the one whose failure means there is no grid at all. It is
    // bounded FIRST and alone: a peer that does not answer this answers nothing, and every probe
    // below would otherwise spend its own deadline learning that again.
    let inv = {
        let s = Arc::clone(store);
        bounded("the stored catalog's inventory scan", deadline, move || s.inventory())?
            .map_err(|e| format!("inventory scan: {e}"))?
    };
    tracing::debug!(series = inv.len(), "stored catalog: inventory answered; probing gaps");
    let mut gaps = GapMap::new();
    // Unanswered probes IN A ROW — reset by any answer, refusal included (a store that REFUSES is
    // a store that is talking). See [`DEAD_PEER_STREAK`].
    let mut streak = 0usize;
    for (probed, (id, _cov)) in inv.iter().enumerate() {
        if streak >= DEAD_PEER_STREAK {
            tracing::warn!(
                probed,
                series = inv.len(),
                "stored catalog: {DEAD_PEER_STREAK} gap probes in a row went unanswered — the \
                 store stopped answering mid-walk, so the remaining series keep no gap entry. The \
                 TREE is still complete and still renders; an absent entry reads as 'no known \
                 gaps', which is why this is a warning rather than a failed load"
            );
            break;
        }
        // One `SeriesId` clone per probe, because the helper thread needs an owned value — the
        // price of bounding the call at all, and small beside the round trip it is bounding.
        let (s, wanted) = (Arc::clone(store), id.clone());
        match bounded("a stored-catalog gap probe", deadline, move || s.series_gaps(&wanted)) {
            Ok(Ok(ranges)) => {
                streak = 0;
                if !ranges.is_empty() {
                    // ⚠ `label()`, NOT `symbol`. A GROUPED series carries an EMPTY `symbol` — it
                    // holds many, told apart by a row column — and its identity on disk and in the
                    // display tree is its GROUP NAME. `crates/vike-data-manager/src/model.rs`'s
                    // `build_tree` keys the tree on `id.label()` for exactly that reason, and the
                    // grid's own `series_key` looks up this map with the node label it got from
                    // there.
                    //
                    // Keyed on `symbol`, every grouped series produced a key that matched no row,
                    // so the Stored grid painted every polymarket group as gap-FREE and the "Has
                    // gaps" smart view could never list one. No test caught it: every gap test
                    // builds its fixture with `SeriesId::per_symbol`, where `symbol == label` and
                    // the two spellings are indistinguishable.
                    let key = vike_data_manager::SeriesKey {
                        venue: id.venue.clone(),
                        symbol: id.label().to_string(),
                        kind: id.kind.clone(),
                        interval: id.interval.clone(),
                    };
                    gaps.insert(key, ranges);
                }
            }
            // No gaps -> omit (an absent key reads as "no known gaps").
            Ok(Err(e)) => {
                streak = 0;
                tracing::warn!(
                    "stored catalog: series_gaps({}/{}/{}) failed: {e}",
                    id.venue,
                    id.symbol,
                    id.kind
                );
            }
            Err(deadline_hit) => {
                streak += 1;
                tracing::warn!(
                    streak,
                    "stored catalog: series_gaps({}/{}/{}) {deadline_hit}",
                    id.venue,
                    id.symbol,
                    id.kind
                );
            }
        }
    }
    Ok((build_tree(inv), gaps))
}

/// The Stored grid's cross-kind PARTIAL-day map, walked through the TRAIT verb — the §6-Q2 sibling
/// of [`load_stored_tree`], and the one fold BOTH the local and the remote arm now run.
///
/// `store.coverage_report()` is a manifest fold on either backend (no Parquet scan, one small RPC
/// over the wire), and [`partial_days_from_coverage`] is the same pure rendering step in both
/// cases — which is exactly the property worth having: the map a remote grid draws is folded from
/// the same value type, by the same function, as the map a local grid draws. There is no second
/// shape for the two modes to drift apart in.
///
/// An `Err` means the store could not answer AT ALL — over the wire, that is the negotiation
/// refusal a server older than the verb produces (`RemoteHistStore` surfaces it rather than
/// inheriting the trait's empty default, precisely so this stays distinguishable), a read that
/// failed, or the RPC not answering inside `deadline`. The caller renders
/// [`crate::data::stored_mode::PARTIALS_UNSERVED`] for it and keeps the rest of the grid; it must NOT
/// substitute an empty map, which would claim "nothing is partial".
pub fn load_partials(
    store: &Arc<dyn HistStore + Send + Sync>,
    deadline: Duration,
) -> Result<PartialDayMap, String> {
    let s = Arc::clone(store);
    let report =
        bounded("the stored catalog's coverage report", deadline, move || s.coverage_report())?
            .map_err(|e| format!("coverage report: {e}"))?;
    Ok(partial_days_from_coverage(&report))
}

#[path = "stored_load_tests.rs"]
#[cfg(test)]
mod stored_load_tests;

//! The Stored grid's inventory WALK, spelled over the `HistStore` TRAIT — the load half of the
//! #1378 seam close (see [`crate::stored_mode`] for the decision half).
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
//!   ([`crate::stored_mode::PARTIALS_UNSERVED`]) while the tree still renders.
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
//! [`crate::md_session`] already makes for `md_subscribe`/`md_update`, and the same answer: a
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

use crate::inventory::{VenueNode, build_tree};
use crate::stored_mode::RemoteCoverage;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vike_data::HistStore;
use vike_data_manager::{GapMap, PartialDayMap, partial_days_from_coverage};

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
}

/// Run one blocking catalog RPC on a SHORT-LIVED helper thread and give up on it after `deadline`.
///
/// ⚠ **This is the module doc's bounded-RPC rule, and it is a THREAD because the socket option is
/// not ours to set**: `DatahubClient` clears its read timeout at the end of every connect,
/// deliberately, and exposes no hatch to re-arm it for one verb. Abandoning the helper is safe
/// because it borrows NOTHING — it returns a value, and a value nobody receives is dropped, closing
/// the socket inside it.
///
/// The twin of `crates/vike-app-core/src/md_session.rs`'s `bounded`, which bounds the market-data
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
    StoredLoadOutcome { tree, gaps, partials, coverage, error }
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
/// [`crate::stored_mode::PARTIALS_UNSERVED`] for it and keeps the rest of the grid; it must NOT
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vike_data::{DataError, ExecFillRow, ExecOrderRow, SeriesCoverage, SeriesId, TsRange};
    use vike_data_manager::SeriesKey;
    use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

    /// The deadline every test that must SUCCEED runs under. Deliberately generous: these stores
    /// answer from memory, so the only thing between the call and the answer is a thread spawn and
    /// a channel hop — but the CI runners are shared boxes that go CPU-starved, and a bound tight
    /// enough to be "realistic" here would turn every test in this file into a load-dependent
    /// flake. Nothing in the file asserts that a HEALTHY call is fast; the timing assertions all
    /// live on the timeout side, where the margin is three orders of magnitude.
    const TEST_DEADLINE: Duration = Duration::from_secs(5);

    /// The deadline the timeout tests run under, against a store that parks for [`PARK`]. The 300x
    /// gap between them is what makes "the bound fired" a fact rather than a race.
    const SHORT_DEADLINE: Duration = Duration::from_millis(100);

    /// How long a planted hang parks for — the stand-in for the real failure, which is a socket
    /// read `crates/vike-datahub-client/src/client.rs` deliberately leaves unbounded. Long enough
    /// that a test observing a RETURN has observed the bound and not the store; finite rather than
    /// infinite only so the parked helper threads are reclaimed if the harness outlives them.
    const PARK: Duration = Duration::from_secs(30);

    /// Wrap a double in the shape the walk takes. The walk needs `Arc<dyn HistStore + Send + Sync>`
    /// rather than a `&dyn HistStore` because each RPC is handed to a helper thread that outlives
    /// the call (see [`bounded`]) — a borrow could not be, which is the one API consequence of
    /// bounding the walk at all.
    fn arc(store: FakeCatalogStore) -> Arc<dyn HistStore + Send + Sync> {
        Arc::new(store)
    }

    /// A seeded catalog-only `HistStore` double: answers the two verbs the walk uses
    /// (`inventory`, `series_gaps`); every other required verb is unreachable and says so.
    struct FakeCatalogStore {
        inv: Result<Vec<(SeriesId, SeriesCoverage)>, String>,
        gaps: Vec<(SeriesId, Vec<(i64, i64)>)>,
        /// A planted per-series `series_gaps` failure (the degrade-contract probe).
        failing_gap_probe: Option<SeriesId>,
        /// The §6-Q2 cross-kind report this store answers with. `Err` stands in for BOTH ways a
        /// store cannot answer: a `RemoteHistStore` whose peer predates the verb (refused
        /// client-side by the capability check) and a read that failed outright.
        coverage: Result<Vec<vike_data::InstrumentCoverage>, String>,
        /// Park in `inventory()` instead of answering — the HANG the module's bound exists for, and
        /// the failure a planted `Err` cannot stand in for: an `Err` is an answer.
        hang_inventory: bool,
        /// The same, for every `series_gaps` probe.
        hang_gaps: bool,
        /// REFUSE every probe — a store that is talking and saying no, which is a different fact
        /// from one that has gone quiet and is the premise of
        /// `a_refusal_is_an_answer_and_does_not_count_towards_the_streak`. `failing_gap_probe`
        /// cannot express it: it plants ONE failure, and one is below [`DEAD_PEER_STREAK`], so a
        /// test built on it would pass whether or not refusals counted towards the streak.
        fail_all_gaps: bool,
        /// How many probes actually REACHED this store. The streak test reads it because "the walk
        /// stopped early" is a claim about calls made, and asserting it by elapsed time instead
        /// would be a timing race dressed up as a behaviour check.
        probes: AtomicUsize,
    }

    impl Default for FakeCatalogStore {
        fn default() -> Self {
            Self {
                inv: Ok(Vec::new()),
                gaps: Vec::new(),
                failing_gap_probe: None,
                coverage: Ok(Vec::new()),
                hang_inventory: false,
                hang_gaps: false,
                fail_all_gaps: false,
                probes: AtomicUsize::new(0),
            }
        }
    }

    fn off_walk(verb: &str) -> DataError {
        DataError::Query(format!("FakeCatalogStore: {verb} is not part of the stored walk"))
    }

    impl HistStore for FakeCatalogStore {
        fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
            if self.hang_inventory {
                std::thread::sleep(PARK);
            }
            self.inv.clone().map_err(DataError::Query)
        }
        fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, DataError> {
            // Counted BEFORE any park or refusal, because the claim it backs is "how many probes
            // did the walk ATTEMPT" — a probe the walk started and abandoned still reached here.
            self.probes.fetch_add(1, Ordering::Relaxed);
            if self.hang_gaps {
                std::thread::sleep(PARK);
            }
            if self.fail_all_gaps || self.failing_gap_probe.as_ref() == Some(id) {
                return Err(DataError::Query("planted gap-probe failure".into()));
            }
            Ok(self
                .gaps
                .iter()
                .find(|(gid, _)| gid == id)
                .map(|(_, ranges)| ranges.clone())
                .unwrap_or_default())
        }
        fn coverage_report(&self) -> Result<Vec<vike_data::InstrumentCoverage>, DataError> {
            self.coverage.clone().map_err(DataError::Query)
        }

        // ---- everything below is unreachable for the walk ----
        fn load_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _r: TsRange,
        ) -> Result<Vec<Bar>, DataError> {
            Err(off_walk("load_bars"))
        }
        fn scan_quotes(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<QuoteTick>, DataError> {
            Err(off_walk("scan_quotes"))
        }
        fn scan_trades(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<TradeTick>, DataError> {
            Err(off_walk("scan_trades"))
        }
        fn append_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _b: &[Bar],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_bars"))
        }
        fn append_quotes(
            &self,
            _v: &str,
            _s: &str,
            _t: &[QuoteTick],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_quotes"))
        }
        fn append_trades(
            &self,
            _v: &str,
            _s: &str,
            _t: &[TradeTick],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_trades"))
        }
        fn append_book_updates(
            &self,
            _v: &str,
            _s: &str,
            _u: &[BookUpdate],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_book_updates"))
        }
        fn scan_book_updates(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<BookUpdate>, DataError> {
            Err(off_walk("scan_book_updates"))
        }
        fn append_symbol_properties(
            &self,
            _v: &str,
            _s: &str,
            _rows: &[(i64, SymbolProperties)],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_symbol_properties"))
        }
        fn scan_symbol_properties(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
            Err(off_walk("scan_symbol_properties"))
        }
        fn append_equity(
            &self,
            _v: &str,
            _s: &str,
            _rows: &[EquitySample],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_equity"))
        }
        fn scan_equity(
            &self,
            _v: &str,
            _s: &str,
            _r: TsRange,
        ) -> Result<Vec<EquitySample>, DataError> {
            Err(off_walk("scan_equity"))
        }
        fn append_exec_fills(
            &self,
            _v: &str,
            _s: &str,
            _rows: &[ExecFillRow],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_exec_fills"))
        }
        fn scan_exec_fills(&self, _v: &str, _s: &str) -> Result<Vec<ExecFillRow>, DataError> {
            Err(off_walk("scan_exec_fills"))
        }
        fn append_exec_orders(
            &self,
            _v: &str,
            _s: &str,
            _rows: &[ExecOrderRow],
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("append_exec_orders"))
        }
        fn scan_exec_orders(&self, _v: &str, _s: &str) -> Result<Vec<ExecOrderRow>, DataError> {
            Err(off_walk("scan_exec_orders"))
        }
        fn resample_quotes_to_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _r: TsRange,
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("resample_quotes_to_bars"))
        }
        fn resample_trades_to_bars(
            &self,
            _v: &str,
            _s: &str,
            _i: &str,
            _r: TsRange,
            _k: Option<&str>,
        ) -> Result<usize, DataError> {
            Err(off_walk("resample_trades_to_bars"))
        }
    }

    fn sid(kind: &str, venue: &str, sym: &str, iv: Option<&str>) -> SeriesId {
        SeriesId::per_symbol(kind, venue, sym, iv.map(Into::into))
    }
    fn cov(rows: u64, bytes: u64, a: i64, b: i64) -> SeriesCoverage {
        SeriesCoverage { first_ts: a, last_ts: b, rows, bytes, parts: 1, dates: 1 }
    }
    fn key(kind: &str, venue: &str, sym: &str, iv: Option<&str>) -> SeriesKey {
        SeriesKey {
            venue: venue.into(),
            symbol: sym.into(),
            kind: kind.into(),
            interval: iv.map(Into::into),
        }
    }

    fn small_fixture() -> Vec<(SeriesId, SeriesCoverage)> {
        vec![
            (sid("bar", "binance", "BTCUSDT", Some("1m")), cov(10, 100, 1_000, 2_000)),
            (sid("trade", "binance", "BTCUSDT", None), cov(5, 50, 1_000, 1_500)),
            (sid("bar", "okx", "ETH-USDT", Some("5m")), cov(7, 70, 1_100, 2_100)),
        ]
    }

    /// A catalog with more series than [`DEAD_PEER_STREAK`], so "the walk stopped probing" is a
    /// claim that CAN fail. `small_fixture` is exactly the streak long, which makes it useless for
    /// that one test and is why this second fixture exists rather than the first one growing — the
    /// tests above pin counts against its length.
    fn wide_fixture() -> Vec<(SeriesId, SeriesCoverage)> {
        (0..DEAD_PEER_STREAK * 2 + 1)
            .map(|n| (sid("bar", "binance", &format!("SYM{n}USDT"), Some("1m")), cov(1, 1, 0, 1)))
            .collect()
    }

    /// THE equality pin of the seam close: walking a seeded TRAIT store yields byte-identically
    /// the `VenueNode` tree the local arm's fold (`build_tree` over the same inventory) yields —
    /// so a remote grid renders exactly what a local grid over the same data renders.
    #[test]
    fn the_trait_walk_yields_the_tree_the_local_fold_yields_for_identical_data() {
        let inv = small_fixture();
        let store = FakeCatalogStore {
            inv: Ok(inv.clone()),
            gaps: vec![(sid("bar", "okx", "ETH-USDT", Some("5m")), vec![(1_200, 1_300)])],
            failing_gap_probe: None,
            coverage: Ok(Vec::new()),
            ..Default::default()
        };
        let (tree, gaps) = load_stored_tree(&arc(store), TEST_DEADLINE).expect("seeded walk");
        assert_eq!(tree, build_tree(inv), "remote walk and local fold must agree on the tree");
        assert_eq!(gaps.len(), 1, "exactly the one gappy series gets an entry");
        assert_eq!(gaps[&key("bar", "okx", "ETH-USDT", Some("5m"))], vec![(1_200, 1_300)]);
    }

    /// A GROUPED series' gaps land under a key the grid can actually look up.
    ///
    /// ⚠ This is a regression test for a bug that shipped, and the reason it survived is the shape
    /// of every other test in this file: they all build fixtures with `SeriesId::per_symbol`, where
    /// `symbol == label()` and the two spellings are indistinguishable. A grouped series carries an
    /// EMPTY `symbol` and is identified by its GROUP NAME, which is what `build_tree` keys the
    /// display tree on and what the grid looks this map up with — so keying here on `symbol`
    /// produced an entry nothing could ever match. Every polymarket group rendered gap-free, and
    /// `ViewFilter::HasGaps` could not list one.
    #[test]
    fn a_grouped_series_gap_is_keyed_by_its_group_name_not_an_empty_symbol() {
        let g = SeriesId::grouped("quote", "polymarket", "btc-5m");
        assert!(g.symbol.is_empty(), "the premise: a grouped series carries no symbol");
        let store = FakeCatalogStore {
            inv: Ok(vec![(g.clone(), cov(9, 90, 1_000, 3_000))]),
            gaps: vec![(g.clone(), vec![(1_500, 1_600)])],
            failing_gap_probe: None,
            coverage: Ok(Vec::new()),
            ..Default::default()
        };
        let (tree, gaps) = load_stored_tree(&arc(store), TEST_DEADLINE).expect("seeded walk");

        let node = &tree[0].symbols[0];
        assert_eq!(node.symbol, "btc-5m", "the tree names the group");
        assert!(
            gaps.contains_key(&key("quote", "polymarket", &node.symbol, None)),
            "the gap map must be reachable by the label the tree carries, not by an empty symbol"
        );
        assert!(
            !gaps.contains_key(&key("quote", "polymarket", "", None)),
            "no entry may hide under the empty symbol"
        );
    }

    /// An absent key already reads as "no known gaps" downstream, so a gap-free series must not
    /// insert an empty entry (byte-preserves `refresh_stored`'s original omit-empty behavior).
    #[test]
    fn a_gap_free_series_gets_no_gap_map_entry() {
        let store = FakeCatalogStore {
            inv: Ok(small_fixture()),
            gaps: Vec::new(),
            failing_gap_probe: None,
            coverage: Ok(Vec::new()),
            ..Default::default()
        };
        let (tree, gaps) = load_stored_tree(&arc(store), TEST_DEADLINE).expect("seeded walk");
        assert_eq!(tree.len(), 2, "both venues present");
        assert!(gaps.is_empty(), "no gaps anywhere ⇒ an empty map, not empty entries");
    }

    /// The degrade contract: one series' failing gap probe skips THAT entry, never the refresh —
    /// the tree stays complete and every other series' gaps still land.
    #[test]
    fn a_failing_gap_probe_skips_that_series_never_the_walk() {
        let inv = small_fixture();
        let store = FakeCatalogStore {
            inv: Ok(inv.clone()),
            gaps: vec![
                (sid("bar", "binance", "BTCUSDT", Some("1m")), vec![(1_400, 1_600)]),
                (sid("bar", "okx", "ETH-USDT", Some("5m")), vec![(1_200, 1_300)]),
            ],
            failing_gap_probe: Some(sid("bar", "okx", "ETH-USDT", Some("5m"))),
            coverage: Ok(Vec::new()),
            ..Default::default()
        };
        let (tree, gaps) = load_stored_tree(&arc(store), TEST_DEADLINE)
            .expect("a per-series failure is not fatal");
        assert_eq!(tree, build_tree(inv), "the tree survives a gap-probe failure whole");
        assert_eq!(gaps.len(), 1, "only the healthy series' gaps land");
        assert_eq!(gaps[&key("bar", "binance", "BTCUSDT", Some("1m"))], vec![(1_400, 1_600)]);
    }

    /// An inventory failure IS fatal to the load (there is nothing to render) — surfaced as `Err`
    /// naming the cause, which [`load_stored`] turns into `StoredLoadOutcome::error` and the caller
    /// RENDERS (it used to render an empty tree, which is indistinguishable from an empty store —
    /// see `an_unreadable_store_and_an_empty_one_are_different_outcomes`).
    #[test]
    fn an_inventory_failure_is_an_err_naming_the_cause() {
        let store = FakeCatalogStore {
            inv: Err("planted inventory failure".into()),
            gaps: Vec::new(),
            failing_gap_probe: None,
            coverage: Ok(Vec::new()),
            ..Default::default()
        };
        let err = load_stored_tree(&arc(store), TEST_DEADLINE).expect_err("no inventory ⇒ no load");
        assert!(err.contains("planted inventory failure"), "the cause must survive: {err}");
    }

    // ---- the §6-Q2 partial-day fold ------------------------------------------------------------

    /// One instrument recording BOTH trade and quote, where day 2 has trades and no quotes — the
    /// smallest shape that makes `partial_days` non-empty (a report with one recorded kind can
    /// never disagree with itself, so a single-kind fixture would pass vacuously).
    fn partial_fixture() -> Vec<vike_data::InstrumentCoverage> {
        vike_data::coverage::join_coverage(&[
            (sid("trade", "binance", "BTCUSDT", None), vec![0, 1, 2]),
            (sid("quote", "binance", "BTCUSDT", None), vec![0, 1]),
        ])
    }

    /// The fold a REMOTE grid runs is the fold a LOCAL grid runs: `load_partials` over a store that
    /// answers the trait verb equals `partial_days_from_coverage` over the same report. This is the
    /// app-core half of the wire-adds-nothing property the composed datahub test proves end to end.
    #[test]
    fn the_partial_fold_equals_the_direct_fold_over_the_same_report() {
        let report = partial_fixture();
        let store = FakeCatalogStore { coverage: Ok(report.clone()), ..Default::default() };
        let folded =
            load_partials(&arc(store), TEST_DEADLINE).expect("a store that answers coverage");
        assert_eq!(folded, partial_days_from_coverage(&report), "one fold, whichever store");
        assert!(!folded.is_empty(), "the fixture must actually produce a partial day");
    }

    /// A store that cannot answer is an `Err`, NOT an empty map. The distinction is the whole
    /// honesty of the column: an empty map renders as "nothing is partial", which is a claim, while
    /// the `Err` is what the caller turns into `stored_mode::PARTIALS_UNSERVED`.
    #[test]
    fn an_unanswerable_coverage_report_is_an_err_not_an_empty_map() {
        let store = FakeCatalogStore {
            coverage: Err("does not advertise `coverage`".into()),
            ..Default::default()
        };
        let err = load_partials(&arc(store), TEST_DEADLINE)
            .expect_err("an unanswerable report must not fold to empty");
        assert!(err.contains("coverage"), "the cause must survive for the log line: {err}");
    }

    /// A store that answers with an EMPTY report is a different fact from one that cannot answer,
    /// and must stay one: it folds to an empty map through `Ok`, so the column renders blank
    /// (correctly — nothing is partial) instead of showing the unserved note.
    #[test]
    fn an_empty_report_folds_to_an_empty_map_through_ok() {
        let store = FakeCatalogStore::default();
        assert!(
            load_partials(&arc(store), TEST_DEADLINE)
                .expect("an empty report is still an answer")
                .is_empty()
        );
    }

    // ---- the BOUND (see the module doc's "THE WALK IS BOUNDED HERE") ---------------------------

    /// THE kill proof of this module's bound, and the one thing a planted `Err` can never stand in
    /// for: a store that does not ANSWER. Before the bound, this call returned never — the window
    /// sat on "Loading stored data…" for the life of the process with no `Err` to log, which is
    /// precisely why the module's degrade contract was a promise the code could not keep.
    ///
    /// The elapsed assertion is the half that matters: without it, a test that merely reached an
    /// `Err` would also pass against a walk that waited out the full [`PARK`] and then failed for
    /// some other reason.
    #[test]
    fn an_inventory_that_never_answers_is_bounded_rather_than_waited_out() {
        let store = FakeCatalogStore {
            inv: Ok(small_fixture()),
            hang_inventory: true,
            ..Default::default()
        };
        let began = Instant::now();
        let err = load_stored_tree(&arc(store), SHORT_DEADLINE)
            .expect_err("a store that never answers must not be waited out");
        let waited = began.elapsed();
        assert!(
            err.contains("did not answer within"),
            "the reason must say the store went QUIET rather than failed — an operator reading \
             this has to know it is a link problem, not a bad store: {err}"
        );
        assert!(
            waited < PARK / 2,
            "the bound must be what returned, not the planted park finishing: waited {waited:?} \
             against a {PARK:?} park"
        );
    }

    /// A gap probe that never answers is treated exactly as one that FAILS: that series keeps no
    /// entry and the walk finishes whole. `small_fixture` is deliberately exactly
    /// [`DEAD_PEER_STREAK`] long, so every probe is attempted and the streak break below is a
    /// SEPARATE claim rather than something this test could accidentally be proving.
    #[test]
    fn a_gap_probe_that_never_answers_skips_that_series_never_the_walk() {
        let inv = small_fixture();
        assert_eq!(inv.len(), DEAD_PEER_STREAK, "the premise: no probe is skipped by the streak");
        let store = Arc::new(FakeCatalogStore {
            inv: Ok(inv.clone()),
            hang_gaps: true,
            ..Default::default()
        });
        let dyn_store: Arc<dyn HistStore + Send + Sync> = store.clone();
        let (tree, gaps) = load_stored_tree(&dyn_store, SHORT_DEADLINE)
            .expect("a probe that never answers is not fatal, any more than one that fails is");
        assert_eq!(tree, build_tree(inv), "the tree survives every probe going quiet");
        assert!(gaps.is_empty(), "an unanswered probe leaves no entry — never a made-up empty one");
        assert_eq!(
            store.probes.load(Ordering::Relaxed),
            DEAD_PEER_STREAK,
            "every series was still attempted"
        );
    }

    /// The streak: a peer that goes away MID-walk (a tunnel dropping is the ordinary way) stops
    /// being probed, so the walk costs [`DEAD_PEER_STREAK`] deadlines rather than one per series.
    ///
    /// Asserted on the probe COUNT rather than on elapsed time: "the walk stopped early" is a claim
    /// about calls made, and an elapsed-time assertion would be a race dressed as a behaviour check
    /// — it would also pass on a box slow enough to make the arithmetic ambiguous.
    #[test]
    fn the_walk_stops_probing_a_peer_that_went_away_mid_walk() {
        let inv = wide_fixture();
        assert!(
            inv.len() > DEAD_PEER_STREAK,
            "the premise: there must be series LEFT to skip, or this test cannot fail"
        );
        let store = Arc::new(FakeCatalogStore {
            inv: Ok(inv.clone()),
            hang_gaps: true,
            ..Default::default()
        });
        let dyn_store: Arc<dyn HistStore + Send + Sync> = store.clone();
        let (tree, gaps) = load_stored_tree(&dyn_store, SHORT_DEADLINE)
            .expect("a dead peer mid-walk is not fatal");
        assert_eq!(
            store.probes.load(Ordering::Relaxed),
            DEAD_PEER_STREAK,
            "the walk must stop after {DEAD_PEER_STREAK} unanswered probes, not pay one deadline \
             per series for the rest of the catalog"
        );
        assert_eq!(tree, build_tree(inv), "and the TREE is still complete — that is what renders");
        assert!(gaps.is_empty());
    }

    /// An ANSWER resets the streak, refusal included: a store that says "I cannot read that
    /// series' manifest" is a store that is talking, and unreadable manifests must not end a walk
    /// over a catalog of thousands. Without the reset, `DEAD_PEER_STREAK` `Err`s in a row anywhere
    /// in a large catalog would silently truncate the gap map.
    ///
    /// ⚠ **Every probe refuses, and that is load-bearing.** A first draft planted ONE failure and
    /// asserted the whole catalog was still probed — which would have passed whether or not
    /// refusals counted towards the streak, since one is below [`DEAD_PEER_STREAK`]. With all of
    /// them refusing, a streak that counted refusals would stop after three, so the probe count
    /// below is a claim that can actually fail.
    #[test]
    fn a_refusal_is_an_answer_and_does_not_count_towards_the_streak() {
        let inv = wide_fixture();
        let store = Arc::new(FakeCatalogStore {
            inv: Ok(inv.clone()),
            fail_all_gaps: true,
            ..Default::default()
        });
        let dyn_store: Arc<dyn HistStore + Send + Sync> = store.clone();
        let (tree, _gaps) =
            load_stored_tree(&dyn_store, TEST_DEADLINE).expect("a refusal is not a dead peer");
        assert_eq!(
            store.probes.load(Ordering::Relaxed),
            inv.len(),
            "every series must still be probed — refusals are answers"
        );
        assert_eq!(tree, build_tree(inv));
    }

    /// THE render honesty, and the half of the defect the bound alone does not fix: an EMPTY store
    /// and an UNREADABLE one both produce an empty tree, and before `StoredLoadOutcome::error`
    /// existed they were the same value — so both drew the same empty grid and an operator could
    /// not tell "there is nothing recorded" from "nothing could be reached".
    #[test]
    fn an_unreadable_store_and_an_empty_one_are_different_outcomes() {
        let empty = load_stored(&arc(FakeCatalogStore::default()), "test", TEST_DEADLINE);
        assert!(empty.tree.is_empty(), "the premise: an empty store draws an empty tree");
        assert!(
            empty.error.is_none(),
            "a store that answered 'nothing' has no error to show — that IS the answer"
        );

        let unreadable = load_stored(
            &arc(FakeCatalogStore {
                inv: Err("planted inventory failure".into()),
                ..Default::default()
            }),
            "test",
            TEST_DEADLINE,
        );
        assert!(unreadable.tree.is_empty(), "the premise: it draws the SAME empty tree");
        let why =
            unreadable.error.expect("an unreadable store must carry its reason to the render");
        assert!(why.contains("planted inventory failure"), "the cause must survive: {why}");
    }

    /// A load whose walk failed does not then spend a SECOND deadline asking the same dead peer for
    /// a coverage report — the column reads `Unserved`, which is exactly the state its honest note
    /// exists for. The fixture answers coverage happily, so a load that DID ask would come back
    /// `Served` with a non-empty map and fail this.
    #[test]
    fn a_failed_walk_skips_the_coverage_fold_and_reads_unserved() {
        let out = load_stored(
            &arc(FakeCatalogStore {
                inv: Err("planted inventory failure".into()),
                coverage: Ok(partial_fixture()),
                ..Default::default()
            }),
            "test",
            TEST_DEADLINE,
        );
        assert_eq!(out.coverage, RemoteCoverage::Unserved);
        assert!(out.partials.is_empty(), "no fold may run against a store that could not be read");
    }

    /// The healthy path through [`load_stored`]: the walk, the fold and the negotiated answer all
    /// land, and nothing reports an error. The counterweight to every degrade test above — without
    /// it they would all still pass over a function that failed unconditionally.
    #[test]
    fn a_healthy_store_loads_with_no_error_and_a_served_coverage_answer() {
        let out = load_stored(
            &arc(FakeCatalogStore {
                inv: Ok(small_fixture()),
                gaps: vec![(sid("bar", "okx", "ETH-USDT", Some("5m")), vec![(1_200, 1_300)])],
                coverage: Ok(partial_fixture()),
                ..Default::default()
            }),
            "test",
            TEST_DEADLINE,
        );
        assert!(
            out.error.is_none(),
            "a store that answered has nothing to report: {:?}",
            out.error
        );
        assert_eq!(out.coverage, RemoteCoverage::Served);
        assert_eq!(out.tree.len(), 2, "both venues present");
        assert_eq!(out.gaps.len(), 1, "the one gappy series");
        assert!(!out.partials.is_empty(), "the §6-Q2 fold ran");
    }
}

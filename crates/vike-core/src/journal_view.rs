//! Build the THIRD leg of the reconcile cross-check — a [`vike_exec::recon::JournalView`] — from
//! the materialized Tier-2 exec log a [`vike_data::HistStore`] holds.
//!
//! # Why this lives here rather than in the binary that calls it
//!
//! The workspace's ONE production [`crate::JournalViewHook`] is built in `crates/vike-app`'s
//! `main.rs`, which is the one workspace member the merge gate does not RUN
//! (`xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI`, and the ratchet
//! `crates/vike-ops/tests/ci_excluded_gui_shell_ratchet.rs` exists because of it). The store walk,
//! the `kind=`/`venue` filter, the window-scoped scans and the status parse are all PURE over the
//! `HistStore` trait, so none of them needed to be there — and while they were, no test in this
//! repository ever executed them.
//!
//! What CANNOT come down here, and deliberately stays in the composition root: the CLOCK (`since_ms`
//! is a parameter, not a `SystemTime::now()` this function takes for itself), the process
//! environment (only a binary reads it — `crates/vike-ops/tests/settings_registry.rs`, whose
//! `LIBRARY_PIN` may shrink and never grow, so a `VIKE_RECONCILE_LOOKBACK_MS` read under any
//! `src/` is a ratchet violation by construction), and the concrete
//! `vike_data::DataFusionHist::open` (vike-core's `vike-data` edge is DEFAULT-FEATURES, i.e. no
//! DataFusion in this crate's tree).
//!
//! # The window this function is handed must be the venue leg's window
//!
//! `vike_exec::recon::diff` raises `JournalDivergence` — the restore/persistence-bug signal this
//! whole third leg exists for — only when `journal.seen_trade_ids` CONTAINS the venue's trade id;
//! a venue fill the journal never saw falls through to `MissingFill`, which `hybrid` (the default
//! policy) AUTO-APPLIES with no operator in front of it. So a journal window NARROWER than the
//! venue-report window does not merely miss detections: it silently converts them into automatic
//! folds. The caller therefore owes this function the same `lookback_ms` the venue leg used
//! (`vike_ops::reconcile_config::lookback_ms`), never a literal of its own.

use vike_data::{HistStore, exec_index};
use vike_exec::OrderStatus;
use vike_exec::recon::JournalView;

/// The journal's view of `venue`, restricted to activity at or after `since_ms`.
///
/// `since_ms` is an ABSOLUTE epoch-ms floor (the caller's `now − lookback`), matching
/// [`vike_data::exec_index::recent_seen_trade_ids`]'s own `since_ts` contract.
///
/// # Failure is LOUD, never silent
///
/// Every read here is best-effort — a store that cannot be enumerated, a series that cannot be
/// scanned, or a status string this build cannot parse each degrade to "that much less journal"
/// rather than failing the reconcile pass. But an EMPTY [`JournalView`] is behaviourally identical
/// to handing `diff` no journal at all (that type's own doc spells out why), so each degradation
/// is LOGGED at the point it happens, naming what could not be read. A silent empty view is exactly
/// the three-way→two-way downgrade this function was extracted to make visible.
pub fn journal_view_from_store(
    store: &(dyn HistStore + Send + Sync),
    venue: &str,
    since_ms: i64,
) -> JournalView {
    let mut view = JournalView::default();

    // The catalog walk. A store that cannot enumerate its inventory returns the trait's own
    // "cannot enumerate" error (`HistStore::list_series`'s default body) and every store that CAN
    // may still fail on IO — either way the third leg is GONE for this pass and the operator has
    // to be able to see that in the log, because the returned value cannot say it.
    let series = match store.list_series() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                target: "vike_core::journal_view",
                venue = %venue,
                since_ms,
                error = %e,
                "journal cross-check DEGRADED to a two-way reconcile for this venue: the hist \
                 store's series inventory could not be read, so the journal leg is empty. A venue \
                 fill this process has lost will now surface as MissingFill instead of \
                 JournalDivergence — HELD for you under the default `quarantine` policy, and \
                 auto-applied with no operator in front of it if this mount named `hybrid` in \
                 VIKE_RECONCILE_POLICY"
            );
            return view;
        }
    };

    // Fill side: the seen-trade_id dedup set — the input `diff`'s `JournalDivergence` check keys
    // on. One scan per per-symbol `exec_fill` series this venue owns.
    for id in series.iter().filter(|s| s.kind == "exec_fill" && s.venue == venue) {
        match exec_index::recent_seen_trade_ids(store, venue, &id.symbol, since_ms) {
            Ok(ids) => view.seen_trade_ids.extend(ids),
            Err(e) => tracing::warn!(
                target: "vike_core::journal_view",
                venue = %venue,
                symbol = %id.symbol,
                since_ms,
                error = %e,
                "journal fill scan failed; this symbol contributes NO recorded trade ids to the \
                 cross-check, so its lost fills read as MissingFill rather than JournalDivergence"
            ),
        }
    }

    // Order side: latest status per coid, from the `exec_order` lifecycle snapshots the
    // materializer writes. An unparseable status is a forward-compat guard (a status string a
    // newer writer emitted and this build has no variant for) — skipped, but never in silence.
    for id in series.iter().filter(|s| s.kind == "exec_order" && s.venue == venue) {
        match exec_index::recent_order_statuses(store, venue, &id.symbol, since_ms) {
            Ok(statuses) => {
                for (coid, status) in statuses {
                    match OrderStatus::parse(&status) {
                        Some(st) => {
                            view.orders.insert(coid, st);
                        }
                        None => tracing::warn!(
                            target: "vike_core::journal_view",
                            venue = %venue,
                            symbol = %id.symbol,
                            client_order_id = %coid,
                            status = %status,
                            "journal order status is not a status this build knows; the order is \
                             omitted from the cross-check"
                        ),
                    }
                }
            }
            Err(e) => tracing::warn!(
                target: "vike_core::journal_view",
                venue = %venue,
                symbol = %id.symbol,
                since_ms,
                error = %e,
                "journal order scan failed; this symbol contributes NO recorded order statuses to \
                 the cross-check, so a live order local has lost reads as UnknownOrder rather than \
                 JournalDivergence"
            ),
        }
    }

    view
}

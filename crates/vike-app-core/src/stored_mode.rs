//! WHICH store the Data-Manager "Stored" grid reads — and which of its columns/actions exist in
//! that mode (the #1378 seam close, split-plane REQ-9's inventory half plus the §6-Q2 coverage
//! half).
//!
//! #1378 routed the grid's bulk Backfill over the wire when `config.datahub_addr` is set
//! ([`crate::backfill_route`]), but `refresh_stored`'s inventory read still opened the LOCAL store
//! in a fat build — so a wire backfill landed its rows in the SERVER's store while the grid kept
//! rendering the local one, and the freshly-backfilled series never appeared. The cure is the same
//! ONE-value split every other store consumer already makes (`open_studio_store`,
//! `backfill_route`), over the RESOLVED datahub address (since REQ-2:
//! [`crate::datahub_resolve::resolve_datahub_addr`] — explicit `config.datahub_addr` first, else
//! the active backend's advertisement): resolved → the grid walks the REMOTE store through the
//! `HistStore` TRAIT verbs (`inventory`/`series_gaps` — served by `RemoteHistStore` since
//! B12/PR-6); none → the local store, exactly as before.
//!
//! Following the store is only half the honesty. One of the Stored tab's surfaces is
//! LOCAL-STORE-ONLY, and in remote mode it must be visibly absent rather than silently wrong:
//!
//! - **Delete** (single + bulk) calls `DataFusionHist::delete_series` — a concrete method, not a
//!   trait verb, and the datahub wire serves no delete. A remote-mode Delete that "worked" would
//!   delete from the LOCAL store the grid is not even showing.
//!
//! **The Partial column used to be the second such surface, and is not any more.** It is fed by
//! `HistStore::coverage_report`, which spec §6-Q2 promoted from a concrete `DataFusionHist` fold
//! onto the TRAIT with a datahub wire verb behind it — so a remote grid CAN compute it, against a
//! server new enough to serve it. That "new enough" is the only remaining uncertainty and it is not
//! knowable from `datahub_addr` alone, so the column now has THREE states rather than two, and
//! [`RemoteCoverage`] is the third input that picks between them. The honest note survives for
//! exactly one of them.
//!
//! (Bulk Backfill/Update needs NO row here: the wire arm is precisely what #1378 shipped, so the
//! action exists in both modes — `backfill_route` picks where it runs.)
//!
//! This module is the PURE decision table; `vike-app`'s `main.rs` (`refresh_stored`, the
//! `stored_deletes` drain) and `tool_views::stored` only wire it.

use crate::backfill_route::{BackfillRoute, backfill_route};

/// The one-line reason Delete is grayed out in remote mode (hover text + the gate's why).
pub const DELETE_LOCAL_ONLY: &str =
    "Delete is a local-store operation — the datahub wire serves no delete verb";

/// The one-line note shown for the Partial column when the CONNECTED datahub could not answer the
/// cross-kind coverage verb (a visible disclosure, never a silent empty).
///
/// ⚠ This replaces the old `PARTIALS_LOCAL_ONLY`, whose text said the coverage report was "not a
/// wire verb". That claim was true when it was written and is now FALSE — §6-Q2 built the verb — so
/// the constant was renamed rather than left standing with a stale reason inside it. The note that
/// remains makes the narrower, still-true claim: *this* peer did not answer.
///
/// It deliberately covers both ways that happens — a server older than the verb (the designed case,
/// refused client-side by the `FEATURE_COVERAGE` negotiation without a frame being sent) and a
/// read that failed on a server that does serve it — because from the grid's seat they are the same
/// fact: there is no report to render, and pretending "nothing is partial" would be the lie the
/// column exists to avoid. The log line distinguishes them for whoever needs the difference.
pub const PARTIALS_UNSERVED: &str = "Partial column unavailable: the connected datahub did not answer the cross-kind coverage \
     verb (an older server, or a read failure — see the log)";

/// What the connected datahub said about the cross-kind coverage verb — the third input to the
/// Partial column's state, and the one `config.datahub_addr` cannot supply.
///
/// It is a NEGOTIATED fact, so it exists only after a dial: `vike-app`'s `refresh_stored` learns it
/// from the remote walk's `coverage_report` call (served → [`Self::Served`], refused/failed →
/// [`Self::Unserved`]) and remembers it for the next render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteCoverage {
    /// Nothing has been dialled yet (the pre-first-load render), or the grid is LOCAL, where no
    /// negotiation happens at all. In remote mode this reads as "not yet available" and shows the
    /// note — the honest answer while the grid is still empty and loading; it flips to
    /// [`Self::Served`] with the first successful load. It is deliberately NOT optimistic: a note
    /// that later disappears is a better failure than a column header promising data that never
    /// arrives.
    Unknown,
    /// The dial succeeded and the report came back — the column renders from the WIRE answer,
    /// folded by the same `partial_days_from_coverage` a local grid folds.
    Served,
    /// The connected server did not answer the verb (it predates it, or the read failed). The
    /// column shows [`PARTIALS_UNSERVED`] instead.
    Unserved,
}

/// The Stored grid's per-mode capability table — computed ONCE per decision point from the
/// RESOLVED datahub address, the SAME value `open_studio_store` and [`backfill_route`] branch
/// on, so the store the grid READS, the plane a backfill WRITES and the actions the UI OFFERS
/// can never disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredModeTable {
    /// `Some(addr)` → the grid walks the REMOTE trait store at `addr`; `None` → the local store.
    pub remote_addr: Option<String>,
    /// `Some(reason)` → Delete (single + bulk) is grayed out with this reason.
    pub delete_unavailable: Option<&'static str>,
    /// `Some(note)` → the Partial column cannot be filled in this mode; the note is SHOWN.
    /// `None` → the column renders (from the local fold, or from the wire answer).
    pub partials_unavailable: Option<&'static str>,
}

/// The mode decision. `datahub_addr` is the RESOLVED datahub address the composition root arrived
/// at — since REQ-2 that is `config.datahub_addr` when the operator set one, else the ACTIVE
/// backend's own advertisement (see [`crate::datahub_resolve`]); `None` = no datahub at all.
/// `coverage` is what the last dial negotiated (see [`RemoteCoverage`]; [`RemoteCoverage::Unknown`]
/// at a call site that only reads `remote_addr`/`delete_unavailable`). The store half is derived
/// THROUGH [`backfill_route`] rather than by re-matching the option, so the read mode and the
/// backfill route split on the same value by construction, not by convention.
///
/// The Partial column's three states:
///
/// | mode | coverage | Partial column |
/// |---|---|---|
/// | local (no resolved datahub) | *ignored* — nothing is dialled | rendered, from the local store's own fold |
/// | remote | [`RemoteCoverage::Served`] | rendered, from the wire answer |
/// | remote | [`RemoteCoverage::Unserved`] / [`RemoteCoverage::Unknown`] | [`PARTIALS_UNSERVED`] |
pub fn stored_mode(datahub_addr: Option<&str>, coverage: RemoteCoverage) -> StoredModeTable {
    match backfill_route(datahub_addr) {
        BackfillRoute::Wire { addr } => StoredModeTable {
            remote_addr: Some(addr),
            delete_unavailable: Some(DELETE_LOCAL_ONLY),
            partials_unavailable: match coverage {
                RemoteCoverage::Served => None,
                RemoteCoverage::Unserved | RemoteCoverage::Unknown => Some(PARTIALS_UNSERVED),
            },
        },
        BackfillRoute::Local => StoredModeTable {
            remote_addr: None,
            delete_unavailable: None,
            partials_unavailable: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_datahub_addr_reads_the_local_store_and_gates_nothing() {
        assert_eq!(
            stored_mode(None, RemoteCoverage::Unknown),
            StoredModeTable {
                remote_addr: None,
                delete_unavailable: None,
                partials_unavailable: None,
            },
            "local mode is today's grid untouched: local read, Delete live, Partial computed"
        );
    }

    /// Local mode never dials, so no negotiated answer may change ANY of its three columns — a
    /// stale `RemoteCoverage` left over from a previous remote session must not gray the local
    /// grid's Partial column.
    #[test]
    fn local_mode_ignores_the_negotiated_coverage_answer() {
        for coverage in [RemoteCoverage::Unknown, RemoteCoverage::Served, RemoteCoverage::Unserved]
        {
            assert_eq!(
                stored_mode(None, coverage).partials_unavailable,
                None,
                "local mode computes the column itself, whatever {coverage:?} says"
            );
        }
    }

    #[test]
    fn a_datahub_addr_reads_the_remote_store_carrying_the_addr_verbatim() {
        assert_eq!(
            stored_mode(Some("<host>:7878"), RemoteCoverage::Served).remote_addr.as_deref(),
            Some("<host>:7878")
        );
    }

    #[test]
    fn remote_mode_grays_delete_with_a_reason() {
        let t = stored_mode(Some("127.0.0.1:7878"), RemoteCoverage::Served);
        let reason = t.delete_unavailable.expect("remote-mode Delete must carry a visible reason");
        assert!(
            reason.contains("local"),
            "the reason should say delete is a local-store operation: {reason}"
        );
    }

    /// THE §6-Q2 DECISION TABLE — the Partial column's three states, in one place, over the two
    /// inputs that decide it. This is the test that would go red if the coverage verb were ever
    /// unwired (state 2 collapsing back into state 3) or if the note were shown against a server
    /// that does answer.
    #[test]
    fn the_partial_column_has_three_states() {
        let table = [
            // (datahub_addr, negotiated coverage) -> the note, or None = the column renders
            (None, RemoteCoverage::Unknown, None),
            (Some("127.0.0.1:7878"), RemoteCoverage::Served, None),
            (Some("127.0.0.1:7878"), RemoteCoverage::Unserved, Some(PARTIALS_UNSERVED)),
        ];
        for (addr, coverage, expected) in table {
            assert_eq!(
                stored_mode(addr, coverage).partials_unavailable,
                expected,
                "addr={addr:?} coverage={coverage:?}"
            );
        }
    }

    /// State 2 is the whole point of §6-Q2: a remote grid against a server that SERVES the verb
    /// renders the column, exactly like a local one — no note, nothing grayed.
    #[test]
    fn remote_mode_against_a_serving_server_renders_the_partial_column() {
        assert_eq!(
            stored_mode(Some("127.0.0.1:7878"), RemoteCoverage::Served).partials_unavailable,
            None,
            "the wire answers this column now; the local-only note would be a false claim"
        );
    }

    /// State 3: an OLDER server keeps the honest note — and the note may not claim the column is
    /// local-only (it is not, since the verb exists), only that THIS peer did not answer.
    #[test]
    fn remote_mode_against_an_older_server_keeps_an_honest_note() {
        let t = stored_mode(Some("127.0.0.1:7878"), RemoteCoverage::Unserved);
        let note = t.partials_unavailable.expect("an unserved remote must carry a visible note");
        assert!(
            note.contains("unavailable") && note.contains("coverage"),
            "the note names the column state and the verb: {note}"
        );
        assert!(
            !note.contains("not a wire verb"),
            "the pre-§6-Q2 reason is now FALSE and must not survive in the note: {note}"
        );
    }

    /// The pre-dial render (nothing negotiated yet) shows the note rather than an empty column
    /// promising "nothing is partial" — the pessimistic default the enum documents.
    #[test]
    fn a_remote_grid_that_has_not_dialled_yet_shows_the_note() {
        assert_eq!(
            stored_mode(Some("127.0.0.1:7878"), RemoteCoverage::Unknown).partials_unavailable,
            Some(PARTIALS_UNSERVED)
        );
    }

    /// The read mode and #1378's backfill route must split on the same key the same way — a grid
    /// that READS remote while its Backfill button writes LOCAL (or vice versa) is exactly the
    /// seam this module closes. Pinned over both inputs even though `stored_mode` derives through
    /// `backfill_route` by construction: the pin outlives a refactor that unpicks the derivation.
    #[test]
    fn the_grid_mode_and_the_backfill_route_split_on_the_same_key() {
        for input in [None, Some("the CI box:7878")] {
            let mode = stored_mode(input, RemoteCoverage::Served);
            match backfill_route(input) {
                BackfillRoute::Wire { addr } => assert_eq!(
                    mode.remote_addr,
                    Some(addr),
                    "wire backfill ⇒ remote read, same addr"
                ),
                BackfillRoute::Local => {
                    assert_eq!(mode.remote_addr, None, "local backfill ⇒ local read")
                }
            }
        }
    }
}

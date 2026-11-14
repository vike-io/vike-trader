//! WHICH store the Data-Manager "Stored" grid reads — and which of its columns/actions exist in
//! that mode (the #1378 seam close, split-plane REQ-9's inventory half plus the §6-Q2 coverage
//! half).
//!
//! #1378 routed the grid's bulk Backfill over the wire when `config.datahub_addr` is set (the
//! `BackfillRoute` of the day, whose local arm closed on 2026-11-12), but `refresh_stored`'s
//! inventory read still opened the LOCAL store in a fat build — so a wire backfill landed its rows
//! in the SERVER's store while the grid kept rendering the local one, and the freshly-backfilled
//! series never appeared. The cure is the same ONE-value split every other store consumer makes
//! (`open_studio_store`, the desktop's `maybe_spawn_stored_backfill`), over the RESOLVED datahub
//! address (since REQ-2:
//! [`crate::data::datahub_resolve::resolve_datahub_address`] — explicit `config.datahub_addr` first, else
//! the active backend's advertisement): resolved → the grid walks the REMOTE store through the
//! `HistoryStore` TRAIT verbs (`inventory`/`series_gaps` — served by `HistoryStoreDatahub` since
//! B12/PR-6); none → no store at all, so the grid reads nothing (the desktop's local engine went
//! with the fat build).
//!
//! Following the store is only half the honesty. **Delete** (single + bulk) is offered by NEITHER
//! arm, and must be visibly absent rather than silently wrong: the delete verb is served only by a
//! KEYED datahub and this crate resolves no datahub keys, and with no datahub resolved there is no
//! store at all (the desktop's local engine, and `HistoryStoreDataFusion::delete_series` with it, went with
//! the fat build). [`DELETE_LOCAL_ONLY`] carries the whole argument, including why its name is
//! older than its text.
//!
//! **The Partial column used to be the second such surface, and is not any more.** It is fed by
//! `HistoryStore::coverage_report`, which spec §6-Q2 promoted from a concrete `HistoryStoreDataFusion` fold
//! onto the TRAIT with a datahub wire verb behind it — so a remote grid CAN compute it, against a
//! server new enough to serve it. That "new enough" is the only remaining uncertainty and it is not
//! knowable from `datahub_addr` alone, so the column now has THREE states rather than two, and
//! [`DatahubCoverage`] is the third input that picks between them. The honest note survives for
//! exactly one of them.
//!
//! (Bulk Backfill/Update needs NO row here: the wire verb is precisely what #1378 shipped, and with
//! no datahub resolved the shell's `maybe_spawn_stored_backfill` says so in the status slot.)
//!
//! This module is the PURE decision table; the shell's `refresh_stored` (`vike-desktop`) and
//! `tool_views::stored` only wire it. (This named `vike-app`'s `main.rs` and a `stored_deletes`
//! drain as the wiring until 2026-09-28; the drain is gone.)

/// The one-line reason Delete is grayed out (hover text + the gate's why), with a datahub resolved
/// and without one.
///
/// ⚠ **The TEXT changed twice and the NAME did not** (existing names change in the planned rename
/// pass, not in a fix), so the name now says more than is true: there is no local store on either
/// side of this seat.
///
/// 1. On 2026-09-07 the old *"Delete is a local-store operation — the datahub wire serves no delete
///    verb"* lost its second clause: PR #1688 built `Request::DeleteSeries`, and
///    `crates/vike-cli/src/cmd/data.rs` reaches it over `--addr`. The CONCLUSION stood: the verb is
///    served only by a KEYED datahub (`docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`),
///    and nothing in the shell or `vike-app-core` resolves `VIKE_DATAHUB_CONTROL_KEY`, the key a
///    delete needs (`vike-cli` learned to in #1691 and this GUI has not). So a Delete from here would
///    be refused at the far end whichever kind of server answered: unadvertised on a key-less one,
///    unauthenticated on a keyed one. The observe key IS resolved, through
///    `crate::backend::backend_registry::datahub_observe_key_name`, for the market-data session — an
///    Observe key, which a delete does not accept.
/// 2. On 2026-11-12 it lost "local-store only here". The desktop has no local store engine, so the
///    no-datahub arm of [`stored_mode`], which used to GRANT Delete, now grays it with this same
///    reason: a grid still holding the last remote load after the backend dropped offered a Delete
///    whose confirm modal reached a tombstone in the shell and deleted nothing, silently.
///
/// The day this crate resolves datahub keys, the remote arm can grant it, and the name goes too.
pub const DELETE_LOCAL_ONLY: &str =
    "Delete needs a keyed datahub, and this app holds no datahub keys";

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
/// It is a NEGOTIATED fact, so it exists only after a dial: `vike-desktop`'s `refresh_stored` learns it
/// from the remote walk's `coverage_report` call (served → [`Self::Served`], refused/failed →
/// [`Self::Unserved`]) and remembers it for the next render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatahubCoverage {
    /// Nothing has been dialled yet (the pre-first-load render), or no datahub is resolved, where no
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
/// RESOLVED datahub address, the SAME value `open_studio_store` and the desktop's
/// `maybe_spawn_stored_backfill` branch on, so the store the grid READS, the plane a backfill WRITES
/// and the actions the UI OFFERS can never disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredModeTable {
    /// `Some(addr)` → the grid walks the REMOTE trait store at `addr`; `None` → no datahub is
    /// resolved and the grid reads nothing.
    pub remote_address: Option<String>,
    /// `Some(reason)` → Delete (single + bulk) is grayed out with this reason; [`DELETE_LOCAL_ONLY`]
    /// in both arms today.
    pub delete_unavailable: Option<&'static str>,
    /// `Some(note)` → the Partial column cannot be filled in this mode; the note is SHOWN.
    /// `None` → the column renders what the last load folded (the wire answer, or nothing).
    pub partials_unavailable: Option<&'static str>,
}

/// The mode decision. `datahub_addr` is the RESOLVED datahub address the composition root arrived
/// at — since REQ-2 that is `config.datahub_addr` when the operator set one, else the ACTIVE
/// backend's own advertisement (see [`crate::data::datahub_resolve`]); `None` = no datahub at all.
/// `coverage` is what the last dial negotiated (see [`DatahubCoverage`]; [`DatahubCoverage::Unknown`]
/// at a call site that only reads `remote_address`/`delete_unavailable`). The read mode is the address
/// itself: with the local arm closed there is no route left to derive it through.
///
/// The Partial column's three states:
///
/// | mode | coverage | Partial column |
/// |---|---|---|
/// | no resolved datahub | *ignored* — nothing is dialled | rendered from what the last load left (nothing before one) |
/// | remote | [`DatahubCoverage::Served`] | rendered, from the wire answer |
/// | remote | [`DatahubCoverage::Unserved`] / [`DatahubCoverage::Unknown`] | [`PARTIALS_UNSERVED`] |
pub fn stored_mode(datahub_addr: Option<&str>, coverage: DatahubCoverage) -> StoredModeTable {
    match datahub_addr {
        Some(addr) => StoredModeTable {
            remote_address: Some(addr.to_string()),
            delete_unavailable: Some(DELETE_LOCAL_ONLY),
            partials_unavailable: match coverage {
                DatahubCoverage::Served => None,
                DatahubCoverage::Unserved | DatahubCoverage::Unknown => Some(PARTIALS_UNSERVED),
            },
        },
        None => StoredModeTable {
            remote_address: None,
            delete_unavailable: Some(DELETE_LOCAL_ONLY),
            partials_unavailable: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_datahub_address_reads_no_store_and_grants_no_delete() {
        assert_eq!(
            stored_mode(None, DatahubCoverage::Unknown),
            StoredModeTable {
                remote_address: None,
                delete_unavailable: Some(DELETE_LOCAL_ONLY),
                partials_unavailable: None,
            },
            "no datahub: nothing is read, Delete stays grayed, the Partial column adds no note"
        );
    }

    /// The no-datahub mode never dials, so no negotiated answer may change ANY of its three columns
    /// — a stale `DatahubCoverage` left over from a previous remote session must not put the
    /// unserved note on it.
    #[test]
    fn no_datahub_mode_ignores_the_negotiated_coverage_answer() {
        for coverage in
            [DatahubCoverage::Unknown, DatahubCoverage::Served, DatahubCoverage::Unserved]
        {
            assert_eq!(
                stored_mode(None, coverage).partials_unavailable,
                None,
                "nothing was dialled, whatever {coverage:?} says"
            );
        }
    }

    /// ⚠ **No datahub grants no Delete.** There is no local store engine to delete from, so a
    /// Delete offered with no datahub resolved was a confirm modal over a no-op: the grid still
    /// showed the last remote load's rows after the backend dropped, the click passed the gate, and
    /// nothing was deleted and nothing said so. The reason names what a delete needs.
    #[test]
    fn no_datahub_grants_no_delete() {
        let reason = stored_mode(None, DatahubCoverage::Unknown)
            .delete_unavailable
            .expect("with no datahub there is nothing to delete from");
        assert!(reason.contains("keyed datahub"), "{reason}");
        assert!(!reason.contains("local"), "there is no local store to point at: {reason}");
    }

    #[test]
    fn a_datahub_address_reads_the_remote_store_carrying_the_address_verbatim() {
        assert_eq!(
            stored_mode(Some("<host>:7878"), DatahubCoverage::Served).remote_address.as_deref(),
            Some("<host>:7878")
        );
    }

    #[test]
    fn remote_mode_grays_delete_with_a_reason() {
        let t = stored_mode(Some("127.0.0.1:7878"), DatahubCoverage::Served);
        let reason = t.delete_unavailable.expect("remote-mode Delete must carry a visible reason");
        // ⚠ It must say WHY, which is the half that went stale once the wire grew the verb: a
        // `contains("local")` here stayed green through a reason that had become false.
        assert!(
            reason.contains("key"),
            "the reason must name the key requirement — the wire DOES serve a delete verb since \
             #1688, and what stops this app is holding no datahub keys: {reason}"
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
            (None, DatahubCoverage::Unknown, None),
            (Some("127.0.0.1:7878"), DatahubCoverage::Served, None),
            (Some("127.0.0.1:7878"), DatahubCoverage::Unserved, Some(PARTIALS_UNSERVED)),
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
            stored_mode(Some("127.0.0.1:7878"), DatahubCoverage::Served).partials_unavailable,
            None,
            "the wire answers this column now; the local-only note would be a false claim"
        );
    }

    /// State 3: an OLDER server keeps the honest note — and the note may not claim the column is
    /// local-only (it is not, since the verb exists), only that THIS peer did not answer.
    #[test]
    fn remote_mode_against_an_older_server_keeps_an_honest_note() {
        let t = stored_mode(Some("127.0.0.1:7878"), DatahubCoverage::Unserved);
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
            stored_mode(Some("127.0.0.1:7878"), DatahubCoverage::Unknown).partials_unavailable,
            Some(PARTIALS_UNSERVED)
        );
    }

    /// The read mode is the resolved address itself, verbatim, for both inputs — the value the
    /// shell's `maybe_spawn_stored_backfill` sends a backfill to. A grid that READS one plane while
    /// its Backfill button writes another is exactly the seam this module closes (#1378); this pinned
    /// the agreement through `backfill_route` until that route's local arm closed.
    #[test]
    fn the_grid_reads_the_address_a_backfill_is_sent_to() {
        for input in [None, Some("<host>:7878")] {
            assert_eq!(
                stored_mode(input, DatahubCoverage::Served).remote_address.as_deref(),
                input
            );
        }
    }
}

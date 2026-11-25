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
//! [`crate::data::datahub::resolve_datahub_address`] — explicit `config.datahub_addr` first, else
//! the active backend's advertisement): resolved → the grid walks the REMOTE store through the
//! `HistoryStore` TRAIT verbs (`inventory`/`series_gaps` — served by `HistoryStoreDatahub` since
//! B12/PR-6); none → no store at all, so the grid reads nothing (the desktop's local engine went
//! with the fat build).
//!
//! Following the store is only half the honesty. **Delete** (single + bulk) is offered by NEITHER
//! arm, and must be visibly absent rather than silently wrong: the desktop sends no delete of its
//! own (the wire's `Request::DeleteSeries` is `vike-cli`'s), and with no datahub resolved there is no
//! store at all (the desktop's local engine, and `HistoryStoreDataFusion::delete_series` with it, went with
//! the fat build). `DELETE_LOCAL_ONLY` carries the whole argument, including why its name is
//! older than its text.
//!
//! **The Partial column used to be the second such surface, and is not any more.** It is fed by
//! `HistoryStore::coverage_report`, which spec §6-Q2 promoted from a concrete `HistoryStoreDataFusion` fold
//! onto the TRAIT with a datahub wire verb behind it — so a remote grid CAN compute it, against a
//! server new enough to serve it. That "new enough" is the only remaining uncertainty and it is not
//! knowable from `datahub_address` alone, so the column now has THREE states rather than two, and
//! [`DatahubCoverage`] is the third input that picks between them. The honest note survives for
//! exactly one of them.
//!
//! (Bulk Backfill/Update needs NO row here: the wire verb is precisely what #1378 shipped, and with
//! no datahub resolved the shell's `maybe_spawn_stored_backfill` says so in the status slot.)
//!
//! This module is the PURE decision table; the shell's `refresh_stored` (`vike-desktop`) and
//! `windows::data_manager`'s inventory only wire it. (This named `vike-app`'s `main.rs` and a `stored_deletes`
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
///    and this GUI held no key a delete is admitted with (`vike-cli` learned to in #1691). So a
///    Delete from here was refused at the far end whichever kind of server answered: unadvertised on
///    a key-less one, unauthenticated on a keyed one. Since decision 0288 every dial signs with this
///    PC's device key and the server's access list decides the role; the seat stays grayed.
/// 2. On 2026-11-12 it lost "local-store only here". The desktop has no local store engine, so the
///    no-datahub arm of [`stored_mode`], which used to GRANT Delete, now grays it with this same
///    reason: a grid still holding the last remote load after the backend dropped offered a Delete
///    whose confirm modal reached a tombstone in the shell and deleted nothing, silently.
///
/// 3. On 2026-11-16 it lost "this app holds no datahub keys": since the ACCESS cutover the desktop
///    signs every datahub dial with this PC's device key, so the sentence was false on every PC that
///    had connected. What is still true is that the desktop has no delete of its own to send.
///
/// The day the desktop sends `Request::DeleteSeries` (signed by the device key, the access list
/// deciding), the remote arm can grant it, and the name goes too.
pub(crate) const DELETE_LOCAL_ONLY: &str = "Delete is not built into the desktop: remove a series on the server with `vike-cli data history rm`";

/// The one-line note shown for the Partial column when the CONNECTED datahub could not answer the
/// cross-kind coverage verb (a visible disclosure, never a silent empty).
///
/// ⚠ This replaces the old `PARTIALS_LOCAL_ONLY`, whose text said the coverage report was "not a
/// wire verb". That claim was true when it was written and is now FALSE — §6-Q2 built the verb — so
/// the constant was renamed rather than left standing with a stale reason inside it. The note that
/// remains makes the narrower, still-true claim: *this* peer did not answer.
///
/// It covers a read that FAILED on a server that serves the verb (an error, or no byte within the
/// idle bound). A server older than the verb has its own words, [`PARTIALS_OLDER_SERVER`]: the two
/// used to share one note that guessed between them ("an older server, or a read failure"), and it
/// was shown even when the verb was never asked because the store itself could not be read.
pub(crate) const PARTIALS_UNSERVED: &str = "Partial column unavailable: the datahub's cross-kind coverage report could not be read (see the log)";

/// The Partial column's note against a datahub that does not serve the cross-kind coverage report
/// at all (it does not advertise the verb, so nothing was sent).
pub(crate) const PARTIALS_OLDER_SERVER: &str = "Partial column unavailable: this datahub is older than the desktop and does not serve the \
     cross-kind coverage report";

/// What the connected datahub said about the cross-kind coverage verb — the third input to the
/// Partial column's state, and the one `config.datahub_addr` cannot supply.
///
/// It is a NEGOTIATED fact, so it exists only after a dial: `vike-desktop`'s `refresh_stored` learns it
/// from the remote walk's `coverage_report` call and remembers it for the next render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatahubCoverage {
    /// The verb has not been asked: nothing has been dialled yet (the pre-first-load render), no
    /// datahub is resolved, or the last load could not read the store at all (it skips the report,
    /// and the page says the store could not be read). No note: each of these draws an EMPTY grid,
    /// so the column claims nothing, and a note would describe a question nobody asked.
    Unknown,
    /// The dial succeeded and the report came back — the column renders from the WIRE answer,
    /// folded by the same `partial_days_from_coverage` a local grid folds.
    Served,
    /// The server predates the verb (refused before anything was sent). The column shows
    /// `PARTIALS_OLDER_SERVER`.
    Older,
    /// The server serves the verb and the read failed. The column shows `PARTIALS_UNSERVED`.
    Unserved,
}

/// The Stored grid's per-mode capability table — computed ONCE per decision point from the
/// RESOLVED datahub address, the SAME value `open_studio_store` and the desktop's
/// `maybe_spawn_stored_backfill` branch on, so the store the grid READS, the plane a backfill WRITES
/// and the actions the UI OFFERS can never disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataManagerInventoryMode {
    /// `Some(address)` → the grid walks the REMOTE trait store at `address`; `None` → no datahub is
    /// resolved and the grid reads nothing.
    pub datahub_address: Option<String>,
    /// `Some(reason)` → Delete (single + bulk) is grayed out with this reason; `DELETE_LOCAL_ONLY`
    /// in both arms today.
    pub delete_unavailable: Option<&'static str>,
    /// `Some(note)` → the Partial column cannot be filled in this mode; the note is SHOWN.
    /// `None` → the column renders what the last load folded (the wire answer, or nothing).
    pub(crate) partials_unavailable: Option<&'static str>,
}

/// The mode decision. `datahub_address` is the RESOLVED datahub address the composition root arrived
/// at — since REQ-2 that is `config.datahub_addr` when the operator set one, else the ACTIVE
/// backend's own advertisement (see [`crate::data::datahub`]); `None` = no datahub at all.
/// `coverage` is what the last dial negotiated (see [`DatahubCoverage`]; [`DatahubCoverage::Unknown`]
/// at a call site that only reads `datahub_address`/`delete_unavailable`). The read mode is the address
/// itself: with the local arm closed there is no route left to derive it through.
///
/// The Partial column's states:
///
/// | mode | coverage | Partial column |
/// |---|---|---|
/// | no resolved datahub | *ignored* — nothing is dialled | rendered from what the last load left (nothing before one) |
/// | remote | [`DatahubCoverage::Served`] / [`DatahubCoverage::Unknown`] | rendered (an empty grid for `Unknown`), no note |
/// | remote | [`DatahubCoverage::Older`] | `PARTIALS_OLDER_SERVER` |
/// | remote | [`DatahubCoverage::Unserved`] | `PARTIALS_UNSERVED` |
pub fn stored_mode(
    datahub_address: Option<&str>,
    coverage: DatahubCoverage,
) -> DataManagerInventoryMode {
    match datahub_address {
        Some(address) => DataManagerInventoryMode {
            datahub_address: Some(address.to_string()),
            delete_unavailable: Some(DELETE_LOCAL_ONLY),
            partials_unavailable: match coverage {
                DatahubCoverage::Served | DatahubCoverage::Unknown => None,
                DatahubCoverage::Older => Some(PARTIALS_OLDER_SERVER),
                DatahubCoverage::Unserved => Some(PARTIALS_UNSERVED),
            },
        },
        None => DataManagerInventoryMode {
            datahub_address: None,
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
            DataManagerInventoryMode {
                datahub_address: None,
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
        for coverage in [
            DatahubCoverage::Unknown,
            DatahubCoverage::Served,
            DatahubCoverage::Older,
            DatahubCoverage::Unserved,
        ] {
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
    /// nothing was deleted and nothing said so. The reason names the verb that deletes.
    #[test]
    fn no_datahub_grants_no_delete() {
        let reason = stored_mode(None, DatahubCoverage::Unknown)
            .delete_unavailable
            .expect("with no datahub there is nothing to delete from");
        assert!(reason.contains("vike-cli data history rm"), "{reason}");
        assert!(!reason.contains("local"), "there is no local store to point at: {reason}");
    }

    #[test]
    fn a_datahub_address_reads_the_remote_store_carrying_the_address_verbatim() {
        assert_eq!(
            stored_mode(Some("<host>:7878"), DatahubCoverage::Served).datahub_address.as_deref(),
            Some("<host>:7878")
        );
    }

    #[test]
    fn remote_mode_grays_delete_with_a_reason() {
        let t = stored_mode(Some("127.0.0.1:17878"), DatahubCoverage::Served);
        let reason = t.delete_unavailable.expect("remote-mode Delete must carry a visible reason");
        // ⚠ It must say WHY and where to go instead, the half that went stale twice: a
        // `contains("local")` stayed green through a reason that had become false, and so did a
        // `contains("key")` once the device key signed every datahub dial.
        assert!(
            reason.contains("not built into the desktop")
                && reason.contains("vike-cli data history rm"),
            "the reason must say the desktop sends no delete and name the verb that does: {reason}"
        );
    }

    /// THE §6-Q2 DECISION TABLE — the Partial column's states, in one place, over the two inputs
    /// that decide it. This is the test that would go red if the coverage verb were ever unwired,
    /// if the note were shown against a server that does answer, or if the older-server and the
    /// failed-read words merged back into one guess.
    #[test]
    fn the_partial_column_states() {
        let local = "127.0.0.1:17878";
        let table = [
            // (datahub_addr, negotiated coverage) -> the note, or None = the column renders
            (None, DatahubCoverage::Unknown, None),
            (Some(local), DatahubCoverage::Served, None),
            (Some(local), DatahubCoverage::Unknown, None),
            (Some(local), DatahubCoverage::Older, Some(PARTIALS_OLDER_SERVER)),
            (Some(local), DatahubCoverage::Unserved, Some(PARTIALS_UNSERVED)),
        ];
        for (address, coverage, expected) in table {
            assert_eq!(
                stored_mode(address, coverage).partials_unavailable,
                expected,
                "addr={address:?} coverage={coverage:?}"
            );
        }
    }

    /// State 2 is the whole point of §6-Q2: a remote grid against a server that SERVES the verb
    /// renders the column, exactly like a local one — no note, nothing grayed.
    #[test]
    fn remote_mode_against_a_serving_server_renders_the_partial_column() {
        assert_eq!(
            stored_mode(Some("127.0.0.1:17878"), DatahubCoverage::Served).partials_unavailable,
            None,
            "the wire answers this column now; the local-only note would be a false claim"
        );
    }

    /// An OLDER server and a FAILED read each get their own words: the older server's note says the
    /// server is older and guesses no failure, the failed read's names no older server. Neither may
    /// claim the column is local-only (the verb exists).
    #[test]
    fn an_older_server_and_a_failed_read_say_different_things() {
        let note = |coverage| {
            stored_mode(Some("127.0.0.1:17878"), coverage)
                .partials_unavailable
                .expect("both carry a visible note")
        };
        let (older, failed) = (note(DatahubCoverage::Older), note(DatahubCoverage::Unserved));
        assert!(older.contains("older than the desktop"), "{older}");
        assert!(!older.contains("log") && !older.contains("failure"), "no guess: {older}");
        assert!(failed.contains("could not be read") && !failed.contains("older"), "{failed}");
        for n in [older, failed] {
            assert!(n.contains("unavailable") && n.contains("coverage"), "{n}");
            assert!(!n.contains("not a wire verb"), "the pre-§6-Q2 reason is FALSE: {n}");
        }
    }

    /// The read mode is the resolved address itself, verbatim, for both inputs — the value the
    /// shell's `maybe_spawn_stored_backfill` sends a backfill to. A grid that READS one plane while
    /// its Backfill button writes another is exactly the seam this module closes (#1378); this pinned
    /// the agreement through `backfill_route` until that route's local arm closed.
    #[test]
    fn the_grid_reads_the_address_a_backfill_is_sent_to() {
        for input in [None, Some("<host>:7878")] {
            assert_eq!(
                stored_mode(input, DatahubCoverage::Served).datahub_address.as_deref(),
                input
            );
        }
    }
}

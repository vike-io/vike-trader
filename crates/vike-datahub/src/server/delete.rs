//! The `DeleteSeries` verb: the one destructive verb on this wire, and the one a KEY-LESS server
//! refuses outright. `handle_request` (the parent module) routes the request here with the `keyed`
//! fact the connection loop knows; the refusal text this answers a key-less server with,
//! `KEYLESS_DELETE_REFUSAL`, is a public constant of the parent module (tests and the CLI name it),
//! so it stays there.

use super::*;

/// The `DeleteSeries` verb: refuse on a key-less server, plan, assert, then delete.
///
/// # ⚠ The key-less refusal is FIRST, and it is unconditional
///
/// Before the selector is validated, before the store is touched, before anything is reported. This
/// is the narrowing `docs/decisions/0025-datahub-remote-posture.md`'s argument demands and does not
/// itself state: 0025 records that the BACKFILL write verb is why authentication must come at the
/// first non-loopback need, and its reason 1 — *"A surface with a write verb and no way to say
/// 'reads yes, writes no' cannot be handed even a trusted LAN"* — applies with more force to a verb
/// that destroys the only copy. The answer here is stronger than 0025 asked for and in the same
/// direction: the key-less DEFAULT does not get a weaker version of this verb, it gets none.
///
/// `served_features` refuses to advertise it too, so a well-behaved client never sends one; this is
/// the half that holds for a client that does.
///
/// # ⚠ The provenance filter is RESOLVED before the sweep gate reads it
///
/// **This is a NARROWING of a signed-off wire, and it is deliberate.** Until 2026-09-11 the raw
/// `produced_by` spelling went straight to [`removal::plan_removal`] and the sweep gate below asked
/// only whether it was `None`. A BLANK value is `Some("")`, which is not `None` — so it satisfied
/// the require-provenance-before-a-wildcard-delete gate, and then satisfied the provenance check
/// too, VACUOUSLY: `crates/vike-data/src/store_kind.rs`'s `key_matches_prefix` is `starts_with`,
/// every key starts with the empty string, `RemovalPlan::verdict` found no foreign key in any
/// series, and `crates/vike-data/src/datafusion_hist.rs`'s `delete_series_checked` re-check under
/// the series lock — the TOCTOU guard the whole verb turns on — passed for the same reason. **Two
/// guards and a re-check fell to one token, on the verb that takes the only copy.**
///
/// A server that accepts a wildcard delete wearing an assertion is not honouring §5.4 of
/// `docs/superpowers/specs/2026-09-07-cli-data-rm-design.md`; it is failing to implement it. So the
/// spelling is resolved through `vike_datahub_client::proto`'s `resolve_produced_by` — the ONE
/// validator, re-exported beside the removal vocabulary precisely so both ends of this wire share
/// one definition — at the DOOR: before the plan, before `inventory()`, before a single
/// `series_commits` read.
///
/// Two consequences worth stating rather than discovering:
///
/// * **It is unconditional** — sweep or fully-named, `dry_run` or not. A blank assertion is
///   meaningless on a named series too, and it is actively harmful there: it flips a keyless series
///   from "deletable, provenance none recorded" to "refused". And refusing a DRY RUN matters because
///   the dry-run arm below returns the plan without consulting `RemovalPlan::verdict` — so a blank
///   dry run used to render `provenance: SATISFIED`, and the MCP surface's `preview_token` binds to
///   exactly that plan.
/// * **A producer PATH now RESOLVES here** rather than being asserted literally. That is the second
///   acceptance change and it closes a defect `crates/vike-cli/src/cmd/data.rs`'s
///   `refuse_a_producer_path_on_the_remote_route` had to paper over: the same command line answered
///   two ways depending on `--store` versus `--addr`. An UNDECLARED path is refused by name (it
///   fails closed and deletes nothing) instead of matching no key and reporting the operator's data
///   as foreign.
///
/// The KEY-LESS refusal stays unconditionally FIRST — see above; it is about the SERVER, not about
/// the request, and `crates/vike-datahub/tests/auth_roundtrip.rs`'s
/// `a_keyless_server_serves_no_delete_verb` pins it.
///
/// # The sweep rule
///
/// A SWEEP (any wildcarded dimension) must carry `produced_by`, and the gate now reads a RESOLVED
/// value. A fully-named single series need
/// not — that is byte-for-byte the act the Data Manager's Delete already performs behind a confirm
/// modal, and requiring more of one surface than of another for the identical operation is a rule
/// nobody keeps. ⚠ The MCP surface requires it unconditionally on ITS side, which is a decision
/// about the CALLER rather than about the store, and so does not live here.
pub(super) fn delete_series_verb(
    selector: &removal::SeriesSelector,
    produced_by: Option<&str>,
    dry_run: bool,
    keyed: bool,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    if !keyed {
        return Response::Error(KEYLESS_DELETE_REFUSAL.to_string());
    }
    // AT THE DOOR: nothing is planned, no series is enumerated and no provenance is read until the
    // argument itself is known to be an assertion. The resolver's message quotes the spelling back
    // and says what is wrong with it, which is what an operator who typed something needs.
    let produced_by = match produced_by.map(resolve_produced_by) {
        Some(Ok(prefix)) => Some(prefix),
        Some(Err(why)) => return Response::Error(format!("delete_series: {why}")),
        None => None,
    };
    let produced_by = produced_by.as_deref();
    if selector.is_sweep() && produced_by.is_none() {
        return Response::Error(format!(
            "refusing a SWEEP with no provenance assertion: {} matches more than one series, and \
             deleting by name alone is what `produced_by` exists to replace. Name every dimension, \
             or pass the commit-key prefix the rows carry.",
            selector.describe()
        ));
    }
    let plan = match removal::plan_removal(store.as_ref(), selector, produced_by) {
        Ok(p) => p,
        Err(e) => return Response::Error(e.to_string()),
    };
    if dry_run {
        return Response::Deleted(DeleteDone { plan, outcome: None });
    }
    match removal::execute_removal(store.as_ref(), &plan) {
        // A partial failure rides the OUTCOME, not an error: the series that went are gone, and a
        // caller that was told only "it failed" would not know which.
        Ok(outcome) => Response::Deleted(DeleteDone { plan, outcome: Some(outcome) }),
        // A provenance refusal deleted NOTHING, so it is an error rather than an outcome — the
        // request did not happen.
        Err(e) => Response::Error(e.to_string()),
    }
}

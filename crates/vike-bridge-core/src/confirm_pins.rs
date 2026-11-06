//! The scenarios every `crate::confirm::ConfirmByCoid` client answers alike (`test-support` only).
//!
//! bybit's and okx's `tests/offline/confirm_by_coid.rs` drive their own REST client over their own
//! lookup stub; what a found / absent / failed answer must turn into is the shared form's contract
//! (`crate::confirm`'s module doc), so those assertions are written here once and each venue hands in
//! clients built over its stub's answers. Each venue keeps its `#[test]`s, its answers and the
//! assertions that read its stub (which endpoints were asked, in which order).

use std::sync::Arc;

use vike_model::events::Event;

use crate::confirm::{ConfirmByCoid, DispatchLedger, confirm_closure};
use crate::rest::VenueRest;

/// A FAILED query says nothing about the order: each client (one per way the venue's query fails)
/// answers `confirm_order` with no event at all.
pub fn assert_failed_queries_emit_nothing<R: VenueRest>(failing: impl IntoIterator<Item = R>) {
    for c in failing {
        assert!(c.confirm_order("c-failed", 5).is_empty());
    }
}

/// "Absent" is trusted only once the exec thread finished dispatching the coid: `absent` (a client whose
/// lookup finds nothing) is silent for an undispatched coid and rejects a dispatched one; `live` (one
/// whose lookup finds the order) accepts without any ledger.
pub fn assert_absent_rejects_only_a_dispatched_coid<R: ConfirmByCoid>(absent: R, live: R) {
    assert!(absent.confirm_when("q1", 5, false).is_empty());
    assert!(matches!(absent.confirm_when("q1", 5, true)[..], [Event::OrderRejected(_)]));
    assert!(matches!(live.confirm_when("q2", 5, false)[..], [Event::OrderAccepted(_)]));
}

/// The closure the exec client hands to `ExecActor::with_confirm` reads the dispatch ledger BEFORE the
/// query: over `absent` it rejects a dispatched coid only and skips a repeat inside the backoff, over
/// each `failing` client it is silent even for a dispatched coid, and over `live` it accepts with no
/// ledger entry.
pub fn assert_the_closure_rejects_absent_only_after_dispatch<R: ConfirmByCoid + Sync + 'static>(
    absent: R,
    live: R,
    failing: impl IntoIterator<Item = R>,
) {
    let ledger = Arc::new(DispatchLedger::default());
    let absent = confirm_closure(absent, ledger.clone(), |_| true);
    assert!(absent("q1").is_empty(), "still queued: absent proves nothing");
    ledger.mark("q2");
    assert!(matches!(absent("q2")[..], [Event::OrderRejected(_)]), "dispatched: absent is real");
    assert!(absent("q2").is_empty(), "the same coid inside the backoff is skipped");

    for (i, client) in failing.into_iter().enumerate() {
        let coid = format!("f{i}");
        ledger.mark(&coid);
        let failing = confirm_closure(client, ledger.clone(), |_| true);
        assert!(failing(&coid).is_empty(), "a failed query is silence even once dispatched");
    }

    let found = confirm_closure(live, Arc::new(DispatchLedger::default()), |_| true);
    assert!(matches!(found("q3")[..], [Event::OrderAccepted(_)]), "found needs no ledger");
}

/// `run` (the exec thread) is private and starts with network reads, so the wiring is pinned at the
/// source: `<manifest_dir>/src/exec.rs` must wrap the command loop's client in `Dispatched`. Lose it and
/// the ledger is never stamped, so every "absent" answer goes silent and the reject path is quietly
/// disabled.
pub fn assert_the_command_loop_client_is_wrapped_in_dispatched(manifest_dir: &str) {
    let path = std::path::Path::new(manifest_dir).join("src/exec.rs");
    let src = std::fs::read_to_string(&path).expect("the exec source is readable");
    let wrapped = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .any(|l| l.contains("&Dispatched { rest: routed, ledger }"));
    assert!(wrapped, "{} no longer wraps run_loop's client in Dispatched", path.display());
}

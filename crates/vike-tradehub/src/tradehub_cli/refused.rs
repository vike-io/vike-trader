//! **A start refused for a cause only an operator mends exits 78** (`vike_utility::cli::OPERATOR_NEEDED_EXIT`, sysexits'
//! `EX_CONFIG`; owner 2026-11-14: the tradehub never crash-loops). The unit's `RestartPreventExitStatus=78` and the
//! server install's supervisor (`deploy/docker/entrypoint.sh`'s `SERVER_RULES`, whose tradehub row pages) PARK it on
//! that status and page once, where any other non-zero status is restarted every 5 s into the same refusal.
//!
//! Such a cause is one a restart meets again: a refused configuration (a settings row, the daemon profile, the run
//! profile and its risk budget, a mount, a feed plan, the live gate's own checks), a profile store that exists and
//! cannot be read, a gatewayhub Trade key nothing in the process can mint. A gatewayhub, a datahub or a venue that does
//! not answer is NOT one: the start waits for the hubs (`super::hub_wait`), and a venue that did not connect keeps
//! today's restart (`super::live_runtime::LiveStartFailed`).
//!
//! The door is gone with the process, so the refusal is also left as ONE note in the state directory
//! ([`vike_paths::state_path::TRADEHUB_REFUSED_START_FILE`]), which `vike-cli trade health` on the box reads while the
//! door does not answer; the next start removes it ([`clear_refusal_note`]) before anything can refuse. A note that
//! cannot be written or removed is said at `warn` and changes nothing else: it is a reading, never a gate.

use std::path::Path;
use std::process::ExitCode;

/// **Refuse the start for good**: say so once at `error`, leave the note in `state_dir` (`None`: no project, no
/// note), and return 78. `why` is the refusal in the words the operator mends it by; this is the one log line of
/// it (its site returns it unlogged, log once where the decision is made).
pub(super) fn refused_start(state_dir: Option<&Path>, why: &str) -> ExitCode {
    tracing::error!(
        cause = %why,
        "the start is REFUSED for a cause only an operator mends, so this tradehub exits {} and is NOT restarted \
         (its supervisor parks it and pages once): mend the cause, then `bin/vikectl restart tradehub` (system \
         units: `systemctl restart vike-tradehub`)",
        vike_utility::cli::OPERATOR_NEEDED_EXIT
    );
    if let Some(dir) = state_dir
        && let Err(e) = std::fs::write(
            dir.join(vike_paths::state_path::TRADEHUB_REFUSED_START_FILE),
            note(why, vike_model::now_ms()),
        )
    {
        tracing::warn!(error = %e, "the refusal note for `vike-cli trade health` was not written");
    }
    ExitCode::from(vike_utility::cli::OPERATOR_NEEDED_EXIT)
}

/// The note's text: when (`now_ms`, UTC), then the refusal.
fn note(why: &str, now_ms: i64) -> String {
    format!("refused at {}\n{why}\n", vike_model::time::epoch_ms_to_utc_timestamp(now_ms))
}

/// **Remove the note an earlier start left**: this start has not refused (yet). Absent is the ordinary case.
pub(super) fn clear_refusal_note(state_dir: Option<&Path>) {
    let Some(dir) = state_dir else { return };
    match std::fs::remove_file(dir.join(vike_paths::state_path::TRADEHUB_REFUSED_START_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(error = %e, "an earlier start's refusal note could not be removed")
        }
    }
}

#[cfg(test)]
#[path = "refused_tests.rs"]
mod refused_tests;

//! The recording phases: the profile load, the startup mode line, the exit status.

#[cfg(feature = "record")]
use std::process::ExitCode;

use super::args::{ProfileSource, RECORDER_PROFILE_FLAG, RecordRequest};

/// **Phase: the recording profile** — `None` when no recording was asked for, `Some` once the
/// profile has been read and passed every refusal `crate::recorder` owns, and `Err` carrying the
/// exit status (2: a refused CONFIGURATION, nothing opened or bound) when it was not. `run` calls
/// this BEFORE the bind guard, the store open and the listener, and that position is the point.
#[cfg(feature = "record")]
pub(super) fn load_recording_profile(
    record: Option<&RecordRequest>,
    booted: &vike_boot::Booted,
    resolved: &vike_model::paths::store_path::StoreRoot,
    cwd: Option<&std::path::Path>,
) -> Result<Option<vike_recorder::config::RecorderProfile>, ExitCode> {
    // ⚠⚠ **EVERY RECORDING REFUSAL HAPPENS HERE — BEFORE THE BIND GUARD, THE STORE OPEN AND THE
    // LISTENER.** `crate::recorder::load_and_check_profile` carries the four questions and the
    // argument; the ORDER is this file's to keep, and getting it wrong has a name. These checks
    // used to live inside `crate::recorder::record`, which this file reaches only after the port is
    // bound and the serve thread is spawned — so a profile with a typo'd `store` key did not
    // REFUSE TO START, it bound `VIKE_DATAHUB_ADDR`, brought the wire up, refused, exited, and let
    // `Restart=on-failure` do it again every five seconds. A crash-loop is strictly worse than a
    // refusal: it takes the data wire up and down, it buries the one line that says why under a
    // repeating boot sequence, and `systemctl status` shows "activating" rather than "failed".
    //
    // Everything below this point can still fail (a port in use, a store that will not open) — but
    // those are failures of an ACTION, and this is a refusal of a CONFIGURATION. Exit 2 is the
    // family this binary already uses for the second kind, and it is deliberately distinct from the
    // `1` a recording that RAN and then broke returns through `finish_recording`.
    match record {
        Some(req) => {
            // ⚠ FIRST, and before anything that can fail: the retired-flag warning. A deploy that
            // rolls back after this point must still have left the operator the one line telling
            // them to edit the unit — a warning only printed on the happy path is a warning nobody
            // gets on the day it matters. `vike_log::init` has run by here, so it reaches the JSON
            // file layer every service manager captures.
            if let Some(w) = &req.retired_flag_warning {
                tracing::warn!("{w}");
            }
            // ⚠ The ROW arm reads the store BEFORE the bind guard too, which is the whole point of
            // this block's position in the sequence — see `crate::recorder`'s
            // `load_and_check_profile_row` for the ONE question that differs (a profile NAME the
            // store does not hold is a REFUSAL, not the empty answer `read_profiles` gives a
            // selection).
            let loaded = match &req.profile {
                ProfileSource::Row(name) => crate::recorder::load_and_check_profile_row(
                    name,
                    booted.settings_dir_override.as_deref(),
                    &resolved.root,
                    cwd,
                ),
                // The VALUELESS flag. It reads the same store through the same loader; only the
                // NAME is resolved rather than given, and it is resolved by the one function the
                // CLI's own default also calls (`Profiles::resolve_active`), so the profile the
                // CLI calls the default and the profile this daemon mounts cannot become two
                // answers. Its three refusals are three different next commands — see
                // `crate::recorder::load_and_check_active_profile_row`.
                ProfileSource::ActiveRow => crate::recorder::load_and_check_active_profile_row(
                    booted.settings_dir_override.as_deref(),
                    &resolved.root,
                    cwd,
                ),
            };
            match loaded {
                Ok(p) => Ok(Some(p)),
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "vike-datahub: refusing to start — the recording was refused before \
                         anything was opened or bound, so nothing on this box changed"
                    );
                    eprintln!("vike-datahub: {e}");
                    Err(ExitCode::from(2))
                }
            }
        }
        None => Ok(None),
    }
}

/// The recording's own exit statuses, kept in ONE place so the dry run and the daemon cannot map
/// the same outcome differently.
///
/// `EXIT_SILENT` (3) and `EXIT_DRY_RUN` (4) are deliberately distinct from `1` (a startup failure)
/// and `2` (a refused configuration): a supervisor should be able to tell "the daemon worked and
/// the DATA did not" apart from "the profile was bad". Their own docs carry the the CI box runs that
/// made each one necessary.
///
/// ⚠ **A bad PROFILE no longer reaches this function at all** — a read failure, a parse failure, a
/// store-root disagreement and a venue this build cannot record are all answered by
/// `crate::recorder::load_and_check_profile` before the listener binds, and exit **2**. What is
/// left for the `Err` arm here is a recording that STARTED and then broke: the writer thread would
/// not spawn, a feed refused to mount. That split is what makes the status readable — `2` means
/// nothing was opened or bound and the fix is in a file; `1` means the daemon ran.
#[cfg(feature = "record")]
pub(super) fn finish_recording(
    outcome: Result<crate::recorder::RecordOutcome, String>,
) -> std::process::ExitCode {
    match outcome {
        Ok(crate::recorder::RecordOutcome::Stopped) => ExitCode::SUCCESS,
        Ok(crate::recorder::RecordOutcome::Silent(series)) => {
            tracing::error!(
                series = ?series,
                "vike-datahub: exiting on silence (--exit-on-silence) — these subscribed series \
                 are receiving no rows"
            );
            eprintln!("vike-datahub: exiting on silence: {}", series.join(", "));
            ExitCode::from(crate::recorder::EXIT_SILENT)
        }
        Ok(crate::recorder::RecordOutcome::DryRunFailed(reasons)) => {
            for why in &reasons {
                tracing::error!(reason = %why, "vike-datahub: --once dry run FAILED");
            }
            eprintln!(
                "vike-datahub: --once proved NOTHING — this profile would record no data:\n  {}",
                reasons.join("\n  ")
            );
            ExitCode::from(crate::recorder::EXIT_DRY_RUN)
        }
        Err(e) => {
            tracing::error!(error = %e, "vike-datahub: recording failed");
            eprintln!("vike-datahub: {e}");
            ExitCode::FAILURE
        }
    }
}

/// **WHICH MODE IS THIS DAEMON IN** — recording, or serving only — as one line, said at startup
/// before anything opens a store or binds a port.
///
/// ⚠ **It exists because forgetting `--record` is a SILENT no-op, and one shipped.** The runbook's
/// install path put a bare `datahub` on the unit's `ExecStart=`, so an install its own page called
/// "recording" came up healthy, answered every query, passed `systemctl is-active`, and
/// accumulated nothing — indistinguishable from a venue that is merely quiet, and invisible until
/// somebody queried a date range and got zero rows. Every other symptom this daemon has is loud;
/// this one had no symptom at all, so the cure is a line that makes the mode a FACT in the journal:
/// `journalctl -u vike-datahub | grep 'vike-datahub: '` answers it without reading a unit file.
///
/// It is a pure function of the parse so it can be tested in BOTH builds ([`the_mode_line_says_which_mode`]);
/// the `run` bodies below only print it. It names the profile PATH deliberately — on a box with
/// two datahubs "recording" is not enough to tell you WHICH tape.
// Only the serving `run` prints it — the feature-off stub has no subscriber and no run to describe,
// it just says what it cannot do and exits. The tests below exercise it in every build, which is
// what makes this an `allow` rather than a `#[cfg]` on the function itself.
#[cfg_attr(not(feature = "serve-datafusion"), allow(dead_code, clippy::allow_attributes))]
pub(super) fn startup_mode_line(record: Option<&RecordRequest>) -> String {
    match record {
        // ⚠ The SOURCE is named, not just the profile: since 2026-09-16 a profile can come from a
        // file or from a `recorder` row, and "which store answered" is exactly the question an
        // operator reading a journal hours later cannot reconstruct. It is the same reason
        // `vike-cli secrets list` leads with a `source:` line.
        Some(req) => format!(
            "vike-datahub: RECORDING AND SERVING — venue feeds fill the very store this server \
             answers from ({})",
            match &req.profile {
                ProfileSource::Row(n) =>
                    format!("{RECORDER_PROFILE_FLAG} {n} (a row in the settings store)"),
                // ⚠ It names the FLAG and not a profile, because at this point in the startup
                // sequence no store has been read and the name is genuinely not known yet. The
                // resolved name reaches the journal through `RecordArgs::profile`, which is built
                // after the load — a line here that guessed one would be the wrong kind of
                // certain.
                ProfileSource::ActiveRow => format!(
                    "{RECORDER_PROFILE_FLAG} with no value — the ACTIVE recorder row in the \
                     settings store"
                ),
            }
        ),
        None => {
            format!(
                "vike-datahub: SERVE-ONLY — no --record or {RECORDER_PROFILE_FLAG}, so this \
                 daemon subscribes to NO venue and writes NO rows; it answers from a store some \
                 other process fills. Add `{RECORDER_PROFILE_FLAG} <name>` to the unit's \
                 ExecStart= to make it record"
            )
        }
    }
}

/// The feature-free [`RecordRequest`] this file parses, as the `record` module's own argument type.
///
/// Two types rather than one because the PARSE must compile in every build and `RecordArgs` lives
/// behind the feature — see [`RecordRequest`]'s doc. The conversion is the seam, and
/// [`the_parser_defaults_match_the_recorders`] is what keeps the two spellings of each default from
/// drifting.
#[cfg(feature = "record")]
pub(super) fn to_record_args(req: &RecordRequest) -> crate::recorder::RecordArgs {
    crate::recorder::RecordArgs {
        profile: match &req.profile {
            ProfileSource::Row(n) => format!("profile row `{n}`"),
            // ⚠ It says HOW the row was chosen, not just which — "the active row" is the fact an
            // operator needs when a unit's ExecStart= carries no name to compare against. The
            // resolved name is not threaded here deliberately: this struct is built from the
            // PARSE, and reaching into the loaded profile for it would make provenance depend on a
            // read that has already happened somewhere else.
            ProfileSource::ActiveRow => "the ACTIVE recorder profile row".to_string(),
        },
        tick: std::time::Duration::from_secs(req.tick_secs),
        once: req.once,
        silent_secs: req.silent_secs,
        exit_on_silence: req.exit_on_silence,
    }
}

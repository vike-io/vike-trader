//! The recording phases: the profile load, the per-kind retention row, the startup mode line, the
//! exit status.

use std::collections::BTreeMap;
#[cfg(feature = "datafusion-serve")]
use std::process::ExitCode;

use vike_data::StoreKindId;

#[cfg(feature = "record")]
use super::args::{DEFAULT_SILENT_SECS, DEFAULT_TICK_SECS};
use super::args::{ProfileSource, RECORDER_PROFILE_FLAG, RecordRequest};

/// **Phase: the per-kind retention row** — the box's `config.recorder_retention_days_by_kind`, as the
/// `StoreKindId -> days` map `crate::recorder::record` hands the maintenance pass. `Err` carries the
/// exit status (2: a refused CONFIGURATION, nothing opened or bound). `run` calls this BEFORE the
/// bind guard, beside the profile load, for the profile load's reason: a refusal must be a refusal and
/// not a crash-loop after a port is bound.
///
/// The row was already refused at the write and at the load (`vike_config::Config::apply`, and a
/// stored row it refuses stops the boot), so this refusal is the reader's own belt: it parses with
/// the same function and would only fire on a build whose kind list had drifted from the grammar's
/// (`the_retention_grammar_and_the_store_kinds_agree` holds them together).
#[cfg(feature = "datafusion-serve")]
pub(super) fn load_retention_by_kind(
    booted: &vike_boot::Booted,
) -> Result<BTreeMap<StoreKindId, u32>, ExitCode> {
    match retention_by_kind_of(booted.settings.config.recorder_retention_days_by_kind.as_deref()) {
        Ok(map) => Ok(map),
        Err(e) => {
            tracing::error!(
                error = %e,
                "vike-datahub: refusing to start — the per-kind retention row was refused before \
                 anything was opened or bound, so nothing on this box changed"
            );
            eprintln!("vike-datahub: {e}");
            Err(ExitCode::from(2))
        }
    }
}

/// The retention map a `config.recorder_retention_days_by_kind` row says: empty for an absent row and
/// for the empty string (today's behaviour), otherwise one entry per kind named.
///
/// A pure function of the row so it is tested in every build. The grammar and every refusal are
/// `vike_config::retention::parse_retention_days_by_kind`'s, which already ran when the row was
/// written; this turns its kind WORDS into `StoreKindId`s, and refuses a word the enum does not
/// spell rather than skipping it.
// Only the serving `run` calls it (through `load_retention_by_kind`); the tests exercise it in every
// build, which is what makes this an `allow` rather than a `#[cfg]` (`startup_mode_line`'s shape).
#[cfg_attr(not(feature = "datafusion-serve"), allow(dead_code, clippy::allow_attributes))]
pub(super) fn retention_by_kind_of(
    row: Option<&str>,
) -> Result<BTreeMap<StoreKindId, u32>, String> {
    let Some(row) = row else {
        return Ok(BTreeMap::new());
    };
    let pairs = vike_config::retention::parse_retention_days_by_kind(row)
        .map_err(|why| format!("config.recorder_retention_days_by_kind is refused: {why}"))?;
    let mut map = BTreeMap::new();
    for (word, days) in pairs {
        let kind: StoreKindId = word.parse().map_err(|e| {
            format!(
                "config.recorder_retention_days_by_kind is refused: `{word}` is not a store kind \
                 this build knows ({e})"
            )
        })?;
        map.insert(kind, days);
    }
    Ok(map)
}

/// **Phase: the recording profile** — `None` when no recording was asked for, `Some` once the
/// profile has been read and passed every refusal `crate::recorder` owns, and `Err` carrying the
/// exit status (2: a refused CONFIGURATION, nothing opened or bound) when it was not. `run` calls
/// this BEFORE the bind guard, the store open and the listener, and that position is the point.
#[cfg(feature = "record")]
pub(super) fn load_recording_profile(
    record: Option<&RecordRequest>,
    booted: &vike_boot::Booted,
    resolved: &vike_paths::store_path::StoreRoot,
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
#[cfg_attr(not(feature = "datafusion-serve"), allow(dead_code, clippy::allow_attributes))]
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

/// Where a recording knob's value came from — said in the startup line, so an operator reading a
/// journal can tell a flag from a row from the compiled-in default.
#[cfg(feature = "record")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    /// The command line named it.
    Flag,
    /// The box's `config.*` row.
    Row,
    /// Neither: the compiled-in default.
    Default,
}

/// One timing knob: the flag if the command line gave one, else the box's row, else `default`.
///
/// The flag wins because it is typed by the person running THIS command and is the per-invocation
/// override the daemon had before the row existed; the row is for the box whose unit passes
/// nothing (the shipped unit passes neither). Decision 0111 forbids a CLI layer over `Policy` and an
/// environment layer over everything; a flag over a `Config` row is neither.
#[cfg(feature = "record")]
fn pick(flag: Option<u64>, row: Option<u32>, default: u64) -> (u64, Source) {
    match (flag, row) {
        (Some(v), _) => (v, Source::Flag),
        (None, Some(v)) => (u64::from(v), Source::Row),
        (None, None) => (default, Source::Default),
    }
}

/// The recording's arguments after the settings are folded in, with what the rows said about it.
///
/// ⚠ `disclosure` and `warnings` are `None`/empty for a box with no recorder row and a command line
/// that names nothing new: the startup log of a box that never heard of the rows is unchanged.
#[cfg(feature = "record")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Resolved {
    pub(super) args: crate::recorder::RecordArgs,
    /// One line naming every knob a ROW supplied and its value, or `None` when no row did.
    pub(super) disclosure: Option<String>,
    /// What a row's value costs, one line each: a detection weakened, or a bigger loss window.
    pub(super) warnings: Vec<String>,
}

/// The feature-free [`RecordRequest`] this file parses, folded with the box's rows into the `record`
/// module's own argument type.
///
/// Two types rather than one because the PARSE must compile in every build and `RecordArgs` lives
/// behind the feature — see [`RecordRequest`]'s doc. The conversion is the seam, and
/// [`the_parser_defaults_match_the_recorders`] is what keeps the two spellings of each default from
/// drifting.
///
/// It is also where the `config.recorder_*` rows are read (`run` hands it the boot's `Config`):
/// `--tick-secs` and `--silent-secs` beat the row, the row beats the default, and the two flush
/// rows have no flag. A box with no row and no flag gets the defaults, so its `RecordArgs` is
/// exactly what this function built before the rows existed.
///
/// # Errors
/// `--exit-on-silence` with detection off by the `config.recorder_silent_secs` row: an exit that
/// could never fire, found here because the parser reads no settings. (The flag spelling of the same
/// contradiction is refused by [`read_args`](super::args::read_args).)
#[cfg(feature = "record")]
pub(super) fn to_record_args(
    req: &RecordRequest,
    config: &vike_config::Config,
) -> Result<Resolved, String> {
    use std::time::Duration;

    let (tick_secs, tick_from) = pick(req.tick_secs, config.recorder_tick_secs, DEFAULT_TICK_SECS);
    let (silent_secs, silent_from) =
        pick(req.silent_secs, config.recorder_silent_secs, DEFAULT_SILENT_SECS);
    if req.exit_on_silence && silent_secs == 0 {
        return Err("--exit-on-silence needs silence detection on (--silent-secs > 0), but the \
             config.recorder_silent_secs row is 0, which switches the watchdog off: pass \
             --silent-secs <N>, or write the row above 0 (`vike-cli config set \
             config.recorder_silent_secs 300`)"
            .to_string());
    }
    let flush_max_rows = config.recorder_flush_max_rows.map(|v| v as usize);
    let flush_max_age = config.recorder_flush_max_age_secs.map(|s| Duration::from_secs(s.into()));

    let args = crate::recorder::RecordArgs {
        profile: match &req.profile {
            ProfileSource::Row(n) => format!("profile row `{n}`"),
            // ⚠ It says HOW the row was chosen, not just which — "the active row" is the fact an
            // operator needs when a unit's ExecStart= carries no name to compare against. The
            // resolved name is not threaded here deliberately: this struct is built from the
            // PARSE, and reaching into the loaded profile for it would make provenance depend on a
            // read that has already happened somewhere else.
            ProfileSource::ActiveRow => "the ACTIVE recorder profile row".to_string(),
        },
        tick: Duration::from_secs(tick_secs),
        once: req.once,
        silent_secs,
        exit_on_silence: req.exit_on_silence,
        flush_max_rows,
        flush_max_age,
    };

    // What the rows did, for the log. Nothing is said for a knob the flag or the default decided.
    let mut supplied = Vec::new();
    let mut warnings = Vec::new();
    if tick_from == Source::Row {
        supplied.push(format!("tick {tick_secs} s (config.recorder_tick_secs)"));
        if tick_secs > 60 {
            warnings.push(format!(
                "a {tick_secs} s tick (config.recorder_tick_secs) paces every watchdog verdict: \
                 the family-collapse window closes on the first tick past 30 s, so that rule's \
                 detection latency doubles by 60 s and keeps growing with the tick, and a \
                 Polymarket window (five minutes) has fewer chances to be seen listed before the \
                 current one expires"
            ));
        }
    }
    if silent_from == Source::Row {
        supplied.push(format!("silence threshold {silent_secs} s (config.recorder_silent_secs)"));
        if silent_secs == 0 {
            warnings.push(
                "the config.recorder_silent_secs row is 0: the silence watchdog is OFF — no \
                 recency, rate, family or never-resolved verdict, no log line and no alert"
                    .to_string(),
            );
        } else if silent_secs < DEFAULT_SILENT_SECS {
            warnings.push(format!(
                "a {silent_secs} s silence threshold (config.recorder_silent_secs) is under the \
                 {DEFAULT_SILENT_SECS} s default: a Polymarket five-minute rotation can be quiet \
                 for that long, so expect false alarms"
            ));
        }
    }
    let defaults = vike_store::RecorderConfig::default();
    if let Some(rows) = flush_max_rows {
        supplied.push(format!("flush at {rows} rows (config.recorder_flush_max_rows)"));
        if rows > defaults.max_rows {
            warnings.push(format!(
                "flushing at {rows} rows (config.recorder_flush_max_rows) instead of {} loses \
                 more buffered tape on a hard kill (SIGKILL, an OOM kill, a power cut) and holds \
                 more memory; a graceful stop still flushes it",
                defaults.max_rows
            ));
        }
    }
    if let Some(age) = flush_max_age {
        supplied.push(format!(
            "flush at {} s of age (config.recorder_flush_max_age_secs)",
            age.as_secs()
        ));
        if age > defaults.max_age {
            warnings.push(format!(
                "flushing at {} s of age (config.recorder_flush_max_age_secs) instead of {} s \
                 loses more buffered tape on a hard kill and holds more memory; a graceful stop \
                 still flushes it",
                age.as_secs(),
                defaults.max_age.as_secs()
            ));
        }
    }
    let disclosure = (!supplied.is_empty())
        .then(|| format!("recording knobs from settings rows: {}", supplied.join(", ")));
    Ok(Resolved { args, disclosure, warnings })
}

/// **Phase: the recording's arguments** — `None` when no recording was asked for, `Some` once the
/// flags and the box's `config.recorder_*` rows are folded ([`to_record_args`]), and `Err` carrying
/// the exit status (2: a refused CONFIGURATION, nothing opened or bound) when they contradict each
/// other. `run` calls this right after [`load_recording_profile`], so it sits BEFORE the bind guard,
/// the store open and the listener for the same reason that one does.
#[cfg(feature = "record")]
pub(super) fn load_record_args(
    record: Option<&RecordRequest>,
    config: &vike_config::Config,
) -> Result<Option<crate::recorder::RecordArgs>, ExitCode> {
    let Some(req) = record else { return Ok(None) };
    match to_record_args(req, config) {
        Ok(resolved) => {
            if let Some(line) = &resolved.disclosure {
                tracing::info!("vike-datahub: {line}");
            }
            for warning in &resolved.warnings {
                tracing::warn!("vike-datahub: {warning}");
            }
            Ok(Some(resolved.args))
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "vike-datahub: refusing to start — the recording was refused before anything was \
                 opened or bound, so nothing on this box changed"
            );
            eprintln!("vike-datahub: {e}");
            Err(ExitCode::from(2))
        }
    }
}

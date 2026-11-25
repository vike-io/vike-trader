//! `vike-cli config profile create daemon` — **the one act that gets a fresh box from "no `--config`, no
//! active row, refuses to start" to a running paper mount**, with no profile TOML anywhere.
//!
//! # Why this exists
//!
//! decision 0086 ("settings live only in the database") verdict 1 forbids reading a profile
//! TOML from any binary, so a daemon-profile BODY reaches the store only through a writer that
//! takes no file. A box whose store holds no `active` daemon row would otherwise have NO WAY to
//! get one: `vike-tradehub`'s `--config <profile.toml>` argument is retired (it decides nothing —
//! see `crate::cmd::config::profile::activate`'s module doc for the ROW-wins ruling this predates), and
//! `config profile activate` only flips the `active` column of a body that is already stored.
//!
//! This verb is the missing rung: it builds a `ProfileRecord` DIRECTLY from argv — never through a
//! file, never through TOML text at all — and stores + activates it in one act. It is deliberately
//! narrow: a single mount, the fields a paper A-S mount actually needs
//! (`crates/vike-tradehub/src/config/validate.rs`'s `ProfileDaemon::validate` requires only `venue`
//! and `symbol`, which this verb takes as `--symbol` or, a polymarket token id the door does not read, `--token-id`
//! (both land in the mount's one `symbol` column); everything else is optional exactly as the TOML spelling is — see
//! `vike_secrets::profile_store::MountRow`'s own doc). It is a BOOTSTRAP path for a box with nothing,
//! not a general profile editor: changing an existing profile's fields is still open work (0086's
//! OQ1/OQ2 in the phase-3 scout notes), and this verb does not attempt it.
//!
//! Three flags carry what the OANDA live recipe (`docs/ops/tradehub-container.md`) states beyond the
//! mount's identity: `--interval-ms` (the mount row's own `interval_ms` column) and
//! `--summary-ms` / `--shutdown-deadline-ms` (the `[daemon]` table, stored as the
//! `daemon.summary_ms` / `daemon.shutdown_deadline_ms` profile settings). They need no new
//! storage — `MountRow` and `ProfileRecord::settings` already carry them — and a flag left off
//! stores no row, so the daemon's own defaults stay the daemon's. The OANDA recipe is why they
//! exist: its quote reader observes its stop flag only between the venue's ~5 s heartbeats, so
//! its teardown needs the 10 s deadline the default does not give.
//!
//! Three things the flags do not say on their face. `--interval-ms` is compared with `--interval`
//! only when BOTH are given in the same run (a lone `--interval 5m` leaves the mount's 60000 ms
//! default in place, unchecked). Re-running the verb under the same name REPLACES the stored body,
//! so a knob left off the second time returns to its default rather than keeping its old value —
//! EXCEPT the body's RUN part (`risk.*`, `guards.*`, `sinks.*`), which `config profile create run` writes
//! and this verb KEEPS (decision 0297: one live profile, two writers), saying so in its report.
//! And a `--shutdown-deadline-ms` that is not under the shipped unit's `TimeoutStopSec=` prints a
//! one-line stderr warning (`stop_timeout_warning`): that unit's 10 s equals the OANDA recipe's
//! deadline, and the stop timeout has to be raised with it.
//!
//! # Why `vike-cli` builds the row itself rather than reusing `vike_tradehub::config::ProfileDaemon`
//!
//! `vike-cli` is layer 35; `vike-tradehub` is layer 65. `crates/vike-ops/tests/architecture/layer_gate.rs` refuses
//! a normal dependency the wrong direction, so this crate cannot parse or validate a `ProfileDaemon`
//! at all. The fields this verb accepts are therefore a hand-kept subset of `MountConfig`'s own keys,
//! matched 1:1 against `vike_secrets::profile_store::MountRow`'s columns (which already mirror the
//! TOML spelling verbatim); the daemon's own loader (`ProfileDaemon::from_toml_str`, reached through
//! `crate::profile_rows::rows_to_daemon_profile` at boot) is what actually validates the body,
//! exactly as it validates any stored one.
//!
//! # Why store-then-activate rather than two verbs
//!
//! `crate::cmd::config::profile::activate::run_activate` is the deliberate second act for a body that is
//! already stored. That reason does not apply to a genuinely fresh box: there is nothing to
//! migrate FROM, so withholding activation here would just add a second command with nothing left
//! for it to check. `--dry-run` is the inspection step instead.

use std::path::Path;
use std::process::ExitCode;

use vike_secrets::profile_store::{
    MountRow, ProfileKind, ProfileOperatorWrite, ProfileRecord, ProfileRow, edit_profiles,
    is_run_part, read_profiles,
};

use crate::cmd::args;

/// `config profile create daemon`'s arguments, as `clap` reads them. Every required one is kept
/// optional here so that its absence is refused in this verb's own words, after the parse (they
/// name the permitted words), and the numbers are kept raw so each refusal names its flag.
#[derive(clap::Args, Debug)]
#[command(
    about = "Build a daemon profile FROM ARGUMENTS (no file, ever) and activate it",
    long_about = "Store a daemon profile with ONE mount, built entirely from these arguments —\n\
                  never from a file, never through the settings-file rungs (0086) — and make it \
                  the ACTIVE\ndaemon profile. This is the bootstrap path for a box with no \
                  profile at all.\nRe-running it under the same name \
                  replaces the stored body in place, so a knob left off the\nsecond time returns \
                  to its default."
)]
pub(crate) struct BootstrapDaemonCommand {
    #[arg(value_name = "NAME", help = "the profile NAME the body is stored under")]
    positionals: Vec<String>,
    #[arg(long, value_name = "VENUE", help = "REQUIRED")]
    venue: Option<String>,
    #[arg(long, value_name = "CLASS", help = "REQUIRED")]
    asset_class: Option<String>,
    #[arg(
        long,
        value_name = "SYM",
        help = "the mount symbol (mutually exclusive with --token-id)"
    )]
    symbol: Option<String>,
    #[arg(long, value_name = "ID", help = "the polymarket spelling of --symbol")]
    token_id: Option<String>,
    #[arg(long, value_name = "I", help = "the mount row's bar `interval`, e.g. 1m")]
    interval: Option<String>,
    #[arg(
        long,
        value_name = "MS",
        help = "the mount's bar window in ms (default 60000); checked against --interval only \
                when --interval is given too"
    )]
    interval_ms: Option<String>,
    #[arg(
        long,
        value_name = "MS",
        help = "the daemon's snapshot-summary cadence in ms (default 5000)"
    )]
    summary_ms: Option<String>,
    #[arg(
        long,
        value_name = "MS",
        help = "the daemon's bounded-teardown deadline in ms (default 5000). It must stay UNDER \
                the unit's TimeoutStopSec= (10 s as shipped) and a container's --stop-timeout; a \
                value not under 10 s warns on stderr"
    )]
    shutdown_deadline_ms: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "REQUIRED: the mount's strategy FILE, its path under the project's strategies/ \
                folder (a .rs or .rhai file)"
    )]
    strategy: Option<String>,
    #[arg(long, value_name = "N", help = "the strategy file's `qty` (its `[strategy.params]`)")]
    qty: Option<String>,
    #[arg(
        long,
        value_name = "N",
        help = "the strategy file's `half_spread` (its `[strategy.params]`)"
    )]
    half_spread: Option<String>,
    #[arg(long, value_name = "N", help = "the mount row's `tick_size`")]
    tick_size: Option<String>,
    #[arg(long, value_name = "N", help = "the mount row's `seed_cash`")]
    seed_cash: Option<String>,
    #[arg(long, value_name = "LABEL", help = "the mount row's `account` label")]
    account: Option<String>,
    #[arg(long, help = "the mount row's `data_only` switch")]
    data_only: bool,
    #[arg(long, help = "report the verdict and write nothing")]
    dry_run: bool,
}
/// There is no constant for it in the daemon: the number lives in `deploy/vike-tradehub.service`
/// (`TimeoutStopSec=20`) and, for a container, in the ops page's `--stop-timeout`. This one is
/// pinned to the unit's text by `crates/vike-cli/tests/cli_verbs/bootstrap_daemon_cli.rs`, which reads the
/// unit itself — so a stop timeout raised in the unit makes that test fail until this follows it.
/// `crates/vike-tradehub/src/tradehub_cli/tests/stop_and_deadlines.rs`'s
/// `the_default_shutdown_deadline_fits_inside_the_units_stop_timeout` requires the deadline
/// STRICTLY under the stop timeout (the observe publisher's stop sits outside the capped teardown
/// and rides the difference) but reads only the DEFAULT deadline, so a deadline raised through this
/// verb was checked by nothing.
const SHIPPED_UNIT_STOP_TIMEOUT_MS: i64 = 20_000;

/// The one-line warning for a `--shutdown-deadline-ms` that is not under the shipped unit's stop
/// timeout, or `None`. A warning only: the verb cannot change the unit, and a box whose unit was
/// already raised is legitimate, so the exit code stays neutral.
fn stop_timeout_warning(shutdown_deadline_ms: Option<i64>) -> Option<String> {
    let ms = shutdown_deadline_ms.filter(|ms| *ms >= SHIPPED_UNIT_STOP_TIMEOUT_MS)?;
    let secs = SHIPPED_UNIT_STOP_TIMEOUT_MS / 1000;
    Some(format!(
        "warning: --shutdown-deadline-ms {ms} is not under the shipped unit's TimeoutStopSec={secs} \
         ({SHIPPED_UNIT_STOP_TIMEOUT_MS} ms) — raise TimeoutStopSec= (and a container's \
         --stop-timeout) above it, for example to {}, or SIGKILL cuts the teardown in half",
        2 * secs
    ))
}

#[derive(Debug)]
pub(in crate::cmd::config) struct BootstrapArgs {
    name: String,
    venue: String,
    asset_class: String,
    symbol: Option<String>,
    token_id: Option<String>,
    interval: Option<String>,
    interval_ms: Option<i64>,
    summary_ms: Option<i64>,
    shutdown_deadline_ms: Option<i64>,
    strategy_file: String,
    qty: Option<f64>,
    half_spread: Option<f64>,
    tick_size: Option<f64>,
    seed_cash: Option<f64>,
    account: Option<String>,
    data_only: bool,
    dry_run: bool,
}

fn parse_number(flag: &str, v: String) -> Result<f64, String> {
    v.parse::<f64>().map_err(|_| format!("{flag} must be a number, got '{v}'"))
}

/// A whole, positive number of milliseconds — the shape of `--interval-ms`, `--summary-ms` and
/// `--shutdown-deadline-ms`. The daemon's loader takes any `u64` for the last two, so this is the
/// verb's own sanity check: a zero deadline times out a teardown that never got to run, a zero
/// cadence is no cadence, and a bar window of zero or below is no bar.
fn parse_ms(flag: &str, v: String) -> Result<i64, String> {
    match v.parse::<i64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!(
            "{flag} must be a whole number of milliseconds greater than zero, got '{v}'"
        )),
    }
}

impl BootstrapDaemonCommand {
    fn into_args(self) -> Result<BootstrapArgs, String> {
        let mut name = None;
        for word in self.positionals {
            if name.is_some() {
                return Err(format!("unexpected extra argument '{word}'"));
            }
            name = Some(word);
        }
        let ms = |flag: &str, v: Option<String>| v.map(|v| parse_ms(flag, v)).transpose();
        let number = |flag: &str, v: Option<String>| v.map(|v| parse_number(flag, v)).transpose();
        let interval_ms = ms("--interval-ms", self.interval_ms)?;
        let summary_ms = ms("--summary-ms", self.summary_ms)?;
        let shutdown_deadline_ms = ms("--shutdown-deadline-ms", self.shutdown_deadline_ms)?;
        let qty = number("--qty", self.qty)?;
        let half_spread = number("--half-spread", self.half_spread)?;
        let tick_size = number("--tick-size", self.tick_size)?;
        let seed_cash = number("--seed-cash", self.seed_cash)?;
        let (symbol, token_id, interval) = (self.symbol, self.token_id, self.interval);

        match (&symbol, &token_id) {
            (Some(_), Some(_)) => {
                return Err(
                    "set --symbol OR --token-id, not both (they name the same field)".into()
                );
            }
            (None, None) => {
                return Err(
                    "a mount symbol is required: --symbol <sym> (or, on polymarket, --token-id \
                     <id>)"
                        .into(),
                );
            }
            _ => {}
        }

        // The recipes say "`interval_ms` must agree with `interval`", and the mount takes the two
        // independently — a disagreement is a bar window that is not the series it is labelled as.
        // Only an interval `vike_marketdata::interval::interval_ms` can measure is judged;
        // `1w`/`1mo` have no width there and are not refused.
        if let (Some(i), Some(ms)) = (&interval, interval_ms)
            && let Some(want) = vike_marketdata::interval::interval_ms(i)
            && want != ms
        {
            return Err(format!(
                "--interval-ms {ms} disagrees with --interval {i}, which is {want} ms — the mount \
                 takes the two independently, so they must name one bar width"
            ));
        }

        Ok(BootstrapArgs {
            name: name.ok_or("missing the profile NAME")?,
            venue: self.venue.ok_or("missing --venue <venue>, REQUIRED")?,
            asset_class: self.asset_class.ok_or_else(|| {
                format!(
                    "missing --asset-class <class>, REQUIRED. The permitted words are: {}",
                    vike_model::AssetClass::SQL_WORDS.join(", ")
                )
            })?,
            symbol,
            token_id,
            interval,
            interval_ms,
            summary_ms,
            shutdown_deadline_ms,
            // No default strategy exists, so a profile without its file is not one the daemon mounts.
            strategy_file: self.strategy.ok_or(
                "missing --strategy <path>, REQUIRED: the path of the mount's strategy file \
                 under the project's strategies/ folder (a .rs or .rhai file). A mount without a \
                 file is not mounted",
            )?,
            qty,
            half_spread,
            tick_size,
            seed_cash,
            account: self.account,
            data_only: self.data_only,
            dry_run: self.dry_run,
        })
    }
}

/// Read `config profile create daemon`'s argv tail through the plane's own parser, so a test meets the
/// grammar an operator does.
#[cfg(test)]
pub(in crate::cmd::config) fn parse_bootstrap_daemon(
    argv: impl Iterator<Item = String>,
) -> Result<BootstrapArgs, args::Stop> {
    use crate::cmd::config::{CreateKind, ProfileVerb, Sub};
    parse_config_verb!(
        argv,
        "profile create daemon",
        cmd in Sub::Profile { verb: ProfileVerb::Create { kind: CreateKind::Daemon(cmd) } },
        "vike-cli config profile create daemon"
    )
}

/// Entry point for `config profile create daemon`.
pub(crate) fn run(cmd: BootstrapDaemonCommand, settings_dir: Option<&Path>) -> ExitCode {
    let args = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse("vike-cli config profile create daemon", msg),
    };
    if let Some(warning) = stop_timeout_warning(args.shutdown_deadline_ms) {
        eprintln!("vike-cli config profile create daemon: {warning}");
    }
    match bootstrap(&args, settings_dir, vike_model::time::clock::now_secs()) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli config profile create daemon: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// **A daemon mount's symbol as stored** (decision 0290): the user name `venue`'s KEPT list in the project of `settings`
/// (`<project>/instruments/<venue>.json`, the datahub's) answers, through the door `vike-cli account
/// set-symbol` reads (`vike_catalog::venue_engine_symbol`), or its refusal; with no list of the venue on this box, the
/// name's shape (`vike_catalog::door_symbol`), which the daemon's start answers; a venue with no rule keeps it as typed.
fn mount_symbol(settings: &Path, venue: &str, typed: &str) -> Result<String, String> {
    let Some(shape) = vike_catalog::door_symbol(venue, typed) else {
        return Ok(typed.to_string());
    };
    let kept = vike_paths::state_path::instruments_dir_beside(Some(settings))
        .and_then(|dir| vike_catalog::read_kept_list(&dir, venue))
        .map(|list| list.instruments)
        .unwrap_or_default();
    if kept.is_empty() { shape } else { vike_catalog::venue_engine_symbol(venue, typed, &kept) }
}

fn already_active_daemon(db: &Path) -> Option<String> {
    read_profiles(db).ok().and_then(|p| p.active(ProfileKind::Daemon).map(|s| s.row.name.clone()))
}

/// [`crate::cmd::config::crossing_line`]'s sentence for a store that selects no daemon profile.
pub(in crate::cmd::config) const UNSELECTED: &str = " Nothing selects a daemon profile in this store today, \
                                      so this is the CROSSING that lets `vike-tradehub` start \
                                      with no `--config` at all.";

pub(in crate::cmd::config) fn bootstrap(
    args: &BootstrapArgs,
    settings_dir: Option<&Path>,
    now_utc: i64,
) -> Result<String, String> {
    let dir = settings_dir.ok_or(
        "no settings directory resolved — there is no store to write a profile into. Set \
         VIKE_SETTINGS_DIR, or run from a project that has one.",
    )?;
    if vike_model::AssetClass::from_sql_word(&args.asset_class).is_none() {
        return Err(format!(
            "'{}' is not an asset class vike knows. The permitted words are: {}",
            args.asset_class,
            vike_model::AssetClass::SQL_WORDS.join(", ")
        ));
    }
    let db = vike_secrets::db_path_in(dir);

    let mut mount = MountRow::new(0, &args.venue, &args.asset_class);
    // The user name, as `account set-symbol` stores an account's (the C-D4 follow-up, decision 0290): with the
    // venue's kept list on this box, the name its row answers is stored canonical and one no row answers is refused
    // in the door's words; with none (a fresh project, before the datahub's first refresh), the name's shape is
    // stored and the daemon's start answers it; a venue with no rule keeps the name as typed.
    // `--token-id` lands in the same ONE name column (PM-3), as typed and unread by the door, as it always was.
    mount.symbol = match &args.token_id {
        Some(token) => Some(token.clone()),
        None => args.symbol.as_deref().map(|s| mount_symbol(dir, &args.venue, s)).transpose()?,
    };
    mount.interval = args.interval.clone();
    mount.interval_ms = args.interval_ms;
    mount.tick_size = args.tick_size;
    // The mount's strategy FILE: the daemon reads it, its header and the params below at its start.
    mount.strategy_file = Some(args.strategy_file.clone());
    // `--qty` and `--half-spread` land in the file's `[strategy.params]` table (step 10 of the
    // one-broker plan: the daemon refuses the retired `qty`/`half_spread` mount columns at load), where
    // the file's header judges them like any other key.
    let mut params = std::collections::BTreeMap::new();
    for (key, value) in [("qty", args.qty), ("half_spread", args.half_spread)] {
        if let Some(v) = value {
            params
                .insert((0, key.to_string()), vike_model::params::ParamValue::Float(v).to_string());
        }
    }
    mount.seed_cash = args.seed_cash;
    mount.account = args.account.clone();
    // ⚠ `Some(true)` or ABSENT, never `Some(false)` — a stored `false` would freeze a default onto
    // the row that the TOML spelling never carries (`MountRow`'s own doc).
    mount.data_only = args.data_only.then_some(true);

    // The `[daemon]` table rides `profile_setting` rows at their dotted paths
    // (`vike_tradehub::profile_rows`'s `daemon_profile_to_rows` writes both, always). A knob left
    // off stores NO row: the daemon's own default stays the daemon's, and a stored default would
    // freeze today's number onto the profile — the `data_only` hazard above, for a number.
    let mut settings = std::collections::BTreeMap::new();
    if let Some(ms) = args.summary_ms {
        settings.insert("daemon.summary_ms".to_string(), ms.to_string());
    }
    if let Some(ms) = args.shutdown_deadline_ms {
        settings.insert("daemon.shutdown_deadline_ms".to_string(), ms.to_string());
    }
    // The RUN part a body under this name already carries is KEPT (decision 0297): it is
    // `profile create run`'s to write, and a re-stored mount must not drop the ceilings. The write keeps
    // it from the body read inside its own transaction (`edit_profiles`), so a `profile create run` landing
    // between this read and the write is kept too; this read is the rehearsal's and the report's.
    // A store this verb cannot read is refused rather than read as "no run part".
    let profiles = read_profiles(&db).map_err(|e| e.to_string())?;
    let mount_part = ProfileRecord {
        row: ProfileRow {
            name: args.name.clone(),
            kind: ProfileKind::Daemon,
            active: false,
            note: None,
        },
        mounts: vec![mount],
        params,
        settings,
        recorder: None,
    };
    let stored = keeping_run_part(mount_part.clone(), profiles.by_name(&args.name));

    let already = already_active_daemon(&db);
    let symbol_word = args.token_id.as_deref().or(args.symbol.as_deref()).unwrap_or_default();
    // A REPOINT (or a crossing) changes the ceilings with the mounts: say which run part comes in,
    // or that none does, as `config profile activate daemon` does — on the rehearsal as on the write.
    let brought = |body: &ProfileRecord| {
        if already.as_deref() == Some(args.name.as_str()) {
            String::new()
        } else {
            super::super::activate::run_part_line(body)
        }
    };

    if args.dry_run {
        return Ok(format!(
            "would store daemon profile `{}` (venue={} symbol={} asset_class={}) in {} and make it \
             ACTIVE — NOTHING WAS WRITTEN.{}{}{}{}",
            args.name,
            args.venue,
            symbol_word,
            args.asset_class,
            db.display(),
            knobs_line(args),
            kept_line(&stored),
            crate::cmd::config::crossing_line("daemon", UNSELECTED, &already, &args.name, true),
            brought(&stored),
        ));
    }

    // ONE transaction: the run part is kept from the body read under the write's own lock (so a
    // `profile create run` landing since the read above is kept), and the body is selected, both
    // committed together or not.
    let write = ProfileOperatorWrite::claim("vike-cli config profile create daemon");
    let written = edit_profiles(&db, &write, now_utc, vike_model::AssetClass::SQL_WORDS, |t| {
        let written = keeping_run_part(mount_part, t.read(&args.name)?.as_ref());
        t.store(&written)?;
        t.set_active(ProfileKind::Daemon, &args.name)?;
        Ok(written)
    })
    .map_err(|e| e.to_string())?;
    let brought = brought(&written);

    Ok(format!(
        "stored and ACTIVATED daemon profile `{}` in {} (venue={} symbol={} asset_class={}).{}{}{}{brought}\n\n\
         Restart `vike-tradehub` (no `--config` needed) to run it. `vike-cli config profile deactivate \
         daemon` steps back to a refusal — there is no settings-file rung left to fall back to \
         (0086) — and re-running this verb under the same name replaces the body in place.",
        args.name,
        db.display(),
        args.venue,
        symbol_word,
        args.asset_class,
        knobs_line(args),
        kept_line(&written),
        crate::cmd::config::crossing_line("daemon", UNSELECTED, &already, &args.name, false),
    ))
}

/// `body` (the mount part this run built) with the RUN part `held` carries, when `held` is a daemon
/// body (decision 0297: `profile create run` writes that part, and this verb keeps it).
fn keeping_run_part(mut body: ProfileRecord, held: Option<&ProfileRecord>) -> ProfileRecord {
    if let Some(held) = held.filter(|p| p.row.kind == ProfileKind::Daemon) {
        body.settings.extend(run_part_of(held));
    }
    body
}

/// The run-part rows of a daemon body.
fn run_part_of(body: &ProfileRecord) -> std::collections::BTreeMap<String, String> {
    body.settings
        .iter()
        .filter(|(path, _)| is_run_part(path))
        .map(|(path, value)| (path.clone(), value.clone()))
        .collect()
}

/// The report's line for the run part a re-store keeps: empty when there is none.
fn kept_line(body: &ProfileRecord) -> String {
    let kept: Vec<String> =
        run_part_of(body).iter().map(|(path, value)| format!("{path} = {value}")).collect();
    if kept.is_empty() {
        String::new()
    } else {
        format!(" Keeps its stored run part ({}).", kept.join(", "))
    }
}

/// The knobs this run set beyond the mount's identity, for the report — so the operator sees the
/// number that matters on this venue (a teardown deadline) land, rather than inferring it from a
/// row nobody prints. Empty when none was given.
fn knobs_line(args: &BootstrapArgs) -> String {
    let mut set = Vec::new();
    if let Some(v) = args.interval_ms {
        set.push(format!("interval_ms={v}"));
    }
    if let Some(v) = args.summary_ms {
        set.push(format!("daemon.summary_ms={v}"));
    }
    if let Some(v) = args.shutdown_deadline_ms {
        set.push(format!("daemon.shutdown_deadline_ms={v}"));
    }
    if set.is_empty() { String::new() } else { format!(" Carries {}.", set.join(" ")) }
}

#[path = "tests/daemon.rs"]
#[cfg(test)]
mod config_profile_bootstrap_tests;

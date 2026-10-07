//! `vike-cli config bootstrap-daemon` — **the one act that gets a fresh box from "no `--config`, no
//! active row, refuses to start" to a running paper mount**, with no profile TOML anywhere.
//!
//! # Why this exists
//!
//! decision 0086 ("settings live only in the database") verdict 1 forbids reading a profile
//! TOML from any binary, and `crate::cmd::config::mirror`/`mirror_profile`/
//! `mirror_recorder` — the only writers that ever put a daemon-profile BODY into the store —
//! read one to build it. Once those verbs are gone (0086 phase 1, alongside this record's owner
//! ruling), a box whose store holds no `active` daemon row has NO WAY to get one: `vike-tradehub`'s
//! `--config <profile.toml>` argument is retired (it decides nothing — see
//! `crate::cmd::config::activate`'s module doc for the ROW-wins ruling this predates), and
//! `config activate` cannot help either, because its whole fence is `--proves <file>` — comparing
//! the stored rows against a document that, on a genuinely fresh box, does not exist.
//!
//! This verb is the missing rung: it builds a `StoredProfile` DIRECTLY from argv — never through a
//! file, never through TOML text at all — and stores + activates it in one act. It is deliberately
//! narrow: a single mount, the fields a paper A-S mount actually needs
//! (`crates/vike-tradehub/src/config/validate.rs`'s `DaemonProfile::validate` requires only `venue`
//! and one of `symbol`/`token_id`; everything else is optional exactly as the TOML spelling is — see
//! `vike_secrets::profile_store::MountRow`'s own doc). It is a BOOTSTRAP path for a box with nothing,
//! not a general profile editor: changing an existing profile's fields is still open work (0086's
//! OQ1/OQ2 in the phase-3 scout notes), and this verb does not attempt it.
//!
//! Three flags carry what the shipped recipes (`docs/ops/tradehub-oanda-live.toml` first) state beyond the
//! mount's identity: `--interval-ms` (the mount row's own `interval_ms` column) and
//! `--summary-ms` / `--shutdown-deadline-ms` (the `[daemon]` table, stored as the
//! `daemon.summary_ms` / `daemon.shutdown_deadline_ms` profile settings). They need no new
//! storage — `MountRow` and `StoredProfile::settings` already carry them — and a flag left off
//! stores no row, so the daemon's own defaults stay the daemon's. The OANDA recipe is why they
//! exist: its quote reader observes its stop flag only between the venue's ~5 s heartbeats, so
//! its teardown needs the 10 s deadline the default does not give.
//!
//! Three things the flags do not say on their face. `--interval-ms` is compared with `--interval`
//! only when BOTH are given in the same run (a lone `--interval 5m` leaves the mount's 60000 ms
//! default in place, unchecked). Re-running the verb under the same name REPLACES the stored body,
//! so a knob left off the second time returns to its default rather than keeping its old value.
//! And a `--shutdown-deadline-ms` that is not under the shipped unit's `TimeoutStopSec=` prints a
//! one-line stderr warning (`stop_timeout_warning`): that unit's 10 s equals the OANDA recipe's
//! deadline, and the stop timeout has to be raised with it.
//!
//! # Why `vike-cli` builds the row itself rather than reusing `vike_tradehub::config::DaemonProfile`
//!
//! `vike-cli` is layer 30; `vike-tradehub` is layer 65. `crates/vike-ops/tests/arch/layer_gate.rs` refuses
//! a normal dependency the wrong direction, so this crate cannot parse or validate a `DaemonProfile`
//! at all — the same constraint `super::mirror_profile`'s module doc states for the run
//! plane. The fields this verb accepts are therefore a hand-kept subset of `MountCfg`'s own keys,
//! matched 1:1 against `vike_secrets::profile_store::MountRow`'s columns (which already mirror the
//! TOML spelling verbatim); the daemon's own loader (`DaemonProfile::from_toml_str`, reached through
//! `crate::profile_rows::rows_to_daemon_profile` at boot) is what actually validates the body,
//! exactly as it validates a mirrored or file-loaded one.
//!
//! # Why store-then-activate rather than two verbs
//!
//! `crate::cmd::config::mirror_profile::write` and `crate::cmd::config::activate::run_activate` are
//! kept separate because a migration cannot see what is in force and an operator's `--proves` fence
//! is a deliberate second act. Neither reason applies to a genuinely fresh box: there is nothing to
//! migrate FROM and no file to prove against, so withholding activation here would just add a second
//! command with nothing left for it to check. `--dry-run` is the inspection step instead.

use std::path::Path;
use std::process::ExitCode;

use vike_secrets::profile_store::{
    MountRow, OperatorWrite, ProfileKind, ProfileRow, StoredProfile, read_profiles, set_active,
    store_profile,
};

use crate::cmd::args::{self, Flags};

/// `config bootstrap-daemon`'s usage.
pub const BOOTSTRAP_DAEMON_USAGE: &str = "usage: vike-cli config bootstrap-daemon <name> --venue <venue> --asset-class <class> \
     (--symbol <sym> | --token-id <id>)\n     [--interval <i>] [--interval-ms <ms>] [--qty <n>] \
     [--half-spread <n>] [--tick-size <n>] [--seed-cash <n>]\n     [--summary-ms <ms>] \
     [--shutdown-deadline-ms <ms>] [--account <label>] [--data-only] \
     [--dry-run]\n\n  Store a daemon profile with ONE mount, built entirely from these arguments —\n  \
     never from a file, never through the settings-file rungs (0086) — and make it the ACTIVE\n  \
     daemon profile. This is the bootstrap path for a box with no profile at all: unlike `config\n  \
     activate`, it takes no `--proves <file>` fence, because there is no file to prove against.\n  \
     Re-running it under the same name replaces the stored body in place, so a knob left off the\n  \
     second time returns to its default.\n\n  \
     --venue <venue>        REQUIRED\n  --asset-class <class>  REQUIRED\n  --symbol <sym>         \
     the mount symbol (mutually exclusive with --token-id)\n  --token-id <id>        the polymarket \
     spelling of --symbol\n  --interval-ms <ms>     the mount's bar window in ms (default 60000); \
     checked against --interval\n                         only when --interval is given too\n  \
     --summary-ms <ms>      the daemon's snapshot-summary cadence in ms (default 5000)\n  \
     --shutdown-deadline-ms <ms>\n                         the daemon's bounded-teardown deadline \
     in ms (default 5000). It must\n                         stay UNDER the unit's TimeoutStopSec= \
     (10 s as shipped) and a\n                         container's --stop-timeout; a value not \
     under 10 s warns on stderr\n  --dry-run              report the verdict and write nothing";

/// The shipped unit's `TimeoutStopSec=`, in ms — the stop budget a `--shutdown-deadline-ms` has to
/// stay UNDER.
///
/// There is no constant for it in the daemon: the number lives in `deploy/vike-tradehub.service`
/// (`TimeoutStopSec=10`) and, for a container, in the ops page's `--stop-timeout 10`. This one is
/// pinned to the unit's text by `crates/vike-cli/tests/bootstrap_daemon_cli.rs`, which reads the
/// unit itself — so a stop timeout raised in the unit makes that test fail until this follows it.
/// `crates/vike-tradehub/src/tradehub_cli/tests/stop_and_deadlines.rs`'s
/// `the_default_shutdown_deadline_fits_inside_the_units_stop_timeout` requires the deadline
/// STRICTLY under the stop timeout (the observe publisher's stop sits outside the capped teardown
/// and rides the difference) but reads only the DEFAULT deadline, so a deadline raised through this
/// verb was checked by nothing.
const SHIPPED_UNIT_STOP_TIMEOUT_MS: i64 = 10_000;

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
struct BootstrapArgs {
    name: String,
    venue: String,
    asset_class: String,
    symbol: Option<String>,
    token_id: Option<String>,
    interval: Option<String>,
    interval_ms: Option<i64>,
    summary_ms: Option<i64>,
    shutdown_deadline_ms: Option<i64>,
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

fn parse_bootstrap_daemon(args: impl Iterator<Item = String>) -> Result<BootstrapArgs, String> {
    let (
        mut name,
        mut venue,
        mut asset_class,
        mut symbol,
        mut token_id,
        mut interval,
        mut qty,
        mut half_spread,
        mut tick_size,
        mut seed_cash,
        mut account,
    ) = (None, None, None, None, None, None, None, None, None, None, None);
    let (mut interval_ms, mut summary_ms, mut shutdown_deadline_ms) = (None, None, None);
    let mut data_only = false;
    let mut dry_run = false;

    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "-h" | "--help" => return args::help_requested(),
            "--venue" => venue = Some(flags.value(&flag, inline)?),
            "--asset-class" => asset_class = Some(flags.value(&flag, inline)?),
            "--symbol" => symbol = Some(flags.value(&flag, inline)?),
            "--token-id" => token_id = Some(flags.value(&flag, inline)?),
            "--interval" => interval = Some(flags.value(&flag, inline)?),
            "--interval-ms" => interval_ms = Some(parse_ms(&flag, flags.value(&flag, inline)?)?),
            "--summary-ms" => summary_ms = Some(parse_ms(&flag, flags.value(&flag, inline)?)?),
            "--shutdown-deadline-ms" => {
                shutdown_deadline_ms = Some(parse_ms(&flag, flags.value(&flag, inline)?)?)
            }
            "--qty" => qty = Some(parse_number(&flag, flags.value(&flag, inline)?)?),
            "--half-spread" => {
                half_spread = Some(parse_number(&flag, flags.value(&flag, inline)?)?)
            }
            "--tick-size" => tick_size = Some(parse_number(&flag, flags.value(&flag, inline)?)?),
            "--seed-cash" => seed_cash = Some(parse_number(&flag, flags.value(&flag, inline)?)?),
            "--account" => account = Some(flags.value(&flag, inline)?),
            "--data-only" => {
                args::no_value(&flag, inline)?;
                data_only = true;
            }
            "--dry-run" => {
                args::no_value(&flag, inline)?;
                dry_run = true;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'"));
            }
            _ => {
                if let Some(v) = &inline {
                    return Err(format!(
                        "'{flag}={v}' looks like a flag with a value, but the profile NAME is a \
                         plain word"
                    ));
                }
                if name.is_some() {
                    return Err(format!("unexpected extra argument '{flag}'"));
                }
                name = Some(flag.clone());
            }
        }
    }

    match (&symbol, &token_id) {
        (Some(_), Some(_)) => {
            return Err("set --symbol OR --token-id, not both (they name the same field)".into());
        }
        (None, None) => {
            return Err(
                "a mount symbol is required: --symbol <sym> (or, on polymarket, --token-id <id>)"
                    .into(),
            );
        }
        _ => {}
    }

    // The recipes say "`interval_ms` must agree with `interval`", and the mount takes the two
    // independently — a disagreement is a bar window that is not the series it is labelled as. Only
    // an interval `vike_model::time::interval_ms` can measure is judged; `1w`/`1mo` have no width
    // there and are not refused.
    if let (Some(i), Some(ms)) = (&interval, interval_ms)
        && let Some(want) = vike_model::time::interval_ms(i)
        && want != ms
    {
        return Err(format!(
            "--interval-ms {ms} disagrees with --interval {i}, which is {want} ms — the mount \
             takes the two independently, so they must name one bar width"
        ));
    }

    Ok(BootstrapArgs {
        name: name.ok_or("missing the profile NAME")?,
        venue: venue.ok_or("missing --venue <venue>, REQUIRED")?,
        asset_class: asset_class.ok_or_else(|| {
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
        qty,
        half_spread,
        tick_size,
        seed_cash,
        account,
        data_only,
        dry_run,
    })
}

/// Entry point for `config bootstrap-daemon`.
pub fn run(args: impl Iterator<Item = String>, settings_dir: Option<&Path>) -> ExitCode {
    let args = match parse_bootstrap_daemon(args) {
        Ok(a) => a,
        Err(msg) => {
            return args::exit_for_parse_error(
                "config bootstrap-daemon",
                BOOTSTRAP_DAEMON_USAGE,
                &msg,
            );
        }
    };
    if let Some(warning) = stop_timeout_warning(args.shutdown_deadline_ms) {
        eprintln!("vike-cli config bootstrap-daemon: {warning}");
    }
    match bootstrap(&args, settings_dir, now_utc()) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli config bootstrap-daemon: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn already_active_daemon(db: &Path) -> Option<String> {
    read_profiles(db).ok().and_then(|p| p.active(ProfileKind::Daemon).map(|s| s.row.name.clone()))
}

fn crossing_line(already: &Option<String>, name: &str, dry_run: bool) -> String {
    match already {
        Some(held) if held == name => {
            let was = if dry_run { "is" } else { "was" };
            format!(" `{name}` {was} already the active daemon profile; its body is this one now.")
        }
        Some(held) => {
            let verb = if dry_run { "would REPOINT" } else { "REPOINTED" };
            format!(" This {verb} the box's daemon profile from `{held}` to `{name}`.")
        }
        None => " Nothing selects a daemon profile in this store today, so this is the CROSSING \
                  that lets `vike-tradehub` start with no `--config` at all."
            .to_string(),
    }
}

fn bootstrap(
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
    mount.symbol = args.symbol.clone();
    mount.token_id = args.token_id.clone();
    mount.interval = args.interval.clone();
    mount.interval_ms = args.interval_ms;
    mount.qty = args.qty;
    mount.half_spread = args.half_spread;
    mount.tick_size = args.tick_size;
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

    let stored = StoredProfile {
        row: ProfileRow {
            name: args.name.clone(),
            kind: ProfileKind::Daemon,
            active: false,
            note: None,
        },
        mounts: vec![mount],
        params: Default::default(),
        settings,
        recorder: None,
    };

    let already = already_active_daemon(&db);
    let symbol_word = args.token_id.as_deref().or(args.symbol.as_deref()).unwrap_or_default();

    if args.dry_run {
        return Ok(format!(
            "would store daemon profile `{}` (venue={} symbol={} asset_class={}) in {} and make it \
             ACTIVE — NOTHING WAS WRITTEN.{}{}",
            args.name,
            args.venue,
            symbol_word,
            args.asset_class,
            db.display(),
            knobs_line(args),
            crossing_line(&already, &args.name, true)
        ));
    }

    let write = OperatorWrite::claim("vike-cli config bootstrap-daemon");
    store_profile(&db, &stored, &write, now_utc, vike_model::AssetClass::SQL_WORDS)
        .map_err(|e| e.to_string())?;
    set_active(
        &db,
        ProfileKind::Daemon,
        &args.name,
        &write,
        now_utc,
        vike_model::AssetClass::SQL_WORDS,
    )
    .map_err(|e| e.to_string())?;

    Ok(format!(
        "stored and ACTIVATED daemon profile `{}` in {} (venue={} symbol={} asset_class={}).{}{}\n\n\
         Restart `vike-tradehub` (no `--config` needed) to run it. `vike-cli config deactivate \
         daemon` steps back to a refusal — there is no settings-file rung left to fall back to \
         (0086) — and re-running this verb under the same name replaces the body in place.",
        args.name,
        db.display(),
        args.venue,
        symbol_word,
        args.asset_class,
        knobs_line(args),
        crossing_line(&already, &args.name, false)
    ))
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

fn now_utc() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[path = "tests/bootstrap_daemon.rs"]
#[cfg(test)]
mod config_profile_bootstrap_tests;

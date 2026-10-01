//! `vike-cli config bootstrap-daemon` — **the one act that gets a fresh box from "no `--config`, no
//! active row, refuses to start" to a running paper mount**, with no profile TOML anywhere.
//!
//! # Why this exists
//!
//! decision 0086 ("settings live only in the database") verdict 1 forbids reading a profile
//! TOML from any binary, and `crate::cmd::config_mirror`/`config_mirror_profile`/
//! `config_mirror_recorder` — the only writers that ever put a daemon-profile BODY into the store —
//! read one to build it. Once those verbs are gone (0086 phase 1, alongside this record's owner
//! ruling), a box whose store holds no `active` daemon row has NO WAY to get one: `vike-tradehub`'s
//! `--config <profile.toml>` argument is retired (it decides nothing — see
//! `crate::cmd::config_activate`'s module doc for the ROW-wins ruling this predates), and
//! `config activate` cannot help either, because its whole fence is `--proves <file>` — comparing
//! the stored rows against a document that, on a genuinely fresh box, does not exist.
//!
//! This verb is the missing rung: it builds a `StoredProfile` DIRECTLY from argv — never through a
//! file, never through TOML text at all — and stores + activates it in one act. It is deliberately
//! narrow: a single mount, the fields a paper A-S mount actually needs
//! (`crates/vike-tradehub/src/config.rs`'s `DaemonProfile::validate` requires only `venue` and one of
//! `symbol`/`token_id`; everything else is optional exactly as the TOML spelling is — see
//! `vike_secrets::profile_store::MountRow`'s own doc). It is a BOOTSTRAP path for a box with nothing,
//! not a general profile editor: changing an existing profile's fields is still open work (0086's
//! OQ1/OQ2 in the phase-3 scout notes), and this verb does not attempt it.
//!
//! # Why `vike-cli` builds the row itself rather than reusing `vike_tradehub::config::DaemonProfile`
//!
//! `vike-cli` is layer 30; `vike-tradehub` is layer 65. `crates/vike-ops/tests/layer_gate.rs` refuses
//! a normal dependency the wrong direction, so this crate cannot parse or validate a `DaemonProfile`
//! at all — the same constraint `crate::cmd::config_mirror_profile`'s module doc states for the run
//! plane. The fields this verb accepts are therefore a hand-kept subset of `MountCfg`'s own keys,
//! matched 1:1 against `vike_secrets::profile_store::MountRow`'s columns (which already mirror the
//! TOML spelling verbatim); the daemon's own loader (`DaemonProfile::from_toml_str`, reached through
//! `crate::profile_rows::rows_to_daemon_profile` at boot) is what actually validates the body,
//! exactly as it validates a mirrored or file-loaded one.
//!
//! # Why store-then-activate rather than two verbs
//!
//! `crate::cmd::config_mirror_profile::write` and `crate::cmd::config_activate::run_activate` are
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
     (--symbol <sym> | --token-id <id>)\n     [--interval <i>] [--qty <n>] [--half-spread <n>] \
     [--tick-size <n>] [--seed-cash <n>]\n     [--account <label>] [--data-only] \
     [--dry-run]\n\n  Store a daemon profile with ONE mount, built entirely from these arguments —\n  \
     never from a file, never through the settings-file rungs (0086) — and make it the ACTIVE\n  \
     daemon profile. This is the bootstrap path for a box with no profile at all: unlike `config\n  \
     activate`, it takes no `--proves <file>` fence, because there is no file to prove against.\n\n  \
     --venue <venue>        REQUIRED\n  --asset-class <class>  REQUIRED\n  --symbol <sym>         \
     the mount symbol (mutually exclusive with --token-id)\n  --token-id <id>        the polymarket \
     spelling of --symbol\n  --dry-run              report the verdict and write nothing";

#[derive(Debug)]
struct BootstrapArgs {
    name: String,
    venue: String,
    asset_class: String,
    symbol: Option<String>,
    token_id: Option<String>,
    interval: Option<String>,
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
    mount.qty = args.qty;
    mount.half_spread = args.half_spread;
    mount.tick_size = args.tick_size;
    mount.seed_cash = args.seed_cash;
    mount.account = args.account.clone();
    // ⚠ `Some(true)` or ABSENT, never `Some(false)` — a stored `false` would freeze a default onto
    // the row that the TOML spelling never carries (`MountRow`'s own doc).
    mount.data_only = args.data_only.then_some(true);

    let stored = StoredProfile {
        row: ProfileRow {
            name: args.name.clone(),
            kind: ProfileKind::Daemon,
            active: false,
            note: None,
        },
        mounts: vec![mount],
        params: Default::default(),
        settings: Default::default(),
        recorder: None,
    };

    let already = already_active_daemon(&db);
    let symbol_word = args.token_id.as_deref().or(args.symbol.as_deref()).unwrap_or_default();

    if args.dry_run {
        return Ok(format!(
            "would store daemon profile `{}` (venue={} symbol={} asset_class={}) in {} and make it \
             ACTIVE — NOTHING WAS WRITTEN.{}",
            args.name,
            args.venue,
            symbol_word,
            args.asset_class,
            db.display(),
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
        "stored and ACTIVATED daemon profile `{}` in {} (venue={} symbol={} asset_class={}).{}\n\n\
         Restart `vike-tradehub` (no `--config` needed) to run it. `vike-cli config deactivate \
         daemon` steps back to a refusal — there is no settings-file rung left to fall back to \
         (0086) — and re-running this verb under the same name replaces the body in place.",
        args.name,
        db.display(),
        args.venue,
        symbol_word,
        args.asset_class,
        crossing_line(&already, &args.name, false)
    ))
}

fn now_utc() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[path = "config_profile_bootstrap_tests.rs"]
#[cfg(test)]
mod config_profile_bootstrap_tests;

//! `vike-cli config bootstrap-recorder` — **the recorder twin of `config bootstrap-daemon`**: the
//! one act that gets a box with NO recorder profile at all, and a `--record <path>` argument that is
//! retiring the same way `--config` did, to a running recording daemon — with no profile TOML
//! anywhere.
//!
//! # Why this exists
//!
//! decision 0086 ("settings live only in the database") verdict 1 forbids reading a profile TOML
//! from any binary. `crate::cmd::config::mirror_recorder` is the only writer that has ever put a
//! recorder profile BODY into the store, and it reads one to build it — so once that verb is gone
//! (0086 phase 1, alongside the daemon-profile bootstrap this mirrors), a box whose store holds no
//! recorder profile at all has no way to get one: the data daemon's `--record <path>` argument is
//! retiring (see `crates/vike-datahub/src/datahub_cli.rs`'s `ProfileSource` doc — it is accepted for
//! one release and its VALUE is never read, exactly as `vike-tradehub`'s `--config` is), and
//! `config activate recorder <name> --proves <file>` cannot help either, because its whole fence is
//! comparing stored rows against a document that, on a genuinely fresh box, does not exist.
//!
//! This verb is the missing rung: it builds a `StoredProfile` DIRECTLY from argv — never through a
//! file, never through TOML text at all — and stores + activates it in one act. It is deliberately
//! narrow, the same way `bootstrap-daemon` is narrow: ONE `[[subscribe]]` entry, the fields that
//! entry actually needs (`vike_recorder::Subscription`'s own shape — a venue, and EITHER a
//! family OR an explicit symbol list). It is a BOOTSTRAP path for a box with nothing, not a general
//! profile editor: a multi-subscription profile, or editing an existing one's fields, is still open
//! work, and this verb does not attempt it.
//!
//! # Why `vike-cli` builds the row itself rather than reusing `vike_recorder::config::RecorderProfile`
//!
//! `super::mirror_recorder`'s own module doc states the reason first: `vike_recorder` is
//! one layer up from `vike-cli` in the DEPENDENCY GRAPH this workspace keeps light — it drags
//! DataFusion, which must never join the CLI's build graph (`scripts/ci_feature_suite.sh`'s
//! `light-consumers` lane) — so this crate cannot parse or validate a `RecorderProfile` at all. The
//! fields this verb accepts are therefore a hand-kept subset of `Subscription`/`Maintenance`/
//! `Alerting`'s own keys, matched 1:1 against `vike_secrets::profile_store::RecorderRow`/
//! `SubscriptionRow`'s columns (which already mirror the TOML spelling verbatim, per
//! `render_recorder_toml`'s own doc); the data daemon's own loader
//! (`RecorderProfile::from_toml`, reached through `crate::recorder::load_and_check_active_profile_row`
//! at boot) is what actually validates the body, exactly as it validates a mirrored or
//! (formerly) file-loaded one. The two rules this verb DOES check locally —
//! family/symbols mutual exclusivity and a known `backfill` word — are cheap, string-only checks that
//! need no DataFusion-carrying type, so refusing them here saves an operator a round trip to a
//! daemon restart for a typo the daemon would refuse anyway.
//!
//! # Why store-then-activate rather than two verbs
//!
//! Exactly `bootstrap_daemon`'s own argument: there is nothing to migrate FROM and no file to
//! prove against on a genuinely fresh box, so withholding activation here would add a second command
//! with nothing left for it to check. `--dry-run` is the inspection step instead.

use std::path::Path;
use std::process::ExitCode;

use vike_secrets::profile_store::{
    OperatorWrite, ProfileKind, ProfileRow, RecorderBody, RecorderRow, StoredProfile,
    SubscriptionRow, read_profiles, set_active, store_profile, toml_string_array,
};

use crate::cmd::args::{self, Flags};

/// `config bootstrap-recorder`'s usage.
pub const BOOTSTRAP_RECORDER_USAGE: &str = "usage: vike-cli config bootstrap-recorder <name> --store <root> --venue <venue> \
     (--family <fam> | --symbols <a,b,c>)\n     [--backfill venue|archive|off] \
     [--interval-secs <n>] [--min-parts <n>] [--target-mb <n>]\n     [--max-merge-rows <n>] \
     [--retention-days <n>] [--webhooks <a,b>] [--alert-repeat-secs <n>]\n     \
     [--alert-series-prefix <s>] [--note <n>] [--dry-run]\n\n  Store a recorder profile with ONE \
     [[subscribe]] entry, built entirely from these arguments —\n  never from a file, never through \
     the settings-file rungs (0086) — and make it the ACTIVE\n  recorder profile. This is the \
     bootstrap path for a box with no recorder profile at all: unlike\n  `config activate`, it takes \
     no `--proves <file>` fence, because there is no file to prove against.\n\n  --store <root>      \
     REQUIRED — the profile's store root\n  --venue <venue>     REQUIRED\n  --family <fam>      a \
     whole market family (mutually exclusive with --symbols)\n  --symbols <a,b,c>   explicit symbols, \
     comma-separated (mutually exclusive with --family)\n  --dry-run           report the verdict and \
     write nothing";

#[derive(Debug)]
struct BootstrapArgs {
    name: String,
    store: String,
    venue: String,
    family: Option<String>,
    symbols: Vec<String>,
    backfill: Option<String>,
    interval_secs: Option<i64>,
    min_parts: Option<i64>,
    target_mb: Option<i64>,
    max_merge_rows: Option<i64>,
    retention_days: Option<i64>,
    webhooks: Vec<String>,
    alert_repeat_secs: Option<i64>,
    alert_series_prefix: Option<String>,
    note: Option<String>,
    dry_run: bool,
}

/// The three words `vike_recorder::config::Backfill` accepts (`#[serde(rename_all = "lowercase")]`
/// over its three unit variants) — kept as a local list rather than a dependency on that crate, per
/// this module's doc.
const BACKFILL_WORDS: [&str; 3] = ["venue", "archive", "off"];

fn parse_i64(flag: &str, v: String) -> Result<i64, String> {
    v.parse::<i64>().map_err(|_| format!("{flag} must be a whole number, got '{v}'"))
}

fn split_list(v: &str) -> Vec<String> {
    v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

fn parse_bootstrap_recorder(args: impl Iterator<Item = String>) -> Result<BootstrapArgs, String> {
    let (
        mut name,
        mut store,
        mut venue,
        mut family,
        mut backfill,
        mut interval_secs,
        mut min_parts,
        mut target_mb,
        mut max_merge_rows,
        mut retention_days,
        mut alert_repeat_secs,
        mut alert_series_prefix,
        mut note,
    ) = (None, None, None, None, None, None, None, None, None, None, None, None, None);
    let mut symbols: Vec<String> = Vec::new();
    let mut webhooks: Vec<String> = Vec::new();
    let mut dry_run = false;

    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "-h" | "--help" => return args::help_requested(),
            "--store" => store = Some(flags.value(&flag, inline)?),
            "--venue" => venue = Some(flags.value(&flag, inline)?),
            "--family" => family = Some(flags.value(&flag, inline)?),
            "--symbols" => symbols = split_list(&flags.value(&flag, inline)?),
            "--backfill" => backfill = Some(flags.value(&flag, inline)?),
            "--interval-secs" => {
                interval_secs = Some(parse_i64(&flag, flags.value(&flag, inline)?)?)
            }
            "--min-parts" => min_parts = Some(parse_i64(&flag, flags.value(&flag, inline)?)?),
            "--target-mb" => target_mb = Some(parse_i64(&flag, flags.value(&flag, inline)?)?),
            "--max-merge-rows" => {
                max_merge_rows = Some(parse_i64(&flag, flags.value(&flag, inline)?)?)
            }
            "--retention-days" => {
                retention_days = Some(parse_i64(&flag, flags.value(&flag, inline)?)?)
            }
            "--webhooks" => webhooks = split_list(&flags.value(&flag, inline)?),
            "--alert-repeat-secs" => {
                alert_repeat_secs = Some(parse_i64(&flag, flags.value(&flag, inline)?)?)
            }
            "--alert-series-prefix" => alert_series_prefix = Some(flags.value(&flag, inline)?),
            "--note" => note = Some(flags.value(&flag, inline)?),
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

    match (&family, symbols.is_empty()) {
        (Some(_), false) => {
            return Err(
                "set --family OR --symbols, not both (a subscription is one or the other)".into()
            );
        }
        (None, true) => {
            return Err(
                "a subscription needs --family <name> or --symbols <a,b,c> — neither was given"
                    .into(),
            );
        }
        _ => {}
    }
    if let Some(b) = &backfill
        && !BACKFILL_WORDS.contains(&b.as_str())
    {
        return Err(format!(
            "'{b}' is not a backfill word. The permitted words are: {}",
            BACKFILL_WORDS.join(", ")
        ));
    }

    Ok(BootstrapArgs {
        name: name.ok_or("missing the profile NAME")?,
        store: store.ok_or("missing --store <root>, REQUIRED")?,
        venue: venue.ok_or("missing --venue <venue>, REQUIRED")?,
        family,
        symbols,
        backfill,
        interval_secs,
        min_parts,
        target_mb,
        max_merge_rows,
        retention_days,
        webhooks,
        alert_repeat_secs,
        alert_series_prefix,
        note,
        dry_run,
    })
}

/// Entry point for `config bootstrap-recorder`.
pub fn run(args: impl Iterator<Item = String>, settings_dir: Option<&Path>) -> ExitCode {
    let args = match parse_bootstrap_recorder(args) {
        Ok(a) => a,
        Err(msg) => {
            return args::exit_for_parse_error(
                "config bootstrap-recorder",
                BOOTSTRAP_RECORDER_USAGE,
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
            eprintln!("vike-cli config bootstrap-recorder: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn already_active_recorder(db: &Path) -> Option<String> {
    read_profiles(db).ok().and_then(|p| p.active(ProfileKind::Recorder).map(|s| s.row.name.clone()))
}

fn crossing_line(already: &Option<String>, name: &str, dry_run: bool) -> String {
    match already {
        Some(held) if held == name => {
            let was = if dry_run { "is" } else { "was" };
            format!(
                " `{name}` {was} already the active recorder profile; its body is this one now."
            )
        }
        Some(held) => {
            let verb = if dry_run { "would REPOINT" } else { "REPOINTED" };
            format!(" This {verb} the box's recorder profile from `{held}` to `{name}`.")
        }
        None => " Nothing selects a recorder profile in this store today, so this is the CROSSING \
                  that lets the data daemon record with no `--record` file at all."
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
    let db = vike_secrets::db_path_in(dir);

    let subscription = SubscriptionRow {
        ord: 0,
        venue: args.venue.clone(),
        family: args.family.clone(),
        symbols: (!args.symbols.is_empty()).then(|| toml_string_array(&args.symbols)),
        backfill: args.backfill.clone(),
        note: None,
    };
    let recorder_row = RecorderRow {
        store: args.store.clone(),
        interval_secs: args.interval_secs,
        min_parts: args.min_parts,
        target_mb: args.target_mb,
        max_merge_rows: args.max_merge_rows,
        retention_days: args.retention_days,
        alert_webhooks: (!args.webhooks.is_empty()).then(|| toml_string_array(&args.webhooks)),
        alert_repeat_secs: args.alert_repeat_secs,
        alert_series_prefix: args.alert_series_prefix.clone(),
        note: args.note.clone(),
    };
    let body = RecorderBody { row: recorder_row, subscriptions: vec![subscription] };
    // The SAME renderer `crate::cmd::config::activate`'s `--proves` fence and the data daemon's own
    // `crate::recorder::load_and_check_active_profile_row` both go through
    // (`vike_secrets::profile_store::render_recorder_toml`'s own doc: "the ONE renderer, and it is
    // in this leaf crate deliberately: both consumers need it and they cannot see each other"). A
    // bootstrap has no file to prove against, so there is no round-trip fence to run here — but
    // showing the operator the document the daemon will actually load, rather than the argv they
    // typed, is the same transparency `config activate` gives on the read side.
    let rendered = vike_secrets::profile_store::render_recorder_toml(&body);
    let stored = StoredProfile {
        row: ProfileRow {
            name: args.name.clone(),
            kind: ProfileKind::Recorder,
            active: false,
            note: None,
        },
        mounts: Vec::new(),
        params: Default::default(),
        settings: Default::default(),
        recorder: Some(body),
    };

    let already = already_active_recorder(&db);
    let joined_symbols = args.symbols.join(",");
    let subscribe_word = args.family.as_deref().unwrap_or(&joined_symbols);

    if args.dry_run {
        return Ok(format!(
            "would store recorder profile `{}` (store={} venue={} subscribe={}) in {} and make it \
             ACTIVE — NOTHING WAS WRITTEN.{}\n\nThe document the data daemon would load:\n{}",
            args.name,
            args.store,
            args.venue,
            subscribe_word,
            db.display(),
            crossing_line(&already, &args.name, true),
            rendered
        ));
    }

    let write = OperatorWrite::claim("vike-cli config bootstrap-recorder");
    store_profile(&db, &stored, &write, now_utc, vike_model::AssetClass::SQL_WORDS)
        .map_err(|e| e.to_string())?;
    set_active(
        &db,
        ProfileKind::Recorder,
        &args.name,
        &write,
        now_utc,
        vike_model::AssetClass::SQL_WORDS,
    )
    .map_err(|e| e.to_string())?;

    Ok(format!(
        "stored and ACTIVATED recorder profile `{}` in {} (store={} venue={} subscribe={}).{}\n\n\
         Restart the data daemon with `--recorder-profile {}` (or the bare flag, once nothing else \
         is active) to run it. `vike-cli config deactivate recorder` steps back to a refusal — \
         there is no settings-file rung left to fall back to (0086) — and re-running this verb \
         under the same name replaces the body in place.\n\nThe document the data daemon will load:\n{}",
        args.name,
        db.display(),
        args.store,
        args.venue,
        subscribe_word,
        crossing_line(&already, &args.name, false),
        args.name,
        rendered
    ))
}

fn now_utc() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[path = "tests/bootstrap_recorder.rs"]
#[cfg(test)]
mod config_recorder_bootstrap_tests;

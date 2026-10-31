//! `vike-cli config bootstrap-recorder` — **the recorder twin of `config bootstrap-daemon`**: the
//! one act that gets a box with NO recorder profile at all, and a `--record <path>` argument that is
//! retiring the same way `--config` did, to a running recording daemon — with no profile TOML
//! anywhere.
//!
//! # Why this exists
//!
//! decision 0086 ("settings live only in the database") verdict 1 forbids reading a profile TOML
//! from any binary, so a recorder profile BODY reaches the store only through a writer that takes
//! no file (the daemon-profile bootstrap this mirrors). A box whose store holds no recorder profile
//! at all would otherwise have no way to get one: the data daemon's `--record <path>` argument is
//! retiring (see `crates/vike-datahub/src/datahub_cli/args.rs`'s `ProfileSource` doc — it is accepted for
//! one release and its VALUE is never read, exactly as `vike-tradehub`'s `--config` is), and
//! `config activate recorder <name>` cannot help either, because it only flips the `active` column of
//! a body that is already stored.
//!
//! This verb is the missing rung: it builds a `StoredProfile` DIRECTLY from argv — never through a
//! file, never through TOML text at all — and stores + activates it in one act. It is deliberately
//! narrow, the same way `bootstrap-daemon` is narrow: ONE `[[subscribe]]` entry, the fields that
//! entry actually needs (`vike_recorder::config::Subscription`'s own shape — a venue, and EITHER a
//! family OR an explicit symbol list). It is a BOOTSTRAP path for a box with nothing, not a general
//! profile editor: a multi-subscription profile, or editing an existing one's fields, is still open
//! work, and this verb does not attempt it.
//!
//! # Why `vike-cli` builds the row itself rather than reusing `vike_recorder::config::RecorderProfile`
//!
//! `vike_recorder` is
//! one layer up from `vike-cli` in the DEPENDENCY GRAPH this workspace keeps light — it drags
//! DataFusion, which must never join the CLI's build graph (`scripts/ci_feature_suite.sh`'s
//! `light-consumers` lane) — so this crate cannot parse or validate a `RecorderProfile` at all. The
//! fields this verb accepts are therefore a hand-kept subset of `Subscription`/`Maintenance`/
//! `Alerting`'s own keys, matched 1:1 against `vike_secrets::profile_store::RecorderRow`/
//! `SubscriptionRow`'s columns (which already mirror the TOML spelling verbatim, per
//! `render_recorder_toml`'s own doc); the data daemon's own loader
//! (`RecorderProfile::from_toml`, reached through `crate::recorder::load_and_check_active_profile_row`
//! at boot) is what actually validates the body, exactly as it validates any stored one. The one rule this verb DOES check locally —
//! family/symbols mutual exclusivity — is a cheap, string-only check that needs no
//! DataFusion-carrying type, so refusing it here saves an operator a round trip to a daemon restart
//! for a mistake the daemon would refuse anyway.
//!
//! # Why store-then-activate rather than two verbs
//!
//! Exactly `bootstrap_daemon`'s own argument: there is nothing to migrate FROM on a
//! genuinely fresh box, so withholding activation here would add a second command
//! with nothing left for it to check. `--dry-run` is the inspection step instead.

use std::path::Path;
use std::process::ExitCode;

use vike_secrets::profile_store::{
    OperatorWrite, ProfileKind, ProfileRow, RecorderBody, RecorderRow, StoredProfile,
    SubscriptionRow, read_profiles, set_active, store_profile, toml_string_array,
};

use crate::cmd::args;

/// `config bootstrap-recorder`'s arguments, as `clap` reads them. The required ones are kept
/// optional here so that their absence is refused in this verb's own words, after the parse, and
/// the numbers are kept raw so each refusal names its flag.
#[derive(clap::Args, Debug)]
#[command(
    about = "Build a ONE-subscription recorder profile FROM ARGUMENTS and activate it",
    long_about = "Store a recorder profile with ONE [[subscribe]] entry, built entirely from \
                  these arguments —\nnever from a file, never through the settings-file rungs \
                  (0086) — and make it the ACTIVE\nrecorder profile. This is the bootstrap path \
                  for a box with no recorder profile at all."
)]
pub(crate) struct BootstrapRecorderCmd {
    #[arg(value_name = "NAME", help = "the profile NAME the body is stored under")]
    positionals: Vec<String>,
    #[arg(long, value_name = "ROOT", help = "REQUIRED — the profile's store root")]
    store: Option<String>,
    #[arg(long, value_name = "VENUE", help = "REQUIRED")]
    venue: Option<String>,
    #[arg(
        long,
        value_name = "FAM",
        help = "a whole market family (mutually exclusive with --symbols)"
    )]
    family: Option<String>,
    #[arg(
        long,
        value_name = "A,B,C",
        help = "explicit symbols, comma-separated (mutually exclusive with --family)"
    )]
    symbols: Option<String>,
    #[arg(
        long,
        value_name = "N",
        help = "the recorder profile's `interval_secs` (a whole number)"
    )]
    interval_secs: Option<String>,
    #[arg(long, value_name = "N", help = "the recorder profile's `min_parts` (a whole number)")]
    min_parts: Option<String>,
    #[arg(long, value_name = "N", help = "the recorder profile's `target_mb` (a whole number)")]
    target_mb: Option<String>,
    #[arg(
        long,
        value_name = "N",
        help = "the recorder profile's `max_merge_rows` (a whole number)"
    )]
    max_merge_rows: Option<String>,
    #[arg(
        long,
        value_name = "N",
        help = "the recorder profile's `retention_days` (a whole number)"
    )]
    retention_days: Option<String>,
    #[arg(long, value_name = "A,B", help = "the recorder profile's `webhooks`, comma-separated")]
    webhooks: Option<String>,
    #[arg(
        long,
        value_name = "N",
        help = "the recorder profile's `alert_repeat_secs` (a whole number)"
    )]
    alert_repeat_secs: Option<String>,
    #[arg(long, value_name = "S", help = "the recorder profile's `alert_series_prefix`")]
    alert_series_prefix: Option<String>,
    #[arg(long, value_name = "N", help = "the recorder profile's free-text `note`")]
    note: Option<String>,
    #[arg(long, help = "report the verdict and write nothing")]
    dry_run: bool,
}

#[derive(Debug)]
struct BootstrapArgs {
    name: String,
    store: String,
    venue: String,
    family: Option<String>,
    symbols: Vec<String>,
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

fn parse_i64(flag: &str, v: String) -> Result<i64, String> {
    v.parse::<i64>().map_err(|_| format!("{flag} must be a whole number, got '{v}'"))
}

fn split_list(v: &str) -> Vec<String> {
    v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

impl BootstrapRecorderCmd {
    fn into_args(self) -> Result<BootstrapArgs, String> {
        let mut name = None;
        for word in self.positionals {
            if name.is_some() {
                return Err(format!("unexpected extra argument '{word}'"));
            }
            name = Some(word);
        }
        let int = |flag: &str, v: Option<String>| v.map(|v| parse_i64(flag, v)).transpose();
        let interval_secs = int("--interval-secs", self.interval_secs)?;
        let min_parts = int("--min-parts", self.min_parts)?;
        let target_mb = int("--target-mb", self.target_mb)?;
        let max_merge_rows = int("--max-merge-rows", self.max_merge_rows)?;
        let retention_days = int("--retention-days", self.retention_days)?;
        let alert_repeat_secs = int("--alert-repeat-secs", self.alert_repeat_secs)?;
        let symbols = self.symbols.as_deref().map(split_list).unwrap_or_default();
        let webhooks = self.webhooks.as_deref().map(split_list).unwrap_or_default();
        let family = self.family;

        match (&family, symbols.is_empty()) {
            (Some(_), false) => {
                return Err(
                    "set --family OR --symbols, not both (a subscription is one or the other)"
                        .into(),
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

        Ok(BootstrapArgs {
            name: name.ok_or("missing the profile NAME")?,
            store: self.store.ok_or("missing --store <root>, REQUIRED")?,
            venue: self.venue.ok_or("missing --venue <venue>, REQUIRED")?,
            family,
            symbols,
            interval_secs,
            min_parts,
            target_mb,
            max_merge_rows,
            retention_days,
            webhooks,
            alert_repeat_secs,
            alert_series_prefix: self.alert_series_prefix,
            note: self.note,
            dry_run: self.dry_run,
        })
    }
}

/// Read `config bootstrap-recorder`'s argv tail through the plane's own parser, so a test meets the
/// grammar an operator does.
#[cfg(test)]
fn parse_bootstrap_recorder(
    argv: impl Iterator<Item = String>,
) -> Result<BootstrapArgs, args::Stop> {
    let argv = std::iter::once("bootstrap-recorder".to_string()).chain(argv);
    match crate::cmd::config::parse(argv)? {
        crate::cmd::config::Sub::BootstrapRecorder(cmd) => cmd
            .into_args()
            .map_err(|msg| args::Stop::message("vike-cli config bootstrap-recorder", msg)),
        other => unreachable!("`bootstrap-recorder` parsed as {other:?}"),
    }
}

/// Entry point for `config bootstrap-recorder`.
pub(crate) fn run(cmd: BootstrapRecorderCmd, settings_dir: Option<&Path>) -> ExitCode {
    let args = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse("vike-cli config bootstrap-recorder", msg),
    };
    match bootstrap(&args, settings_dir, vike_model::time::clock::now_secs()) {
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
    // The SAME renderer the data daemon's own `crate::recorder::load_and_check_active_profile_row`
    // goes through (`vike_secrets::profile_store::render_recorder_toml`'s own doc: "the ONE
    // renderer, and it is in this leaf crate deliberately: both consumers need it and they cannot
    // see each other"). Showing the operator the document the daemon will actually load, rather
    // than the argv they typed, is the transparency a dry run exists for.
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

#[path = "tests/bootstrap_recorder.rs"]
#[cfg(test)]
mod config_recorder_bootstrap_tests;

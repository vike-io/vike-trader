//! `vike-cli config profile mount set-param --mount <PROFILE>:<ORD> --key NAME --value V [--dry-run]` — **ONE stored
//! daemon mount's ONE `[strategy.params]` row, set to V**, every other mount, param and setting of the body kept
//! as it is.
//!
//! # Why a verb of its own
//!
//! A knob the mount's strategy file declares had no writer: `config profile create daemon` REPLACES the whole body and
//! spells only `--qty`/`--half-spread`, `trade` deliberately spells no `UpdateParams`, and the store is never
//! edited by hand. The case that asked for it is the CI box's spread maker, whose `max_half_spread_ticks` the owner
//! raised on 2026-11-19 (the crypto preset's 60 ticks is below the fee floor on bybit's 0.1 tick).
//!
//! # The rules
//!
//! - **The value is stored as `config profile create daemon` stores one**: a number as a float
//!   (`vike_model::params::ParamValue::Float`, so `150` is stored `150.0`), anything else as text, each its
//!   `ParamValue` rendering, the TOML the daemon's loader reads back out of `[strategy.params]`
//!   (`vike_secrets::profile_store::render_daemon_toml`). An empty value is refused.
//! - **The key is a bare TOML key** (`[A-Za-z0-9_-]+`): the renderer writes it unquoted, so any other spelling
//!   is a document the daemon refuses whole at its start.
//! - **Under a `.rhai` strategy file the value must be a number**, the daemon's own load rule
//!   (`ProfileDaemon::validate_script`). No key is refused by name: the retired `qty`, `half_spread` and
//!   `resolution_ts_ms` the loader refuses are MOUNT columns, and `qty`/`half_spread` are the maker's params.
//!   Whether the file READS the key is answered at the daemon's start, from the file's header: this verb reads
//!   no strategy file, and `vike-cli config check` judges the active profile through the daemon's own loader.
//! - **A row already holding V answers `unchanged`** (exit 0); otherwise the report is `old -> new`, `<unset>`
//!   for a key the mount did not carry.
//! - **The write** stores the profile's body again with that one row changed
//!   (`vike_secrets::profile_store::update_profile`, which keeps the `active` bit and sets the row on the body read
//!   under the write's own lock, so a concurrent edit of the body is kept) and is recorded as `config
//!   rename-symbol` records a mount write (`crate::cmd::settings_write`). A running daemon keeps the mount it
//!   started on: the change binds at its next start.
//! - **`--dry-run`** says what would change and writes nothing.
//! - Not on the MCP surface, as no `config` writer is.

use std::path::Path;
use std::process::ExitCode;

use vike_change_journal::{Change, ChangeActor, ChangeOutcome};
use vike_model::params::ParamValue;
use vike_secrets::profile_store::{
    ProfileKind, ProfileOperatorWrite, read_profiles, update_profile,
};
use vike_strategy::file::scan::{StrategyFileLanguage, language_of};

use crate::cmd::args;
use crate::cmd::config::rename_symbol::parse_mount;
use crate::cmd::settings_write;
use crate::exit::{CliError, CommandResult};

/// The program name this verb is read and reported under.
const BIN: &str = "vike-cli config profile mount set-param";

/// `config profile mount set-param`'s arguments, as `clap` reads them.
#[derive(clap::Args, Debug)]
#[command(
    about = "Set ONE stored daemon mount's ONE strategy param, keeping the rest of the body",
    long_about = "Set ONE `[strategy.params]` row of ONE stored daemon profile's mount (--mount \
                  PROFILE:ORD),\nkeeping every other mount, param and setting. A number is stored as \
                  a float, anything else as\ntext; a row already holding the value is unchanged (exit \
                  0). A running daemon keeps its mount:\nthe change binds at its next start."
)]
pub(crate) struct SetMountParamCommand {
    #[arg(
        long,
        value_name = "PROFILE:ORD",
        help = "the stored daemon profile's mount of that ordinal (`tradehub:0`)"
    )]
    mount: String,
    #[arg(long, value_name = "NAME", help = "the `[strategy.params]` key, a bare TOML key")]
    key: String,
    #[arg(
        long,
        value_name = "VALUE",
        help = "a number is stored as a float, anything else as text"
    )]
    value: String,
    #[arg(long, help = "say what would change and write nothing")]
    dry_run: bool,
}

#[derive(Debug)]
struct Args {
    profile: String,
    ord: i64,
    key: String,
    value: String,
    dry_run: bool,
}

impl SetMountParamCommand {
    fn into_args(self) -> Result<Args, String> {
        let (profile, ord) = parse_mount(&self.mount)?;
        // The renderer writes the key unquoted (`render_daemon_toml`), so only a bare key reaches the loader whole.
        let bare = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
        if self.key.is_empty() || !self.key.bytes().all(bare) {
            return Err(format!(
                "--key `{}` is not a params key: one or more of A-Z a-z 0-9 _ -. Nothing was \
                 written.",
                self.key
            ));
        }
        if self.value.is_empty() {
            return Err("--value is empty: name the value to store. Nothing was written.".into());
        }
        Ok(Args { profile, ord, key: self.key, value: self.value, dry_run: self.dry_run })
    }
}

/// Read `config profile mount set-param`'s argv tail through the plane's own parser.
#[cfg(test)]
fn parse(argv: impl Iterator<Item = String>) -> Result<Args, args::Stop> {
    use super::super::{MountVerb, ProfileVerb, Sub};
    parse_config_verb!(
        argv,
        "profile mount set-param",
        cmd in Sub::Profile { verb: ProfileVerb::Mount { verb: MountVerb::SetParam(cmd) } },
        BIN
    )
}

/// Entry point: the write, then its report.
pub(crate) fn run(cmd: SetMountParamCommand, ctx: settings_write::Context<'_>) -> ExitCode {
    let parsed = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse(BIN, msg),
    };
    match execute(&parsed, &ctx) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{BIN}: {}", e.message);
            e.exit.into()
        }
    }
}

/// The value as `config profile create daemon` stores one: a number a float, anything else text.
fn stored_value(value: &str) -> ParamValue {
    value.parse::<f64>().map_or_else(|_| ParamValue::Str(value.to_string()), ParamValue::Float)
}

fn execute(args: &Args, ctx: &settings_write::Context<'_>) -> CommandResult<String> {
    let dir = ctx.settings_dir.ok_or_else(|| {
        CliError::failed(
            "no settings directory resolved — there is no store to set a mount param in. Set \
             VIKE_SETTINGS_DIR, or run from a project that has one.",
        )
    })?;
    let (profile, ord) = (args.profile.as_str(), args.ord);
    let db = vike_secrets::db_path_in(dir);
    let profiles = read_profiles(&db).map_err(|e| CliError::failed(e.to_string()))?;
    let body = match profiles.by_name(profile) {
        Some(stored) if stored.row.kind == ProfileKind::Daemon => stored.clone(),
        Some(stored) => {
            return Err(CliError::usage(format!(
                "`{profile}` is stored as a `{}` profile, and only a daemon profile has mounts. \
                 Nothing was written.",
                stored.row.kind.sql_word()
            )));
        }
        None => {
            return Err(CliError::usage(format!(
                "no stored profile called `{profile}` in {}. Nothing was written.",
                db.display()
            )));
        }
    };
    let Some(mount) = body.mounts.iter().find(|mount| mount.ord == ord) else {
        return Err(CliError::usage(format!(
            "daemon profile `{profile}` has no mount {ord}. Nothing was written."
        )));
    };
    let value = stored_value(&args.value);
    let file = mount.strategy_file.as_deref().unwrap_or_default();
    if matches!(value, ParamValue::Str(_))
        && matches!(language_of(Path::new(file)), Some(StrategyFileLanguage::Rhai))
    {
        return Err(CliError::usage(format!(
            "mount {profile}:{ord} runs the Rhai file `{file}`, whose `[strategy.params]` values the \
             daemon refuses unless each is a NUMBER, and `{}` is not one. Nothing was written.",
            args.value
        )));
    }
    let what = format!("mount {profile}:{ord} ({}) param `{}`", mount.venue, args.key);
    let new = value.to_string();
    let row = (ord, args.key.clone());
    let old = body.params.get(&row).cloned();
    if old.as_deref() == Some(new.as_str()) {
        return Ok(format!("unchanged — {what} already holds {new}. Nothing was written."));
    }
    let shown_old = old.as_deref().unwrap_or("<unset>");
    if args.dry_run {
        return Ok(format!(
            "would set {what}: {shown_old} -> {new} — NOTHING WAS WRITTEN (--dry-run)."
        ));
    }
    let key = format!("profile.daemon.{profile}.mount.{ord}.param.{}", args.key);
    let record = |outcome: ChangeOutcome, reason: Option<&str>| {
        let change = Change::set_setting(
            outcome,
            ChangeActor::cli("vike-cli"),
            "profile",
            &key,
            old.as_deref(),
            &new,
        )
        .with_reason(reason);
        settings_write::record_change(ctx, &key, "written", outcome, &change);
    };
    // The ONE row is set on the body read under the write's own lock (`update_profile`), so an edit
    // of another part of this body landing since the read above is kept, not lost (decision 0297).
    match update_profile(
        &db,
        profile,
        &ProfileOperatorWrite::claim(BIN),
        ctx.now_ms / 1000,
        vike_model::AssetClass::SQL_WORDS,
        |held| {
            let mut held = held
                .filter(|p| {
                    p.row.kind == ProfileKind::Daemon && p.mounts.iter().any(|m| m.ord == ord)
                })
                .ok_or_else(|| {
                    format!(
                        "daemon profile `{profile}` no longer holds mount {ord}: it changed while \
                         this ran. Nothing was written."
                    )
                })?;
            held.params.insert(row, new.clone());
            Ok(Some(held))
        },
    ) {
        Ok(_) => {
            record(ChangeOutcome::AppliedPendingRestart, None);
            Ok(format!(
                "set {what}: {shown_old} -> {new}; a running daemon keeps the mount it started on, so \
                 it binds at the daemon's next start."
            ))
        }
        Err(e) => {
            record(ChangeOutcome::Refused, Some(&e.to_string()));
            Err(CliError::failed(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use vike_model::AssetClass;
    use vike_secrets::profile_store::store_profile;

    use super::*;
    use crate::cmd::config::rename_symbol::tests::{
        ctx, journal_text, store, store_prod2_tradehub, tradehub,
    };
    use crate::exit::Exit;

    fn parsed(argv: &[&str]) -> Result<Args, args::Stop> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    const MAX_TICKS: [&str; 6] =
        ["--mount", "tradehub:0", "--key", "max_half_spread_ticks", "--value", "150"];

    #[test]
    fn the_parser_takes_a_mount_a_bare_key_and_a_value() {
        let a = parsed(&MAX_TICKS).unwrap();
        assert_eq!(
            (a.profile.as_str(), a.ord, a.key.as_str()),
            ("tradehub", 0, "max_half_spread_ticks")
        );
        assert!(!a.dry_run);
        for bad in [
            &["--mount", "tradehub", "--key", "k", "--value", "1"][..],
            &["--mount", "tradehub:-1", "--key", "k", "--value", "1"],
            &["--mount", "tradehub:0", "--key", "", "--value", "1"],
            &["--mount", "tradehub:0", "--key", "a b", "--value", "1"],
            &["--mount", "tradehub:0", "--key", "a.b", "--value", "1"],
            &["--mount", "tradehub:0", "--key", "k", "--value", ""],
            &["--mount", "tradehub:0", "--key", "k"],
        ] {
            assert!(parsed(bad).is_err(), "{bad:?}");
        }
        assert!(parsed(&["--help"]).unwrap_err().is_help());
    }

    /// **A write changes exactly the one row**, keeps the selection, journals it, loads through the daemon's own
    /// loader as a float, and the same write again is `unchanged`.
    #[test]
    fn a_write_changes_exactly_one_row_and_a_rerun_is_unchanged() {
        let d = store();
        store_prod2_tradehub(d.path());
        let state = d.path().join("state");
        let before = tradehub(d.path());
        let args = parsed(&MAX_TICKS).unwrap();
        let report = execute(&args, &ctx(d.path(), &state)).unwrap();
        assert!(
            report.starts_with(
                "set mount tradehub:0 (bybit) param `max_half_spread_ticks`: <unset> -> 150.0;"
            ),
            "{report}"
        );
        let mut want = before.clone();
        want.params.insert((0, "max_half_spread_ticks".into()), "150.0".into());
        let after = tradehub(d.path());
        assert_eq!(after, want, "one row added, the rest of the body byte-identical");
        assert!(after.row.active, "the write keeps the selection");
        let journal = journal_text(&state);
        assert!(journal.contains("profile.daemon.tradehub.mount.0.param.max_half_spread_ticks"));
        let daemon = vike_tradehub::profile_rows::rows_to_daemon_profile(&after)
            .expect("the daemon's loader takes the body");
        let params = &daemon.strategy.as_ref().expect("a strategy table").params;
        assert_eq!(params.get("max_half_spread_ticks"), Some(&ParamValue::Float(150.0)));

        let again = execute(&args, &ctx(d.path(), &state)).unwrap();
        assert!(again.starts_with("unchanged"), "{again}");
        let changed =
            parsed(&["--mount", "tradehub:0", "--key", "qty", "--value", "0.002"]).unwrap();
        let report = execute(&changed, &ctx(d.path(), &state)).unwrap();
        assert!(report.contains("`qty`: 0.001 -> 0.002;"), "{report}");
    }

    /// `--dry-run` says what would change and writes nothing.
    #[test]
    fn a_dry_run_writes_nothing() {
        let d = store();
        store_prod2_tradehub(d.path());
        let state = d.path().join("state");
        let before = tradehub(d.path());
        let mut argv = MAX_TICKS.to_vec();
        argv.push("--dry-run");
        let report = execute(&parsed(&argv).unwrap(), &ctx(d.path(), &state)).unwrap();
        assert!(report.contains("<unset> -> 150.0 — NOTHING WAS WRITTEN"), "{report}");
        assert_eq!(tradehub(d.path()), before);
        assert!(!state.exists(), "a rehearsal writes no ledger row");
    }

    /// A mount the store does not hold, a profile it does not hold, and a text value on a Rhai mount are refused,
    /// and nothing is written.
    #[test]
    fn a_missing_mount_or_profile_and_text_on_a_rhai_mount_are_refused() {
        let d = store();
        store_prod2_tradehub(d.path());
        let state = d.path().join("state");
        let before = tradehub(d.path());
        for (mount, says) in [("tradehub:1", "has no mount 1"), ("nonsuch:0", "no stored profile")]
        {
            let args = parsed(&["--mount", mount, "--key", "k", "--value", "1"]).unwrap();
            let e = execute(&args, &ctx(d.path(), &state)).unwrap_err();
            assert_eq!(e.exit, Exit::Usage);
            assert!(e.message.contains(says), "{mount}: {}", e.message);
        }
        assert_eq!(tradehub(d.path()), before);

        let mut rhai = before.clone();
        rhai.mounts[0].strategy_file = Some("scripts/maker.rhai".into());
        let db = vike_secrets::db_path_in(d.path());
        let w = ProfileOperatorWrite::claim("test");
        store_profile(&db, &rhai, &w, 1, AssetClass::SQL_WORDS).unwrap();
        let text = parsed(&["--mount", "tradehub:0", "--key", "mode", "--value", "fast"]).unwrap();
        let e = execute(&text, &ctx(d.path(), &state)).unwrap_err();
        assert!(e.message.contains("NUMBER"), "{}", e.message);
        assert_eq!(tradehub(d.path()).params, before.params);
    }
}

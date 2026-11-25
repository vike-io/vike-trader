//! `vike-cli config profile mount set-max-leverage --mount <PROFILE>:<ORD> --max-leverage X|none [--dry-run]` —
//! **ONE stored daemon mount's own LEVERAGE CAP** (its `mount.max_leverage` column) set to X or cleared, every
//! other column, param and setting of the body kept as it is.
//!
//! # Why a verb of its own
//!
//! The cap is a mount COLUMN, not a strategy param (`config profile mount set-param` writes those), and not a
//! `policy` row (`config set policy.max_mount_leverage` writes the node-wide one). The record is
//! `docs/decisions/0249-a-mounts-loss-budget-has-a-source-a-policy-row-read-at-boot-and-judged-on-the-risk-clock.md`'s
//! amendment of 2026-11-23: a mount's gross notional after an opening order, over its account's sizing
//! equity, may not exceed the cap, and the mount's own figure may only NARROW the policy row.
//!
//! # The rules
//!
//! - **X is a finite number of at least 1** (1 = no leverage), refused in words otherwise; `none` clears the
//!   column, and the mount is then held to the policy row alone (or to nothing, with no row).
//! - **A figure above `policy.max_mount_leverage` is REFUSED in words, never clamped**: the column may only
//!   narrow the row. With no row written any figure of at least 1 is taken. A figure above the engine's own
//!   leverage is NOT refused: it is inert there, because the engine's own gate binds first.
//! - **A row already holding the figure answers `unchanged`** (exit 0); otherwise the report is `old -> new`.
//! - **The write** stores the profile's body again with that one column changed
//!   (`vike_secrets::profile_store::update_profile`, the body read under the write's own lock), recorded as
//!   `config profile mount set-param` records one (`crate::cmd::settings_write`). A running daemon keeps the mount it
//!   started on: the change binds at its next start. `--dry-run` says what would change and writes nothing.
//! - Not on the MCP surface, as no `config` writer is.

use std::process::ExitCode;

use vike_change_journal::{Change, ChangeActor, ChangeOutcome};
use vike_config::redact::UNSET;
use vike_secrets::profile_store::{
    ProfileKind, ProfileOperatorWrite, read_profiles, update_profile,
};

use crate::cmd::args;
use crate::cmd::config::rename_symbol::parse_mount;
use crate::cmd::settings_write;
use crate::exit::{CliError, CommandResult};

/// The program name this verb is read and reported under.
const BIN: &str = "vike-cli config profile mount set-max-leverage";

/// `config profile mount set-max-leverage`'s arguments, as `clap` reads them.
#[derive(clap::Args, Debug)]
#[command(
    about = "Set or clear ONE stored daemon mount's own leverage cap, never above the policy row",
    long_about = "Set or clear ONE stored daemon profile's mount's own leverage cap (--mount \
                  PROFILE:ORD), keeping\nevery other column, param and setting: its gross notional after \
                  an opening order, over its\naccount's sizing equity, may not exceed it. A figure above \
                  policy.max_mount_leverage is refused,\nnever clamped; `none` clears it. A running daemon \
                  keeps its mount: the change binds at its next start."
)]
pub(crate) struct SetMountMaxLeverageCommand {
    #[arg(
        long,
        value_name = "PROFILE:ORD",
        help = "the stored daemon profile's mount of that ordinal (`tradehub:0`)"
    )]
    mount: String,
    #[arg(
        long,
        value_name = "X|none",
        help = "the cap, a number of at least 1, or `none` to clear the mount's own cap"
    )]
    max_leverage: String,
    #[arg(long, help = "say what would change and write nothing")]
    dry_run: bool,
}

#[derive(Debug)]
struct Args {
    profile: String,
    ord: i64,
    /// `None` clears the column.
    cap: Option<f64>,
    dry_run: bool,
}

impl SetMountMaxLeverageCommand {
    fn into_args(self) -> Result<Args, String> {
        let (profile, ord) = parse_mount(&self.mount)?;
        let cap = match self.max_leverage.as_str() {
            "none" => None,
            text => match text.parse::<f64>() {
                Ok(cap) if cap.is_finite() && cap >= 1.0 => Some(cap),
                _ => {
                    return Err(format!(
                        "--max-leverage `{text}` is not a leverage cap: a finite number of at least 1 \
                         (1 = no leverage), or `none` to clear it. Nothing was written."
                    ));
                }
            },
        };
        Ok(Args { profile, ord, cap, dry_run: self.dry_run })
    }
}

/// Read `config profile mount set-max-leverage`'s argv tail through the plane's own parser.
#[cfg(test)]
fn parse(argv: impl Iterator<Item = String>) -> Result<Args, args::Stop> {
    use super::super::{MountVerb, ProfileVerb, Sub};
    parse_config_verb!(
        argv,
        "profile mount set-max-leverage",
        cmd in Sub::Profile { verb: ProfileVerb::Mount { verb: MountVerb::SetMaxLeverage(cmd) } },
        BIN
    )
}

/// Entry point: the write, then its report. `ceiling` is the `policy.max_mount_leverage` row the
/// dispatcher's boot resolved.
pub(crate) fn run(
    cmd: SetMountMaxLeverageCommand,
    ctx: settings_write::Context<'_>,
    ceiling: Option<f64>,
) -> ExitCode {
    let parsed = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse(BIN, msg),
    };
    match execute(&parsed, &ctx, ceiling) {
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

/// A cap as the report and the ledger spell it: its number, or `<unset>`.
fn shown(cap: Option<f64>) -> String {
    cap.map_or_else(|| UNSET.to_string(), |cap| cap.to_string())
}

fn execute(
    args: &Args,
    ctx: &settings_write::Context<'_>,
    ceiling: Option<f64>,
) -> CommandResult<String> {
    if let (Some(cap), Some(ceiling)) = (args.cap, ceiling)
        && cap > ceiling
    {
        return Err(CliError::usage(format!(
            "--max-leverage {cap} is above policy.max_mount_leverage {ceiling}: a mount's own cap may \
             only narrow the node's, so it is refused, not lowered. Name {ceiling} or less, or raise \
             the row first (`vike-cli config set policy.max_mount_leverage <x>`). Nothing was written."
        )));
    }
    let dir = ctx.settings_dir.ok_or_else(|| {
        CliError::failed(
            "no settings directory resolved — there is no store to set a mount's cap in. Set \
             VIKE_SETTINGS_DIR, or run from a project that has one.",
        )
    })?;
    let (profile, ord) = (args.profile.as_str(), args.ord);
    let db = vike_secrets::db_path_in(dir);
    let profiles = read_profiles(&db).map_err(|e| CliError::failed(e.to_string()))?;
    let body = match profiles.by_name(profile) {
        Some(stored) if stored.row.kind == ProfileKind::Daemon => stored,
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
    let what = format!("mount {profile}:{ord} ({}) max_leverage", mount.venue);
    let (old, new) = (shown(mount.max_leverage), shown(args.cap));
    if mount.max_leverage == args.cap {
        return Ok(format!("unchanged — {what} already holds {new}. Nothing was written."));
    }
    if args.dry_run {
        return Ok(format!("would set {what}: {old} -> {new} — NOTHING WAS WRITTEN (--dry-run)."));
    }
    let key = format!("profile.daemon.{profile}.mount.{ord}.max_leverage");
    let record = |outcome: ChangeOutcome, reason: Option<&str>| {
        let change = Change::set_setting(
            outcome,
            ChangeActor::cli("vike-cli"),
            "profile",
            &key,
            mount.max_leverage.is_some().then_some(old.as_str()),
            &new,
        )
        .with_reason(reason);
        settings_write::record_change(ctx, &key, "written", outcome, &change);
    };
    // The ONE column is set on the body read under the write's own lock (`update_profile`), so an edit
    // of another part of this body landing since the read above is kept, not lost (decision 0297).
    match update_profile(
        &db,
        profile,
        &ProfileOperatorWrite::claim(BIN),
        ctx.now_ms / 1000,
        vike_model::AssetClass::SQL_WORDS,
        |held| {
            let mut held = held.filter(|p| p.row.kind == ProfileKind::Daemon).ok_or_else(|| {
                format!("daemon profile `{profile}` changed while this ran. Nothing was written.")
            })?;
            let mount = held.mounts.iter_mut().find(|m| m.ord == ord).ok_or_else(|| {
                format!(
                    "daemon profile `{profile}` no longer holds mount {ord}: it changed while this \
                     ran. Nothing was written."
                )
            })?;
            mount.max_leverage = args.cap;
            Ok(Some(held))
        },
    ) {
        Ok(_) => {
            record(ChangeOutcome::AppliedPendingRestart, None);
            Ok(format!(
                "set {what}: {old} -> {new}; a running daemon keeps the mount it started on, so it \
                 binds at the daemon's next start."
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
    use super::*;
    use crate::cmd::config::rename_symbol::tests::{
        ctx, journal_text, store, store_prod2_tradehub, tradehub,
    };
    use crate::exit::Exit;

    fn parsed(argv: &[&str]) -> Result<Args, args::Stop> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn the_parser_takes_a_mount_and_a_cap_or_none() {
        let a = parsed(&["--mount", "tradehub:0", "--max-leverage", "3"]).unwrap();
        assert_eq!((a.profile.as_str(), a.ord, a.cap), ("tradehub", 0, Some(3.0)));
        assert!(!a.dry_run);
        let none = parsed(&["--mount", "tradehub:0", "--max-leverage", "none"]).unwrap();
        assert_eq!(none.cap, None, "`none` clears the column");
        for bad in ["0.5", "0", "-1", "inf", "NaN", "three", ""] {
            assert!(parsed(&["--mount", "tradehub:0", "--max-leverage", bad]).is_err(), "{bad}");
        }
        assert!(parsed(&["--mount", "tradehub", "--max-leverage", "3"]).is_err());
        assert!(parsed(&["--mount", "tradehub:0"]).is_err(), "the cap is required");
        assert!(parsed(&["--help"]).unwrap_err().is_help());
    }

    /// **A write changes exactly the one column**, keeps the selection, journals it, loads through the
    /// daemon's own loader, and the same write again is `unchanged`; `none` clears it.
    #[test]
    fn a_write_sets_and_clears_exactly_the_one_column() {
        let d = store();
        store_prod2_tradehub(d.path());
        let state = d.path().join("state");
        let before = tradehub(d.path());
        let three = parsed(&["--mount", "tradehub:0", "--max-leverage", "3"]).unwrap();
        let report = execute(&three, &ctx(d.path(), &state), Some(5.0)).unwrap();
        assert!(
            report.starts_with("set mount tradehub:0 (bybit) max_leverage: <unset> -> 3;"),
            "{report}"
        );
        let mut want = before.clone();
        want.mounts[0].max_leverage = Some(3.0);
        let after = tradehub(d.path());
        assert_eq!(after, want, "one column set, the rest of the body byte-identical");
        assert!(after.row.active, "the write keeps the selection");
        assert!(journal_text(&state).contains("profile.daemon.tradehub.mount.0.max_leverage"));
        let daemon = vike_tradehub::profile_rows::rows_to_daemon_profile(&after)
            .expect("the daemon's loader takes the body");
        assert_eq!(daemon.max_leverage, Some(3.0));

        let again = execute(&three, &ctx(d.path(), &state), Some(5.0)).unwrap();
        assert!(again.starts_with("unchanged"), "{again}");

        let clear = parsed(&["--mount", "tradehub:0", "--max-leverage", "none"]).unwrap();
        let report = execute(&clear, &ctx(d.path(), &state), Some(5.0)).unwrap();
        assert!(report.contains("max_leverage: 3 -> <unset>;"), "{report}");
        assert_eq!(tradehub(d.path()), before, "cleared, the body is what it was");
    }

    /// **A figure above `policy.max_mount_leverage` is refused, never clamped**, and nothing is written;
    /// the figure AT the row is taken, and with no row any figure of at least 1 is.
    #[test]
    fn a_figure_above_the_policy_row_is_refused_in_words() {
        let d = store();
        store_prod2_tradehub(d.path());
        let state = d.path().join("state");
        let before = tradehub(d.path());
        let eight = parsed(&["--mount", "tradehub:0", "--max-leverage", "8"]).unwrap();
        let e = execute(&eight, &ctx(d.path(), &state), Some(5.0)).unwrap_err();
        assert_eq!(e.exit, Exit::Usage);
        assert!(e.message.contains("above policy.max_mount_leverage 5"), "{}", e.message);
        assert!(e.message.contains("refused, not lowered"), "{}", e.message);
        assert_eq!(tradehub(d.path()), before, "nothing was written");
        assert!(!state.exists(), "a refusal before the store writes no ledger row");

        let five = parsed(&["--mount", "tradehub:0", "--max-leverage", "5"]).unwrap();
        execute(&five, &ctx(d.path(), &state), Some(5.0)).expect("the row itself is a cap");
        execute(&eight, &ctx(d.path(), &state), None).expect("no row, no ceiling");
        assert_eq!(tradehub(d.path()).mounts[0].max_leverage, Some(8.0));
    }

    /// `--dry-run` writes nothing; a mount or a profile the store does not hold is refused.
    #[test]
    fn a_dry_run_writes_nothing_and_a_missing_mount_is_refused() {
        let d = store();
        store_prod2_tradehub(d.path());
        let state = d.path().join("state");
        let before = tradehub(d.path());
        let dry = parsed(&["--mount", "tradehub:0", "--max-leverage", "2", "--dry-run"]).unwrap();
        let report = execute(&dry, &ctx(d.path(), &state), None).unwrap();
        assert!(report.contains("<unset> -> 2 — NOTHING WAS WRITTEN"), "{report}");
        for (mount, says) in [("tradehub:1", "has no mount 1"), ("nonsuch:0", "no stored profile")]
        {
            let args = parsed(&["--mount", mount, "--max-leverage", "2"]).unwrap();
            let e = execute(&args, &ctx(d.path(), &state), None).unwrap_err();
            assert_eq!(e.exit, Exit::Usage);
            assert!(e.message.contains(says), "{mount}: {}", e.message);
        }
        assert_eq!(tradehub(d.path()), before);
        assert!(!state.exists(), "a rehearsal and a refusal write no ledger row");
    }
}

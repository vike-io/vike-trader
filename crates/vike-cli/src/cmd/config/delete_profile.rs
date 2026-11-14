//! `vike-cli config delete-profile <run|daemon|recorder> <name> --yes` — **delete ONE stored
//! profile body**: its `profile` row and, by the schema's cascades, every mount, parameter, setting,
//! recorder row and subscription under it.
//!
//! # What commissioned it
//!
//! `profile.name` is ONE namespace across the three kinds, and until this verb nothing removed a
//! body once stored. A daemon profile called `default` takes the name the datahub's recorder profile
//! is stored under (the container proof of decision 0260 met it), `config bootstrap-recorder
//! default` is then refused (`vike_secrets::profile_store::ProfileStoreError::NameHeldByAnotherKind`),
//! and a box in that state could not be repaired from the CLI.
//!
//! # The shape
//!
//! `<KIND> <NAME>`, the two positionals `config activate` takes (`activate.rs`'s `kind_and_name`),
//! so one profile is named one way on every verb. The KIND is required although the name alone is
//! unique: a delete that names the kind the body is stored as cannot take another kind's body by a
//! slip of the name.
//!
//! # The fences
//!
//! - **The ACTIVE profile of its kind is never deleted**, and neither is a name the store does not
//!   hold AS THAT KIND. The rule is `vike_secrets::profile_store::delete_verdict`, asked inside the
//!   delete's own transaction and by `--dry-run` of the same rows, so a rehearsal cannot promise a
//!   delete the store would refuse.
//! - **`--yes` is required; `--dry-run` reports the verdict and deletes nothing.** The `config`
//!   plane's other writers take no confirmation (decision 0086 point 7: a row write is undone by
//!   another write), but a deleted body is undone by no `config` verb, so this one takes the
//!   `--yes` token `data hist rm` takes, never a retyped name.
//! - **One row of the `profile` table** through the store's own writer
//!   (`vike_secrets::profile_store::delete_profile`): no credential, account, hub-key or setting row
//!   is touched, and nothing here writes SQL.
//! - **Journalled as `config unset` journals a removal** (`crate::cmd::settings_write`): a
//!   `set_setting` record under the section `profile` and the key `profile.<kind>.<name>`, `<set>`
//!   -> `<unset>` with a reason saying it was a delete, the body never recorded; a refusal by the
//!   store is recorded with its reason. The journal is the append-only file beside the store, so the
//!   record follows the delete's transaction rather than sharing it, as every settings write's does.
//! - Not on the MCP surface: neither is `config activate` nor `config deactivate`.

use std::process::ExitCode;

use vike_change_journal::{Change, ChangeActor, ChangeOutcome};
use vike_secrets::profile_store::{
    ProfileKind, ProfileOperatorWrite, ProfileStoreError, delete_profile, delete_verdict,
    read_profiles,
};

use super::activate::kind_and_name;
use crate::cmd::args;
use crate::cmd::settings_write;
use crate::exit::{CliError, CommandResult};

/// The program name this verb is read and reported under.
const BIN: &str = "vike-cli config delete-profile";

/// `config delete-profile`'s arguments, as `clap` reads them: a KIND, a NAME and the confirmation.
#[derive(clap::Args, Debug)]
#[command(
    about = "Delete ONE stored profile body (run|daemon|recorder); never the active one",
    long_about = "Delete ONE stored profile body: its row and every mount, parameter, setting and \
                  subscription\nunder it. The ACTIVE profile of its kind is refused (`config \
                  activate <kind> <another-name>`\nor `config deactivate <kind>` first), and so is \
                  a name not stored as that kind. No `config`\nverb brings a deleted body back: \
                  it takes --yes, and --dry-run shows the verdict. The delete\nis recorded in the \
                  change journal."
)]
pub(crate) struct DeleteProfileCommand {
    #[arg(
        value_name = "KIND / NAME",
        help = "the profile KIND (run | daemon | recorder), then the stored profile's NAME"
    )]
    positionals: Vec<String>,
    #[arg(long, help = "delete it: a deleted body comes back only by storing it again")]
    yes: bool,
    #[arg(long, help = "report the verdict and delete nothing")]
    dry_run: bool,
}

#[derive(Debug)]
struct Args {
    kind: ProfileKind,
    name: String,
    dry_run: bool,
}

impl DeleteProfileCommand {
    fn into_args(self) -> Result<Args, String> {
        let (kind, name) = kind_and_name(self.positionals)?;
        if !self.yes && !self.dry_run {
            return Err(
                "a deleted profile body is brought back by no `config` verb, so the delete \
                 takes --yes; --dry-run shows the verdict. Nothing was deleted."
                    .to_string(),
            );
        }
        Ok(Args { kind, name, dry_run: self.dry_run })
    }
}

/// Read `config delete-profile`'s argv tail through the plane's own parser.
#[cfg(test)]
fn parse(argv: impl Iterator<Item = String>) -> Result<Args, args::Stop> {
    parse_config_verb!(argv, "delete-profile", DeleteProfile, BIN)
}

/// Entry point: gate, delete, journal, report.
pub(crate) fn run(cmd: DeleteProfileCommand, ctx: settings_write::Context<'_>) -> ExitCode {
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

/// Why the journal record carries a reason: its `old`/`new` cells are the redaction words.
const DELETE_REASON: &str =
    "the profile was deleted (`vike-cli config delete-profile`); its body is not recorded";

fn execute(args: &Args, ctx: &settings_write::Context<'_>) -> CommandResult<String> {
    let dir = ctx.settings_dir.ok_or_else(|| {
        CliError::failed(
            "no settings directory resolved — there is no store to delete a profile from. Set \
             VIKE_SETTINGS_DIR, or run from a project that has one.",
        )
    })?;
    let db = vike_secrets::db_path_in(dir);
    let word = args.kind.sql_word();
    let name = &args.name;

    if args.dry_run {
        let profiles = read_profiles(&db).map_err(|e| CliError::failed(e.to_string()))?;
        let held = profiles.by_name(name).map(|p| (p.row.kind.sql_word(), p.row.active));
        delete_verdict(args.kind, name, held).map_err(|e| rung(&e))?;
        return Ok(format!(
            "would delete {word} profile `{name}` from {} — NOTHING WAS DELETED. It is not the \
             active {word} profile; re-run with --yes to delete it.",
            db.display()
        ));
    }

    let key = format!("profile.{word}.{name}");
    let record = |outcome: ChangeOutcome, change: Change| {
        settings_write::record_change(ctx, &key, "deleted", outcome, &change);
    };
    match delete_profile(&db, args.kind, name, &ProfileOperatorWrite::claim(BIN)) {
        Ok(()) => {
            record(
                ChangeOutcome::AppliedPendingRestart,
                Change::set_setting(
                    ChangeOutcome::AppliedPendingRestart,
                    ChangeActor::cli("vike-cli"),
                    "profile",
                    &key,
                    Some(vike_config::redact::SET),
                    vike_config::redact::UNSET,
                )
                .with_reason(Some(DELETE_REASON)),
            );
            // Only a recorder is read by NAME with no active row (`--recorder-profile <name>`).
            let by_name = if args.kind == ProfileKind::Recorder {
                format!(
                    "\n⚠ A datahub started with `--recorder-profile {name}` and no active recorder \
                     row reads this name: its next start no longer finds a body."
                )
            } else {
                String::new()
            };
            Ok(format!(
                "deleted {word} profile `{name}` from {}.\n  Its body (every mount, parameter, \
                 setting and subscription under it) is gone; no `config` verb brings it back, \
                 `vike-cli config bootstrap-{word}` stores a body again.\n  It was not the active \
                 {word} profile, so no selection changed.{by_name}",
                db.display()
            ))
        }
        Err(e) => {
            record(
                ChangeOutcome::Refused,
                Change::set_setting(
                    ChangeOutcome::Refused,
                    ChangeActor::cli("vike-cli"),
                    "profile",
                    &key,
                    None,
                    vike_config::redact::UNSET,
                )
                .with_reason(Some(&e.to_string())),
            );
            Err(rung(&e))
        }
    }
}

/// The exit rung of a store refusal: a name the store does not hold as that kind, or the active
/// profile, is the command line's to change (`Usage`); anything else is the box's (`Failed`).
fn rung(e: &ProfileStoreError) -> CliError {
    match e {
        ProfileStoreError::NoSuchProfile { .. } | ProfileStoreError::DeleteActive { .. } => {
            CliError::usage(e.to_string())
        }
        _ => CliError::failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::exit::Exit;

    fn parsed(argv: &[&str]) -> Result<Args, args::Stop> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    /// A store holding two `run` bodies, `a` (ACTIVE) and `b`.
    fn store() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let db = vike_secrets::db_path_in(dir.path());
        vike_secrets::create_empty_store_for_test(&db).unwrap();
        let w = ProfileOperatorWrite::claim("test");
        for name in ["a", "b"] {
            let mut stored = vike_config::run_profile::samples::stored(&[]);
            stored.row.name = name.to_string();
            vike_secrets::profile_store::store_profile(
                &db,
                &stored,
                &w,
                1,
                vike_model::AssetClass::SQL_WORDS,
            )
            .unwrap();
        }
        vike_secrets::profile_store::set_active(
            &db,
            ProfileKind::Run,
            "a",
            &w,
            2,
            vike_model::AssetClass::SQL_WORDS,
        )
        .unwrap();
        dir
    }

    fn ctx<'a>(dir: &'a Path, state: &'a Path) -> settings_write::Context<'a> {
        settings_write::Context {
            settings_dir: Some(dir),
            state_dir: Some(state),
            now_ms: 1,
            surface: BIN,
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let profiles = read_profiles(&vike_secrets::db_path_in(dir)).unwrap();
        profiles.all().iter().map(|p| p.row.name.clone()).collect()
    }

    /// Every file under the journal's state directory, concatenated.
    fn journal_text(state: &Path) -> String {
        let mut out = String::new();
        let mut stack = vec![state.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(t) = std::fs::read_to_string(&p) {
                    out.push_str(&t);
                }
            }
        }
        out
    }

    #[test]
    fn the_parser_takes_a_kind_a_name_and_requires_yes_or_dry_run() {
        let a = parsed(&["daemon", "default", "--yes"]).unwrap();
        assert_eq!((a.kind, a.name.as_str(), a.dry_run), (ProfileKind::Daemon, "default", false));
        assert!(parsed(&["--dry-run", "recorder", "x"]).unwrap().dry_run);
        let e = parsed(&["daemon", "default"]).unwrap_err();
        assert!(e.contains("--yes") && e.contains("Nothing was deleted"), "{e}");
        assert!(parsed(&["daemon", "--yes"]).is_err(), "the name is required");
        assert!(parsed(&["nonsuch", "x", "--yes"]).is_err(), "the kind is checked");
        assert!(parsed(&["run", "a", "b", "--yes"]).is_err(), "one name only");
        assert!(parsed(&["--help"]).unwrap_err().is_help());
    }

    /// **The delete**: one inactive body goes, the active one stays, and the journal records it the
    /// way `config unset` records a removal.
    #[test]
    fn an_inactive_body_is_deleted_and_journalled() {
        let d = store();
        let state = d.path().join("state");
        let args = Args { kind: ProfileKind::Run, name: "b".into(), dry_run: false };
        let report = execute(&args, &ctx(d.path(), &state)).unwrap();
        assert!(report.contains("deleted run profile `b`"), "{report}");
        assert_eq!(names(d.path()), ["a"]);
        let text = journal_text(&state);
        assert!(text.contains("set_setting") && text.contains("profile.run.b"), "{text}");
        assert!(text.contains("<set>") && text.contains("<unset>"), "{text}");
        assert!(text.contains("pending_restart") && text.contains("deleted"), "{text}");
    }

    /// **The active refusal** — nothing deleted, the refusal journalled, the usage rung, and the
    /// way out named.
    #[test]
    fn the_active_profile_is_refused_and_nothing_is_deleted() {
        let d = store();
        let state = d.path().join("state");
        let args = Args { kind: ProfileKind::Run, name: "a".into(), dry_run: false };
        let e = execute(&args, &ctx(d.path(), &state)).unwrap_err();
        assert_eq!(e.exit, Exit::Usage);
        assert!(
            e.message.contains("ACTIVE") && e.message.contains("config deactivate run"),
            "{}",
            e.message
        );
        assert_eq!(names(d.path()), ["a", "b"]);
        let text = journal_text(&state);
        assert!(text.contains("refused") && text.contains("profile.run.a"), "{text}");
    }

    /// **The unknown refusal** — a name not stored, and a name stored as another kind.
    #[test]
    fn an_unknown_name_and_another_kind_are_refused() {
        let d = store();
        let state = d.path().join("state");
        let none = Args { kind: ProfileKind::Run, name: "nonsuch".into(), dry_run: false };
        let e = execute(&none, &ctx(d.path(), &state)).unwrap_err();
        assert_eq!(e.exit, Exit::Usage);
        assert!(e.message.contains("no `run` profile called `nonsuch`"), "{}", e.message);
        let other = Args { kind: ProfileKind::Daemon, name: "b".into(), dry_run: false };
        let e = execute(&other, &ctx(d.path(), &state)).unwrap_err();
        assert!(e.message.contains("as a `run` profile, not a `daemon` one"), "{}", e.message);
        assert_eq!(names(d.path()), ["a", "b"]);
    }

    /// `--dry-run` asks the same rule and deletes nothing, journalling nothing.
    #[test]
    fn a_dry_run_gives_the_verdict_and_deletes_nothing() {
        let d = store();
        let state = d.path().join("state");
        let ok = Args { kind: ProfileKind::Run, name: "b".into(), dry_run: true };
        let report = execute(&ok, &ctx(d.path(), &state)).unwrap();
        assert!(report.contains("NOTHING WAS DELETED"), "{report}");
        let active = Args { kind: ProfileKind::Run, name: "a".into(), dry_run: true };
        assert_eq!(execute(&active, &ctx(d.path(), &state)).unwrap_err().exit, Exit::Usage);
        assert_eq!(names(d.path()), ["a", "b"]);
        assert!(!state.exists(), "a rehearsal writes no ledger row");
    }
}

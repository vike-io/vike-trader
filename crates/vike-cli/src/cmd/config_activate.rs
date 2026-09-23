//! `vike-cli config activate` / `config deactivate` — **WHICH stored profile this box runs, as the
//! one deliberate act that changes it.**
//!
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Question 3 was
//! answered by the owner on 2026-09-15: **the ROW wins.**
//! `vike_secrets::profile_store::select` is that ruling written once, and it has been consulted at
//! every daemon boot since — with `None` hardcoded where the row goes for the run plane, and with a
//! real value for the daemon plane. What has never existed is a verb that writes the row.
//!
//! This is that verb, and it is deliberately NOT part of `config mirror`.
//!
//! # Why storing and selecting are different verbs
//!
//! `vike_secrets::profile_store::plan_active_row` is *"the one place a MIGRATION may decide to arm
//! something"*, and it says no unless the row it would write reproduces what is already in force.
//! That is the right rule for a migration, which is a tidy-up nobody is watching. It is the WRONG
//! rule for an operator who has read a dry run and typed a command: `plan_active_row` cannot see
//! what is in force (`VIKE_RUN_PROFILE` reaches the daemon through its unit's `EnvironmentFile=`,
//! `--config` lives on its `ExecStart=`, and neither is visible from an operator shell), so it can
//! only ever answer `Withhold { NothingSelectedToday }` here — which would make the crossing
//! impossible rather than safe.
//!
//! So the crossing is a SEPARATE command with its own fence, and the fence is a PROOF rather than a
//! guess:
//!
//! > `--proves <file>` is REQUIRED, and the stored rows must render back to a document that parses
//! > EQUAL to it.
//!
//! That is `config adopt`'s precondition restated in this plane's terms — **the crossing changes the
//! ARTIFACT, never the VALUE** — and it is what stops `activate` being a way to point a live daemon
//! at a body nobody compared to anything. There is no `--force`.
//!
//! # ⚠ What the fence does NOT prove, stated rather than implied
//!
//! It proves the rows ARE that file. It does not prove that file is what the box runs: a
//! `settings/run-live.toml` can sit on disk with nothing naming it. So on a box whose
//! `flags.tradehub_live` is ON and where nothing selects a run profile today, the daemon currently
//! REFUSES to start (`vike_mount::MountError::MissingRiskBudget`) and activating a `run` row makes
//! it start LIVE at the next restart. The report says so in those words, every time, for the `run`
//! kind — because that is a state an operator may genuinely be curing, and a verb that refused it
//! would leave them with no cure at all.
//!
//! # …and what a RUNNING daemon does about any of this
//!
//! Nothing, until it restarts. Both roots read the profile rows ONCE, at boot; there is no watcher
//! and no reload. The report ends on that sentence, the same one `crate::cmd::config_adopt` ends
//! both of its verbs with.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_secrets::profile_store::{
    ActiveProfile, OperatorWrite, ProfileKind, StoredProfile, clear_active, read_profiles,
    render_daemon_toml, render_recorder_toml, render_run_toml, set_active,
};

use crate::cmd::args::{self, Flags};

/// `config activate`'s usage.
pub const ACTIVATE_USAGE: &str = "usage: vike-cli config activate <run|daemon|recorder> <name> --proves <file> \
     [--dry-run]\n\n  Make a STORED profile the one this box reads, from its next restart.\n  The \
     row beats the daemon's own `--config` argument and `VIKE_RUN_PROFILE` — that is the owner's \
     ruling\n  (decision 0057 Question 3), and the daemon discloses what it shadowed at every \
     boot.\n\n  --proves <file>  REQUIRED, and it is the fence: the stored rows must render back \
     to a\n                   document that parses EQUAL to this file. The crossing changes the \
     ARTIFACT,\n                   never the VALUE. There is no --force.\n  --dry-run        \
     report the verdict and write nothing";

/// `config deactivate`'s usage.
pub const DEACTIVATE_USAGE: &str = "usage: vike-cli config deactivate <run|daemon|recorder> [--dry-run]\n\n  Step back to the \
     file rungs: clear this kind's active row and let `--config` /\n  `VIKE_RUN_PROFILE` decide \
     again, exactly as they do on every box that has not crossed.\n  The stored BODIES are \
     untouched, so re-crossing is one command and needs no re-mirror.\n\n  --dry-run  report and \
     write nothing";

/// The kind word, parsed from the operator's own argv.
fn parse_kind(word: &str) -> Result<ProfileKind, String> {
    match word {
        "run" => Ok(ProfileKind::Run),
        "daemon" => Ok(ProfileKind::Daemon),
        "recorder" => Ok(ProfileKind::Recorder),
        other => Err(format!(
            "unknown profile kind '{other}' — expected `run` (the risk budget and sinks, \
             `run-live.toml`), `daemon` (the mount set, `tradehub.toml`) or `recorder` (the \
             subscription set, `recorder.toml`)"
        )),
    }
}

#[derive(Debug)]
struct ActivateArgs {
    kind: ProfileKind,
    name: String,
    proves: PathBuf,
    dry_run: bool,
}

fn parse_activate(args: impl Iterator<Item = String>) -> Result<ActivateArgs, String> {
    let (mut kind, mut name, mut proves, mut dry_run) = (None, None, None, false);
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "-h" | "--help" => return args::help_requested(),
            "--dry-run" => {
                args::no_value(&flag, inline)?;
                dry_run = true;
            }
            "--proves" => proves = Some(PathBuf::from(flags.value(&flag, inline)?)),
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'"));
            }
            _ => {
                // The two positionals, in order. `next_flag` splits on `=`, so a positional that
                // contained one would arrive halved — no profile kind or name may, and a name that
                // did could not be typed into this verb at all, which is a refusal rather than a
                // silent truncation.
                if let Some(v) = &inline {
                    return Err(format!(
                        "'{flag}={v}' looks like a flag with a value, but this position takes a \
                         plain word"
                    ));
                }
                let word = flag.clone();
                if kind.is_none() {
                    kind = Some(parse_kind(&word)?);
                } else if name.is_none() {
                    name = Some(word);
                } else {
                    return Err(format!("unexpected extra argument '{word}'"));
                }
            }
        }
    }
    Ok(ActivateArgs {
        kind: kind.ok_or("missing the profile KIND (run | daemon | recorder)")?,
        name: name.ok_or("missing the profile NAME — `vike-cli config recorder` and the mirror's own report print the names this store holds")?,
        proves: proves.ok_or(
            "missing `--proves <file>`, which is REQUIRED and is this verb's whole fence: the \
             stored rows must render back to a document that parses EQUAL to that file, so the \
             crossing changes the ARTIFACT and never the VALUE. There is no --force.",
        )?,
        dry_run,
    })
}

#[derive(Debug)]
struct DeactivateArgs {
    kind: ProfileKind,
    dry_run: bool,
}

fn parse_deactivate(args: impl Iterator<Item = String>) -> Result<DeactivateArgs, String> {
    let (mut kind, mut dry_run) = (None, false);
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "-h" | "--help" => return args::help_requested(),
            "--dry-run" => {
                args::no_value(&flag, inline)?;
                dry_run = true;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'"));
            }
            _ => {
                if kind.is_some() {
                    return Err(format!("unexpected extra argument '{flag}'"));
                }
                kind = Some(parse_kind(&flag)?);
            }
        }
    }
    Ok(DeactivateArgs {
        kind: kind.ok_or("missing the profile KIND (run | daemon | recorder)")?,
        dry_run,
    })
}

/// Entry point for `config activate`.
pub fn run_activate(args: impl Iterator<Item = String>, settings_dir: Option<&Path>) -> ExitCode {
    let args = match parse_activate(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("config activate", ACTIVATE_USAGE, &msg),
    };
    match activate(&args, settings_dir, now_utc()) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli config activate: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Entry point for `config deactivate`.
pub fn run_deactivate(args: impl Iterator<Item = String>, settings_dir: Option<&Path>) -> ExitCode {
    let args = match parse_deactivate(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("config deactivate", DEACTIVATE_USAGE, &msg),
    };
    match deactivate(&args, settings_dir, now_utc()) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli config deactivate: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// **Render a stored body back into the document its own loader parses**, for the `--proves`
/// comparison. One renderer per kind, all three from `vike_secrets::profile_store` — the same three
/// the DAEMONS load through, so "what the operator proved" and "what the daemon loads" are one
/// document by construction rather than by agreement.
///
/// # Errors
///
/// Whatever the kind's renderer refuses.
fn render(kind: ProfileKind, stored: &StoredProfile) -> Result<String, String> {
    match kind {
        ProfileKind::Run => Ok(render_run_toml(stored)),
        ProfileKind::Daemon => render_daemon_toml(stored),
        ProfileKind::Recorder => {
            let body = stored.recorder.as_ref().ok_or_else(|| {
                format!(
                    "profile `{}` is stored as a `recorder` profile and carries no recorder body — \
                     there is nothing to prove against. Re-mirror it with `vike-cli config mirror \
                     --recorder <file>`.",
                    stored.row.name
                )
            })?;
            Ok(render_recorder_toml(body))
        }
    }
}

/// The sentence every `run` activation must carry — see this module's doc for why it is a WARNING
/// and not a refusal.
const RUN_ARMING_WARNING: &str = "\n\n⚠ A `run` profile supplies the PRE-TRADE RISK CEILINGS a LIVE mount refuses to start \
     without. If `flags.tradehub_live` is ON on this box and nothing selects a run profile today, \
     that daemon is exiting FAILURE right now (`MountError::MissingRiskBudget`) and this row will \
     make it START LIVE at the next restart — which may be exactly what you want, and is not \
     something this command can decide for you. `vike-cli config show --filter tradehub_live` says \
     which state this box is in.";

fn activate(
    args: &ActivateArgs,
    settings_dir: Option<&Path>,
    now_utc: i64,
) -> Result<String, String> {
    let dir = settings_dir.ok_or(
        "no settings directory resolved — there is no store to select a profile in. Set \
         VIKE_SETTINGS_DIR, or run from a project that has one.",
    )?;
    let db = vike_secrets::db_path_in(dir);
    let profiles = read_profiles(&db).map_err(|e| e.to_string())?;
    let word = args.kind.sql_word();

    let stored = profiles.by_name(&args.name).ok_or_else(|| {
        let held: Vec<&str> = profiles
            .all()
            .iter()
            .filter(|p| p.row.kind == args.kind)
            .map(|p| p.row.name.as_str())
            .collect();
        format!(
            "the settings store at {} holds no profile called `{}`. Nothing was written. {} \
             profiles it does hold: {}. `vike-cli config mirror` is what puts a body there.",
            db.display(),
            args.name,
            word,
            if held.is_empty() { "(none)".to_string() } else { held.join(", ") }
        )
    })?;
    if stored.row.kind != args.kind {
        return Err(format!(
            "profile `{}` is stored as a `{}` profile, not a `{word}` one. Nothing was written: \
             the kinds are selected independently and activating one as the other would point a \
             loader at a body it cannot read.",
            args.name,
            stored.row.kind.sql_word()
        ));
    }

    // ── THE FENCE ───────────────────────────────────────────────────────────────────────────
    let text = std::fs::read_to_string(&args.proves)
        .map_err(|e| format!("reading {}: {e}", args.proves.display()))?;
    let want: toml::Value = toml::from_str(&text)
        .map_err(|e| format!("{} does not parse as TOML: {e}", args.proves.display()))?;
    let rendered = render(args.kind, stored)?;
    let got: toml::Value = toml::from_str(&rendered).map_err(|e| {
        format!(
            "the stored rows rendered a document that does not parse ({e}). Nothing was written; \
             this is a defect in the migration rather than in your profile.\n--- as the rows \
             render it ---\n{rendered}"
        )
    })?;
    if got != want {
        return Err(format!(
            "REFUSING to activate `{}`: the stored rows do NOT reproduce {}, so this crossing \
             would change what this box does rather than only where it reads it from.\n\nNothing \
             was written. Re-mirror the file first (`vike-cli config mirror`), or prove against \
             the file the rows actually came from.\n\n--- {} ---\n{}\n--- the rows as they render \
             ---\n{rendered}",
            args.name,
            args.proves.display(),
            args.proves.display(),
            text.trim_end()
        ));
    }

    let already = match profiles.resolve_active(args.kind) {
        ActiveProfile::Row(p) => Some(p.row.name.clone()),
        ActiveProfile::NoProfileStore
        | ActiveProfile::NoneStored
        | ActiveProfile::NoneActive { .. } => None,
    };
    let change = match &already {
        Some(held) if held == &args.name => {
            format!("`{}` is ALREADY the active {word} profile — this changes nothing.", args.name)
        }
        Some(held) => {
            format!("this REPOINTS the box's {word} profile from `{held}` to `{}`.", args.name)
        }
        None => format!(
            "nothing selects a {word} profile in this store today, so this is the CROSSING: from \
             the next restart, the row `{}` beats {}.",
            args.name,
            match args.kind {
                ProfileKind::Run => "`--profile` and `VIKE_RUN_PROFILE`",
                ProfileKind::Daemon => "the daemon's own `--config <path>` argument",
                ProfileKind::Recorder => "`--record-profile <name>` on the recording daemon",
            }
        ),
    };
    let arming = if args.kind == ProfileKind::Run { RUN_ARMING_WARNING } else { "" };

    if args.dry_run {
        return Ok(format!(
            "would activate {word} profile `{}` in {} — NOTHING WAS WRITTEN.\n  The rows \
             reproduce {} exactly, so the crossing changes the ARTIFACT and not the VALUE.\n  {}\
             {arming}",
            args.name,
            db.display(),
            args.proves.display(),
            change
        ));
    }

    set_active(
        &db,
        args.kind,
        &args.name,
        &OperatorWrite::claim("vike-cli config activate"),
        now_utc,
        vike_model::AssetClass::SQL_WORDS,
    )
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "activated {word} profile `{}` in {}.\n  The rows reproduce {} exactly, so the crossing \
         changed the ARTIFACT and not the VALUE.\n  {}{arming}\n\n⚠ A RUNNING daemon still holds \
         what it BOOTED with — restart it to pick this up. `vike-cli config deactivate {word}` is \
         the rollback: one column back to 0, the bodies untouched, the files and the unit \
         unchanged.",
        args.name,
        db.display(),
        args.proves.display(),
        change
    ))
}

fn deactivate(
    args: &DeactivateArgs,
    settings_dir: Option<&Path>,
    now_utc: i64,
) -> Result<String, String> {
    let dir = settings_dir.ok_or(
        "no settings directory resolved — there is no store to deselect a profile in. Set \
         VIKE_SETTINGS_DIR, or run from a project that has one.",
    )?;
    let db = vike_secrets::db_path_in(dir);
    let word = args.kind.sql_word();
    let held = read_profiles(&db)
        .map_err(|e| e.to_string())?
        .active(args.kind)
        .map(|p| p.row.name.clone());
    let Some(held) = held else {
        return Ok(format!(
            "nothing to do: no {word} profile is active in {}, so this box already resolves its \
             {word} profile from the file rungs.",
            db.display()
        ));
    };
    if args.dry_run {
        return Ok(format!(
            "would clear the active {word} profile (`{held}`) in {} — NOTHING WAS WRITTEN. The \
             stored BODIES are untouched, so re-crossing is one `config activate` away.",
            db.display()
        ));
    }
    clear_active(
        &db,
        args.kind,
        &OperatorWrite::claim("vike-cli config deactivate"),
        now_utc,
        vike_model::AssetClass::SQL_WORDS,
    )
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "cleared the active {word} profile (`{held}`) in {}.\n  The stored BODIES are untouched — \
         this changed one column, not any data — so re-crossing is one `config activate` \
         away.\n\n⚠ A RUNNING daemon still holds what it BOOTED with; restart it to fall back to \
         the file rungs.",
        db.display()
    ))
}

/// Seconds since the Unix epoch, for the row's `updated_utc` stamp. A wall clock is the right one:
/// the column answers "when did an operator last write this", not an interval.
fn now_utc() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_a(args: &[&str]) -> Result<ActivateArgs, String> {
        parse_activate(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn the_parser_takes_a_kind_a_name_and_a_required_proof() {
        let a = parse_a(&["run", "run-live", "--proves", "settings/run-live.toml"]).unwrap();
        assert_eq!(a.kind, ProfileKind::Run);
        assert_eq!(a.name, "run-live");
        assert_eq!(a.proves, PathBuf::from("settings/run-live.toml"));
        assert!(!a.dry_run);

        // ⚠ `--proves` is REQUIRED and the refusal has to SAY why, because an operator who is told
        // only "missing --proves" will look for a way to skip it.
        let e = parse_a(&["run", "run-live"]).unwrap_err();
        assert!(e.contains("--proves"), "{e}");
        assert!(e.contains("no --force"), "{e}");

        assert!(parse_a(&["nonsuch", "x", "--proves", "f"]).is_err());
        assert!(parse_a(&["run", "--proves", "f"]).is_err(), "the name is required");
        assert!(parse_a(&["run", "a", "b", "--proves", "f"]).is_err(), "one name only");
        assert!(parse_a(&["--help"]).is_err(), "help travels back as the shared sentinel");
    }

    #[test]
    fn deactivate_takes_a_kind_and_nothing_else() {
        let d = parse_deactivate(["daemon".to_string()].into_iter()).unwrap();
        assert_eq!(d.kind, ProfileKind::Daemon);
        assert!(parse_deactivate(std::iter::empty()).is_err());
        assert!(
            parse_deactivate(["run".to_string(), "x".to_string()].into_iter()).is_err(),
            "deactivating is per KIND — there is nothing to name"
        );
    }

    #[test]
    fn a_run_with_no_settings_directory_refuses_rather_than_guessing_one() {
        let a = ActivateArgs {
            kind: ProfileKind::Run,
            name: "x".into(),
            proves: PathBuf::from("f"),
            dry_run: true,
        };
        let e = activate(&a, None, 0).unwrap_err();
        assert!(e.contains("VIKE_SETTINGS_DIR"), "{e}");
        let e = deactivate(&DeactivateArgs { kind: ProfileKind::Run, dry_run: true }, None, 0)
            .unwrap_err();
        assert!(e.contains("VIKE_SETTINGS_DIR"), "{e}");
    }
}

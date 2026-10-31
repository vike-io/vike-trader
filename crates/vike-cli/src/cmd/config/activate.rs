//! `vike-cli config activate` / `config deactivate` — **WHICH stored profile this box runs, as the
//! one deliberate act that changes it.**
//!
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Question 3 was
//! answered by the owner on 2026-09-15: **the ROW wins.**
//! `vike_secrets::profile_store::select` is that ruling written once, and it is consulted at every
//! daemon boot. This is the verb that writes the row: it flips the one `active` column of a profile
//! the store already holds, and it writes no body.
//!
//! # Why storing and selecting are different acts
//!
//! The bodies are written by `config bootstrap-run`, `config bootstrap-daemon` and
//! `config bootstrap-recorder`, each of which stores AND activates in one act. `activate` is the
//! deliberate second act for a body that is already stored under another name — the repoint — and
//! `deactivate` is its rollback.
//!
//! ⚠ **For the `run` kind the active row is now the ONLY rung** (decision 0111): no file and no
//! variable selects a run profile, so `deactivate run` leaves NONE — the paper engines run without
//! an operator budget and a live venue refuses to start.
//!
//! # The fence (decision 0157): a `run` body the daemon cannot load is never activated
//!
//! The comparison against a profile FILE (`--proves`) went with the files. What replaces it is a
//! check the CLI can now make because the run profile's loader is `vike-config`'s: the stored body
//! must load and validate through `vike_config::rows_to_run_profile`, the daemon's own reader.
//! A body it refuses (a stale `mode` row, a typo'd leaf, `max_leverage < 1.0`) is refused here, with
//! its words, and nothing is written.
//!
//! # ⚠ What a `run` activation does, stated rather than implied
//!
//! On a box whose `flags.tradehub_live` is ON and where nothing selects a run profile today, the
//! daemon currently REFUSES to start (`vike_tradehub_core::EngineError::MissingRiskBudget`) and activating a
//! `run` row makes it start LIVE at the next restart. The report says so in those words, every time,
//! for the `run` kind — because that is a state an operator may genuinely be curing, and a verb that
//! refused it would leave them with no cure at all.
//!
//! # …and what a RUNNING daemon does about any of this
//!
//! Nothing, until it restarts. Both roots read the profile rows ONCE, at boot; there is no watcher
//! and no reload. The report ends on that sentence, the same restart-to-apply note every settings
//! write in this tree ends on (`docs/decisions/0086`).

use std::path::Path;
use std::process::ExitCode;

use vike_secrets::profile_store::{
    ActiveProfile, OperatorWrite, ProfileKind, clear_active, read_profiles, set_active,
};

use crate::cmd::args;

/// `config activate`'s arguments, as `clap` reads them: a KIND and a NAME.
#[derive(clap::Args, Debug)]
#[command(
    about = "Make a STORED profile (run|daemon|recorder) the one this box reads from its next \
             restart",
    long_about = "Make a STORED profile the one this box reads, from its next restart.\nThe row \
                  beats the daemon's own `--config` argument — that is the owner's ruling\n\
                  (decision 0057 Question 3) — and for a `run` profile it is the only rung there \
                  is (0111);\n`vike-cli config bootstrap-run` stores and activates a run body \
                  from arguments."
)]
pub(crate) struct ActivateCmd {
    #[arg(
        value_name = "KIND / NAME",
        help = "the profile KIND (run | daemon | recorder), then the stored profile's NAME"
    )]
    positionals: Vec<String>,
    #[arg(long, help = "report the verdict and write nothing")]
    dry_run: bool,
}

/// `config deactivate`'s arguments, as `clap` reads them: a KIND and nothing else.
#[derive(clap::Args, Debug)]
#[command(
    about = "Clear that kind's active row; the stored bodies are untouched, so it is the rollback",
    long_about = "Clear this kind's active row. The stored BODIES are untouched, so re-crossing \
                  is one command.\nNo rung is left below the row for any \
                  kind: with no active `run` row the\npaper engines run without an operator [risk] \
                  budget and a LIVE venue refuses to start\n(0111), and a daemon with no active \
                  `daemon` row refuses to start (0086)."
)]
pub(crate) struct DeactivateCmd {
    #[arg(value_name = "KIND", help = "the profile KIND: run | daemon | recorder")]
    positionals: Vec<String>,
    #[arg(long, help = "report and write nothing")]
    dry_run: bool,
}

/// The kind word, parsed from the operator's own argv.
fn parse_kind(word: &str) -> Result<ProfileKind, String> {
    match word {
        "run" => Ok(ProfileKind::Run),
        "daemon" => Ok(ProfileKind::Daemon),
        "recorder" => Ok(ProfileKind::Recorder),
        other => Err(format!(
            "unknown profile kind '{other}' — expected `run` (the risk budget and sinks), \
             `daemon` (the strategy-mount set) or `recorder` (the subscription set)"
        )),
    }
}

#[derive(Debug)]
struct ActivateArgs {
    kind: ProfileKind,
    name: String,
    dry_run: bool,
}

impl ActivateCmd {
    fn into_args(self) -> Result<ActivateArgs, String> {
        let (mut kind, mut name) = (None, None);
        // The two positionals, in order.
        for word in self.positionals {
            if kind.is_none() {
                kind = Some(parse_kind(&word)?);
            } else if name.is_none() {
                name = Some(word);
            } else {
                return Err(format!("unexpected extra argument '{word}'"));
            }
        }
        Ok(ActivateArgs {
            kind: kind.ok_or("missing the profile KIND (run | daemon | recorder)")?,
            name: name.ok_or("missing the profile NAME — `vike-cli config recorder` prints the recorder names this store holds")?,
            dry_run: self.dry_run,
        })
    }
}

#[derive(Debug)]
struct DeactivateArgs {
    kind: ProfileKind,
    dry_run: bool,
}

impl DeactivateCmd {
    fn into_args(self) -> Result<DeactivateArgs, String> {
        let mut kind = None;
        for word in self.positionals {
            if kind.is_some() {
                return Err(format!("unexpected extra argument '{word}'"));
            }
            kind = Some(parse_kind(&word)?);
        }
        Ok(DeactivateArgs {
            kind: kind.ok_or("missing the profile KIND (run | daemon | recorder)")?,
            dry_run: self.dry_run,
        })
    }
}

/// Read `config activate`'s argv tail through the plane's own parser, so a test meets the grammar
/// an operator does.
#[cfg(test)]
fn parse_activate(argv: impl Iterator<Item = String>) -> Result<ActivateArgs, args::Stop> {
    let argv = std::iter::once("activate".to_string()).chain(argv);
    match crate::cmd::config::parse(argv)? {
        crate::cmd::config::Sub::Activate(cmd) => {
            cmd.into_args().map_err(|msg| args::Stop::message("vike-cli config activate", msg))
        }
        other => unreachable!("`activate` parsed as {other:?}"),
    }
}

/// Read `config deactivate`'s argv tail through the plane's own parser.
#[cfg(test)]
fn parse_deactivate(argv: impl Iterator<Item = String>) -> Result<DeactivateArgs, args::Stop> {
    let argv = std::iter::once("deactivate".to_string()).chain(argv);
    match crate::cmd::config::parse(argv)? {
        crate::cmd::config::Sub::Deactivate(cmd) => {
            cmd.into_args().map_err(|msg| args::Stop::message("vike-cli config deactivate", msg))
        }
        other => unreachable!("`deactivate` parsed as {other:?}"),
    }
}

/// Entry point for `config activate`.
pub(crate) fn run_activate(cmd: ActivateCmd, settings_dir: Option<&Path>) -> ExitCode {
    let args = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse("vike-cli config activate", msg),
    };
    match activate(&args, settings_dir, vike_model::time::clock::now_secs()) {
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
pub(crate) fn run_deactivate(cmd: DeactivateCmd, settings_dir: Option<&Path>) -> ExitCode {
    let args = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse("vike-cli config deactivate", msg),
    };
    match deactivate(&args, settings_dir, vike_model::time::clock::now_secs()) {
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

/// The sentence every `run` activation must carry — see this module's doc for why it is a WARNING
/// and not a refusal. `crate::cmd::config::bootstrap_run` activates too and carries the same one.
pub(super) const RUN_ARMING_WARNING: &str = "\n\n⚠ A `run` profile supplies the PRE-TRADE RISK CEILINGS a LIVE venue refuses to start \
     without. If `flags.tradehub_live` is ON on this box and nothing selects a run profile today, \
     that daemon is exiting FAILURE right now (`EngineError::MissingRiskBudget`) and this row will \
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
             profiles it does hold: {}. `vike-cli config bootstrap-run` (a run body), \
             `bootstrap-daemon` (a daemon body) and `bootstrap-recorder` (a recorder body) are \
             what put one there.",
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

    // ── THE FENCE: a `run` body the daemon would refuse at boot is never made active ──────────
    //
    // The daemon loads the active `run` row ONCE, at boot, through `vike_config::rows_to_run_profile`
    // — the render + the parser, so every refusal the type carries (an unknown key such as the
    // `mode` a pre-0156 body still has, a typo'd leaf, `max_leverage < 1.0`, a retired
    // `sinks.journal.dir`, `sinks.gui`, `sinks.recorder`, `sinks.raw_capture_dir` or
    // `guards.freshness_ms`). Activating a body that loader refuses is the one way to turn a daemon
    // that starts into one that exits at its next restart, so it is refused HERE with the loader's own
    // words (which carry the `bootstrap-run` command that re-writes the body without the offending
    // row, or name the re-run for a retired key). `--dry-run` asks the same question. A daemon or
    // recorder body cannot be checked: their loaders live in crates this binary does not link, and
    // `bootstrap-daemon` / `bootstrap-recorder` run them before they store.
    if args.kind == ProfileKind::Run {
        vike_config::rows_to_run_profile(stored).map_err(|e| {
            format!(
                "REFUSING to activate run profile `{}`: `vike-tradehub` would refuse to boot on this \
                 body, so activating it would turn a daemon that starts into one that exits at its \
                 next restart. Nothing was written.\n\n{e}",
                args.name
            )
        })?;
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
             the next restart, {}.",
            match args.kind {
                ProfileKind::Run => format!(
                    "the daemon builds its pre-trade ceilings from the row `{}` — the only rung \
                     a run profile has (0111)",
                    args.name
                ),
                ProfileKind::Daemon => format!(
                    "the row `{}` beats the daemon's own `--config <path>` argument",
                    args.name
                ),
                ProfileKind::Recorder => format!(
                    "the row `{}` beats `--record-profile <name>` on the recording daemon",
                    args.name
                ),
            }
        ),
    };
    let arming = if args.kind == ProfileKind::Run { RUN_ARMING_WARNING } else { "" };
    let loads = if args.kind == ProfileKind::Run {
        "\n  The stored body loads and validates through the daemon's own loader."
    } else {
        ""
    };

    if args.dry_run {
        return Ok(format!(
            "would activate {word} profile `{}` in {} — NOTHING WAS WRITTEN.\n  {}{loads}{arming}",
            args.name,
            db.display(),
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
        "activated {word} profile `{}` in {}.\n  {}{loads}{arming}\n\n⚠ A RUNNING daemon still holds \
         what it BOOTED with — restart it to pick this up. `vike-cli config deactivate {word}` is \
         the rollback: one column back to 0, the bodies untouched and the unit unchanged.",
        args.name,
        db.display(),
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
            "nothing to do: no {word} profile is active in {}, so this box already runs with no \
             {word} profile.",
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
         away.\n\n⚠ A RUNNING daemon still holds what it BOOTED with; from its next restart it \
         runs with NO {word} profile — no rung is left below the row (0086, 0111): with no `run` \
         profile a live venue refuses to start, and with no `daemon` profile the daemon itself \
         refuses to start.",
        db.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_a(args: &[&str]) -> Result<ActivateArgs, args::Stop> {
        parse_activate(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn the_parser_takes_a_kind_and_a_name() {
        let a = parse_a(&["run", "run-live"]).unwrap();
        assert_eq!(a.kind, ProfileKind::Run);
        assert_eq!(a.name, "run-live");
        assert!(!a.dry_run);

        // The fence that compared the rows to a FILE is gone with the files: the flag is an
        // unknown option now, not a requirement.
        assert!(parse_a(&["run", "run-live", "--proves", "f"]).is_err());
        assert!(parse_a(&["nonsuch", "x"]).is_err());
        assert!(parse_a(&["run"]).is_err(), "the name is required");
        assert!(parse_a(&["run", "a", "b"]).is_err(), "one name only");
        assert!(parse_a(&["--help"]).is_err(), "help travels back as the shared sentinel");
    }

    /// A `run`-kind body stored under `name` from `(path, TOML scalar)` rows, NOT activated.
    fn store_run(dir: &Path, name: &str, rows: &[(&str, &str)]) {
        let db = vike_secrets::db_path_in(dir);
        if !db.exists() {
            vike_secrets::create_empty_store_for_test(&db).unwrap();
        }
        let mut stored = vike_config::run_profile::samples::stored(&[]);
        stored.row.name = name.to_string();
        stored.settings = rows.iter().map(|(p, v)| ((*p).to_string(), (*v).to_string())).collect();
        vike_secrets::profile_store::store_profile(
            &db,
            &stored,
            &OperatorWrite::claim("test"),
            1,
            vike_model::AssetClass::SQL_WORDS,
        )
        .unwrap();
    }

    /// **The fence.** A stored `run` body the daemon's loader refuses is NOT made active — with the
    /// loader's own words and nothing written — and a body it loads is, with the verdict stated.
    #[test]
    fn a_run_body_the_daemon_cannot_load_is_never_activated() {
        let dir = tempfile::tempdir().unwrap();
        let ok = [("risk.max_notional_per_order", "100.0"), ("risk.max_total_exposure", "500.0")];
        store_run(dir.path(), "good", &ok);
        // A pre-0156 body still carries `mode`; a typo'd leaf is refused by name too, and so is a
        // body written before `sinks.gui` (no mount ever applied it) was retired.
        store_run(dir.path(), "stale", &[("mode", "\"live\""), ok[0], ok[1]]);
        store_run(dir.path(), "typo", &[("risk.max_notionl_per_order", "100.0")]);
        store_run(dir.path(), "retired", &[ok[0], ok[1], ("sinks.gui", "true")]);

        let want = |name: &str, dry_run: bool| ActivateArgs {
            kind: ProfileKind::Run,
            name: name.into(),
            dry_run,
        };
        let db = vike_secrets::db_path_in(dir.path());

        for bad in ["stale", "typo", "retired"] {
            let e = activate(&want(bad, false), Some(dir.path()), 2).unwrap_err();
            assert!(e.contains("REFUSING to activate"), "{e}");
            assert!(e.contains("Nothing was written"), "{e}");
            let dry = activate(&want(bad, true), Some(dir.path()), 2).unwrap_err();
            assert!(dry.contains("REFUSING to activate"), "--dry-run asks the same: {dry}");
            assert!(
                read_profiles(&db).unwrap().active(ProfileKind::Run).is_none(),
                "a refused body must not become active"
            );
        }
        let e = activate(&want("stale", true), Some(dir.path()), 2).unwrap_err();
        assert!(e.contains("mode"), "the loader's own words name the offending key: {e}");
        let e = activate(&want("retired", true), Some(dir.path()), 2).unwrap_err();
        assert!(
            e.contains("`sinks.gui` is no longer part of a run profile"),
            "…and a retired key is named with its re-run: {e}"
        );

        let report = activate(&want("good", false), Some(dir.path()), 3).unwrap();
        assert!(report.contains("loads and validates"), "{report}");
        assert_eq!(
            read_profiles(&db).unwrap().active(ProfileKind::Run).map(|p| p.row.name.clone()),
            Some("good".to_string())
        );
    }

    /// **A body that sets `guards.initial_trading_state` activates like any other, with no "not
    /// applied" note** — the key is applied by the daemon now (it sends `SetTradingState` to the
    /// spawned core when the state is not `active`), so the report has nothing to warn about.
    #[test]
    fn a_run_body_setting_the_initial_trading_state_is_activated_without_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let ceilings =
            [("risk.max_notional_per_order", "100.0"), ("risk.max_total_exposure", "500.0")];
        store_run(
            dir.path(),
            "halted",
            &[ceilings[0], ceilings[1], ("guards.initial_trading_state", "\"halted\"")],
        );

        let rehearsal =
            ActivateArgs { kind: ProfileKind::Run, name: "halted".into(), dry_run: true };
        let report = activate(&rehearsal, Some(dir.path()), 2).unwrap();
        assert!(report.contains("NOTHING WAS WRITTEN"), "still a rehearsal: {report}");
        assert!(!report.contains("NOT APPLIED"), "the key is applied now: {report}");

        let live = ActivateArgs { kind: ProfileKind::Run, name: "halted".into(), dry_run: false };
        let report = activate(&live, Some(dir.path()), 3).unwrap();
        assert!(!report.contains("NOT APPLIED"), "{report}");
        let db = vike_secrets::db_path_in(dir.path());
        assert_eq!(
            read_profiles(&db).unwrap().active(ProfileKind::Run).map(|p| p.row.name.clone()),
            Some("halted".to_string())
        );
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
        let a = ActivateArgs { kind: ProfileKind::Run, name: "x".into(), dry_run: true };
        let e = activate(&a, None, 0).unwrap_err();
        assert!(e.contains("VIKE_SETTINGS_DIR"), "{e}");
        let e = deactivate(&DeactivateArgs { kind: ProfileKind::Run, dry_run: true }, None, 0)
            .unwrap_err();
        assert!(e.contains("VIKE_SETTINGS_DIR"), "{e}");
    }
}

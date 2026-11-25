//! `vike-cli config profile activate` / `config profile deactivate` — **WHICH stored profile this box runs, as the
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
//! The bodies are written by `config profile create daemon` and `config profile create recorder`, each of
//! which stores AND activates in one act (`config profile create run` writes a daemon body's run part and
//! selects nothing). `activate` is the deliberate second act for a body that is already stored
//! under another name — the repoint — and `deactivate` is its rollback.
//!
//! ⚠ **The `run` kind is RETIRED** (decision 0297: one live profile per trading daemon): the run
//! document is the RUN part of the daemon body, so `profile activate run` is refused by name, and
//! `profile deactivate run` stays, the second step of the fold. `config profile activate daemon <other>` swaps the
//! ceilings WITH the mounts, so its report prints the run part that takes effect.
//!
//! # The fence (decision 0157): a daemon body whose run part the daemon cannot load is never activated
//!
//! The comparison against a profile FILE (`--proves`) went with the files. What replaces it is a
//! check the CLI can now make because the run profile's loader is `vike-config`'s: the stored body's
//! run part must load and validate through `vike_config::rows_to_run_profile`, the daemon's own
//! reader. A body it refuses (a stale `mode` row, a typo'd leaf, `max_leverage < 1.0`) is refused
//! here, with its words, and nothing is written. The mount part cannot be checked here (its loader
//! is in a crate this binary does not link): `profile create daemon` runs it before it stores, and
//! `config check` asks the daemon.
//!
//! # ⚠ What a daemon activation does to the ceilings, stated rather than implied
//!
//! On a box whose `flags.tradehub_live` is ON and whose active daemon profile carries no run part
//! today, a `demo`/`live` venue currently REFUSES to start (`vike_tradehub_core::EngineError::MissingRiskBudget`)
//! and activating a body that carries one makes it start at the next restart. The report says so in
//! those words, every time a body with a run part is activated — because that is a state an
//! operator may genuinely be curing, and a verb that refused it would leave them with no cure at all.
//!
//! # …and what a RUNNING daemon does about any of this
//!
//! Nothing, until it restarts. Both roots read the profile rows ONCE, at boot; there is no watcher
//! and no reload. The report ends on that sentence, the same restart-to-apply note every settings
//! write in this tree ends on (`docs/decisions/0086`).

use std::path::Path;
use std::process::ExitCode;

use vike_secrets::profile_store::{
    ProfileActive, ProfileKind, ProfileOperatorWrite, ProfileRecord, ProfileStoreError,
    clear_active, edit_profiles, is_run_part, read_profiles, set_active,
};

use crate::cmd::args;

/// `config profile activate`'s arguments, as `clap` reads them: a KIND and a NAME.
#[derive(clap::Args, Debug)]
#[command(
    about = "Make a STORED profile (daemon|recorder) the one this box reads from its next restart",
    long_about = "Make a STORED profile the one this box reads, from its next restart.\nThe row \
                  beats the daemon's own `--config` argument — that is the owner's ruling\n\
                  (decision 0057 Question 3). A daemon profile carries its run part (the [risk]\n\
                  ceilings, [guards], [sinks]) with it; the `run` kind is RETIRED (0297)."
)]
pub(crate) struct ActivateCommand {
    #[arg(
        value_name = "KIND / NAME",
        help = "the profile KIND (daemon | recorder; `run` is retired), then the stored profile's NAME"
    )]
    positionals: Vec<String>,
    #[arg(long, help = "report the verdict and write nothing")]
    dry_run: bool,
}

/// `config profile deactivate`'s arguments, as `clap` reads them: a KIND and nothing else.
#[derive(clap::Args, Debug)]
#[command(
    about = "Clear that kind's active row; the stored bodies are untouched, so it is the rollback",
    long_about = "Clear this kind's active row. The stored BODIES are untouched, so re-crossing \
                  is one command.\nNo rung is left below the row: a daemon with no active \
                  `daemon` row refuses to start (0086).\n`profile deactivate run` deselects a RETIRED \
                  `run` row, the fold's second step (0297)."
)]
pub(crate) struct DeactivateCommand {
    #[arg(value_name = "KIND", help = "the profile KIND: run | daemon | recorder")]
    positionals: Vec<String>,
    #[arg(long, help = "report and write nothing")]
    dry_run: bool,
}

/// The kind word, parsed from the operator's own argv by the store's own reader of the word
/// (`ProfileKind::parse`); only the refusal is this verb's, worded for a person rather than a row.
fn parse_kind(word: &str) -> Result<ProfileKind, String> {
    ProfileKind::parse(word).map_err(|_| {
        format!(
            "unknown profile kind '{word}' — expected `daemon` (the strategy-mount set and its \
             run part), `recorder` (the subscription set) or `run` (RETIRED: deactivate or delete \
             only)"
        )
    })
}

#[derive(Debug)]
struct ActivateArgs {
    kind: ProfileKind,
    name: String,
    dry_run: bool,
}

/// The two positionals `profile activate` and `profile rm` both take, in order: a KIND and a NAME.
pub(in crate::cmd::config) fn kind_and_name(
    positionals: Vec<String>,
) -> Result<(ProfileKind, String), String> {
    let (mut kind, mut name) = (None, None);
    for word in positionals {
        if kind.is_none() {
            kind = Some(parse_kind(&word)?);
        } else if name.is_none() {
            name = Some(word);
        } else {
            return Err(format!("unexpected extra argument '{word}'"));
        }
    }
    Ok((
        kind.ok_or("missing the profile KIND (run | daemon | recorder)")?,
        name.ok_or("missing the profile NAME — `vike-cli config profile ls recorder` prints the recorder names this store holds")?,
    ))
}

impl ActivateCommand {
    fn into_args(self) -> Result<ActivateArgs, String> {
        let (kind, name) = kind_and_name(self.positionals)?;
        Ok(ActivateArgs { kind, name, dry_run: self.dry_run })
    }
}

#[derive(Debug)]
struct DeactivateArgs {
    kind: ProfileKind,
    dry_run: bool,
}

impl DeactivateCommand {
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

/// Read `config profile activate`'s argv tail through the plane's own parser, so a test meets the grammar
/// an operator does.
#[cfg(test)]
fn parse_activate(argv: impl Iterator<Item = String>) -> Result<ActivateArgs, args::Stop> {
    use super::super::{ProfileVerb, Sub};
    parse_config_verb!(
        argv,
        "profile activate",
        cmd in Sub::Profile { verb: ProfileVerb::Activate(cmd) },
        "vike-cli config profile activate"
    )
}

/// Read `config profile deactivate`'s argv tail through the plane's own parser.
#[cfg(test)]
fn parse_deactivate(argv: impl Iterator<Item = String>) -> Result<DeactivateArgs, args::Stop> {
    use super::super::{ProfileVerb, Sub};
    parse_config_verb!(
        argv,
        "profile deactivate",
        cmd in Sub::Profile { verb: ProfileVerb::Deactivate(cmd) },
        "vike-cli config profile deactivate"
    )
}

/// Entry point for `config profile activate`.
pub(crate) fn run_activate(cmd: ActivateCommand, settings_dir: Option<&Path>) -> ExitCode {
    let args = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse("vike-cli config profile activate", msg),
    };
    match activate(&args, settings_dir, vike_model::time::clock::now_secs()) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli config profile activate: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Entry point for `config profile deactivate`.
pub(crate) fn run_deactivate(cmd: DeactivateCommand, settings_dir: Option<&Path>) -> ExitCode {
    let args = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse("vike-cli config profile deactivate", msg),
    };
    match deactivate(&args, settings_dir, vike_model::time::clock::now_secs()) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli config profile deactivate: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// The sentence every activation of a body with a run part must carry — see this module's doc for
/// why it is a WARNING and not a refusal. `crate::cmd::config::profile::create::run` carries the same one.
pub(in crate::cmd::config) const RUN_ARMING_WARNING: &str = "\n\n⚠ A run part supplies the PRE-TRADE RISK CEILINGS a `demo`/`live` venue refuses to \
     start without. If `flags.tradehub_live` is ON on this box and the active daemon profile \
     carries none today, that venue is refusing to start right now (`EngineError::MissingRiskBudget`) \
     and this run part will make it START at the next restart — which may be exactly what you \
     want, and is not something this command can decide for you. `vike-cli config show --filter \
     tradehub_live` says which state this box is in.";

/// The run part a daemon body `stored` brings with it (decision 0297), for a report: its rows, or
/// what having none means.
pub(in crate::cmd::config) fn run_part_line(stored: &ProfileRecord) -> String {
    let rows: Vec<String> = stored
        .settings
        .iter()
        .filter(|(path, _)| is_run_part(path))
        .map(|(path, value)| format!("{path} = {value}"))
        .collect();
    if rows.is_empty() {
        return format!(
            "\n  Its run part is EMPTY: no operator [risk] budget comes with it — the paper engines \
             run without one and a `demo`/`live` venue refuses to start. `vike-cli config \
             profile create run {} --risk.<key> <value> …` writes one.",
            stored.row.name
        );
    }
    format!(
        "\n  The run part that takes effect with it (ceilings, guards, sinks):\n    {}{RUN_ARMING_WARNING}",
        rows.join("\n    ")
    )
}

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
    if args.kind == ProfileKind::Run {
        let daemon = profiles
            .active(ProfileKind::Daemon)
            .map_or("<daemon profile>", |p| p.row.name.as_str());
        return Err(format!(
            "the `run` profile kind is RETIRED (decision 0297): the ceilings, guards and sinks are \
             the RUN part of the daemon profile now. Nothing was written. Write them there with \
             `vike-cli config profile create run {daemon} --risk.<key> <value> …`; `config profile deactivate \
             run` deselects an old row and `config profile rm run <name> --yes` removes one."
        ));
    }

    let stored = profiles.by_name(&args.name).ok_or_else(|| {
        let held: Vec<&str> = profiles
            .all()
            .iter()
            .filter(|p| p.row.kind == args.kind)
            .map(|p| p.row.name.as_str())
            .collect();
        format!(
            "the settings store at {} holds no profile called `{}`. Nothing was written. {} \
             profiles it does hold: {}. `vike-cli config profile create daemon` (a daemon body) and \
             `profile create recorder` (a recorder body) are what put one there.",
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

    // ── THE FENCE: a daemon body whose run part the daemon would refuse at boot is never made active
    //
    // The daemon loads the active daemon row's run part ONCE, at boot, through
    // `vike_config::rows_to_run_profile` — the render + the parser, so every refusal the type carries
    // (an unknown key such as the `mode` a pre-0156 body still has, a typo'd leaf, `max_leverage <
    // 1.0`, a retired `sinks.journal.dir`, `sinks.gui`, `sinks.recorder`, `sinks.raw_capture_dir` or
    // `guards.freshness_ms`). Activating a body that loader refuses is the one way to turn a daemon
    // that starts into one that exits at its next restart, so it is refused HERE with the loader's own
    // words (which carry the `profile create run` command that re-writes the run part without the offending
    // row, or name the re-run for a retired key). `--dry-run` asks the same question. The mount part
    // and a recorder body cannot be checked: their loaders live in crates this binary does not link,
    // and `profile create daemon` / `profile create recorder` run them before they store.
    // Asked again of the body read inside the write's own transaction, below.
    let fence = |body: &ProfileRecord| {
        vike_config::rows_to_run_profile(body).map(|_| ()).map_err(|e| {
            format!(
                "REFUSING to activate daemon profile `{}`: `vike-tradehub` would refuse to boot on \
                 its run part, so activating it would turn a daemon that starts into one that exits \
                 at its next restart. Nothing was written.\n\n{e}",
                args.name
            )
        })
    };
    if args.kind == ProfileKind::Daemon {
        fence(stored)?;
    }

    let already = match profiles.resolve_active(args.kind) {
        ProfileActive::Row(p) => Some(p.row.name.clone()),
        ProfileActive::NoProfileStore
        | ProfileActive::NoneStored
        | ProfileActive::NoneActive { .. } => None,
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
                // Refused above: the kind is retired (decision 0297).
                ProfileKind::Run => String::new(),
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
    let (loads, arming) = if args.kind == ProfileKind::Daemon {
        (
            "\n  Its run part loads and validates through the daemon's own loader.",
            run_part_line(stored),
        )
    } else {
        ("", String::new())
    };

    if args.dry_run {
        return Ok(format!(
            "would activate {word} profile `{}` in {} — NOTHING WAS WRITTEN.\n  {}{loads}{arming}",
            args.name,
            db.display(),
            change
        ));
    }

    let write = ProfileOperatorWrite::claim("vike-cli config profile activate");
    // ONE transaction for a daemon profile: the body is read (and fenced) under the write's own
    // lock, so a `profile create run` landing since the read above is what gets selected.
    let arming = if args.kind == ProfileKind::Daemon {
        edit_profiles(&db, &write, now_utc, vike_model::AssetClass::SQL_WORDS, |t| {
            let body = t
                .read(&args.name)?
                .filter(|p| p.row.kind == ProfileKind::Daemon)
                .ok_or_else(|| {
                    ProfileStoreError::EditRefused(format!(
                        "daemon profile `{}` is no longer stored. Nothing was written.",
                        args.name
                    ))
                })?;
            fence(&body).map_err(ProfileStoreError::EditRefused)?;
            t.set_active(ProfileKind::Daemon, &args.name)?;
            Ok(run_part_line(&body))
        })
        .map_err(|e| e.to_string())?
    } else {
        set_active(&db, args.kind, &args.name, &write, now_utc, vike_model::AssetClass::SQL_WORDS)
            .map_err(|e| e.to_string())?;
        arming
    };
    Ok(format!(
        "activated {word} profile `{}` in {}.\n  {}{loads}{arming}\n\n⚠ A RUNNING daemon still \
         holds what it BOOTED with — restart it to pick this up. `vike-cli config profile deactivate {word}` \
         is the rollback: one column back to 0, the bodies untouched and the unit unchanged.",
        args.name,
        db.display(),
        change,
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
             stored BODIES are untouched, so re-crossing is one `config profile activate` away.",
            db.display()
        ));
    }
    clear_active(
        &db,
        args.kind,
        &ProfileOperatorWrite::claim("vike-cli config profile deactivate"),
        now_utc,
        vike_model::AssetClass::SQL_WORDS,
    )
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "cleared the active {word} profile (`{held}`) in {}.\n  The stored BODIES are untouched — \
         this changed one column, not any data — so re-crossing is one `config profile activate` \
         away.\n\n⚠ A RUNNING daemon still holds what it BOOTED with; {}",
        db.display(),
        if args.kind == ProfileKind::Run {
            "the trading daemon refuses to start while a retired `run` row is ACTIVE, and reads its \
             ceilings from the active daemon profile's run part alone from its next start \
             (decision 0297). `vike-cli config profile rm run <name> --yes` removes the body."
                .to_string()
        } else {
            format!(
                "from its next restart it runs with NO {word} profile — no rung is left below the \
                 row (0086): with no `daemon` profile the daemon itself refuses to start."
            )
        }
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

    /// A DAEMON body (one bybit mount) stored under `name` with `(path, TOML scalar)` rows as its
    /// settings — its run part when they are `risk.`/`guards.`/`sinks.` — NOT activated.
    fn store_daemon(dir: &Path, name: &str, rows: &[(&str, &str)]) {
        let db = vike_secrets::db_path_in(dir);
        if !db.exists() {
            vike_secrets::create_empty_store_for_test(&db).unwrap();
        }
        let mut stored = vike_config::run_profile::samples::stored(&[]);
        stored.row.name = name.to_string();
        stored.row.kind = ProfileKind::Daemon;
        let mut mount = vike_secrets::profile_store::MountRow::new(0, "bybit", "CryptoPerp");
        mount.symbol = Some("BTCUSDT.P".to_string());
        stored.mounts = vec![mount];
        stored.settings = rows.iter().map(|(p, v)| ((*p).to_string(), (*v).to_string())).collect();
        vike_secrets::profile_store::store_profile(
            &db,
            &stored,
            &ProfileOperatorWrite::claim("test"),
            1,
            vike_model::AssetClass::SQL_WORDS,
        )
        .unwrap();
    }

    fn active_daemon(dir: &Path) -> Option<String> {
        let db = vike_secrets::db_path_in(dir);
        read_profiles(&db).unwrap().active(ProfileKind::Daemon).map(|p| p.row.name.clone())
    }

    /// **The fence.** A stored daemon body whose RUN part the daemon's loader refuses is NOT made
    /// active — with the loader's own words and nothing written — and a body it loads is, with the
    /// verdict and the run part that takes effect stated (decision 0297).
    #[test]
    fn a_daemon_body_whose_run_part_the_daemon_cannot_load_is_never_activated() {
        let dir = tempfile::tempdir().unwrap();
        let ok = [("risk.max_notional_per_order", "100.0"), ("risk.max_total_exposure", "500.0")];
        store_daemon(dir.path(), "good", &[("daemon.summary_ms", "60000"), ok[0], ok[1]]);
        // A pre-0156 body still carries `mode`; a typo'd leaf is refused by name too, and so is a
        // body written before `sinks.gui` (no mount ever applied it) was retired.
        store_daemon(dir.path(), "stale", &[("mode", "\"live\""), ok[0], ok[1]]);
        store_daemon(dir.path(), "typo", &[("risk.max_notionl_per_order", "100.0")]);
        store_daemon(dir.path(), "retired", &[ok[0], ok[1], ("sinks.gui", "true")]);

        let want = |name: &str, dry_run: bool| ActivateArgs {
            kind: ProfileKind::Daemon,
            name: name.into(),
            dry_run,
        };

        for bad in ["stale", "typo", "retired"] {
            let e = activate(&want(bad, false), Some(dir.path()), 2).unwrap_err();
            assert!(e.contains("REFUSING to activate"), "{e}");
            assert!(e.contains("Nothing was written"), "{e}");
            let dry = activate(&want(bad, true), Some(dir.path()), 2).unwrap_err();
            assert!(dry.contains("REFUSING to activate"), "--dry-run asks the same: {dry}");
            assert!(active_daemon(dir.path()).is_none(), "a refused body must not become active");
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
        assert!(report.contains("risk.max_total_exposure = 500.0"), "the run part: {report}");
        assert!(!report.contains("summary_ms"), "the daemon part is no run part: {report}");
        assert!(report.contains("PRE-TRADE RISK CEILINGS"), "the arming warning: {report}");
        assert_eq!(active_daemon(dir.path()), Some("good".to_string()));

        // A body with NO run part activates, and the report says what that means.
        store_daemon(dir.path(), "bare", &[]);
        let report = activate(&want("bare", true), Some(dir.path()), 4).unwrap();
        assert!(report.contains("Its run part is EMPTY"), "{report}");
    }

    /// **The `run` kind is RETIRED (decision 0297)**: `activate run` is refused by name, with the
    /// cure naming the active daemon profile, and nothing is written — on a rehearsal as on a write.
    #[test]
    fn activating_a_run_profile_is_refused_naming_the_daemon_profile() {
        let dir = tempfile::tempdir().unwrap();
        store_daemon(dir.path(), "tradehub", &[]);
        let daemon =
            ActivateArgs { kind: ProfileKind::Daemon, name: "tradehub".into(), dry_run: false };
        activate(&daemon, Some(dir.path()), 2).unwrap();
        for dry_run in [true, false] {
            let run = ActivateArgs { kind: ProfileKind::Run, name: "run-live".into(), dry_run };
            let e = activate(&run, Some(dir.path()), 3).unwrap_err();
            assert!(e.contains("RETIRED"), "{e}");
            assert!(
                e.contains("profile create run tradehub"),
                "the cure names the daemon profile: {e}"
            );
            assert!(e.contains("Nothing was written"), "{e}");
        }
        let db = vike_secrets::db_path_in(dir.path());
        assert!(read_profiles(&db).unwrap().active(ProfileKind::Run).is_none());
    }

    /// **A body that sets `guards.initial_trading_state` activates like any other, with no "not
    /// applied" note** — the key is applied by the daemon now (it sends `SetTradingState` to the
    /// spawned core when the state is not `active`), so the report has nothing to warn about.
    #[test]
    fn a_daemon_body_setting_the_initial_trading_state_is_activated_without_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let ceilings =
            [("risk.max_notional_per_order", "100.0"), ("risk.max_total_exposure", "500.0")];
        store_daemon(
            dir.path(),
            "halted",
            &[ceilings[0], ceilings[1], ("guards.initial_trading_state", "\"halted\"")],
        );

        let rehearsal =
            ActivateArgs { kind: ProfileKind::Daemon, name: "halted".into(), dry_run: true };
        let report = activate(&rehearsal, Some(dir.path()), 2).unwrap();
        assert!(report.contains("NOTHING WAS WRITTEN"), "still a rehearsal: {report}");
        assert!(!report.contains("NOT APPLIED"), "the key is applied now: {report}");
        assert!(report.contains("guards.initial_trading_state = \"halted\""), "{report}");

        let live =
            ActivateArgs { kind: ProfileKind::Daemon, name: "halted".into(), dry_run: false };
        let report = activate(&live, Some(dir.path()), 3).unwrap();
        assert!(!report.contains("NOT APPLIED"), "{report}");
        assert_eq!(active_daemon(dir.path()), Some("halted".to_string()));
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

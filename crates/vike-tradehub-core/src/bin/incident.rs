//! `incident --since <dur>` — freeze the perishable evidence of a live run into ONE timestamped
//! bundle. Feature-free (no venue/native deps), so it builds in the default/CI lane.
//!
//! ## What it collects (see [`vike_tradehub_core::incident`])
//! Into `incident-<UTC>/` (per-file gz + a plain `manifest.json`): journal segments
//! ([`vike_journal`]) and the trace-log slice overlapping the window, the active `run` row's body
//! (rendered to TOML) + its FNV-1a64 hash, the latest [`vike_exec::EngineSnapshot`] + `state_hash`
//! from the journal's last `Snap`, git SHA / build info, and a REDACTED environment dump (secrets
//! masked like the signers' `Debug` impls).
//!
//! ## Usage
//! ```sh
//! cargo run -p vike-tradehub-core --bin incident -- --since 2h
//! # a leading `incident` token is accepted and skipped
//! cargo run -p vike-tradehub-core --bin incident -- incident --since 30m --out /var/vike/incidents
//! cargo run -p vike-tradehub-core --bin incident -- --since 1d --journal-dir data/journal
//! ```
//! `--since` takes a vike interval (`30s`, `15m`, `2h`, `1d`); the bundle path is printed on
//! stdout. Defaults: `--out` → `<project>/settings/state/incidents` (else `<exe dir>/incidents`);
//! journal → `--journal-dir` / the `config.journal_dir` row (a run profile names no directory);
//! logs → `vike_log::resolve_log_dir` (`VIKE_LOG_DIR` > `--log-dir` >
//! `<project>/settings/state/logs` > `<exe dir>/logs`); run profile → the ACTIVE `run` row of the
//! project's settings database (`VIKE_SETTINGS_DIR`, else the project walk from the working
//! directory), read-only — the one place the daemon reads it too. No profile FILE is read
//! (decision 0111).
//!
//! ## The command line
//! [`Cli`] is the program's whole argv, read by `clap` through `vike_util::cli::Program`
//! (decision 0155): `--flag value` and `--flag=value`, an unknown flag refused with a suggestion
//! (`--profile` is one: the run profile is the settings database's ACTIVE `run` row), a flag given
//! twice refused, `-h`/`--help` and `-V`/`--version` on stdout with exit 0, any other usage error
//! on stderr with exit 2. ⚠ `--out --help` is an ERROR (a value is required for `--out`), never a
//! short-circuit to the help page: exiting 0 would silently discard the typed `--out`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use vike_paths::state_path::INCIDENTS_SUBDIR;
use vike_tradehub_core::incident::{IncidentConfig, RunProfileRow, run_incident};
use vike_util::cli::{CliExit, Program};

/// The command line: what the operator typed, before any source is resolved.
#[derive(clap::Parser, Debug)]
#[command(
    name = "incident",
    about = "Freeze the perishable evidence of a live run into one timestamped bundle"
)]
struct Cli {
    /// How far back the evidence reaches: a vike interval (30s, 15m, 2h, 1d).
    #[arg(long, value_name = "DUR", value_parser = parse_since)]
    since: Duration,
    /// Where the bundle is written (default: <project>/settings/state/incidents).
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
    /// The journal directory to freeze (default: the config.journal_dir row).
    #[arg(long, value_name = "DIR")]
    journal_dir: Option<PathBuf>,
    /// The log directory (loses to $VIKE_LOG_DIR; default: <project>/settings/state/logs).
    #[arg(long, value_name = "DIR")]
    log_dir: Option<PathBuf>,
}

/// The resolved arguments: [`Cli`] with every default filled from the settings database, the
/// environment, the exe and the working directory.
///
/// `Debug` because `Result::expect_err` needs it.
#[derive(Debug)]
struct Args {
    since: Duration,
    out: PathBuf,
    journal_dir: Option<PathBuf>,
    log_dir: PathBuf,
}

/// `--since`'s value parser: a vike interval, and nothing else (`2`, `2 hours`, `2H` and `1.5h`
/// are errors, not a zero-length window freezing an empty bundle).
fn parse_since(raw: &str) -> Result<Duration, String> {
    vike_marketdata::interval::interval_ms(raw)
        .map(|ms| Duration::from_millis(ms as u64))
        .ok_or_else(|| "use e.g. 30s, 15m, 2h, 1d".to_string())
}

/// The program's identity for the parse: the name in `<bin>: <message>` and the `--version` line.
fn program() -> Program<'static> {
    Program::new("incident", concat!("incident ", env!("CARGO_PKG_VERSION")))
}

/// [`parse_args_from`] over the process argv; this wrapper decides only the SOURCE of argv, and on
/// `--help`, `--version` or a usage error prints and exits.
fn read_args() -> Args {
    match parse_args_from(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(exit) => exit.emit(),
    }
}

/// Parse an `argv[0]`-stripped stream, then resolve the sources the way a live node does. A flag
/// MENTIONED and not fed is an error; an unmentioned one keeps its default.
///
/// ⚠ Only the ARGV is injected: resolution still reads the settings database's
/// `config.journal_dir` row, `VIKE_LOG_DIR` (the logger's bootstrap directory), the exe and the cwd
/// (binary-only reads, per the settings registry), and ONLY after the command line has parsed. A
/// test may assert only on `since` and flags it supplied (`--out`, `--journal-dir` win outright;
/// `--log-dir` LOSES to `$VIKE_LOG_DIR`). The run profile is not an argument at all:
/// [`active_run_profile`] reads it out of the settings database in `main`.
///
/// # Errors
/// The [`CliExit`] a process would have printed: `--help`/`--version` (code 0) or a usage error
/// (code 2).
fn parse_args_from(argv: impl Iterator<Item = String>) -> Result<Args, CliExit> {
    let mut argv: Vec<String> = argv.collect();
    // Drop ONE leading `incident` subcommand token (the multicall spelling).
    if argv.first().map(String::as_str) == Some("incident") {
        argv.remove(0);
    }
    let Cli { since, out, journal_dir, log_dir: log_dir_flag } = program().try_parse_from(argv)?;

    // Resolve like a live node (the `config.journal_dir` row, vike_log's log-dir rule).
    let journal_dir = journal_dir.or_else(|| settings_dir().and_then(|d| journal_dir_row(&d)));
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    // The SAME four-layer precedence a live node writes with, so the collector never looks where
    // nothing writes; nodes write to the project layer (`<project>/settings/state/logs`).
    let log_dir = vike_log::resolve_log_dir(
        std::env::var("VIKE_LOG_DIR").ok().as_deref(),
        log_dir_flag.as_deref(),
        std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_paths::state_path::project_log_dir(&cwd))
            .as_deref(),
        &exe_dir,
    );
    // ⚠ **NOT scratch: never `<project>/tmp`, a `ScratchDir` or system temp.** A bundle is evidence
    // read after its producer is gone; a drop-deleting `ScratchDir` or a newest-N-pruned root
    // (`crates/vike-paths/src/state_path.rs`'s `PROJECT_TMP_DIR`) would destroy it, and a
    // fixed system-temp name dies with a container restart and is owned forever by whichever user
    // created it first. It lands beside the logs, in `<project>/settings/state` from the project
    // walk — not off `$VIKE_LOG_DIR`'s parent, which may be anywhere; `<exe_dir>` is the last resort.
    let out = out.unwrap_or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_paths::state_path::project_state_dir(&cwd))
            .unwrap_or_else(|| exe_dir.clone())
            .join(INCIDENTS_SUBDIR)
    });

    Ok(Args { since, out, journal_dir, log_dir })
}

/// **The run profile in force: the ACTIVE `run` row of this project's settings database**, read
/// read-only — the same row the daemon loads at boot, rendered by the same renderer
/// (`vike_secrets::profile_store::render_run_toml`), so the bundle pins exactly what the daemon
/// parsed.
///
/// `None` when there is no project, no database or no active row (nothing to capture). A database
/// that EXISTS and cannot be read is `Some(Err(..))`, recorded in the manifest rather than read as
/// "no run profile" — the distinction every store reader in this workspace keeps.
///
/// The settings directory is `VIKE_SETTINGS_DIR` when it names one, else the project walk from the
/// working directory — the one pointer decision 0111 keeps in the environment.
fn active_run_profile() -> Option<Result<RunProfileRow, String>> {
    let settings = settings_dir()?;
    let db = vike_secrets::db_path_in(&settings);
    let profiles = match vike_secrets::profile_store::read_profiles(&db) {
        Ok(p) => p,
        Err(e) => {
            return Some(Err(format!(
                "the settings database at {} exists and could not be read: {e}",
                db.display()
            )));
        }
    };
    let stored = profiles.active(vike_secrets::profile_store::ProfileKind::Run)?;
    Some(Ok(RunProfileRow {
        name: stored.row.name.clone(),
        toml: vike_secrets::profile_store::render_run_toml(stored),
    }))
}

/// This project's settings directory: `VIKE_SETTINGS_DIR` when it names one, else the project walk
/// from the working directory — the one pointer decision 0111 keeps in the environment.
fn settings_dir() -> Option<PathBuf> {
    let named = std::env::var("VIKE_SETTINGS_DIR").ok();
    match std::env::current_dir() {
        Ok(cwd) => vike_paths::state_path::project_settings_dir_from(named.as_deref(), &cwd),
        Err(_) => named.filter(|s| !s.trim().is_empty()).map(PathBuf::from),
    }
}

/// The `config.journal_dir` row of the settings database under `settings`, read-only — the WAL
/// directory a live node writes to, and the ONLY place it is named (decision 0111, amended
/// 2026-10-11). `None` for no database, no row, or a row that is not a JSON string (the loader
/// refuses such a row at the daemon's boot; this collector, which must keep working on a broken
/// box, just does not use it).
fn journal_dir_row(settings: &Path) -> Option<PathBuf> {
    let source = vike_secrets::read_settings_in(settings).ok()?;
    let row =
        source.rows()?.settings.iter().find(|r| r.section == "config" && r.key == "journal_dir")?;
    serde_json::from_str::<String>(&row.value).ok().map(PathBuf::from)
}

fn main() -> ExitCode {
    // Console only: file logging OFF so the collector never writes into the logs it freezes.
    let _guards = vike_log::init(vike_log::LogConfig {
        file_enabled: false,
        file_prefix: "vike-incident".to_string(),
        ..Default::default()
    });

    let args = read_args();

    let cfg = IncidentConfig {
        since: args.since,
        out_root: args.out,
        journal_dir: args.journal_dir,
        log_dir: Some(args.log_dir),
        run_profile: active_run_profile(),
    };

    match run_incident(&cfg) {
        Ok(bundle) => {
            // Result path on raw stdout (logging convention for results).
            println!("{}", bundle.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!("incident bundle failed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> std::vec::IntoIter<String> {
        v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>().into_iter()
    }

    /// The parsed `Args`, or a panic naming what came back instead.
    fn run_args(v: &[&str]) -> Args {
        match parse_args_from(args(v)) {
            Ok(a) => a,
            Err(exit) => panic!("expected a run from {v:?}, got {exit:?}"),
        }
    }

    /// The refusal the test expects: a usage error (stderr, exit 2), returned as its text.
    fn refused(v: &[&str]) -> String {
        let exit = parse_args_from(args(v)).expect_err("this command line must be refused");
        assert_eq!(exit.code, vike_util::cli::USAGE_EXIT, "{v:?}: {exit:?}");
        assert!(!exit.to_stdout, "{v:?}: an error goes to stderr: {exit:?}");
        exit.text
    }

    /// `--help`/`--version`: a success on stdout, never a usage error.
    fn help_text(v: &[&str]) -> String {
        let exit = parse_args_from(args(v)).expect_err("help is an exit, not a run");
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{v:?}: {exit:?}");
        exit.text
    }

    /// Every documented `--since` unit resolves.
    #[test]
    fn since_accepts_every_documented_unit() {
        for (spelled, want_ms) in
            [("30s", 30_000u64), ("15m", 900_000), ("2h", 7_200_000), ("1d", 86_400_000)]
        {
            let a = run_args(&["--since", spelled, "--out", "/tmp/b"]);
            assert_eq!(a.since, Duration::from_millis(want_ms), "--since {spelled}");
        }
    }

    /// A malformed `--since` is an ERROR, not a zero window freezing an empty bundle.
    #[test]
    fn a_malformed_since_is_an_error_not_an_empty_window() {
        for bad in ["2", "2 hours", "-1m", "1.5h", "2H", ""] {
            let e = refused(&["--since", bad]);
            assert!(e.contains("--since"), "the error names the flag for {bad:?}: {e}");
        }
        let missing = refused(&[]);
        assert!(missing.contains("--since"), "{missing}");
        let trailing = refused(&["--since"]);
        assert!(trailing.contains("--since"), "{trailing}");
    }

    /// ONE leading `incident` token is dropped; a second is an unknown argument.
    #[test]
    fn one_leading_incident_token_is_dropped_and_a_second_is_not() {
        let a = run_args(&["incident", "--since", "30m", "--out", "/tmp/b"]);
        assert_eq!(a.since, Duration::from_millis(1_800_000));
        let e = refused(&["incident", "incident", "--since", "30m"]);
        assert!(e.contains("incident"), "{e}");
    }

    /// `--out` and `--journal-dir` win outright, so they are deterministic on any harness.
    #[test]
    fn out_and_journal_dir_are_taken_verbatim() {
        let a = run_args(&[
            "--since",
            "1h",
            "--out",
            "/var/vike/incidents",
            "--journal-dir",
            "/data/journal",
        ]);
        assert_eq!(a.out, PathBuf::from("/var/vike/incidents"));
        assert_eq!(a.journal_dir, Some(PathBuf::from("/data/journal")));
    }

    /// `--flag=value` is read like `--flag value` (decision 0155, rule 1).
    #[test]
    fn the_equals_spelling_is_read() {
        let a = run_args(&["--since=2h", "--out=/tmp/b", "--journal-dir=/j"]);
        assert_eq!(a.since, Duration::from_secs(7_200));
        assert_eq!(a.out, PathBuf::from("/tmp/b"));
        assert_eq!(a.journal_dir, Some(PathBuf::from("/j")));
    }

    /// `--profile` is GONE (decision 0111): the run profile is the settings database's ACTIVE
    /// `run` row, which the bin reads itself, so a profile FILE named on argv is refused as the
    /// unknown flag it now is rather than silently ignored.
    #[test]
    fn a_profile_file_argument_is_refused() {
        let e = refused(&["--since", "1h", "--profile", "run.toml"]);
        assert!(e.contains("--profile"), "{e}");
    }

    /// A typo'd flag is REJECTED and the message quotes it.
    #[test]
    fn an_unknown_flag_is_rejected_and_named() {
        let e = refused(&["--since", "1h", "--outt", "/tmp/b"]);
        assert!(e.contains("--outt"), "{e}");
    }

    /// A valued flag may not swallow a following flag (`--out --profile` once wrote the bundle into
    /// `./--profile`, back when that flag existed): a usage error naming the starved flag, refused
    /// before any [`Args`] exists.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        let a = refused(&["--since", "1h", "--out", "--log-dir"]);
        assert!(a.contains("--out"), "the starved flag is named: {a}");

        let b = refused(&["--since", "1h", "--log-dir", "--out"]);
        assert!(b.contains("--log-dir"), "{b}");

        // `--since` too: no window from a flag name.
        let c = refused(&["--since", "--out", "/tmp/b"]);
        assert!(c.contains("--since"), "{c}");

        // `-1m` is not a number, so it is a flag-shaped token and refused, not taken as a value.
        let d = refused(&["--since", "-1m"]);
        assert!(d.contains("--since"), "{d}");
    }

    /// `--help` in a VALUE slot is a swallow, not a short-circuit: exiting 0 would silently discard
    /// the typed `--out`. In FLAG position it still succeeds.
    #[test]
    fn a_help_in_value_position_is_a_swallow_while_one_in_flag_position_still_succeeds() {
        let e = refused(&["--out", "--help"]);
        assert!(e.contains("--out"), "the starved flag is named: {e}");
        // …and the two orders that DO reach the help page.
        assert!(help_text(&["--help", "--out"]).contains("Usage"));
        assert!(help_text(&["--out", "/tmp/b", "--help"]).contains("Usage"));
    }

    /// A repeated flag is REFUSED (decision 0155, rule 3); it used to take the last value.
    #[test]
    fn a_repeated_flag_is_refused() {
        let e = refused(&["--since", "1h", "--since", "30s", "--out", "/tmp/b"]);
        assert!(e.contains("--since"), "{e}");
    }

    /// POSITIVE argv pins (decision 0155, written against the hand-rolled parser and kept through
    /// the `clap` migration): every spelling the module doc and an operator's shell history carry
    /// is ACCEPTED, in any flag order, with the same meaning.
    #[test]
    fn every_documented_line_is_accepted_with_its_meaning() {
        let a = run_args(&["--since", "2h"]);
        assert_eq!(a.since, Duration::from_secs(7_200));
        let b = run_args(&["incident", "--since", "30m", "--out", "/var/vike/incidents"]);
        assert_eq!(
            (b.since, b.out),
            (Duration::from_secs(1_800), PathBuf::from("/var/vike/incidents"))
        );
        let c = run_args(&["--since", "1d", "--journal-dir", "data/journal"]);
        assert_eq!(c.since, Duration::from_secs(86_400));
        assert_eq!(c.journal_dir, Some(PathBuf::from("data/journal")));
        // Every flag, in a different order from the usage line.
        let d = run_args(&[
            "--out",
            "/tmp/b",
            "--journal-dir",
            "/j",
            "--log-dir",
            "/l",
            "--since",
            "30s",
        ]);
        assert_eq!(d.since, Duration::from_secs(30));
        assert_eq!(d.out, PathBuf::from("/tmp/b"));
        assert_eq!(d.journal_dir, Some(PathBuf::from("/j")));
        // `--log-dir` LOSES to `$VIKE_LOG_DIR` (the logger's bootstrap directory), so it is
        // asserted only on a harness that sets none.
        if std::env::var("VIKE_LOG_DIR").is_err() {
            assert_eq!(d.log_dir, PathBuf::from("/l"));
        }
    }

    /// A directory whose name begins `--` is reachable as `./--weird`, and one with a space needs
    /// no quoting beyond the shell's own.
    #[test]
    fn a_dot_slash_dashed_or_spaced_directory_is_a_value() {
        let a = run_args(&["--since", "1h", "--out", "./--weird", "--journal-dir", "my journal"]);
        assert_eq!(a.out, PathBuf::from("./--weird"));
        assert_eq!(a.journal_dir, Some(PathBuf::from("my journal")));
    }

    /// `-h`/`--help` print the help page (exit 0), even without `--since` and after other flags,
    /// and `-V`/`--version` print the version line.
    #[test]
    fn help_and_version_succeed_even_without_since() {
        for flag in ["-h", "--help"] {
            assert!(help_text(&[flag]).contains("--since"), "{flag}");
        }
        assert!(help_text(&["--since", "1h", "--help"]).contains("--journal-dir"));
        for flag in ["-V", "--version"] {
            assert_eq!(
                help_text(&[flag]),
                concat!("incident ", env!("CARGO_PKG_VERSION"), "\n"),
                "{flag}"
            );
        }
    }
}

//! `incident --since <dur>` — freeze the perishable evidence of a live run into ONE timestamped
//! bundle. Feature-free (no venue/native deps), so it builds in the default/CI lane.
//!
//! ## What it collects (see [`vike_mount::incident`])
//! Into `incident-<UTC>/` (per-file gz + a plain `manifest.json`): journal segments
//! ([`vike_journal`]) and the trace-log slice overlapping the window, the resolved `RunProfile`
//! TOML + its FNV-1a64 hash, the latest [`vike_exec::EngineSnapshot`] + `state_hash` from the
//! journal's last `Snap`, git SHA / build info, and a REDACTED environment dump (secrets masked
//! like the signers' `Debug` impls).
//!
//! ## Usage
//! ```sh
//! cargo run -p vike-mount --bin incident -- --since 2h
//! # a leading `incident` token is accepted and skipped
//! cargo run -p vike-mount --bin incident -- incident --since 30m --out /var/vike/incidents
//! cargo run -p vike-mount --bin incident -- --since 1d \
//!     --journal-dir data/journal --profile run.toml
//! ```
//! `--since` takes a vike interval (`30s`, `15m`, `2h`, `1d`); the bundle path is printed on
//! stdout. Defaults: `--out` → `<project>/settings/state/incidents` (else `<exe dir>/incidents`);
//! journal → `--journal-dir` / `VIKE_JOURNAL_DIR` / the profile's journal sink; logs →
//! `vike_log::resolve_log_dir` (`VIKE_LOG_DIR` > `--log-dir` > `<project>/settings/state/logs` >
//! `<exe dir>/logs`); profile → `--profile` / `VIKE_RUN_PROFILE`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use vike_model::paths::state_path::INCIDENTS_SUBDIR;
use vike_mount::incident::{IncidentConfig, run_incident};

/// `Debug` because `Result::expect_err` needs it.
#[derive(Debug)]
struct Args {
    since: Duration,
    out: PathBuf,
    journal_dir: Option<PathBuf>,
    log_dir: PathBuf,
    profile: Option<PathBuf>,
}

/// What a successful parse produced. `-h`/`--help` is a success, not an error: a non-zero `--help`
/// breaks `set -e` and status-checking wrappers (same shape as `vike_tradehub`'s `Parsed::Help`).
#[derive(Debug)]
enum Parsed {
    Args(Args),
    Help,
}

/// Printed by both the `--help` and the error path.
const USAGE: &str = "usage: incident --since <dur> [--out <dir>] [--journal-dir <dir>] \
     [--log-dir <dir>] [--profile <run.toml>]";

/// [`parse_args_from`] over the process argv; this wrapper decides only the SOURCE of argv.
fn parse_args() -> Result<Parsed, String> {
    parse_args_from(std::env::args().skip(1))
}

/// **A valued flag must be GIVEN a value, and a token beginning with `--` is a FLAG, never a
/// value** — the rule `vike_backfill::cli::flag_value` spells, repeated because nothing may depend
/// on that crate (it pulls every bridge). `--`, not `-`: negative numbers are real values here; a
/// directory that truly starts with `--` is reachable as `./--weird`. Without it `--out --profile`
/// wrote the evidence bundle into a directory named `--profile`.
///
/// ⚠ **`--out --help` is refused as a swallow, not short-circuited to [`Parsed::Help`]**: exiting 0
/// would silently discard the typed `--out`. `main` still prints usage on the error path. `--help`
/// in FLAG position (first, or after a fed flag) still succeeds.
fn flag_value(flag: &str, next: Option<String>) -> Result<String, String> {
    match next {
        None => Err(format!("{flag} needs a value")),
        Some(v) if v.starts_with("--") => {
            Err(format!("{flag} needs a value, but the next argument is another flag ({v})"))
        }
        Some(v) => Ok(v),
    }
}

/// Parse an `argv[0]`-stripped stream, then resolve the sources the way a live node does. A flag
/// MENTIONED and not fed is an error ([`flag_value`]); an unmentioned one keeps its default.
///
/// ⚠ Only the ARGV is injected: resolution still reads `VIKE_RUN_PROFILE`, `VIKE_JOURNAL_DIR`,
/// `VIKE_LOG_DIR`, the exe and the cwd (binary-only reads, per the settings registry). A test may
/// assert only on `since` and flags it supplied (`--out`, `--journal-dir` win outright; `--log-dir`
/// LOSES to `$VIKE_LOG_DIR`).
fn parse_args_from(argv: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut argv: Vec<String> = argv.collect();
    // Drop ONE leading `incident` subcommand token.
    if argv.first().map(String::as_str) == Some("incident") {
        argv.remove(0);
    }

    let mut since: Option<Duration> = None;
    let mut out: Option<PathBuf> = None;
    let mut journal_dir: Option<PathBuf> = None;
    let mut log_dir_flag: Option<PathBuf> = None;
    let mut profile: Option<PathBuf> = None;

    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].as_str();
        let mut val = || {
            i += 1;
            flag_value(flag, argv.get(i).cloned())
        };
        match flag {
            "--since" => {
                let v = val()?;
                let ms = vike_model::time::interval_ms(&v)
                    .ok_or_else(|| format!("bad --since {v:?} (use e.g. 30s, 15m, 2h, 1d)"))?;
                since = Some(Duration::from_millis(ms as u64));
            }
            "--out" => out = Some(PathBuf::from(val()?)),
            "--journal-dir" => journal_dir = Some(PathBuf::from(val()?)),
            "--log-dir" => log_dir_flag = Some(PathBuf::from(val()?)),
            "--profile" => profile = Some(PathBuf::from(val()?)),
            "-h" | "--help" => return Ok(Parsed::Help),
            other => return Err(format!("unknown flag {other:?}")),
        }
        i += 1;
    }

    let since = since.ok_or_else(|| "missing required --since <dur> (e.g. 2h)".to_string())?;

    // Resolve like a live node (vike_core::journal_config_from_env, vike_log's log-dir rule); the
    // `--profile` vs `VIKE_RUN_PROFILE` precedence is `vike_core::run_profile`'s
    // `resolve_profile_path` / `resolve_profile`, called, never re-derived.
    let mut profile_vars = std::collections::HashMap::new();
    if let Some(v) = std::env::var_os("VIKE_RUN_PROFILE") {
        profile_vars.insert("VIKE_RUN_PROFILE".to_string(), v.to_string_lossy().into_owned());
    }
    let profile = vike_core::run_profile::resolve_profile_path(profile.as_deref(), &profile_vars);
    let journal_dir =
        journal_dir.or_else(|| std::env::var_os("VIKE_JOURNAL_DIR").map(PathBuf::from));
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
            .and_then(|cwd| vike_model::paths::state_path::project_log_dir(&cwd))
            .as_deref(),
        &exe_dir,
    );
    // ⚠ **NOT scratch: never `<project>/tmp`, a `ScratchDir` or system temp.** A bundle is evidence
    // read after its producer is gone; a drop-deleting `ScratchDir` or a newest-N-pruned root
    // (`crates/vike-model/src/paths/state_path.rs`'s `PROJECT_TMP_DIR`) would destroy it, and a
    // fixed system-temp name dies with a container restart and is owned forever by whichever user
    // created it first. It lands beside the logs, in `<project>/settings/state` from the project
    // walk — not off `$VIKE_LOG_DIR`'s parent, which may be anywhere; `<exe_dir>` is the last resort.
    let out = out.unwrap_or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_model::paths::state_path::project_state_dir(&cwd))
            .unwrap_or_else(|| exe_dir.clone())
            .join(INCIDENTS_SUBDIR)
    });

    Ok(Parsed::Args(Args { since, out, journal_dir, log_dir, profile }))
}

fn main() -> ExitCode {
    // Console only: file logging OFF so the collector never writes into the logs it freezes.
    let _guards = vike_log::init(vike_log::LogConfig {
        file_enabled: false,
        file_prefix: "vike-incident".to_string(),
        ..Default::default()
    });

    let args = match parse_args() {
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Args(a)) => a,
        Err(e) => {
            eprintln!("arg error: {e}");
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let cfg = IncidentConfig {
        since: args.since,
        out_root: args.out,
        journal_dir: args.journal_dir,
        log_dir: Some(args.log_dir),
        profile_path: args.profile,
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
            Ok(Parsed::Args(a)) => a,
            other => panic!("expected a run from {v:?}, got {other:?}"),
        }
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
            let e = parse_args_from(args(&["--since", bad]))
                .expect_err("a bad interval must not resolve to zero");
            assert!(e.contains("--since"), "the error names the flag for {bad:?}: {e}");
        }
        let missing = parse_args_from(args(&[])).expect_err("--since is required");
        assert!(missing.contains("--since"), "{missing}");
        let trailing = parse_args_from(args(&["--since"])).expect_err("a trailing --since");
        assert!(trailing.contains("--since"), "{trailing}");
    }

    /// ONE leading `incident` token is dropped; a second is an unknown argument.
    #[test]
    fn one_leading_incident_token_is_dropped_and_a_second_is_not() {
        let a = run_args(&["incident", "--since", "30m", "--out", "/tmp/b"]);
        assert_eq!(a.since, Duration::from_millis(1_800_000));
        let e = parse_args_from(args(&["incident", "incident", "--since", "30m"]))
            .expect_err("a second one is not a flag");
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
            "--profile",
            "run.toml",
        ]);
        assert_eq!(a.out, PathBuf::from("/var/vike/incidents"));
        assert_eq!(a.journal_dir, Some(PathBuf::from("/data/journal")));
        assert_eq!(a.profile, Some(PathBuf::from("run.toml")), "--profile beats VIKE_RUN_PROFILE");
    }

    /// A typo'd flag is REJECTED and the message quotes it.
    #[test]
    fn an_unknown_flag_is_rejected_and_named() {
        let e = parse_args_from(args(&["--since", "1h", "--outt", "/tmp/b"]))
            .expect_err("a typo must not run");
        assert!(e.contains("--outt"), "{e}");
    }

    /// A valued flag may not swallow a following flag (`--out --profile` once wrote the bundle into
    /// `./--profile`): a usage error naming BOTH tokens, refused before any [`Parsed`] exists.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        let a = parse_args_from(args(&["--since", "1h", "--out", "--profile"]))
            .expect_err("the bundle may not land in `./--profile`");
        assert!(a.contains("--out") && a.contains("--profile"), "both tokens are named: {a}");

        let b = parse_args_from(args(&["--since", "1h", "--profile", "--out"]))
            .expect_err("the mirror case is refused too");
        assert!(b.contains("--profile") && b.contains("--out"), "{b}");

        // `--since` too: no window from a flag name.
        let c = parse_args_from(args(&["--since", "--out", "/tmp/b"]))
            .expect_err("a flag is not an interval");
        assert!(c.contains("--since") && c.contains("--out"), "{c}");

        // `--`, not `-`: `-1m` is taken as a value and refused by `--since`'s OWN interval check.
        let d = parse_args_from(args(&["--since", "-1m"])).expect_err("not an interval");
        assert!(d.contains("--since"), "{d}");
    }

    /// `--help` in a VALUE slot is a swallow (see [`flag_value`]); in FLAG position it still
    /// short-circuits ([`help_short_circuits_to_a_success_even_without_since`]).
    #[test]
    fn a_help_in_value_position_is_a_swallow_while_one_in_flag_position_still_succeeds() {
        let e = parse_args_from(args(&["--out", "--help"])).expect_err("a value slot, not a plea");
        assert!(e.contains("--out") && e.contains("--help"), "both tokens are named: {e}");
        // …and the two orders that DO reach the short-circuit.
        assert!(matches!(parse_args_from(args(&["--help", "--out"])), Ok(Parsed::Help)));
        assert!(matches!(parse_args_from(args(&["--out", "/tmp/b", "--help"])), Ok(Parsed::Help)));
    }

    /// The LAST spelling of a repeated flag wins (an appended override from shell history applies).
    #[test]
    fn a_repeated_flag_takes_the_last_value() {
        let a = run_args(&["--since", "1h", "--since", "30s", "--out", "/tmp/b"]);
        assert_eq!(a.since, Duration::from_secs(30));
    }

    /// `-h`/`--help` short-circuit to `Parsed::Help` (exit 0), even without `--since` and after
    /// other flags, matching `vike_tradehub`'s pin.
    #[test]
    fn help_short_circuits_to_a_success_even_without_since() {
        for flag in ["-h", "--help"] {
            assert!(matches!(parse_args_from(args(&[flag])), Ok(Parsed::Help)), "{flag}");
        }
        assert!(matches!(parse_args_from(args(&["--since", "1h", "--help"])), Ok(Parsed::Help)));
    }
}

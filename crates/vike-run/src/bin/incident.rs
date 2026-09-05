//! `incident --since <dur>` — freeze the perishable evidence of a live run into ONE timestamped
//! bundle. A brand-new, feature-free subcommand (no venue/native deps), so it builds in the
//! default/CI lane like the offline mount test.
//!
//! ## What it collects (see [`vike_run::incident`])
//! Into `incident-<UTC>/` (a per-file-gz directory + a plain `manifest.json`):
//!   - journal segment files ([`vike_core::journal`]) whose last-write overlaps the window,
//!   - the trace-log slice from `$VIKE_LOG_DIR` (or `<exe>/logs`) overlapping the window,
//!   - the resolved `RunProfile` TOML (`--profile` / `VIKE_RUN_PROFILE`) + its FNV-1a64 hash,
//!   - the latest [`vike_exec::EngineSnapshot`] + `state_hash` from the journal's last `Snap`,
//!   - git SHA / build info, and a REDACTED environment dump (secrets masked like the signers'
//!     `Debug` impls).
//!
//! ## Usage
//! ```sh
//! # look back two hours; sources resolved from the same env a live node uses
//! cargo run -p vike-run --bin incident -- --since 2h
//! # `vike-run incident …` also works — a leading `incident` token is accepted and skipped
//! cargo run -p vike-run --bin incident -- incident --since 30m --out /var/vike/incidents
//! # pin the journal / profile explicitly
//! cargo run -p vike-run --bin incident -- --since 1d \
//!     --journal-dir data/journal --profile run.toml
//! ```
//! `--since` takes a vike interval (`30s`, `15m`, `2h`, `1d`). The bundle directory path is printed
//! on stdout. Defaults: `--out` → `<temp>/vike-incidents`; journal dir → `--journal-dir` /
//! `VIKE_JOURNAL_DIR` / the loaded profile's journal sink; log dir → `--log-dir` / `VIKE_LOG_DIR` /
//! `<exe>/logs`; profile → `--profile` / `VIKE_RUN_PROFILE`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use vike_model::state_path::INCIDENTS_SUBDIR;
use vike_run::incident::{IncidentConfig, run_incident};

/// `Debug` so a parse that was supposed to FAIL can report what it produced instead
/// (`Result::expect_err` requires it).
#[derive(Debug)]
struct Args {
    since: Duration,
    out: PathBuf,
    journal_dir: Option<PathBuf>,
    log_dir: PathBuf,
    profile: Option<PathBuf>,
}

/// What a successful parse produced. `-h`/`--help` is NOT an error — it used to fall into this
/// parser's `other` arm (`unknown flag "--help"`) and exit **1**, the same defect class
/// `vike_tradehub`'s `Parsed::Help` was introduced to fix (a non-zero `--help` breaks `set -e` and
/// any wrapper that checks a status). Same shape here, followed rather than re-derived.
#[derive(Debug)]
enum Parsed {
    Args(Args),
    Help,
}

/// The usage line, shared between the `--help` success path and the error path below.
const USAGE: &str = "usage: incident --since <dur> [--out <dir>] [--journal-dir <dir>] \
     [--log-dir <dir>] [--profile <run.toml>]";

/// The process-argv entry point: [`parse_args_from`] over `std::env::args().skip(1)`. The SOURCE of
/// argv is the only thing this wrapper decides — every rule, and every environment read the
/// resolution below performs, stays in the function it always lived in.
fn parse_args() -> Result<Parsed, String> {
    parse_args_from(std::env::args().skip(1))
}

/// **THE RULE: a valued flag must be GIVEN a value, and a token beginning with `--` is a FLAG,
/// never a value.** The same rule `vike_backfill::cli::flag_value` spells for the backfill bins;
/// this crate cannot depend on that one (nothing may — it pulls every bridge crate), so the
/// spelling is repeated rather than shared.
///
/// It is `--`, not a bare `-`: a negative number is a real value in this workspace's parsers, and
/// a `starts_with('-')` test would turn every signed field into a usage error. Nothing this bin
/// takes legitimately begins with `--`; a bundle directory that genuinely did is reachable as
/// `./--weird`.
///
/// What it closes here: `--out --profile` used to parse CLEANLY and write the post-mortem evidence
/// bundle into a directory literally named `--profile`, and `--profile --out` put the eaten flag in
/// the profile path while leaving `--out` at its default. A collector whose evidence lands
/// somewhere nobody looks is the same as no collector.
///
/// ⚠ **It never fires on `-h`/`--help`, and the one place the two rules MEET is a value slot.**
/// `--help` takes no value, so it never reaches this function; but `--out --help` does, and it is
/// refused as a swallow rather than short-circuiting to [`Parsed::Help`] — the `--out` arm matches
/// the token in FLAG position first and asks for its value. That is the intended order: printing
/// usage and exiting 0 would silently discard the `--out` the operator typed, which is the exact
/// shape of failure this rule exists to end. They still see the usage — `main` prints it on the
/// error path — plus a line naming both tokens. `--help` FIRST, or after a fed flag
/// (`--since 1h --help`), still short-circuits to a success.
fn flag_value(flag: &str, next: Option<String>) -> Result<String, String> {
    match next {
        None => Err(format!("{flag} needs a value")),
        Some(v) if v.starts_with("--") => {
            Err(format!("{flag} needs a value, but the next argument is another flag ({v})"))
        }
        Some(v) => Ok(v),
    }
}

/// Parse an already-`argv[0]`-stripped argument stream, then resolve the sources the way a live node
/// does.
///
/// Every valued flag resolves through [`flag_value`], so a flag that was MENTIONED and not fed is
/// an error — a missing value, or a value that is itself a flag. An unmentioned flag still keeps
/// its default.
///
/// ⚠ Only the ARGV is injected. The resolution half still reads `VIKE_RUN_PROFILE`,
/// `VIKE_JOURNAL_DIR`, `VIKE_LOG_DIR`, the current exe and the working directory — deliberately, per
/// the settings-registry rule that these reads belong in a binary. So the fields a flag does NOT
/// pin are AMBIENT: a test may only assert on `since` and on the flags it supplied explicitly
/// (`--out` and `--journal-dir` win outright; `--log-dir` LOSES to `$VIKE_LOG_DIR`).
fn parse_args_from(argv: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut argv: Vec<String> = argv.collect();
    // Accept (and drop) a leading `incident` subcommand token, so `vike-run incident --since …`
    // reads the same as `--bin incident -- --since …`.
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

    // Resolve the sources from the environment the way a live node does (mirrors
    // vike_core::journal_config_from_env / vike_log's log-dir precedence). The `--profile` vs
    // `VIKE_RUN_PROFILE` precedence itself is the shared rule in `vike_core::run_profile`
    // (`resolve_profile_path` / `resolve_profile`) — read here, not re-derived, so this bin can
    // never silently diverge from that resolver.
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
    // The SAME four-layer precedence a live node writes with — read here, never re-derived, so the
    // collector cannot look in a directory nothing writes to. The project layer is what a node
    // actually uses today (`<project>/settings/state/logs`); before it existed this resolved to
    // `<exe_dir>/logs` and would now miss every log it was built to collect.
    let log_dir = vike_log::resolve_log_dir(
        std::env::var("VIKE_LOG_DIR").ok().as_deref(),
        log_dir_flag.as_deref(),
        std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_model::state_path::project_log_dir(&cwd))
            .as_deref(),
        &exe_dir,
    );
    // ⚠ **This is NOT scratch, and it deliberately does not go to `<project>/tmp`.** A bundle is
    // frozen post-mortem evidence: it is written precisely so it can be read after the thing that
    // produced it is gone, so an owned `ScratchDir` (which deletes on drop) or a swept scratch root
    // (which prunes to a bounded newest-N) would each destroy the only artifact this binary exists
    // to make. `crates/vike-model/src/state_path.rs`'s `PROJECT_TMP_DIR` is for the opposite
    // lifecycle — re-derivable by definition.
    //
    // It defaulted to a FIXED name under the system temp directory, which is wrong for the same two
    // reasons everything else in this sweep was: inside the container this project is moving to that
    // directory is not the host's and does not survive a restart — so the evidence would be gone by
    // the time anybody went looking — and on a box where CI and agents run as different users,
    // whichever created `/tmp/vike-incidents` first owned it permanently.
    //
    // So it lands beside the LOGS it freezes, in `<project>/settings/state`, derived from the same
    // project the four-layer log precedence above just resolved. That is the point of reusing the
    // walk rather than the resolved `log_dir`: `$VIKE_LOG_DIR` may name a directory anywhere
    // (`/var/log/vike`), and hanging bundles off its PARENT would scatter them wherever that
    // variable pointed. `<exe_dir>` is the last resort, exactly as it is for the log directory, for
    // a binary with no project above its working directory.
    let out = out.unwrap_or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_model::state_path::project_state_dir(&cwd))
            .unwrap_or_else(|| exe_dir.clone())
            .join(INCIDENTS_SUBDIR)
    });

    Ok(Parsed::Args(Args { since, out, journal_dir, log_dir, profile }))
}

fn main() -> ExitCode {
    // One-shot CLI: console logging only. File logging is OFF so the collector never writes into the
    // very log directory it is about to freeze.
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
            // Result path on stdout (raw — the CLAUDE.md logging convention for protocol/result).
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

    /// The parsed `Args`, or a panic naming what came back instead — every happy-path case here
    /// expects a run rather than help.
    fn run_args(v: &[&str]) -> Args {
        match parse_args_from(args(v)) {
            Ok(Parsed::Args(a)) => a,
            other => panic!("expected a run from {v:?}, got {other:?}"),
        }
    }

    /// `--since` is the one required flag, and it takes a vike INTERVAL rather than a bare number —
    /// so every unit this collector documents must actually resolve.
    #[test]
    fn since_accepts_every_documented_unit() {
        for (spelled, want_ms) in
            [("30s", 30_000u64), ("15m", 900_000), ("2h", 7_200_000), ("1d", 86_400_000)]
        {
            let a = run_args(&["--since", spelled, "--out", "/tmp/b"]);
            assert_eq!(a.since, Duration::from_millis(want_ms), "--since {spelled}");
        }
    }

    /// A malformed `--since` is an ERROR rather than a zero-length window that would freeze an empty
    /// bundle and read as "there was no evidence".
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

    /// The `vike-run incident …` spelling: ONE leading `incident` token is accepted and dropped, and
    /// a SECOND one is not — it lands in the flag loop as an unknown argument rather than being
    /// swallowed.
    #[test]
    fn one_leading_incident_token_is_dropped_and_a_second_is_not() {
        let a = run_args(&["incident", "--since", "30m", "--out", "/tmp/b"]);
        assert_eq!(a.since, Duration::from_millis(1_800_000));
        let e = parse_args_from(args(&["incident", "incident", "--since", "30m"]))
            .expect_err("a second one is not a flag");
        assert!(e.contains("incident"), "{e}");
    }

    /// The two flags a test may assert on: `--out` short-circuits its default outright and an
    /// explicit `--journal-dir` beats `$VIKE_JOURNAL_DIR`, so both are deterministic wherever the
    /// harness runs.
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

    /// A typo'd flag is REJECTED, not ignored — and the message quotes the offending token, so a
    /// stray shell expansion is visible rather than being read as a path.
    #[test]
    fn an_unknown_flag_is_rejected_and_named() {
        let e = parse_args_from(args(&["--since", "1h", "--outt", "/tmp/b"]))
            .expect_err("a typo must not run");
        assert!(e.contains("--outt"), "{e}");
    }

    /// **A FINDING, now refused.** A valued flag used to consume WHATEVER token followed, including
    /// another flag. `--out --profile` parsed CLEANLY and would have written the post-mortem
    /// evidence bundle into a directory literally named `--profile`; `--profile --out` put the
    /// swallowed flag in the profile path and left `--out` at its default. Nothing in this parser
    /// distinguished a value from a flag, so the failure was silent by construction.
    ///
    /// Both are now usage errors naming BOTH tokens. The old accepting behaviour is asserted GONE,
    /// which is the half that keeps a future edit from reintroducing it — note the `expect_err` on
    /// `parse_args_from` itself rather than an unwrap through [`Parsed`]: the refusal happens
    /// BEFORE any `Parsed` is produced, so there is nothing to unwrap.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        let a = parse_args_from(args(&["--since", "1h", "--out", "--profile"]))
            .expect_err("the bundle may not land in `./--profile`");
        assert!(a.contains("--out") && a.contains("--profile"), "both tokens are named: {a}");

        let b = parse_args_from(args(&["--since", "1h", "--profile", "--out"]))
            .expect_err("the mirror case is refused too");
        assert!(b.contains("--profile") && b.contains("--out"), "{b}");

        // `--since` is the required flag, and it is refused the same way rather than resolving a
        // window from a flag name.
        let c = parse_args_from(args(&["--since", "--out", "/tmp/b"]))
            .expect_err("a flag is not an interval");
        assert!(c.contains("--since") && c.contains("--out"), "{c}");

        // The rule is `--`, not `-`: a single-dash token is still taken as a value. `-1m` is not a
        // valid interval, so it is `--since`'s OWN check that refuses it — by name, as before.
        let d = parse_args_from(args(&["--since", "-1m"])).expect_err("not an interval");
        assert!(d.contains("--since"), "{d}");
    }

    /// **The interaction between this rule and the `--help` short-circuit**, which the two fixes
    /// that landed together both have an opinion about. `--help` takes no value, so the swallow
    /// rule can never fire on it — but a `--help` sitting in a VALUE slot reaches the valued flag's
    /// arm first, and is refused as a swallow rather than printing usage and exiting 0. That is the
    /// deliberate order: a success there would silently discard the `--out` the operator typed.
    /// `--help` in FLAG position — first, or after a fed flag — still short-circuits, which is the
    /// property [`help_short_circuits_to_a_success_even_without_since`] owns.
    #[test]
    fn a_help_in_value_position_is_a_swallow_while_one_in_flag_position_still_succeeds() {
        let e = parse_args_from(args(&["--out", "--help"])).expect_err("a value slot, not a plea");
        assert!(e.contains("--out") && e.contains("--help"), "both tokens are named: {e}");
        // …and the two orders that DO reach the short-circuit, unaffected by the rule.
        assert!(matches!(parse_args_from(args(&["--help", "--out"])), Ok(Parsed::Help)));
        assert!(matches!(parse_args_from(args(&["--out", "/tmp/b", "--help"])), Ok(Parsed::Help)));
    }

    /// The LAST spelling of a repeated flag wins, silently. Pinned because an operator who reruns a
    /// shell-history line with an appended override gets the appended one — which is the useful
    /// direction, and is worth knowing is the direction.
    #[test]
    fn a_repeated_flag_takes_the_last_value() {
        let a = run_args(&["--since", "1h", "--since", "30s", "--out", "/tmp/b"]);
        assert_eq!(a.since, Duration::from_secs(30));
    }

    /// **FIXED.** `-h`/`--help` used to fall into the `other` arm (`unknown flag "--help"`) and exit
    /// **1** — the same defect class `vike_tradehub`'s `Parsed::Help` was introduced to fix (a
    /// non-zero `--help` breaks `set -e` and any wrapper that checks a status). Both spellings now
    /// short-circuit to `Parsed::Help`, even before `--since` — the one required flag — has been
    /// supplied, and even after other flags, matching `vike_tradehub`'s own pin.
    #[test]
    fn help_short_circuits_to_a_success_even_without_since() {
        for flag in ["-h", "--help"] {
            assert!(matches!(parse_args_from(args(&[flag])), Ok(Parsed::Help)), "{flag}");
        }
        assert!(matches!(parse_args_from(args(&["--since", "1h", "--help"])), Ok(Parsed::Help)));
    }
}

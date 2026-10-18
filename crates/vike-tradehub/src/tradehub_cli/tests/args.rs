//! `read_args`: the live daemon's whole argument surface.

use super::*;
use crate::tradehub_cli::args::PROFILE_FLAG_REMOVED;
use vike_util::cli::{CliExit, USAGE_EXIT};

// ---------------------------------------------------------------------------------------------
// `read_args` — the LIVE DAEMON's whole argument surface.
//
// Everything below is the SAME reader the shipped daemon runs (`vike_util::cli::Program` over the
// `Args` derive); only the argv SOURCE is injected, so the daemon's command line is drivable from a
// test without a process. The exit status and the stream each outcome earns are asserted over the
// shipped binary in `tests/daemon/help_and_log_dir.rs`.
// ---------------------------------------------------------------------------------------------

/// An argv tail as `read_args` takes it — already `argv[0]`-stripped.
fn argv(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

/// The parsed `Args`, or a panic naming what came back instead — every happy-path case here
/// expects a run rather than help or a version.
fn run_args(v: &[&str]) -> Args {
    match read_args(&argv(v)) {
        Ok(a) => a,
        Err(exit) => panic!("expected a run from {v:?}, got {exit:?}"),
    }
}

/// The refusal a command line earns: a usage error, exit 2, on stderr.
fn refusal(v: &[&str]) -> CliExit {
    match read_args(&argv(v)) {
        Ok(a) => panic!("{v:?} must be refused, got a run: {a:?}"),
        Err(exit) => {
            assert_eq!(exit.code, USAGE_EXIT, "{v:?}: a usage error exits 2: {exit:?}");
            assert!(!exit.to_stdout, "{v:?}: a usage error is stderr: {exit:?}");
            exit
        }
    }
}

/// ⚠ **`--config` is RETIRED (0086) and OPTIONAL**: it no longer names anything this binary
/// reads — the ACTIVE daemon-profile ROW does — so a bare invocation must parse cleanly. `run` is
/// what refuses a bare invocation now, and only when no active daemon-profile row exists either;
/// the PARSER's job ends at "did this argv make sense", which a `--config`-less one always does.
///
/// This test used to be named for the opposite claim (`--config` REQUIRED, its absence refused
/// here) — the exact regression a reader tracing this history should see stated rather than
/// silently swept.
#[test]
fn config_is_optional_and_retired() {
    let bare = run_args(&[]);
    assert_eq!(bare.config_path, None, "a bare invocation must parse — nothing is required");

    // A GIVEN `--config` still parses — and its value is carried, so `run` can warn that it was
    // ignored — it is simply never required.
    assert_eq!(run_args(&["--config", "d.toml"]).config_path.as_deref(), Some("d.toml"));
}

/// The flag in BOTH spellings — `--config value` and `--config=value`. The `=` form is what a
/// systemd `ExecStart=` line tends to carry.
#[test]
fn config_parses_in_both_spellings() {
    for spelling in [&["--config", "d.toml"][..], &["--config=d.toml"][..]] {
        let a = run_args(spelling);
        assert_eq!(a.config_path.as_deref(), Some("d.toml"), "{spelling:?}");
    }
}

/// **`--profile` is REFUSED BY NAME (decision 0111)**, in every spelling, given a value or not:
/// the run profile is the ACTIVE `run` row, and no profile FILE is read. Refused rather than
/// ignored because the file it named held the pre-trade ceilings a live mount's orders are judged
/// against — an operator must learn it stopped deciding anything, and the message names the
/// command that writes the row instead.
#[test]
fn profile_is_refused_by_name_in_every_spelling() {
    for line in [
        &["--profile", "run.toml"][..],
        &["--profile=run.toml"][..],
        &["--config", "d.toml", "--profile", "run.toml"][..],
        &["--profile"][..],
    ] {
        let e = refusal(line).text;
        assert!(e.contains(PROFILE_FLAG_REMOVED), "{line:?}: {e}");
        assert!(e.contains("vike-cli config bootstrap-run"), "names the writer: {e}");
        assert!(!e.contains("run.toml"), "the file named is not echoed as if it mattered: {e}");
    }
    // A BLANK value is refused for every flag before this program sees it, so `--profile=` (an
    // unset shell variable in an `ExecStart=`) is still refused and still names the flag, with the
    // shared blank-value wording rather than the removal sentence.
    let blank = refusal(&["--profile="]).text;
    assert!(blank.contains("--profile"), "{blank}");
}

/// A value containing `=` survives BOTH spellings — the split is at the FIRST `=` only, so a path
/// like `/etc/vike/a=b.toml` is not truncated either way. Worth pinning because it is the one place
/// the `=` spelling could silently corrupt an operator's path.
#[test]
fn a_value_containing_an_equals_sign_is_not_truncated() {
    assert_eq!(
        run_args(&["--config", "/etc/a=b.toml"]).config_path.as_deref(),
        Some("/etc/a=b.toml")
    );
    assert_eq!(run_args(&["--config=/etc/a=b.toml"]).config_path.as_deref(), Some("/etc/a=b.toml"));
}

/// **`--help` and `--version` are SUCCESSES, not errors** — the regression the old `Parsed` enum
/// existed to prevent (a non-zero `--help` breaks `set -e`, packaging smoke tests and every
/// wrapper that checks a status): exit 0 on STDOUT. Both spellings of each, and `--help` reached
/// AFTER other arguments, which is how a person actually asks for it.
#[test]
fn help_and_version_are_outcomes_not_errors() {
    let outcome = |v: &[&str]| match read_args(&argv(v)) {
        Err(exit) => exit,
        Ok(a) => panic!("{v:?} is a question, not a run: {a:?}"),
    };
    for flag in ["-h", "--help"] {
        let exit = outcome(&[flag]);
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{flag}: {exit:?}");
        assert!(exit.text.contains("Usage:"), "{flag}: {}", exit.text);
        assert!(exit.text.contains("--allow-public-bind"), "{flag}: {}", exit.text);
    }
    for flag in ["-V", "--version"] {
        let exit = outcome(&[flag]);
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{flag}: {exit:?}");
        assert!(exit.text.starts_with("vike-tradehub "), "name and version lead: {}", exit.text);
    }
    // Asked for after a --config: help still wins.
    let exit = outcome(&["--config", "d.toml", "--help"]);
    assert_eq!((exit.code, exit.to_stdout), (0, true), "{exit:?}");
    // `-V`, never `-v`: lowercase `-v` is verbosity everywhere else on the box, so it stays an
    // unknown argument here rather than silently printing a version.
    refusal(&["-v"]);
}

/// A typo'd flag is REJECTED and NAMED, not ignored — including the `=` spelling of one, which
/// is the form a `systemd` unit line takes. A stray POSITIONAL is refused too, and is never echoed.
#[test]
fn an_unknown_argument_is_rejected_and_named() {
    for bad in ["--conf", "--config-path", "--Config", "-c"] {
        let e = refusal(&[bad, "d.toml"]).text;
        assert!(e.contains(bad), "the error names the offending argument: {e}");
    }
    let inline = refusal(&["--conf=d.toml"]).text;
    assert!(inline.contains("--conf"), "{inline}");
    assert!(!inline.contains("d.toml"), "…and only the flag, never its value: {inline}");
    let stray = refusal(&["d.toml"]).text;
    assert!(!stray.contains("d.toml"), "a positional is never echoed: {stray}");
}

/// A trailing valued flag is an ERROR rather than a silently-defaulted one.
#[test]
fn a_trailing_valued_flag_is_an_error() {
    let e = refusal(&["--config"]).text;
    assert!(e.contains("--config"), "{e}");
    // …and one with a value already given still errors on the trailing one (a repeat, too).
    let e = refusal(&["--config", "d.toml", "--config"]).text;
    assert!(e.contains("--config"), "{e}");
}

/// **A FINDING, now refused — and the point is that the two SPELLINGS agree.** `--config=` with
/// nothing after the `=` used to be ACCEPTED and to yield an EMPTY config path. A `systemd` unit
/// whose `ExecStart` interpolates an unset shell variable produces exactly that line
/// (`--config=$VIKE_CONFIG`, or `--config "$VIKE_CONFIG"` with quotes). Every way of writing "no
/// value" is the same refusal, and each error names the flag: no value at all, an empty inline
/// value, an empty quoted argument and a whitespace-only one.
#[test]
fn an_empty_value_is_refused_in_both_spellings_exactly_like_a_missing_one() {
    for line in
        [&["--config"][..], &["--config="][..], &["--config", ""][..], &["--config", "   "][..]]
    {
        let e = refusal(line).text;
        assert!(e.contains("--config"), "the error names the flag for {line:?}: {e}");
    }
}

/// **A repeated flag is REFUSED** (decision 0155 rule 3). It used to take the LAST value,
/// silently — so a unit file that gained a second `--config` line (a merge, an override drop-in)
/// ran the daemon on the second profile with nothing said about the first.
#[test]
fn a_repeated_flag_is_refused() {
    for line in [
        &["--config", "a.toml", "--config", "b.toml"][..],
        &["--config=a.toml", "--config=b.toml"][..],
        &["--allow-public-bind", "--allow-public-bind"][..],
    ] {
        refusal(line);
    }
}

/// **A flag in the value slot is no value.** In the SPACE spelling a valued flag used to consume
/// whatever followed, including another flag: `--config --profile r.toml` yielded the config path
/// `"--profile"`. Now the unfed flag is named and the flag is left alone; the `=` spelling states
/// its value outright, so `--config=--profile` is a path with an odd name, not a swallowed flag.
#[test]
fn a_flag_is_never_swallowed_as_a_value() {
    let e = refusal(&["--config", "--profile", "r.toml"]).text;
    assert!(e.contains("--config"), "the unfed flag is named: {e}");
    assert!(!e.contains("r.toml"), "…and the message is not about the value: {e}");
    refusal(&["--config", "--allow-public-bind"]);
    assert_eq!(run_args(&["--config=--profile"]).config_path.as_deref(), Some("--profile"));
}

/// **`--allow-public-bind` is a SWITCH** (decision 0111: the container's deployment shape is an
/// argument, not a variable). Absent it is `false`; given, `true`; an inline value is not a
/// spelling of it and is refused rather than read.
#[test]
fn allow_public_bind_is_a_switch() {
    assert!(!run_args(&[]).allow_public_bind, "absent = no consent");
    assert!(run_args(&["--allow-public-bind"]).allow_public_bind);
    assert!(run_args(&["--config", "d.toml", "--allow-public-bind"]).allow_public_bind);
    let e = refusal(&["--allow-public-bind=1"]).text;
    assert!(e.contains("--allow-public-bind"), "{e}");
}

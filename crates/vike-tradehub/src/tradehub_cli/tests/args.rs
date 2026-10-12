//! `parse_args_from`: the live daemon's whole argument surface.

use super::*;
use std::assert_matches;

// ---------------------------------------------------------------------------------------------
// `parse_args_from` — the LIVE DAEMON's whole argument surface.
//
// It had no test at all until this section, because `parse_args` read `std::env::args()` and so
// could not be driven. Everything below is the SAME parser the shipped daemon runs; only the
// argv SOURCE is injected.
// ---------------------------------------------------------------------------------------------

/// An argv stream as `parse_args_from` takes it — already `argv[0]`-stripped.
fn argv(v: &[&str]) -> std::vec::IntoIter<String> {
    v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>().into_iter()
}

/// The parsed `Args`, or a panic naming what came back instead — every happy-path case here
/// expects a run rather than help or a version.
fn run_args(v: &[&str]) -> Args {
    match parse_args_from(argv(v)) {
        Ok(Parsed::Args(a)) => a,
        other => panic!("expected a run from {v:?}, got {other:?}"),
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
/// systemd `ExecStart=` line tends to carry, and this is the only parser in this sweep that
/// accepts it.
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
        &["--profile="][..],
    ] {
        let e = parse_args_from(argv(line)).expect_err("no profile file is read");
        assert!(e.contains("--profile is REMOVED"), "{line:?}: {e}");
        assert!(e.contains("vike-cli config bootstrap-run"), "names the writer: {e}");
        assert!(!e.contains("run.toml"), "the file named is not echoed as if it mattered: {e}");
    }
}

/// A value containing `=` survives BOTH spellings — `split_once` takes the FIRST `=` only, so a
/// path like `/etc/vike/a=b.toml` is not truncated either way. Worth pinning because it is the
/// one place the `=` spelling could silently corrupt an operator's path.
#[test]
fn a_value_containing_an_equals_sign_is_not_truncated() {
    assert_eq!(
        run_args(&["--config", "/etc/a=b.toml"]).config_path.as_deref(),
        Some("/etc/a=b.toml")
    );
    assert_eq!(run_args(&["--config=/etc/a=b.toml"]).config_path.as_deref(), Some("/etc/a=b.toml"));
}

/// **`--help` and `--version` are SUCCESSES, not errors** — the regression this file's `Parsed`
/// enum exists to prevent (a non-zero `--help` breaks `set -e`, packaging smoke tests and every
/// wrapper that checks a status). Both spellings of each, and `--help` reached AFTER other
/// arguments, which is how a person actually asks for it.
#[test]
fn help_and_version_are_outcomes_not_errors() {
    for flag in ["-h", "--help"] {
        assert_matches!(parse_args_from(argv(&[flag])), Ok(Parsed::Help), "{flag}");
    }
    for flag in ["-V", "--version"] {
        assert_matches!(parse_args_from(argv(&[flag])), Ok(Parsed::Version), "{flag}");
    }
    // Asked for after a --config: help still wins, and this is no longer proving anything about
    // a required-flag check — there is none any more — only that `--help` short-circuits
    // regardless of what came before it.
    assert_matches!(parse_args_from(argv(&["--config", "d.toml", "--help"])), Ok(Parsed::Help));
    assert_matches!(parse_args_from(argv(&["--help", "--config"])), Ok(Parsed::Help));
    // `-V`, never `-v`: lowercase `-v` is verbosity everywhere else on the box, so it stays an
    // unknown argument here rather than silently printing a version.
    assert!(parse_args_from(argv(&["-v"])).is_err(), "-v must not be --version");
}

/// A typo'd flag is REJECTED and NAMED, not ignored — including the `=` spelling of one, which
/// is the form a `systemd` unit line takes.
#[test]
fn an_unknown_argument_is_rejected_and_named() {
    for bad in ["--conf", "--config-path", "--Config", "-c", "d.toml"] {
        let e = parse_args_from(argv(&[bad, "d.toml"])).expect_err("a typo must not run");
        assert!(e.contains(bad), "the error names the offending argument: {e}");
    }
    let inline = parse_args_from(argv(&["--conf=d.toml"])).expect_err("the = spelling too");
    assert!(inline.contains("--conf"), "{inline}");
}

/// A trailing valued flag is an ERROR rather than a silently-defaulted one — the hole every
/// `vike-backfill` bin has and this daemon does not, because the arm `ok_or`s instead of letting
/// `it.next()`'s `None` fall through.
#[test]
fn a_trailing_valued_flag_is_an_error() {
    let e = parse_args_from(argv(&["--config"])).expect_err("a trailing valued flag");
    assert!(e.contains("--config"), "{e}");
    // …and one with a value already given still errors on the trailing one.
    let e = parse_args_from(argv(&["--config", "d.toml", "--config"])).expect_err("trailing");
    assert!(e.contains("--config"), "{e}");
}

/// **A FINDING, now refused — and the point is that the two SPELLINGS agree.** `--config=` with
/// nothing after the `=` used to be ACCEPTED and to yield an EMPTY config path: the inline
/// branch took `split_once`'s right half verbatim and no arm checked it for emptiness. A
/// `systemd` unit whose `ExecStart` interpolates an unset shell variable produces exactly that
/// line. The daemon then failed opening `""`, so it did not trade on a default — but the
/// required-flag check it was supposed to trip had already passed, and the diagnostic an
/// operator got was a file-open error rather than "you gave no --config". The space spelling
/// did NOT have the hole, so the same flag behaved differently depending on how it was written.
///
/// All four ways of writing "no value" are now the same refusal, and each error names the flag:
/// no value at all, an empty inline value, an empty quoted argument (`--config ""`, the OTHER
/// shape an unset `$VIKE_CONFIG` takes), and a whitespace-only one.
#[test]
fn an_empty_value_is_refused_in_both_spellings_exactly_like_a_missing_one() {
    for line in
        [&["--config"][..], &["--config="][..], &["--config", ""][..], &["--config", "   "][..]]
    {
        let e = parse_args_from(argv(line)).expect_err("an empty config path is no config path");
        assert!(e.contains("--config"), "the error names the flag for {line:?}: {e}");
    }
}

/// **A FINDING, pinned.** A repeated flag takes the LAST value, silently — so a unit file that
/// gained a second `--config` line (a merge, an override drop-in) runs the daemon on the second
/// profile with nothing said about the first. Pinned because the direction is what an operator
/// appending an override depends on, and because "silently" is the part worth knowing.
#[test]
fn a_repeated_flag_takes_the_last_value_silently() {
    assert_eq!(
        run_args(&["--config", "a.toml", "--config", "b.toml"]).config_path.as_deref(),
        Some("b.toml")
    );
    assert_eq!(
        run_args(&["--config=a.toml", "--config=b.toml"]).config_path.as_deref(),
        Some("b.toml")
    );
}

/// **A FINDING, now refused — and the diagnostic is about the right token.** In the SPACE
/// spelling a valued flag used to consume whatever followed, including another flag:
/// `--config --profile r.toml` yielded the config path `"--profile"` and then died on `r.toml`
/// as an unknown argument, so the message named a token the operator had written correctly.
/// The TRAILING case had nothing left to trip over and parsed CLEANLY with a config path of
/// `--profile`, after which the daemon failed opening a file by that name.
///
/// Both now name the unfed flag AND the flag that would have been eaten, and neither mentions
/// the innocent trailing value.
#[test]
fn a_swallowed_flag_is_refused_and_the_diagnostic_names_both_flags() {
    let e = parse_args_from(argv(&["--config", "--profile", "r.toml"]))
        .expect_err("a flag is not a config path");
    assert!(e.contains("--config") && e.contains("--profile"), "both are named: {e}");
    assert!(!e.contains("r.toml"), "…and the message is no longer about the value: {e}");
    // The trailing case, which used to parse cleanly.
    let tail =
        parse_args_from(argv(&["--config", "--profile"])).expect_err("no longer a config path");
    assert!(tail.contains("--config") && tail.contains("--profile"), "{tail}");
    // The `=` spelling of the same mistake is refused too, so neither form has a hole.
    let inline = parse_args_from(argv(&["--config=--profile"])).expect_err("inline too");
    assert!(inline.contains("--config") && inline.contains("--profile"), "{inline}");
}

/// **`--allow-public-bind` is a SWITCH** (decision 0111: the container's deployment shape is an
/// argument, not a variable). Absent it is `false`; given, `true`; an inline value is not a
/// spelling of it and is refused as an unknown argument rather than read.
#[test]
fn allow_public_bind_is_a_switch() {
    assert!(!run_args(&[]).allow_public_bind, "absent = no consent");
    assert!(run_args(&["--allow-public-bind"]).allow_public_bind);
    assert!(run_args(&["--config", "d.toml", "--allow-public-bind"]).allow_public_bind);
    let e = parse_args_from(argv(&["--allow-public-bind=1"])).expect_err("a switch takes no value");
    assert!(e.contains("--allow-public-bind"), "{e}");
}

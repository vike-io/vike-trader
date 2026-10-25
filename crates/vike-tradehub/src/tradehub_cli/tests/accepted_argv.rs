//! Every command line the trading daemon ACCEPTS today, pinned as POSITIVE cases (decision 0155:
//! "everything an operator could WRITE and have accepted before stays accepted").
//!
//! The case TABLE is the pin and is written against the pre-clap parser first; only
//! [`outcome`], the adapter from the parser's result to a comparable value, changes when the reader
//! does. Each row names where the line comes from.

use super::*;

/// What a command line meant to the daemon: a run with these two values, or one of the two
/// questions that answer and exit 0.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Run { config: Option<String>, allow_public_bind: bool },
    Help,
    Version,
}

/// The adapter: `None` is a REFUSED command line.
fn outcome(argv: &[&str]) -> Option<Outcome> {
    let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
    match read_args(&argv) {
        Ok(a) => {
            Some(Outcome::Run { config: a.config_path, allow_public_bind: a.allow_public_bind })
        }
        // `-h`/`-V` answer on stdout with exit 0; the help page is the only one with a `Usage:`.
        Err(exit) if exit.code == 0 && exit.to_stdout => {
            Some(if exit.text.contains("Usage:") { Outcome::Help } else { Outcome::Version })
        }
        Err(_) => None,
    }
}

fn run(config: Option<&str>, allow_public_bind: bool) -> Option<Outcome> {
    Some(Outcome::Run { config: config.map(str::to_string), allow_public_bind })
}

#[test]
fn every_accepted_command_line_stays_accepted() {
    let cases: Vec<(&str, Vec<&str>, Option<Outcome>)> = vec![
        // deploy/vike-tradehub.service: `ExecStart=.../vike-backend trade`.
        ("the shipped unit", vec![], run(None, false)),
        // deploy/docker/entrypoint.sh: `start_child "$CHILD_TRADE" --allow-public-bind "$@"`.
        ("the image entrypoint", vec!["--allow-public-bind"], run(None, true)),
        // deploy/docker/entrypoint_selftest.sh: the container's own arguments follow the image's.
        (
            "the entrypoint self-test",
            vec!["--allow-public-bind", "--config", "p6.toml"],
            Some(Outcome::Run { config: Some("p6.toml".into()), allow_public_bind: true }),
        ),
        // The RETIRED `--config` (decision 0086) is still accepted and carried, in both spellings.
        ("retired --config", vec!["--config", "d.toml"], run(Some("d.toml"), false)),
        ("retired --config=", vec!["--config=d.toml"], run(Some("d.toml"), false)),
        (
            "a path with a space",
            vec!["--config", "with space.toml"],
            run(Some("with space.toml"), false),
        ),
        ("a path holding =", vec!["--config", "/etc/a=b.toml"], run(Some("/etc/a=b.toml"), false)),
        (
            "an inline path holding =",
            vec!["--config=/etc/a=b.toml"],
            run(Some("/etc/a=b.toml"), false),
        ),
        (
            "flags in either order",
            vec!["--config", "d.toml", "--allow-public-bind"],
            Some(Outcome::Run { config: Some("d.toml".into()), allow_public_bind: true }),
        ),
        // The questions: `vike-backend trade --version` is asserted by release.yml's identity step.
        ("-h", vec!["-h"], Some(Outcome::Help)),
        ("--help", vec!["--help"], Some(Outcome::Help)),
        ("-V", vec!["-V"], Some(Outcome::Version)),
        ("--version", vec!["--version"], Some(Outcome::Version)),
        ("help after a flag", vec!["--config", "d.toml", "--help"], Some(Outcome::Help)),
        ("help before a dangling flag", vec!["--help", "--config"], Some(Outcome::Help)),
        ("version after a flag", vec!["--allow-public-bind", "--version"], Some(Outcome::Version)),
    ];
    for (what, argv, want) in cases {
        assert_eq!(outcome(&argv), want, "{what}: {argv:?}");
    }
}

/// The negative half the positive table must not blur: a line the daemon REFUSED still is.
#[test]
fn a_refused_command_line_stays_refused() {
    for argv in [
        &["--bogus"][..],
        &["-v"][..],
        &["--config"][..],
        &["--config="][..],
        &["--config", ""][..],
        &["--profile", "run.toml"][..],
        &["--profile"][..],
        &["--allow-public-bind=1"][..],
        &["stray"][..],
    ] {
        assert_eq!(outcome(argv), None, "{argv:?}");
    }
}

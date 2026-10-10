use super::*;

fn parse_of(argv: &[&str]) -> Result<Args, String> {
    parse(argv.iter().map(|s| (*s).to_string()))
}

#[test]
fn init_parses_with_and_without_the_dry_run() {
    let a = parse_of(&["init"]).unwrap();
    assert_eq!(a.sub, Sub::Init);
    assert!(!a.dry_run, "the default is the REAL run — a flag turns it into a rehearsal");
    assert!(parse_of(&["init", "--dry-run"]).unwrap().dry_run);
}

/// **`--file` is refused on `init`, and the message names the flag.**
///
/// The inspecting sense of the flag has no counterpart here: this verb resolves a settings
/// DIRECTORY, and the nearest reading of a file path would create a credential database beside
/// an arbitrary one. `parse`'s own note carries the argument.
#[test]
fn file_is_refused_on_init() {
    let err = parse_of(&["init", "--file", "/tmp/a.env"]).unwrap_err();
    assert!(err.contains("--file"), "{err}");
    assert!(err.contains("removed"), "the flag is refused as REMOVED, by name: {err}");
}

/// `--dry-run` is refused on `list`, `path` and `set` rather than ignored. The expensive instance
/// of the class is `set --dry-run`: a dropped flag there writes a credential the operator believed
/// was a rehearsal.
#[test]
fn dry_run_is_refused_on_every_other_subcommand() {
    for sub in ["list", "path"] {
        let err = parse_of(&[sub, "--dry-run"]).unwrap_err();
        assert!(err.contains("--dry-run"), "{sub}: {err}");
    }
    let err = parse_of(&["set", "BINANCE_LIVE_API_KEY", "--dry-run"]).unwrap_err();
    assert!(err.contains("--dry-run"), "{err}");
}

/// A stray positional on `init` is an `unknown option`, not a silently ignored argument — the
/// positional arm belongs to `set` alone.
#[test]
fn init_takes_no_positional() {
    let err = parse_of(&["init", "BINANCE_LIVE_API_KEY"]).unwrap_err();
    assert!(err.contains("unknown option"), "{err}");
}

/// The subcommand is named in the "a subcommand is required" message too — the one an operator
/// who typed `vike-cli secrets` alone reads — and USAGE documents it, its rehearsal, and the two
/// facts an operator must carry: the store is EMPTY, and from then on it answers for every
/// credential.
#[test]
fn usage_and_the_bare_error_both_name_init() {
    assert!(USAGE.contains("vike-cli secrets init"), "USAGE must document the creator");
    assert!(USAGE.contains("--dry-run"), "USAGE must document the rehearsal");
    assert!(USAGE.contains("EMPTY store"), "…and that the store it creates is empty");
    assert!(USAGE.contains("answers for every"), "…and that it answers for every credential");
    assert!(parse_of(&[]).unwrap_err().contains("init"));
}

use super::*;

fn parse_of(argv: &[&str]) -> Result<Args, String> {
    parse(argv.iter().map(|s| (*s).to_string()))
}

#[test]
fn migrate_parses_with_and_without_the_dry_run() {
    let a = parse_of(&["migrate"]).unwrap();
    assert_eq!(a.sub, Sub::Migrate);
    assert!(!a.dry_run, "the default is the REAL run — a flag turns it into a rehearsal");
    assert!(parse_of(&["migrate", "--dry-run"]).unwrap().dry_run);
}

/// **`--file` is refused on `migrate`, and the message names the flag.**
///
/// The inspecting sense of the flag has no counterpart here: this verb resolves a settings
/// DIRECTORY, and the nearest reading of a file path would create a credential database beside
/// an arbitrary one. `parse`'s own note carries the argument.
#[test]
fn file_is_refused_on_migrate() {
    let err = parse_of(&["migrate", "--file", "/tmp/a.env"]).unwrap_err();
    assert!(err.contains("--file"), "{err}");
    assert!(err.contains("removed"), "the flag is refused as REMOVED, by name: {err}");
}

/// `--dry-run` is refused off `migrate` rather than ignored. The expensive instance of the class
/// is `set --dry-run`: a dropped flag there writes a credential the operator believed was a
/// rehearsal.
#[test]
fn dry_run_is_refused_on_every_other_subcommand() {
    for sub in ["list", "path"] {
        let err = parse_of(&[sub, "--dry-run"]).unwrap_err();
        assert!(err.contains("--dry-run"), "{sub}: {err}");
    }
    let err = parse_of(&["set", "BINANCE_LIVE_API_KEY", "--dry-run"]).unwrap_err();
    assert!(err.contains("--dry-run"), "{err}");
}

/// A stray positional on `migrate` is an `unknown option`, not a silently ignored argument —
/// the positional arm belongs to `set` alone.
#[test]
fn migrate_takes_no_positional() {
    let err = parse_of(&["migrate", "BINANCE_LIVE_API_KEY"]).unwrap_err();
    assert!(err.contains("unknown option"), "{err}");
}

/// The subcommand is named in the "a subcommand is required" message too — the one an operator
/// who typed `vike-cli secrets` alone reads.
#[test]
fn usage_and_the_bare_error_both_name_migrate() {
    assert!(USAGE.contains("migrate"), "USAGE must document the migrator");
    assert!(USAGE.contains("--dry-run"), "USAGE must document the rehearsal");
    assert!(parse_of(&[]).unwrap_err().contains("migrate"));
}

/// **`--init` parses on `migrate`, alone and beside `--dry-run`, and is off by default.**
///
/// The default matters most: a plain `migrate` on an empty box must keep creating NOTHING, so the
/// flag is the operator's explicit act and never something the parser assumes.
#[test]
fn init_parses_on_migrate_and_is_off_by_default() {
    let plain = parse_of(&["migrate"]).unwrap();
    assert!(!plain.init, "plain `migrate` must not ask for an empty store");
    let init = parse_of(&["migrate", "--init"]).unwrap();
    assert_eq!(init.sub, Sub::Migrate);
    assert!(init.init && !init.dry_run);
    let both = parse_of(&["migrate", "--init", "--dry-run"]).unwrap();
    assert!(both.init && both.dry_run, "the rehearsal of `--init` is `--init --dry-run`");
    let other_order = parse_of(&["migrate", "--dry-run", "--init"]).unwrap();
    assert!(other_order.init && other_order.dry_run);
}

/// **`--init` is refused off `migrate`, and refuses a value.** A dropped `--init` would leave an
/// operator believing a store was created when nothing was; a `--init=x` has not decided what it
/// is asking.
#[test]
fn init_is_refused_off_migrate_and_takes_no_value() {
    for argv in [
        &["list", "--init"][..],
        &["path", "--init"][..],
        &["accounts", "--init"][..],
        &["confirm", "--init"][..],
        &["move-venue-config", "--init"][..],
        &["set-book", "--id", "1", "--venue-account-id", "2", "--init"][..],
        &["account", "add", "--init"][..],
    ] {
        let err = parse_of(argv).unwrap_err();
        assert!(err.contains("--init"), "{argv:?}: {err}");
    }
    assert!(parse_of(&["migrate", "--init=yes"]).is_err(), "`--init` is a switch, not a value");
}

/// USAGE documents `--init` beside the verb it belongs to, and says the two facts an operator must
/// carry: the store is EMPTY, and the credential files stop being read.
#[test]
fn usage_documents_init() {
    assert!(USAGE.contains("--init"), "USAGE must document --init");
    assert!(USAGE.contains("vike-cli secrets migrate --init"), "…with its spelling");
    assert!(USAGE.contains("EMPTY store"), "…and that the store it creates is empty");
    assert!(USAGE.contains("never read"), "…and that the credential files stop being read");
}

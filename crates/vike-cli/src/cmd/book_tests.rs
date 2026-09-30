use super::*;

fn parse_of(argv: &[&str]) -> Result<Args, String> {
    parse(argv.iter().map(|s| (*s).to_string()))
}

/// The accepted form parses, and neither value is a positional.
#[test]
fn set_book_takes_two_named_flags() {
    let a = parse_of(&["set-book", "--id", "7", "--venue-account-id", "1234567"]).unwrap();
    assert_eq!(a.sub, Sub::SetBook);
    assert_eq!(a.account_id, Some(7));
    assert_eq!(a.venue_account_id.as_deref(), Some("1234567"));
    assert!(!a.replace, "the default REFUSES an overwrite");
    assert!(!a.dry_run);
    // …and the inline spelling of each, which `Flags::next_flag` splits on the first `=`.
    let a = parse_of(&["set-book", "--id=7", "--venue-account-id=0xabc"]).unwrap();
    assert_eq!(a.account_id, Some(7));
    assert_eq!(a.venue_account_id.as_deref(), Some("0xabc"));
}

/// **Neither value may be omitted, and the message says WHICH is missing.**
///
/// A verb that defaulted either half would be guessing about which broker an order routes to.
#[test]
fn set_book_needs_both_values_and_names_the_missing_one() {
    let err = parse_of(&["set-book"]).unwrap_err();
    assert!(err.contains("--id and one of --venue-account-id / --clear"), "{err}");
    let err = parse_of(&["set-book", "--venue-account-id", "1234567"]).unwrap_err();
    assert!(err.contains("--id"), "{err}");
    assert!(!err.contains("--id and"), "only the missing flag should be named: {err}");
    let err = parse_of(&["set-book", "--id", "7"]).unwrap_err();
    assert!(err.contains("--venue-account-id"), "{err}");
}

/// **`--clear` is the BOOK half**, so it is the one form where `--venue-account-id` may be
/// absent — and it still needs an `--id`, because a clear names a row like every other write.
#[test]
fn clear_supplies_the_book_half_and_still_needs_a_row() {
    let a = parse_of(&["set-book", "--id", "3", "--clear"]).unwrap();
    assert_eq!(a.sub, Sub::SetBook);
    assert_eq!(a.account_id, Some(3));
    assert!(a.clear);
    assert_eq!(a.venue_account_id, None);
    assert!(!a.replace, "a clear needs no permission to overwrite");

    let err = parse_of(&["set-book", "--clear"]).unwrap_err();
    assert!(err.contains("--id"), "{err}");
}

/// **The two VALUE flags are mutually exclusive, and so are `--clear` and `--replace`.**
///
/// `--clear` reaches the store as `None`, so a library-level check could not tell *clear this
/// row* from *clear it AND set it to X* — it would silently honour one. And `--replace` is
/// permission to overwrite a KNOWN book with a DIFFERENT one, which a clear does not do:
/// dropping it quietly is how somebody comes to believe a stronger act ran than the one that
/// did. Both refusals say NOTHING WAS WRITTEN, because nothing was.
#[test]
fn clear_refuses_a_value_and_refuses_replace() {
    let err = parse_of(&["set-book", "--id", "3", "--clear", "--venue-account-id", "1234567"])
        .unwrap_err();
    assert!(err.contains("--venue-account-id OR --clear"), "{err}");
    assert!(err.contains("Nothing was written"), "{err}");

    let err = parse_of(&["set-book", "--id", "3", "--clear", "--replace"]).unwrap_err();
    assert!(err.contains("--replace"), "{err}");
    assert!(err.contains("Nothing was written"), "{err}");
}

/// A non-integer `--id` is refused, and the refusal names the verb that prints the real ids.
#[test]
fn a_non_integer_id_is_refused_and_points_at_the_listing() {
    let err = parse_of(&["set-book", "--id", "dukascopy", "--venue-account-id", "1"]).unwrap_err();
    assert!(err.contains("secrets accounts"), "{err}");
}

/// **Every `set-book` flag is refused off the verb**, the same rule the rest of this parser
/// holds: a flag the operator typed and the program dropped is how somebody comes to believe a
/// write was aimed somewhere it was not. `--replace` is the expensive one — typed on another
/// verb it reads as permission that was granted and never asked for.
#[test]
fn the_book_flags_are_refused_off_set_book() {
    for argv in [
        &["list", "--id", "7"][..],
        &["migrate", "--replace"][..],
        &["accounts", "--replace"][..],
        &["list", "--venue-account-id", "1234567"][..],
        // ⚠ `set` included, and it is the case worth pinning. These flags are recognised by the
        // loop on EVERY subcommand, so on `set` they do NOT reach `ARGV_VALUE_REFUSAL` — they
        // are refused by name here instead. That is correct (a named flag is not a stray
        // token), and it is why the `--id` arm's own parse failure quotes nothing: it would
        // otherwise be a second way to print a token on the one verb where an unrecognised one
        // is most likely the secret.
        &["set", "BINANCE_LIVE_API_KEY", "--id", "7"][..],
        &["set", "BINANCE_LIVE_API_KEY", "--replace"][..],
        // ⚠ `--clear` joins the same rule the day it exists, rather than the day somebody
        // notices. On `migrate` it would read as permission to wipe something.
        &["list", "--clear"][..],
        &["migrate", "--clear"][..],
        &["accounts", "--clear"][..],
        &["set", "BINANCE_LIVE_API_KEY", "--clear"][..],
    ] {
        let err = parse_of(argv).unwrap_err();
        // ⚠ The needle is `` `set-book` `` and NOT `` `set-book` only ``, and the word that
        // dropped out is the whole of what changed: `--id` now addresses an `account` row as
        // well as a book, so its refusal reads *applies to `set-book` and `account` only*
        // while every other flag here still reads *`set-book` only*. Keying on the OWNING
        // verb keeps this test asking the question it was written to ask — was the flag
        // refused, and does the refusal say where it belongs — rather than pinning a
        // sentence that is now true of only some of these rows.
        assert!(err.contains("`set-book`"), "{argv:?} must be refused: {err}");
        assert!(
            err.contains("only"),
            "{argv:?}: the refusal must still name the verb set this flag belongs to, so an \
                 operator is not left guessing where to retype it: {err}"
        );
    }
}

/// **A non-integer `--id` quotes NOTHING**, on every subcommand — including `set`, where the
/// token most likely to be typed by accident is the credential itself.
#[test]
fn a_bad_id_never_echoes_the_token_on_any_subcommand() {
    for verb in [&["set-book"][..], &["set", "BINANCE_LIVE_API_KEY"][..], &["list"][..]] {
        let mut argv: Vec<&str> = verb.to_vec();
        argv.extend_from_slice(&["--id", "sk-live-not-an-integer"]);
        let err = parse_of(&argv).unwrap_err();
        assert!(!err.contains("sk-live-not-an-integer"), "{verb:?} echoed the token back: {err}");
    }
}

/// `--dry-run` now applies to TWO verbs and to no others — and the message says both, so an
/// operator who typed it on `list` is not told it belongs to `migrate` alone.
#[test]
fn dry_run_applies_to_migrate_and_set_book_only() {
    assert!(
        parse_of(&["set-book", "--id", "1", "--venue-account-id", "x", "--dry-run"])
            .unwrap()
            .dry_run
    );
    assert!(parse_of(&["migrate", "--dry-run"]).unwrap().dry_run);
    let err = parse_of(&["list", "--dry-run"]).unwrap_err();
    assert!(err.contains("migrate") && err.contains("set-book"), "{err}");
}

/// **`--file` is refused on BOTH new subcommands, for two different reasons**, and each message
/// carries its own — `set-book` because a write's destination is never operator-supplied,
/// `accounts` because the flag names a FILE and the table lives in the DATABASE.
#[test]
fn file_is_refused_on_both_account_subcommands() {
    let err =
        parse_of(&["set-book", "--id", "1", "--venue-account-id", "x", "--file", "/tmp/a.env"])
            .unwrap_err();
    assert!(err.contains("set-book"), "{err}");
    assert!(err.contains("WRITE"), "the refusal must say why: {err}");

    let err = parse_of(&["accounts", "--file", "/tmp/a.env"]).unwrap_err();
    assert!(err.contains("account table"), "{err}");
    assert!(err.contains("DATABASE"), "{err}");
}

/// `accounts` takes no flags of its own, and the flags of its neighbours are refused on it.
#[test]
fn accounts_parses_bare() {
    assert_eq!(parse_of(&["accounts"]).unwrap().sub, Sub::Accounts);
    assert!(parse_of(&["accounts", "--json"]).unwrap_err().contains("`list` only"));
    assert!(parse_of(&["accounts", "--dry-run"]).unwrap_err().contains("set-book"));
}

/// The USAGE documents both verbs and the flags that make the writer safe — the same
/// self-check `migrate_tests` makes, and for the same reason: `crate::cmd::mcp`'s
/// `the_instructions_name_only_real_commands` holds prose to this string.
#[test]
fn the_usage_documents_the_account_verbs() {
    for needle in ["accounts", "set-book", "--venue-account-id", "--replace", "--clear", "--id"] {
        assert!(USAGE.contains(needle), "USAGE must document {needle}");
    }
    // ⚠ …and the two properties the verb is UNUSABLE without, stated where an operator reads
    // them: that `accounts` shows the credential key names (the only thing separating two rows
    // of one venue at one tier) and that an `id` is scoped to this database file.
    // ⚠ The dukascopy needle carries its `*`, and not for tidiness: a BARE
    // `"DUKASCOPY_DEMO1_"` is a whole string literal in SCREAMING_SNAKE with a known venue
    // prefix, which is exactly what `vike_ops::scan::find_map_lookups` harvests as an
    // env-variable sighting — `crates/vike-ops/tests/settings_registry.rs`'s
    // `every_read_variable_is_declared` then demands a `SETTINGS` row for a variable nothing
    // reads. The glob form says the same thing about the USAGE text and is not env-shaped.
    for needle in ["CREDENTIAL KEY NAMES", "DUKASCOPY_DEMO1_*", "stable for the life of THIS"] {
        assert!(USAGE.contains(needle), "USAGE must document {needle}");
    }
    // ⚠ …and the column whose absence was the measured failure. A writer with no reader is not
    // a fix, and a reader nobody is told about is the same defect one step later: the USAGE is
    // where an operator learns the column exists at all and that its empty state is SPELLED
    // rather than blank. [`NEVER_VERIFIED`] carries why it is not a dash.
    assert!(USAGE.contains(NEVER_VERIFIED), "USAGE must name the unverified state verbatim");
    assert!(USAGE.contains("LAST VERIFIED"), "USAGE must name the column");
    // …and that an UNMIGRATED box still hears about parked confirmations, which is the half of
    // `run_accounts` that reached nobody until it took a [`Fold`] parameter.
    assert!(USAGE.contains("PARKED"), "USAGE must say the unmigrated box still reports them");
    let err = parse_of(&[]).unwrap_err();
    assert!(err.contains("accounts") && err.contains("set-book"), "{err}");
}

// -----------------------------------------------------------------------------------------
// `confirm` — the HANDSHAKE fold's grammar
// -----------------------------------------------------------------------------------------

/// `confirm` parses, takes `--dry-run`, and takes NOTHING else — every `set-book` flag is
/// refused off it by name.
///
/// ⚠ `--replace` is the one that matters: typed on this verb it would read as permission that
/// was granted, and this verb's whole disposition is that a DISAGREEMENT is never overwritten
/// by a fold. There is no flag that makes it one, so there is nothing to accept-and-drop.
#[test]
fn confirm_takes_only_dry_run() {
    let a = parse_of(&["confirm"]).unwrap();
    assert_eq!(a.sub, Sub::Confirm);
    assert!(!a.dry_run);
    assert!(parse_of(&["confirm", "--dry-run"]).unwrap().dry_run);

    for (argv, needle) in [
        (vec!["confirm", "--replace"], "--replace"),
        (vec!["confirm", "--clear"], "--clear"),
        (vec!["confirm", "--id", "7"], "--id"),
        (vec!["confirm", "--venue-account-id", "1234567"], "--venue-account-id"),
        (vec!["confirm", "--venue", "dukascopy"], "--venue"),
    ] {
        let err = parse_of(&argv).unwrap_err();
        assert!(err.contains(needle), "{argv:?} must be refused by name: {err}");
    }
}

/// `--file` is refused on `confirm`, and the message says why the flag has no honest reading
/// here: BOTH ends of this verb — the parked file it reads and the database it writes — are
/// resolved from one settings directory, which `$VIKE_SETTINGS_DIR` moves together.
#[test]
fn confirm_refuses_a_named_file() {
    let err = parse_of(&["confirm", "--file", "somewhere.env"]).unwrap_err();
    assert!(err.contains("confirm"), "the refusal must name the verb: {err}");
    assert!(err.contains("VIKE_SETTINGS_DIR"), "…and the thing to use instead: {err}");
}

/// The USAGE documents the verb and the three dispositions, because `crate::cmd::mcp`'s
/// `the_instructions_name_only_real_commands` holds prose to this string and because the
/// DISAGREEMENT rule is the one an operator must not learn from a stack trace.
#[test]
fn the_usage_documents_the_confirm_verb() {
    for needle in [
        "confirm",
        "account-confirmations.json",
        "DISAGREES",
        "last_verified_at",
        "sandbox cannot write the settings database",
    ] {
        assert!(USAGE.contains(needle), "USAGE must document {needle}");
    }
    let err = parse_of(&[]).unwrap_err();
    assert!(err.contains("confirm"), "the subcommand roster must name it: {err}");
}

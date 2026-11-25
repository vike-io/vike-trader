use super::account::ls::NEVER_VERIFIED;
use super::*;

fn parse_of(argv: &[&str]) -> Result<Args, args::Stop> {
    parse(argv.iter().map(|s| (*s).to_string()))
}

/// The `account` plane's parser, over the same [`Args`].
fn account_of(argv: &[&str]) -> Result<Args, args::Stop> {
    parse_account(argv.iter().map(|s| (*s).to_string()))
}

/// The accepted form parses, and neither value is a positional.
#[test]
fn set_book_takes_two_named_flags() {
    let a = account_of(&["set-book", "--id", "7", "--venue-account-id", "1234567"]).unwrap();
    assert_eq!(a.sub, Sub::SetBook);
    assert_eq!(a.account_id, Some(7));
    assert_eq!(a.venue_account_id.as_deref(), Some("1234567"));
    assert!(!a.replace, "the default REFUSES an overwrite");
    assert!(!a.dry_run);
    // …and the inline spelling of each, which `Flags::next_flag` splits on the first `=`.
    let a = account_of(&["set-book", "--id=7", "--venue-account-id=0xabc"]).unwrap();
    assert_eq!(a.account_id, Some(7));
    assert_eq!(a.venue_account_id.as_deref(), Some("0xabc"));
}

/// **Neither value may be omitted, and the message says WHICH is missing.**
///
/// A verb that defaulted either half would be guessing about which broker an order routes to.
#[test]
fn set_book_needs_both_values_and_names_the_missing_one() {
    let err = account_of(&["set-book"]).unwrap_err();
    assert!(err.contains("--id and one of --venue-account-id / --clear"), "{err}");
    let err = account_of(&["set-book", "--venue-account-id", "1234567"]).unwrap_err();
    assert!(
        err.contains("`account set-book` needs --id. It writes ONE account row's"),
        "only the missing flag should be named: {err}"
    );
    let err = account_of(&["set-book", "--id", "7"]).unwrap_err();
    assert!(err.contains("--venue-account-id"), "{err}");
}

/// **`--clear` is the BOOK half**, so it is the one form where `--venue-account-id` may be
/// absent — and it still needs an `--id`, because a clear names a row like every other write.
#[test]
fn clear_supplies_the_book_half_and_still_needs_a_row() {
    let a = account_of(&["set-book", "--id", "3", "--clear"]).unwrap();
    assert_eq!(a.sub, Sub::SetBook);
    assert_eq!(a.account_id, Some(3));
    assert!(a.clear);
    assert_eq!(a.venue_account_id, None);
    assert!(!a.replace, "a clear needs no permission to overwrite");

    let err = account_of(&["set-book", "--clear"]).unwrap_err();
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
    let err = account_of(&["set-book", "--id", "3", "--clear", "--venue-account-id", "1234567"])
        .unwrap_err();
    assert!(err.contains("--venue-account-id OR --clear"), "{err}");
    assert!(err.contains("Nothing was written"), "{err}");

    let err = account_of(&["set-book", "--id", "3", "--clear", "--replace"]).unwrap_err();
    assert!(err.contains("--replace"), "{err}");
    assert!(err.contains("Nothing was written"), "{err}");
}

/// A non-integer `--id` is refused, and the refusal names the verb that prints the real ids.
#[test]
fn a_non_integer_id_is_refused_and_points_at_the_listing() {
    let err =
        account_of(&["set-book", "--id", "dukascopy", "--venue-account-id", "1"]).unwrap_err();
    assert!(err.contains("account ls"), "{err}");
}

/// **Every `set-book` flag is refused off the verb**, the same rule the rest of this parser
/// holds: a flag the operator typed and the program dropped is how somebody comes to believe a
/// write was aimed somewhere it was not. `--replace` is the expensive one — typed on another
/// verb it reads as permission that was granted and never asked for.
#[test]
fn the_book_flags_are_refused_off_set_book() {
    type Plane = fn(&[&str]) -> Result<Args, args::Stop>;
    for (plane, argv, needle) in [
        (parse_of as Plane, &["ls", "--id", "7"][..], "--id applies to `secrets set` and the"),
        (parse_of, &["init", "--replace"][..], "`account set-book`"),
        (account_of, &["ls", "--replace"][..], "`account set-book`"),
        (parse_of, &["ls", "--venue-account-id", "1234567"][..], "`account set-book` only"),
        (parse_of, &["set", "ACME_LIVE_API_KEY", "--replace"][..], "`account set-book`"),
        // ⚠ `--clear` joins the same rule the day it exists, rather than the day somebody
        // notices. On `init` it would read as permission to wipe something.
        (parse_of, &["ls", "--clear"][..], "`account set-book` only"),
        (parse_of, &["init", "--clear"][..], "`account set-book` only"),
        (account_of, &["ls", "--clear"][..], "`account set-book` only"),
        (parse_of, &["set", "ACME_LIVE_API_KEY", "--clear"][..], "`account set-book` only"),
    ] {
        let err = plane(argv).unwrap_err();
        // The needle names the OWNING verb, so the refusal says where the flag belongs and an
        // operator is not left guessing where to retype it.
        assert!(err.contains(needle), "{argv:?} must be refused naming its verb: {err}");
    }
    // ⚠ `set --id` is no longer one of them: ONE flag names a row on every verb, and on `set` it
    // is the row the credential is filed against.
    let set = parse_of(&["set", "ACME_LIVE_API_KEY", "--id", "7"]).unwrap();
    assert_eq!((set.account, set.account_id), (Some(7), None));
}

/// **A non-integer `--id` quotes NOTHING**, on every subcommand — including `set`, where the
/// token most likely to be typed by accident is the credential itself.
#[test]
fn a_bad_id_never_echoes_the_token_on_any_subcommand() {
    type Plane = fn(&[&str]) -> Result<Args, args::Stop>;
    for (plane, verb) in [
        (account_of as Plane, &["set-book"][..]),
        (parse_of, &["set", "ACME_LIVE_API_KEY"][..]),
        (parse_of, &["ls"][..]),
    ] {
        let mut argv: Vec<&str> = verb.to_vec();
        argv.extend_from_slice(&["--id", "sk-live-not-an-integer"]);
        let err = plane(&argv).unwrap_err();
        // secret-absent: the credential-shaped token the test planted is never echoed
        assert!(!err.contains("sk-live-not-an-integer"), "{verb:?} echoed the token back: {err}");
    }
    // …and the RETIRED `set --account` quotes nothing either — on `set` the token most likely to
    // land in a mistyped flag value is the credential itself — and names its new spelling.
    let err = parse_of(&["set", "API_KEY", "--account", "sk-live-not-an-integer"]).unwrap_err();
    // secret-absent: the credential-shaped token the test planted is never echoed
    assert!(!err.contains("sk-live-not-an-integer"), "--account echoed the token back: {err}");
    assert!(
        err.contains(&crate::refusals::RetiredSpelling::SecretsSetAccountFlag.rendered()),
        "{err}"
    );
}

/// `--dry-run` applies to the writers and to no others — and the message names them, so an
/// operator who typed it on `ls` is not told it belongs to `init` alone.
#[test]
fn dry_run_applies_to_init_and_set_book_only() {
    assert!(
        account_of(&["set-book", "--id", "1", "--venue-account-id", "x", "--dry-run"])
            .unwrap()
            .dry_run
    );
    assert!(parse_of(&["init", "--dry-run"]).unwrap().dry_run);
    let err = parse_of(&["ls", "--dry-run"]).unwrap_err();
    assert!(err.contains("secrets init") && err.contains("every `account` verb"), "{err}");
}

/// **`--file` is refused on BOTH account subcommands** — by name, as a removed flag (the credential
/// FILE store went on 2026-10-07; a write's destination was never operator-supplied anyway).
#[test]
fn file_is_refused_on_both_account_subcommands() {
    for argv in [
        &["set-book", "--id", "1", "--venue-account-id", "x", "--file", "/tmp/a.env"][..],
        &["ls", "--file", "/tmp/a.env"][..],
    ] {
        let err = account_of(argv).unwrap_err();
        assert!(err.contains("--file was removed"), "{argv:?}: {err}");
    }
}

/// `account ls` takes no flags of its own, and the flags of its neighbours are refused on it.
#[test]
fn accounts_parses_bare() {
    assert_eq!(account_of(&["ls"]).unwrap().sub, Sub::Accounts);
    assert!(account_of(&["ls", "--json"]).unwrap_err().contains("`secrets ls` only"));
    assert!(account_of(&["ls", "--dry-run"]).unwrap_err().contains("but `ls`"));
}

/// The USAGE documents both verbs and the flags that make the writer safe — the same
/// self-check `init_tests` makes, and for the same reason: `vike_mcp`'s
/// `the_mcp_instructions_name_only_real_commands` holds prose to this string.
#[test]
fn the_usage_documents_the_account_verbs() {
    for needle in ["ls", "set-book", "--venue-account-id", "--replace", "--clear", "--id"] {
        assert!(account_usage().contains(needle), "USAGE must document {needle}");
    }
    // ⚠ …and the properties the verb is UNUSABLE without, stated where an operator reads them:
    // that `account ls` shows the credential key names and the LABELS (what separates two rows of
    // one venue at one tier — decision 0156 item 6 gives such accounts labels) and that an `id`
    // is scoped to this database file.
    for needle in ["CREDENTIAL KEY NAMES", "LABELS", "stable for the life of THIS"] {
        assert!(account_usage().contains(needle), "USAGE must document {needle}");
    }
    // ⚠ …and the column whose absence was the measured failure. A writer with no reader is not
    // a fix, and a reader nobody is told about is the same defect one step later: the USAGE is
    // where an operator learns the column exists at all and that its empty state is SPELLED
    // rather than blank. [`NEVER_VERIFIED`] carries why it is not a dash.
    assert!(
        account_usage().contains(NEVER_VERIFIED),
        "USAGE must name the unverified state verbatim"
    );
    assert!(account_usage().contains("LAST VERIFIED"), "USAGE must name the column");
    // …and that an UNMIGRATED box still hears about parked confirmations, which is the half of
    // `run_accounts` that reached nobody until it took a [`Fold`] parameter.
    assert!(
        account_usage().contains("PARKED"),
        "USAGE must say the unmigrated box still reports them"
    );
    let err = account_of(&[]).unwrap_err();
    assert!(err.contains("ls") && err.contains("set-book"), "{err}");
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
    let a = account_of(&["confirm"]).unwrap();
    assert_eq!(a.sub, Sub::Confirm);
    assert!(!a.dry_run);
    assert!(account_of(&["confirm", "--dry-run"]).unwrap().dry_run);

    for (argv, needle) in [
        (vec!["confirm", "--replace"], "--replace"),
        (vec!["confirm", "--clear"], "--clear"),
        (vec!["confirm", "--id", "7"], "--id"),
        (vec!["confirm", "--venue-account-id", "1234567"], "--venue-account-id"),
        (vec!["confirm", "--venue", "dukascopy"], "--venue"),
    ] {
        let err = account_of(&argv).unwrap_err();
        assert!(err.contains(needle), "{argv:?} must be refused by name: {err}");
    }
}

/// `--file` is refused on `confirm`, and the message says why the flag has no honest reading
/// here: BOTH ends of this verb — the parked file it reads and the database it writes — are
/// resolved from one settings directory, which `$VIKE_SETTINGS_DIR` moves together.
#[test]
fn confirm_refuses_a_named_file() {
    let err = account_of(&["confirm", "--file", "somewhere.env"]).unwrap_err();
    assert!(err.contains("--file was removed"), "the refusal must name the flag: {err}");
    assert!(err.contains("VIKE_SETTINGS_DIR"), "…and the thing to use instead: {err}");
}

/// The USAGE documents the verb and the three dispositions, because `vike_mcp`'s
/// `the_mcp_instructions_name_only_real_commands` holds prose to this string and because the
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
        assert!(account_usage().contains(needle), "USAGE must document {needle}");
    }
    let err = account_of(&[]).unwrap_err();
    assert!(err.contains("confirm"), "the subcommand roster must name it: {err}");
}

/// **Every spelling the account move retired answers with its new one**, decided on the typed
/// refusal (its text), never as an unknown word — and the old words are hidden from the help.
#[test]
fn the_retired_spellings_name_the_new_ones() {
    use crate::refusals::RetiredSpelling as R;
    for (argv, spelling) in [
        (&["list"][..], R::SecretsList),
        (&["list", "--json"][..], R::SecretsList),
        (&["delete", "KEY", "--confirm", "KEY"][..], R::SecretsDelete),
        (&["accounts"][..], R::SecretsAccounts),
        (&["account", "add", "--venue", "binance", "--tier", "demo"][..], R::SecretsAccount),
        (&["account", "set-symbol", "--id", "4", "BTCUSDT"][..], R::SecretsAccount),
        (&["account", "--help"][..], R::SecretsAccount),
        (&["set-book", "--id", "7", "--venue-account-id", "1"][..], R::SecretsSetBook),
        (&["confirm", "--dry-run"][..], R::SecretsConfirm),
        (&["set", "--account", "7", "API_KEY"][..], R::SecretsSetAccountFlag),
    ] {
        let err = parse_of(argv).unwrap_err();
        assert!(!err.is_help(), "{argv:?} is a refusal, not a help page");
        assert!(err.contains(&spelling.rendered()), "{argv:?}: {err}");
    }
    let err = account_of(&["remove", "--id", "7", "--confirm", "7"]).unwrap_err();
    assert!(err.contains(&R::AccountRemove.rendered()), "{err}");
    // A secret typed after a retired word is never quoted back.
    let err = parse_of(&["delete", "KEY", "sk-live-do-not-print"]).unwrap_err();
    // secret-absent: the credential-shaped token the test planted is never echoed
    assert!(!err.contains("sk-live-do-not-print"), "{err}");
    for word in ["list", "delete", "accounts", "set-book", "confirm"] {
        assert!(
            !usage().contains(&format!("\n  {word} ")),
            "`secrets --help` must not offer `{word}`"
        );
    }
    assert!(!account_usage().contains("\n  remove "), "`account --help` must not offer `remove`");
}

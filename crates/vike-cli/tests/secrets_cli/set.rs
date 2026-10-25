//! `set`, the ONE writer, asserted over the shipped binary: streams, exit rung, rows on disk.

use super::support::{PlantedAccount, SetCase, canon, exit_code, rung};
use super::{stderr, stdout};

// ── set — the ONE writer ────────────────────────────────────────────────────────────────────────
//
// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` fixed this
// command's shape before it was built; these cases are that shape asserted over the SHIPPED binary,
// which is the only place several of the properties are visible at all (the exit RUNG, the two
// streams, and the rows on disk afterwards).
//
// Decision 0156 item 6 is the address: a credential belongs to its ACCOUNT, so an account's
// credential is written by `--account ID FIELD` (or `--venue VENUE [--label L] FIELD`) against a
// row that ALREADY exists, and a credential write never creates an account or chooses a tier.
// Plain `set NAME` writes the DEPLOYMENT's own names.
//
// ⚠ Every case drives its own temp settings directory through `$VIKE_SETTINGS_DIR`, so the store
// written is the case's own and never the developer box's, and so is the change journal that hangs
// off that same directory.

/// The DEPLOYMENT rows every `set` case starts from: comments, a blank line, a vendor key and the
/// pager token. Every row a write does not name must survive.
const SET_STORE: &str = "# vike credential store\n\
                         # hand-written, and it stays hand-written\n\
                         \n\
                         TARDIS_API_KEY=old-vendor-value\n\
                         VIKE_ALERT_TELEGRAM_TOKEN=\"quoted token\"\n";

/// …and the ACCOUNTS beside them, each created with its tier first: a binance LIVE account holding
/// the key the replace case targets, and an okx DEMO account holding a passphrase.
const SET_ACCOUNTS: &[PlantedAccount<'static>] = &[
    ("binance", "live", None, &[("API_KEY", "old-key-value")]),
    ("okx", "demo", None, &[("API_PASSPHRASE", "quoted pass")]),
];

/// Seed [`SET_STORE`] and [`SET_ACCOUNTS`]; answer `(binance id, okx id)` as strings, the shape a
/// command line takes them in.
fn seeded(c: &SetCase) -> (String, String) {
    c.store_rows(SET_STORE);
    let ids = c.seed_accounts(SET_ACCOUNTS);
    (ids[0].to_string(), ids[1].to_string())
}

/// The store's own name for an account's row — what `read_store` lists it under.
fn row(venue: &str, id: &str, field: &str) -> String {
    vike_secrets::account_credential_name(venue, id.parse().expect("an id"), field)
}

/// How many account rows the shipped binary's `accounts` listing prints.
fn account_count(c: &SetCase) -> usize {
    c.account_rows().lines().count()
}

/// **The stdin form: the field is APPENDED to the account, every other row survives, the value
/// reaches neither stream, and the exit is clean.**
///
/// The row-for-row assertion is the point. The store is the user's only copy of live venue keys,
/// and a CLI writer is a third surface that property has to hold on — reason 1 in
/// `docs/decisions/0036`.
#[test]
fn set_from_stdin_appends_the_key_and_preserves_every_other_byte() {
    let c = SetCase::new("stdin");
    let (_, okx) = seeded(&c);
    let before = c.read_store();

    let out = c.run(&["set", "--account", &okx, "API_KEY"], Some("piped-secret-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let landed = row("okx", &okx, "API_KEY");
    let after = c.read_store();
    // The store is the ORIGINAL rows plus exactly one — filed against the account.
    assert_eq!(
        after,
        canon(&format!("{before}{landed}=piped-secret-value\n")),
        "a set must add one row and touch nothing else"
    );
    // The trailing newline the shell put on the pipe is NOT part of the credential.
    assert!(after.contains(&format!("{landed}=piped-secret-value\n")), "{after}");

    // The report names the field, the account, the row and the store, and says which of the two
    // things happened.
    let text = stdout(&out);
    assert!(text.contains(&format!("API_KEY of account {okx} (okx/demo")), "{text}");
    assert!(text.contains("appended"), "{text}");
    assert!(text.contains(&landed), "{text}");
    assert!(text.contains(&c.store().display().to_string()), "{text}");

    // …and the VALUE is on neither stream. This is the assertion the whole command is shaped
    // around: a writer that echoed what it wrote would put the credential in the scrollback of
    // every session that used it.
    assert!(!text.contains("piped-secret-value"), "a VALUE reached stdout: {text}");
    assert!(!stderr(&out).contains("piped-secret-value"), "a VALUE reached stderr");
    assert_eq!(account_count(&c), 2, "a credential write creates no account");
}

/// **The `--from-env` form**, taking the value out of the map the dispatcher swept — the second and
/// last accepted source, and the one a deploy script uses.
#[test]
fn set_from_env_takes_the_named_variable() {
    let c = SetCase::new("from-env");
    let (_, okx) = seeded(&c);
    let before = c.read_store();

    let out = c.run(
        &["set", "--account", &okx, "API_SECRET", "--from-env", "VIKE_TEST_SECRET_SOURCE"],
        None,
        &[("VIKE_TEST_SECRET_SOURCE", "env-sourced-value")],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let landed = row("okx", &okx, "API_SECRET");
    assert_eq!(c.read_store(), canon(&format!("{before}{landed}=env-sourced-value\n")));
    assert!(!stdout(&out).contains("env-sourced-value"), "a VALUE reached stdout");
    assert!(!stderr(&out).contains("env-sourced-value"), "a VALUE reached stderr");

    // An UNSET variable is a usage error naming the variable, and writes nothing.
    let after = c.read_store();
    let out =
        c.run(&["set", "--account", &okx, "API_KEY", "--from-env", "VIKE_TEST_NOT_SET"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("VIKE_TEST_NOT_SET"), "{}", stderr(&out));
    assert_eq!(c.read_store(), after, "nothing may be written");
}

/// **A REPLACE changes exactly one row, in place**, and leaves the old value nowhere in the store.
#[test]
fn set_replaces_an_existing_key_in_place_and_says_so() {
    let c = SetCase::new("replace");
    let (binance, _) = seeded(&c);
    let before = c.read_store();

    let out = c.run(&["set", "--account", &binance, "API_KEY"], Some("rotated-key-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(stdout(&out).contains("replaced"), "{}", stdout(&out));

    let landed = row("binance", &binance, "API_KEY");
    let after = c.read_store();
    assert_eq!(
        after,
        canon(
            &before.replace(
                &format!("{landed}=old-key-value"),
                &format!("{landed}=rotated-key-value")
            )
        ),
        "a replace changes exactly one row"
    );
    assert!(!after.contains("old-key-value"), "the old value must not survive");
}

/// **A write to an account id no row carries is REFUSED, and it creates NO account.** The store's
/// own `NoSuchAccount` sentence, the `account add` line that creates one, the usage rung, nothing
/// written, nothing journalled — and the piped value was never read.
#[test]
fn a_write_to_a_missing_account_is_refused_and_creates_no_account() {
    let c = SetCase::new("no-such-account");
    seeded(&c);
    let before = c.read_store();

    let out = c.run(&["set", "--account", "999", "API_KEY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("holds no account with id 999"), "the store's own sentence: {err}");
    assert!(err.contains("no account was created"), "{err}");
    assert!(
        err.contains("vike-cli secrets account add --venue <VENUE> --tier <paper|demo|live>"),
        "the refusal names the act that creates an account: {err}"
    );
    assert!(!err.contains("never-written"), "{err}");
    assert_eq!(c.read_store(), before, "a refused write must write nothing");
    assert_eq!(account_count(&c), 2, "NO account row was created");
    assert!(c.journal_lines().is_empty(), "a refusal records nothing");
}

/// **`--venue VENUE [--label L] FIELD` writes the ONE active account of that address** — and a
/// venue with no such account is refused naming `account add`, creating nothing.
#[test]
fn set_by_venue_writes_the_one_active_account_and_refuses_an_absent_one() {
    let c = SetCase::new("by-venue");
    let (binance, _) = seeded(&c);

    let out = c.run(&["set", "--venue", "binance", "API_SECRET"], Some("venue-addressed\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains(&format!("API_SECRET of account {binance}")), "{text}");
    let landed = row("binance", &binance, "API_SECRET");
    assert!(c.read_store().contains(&format!("{landed}=venue-addressed\n")), "{}", c.read_store());

    let before = c.read_store();
    let out = c.run(&["set", "--venue", "bybit", "API_KEY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("no ACTIVE bybit account"), "{err}");
    assert!(err.contains("account add --venue bybit --tier <paper|demo|live> --no-label"), "{err}");
    assert_eq!(c.read_store(), before);
    assert_eq!(account_count(&c), 2, "a credential write creates no account");
}

/// **A LABELLED account is written under its OWN row, and the DEFAULT account's key is left
/// exactly as it was** — the property the old `KEY__LABEL` refusal was only ever a proxy for: the
/// suggestion list once offered the unlabelled account's key to an operator aiming at a second
/// account, and following it overwrote the credential the primary account signs with.
#[test]
fn a_labelled_account_is_written_and_the_base_key_is_untouched() {
    let c = SetCase::new("labelled-account");
    let (binance, _) = seeded(&c);
    let alt = c.added_account("binance", "live", "ALT").to_string();
    let before = c.read_store();

    let out = c.run(
        &["set", "--venue", "binance", "--label", "ALT", "API_KEY"],
        Some("alt-account-key\n"),
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    // ⚠ THE HALF THAT MATTERS, asserted as an EXACT addition: the store is what it was plus exactly
    // one row on the ALT account, so the DEFAULT account's `API_KEY` is byte-for-byte what it was.
    assert_eq!(
        c.read_store(),
        canon(&format!("{before}{}=alt-account-key\n", row("binance", &alt, "API_KEY"))),
        "the labelled account's key is added and nothing else moves"
    );
    let default_key = row("binance", &binance, "API_KEY");
    assert!(c.read_store().contains(&format!("{default_key}=old-key-value\n")));
    assert!(!stderr(&out).contains("alt-account-key"), "{}", stderr(&out));
    assert!(!stdout(&out).contains("alt-account-key"), "{}", stdout(&out));
}

/// **A FIELD the venue does not carry is refused, listing its fields; a VENUE-wide field is written
/// once, by the venue, and refused on one account.**
#[test]
fn a_field_is_bound_by_the_venues_schema() {
    let c = SetCase::new("fields");
    let (binance, _) = seeded(&c);
    let ctrader = c.added_unlabelled_account("ctrader", "demo").to_string();
    let before = c.read_store();

    let out = c.run(&["set", "--account", &binance, "LOGIN"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("'LOGIN' is not a credential field of binance"), "{err}");
    assert!(err.contains("API_KEY, API_SECRET, API_PASSPHRASE"), "{err}");

    let out = c.run(&["set", "--account", &ctrader, "CLIENT_ID"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("vike-cli secrets set --venue ctrader CLIENT_ID"), "{err}");
    assert_eq!(c.read_store(), before, "refusals write nothing");

    let out = c.run(&["set", "--venue", "ctrader", "CLIENT_ID"], Some("app-client-id\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(stdout(&out).contains("CLIENT_ID of venue ctrader"), "{}", stdout(&out));
    let landed = vike_secrets::venue_credential_name("ctrader", "CLIENT_ID");
    assert_eq!(c.read_store(), canon(&format!("{before}{landed}=app-client-id\n")));
}

/// **A VALUE IN ARGV IS A USAGE ERROR, and the refusal quotes nothing.**
///
/// Reason 2 in `docs/decisions/0036`: a CLI is the surface people SCRIPT, and a value in argv lands
/// in shell history and in `ps` output for every user on the box. The last assertion is why the
/// message is a constant rather than a `format!` — a refusal that echoed the token would write the
/// credential into the very scrollback it exists to keep it out of.
#[test]
fn a_value_in_argv_is_refused_on_the_usage_rung_and_never_echoed() {
    let c = SetCase::new("argv");
    let (binance, _) = seeded(&c);
    let before = c.read_store();

    let out =
        c.run(&["set", "--account", &binance, "API_KEY", "sk-live-never-print-me"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("may not be given on the command line"), "{err}");
    assert!(err.contains("shell history"), "the refusal must say WHY: {err}");
    assert!(err.contains("--from-env") && err.contains("stdin"), "both forms: {err}");
    assert!(!err.contains("sk-live-never-print-me"), "the refusal ECHOED the value: {err}");
    assert!(!stdout(&out).contains("sk-live-never-print-me"), "the refusal ECHOED the value");

    // …and the store is untouched.
    assert_eq!(c.read_store(), before);
}

/// **An ACCOUNT's credential NAME is not a name plain `set` writes** — refused on the usage rung,
/// pointing at `--account`, writing nothing and creating no account (decision 0156 item 6: it used
/// to mint the account the name implied). A typo of a DEPLOYMENT name is refused by name with the
/// real spelling offered.
#[test]
fn an_accounts_name_and_an_unknown_name_are_refused_and_write_nothing() {
    let c = SetCase::new("unknown-key");
    seeded(&c);
    let before = c.read_store();

    let out = c.run(&["set", "BYBIT_DEMO_API_KEY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("BYBIT_DEMO_API_KEY"), "the refusal must name the key: {err}");
    assert!(err.contains("vike-cli secrets set --account <ID> <FIELD>"), "{err}");
    assert!(err.contains("decision 0156"), "{err}");
    assert_eq!(c.read_store(), before, "a refused name must write nothing");
    assert_eq!(account_count(&c), 2, "and create no account");

    let out = c.run(&["set", "TARDIS_API_KEYY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("TARDIS_API_KEYY"), "the refusal must name the key: {err}");
    assert!(err.contains("did you mean: TARDIS_API_KEY"), "…and suggest the real one: {err}");
    assert_eq!(c.read_store(), before, "a refused key must write nothing");
    assert!(!c.read_store().contains("never-written"));
}

/// **A venue SETTING's credential-style name is refused like any name `set` does not write**, and
/// the value is not even read.
#[test]
fn a_venue_settings_credential_style_name_is_refused() {
    let c = SetCase::new("stranded-setting");
    seeded(&c);
    let before = c.read_store();

    let out = c.run(&["set", "POLY_PROXY_HOST"], Some("<host>\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("POLY_PROXY_HOST"), "the refusal names the key: {err}");
    assert!(!err.contains("<host>"), "the value was not even read: {err}");
    assert_eq!(c.read_store(), before, "a refused key must write nothing");
}

/// **A key this workspace READS is refused WITHOUT being called dead**, over the shipped binary.
///
/// `vike-cli secrets set VIKE_TRADEHUB_OBSERVE_KEY` used to answer "is not a credential key this
/// workspace reads, so setting it would write a line nothing would ever load". Measured on the CI box,
/// 2026-09-07, and false: `crates/vike-tradehub/src/hub_server.rs`'s `start_observe_server` reads
/// that exact name out of the credential map. A hub key stays refused on its own merits — it is
/// MINTED, and a hand-pasted 256-bit HMAC that is truncated fails as an opaque auth denial —
/// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` fences the
/// surface and this test must not be read as reopening it. What is asserted here is that the refusal
/// tells the truth, names a reader, and points at the route that exists today. The unit twin
/// (`the_hub_keys_refusal_names_the_command_that_mints_them`) covers the whole set of such names;
/// this one covers the two things only the real binary shows — the RUNG and the STREAM.
///
/// **A refusal names the route that exists TODAY, and never one that does not.**
#[test]
fn a_key_the_workspace_reads_is_refused_without_being_called_unloadable() {
    let c = SetCase::new("read-elsewhere");
    seeded(&c);
    let before = c.read_store();

    let out = c.run(&["set", "VIKE_TRADEHUB_OBSERVE_KEY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "the refusal must name the key: {err}");
    assert!(!err.contains("nothing would ever load"), "the measured lie is back: {err}");
    assert!(err.contains("IS read by this workspace"), "{err}");
    assert!(err.contains("backend setup"), "the operator must be given a route: {err}");
    assert!(err.contains("backend connect"), "…including the client's half: {err}");
    assert!(err.contains("vike-tradehub"), "which box, by service: {err}");
    assert!(!err.contains("EDITOR"), "the stale route is back: {err}");
    assert_eq!(c.read_store(), before, "a refused key must write nothing");
    assert!(!c.read_store().contains("never-written"));

    // ⚠ THE DATAHUB PAIR: `vike-cli datahub setup` is built; `datahub connect` is NOT.
    let out = c.run(&["set", "VIKE_DATAHUB_CONTROL_KEY"], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("VIKE_DATAHUB_CONTROL_KEY"), "{err}");
    assert!(err.contains("datahub setup"), "the datahub's own minting command: {err}");
    assert!(err.contains("vike-datahub"), "and its box: {err}");
    assert!(
        !err.contains("backend setup"),
        "it must not route a datahub key at the tradehub: {err}"
    );
    assert!(
        !err.contains("datahub connect"),
        "there is no such command — a refusal may not invent one: {err}"
    );
    assert!(!err.contains("EDITOR"), "{err}");
    assert_eq!(c.read_store(), before, "a refused key must write nothing");

    // ⚠ THE GATEWAYHUB PAIR — the third service to own hub keys: `vike-cli gatewayhub setup` mints
    // it, there is no `gatewayhub connect`, and the route for the datahub that holds the client
    // side is the database-to-database copy. The name is COMPOSED for the literal-harvest reason.
    let out = c.run(
        &["set", &format!("VIKE_{}", "GATEWAYHUB_CONTROL_KEY")],
        Some("never-written\n"),
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("IS read by this workspace"), "{err}");
    assert!(!err.contains("nothing would ever load"), "the measured lie is back: {err}");
    assert!(err.contains("gatewayhub setup"), "the gatewayhub's own minting command: {err}");
    assert!(err.contains("vike-gatewayhub"), "and its box: {err}");
    assert!(err.contains("--only gatewayhub"), "and how the datahub gets the same pair: {err}");
    assert!(
        !err.contains("backend setup") && !err.contains("datahub setup"),
        "it must not route a gatewayhub key at another service's minter: {err}"
    );
    assert!(!err.contains("backend connect") && !err.contains("datahub connect"), "{err}");
    assert!(!err.contains("EDITOR"), "{err}");
    assert_eq!(c.read_store(), before, "a refused key must write nothing");
}

/// **An ABSENT store is refused, and the refusal names the command that creates one.**
///
/// This command upserts and creates nothing: `vike-cli secrets init` is the one creator.
/// The rung is FAILED rather than USAGE — the command line was fine, the box is not configured —
/// and that distinction is the whole reason the ladder exists.
#[test]
fn an_absent_store_is_refused_and_names_migrate_init() {
    let c = SetCase::new("absent");
    // Deliberately NO store written.

    for argv in [&["set", "--account", "1", "API_KEY"][..], &["set", "TARDIS_API_KEY"][..]] {
        let out = c.run(argv, Some("never-written\n"), &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Failed), "{}", stderr(&out));
        let err = stderr(&out);
        assert!(err.contains("no credential store"), "{err}");
        assert!(err.contains("secrets init"), "…and the way to make the EMPTY store: {err}");
        assert!(err.contains(&c.store().display().to_string()), "…and which store: {err}");
        assert!(!c.store().exists(), "a refused set must CREATE no store");
    }
}

/// **The write is JOURNALLED: one `credential_write` record, carrying the row NAME, the ACCOUNT's
/// venue and tier word, and no value.** A deployment NAME's record carries no tier at all.
///
/// The value assertion is over the RAW LINE rather than over a field: a record type that grew a
/// value cell would pass a field-level check while doing exactly the thing that must be impossible.
#[test]
fn the_write_is_journalled_once_with_the_key_name_and_no_value() {
    let c = SetCase::new("journal");
    let (_, okx) = seeded(&c);

    let out = c.run(&["set", "--account", &okx, "API_KEY"], Some("journalled-secret-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let lines = c.journal_lines();
    assert_eq!(lines.len(), 1, "ONE write ⇒ ONE record: {lines:?}");
    let line = &lines[0];
    // The record keeps the store's row name whole — `okx#ID.API_KEY`, the name a reader searches.
    let landed = row("okx", &okx, "API_KEY");
    assert!(line.contains(&landed), "the record must carry the row: {line}");
    assert!(line.contains("\"okx\""), "…and the venue it belongs to: {line}");
    assert!(line.contains("\"demo\""), "…and the ACCOUNT's tier word: {line}");
    assert!(line.contains("vike.db"), "…and which store — the settings database: {line}");
    assert!(line.contains("vike-cli"), "…and that the CLI was the actor: {line}");
    assert!(!line.contains("journalled-secret-value"), "a VALUE reached the ledger: {line}");

    // A deployment NAME belongs to no account: the record says `deployment` and `untiered`.
    let out = c.run(&["set", "TARDIS_API_KEY"], Some("rotated-vendor-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let lines = c.journal_lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    let named = lines.iter().find(|l| l.contains("TARDIS_API_KEY")).expect("the name's record");
    assert!(named.contains("\"deployment\"") && named.contains("\"untiered\""), "{named}");
    assert!(!named.contains("rotated-vendor-value"), "a VALUE reached the ledger: {named}");

    // A REFUSED set records nothing — the ledger says what happened, not what was attempted.
    let out = c.run(&["set", "NOT_A_REAL_KEY"], Some("x\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert_eq!(c.journal_lines().len(), 2, "a refusal must append no record");
}

/// **No value on stdin is a usage error**, not an empty credential written over a real one.
///
/// An empty value is equivalent to an absent key — the venue stays on paper — so writing one would
/// report success for a change that arms nothing.
#[test]
fn an_empty_value_is_refused_rather_than_written() {
    let c = SetCase::new("empty");
    let (binance, _) = seeded(&c);
    let before = c.read_store();

    // Closed stdin — what a shell hands a process with no pipe.
    let out = c.run(&["set", "--account", &binance, "API_KEY"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("no value on stdin"), "{err}");
    assert!(err.contains(&format!("set --account {binance} API_KEY")), "{err}");

    // …and whitespace is not a value either.
    let out = c.run(&["set", "--account", &binance, "API_KEY"], Some("   \n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));

    assert_eq!(c.read_store(), before, "the existing credential must survive both");
}

/// **A value that BEGINS WITH A DASH is refused like any other argv value — and not echoed.**
///
/// A dash-leading token missed the positional arm's guard and fell through to the generic
/// `unknown option '{other}'`, which printed the credential verbatim to STDERR — the stream CI logs
/// and every service manager captures. base64url alphabets contain `-`, so this is an ordinary
/// credential rather than a contrived one.
#[test]
fn a_dash_leading_value_in_argv_is_refused_and_never_echoed() {
    let c = SetCase::new("argv-dash");
    let (binance, _) = seeded(&c);
    let before = c.read_store();

    for value in ["-sk-live-never-print-me", "-----BEGIN-never-print-me"] {
        let out = c.run(&["set", "--account", &binance, "API_KEY", value], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
        assert!(
            !stderr(&out).contains("never-print-me") && !stdout(&out).contains("never-print-me"),
            "the refusal ECHOED the value: {} / {}",
            stderr(&out),
            stdout(&out)
        );
    }
    assert_eq!(c.read_store(), before, "a refused value must write nothing");
}

/// **`--file` cannot aim a WRITE at a path the operator names.**
///
/// It used to resolve for `set`, so `set KEY --file ~/.bashrc` appended a live credential to a
/// shell rc file and exited 0. `docs/decisions/0036` fixes this verb as an upsert into the
/// PROJECT's store; a scripted run that must aim elsewhere moves the whole settings directory with
/// `$VIKE_SETTINGS_DIR`.
#[test]
fn set_refuses_to_write_into_a_file_named_on_the_command_line() {
    let c = SetCase::new("file-flag");
    let (binance, _) = seeded(&c);
    let before = c.read_store();
    // An ordinary, non-credential file that happens to exist — the class this defect reached.
    let bystander = c.dir().join("bashrc");
    let bystander_text = "export PATH=/usr/local/bin\n";
    std::fs::write(&bystander, bystander_text).unwrap();

    let out = c.run(
        &["set", "--account", &binance, "API_KEY", "--file", &bystander.display().to_string()],
        Some("sk-written-here\n"),
        &[],
    );
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(stderr(&out).contains("--file was removed"), "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(&bystander).unwrap(),
        bystander_text,
        "a file named on the command line must not be written"
    );
    assert_eq!(c.read_store(), before, "…and neither is the real store");
    assert!(c.journal_lines().is_empty(), "a refusal records nothing");
}

/// **A MULTI-LINE `--from-env` value is refused, and nothing is written.**
///
/// The blocker this case exists for: a multi-line value once became a physical line break in the
/// credential FILE store, read back as a SILENTLY TRUNCATED credential plus a WHOLE NEW `KEY=VALUE`
/// for a venue the operator never configured. `--from-env` is the CI/deploy-script form and a
/// multi-line secret is the ordinary shape of a Vault- or Actions-injected variable.
#[test]
fn a_multiline_env_value_cannot_inject_a_second_key() {
    let c = SetCase::new("from-env-multiline");
    let (_, okx) = seeded(&c);
    let before = c.read_store();

    for raw in ["abc\nOKX_LIVE_API_SECRET=injected-by-a-newline", "trailing-newline\n"] {
        let out = c.run(
            &["set", "--account", &okx, "API_KEY", "--from-env", "VIKE_TEST_MULTILINE"],
            None,
            &[("VIKE_TEST_MULTILINE", raw)],
        );
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
        let err = stderr(&out);
        assert!(err.contains("more than one line"), "{err}");
        assert!(err.contains("VIKE_TEST_MULTILINE"), "the refusal must name the VARIABLE: {err}");
        assert!(!err.contains("injected-by-a-newline"), "the refusal ECHOED the value: {err}");
    }

    assert_eq!(c.read_store(), before, "a refused value must write nothing at all");
    assert!(c.journal_lines().is_empty(), "…and record nothing");

    // …and the store still holds exactly what it held: no third key, no third account.
    let out = c.run(&["list"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(!stdout(&out).contains("OKX_LIVE_API_SECRET"), "a key was injected: {}", stdout(&out));
    assert_eq!(account_count(&c), 2);
}

/// **The IBKR gateway's login pair is written BY NAME — and creates NO ibkr account row.** The two
/// launchers read the pair by name; writing it used to MINT an active ibkr account at the tier the
/// name spelled (decision 0119's side effect), which is exactly what decision 0156 item 6 removed:
/// the account and its tier are `account add`'s.
#[test]
fn the_gateway_login_is_written_by_name_and_creates_no_account() {
    let c = SetCase::new("gateway-login");
    seeded(&c);
    let user = format!("IBKR_{}", "DEMO_USERNAME");
    let out = c.run(&["set", &user], Some("gateway-user-value\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    assert!(stdout(&out).contains(&format!("{user} appended")), "{}", stdout(&out));
    assert!(c.read_store().contains(&format!("{user}=gateway-user-value\n")));
    assert_eq!(account_count(&c), 2, "a credential write creates no account");
    assert!(!c.account_rows().contains("ibkr"), "no ibkr row: {}", c.account_rows());
}

// ── the CLOSED list outside the grid ─────────────────────────────────────────────────────────────

/// **Every name of `vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID` is written into a store
/// that does NOT already hold it** — the gap the list closes: before it, the pager token, the
/// collector key and their siblings reached `set` only by ROTATION, so a box with a database and
/// no copy had no writer at all. Each write is journalled by NAME, and no value reaches either
/// stream or the ledger.
#[test]
fn every_listed_name_is_appended_into_a_store_that_never_held_it() {
    let c = SetCase::new("closed-list");
    c.store_rows("# an empty-ish store\n");
    let list = vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID;
    for (i, (name, _)) in list.iter().enumerate() {
        let value = format!("closed-list-value-{i}");
        let out = c.run(&["set", name], Some(&format!("{value}\n")), &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{name}: {}", stderr(&out));
        assert!(stdout(&out).contains("appended"), "{name}: {}", stdout(&out));
        assert!(!stdout(&out).contains(&value) && !stderr(&out).contains(&value), "{name}: VALUE");
        assert!(c.read_store().contains(&format!("{name}={value}\n")), "{name} did not land");
    }
    let listed = c.run(&["list"], None, &[]);
    for (name, _) in list {
        assert!(stdout(&listed).contains(name), "`list` must show {name}");
    }
    let writes: Vec<String> =
        c.journal_lines().into_iter().filter(|l| l.contains("credential_write")).collect();
    assert_eq!(writes.len(), list.len(), "one record per write");
    for (i, ((name, _), line)) in list.iter().zip(&writes).enumerate() {
        assert!(line.contains(name), "the record must name {name}");
        assert!(!line.contains(&format!("closed-list-value-{i}")), "a VALUE reached the ledger");
    }
}

/// **The Studio chat pane's two provider keys are written into an EXISTING store that never held
/// them** — the false refusal this pins closed. The value travels on stdin (never argv), each write
/// adds one row and moves nothing else, and neither stream, the listing's values, nor the ledger
/// carries it.
#[test]
fn the_studio_chat_keys_are_appended_into_a_store_that_never_held_them() {
    let c = SetCase::new("studio-chat-keys");
    seeded(&c);
    let anthropic = concat!("ANTHROPIC", "_API_KEY");
    let cerebras = concat!("CEREBRAS", "_API_KEY");
    let mut expected = c.read_store();
    for (name, value) in [(anthropic, "sk-ant-studio-value-1"), (cerebras, "csk-studio-value-2")] {
        let out = c.run(&["set", name], Some(&format!("{value}\n")), &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{name}: {}", stderr(&out));
        assert!(stdout(&out).contains("appended"), "{name}: {}", stdout(&out));
        assert!(stdout(&out).contains(name), "the report names the key: {}", stdout(&out));
        assert!(!stdout(&out).contains(value), "{name}: a VALUE reached stdout");
        assert!(!stderr(&out).contains(value), "{name}: a VALUE reached stderr");
        expected = canon(&format!("{expected}{name}={value}\n"));
        assert_eq!(c.read_store(), expected, "{name}: one row, nothing else");
    }
    let listed = c.run(&["list"], None, &[]);
    assert!(stdout(&listed).contains(anthropic) && stdout(&listed).contains(cerebras));
    for value in ["sk-ant-studio-value-1", "csk-studio-value-2"] {
        assert!(!stdout(&listed).contains(value), "`list` printed a VALUE");
        assert!(c.journal_lines().iter().all(|l| !l.contains(value)), "a VALUE reached the ledger");
    }
}

/// **A registry name OUTSIDE the closed list is still refused, BY NAME, and writes nothing** — a
/// SETTING read off the process environment, and a typo of a listed name, which is offered the
/// real spelling.
#[test]
fn a_name_outside_the_closed_list_is_still_refused_by_name() {
    let c = SetCase::new("closed-list-out");
    seeded(&c);
    let before = c.read_store();
    let compute = format!("VIKE_{}", "STUDIO_BACKTEST_KEY");
    let out = c.run(&["set", &compute], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&compute) && stderr(&out).contains("SETTING"),
        "{}",
        stderr(&out)
    );

    let typo = format!("VIKE_{}", "ALERT_TELEGRAM_TOKN");
    let out = c.run(&["set", &typo], Some("never-written\n"), &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains(&typo), "{err}");
    assert!(err.contains(&format!("did you mean: VIKE_{}", "ALERT_TELEGRAM_TOKEN")), "{err}");

    assert_eq!(c.read_store(), before, "a refused key must write nothing");
    assert!(
        c.journal_lines().iter().all(|l| !l.contains("credential_write")),
        "a refusal journalled"
    );
}

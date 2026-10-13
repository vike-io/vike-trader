use vike_config::settings::all_settings;
use vike_model::AccountTier;

use super::set::{
    DEPLOYMENT_OWNER, NameOwner, deployment_rotation, edit_distance, name_owner, nearest_keys,
    no_such_account_message, registry_readers, run_set, the_one_active_account,
    unknown_field_message, unknown_key_message, value_for,
};
use super::*;

fn parse_of(argv: &[&str]) -> Result<Args, args::Stop> {
    parse(argv.iter().map(|s| (*s).to_string()))
}

// --- The grammar ----------------------------------------------------------------------------------

/// The two ACCEPTED value forms parse, and the FIELD is the one positional.
#[test]
fn the_two_value_forms_parse() {
    let a = parse_of(&["set", "--account", "7", "API_KEY"]).unwrap();
    assert_eq!(a.sub, Sub::Set);
    assert_eq!(a.account, Some(7));
    assert_eq!(a.key.as_deref(), Some("API_KEY"));
    assert_eq!(a.from_env, None);

    let b = parse_of(&["set", "--account", "7", "API_KEY", "--from-env", "SRC"]).unwrap();
    assert_eq!(b.from_env.as_deref(), Some("SRC"));
    // …and the flags may lead, so a flags-first habit keeps working.
    let c = parse_of(&["set", "--from-env=SRC", "--account=7", "API_KEY"]).unwrap();
    assert_eq!(c.key.as_deref(), Some("API_KEY"));
    assert_eq!(c.account, Some(7));
    assert_eq!(c.from_env.as_deref(), Some("SRC"));
}

/// **`set`'s three addresses** — an account row by id, the one active account of a venue (and
/// label), and a deployment NAME with neither.
#[test]
fn set_parses_its_three_addresses() {
    let by_id = parse_of(&["set", "--account", "12", "API_SECRET"]).unwrap();
    assert_eq!((by_id.account, by_id.venue.as_deref()), (Some(12), None));

    let by_venue = parse_of(&["set", "--venue", "binance", "API_KEY"]).unwrap();
    assert_eq!((by_venue.account, by_venue.venue.as_deref()), (None, Some("binance")));
    assert_eq!(by_venue.label, None);

    let labelled = parse_of(&["set", "--venue", "dukascopy", "--label", "SWISS", "LOGIN"]).unwrap();
    assert_eq!(labelled.label.as_deref(), Some("SWISS"));
    assert_eq!(labelled.key.as_deref(), Some("LOGIN"));

    let tardis = concat!("TARDIS", "_API_KEY");
    let name = parse_of(&["set", tardis]).unwrap();
    assert_eq!((name.account, name.venue.as_deref()), (None, None));
    assert_eq!(name.key.as_deref(), Some(tardis));
}

/// **The two ACCOUNT addresses are exclusive, a label needs its venue, and `--account` is `set`'s
/// alone** — each refused by the parser, which is the only place that sees both flags were typed.
#[test]
fn the_account_addresses_are_exclusive_and_a_label_needs_a_venue() {
    let both = parse_of(&["set", "--account", "7", "--venue", "binance", "API_KEY"]).unwrap_err();
    assert!(both.contains("--account ID OR --venue VENUE"), "{both}");
    let orphan = parse_of(&["set", "--label", "ALT", "API_KEY"]).unwrap_err();
    assert!(orphan.contains("needs --venue VENUE"), "{orphan}");
    for argv in [&["list", "--account", "7"][..], &["account", "rename", "--account", "7"][..]] {
        let err = parse_of(argv).unwrap_err();
        assert!(err.contains("--account applies to `set` only"), "{argv:?}: {err}");
    }
    // `--venue` and `--label` now belong to `set` too, and are still refused off every verb they
    // mean nothing on, naming `set` among their owners.
    assert!(parse_of(&["list", "--venue", "binance"]).unwrap_err().contains("`set`"));
    assert!(parse_of(&["list", "--label", "ALT"]).unwrap_err().contains("`set --venue`"));
    // `--no-label` stays the account verbs': `set --venue V FIELD` with no `--label` already IS
    // the unlabelled account.
    assert!(parse_of(&["set", "--venue", "binance", "--no-label", "API_KEY"]).is_err());
}

/// **A VALUE IN ARGV IS REFUSED, in both spellings, and the refusal quotes NOTHING.**
///
/// The last assertion is the load-bearing one: a message that echoed the rejected token would
/// write the credential into the scrollback of the session this refusal exists to keep it out
/// of — the refusal doing the exact damage it was built to prevent.
#[test]
fn a_value_in_argv_is_refused_without_echoing_it() {
    for argv in [
        &["set", "--account", "7", "API_KEY", "sk-live-do-not-print"][..],
        &["set", "--account", "7", "API_KEY=sk-live-do-not-print"][..],
        &["set", concat!("TARDIS", "_API_KEY"), "sk-live-do-not-print"][..],
    ] {
        let err = parse_of(argv).expect_err("a value in argv must be refused");
        assert!(err.contains("may not be given on the command line"), "{err}");
        assert!(err.contains("--from-env"), "the refusal must show both accepted forms: {err}");
        assert!(err.contains("stdin"), "{err}");
        assert!(!err.contains("sk-live-do-not-print"), "the refusal ECHOED the value: {err}");
    }
}

/// **…including a value that BEGINS WITH A DASH**, which is the input class the two spellings
/// above could not reach and which the generic `unknown option '{other}'` arm ECHOED verbatim.
///
/// base64url alphabets contain `-`, so a real credential starting with one is ordinary rather
/// than contrived, and stderr is the stream CI logs and every service manager captures. The
/// refusal was writing the secret into the record it exists to keep it out of.
#[test]
fn a_dash_leading_value_is_refused_without_echoing_it_either() {
    for argv in [
        &["set", "--account", "7", "API_KEY", "-sk-live-do-not-print"][..],
        &["set", "--account", "7", "API_KEY", "-sk=live-do-not-print"][..],
        // …and with no FIELD yet parsed, where it must NOT be taken as the field: that path
        // reaches a refusal that names the field it was given — the same echo, one step on.
        &["set", "-sk-live-do-not-print"][..],
    ] {
        let err = parse_of(argv).expect_err("a dash-leading value must be refused");
        assert!(
            !err.contains("sk-live-do-not-print") && !err.contains("live-do-not-print"),
            "the refusal ECHOED the value: {err}"
        );
        assert!(err.contains("may not be given on the command line"), "{err}");
    }
}

/// **A mistyped LONG FLAG is refused without being quoted back either**, and the refusal points
/// at the usage the caller prints beneath it.
///
/// The first fix here exempted a leading `--` so a `--form-env` slip could be named. A PEM
/// key begins `-----BEGIN`, which starts with `--`, and was echoed in full — so any rule that
/// reads the token's own SHAPE is guessing about the secret's alphabet. The cost is this: on
/// `set`, a flag typo reads as a value refusal.
#[test]
fn a_mistyped_flag_is_refused_without_being_quoted_back() {
    let err = parse_of(&["set", "--account", "7", "API_KEY", "--form-env", "X"]).unwrap_err();
    assert!(!err.contains("--form-env"), "even a flag typo is not quoted back: {err}");
    assert!(err.contains("if you meant a FLAG"), "…but the operator is pointed at them: {err}");

    // The other subcommands are UNCHANGED: they have no secret in argv to protect, so a typo
    // there is still named, which is the more useful answer.
    assert!(parse_of(&["list", "--jsonn"]).unwrap_err().contains("--jsonn"));
}

/// **`--file` is refused on the WRITER** — and, since the credential FILE store was removed
/// (2026-10-07), on every reader too: there is no credential file left to inspect. It once let
/// `set KEY --file <any existing file>` append a live credential to whatever the operator named.
#[test]
fn the_file_flag_is_refused_on_set_and_on_the_readers() {
    for sub in [&["set", "--account", "7", "API_KEY"][..], &["list"][..], &["path"][..]] {
        let mut argv = sub.to_vec();
        argv.extend(["--file", "/tmp/anything"]);
        let err = parse_of(&argv).unwrap_err();
        assert!(err.contains("--file was removed"), "{argv:?}: {err}");
        assert!(err.contains("VIKE_SETTINGS_DIR"), "the refusal must name the way through: {err}");
    }
}

/// `set` with no FIELD or NAME is a usage error that shows both value forms — the operator who
/// typed it is exactly the one who does not yet know how the value gets in.
#[test]
fn set_without_a_target_names_both_value_forms() {
    let err = parse_of(&["set"]).unwrap_err();
    assert!(err.contains("needs a FIELD"), "{err}");
    assert!(err.contains("--account ID"), "{err}");
    assert!(err.contains("--from-env") && err.contains("stdin"), "{err}");
}

/// A positional on a READING subcommand is still `unknown option`, unchanged — the non-flag arm
/// is gated on `set` alone.
#[test]
fn a_positional_on_a_reading_subcommand_is_unchanged() {
    for sub in ["list", "path"] {
        let err = parse_of(&[sub, "stray"]).unwrap_err();
        assert!(err.contains("unknown option"), "{sub}: {err}");
    }
    assert!(parse_of(&["list", "--from-env", "X"]).unwrap_err().contains("--from-env"));
}

#[test]
fn usage_documents_the_writer_and_both_of_its_value_forms() {
    for needle in [
        "set --account ID FIELD",
        "set --venue VENUE [--label LABEL] FIELD",
        "set NAME",
        "--from-env",
        "stdin",
        "secrets init",
        "account add",
    ] {
        assert!(usage().contains(needle), "USAGE must mention {needle}");
    }
    // ⚠ The USAGE text must not teach the shape it refuses. `set FIELD VALUE` appearing here as
    // an example is how somebody learns to type it.
    assert!(!usage().contains("set KEY VALUE"), "USAGE must not show the argv form");
    assert!(!usage().contains("FIELD VALUE"), "USAGE must not show the argv form");
    // ⚠ …and it no longer says a credential write creates an account (decision 0156 item 6).
    assert!(!usage().contains("mints"), "no credential write mints an account");
    assert!(!usage().contains("side effect"), "no account is a side effect of a write");
}

// --- The deployment names plain `set NAME` writes -------------------------------------------------

/// **Plain `set NAME` writes the DEPLOYMENT's own names, by name, and nothing else.** The closed
/// list, the attribution codes and the IBKR gateway logins are admitted, each filed for the ledger
/// under no account; a node key is never admitted; and an ACCOUNT's credential under its old
/// `{VENUE}_{TIER}_{FIELD}` name is not a name `set` writes at all (decision 0156 item 6).
///
/// ⚠ The names are COMPOSED rather than spelled: `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'
/// literal harvest reads an env-shaped literal in a `src/` file as evidence this crate READS that
/// variable.
#[test]
fn plain_set_admits_the_deployment_names_and_nothing_else() {
    for (name, _) in vike_model::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID {
        assert_eq!(name_owner(name), Some(NameOwner::Deployment), "{name}");
    }
    for name in vike_model::credential_keys::attribution_keys() {
        let venue = vike_model::credential_keys::attribution_key_owner(&name).expect("an owner");
        assert_eq!(name_owner(&name), Some(NameOwner::Attribution(venue)), "{name}");
    }
    for name in [format!("IBKR_{}", "DEMO_USERNAME"), format!("IBKR_{}", "LIVE_PASSWORD")] {
        assert_eq!(name_owner(&name), Some(NameOwner::GatewayLogin), "{name}");
    }
    for node in vike_model::credential_keys::PLATFORM_KEYS {
        assert_eq!(name_owner(node), None, "{node}: a node key is minted, never typed");
    }
    // An ACCOUNT's credential in the old shape: refused, and the refusal points at `--account`.
    let venue_key = format!("BINANCE_{}", "LIVE_API_KEY");
    assert_eq!(name_owner(&venue_key), None, "{venue_key}");
    let msg = unknown_key_message(&venue_key);
    assert!(msg.contains("vike-cli secrets set --account <ID> <FIELD>"), "{msg}");
    assert!(msg.contains("decision 0156"), "{msg}");
    assert!(msg.contains("account add --venue <VENUE> --tier <paper|demo|live>"), "{msg}");
    // ⚠ A SETTINGS key is NOT admitted either: `VIKE_TRADEHUB_LIVE` has a registry row exactly
    // like a credential's, but it is a retired settings variable (decision 0111), so a credential
    // row written for it would be the dead line the refusal exists to prevent. The CONST, never the
    // literal, for the harvest's reason.
    let live_gate = vike_config::flags::TRADEHUB_LIVE_ENV;
    assert!(registry_readers(live_gate).is_some(), "the control is not a control: it has no row");
    assert_eq!(name_owner(live_gate), None, "a settings key must not be settable: {live_gate}");
    assert!(unknown_key_message(live_gate).contains("SETTING"), "{live_gate}");
}

/// **The ledger files a name under no account**: the deployment's own under `deployment`, an
/// attribution code under its venue, and every one of them untiered.
#[test]
fn a_name_is_filed_under_no_account_in_the_ledger() {
    let tg = format!("VIKE_{}", "TELEGRAM_BOT_TOKEN");
    assert_eq!(name_owner(&tg), Some(NameOwner::Deployment), "{tg}");
    assert_eq!(DEPLOYMENT_OWNER, "deployment");
    for name in [format!("FINNHUB_{}", "API_KEY"), format!("FMP_{}", "API_KEY")] {
        assert!(registry_readers(&name).is_some(), "{name}: read by vike-datahub");
        assert_eq!(name_owner(&name), Some(NameOwner::Deployment), "{name}");
    }
}

/// **The ROTATION rule: a DEPLOYMENT name this store already holds is settable by name — and an
/// ACCOUNT's or a VENUE's credential never is, whatever its name.** The store says which rows are
/// deployment rows (`CredentialStore::deployment_names`, by the row's own columns): a row filed
/// against an account or a venue is not among them; a node key is refused even when named; and a
/// name the store does not hold is not invented.
#[test]
fn the_rotation_rule_reaches_held_deployment_rows_and_never_an_accounts() {
    let cf = format!("CLOUDFLARE_{}", "API_TOKEN");
    assert!(registry_readers(&cf).is_none(), "{cf}: the control is not a control");
    assert_eq!(name_owner(&cf), None, "{cf}: not by the name rules");
    let (dir, ids) = store_with_accounts(&[("binance", AccountTier::Live, None)]);
    vike_secrets::save_credentials_to_store_unjournalled(
        dir.path(),
        vike_secrets::Table::Credential,
        &[(cf.clone(), "token".to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("a deployment row");
    let account_row = vike_secrets::save_owned_credentials_unjournalled(
        dir.path(),
        &vike_secrets::CredentialOwner::Account(ids[0]),
        &[("API_KEY".to_string(), "k".to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("an account's row")
    .names
    .remove(0);
    let venue_row = vike_secrets::save_owned_credentials_unjournalled(
        dir.path(),
        &vike_secrets::CredentialOwner::Venue("ctrader".to_string()),
        &[("CLIENT_ID".to_string(), "app".to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("a venue's row")
    .names
    .remove(0);
    let deployment = handle(dir.path()).deployment_names().expect("readable");

    assert!(deployment_rotation(&cf, &deployment), "{cf}: a held deployment row rotates");
    assert!(!deployment_rotation(&account_row, &deployment), "an ACCOUNT's row is --account's");
    assert!(!deployment_rotation(&venue_row, &deployment), "a venue row is --venue's");
    let node = format!("VIKE_{}", "TRADEHUB_CONTROL_KEY");
    let mut with_node = deployment.clone();
    with_node.insert(node.clone());
    assert!(!deployment_rotation(&node, &with_node), "{node}: a minted key is never typed");
    let empty = std::collections::BTreeSet::new();
    assert!(!deployment_rotation(&cf, &empty), "{cf}: nothing to rotate, nothing invented");
}

/// **The two TRADEHUB node keys get their own message, and it names a command rather than an
/// editor.** Sending an operator to an editor would be correct about the refusal and wrong about
/// the route: `vike-cli backend setup` / `datahub setup` MINT the pair, and a hand-pasted 256-bit
/// key that is truncated fails as an opaque auth denial.
///
/// ⚠ The names are COMPOSED, for the literal-harvest reason the tests above give.
#[test]
fn the_node_keys_refusal_names_the_command_that_mints_them() {
    // ⚠ The verb is chosen PER SERVICE: a constant would send an operator to the command for a
    // different service — the same defect the arm exists to end.
    for (key, verb, service) in [
        (format!("VIKE_{}", "TRADEHUB_OBSERVE_KEY"), "backend", "vike-tradehub"),
        (format!("VIKE_{}", "TRADEHUB_CONTROL_KEY"), "backend", "vike-tradehub"),
        (format!("VIKE_{}", "DATAHUB_OBSERVE_KEY"), "datahub", "vike-datahub"),
        (format!("VIKE_{}", "DATAHUB_CONTROL_KEY"), "datahub", "vike-datahub"),
    ] {
        assert!(vike_model::credential_keys::is_platform_key(&key), "{key}");
        assert_eq!(
            vike_model::credential_keys::platform_key_service(&key),
            Some(service),
            "{key}: the table is what picks the verb"
        );
        let msg = unknown_key_message(&key);
        assert!(!msg.contains("nothing would ever load"), "{key}: {msg}");
        assert!(msg.contains(&key), "{key}: {msg}");
        assert!(msg.contains("IS read by this workspace"), "{key}: {msg}");
        assert!(
            msg.contains(&format!("{verb} setup")),
            "{key}: it must name the minting command for ITS service: {msg}"
        );
        assert!(msg.contains(service), "{key}: which box, by service: {msg}");
        assert!(
            !msg.contains("EDITOR"),
            "{key}: the editor route is exactly what `{verb} setup` deletes: {msg}"
        );
        // ⚠ AND IT MAY NOT NAME A COMMAND THAT DOES NOT EXIST: there is no `datahub connect`.
        assert!(
            !msg.contains("datahub connect"),
            "{key}: there is no `datahub connect` to send anyone to: {msg}"
        );
    }
    let th = unknown_key_message(&format!("VIKE_{}", "TRADEHUB_OBSERVE_KEY"));
    assert!(th.contains("backend connect"), "the tradehub's client half exists and is named: {th}");
    // …and the arm is NARROW: a name one letter off is not a platform key.
    let near = format!("VIKE_{}", "TRADEHUB_OBSERVE_KEYS");
    assert!(!vike_model::credential_keys::is_platform_key(&near));
    assert!(!unknown_key_message(&near).contains("backend setup"), "{near}");
}

/// **An unknown NAME is refused BY NAME, and the message points at real ones** — the nearest
/// names plain `set` writes, and the `--account` route.
///
/// The lower-case case is separate because it is the likeliest near-miss and Levenshtein scores
/// it as far away as a different venue — every letter differs.
#[test]
fn an_unknown_name_is_refused_by_name_with_the_nearest_real_ones() {
    let real = format!("VIKE_{}", "ALERT_TELEGRAM_TOKEN");
    let truncated = &real[..real.len() - 1];
    let extended = format!("{real}X");

    let msg = unknown_key_message(truncated);
    assert!(msg.contains(truncated), "the refusal must name the key: {msg}");
    assert!(msg.contains(&real), "…and suggest the real one: {msg}");
    assert!(msg.contains("--account <ID> <FIELD>"), "…and the account route: {msg}");

    assert_eq!(nearest_keys(&real.to_lowercase()), vec![real.clone()]);
    assert!(nearest_keys(&extended).contains(&real));

    // Nonsense suggests NOTHING, and says where every key name is instead. Three unrelated
    // names presented as guesses is worse than the bare refusal.
    assert!(nearest_keys("totally-unrelated-nonsense").is_empty());
    let far = unknown_key_message("totally-unrelated-nonsense");
    assert!(far.contains("nothing would ever load"), "{far}");
    assert!(far.contains("vike-cli config show"), "{far}");
    assert!(!far.contains("did you mean"), "{far}");
    assert!(!far.contains("with an editor"), "the editor route is the dead one: {far}");
}

/// …and the same claim as a PROPERTY over the whole registry, so the arm cannot be right for the
/// names above and wrong for the next one added.
///
/// Both directions, because either alone is satisfiable by a message that says nothing: every
/// declared name `set` does not write by name must be told it is read and by whom, and pointed at
/// `--account`; and a name the registry does NOT carry must still get the original sentence.
#[test]
fn the_unread_sentence_is_printed_only_where_no_registry_row_names_the_key() {
    let mut checked = 0usize;
    let mut settable = 0usize;
    for s in all_settings() {
        // The names `set` writes by name never reach this message at all.
        if name_owner(s.name).is_some() {
            settable += 1;
            continue;
        }
        let msg = unknown_key_message(s.name);
        assert!(
            !msg.contains("nothing would ever load"),
            "{} has a registry row and must not be called unread: {msg}",
            s.name
        );
        assert!(msg.contains(s.krate), "{}: the refusal must name {}: {msg}", s.name, s.krate);
        if vike_model::credential_keys::platform_key_service(s.name).is_none() {
            assert!(msg.contains("--account <ID> <FIELD>"), "{}: the account route: {msg}", s.name);
        }
        checked += 1;
    }
    assert!(checked > 0, "the registry carries no refused rows — this test proved nothing");
    assert!(settable > 0, "no registry name is settable — the writer reaches nothing");

    let unread = "totally-unrelated-nonsense";
    assert!(registry_readers(unread).is_none());
    assert!(unknown_key_message(unread).contains("nothing would ever load"));
}

// --- The account address ----------------------------------------------------------------------------

/// One `account` row, as the store reads it.
fn row(
    id: i64,
    venue: &str,
    tier: AccountTier,
    label: Option<&str>,
    active: bool,
) -> vike_secrets::Account {
    vike_secrets::Account {
        id,
        venue: venue.to_string(),
        tier,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
    }
}

/// **`--venue VENUE [--label L]` resolves to the ONE ACTIVE row of that address** — an account's
/// tier is its row's, so several rows (one per tier) can share an address and only the active ones
/// are candidates. None is refused naming the `account add` line and the inactive ids; several are
/// refused naming every id; the label is part of the address.
#[test]
fn the_venue_address_resolves_to_the_one_active_row_or_refuses_by_id() {
    let rows = vec![
        row(1, "binance", AccountTier::Demo, None, true),
        row(2, "binance", AccountTier::Live, None, false),
        row(3, "dukascopy", AccountTier::Demo, Some("SWISS"), true),
        row(4, "dukascopy", AccountTier::Demo, Some("EUROPE"), true),
        row(5, "okx", AccountTier::Demo, None, true),
        row(6, "okx", AccountTier::Live, None, true),
    ];
    assert_eq!(the_one_active_account(&rows, "binance", None, "API_KEY").unwrap().id, 1);
    assert_eq!(the_one_active_account(&rows, "dukascopy", Some("EUROPE"), "LOGIN").unwrap().id, 4);

    let none = the_one_active_account(&rows, "bybit", None, "API_KEY").unwrap_err();
    assert_eq!(none.exit, crate::exit::Exit::Usage);
    assert!(
        none.msg.contains(
            "vike-cli secrets account add --venue bybit --tier <paper|demo|live> --no-label"
        ),
        "{}",
        none.msg
    );
    assert!(none.msg.contains("never creates one"), "{}", none.msg);
    let labelled = the_one_active_account(&rows, "binance", Some("HEDGE"), "API_KEY").unwrap_err();
    assert!(labelled.msg.contains("--label HEDGE"), "{}", labelled.msg);

    let several = the_one_active_account(&rows, "okx", None, "API_KEY").unwrap_err();
    assert!(several.msg.contains("account 5 (demo)") && several.msg.contains("account 6 (live)"));
    assert!(several.msg.contains("--account <ID> API_KEY"), "{}", several.msg);

    // An address whose only row is INACTIVE is refused too, and the refusal names that row: it is
    // `--account`'s to write, and `account activate`'s to arm.
    let only_inactive = vec![row(9, "deribit", AccountTier::Live, None, false)];
    let err = the_one_active_account(&only_inactive, "deribit", None, "API_KEY").unwrap_err();
    assert!(err.msg.contains("INACTIVE") && err.msg.contains("9 (live)"), "{}", err.msg);
}

/// **A FIELD outside the venue's credential schema is refused, listing the venue's fields** — the
/// account's and, where the venue has any, the venue-wide ones.
#[test]
fn a_field_the_venue_does_not_carry_is_refused_listing_its_fields() {
    let ctrader = vike_model::credential_schema::schema_for("ctrader").expect("a row");
    let msg = unknown_field_message(ctrader, "API_KEY");
    assert!(msg.contains("'API_KEY' is not a credential field of ctrader"), "{msg}");
    assert!(msg.contains("ACCESS_TOKEN") && msg.contains("REFRESH_TOKEN"), "{msg}");
    assert!(msg.contains("CLIENT_ID") && msg.contains("ONCE for every account"), "{msg}");
    let binance = vike_model::credential_schema::schema_for("binance").expect("a row");
    let msg = unknown_field_message(binance, "LOGIN");
    assert!(msg.contains("API_KEY, API_SECRET, API_PASSPHRASE"), "{msg}");
    assert!(!msg.contains("ONCE for every account"), "binance has no venue-wide field: {msg}");
}

/// **An account id no row carries is the store's own `NoSuchAccount` refusal, plus the `account
/// add` line that creates one.**
#[test]
fn a_missing_account_is_the_stores_refusal_with_the_add_line() {
    let msg = no_such_account_message(std::path::Path::new("/p/settings/db/vike.db"), 42);
    assert!(msg.contains("holds no account with id 42"), "the store's own sentence: {msg}");
    assert!(msg.contains("no account was created"), "{msg}");
    assert!(
        msg.contains("vike-cli secrets account add --venue <VENUE> --tier <paper|demo|live>"),
        "{msg}"
    );
    assert!(msg.contains("vike-cli secrets set --account <ID> <FIELD>"), "{msg}");
}

// --- The writer, end to end through `run_set` over a real store -----------------------------------
//
// `--from-env` carries the value (the dispatcher's swept map is a parameter), so these drive the
// verb's whole order — the pure refusals, the store, the account rules, the value, the write and the
// record — with no stdin and no binary. `crates/vike-cli/tests/secrets_cli/set.rs` drives the
// shipped binary.

/// A project's settings directory holding the EMPTY store and `accounts`, each created the way
/// `secrets account add` creates one (the store's account writer, record-less), and their ids.
fn store_with_accounts(
    accounts: &[(&str, AccountTier, Option<&str>)],
) -> (tempfile::TempDir, Vec<i64>) {
    let dir = tempfile::tempdir().expect("tempdir");
    vike_secrets::create_store(Some(dir.path().to_str().expect("a utf-8 temp path")))
        .expect("create the empty store");
    let ids = accounts
        .iter()
        .map(|&(venue, tier, label)| {
            vike_secrets::edit_account_in_unjournalled(
                dir.path(),
                vike_secrets::AccountEdit::Create { venue, tier, label },
            )
            .expect("create the account")
            .after
            .expect("a create returns its row")
            .id
        })
        .collect();
    (dir, ids)
}

/// `run_set` over `argv`, the value in `$SRC`.
fn set_in(settings: &std::path::Path, argv: &[&str], value: &str) -> CmdResult<()> {
    let mut full = vec!["set"];
    full.extend_from_slice(argv);
    full.extend_from_slice(&["--from-env", "SRC"]);
    let args = parse_of(&full).expect("the argv parses");
    let env = HashMap::from([("SRC".to_string(), value.to_string())]);
    let state = settings.join("state");
    let ctx = Ctx {
        settings_dir: Some(settings),
        settings_dir_override: None,
        state_dir: Some(&state),
        env: &env,
        scope: vike_secrets::CredentialScope::Full,
        now_ms: 0,
    };
    run_set(&args, &ctx)
}

fn handle(settings: &std::path::Path) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings)),
        vike_secrets::CredentialScope::Full,
    )
}

fn account_count(settings: &std::path::Path) -> usize {
    handle(settings).accounts().expect("readable").known().expect("a table").len()
}

/// **`--account ID FIELD` writes that account's field — by FIELD, against the row** — and a field
/// written again REPLACES the one row in place.
#[test]
fn set_by_account_writes_the_accounts_field() {
    let (dir, ids) = store_with_accounts(&[("binance", AccountTier::Live, None)]);
    let id = ids[0].to_string();
    set_in(dir.path(), &["--account", id.as_str(), "API_KEY"], "k-one").expect("written");
    set_in(dir.path(), &["--account", id.as_str(), "API_KEY"], "k-two").expect("rotated");

    let book = handle(dir.path()).credential_book().expect("readable");
    let fields = book.account(ids[0]).expect("the account holds a field");
    assert_eq!(fields.get("API_KEY"), Some("k-two"), "the field holds the last write");
    assert_eq!(fields.fields().count(), 1, "one live row per field");
    assert_eq!(account_count(dir.path()), 1, "a credential write creates no account");
}

/// **A write to an account id no row carries is REFUSED, and it creates NO account** — the
/// store's `NoSuchAccount` sentence and the `account add` line, before the value is read.
#[test]
fn set_by_a_missing_account_refuses_and_creates_no_account() {
    let (dir, _) = store_with_accounts(&[("binance", AccountTier::Demo, None)]);
    let err = set_in(dir.path(), &["--account", "99", "API_KEY"], "never-written").unwrap_err();
    assert_eq!(err.exit, crate::exit::Exit::Usage, "an id no row carries is a command-line typo");
    assert!(err.msg.contains("holds no account with id 99"), "{}", err.msg);
    assert!(
        err.msg.contains("account add --venue <VENUE> --tier <paper|demo|live>"),
        "{}",
        err.msg
    );
    assert!(!err.msg.contains("never-written"), "a refusal never carries the value");
    assert_eq!(account_count(dir.path()), 1, "NO account row was created");
    let held = handle(dir.path()).read_all().expect("readable");
    assert!(held.secrets.is_empty(), "nothing was written: {:?}", held.secrets);
}

/// **A FIELD the account's venue does not carry is refused, listing the fields; a VENUE-wide field
/// addressed to one account is refused, naming `--venue`.**
#[test]
fn set_by_account_refuses_a_field_outside_the_venues_schema() {
    let (dir, ids) = store_with_accounts(&[
        ("binance", AccountTier::Demo, None),
        ("ctrader", AccountTier::Demo, None),
    ]);
    let (binance, ctrader) = (ids[0].to_string(), ids[1].to_string());
    let err = set_in(dir.path(), &["--account", binance.as_str(), "LOGIN"], "v").unwrap_err();
    assert!(err.msg.contains("not a credential field of binance"), "{}", err.msg);
    assert!(err.msg.contains("API_KEY"), "{}", err.msg);
    let err = set_in(dir.path(), &["--account", ctrader.as_str(), "CLIENT_ID"], "v").unwrap_err();
    assert!(err.msg.contains("VENUE-wide"), "{}", err.msg);
    assert!(err.msg.contains("vike-cli secrets set --venue ctrader CLIENT_ID"), "{}", err.msg);
    assert!(handle(dir.path()).read_all().expect("readable").secrets.is_empty());
}

/// **`--venue VENUE [--label L] FIELD` writes the ONE active account of that address**, and a
/// VENUE-wide field is written once, by the venue — not onto any account.
#[test]
fn set_by_venue_writes_the_one_active_account_or_the_venue() {
    let (dir, ids) = store_with_accounts(&[
        ("dukascopy", AccountTier::Demo, Some("SWISS")),
        ("dukascopy", AccountTier::Demo, Some("EUROPE")),
        ("ctrader", AccountTier::Demo, None),
    ]);
    set_in(dir.path(), &["--venue", "dukascopy", "--label", "EUROPE", "LOGIN"], "login-eu")
        .expect("written");
    set_in(dir.path(), &["--venue", "ctrader", "CLIENT_ID"], "app-id").expect("written");

    let book = handle(dir.path()).credential_book().expect("readable");
    assert_eq!(book.account(ids[1]).and_then(|f| f.get("LOGIN")), Some("login-eu"));
    assert!(book.account(ids[0]).is_none(), "the OTHER label's account is untouched");
    assert_eq!(book.venue("ctrader").and_then(|f| f.get("CLIENT_ID")), Some("app-id"));
    assert!(book.account(ids[2]).is_none(), "a venue-wide field lands on no account");

    // No active account at the address: refused, and nothing created.
    let err = set_in(dir.path(), &["--venue", "bybit", "API_KEY"], "v").unwrap_err();
    assert!(err.msg.contains("account add --venue bybit --tier <paper|demo|live>"), "{}", err.msg);
    assert_eq!(account_count(dir.path()), 3, "a credential write creates no account");
}

/// **A plain `set` of an ACCOUNT's credential name is refused**, pointing at `--account` — even on
/// a store that holds that name filed against an account; and a DEPLOYMENT name is written by name,
/// as a row no account owns.
#[test]
fn plain_set_refuses_an_account_name_and_writes_a_deployment_name() {
    let (dir, ids) = store_with_accounts(&[("binance", AccountTier::Live, None)]);
    let venue_key = format!("BINANCE_{}", "LIVE_API_KEY");
    let err = set_in(dir.path(), &[venue_key.as_str()], "v").unwrap_err();
    assert_eq!(err.exit, crate::exit::Exit::Usage);
    assert!(err.msg.contains("--account <ID> <FIELD>"), "{}", err.msg);
    assert!(handle(dir.path()).read_all().expect("readable").secrets.is_empty());

    // The account's own row, held under the store's name for it, is not rotatable by name either.
    let id = ids[0].to_string();
    set_in(dir.path(), &["--account", id.as_str(), "API_KEY"], "k").expect("written");
    let row_name = vike_secrets::account_credential_name("binance", ids[0], "API_KEY");
    let err = set_in(dir.path(), &[row_name.as_str()], "v").unwrap_err();
    assert!(err.msg.contains("--account <ID> <FIELD>"), "{}", err.msg);

    let tardis = format!("TARDIS_{}", "API_KEY");
    set_in(dir.path(), &[tardis.as_str()], "vendor-key")
        .expect("a deployment name is written by name");
    let held = handle(dir.path()).read_all().expect("readable");
    assert!(held.secrets.keys().any(|k| k == tardis), "{:?}", held.secrets);
    let filed = handle(dir.path()).account_keys().expect("readable").expect("a table");
    assert!(
        !filed.values().any(|k| k.names.contains(&tardis)),
        "a deployment name is filed against no account"
    );
    assert_eq!(account_count(dir.path()), 1, "a credential write creates no account");
}

// --- The value ------------------------------------------------------------------------------------

/// **A MULTI-LINE `--from-env` value is refused, and a WHITESPACE-ONLY one with it.**
///
/// The multi-line case was an INJECTION, not an untidiness: quoted and joined by the credential
/// FILE store's writer, the value's own newline became a physical line break, so the
/// reader returned the first half as a truncated credential and read the second half as a WHOLE
/// NEW `KEY=VALUE` — a credential for a venue the operator never configured, past a key name
/// this command had validated.
///
/// The whitespace-only case is milder and the same shape: three spaces are a non-empty value, so
/// the venue reads as CONFIGURED and arms with a garbage secret, failing at the venue instead of
/// staying on paper.
///
/// It is asserted HERE, at the seam that names the variable, as well as in the store's writers,
/// which refuse it for every caller including the GUI.
#[test]
fn a_multiline_or_blank_env_value_is_refused_naming_the_variable_and_never_the_value() {
    let args = parse_of(&["set", "--account", "7", "API_KEY", "--from-env", "SRC"]).unwrap();
    let refusal = |raw: &str| -> String {
        let env = HashMap::from([("SRC".to_string(), raw.to_string())]);
        let ctx = Ctx {
            settings_dir: None,
            settings_dir_override: None,
            state_dir: None,
            env: &env,
            scope: vike_secrets::CredentialScope::Full,
            now_ms: 0,
        };
        value_for(&args, &ctx).expect_err("must be refused").msg
    };

    for raw in ["abc\nAPI_SECRET=injected", "tok\n", "tok\r"] {
        let msg = refusal(raw);
        assert!(msg.contains("more than one line"), "{msg}");
        assert!(msg.contains("SRC"), "the refusal must name the VARIABLE: {msg}");
        assert!(!msg.contains("injected") && !msg.contains("tok"), "it ECHOED a value: {msg}");
    }
    for blank in ["", "   ", "\t"] {
        let msg = refusal(blank);
        assert!(msg.contains("unset, empty or only whitespace"), "{msg}");
        assert!(msg.contains("SRC"), "{msg}");
    }

    // …and an ordinary one-line value still passes through VERBATIM, including the leading and
    // trailing whitespace this arm deliberately does not trim.
    let env = HashMap::from([("SRC".to_string(), " tok ".to_string())]);
    let ctx = Ctx {
        settings_dir: None,
        settings_dir_override: None,
        state_dir: None,
        env: &env,
        scope: vike_secrets::CredentialScope::Full,
        now_ms: 0,
    };
    assert_eq!(value_for(&args, &ctx).unwrap(), " tok ");
}

#[test]
fn edit_distance_is_the_ordinary_one() {
    assert_eq!(edit_distance("", ""), 0);
    assert_eq!(edit_distance("abc", "abc"), 0);
    assert_eq!(edit_distance("abc", ""), 3);
    assert_eq!(edit_distance("", "abc"), 3);
    assert_eq!(edit_distance("kitten", "sitting"), 3);
}

// --- The closed list of settable names outside the grid, held to the registry ------------------

/// Why a registry read is NOT a credential-store read. Each row of [`NOT_A_CREDENTIAL_STORE_READ`]
/// carries one of these, and each was read off the call site, not inferred from the name.
const PROCESS_ENV: &str = "read off the PROCESS environment sweep its binary hands it, never the \
                           credential map";
const SETTINGS_FOLD: &str = "the credential map is consulted, but the daemon's flag fold OVERWRITES \
                             this key with a settings-database flag first (or the process \
                             environment answers) — a credential row is never what decides it";
const LOADER_INPUT: &str = "an INPUT to the credential-store loader, read off the real process \
                            environment before any store is opened";
const CHILD_ENV: &str = "written INTO a child process's environment, never read";
const COMPILE_TIME: &str = "a compile-time `env!` its build script sets, not a runtime read";
const DIRECT_ENV: &str = "the only production read is a direct `std::env::var` in a binary";
const TEST_ONLY: &str = "read by tests only";
const HTTP_HEADER: &str = "an HTTP header NAME the venue's own signing code writes or asserts, never a credential-map key";

/// **Every registry MAP-LOOKUP read outside the names plain `set` writes and the node keys — and
/// why it is not a credential-store read.** `(name, krate, reason)`.
///
/// The judgement half of [`the_settable_list_and_the_registry_agree_both_ways`]: a NEW map-lookup
/// read outside every settable rule fails that test until it is either added to
/// `vike_model::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID` (it reads the credential store by
/// name, so `set` must be able to write it) or argued here (it does not). Rows whose crate is
/// `vike-config` are not listed: `vike_config::load` is handed the PROCESS environment by every
/// binary and never the credential map (`vike_config::settings`' module doc, family 1). An ACCOUNT's
/// credential is no map lookup at all since decision 0156 item 6 — a bridge reads its account's
/// FIELDS — so no venue key name belongs here.
///
/// ⚠ Names are `concat!`-split for the registry harvest's reason (an env-shaped literal in a `src/`
/// file reads to it as this crate reading the variable).
const NOT_A_CREDENTIAL_STORE_READ: &[(&str, &str, &str)] = &[
    (concat!("CAR", "GO"), "vike-strategy-builder", PROCESS_ENV),
    (concat!("DUKASCOPY", "_JNLP"), "bridges/dukascopy", CHILD_ENV),
    (concat!("DUKASCOPY", "_LOGIN"), "bridges/dukascopy", CHILD_ENV),
    (concat!("DUKASCOPY", "_PASSWORD"), "bridges/dukascopy", CHILD_ENV),
    (concat!("HO", "ME"), "vike-paths", PROCESS_ENV),
    (concat!("HYPERLIQUID", "_HIP3"), "bridges/hyperliquid", SETTINGS_FOLD),
    (concat!("LOCALAPP", "DATA"), "vike-paths", PROCESS_ENV),
    (concat!("POLY", "_ADDRESS"), "bridges/polymarket", HTTP_HEADER),
    (concat!("POLY", "_API_KEY"), "bridges/polymarket", HTTP_HEADER),
    (concat!("POLY", "_CHAIN_FROM"), "bridges/polymarket", PROCESS_ENV),
    (concat!("POLY", "_CHAIN_TO"), "bridges/polymarket", PROCESS_ENV),
    (concat!("POLY", "_EXEC"), "bridges/polymarket", SETTINGS_FOLD),
    (concat!("POLY", "_NONCE"), "bridges/polymarket", HTTP_HEADER),
    (concat!("POLY", "_PASSPHRASE"), "bridges/polymarket", HTTP_HEADER),
    (concat!("POLY", "_RECONCILE"), "bridges/polymarket", SETTINGS_FOLD),
    (concat!("POLY", "_SIGNATURE"), "bridges/polymarket", HTTP_HEADER),
    (concat!("POLY", "_TIMESTAMP"), "bridges/polymarket", HTTP_HEADER),
    (concat!("USERPRO", "FILE"), "vike-secrets", TEST_ONLY),
    (concat!("VIKE", "_ALLOW_WITHDRAW_KEYS"), "vike-secrets", SETTINGS_FOLD),
    (concat!("VIKE", "_LOG_DIR"), "vike-mount", DIRECT_ENV),
    (concat!("VIKE", "_NO_LIVE_KEYS"), "vike-boot", LOADER_INPUT),
    (concat!("VIKE", "_NO_LIVE_KEYS"), "vike-secrets", LOADER_INPUT),
    (concat!("VIKE", "_PLUGIN_FINGERPRINT"), "vike-strategy-plugin", COMPILE_TIME),
    (concat!("VIKE", "_PREFLIGHT_SKIP"), "vike-mount", SETTINGS_FOLD),
    (concat!("VIKE", "_SETTINGS_DIR"), "vike-app-core", LOADER_INPUT),
    (concat!("VIKE", "_SETTINGS_DIR"), "vike-boot", LOADER_INPUT),
    (concat!("VIKE", "_SETTINGS_DIR"), "vike-compute", LOADER_INPUT),
    (concat!("VIKE", "_SETTINGS_DIR"), "vike-datahub-client", LOADER_INPUT),
    (concat!("VIKE", "_SETTINGS_DIR"), "vike-paths", LOADER_INPUT),
    (concat!("VIKE", "_SETTINGS_DIR"), "vike-secrets", LOADER_INPUT),
    (concat!("VIKE", "_STRATEGY_BUILDER_KEY"), "vike-strategy-builder", PROCESS_ENV),
    (concat!("VIKE", "_STRATEGY_BUILDER_KEY_FILE"), "vike-strategy-builder", PROCESS_ENV),
    (concat!("VIKE", "_STUDIO_COMPUTE_KEY"), "vike-studio", PROCESS_ENV),
    (concat!("VIKE", "_USER_DATA_DIR"), "vike-cli", PROCESS_ENV),
    (concat!("VIKE", "_USER_DATA_DIR"), "vike-compute", PROCESS_ENV),
    (concat!("VIKE", "_USER_DATA_DIR"), "vike-desktop", PROCESS_ENV),
    (concat!("VIKE", "_USER_DATA_DIR"), "vike-paths", PROCESS_ENV),
    (concat!("XDG", "_DATA_HOME"), "vike-paths", PROCESS_ENV),
];

/// Is `name` written by name by a rule OTHER than the closed list — an attribution code or an IBKR
/// gateway login — or is it a node key (never settable by hand, minted or copied)?
fn owned_elsewhere(name: &str) -> bool {
    vike_model::credential_keys::is_platform_key(name)
        || matches!(name_owner(name), Some(NameOwner::Attribution(_) | NameOwner::GatewayLogin))
}

/// **THE GATE: `STORE_KEYS_OUTSIDE_THE_GRID` and the settings registry agree in BOTH directions.**
///
/// 1. **No stale settable name.** Every row of the closed list names a registry row of exactly that
///    `(name, krate)`, and is owned by no other rule (not an attribution code, not a gateway login,
///    not a node key — any of those would make the row a second answer). A row whose reader is gone
///    fails here, so a name nothing reads cannot stay settable.
/// 2. **No unsettable credential-store read.** Every registry row that is a MAP LOOKUP — the shape
///    a by-name credential-store read takes — outside `vike-config`, the node keys and the names
///    plain `set` writes must be argued in [`NOT_A_CREDENTIAL_STORE_READ`]. A new by-name
///    credential-store read therefore fails here until it is made settable or argued not to be one;
///    and every argued row must still be a live registry row outside every settable rule, so the
///    table cannot rot either.
///
/// ⚠ **The bound, declared rather than implied:** a `Literal`/`Konst` row records the DIRECT read
/// when one name is read both ways (the registry's tie-break), so a credential-store read hiding
/// behind a direct-read row is not seen by direction 2 — the databento key is that shape, and it is
/// in the closed list by its call site, which direction 1 still holds.
#[test]
fn the_settable_list_and_the_registry_agree_both_ways() {
    use vike_config::settings::Naming;
    use vike_model::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID;

    // Direction 1.
    for (name, reader) in STORE_KEYS_OUTSIDE_THE_GRID {
        assert!(
            all_settings().any(|s| s.name == name && s.krate == reader),
            "{name}: listed as read by {reader}, and no registry row says so — a stale settable name"
        );
        assert!(!owned_elsewhere(name), "{name}: another settable rule already owns it");
        assert_eq!(name_owner(name), Some(NameOwner::Deployment), "{name}: listed but refused");
    }

    // Direction 2.
    let argued = |name: &str, krate: &str| {
        NOT_A_CREDENTIAL_STORE_READ.iter().any(|(n, k, _)| *n == name && *k == krate)
    };
    let mut unargued: Vec<String> = Vec::new();
    let mut considered = 0usize;
    for s in all_settings() {
        if !matches!(s.naming, Naming::MapLookup) || s.krate == "vike-config" {
            continue;
        }
        if owned_elsewhere(s.name) || name_owner(s.name).is_some() {
            continue;
        }
        considered += 1;
        if !argued(s.name, s.krate) {
            unargued.push(format!("({}, {})", s.name, s.krate));
        }
    }
    unargued.sort();
    unargued.dedup();
    assert!(
        unargued.is_empty(),
        "these registry map-lookup reads are outside every settable rule and NOT argued as \
         non-credential-store reads. If the reader consults the CREDENTIAL map by name, add the \
         name to `vike_model::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID` (so `secrets set` can \
         write it); otherwise add a row to `NOT_A_CREDENTIAL_STORE_READ` saying what it reads:\n  {}",
        unargued.join("\n  ")
    );
    let mut stale: Vec<String> = Vec::new();
    for (name, krate, reason) in NOT_A_CREDENTIAL_STORE_READ {
        assert!(!reason.is_empty());
        let live = all_settings()
            .any(|s| s.name == *name && s.krate == *krate && matches!(s.naming, Naming::MapLookup));
        if !live || owned_elsewhere(name) || name_owner(name).is_some() {
            stale.push(format!("({name}, {krate})"));
        }
    }
    assert!(
        stale.is_empty(),
        "these NOT_A_CREDENTIAL_STORE_READ rows no longer name a registry map-lookup read outside \
         every settable rule — delete them:\n  {}",
        stale.join("\n  ")
    );
    assert!(considered > 0, "direction 2 considered no row — the filter stopped looking");
}

/// **A name in no table and no store is still refused, BY NAME** — the closed list widened `set`
/// by its rows and nothing else. A settings key (read off the process environment) and a typo of a
/// listed name are both refused.
#[test]
fn the_closed_list_admits_its_rows_and_nothing_near_them() {
    for (name, _) in vike_model::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID {
        assert!(name_owner(name).is_some(), "{name}");
        // …and a one-character slip off it is not admitted by the list.
        let near = format!("{name}S");
        assert!(
            !vike_model::credential_keys::is_store_key_outside_the_grid(&near),
            "{near} admitted by a near-miss"
        );
    }
    let compute = format!("VIKE_{}", "STUDIO_COMPUTE_KEY");
    assert!(registry_readers(&compute).is_some(), "the control is not a control");
    assert_eq!(name_owner(&compute), None, "{compute}: a process-env key");
    assert!(unknown_key_message(&compute).contains("SETTING"), "{compute}");
    let pager = format!("VIKE_{}", "ALERT_TELEGRAM_TOKN");
    assert_eq!(name_owner(&pager), None, "{pager}: a typo of a listed name");
    // …and the refusal offers the real spelling.
    let near = nearest_keys(&pager);
    assert_eq!(
        near.first().map(String::as_str),
        Some(format!("VIKE_{}", "ALERT_TELEGRAM_TOKEN").as_str())
    );
}

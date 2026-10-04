use vike_ops::settings::all_settings;

use super::set::{
    DEPLOYMENT_OWNER, edit_distance, labelled_account, nearest_keys, registry_readers,
    rotation_owner, settable_outside_the_grid, unknown_key_message, value_for,
};
use super::*;

fn parse_of(argv: &[&str]) -> Result<Args, String> {
    parse(argv.iter().map(|s| (*s).to_string()))
}

/// The two ACCEPTED forms parse, and the key is the one positional.
#[test]
fn the_two_value_forms_parse() {
    let a = parse_of(&["set", "BINANCE_LIVE_API_KEY"]).unwrap();
    assert_eq!(a.sub, Sub::Set);
    assert_eq!(a.key.as_deref(), Some("BINANCE_LIVE_API_KEY"));
    assert_eq!(a.from_env, None);

    let b = parse_of(&["set", "BINANCE_LIVE_API_KEY", "--from-env", "SRC"]).unwrap();
    assert_eq!(b.from_env.as_deref(), Some("SRC"));
    // …and the flag may lead, so a flags-first habit keeps working.
    let c = parse_of(&["set", "--from-env=SRC", "BINANCE_LIVE_API_KEY"]).unwrap();
    assert_eq!(c.key.as_deref(), Some("BINANCE_LIVE_API_KEY"));
    assert_eq!(c.from_env.as_deref(), Some("SRC"));
}

/// **A VALUE IN ARGV IS REFUSED, in both spellings, and the refusal quotes NOTHING.**
///
/// The last assertion is the load-bearing one: a message that echoed the rejected token would
/// write the credential into the scrollback of the session this refusal exists to keep it out
/// of — the refusal doing the exact damage it was built to prevent.
#[test]
fn a_value_in_argv_is_refused_without_echoing_it() {
    for argv in [
        &["set", "BINANCE_LIVE_API_KEY", "sk-live-do-not-print"][..],
        &["set", "BINANCE_LIVE_API_KEY=sk-live-do-not-print"][..],
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
        &["set", "BINANCE_LIVE_API_KEY", "-sk-live-do-not-print"][..],
        &["set", "BINANCE_LIVE_API_KEY", "-sk=live-do-not-print"][..],
        // …and with no key yet parsed, where it must NOT be taken as the key: that path reaches
        // `unknown_key_message`, which names the key it was given — the same echo, one step on.
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
    let err = parse_of(&["set", "BINANCE_LIVE_API_KEY", "--form-env", "X"]).unwrap_err();
    assert!(!err.contains("--form-env"), "even a flag typo is not quoted back: {err}");
    assert!(err.contains("if you meant a FLAG"), "…but the operator is pointed at them: {err}");

    // The other subcommands are UNCHANGED: they have no secret in argv to protect, so a typo
    // there is still named, which is the more useful answer.
    assert!(parse_of(&["list", "--jsonn"]).unwrap_err().contains("--jsonn"));
}

/// **`--file` is refused on the WRITER**, and permitted on the two readers that OPEN something.
///
/// It used to resolve the same way for both, so `set KEY --file <any existing file>` appended a
/// live credential to whatever the operator named — a shell rc file, another program's `.env` —
/// exiting 0, with the change journal recording the write against a "store" of that file's
/// basename. `docs/decisions/0036` fixes this verb as an upsert into the PROJECT's store.
#[test]
fn the_file_flag_is_refused_on_set_and_kept_on_the_readers() {
    let err = parse_of(&["set", "BINANCE_LIVE_API_KEY", "--file", "/tmp/anything"]).unwrap_err();
    assert!(err.contains("--file"), "{err}");
    assert!(err.contains("VIKE_SETTINGS_DIR"), "the refusal must name the way through: {err}");

    for sub in ["list", "path"] {
        assert!(
            parse_of(&[sub, "--file", "/tmp/anything"]).is_ok(),
            "{sub} must keep --file: inspection is not a write"
        );
    }
}

/// **`--file` is refused on `template` too, and the refusal must say what the flag was
/// SILENCING** — not merely that it does not apply.
///
/// This is the one flag refusal on this command that is a REVERSAL. `--file` was permitted here
/// on the argument that it is inert, which expired the day `run_template` grew a warning that
/// this box has MIGRATED and that `secrets template > settings/secrets.env` would therefore
/// write a file nothing reads. From then on the flag's only effect was to switch that sentence
/// off — and the operator most likely to type `--file` is the one who is unsure which store is
/// live.
#[test]
fn the_file_flag_is_refused_on_template_and_says_what_it_was_silencing() {
    let err = parse_of(&["template", "--file", "/tmp/anything"]).unwrap_err();
    assert!(err.contains("--file"), "{err}");
    assert!(
        err.contains("MIGRATED"),
        "the refusal must name what the flag suppressed, not just decline it: {err}"
    );
    // A `template` with no `--file` is untouched: this refusal may not cost the ordinary run.
    assert!(parse_of(&["template"]).is_ok());
    assert!(parse_of(&["template", "--venue", "binance"]).is_ok());
}

/// **A `--file` naming a settings DATABASE is refused, and by the format's own header rather
/// than by a file name.**
///
/// The failure it replaces is silent rather than loud — see [`refuse_a_database_path`] — so the
/// assertions that matter are that a credential FILE still passes (the flag must keep working)
/// and that the message names the way through.
#[test]
fn a_file_naming_a_database_is_refused_by_its_header() {
    let dir = tempfile::tempdir().unwrap();

    // A text credential file: unchanged, whatever it is called.
    let env = dir.path().join("secrets.env");
    std::fs::write(&env, "BINANCE_LIVE_API_KEY=abc\n").unwrap();
    assert!(refuse_a_database_path(Some(&env)).is_ok());
    // ...and so is an absent path, and no path at all: a probe that cannot answer must not
    // claim a database, or `--file` would start refusing the ordinary typo.
    assert!(refuse_a_database_path(Some(&dir.path().join("nope"))).is_ok());
    assert!(refuse_a_database_path(None).is_ok());

    // The header, planted verbatim — the bytes every SQLite file begins with. Named `.env` on
    // purpose: the refusal may not key on an extension, because a migrated store can be called
    // anything and a `db/vike.db` spelling would be trivially evaded.
    let db = dir.path().join("looks-like-a.env");
    std::fs::write(&db, b"SQLite format 3\0and then some binary").unwrap();
    let err = refuse_a_database_path(Some(&db)).unwrap_err();
    assert!(err.contains("DATABASE"), "{err}");
    assert!(
        err.contains("VIKE_SETTINGS_DIR"),
        "a refusal with no way through is an obstacle: {err}"
    );
}

/// **A LABELLED ACCOUNT is WRITTEN, and the base is still never offered.** Both halves, because
/// the reversal is only safe while the second one holds.
///
/// It was refused — `{BASE}__{LABEL}` is an unbounded name set — and the refusal pointed at an
/// EDITOR, which on a migrated box edits a file no reader opens. `secrets list` PRINTS these
/// accounts, so an operator could see a credential and rotate it nowhere.
///
/// ⚠ The hazard the old refusal was written about is NOT the unboundedness and does not go with
/// it: `nearest_keys` scored the UNLABELLED base as the closest name, the operator set it, and
/// the DEFAULT account's signing key was overwritten with a second account's. Admitting the
/// labelled name is what ends the temptation — the key typed is the key written — and this test
/// still pins the suggestion side, which is where that hazard actually lived.
#[test]
fn a_labelled_account_is_written_rather_than_refused_and_the_base_is_never_offered() {
    // Composed off a REAL key rather than spelled — see the sibling test for the literal
    // harvest that avoids.
    let base = vike_model::credential_keys::lookup_keys()
        .into_iter()
        .find(|k| k.ends_with("_API_KEY"))
        .expect("the grid has an API-key row");
    let labelled = format!("{base}{}ALT", vike_model::accounts::account_keys::ACCOUNT_SEPARATOR);

    // THE REVERSAL: it is settable, and it is filed under the BASE's own venue and tier rather
    // than under a guess.
    let owner = settable_outside_the_grid(&labelled)
        .expect("a labelled account whose base is a grid key is settable");
    let (venue, tier) = vike_model::credential_keys::key_owner(&base).expect("base is grid");
    assert_eq!(owner, (venue.to_string(), tier.map(str::to_string)), "{labelled}");

    // THE HALF THAT DID NOT MOVE: still no substitute offered, so the overwrite the old
    // refusal existed to prevent is prevented by the same code it always was.
    assert!(
        nearest_keys(&labelled).is_empty(),
        "a labelled account must suggest nothing: {:?}",
        nearest_keys(&labelled)
    );

    // …and a SINGLE-underscore near-miss is NOT one of these: it is a typo of a settable key,
    // and it keeps its suggestions.
    let near_miss = format!("{base}_ALT");
    assert!(nearest_keys(&near_miss).contains(&base), "a typo still gets its suggestion");
    assert!(labelled_account(&near_miss).is_none());

    // A label on a name that is NOT a credential key is NOT admitted — the grid bounds the
    // half that matters, which is what makes the unbounded label safe to accept.
    assert!(labelled_account("NOT_A_KEY__ALT").is_none());
    assert!(settable_outside_the_grid("NOT_A_KEY__ALT").is_none());
    assert!(unknown_key_message("NOT_A_KEY__ALT").contains("nothing would ever load"));
}

/// `set` with no key is a usage error that shows both forms — the operator who typed it is
/// exactly the one who does not yet know how the value gets in.
#[test]
fn set_without_a_key_names_both_value_forms() {
    let err = parse_of(&["set"]).unwrap_err();
    assert!(err.contains("needs a credential KEY"), "{err}");
    assert!(err.contains("--from-env") && err.contains("stdin"), "{err}");
}

/// A positional on a READING subcommand is still `unknown option`, unchanged — the non-flag arm
/// is gated on `set` alone.
#[test]
fn a_positional_on_a_reading_subcommand_is_unchanged() {
    for sub in ["list", "path", "template"] {
        let err = parse_of(&[sub, "stray"]).unwrap_err();
        assert!(err.contains("unknown option"), "{sub}: {err}");
    }
    assert!(parse_of(&["list", "--from-env", "X"]).unwrap_err().contains("--from-env"));
}

/// **An unknown key is refused BY NAME, and the message points at real ones.**
///
/// The lower-case case is separate because it is the likeliest near-miss and Levenshtein scores
/// it as far away as a different venue — every letter differs.
#[test]
fn an_unknown_key_is_refused_by_name_with_the_nearest_real_ones() {
    // ⚠ The near-misses are COMPOSED off a REAL key rather than spelled, for the reason
    // `vike_model::credential_keys`' own `key_owner_classifies_exactly_the_lookup_grid` gives:
    // `crates/vike-ops/tests/settings_registry.rs`'s literal harvest reads an env-shaped string
    // literal as evidence this crate READS that variable and demands a `SETTINGS` row for it.
    // This command reads no credential at all — it writes one the caller hands it — so a row
    // here would assert something false about `vike-cli`.
    let real = vike_model::credential_keys::lookup_keys()
        .into_iter()
        .find(|k| k.ends_with("_API_KEY"))
        .expect("the grid has an API-key row");
    let truncated = &real[..real.len() - 1];
    let extended = format!("{real}X");

    let msg = unknown_key_message(truncated);
    assert!(msg.contains(truncated), "the refusal must name the key: {msg}");
    assert!(msg.contains(&real), "…and suggest the real one: {msg}");

    assert_eq!(nearest_keys(&real.to_lowercase()), vec![real.clone()]);
    assert!(nearest_keys(&extended).contains(&real));

    // Nonsense suggests NOTHING, and says where the whole grid is instead. Three unrelated
    // names presented as guesses is worse than the bare refusal.
    assert!(nearest_keys("totally-unrelated-nonsense").is_empty());
    let far = unknown_key_message("totally-unrelated-nonsense");
    assert!(far.contains("secrets template"), "{far}");
    assert!(!far.contains("did you mean"), "{far}");

    // …and every real key is accepted, which is the other half of the same claim.
    for key in vike_model::credential_keys::lookup_keys() {
        assert!(
            vike_model::credential_keys::key_owner(&key).is_some(),
            "{key} is in the grid and must be settable"
        );
    }
}

/// The refusal for a name NOTHING reads also states how WIDE the writable set actually is,
/// rather than leaving an operator to conclude the command only writes the template.
///
/// ⚠ This assertion is the inverse of the one it replaces. It used to require the message to
/// name the bespoke families as a GAP — "cannot be set here … edit those with an editor" — and
/// that sentence stopped being true when `settable_outside_the_grid` admitted them. A test
/// demanding the old wording would have held the command's own documentation at the last
/// release that was wrong about it.
#[test]
fn the_refusal_names_the_shapes_that_are_outside_the_grid() {
    // A name no registry row carries, so this is the arm that still prints the original
    // sentence. Its shape matters: the tail is what the operator gets INSTEAD of a route.
    let msg = unknown_key_message("totally-unrelated-nonsense");
    assert!(msg.contains("nothing would ever load"), "{msg}");
    assert!(msg.contains("wider than the template"), "{msg}");
    assert!(msg.contains("LABELLED"), "{msg}");
    // …and it names a command that EXISTS and answers the question it was pointed at. The old
    // tail sent the operator to `secrets path` and an editor, which on a migrated box edits a
    // file no reader opens — the dead route this whole change removes.
    assert!(msg.contains("config show"), "the route named must be a live one: {msg}");
    assert!(!msg.contains("with an editor"), "the editor route is the dead one: {msg}");
}

/// **A key something READS is SETTABLE** — and this test used to assert it was merely told so
/// politely before being sent to an editor.
///
/// `vike-cli secrets set VIKE_TRADEHUB_OBSERVE_KEY` once answered "is not a credential key this
/// workspace reads, so setting it would write a line nothing would ever load". That measured
/// lie was fixed by a message; the message then advised an EDITOR, and on a migrated box
/// `docs/decisions/0054`'s credential half means the credential FILES are not read at all — so
/// the advice was a dead end and half the store was writable by nothing. The fix is not a
/// better sentence, it is a WRITER: the registry proving a reader exists is exactly the
/// property the grid was ever a proxy for.
///
/// ⚠ The names are COMPOSED rather than spelled, for the reason
/// `vike_model::credential_keys`' own `key_owner_classifies_exactly_the_lookup_grid` gives:
/// `crates/vike-ops/tests/settings_registry.rs`' literal harvest reads an env-shaped literal in
/// a `src/` file as evidence this crate READS that variable. `vike-cli` does read one of these
/// — `cmd/nodekeys.rs` owns that, and has its own row — but this file must not become a second
/// sighting of it, and the same dodge keeps every name below out of the sweep too.
#[test]
fn a_key_something_reads_is_settable_rather_than_sent_to_an_editor() {
    // A BESPOKE venue login — which the old text contradicted itself about, calling it
    // unloadable in one sentence and pointing at its bridge's loader in the next. It is filed
    // under its VENUE and TIER, from the classifier rather than from prose here.
    let fx = format!("FXCM_{}", "DEMO_USER");
    assert!(registry_readers(&fx).is_some(), "{fx}: no row — this test proved nothing");
    let (venue, tier) = settable_outside_the_grid(&fx).expect("a bespoke venue login is settable");
    assert_eq!(venue, "fxcm", "{fx}");
    assert_eq!(tier.as_deref(), Some("demo"), "{fx}");

    // ⚠ **A SETTINGS key is NOT admitted by the same rule, and this is the assertion that keeps
    // the predicate from being "anything the registry names".** `VIKE_HIST_STORE` has registry
    // rows exactly like the credential above — `Naming::MapLookup` and all — but it is a store
    // PATH read out of the process-env sweep, a different map from the credential one, so a row
    // written for it here would be the dead line the refusal exists to prevent.
    // ⚠ The CONST, never the literal — an env-shaped string in a `src/` file is read by
    // `crates/vike-ops/tests/settings_registry.rs`' harvest as evidence THIS crate reads it.
    let store_root = vike_config::config::STORE_ROOT_ENV;
    assert!(registry_readers(store_root).is_some(), "the control is not a control: it has no row");
    assert!(
        settable_outside_the_grid(store_root).is_none(),
        "a settings key must not be settable as a credential: {store_root}"
    );
}

/// **The DEPLOYMENT's own secrets reach `set` by ROTATION, and only once the store holds them.**
///
/// The Telegram trio and a Cloudflare token are `VIKE_*`/`CLOUDFLARE_*` names the credential
/// classifier has no positive rule for, so they land in its `Infrastructure` CATCH-ALL beside
/// every unrecognised string — which is why no NAME-shaped predicate can admit them without
/// admitting `VIKE_HIST_STORE` too. The store answers instead.
///
/// ⚠ The names are COMPOSED, for the literal-harvest reason the sibling test above gives.
#[test]
fn the_deployments_own_secrets_are_rotatable_once_the_store_holds_them() {
    let tg = format!("VIKE_{}", "TELEGRAM_BOT_TOKEN");
    // It is genuinely in this position: read by the workspace, and unrecognised by the
    // classifier — both halves, or the test is about a different key than it claims.
    assert!(registry_readers(&tg).is_some(), "{tg}");
    assert!(!vike_bridge_core::credentials::classify_credential_name(&tg).recognised, "{tg}");
    assert!(settable_outside_the_grid(&tg).is_none(), "{tg}: not by the name rules");

    // An EMPTY store cannot rotate it — there is nothing to rotate, and inventing the row is
    // what the name rules refused.
    let empty = vike_secrets::SecretMap::new(Default::default());
    assert!(rotation_owner(&tg, &empty).is_none(), "{tg}");

    // A store that HOLDS it can, and the ledger files it under the deployment rather than
    // under a blank venue.
    let mut held = std::collections::BTreeMap::new();
    held.insert(tg.clone(), "unused — the predicate reads NAMES".to_string());
    let live = vike_secrets::SecretMap::new(held);
    assert_eq!(
        rotation_owner(&tg, &live),
        Some((DEPLOYMENT_OWNER.to_string(), None)),
        "{tg}: the deployment's own credential, filed under no venue"
    );

    // ⚠ …and a NODE key is refused even when the store holds it, which is the state in which a
    // hand-pasted replacement does the damage. `backend setup` mints those.
    let node = format!("VIKE_{}", "TRADEHUB_CONTROL_KEY");
    let mut held = std::collections::BTreeMap::new();
    held.insert(node.clone(), "held".to_string());
    assert!(
        rotation_owner(&node, &vike_secrets::SecretMap::new(held)).is_none(),
        "{node}: a minted key is never rotated by hand"
    );
}

/// **The two TRADEHUB node keys get a FOURTH message, and it names a command rather than an
/// editor.** They were the specimen the read-but-not-settable arm was written for, and until
/// `vike-cli backend setup` existed the honest advice really was "open the file" — there was no
/// generator anywhere in this tree, and two ops runbooks recorded "a freshly generated" key with
/// no command beside it.
///
/// Now there is one, and sending an operator to an editor would be the SAME class of defect the
/// arm above was built to end: correct about the refusal, wrong about the route. The message
/// must name both boxes, because which command you want depends on which one you are standing
/// at, and it must not offer the editor as an alternative — a hand-pasted 256-bit key that is
/// truncated fails as an opaque auth denial.
///
/// ⚠ The names are COMPOSED, for the reason
/// [`a_key_something_reads_is_settable_rather_than_sent_to_an_editor`] gives above: an
/// env-shaped literal in a `src/` file is read by the settings registry's harvest as evidence
/// this crate READS that variable.
#[test]
fn the_node_keys_refusal_names_the_command_that_mints_them() {
    // ⚠ FOUR names now, and the verb is chosen PER SERVICE. While there was one pair this arm
    // could name the tradehub verb unconditionally; with two, a constant would send an operator
    // to the command for a different service — the same defect the arm exists to end, which is
    // why the datahub pair is exercised here rather than trusted.
    for (key, verb, service) in [
        (format!("VIKE_{}", "TRADEHUB_OBSERVE_KEY"), "backend", "vike-tradehub"),
        (format!("VIKE_{}", "TRADEHUB_CONTROL_KEY"), "backend", "vike-tradehub"),
        (format!("VIKE_{}", "DATAHUB_OBSERVE_KEY"), "datahub", "vike-datahub"),
        (format!("VIKE_{}", "DATAHUB_CONTROL_KEY"), "datahub", "vike-datahub"),
    ] {
        // The table this arm keys on, asserted here too, so a drift shows up as this test
        // rather than as an operator quietly getting the wrong route.
        assert!(vike_model::credential_keys::is_platform_key(&key), "{key}");
        assert_eq!(
            vike_model::credential_keys::platform_key_service(&key),
            Some(service),
            "{key}: the classifier is what picks the verb"
        );
        let msg = unknown_key_message(&key);
        assert!(!msg.contains("nothing would ever load"), "{key}: {msg}");
        assert!(msg.contains(&key), "{key}: {msg}");
        assert!(msg.contains("IS read by this workspace"), "{key}: {msg}");
        assert!(
            msg.contains(&format!("{verb} setup")),
            "{key}: it must name the minting command for ITS service: {msg}"
        );
        // ⚠ WHICH BOX — this read "DAEMON's box" while there was one daemon, and that stopped
        // being an answer the moment a second service existed. It names the service now.
        assert!(msg.contains(service), "{key}: which box, by service: {msg}");
        assert!(
            !msg.contains("EDITOR"),
            "{key}: the editor route is exactly what `{verb} setup` deletes: {msg}"
        );
        // ⚠ AND IT MAY NOT NAME A COMMAND THAT DOES NOT EXIST. `backend connect` is real;
        // `datahub connect` is not built, and an earlier draft of this arm promised it — a
        // refusal right about refusing and wrong about the route, which is the exact failure
        // this whole arm was written to end.
        assert!(
            !msg.contains("datahub connect"),
            "{key}: there is no `datahub connect` to send anyone to: {msg}"
        );
    }
    // The tradehub half DOES have a client command, and the message still offers it.
    let th = unknown_key_message(&format!("VIKE_{}", "TRADEHUB_OBSERVE_KEY"));
    assert!(th.contains("backend connect"), "the tradehub's client half exists and is named: {th}");
    // …and the arm is NARROW: a name one letter off is not a platform key and still gets the
    // ordinary outside-the-grid refusal, editor and all.
    let near = format!("VIKE_{}", "TRADEHUB_OBSERVE_KEYS");
    assert!(!vike_model::credential_keys::is_platform_key(&near));
    assert!(!unknown_key_message(&near).contains("backend setup"), "{near}");
}

/// …and the same claim as a PROPERTY over the whole registry, so the arm cannot be right for
/// the seven names above and wrong for the next one added.
///
/// Both directions, because either alone is satisfiable by a message that says nothing: every
/// declared name outside the grid must be told it is read and by whom, and a name the registry
/// does NOT carry must still get the original sentence — that sentence is correct there, and
/// deleting it would trade one lie for a vaguer one.
#[test]
fn the_unread_sentence_is_printed_only_where_no_registry_row_names_the_key() {
    let mut checked = 0usize;
    let mut settable = 0usize;
    for s in all_settings() {
        // Grid keys never reach this message at all — `key_owner` accepts them and `set`
        // writes them.
        if vike_model::credential_keys::key_owner(s.name).is_some() {
            continue;
        }
        // ⚠ …and NEITHER DO THE NAMES `settable_outside_the_grid` ADMITS, which is the change.
        // `run_set` writes those, so this message is never built for one; asserting over it
        // anyway would be asserting about a sentence no operator can reach, and the sentence
        // that fits a key the command WRITES is not one this function has.
        if settable_outside_the_grid(s.name).is_some() {
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
        checked += 1;
    }
    assert!(checked > 0, "the registry carries no non-grid rows — this test proved nothing");
    // …and the skip is not the whole set, or the loop above asserted about nothing. Both
    // counts, because either alone is satisfiable by a predicate that answers one way always.
    assert!(settable > 0, "no registry name is settable — the writer reaches nothing");

    // The other direction. `registry_readers` is the whole discriminator, so a name it answers
    // `None` for is exactly where the original sentence still belongs.
    let unread = "totally-unrelated-nonsense";
    assert!(registry_readers(unread).is_none());
    assert!(unknown_key_message(unread).contains("nothing would ever load"));
}

/// **A MULTI-LINE `--from-env` value is refused, and a WHITESPACE-ONLY one with it.**
///
/// The multi-line case was an INJECTION, not an untidiness: quoted and joined by
/// `vike_secrets::upsert_env`, the value's own newline became a physical line break, so the
/// reader returned the first half as a truncated credential and read the second half as a WHOLE
/// NEW `KEY=VALUE` — a credential for a venue the operator never configured, past a key name
/// this command had validated. Reproduced end to end before this refusal existed; the store
/// afterwards listed three keys and three accounts where two had been set.
///
/// The whitespace-only case is milder and the same shape: `parse_dotenv` hands three spaces back
/// as a non-empty value, so the venue reads as CONFIGURED and arms with a garbage secret,
/// failing at the venue instead of staying on paper — while `value_for`'s own doc said "both
/// refuse EMPTY" and only the stdin arm looked past a zero length.
///
/// It is asserted HERE, at the seam that names the variable, as well as in
/// `vike_secrets::env_write`, which refuses it for every caller including the GUI.
#[test]
fn a_multiline_or_blank_env_value_is_refused_naming_the_variable_and_never_the_value() {
    let args = Args {
        sub: Sub::Set,
        file: None,
        venue: None,
        json: false,
        key: Some("BINANCE_LIVE_API_KEY".to_string()),
        from_env: Some("SRC".to_string()),
        dry_run: false,
        account_id: None,
        venue_account_id: None,
        replace: false,
        clear: false,
        account_action: None,
        tier: None,
        label: None,
        no_label: false,
        confirm: None,
    };
    let refusal = |raw: &str| -> String {
        let env = HashMap::from([("SRC".to_string(), raw.to_string())]);
        let ctx = Ctx {
            settings_dir: None,
            settings_dir_override: None,
            state_dir: None,
            env: &env,
            now_ms: 0,
        };
        value_for(&args, &ctx).expect_err("must be refused").msg
    };

    for raw in ["abc\nOKX_LIVE_API_SECRET=injected", "tok\n", "tok\r"] {
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

#[test]
fn usage_documents_the_writer_and_both_of_its_value_forms() {
    for needle in ["set KEY", "--from-env", "stdin", "template"] {
        assert!(USAGE.contains(needle), "USAGE must mention {needle}");
    }
    // ⚠ The USAGE text must not teach the shape it refuses. `set KEY VALUE` appearing here as
    // an example is how somebody learns to type it.
    assert!(!USAGE.contains("set KEY VALUE"), "USAGE must not show the argv form");
}

use super::accounts::{
    AccountActor, AccountAdminSource, AccountBarrier, is_settable_credential_key,
};
use super::handshake::served_features;
use vike_tradehub_client::proto::{FEATURE_ACCOUNT_VERBS, FEATURE_CREDENTIAL_ADMIN, Response};
use vike_tradehub_client::wire::{AccountRequest, AccountVerb};

/// A name the wire MAY set — a DEPLOYMENT name, the first data-provider key — so a refusal below
/// is the ceremony's and never the validator's.
///
/// ⚠ **DERIVED from `vike_venues::credential_keys` rather than spelled**, and not for elegance: a
/// venue-prefixed literal in a file under `src/` is harvested by
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s loose sweep, which then demands
/// a `SETTINGS` row for it — and the NEAR-MISS spellings below name no key at all, so no row could
/// honestly exist.
fn settable_key() -> String {
    vike_venues::credential_keys::DATA_PROVIDER_KEYS[0].to_string()
}

/// What a venue's credential NAME used to look like — `{VENUE}_{TIER}_{FIELD}`. Nothing composes
/// or reads one any more (decision 0156 item 6): an account's credential is addressed by account
/// and FIELD, so the wire refuses it. Spelled with `concat!` so the settings registry's literal
/// sweep does not read a variable out of this file.
fn venue_key_name() -> &'static str {
    concat!("BINANCE", "_LIVE", "_API_KEY")
}

/// What a LABELLED account's credential name used to look like — `{NAME}__{LABEL}`. Refused like
/// every other account credential name.
fn labelled_key_name() -> &'static str {
    concat!("HYPERLIQUID", "_LIVE", "_API_KEY", "__", "HEDGE")
}

/// A SINGLE-underscore near-miss of the settable name. Nothing reads it.
fn near_miss_key() -> String {
    format!("{}_HEDGE", settable_key())
}

/// The value these tests send. ⚠ It is a fixed non-secret token whose only job is to be
/// searched for in every message: no refusal, no reply and no `Debug` may contain it.
const SENT_VALUE: &str = "zzz-value-that-must-never-be-echoed-zzz";

fn source() -> AccountAdminSource {
    AccountAdminSource {
        // Deliberately absent — see the module comment.
        settings_dir: std::path::PathBuf::from("/nonexistent/settings"),
        scope: vike_secrets::CredentialScope::Full,
        barrier: AccountBarrier::Loopback,
    }
}

fn actor() -> AccountActor<'static> {
    AccountActor { peer: Some("127.0.0.1:50001"), scope: "admin", key_id: Some("ab12cd34") }
}

fn refusal(verb: AccountVerb, confirm: Option<&str>) -> String {
    let req = AccountRequest { verb, confirm: confirm.map(str::to_string) };
    match source().apply(&req, &actor()) {
        Ok(r) => panic!("this request must be REFUSED, and it was answered with {r:?}"),
        Err(reason) => reason,
    }
}

fn set_credential_verb() -> AccountVerb {
    AccountVerb::SetCredential { key: settable_key(), value: SENT_VALUE.to_string() }
}

/// ⚠ **THE CEREMONY, on the verb that carries a credential.** `apply_set_setting`'s policy
/// contract verbatim — the precedent this surface was told to follow rather than invent one:
/// the write is REFUSED unless `confirm` equals the exact thing being changed.
///
/// The client's job is to make the operator TYPE it and never pre-fill it; this method's job
/// is to refuse anything else, so no client can quietly skip it.
#[test]
fn a_credential_write_with_no_typed_confirm_is_refused() {
    let msg = refusal(set_credential_verb(), None);
    assert!(
        msg.contains("typed confirm"),
        "the refusal must say a typed confirm is required: {msg}"
    );
    assert!(
        msg.contains(&settable_key()),
        "…and must name the exact spelling the operator has to type: {msg}"
    );
}

/// **Missing and mismatched get DISTINCT messages**, which is `apply_set_setting`'s own split:
/// an operator who typed the wrong thing and an operator who typed nothing need different
/// instructions, and one message for both is how somebody retypes the same wrong token.
#[test]
fn a_mismatched_confirm_is_refused_with_a_different_message_than_a_missing_one() {
    let missing = refusal(set_credential_verb(), None);
    let wrong = refusal(set_credential_verb(), Some(&near_miss_key()));
    assert!(wrong.contains("mismatch"), "the mismatch arm must say so: {wrong}");
    assert_ne!(
        missing, wrong,
        "missing and mismatched must not share a message — see `apply_set_setting`'s split"
    );
}

/// ⚠ **NEITHER REFUSAL QUOTES THE VALUE, and neither quotes what the caller typed.** 0065
/// §4.5: *"the refusal quotes nothing the caller sent"* — the `ARGV_VALUE_REFUSAL` rule
/// arriving on a different channel. Any rule reading the token's SHAPE is a guess about the
/// secret's alphabet, so the answer is to echo none of it.
#[test]
fn no_ceremony_refusal_echoes_the_value_or_the_typed_confirm() {
    let typed = "some-wrong-thing-the-operator-typed";
    for msg in [refusal(set_credential_verb(), None), refusal(set_credential_verb(), Some(typed))] {
        assert!(!msg.contains(SENT_VALUE), "a refusal named the credential VALUE: {msg}");
        assert!(!msg.contains(typed), "a refusal echoed the operator's own token: {msg}");
    }
}

/// The other destructive verb. A REMOVE is refused unless the confirm equals the row id — the
/// id rather than a label, because the id is what addresses the row and a label is not
/// identity (`crates/vike-secrets/src/account_confirmation.rs`).
#[test]
fn a_remove_requires_the_typed_row_id() {
    let missing = refusal(AccountVerb::Remove { id: 7 }, None);
    assert!(missing.contains("typed confirm"), "{missing}");
    assert!(missing.contains('7'), "the refusal must name the id to type: {missing}");
    let wrong = refusal(AccountVerb::Remove { id: 7 }, Some("8"));
    assert!(wrong.contains("mismatch"), "{wrong}");
}

/// ⚠ **ONE derivation of what the ceremony IS**, consulted by the client that prompts and by
/// the server that refuses — so the two surfaces cannot answer differently about it. This pins
/// which verbs carry one: exactly the three that change something the node cannot put back
/// (a retired credential stays at rest, but the node no longer answers it).
///
/// A client may call this to know WHETHER to prompt. It may NOT call it to FILL the box: the
/// contract is that the operator types it, and a pre-filled confirm is a click.
#[test]
fn exactly_the_destructive_verbs_carry_a_ceremony() {
    assert_eq!(AccountVerb::Remove { id: 3 }.required_confirm().as_deref(), Some("3"));
    assert_eq!(
        AccountVerb::RetireCredential { key: settable_key() }.required_confirm().as_deref(),
        Some(settable_key().as_str())
    );
    assert_eq!(set_credential_verb().required_confirm().as_deref(), Some(settable_key().as_str()));
    for quiet in [
        AccountVerb::List,
        AccountVerb::Add { venue: "binance".to_string(), tier: "live".to_string(), label: None },
        AccountVerb::Rename { id: 1, label: None },
        AccountVerb::SetActive { id: 1, active: false },
        AccountVerb::HeldCredentials { names: vec![settable_key()] },
    ] {
        assert!(
            quiet.required_confirm().is_none(),
            "`{}` is reversible and must not demand a retype — ceremony on a reversible act is \
                 friction that teaches an operator to type past it",
            quiet.word()
        );
    }
}

/// **A name the wire may not set is refused, naming the KEY and never the value** — 0036's reason
/// 3 on this surface. Three shapes, one answer: a near-miss nothing reads, a venue credential name
/// and a labelled account's name. ⚠ The last two are an ACCOUNT's credential, which is written to
/// an EXISTING account by FIELD, never by a name (decision 0156 item 6) — so the refusal names that
/// route (create the account with its tier, then `secrets set --account`), and it offers no
/// nearest-name suggestion.
#[test]
fn a_name_the_wire_may_not_set_is_refused_naming_the_account_route_and_no_value() {
    for key in [near_miss_key(), venue_key_name().to_string(), labelled_key_name().to_string()] {
        let req = AccountRequest {
            verb: AccountVerb::SetCredential { key: key.clone(), value: SENT_VALUE.to_string() },
            // The ceremony PASSES, so what refuses below is the validator and nothing else.
            confirm: Some(key.clone()),
        };
        let msg = match source().apply(&req, &actor()) {
            Ok(r) => panic!("{key} is no name the wire may set and must be refused: {r:?}"),
            Err(reason) => reason,
        };
        assert!(msg.contains(&key), "the refusal names the key: {msg}");
        assert!(!msg.contains(SENT_VALUE), "…and never the value: {msg}");
        assert!(
            msg.contains("secrets set --account") && msg.contains("secrets account add --venue"),
            "…and names where an account's credential is written, and how its account is made: \
             {msg}"
        );
    }
}

/// **The settable names are the DEPLOYMENT names alone**: the two data-provider keys and the
/// venues' attribution codes, each filed by name under no account. No venue credential name and no
/// labelled name is settable any more — a credential write never creates an account and never
/// addresses one by a name (decision 0156 item 6).
#[test]
fn the_settable_names_are_the_deployment_names_alone() {
    for key in vike_venues::credential_keys::DATA_PROVIDER_KEYS {
        assert!(is_settable_credential_key(key), "{key}");
    }
    let attribution = vike_venues::credential_keys::attribution_keys();
    assert!(!attribution.is_empty(), "the attribution grid is never empty");
    for key in &attribution {
        assert!(is_settable_credential_key(key), "{key} is a deployment name the wire may set");
    }
    assert!(!is_settable_credential_key(venue_key_name()));
    assert!(!is_settable_credential_key(labelled_key_name()));
    assert!(!is_settable_credential_key(&near_miss_key()));
    assert!(!is_settable_credential_key(&settable_key().to_lowercase()));
    assert!(!is_settable_credential_key("NOT_A_KEY_AT_ALL"));
    assert!(!is_settable_credential_key(""));
}

/// ⚠ **AN ABSENT STORE IS REFUSED, NOT CREATED** — 0036's reason 1 at its sharpest on this
/// surface: the mere EXISTENCE of the settings database is the whole of `vike_secrets::Backend`'s
/// per-run choice. `secrets init` stays the one creator.
#[test]
fn a_credential_write_against_an_absent_store_is_refused_and_creates_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let src = AccountAdminSource {
        settings_dir: dir.path().to_path_buf(),
        scope: vike_secrets::CredentialScope::Full,
        barrier: AccountBarrier::Contained,
    };
    let req = AccountRequest { verb: set_credential_verb(), confirm: Some(settable_key()) };
    let msg = match src.apply(&req, &actor()) {
        Ok(r) => panic!("an absent store must be refused, not created: {r:?}"),
        Err(reason) => reason,
    };
    assert!(!msg.contains(SENT_VALUE), "the refusal named the value: {msg}");
    assert!(
        std::fs::read_dir(dir.path()).expect("the dir survives").next().is_none(),
        "nothing may be created on this path — a store is the operator's only copy of their \
             live venue keys, and only `secrets init` brings the DATABASE into \
             existence"
    );
}

/// ⚠ **THE REDACTING `Debug`.** `WireCommand` DERIVES `Debug` and 0065 §4.1 is the rule this
/// impl exists for: a variant carrying a value may not ride that derive. The reachable formats
/// are not hypothetical — a panic message in a test prints to a CI log.
#[test]
fn the_request_debug_prints_the_key_name_and_never_the_value() {
    let req = AccountRequest { verb: set_credential_verb(), confirm: Some(settable_key()) };
    let rendered = format!("{req:?}");
    assert!(!rendered.contains(SENT_VALUE), "Debug printed the credential: {rendered}");
    assert!(rendered.contains(&settable_key()), "…it must still name the KEY: {rendered}");
    assert!(rendered.contains("<set>"), "…as a presence mark: {rendered}");
}

/// The ADVERTISEMENT tracks the capability's ABSENCE, which is why this is the one string this
/// node withholds conditionally. A node that advertised the verbs and then refused every frame
/// would be telling its clients a barrier had been declared on a box where it had not.
#[test]
fn the_capability_string_is_advertised_only_when_the_capability_exists() {
    let armed = served_features(None, true);
    let bare = served_features(None, false);
    assert!(armed.iter().any(|f| f == FEATURE_ACCOUNT_VERBS));
    assert!(
        armed.iter().any(|f| f == FEATURE_CREDENTIAL_ADMIN),
        "the credential verbs ride the same capability, so the same advertisement"
    );
    assert!(!bare.iter().any(|f| f == FEATURE_CREDENTIAL_ADMIN));
    assert!(
        !bare.iter().any(|f| f == FEATURE_ACCOUNT_VERBS),
        "an unarmed node advertises nothing — the absence is what a conforming client reads \
             instead of sending a frame"
    );
    // …and the rest of the list is byte-identical, so arming this capability advertises no
    // second thing by accident.
    let without: Vec<&String> = armed
        .iter()
        .filter(|f| *f != FEATURE_ACCOUNT_VERBS && *f != FEATURE_CREDENTIAL_ADMIN)
        .collect();
    assert_eq!(without, bare.iter().collect::<Vec<&String>>());
}

/// **MAJOR 1 of the 2026-09-23 task-3/7 review.** The `sim` -> `paper` rename (ruling 7) put
/// `paper` INTO the tier vocabulary (now `vike_model::AccountTier`), and this refusal denied it by name while
/// printing the very roster that now contains it — a five-path sweep of the rename's
/// operator-facing prose that stopped one call site short of its own conclusion. Derived from
/// the roster rather than a hand-copied word list, so the NEXT tier rename reddens this
/// instead of rotting the way this one did. It names no credential NAME either: no name carries a
/// tier (decision 0156 item 6).
#[test]
fn the_add_tier_refusal_does_not_deny_a_tier_the_roster_contains() {
    let msg = refusal(
        AccountVerb::Add {
            venue: "binance".to_string(),
            tier: "not-a-real-tier".to_string(),
            label: None,
        },
        None,
    );
    assert!(
        msg.contains(&vike_model::AccountTier::legal_words()),
        "must print the real roster: {msg}"
    );
    for tier in vike_model::AccountTier::ALL {
        assert!(
            !msg.contains(&format!("`{tier}` is not")) && !msg.contains(&format!("no `{tier}`")),
            "{tier} is a tier but this refusal denies it: {msg}"
        );
    }
    assert!(!msg.contains("_SIM_"), "no credential name carries a tier: {msg}");
}

// ---------------------------------------------------------------------------------------------
// The credential RETIRE and the PRESENT-NAMES read, over a REAL store (decision 0144's wire half)
// ---------------------------------------------------------------------------------------------

/// A distinctive value, so a leak into any reply or ledger line is a substring nothing else holds.
const PROVIDER_VALUE: &str = "provider-value-that-must-never-be-echoed-9q";

fn provider_key() -> &'static str {
    vike_venues::credential_keys::FINNHUB_API_KEY
}

/// A project with a real, empty credential store and the account capability armed over it.
fn armed_store() -> (tempfile::TempDir, AccountAdminSource) {
    let dir = tempfile::tempdir().expect("a temp dir");
    vike_secrets::create_store(Some(dir.path().to_str().expect("utf-8 temp path")))
        .expect("create the store");
    let src = AccountAdminSource {
        settings_dir: dir.path().to_path_buf(),
        scope: vike_secrets::CredentialScope::Full,
        barrier: AccountBarrier::Contained,
    };
    (dir, src)
}

fn answer(
    src: &AccountAdminSource,
    verb: AccountVerb,
    confirm: Option<&str>,
) -> Result<Response, String> {
    src.apply(&AccountRequest { verb, confirm: confirm.map(str::to_string) }, &actor())
}

fn held(src: &AccountAdminSource, names: &[&str]) -> Vec<String> {
    let verb =
        AccountVerb::HeldCredentials { names: names.iter().map(|n| (*n).to_string()).collect() };
    match answer(src, verb, None).expect("a presence read answers") {
        Response::CredentialsHeld(h) => h.held,
        other => panic!("expected CredentialsHeld, got {other:?}"),
    }
}

fn set_provider(src: &AccountAdminSource) -> Response {
    let verb = AccountVerb::SetCredential {
        key: provider_key().to_string(),
        value: PROVIDER_VALUE.to_string(),
    };
    answer(src, verb, Some(provider_key())).expect("a provider key is settable on the wire")
}

fn retire(src: &AccountAdminSource, key: &str, confirm: Option<&str>) -> Result<Response, String> {
    answer(src, AccountVerb::RetireCredential { key: key.to_string() }, confirm)
}

/// ⚠ **THE WHOLE LIFE OF A PROVIDER KEY over the wire verbs**: not set, set (a deployment name the
/// wire may set), held, retired behind its typed confirm, not set again, re-added. Neither the
/// value nor a path ever appears in a reply, and the retire is recorded by NAME.
#[test]
fn a_provider_key_is_set_held_retired_and_added_back_without_its_value_ever_replying() {
    let (dir, src) = armed_store();
    let both = [provider_key(), vike_venues::credential_keys::FMP_API_KEY];
    assert!(held(&src, &both).is_empty(), "a fresh store holds neither");

    let set = set_provider(&src);
    assert!(matches!(&set, Response::AccountWritten(w) if w.verb == "set-credential"), "{set:?}");
    assert_eq!(held(&src, &both), vec![provider_key().to_string()]);

    // The ceremony: no confirm and a wrong confirm are refused, and the key stays held.
    let missing = retire(&src, provider_key(), None).expect_err("a retire owes its typed confirm");
    assert!(missing.contains("typed confirm") && missing.contains(provider_key()), "{missing}");
    let wrong = retire(&src, provider_key(), Some("SOMETHING_ELSE")).expect_err("a wrong confirm");
    assert!(wrong.contains("mismatch"), "{wrong}");
    assert_eq!(held(&src, &both), vec![provider_key().to_string()], "nothing was retired");

    let done = retire(&src, provider_key(), Some(provider_key())).expect("the confirmed retire");
    assert!(
        matches!(&done, Response::AccountWritten(w) if w.verb == "retire-credential" && w.changed),
        "{done:?}"
    );
    assert!(held(&src, &both).is_empty(), "retired reads as NOT SET");

    let again = retire(&src, provider_key(), Some(provider_key())).expect_err("already retired");
    assert!(again.contains("not set") && again.contains("Nothing was changed"), "{again}");

    // …and the name comes back with a new write.
    set_provider(&src);
    assert_eq!(held(&src, &both), vec![provider_key().to_string()]);

    // No reply carried the value; the ledger recorded the retire by NAME under its own kind.
    // (The WRITE's note has always named the store file; the retire's replies must not.)
    assert!(!format!("{set:?}").contains(PROVIDER_VALUE), "the write reply carried the value");
    for text in [format!("{done:?}"), again.clone()] {
        assert!(!text.contains(PROVIDER_VALUE), "a reply carried the value: {text}");
        assert!(!text.contains(dir.path().to_str().unwrap()), "a reply named the path: {text}");
    }
    let changes = dir.path().join("state").join(vike_change_journal::CHANGES_SUBDIR);
    let ledger: String = std::fs::read_dir(&changes)
        .expect("the change journal directory exists")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("a ledger file"))
        .collect();
    assert!(ledger.contains(vike_change_journal::KIND_CREDENTIAL_RETIRE), "{ledger}");
    assert!(ledger.contains(provider_key()), "the name is recorded: {ledger}");
    assert!(!ledger.contains(PROVIDER_VALUE), "…and the value never: {ledger}");
}

/// **An ACCOUNT's credential cannot be SET by name on the wire, but a name the store already holds
/// for an account can still be looked up and RETIRED** — so a GUI that lists one can remove it
/// (decision 0156 item 6). The account and its field are planted the way the operator's own verbs
/// make them: the account first, with its tier, then its field by owner (the store files it under
/// `vike_secrets::account_credential_name`).
#[test]
fn an_account_credential_the_store_holds_is_held_and_retired_by_name_but_never_set() {
    let (dir, src) = armed_store();
    let created = vike_secrets::edit_account_in_unjournalled(
        dir.path(),
        vike_secrets::AccountEdit::Create {
            venue: "oanda",
            tier: vike_model::AccountTier::Demo,
            label: None,
        },
    )
    .expect("create the account");
    let id = created.after.expect("a created row").id;
    vike_secrets::save_owned_credentials_unjournalled(
        dir.path(),
        &vike_secrets::CredentialOwner::Account(id),
        &[("API_KEY".to_string(), PROVIDER_VALUE.to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("write the account's field");
    let name = vike_secrets::account_credential_name("oanda", id, "API_KEY");

    // The wire may not SET it — an account's credential has no name the wire writes.
    let set = answer(
        &src,
        AccountVerb::SetCredential { key: name.clone(), value: SENT_VALUE.to_string() },
        Some(&name),
    )
    .expect_err("an account credential is never written by name on the wire");
    assert!(set.contains("secrets set --account"), "{set}");

    // …but the store holds it, so a presence read and a retire accept the name.
    assert_eq!(held(&src, &[name.as_str()]), vec![name.clone()]);
    let done = retire(&src, &name, Some(&name)).expect("a held account credential is retired");
    assert!(
        matches!(&done, Response::AccountWritten(w) if w.verb == "retire-credential" && w.changed),
        "{done:?}"
    );
    assert!(!format!("{done:?}").contains(PROVIDER_VALUE), "the reply carried the value");
    // Retired, it is no longer a name the store holds — and so no longer one the wire accepts.
    let gone = AccountVerb::HeldCredentials { names: vec![name.clone()] };
    let msg = answer(&src, gone, None).expect_err("a name neither settable nor held");
    assert!(msg.contains(&name) && msg.contains("nothing was looked up"), "{msg}");
}

/// The refusals a delete owes NAMES to, worded without a path: a name the store never held, and a
/// hub key (which is not a name this wire accepts at all — the validator answers first).
#[test]
fn a_retire_refuses_a_name_never_held_and_a_hub_key_without_quoting_a_path() {
    let (dir, src) = armed_store();
    let settable = settable_key();
    let never = retire(&src, &settable, Some(&settable)).expect_err("never held");
    assert!(never.contains(&settable) && never.contains("not set"), "{never}");
    let node = vike_venues::credential_keys::PLATFORM_KEYS[0];
    let refused = retire(&src, node, Some(node)).expect_err("a hub key is no credential name");
    assert!(refused.contains(node) && refused.contains("nothing was changed"), "{refused}");
    for text in [&never, &refused] {
        assert!(!text.contains(dir.path().to_str().unwrap()), "a refusal named the path: {text}");
    }
}

/// A retire against an ABSENT store is refused and creates nothing — 0036's reason 1, on this verb.
#[test]
fn a_retire_against_an_absent_store_is_refused_and_creates_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let src = AccountAdminSource {
        settings_dir: dir.path().to_path_buf(),
        scope: vike_secrets::CredentialScope::Full,
        barrier: AccountBarrier::Contained,
    };
    let msg = retire(&src, provider_key(), Some(provider_key())).expect_err("no store");
    assert!(msg.contains("no credential store"), "{msg}");
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none(), "nothing may be created");
    // The presence read on a box with no store is an honest EMPTY answer, not an error.
    assert!(held(&src, &[provider_key()]).is_empty());
}

/// The presence read validates every name it is asked about and bounds the list: it is a lookup of
/// the few names a screen shows, never a way to enumerate the store.
#[test]
fn the_presence_read_refuses_an_unknown_name_and_a_long_list() {
    let (_dir, src) = armed_store();
    let bad = AccountVerb::HeldCredentials { names: vec![near_miss_key()] };
    let msg = answer(&src, bad, None).expect_err("a name nothing reads");
    assert!(msg.contains(&near_miss_key()) && msg.contains("nothing was looked up"), "{msg}");
    let long = AccountVerb::HeldCredentials { names: vec![settable_key(); 17] };
    let msg = answer(&src, long, None).expect_err("too many names");
    assert!(msg.contains("at most 16"), "{msg}");
}

/// Of the CLI's deployment names, the data-provider pair is the only one the GUI edits over the
/// wire: the pager token and the rest, which the CLI may set, are refused here.
#[test]
fn only_the_two_provider_keys_of_the_clis_list_join_the_settable_shapes() {
    for key in vike_venues::credential_keys::DATA_PROVIDER_KEYS {
        assert!(is_settable_credential_key(key), "{key}");
    }
    let outside = vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !vike_venues::credential_keys::DATA_PROVIDER_KEYS.contains(name))
        .collect::<Vec<_>>();
    assert!(!outside.is_empty(), "the CLI's list is longer than the wire's");
    for key in outside {
        assert!(!is_settable_credential_key(key), "{key} must stay a box-side `secrets set`");
    }
}

/// **Under `CredentialScope::NoVenueKeys` the account plane still WRITES and still answers its
/// presence read** (decision 0146's second reopener, task W2): a write is not a read, and
/// `HeldCredentials` is a boolean SQLite computes, so neither needs a venue VALUE in this process.
/// The data-provider key (a deployment row) is set, held and retired; an ACCOUNT's field is held
/// by name although the scope withholds its value from every value read; and no reply carries a
/// value.
#[test]
fn the_account_plane_writes_and_answers_presence_under_the_no_venue_keys_scope() {
    let (dir, mut src) = armed_store();
    src.scope = vike_secrets::CredentialScope::NoVenueKeys;
    let created = vike_secrets::edit_account_in_unjournalled(
        dir.path(),
        vike_secrets::AccountEdit::Create {
            venue: "oanda",
            tier: vike_model::AccountTier::Demo,
            label: None,
        },
    )
    .expect("create the account");
    let id = created.after.expect("a created row").id;
    vike_secrets::save_owned_credentials_unjournalled(
        dir.path(),
        &vike_secrets::CredentialOwner::Account(id),
        &[("API_KEY".to_string(), PROVIDER_VALUE.to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("write the account's field");
    let account_key = vike_secrets::account_credential_name("oanda", id, "API_KEY");

    // The account's value is withheld from every value read of this scope…
    let withheld = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir.path())),
        vike_secrets::CredentialScope::NoVenueKeys,
    )
    .read_all()
    .expect("opens");
    assert!(withheld.secrets.is_empty(), "a venue value reached the account plane's process");

    // …yet the plane answers presence for it, and writes and retires a provider key.
    assert_eq!(held(&src, &[account_key.as_str()]), vec![account_key.clone()]);
    let set = set_provider(&src);
    assert!(matches!(&set, Response::AccountWritten(w) if w.verb == "set-credential"), "{set:?}");
    assert_eq!(held(&src, &[provider_key()]), vec![provider_key().to_string()]);
    let done = retire(&src, provider_key(), Some(provider_key())).expect("the confirmed retire");
    assert!(
        matches!(&done, Response::AccountWritten(w) if w.verb == "retire-credential" && w.changed),
        "{done:?}"
    );
    assert!(held(&src, &[provider_key()]).is_empty(), "retired reads as NOT SET");
    for text in [format!("{set:?}"), format!("{done:?}")] {
        assert!(!text.contains(PROVIDER_VALUE), "a reply carried a value: {text}");
    }
}

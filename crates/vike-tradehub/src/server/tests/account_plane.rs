use super::accounts::{
    AccountActor, AccountAdminSource, AccountBarrier, is_settable_credential_key,
};
use super::handshake::served_features;
use vike_tradehub_client::proto::FEATURE_ACCOUNT_VERBS;
use vike_tradehub_client::wire::{AccountRequest, AccountVerb};

/// A key name that IS in `vike_model::credential_keys` — so a refusal below is the ceremony's
/// and never the validator's.
///
/// ⚠ **DERIVED from the grid rather than spelled**, and not for elegance: a venue-prefixed
/// literal in a file under `src/` is harvested by `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
/// loose sweep, which then demands a `SETTINGS` row for it — and the NEAR-MISS spellings below
/// name no key at all, so no row could honestly exist. Taking the first grid entry also keeps
/// these tests true when the grid is reordered.
fn real_key() -> String {
    vike_model::credential_keys::lookup_keys()
        .first()
        .expect("the credential grid is never empty")
        .clone()
}

/// `{BASE}__{LABEL}` — a LABELLED account's key, the second settable shape.
fn labelled_key() -> String {
    format!("{}{}{}", real_key(), vike_model::accounts::account_keys::ACCOUNT_SEPARATOR, "HEDGE")
}

/// A SINGLE-underscore near-miss of the same base. Nothing reads it, and the separator is a
/// DOUBLE underscore, so it is correctly outside the labelled grammar.
fn near_miss_key() -> String {
    format!("{}_HEDGE", real_key())
}

/// The value these tests send. ⚠ It is a fixed non-secret token whose only job is to be
/// searched for in every message: no refusal, no reply and no `Debug` may contain it.
const SENT_VALUE: &str = "zzz-value-that-must-never-be-echoed-zzz";

fn source() -> AccountAdminSource {
    AccountAdminSource {
        // Deliberately absent — see the module comment.
        settings_dir: std::path::PathBuf::from("/nonexistent/settings"),
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
    AccountVerb::SetCredential { key: real_key(), value: SENT_VALUE.to_string() }
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
        msg.contains(&real_key()),
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
/// identity (`crates/vike-model/src/accounts/account_confirmation.rs`).
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
/// which verbs carry one: exactly the two that change something the node cannot put back.
///
/// A client may call this to know WHETHER to prompt. It may NOT call it to FILL the box: the
/// contract is that the operator types it, and a pre-filled confirm is a click.
#[test]
fn exactly_the_two_destructive_verbs_carry_a_ceremony() {
    assert_eq!(AccountVerb::Remove { id: 3 }.required_confirm().as_deref(), Some("3"));
    assert_eq!(set_credential_verb().required_confirm().as_deref(), Some(real_key().as_str()));
    for quiet in [
        AccountVerb::List,
        AccountVerb::Add { venue: "binance".to_string(), tier: "live".to_string(), label: None },
        AccountVerb::Rename { id: 1, label: None },
        AccountVerb::SetActive { id: 1, active: false },
    ] {
        assert!(
            quiet.required_confirm().is_none(),
            "`{}` is reversible and must not demand a retype — ceremony on a reversible act is \
                 friction that teaches an operator to type past it",
            quiet.word()
        );
    }
}

/// A key outside the grid is refused, naming the KEY and never the value — 0036's reason 3,
/// reused on this surface verbatim. ⚠ The refusal deliberately offers NO nearest-name
/// suggestions: the nearest name to `HYPERLIQUID_LIVE_API_KEY__ALT` is a DIFFERENT ACCOUNT's
/// live key.
#[test]
fn a_key_outside_the_grid_is_refused_and_names_no_value() {
    let req = AccountRequest {
        verb: AccountVerb::SetCredential { key: near_miss_key(), value: SENT_VALUE.to_string() },
        // The ceremony PASSES, so what refuses below is the validator and nothing else.
        confirm: Some(near_miss_key()),
    };
    let msg = match source().apply(&req, &actor()) {
        Ok(r) => {
            panic!("a single-underscore near-miss reads nothing and must be refused: {r:?}")
        }
        Err(reason) => reason,
    };
    assert!(msg.contains(&near_miss_key()), "the refusal names the key: {msg}");
    assert!(!msg.contains(SENT_VALUE), "…and never the value: {msg}");
}

/// The two shapes that ARE settable are the same two `vike-cli secrets set` accepts: a name in
/// the grid, and a LABELLED account's `{BASE}__{LABEL}` whose base resolves. The separator is a
/// DOUBLE underscore, so the single-underscore near-miss above is correctly outside it.
#[test]
fn the_settable_key_shapes_are_the_grid_and_the_labelled_grammar() {
    assert!(is_settable_credential_key(&real_key()));
    assert!(is_settable_credential_key(&labelled_key()));
    assert!(!is_settable_credential_key(&near_miss_key()));
    assert!(!is_settable_credential_key(&labelled_key().to_lowercase()));
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
        barrier: AccountBarrier::Contained,
    };
    let req = AccountRequest { verb: set_credential_verb(), confirm: Some(real_key()) };
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
    let req = AccountRequest { verb: set_credential_verb(), confirm: Some(real_key()) };
    let rendered = format!("{req:?}");
    assert!(!rendered.contains(SENT_VALUE), "Debug printed the credential: {rendered}");
    assert!(rendered.contains(&real_key()), "…it must still name the KEY: {rendered}");
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
        !bare.iter().any(|f| f == FEATURE_ACCOUNT_VERBS),
        "an unarmed node advertises nothing — the absence is what a conforming client reads \
             instead of sending a frame"
    );
    // …and the rest of the list is byte-identical, so arming this capability advertises no
    // second thing by accident.
    let without: Vec<&String> = armed.iter().filter(|f| *f != FEATURE_ACCOUNT_VERBS).collect();
    assert_eq!(without, bare.iter().collect::<Vec<&String>>());
}

/// **MAJOR 1 of the 2026-09-23 task-3/7 review.** The `sim` -> `paper` rename (ruling 7) put
/// `paper` INTO `vike_secrets::ACCOUNT_TIERS`, and this refusal denied it by name while
/// printing the very roster that now contains it — a five-path sweep of the rename's
/// operator-facing prose that stopped one call site short of its own conclusion. Derived from
/// the roster rather than a hand-copied word list, so the NEXT tier rename reddens this
/// instead of rotting the way this one did.
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
        msg.contains(&vike_secrets::ACCOUNT_TIERS.join(" | ")),
        "must print the real roster: {msg}"
    );
    for tier in vike_secrets::ACCOUNT_TIERS {
        assert!(
            !msg.contains(&format!("`{tier}` is not")) && !msg.contains(&format!("no `{tier}`")),
            "{tier} is in ACCOUNT_TIERS but this refusal denies it: {msg}"
        );
    }
}

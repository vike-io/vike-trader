//! Handshake tests: `sign`/`verify` failure modes, `NodeKeys` loading and redaction, `Account`.

use super::*;

/// ⚠ An ARBITRARY version, and it has to be: this crate knows neither protocol's schema, so
/// there is no `PROTO_VERSION` here to borrow. It read `crate::proto::PROTO_VERSION` while the
/// module lived in `vike-datahub-client`, which looked like coupling and was not — every test
/// below proves that `sign` and `verify` AGREE about whatever version they are handed, or
/// (`bumped_version_fails`) that they disagree when the numbers differ. The VALUE never
/// mattered; binding it to one protocol's was the thing that would have rotted.
const V: u32 = 1;
const D: Domain = DATAHUB_DOMAIN;

fn nonce_a() -> [u8; 32] {
    let mut n = [0u8; 32];
    for (i, b) in n.iter_mut().enumerate() {
        *b = i as u8;
    }
    n
}

fn nonce_b() -> [u8; 32] {
    [0xABu8; 32]
}

/// Lowercase hex of `bytes` — the test side's own renderer, so an assertion about "the key's
/// hex does not appear" is not asking the implementation whether it leaked.
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A correct mac for a given `(domain, key, nonce, version, scope)` verifies true — the happy
/// path.
#[test]
fn correct_mac_verifies() {
    let key = b"observe-secret";
    let n = nonce_a();
    let mac = sign(D, key, &n, V, Scope::Read);
    assert!(verify(D, key, &n, V, Scope::Read, &mac));
}

/// A WRONG key fails verification.
#[test]
fn wrong_key_fails() {
    let n = nonce_a();
    let mac = sign(D, b"the-real-key", &n, V, Scope::Write);
    assert!(!verify(D, b"a-different-key", &n, V, Scope::Write, &mac));
}

/// A WRONG scope fails: the scope byte is signed, so a mac minted for `Observe` does not verify
/// under `Control` even with the same key bytes.
#[test]
fn wrong_scope_fails() {
    let key = b"same-key-bytes";
    let n = nonce_a();
    let mac = sign(D, key, &n, V, Scope::Read);
    assert!(!verify(D, key, &n, V, Scope::Write, &mac));
}

/// A BUMPED proto_version fails: the version is folded into the signed message, so a skew can
/// never produce a matching mac (it fails the handshake instead of talking past the peer).
#[test]
fn bumped_version_fails() {
    let key = b"k";
    let n = nonce_a();
    let mac = sign(D, key, &n, V, Scope::Write);
    assert!(!verify(D, key, &n, V + 1, Scope::Write, &mac));
}

/// A REPLAYED nonce fails: a mac signed under connection A's nonce does not verify under
/// connection B's nonce — the anti-replay property the whole handshake exists for.
#[test]
fn replayed_nonce_from_a_prior_connection_fails() {
    let key = b"k";
    let mac_under_a = sign(D, key, &nonce_a(), V, Scope::Write);
    assert!(!verify(D, key, &nonce_b(), V, Scope::Write, &mac_under_a));
}

/// ⚠ The property the DOMAIN PARAMETER exists for, and the one the generalization could have
/// silently destroyed: a tag minted for one service does NOT verify at the other, even with the
/// same key, nonce, version and scope. Had the shared module hard-coded one separator (the
/// obvious way to "share" it), a leaked datahub observe key would authenticate at the tradehub
/// node and vice versa.
#[test]
fn domain_separators_are_disjoint() {
    let other = Domain::new(b"vike-tradehub-auth\0");
    let key = b"a-key-both-services-happen-to-share";
    let n = nonce_a();
    for scope in [Scope::Read, Scope::Write] {
        let datahub_mac = sign(D, key, &n, V, scope);
        assert!(
            !verify(other, key, &n, V, scope, &datahub_mac),
            "a datahub tag must not verify under the tradehub domain ({scope:?})"
        );
        let tradehub_mac = sign(other, key, &n, V, scope);
        assert!(
            !verify(D, key, &n, V, scope, &tradehub_mac),
            "a tradehub tag must not verify under the datahub domain ({scope:?})"
        );
        // ...and each still verifies under its OWN domain, so the failure above is the domain
        // and not the scenario.
        assert!(verify(D, key, &n, V, scope, &datahub_mac));
        assert!(verify(other, key, &n, V, scope, &tradehub_mac));
    }
}

/// The datahub separator obeys the NUL-terminated convention, so no separator can be a PREFIX
/// of another.
#[test]
fn the_datahub_domain_is_nul_terminated() {
    assert_eq!(DATAHUB_DOMAIN.as_bytes().last(), Some(&0u8));
    assert_eq!(DATAHUB_DOMAIN.as_bytes(), b"vike-datahub-auth\0");
}

/// An `Observe` key presented for `Control` scope is denied: verifying a `Control` handshake
/// against the observe key fails, so a read-only credential cannot escalate.
#[test]
fn observe_key_cannot_authenticate_control() {
    let keys = NodeKeys::new(b"observe-key".to_vec(), b"control-key".to_vec());
    let n = nonce_a();
    let forged = sign(D, keys.key_for(Scope::Read), &n, V, Scope::Write);
    assert!(!verify(D, keys.key_for(Scope::Write), &n, V, Scope::Write, &forged));
    let real = sign(D, keys.key_for(Scope::Write), &n, V, Scope::Write);
    assert!(verify(D, keys.key_for(Scope::Write), &n, V, Scope::Write, &real));
}

/// A truncated / empty mac never verifies (the constant-time compare rejects a length mismatch).
#[test]
fn short_or_empty_mac_fails() {
    let key = b"k";
    let n = nonce_a();
    assert!(!verify(D, key, &n, V, Scope::Read, &[]));
    assert!(!verify(D, key, &n, V, Scope::Read, &[0u8; 16]));
}

/// An ABSENT (empty) key is a closed gate: it can neither be used to forge a tag a real server
/// accepts, nor to verify a real client's tag.
#[test]
fn absent_key_is_a_closed_gate() {
    let keys = NodeKeys::new(b"observe".to_vec(), Vec::new()); // control absent
    assert!(!keys.has(Scope::Write));
    let n = nonce_a();
    let real = sign(D, b"real-control-key", &n, V, Scope::Write);
    assert!(!verify(D, keys.key_for(Scope::Write), &n, V, Scope::Write, &real));
}

/// The redaction convention: `format!("{:?}", NodeKeys)` contains NEITHER key's bytes NOR its
/// hex — only presence tags.
#[test]
fn debug_redacts_key_material() {
    let observe = b"SUPERSECRET-observe";
    let control = b"SUPERSECRET-control";
    let keys = NodeKeys::new(observe.to_vec(), control.to_vec());
    let dbg = format!("{keys:?}");
    assert!(!dbg.contains("SUPERSECRET-observe"), "observe key bytes leaked: {dbg}");
    assert!(!dbg.contains("SUPERSECRET-control"), "control key bytes leaked: {dbg}");
    assert!(!dbg.contains(&hex(observe)), "observe key hex leaked: {dbg}");
    assert!(!dbg.contains(&hex(control)), "control key hex leaked: {dbg}");
    assert!(dbg.contains("observe=set") && dbg.contains("control=set"), "{dbg}");
}

/// `from_vars_named` reads the two CALLER-NAMED keys from a supplied map (NOT process env):
/// neither present ⇒ `None`; both ⇒ both capabilities; an empty/whitespace value ⇒ that one
/// capability absent while the other still loads.
#[test]
fn from_vars_named_reads_the_callers_scoped_keys() {
    assert!(node_keys_from_vars(&HashMap::new()).is_none());

    let m = HashMap::from([
        (DATAHUB_OBSERVE_KEY_ENV.to_string(), "obs".to_string()),
        (DATAHUB_CONTROL_KEY_ENV.to_string(), "ctl".to_string()),
    ]);
    let keys = node_keys_from_vars(&m).expect("both keys present");
    assert_eq!(keys.key_for(Scope::Read), b"obs");
    assert_eq!(keys.key_for(Scope::Write), b"ctl");
    assert!(keys.has(Scope::Read) && keys.has(Scope::Write));

    let m2 = HashMap::from([
        (DATAHUB_OBSERVE_KEY_ENV.to_string(), "obs".to_string()),
        (DATAHUB_CONTROL_KEY_ENV.to_string(), "   ".to_string()),
    ]);
    let keys2 = node_keys_from_vars(&m2).expect("observe present");
    assert!(keys2.has(Scope::Read));
    assert!(!keys2.has(Scope::Write), "whitespace-only control key is absent");
}

/// ⚠ The datahub reader does NOT pick up the TRADEHUB key names, and vice versa. Two services,
/// two key pairs (0025) — a box running both must be able to hold a datahub-observe-only
/// credential without that also being a tradehub credential.
#[test]
fn the_two_services_key_names_do_not_bleed() {
    let tradehub_only = HashMap::from([
        // ⚠ BARE LITERALS ON PURPOSE, twice over. This crate is layer 15 and cannot see
        // vike-tradehub-client (layer 25), so importing the constants is not available — and
        // it would be wrong anyway: this test asserts these names do NOT resolve on the
        // datahub plane, so hard-coding what it is testing against is the honest spelling.
        // The reference pair is `vike_tradehub_client::auth::OBSERVE_KEY_ENV`.
        ("VIKE_TRADEHUB_OBSERVE_KEY".to_string(), "obs".to_string()),
        ("VIKE_TRADEHUB_CONTROL_KEY".to_string(), "ctl".to_string()),
    ]);
    assert!(
        node_keys_from_vars(&tradehub_only).is_none(),
        "tradehub keys must not configure the datahub"
    );
    let datahub_only = HashMap::from([
        (DATAHUB_OBSERVE_KEY_ENV.to_string(), "obs".to_string()),
        (DATAHUB_CONTROL_KEY_ENV.to_string(), "ctl".to_string()),
    ]);
    assert!(
        NodeKeys::from_vars_named(
            &datahub_only,
            "VIKE_TRADEHUB_OBSERVE_KEY",
            "VIKE_TRADEHUB_CONTROL_KEY"
        )
        .is_none(),
        "datahub keys must not configure the tradehub node"
    );
}

// ----- the ACCOUNT scope --------------------------------------------------------------------

/// The `Account` tag byte (`0x02`) is SIGNED: a mac minted for `Write` (or `Read`) does not verify
/// as `Account` under the same key bytes, nor the reverse — so the scopes stay different grants
/// even when an operator puts one key in every slot.
#[test]
fn an_account_mac_and_a_write_mac_are_not_interchangeable() {
    let key = b"one-key-in-every-slot";
    let n = nonce_a();
    let account_mac = sign(D, key, &n, V, Scope::Account);
    for other in [Scope::Read, Scope::Write] {
        let other_mac = sign(D, key, &n, V, other);
        assert!(
            !verify(D, key, &n, V, Scope::Account, &other_mac),
            "a {other:?} mac verified as Account"
        );
        assert!(
            !verify(D, key, &n, V, other, &account_mac),
            "an Account mac verified as {other:?}"
        );
        // The control: each verifies under its OWN scope, so the refusals above are the scope
        // byte and not a scenario in which nothing verifies.
        assert!(verify(D, key, &n, V, other, &other_mac));
        assert_ne!(other_mac, account_mac);
    }
    assert!(verify(D, key, &n, V, Scope::Account, &account_mac));
}

/// `NodeKeys::new` leaves the `Account` capability ABSENT; `with_admin` arms it, and nothing else.
#[test]
fn only_with_admin_arms_the_account_scope() {
    let keys = NodeKeys::new(b"obs".to_vec(), b"ctl".to_vec());
    assert!(!keys.has(Scope::Account));
    assert!(keys.key_for(Scope::Account).is_empty());

    let armed = keys.clone().with_admin(b"adm".to_vec());
    assert!(armed.has(Scope::Account), "with_admin must arm the Account scope");
    assert_eq!(armed.key_for(Scope::Account), b"adm");
    // ...and leaves the other two slots exactly as they were.
    assert_eq!(armed.key_for(Scope::Read), b"obs");
    assert_eq!(armed.key_for(Scope::Write), b"ctl");

    // An EMPTY admin key is still an absent capability: calling `with_admin` alone arms nothing.
    assert!(!keys.with_admin(Vec::new()).has(Scope::Account));
}

/// The DATAHUB reader never yields an `Account` key, even when the map it is handed also carries
/// an admin key name: it reads two names, so its refusal of that scope is structural.
#[test]
fn the_datahub_reader_never_arms_the_account_scope() {
    let m = HashMap::from([
        (DATAHUB_OBSERVE_KEY_ENV.to_string(), "obs".to_string()),
        (DATAHUB_CONTROL_KEY_ENV.to_string(), "ctl".to_string()),
        // A bare literal, for the reason `the_two_services_key_names_do_not_bleed` gives: this
        // crate cannot see `vike_tradehub_client::auth::ADMIN_KEY_ENV`.
        ("VIKE_TRADEHUB_ADMIN_KEY".to_string(), "adm".to_string()),
    ]);
    let keys = node_keys_from_vars(&m).expect("both datahub keys present");
    // The control: the two names it DOES read were read, so the map was not ignored wholesale.
    assert!(keys.has(Scope::Read) && keys.has(Scope::Write));
    assert!(!keys.has(Scope::Account), "the datahub reader armed the Account scope");
    assert!(keys.key_for(Scope::Account).is_empty());
    assert!(keys.key_id(Scope::Account).is_none());
}

/// `key_id(Account)` follows the credential-is-the-gate rule of the other scopes: `None` without
/// an admin key, and with one, exactly that key's fingerprint.
#[test]
fn the_account_key_id_is_none_until_armed_and_then_the_admin_keys_fingerprint() {
    let keys = NodeKeys::new(b"obs".to_vec(), b"ctl".to_vec());
    assert!(keys.key_id(Scope::Account).is_none());

    let armed = keys.with_admin(b"the-admin-key".to_vec());
    let id = armed.key_id(Scope::Account).expect("an armed admin key has an id");
    assert_eq!(id, key_fingerprint(b"the-admin-key"));
    // ...the ADMIN slot's id, not one of the other two slots'.
    assert_ne!(Some(id.as_str()), armed.key_id(Scope::Read).as_deref());
    assert_ne!(Some(id.as_str()), armed.key_id(Scope::Write).as_deref());
}

/// `Debug` reports the admin slot's PRESENCE — `admin=absent` / `admin=set` — and never its bytes.
#[test]
fn debug_reports_the_admin_slot_without_its_bytes() {
    let admin = b"SUPERSECRET-admin";
    let keys = NodeKeys::new(b"o".to_vec(), b"c".to_vec());
    let absent = format!("{keys:?}");
    assert!(absent.contains("admin=absent"), "{absent}");

    let armed = format!("{:?}", keys.with_admin(admin.to_vec()));
    // The control: the rendering DID change with the key, so the two checks below read a line
    // that reflects an armed admin slot rather than the absent one.
    assert!(armed.contains("admin=set"), "{armed}");
    assert!(!armed.contains("SUPERSECRET-admin"), "admin key bytes leaked: {armed}");
    assert!(!armed.contains(&hex(admin)), "admin key hex leaked: {armed}");
}

/// [`Scope::name`] maps each scope to the operator-facing word — the credential store's names,
/// not the variant names.
#[test]
fn each_scope_has_its_operator_facing_name() {
    assert_eq!(Scope::Read.name(), "observe");
    assert_eq!(Scope::Write.name(), "control");
    assert_eq!(Scope::Account.name(), "admin");
}

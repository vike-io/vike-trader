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
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
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

// ----- the key FINGERPRINT ------------------------------------------------------------------

/// Lowercase hex of `bytes` — the test side's own renderer, so an assertion about "the key's
/// hex does not appear" is not asking the implementation whether it leaked.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Does `haystack` carry ANY of `key`'s material — the raw bytes, their hex, or any window of
/// four consecutive bytes of either?
///
/// The window is what makes this an assertion about BYTES rather than about a whole-string
/// equality that a truncation would sail past: a leak of "the first four bytes of the key"
/// is a leak. Four is the smallest window whose hex (8 characters) cannot plausibly appear in a
/// 16-character digest by chance.
fn carries_key_material(haystack: &str, key: &[u8]) -> bool {
    let hay = haystack.as_bytes();
    let hay_hex = haystack.to_ascii_lowercase();
    let contains = |needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
    if contains(key) || hay_hex.contains(&hex(key)) {
        return true;
    }
    key.windows(4).any(|w| contains(w) || hay_hex.contains(&hex(w)))
}

/// **STABILITY.** The same key yields the same id — across separate calls, across separate
/// [`NodeKeys`] values, and (the property that matters for a ledger read months later) across
/// separate processes, which is what the golden pin below stands in for.
#[test]
fn a_key_id_is_stable_across_separate_constructions() {
    let key = b"the-control-key";
    assert_eq!(key_fingerprint(key), key_fingerprint(key));

    let a = NodeKeys::new(Vec::new(), key.to_vec());
    let b = NodeKeys::new(b"a-totally-different-observe-key".to_vec(), key.to_vec());
    assert_eq!(a.key_id(Scope::Write), b.key_id(Scope::Write));
    // …and it does not depend on what ELSE the carrier holds: `b` has an observe key and `a`
    // has none, and the control id is the same string.
    assert!(a.key_id(Scope::Read).is_none() && b.key_id(Scope::Read).is_some());
}

/// **THE GOLDEN PIN** — the id for a fixed key, verbatim.
///
/// Every input that could make an id move is pinned by a constant ([`KEY_ID_DOMAIN`],
/// [`KEY_ID_VERSION`], [`KEY_ID_NONCE`], [`KEY_ID_SCOPE`], [`KEY_ID_TAG_BYTES`],
/// [`KEY_ID_PREFIX`]), and a constant can be edited. This is the assertion that turns
/// "stable across restarts" into "stable across RELEASES": a change to any one of them
/// re-mints every operator's id and silently orphans every id already in a ledger, so it must
/// be a deliberate diff here rather than a side effect elsewhere.
#[test]
fn a_key_id_is_pinned_to_its_exact_string() {
    // Derived INDEPENDENTLY of this implementation, with `openssl dgst -sha256 -mac HMAC` over
    // the hand-assembled preimage (`b"vike-node-key-id\0" ++ 00000001 ++ 00 ++ 32 zero bytes`,
    // 54 bytes), the tool itself first checked against RFC 4231 test case 1. So this pins the
    // construction against a second implementation, not merely against its own past output.
    assert_eq!(key_fingerprint(b"the-control-key"), "nk-44c25d30c55b3ae7");
    // The SHAPE, stated separately so a future change to the length is also a deliberate diff.
    let id = key_fingerprint(b"anything");
    assert!(id.starts_with(KEY_ID_PREFIX), "{id}");
    let digits = &id[KEY_ID_PREFIX.len()..];
    assert_eq!(digits.len(), KEY_ID_TAG_BYTES * 2, "{id}");
    assert!(digits.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{id}");
}

/// **TWO KEYS, TWO IDS** — including the hardest case, a ONE-BIT difference, which a
/// truncating-prefix or length-only construction would collapse.
#[test]
fn two_different_keys_get_different_ids() {
    assert_ne!(key_fingerprint(b"key-one"), key_fingerprint(b"key-two"));
    // A single flipped bit in the last byte.
    assert_ne!(key_fingerprint(b"observe-secret"), key_fingerprint(b"observe-secreu"));
    // A PREFIX relationship must not survive into the id either.
    assert_ne!(key_fingerprint(b"secret"), key_fingerprint(b"secret-extended"));
    // …and the two scopes of one carrier, holding different keys, are distinguishable.
    let keys = NodeKeys::new(b"obs".to_vec(), b"ctl".to_vec());
    assert_ne!(keys.key_id(Scope::Read), keys.key_id(Scope::Write));
}

/// ⚠ **ONE key, ONE id, in either scope — deliberately.** An operator who set both node keys to
/// the same value has destroyed the property [`NodeKeys`] exists for (an observe credential
/// must not be able to forge a `Control` mac) and nothing else in this workspace notices. Two
/// matching ids in a ledger is the one place that shows, so the scope is NOT folded into the
/// fingerprint. See the module doc.
#[test]
fn one_key_has_one_id_whatever_scope_it_is_configured_in() {
    let reused =
        NodeKeys::new(b"same-bytes-in-both-slots".to_vec(), b"same-bytes-in-both-slots".to_vec());
    assert_eq!(
        reused.key_id(Scope::Read),
        reused.key_id(Scope::Write),
        "reusing one key across both scopes must be VISIBLE, not hidden by a scope-folded id"
    );
}

/// **THE KEY BYTES ARE NOT IN THE ID, NOR IN THE CARRIER'S `Debug`.** Asserted on the actual
/// bytes — raw, hex, and every four-byte window of both — never on a formatting call merely
/// succeeding.
#[test]
fn a_key_id_and_a_debug_line_carry_no_key_material() {
    // ⚠ Deliberately NOT spelled with the words `observe`/`control`, and not with digits. The
    // carrier's own `Debug` line renders `NodeKeys(observe=set, control=set)`, so a key named
    // `SUPERSECRET-observe-…` shares the four-byte window `erve` with a string containing no
    // leak whatsoever — the detector would fire on the redaction working correctly. Digits are
    // out for the mirror reason: their hex is a digit run that could collide with a numeric
    // field.
    let observe: &[u8] = b"ZqXvNbKdMsWtYrHjPlGf";
    let control: &[u8] = b"TcRxSaUeObJwLiVnEyDu";
    let keys = NodeKeys::new(observe.to_vec(), control.to_vec());

    for (key, id) in [
        (observe, keys.key_id(Scope::Read).expect("observe id")),
        (control, keys.key_id(Scope::Write).expect("control id")),
    ] {
        assert!(!carries_key_material(&id, key), "key material in the id: {id}");
    }
    // The carrier's own rendering, which the id must not have widened.
    let dbg = format!("{keys:?}");
    assert!(!carries_key_material(&dbg, observe), "observe key material in Debug: {dbg}");
    assert!(!carries_key_material(&dbg, control), "control key material in Debug: {dbg}");

    // ⚠ The anti-vacuity control: `carries_key_material` must be capable of FINDING a leak,
    // otherwise every assertion above passes by the helper being broken. Keyed on planted
    // strings — a precondition independent of whether the implementation leaks.
    assert!(carries_key_material(&format!("id={}", hex(observe)), observe), "whole hex");
    assert!(carries_key_material("leak: ZqXvNbKdMsWtYrHjPlGf", observe), "whole raw bytes");
    assert!(carries_key_material(&format!("nk-{}", hex(&observe[..6])), observe), "hex prefix");
    assert!(carries_key_material("nk-…ZqXv…", observe), "a four-byte raw window");
}

/// **AN ABSENT KEY YIELDS NO ID.** An empty key is a closed gate, not a key whose fingerprint
/// happens to be the fingerprint of nothing — a ledger row naming the id of a credential nobody
/// configured would be a lie in the record whose whole job is to be true.
#[test]
fn an_absent_key_yields_no_id() {
    let control_only = NodeKeys::new(Vec::new(), b"ctl".to_vec());
    assert!(control_only.key_id(Scope::Read).is_none(), "no observe key ⇒ no observe id");
    assert!(control_only.key_id(Scope::Write).is_some(), "…and the configured one still has");

    let none = NodeKeys::new(Vec::new(), Vec::new());
    assert!(none.key_id(Scope::Read).is_none() && none.key_id(Scope::Write).is_none());

    // ⚠ The absence is not "the id of the empty key" by another name: that string EXISTS (the
    // free function is total) and must not be what an absent scope reports.
    let empty_id = key_fingerprint(b"");
    for scope in [Scope::Read, Scope::Write] {
        assert_ne!(none.key_id(scope).as_deref(), Some(empty_id.as_str()));
    }
}

/// ⚠⚠ **THE ASSERTION THAT MAKES THIS A SAFE DIAGNOSTIC: a fingerprint can never be replayed as
/// an auth tag.**
///
/// Two independent reasons, both checked. (1) [`KEY_ID_DOMAIN`] is byte-distinct from every
/// protocol separator, so the preimages are disjoint and the tag is not the tag any handshake
/// would accept. (2) Structurally, a presented mac must match the connection's FRESH nonce
/// while this one is pinned to all-zero — so even the parameters chosen to be maximally
/// favourable to an attacker (the id's own pinned nonce and version) do not verify.
#[test]
fn a_fingerprint_can_never_be_replayed_as_an_auth_tag() {
    // The tradehub separator, spelled as a literal exactly as `domain_separators_are_disjoint`
    // does — this crate cannot see `vike_tradehub_client::auth::DOMAIN` (that is the crate
    // ABOVE). `the_key_id_domain_is_disjoint_from_the_real_tradehub_domain`, over there, is
    // what keeps this literal honest.
    let tradehub = Domain::new(b"vike-tradehub-auth\0");
    for protocol in [DATAHUB_DOMAIN, tradehub] {
        assert_ne!(
            KEY_ID_DOMAIN.as_bytes(),
            protocol.as_bytes(),
            "the id domain must not BE a protocol domain"
        );
    }
    assert_eq!(KEY_ID_DOMAIN.as_bytes().last(), Some(&0u8), "still NUL-terminated");
    assert!(
        !KEY_ID_DOMAIN.as_bytes().ends_with(b"-auth\0"),
        "an identification domain must not be spelled like an authentication one"
    );

    let key = b"a-key-an-attacker-wants";
    let fingerprint_tag = sign(KEY_ID_DOMAIN, key, &KEY_ID_NONCE, KEY_ID_VERSION, KEY_ID_SCOPE);
    for protocol in [DATAHUB_DOMAIN, tradehub] {
        for scope in [Scope::Read, Scope::Write] {
            assert!(
                !verify(protocol, key, &KEY_ID_NONCE, KEY_ID_VERSION, scope, &fingerprint_tag),
                "a fingerprint tag verified as an auth mac ({scope:?})"
            );
            // …and the CONTROL that says the failure above is the DOMAIN and not the scenario:
            // a tag genuinely signed under the protocol domain, at these very parameters,
            // DOES verify. Without this the test would pass against a `verify` that always
            // returned false.
            let real = sign(protocol, key, &KEY_ID_NONCE, KEY_ID_VERSION, scope);
            assert!(verify(protocol, key, &KEY_ID_NONCE, KEY_ID_VERSION, scope, &real));
            assert_ne!(real, fingerprint_tag, "the two constructions must not coincide");
        }
    }
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

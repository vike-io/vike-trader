//! The Layer-2 node auth primitive (PR-10): an HMAC-SHA256 nonce-challenge handshake that gates the
//! headless live core's connection, plus the scoped key store the two sides sign with.
//!
//! # ⚠ This module MOVED — it is now this service's BINDING of a shared primitive
//!
//! The crypto, [`NodeKeys`] and [`Scope`] live in [`vike_datahub_client::node_auth`], generalized
//! over their DOMAIN SEPARATOR. What stays here is the half that is genuinely tradehub's:
//! [`DOMAIN`], the two credential-store key names, and [`sign`]/[`verify`] pre-bound to them —
//! whose signatures are UNCHANGED, so every call site in this workspace reads exactly as before.
//!
//! Why the move: `docs/decisions/0025-datahub-remote-posture.md` adopted this scheme for the
//! `vike-datahub` data-service, and its verdict named the shape of the borrow — *"One scheme, not
//! two … 'Shape', not bytes: the domain separator and the key names must be datahub's own … so the
//! borrow is the module generalized over its domain constant, not a second implementation."* This
//! crate already re-exports `vike-datahub-client`'s FRAME CODEC rather than growing a second one
//! (see [`crate::proto`]'s "ONE framing across both localhost services"), and that crate sits BELOW
//! this one in the layer gate — so it is where a primitive the two services must not disagree about
//! belongs. `vike_datahub_client::node_auth`'s module doc carries the full argument.
//!
//! # The signed message
//!
//! Unchanged, and unchanged on the wire:
//!
//! ```text
//! msg = b"vike-tradehub-auth\0"           (19-byte domain separator, NUL-terminated)
//!     ++ proto_version.to_be_bytes()       (4 bytes, big-endian u32)
//!     ++ [scope_tag(scope)]                (1 byte: Observe=0x00, Control=0x01, Admin=0x02)
//!     ++ nonce                             (32 bytes, the per-connection challenge)
//! mac = HMAC-SHA256(key, msg)              (32-byte tag)
//! ```
//!
//! The separator makes this HMAC's preimage disjoint from every OTHER HMAC the workspace signs —
//! the venue request signers in `vike-bridge-core`, and now the datahub's own
//! [`vike_datahub_client::node_auth::DATAHUB_DOMAIN`] — so a tag can never be cross-purposed
//! between surfaces. The scope byte binds capability INTO the signature; the nonce binds the tag to
//! ONE connection. Verification is constant-time. All three properties, and the tests that pin
//! them, are in the shared module.

use std::collections::HashMap;

// The shared primitive. `NodeKeys` and `Scope` are RE-EXPORTED (not re-declared), so
// `vike_tradehub_client::auth::NodeKeys` and `…::proto::Scope` keep resolving for every consumer
// and the serde wire representation is byte-identical to before the move.
pub use vike_datahub_client::node_auth::{Domain, NodeKeys, Scope};

/// The 19-byte domain separator prefixed to every message signed for the TRADEHUB node — disjoint
/// from the datahub's [`vike_datahub_client::node_auth::DATAHUB_DOMAIN`] and from any venue
/// signer's preimage, so a tag signed here can never be replayed against another surface. The bytes
/// are unchanged from before the shared module existed, so every deployed node interoperates.
pub const DOMAIN: Domain = Domain::new(b"vike-tradehub-auth\0");

/// The credential-store key names [`from_vars`] reads. Spelled as constants IN THIS CRATE so
/// `vike_ops::scan`'s map-lookup sweep can resolve them (it resolves constants crate-wide; an
/// imported one would make the read invisible to the settings registry).
///
/// ⚠ **THIS PAIR IS THE REFERENCE SPELLING**, because this is the crate the DAEMON reads through
/// (`vike-tradehub`'s `tradehub_cli.rs` calls [`from_vars`] and declares no constant of its own).
/// The same two names are spelled again in other crates, and that duplication is DELIBERATE for the
/// reason above — a crate that imported them would pass the settings-registry gate by blindness
/// rather than by declaration. The cost of that trade is that every copy must be ASSERTED equal to
/// this one, or it drifts and the only symptom is an endless `bad mac` at the node with every test
/// green. The copies, and the test that holds each equal:
///
/// | crate | constants | held equal by |
/// |---|---|---|
/// | `vike-cli` | `cmd::nodekeys::{OBSERVE_KEY_ENV, CONTROL_KEY_ENV}` | `both_key_names_match_the_servers_own` |
/// | `vike-app-core` | `backend_registry::{OBSERVE_KEY_NAME, CONTROL_KEY_NAME}` | `observe_and_control_key_names_match_the_client` |
///
/// `vike-datahub-client` also names them, as bare literals in one test, and deliberately does NOT
/// import them: it is layer 30 and cannot see this crate (layer 50), and that test asserts these
/// names do NOT resolve on the datahub plane — a literal is the honest spelling there.
///
/// ⚠ Adding a fifth copy without adding its equality assertion re-opens the gap this table exists
/// to close. `vike-cli`'s was missing until 2026-09-04, leaving its OBSERVE half covered by nothing.
pub const OBSERVE_KEY_ENV: &str = "VIKE_TRADEHUB_OBSERVE_KEY";
/// The control-scope key name — see [`OBSERVE_KEY_ENV`].
pub const CONTROL_KEY_ENV: &str = "VIKE_TRADEHUB_CONTROL_KEY";
/// **The ADMIN-scope key name** — `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-
/// declared.md` §3c part 2.
///
/// ⚠ **A THIRD key rather than a wider Control grant**, and the whole authorization half of 0065's
/// barrier is this one name: the key every desktop carries to place orders is NOT the key that
/// writes key material. It is minted by `vike-cli backend setup` alongside the other two, from the
/// CSPRNG, and it is the ONE thing a settings write cannot forge — which is what keeps 0065's named
/// self-escalation path (a Control peer writing `flags.toml`) from arming the capability.
///
/// ⚠ It is NOT in [`from_vars`]' two-name read: an admin key that loaded wherever the pair does
/// would arm the account surface on every box that has one. [`from_vars_with_admin`] is the door,
/// and the daemon only opens it when its own three-valued declaration says to.
pub const ADMIN_KEY_ENV: &str = "VIKE_TRADEHUB_ADMIN_KEY";

/// Compute the HMAC-SHA256 tag a client presents in [`crate::proto::Request::Auth`], binding the
/// per-connection `nonce`, the `proto_version`, and the `scope` under `key`, in the TRADEHUB
/// [`DOMAIN`]. `key` is the scope's node key (see [`NodeKeys::key_for`]). Returns the 32-byte tag.
///
/// Signature-identical to the pre-move function: the domain is bound here, not passed by callers,
/// so no call site can accidentally sign a tradehub handshake under the datahub separator.
pub fn sign(key: &[u8], nonce: &[u8; 32], proto_version: u32, scope: Scope) -> Vec<u8> {
    vike_datahub_client::node_auth::sign(DOMAIN, key, nonce, proto_version, scope)
}

/// Constant-time verify a client-presented `mac` against the server-held `key` for `scope`, over the
/// connection's `nonce` and `proto_version`, in the TRADEHUB [`DOMAIN`]. Returns `true` iff the mac
/// is valid. A wrong key, wrong scope, bumped version, replayed/foreign nonce, or a tag minted for
/// the DATAHUB all fail here.
pub fn verify(key: &[u8], nonce: &[u8; 32], proto_version: u32, scope: Scope, mac: &[u8]) -> bool {
    vike_datahub_client::node_auth::verify(DOMAIN, key, nonce, proto_version, scope, mac)
}

/// Build the tradehub node's keys from an already-loaded var map — [`OBSERVE_KEY_ENV`] and
/// [`CONTROL_KEY_ENV`].
///
/// ⚠ Was `NodeKeys::from_vars`, an inherent method; it became a free function when the TYPE moved
/// to the shared module (a foreign type cannot carry an inherent impl). The behaviour, the two key
/// names, and the credential-is-the-gate contract below are unchanged.
///
/// The CALLER supplies the map (the server/CLI binary, typically the credential store via
/// `vike_bridge_core::credentials::load_workspace_secrets_from_env`), so this crate stays a pure,
/// I/O-free wire primitive that never pulls the bridge transport stack just to read a file — the
/// whole point of a LIGHT client crate. ⚠ It must be the credential-store MAP, not process env: a
/// store-defined key is INVISIBLE to a `std::env::var` reader, so a caller reading `std::env` would
/// silently miss it.
///
/// Returns `None` when NEITHER key is present (nothing to authenticate — the credential-is-the-gate
/// idiom, byte-identical to the venue loaders' "no creds → stay paper"). An absent single key loads
/// as an empty `Vec`, so a node can be brought up observe-only (or control-only) without the other
/// capability existing at all.
pub fn from_vars(vars: &HashMap<String, String>) -> Option<NodeKeys> {
    NodeKeys::from_vars_named(vars, OBSERVE_KEY_ENV, CONTROL_KEY_ENV)
}

/// [`from_vars`] plus the [`ADMIN_KEY_ENV`] key — the ONLY way a `NodeKeys` acquires an admin
/// capability anywhere in this workspace.
///
/// ⚠ **A separate function rather than a third name inside [`from_vars`]**, and the choice is the
/// authorization barrier itself. Every caller that reads a node pair today keeps reading two names
/// and keeps producing keys whose `Scope::Account` is ABSENT — so a box that has an admin key in its
/// store does not thereby arm the account surface. The daemon calls this one only when its own
/// declaration (`docs/decisions/0065`'s Part 3) says the barrier is in place, which is what makes
/// the capability an ABSENCE by default rather than a check somebody remembers.
///
/// A blank or whitespace-only admin value loads as absent, exactly as the other two do.
pub fn from_vars_with_admin(vars: &HashMap<String, String>) -> Option<NodeKeys> {
    let keys = from_vars(vars)?;
    let admin = vars
        .get(ADMIN_KEY_ENV)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(|v| v.as_bytes().to_vec())
        .unwrap_or_default();
    Some(keys.with_admin(admin))
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: u32 = crate::proto::NODE_PROTO_VERSION;

    fn nonce_a() -> [u8; 32] {
        let mut n = [0u8; 32];
        for (i, b) in n.iter_mut().enumerate() {
            *b = i as u8;
        }
        n
    }

    /// The BINDING is what this file still owns, so this is what it still tests: `sign`/`verify`
    /// here agree with each other, and the exhaustive property suite (wrong key / wrong scope /
    /// bumped version / replayed nonce / short mac / absent key / redacted Debug / cross-domain
    /// replay) lives with the implementation in `vike_datahub_client::node_auth` and is not
    /// duplicated here.
    #[test]
    fn the_tradehub_binding_signs_and_verifies() {
        let key = b"observe-secret";
        let n = nonce_a();
        for scope in [Scope::Read, Scope::Write] {
            let mac = sign(key, &n, V, scope);
            assert!(verify(key, &n, V, scope, &mac), "{scope:?}");
        }
    }

    /// ⚠ The regression the move could have introduced: this binding must sign under the TRADEHUB
    /// separator, not the shared module's datahub one. A tag from here must NOT verify under the
    /// datahub domain — otherwise a leaked tradehub key would open the datahub too.
    #[test]
    fn the_binding_uses_the_tradehub_domain_not_the_datahub_one() {
        let key = b"k";
        let n = nonce_a();
        let mac = sign(key, &n, V, Scope::Write);
        assert!(
            !vike_datahub_client::node_auth::verify(
                vike_datahub_client::node_auth::DATAHUB_DOMAIN,
                key,
                &n,
                V,
                Scope::Write,
                &mac,
            ),
            "a tradehub tag must not verify at the datahub"
        );
        assert!(
            vike_datahub_client::node_auth::verify(DOMAIN, key, &n, V, Scope::Write, &mac),
            "...and it must verify under the tradehub domain, proving the failure is the domain"
        );
    }

    /// ⚠ **The KEY-ID domain is byte-distinct from THIS service's REAL separator**, checked against
    /// the constant rather than against a literal.
    ///
    /// [`vike_datahub_client::node_auth`]'s own
    /// `a_fingerprint_can_never_be_replayed_as_an_auth_tag` proves the replay property, but it has
    /// to spell the tradehub separator as a literal — that crate is the one BELOW and cannot see
    /// [`DOMAIN`]. This is the assertion that keeps the literal honest: if [`DOMAIN`] is ever
    /// edited to collide with the id domain, the gate down there would still be green and this one
    /// goes red. A fingerprint that were also a valid auth tag is a credential leak wearing a
    /// diagnostic's clothes.
    #[test]
    fn the_key_id_domain_is_disjoint_from_the_real_tradehub_domain() {
        use vike_datahub_client::node_auth::KEY_ID_DOMAIN;
        assert_ne!(KEY_ID_DOMAIN.as_bytes(), DOMAIN.as_bytes());
        assert_eq!(DOMAIN.as_bytes(), b"vike-tradehub-auth\0", "the literal the gate below uses");
        // Neither may be a PREFIX of the other, which is the property the NUL terminator buys and
        // the one a plain inequality would miss.
        assert!(!KEY_ID_DOMAIN.as_bytes().starts_with(DOMAIN.as_bytes()));
        assert!(!DOMAIN.as_bytes().starts_with(KEY_ID_DOMAIN.as_bytes()));
    }

    /// The fingerprint reaches this service through the RE-EXPORTED [`NodeKeys`], with the
    /// contract intact: a configured key is identified, an absent one is not.
    #[test]
    fn the_tradehub_binding_carries_the_key_id() {
        let keys = NodeKeys::new(b"obs".to_vec(), Vec::new());
        let id = keys.key_id(Scope::Read).expect("a configured key has an id");
        assert!(id.starts_with("nk-"), "{id}");
        assert!(!id.contains("obs"), "the key bytes never reach the id: {id}");
        assert!(keys.key_id(Scope::Write).is_none(), "an absent key has no id");
    }

    /// `from_vars` reads the two TRADEHUB-named keys from a supplied map (NOT process env): neither
    /// present ⇒ `None` (the credential-is-the-gate idiom); both ⇒ both capabilities; an
    /// empty/whitespace value ⇒ that one capability absent while the other still loads.
    #[test]
    fn from_vars_reads_scoped_keys() {
        assert!(from_vars(&HashMap::new()).is_none());

        let m = HashMap::from([
            (OBSERVE_KEY_ENV.to_string(), "obs".to_string()),
            (CONTROL_KEY_ENV.to_string(), "ctl".to_string()),
        ]);
        let keys = from_vars(&m).expect("both keys present");
        assert_eq!(keys.key_for(Scope::Read), b"obs");
        assert_eq!(keys.key_for(Scope::Write), b"ctl");
        assert!(keys.has(Scope::Read) && keys.has(Scope::Write));

        let m2 = HashMap::from([
            (OBSERVE_KEY_ENV.to_string(), "obs".to_string()),
            (CONTROL_KEY_ENV.to_string(), "   ".to_string()),
        ]);
        let keys2 = from_vars(&m2).expect("observe present");
        assert!(keys2.has(Scope::Read));
        assert!(!keys2.has(Scope::Write), "whitespace-only control key is absent");
    }
}

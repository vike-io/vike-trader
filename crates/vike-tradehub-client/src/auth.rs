//! The tradehub node's BINDING of the shared HMAC-SHA256 nonce-challenge auth primitive.
//!
//! The crypto, [`NodeKeys`], [`Scope`] and [`Domain`] are RE-EXPORTED from
//! [`vike_node_proto::auth`], which is generalized over its DOMAIN SEPARATOR so that each service
//! (tradehub, datahub, a third one) names its own constant instead of copying the crypto
//! (`docs/decisions/0025-datahub-remote-posture.md`,
//! `docs/decisions/0107-the-node-protocol-substrate-is-a-crate-below-both-clients.md`). What is
//! genuinely tradehub's lives here: [`DOMAIN`], the credential-store key names, and
//! [`sign`]/[`verify`] pre-bound to that domain.
//!
//! # The signed message
//!
//! ```text
//! msg = b"vike-tradehub-auth\0"           (19-byte domain separator, NUL-terminated)
//!     ++ proto_version.to_be_bytes()       (4 bytes, big-endian u32)
//!     ++ [scope_tag(scope)]                (1 byte: Read=0x00, Write=0x01, Account=0x02)
//!     ++ nonce                             (32 bytes, the per-connection challenge)
//! mac = HMAC-SHA256(key, msg)              (32-byte tag)
//! ```
//!
//! The domain separator makes the preimage disjoint from every other HMAC the workspace signs (the
//! venue signers in `vike-bridge-core`, the datahub's [`vike_node_proto::auth::DATAHUB_DOMAIN`]),
//! the scope byte binds capability INTO the signature, and the nonce binds the tag to ONE
//! connection. Verification is constant-time; the property tests live in the shared module.

use std::collections::HashMap;

// Re-exported, not re-declared: `auth::NodeKeys` and `proto::Scope` resolve for every consumer and
// the serde wire representation is the shared one.
pub use vike_node_proto::auth::{Domain, NodeKeys, Scope};

/// The 19-byte domain separator prefixed to every message signed for the TRADEHUB node, disjoint
/// from [`vike_node_proto::auth::DATAHUB_DOMAIN`] and every venue signer's preimage. The bytes are
/// the deployed wire: changing them breaks every running node.
pub const DOMAIN: Domain = Domain::new(b"vike-tradehub-auth\0");

/// The credential-store key names [`from_vars`] reads. Spelled as constants IN THIS CRATE so
/// `vike_model::scan`'s map-lookup sweep can resolve them (an imported constant would make the read
/// invisible to the settings registry).
///
/// ⚠ **THIS PAIR IS THE REFERENCE SPELLING** (the daemon reads through [`from_vars`]). Other crates
/// spell the same names again DELIBERATELY, for the registry reason above, so every copy must be
/// ASSERTED equal to this one or it drifts and the only symptom is an endless `bad mac` at the node
/// with every test green. The copies, and the test that holds each equal:
///
/// | crate | constants | held equal by |
/// |---|---|---|
/// | `vike-cli` | `cmd::nodekeys::{OBSERVE_KEY_ENV, CONTROL_KEY_ENV}` | `both_key_names_match_the_servers_own` |
/// | `vike-app-core` | `backend_registry::{OBSERVE_KEY_NAME, CONTROL_KEY_NAME}` | `observe_and_control_key_names_match_the_client` |
///
/// `vike-datahub-client` also names them, as bare literals in one test that asserts they do NOT
/// resolve on the datahub plane: a literal is the honest spelling for a name you prove absent (it
/// may not import this crate either, by the same-rank layer rule).
///
/// ⚠ Adding a fifth copy without adding its equality assertion re-opens the gap this table exists
/// to close.
pub const OBSERVE_KEY_ENV: &str = "VIKE_TRADEHUB_OBSERVE_KEY";
/// The control-scope key name — see [`OBSERVE_KEY_ENV`].
pub const CONTROL_KEY_ENV: &str = "VIKE_TRADEHUB_CONTROL_KEY";
/// **The ADMIN-scope key name** — `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-
/// declared.md` §3c part 2.
///
/// ⚠ **A THIRD key rather than a wider Control grant**: the key every desktop carries to place
/// orders is NOT the key that writes key material, and a settings write cannot forge it (0065's
/// self-escalation path). It is NOT in [`from_vars`]' two-name read, or it would arm the account
/// surface on every box holding one: [`from_vars_with_admin`] is the door, opened only by the
/// daemon's own three-valued declaration.
pub const ADMIN_KEY_ENV: &str = "VIKE_TRADEHUB_ADMIN_KEY";

/// The HMAC-SHA256 tag a client presents in [`crate::proto::Request::Auth`], binding `nonce`,
/// `proto_version` and `scope` under `key` (the scope's node key, [`NodeKeys::key_for`]) in the
/// TRADEHUB [`DOMAIN`]. The domain is bound here, not passed, so no call site can sign a tradehub
/// handshake under the datahub separator.
pub fn sign(key: &[u8], nonce: &[u8; 32], proto_version: u32, scope: Scope) -> Vec<u8> {
    vike_node_proto::auth::sign(DOMAIN, key, nonce, proto_version, scope)
}

/// Constant-time verify a client's `mac` against the server-held `key` for `scope`, over `nonce`
/// and `proto_version`, in the TRADEHUB [`DOMAIN`]. A wrong key, wrong scope, bumped version,
/// replayed/foreign nonce, or a tag minted for the DATAHUB all fail.
pub fn verify(key: &[u8], nonce: &[u8; 32], proto_version: u32, scope: Scope, mac: &[u8]) -> bool {
    vike_node_proto::auth::verify(DOMAIN, key, nonce, proto_version, scope, mac)
}

/// Build the tradehub node's keys from an already-loaded var map — [`OBSERVE_KEY_ENV`] and
/// [`CONTROL_KEY_ENV`].
///
/// ⚠ The CALLER owns the I/O: this takes an already-loaded map, opens no file and reads no process
/// env, so this light crate never reads a key store. The daemon hands it the NODE store
/// `vike_secrets::resolve_node_keys` answers, never the venue credential map
/// (`crates/vike-ops/tests/settings_secrets/node_key_store_gate.rs` refuses that). It must be a
/// store MAP, not process env: a store-defined key is invisible to a `std::env::var` reader.
///
/// `None` when NEITHER key is present (the credential-is-the-gate idiom: no creds, nothing to
/// authenticate). An absent single key loads as an empty `Vec`, so a node can run observe-only
/// (or control-only).
pub fn from_vars(vars: &HashMap<String, String>) -> Option<NodeKeys> {
    NodeKeys::from_vars_named(vars, OBSERVE_KEY_ENV, CONTROL_KEY_ENV)
}

/// [`from_vars`] plus the [`ADMIN_KEY_ENV`] key — the ONLY way a `NodeKeys` acquires an admin
/// capability anywhere in this workspace.
///
/// ⚠ **A separate function, not a third name inside [`from_vars`]**: that is the authorization
/// barrier. Every pair reader keeps producing keys whose `Scope::Account` is ABSENT; the daemon
/// calls this only when its own declaration (`docs/decisions/0065`'s Part 3) says the barrier is
/// in place, so the capability is an absence by default. A blank admin value loads as absent.
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

    /// This file owns the BINDING, so it tests that `sign`/`verify` agree; the exhaustive property
    /// suite lives with the implementation in `vike_node_proto::auth`.
    #[test]
    fn the_tradehub_binding_signs_and_verifies() {
        let key = b"observe-secret";
        let n = nonce_a();
        for scope in [Scope::Read, Scope::Write] {
            let mac = sign(key, &n, V, scope);
            assert!(verify(key, &n, V, scope, &mac), "{scope:?}");
        }
    }

    /// ⚠ This binding signs under the TRADEHUB separator: a tag from here must NOT verify under the
    /// datahub domain, or a leaked tradehub key would open the datahub too.
    #[test]
    fn the_binding_uses_the_tradehub_domain_not_the_datahub_one() {
        let key = b"k";
        let n = nonce_a();
        let mac = sign(key, &n, V, Scope::Write);
        assert!(
            !vike_node_proto::auth::verify(
                vike_node_proto::auth::DATAHUB_DOMAIN,
                key,
                &n,
                V,
                Scope::Write,
                &mac,
            ),
            "a tradehub tag must not verify at the datahub"
        );
        assert!(
            vike_node_proto::auth::verify(DOMAIN, key, &n, V, Scope::Write, &mac),
            "...and it must verify under the tradehub domain, proving the failure is the domain"
        );
    }

    /// ⚠ **The KEY-ID domain is byte-distinct from THIS service's REAL separator**, checked against
    /// the constant. [`vike_node_proto::auth`]'s `a_fingerprint_can_never_be_replayed_as_an_auth_tag`
    /// must spell the tradehub separator as a literal (that crate cannot see [`DOMAIN`]); this keeps
    /// the literal honest. A fingerprint that were also a valid auth tag would leak a credential.
    #[test]
    fn the_key_id_domain_is_disjoint_from_the_real_tradehub_domain() {
        use vike_node_proto::auth::KEY_ID_DOMAIN;
        assert_ne!(KEY_ID_DOMAIN.as_bytes(), DOMAIN.as_bytes());
        assert_eq!(DOMAIN.as_bytes(), b"vike-tradehub-auth\0", "the literal the gate below uses");
        // Neither may be a PREFIX of the other (what the NUL terminator buys).
        assert!(!KEY_ID_DOMAIN.as_bytes().starts_with(DOMAIN.as_bytes()));
        assert!(!DOMAIN.as_bytes().starts_with(KEY_ID_DOMAIN.as_bytes()));
    }

    /// Through the re-exported [`NodeKeys`]: a configured key has an id, an absent one does not.
    #[test]
    fn the_tradehub_binding_carries_the_key_id() {
        let keys = NodeKeys::new(b"obs".to_vec(), Vec::new());
        let id = keys.key_id(Scope::Read).expect("a configured key has an id");
        assert!(id.starts_with("nk-"), "{id}");
        assert!(!id.contains("obs"), "the key bytes never reach the id: {id}");
        assert!(keys.key_id(Scope::Write).is_none(), "an absent key has no id");
    }

    /// `from_vars` reads a supplied map: neither key ⇒ `None`; both ⇒ both capabilities; a
    /// whitespace value ⇒ that capability absent while the other still loads.
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

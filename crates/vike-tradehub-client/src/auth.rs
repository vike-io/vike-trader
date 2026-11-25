//! The tradehub's BINDING of the shared HMAC-SHA256 nonce-challenge auth primitive.
//!
//! The crypto, [`Scope`] and [`Domain`] (and `HubKeys`) come from
//! [`vike_hub_protocol::auth`] (named there, not re-exported), which is generalized over its DOMAIN SEPARATOR so that each service
//! (tradehub, datahub, a third one) names its own constant instead of copying the crypto
//! (`docs/decisions/0025-datahub-remote-posture.md`,
//! `docs/decisions/0107-the-node-protocol-substrate-is-a-crate-below-both-clients.md`). What is
//! genuinely tradehub's lives here: [`DOMAIN`] and [`sign`]/[`verify`] pre-bound to that domain.
//! No key NAME lives here: a person dials with its own device key (decision 0288), and the shared-key
//! `Auth` is answered only for keys a test builds by hand.
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
//! venue signers in `vike-bridge-core`, the datahub's [`vike_hub_protocol::auth::DATAHUB_DOMAIN`]),
//! the scope byte binds capability INTO the signature, and the nonce binds the tag to ONE
//! connection. Verification is constant-time; the property tests live in the shared module.
//! A device's `PersonAuth` signs the same message with Ed25519 (`vike_hub_protocol::access`).

// Used, not re-declared and not re-exported: callers name `vike_hub_protocol::auth::{HubKeys, Scope}`
// themselves (every consumer links that crate, decision 0107), and the serde wire representation is
// the shared one.
use vike_hub_protocol::auth::{Domain, Scope};

/// The 19-byte domain separator prefixed to every message signed for the TRADEHUB node, disjoint
/// from [`vike_hub_protocol::auth::DATAHUB_DOMAIN`] and every venue signer's preimage. The bytes are
/// the deployed wire: changing them breaks every running node.
pub const DOMAIN: Domain = Domain::new(b"vike-tradehub-auth\0");

/// The HMAC-SHA256 tag a client presents in the handshake's `Auth`, binding `nonce`,
/// `proto_version` and `scope` under `key` (the scope's hub key,
/// [`vike_hub_protocol::auth::HubKeys::key_for`]) in the TRADEHUB [`DOMAIN`]. The domain is bound here, not passed, so no call site can sign a tradehub
/// handshake under the datahub separator.
pub fn sign(key: &[u8], nonce: &[u8; 32], proto_version: u32, scope: Scope) -> Vec<u8> {
    vike_hub_protocol::auth::sign(DOMAIN, key, nonce, proto_version, scope)
}

/// Constant-time verify a client's `mac` against the server-held `key` for `scope`, over `nonce`
/// and `proto_version`, in the TRADEHUB [`DOMAIN`]. A wrong key, wrong scope, bumped version,
/// replayed/foreign nonce, or a tag minted for the DATAHUB all fail.
pub fn verify(key: &[u8], nonce: &[u8; 32], proto_version: u32, scope: Scope, mac: &[u8]) -> bool {
    vike_hub_protocol::auth::verify(DOMAIN, key, nonce, proto_version, scope, mac)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_hub_protocol::auth::HubKeys;

    const V: u32 = crate::proto::NODE_PROTO_VERSION;

    fn nonce_a() -> [u8; 32] {
        let mut n = [0u8; 32];
        for (i, b) in n.iter_mut().enumerate() {
            *b = i as u8;
        }
        n
    }

    /// This file owns the BINDING, so it tests that `sign`/`verify` agree; the exhaustive property
    /// suite lives with the implementation in `vike_hub_protocol::auth`.
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
            !vike_hub_protocol::auth::verify(
                vike_hub_protocol::auth::DATAHUB_DOMAIN,
                key,
                &n,
                V,
                Scope::Write,
                &mac,
            ),
            "a tradehub tag must not verify at the datahub"
        );
        assert!(
            vike_hub_protocol::auth::verify(DOMAIN, key, &n, V, Scope::Write, &mac),
            "...and it must verify under the tradehub domain, proving the failure is the domain"
        );
    }

    /// ⚠ **The KEY-ID domain is byte-distinct from THIS service's REAL separator**, checked against
    /// the constant. [`vike_hub_protocol::auth`]'s `a_fingerprint_can_never_be_replayed_as_an_auth_tag`
    /// must spell the tradehub separator as a literal (that crate cannot see [`DOMAIN`]); this keeps
    /// the literal honest. A fingerprint that were also a valid auth tag would leak a credential.
    #[test]
    fn the_key_id_domain_is_disjoint_from_the_real_tradehub_domain() {
        use vike_hub_protocol::auth::KEY_ID_DOMAIN;
        assert_ne!(KEY_ID_DOMAIN.as_bytes(), DOMAIN.as_bytes());
        assert_eq!(DOMAIN.as_bytes(), b"vike-tradehub-auth\0", "the literal the gate below uses");
        // Neither may be a PREFIX of the other (what the NUL terminator buys).
        assert!(!KEY_ID_DOMAIN.as_bytes().starts_with(DOMAIN.as_bytes()));
        assert!(!DOMAIN.as_bytes().starts_with(KEY_ID_DOMAIN.as_bytes()));
    }

    /// Through the shared [`HubKeys`]: a configured key has an id, an absent one does not.
    #[test]
    fn the_tradehub_binding_carries_the_key_id() {
        let keys = HubKeys::new(b"obs".to_vec(), Vec::new());
        let id = keys.key_id(Scope::Read).expect("a configured key has an id");
        assert!(id.starts_with("nk-"), "{id}");
        assert!(!id.contains("obs"), "the key bytes never reach the id: {id}");
        assert!(keys.key_id(Scope::Write).is_none(), "an absent key has no id");
    }
}

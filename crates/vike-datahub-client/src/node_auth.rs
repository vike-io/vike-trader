//! The SHARED node-auth primitive: an HMAC-SHA256 nonce-challenge handshake, generalized over its
//! DOMAIN SEPARATOR so both localhost services (`vike-datahub` and the `vike-tradehub` node) sign
//! with one implementation and two disjoint preimages.
//!
//! # Why this lives HERE
//!
//! It was born in `crates/vike-tradehub-client/src/auth.rs` (PR-10) and moved down when
//! `docs/decisions/0025-datahub-remote-posture.md` was adopted. That record's verdict is explicit
//! about the shape of the borrow: *"One scheme, not two … 'Shape', not bytes: the domain separator
//! and the key names must be datahub's own … so the borrow is the module generalized over its
//! domain constant, not a second implementation."* A verbatim copy of the crypto would be the
//! wrong answer twice over — this workspace deletes duplicate implementations, and two copies of a
//! constant-time compare is exactly the kind of duplication that drifts silently.
//!
//! The home is this crate for the same reason the FRAMING lives here:
//! `crates/vike-tradehub-client/src/proto.rs` already re-exports `read_frame`/`write_frame`/
//! `MAX_FRAME_LEN` from [`crate::proto`] rather than growing a second codec, because
//! `vike-datahub-client` is the LIGHT crate BELOW it (layer 30 vs 50 — `crates/vike-ops/tests/
//! layer_gate.rs` is what makes that direction load-bearing rather than a preference). Auth is the
//! same class of primitive as framing: a thing the two node protocols must not disagree about. The
//! layering rule this workspace keeps re-learning — *when two sides must not disagree, the cure is
//! a shared crate BELOW both* — points at this crate and no other; the reverse edge
//! (datahub-client → tradehub-client) is a layer-gate failure, and a NEW crate for ~150 lines would
//! buy a workspace member, a CI roster row and a layer negotiation to hold a module that already
//! has a home its sibling depends on.
//!
//! This crate stays what it was: I/O-free, transport-free and DataFusion-free. `hmac`/`sha2` are
//! the EXACT crates `crates/vike-bridge-core/src/signer.rs` already links (the workspace `deny.toml`
//! bans a second crypto stack), so the move adds no package to any consumer's graph that the
//! tradehub half did not already carry.
//!
//! # The signed message
//!
//! [`sign`] and [`verify`] agree on ONE domain-separated message layout so the mac binds every
//! security-relevant field of the handshake — a wrong key, a wrong scope, a bumped protocol
//! version, a REPLAYED nonce, **or a tag minted for the other service** all change the bytes that
//! get HMAC'd and therefore fail verification:
//!
//! ```text
//! msg = domain.as_bytes()                  (the NUL-terminated per-service separator)
//!     ++ proto_version.to_be_bytes()       (4 bytes, big-endian u32)
//!     ++ [scope_tag(scope)]                (1 byte: Observe=0x00, Control=0x01)
//!     ++ nonce                             (32 bytes, the per-connection challenge)
//! mac = HMAC-SHA256(key, msg)              (32-byte tag)
//! ```
//!
//! The domain separator makes this HMAC's preimage disjoint from any OTHER HMAC the workspace signs
//! (the venue request signers in `vike-bridge-core`) **and from the other node service's**, so a tag
//! can never be cross-purposed. That last clause is the whole reason the parameter exists rather
//! than a shared constant: [`DATAHUB_DOMAIN`] and `vike_tradehub_client::auth::DOMAIN` differ, so a
//! `Control` mac captured off a tradehub connection cannot be replayed at a datahub that happens to
//! hold the same key bytes. `domain_separators_are_disjoint` in this module's tests is what says so.
//!
//! The scope byte binds capability INTO the signature: because [`Scope::Observe`] and
//! [`Scope::Control`] sign under DIFFERENT keys AND stamp a different tag byte, an observe key can
//! never yield a valid `Control` mac. The nonce binds the tag to ONE connection: a mac captured off
//! connection A (nonce A) fails against connection B (nonce B), so the handshake is replay-proof.
//!
//! # The key FINGERPRINT — [`key_fingerprint`], and what it is NOT
//!
//! [`NodeKeys::key_id`] answers *"which configured key authenticated"* with a stable, non-secret,
//! non-reversible string (`nk-<16 hex chars>`), so an audit record can name the credential without
//! carrying it. It exists because there are no human accounts in this system:
//! `vike_model::change_journal::Actor::Wire`'s `key_id` is the honest answer to "who", and until
//! this function existed that field could only be recorded ABSENT.
//!
//! It is the SAME [`sign`] call the protocol uses, over its OWN domain [`KEY_ID_DOMAIN`] and with
//! every other input pinned to a constant, then truncated to [`KEY_ID_TAG_BYTES`] and hex-encoded:
//!
//! ```text
//! id = "nk-" ++ hex(sign(KEY_ID_DOMAIN, key, [0u8; 32], KEY_ID_VERSION, KEY_ID_SCOPE)[..8])
//! ```
//!
//! ⚠ **[`KEY_ID_DOMAIN`] is not an `-auth` domain and must never become one.** A fingerprint that
//! were also a valid auth tag would be a credential leak wearing a diagnostic's clothes, so the
//! separator is `b"vike-node-key-id\0"` — disjoint from [`DATAHUB_DOMAIN`], from
//! `vike_tradehub_client::auth::DOMAIN`, and from every venue signer's preimage.
//! `a_fingerprint_can_never_be_replayed_as_an_auth_tag` is what says so, and the structural half of
//! the argument is stronger than the byte comparison: presenting a tag requires matching the
//! connection's FRESH nonce, while this one is pinned to all-zero forever.
//!
//! ⚠ **The nonce and the version are pinned, and pinning them is the whole stability property.**
//! Signing the live `proto_version` would change every id on a protocol bump; signing a real nonce
//! would change it per connection. [`KEY_ID_VERSION`] versions the FINGERPRINT SCHEME and nothing
//! else — bump it only to deliberately re-mint every id.
//!
//! ⚠ **The id is a pure function of the KEY BYTES — the scope is deliberately NOT folded in**, so
//! one key has one id wherever it is configured. That makes key REUSE visible: an operator who set
//! [`DATAHUB_OBSERVE_KEY_ENV`] and [`DATAHUB_CONTROL_KEY_ENV`] to the same value has silently
//! destroyed the property [`NodeKeys`] exists for (an observe credential must not be able to forge
//! a `Control` mac), nothing else in this workspace checks for it, and two matching ids in a ledger
//! is the one place it would show. Treating that as a disclosure rather than a signal would be
//! backwards.
//!
//! ⚠ **This is NOT a password KDF, and the guidance that reached for one was worth checking.**
//! HMAC keyed by a low-entropy secret over a PUBLIC fixed message costs an attacker the same one
//! hash per guess as a bare digest of the same secret would; a domain separator buys disjointness,
//! never a work factor. What makes that acceptable is not that the construction is strong but that
//! it is **exactly as strong as the protocol's own weakest exposure**: the handshake is PLAINTEXT
//! (see "What this does NOT buy"), so a passive observer already collects
//! `HMAC(key, domain ++ version ++ scope ++ nonce)` with every field but the key in the clear, and
//! can offline-guess against it at the identical cost. A fingerprint derived the same way widens no
//! attack that the wire does not already offer. The one thing it does widen is the AUDIENCE — a
//! ledger file can be read by a backup or a log shipper that never saw the socket — and the reason
//! that is tolerable is that the ledger lives at `<project>/settings/state/changes/` while the key
//! lives at `<project>/settings/secrets.env`, in plaintext, in the same settings tree. An iterated
//! construction was considered and REJECTED: a real work factor (~10^5 rounds) is ~1-3 seconds per
//! `NodeKeys` in a debug test build, and it would harden the fingerprint past a protection the
//! handshake itself does not have. **What would reopen this:** shipping the change journal off the
//! box while `secrets.env` stays on it.
//!
//! # Constant-time verification
//!
//! [`verify`] recomputes the mac with a fresh [`Hmac`] and compares via the [`Mac`] trait's
//! `verify_slice`, which is constant-time — it does NOT early-return on the first differing byte, so
//! it leaks no timing signal about how much of a forged mac matched. This is why verification never
//! does `==` on the raw bytes (and why no `subtle` dependency is needed — the constant-time compare
//! is already in `hmac`).
//!
//! # What this does NOT buy
//!
//! The handshake is PLAINTEXT and authenticates the CONNECTION, not each frame. Confidentiality and
//! integrity still come from the tunnel (or a VPN) in front of it — 0025's "honest limits of B"
//! says so in as many words. This is scoped AUTHORIZATION behind that barrier, never a replacement
//! for it.

use std::collections::HashMap;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// The capability ceiling a client authenticates under. The scope is bound INTO the signed auth
/// message (see [`sign`]) and each scope signs under its OWN key ([`NodeKeys`]), so a client
/// holding only the observe key can never forge a `Control` mac — capability is cryptographic, not
/// a claim the server has to trust.
///
/// ⚠ Defined HERE and re-exported by both node protocols (`vike_datahub_client::proto::Scope` and
/// `vike_tradehub_client::proto::Scope` are this type), because [`sign`]/[`verify`] fold the scope
/// TAG into the message: two `Scope` enums would be two tag tables, which is precisely the
/// disagreement a shared primitive exists to make impossible. The serde representation is
/// unchanged from where it was defined before (the variant names ARE the wire), so the move is
/// invisible on both wires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// Read-only. On the tradehub node: snapshots and subscriptions. On the datahub: the history
    /// and catalog READ verbs — and NOT the compute verbs, which compile client-supplied Rhai (see
    /// `vike_datahub::server`'s `required_scope`, which is the authority for that split).
    Observe,
    /// Read-write: everything `Observe` can do, plus the state-changing and code-executing verbs.
    Control,
}

/// A service's domain separator — the NUL-terminated byte string prefixed to every message it
/// signs. A newtype rather than a bare `&[u8]` so the parameter cannot be confused for the key or
/// the mac at a call site, and so the ONE property that matters (two services never share one) is
/// attached to a named type instead of a convention.
///
/// Each service declares its own next to the protocol it belongs to: [`DATAHUB_DOMAIN`] here,
/// `vike_tradehub_client::auth::DOMAIN` there. Nothing in this module picks a default — a caller
/// that had to choose is a caller that could choose wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domain(&'static [u8]);

impl Domain {
    /// Declare a service's separator. `const` so each protocol's is a compile-time constant beside
    /// its own wire schema.
    ///
    /// ⚠ The convention is `b"vike-<service>-auth\0"` — NUL-terminated so no separator can be a
    /// PREFIX of another (without the terminator, a future `b"vike-datahub-auth-v2"` would share a
    /// prefix with `b"vike-datahub-auth"` and the length-extension-shaped confusion that invites).
    pub const fn new(bytes: &'static [u8]) -> Self {
        Domain(bytes)
    }

    /// The raw separator bytes, as folded into the signed message.
    pub const fn as_bytes(&self) -> &'static [u8] {
        self.0
    }
}

/// The `vike-datahub` data-service separator (18 bytes, NUL-terminated) — datahub's OWN, disjoint
/// from the tradehub node's by construction, which is what stops a tag signed for one surface from
/// being replayed against the other. 0025: *"the separator exists precisely so a tag signed for one
/// surface can never be replayed against another"*.
pub const DATAHUB_DOMAIN: Domain = Domain::new(b"vike-datahub-auth\0");

/// The credential-store key names datahub's [`node_keys_from_vars`] reads — the
/// `VIKE_TRADEHUB_{OBSERVE,CONTROL}_KEY` pair's datahub twin, per 0025's "the key names must be
/// datahub's own".
///
/// Spelled as constants IN THIS CRATE (not imported from elsewhere) because `vike_ops::scan`'s
/// map-lookup sweep resolves constants CRATE-wide: an imported one would make the read invisible to
/// the settings registry, and a DECLARED read is the whole point of that gate. Same reasoning
/// `crates/vike-bridge-core/src/credentials.rs`'s `load_workspace_secrets_from_env` spells out for
/// its own literal.
pub const DATAHUB_OBSERVE_KEY_ENV: &str = "VIKE_DATAHUB_OBSERVE_KEY";
/// The control-scope key name — see [`DATAHUB_OBSERVE_KEY_ENV`].
pub const DATAHUB_CONTROL_KEY_ENV: &str = "VIKE_DATAHUB_CONTROL_KEY";

/// The separator [`key_fingerprint`] signs under — **IDENTIFICATION, never authentication.**
///
/// ⚠ It deliberately breaks [`Domain::new`]'s `b"vike-<service>-auth\0"` convention, and the break
/// is the point: this domain names no service and ends in `-key-id` rather than `-auth`, so it can
/// never be mistaken for (or grown into) one of the two protocol separators. Still NUL-terminated,
/// for the prefix reason [`Domain::new`] gives. The disjointness this buys is what stops a
/// fingerprint from ever being a valid auth tag —
/// `a_fingerprint_can_never_be_replayed_as_an_auth_tag`, and the module doc's argument.
pub const KEY_ID_DOMAIN: Domain = Domain::new(b"vike-node-key-id\0");

/// The FINGERPRINT SCHEME's version, folded in as [`sign`]'s `proto_version`.
///
/// ⚠ **Never the live `PROTO_VERSION`.** A wire-protocol bump must not re-mint every operator's key
/// id — the whole value of the id is that it is the same string this month as last. Bump this only
/// to deliberately invalidate every previously-recorded id (a changed construction).
const KEY_ID_VERSION: u32 = 1;

/// The pinned nonce. All-zero, because the id must not vary per connection — the exact opposite of
/// what a nonce is for in [`sign`], which is why it is a constant here and a `fresh_nonce()` there.
const KEY_ID_NONCE: [u8; 32] = [0u8; 32];

/// The pinned scope tag. **Arbitrary and immaterial**: the id is a pure function of the key bytes
/// (module doc), so ONE scope has to be chosen and which one cannot matter — [`KEY_ID_DOMAIN`]
/// already separates this preimage from every auth message, so the scope byte is carrying no
/// security work here at all. It exists only because [`sign`] is reused verbatim rather than
/// re-implemented with a bespoke message layout.
const KEY_ID_SCOPE: Scope = Scope::Observe;

/// How many bytes of the 32-byte tag survive into the id: **8**, rendered as 16 hex characters.
///
/// Not a security parameter — truncation neither helps nor hurts one-wayness. It is here because
/// the id lands in log lines and in a `vike_model::change_journal` identifier cell that a human
/// reads, where 19 characters is legible and 67 is not, and because a truncated tag does not have
/// the SHAPE of a mac. 64 bits distinguishes the at-most-two keys a node holds by an enormous
/// margin.
pub const KEY_ID_TAG_BYTES: usize = 8;

/// Every key id starts with this, so a bare string in a ledger is self-describing.
pub const KEY_ID_PREFIX: &str = "nk-";

/// The 1-byte scope tag folded into the signed message. Distinct per scope so capability is bound
/// into the signature, not merely asserted alongside it.
fn scope_tag(scope: Scope) -> u8 {
    match scope {
        Scope::Observe => 0x00,
        Scope::Control => 0x01,
    }
}

/// Build the exact domain-separated challenge message [`sign`]/[`verify`] both HMAC over. Kept
/// private and shared so the two sides can never drift.
fn message(domain: Domain, nonce: &[u8; 32], proto_version: u32, scope: Scope) -> Vec<u8> {
    let mut msg = Vec::with_capacity(domain.as_bytes().len() + 4 + 1 + 32);
    msg.extend_from_slice(domain.as_bytes());
    msg.extend_from_slice(&proto_version.to_be_bytes());
    msg.push(scope_tag(scope));
    msg.extend_from_slice(nonce);
    msg
}

/// Compute the HMAC-SHA256 tag a client presents in its protocol's `Auth` request, binding the
/// service `domain`, the per-connection `nonce`, the `proto_version`, and the `scope` under `key`.
/// `key` is the scope's node key (see [`NodeKeys::key_for`]). Returns the 32-byte tag.
pub fn sign(
    domain: Domain,
    key: &[u8],
    nonce: &[u8; 32],
    proto_version: u32,
    scope: Scope,
) -> Vec<u8> {
    // HMAC accepts a key of any length (it internally pads/hashes), so this never fails.
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(&message(domain, nonce, proto_version, scope));
    mac.finalize().into_bytes().to_vec()
}

/// Constant-time verify a client-presented `mac` against the server-held `key` for `scope`, over the
/// connection's `nonce`, `proto_version` and service `domain`. Returns `true` iff the mac is valid.
/// Uses the [`Mac`] trait's `verify_slice` (constant-time) — NEVER a byte-wise `==` — so a near-miss
/// forgery leaks no timing signal. A wrong key, wrong scope, bumped version, replayed/foreign nonce,
/// or a tag minted under the OTHER service's domain all fail here.
pub fn verify(
    domain: Domain,
    key: &[u8],
    nonce: &[u8; 32],
    proto_version: u32,
    scope: Scope,
    mac: &[u8],
) -> bool {
    let mut h = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    h.update(&message(domain, nonce, proto_version, scope));
    h.verify_slice(mac).is_ok()
}

/// The stable, non-secret, non-reversible id for one raw key — `nk-` plus [`KEY_ID_TAG_BYTES`]
/// hex-encoded bytes of [`sign`]'s tag under [`KEY_ID_DOMAIN`].
///
/// Safe to log, to journal and to print: recovering `key` from the result means inverting
/// HMAC-SHA256, and the module doc states the one residual (this is not a password KDF, and why the
/// plaintext handshake means it need not be one) rather than assuming it away. Same key ⇒ same id,
/// in this process and the next; different keys ⇒ different ids.
///
/// ⚠ An EMPTY key is not identified — see [`NodeKeys::key_id`], which is the form every caller
/// should reach for. This free function is the seam for a caller that holds raw bytes and no
/// `NodeKeys`; it does not decide what an absent key means, because that is the credential-is-the-
/// gate contract and it belongs on the carrier.
pub fn key_fingerprint(key: &[u8]) -> String {
    let tag = sign(KEY_ID_DOMAIN, key, &KEY_ID_NONCE, KEY_ID_VERSION, KEY_ID_SCOPE);
    let mut out = String::with_capacity(KEY_ID_PREFIX.len() + KEY_ID_TAG_BYTES * 2);
    out.push_str(KEY_ID_PREFIX);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in tag.iter().take(KEY_ID_TAG_BYTES) {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    out
}

/// A node's per-scope signing keys, loaded from the credential store. Each scope signs under its
/// OWN key so an observe-only credential can never forge a `Control` mac. An EMPTY key means that
/// capability is ABSENT (the credential-is-the-gate idiom): a scope with no key can neither sign nor
/// verify a valid mac, so the node simply does not offer it.
///
/// HARD: the raw key bytes never reach `Debug`/`Display`/any log line — see the manual redacting
/// [`std::fmt::Debug`] impl below (mirrors `vike_bridge_core::credentials::Credentials`).
///
/// The type is SERVICE-AGNOSTIC: which key NAMES fill it is each protocol's own decision
/// ([`node_keys_from_vars`] here, `vike_tradehub_client::auth::from_vars` there), and which DOMAIN
/// the keys then sign under is a [`sign`]/[`verify`] argument. Nothing about a `NodeKeys` value
/// says which service it belongs to, which is exactly why the domain is not optional at the
/// signing site.
#[derive(Clone)]
pub struct NodeKeys {
    observe: Vec<u8>,
    control: Vec<u8>,
}

impl NodeKeys {
    /// Construct from raw key bytes (an empty `Vec` = that capability absent). The keys are the
    /// UTF-8 bytes of the credential-store values [`NodeKeys::from_vars_named`] reads.
    pub fn new(observe: Vec<u8>, control: Vec<u8>) -> Self {
        NodeKeys { observe, control }
    }

    /// The signing key for `scope`. An EMPTY slice means the scope has no configured key.
    ///
    /// ⚠ **This doc claimed until 2026-08-29 that "verifying against it always fails, so an absent
    /// key is a closed gate". THAT IS FALSE, and it had already propagated into new code.**
    /// [`verify`] builds `HmacSha256::new_from_slice(key)`, which accepts ANY key length including
    /// zero, so an empty key yields a perfectly ordinary HMAC — a peer that signs with the empty
    /// key produces a mac that VERIFIES. The empty slice is not self-guarding.
    ///
    /// **What actually closes the gate is [`NodeKeys::has`], consulted BEFORE the key.** Both
    /// handshakes refuse an unkeyed scope without reaching `verify` at all
    /// (`crates/vike-datahub/src/server.rs`'s `run_handshake` and
    /// `crates/vike-tradehub/src/server.rs`'s handshake), which is why the false claim was never
    /// exploitable — and exactly why it survived long enough to be copied. Cite `has`, never this.
    pub fn key_for(&self, scope: Scope) -> &[u8] {
        match scope {
            Scope::Observe => &self.observe,
            Scope::Control => &self.control,
        }
    }

    /// `true` iff `scope` has a non-empty configured key (the capability is available at all).
    pub fn has(&self, scope: Scope) -> bool {
        !self.key_for(scope).is_empty()
    }

    /// The stable, non-secret [`key_fingerprint`] of `scope`'s key — the string an audit record
    /// names the authenticating credential by (`vike_model::change_journal::Actor::Wire`'s
    /// `key_id`).
    ///
    /// ⚠ **`None` for an ABSENT key, and that is not a convenience.** An empty key means the
    /// capability does not exist ([`NodeKeys::new`]: "an empty `Vec` = that capability absent"),
    /// and a fingerprint of nothing is a perfectly stable string that would appear in a ledger as
    /// the id of a key nobody ever configured — a lie in the one record whose job is to be true.
    /// `an_absent_key_yields_no_id` is the gate.
    pub fn key_id(&self, scope: Scope) -> Option<String> {
        self.has(scope).then(|| key_fingerprint(self.key_for(scope)))
    }

    /// Build the node keys from an already-loaded var map, reading the two CALLER-NAMED keys.
    ///
    /// The names are parameters because each service owns its own pair — `VIKE_TRADEHUB_*` there,
    /// [`DATAHUB_OBSERVE_KEY_ENV`]/[`DATAHUB_CONTROL_KEY_ENV`] here — and 0025 requires exactly
    /// that ("the key names must be datahub's own"). Sharing ONE pair across both services would
    /// mean one leaked key opens both, which is the property the separate domains exist to prevent.
    ///
    /// The CALLER supplies the map (the server/CLI binary, from the credential store), so THIS crate
    /// stays a pure, I/O-free wire primitive that never pulls the bridge transport stack just to
    /// read a file — the whole point of a LIGHT client crate. ⚠ It must be the credential-store MAP,
    /// not process env: a store-defined key is INVISIBLE to a `std::env::var` reader (the
    /// store-is-not-exported gotcha), so a caller reading `std::env` would silently miss it.
    ///
    /// Returns `None` when NEITHER key is present (nothing to authenticate — the
    /// credential-is-the-gate idiom, byte-identical to the venue loaders' "no creds → stay paper").
    /// An absent single key loads as an empty `Vec`, so a node can be brought up observe-only (or
    /// control-only) without the other capability existing at all.
    pub fn from_vars_named(
        vars: &HashMap<String, String>,
        observe_name: &str,
        control_name: &str,
    ) -> Option<NodeKeys> {
        let read = |name: &str| -> Option<Vec<u8>> {
            let v = vars.get(name)?.trim();
            if v.is_empty() { None } else { Some(v.as_bytes().to_vec()) }
        };
        let observe = read(observe_name);
        let control = read(control_name);
        if observe.is_none() && control.is_none() {
            return None;
        }
        Some(NodeKeys::new(observe.unwrap_or_default(), control.unwrap_or_default()))
    }
}

impl std::fmt::Debug for NodeKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // NEVER print the key bytes/hex — only whether each scope's key is present. Mirrors the
        // redacting Debug on `vike_bridge_core::credentials::Credentials`.
        let tag = |k: &[u8]| if k.is_empty() { "absent" } else { "set" };
        write!(f, "NodeKeys(observe={}, control={})", tag(&self.observe), tag(&self.control))
    }
}

/// The DATAHUB's named constructor: [`NodeKeys::from_vars_named`] over
/// [`DATAHUB_OBSERVE_KEY_ENV`] / [`DATAHUB_CONTROL_KEY_ENV`].
///
/// `None` — neither key configured — is what makes datahub authentication OPT-IN: the server then
/// authenticates nothing, and its `Welcome` is byte-identical to the pre-auth protocol's (see
/// `vike_datahub::server`'s `serve_authed`, which is where that contract is stated and tested).
/// Writing the two keys into `<project>/settings/secrets.env` is the whole of turning auth on.
///
/// ⚠ **This used to say the server "serves exactly as it did before this module existed", and that
/// stopped being true on 2026-09-07.** Key absence now also decides ONE verb: the destructive
/// [`Request::DeleteSeries`](crate::proto::Request::DeleteSeries) is neither advertised nor
/// answered without keys, because `Scope::Control` is a word nothing enforces on a server that
/// authenticates nothing (`docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`). The
/// still-true claim is the narrower one above — the HANDSHAKE is unchanged, not the verb set — and
/// [`FEATURE_DELETE_SERIES`](crate::proto::FEATURE_DELETE_SERIES) carries the argument.
pub fn node_keys_from_vars(vars: &HashMap<String, String>) -> Option<NodeKeys> {
    NodeKeys::from_vars_named(vars, DATAHUB_OBSERVE_KEY_ENV, DATAHUB_CONTROL_KEY_ENV)
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: u32 = crate::proto::PROTO_VERSION;
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
        let mac = sign(D, key, &n, V, Scope::Observe);
        assert!(verify(D, key, &n, V, Scope::Observe, &mac));
    }

    /// A WRONG key fails verification.
    #[test]
    fn wrong_key_fails() {
        let n = nonce_a();
        let mac = sign(D, b"the-real-key", &n, V, Scope::Control);
        assert!(!verify(D, b"a-different-key", &n, V, Scope::Control, &mac));
    }

    /// A WRONG scope fails: the scope byte is signed, so a mac minted for `Observe` does not verify
    /// under `Control` even with the same key bytes.
    #[test]
    fn wrong_scope_fails() {
        let key = b"same-key-bytes";
        let n = nonce_a();
        let mac = sign(D, key, &n, V, Scope::Observe);
        assert!(!verify(D, key, &n, V, Scope::Control, &mac));
    }

    /// A BUMPED proto_version fails: the version is folded into the signed message, so a skew can
    /// never produce a matching mac (it fails the handshake instead of talking past the peer).
    #[test]
    fn bumped_version_fails() {
        let key = b"k";
        let n = nonce_a();
        let mac = sign(D, key, &n, V, Scope::Control);
        assert!(!verify(D, key, &n, V + 1, Scope::Control, &mac));
    }

    /// A REPLAYED nonce fails: a mac signed under connection A's nonce does not verify under
    /// connection B's nonce — the anti-replay property the whole handshake exists for.
    #[test]
    fn replayed_nonce_from_a_prior_connection_fails() {
        let key = b"k";
        let mac_under_a = sign(D, key, &nonce_a(), V, Scope::Control);
        assert!(!verify(D, key, &nonce_b(), V, Scope::Control, &mac_under_a));
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
        for scope in [Scope::Observe, Scope::Control] {
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
        let forged = sign(D, keys.key_for(Scope::Observe), &n, V, Scope::Control);
        assert!(!verify(D, keys.key_for(Scope::Control), &n, V, Scope::Control, &forged));
        let real = sign(D, keys.key_for(Scope::Control), &n, V, Scope::Control);
        assert!(verify(D, keys.key_for(Scope::Control), &n, V, Scope::Control, &real));
    }

    /// A truncated / empty mac never verifies (the constant-time compare rejects a length mismatch).
    #[test]
    fn short_or_empty_mac_fails() {
        let key = b"k";
        let n = nonce_a();
        assert!(!verify(D, key, &n, V, Scope::Observe, &[]));
        assert!(!verify(D, key, &n, V, Scope::Observe, &[0u8; 16]));
    }

    /// An ABSENT (empty) key is a closed gate: it can neither be used to forge a tag a real server
    /// accepts, nor to verify a real client's tag.
    #[test]
    fn absent_key_is_a_closed_gate() {
        let keys = NodeKeys::new(b"observe".to_vec(), Vec::new()); // control absent
        assert!(!keys.has(Scope::Control));
        let n = nonce_a();
        let real = sign(D, b"real-control-key", &n, V, Scope::Control);
        assert!(!verify(D, keys.key_for(Scope::Control), &n, V, Scope::Control, &real));
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
        assert_eq!(keys.key_for(Scope::Observe), b"obs");
        assert_eq!(keys.key_for(Scope::Control), b"ctl");
        assert!(keys.has(Scope::Observe) && keys.has(Scope::Control));

        let m2 = HashMap::from([
            (DATAHUB_OBSERVE_KEY_ENV.to_string(), "obs".to_string()),
            (DATAHUB_CONTROL_KEY_ENV.to_string(), "   ".to_string()),
        ]);
        let keys2 = node_keys_from_vars(&m2).expect("observe present");
        assert!(keys2.has(Scope::Observe));
        assert!(!keys2.has(Scope::Control), "whitespace-only control key is absent");
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
        assert_eq!(a.key_id(Scope::Control), b.key_id(Scope::Control));
        // …and it does not depend on what ELSE the carrier holds: `b` has an observe key and `a`
        // has none, and the control id is the same string.
        assert!(a.key_id(Scope::Observe).is_none() && b.key_id(Scope::Observe).is_some());
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
        assert_ne!(keys.key_id(Scope::Observe), keys.key_id(Scope::Control));
    }

    /// ⚠ **ONE key, ONE id, in either scope — deliberately.** An operator who set both node keys to
    /// the same value has destroyed the property [`NodeKeys`] exists for (an observe credential
    /// must not be able to forge a `Control` mac) and nothing else in this workspace notices. Two
    /// matching ids in a ledger is the one place that shows, so the scope is NOT folded into the
    /// fingerprint. See the module doc.
    #[test]
    fn one_key_has_one_id_whatever_scope_it_is_configured_in() {
        let reused = NodeKeys::new(
            b"same-bytes-in-both-slots".to_vec(),
            b"same-bytes-in-both-slots".to_vec(),
        );
        assert_eq!(
            reused.key_id(Scope::Observe),
            reused.key_id(Scope::Control),
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
            (observe, keys.key_id(Scope::Observe).expect("observe id")),
            (control, keys.key_id(Scope::Control).expect("control id")),
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
        assert!(control_only.key_id(Scope::Observe).is_none(), "no observe key ⇒ no observe id");
        assert!(control_only.key_id(Scope::Control).is_some(), "…and the configured one still has");

        let none = NodeKeys::new(Vec::new(), Vec::new());
        assert!(none.key_id(Scope::Observe).is_none() && none.key_id(Scope::Control).is_none());

        // ⚠ The absence is not "the id of the empty key" by another name: that string EXISTS (the
        // free function is total) and must not be what an absent scope reports.
        let empty_id = key_fingerprint(b"");
        for scope in [Scope::Observe, Scope::Control] {
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
            for scope in [Scope::Observe, Scope::Control] {
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
            // ⚠ BARE LITERALS ON PURPOSE, twice over. This crate is layer 30 and cannot see
            // vike-tradehub-client (layer 50), so importing the constants is not available — and
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
}

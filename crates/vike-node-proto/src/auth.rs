//! The SHARED node-auth primitive: an HMAC-SHA256 nonce-challenge handshake, generalized over its
//! DOMAIN SEPARATOR so both localhost services (`vike-datahub` and the `vike-tradehub` node) sign
//! with one implementation and two disjoint preimages.
//!
//! `docs/decisions/0025-datahub-remote-posture.md` fixed the shape of the borrow: *"One scheme, not
//! two … the borrow is the module generalized over its domain constant, not a second
//! implementation."* Two copies of a constant-time compare are exactly the duplication that drifts
//! silently. Why the module lives in this crate rather than in either client is [`crate`]'s doc.
//! `hmac`/`sha2` are the EXACT crates `crates/vike-bridge-core/src/signer.rs` already links (the
//! workspace `deny.toml` bans a second crypto stack).
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
//!     ++ [scope_tag(scope)]                (1 byte: Read=0x00, Write=0x01, Account=0x02)
//!     ++ nonce                             (32 bytes, the per-connection challenge)
//! mac = HMAC-SHA256(key, msg)              (32-byte tag)
//! ```
//!
//! The domain separator makes this preimage disjoint from every OTHER HMAC the workspace signs
//! (the venue request signers in `vike-bridge-core`) **and from the other node service's**:
//! [`DATAHUB_DOMAIN`] and `vike_tradehub_client::auth::DOMAIN` differ, so a `Write` mac captured
//! off a tradehub connection cannot be replayed at a datahub that holds the same key bytes
//! (`domain_separators_are_disjoint`). The scope byte binds capability INTO the signature, and
//! each scope signs under its OWN key, so a read-scope key can never yield a valid `Write` mac.
//! The nonce binds the tag to ONE connection: a mac captured off connection A fails against B.
//!
//! # The key FINGERPRINT — [`key_fingerprint`], and what it is NOT
//!
//! [`NodeKeys::key_id`] answers *"which configured key authenticated"* with a stable, non-secret,
//! non-reversible string (`nk-<16 hex chars>`), so an audit record
//! (`vike_model::change_journal::Actor::Wire`'s `key_id`) can name the credential without
//! carrying it — there are no human accounts, so the key is the honest answer to "who". It is
//! the SAME [`sign`] call over its OWN domain [`KEY_ID_DOMAIN`], with every other input pinned to
//! a constant, truncated to [`KEY_ID_TAG_BYTES`] and hex-encoded:
//!
//! ```text
//! id = "nk-" ++ hex(sign(KEY_ID_DOMAIN, key, [0u8; 32], KEY_ID_VERSION, KEY_ID_SCOPE)[..8])
//! ```
//!
//! ⚠ **[`KEY_ID_DOMAIN`] is not an `-auth` domain and must never become one.** A fingerprint that
//! were also a valid auth tag would be a credential leak, so the separator is disjoint from both
//! protocol domains and from every venue signer's preimage — and, structurally, a presented tag
//! must match the connection's FRESH nonce while this one is pinned to all-zero forever.
//! `a_fingerprint_can_never_be_replayed_as_an_auth_tag` is what says so.
//!
//! ⚠ **The nonce and the version are pinned, and pinning them is the whole stability property.**
//! Signing the live `proto_version` would change every id on a protocol bump; signing a real nonce
//! would change it per connection.
//!
//! ⚠ **The id is a pure function of the KEY BYTES — the scope is deliberately NOT folded in**, so
//! one key has one id wherever it is configured. That makes key REUSE visible: an operator who set
//! [`DATAHUB_OBSERVE_KEY_ENV`] and [`DATAHUB_CONTROL_KEY_ENV`] to the same value has destroyed the
//! property [`NodeKeys`] exists for, nothing else in this workspace checks for it, and two matching
//! ids in a ledger is the one place it would show.
//!
//! ⚠ **This is NOT a password KDF.** HMAC over a public fixed message costs an attacker one hash
//! per guess, which is exactly what the PLAINTEXT handshake already offers a passive observer
//! (every field of its preimage but the key is on the wire), so the fingerprint widens no attack
//! the wire does not, and an iterated construction was rejected as hardening it past the
//! handshake's own protection. What it widens is the AUDIENCE (a backup or log shipper reading the
//! ledger), tolerable because the ledger and the plaintext node-key store share a settings tree.
//! **What would reopen this:** shipping the change journal off the box while the node-key store
//! stays on it.
//!
//! # What this does NOT buy
//!
//! The handshake is PLAINTEXT and authenticates the CONNECTION, not each frame. Confidentiality and
//! integrity still come from the tunnel (or a VPN) in front of it — 0025's "honest limits of B".
//! This is scoped AUTHORIZATION behind that barrier, never a replacement for it.

use std::collections::HashMap;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// The capability ceiling a client authenticates under. The scope is bound INTO the signed auth
/// message (see [`sign`]) and each scope signs under its OWN key ([`NodeKeys`]), so a client
/// holding only the read-scope key (the credential store still names it the `observe` key) can
/// never forge a [`Scope::Write`] mac — capability is cryptographic, not a claim the server has to
/// trust.
///
/// ⚠ Defined HERE and re-exported by both node protocols (`vike_datahub_client::proto::Scope` and
/// `vike_tradehub_client::proto::Scope` are this type): [`sign`]/[`verify`] fold the scope TAG
/// into the message, and two `Scope` enums would be two tag tables.
///
/// ⚠ **THE VARIANT NAMES ARE THE WIRE — there are TWO encodings here and only one of them is the
/// byte.** `scope_tag` folds a stable byte (`0x00`/`0x01`/`0x02`) into the signed preimage, but
/// `Request::Auth` carries this type through the derived serde impl, so the VARIANT NAME travels
/// as a string on the frame itself. A rename is therefore a HARD protocol change — the 2026-09-20
/// `Observe`/`Control`/`Admin` → `Read`/`Write`/`Account` rename added no `#[serde(alias)]` (*a
/// symbol has ONE name*), so a client and a node across it fail at the handshake: **both sides
/// update together.**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// Read-only. On the tradehub node: snapshots and subscriptions. On the datahub: the history
    /// and catalog READ verbs — and NOT the compute verbs, which compile client-supplied Rhai (see
    /// `crates/vike-datahub-client/src/proto/verbs.rs`'s `required_scope`, which is the authority for that
    /// split).
    Read,
    /// Read-write: everything [`Scope::Read`] can do, plus the state-changing and code-executing
    /// verbs.
    Write,
    /// **KEY MATERIAL and the filing around it** — the tradehub node's account verbs
    /// (`docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` §3c part 2).
    ///
    /// ⚠ **It is not a superset of [`Scope::Write`] and must never become one.** The two are
    /// different GRANTS rather than two rungs of one ladder: a [`Scope::Write`] peer can already
    /// place real orders, flatten the book and mount a Rhai script into the live-trading core, and
    /// what it cannot do is reach key material. 0065's reason 4: *"the key every desktop carries to
    /// place orders is NOT the key that writes key material"*; collapsing the two is that record's
    /// own named reopener.
    ///
    /// ⚠ **The DATAHUB never grants it**, and that is structural rather than a check:
    /// [`node_keys_from_vars`] reads two names and leaves this key EMPTY, so [`NodeKeys::has`]
    /// answers `false` and its handshake refuses the scope before consulting a key.
    Account,
}

impl Scope {
    /// The scope as a lowercase word — `observe` / `control` / `admin`, the names an operator reads
    /// in an auth-denied error and in `vike-cli node ping --json`. A match rather than `{scope:?}`:
    /// the `Debug` rendering of a wire enum is not a promised string, and a script reads this one.
    /// The ONE spelling — callers do not carry a private copy of this `match`.
    pub const fn name(self) -> &'static str {
        match self {
            Scope::Read => "observe",
            Scope::Write => "control",
            Scope::Account => "admin",
        }
    }
}

/// A service's domain separator — the NUL-terminated byte string prefixed to every message it
/// signs. A newtype rather than a bare `&[u8]` so the parameter cannot be confused for the key or
/// the mac at a call site, and so the ONE property that matters — two services never share one —
/// is attached to a named type.
///
/// Each service declares its own next to the protocol it belongs to: [`DATAHUB_DOMAIN`] here,
/// `vike_tradehub_client::auth::DOMAIN` there. Nothing in this module picks a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domain(&'static [u8]);

impl Domain {
    /// Declare a service's separator. `const` so each protocol's is a compile-time constant beside
    /// its own wire schema.
    ///
    /// ⚠ The convention is `b"vike-<service>-auth\0"` — NUL-terminated so no separator can be a
    /// PREFIX of another (`b"vike-datahub-auth-v2"` would otherwise extend `b"vike-datahub-auth"`).
    pub const fn new(bytes: &'static [u8]) -> Self {
        Domain(bytes)
    }

    /// The raw separator bytes, as folded into the signed message.
    pub const fn as_bytes(&self) -> &'static [u8] {
        self.0
    }
}

/// The `vike-datahub` data-service separator (18 bytes, NUL-terminated) — datahub's OWN, disjoint
/// from the tradehub node's, so a tag signed for one surface can never be replayed against the
/// other (0025).
pub const DATAHUB_DOMAIN: Domain = Domain::new(b"vike-datahub-auth\0");

/// The credential-store key names datahub's [`node_keys_from_vars`] reads — the
/// `VIKE_TRADEHUB_{OBSERVE,CONTROL}_KEY` pair's datahub twin, per 0025's "the key names must be
/// datahub's own".
///
/// Spelled as constants IN THIS CRATE because `vike_model::scan`'s map-lookup sweep resolves
/// constants CRATE-wide: an imported one would make the read invisible to the settings registry.
pub const DATAHUB_OBSERVE_KEY_ENV: &str = "VIKE_DATAHUB_OBSERVE_KEY";
/// The write-scope key's name (the credential store still calls it `control`) — see
/// [`DATAHUB_OBSERVE_KEY_ENV`].
pub const DATAHUB_CONTROL_KEY_ENV: &str = "VIKE_DATAHUB_CONTROL_KEY";

/// The separator [`key_fingerprint`] signs under — **IDENTIFICATION, never authentication.**
///
/// ⚠ It deliberately breaks [`Domain::new`]'s `b"vike-<service>-auth\0"` convention: it names no
/// service and ends in `-key-id`, so it can never be mistaken for (or grown into) a protocol
/// separator. Still NUL-terminated, for the prefix reason [`Domain::new`] gives.
pub const KEY_ID_DOMAIN: Domain = Domain::new(b"vike-node-key-id\0");

/// The FINGERPRINT SCHEME's version, folded in as [`sign`]'s `proto_version`.
///
/// ⚠ **Never the live `PROTO_VERSION`**: a wire-protocol bump must not re-mint every operator's key
/// id. Bump this only to deliberately invalidate every previously-recorded id.
const KEY_ID_VERSION: u32 = 1;

/// The pinned nonce. All-zero, because the id must not vary per connection.
const KEY_ID_NONCE: [u8; 32] = [0u8; 32];

/// The pinned scope tag. **Arbitrary and immaterial**: the id is a pure function of the key bytes
/// and [`KEY_ID_DOMAIN`] already separates this preimage from every auth message; it exists only
/// because [`sign`] is reused verbatim.
const KEY_ID_SCOPE: Scope = Scope::Read;

/// How many bytes of the 32-byte tag survive into the id: **8**, rendered as 16 hex characters.
///
/// Not a security parameter: the id lands in log lines and journal cells a human reads, a
/// truncated tag does not have the SHAPE of a mac, and 64 bits distinguishes a node's few keys by
/// an enormous margin.
pub const KEY_ID_TAG_BYTES: usize = 8;

/// Every key id starts with this, so a bare string in a ledger is self-describing.
pub const KEY_ID_PREFIX: &str = "nk-";

/// The 1-byte scope tag folded into the signed message. Distinct per scope so capability is bound
/// into the signature, not merely asserted alongside it.
fn scope_tag(scope: Scope) -> u8 {
    match scope {
        Scope::Read => 0x00,
        Scope::Write => 0x01,
        // ⚠ A THIRD tag inside the SIGNED preimage, which is what makes this a cryptographic
        // capability rather than a claim: a `Write` mac cannot satisfy an `Account` challenge even
        // when both scopes hold the same key bytes. It needs no `NODE_PROTO_VERSION` bump — the
        // version is folded into the same preimage, so a bump would break the handshake against
        // every running node, and the scope byte carries this capability on its own.
        Scope::Account => 0x02,
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
/// Uses the [`Mac`] trait's `verify_slice`, which is constant-time — NEVER a byte-wise `==` — so a
/// near-miss forgery leaks no timing signal (and no `subtle` dependency is needed). A wrong key,
/// wrong scope, bumped version, replayed/foreign nonce, or a tag minted under the OTHER service's
/// domain all fail here.
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

/// 32 fresh random bytes for ONE connection's auth challenge — what makes a captured handshake
/// transcript unreplayable against a later connection.
///
/// ⚠ **It lives here, below the servers, because that placement is forced:**
/// `vike_backtest::compute_server` is one of its callers, and
/// `crates/vike-ops/tests/architecture/clock_pin/ratchet.rs`'s `no_scoped_crate_declares_an_rng_dependency`
/// forbids the DETERMINISM-CRITICAL `vike-backtest` from naming an RNG at all. `rand`'s OS-backed
/// default generator, the same call `vike_tradehub::server::handshake`'s own `fresh_nonce` makes.
pub fn fresh_nonce() -> [u8; 32] {
    use rand::Rng; // rand 0.10 core trait — provides `fill_bytes` (formerly `RngCore` in rand 0.8)
    let mut nonce = [0u8; 32];
    rand::rng().fill_bytes(&mut nonce);
    nonce
}

/// The stable, non-secret, non-reversible id for one raw key — `nk-` plus [`KEY_ID_TAG_BYTES`]
/// hex-encoded bytes of [`sign`]'s tag under [`KEY_ID_DOMAIN`].
///
/// Safe to log, to journal and to print: recovering `key` from the result means inverting
/// HMAC-SHA256 (the module doc states the one residual: this is not a password KDF). Same key ⇒
/// same id, in this process and the next; different keys ⇒ different ids.
///
/// ⚠ An EMPTY key is not identified — [`NodeKeys::key_id`] is the form every caller should reach
/// for. This free function is the seam for a caller that holds raw bytes and no `NodeKeys`; what an
/// absent key means belongs on the carrier.
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
/// OWN key so a read-scope (`observe`-named) credential can never forge a [`Scope::Write`] mac.
/// An EMPTY key means that capability is ABSENT (the credential-is-the-gate idiom):
/// [`NodeKeys::has`] answers `false` and the node does not offer the scope (the empty key itself
/// guards nothing — see [`NodeKeys::key_for`]).
///
/// HARD: the raw key bytes never reach `Debug`/`Display`/any log line — the manual
/// [`std::fmt::Debug`] impl below shows only presence (mirrors
/// `vike_bridge_core::credentials::Credentials`).
///
/// The type is SERVICE-AGNOSTIC: which key NAMES fill it is each protocol's own decision
/// ([`node_keys_from_vars`] here, `vike_tradehub_client::auth::from_vars` there), and which DOMAIN
/// the keys sign under is a [`sign`]/[`verify`] argument, which is why the domain is not optional
/// at the signing site.
#[derive(Clone)]
pub struct NodeKeys {
    observe: Vec<u8>,
    control: Vec<u8>,
    /// [`Scope::Account`]'s key. EMPTY unless a caller used [`NodeKeys::with_admin`], which is what
    /// makes the datahub's refusal of that scope structural rather than a check.
    admin: Vec<u8>,
}

impl NodeKeys {
    /// Construct from raw key bytes (an empty `Vec` = that capability absent). The keys are the
    /// UTF-8 bytes of the credential-store values [`NodeKeys::from_vars_named`] reads.
    pub fn new(observe: Vec<u8>, control: Vec<u8>) -> Self {
        NodeKeys { observe, control, admin: Vec::new() }
    }

    /// …and the [`Scope::Account`] key beside them.
    ///
    /// ⚠ **A separate constructor rather than a third parameter on [`NodeKeys::new`]**, so every
    /// other caller — the datahub's server and CLI included — produces keys whose admin capability
    /// is absent without stating a value for it. The only caller that passes a non-empty key here
    /// is the tradehub daemon, and only when its own declaration armed it.
    #[must_use]
    pub fn with_admin(mut self, admin: Vec<u8>) -> Self {
        self.admin = admin;
        self
    }

    /// The signing key for `scope`. An EMPTY slice means the scope has no configured key.
    ///
    /// ⚠ **The empty slice is NOT self-guarding.** [`verify`] builds
    /// `HmacSha256::new_from_slice(key)`, which accepts a zero-length key, so a peer that signs
    /// with the empty key produces a mac that VERIFIES against it. **What closes the gate is
    /// [`NodeKeys::has`], consulted BEFORE the key**: both handshakes refuse an unkeyed scope
    /// without reaching `verify` (`crates/vike-datahub/src/server/handshake.rs`'s `run_handshake`,
    /// and `crates/vike-tradehub/src/server/handshake.rs`). Cite `has`, never this.
    pub fn key_for(&self, scope: Scope) -> &[u8] {
        match scope {
            Scope::Read => &self.observe,
            Scope::Write => &self.control,
            Scope::Account => &self.admin,
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
    /// ⚠ **`None` for an ABSENT key, and that is not a convenience**: a fingerprint of nothing is a
    /// stable string that would appear in a ledger as the id of a key nobody ever configured.
    /// `an_absent_key_yields_no_id` is the gate.
    pub fn key_id(&self, scope: Scope) -> Option<String> {
        self.has(scope).then(|| key_fingerprint(self.key_for(scope)))
    }

    /// Build the node keys from an already-loaded var map, reading the two CALLER-NAMED keys.
    ///
    /// The names are parameters because each service owns its own pair — `VIKE_TRADEHUB_*` there,
    /// [`DATAHUB_OBSERVE_KEY_ENV`]/[`DATAHUB_CONTROL_KEY_ENV`] here (0025): one pair shared across
    /// both services would mean one leaked key opens both.
    ///
    /// The CALLER supplies the map (the server/CLI binary, from the credential store), so this
    /// crate stays I/O-free. ⚠ It must be the credential-store MAP, not process env: a
    /// store-defined key is INVISIBLE to a `std::env::var` reader, so a caller reading `std::env`
    /// would silently miss it.
    ///
    /// Returns `None` when NEITHER key is present (nothing to authenticate — the
    /// credential-is-the-gate idiom). An absent single key loads as an empty `Vec`, so a node can
    /// be brought up observe-only (or control-only) without the other capability existing at all.
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
        write!(
            f,
            "NodeKeys(observe={}, control={}, admin={})",
            tag(&self.observe),
            tag(&self.control),
            tag(&self.admin)
        )
    }
}

/// The DATAHUB's named constructor: [`NodeKeys::from_vars_named`] over
/// [`DATAHUB_OBSERVE_KEY_ENV`] / [`DATAHUB_CONTROL_KEY_ENV`].
///
/// `None` — neither key configured — is what makes datahub authentication OPT-IN: the server then
/// authenticates nothing, and its `Welcome` is byte-identical to the pre-auth protocol's (see
/// `vike_datahub::server`'s `serve_authed`). Writing the two keys into the node-key store
/// (`vike-cli datahub setup` mints and writes the pair) is the whole of turning auth on.
///
/// ⚠ The HANDSHAKE is unchanged without keys, not the verb set: the destructive
/// `crates/vike-datahub-client/src/proto.rs`'s `Request::DeleteSeries` is neither advertised nor
/// answered by a key-less server
/// (`docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`; that module's
/// `FEATURE_DELETE_SERIES` carries the argument).
pub fn node_keys_from_vars(vars: &HashMap<String, String>) -> Option<NodeKeys> {
    NodeKeys::from_vars_named(vars, DATAHUB_OBSERVE_KEY_ENV, DATAHUB_CONTROL_KEY_ENV)
}

#[path = "auth_tests.rs"]
#[cfg(test)]
mod auth_tests;

#[path = "key_id_tests.rs"]
#[cfg(test)]
mod key_id_tests;

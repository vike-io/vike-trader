//! **Who may do what: a device's own key, the one access list and the ONE check every hub runs**
//! (`docs/decisions/0288-every-device-signs-with-its-own-key-one-access-list-decides-who-and-a-guest-joins-by-one-invitation.md`).
//!
//! Each device holds an Ed25519 pair ([`DeviceKey`], `ring`, already linked through rustls). The private half is born
//! on the device and never leaves it; a hub holds only public halves, one row per device in its `access_key` list,
//! with a person, a device name and a ROLE. A hub reaches the list through [`AccessList`] (the daemon builds it over
//! its own store: this crate does no I/O) and asks [`admit`] once per connection, inside [`verify_person`], and again
//! before every WRITE. No timer, no poll: one row read at the moment a decision is taken.
//!
//! # The signed bytes
//!
//! A `PersonAuth { key_id, scope, sig }` frame carries an Ed25519 signature over EXACTLY the bytes the shared-key HMAC
//! signs (`crate::auth`'s module doc: the domain separator, the protocol version, the scope tag, the connection's
//! nonce), so the same domain separation and the same one-connection binding hold, and no second message layout
//! exists to drift.
//!
//! # Roles
//!
//! A role is named by a scope's [`Scope::name`] (`observe`, `control`, `admin`: one word per thing) and admits every
//! scope of [`ROLES`] up to its own. No role admits [`Scope::Trade`]: that is the daemons' own channel and no person
//! reaches it.

use std::fmt;
use std::sync::Arc;

use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use sha2::{Digest, Sha256};

use crate::auth::{Domain, Scope, key_fingerprint, message, sign, verify};

/// Advertised in `Welcome.features` exactly when the hub holds the access list, so a device can sign
/// `PersonAuth` (decision 0112: an additive frame is a feature string, never a version bump).
pub const FEATURE_PERSON_AUTH: &str = "person-auth";

/// Advertised by the TRADEHUB alone, exactly when it holds the access list: it serves `Register`, an invitation's
/// registration of a device key and an ssh key (decision 0288 item 10). No other hub registers anything.
pub const FEATURE_ACCESS_REGISTER: &str = "access-register";

/// The roles, lowest first. A role's word is its scope's [`Scope::name`]; it admits every scope up to its own.
pub const ROLES: [Scope; 3] = [Scope::Read, Scope::Write, Scope::Account];

/// The role a box's OWNER device is registered at on a server it connects to (`vike-client`'s `attach`): the top
/// [`ROLES`] scope, since whoever runs a command there owns that server. Its word is [`Scope::name`]. It lives here,
/// beside [`ROLES`], so the client crates name a role word and never the admin scope itself
/// (`crates/vike-mcp/src/tests/credential_fence.rs`'s `a_dialing_file_does_not_present_the_admin_scope`).
pub const OWNER_DEVICE_ROLE: Scope = Scope::Account;

/// One of a server box's OWN devices (decision 0288 items 6 and 8): the person and device name of its row on the
/// box's own access list, and its role as the [`ROLES`] scope it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerDevice {
    /// The row's person.
    pub person: &'static str,
    /// The row's device name.
    pub device: &'static str,
    /// The role, a scope of [`ROLES`]; its word is [`Scope::name`].
    pub role: Scope,
}

/// The CLI and MCP on the server, `admin`: [`SERVER_DEVICES`]' first row, so a box's first device key answers it.
pub const OWNER_SERVER_DEVICE: ServerDevice =
    ServerDevice { person: "owner", device: "server", role: Scope::Account };

/// The backtesthub's own device, `observe`: what it dials the datahub as.
pub const BACKTESTHUB_DEVICE: ServerDevice =
    ServerDevice { person: "server", device: "backtesthub", role: Scope::Read };

/// The Telegram channel's device, `control`: what it dials the tradehub as (it places orders).
pub const TELEGRAM_DEVICE: ServerDevice =
    ServerDevice { person: "owner", device: "telegram", role: Scope::Write };

/// **The server box's own devices**, in the order `vike-cli backend mint-missing` mints them: [`OWNER_SERVER_DEVICE`]
/// first, then the daemons that dial a hub.
pub const SERVER_DEVICES: [ServerDevice; 3] =
    [OWNER_SERVER_DEVICE, BACKTESTHUB_DEVICE, TELEGRAM_DEVICE];

/// How long an invitation registers, from its mint (decision 0288 item 9): checked at use, never by a timer.
pub const INVITE_LIFETIME_MS: i64 = 15 * 60 * 1000;

/// The separator an invitation's proof signs under: an `-auth` domain of its own, so a proof can never verify as a
/// hub's auth tag nor the reverse (`crate::auth`'s matrix test holds every domain disjoint).
pub const INVITE_DOMAIN: Domain = Domain::new(b"vike-access-invite-auth\0");

/// Whether `role` (a word of [`ROLES`]) admits a connection under `scope`. An unknown word admits nothing.
pub fn role_admits(role: &str, scope: Scope) -> bool {
    let rank = |s: Scope| ROLES.iter().position(|r| *r == s);
    match (ROLES.iter().position(|r| r.name() == role), rank(scope)) {
        (Some(role), Some(asked)) => asked <= role,
        _ => false,
    }
}

/// Whether `word` names a role.
pub fn valid_role(word: &str) -> bool {
    ROLES.iter().any(|r| r.name() == word)
}

/// A person's or a device's name: a lower-case letter or digit, then up to 31 of `[a-z0-9_-]`. It lands in the
/// journal, a log line and an ssh line's tag, so it carries no space, quote or control character.
pub fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let tail = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-';
    (1..=32).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(tail)
}

/// An ssh PUBLIC key as an invitation registers it: `ssh-ed25519 <base64>`, no comment, nothing else. ⚠ It lands in
/// `authorized_keys`, so the shape is exact: an ed25519 blob is 51 bytes, 68 base64 characters with the fixed
/// prefix of its type and length, and no byte that could open an option or end the line.
pub fn valid_ssh_key(line: &str) -> bool {
    line.strip_prefix("ssh-ed25519 ").is_some_and(|body| {
        body.len() == 68
            && body.starts_with("AAAAC3NzaC1lZDI1NTE5AAAAI")
            && body.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/')
    })
}

/// One device's key PAIR: what a device signs `PersonAuth` and a registration with. The private half lives only here
/// and in the device's own `device_key` row (its PKCS#8 document); `Debug` shows the key id and nothing else.
#[derive(Clone)]
pub struct DeviceKey {
    pair: Arc<Ed25519KeyPair>,
}

impl DeviceKey {
    /// A NEW pair from the OS generator, with the PKCS#8 document the caller stores (and nothing else may see).
    ///
    /// # Errors
    /// The OS generator failed.
    pub fn generate() -> std::io::Result<(DeviceKey, Vec<u8>)> {
        let rng = ring::rand::SystemRandom::new();
        let document = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| std::io::Error::other("the OS random generator failed"))?;
        let key = DeviceKey::from_pkcs8(document.as_ref())?;
        Ok((key, document.as_ref().to_vec()))
    }

    /// The pair a stored PKCS#8 document holds.
    ///
    /// # Errors
    /// [`std::io::ErrorKind::InvalidData`] when the document is not an Ed25519 PKCS#8 pair.
    pub fn from_pkcs8(document: &[u8]) -> std::io::Result<DeviceKey> {
        Ed25519KeyPair::from_pkcs8(document).map(|pair| DeviceKey { pair: Arc::new(pair) }).map_err(
            |e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("not an Ed25519 device key: {e}"),
                )
            },
        )
    }

    /// The 32 public bytes, the half a hub's list holds.
    pub fn public_key(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(self.pair.public_key().as_ref());
        out
    }

    /// The key's id: [`key_fingerprint`] of the public bytes, the id the list and the journal name it by.
    pub fn key_id(&self) -> String {
        key_fingerprint(&self.public_key())
    }

    /// The `PersonAuth` signature: Ed25519 over the bytes the shared-key HMAC signs (the module doc).
    pub fn sign(
        &self,
        domain: Domain,
        nonce: &[u8; 32],
        proto_version: u32,
        scope: Scope,
    ) -> Vec<u8> {
        self.pair.sign(&message(domain, nonce, proto_version, scope)).as_ref().to_vec()
    }
}

impl fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceKey({})", self.key_id())
    }
}

/// What a client signs its handshake with: its device key (`PersonAuth`, every person since decision
/// 0288) or a shared scope key (`Auth`: a daemon's own key, or one a test builds by hand). The `From`s
/// let a caller hand either without naming this type.
#[derive(Clone, Copy, Debug)]
pub enum Signer<'a> {
    /// A shared scope key's bytes: a daemon's or a test's, never a person's.
    Shared(&'a [u8]),
    /// This device's own key.
    Device(&'a DeviceKey),
}

impl<'a> From<&'a [u8]> for Signer<'a> {
    fn from(key: &'a [u8]) -> Self {
        Signer::Shared(key)
    }
}

impl<'a> From<&'a Vec<u8>> for Signer<'a> {
    fn from(key: &'a Vec<u8>) -> Self {
        Signer::Shared(key)
    }
}

impl<'a, const N: usize> From<&'a [u8; N]> for Signer<'a> {
    fn from(key: &'a [u8; N]) -> Self {
        Signer::Shared(key)
    }
}

impl<'a> From<&'a DeviceKey> for Signer<'a> {
    fn from(key: &'a DeviceKey) -> Self {
        Signer::Device(key)
    }
}

/// What a client signs with, OWNED so it moves into the thread that dials: this device's key
/// (decision 0288). `Shared` is the daemons' and the tests' seam (`vike-client`'s `hubkeys::resolve`),
/// never a person's: no person holds a shared hub key. `&HubSigner` is a [`Signer`].
/// `Debug` names the kind and the key id, never a byte.
#[derive(Clone)]
pub enum HubSigner {
    /// This device's own key.
    Device(DeviceKey),
    /// A shared scope key's bytes: a daemon's or a test's, never a person's.
    Shared(Vec<u8>),
}

impl fmt::Debug for HubSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HubSigner::Device(key) => write!(f, "HubSigner::Device({})", key.key_id()),
            HubSigner::Shared(_) => f.write_str("HubSigner::Shared"),
        }
    }
}

impl<'a> From<&'a HubSigner> for Signer<'a> {
    fn from(signer: &'a HubSigner) -> Self {
        match signer {
            HubSigner::Device(key) => Signer::Device(key),
            HubSigner::Shared(key) => Signer::Shared(key),
        }
    }
}

/// One `access_key` row as a hub reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessEntry {
    /// Who.
    pub person: String,
    /// Which of their devices.
    pub device: String,
    /// The role's word ([`ROLES`]).
    pub role: String,
    /// The device's 32 public bytes.
    pub public_key: Vec<u8>,
    /// `true` once the row is revoked.
    pub revoked: bool,
}

/// The access list as a hub reads it: one row by `key_id`, read afresh at every call. `None` is no row, and also a
/// store the daemon could not read (it logs that itself), so an unreadable list admits nobody.
#[derive(Clone)]
pub struct AccessList {
    lookup: Arc<Lookup>,
    held: Option<Arc<Held>>,
}

/// The daemon's row lookup an [`AccessList`] holds.
type Lookup = dyn Fn(&str) -> Option<AccessEntry> + Send + Sync;

/// The daemon's "does the list hold any row" probe ([`AccessList::with_held`]).
type Held = dyn Fn() -> bool + Send + Sync;

/// The wire's answer to a device when the hub's list holds NO row at all (decision 0288's cutover: every hub always
/// authenticates, so an empty list admits nobody, and says how a box gets its first device).
pub const NO_DEVICE_REGISTERED: &str =
    "no device registered: run `vike-cli backend mint-missing` or `vike-cli access add`";

impl AccessList {
    /// A list over the daemon's own lookup.
    pub fn new(lookup: impl Fn(&str) -> Option<AccessEntry> + Send + Sync + 'static) -> Self {
        AccessList { lookup: Arc::new(lookup), held: None }
    }

    /// …and the daemon's probe of whether the list holds any row, asked at each `PersonAuth`, so an EMPTY list is
    /// refused as [`NO_DEVICE_REGISTERED`] rather than as an unknown key. Without one the list counts as held.
    #[must_use]
    pub fn with_held(mut self, held: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.held = Some(Arc::new(held));
        self
    }

    /// **A list over the daemon's store, with the ONE policy for a store that fails**: `find` reads the row of a
    /// key id, `held` whether the list holds any row (both afresh at every call, so a revoke or a role change needs
    /// no restart). A `find` error is handed to `on_unreadable` (the daemon's own log line: this crate links no
    /// `tracing`) and admits NOBODY; a `held` error counts as held, so the refusal is the failed lookup's, never
    /// [`NO_DEVICE_REGISTERED`] for a list that may well hold rows.
    pub fn from_store<E: fmt::Display + 'static>(
        find: impl Fn(&str) -> Result<Option<AccessEntry>, E> + Send + Sync + 'static,
        held: impl Fn() -> Result<bool, E> + Send + Sync + 'static,
        on_unreadable: impl Fn(&dyn fmt::Display) + Send + Sync + 'static,
    ) -> Self {
        AccessList::new(move |key_id| {
            find(key_id).unwrap_or_else(|e| {
                on_unreadable(&e);
                None
            })
        })
        .with_held(move || held().unwrap_or(true))
    }

    /// The list of a daemon started with no store: no row and an empty list, so every device is refused as
    /// [`NO_DEVICE_REGISTERED`].
    pub fn without_store() -> Self {
        AccessList::new(|_| None).with_held(|| false)
    }

    /// The row of `key_id`, read now.
    pub fn entry(&self, key_id: &str) -> Option<AccessEntry> {
        (self.lookup)(key_id)
    }

    /// Whether the list holds any row, read now (`true` without a probe).
    pub fn held(&self) -> bool {
        self.held.as_ref().is_none_or(|held| held())
    }
}

impl fmt::Debug for AccessList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessList")
    }
}

/// Who a person connection authenticated as; it rides the connection beside its scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// The device key's id.
    pub key_id: String,
    /// The row's person.
    pub person: String,
    /// The row's device.
    pub device: String,
    /// The row's role's word ([`ROLES`]), as read at that check.
    pub role: String,
}

/// Why [`admit`] or [`verify_person`] said no. A hub answers every one with the same coarse `access denied`; the
/// word goes to its log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRefusal {
    /// No row has this key id (or the list could not be read).
    Unknown,
    /// The row is revoked.
    Revoked,
    /// The row's role does not admit the scope asked.
    Role,
    /// The signature did not verify under the row's public key, or the row's key is not the one its id names.
    Signature,
}

impl AccessRefusal {
    /// The word a daemon logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            AccessRefusal::Unknown => "no such device key",
            AccessRefusal::Revoked => "revoked",
            AccessRefusal::Role => "the role does not admit this scope",
            AccessRefusal::Signature => "bad signature",
        }
    }
}

/// The wire's answer to every person refusal (the module doc).
pub const ACCESS_DENIED: &str = "access denied";

/// **THE ONE CHECK** (decision 0288 item 3): the row of `key_id` exists, is not revoked, and its role admits `scope`.
/// A hub calls it inside [`verify_person`] at connect and alone before every write.
///
/// # Errors
/// The [`AccessRefusal`] that decided.
pub fn admit(list: &AccessList, key_id: &str, scope: Scope) -> Result<Person, AccessRefusal> {
    let entry = list.entry(key_id).ok_or(AccessRefusal::Unknown)?;
    admitted(entry, key_id, scope)
}

/// [`admit`]'s decision over a row already read.
fn admitted(entry: AccessEntry, key_id: &str, scope: Scope) -> Result<Person, AccessRefusal> {
    if entry.revoked {
        return Err(AccessRefusal::Revoked);
    }
    if !role_admits(&entry.role, scope) {
        return Err(AccessRefusal::Role);
    }
    Ok(Person {
        key_id: key_id.to_string(),
        person: entry.person,
        device: entry.device,
        role: entry.role,
    })
}

/// [`admit`] plus the signature: a `PersonAuth`'s `sig` verifies under the row's public key over this connection's
/// bytes, and that key is the one `key_id` names.
///
/// # Errors
/// The [`AccessRefusal`] that decided.
pub fn verify_person(
    list: &AccessList,
    domain: Domain,
    nonce: &[u8; 32],
    proto_version: u32,
    key_id: &str,
    scope: Scope,
    sig: &[u8],
) -> Result<Person, AccessRefusal> {
    let entry = list.entry(key_id).ok_or(AccessRefusal::Unknown)?;
    let signed = key_fingerprint(&entry.public_key) == key_id
        && UnparsedPublicKey::new(&ED25519, &entry.public_key)
            .verify(&message(domain, nonce, proto_version, scope), sig)
            .is_ok();
    let person = admitted(entry, key_id, scope)?;
    if signed { Ok(person) } else { Err(AccessRefusal::Signature) }
}

/// **The per-write re-check** (decision 0288 item 4) a hub's session asks before every WRITE: a connection a device
/// signed has its row read again by [`admit`], so a revoke or a lowered role refuses the very next write while its
/// reads go on. A shared-key connection (`person` `None`) has no row to re-read and passes, its scope fixed at
/// connect; a person on a hub holding no list is refused, since nothing can say it is still admitted.
///
/// # Errors
/// The [`AccessRefusal`] that decided.
pub fn recheck(
    list: Option<&AccessList>,
    person: Option<&Person>,
    scope: Scope,
) -> Result<(), AccessRefusal> {
    let Some(person) = person else { return Ok(()) };
    admit(list.ok_or(AccessRefusal::Unknown)?, &person.key_id, scope).map(drop)
}

/// The key an invitation's proof signs under, from its secret: SHA-256 of the secret's bytes. The server's
/// `access_invite` row keeps this, never the secret.
pub fn invite_auth_secret(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.trim().as_bytes()).to_vec()
}

/// A registration's proof that it holds the invitation's secret, bound to this connection's nonce: the secret never
/// crosses the wire.
pub fn invite_proof(secret: &str, nonce: &[u8; 32], proto_version: u32) -> Vec<u8> {
    sign(INVITE_DOMAIN, &invite_auth_secret(secret), nonce, proto_version, Scope::Read)
}

/// Whether `proof` was made with the secret whose [`invite_auth_secret`] the row holds. An empty stored key verifies
/// nothing.
pub fn invite_proof_verifies(
    auth_secret: &[u8],
    nonce: &[u8; 32],
    proto_version: u32,
    proof: &[u8],
) -> bool {
    !auth_secret.is_empty()
        && verify(INVITE_DOMAIN, auth_secret, nonce, proto_version, Scope::Read, proof)
}

#[cfg(test)]
#[path = "access_tests.rs"]
mod access_tests;

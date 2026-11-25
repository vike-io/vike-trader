//! The KEYED pre-auth handshake three daemons run, the datahub, the backtesthub and the gatewayhub:
//! `Hello` -> `Welcome{nonce}` -> `Auth` -> verify, spelled ONCE (the refusal order, every reason on the
//! wire), and the frames it is made of. What differs per daemon is a value, [`HandshakeSpec`] (its
//! domain separator, version, plane, advertisement, deadline and reply ceiling), plus the scopes it
//! serves. Each daemon keeps its own LOG lines (its words and its `tracing` target, which subscribers
//! filter on), so the steps worth a line reach the caller as [`HandshakeNote`]s, at the moment each
//! happens. The tradehub's handshake is its own (its refusals name the scope), and shares the read.
//!
//! A hub whose keys carry the access list (`HubKeys::with_access`, decision 0288) also advertises
//! `person-auth` and takes `PersonAuth` in `Auth`'s place: a device's signature, admitted by the list alone
//! (`crate::access::verify_person`), to Read or Write and nothing else on these planes. The outcome then names the
//! person, and the session re-checks the row before every write.
//!
//! Both frames are read under [`SERVER_HANDSHAKE_MAX_FRAME_LEN`] rather than the shared 64 MiB ceiling
//! (an unauthenticated peer must not be able to name a large allocation), and the WHOLE phase under
//! ONE deadline ([`read_pre_auth_frame`]'s `by`; `crate::limits::SERVER_HANDSHAKE_DEADLINE` in
//! production): a peer that opens a socket and says nothing frees its thread in seconds, and so does
//! one that trickles a byte just inside every read timeout. A socket read timeout alone bounds each
//! `read`, not the phase, and a peer sending 1 byte per timeout would hold the thread (and one of the
//! connection slots) for 65540 timeouts; [`read_pre_auth_frame`] re-arms the timeout to the time that
//! is LEFT before every read. Every hub daemon reads its pre-auth frames through it: the three above
//! inside [`run_keyed_handshake`], the tradehub in its own handshake.
//!
//! The frames here are the handshake's ONLY spelling: no hub protocol carries a copy of them (decision
//! 0288's amendment of 2026-11-21), and every client and every daemon writes and reads these. A frame read
//! AFTER the handshake (a second `Hello`, a late `Auth`) is found by [`handshake_frame_of`], JSON only; every
//! hub daemon, the tradehub included, answers it [`already_authenticated`], and a key-less data-plane daemon,
//! which runs no handshake, answers its `Hello` with a `Welcome` carrying no nonce. `handshake_tests.rs`'s `every_handshake_frame_is_pinned_to_its_json_bytes` holds the
//! bytes of every variant.
//!
//! The one handshake frame a hub ACCEPTS after the handshake is a client's [`HandshakeFrame::Reads`], what that
//! client reads ([`reads_answer`] writes it, only to a hub advertising `crate::features::FEATURE_REFUSAL_FRAME`;
//! [`reads_refusal_frame`] reads it): no reply, the connection's answers change shape from then on.

use std::io::{self, Read};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::access::{
    ACCESS_DENIED, AccessRefusal, FEATURE_PERSON_AUTH, NO_DEVICE_REGISTERED, Person, Signer,
    verify_person,
};
#[cfg(doc)]
use crate::auth::{BACKTESTHUB_DOMAIN, DATAHUB_DOMAIN};
use crate::auth::{Domain, HubKeys, Scope, fresh_nonce};
use crate::call::HubCallError;
use crate::features::FEATURE_REFUSAL_FRAME;
use crate::frame::{BodyCodec, json_body_len, read_frame_raw_capped, write_frame};
use crate::limits::SERVER_HANDSHAKE_MAX_FRAME_LEN;
use crate::plane::{Plane, foreign_verb};

/// What a client sends before it is authenticated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HandshakeFrame {
    /// The version-handshake opener: the client's protocol version.
    Hello { proto_version: u32 },
    /// The answer to a keyed server's nonce: the capability ceiling for the whole connection, bound
    /// INTO the mac.
    Auth { scope: Scope, mac: Vec<u8> },
    /// A DEVICE's answer to the nonce (decision 0288), sent only to a hub advertising
    /// [`crate::access::FEATURE_PERSON_AUTH`]: the device key's id, the ceiling, and the Ed25519 signature over the
    /// bytes `Auth`'s mac signs ([`crate::access::DeviceKey::sign`]).
    PersonAuth { key_id: String, scope: Scope, sig: Vec<u8> },
    /// An invitation's REGISTRATION in `Auth`'s place (decision 0288 item 10), only to a hub advertising
    /// [`crate::access::FEATURE_ACCESS_REGISTER`] (the tradehub alone). Answered by [`HandshakeReply::Registered`] or
    /// `AuthDenied`; the connection closes either way.
    Register(Registration),
    /// What this CLIENT reads, sent once right after its handshake (`AuthOk`, or a key-less `Welcome`) and only to a
    /// hub whose `Welcome` advertised [`FEATURE_REFUSAL_FRAME`] ([`reads_answer`]): an older hub is never sent it.
    /// No reply; the hub records it for the connection ([`reads_refusal_frame`]).
    Reads { features: Vec<String> },
}

impl HandshakeFrame {
    /// The frame's wire tag, as a refusal names it.
    pub fn kind(&self) -> &'static str {
        match self {
            HandshakeFrame::Hello { .. } => "Hello",
            HandshakeFrame::Auth { .. } => "Auth",
            HandshakeFrame::PersonAuth { .. } => "PersonAuth",
            HandshakeFrame::Register(_) => "Register",
            HandshakeFrame::Reads { .. } => "Reads",
        }
    }
}

/// The CLIENT's [`HandshakeFrame::Reads`] for a hub that advertised `features`: `Some` when the hub advertises
/// [`FEATURE_REFUSAL_FRAME`] (this build reads the frame and the stream refusal classes), `None` for an older hub,
/// which is sent nothing. Every hub client writes it the moment its handshake is done.
pub fn reads_answer(features: &[String]) -> Option<HandshakeFrame> {
    features
        .iter()
        .any(|f| f == FEATURE_REFUSAL_FRAME)
        .then(|| HandshakeFrame::Reads { features: vec![FEATURE_REFUSAL_FRAME.to_string()] })
}

/// A HUB's reading of a post-handshake frame: `Some(true)` for a client's [`HandshakeFrame::Reads`] naming
/// [`FEATURE_REFUSAL_FRAME`] (send it the refusal frame and the typed stream refusals), `Some(false)` for one that
/// does not, `None` for any other handshake frame (refused with [`already_authenticated`]). A `Reads` is answered
/// with nothing: the client reads no reply to it.
pub fn reads_refusal_frame(frame: &HandshakeFrame) -> Option<bool> {
    match frame {
        HandshakeFrame::Reads { features } => {
            Some(features.iter().any(|f| f == FEATURE_REFUSAL_FRAME))
        }
        _ => None,
    }
}

/// The refusal of a handshake frame on an AUTHENTICATED connection (a second `Hello`, a late `Auth`): every hub
/// daemon's one text (the tradehub's, the datahub's, the backtesthub's and the gatewayhub's), and the connection
/// is kept.
pub fn already_authenticated(frame: &HandshakeFrame) -> String {
    format!("{} is a handshake frame; this connection is already authenticated", frame.kind())
}

/// The handshake frame a body read AFTER the handshake phase is (a second `Hello`, a late `Auth`, or a key-less
/// test daemon's `Hello`), or `None` for a verb. A handshake frame is JSON only (`docs/ops/wire-encodings.md`), so
/// a msgpack or LZ4 body is answered `None` unread: it is never decompressed here, and a verb's body is decoded
/// once, by its daemon. Every hub daemon's session reads through it.
pub fn handshake_frame_of(body: &[u8]) -> Option<HandshakeFrame> {
    if BodyCodec::of(body) != BodyCodec::Json {
        return None;
    }
    serde_json::from_slice(body).ok()
}

/// What a guest's app registers with an invitation (decision 0288 item 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    /// The invitation's public id.
    pub invite: String,
    /// [`crate::access::invite_proof`] of the invitation's secret over this connection's nonce.
    pub proof: Vec<u8>,
    /// The name the app gives its device ([`crate::access::valid_name`]).
    pub device: String,
    /// The new device key's 32 public bytes.
    pub public_key: Vec<u8>,
    /// The guest's new permanent ssh public key ([`crate::access::valid_ssh_key`]).
    pub ssh_key: String,
}

/// The access row a registration granted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registered {
    /// The invitation's person.
    pub person: String,
    /// The device as registered.
    pub device: String,
    /// The invitation's role.
    pub role: String,
    /// The registered device key's id.
    pub key_id: String,
}

/// A daemon's registration door: the registration and this connection's nonce in, the granted row or the
/// refusal's wire text out. Only the tradehub holds one.
pub type RegisterDoor<'a> = &'a dyn Fn(Registration, &[u8; 32]) -> Result<Registered, &'static str>;

/// What a server answers before a client is authenticated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HandshakeReply {
    /// The server's protocol version, the verbs it answers and, on a keyed server, the
    /// per-connection nonce.
    ///
    /// ⚠ `None` on a key-less server, and the serde attributes are load-bearing: `skip_serializing_if`
    /// keeps a key-less server's `Welcome` BYTE-IDENTICAL to the pre-auth protocol's, and `default` lets
    /// an old server's `Welcome` decode for a new client.
    Welcome {
        proto_version: u32,
        features: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<[u8; 32]>,
    },
    /// The connection is authenticated, and `scope` is its ceiling for the rest of its life.
    ///
    /// `admitted` says WHO, when a device authenticated (`PersonAuth`, decision 0288): its access row's person,
    /// device and role, so a client can show the role it really holds rather than infer it from the scope it
    /// asked for. `None` for a shared key, and from a node older than the field: `default` lets an old answer
    /// decode, and `skip_serializing_if` keeps a shared-key answer BYTE-IDENTICAL to the old one.
    AuthOk {
        scope: Scope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        admitted: Option<Admitted>,
    },
    /// Refused; the connection closes after this frame. ⚠ `reason` is deliberately coarse: it never
    /// distinguishes "no such key" from "wrong key" and never echoes a key, a mac or a nonce.
    AuthDenied { reason: String },
    /// A [`HandshakeFrame::Register`] was accepted; the connection closes after this frame.
    Registered(Registered),
}

impl HandshakeReply {
    /// The reply's wire tag, as a desync refusal names it (never its payload).
    pub fn kind(&self) -> &'static str {
        match self {
            HandshakeReply::Welcome { .. } => "Welcome",
            HandshakeReply::AuthOk { .. } => "AuthOk",
            HandshakeReply::AuthDenied { .. } => "AuthDenied",
            HandshakeReply::Registered(_) => "Registered",
        }
    }
}

/// Who the access list admitted, as [`HandshakeReply::AuthOk`] tells the client: its row's person, device and role
/// (a word of [`crate::access::ROLES`]). Never a key or a key id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Admitted {
    pub person: String,
    pub device: String,
    pub role: String,
}

impl From<&Person> for Admitted {
    fn from(p: &Person) -> Self {
        Admitted { person: p.person.clone(), device: p.device.clone(), role: p.role.clone() }
    }
}

/// The result of the pre-auth phase on a KEYED server.
#[derive(Debug)]
pub enum HandshakeOutcome {
    /// The client authenticated; proceed to the session with `scope` as the ceiling.
    Authed {
        /// The connection's capability ceiling.
        scope: Scope,
        /// The non-secret fingerprint of the key that did it ([`HubKeys::key_id`], or the device key's id), for an
        /// audit trail.
        key_id: Option<String>,
        /// Who, when a DEVICE authenticated (`PersonAuth`, decision 0288); `None` for a shared key. The session
        /// re-checks it before every write ([`crate::access::admit`]).
        person: Option<Person>,
    },
    /// Refused (or a transport error): the connection must close.
    Closed,
}

/// What one daemon's KEYED handshake is, beside its keys and the scopes it serves.
#[derive(Debug, Clone, Copy)]
pub struct HandshakeSpec<'a> {
    /// The service's OWN domain separator (signed into the mac or the device's signature):
    /// [`DATAHUB_DOMAIN`], [`BACKTESTHUB_DOMAIN`], the tradehub's or the gatewayhub's, never another
    /// hub's, so a login signed for another hub never verifies here (decision 0288's amendment).
    pub domain: Domain,
    /// The daemon's protocol version: answered in `Welcome` and signed into the mac.
    pub proto_version: u32,
    /// The DATA plane this daemon serves, so a verb of the other data plane is named by its tag
    /// ([`foreign_verb`]). `None` for a daemon on neither (the gatewayhub): a frame it cannot decode is
    /// undecodable, whatever its tag.
    pub plane: Option<Plane>,
    /// What the `Welcome` advertises.
    pub features: &'a [String],
    /// The deadline of the WHOLE phase, both reads, counted from the call ([`read_pre_auth_frame`]).
    pub deadline: Duration,
    /// The longest pre-auth reply this daemon writes: one over it is refused before its first byte and
    /// the connection closes. The data plane's is `crate::frame::MAX_FRAME_LEN` (its clients read a
    /// `Welcome` under it); the gatewayhub's clients read the handshake under
    /// [`SERVER_HANDSHAKE_MAX_FRAME_LEN`], and so the daemon writes under it.
    pub reply_ceiling: u32,
}

/// A step of [`run_keyed_handshake`] a daemon logs, in its own words, when it happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeNote {
    /// The first frame was a VERB (its wire tag), not `Hello`: refused, the socket dropped.
    VerbRefused(&'static str),
    /// The client's `Hello` named this other protocol version; auth will fail on the signed one.
    VersionDiffers(u32),
    /// `Auth` asked for a scope this server does not serve or holds no key for: refused without
    /// consulting a key.
    NoKey(Scope),
    /// The mac verified and `AuthOk` went out.
    Authenticated(Scope),
    /// The mac did not verify.
    BadMac(Scope),
    /// The daemon's own admission refused the scope with this wire text (the tradehub's "control disabled on this
    /// node"), before any key was consulted.
    Refused(Scope, &'static str),
    /// A device's `PersonAuth` was refused for this reason (the wire says only `access denied`).
    PersonDenied(Scope, AccessRefusal),
    /// A pre-auth read ended (the peer left, went silent past the deadline, or faulted).
    ReadEnded(io::ErrorKind),
}

/// Why a pre-auth read ended ([`HandshakeNote::ReadEnded`]), in words, for a daemon's ONE
/// `connection closed (handshake refused)` line: a peer that only hangs up writes no step line, so
/// without these words the refusal has no reason in the log at all. PURE.
pub fn read_end_words(kind: io::ErrorKind) -> String {
    use io::ErrorKind;
    match kind {
        ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => {
            "the client hung up before authenticating: a dial that carries NO key leaves after the \
             Welcome (the device holds none, or its dial was resolved before the key existed), a port \
             probe before the Hello"
                .to_string()
        }
        ErrorKind::WouldBlock | ErrorKind::TimedOut => {
            "the client sent nothing before the handshake deadline".to_string()
        }
        other => format!("the handshake read failed: {other}"),
    }
}

/// What one pre-auth frame turned out to be.
enum PreAuth {
    Hello(u32),
    Auth(Scope, Vec<u8>),
    PersonAuth(String, Scope, Vec<u8>),
    Register(Registration),
    Verb(&'static str),
    Undecodable,
}

/// Read a pre-auth body. A verb is named whichever data plane it is on: this daemon's by decoding it
/// (`R`, named by `verb_name`), the other daemon's by its tag ([`foreign_verb`]), because the enum that
/// would decode it is not this daemon's. A daemon on no data plane (`here` is `None`) names only its own.
fn pre_auth<R: DeserializeOwned>(
    body: &[u8],
    here: Option<Plane>,
    verb_name: &impl Fn(&R) -> &'static str,
) -> PreAuth {
    match serde_json::from_slice::<HandshakeFrame>(body) {
        Ok(HandshakeFrame::Hello { proto_version }) => PreAuth::Hello(proto_version),
        Ok(HandshakeFrame::Auth { scope, mac }) => PreAuth::Auth(scope, mac),
        Ok(HandshakeFrame::PersonAuth { key_id, scope, sig }) => {
            PreAuth::PersonAuth(key_id, scope, sig)
        }
        Ok(HandshakeFrame::Register(registration)) => PreAuth::Register(registration),
        // A client says what it reads only once authenticated: before that it is an unexpected frame.
        Ok(HandshakeFrame::Reads { .. }) => PreAuth::Verb("Reads"),
        Err(_) => match serde_json::from_slice::<R>(body) {
            Ok(verb) => PreAuth::Verb(verb_name(&verb)),
            Err(_) => here
                .and_then(|here| foreign_verb(body, here))
                .map_or(PreAuth::Undecodable, PreAuth::Verb),
        },
    }
}

/// Run `Hello` -> `Welcome{nonce}` -> `Auth` | `PersonAuth` | `Register` -> verify on a KEYED server — the ONE
/// handshake of every hub daemon (decision 0288: the tradehub's own was deleted) — telling `note` each step a
/// daemon logs.
///
/// `admission` is the daemon's own word on a scope, asked FIRST for every answer, before any key: `Err(text)` is
/// sent verbatim (the tradehub names the scope it refuses; a data plane answers `bad mac`). Then, for `Auth`, the
/// mac is verified against the REQUESTED scope's key: a scope whose key is ABSENT is refused WITHOUT consulting a
/// key (the closed-gate shape), and otherwise `HubKeys::verify` decides in constant time under `spec.domain`, so a
/// tag minted for another service never verifies here. For `PersonAuth` the access list decides
/// ([`verify_person`]); a hub with no list refuses it. `register` is the tradehub's invitation door: a hub without
/// one refuses `Register` as it refuses any other unexpected second frame.
///
/// `R` is the daemon's own request enum and `verb_name` the function that names one of its verbs. The
/// handshake frames themselves are this module's. Both reads share ONE `spec.deadline`, counted from
/// the call ([`read_pre_auth_frame`]).
pub fn run_keyed_handshake<R: DeserializeOwned>(
    stream: &mut TcpStream,
    keys: &HubKeys,
    spec: HandshakeSpec<'_>,
    admission: impl Fn(Scope) -> Result<(), &'static str>,
    register: Option<RegisterDoor<'_>>,
    verb_name: impl Fn(&R) -> &'static str,
    mut note: impl FnMut(HandshakeNote),
) -> HandshakeOutcome {
    let HandshakeSpec { domain, proto_version, plane, features, deadline, reply_ceiling } = spec;
    let by = Instant::now() + deadline;
    let deny = |stream: &mut TcpStream, reason: &str| {
        let denied = HandshakeReply::AuthDenied { reason: reason.to_string() };
        let _ = write_reply(stream, &denied, reply_ceiling);
    };
    // Frame 1: it MUST be Hello. Anything else is an unauthenticated verb: refuse it here. This is
    // where an unauthenticated LoadBars / Backfill / RunSlice is denied and the socket dropped.
    let body = match read_pre_auth_frame(stream, by) {
        Ok(b) => b,
        Err(e) => {
            note(HandshakeNote::ReadEnded(e.kind()));
            return HandshakeOutcome::Closed;
        }
    };
    let client_version = match pre_auth(&body, plane, &verb_name) {
        PreAuth::Hello(proto_version) => proto_version,
        PreAuth::Auth(..) | PreAuth::PersonAuth(..) | PreAuth::Register(_) => {
            note(HandshakeNote::VerbRefused("Auth"));
            deny(stream, "not authenticated: expected Hello first");
            return HandshakeOutcome::Closed;
        }
        PreAuth::Verb(verb) => {
            note(HandshakeNote::VerbRefused(verb));
            deny(stream, "not authenticated: expected Hello first");
            return HandshakeOutcome::Closed;
        }
        PreAuth::Undecodable => {
            deny(stream, "expected Hello (undecodable request)");
            return HandshakeOutcome::Closed;
        }
    };

    // The challenge. We always answer Hello with our version + a fresh nonce; a client on another
    // protocol version cannot forge a valid mac anyway (the version is SIGNED), so a skew fails
    // cleanly at verify rather than needing a branch here.
    let nonce = fresh_nonce();
    // `person-auth` is advertised HERE, where the list is, so no daemon's feature list can disagree with its keys.
    let mut features = features.to_vec();
    if keys.access().is_some() {
        features.push(FEATURE_PERSON_AUTH.to_string());
    }
    let welcome = HandshakeReply::Welcome { proto_version, features, nonce: Some(nonce) };
    if write_reply(stream, &welcome, reply_ceiling).is_err() {
        return HandshakeOutcome::Closed;
    }
    if client_version != proto_version {
        note(HandshakeNote::VersionDiffers(client_version));
    }

    // Frame 2: the answer. Still PRE-AUTH, same small ceiling (a scope plus a mac or a signature, or a
    // registration's few short fields).
    let body2 = match read_pre_auth_frame(stream, by) {
        Ok(b) => b,
        Err(e) => {
            note(HandshakeNote::ReadEnded(e.kind()));
            return HandshakeOutcome::Closed;
        }
    };
    let (scope, mac) = match pre_auth(&body2, plane, &verb_name) {
        PreAuth::Auth(scope, mac) => (scope, mac),
        PreAuth::Register(registration) => {
            let Some(door) = register else {
                deny(stream, "expected Auth after Welcome");
                return HandshakeOutcome::Closed;
            };
            // The door logs its own refusal and its grant; the connection closes after either answer.
            let reply = match door(registration, &nonce) {
                Ok(registered) => HandshakeReply::Registered(registered),
                Err(reason) => HandshakeReply::AuthDenied { reason: reason.to_string() },
            };
            let _ = write_reply(stream, &reply, reply_ceiling);
            return HandshakeOutcome::Closed;
        }
        PreAuth::PersonAuth(key_id, scope, sig) => {
            // A device (decision 0288): the daemon's admission, then the access list, never a shared key.
            if let Err(reason) = admission(scope) {
                note(HandshakeNote::Refused(scope, reason));
                deny(stream, reason);
                return HandshakeOutcome::Closed;
            }
            // A list that holds no row at all admits nobody, and says how a box gets its first device.
            if keys.access().is_some_and(|list| !list.held()) {
                note(HandshakeNote::PersonDenied(scope, AccessRefusal::Unknown));
                deny(stream, NO_DEVICE_REGISTERED);
                return HandshakeOutcome::Closed;
            }
            let decided = match keys.access() {
                Some(list) => {
                    verify_person(list, domain, &nonce, proto_version, &key_id, scope, &sig)
                }
                None => Err(AccessRefusal::Unknown),
            };
            return match decided {
                Ok(person) => {
                    let reply =
                        HandshakeReply::AuthOk { scope, admitted: Some(Admitted::from(&person)) };
                    if write_reply(stream, &reply, reply_ceiling).is_err() {
                        return HandshakeOutcome::Closed;
                    }
                    note(HandshakeNote::Authenticated(scope));
                    HandshakeOutcome::Authed { scope, key_id: Some(key_id), person: Some(person) }
                }
                Err(why) => {
                    note(HandshakeNote::PersonDenied(scope, why));
                    deny(stream, ACCESS_DENIED);
                    HandshakeOutcome::Closed
                }
            };
        }
        PreAuth::Hello(_) | PreAuth::Verb(_) => {
            deny(stream, "expected Auth after Welcome");
            return HandshakeOutcome::Closed;
        }
        PreAuth::Undecodable => {
            deny(stream, "expected Auth (undecodable request)");
            return HandshakeOutcome::Closed;
        }
    };

    // The daemon's own word first, in its own text (the tradehub names the scope it refuses).
    if let Err(reason) = admission(scope) {
        note(HandshakeNote::Refused(scope, reason));
        deny(stream, reason);
        return HandshakeOutcome::Closed;
    }
    // A scope this server holds NO key for is refused without consulting a key: the closed-gate shape.
    // It is also how an observe-only daemon declines control outright rather than letting an empty key
    // decide it by accident.
    if !keys.has(scope) {
        note(HandshakeNote::NoKey(scope));
        // Deliberately the SAME coarse reason a bad mac gets: distinguishing "no key for that scope"
        // from "wrong key for that scope" tells an unauthenticated peer which scopes this server
        // offers. The daemon's log line carries the real answer.
        deny(stream, "bad mac");
        return HandshakeOutcome::Closed;
    }

    // Constant-time verify against the REQUESTED scope's key, under the daemon's own domain separator,
    // so a tag minted for another service (which may hold the same key bytes) never verifies here.
    if keys.verify(domain, scope, &nonce, proto_version, &mac) {
        let reply = HandshakeReply::AuthOk { scope, admitted: None };
        if write_reply(stream, &reply, reply_ceiling).is_err() {
            return HandshakeOutcome::Closed;
        }
        note(HandshakeNote::Authenticated(scope));
        HandshakeOutcome::Authed { scope, key_id: keys.key_id(scope), person: None }
    } else {
        note(HandshakeNote::BadMac(scope));
        deny(stream, "bad mac");
        HandshakeOutcome::Closed
    }
}

/// A CLIENT's answer to a hub's `Welcome` — the one choice every hub client makes (decision 0288): a device key
/// signs [`HandshakeFrame::PersonAuth`] where `features` advertise [`FEATURE_PERSON_AUTH`]; a shared key (a
/// daemon's, or a test's) signs the HMAC [`HandshakeFrame::Auth`] under `domain`.
///
/// # Errors
/// [`HubCallError::NotOffered`] (`person-auth`) for a device key and a hub that does not advertise it (an older
/// hub): nothing is sent.
pub fn auth_answer(
    signer: Signer<'_>,
    domain: Domain,
    nonce: &[u8; 32],
    proto_version: u32,
    scope: Scope,
    features: &[String],
) -> Result<HandshakeFrame, HubCallError> {
    match signer {
        Signer::Shared(key) => Ok(HandshakeFrame::Auth {
            scope,
            mac: crate::auth::sign(domain, key, nonce, proto_version, scope),
        }),
        Signer::Device(device) if features.iter().any(|f| f == FEATURE_PERSON_AUTH) => {
            Ok(HandshakeFrame::PersonAuth {
                key_id: device.key_id(),
                scope,
                sig: device.sign(domain, nonce, proto_version, scope),
            })
        }
        Signer::Device(_) => Err(HubCallError::NotOffered {
            feature: FEATURE_PERSON_AUTH.to_string(),
            text: format!(
                "this hub does not advertise `{FEATURE_PERSON_AUTH}` (an older hub), so this device's key cannot \
                 answer it: nothing was sent"
            ),
        }),
    }
}

/// [`write_frame`] under `ceiling`: a reply over it is refused BEFORE its first byte (`InvalidData`).
fn write_reply(stream: &mut TcpStream, reply: &HandshakeReply, ceiling: u32) -> io::Result<()> {
    if json_body_len(reply).is_none_or(|len| len > ceiling as usize) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a pre-auth reply over the {ceiling}-byte reply ceiling; nothing was sent"),
        ));
    }
    write_frame(stream, reply)
}

/// One PRE-AUTH frame off `stream`: read under [`SERVER_HANDSHAKE_MAX_FRAME_LEN`] and before `by`, the
/// phase's ONE deadline (the module doc says why a per-read timeout is not one). Past `by` it fails
/// with [`io::ErrorKind::TimedOut`]; a read the socket itself times out answers `WouldBlock` on unix
/// and `TimedOut` on windows, as any timed-out read does.
///
/// ⚠ It leaves the socket's read timeout at whatever was LEFT before its last read: a daemon arms
/// its own read bound once the handshake is done.
pub fn read_pre_auth_frame(stream: &mut TcpStream, by: Instant) -> io::Result<Vec<u8>> {
    read_frame_raw_capped(&mut ByDeadline { stream, by }, SERVER_HANDSHAKE_MAX_FRAME_LEN)
}

/// A `Read` over the handshake socket that arms the socket's read timeout to the time LEFT before
/// `by` ahead of every read, and fails with `TimedOut` once `by` has passed. The frame codec reads a
/// length prefix and a body with `read_exact`, i.e. in as many `read`s as the peer cares to split them
/// into, so a per-`read` timeout fixed at the start bounds a silent peer and not a slow one.
struct ByDeadline<'a> {
    stream: &'a mut TcpStream,
    by: Instant,
}

impl Read for ByDeadline<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let left = self.by.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the handshake deadline passed before the frame was complete",
            ));
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buffer)
    }
}

#[cfg(test)]
#[path = "handshake_tests.rs"]
mod handshake_tests;

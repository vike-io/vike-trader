//! The KEYED pre-auth handshake three daemons run, the datahub, the backtesthub and the gatewayhub:
//! `Hello` -> `Welcome{nonce}` -> `Auth` -> verify, spelled ONCE (the refusal order, every reason on the
//! wire), and the frames it is made of. What differs per daemon is a value, [`HandshakeSpec`] (its
//! domain separator, version, plane, advertisement, deadline and reply ceiling), plus the scopes it
//! serves. Each daemon keeps its own LOG lines (its words and its `tracing` target, which subscribers
//! filter on), so the steps worth a line reach the caller as [`HandshakeNote`]s, at the moment each
//! happens. The tradehub's handshake is its own (its refusals name the scope), and shares the read.
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
//! The frames here encode to the same bytes as each client's `Request::Hello`, `Request::Auth`,
//! `Response::Welcome`, `Response::AuthOk` and `Response::AuthDenied`: externally tagged, field for
//! field. Those variants stay in each client's enums because a key-less daemon answers them after the
//! decode (and every client sends and reads them); `crates/vike-datahub-client/src/proto_tests.rs`,
//! its backtesthub twin and `crates/vike-gatewayhub-client/src/proto_tests.rs` hold the spellings to
//! one encoding.

use std::io::{self, Read};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::auth::DATAHUB_DOMAIN;
use crate::auth::{Domain, HubKeys, Scope, fresh_nonce};
use crate::frame::{json_body_len, read_frame_raw_capped, write_frame};
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
}

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
    AuthOk { scope: Scope },
    /// Refused; the connection closes after this frame. ⚠ `reason` is deliberately coarse: it never
    /// distinguishes "no such key" from "wrong key" and never echoes a key, a mac or a nonce.
    AuthDenied { reason: String },
}

/// The result of the pre-auth phase on a KEYED server.
#[derive(Debug)]
pub enum HandshakeOutcome {
    /// The client authenticated; proceed to the session with `scope` as the ceiling.
    Authed {
        /// The connection's capability ceiling.
        scope: Scope,
        /// The non-secret fingerprint of the key that did it ([`HubKeys::key_id`]), for an audit trail.
        key_id: Option<String>,
    },
    /// Refused (or a transport error): the connection must close.
    Closed,
}

/// What one daemon's KEYED handshake is, beside its keys and the scopes it serves.
#[derive(Debug, Clone, Copy)]
pub struct HandshakeSpec<'a> {
    /// The service's domain separator (signed into the mac): [`DATAHUB_DOMAIN`] for both data-plane
    /// daemons, so a tag minted for another service never verifies here.
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
}

/// What one pre-auth frame turned out to be.
enum PreAuth {
    Hello(u32),
    Auth(Scope, Vec<u8>),
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
        Err(_) => match serde_json::from_slice::<R>(body) {
            Ok(verb) => PreAuth::Verb(verb_name(&verb)),
            Err(_) => here
                .and_then(|here| foreign_verb(body, here))
                .map_or(PreAuth::Undecodable, PreAuth::Verb),
        },
    }
}

/// Run `Hello` -> `Welcome{nonce}` -> `Auth` -> verify on a KEYED server, telling `note` each step a
/// daemon logs. The mac is verified against the REQUESTED scope's key: a scope the daemon does not
/// serve (`served` says no) or whose key is ABSENT is refused WITHOUT consulting a key (the closed-gate
/// shape), and otherwise `HubKeys::verify` decides in constant time under `spec.domain`, so a tag
/// minted for another service never verifies here.
///
/// `R` is the daemon's own request enum and `verb_name` the function that names one of its verbs. The
/// handshake frames themselves are this module's. Both reads share ONE `spec.deadline`, counted from
/// the call ([`read_pre_auth_frame`]).
pub fn run_keyed_handshake<R: DeserializeOwned>(
    stream: &mut TcpStream,
    keys: &HubKeys,
    spec: HandshakeSpec<'_>,
    served: impl Fn(Scope) -> bool,
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
        Err(_) => return HandshakeOutcome::Closed,
    };
    let client_version = match pre_auth(&body, plane, &verb_name) {
        PreAuth::Hello(proto_version) => proto_version,
        PreAuth::Auth(..) => {
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
    let welcome =
        HandshakeReply::Welcome { proto_version, features: features.to_vec(), nonce: Some(nonce) };
    if write_reply(stream, &welcome, reply_ceiling).is_err() {
        return HandshakeOutcome::Closed;
    }
    if client_version != proto_version {
        note(HandshakeNote::VersionDiffers(client_version));
    }

    // Frame 2: the Auth answer. Still PRE-AUTH, same small ceiling (a scope plus a 32-byte mac).
    let body2 = match read_pre_auth_frame(stream, by) {
        Ok(b) => b,
        Err(_) => return HandshakeOutcome::Closed,
    };
    let (scope, mac) = match pre_auth(&body2, plane, &verb_name) {
        PreAuth::Auth(scope, mac) => (scope, mac),
        PreAuth::Hello(_) | PreAuth::Verb(_) => {
            deny(stream, "expected Auth after Welcome");
            return HandshakeOutcome::Closed;
        }
        PreAuth::Undecodable => {
            deny(stream, "expected Auth (undecodable request)");
            return HandshakeOutcome::Closed;
        }
    };

    // A scope this server does not serve, or holds NO key for, is refused without consulting a key:
    // the closed-gate shape. It is also how an observe-only daemon declines control outright rather
    // than letting an empty key decide it by accident.
    if !served(scope) || !keys.has(scope) {
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
        if write_reply(stream, &HandshakeReply::AuthOk { scope }, reply_ceiling).is_err() {
            return HandshakeOutcome::Closed;
        }
        note(HandshakeNote::Authenticated(scope));
        HandshakeOutcome::Authed { scope, key_id: keys.key_id(scope) }
    } else {
        note(HandshakeNote::BadMac(scope));
        deny(stream, "bad mac");
        HandshakeOutcome::Closed
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
